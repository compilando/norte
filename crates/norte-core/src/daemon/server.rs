//! Servidor del daemon (ADR 0011): UDS + `SO_PEERCRED`, dispatch de
//! `initialize`/`fs.*`/`task.*`/`daemon.shutdown`, broadcast de
//! `task.progress` y shutdown por inactividad o petición.

use std::collections::HashMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use norte_proto::wire::{
    FrameDecoder, MessageKind, Notification, Request, RequestId, Response, RpcError, classify,
    codes, encode_frame,
};
use norte_proto::{TaskId, methods};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::DaemonError;
use super::approvals::DaemonApprovalResolver;
use crate::Engine;
use crate::engine::TransferOptions;
use crate::journal::Actor;
use crate::policy::{OpSet, Scope, ScopeRegistry};
use crate::scheduler::TaskHandle;

/// Intervalo mínimo entre notificaciones de progreso de UNA task (≤30 Hz).
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(33);
/// Cada cuánto evalúa el server la condición de inactividad.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Frames pendientes de escribir por conexión. Un cliente que no drena su
/// lado del socket llega aquí y se le CORTA: jamás memoria sin límite por
/// un peer lento u hostil (hallazgo M1 del security-reviewer).
const OUTBOX_FRAMES: usize = 1024;
/// Errores de parse consecutivos tolerados antes de cortar la conexión
/// (un peer que solo emite basura no merece respuestas infinitas).
const MAX_PARSE_ERRORS: u32 = 16;
/// Conexiones simultáneas por daemon (mismo uid; anti-agotamiento).
const MAX_CONNECTIONS: usize = 256;
/// Tasks vivas simultáneas encoladas vía el daemon.
const MAX_LIVE_TASKS: usize = 512;
/// Snapshots TERMINALES retenidos para el resync de `task.list` (un
/// frontend que reconecta ve el desenlace de lo que se perdió).
const RECENT_TERMINAL: usize = 64;
/// Listados paginados VIVOS retenidos por conexión (ADR 0017): cada uno es un
/// `EntryStream` perezoso sin drenar. Al abrir el (N+1), se expulsa el más
/// viejo (LRU). Un TUI navega 1-2 a la vez; el tope acota el coste (incluido
/// un hilo blocking parkeado del productor local por listado no drenado).
const MAX_OPEN_LISTINGS: usize = 8;
/// Tope GLOBAL de listados retenidos en todo el daemon (M1 del rust-reviewer):
/// 256 conexiones × 8 = 2048 productores de `vfs-local` podrían quedar
/// parkeados en `blocking_send` > el pool blocking por defecto (512). Con este
/// tope (bien por debajo del pool), al superarlo un listado NUEVO se drena
/// ENTERO en línea (libera el hilo al instante) en vez de retenerse: bajo
/// presión del MISMO uid, la paginación degrada a listado-completo, jamás a
/// agotar el pool. Coste de una llamada lenta, nunca corrupción ni truncado.
const GLOBAL_MAX_LISTINGS: usize = 256;
/// Peticiones de scope PENDIENTES retenidas en TODO el daemon (M3-3b): tope
/// GLOBAL. Debajo, cada conexión tiene su propio sub-cap
/// [`MAX_PENDING_SCOPE_PER_CONN`] para que una sola sesión no agote el canal
/// de las demás (patrón de los listings: por-conexión + global).
const MAX_PENDING_SCOPE: usize = 256;
/// Peticiones de scope pendientes por CONEXIÓN: acota lo que una sola sesión
/// puede retener del tope global. Se limpian al morir la conexión.
const MAX_PENDING_SCOPE_PER_CONN: usize = 16;
/// Tope del TTL de un scope concedido (24 h): una petición con `ttl_ms`
/// enorme no concede acceso cuasi-perpetuo por accidente.
const MAX_SCOPE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Cada cuánto barre el server los listados retenidos expirados de una
/// conexión VIVA-pero-muda (además del barrido perezoso en cada `fs.list`).
const LISTING_SWEEP: Duration = Duration::from_secs(30);

/// Configuración del daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path del socket; `None` = [`super::default_socket_path`].
    pub socket_path: Option<PathBuf>,
    /// Apagado tras este tiempo sin clientes NI tasks. `None` = nunca.
    pub idle_timeout: Option<Duration>,
    /// TTL de un listado paginado retenido sin continuar (ADR 0017): pasado
    /// este tiempo se descarta aunque la conexión siga viva. Configurable para
    /// testear la expiración sin esperas largas.
    pub listing_ttl: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            idle_timeout: Some(Duration::from_mins(5)),
            listing_ttl: Duration::from_mins(2),
        }
    }
}

/// Estado compartido entre conexiones.
struct Shared {
    engine: Arc<Engine>,
    /// Tasks vivas encoladas POR el daemon (para `task.cancel` y graceful).
    tasks: Mutex<HashMap<u64, TaskHandle>>,
    /// Salidas de notificación de cada cliente YA inicializado, por id de
    /// conexión (la conexión retira la SUYA al morir — sin esto el writer
    /// task jamás terminaría: el broadcast retendría su sender). Bounded:
    /// un suscriptor que no drena pierde la suscripción, jamás acumula.
    subscribers: Mutex<HashMap<u64, Subscriber>>,
    /// Desenlaces recientes (snapshots terminales) para `task.list`.
    recent: Mutex<std::collections::VecDeque<norte_proto::TaskProgress>>,
    /// Contador de ids de conexión.
    next_conn: AtomicUsize,
    /// Conexiones autenticadas vivas.
    connections: AtomicUsize,
    /// Apagado global (graceful: primero deja de aceptar).
    shutdown: CancellationToken,
    /// `true` = el shutdown pedido quiere cancelar las tasks primero.
    hard_shutdown: CancellationToken,
    /// uid del daemon (derivado del propio socket): el ÚNICO peer admitido.
    uid: u32,
    /// TTL de un listado paginado retenido (ADR 0017); de [`DaemonConfig`].
    listing_ttl: Duration,
    /// Listados paginados retenidos en TODO el daemon (M1): tope global
    /// [`GLOBAL_MAX_LISTINGS`] para proteger el pool blocking.
    open_listings: Arc<AtomicUsize>,
    /// Registro de scopes concedidos por sesión de agente (M3-3b). Es la MISMA
    /// instancia (`Arc`-backed) que el `ScopedPolicy` del engine consulta en el
    /// gate: conceder aquí abre la frontera allí. Vacío si no hay policy.
    scopes: ScopeRegistry,
    /// Peticiones de scope pendientes de concesión humana, por `request_id`.
    /// Acotado a [`MAX_PENDING_SCOPE`].
    pending_scope: Mutex<HashMap<u64, PendingScope>>,
    /// Contador de `request_id` de scope (monótono).
    next_scope_req: AtomicU64,
    /// Router de aprobaciones `Ask` (M3-3b Task 4): el MISMO objeto (`Arc`)
    /// que el engine usa como `ApprovalResolver` — `policy.decide` aquí
    /// despierta al gate suspendido allí. Con [`Daemon::bind`]/
    /// [`Daemon::bind_with_scopes`] es un router huérfano (el engine no lo
    /// conoce): `policy.pending` responde vacío y `policy.decide` no encuentra
    /// ids — inofensivo.
    approvals: Arc<DaemonApprovalResolver>,
}

/// Una petición de scope registrada por un agente, a la espera de que un
/// humano la conceda con `policy.grant_scope`. Guarda lo pedido verbatim; la
/// sesión ya quedó validada contra el actor de la conexión al registrarla.
struct PendingScope {
    session: String,
    roots: Vec<norte_proto::VPath>,
    ops: Vec<String>,
    ttl_ms: u64,
}

/// Una salida de broadcast: el sender de la outbox y si la conexión es de
/// AGENTE (M3-3b Task 4, security MAJOR-1): las notifs `policy.*` cruzan
/// sesiones (rutas y ops de otras) y solo van a humanos — el mismo criterio
/// que el gate de `policy.pending`. `task.progress` sigue yendo a todos.
struct Subscriber {
    tx: mpsc::Sender<Arc<[u8]>>,
    is_agent: bool,
}

impl Shared {
    fn idle(&self) -> bool {
        self.connections.load(Ordering::SeqCst) == 0
            && self.tasks.lock().expect("tasks lock sano").is_empty()
    }

    fn broadcast(&self, frame: &Arc<[u8]>) {
        self.broadcast_filtered(frame, false);
    }

    /// Difunde SOLO a conexiones humanas (no-agente): notifs `policy.*`.
    fn broadcast_humans(&self, frame: &Arc<[u8]>) {
        self.broadcast_filtered(frame, true);
    }

    fn broadcast_filtered(&self, frame: &Arc<[u8]>, humans_only: bool) {
        let mut subs = self.subscribers.lock().expect("subscribers lock sano");
        // try_send: el que tiene la outbox llena pierde la suscripción (y
        // pronto la conexión, cuando su próximo response tampoco quepa) —
        // el backlog de un cliente lento jamás crece sin límite.
        subs.retain(|conn, s| {
            // Un agente excluido de ESTA notif conserva su suscripción.
            if humans_only && s.is_agent {
                return true;
            }
            match s.tx.try_send(Arc::clone(frame)) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(conn, "suscriptor sin drenar: expulsado del broadcast");
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }
}

/// El daemon enlazado a su socket, listo para [`Daemon::run`].
///
/// El `Debug` es deliberadamente somero (path del socket): el estado
/// interno no es API.
pub struct Daemon {
    listener: UnixListener,
    socket_path: PathBuf,
    shared: Arc<Shared>,
    idle_timeout: Option<Duration>,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Daemon {
    /// Crea (o verifica) el directorio del socket, enlaza y autentica el
    /// entorno: dueño/modo del dir, jamás root, jamás dos daemons.
    ///
    /// # Errors
    /// [`DaemonError`]: dir inseguro, root, socket ocupado por un daemon
    /// vivo, o I/O.
    #[tracing::instrument(skip(engine, cfg))]
    pub async fn bind(engine: Arc<Engine>, cfg: DaemonConfig) -> Result<Self, DaemonError> {
        Self::bind_with_scopes(engine, ScopeRegistry::new(), cfg).await
    }

    /// Como [`Self::bind`] pero comparte `scopes` con el `ScopedPolicy` del
    /// engine (M3-3b): el llamante construye
    /// `engine.with_policy(ScopedPolicy::new(scopes.clone(), cfg), …)` y pasa
    /// el MISMO registro aquí para que `policy.grant_scope` abra la frontera
    /// que el gate del engine consulta. Sin policy, pásale un registro vacío
    /// (o usa [`Self::bind`]).
    ///
    /// # Errors
    /// [`DaemonError`]: dir inseguro, root, socket ocupado por un daemon vivo,
    /// o I/O.
    #[tracing::instrument(skip(engine, scopes, cfg))]
    pub async fn bind_with_scopes(
        engine: Arc<Engine>,
        scopes: ScopeRegistry,
        cfg: DaemonConfig,
    ) -> Result<Self, DaemonError> {
        Self::bind_with_policy(
            engine,
            scopes,
            Arc::new(DaemonApprovalResolver::default()),
            cfg,
        )
        .await
    }

    /// Como [`Self::bind_with_scopes`] pero además comparte el router de
    /// aprobaciones `Ask` (M3-3b Task 4). Orden de construcción: el llamante
    /// crea `approvals` PRIMERO, construye el engine con
    /// `with_policy(ScopedPolicy::new(scopes.clone(), cfg), approvals.clone())`
    /// y pasa el MISMO `Arc` aquí; este bind le instala la salida hacia los
    /// suscriptores (broadcast de `policy.approval_required`) y enruta
    /// `policy.decide`/`policy.pending` hacia él.
    ///
    /// # Errors
    /// [`DaemonError`]: dir inseguro, root, socket ocupado por un daemon vivo,
    /// o I/O.
    #[tracing::instrument(skip(engine, scopes, approvals, cfg))]
    pub async fn bind_with_policy(
        engine: Arc<Engine>,
        scopes: ScopeRegistry,
        approvals: Arc<DaemonApprovalResolver>,
        cfg: DaemonConfig,
    ) -> Result<Self, DaemonError> {
        // La resolución del path por defecto puede tocar el FS (sonda de
        // uid del fallback /tmp): TODO dentro del spawn_blocking (regla 2).
        let (listener, uid, socket_path) = tokio::task::spawn_blocking({
            let requested = cfg.socket_path;
            move || -> Result<(std::os::unix::net::UnixListener, u32, PathBuf), DaemonError> {
                let socket_path = requested.unwrap_or_else(|| super::default_socket_path(None));
                let dir = socket_path
                    .parent()
                    .ok_or(DaemonError::InsecureDir {
                        reason: "el socket necesita un directorio padre",
                    })?
                    .to_path_buf();
                prepare_socket_dir(&dir)?;
                // ¿Hay un daemon VIVO? Un connect lo delata; un socket
                // huérfano (crash previo) da ECONNREFUSED y se retira.
                match std::os::unix::net::UnixStream::connect(&socket_path) {
                    Ok(_) => return Err(DaemonError::AlreadyRunning),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => match std::fs::remove_file(&socket_path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    },
                }
                let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
                    Ok(l) => l,
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        return Err(DaemonError::AlreadyRunning);
                    }
                    Err(e) => return Err(e.into()),
                };
                // Nuestro euid = el dueño del socket que ACABAMOS de crear
                // (sin unsafe, regla 5). Solo el mismo uid podrá hablar.
                let md = std::fs::metadata(&socket_path)?;
                let uid = md.uid();
                if uid == 0 {
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(DaemonError::Root);
                }
                // El socket mismo tampoco regala nada: 0600.
                std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
                Ok((listener, uid, socket_path))
            }
        })
        .await
        // Un panic del closure NO es un problema del dir: categoría honesta.
        .map_err(|e| DaemonError::Io(std::io::Error::other(e)))??;
        listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(listener)?;

        tracing::info!(socket = %socket_path.display(), uid, "daemon enlazado");
        let shared = Arc::new(Shared {
            engine,
            tasks: Mutex::new(HashMap::new()),
            recent: Mutex::new(std::collections::VecDeque::new()),
            subscribers: Mutex::new(HashMap::new()),
            next_conn: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            shutdown: CancellationToken::new(),
            hard_shutdown: CancellationToken::new(),
            uid,
            listing_ttl: cfg.listing_ttl,
            open_listings: Arc::new(AtomicUsize::new(0)),
            scopes,
            pending_scope: Mutex::new(HashMap::new()),
            next_scope_req: AtomicU64::new(0),
            approvals: Arc::clone(&approvals),
        });
        // La salida del router de aprobaciones hacia los suscriptores. `Weak`
        // rompe el ciclo Shared → approvals → closure → Shared: muerto el
        // daemon, un Ask tardío no difunde a nadie (y vencerá por TTL).
        let weak = Arc::downgrade(&shared);
        approvals.set_broadcaster(Box::new(move |notif| {
            let Some(shared) = weak.upgrade() else { return };
            // Si la serialización fallara (no puede: struct plano), mejor NO
            // emitir que emitir una notif con shape corrupto.
            let Ok(params) = serde_json::to_value(&notif) else {
                return;
            };
            let n = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::POLICY_APPROVAL_REQUIRED.into(),
                params: Some(params),
            };
            if let Ok(frame) = encode_frame(&n) {
                // SOLO humanos (security MAJOR-1): la notif cruza sesiones —
                // mismo criterio que el gate User-only de `policy.pending`.
                shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
            }
        }));
        Ok(Self {
            listener,
            socket_path,
            shared,
            idle_timeout: cfg.idle_timeout,
        })
    }

    /// Dónde quedó el socket (para clientes y logs).
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Token que apaga el daemon desde fuera (SIGTERM del binario).
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// Sirve hasta el shutdown (petición, token externo o inactividad).
    /// Al salir, el socket se ha retirado del FS y las tasks han terminado
    /// (graceful) o han sido canceladas (hard).
    ///
    /// # Errors
    /// Solo I/O irrecuperable del listener.
    ///
    /// # Panics
    /// Nunca: los locks internos no se envenenan (nadie panica con ellos).
    #[tracing::instrument(skip(self), fields(socket = %self.socket_path.display()))]
    pub async fn run(self) -> Result<(), DaemonError> {
        let shared = Arc::clone(&self.shared);
        let mut idle_since = tokio::time::Instant::now();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, _addr) = accepted?;
                    // El keepalive de inactividad NO cuenta conexiones sin
                    // autenticar (la resetea serve tras la auth); el cap
                    // anti-agotamiento corta aquí, antes de gastar nada.
                    if shared.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                        tracing::warn!("límite de conexiones alcanzado; rechazada");
                        drop(stream);
                    } else {
                        spawn_connection(stream, Arc::clone(&shared));
                    }
                }
                () = shared.shutdown.cancelled() => break,
                () = tokio::time::sleep(IDLE_POLL) => {
                    if !shared.idle() {
                        idle_since = tokio::time::Instant::now();
                    } else if let Some(t) = self.idle_timeout
                        && idle_since.elapsed() >= t
                    {
                        tracing::info!("shutdown por inactividad");
                        break;
                    }
                }
            }
        }

        // Fase de apagado: nada de clientes nuevos (el listener muere con
        // el drop); hard = cancelar tasks; graceful = esperarlas — y si el
        // hard llega DURANTE la espera (segunda señal), se cancelan ya.
        drop(self.listener);
        let mut hard_done = false;
        loop {
            if shared.hard_shutdown.is_cancelled() && !hard_done {
                hard_done = true;
                for handle in shared.tasks.lock().expect("tasks lock sano").values() {
                    handle.cancel();
                }
            }
            if shared.tasks.lock().expect("tasks lock sano").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let socket_path = self.socket_path.clone();
        // Regla 2: ni un unlink síncrono en el runtime.
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(socket_path)).await;
        tracing::info!("daemon apagado");
        Ok(())
    }

    /// Token que CANCELA las tasks vivas además de apagar (segunda señal
    /// del binario, o `stop --hard`).
    #[must_use]
    pub fn hard_shutdown_token(&self) -> CancellationToken {
        self.shared.hard_shutdown.clone()
    }
}

/// Verifica (creándolo si falta) que el dir del socket es NUESTRO y 0700:
/// jamás symlink, jamás de otro uid, jamás accesible a otros.
fn prepare_socket_dir(dir: &Path) -> Result<(), DaemonError> {
    match std::fs::create_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let md = std::fs::symlink_metadata(dir)?;
    if md.file_type().is_symlink() {
        return Err(DaemonError::InsecureDir {
            reason: "el dir del socket es un symlink",
        });
    }
    if !md.is_dir() {
        return Err(DaemonError::InsecureDir {
            reason: "el path del socket no es un directorio",
        });
    }
    // Endurecer modo ANTES de comparar: si el dir es nuestro, esto lo deja
    // 0700; si es de otro, fallará o lo delatará el check de dueño.
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| {
            DaemonError::InsecureDir {
                reason: "no se pudo endurecer el modo a 0700",
            }
        })?;
    }
    let md = std::fs::symlink_metadata(dir)?;
    if md.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::InsecureDir {
            reason: "el dir del socket es accesible a otros usuarios",
        });
    }
    // Dueño: comparado contra el euid REAL más adelante (el del socket
    // creado); aquí basta rechazar dirs que no podamos poseer.
    let probe = dir.join(format!(".norte-owner-probe-{}", std::process::id()));
    let owned = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.uid() == md.uid());
    let _ = std::fs::remove_file(&probe);
    if !owned {
        return Err(DaemonError::InsecureDir {
            reason: "el dir del socket pertenece a otro usuario",
        });
    }
    Ok(())
}

/// ¿Es un id de sesión de agente admisible? Charset cerrado `[A-Za-z0-9._-]`,
/// 1..=64: el id viaja a journal, tracing y modales de aprobación de TODOS
/// los frontends — un charset cerrado en la frontera vale más que confiar en
/// que cada consumidor enmascare (que además deben, defensa en profundidad).
/// `.`/`..` se rechazan por adelantado (MINOR-4 security M3-4): si algún día
/// una sesión deriva un fichero (export de audit M3-5), jamás será traversal.
fn valid_agent_session(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// ¿Se admite a este peer? Solo el MISMO uid (spec §17.6). Root NO entra:
/// un daemon de usuario no es superficie para procesos privilegiados.
fn peer_allowed(peer_uid: u32, daemon_uid: u32) -> bool {
    peer_uid == daemon_uid
}

fn spawn_connection(stream: UnixStream, shared: Arc<Shared>) {
    tokio::spawn(async move {
        // Auth ANTES de leer un solo byte (ADR 0011).
        let peer = match stream.peer_cred() {
            Ok(cred) => cred,
            Err(e) => {
                tracing::warn!(error = %e, "peer_cred falló; conexión rechazada");
                return;
            }
        };
        if !peer_allowed(peer.uid(), shared.uid) {
            tracing::warn!(peer_uid = peer.uid(), "conexión de otro uid rechazada");
            return;
        }
        shared.connections.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = serve_connection(stream, &shared).await {
            tracing::debug!(error = %e, "conexión terminada con error");
        }
        shared.connections.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Un listado paginado VIVO retenido por el daemon entre páginas (ADR 0017):
/// el `EntryStream` perezoso sin drenar, la ruta que lo abrió (para validar
/// que un `cursor` corresponde a ESTE listado) y cuándo se usó por última vez
/// (TTL/LRU). Soltar la struct = soltar el stream = cancelación cooperativa
/// hasta el productor del provider (regla 3).
struct OpenListing {
    path: norte_proto::VPath,
    stream: norte_vfs::EntryStream,
    last_used: std::time::Instant,
    /// Decrementa el contador GLOBAL al soltarse el listado (remove/evict/
    /// sweep/muerte de la conexión): contabilidad RAII, sin decrementos
    /// dispersos (M1 del rust-reviewer).
    _guard: ListingGuard,
}

/// Guard RAII del contador global de listados retenidos.
struct ListingGuard {
    global: Arc<AtomicUsize>,
}

impl Drop for ListingGuard {
    fn drop(&mut self) {
        self.global.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Estado POR-CONEXIÓN: el `initialized` del handshake más los listados
/// paginados retenidos. Local de [`serve_connection`] — se dropea en TODOS los
/// caminos de salida, así que los streams mueren con la conexión. El dispatch
/// es serial (se awaitea inline), sin concurrencia: basta `&mut`.
struct ConnState {
    initialized: bool,
    /// Actor bajo el que se journaliza y se evalúa la policy TODA mutación de
    /// esta conexión (M3-3b). `User` por defecto (frontend humano, sin
    /// sandbox); pasa a `Agent { session }` si el `initialize` trae
    /// `agent_session`. Lo fija SOLO el servidor en el handshake: un cliente
    /// jamás puede declararse `User` por otra vía.
    actor: crate::journal::Actor,
    listings: HashMap<u64, OpenListing>,
    next_listing_id: u64,
    /// `request_id`s de scope que ESTA conexión dejó pendientes (M3-3b): al
    /// morir la conexión se retiran del mapa global de `Shared` — una petición
    /// sin conceder no sobrevive a su peticionario (sin esto, un agente que
    /// pide y se va clava un slot para siempre). Acotado a
    /// [`MAX_PENDING_SCOPE_PER_CONN`]: una sesión no monopoliza el canal global.
    pending_scope_ids: Vec<u64>,
}

impl ConnState {
    fn new() -> Self {
        Self {
            initialized: false,
            actor: crate::journal::Actor::User,
            listings: HashMap::new(),
            next_listing_id: 0,
            pending_scope_ids: Vec::new(),
        }
    }

    /// Descarta los listados sin continuar en más de `ttl` (barrido perezoso).
    fn sweep_expired(&mut self, ttl: Duration) {
        let now = std::time::Instant::now();
        self.listings
            .retain(|_, l| now.duration_since(l.last_used) < ttl);
    }

    /// Expulsa el listado menos-recientemente-usado (LRU) si el mapa está en
    /// el tope: abrir el (N+1) no debe crecer sin límite.
    fn evict_if_full(&mut self) {
        if self.listings.len() < MAX_OPEN_LISTINGS {
            return;
        }
        if let Some((&victim, _)) = self.listings.iter().min_by_key(|(_, l)| l.last_used) {
            self.listings.remove(&victim);
        }
    }
}

/// Resultado de drenar una página de un `EntryStream`.
enum Drained {
    /// El stream tiene MÁS: se retiene para la página siguiente.
    More,
    /// El stream se agotó: no hay `next_cursor`.
    Done,
}

/// Drena hasta `cap` entradas (o todas si `cap` es `None`) a `out`. Un `Err`
/// del stream se propaga (el listado se descarta arriba).
async fn drain_page(
    stream: &mut norte_vfs::EntryStream,
    cap: Option<usize>,
    out: &mut Vec<norte_proto::Entry>,
) -> Result<Drained, norte_proto::Error> {
    loop {
        if let Some(c) = cap
            && out.len() >= c
        {
            return Ok(Drained::More);
        }
        match stream.next().await {
            Some(item) => out.push(item?),
            None => return Ok(Drained::Done),
        }
    }
}

/// El handler de `fs.list` con paginación por cursor (ADR 0017). Cláusula ADR
/// 0004: sin `cursor` NI `limit` drena el listado COMPLETO con `next_cursor:
/// null` (un cliente 0.7 recibe exactamente lo de antes).
#[tracing::instrument(level = "debug", skip_all, fields(path = %p.path.display_lossy(), paginado = p.cursor.is_some()))]
async fn handle_fs_list(
    p: methods::FsListParams,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // Barrido perezoso antes de tocar el mapa (además del periódico).
    conn.sweep_expired(shared.listing_ttl);

    // `limit == 0` sería una página vacía en bucle: error de params.
    if p.limit == Some(0) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "limit must be >= 1",
        ));
    }
    let cap = p.limit.map(|l| l.min(methods::FS_LIST_MAX_PAGE) as usize);
    let now = std::time::Instant::now();
    let mut entries = Vec::new();

    // Continuación: el cursor es el id opaco de un listado retenido.
    if let Some(cur) = &p.cursor {
        // Cursor no-numérico o desconocido = expirado (el cliente reinicia).
        let id: u64 = cur.parse().map_err(|_| cursor_expired())?;
        let drained = {
            let listing = conn.listings.get_mut(&id).ok_or_else(cursor_expired)?;
            if listing.path != p.path {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    "cursor does not belong to this path",
                ));
            }
            drain_page(&mut listing.stream, cap, &mut entries).await
        };
        return match drained {
            Ok(Drained::More) => {
                // Se conserva bajo el MISMO id (el cliente reusa el cursor).
                if let Some(l) = conn.listings.get_mut(&id) {
                    l.last_used = now;
                }
                Ok(to_value(&methods::FsListResult {
                    entries,
                    next_cursor: Some(id.to_string()),
                })?)
            }
            Ok(Drained::Done) => {
                conn.listings.remove(&id);
                to_value(&methods::FsListResult {
                    entries,
                    next_cursor: None,
                })
            }
            Err(e) => {
                conn.listings.remove(&id);
                Err(RpcError::from(e))
            }
        };
    }

    // Listado NUEVO (sin cursor).
    let mut stream = shared.engine.list(&p.path).await.map_err(RpcError::from)?;
    match drain_page(&mut stream, cap, &mut entries).await {
        Ok(Drained::Done) => to_value(&methods::FsListResult {
            entries,
            next_cursor: None,
        }),
        Ok(Drained::More) => {
            // Presión GLOBAL (M1): por encima del tope no se retiene — se drena
            // el resto EN LÍNEA y se devuelve completo (libera el hilo blocking
            // del productor al instante). Degrada a listado-completo, nunca
            // agota el pool ni trunca.
            if shared.open_listings.load(Ordering::SeqCst) >= GLOBAL_MAX_LISTINGS {
                match drain_page(&mut stream, None, &mut entries).await {
                    Ok(_) => {
                        return to_value(&methods::FsListResult {
                            entries,
                            next_cursor: None,
                        });
                    }
                    Err(e) => return Err(RpcError::from(e)),
                }
            }
            conn.evict_if_full();
            shared.open_listings.fetch_add(1, Ordering::SeqCst);
            let id = conn.next_listing_id;
            conn.next_listing_id += 1;
            conn.listings.insert(
                id,
                OpenListing {
                    path: p.path,
                    stream,
                    last_used: now,
                    _guard: ListingGuard {
                        global: Arc::clone(&shared.open_listings),
                    },
                },
            );
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
            })
        }
        Err(e) => Err(RpcError::from(e)),
    }
}

/// `RpcError` para un cursor de paginación inválido/expirado (ADR 0017): lleva
/// la taxonomía [`Error::CursorExpired`](norte_proto::Error::CursorExpired) en
/// `data`, así el cliente la distingue y reinicia el listado.
fn cursor_expired() -> RpcError {
    RpcError::from(norte_proto::Error::CursorExpired)
}

async fn serve_connection(stream: UnixStream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    // Toda escritura (respuestas Y broadcast) sale por un único canal
    // BOUNDED: jamás dos frames entrelazados y jamás memoria sin límite
    // por un cliente que no lee (M1 del security-reviewer).
    let (tx, mut rx) = mpsc::channel::<Arc<[u8]>>(OUTBOX_FRAMES);
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    let conn_id = shared.next_conn.fetch_add(1, Ordering::SeqCst) as u64;
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut conn = ConnState::new();
    let mut parse_errors = 0u32;
    // Reap de listados paginados expirados en una conexión viva-pero-muda
    // (además del barrido perezoso en cada fs.list): un peer que abre un
    // listado y no lo continúa no retiene el stream (ni su hilo blocking) más
    // allá del TTL.
    let mut sweep = tokio::time::interval(LISTING_SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result: std::io::Result<()> = 'conn: loop {
        let n = tokio::select! {
            r = reader.read(&mut buf) => match r {
                Ok(n) => n,
                // El camino de error TAMBIÉN pasa por la limpieza común
                // (M1 del rust-reviewer: sin esto, suscripción y writer
                // quedaban vivos tras un ECONNRESET).
                Err(e) => break 'conn Err(e),
            },
            () = shared.shutdown.cancelled() => break 'conn Ok(()),
            _ = sweep.tick() => {
                conn.sweep_expired(shared.listing_ttl);
                continue;
            }
        };
        if n == 0 {
            break Ok(());
        }
        if decoder.push(&buf[..n]).is_err() {
            let resp = Response::err(
                None,
                RpcError::protocol(codes::PARSE_ERROR, "frame too large"),
            );
            send(&tx, &resp);
            break Ok(());
        }
        while let Some(frame) = decoder.next_frame() {
            // JSON roto = -32700; JSON válido que no es envelope = -32600
            // (M2 del protocol-guardian).
            let value: serde_json::Value = match serde_json::from_slice(&frame) {
                Ok(v) => v,
                Err(e) => {
                    parse_errors += 1;
                    if parse_errors > MAX_PARSE_ERRORS {
                        break 'conn Ok(());
                    }
                    let resp = Response::err(
                        None,
                        RpcError::protocol(codes::PARSE_ERROR, format!("invalid JSON: {e}")),
                    );
                    send(&tx, &resp);
                    continue;
                }
            };
            parse_errors = 0;
            handle_value(value, conn_id, &tx, &mut conn, shared).await;
        }
    };

    // Limpieza común de TODOS los caminos de salida. Retirar NUESTRA
    // suscripción antes de esperar al writer: es el otro dueño del sender —
    // sin esto, deadlock (el writer drena hasta que TODOS mueren).
    shared
        .subscribers
        .lock()
        .expect("subscribers lock sano")
        .remove(&conn_id);
    // Las peticiones de scope que ESTA conexión dejó pendientes mueren con
    // ella: una petición sin conceder no debe sobrevivir a su peticionario
    // (anti-fuga del canal global, M3-3b). `remove` de un id ya concedido es
    // un no-op benigno.
    if !conn.pending_scope_ids.is_empty() {
        let mut pending = shared
            .pending_scope
            .lock()
            .expect("pending_scope lock sano");
        for id in &conn.pending_scope_ids {
            pending.remove(id);
        }
    }
    drop(tx);
    let _ = writer_task.await;
    result
}

/// Maneja UN mensaje JSON ya parseado de la conexión. La clasificación es
/// estructural para que un id de tipo ilegal NUNCA muera en silencio como
/// notification (M3 del guardian).
async fn handle_value(
    value: serde_json::Value,
    conn_id: u64,
    tx: &mpsc::Sender<Arc<[u8]>>,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
) {
    match classify(&value) {
        MessageKind::Request => {
            let req: Request = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response::err(
                        None,
                        RpcError::protocol(
                            codes::INVALID_REQUEST,
                            format!("invalid request envelope: {e}"),
                        ),
                    );
                    send(tx, &resp);
                    return;
                }
            };
            let id = req.id.clone();
            let was_initialized = conn.initialized;
            // El dispatch respeta el shutdown (regla 3): un fs.list
            // gigante no retiene el apagado.
            let response = tokio::select! {
                r = dispatch(req, conn, shared) => r,
                () = shared.shutdown.cancelled() => {
                    Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    ))
                }
            };
            // La suscripción al broadcast nace CON el handshake (nota del
            // security-reviewer): antes de initialize nadie recibe el
            // progreso de otros.
            if !was_initialized && conn.initialized {
                shared
                    .subscribers
                    .lock()
                    .expect("subscribers lock sano")
                    .insert(
                        conn_id,
                        Subscriber {
                            tx: tx.clone(),
                            // El actor quedó fijado server-side por ESTE initialize.
                            is_agent: !matches!(conn.actor, crate::journal::Actor::User),
                        },
                    );
            }
            send(tx, &Response::from_outcome(id, response));
        }
        // Notificaciones del cliente (ninguna definida aún; JSON-RPC
        // prohíbe responderlas) y responses espurias: se ignoran.
        MessageKind::Notification | MessageKind::Response => {}
        MessageKind::Invalid => {
            let resp = Response::err(
                None,
                RpcError::protocol(codes::INVALID_REQUEST, "not a JSON-RPC message"),
            );
            send(tx, &resp);
        }
    }
}

/// Encola un frame en la outbox de la conexión. `try_send`: si el cliente
/// no drena (outbox llena), el frame se pierde y la conexión morirá en su
/// siguiente lectura — jamás acumulación sin límite.
fn send<T: serde::Serialize>(tx: &mpsc::Sender<Arc<[u8]>>, msg: &T) {
    if let Ok(frame) = encode_frame(msg) {
        let _ = tx.try_send(Arc::from(frame.into_boxed_slice()));
    }
}

/// Azúcar: construye la Response de un dispatch.
trait FromOutcome {
    fn from_outcome(id: RequestId, out: Result<serde_json::Value, RpcError>) -> Response;
}

impl FromOutcome for Response {
    fn from_outcome(id: RequestId, out: Result<serde_json::Value, RpcError>) -> Response {
        match out {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(Some(id), e),
        }
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, RpcError> {
    serde_json::from_value(params.unwrap_or(serde_json::Value::Null))
        .map_err(|e| RpcError::protocol(codes::INVALID_PARAMS, format!("invalid params: {e}")))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(v)
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("serialization: {e}")))
}

#[tracing::instrument(skip_all, fields(method = %req.method))]
async fn dispatch(
    req: Request,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !conn.initialized && req.method != methods::INITIALIZE {
        return Err(RpcError::protocol(
            codes::NOT_INITIALIZED,
            "initialize required first",
        ));
    }
    match req.method.as_str() {
        methods::INITIALIZE => {
            // Repetirlo es un error de protocolo (como LSP): renegociar a
            // mitad de sesión no significa nada.
            if conn.initialized {
                return Err(RpcError::protocol(
                    codes::INVALID_REQUEST,
                    "already initialized",
                ));
            }
            let p: methods::InitializeParams = parse_params(req.params)?;
            if !methods::version_compatible(methods::PROTOCOL_VERSION, &p.protocol_version) {
                // Código PROPIO (B1 del protocol-guardian): la señal de
                // upgrade se distingue por código, jamás por message.
                return Err(RpcError::protocol(
                    codes::VERSION_MISMATCH,
                    format!(
                        "incompatible protocol version: server {}, client {} (N and N-1 accepted)",
                        methods::PROTOCOL_VERSION,
                        p.protocol_version
                    ),
                ));
            }
            if !p.encodings.is_empty() && !p.encodings.iter().any(|e| e == "json") {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    "only supported encoding: json",
                ));
            }
            // El servidor liga el actor a la conexión (deuda M3 de 3a): una
            // conexión con `agent_session` ES una sesión de agente y se
            // sandboxea; sin él es `User` (humano). No es declarable al revés.
            // El id se VALIDA fail-closed (encoding-auditor H1 de M3-3b): va a
            // journal, logs y UIs de aprobación — jamás un vector de inyección
            // de controles/bidi elegido por el agente.
            if let Some(session) = p.agent_session {
                if !valid_agent_session(&session) {
                    return Err(RpcError::protocol(
                        codes::INVALID_PARAMS,
                        "agent_session must be 1..=64 chars of [A-Za-z0-9._-]",
                    ));
                }
                conn.actor = crate::journal::Actor::Agent { session };
            }
            conn.initialized = true;
            to_value(&methods::InitializeResult {
                server_info: methods::ServerInfo {
                    name: "norte-core".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
            })
        }
        methods::DAEMON_SHUTDOWN => {
            // El emisor canónico escribe `params: null` (ADR 0004) y este
            // método es todo-opcionales: null y ausencia = defaults (M1
            // del protocol-guardian; el golden request_null_params lo pinnea).
            let p: methods::DaemonShutdownParams = parse_params(
                req.params
                    .filter(|v| !v.is_null())
                    .or_else(|| Some(serde_json::json!({}))),
            )?;
            if !p.graceful {
                shared.hard_shutdown.cancel();
            }
            shared.shutdown.cancel();
            to_value(&methods::DaemonShutdownResult {})
        }
        // fs.list vive AQUÍ (no en dispatch_fs_task): necesita el ConnState
        // para retener el stream paginado entre páginas (ADR 0017).
        methods::FS_LIST => {
            let p: methods::FsListParams = parse_params(req.params)?;
            handle_fs_list(p, conn, shared).await
        }
        // policy.* con round-trip humano (M3-3b): request/grant de scope. Viven
        // AQUÍ porque atan la operación al ACTOR de la conexión (server-side).
        methods::POLICY_REQUEST_SCOPE => {
            let p: methods::RequestScopeParams = parse_params(req.params)?;
            handle_request_scope(conn, p, shared)
        }
        methods::POLICY_GRANT_SCOPE => {
            let p: methods::GrantScopeParams = parse_params(req.params)?;
            handle_grant_scope(&conn.actor, &p, shared)
        }
        methods::POLICY_DECIDE => {
            let p: methods::PolicyDecideParams = parse_params(req.params)?;
            handle_policy_decide(&conn.actor, &p, shared)
        }
        methods::POLICY_PENDING => {
            // Sin params definidos: null/ausencia se aceptan (ADR 0004) y
            // cualquier objeto se IGNORA deliberadamente — añadir params en
            // una versión futura (filtros) no debe romper servers viejos.
            handle_policy_pending(&conn.actor, shared)
        }
        methods::POLICY_UNDO_SESSION => {
            let p: methods::PolicyUndoSessionParams = parse_params(req.params)?;
            handle_policy_undo_session(&conn.actor, p, shared).await
        }
        _ => dispatch_fs_task(req, conn.actor.clone(), shared).await,
    }
}

/// `policy.request_scope` (M3-3b): un AGENTE pide un scope para SÍ mismo. La
/// petición queda pendiente; no concede nada hasta que un humano la conceda
/// con `policy.grant_scope`. Devuelve el `request_id`. La pendiente se retira
/// del mapa global al morir la conexión que la creó (no sobrevive a su dueño).
#[tracing::instrument(skip_all, fields(ops = ?p.ops, ttl_ms = p.ttl_ms))]
fn handle_request_scope(
    conn: &mut ConnState,
    p: methods::RequestScopeParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // Solo un agente pide scope, y SOLO para su propia sesión: la identidad la
    // fija la conexión (T2), jamás el cuerpo del mensaje. Un `User` no necesita
    // scope (no se sandboxea), así que pedirlo es un error de protocolo.
    let Actor::Agent { session } = &conn.actor else {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only an agent session may request a scope",
        ));
    };
    if p.session != *session {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "session must match the connection's agent session",
        ));
    }
    // Sub-cap por conexión: una sola sesión no agota el canal global de las
    // demás (una pendiente sin conceder ocupa slot hasta grant o desconexión).
    if conn.pending_scope_ids.len() >= MAX_PENDING_SCOPE_PER_CONN {
        return Err(RpcError::protocol(
            codes::OVERLOADED,
            "too many pending scope requests on this connection",
        ));
    }
    let session = session.clone();
    let mut pending = shared
        .pending_scope
        .lock()
        .expect("pending_scope lock sano");
    if pending.len() >= MAX_PENDING_SCOPE {
        return Err(RpcError::protocol(
            codes::OVERLOADED,
            "too many pending scope requests",
        ));
    }
    let request_id = shared.next_scope_req.fetch_add(1, Ordering::SeqCst);
    pending.insert(
        request_id,
        PendingScope {
            session,
            roots: p.roots,
            ops: p.ops,
            ttl_ms: p.ttl_ms,
        },
    );
    drop(pending);
    conn.pending_scope_ids.push(request_id);
    to_value(&methods::RequestScopeResult { request_id })
}

/// `policy.grant_scope` (M3-3b): un humano concede una petición pendiente.
///
/// "Humano" = cualquier conexión que NO declaró `agent_session` (actor `User`).
/// Bajo el threat model UDS same-uid (§14) esto no es una barrera fuerte: un
/// proceso del mismo uid puede abrir una 2ª conexión sin `agent_session` y
/// autoconcederse scope — pero esa conexión `User` YA puede ejecutar las
/// mutaciones directamente (`User` = allow-all), así que el grant no otorga
/// poder extra. La policy es un guardarraíl para agentes que COOPERAN (vía
/// norte-mcp, M3-4), no un sandbox. Materializa el `Scope` con su TTL y abre la
/// frontera que el gate del engine consulta (mismo `ScopeRegistry`).
#[tracing::instrument(skip_all, fields(request_id = p.request_id))]
fn handle_grant_scope(
    actor: &Actor,
    p: &methods::GrantScopeParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may grant a scope",
        ));
    }
    // Consume la petición (una concesión por id; re-conceder es INVALID_PARAMS).
    let req = shared
        .pending_scope
        .lock()
        .expect("pending_scope lock sano")
        .remove(&p.request_id);
    let Some(req) = req else {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown or already-granted request_id",
        ));
    };
    let scope = Scope {
        roots: req.roots,
        ops: OpSet::from_names(&req.ops),
        expires_at: Some(scope_deadline(req.ttl_ms)),
    };
    // Efecto de seguridad (futuro material de auditoría M3-5): traza el grant
    // con la sesión y las ops, jamás las rutas crudas (regla 10).
    tracing::info!(session = %req.session, ops = ?req.ops, "scope concedido a la sesión de agente");
    shared.scopes.grant(&req.session, scope);
    to_value(&methods::GrantScopeResult {})
}

/// `policy.decide` (M3-3b Task 4): un humano aprueba/deniega una pendiente.
///
/// Solo una conexión NO-agente decide (mismo criterio y mismo threat model que
/// [`handle_grant_scope`]): un agente jamás aprueba su propia op — la
/// suspensión del `Ask` sería teatro. Una decisión consume el id; repetirlo (o
/// un id vencido/desconocido) es `INVALID_PARAMS`.
#[tracing::instrument(skip_all, fields(approval_id = p.approval_id, approve = p.approve))]
fn handle_policy_decide(
    actor: &Actor,
    p: &methods::PolicyDecideParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may decide an approval",
        ));
    }
    if !shared.approvals.decide(p.approval_id, p.approve) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown, expired or already-decided approval_id",
        ));
    }
    // Efecto de seguridad (material de auditoría M3-5): quién decidió qué.
    tracing::info!("aprobación de policy decidida por el humano");
    to_value(&methods::PolicyDecideResult {})
}

/// `policy.pending` (M3-3b Task 4): resync de aprobaciones pendientes para un
/// frontend que conecta DESPUÉS del broadcast. Solo humanos: la lista cruza
/// sesiones (rutas de otras) y un agente no decide, así que tampoco lista.
fn handle_policy_pending(
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may list pending approvals",
        ));
    }
    to_value(&methods::PolicyPendingResult {
        pending: shared.approvals.pending(),
    })
}

/// `policy.undo_session` (M3-4): un HUMANO deshace la sesión completa de un
/// agente. Target = `Agent{session}` (selecciona las entradas del journal),
/// ejecutor = `User` (pasa el gate y firma las compensaciones): el undo no
/// depende de que el scope del agente siga vivo. Solo conexiones User — un
/// agente no deshace a otros (su propia sesión, como tool = deuda ADR 0024).
///
/// El span NO registra la session cruda del wire (MINOR-2 del security):
/// hasta pasar `valid_agent_session` puede llevar `\n`/ANSI y fabricar líneas
/// de log falsas — justo en material de auditoría M3-5. Se loguea VALIDADA.
#[tracing::instrument(skip_all)]
async fn handle_policy_undo_session(
    actor: &Actor,
    p: methods::PolicyUndoSessionParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may undo an agent session",
        ));
    }
    if !valid_agent_session(&p.session) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "session must be 1..=64 chars of [A-Za-z0-9._-]",
        ));
    }
    let target = Actor::Agent { session: p.session };
    let (handle, _report) = shared
        .engine
        .undo_session_for(&target, Actor::User)
        .await
        .map_err(RpcError::from)?;
    // Efecto de seguridad (material de auditoría M3-5): quién deshizo a quién.
    // La session ya pasó la validación de charset — segura de loguear.
    if let Actor::Agent { session } = &target {
        tracing::info!(session = %session, "undo de sesión de agente pedido por el humano");
    }
    let task_id = register_task_id(shared, handle)?;
    to_value(&methods::PolicyUndoSessionResult { task_id })
}

/// Instante de expiración de un scope a partir de su `ttl_ms`: clamp a
/// `[1, MAX_SCOPE_TTL_MS]` (ni 0 = ya-expirado inútil, ni cuasi-perpetuo) y
/// suma saturante (jamás panica por overflow del reloj).
fn scope_deadline(ttl_ms: u64) -> Instant {
    let ms = ttl_ms.clamp(1, MAX_SCOPE_TTL_MS);
    let now = Instant::now();
    now.checked_add(Duration::from_millis(ms)).unwrap_or(now)
}

/// Las familias `fs.*`/`task.*` del dispatch (separadas por tamaño). El
/// `actor` viene de la conexión (M3-3b): las mutaciones se journalizan y
/// evalúan bajo él.
async fn dispatch_fs_task(
    req: Request,
    actor: crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::FS_STAT => {
            let p: methods::FsStatParams = parse_params(req.params)?;
            let entry = shared.engine.stat(&p.path).await.map_err(RpcError::from)?;
            to_value(&methods::FsStatResult { entry })
        }
        methods::FS_COPY => {
            let p: methods::FsCopyParams = parse_params(req.params)?;
            let opts = TransferOptions {
                on_collision: p.on_collision,
                symlinks: p.symlinks,
                resume: p.resume,
                verify: p.verify,
            };
            let handle = shared
                .engine
                .copy_with_as(&p.from, &p.to, opts, actor)
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle)
        }
        methods::FS_MOVE => {
            let p: methods::FsMoveParams = parse_params(req.params)?;
            let opts = TransferOptions {
                on_collision: p.on_collision,
                symlinks: p.symlinks,
                resume: p.resume,
                verify: p.verify,
            };
            let handle = shared
                .engine
                .move_with_as(&p.from, &p.to, opts, actor)
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle)
        }
        methods::FS_DELETE => {
            let p: methods::FsDeleteParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .delete_with_as(&p.path, p.mode, actor)
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle)
        }
        _ => dispatch_task_family(req, shared).await,
    }
}

/// El resto del dispatch: `task.*` y los métodos de solo-lectura de 0.5.0.
async fn dispatch_task_family(
    req: Request,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::TASK_LIST => {
            let _p: methods::TaskListParams = parse_params(
                req.params
                    .filter(|v| !v.is_null())
                    .or_else(|| Some(serde_json::json!({}))),
            )?;
            // Resync de un frontend que (re)conecta: tasks VIVAS + los
            // desenlaces recientes (por si su terminal se emitió mientras
            // estaba fuera); lo posterior llega por task.progress.
            let mut tasks: Vec<norte_proto::TaskProgress> = shared
                .recent
                .lock()
                .expect("recent lock sano")
                .iter()
                .cloned()
                .collect();
            tasks.extend(
                shared
                    .tasks
                    .lock()
                    .expect("tasks lock sano")
                    .values()
                    .map(|h| h.progress().borrow().clone()),
            );
            to_value(&methods::TaskListResult { tasks })
        }
        methods::FS_READ => {
            let p: methods::FsReadParams = parse_params(req.params)?;
            dispatch_fs_read(p, shared).await
        }
        methods::FS_CAPABILITIES => {
            let p: methods::FsCapabilitiesParams = parse_params(req.params)?;
            let capabilities = shared
                .engine
                .capabilities(&p.path)
                .await
                .map_err(RpcError::from)?;
            to_value(&methods::FsCapabilitiesResult { capabilities })
        }
        methods::TASK_CANCEL => {
            let p: methods::TaskCancelParams = parse_params(req.params)?;
            // Cancelar algo terminal o desconocido NO es error (contrato
            // del método): la respuesta solo confirma la recepción.
            if let Some(handle) = shared
                .tasks
                .lock()
                .expect("tasks lock sano")
                .get(&p.task_id.get())
            {
                handle.cancel();
            }
            to_value(&methods::TaskCancelResult {})
        }
        methods::CONNECTION_TRUST_HOST_KEY => {
            let p: methods::ConnectionTrustHostKeyParams = parse_params(req.params)?;
            // El engine delega en el conector, que RE-VERIFICA el fingerprint
            // contra la clave que el host presenta ahora (anti-TOCTOU, ADR
            // 0015 D) antes de registrar nada. `algo` es informativo: la
            // identidad que se confirma es el fingerprint.
            shared
                .engine
                .trust_host_key(&p.host, p.port, &p.fingerprint)
                .await
                .map_err(RpcError::from)?;
            to_value(&methods::ConnectionTrustHostKeyResult { trusted: true })
        }
        other => Err(RpcError::protocol(
            codes::METHOD_NOT_FOUND,
            format!("unknown method: {other}"),
        )),
    }
}

/// `fs.read` (0.5.0): UN tramo en base64, con tope por llamada. Se lee
/// UN byte de más para saber si el archivo sigue (`eof` honesto sin un
/// stat extra ni confiar en el tamaño, que puede cambiar bajo los pies).
async fn dispatch_fs_read(
    p: methods::FsReadParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    use base64::Engine as _;
    let offset = p.range.as_ref().map_or(0, |r| r.offset);
    let want = p
        .range
        .as_ref()
        .and_then(|r| r.len)
        .unwrap_or(methods::FS_READ_MAX_CHUNK)
        .min(methods::FS_READ_MAX_CHUNK);
    let probe_range = norte_proto::ByteRange {
        offset,
        len: Some(want.saturating_add(1)),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(probe_range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > want {
            break;
        }
    }
    let eof = bytes.len() as u64 <= want;
    bytes.truncate(usize::try_from(want).unwrap_or(usize::MAX).min(bytes.len()));
    to_value(&methods::FsReadResult {
        content_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        eof,
    })
}

/// Registra la task y arranca su bomba de progreso; devuelve el resultado de
/// wire estándar `FsTaskResult`. Ver [`register_task_id`].
fn register_task(shared: &Arc<Shared>, handle: TaskHandle) -> Result<serde_json::Value, RpcError> {
    let task_id = register_task_id(shared, handle)?;
    to_value(&methods::FsTaskResult { task_id })
}

/// Registra la task y arranca su bomba de progreso: cada snapshot (≤30 Hz)
/// sale como `task.progress` a TODOS los clientes; el estado terminal se
/// difunde SIEMPRE y desregistra la task. Devuelve el `TaskId` (los métodos
/// con result propio lo envuelven ellos, M3-4).
fn register_task_id(shared: &Arc<Shared>, handle: TaskHandle) -> Result<TaskId, RpcError> {
    let task_id: TaskId = handle.id();
    let mut progress = handle.progress();
    {
        let mut tasks = shared.tasks.lock().expect("tasks lock sano");
        // Tope anti-agotamiento (M3 del security-reviewer). La task YA
        // está encolada en el scheduler: se cancela cooperativamente antes
        // de rechazar — jamás una task fantasma sin registrar.
        if tasks.len() >= MAX_LIVE_TASKS {
            handle.cancel();
            return Err(RpcError::protocol(
                codes::OVERLOADED,
                format!("too many live tasks (max {MAX_LIVE_TASKS}); retry later"),
            ));
        }
        tasks.insert(task_id.get(), handle);
    }

    let shared_pump = Arc::clone(shared);
    tokio::spawn(async move {
        loop {
            let snapshot = progress.borrow_and_update().clone();
            let terminal = snapshot.state.is_terminal();
            // Un terminal se recuerda en `recent` ANTES de difundirlo: así
            // un task.list concurrente (o una suscripción que nace tras el
            // broadcast) lo ve por uno de los dos caminos, jamás por
            // ninguno (M1 del rust-reviewer).
            if terminal {
                let mut recent = shared_pump.recent.lock().expect("recent lock sano");
                recent.push_back(snapshot.clone());
                while recent.len() > RECENT_TERMINAL {
                    recent.pop_front();
                }
            }
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::TASK_PROGRESS.into(),
                params: serde_json::to_value(&snapshot).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                shared_pump.broadcast(&Arc::from(frame.into_boxed_slice()));
            }
            if terminal {
                break;
            }
            // Coalescido: como mucho un frame cada PROGRESS_MIN_INTERVAL.
            tokio::time::sleep(PROGRESS_MIN_INTERVAL).await;
            if progress.changed().await.is_err() {
                // El scheduler soltó el emisor: difunde el último estado.
                let last = progress.borrow().clone();
                if last.state.is_terminal() {
                    let mut recent = shared_pump.recent.lock().expect("recent lock sano");
                    recent.push_back(last.clone());
                    while recent.len() > RECENT_TERMINAL {
                        recent.pop_front();
                    }
                }
                let notif = Notification {
                    jsonrpc: norte_proto::wire::JsonRpcVersion,
                    method: methods::TASK_PROGRESS.into(),
                    params: serde_json::to_value(&last).ok(),
                };
                if let Ok(frame) = encode_frame(&notif) {
                    shared_pump.broadcast(&Arc::from(frame.into_boxed_slice()));
                }
                break;
            }
        }
        // El desenlace ya está en `recent` (arriba, antes del broadcast).
        // Solo queda sacar la task del mapa de vivas.
        shared_pump
            .tasks
            .lock()
            .expect("tasks lock sano")
            .remove(&task_id.get());
    });

    Ok(task_id)
}

#[cfg(test)]
mod tests {
    use super::peer_allowed;

    /// La política de admisión es EXACTAMENTE mismo-uid: ni root entra.
    #[test]
    fn peer_allowed_solo_mismo_uid() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(0, 1000), "root NO es el usuario del daemon");
    }
}
