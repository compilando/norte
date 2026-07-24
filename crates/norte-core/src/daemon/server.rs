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
/// Sub-tope de tasks vivas de AGENTES — todas las sesiones juntas (#70): un
/// agente glotón no agota el cupo global; el humano conserva SIEMPRE
/// headroom (`MAX_LIVE_TASKS - MAX_LIVE_TASKS_AGENTS`). Por CLASE, no por
/// sesión: aislar agente-de-agente es la deuda m4 (policy.rs).
const MAX_LIVE_TASKS_AGENTS: usize = 384;
/// Frames parseados EN COLA entre el reader y el dispatch de una conexión
/// (#64). Pequeño a propósito: el dispatch es serial y los clientes son
/// request/response — el valor del inbox es que el READER siga vivo durante
/// un dispatch suspendido (Ask) para observar la muerte del peer.
const INBOX_FRAMES: usize = 16;
/// Tope de frames DIFERIDOS durante un dispatch en vuelo (#72): frames que
/// llegan mientras una request se procesa (típicamente un Ask suspendido) y no
/// son su `rpc.cancel` se bufferizan hasta este tope. Alcanzado, el brazo de
/// lectura del inner-select se deshabilita y el reader vuelve a hacer
/// backpressure sobre el socket (como antes de #72): la memoria total queda
/// acotada a `INBOX_FRAMES + MAX_DEFERRED_FRAMES` en vez de crecer sin límite
/// mientras el peer hace pipeline durante su propio Ask (MAJOR del
/// security-reviewer). Un cliente request/response normal jamás lo roza (0-1
/// diferidos); solo un peer semi-hostil bajo un Ask no-aprobado lo alcanza.
const MAX_DEFERRED_FRAMES: usize = 16;
/// Snapshots TERMINALES retenidos para el resync de `task.list` (un
/// frontend que reconecta ve el desenlace de lo que se perdió).
const RECENT_TERMINAL: usize = 64;
/// Sub-tope de slots de `recent` para terminales de AGENTES (#70): una
/// ráfaga de tasks triviales de agente expulsa las suyas más viejas, jamás
/// los desenlaces del humano (que conserva ≥ `RECENT_TERMINAL -
/// RECENT_TERMINAL_AGENTS` slots).
const RECENT_TERMINAL_AGENTS: usize = 32;
/// Informes de undo retenidos para `policy.undo_report` (#71). Los undos son
/// operaciones humanas raras: un anillo corto basta; el más viejo se expulsa.
const UNDO_REPORTS_MAX: usize = 8;
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
///
/// INVARIANTE de tuning (#53 M1): este tope debe quedar ≤ la mitad del pool
/// blocking del runtime (default de tokio: 512) — cada listing retenido de
/// vfs-local puede parkear UN hilo del pool en `blocking_send`, y el resto
/// del daemon (sqlx, plugins, fs local) necesita su margen. Si algún día se
/// sube, o el binario fija `max_blocking_threads`, revisar juntos.
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
    /// Raíz de config donde vive el catálogo de plugins
    /// (`<dir>/plugins/<id>/plugin.toml`) y su estado (`<dir>/plugins-state.toml`),
    /// M4-P3. `None` = [`crate::connect::config_dir`] real (la capa de usuario);
    /// `Some(dir)` = ese directorio — para tests, SIEMPRE un tempdir, jamás el
    /// `~/.config` real.
    pub plugins_dir: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            idle_timeout: Some(Duration::from_mins(5)),
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
        }
    }
}

/// Estado compartido entre conexiones.
struct Shared {
    engine: Arc<Engine>,
    /// Tasks vivas encoladas POR el daemon (para `task.cancel` y graceful),
    /// etiquetadas con el actor que las encoló: el gate de `task.list`/
    /// `task.cancel` para conexiones de agente decide con el dueño (#66).
    tasks: Mutex<HashMap<u64, RegisteredTask>>,
    /// Salidas de notificación de cada cliente YA inicializado, por id de
    /// conexión (la conexión retira la SUYA al morir — sin esto el writer
    /// task jamás terminaría: el broadcast retendría su sender). Bounded:
    /// un suscriptor que no drena pierde la suscripción, jamás acumula.
    subscribers: Mutex<HashMap<u64, Subscriber>>,
    /// Desenlaces recientes (snapshots terminales) para `task.list`, con el
    /// actor dueño: el resync de un agente tampoco ve terminales ajenos (#66).
    recent: Mutex<std::collections::VecDeque<(norte_proto::TaskProgress, Actor)>>,
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
    /// Registro de plugins descubiertos + estado aprobado/activado (M4-P3).
    /// El descubrimiento es I/O síncrono hecho UNA vez en el bind (dentro del
    /// `spawn_blocking`, regla 2); las mutaciones (`plugin.set_approval`/
    /// `set_enabled`) re-leen y persisten `plugins-state.toml` bajo el lock. El
    /// `Mutex` std basta: el dispatch es serial y las secciones son cortas.
    plugins: Mutex<crate::plugins::PluginRegistry>,
    /// Informes de las últimas Tasks de undo (#71), por `task_id`: el humano
    /// que deshizo consulta QUÉ pasó (`policy.undo_report`) — sin esto, un
    /// undo que saltó/bloqueó todo parece «done». Anillo acotado a
    /// [`UNDO_REPORTS_MAX`] (los undos son raros; mejor esfuerzo, como
    /// `recent`). El `Arc` interior es EL MISMO que llena la Task en vuelo:
    /// el informe se puede consultar en progreso (snapshot parcial).
    undo_reports: Mutex<std::collections::VecDeque<(u64, Arc<Mutex<crate::UndoReport>>)>>,
    /// Runtime WASM compartido para ejecutar comandos de plugin (M4-P4). Se
    /// construye UNA vez en el bind (arranca un hilo "ticker" de época) y se
    /// reutiliza entre `plugin.run_command`. `Arc` porque `PluginRuntime` es
    /// `Send+Sync` pero NO `Clone` (posee el `JoinHandle` del ticker): el
    /// handler clona el `Arc` y ejecuta la instanciación+ejecución (pesada,
    /// síncrona) en un `spawn_blocking`, jamás en el reactor con un lock tomado
    /// (regla 2).
    plugin_runtime: Arc<norte_plugin_host::PluginRuntime>,
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

/// Una salida de broadcast: el sender de la outbox y el ACTOR de la conexión
/// (M3-3b Task 4, security MAJOR-1): las notifs `policy.*` cruzan sesiones
/// (rutas y ops de otras) y solo van a humanos — el mismo criterio que el
/// gate de `policy.pending`. El `task.progress` se enruta por dueño (#66):
/// humanos siempre, un agente solo el de sus propias tasks.
struct Subscriber {
    tx: mpsc::Sender<Arc<[u8]>>,
    /// Actor de la conexión (fijado server-side por SU `initialize`): decide
    /// qué broadcasts recibe — `policy.*` solo humanos; el `task.progress` de
    /// una task ajena jamás llega a un agente (#66, mismo leak que
    /// `task.list`: `current` lleva paths de otros actores).
    actor: Actor,
}

/// Una task viva registrada en el daemon: el handle + QUIÉN la encoló. El
/// dueño gobierna visibilidad (`task.list`, broadcast de progreso) y
/// cancelabilidad (`task.cancel`) frente a conexiones de agente (#66).
struct RegisteredTask {
    handle: TaskHandle,
    owner: Actor,
}

/// Empuja un terminal al anillo `recent` respetando los topes por clase
/// (#70): un terminal de agente expulsa antes al MÁS VIEJO de su clase si
/// los agentes ya ocupan [`RECENT_TERMINAL_AGENTS`] slots; el tope global
/// [`RECENT_TERMINAL`] solo puede comerse entradas del humano cuando es el
/// propio humano quien desborda (los agentes nunca pasan de su sub-tope).
fn push_recent(
    recent: &mut std::collections::VecDeque<(norte_proto::TaskProgress, Actor)>,
    snapshot: norte_proto::TaskProgress,
    owner: &Actor,
) {
    if !matches!(owner, Actor::User) {
        let agents = recent
            .iter()
            .filter(|(_, o)| !matches!(o, Actor::User))
            .count();
        if agents >= RECENT_TERMINAL_AGENTS
            && let Some(pos) = recent.iter().position(|(_, o)| !matches!(o, Actor::User))
        {
            recent.remove(pos);
        }
    }
    recent.push_back((snapshot, owner.clone()));
    while recent.len() > RECENT_TERMINAL {
        recent.pop_front();
    }
}

/// EL criterio de visibilidad/alcance sobre tasks (#66), único para
/// `task.list`, `task.cancel` y el broadcast de progreso: un humano observa
/// todo; cualquier otro actor, solo lo suyo (igualdad de actor — dos
/// conexiones de la misma sesión de agente comparten vista, coherente con
/// `ScopeRegistry`). Un futuro dueño `Plugin` queda fail-closed: solo lo ve
/// el humano.
fn may_observe(viewer: &Actor, owner: &Actor) -> bool {
    matches!(viewer, Actor::User) || viewer == owner
}

/// Observer de avisos de conexión del daemon (#44): codifica cada aviso como
/// `connection.degraded` y lo difunde SOLO a humanos (como `policy.*`). `Weak`
/// rompe el ciclo Shared→engine→observer→Shared.
struct DaemonConnectionObserver {
    shared: std::sync::Weak<Shared>,
}

impl crate::connect::ConnectionObserver for DaemonConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let notif = norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        };
        // Si la serialización fallara (no puede: struct plano), mejor NO emitir
        // que emitir una notif con shape corrupto.
        let Ok(params) = serde_json::to_value(&notif) else {
            return;
        };
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::CONNECTION_DEGRADED.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            // Solo humanos: la sesión degradada es info de seguridad para el
            // usuario, no para el agente (mismo criterio que policy.*).
            shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
        }
    }
}

impl Shared {
    fn idle(&self) -> bool {
        self.connections.load(Ordering::SeqCst) == 0
            && self.tasks.lock().expect("tasks lock sano").is_empty()
    }

    /// Difunde SOLO a conexiones humanas (no-agente): notifs `policy.*`.
    fn broadcast_humans(&self, frame: &Arc<[u8]>) {
        self.broadcast_where(frame, |s| matches!(s.actor, Actor::User));
    }

    /// Difunde el progreso de UNA task: humanos siempre; una conexión de
    /// agente solo si la task es SUYA (mismo actor). Mismo criterio que el
    /// filtro de `task.list` (#66).
    fn broadcast_task_progress(&self, frame: &Arc<[u8]>, owner: &Actor) {
        self.broadcast_where(frame, |s| may_observe(&s.actor, owner));
    }

    /// Envía un frame a UNA conexión concreta (los hits de una `fs.search` son
    /// del que la lanzó — jamás broadcast, security T4). No-op si la conexión
    /// murió o ya no está suscrita; mismo criterio de expulsión que
    /// [`Self::broadcast_where`]: un dueño que no drena su outbox (llena) pierde
    /// la suscripción y morirá en su próximo response — el backlog nunca crece
    /// sin límite.
    fn send_to_conn(&self, conn_id: u64, frame: &Arc<[u8]>) {
        send_to_conn_impl(&self.subscribers, conn_id, frame);
    }

    fn broadcast_where(&self, frame: &Arc<[u8]>, wants: impl Fn(&Subscriber) -> bool) {
        let mut subs = self.subscribers.lock().expect("subscribers lock sano");
        // try_send: el que tiene la outbox llena pierde la suscripción (y
        // pronto la conexión, cuando su próximo response tampoco quepa) —
        // el backlog de un cliente lento jamás crece sin límite.
        subs.retain(|conn, s| {
            // Un suscriptor excluido de ESTA notif conserva su suscripción.
            if !wants(s) {
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

/// Núcleo testeable de [`Shared::send_to_conn`]: envía `frame` a la conexión
/// `conn_id` de `subs` (si existe) y RETIRA la entrada si su receptor murió o
/// no drena (outbox llena) — mismo criterio de expulsión que el broadcast.
fn send_to_conn_impl(subs: &Mutex<HashMap<u64, Subscriber>>, conn_id: u64, frame: &Arc<[u8]>) {
    let mut subs = subs.lock().expect("subscribers lock sano");
    let remove = match subs.get(&conn_id) {
        None => return,
        Some(s) => match s.tx.try_send(Arc::clone(frame)) {
            Ok(()) => false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(conn = conn_id, "dueño de búsqueda sin drenar: expulsado");
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => true,
        },
    };
    if remove {
        subs.remove(&conn_id);
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
        // La resolución del path por defecto puede tocar el FS (sonda de uid
        // del fallback /tmp) y el descubrimiento del catálogo de plugins lee el
        // dir de config: TODO I/O síncrono dentro del spawn_blocking (regla 2).
        let (listener, uid, socket_path, plugins, plugin_runtime) = tokio::task::spawn_blocking({
            let requested = cfg.socket_path;
            let plugins_dir = cfg.plugins_dir;
            move || -> Result<
                (
                    std::os::unix::net::UnixListener,
                    u32,
                    PathBuf,
                    crate::plugins::PluginRegistry,
                    Arc<norte_plugin_host::PluginRuntime>,
                ),
                DaemonError,
            > {
                let (listener, uid, socket_path) = bind_socket(requested)?;
                // Catálogo de plugins + runtime WASM compartido (M4-P3/P4):
                // I/O/CPU síncrono dentro del spawn_blocking (regla 2).
                let (plugins, plugin_runtime) = discover_plugins(plugins_dir)?;
                Ok((listener, uid, socket_path, plugins, plugin_runtime))
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
            undo_reports: Mutex::new(std::collections::VecDeque::new()),
            plugins: Mutex::new(plugins),
            plugin_runtime,
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
        // #44: avisos de conexión (degradación TLS) → `connection.degraded` SOLO
        // a humanos. `Weak` rompe el ciclo Shared → engine → observer → Shared.
        shared
            .engine
            .set_connection_observer(Arc::new(DaemonConnectionObserver {
                shared: Arc::downgrade(&shared),
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
                for task in shared.tasks.lock().expect("tasks lock sano").values() {
                    task.handle.cancel();
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

/// Descubre el catálogo de plugins y construye el runtime WASM compartido
/// (M4-P3/P4). TODO I/O/CPU síncrono: el caller lo invoca dentro del
/// `spawn_blocking` del bind (regla 2).
///
/// Un `plugins-state.toml` corrupto NO impide arrancar el daemon (dejaría al
/// usuario sin ninguna otra operación por un fichero de estado roto): se degrada
/// FAIL-CLOSED a un registro VACÍO (nada aprobado ni activado) con aviso, y el
/// usuario puede re-aprobar. La corrupción NUNCA "abre" un plugin que no estaba
/// consentido. Un catálogo ausente ya es "vacío" sin error. En cambio, un fallo
/// al crear el runtime SÍ aborta el bind: sin runtime no se ejecuta ningún
/// plugin (fail-closed).
fn discover_plugins(
    plugins_dir: Option<PathBuf>,
) -> Result<
    (
        crate::plugins::PluginRegistry,
        Arc<norte_plugin_host::PluginRuntime>,
    ),
    DaemonError,
> {
    let plugins_root = plugins_dir.unwrap_or_else(crate::connect::config_dir);
    let plugins = crate::plugins::PluginRegistry::discover(&plugins_root).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "plugins-state corrupto: se arranca con catálogo vacío");
        crate::plugins::PluginRegistry::empty(&plugins_root)
    });
    let runtime = norte_plugin_host::PluginRuntime::new()
        .map(Arc::new)
        .map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!(
                "no se pudo crear el runtime de plugins: {e}"
            )))
        })?;
    Ok((plugins, runtime))
}

/// Prepara el dir, enlaza el socket UDS y lo endurece (0600, no-root),
/// re-verificando la identidad del dir tras el bind (#34.2). `requested`
/// `None` = fallback por defecto (con mensaje accionable sobre `/tmp`,
/// #34.1). Síncrono: se llama dentro del `spawn_blocking` del bind (regla 2).
fn bind_socket(
    requested: Option<PathBuf>,
) -> Result<(std::os::unix::net::UnixListener, u32, PathBuf), DaemonError> {
    let defaulted = requested.is_none();
    let socket_path = requested.unwrap_or_else(|| super::default_socket_path(None));
    let dir = socket_path
        .parent()
        .ok_or(DaemonError::InsecureDir {
            reason: "el socket necesita un directorio padre",
        })?
        .to_path_buf();
    // #34.1: sobre el fallback /tmp, un dir inseguro (squat) sale con mensaje
    // ACCIONABLE en vez del InsecureDir opaco.
    let dir_id = prepare_socket_dir(&dir).map_err(|e| match e {
        DaemonError::InsecureDir { reason } if is_default_tmp_fallback(&socket_path, defaulted) => {
            DaemonError::UnusableDefaultDir {
                path: dir.clone(),
                reason,
            }
        }
        other => other,
    })?;
    // ¿Hay un daemon VIVO? Un connect lo delata; un socket huérfano (crash
    // previo) da ECONNREFUSED y se retira.
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
    // #34.2 (TOCTOU): detecta el swap COMÚN (rm+recreate cambia el inode) del
    // dir entre prepare y bind; si cambió, el socket recién creado vive en un
    // dir ajeno — se retira y se aborta. NO es barrera total (reuso de inodo,
    // swap posterior): la integridad del canal la garantiza el peer-cred
    // bilateral, ver `DirIdentity`. La ventana bind→este check no es
    // explotable (el accept-loop no arranca hasta que este helper retorna Ok).
    if let Err(e) = dir_id.verify_unchanged(&dir) {
        let _ = std::fs::remove_file(&socket_path);
        return Err(e);
    }
    // Nuestro euid = el dueño del socket que ACABAMOS de crear (sin unsafe,
    // regla 5). Solo el mismo uid podrá hablar.
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

/// Identidad de un directorio: `(dev, ino)`. Capturada al validar el dir del
/// socket y re-verificada tras el bind — un REEMPLAZO común del dir bajo el
/// mismo path durante la ventana prepare→bind (TOCTOU sin sticky bit en
/// /tmp, #34.2) cambia el inode y se detecta.
///
/// ALCANCE (defensa en profundidad, no barrera total): (a) el chequeo por
/// PATH es intrínsecamente racy — cada `of` re-statea; (b) el reuso de inodo
/// (ext4/tmpfs reciclan un ino liberado al instante) puede dar `(dev, ino)`
/// idénticos tras un rm+recreate; (c) es one-shot: no cubre un swap
/// POSTERIOR durante la vida del socket. La garantía REAL contra un daemon
/// impostor en un dir squatteado es el peer-cred BILATERAL (server:
/// `peer_allowed`; cliente: `Client::authenticated` rechaza un socket cuyo
/// dueño no es su uid). El cierre total exigiría anclar a fd (openat/
/// fstatat), que en std pide `unsafe`/dep — fuera de alcance (regla 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirIdentity {
    dev: u64,
    ino: u64,
}

impl DirIdentity {
    /// Identidad del dir en `path` SIN seguir symlinks del último componente.
    fn of(path: &Path) -> Result<Self, DaemonError> {
        let md = std::fs::symlink_metadata(path)?;
        Ok(Self {
            dev: md.dev(),
            ino: md.ino(),
        })
    }

    /// `Ok` si `path` sigue siendo el MISMO objeto de FS (dev+ino) que esta
    /// identidad; `InsecureDir` si fue reemplazado (o desapareció).
    fn verify_unchanged(self, path: &Path) -> Result<(), DaemonError> {
        let now = Self::of(path).map_err(|_| DaemonError::InsecureDir {
            reason: "el dir del socket desapareció durante el bind",
        })?;
        if now == self {
            Ok(())
        } else {
            Err(DaemonError::InsecureDir {
                reason: "el dir del socket fue reemplazado durante el bind",
            })
        }
    }
}

/// Verifica (creándolo si falta) que el dir del socket es NUESTRO y 0700:
/// jamás symlink, jamás de otro uid, jamás accesible a otros. Devuelve la
/// [`DirIdentity`] validada para re-comprobar tras el bind (#34.2).
fn prepare_socket_dir(dir: &Path) -> Result<DirIdentity, DaemonError> {
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
    Ok(DirIdentity {
        dev: md.dev(),
        ino: md.ino(),
    })
}

/// `true` si `path` es el fallback por defecto en `/tmp/norte-<uid>/…` (solo
/// cuando el path se tomó por defecto (`defaulted=true`), sin `--socket`).
/// El fallback es squat-eable (#34.1): un mensaje accionable pide fijar
/// `XDG_RUNTIME_DIR` o pasar `--socket`, en vez del `InsecureDir` opaco.
fn is_default_tmp_fallback(path: &Path, defaulted: bool) -> bool {
    defaulted
        && path.parent().is_some_and(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("norte-"))
                && dir.parent() == Some(Path::new("/tmp"))
        })
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
    /// Omitidas del índice del contenedor (#93), capturado al ABRIR el
    /// listado: cada página lo repite (el cliente puede engancharse en
    /// cualquiera; el total es por-contenedor, no por página).
    skipped: Option<u64>,
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
    // El gate de lectura (#80) lo aplica el ARM del dispatch, fuera de este
    // span instrumentado (no filtra el path a la traza en una denegación).

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
        return continue_listing(cur, &p.path, cap, now, conn, entries).await;
    }

    // Listado NUEVO (sin cursor).
    let mut stream = shared.engine.list(&p.path).await.map_err(RpcError::from)?;
    // Omitidas del contenedor (#93), capturado UNA vez al abrir (el índice
    // archive ya está caliente tras el `list`). Un error aquí NO tumba un
    // listado que ya abrió: degrada a `None` (= desconocido, lo de antes) —
    // pero con traza (el punto de #93 es no callar listados incompletos).
    let skipped = shared
        .engine
        .list_skipped(&p.path)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "list_skipped falló; omitidas = desconocido");
            None
        });
    match drain_page(&mut stream, cap, &mut entries).await {
        Ok(Drained::Done) => to_value(&methods::FsListResult {
            entries,
            next_cursor: None,
            skipped,
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
                            skipped,
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
                    skipped,
                    _guard: ListingGuard {
                        global: Arc::clone(&shared.open_listings),
                    },
                },
            );
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
                skipped,
            })
        }
        Err(e) => Err(RpcError::from(e)),
    }
}

/// Continuación de un listado retenido (el brazo con `cursor` de
/// [`handle_fs_list`], separado por tamaño): valida cursor↔path, drena la
/// página y decide retener (More) o cerrar (Done/error). El `skipped` del
/// open del listado se repite en cada página (#93).
async fn continue_listing(
    cursor: &str,
    path: &norte_proto::VPath,
    cap: Option<usize>,
    now: std::time::Instant,
    conn: &mut ConnState,
    mut entries: Vec<norte_proto::Entry>,
) -> Result<serde_json::Value, RpcError> {
    // Cursor no-numérico o desconocido = expirado (el cliente reinicia).
    let id: u64 = cursor.parse().map_err(|_| cursor_expired())?;
    let (drained, skipped) = {
        let listing = conn.listings.get_mut(&id).ok_or_else(cursor_expired)?;
        if listing.path != *path {
            return Err(RpcError::protocol(
                codes::INVALID_PARAMS,
                "cursor does not belong to this path",
            ));
        }
        let skipped = listing.skipped;
        (
            drain_page(&mut listing.stream, cap, &mut entries).await,
            skipped,
        )
    };
    match drained {
        Ok(Drained::More) => {
            // Se conserva bajo el MISMO id (el cliente reusa el cursor).
            if let Some(l) = conn.listings.get_mut(&id) {
                l.last_used = now;
            }
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
                skipped,
            })
        }
        Ok(Drained::Done) => {
            conn.listings.remove(&id);
            to_value(&methods::FsListResult {
                entries,
                next_cursor: None,
                skipped,
            })
        }
        Err(e) => {
            conn.listings.remove(&id);
            Err(RpcError::from(e))
        }
    }
}

/// `RpcError` para un cursor de paginación inválido/expirado (ADR 0017): lleva
/// la taxonomía [`Error::CursorExpired`](norte_proto::Error::CursorExpired) en
/// `data`, así el cliente la distingue y reinicia el listado.
fn cursor_expired() -> RpcError {
    RpcError::from(norte_proto::Error::CursorExpired)
}

async fn serve_connection(stream: UnixStream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();

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
    // #64: la LECTURA vive en su propia task alimentando un inbox acotado —
    // durante un dispatch SUSPENDIDO (el Ask de policy) el socket se sigue
    // leyendo, y un EOF/reset del peer cancela `peer_gone`: el dispatch en
    // vuelo se dropea y sus guards RAII limpian (la pendiente de aprobación
    // deja de ser zombi hasta el TTL). El dispatch sigue SERIAL (orden de
    // frames = orden de ejecución); solo cambia QUIÉN lee.
    let peer_gone = CancellationToken::new();
    let (inbox_tx, mut inbox_rx) = mpsc::channel::<serde_json::Value>(INBOX_FRAMES);
    let reader_task = tokio::spawn(read_frames(
        reader,
        tx.clone(),
        inbox_tx,
        peer_gone.clone(),
        shared.shutdown.clone(),
    ));

    let mut conn = ConnState::new();
    // #72: tokens de cancelación de las requests cancelables en vuelo.
    let inflight_cancel: InflightCancel = Arc::default();
    // Reap de listados paginados expirados en una conexión viva-pero-muda
    // (además del barrido perezoso en cada fs.list). OJO: entre dispatches —
    // un dispatch suspendido sigue reteniendo el tick (mitigado por el
    // barrido perezoso del siguiente fs.list, nota MINOR-3 de M3-3b).
    let mut sweep = tokio::time::interval(LISTING_SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // #72: frames leídos del inbox DURANTE un dispatch en vuelo que NO son un
    // rpc.cancel de la request en vuelo se bufferizan aquí y se procesan tras
    // el desenlace — el dispatch sigue SERIAL (orden de frames = orden de
    // ejecución). Un agente MCP no pipelinea, así que en la práctica el único
    // frame durante un Ask es el cancel o el EOF.
    let mut pending_frames: std::collections::VecDeque<serde_json::Value> =
        std::collections::VecDeque::new();
    let result: std::io::Result<()> = loop {
        // Siguiente frame: primero el búfer local, luego el inbox (con
        // shutdown/sweep atendidos SOLO entre dispatches, como antes de #72).
        let value = if let Some(v) = pending_frames.pop_front() {
            v
        } else {
            tokio::select! {
                msg = inbox_rx.recv() => match msg {
                    None => break Ok(()),
                    Some(v) => v,
                },
                () = shared.shutdown.cancelled() => break Ok(()),
                _ = sweep.tick() => {
                    conn.sweep_expired(shared.listing_ttl);
                    continue;
                }
            }
        };
        // Despacha vigilando el inbox en paralelo (#72 + #64).
        let dispatch = handle_value(value, conn_id, &tx, &mut conn, shared, &inflight_cancel);
        tokio::pin!(dispatch);
        let peer_died = loop {
            tokio::select! {
                biased;
                // Un dispatch que completa, completa (incluida la respuesta
                // Cancelled del withdrawal): gana a la muerte del peer.
                () = &mut dispatch => break false,
                // Un dispatch SUSPENDIDO muere con su peticionario (#64).
                () = peer_gone.cancelled() => break true,
                // Frames que llegan mientras este dispatch sigue en vuelo —
                // SOLO mientras el búfer de diferidos no esté al tope: en el
                // tope este brazo se deshabilita y el reader vuelve a hacer
                // backpressure sobre el socket (cota anti memoria sin límite,
                // MAJOR del security-reviewer #72). Un cancel que quedara detrás
                // de un flood no se lee (el Ask vencerá por TTL) — aceptable:
                // el pipelining bajo un Ask no-aprobado es el caso semi-hostil.
                msg = inbox_rx.recv(), if pending_frames.len() < MAX_DEFERRED_FRAMES => match msg {
                    // El reader terminó (EOF/shutdown): DROPEA el dispatch en
                    // vuelo (sus guards limpian) y cierra por el camino común.
                    // En la práctica `peer_gone` (biased, arriba) gana antes;
                    // este brazo es defensivo — jamás esperar a un dispatch
                    // suspendido aquí (colgaría hasta el TTL).
                    None => break true,
                    Some(frame) => {
                        if let Some(id) = rpc_cancel_id(&frame) {
                            // rpc.cancel: dispara el token de esa request si
                            // está en vuelo. NO rompe la conexión (a diferencia
                            // de peer_gone). Id desconocido/ya resuelto = no-op.
                            if let Some(tok) = inflight_cancel
                                .lock()
                                .expect("inflight_cancel lock sano")
                                .get(&id)
                            {
                                tok.cancel();
                            }
                        } else {
                            // Cualquier otro frame: se procesa TRAS el
                            // desenlace (dispatch serial).
                            pending_frames.push_back(frame);
                        }
                    }
                }
            }
        };
        if peer_died {
            break Ok(());
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
    // Cerrar el inbox termina al reader si aún vive (su `send` falla).
    drop(inbox_rx);
    let _ = writer_task.await;
    // El error de LECTURA (p. ej. ECONNRESET) se propaga como antes.
    match reader_task.await {
        Ok(read_result) => result.and(read_result),
        Err(_) => result,
    }
}

/// Loop de LECTURA de una conexión (#64): decodifica frames y los encola al
/// dispatch. Vive aunque el dispatch esté suspendido; a CUALQUIER salida
/// (EOF, reset, framing hostil, shutdown) el `DropGuard` cancela `peer_gone`
/// y el dispatch en vuelo se aborta. Nota: un peer que hiciera half-close
/// (shutdown del lado de escritura esperando aún la respuesta) se trata como
/// muerto — ningún cliente de norte lo hace.
async fn read_frames(
    mut reader: tokio::net::unix::OwnedReadHalf,
    tx: mpsc::Sender<Arc<[u8]>>,
    inbox: mpsc::Sender<serde_json::Value>,
    peer_gone: CancellationToken,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let _gone = peer_gone.drop_guard();
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut parse_errors = 0u32;
    loop {
        let n = tokio::select! {
            r = reader.read(&mut buf) => r?,
            () = shutdown.cancelled() => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        if decoder.push(&buf[..n]).is_err() {
            let resp = Response::err(
                None,
                RpcError::protocol(codes::PARSE_ERROR, "frame too large"),
            );
            send(&tx, &resp);
            return Ok(());
        }
        while let Some(frame) = decoder.next_frame() {
            // JSON roto = -32700; JSON válido que no es envelope = -32600
            // (M2 del protocol-guardian).
            let value: serde_json::Value = match serde_json::from_slice(&frame) {
                Ok(v) => v,
                Err(e) => {
                    parse_errors += 1;
                    if parse_errors > MAX_PARSE_ERRORS {
                        return Ok(());
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
            // Backpressure: con el inbox lleno el reader espera aquí (mismo
            // throttling que el dispatch inline daba); la detección de EOF
            // durante un Ask exige que el peer no tenga >INBOX_FRAMES frames
            // en vuelo — los clientes de norte son request/response.
            if inbox.send(value).await.is_err() {
                // El dispatch murió (shutdown): nada que encolar.
                return Ok(());
            }
        }
    }
}

/// Tokens de cancelación de las requests en vuelo cancelables (#72), por id
/// JSON-RPC. Vive FUERA de `ConnState` (que `handle_value` toma `&mut`): el
/// inner loop de `serve_connection` lo consulta EN PARALELO a un dispatch en
/// vuelo para disparar el token de un `rpc.cancel`. `Arc<Mutex<…>>` porque hay
/// dos dueños concurrentes: el loop (dispara) y `handle_value` (registra/retira).
type InflightCancel = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

/// Guard RAII del token en vuelo (#72): retira el id del mapa a CUALQUIER
/// salida del dispatch (respuesta normal, withdrawal por cancel, shutdown, o
/// drop por muerte del peer) — jamás un token huérfano que un `rpc.cancel`
/// tardío dispararía sobre una request ya resuelta.
struct InflightCancelGuard {
    map: InflightCancel,
    id: RequestId,
}

impl Drop for InflightCancelGuard {
    fn drop(&mut self) {
        self.map
            .lock()
            .expect("inflight_cancel lock sano")
            .remove(&self.id);
    }
}

/// Extrae el `id` de un frame `rpc.cancel` ya parseado (#72), o `None` si el
/// frame no es un `rpc.cancel` bien formado. Se aplica a los frames leídos del
/// inbox DURANTE un dispatch en vuelo — clasificación estructural sobre el
/// `Value`, sin deserializar el envelope entero. `rpc.cancel` está especificado
/// SOLO como notificación (sin id de envelope); un frame en forma de Request con
/// ese método se consumiría aquí igualmente (benigno: nunca se responde), pero
/// ningún cliente lo emite así.
fn rpc_cancel_id(value: &serde_json::Value) -> Option<RequestId> {
    if value.get("method").and_then(serde_json::Value::as_str) != Some(methods::RPC_CANCEL) {
        return None;
    }
    let id = value.get("params")?.get("id")?;
    serde_json::from_value::<RequestId>(id.clone()).ok()
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
    inflight_cancel: &InflightCancel,
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
            // #72: fs.copy/move/delete pueden suspenderse en un Ask de policy.
            // Se registra un token por su id para que un `rpc.cancel` (leído
            // por el loop de serve_connection en paralelo) retire el Ask
            // dropeando este dispatch — el gate muere PRE-efecto (su
            // PendingGuard limpia policy.pending) y JAMÁS aprueba (fail-closed
            // por construcción: dropear un future no puede devolver Approved).
            let cancelable = matches!(
                req.method.as_str(),
                methods::FS_COPY | methods::FS_MOVE | methods::FS_DELETE
            );
            let response = if cancelable {
                let cancel = CancellationToken::new();
                inflight_cancel
                    .lock()
                    .expect("inflight_cancel lock sano")
                    .insert(id.clone(), cancel.clone());
                let _cancel_guard = InflightCancelGuard {
                    map: Arc::clone(inflight_cancel),
                    id: id.clone(),
                };
                tokio::select! {
                    biased;
                    // Una op que COMPLETA (aprobada, o rechazada por policy)
                    // gana a un cancel simultáneo: no se retira lo ya resuelto.
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
                    // El agente retiró la request suspendida en el Ask: estado
                    // limpio garantizado (gate pre-efecto), sin filtrar policy.
                    () = cancel.cancelled() => Err(RpcError::from(norte_proto::Error::Cancelled)),
                }
            } else {
                // El dispatch respeta el shutdown (regla 3): un fs.list
                // gigante no retiene el apagado.
                tokio::select! {
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
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
                            actor: conn.actor.clone(),
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

// La tabla de dispatch crece un brazo por método nuevo (G3b añadió dos): es
// una lista plana, no lógica anidada — trocearla en sub-funciones por
// bloque de métodos no reduciría la complejidad real, solo la escondería
// detrás de una indirección. Mismo criterio que otros dispatchers grandes
// del árbol (ver `dispatch_fs_task`).
#[allow(clippy::too_many_lines)]
#[tracing::instrument(skip_all, fields(method = %req.method))]
async fn dispatch(
    req: Request,
    conn_id: u64,
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
        methods::DAEMON_SHUTDOWN => handle_daemon_shutdown(&conn.actor, req.params, shared),
        // fs.list vive AQUÍ (no en dispatch_fs_task): necesita el ConnState
        // para retener el stream paginado entre páginas (ADR 0017).
        methods::FS_LIST => {
            let p: methods::FsListParams = parse_params(req.params)?;
            // Gate de lectura (#80) AQUÍ, FUERA del span instrumentado de
            // `handle_fs_list` (que lleva `path` en sus fields): una denegación
            // no debe filtrar el path a la traza. Basta el ARRANQUE — la
            // continuación por cursor es del MISMO path ya validado.
            read_gate(&conn.actor, &p.path, shared)?;
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
        methods::POLICY_UNDO_REPORT => {
            let p: methods::PolicyUndoReportParams = parse_params(req.params)?;
            handle_policy_undo_report(&conn.actor, &p, shared)
        }
        // plugin.* (M4-P3): listar el catálogo (cualquier conexión) y aprobar/
        // activar (SOLO humanos — es consentir capabilities, acto de seguridad).
        methods::PLUGIN_LIST => handle_plugin_list(req.params, shared),
        methods::PLUGIN_SET_APPROVAL => {
            let p: methods::PluginSetApprovalParams = parse_params(req.params)?;
            handle_plugin_set_approval(&conn.actor, &p, shared).await
        }
        methods::PLUGIN_SET_ENABLED => {
            let p: methods::PluginSetEnabledParams = parse_params(req.params)?;
            handle_plugin_set_enabled(&conn.actor, &p, shared).await
        }
        // plugin.run_command (M4-P4): ABIERTO (ejecutar no consiente nada).
        methods::PLUGIN_RUN_COMMAND => handle_plugin_run_command(req.params, shared).await,
        // plugin.preview (M4-P5): ABIERTO (previsualizar no consiente nada).
        methods::PLUGIN_PREVIEW => handle_plugin_preview(req.params, &conn.actor, shared).await,
        // plugin.preview_styled (G3a, ADR 0037): gemelo con estilo, mismo
        // criterio de apertura que su gemelo plano.
        methods::PLUGIN_PREVIEW_STYLED => {
            handle_plugin_preview_styled(req.params, &conn.actor, shared).await
        }
        // plugin.decorate / plugin.column_values (G3b, ADR 0037): ABIERTOS
        // como el resto de `plugin.preview*`, con el mismo gate de lectura
        // (#80) extendido al lote entero (`read_gate_all`).
        methods::PLUGIN_DECORATE => handle_plugin_decorate(req.params, &conn.actor, shared).await,
        methods::PLUGIN_COLUMN_VALUES => {
            handle_plugin_column_values(req.params, &conn.actor, shared).await
        }
        // plugin.get_config (G3c): ABIERTO, mismo criterio que plugin.list.
        // plugin.set_config (G3c): SOLO humanos, mismo criterio que
        // plugin.set_approval/set_enabled — ajustes de plugin son datos de
        // usuario, un agente no los edita por su cuenta.
        methods::PLUGIN_GET_CONFIG => handle_plugin_get_config(req.params, shared).await,
        methods::PLUGIN_SET_CONFIG => {
            let p: methods::PluginSetConfigParams = parse_params(req.params)?;
            handle_plugin_set_config(&conn.actor, &p, shared).await
        }
        _ => dispatch_fs_task(req, conn_id, conn.actor.clone(), shared).await,
    }
}

/// `daemon.shutdown` — apagar el daemon es acto humano (#66): sin este gate,
/// el hard-shutdown cancela TODAS las tasks (bypass del gate de `task.cancel`)
/// y tumba la sesión del humano.
fn handle_daemon_shutdown(
    actor: &Actor,
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may shut down the daemon",
        ));
    }
    // El emisor canónico escribe `params: null` (ADR 0004) y este método es
    // todo-opcionales: null y ausencia = defaults (M1 del protocol-guardian;
    // el golden request_null_params lo pinnea).
    let p: methods::DaemonShutdownParams = parse_params(
        params
            .filter(|v| !v.is_null())
            .or_else(|| Some(serde_json::json!({}))),
    )?;
    if !p.graceful {
        shared.hard_shutdown.cancel();
    }
    shared.shutdown.cancel();
    to_value(&methods::DaemonShutdownResult {})
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
    let (handle, report) = shared
        .engine
        .undo_session_for(&target, Actor::User)
        .await
        .map_err(RpcError::from)?;
    // Efecto de seguridad (material de auditoría M3-5): quién deshizo a quién.
    // La session ya pasó la validación de charset — segura de loguear.
    if let Actor::Agent { session } = &target {
        tracing::info!(session = %session, "undo de sesión de agente pedido por el humano");
    }
    // El dueño de la task de undo es el EJECUTOR humano (solo User llega aquí).
    let task_id = register_task_id(shared, handle, Actor::User)?;
    // Retiene el informe para `policy.undo_report` (#71) — SOLO si la task
    // quedó registrada. OJO honestidad: en el camino OVERLOADED de arriba la
    // Task YA corre desde el submit y la cancelación es cooperativa — pueden
    // aterrizar reverts reales cuyo informe se pierde (el journal sí registra
    // las compensaciones; el cliente solo ve el OVERLOADED).
    {
        let mut reports = shared.undo_reports.lock().expect("undo_reports lock sano");
        reports.push_back((task_id.get(), report));
        while reports.len() > UNDO_REPORTS_MAX {
            reports.pop_front();
        }
    }
    to_value(&methods::PolicyUndoSessionResult { task_id })
}

/// `policy.undo_report` (#71): el informe de una Task de undo — SOLO-User,
/// como el `policy.undo_session` que lo genera (lleva `seq` del journal y
/// motivo de bloqueo). Snapshot: definitivo con la Task terminal.
fn handle_policy_undo_report(
    actor: &Actor,
    p: &methods::PolicyUndoReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may read an undo report",
        ));
    }
    let snapshot = {
        let reports = shared.undo_reports.lock().expect("undo_reports lock sano");
        let Some((_, r)) = reports.iter().find(|(id, _)| *id == p.task_id.get()) else {
            return Err(RpcError::protocol(
                codes::INVALID_PARAMS,
                "unknown undo task (never an undo, or evicted from the ring)",
            ));
        };
        r.lock().expect("undo report lock").clone()
    };
    to_value(&methods::PolicyUndoReportResult {
        undone: snapshot.undone,
        skipped_irreversible: snapshot.skipped_irreversible,
        skipped_created_no_trash: snapshot.skipped_created_no_trash,
        blocked: snapshot
            .blocked
            .map(|(seq, error)| methods::UndoBlocked { seq, error }),
    })
}

/// `plugin.list` (M4-P3): el catálogo descubierto + su estado, para CUALQUIER
/// conexión (listar no consiente nada). Sin params definidos (objeto vacío
/// reservado): null/ausencia se aceptan como defaults (ADR 0004), igual que
/// `daemon.shutdown`; un objeto cualquiera se ignora — extensión futura no
/// rompe clientes viejos.
fn handle_plugin_list(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let _p: methods::PluginListParams = parse_params(
        params
            .filter(|v| !v.is_null())
            .or_else(|| Some(serde_json::json!({}))),
    )?;
    let list = shared.plugins.lock().expect("plugins lock sano").list();
    to_value(&list)
}

/// `plugin.set_approval` (M4-P3): un HUMANO aprueba (o revoca) las capabilities
/// declaradas por un plugin. Aprobar es un acto de SEGURIDAD (consentir que un
/// plugin ejerza sus capabilities), así que SOLO una conexión no-agente lo hace
/// — el mismo criterio que `policy.grant_scope`/`policy.decide`: un agente jamás
/// consiente por el humano. Un id desconocido es `INVALID_PARAMS` (no se ensucia
/// el estado con plugins fantasma); un fallo de persistencia, `INTERNAL_ERROR`.
// `skip_all` SIN `id = %p.id`: el id viene crudo del wire y NO debe ir al log
// antes de validarse contra el catálogo (inyección de log / spoofing). Se loguea
// (info) SOLO tras confirmar que es un plugin conocido.
#[tracing::instrument(skip_all, fields(approved = p.approved))]
async fn handle_plugin_set_approval(
    actor: &Actor,
    p: &methods::PluginSetApprovalParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may approve a plugin",
        ));
    }
    // Muta EN MEMORIA bajo el lock y captura el snapshot + el dir; el lock se
    // libera al cerrar el bloque, ANTES de cualquier `.await` (regla 2: nada de
    // I/O bloqueante en el reactor, ni sostener un std::Mutex a través de await).
    let (applied, snapshot, dir) = {
        let mut reg = shared.plugins.lock().expect("plugins lock sano");
        let applied = reg.set_approval_in_memory(&p.id, p.approved);
        (
            applied,
            reg.state_snapshot(),
            reg.config_dir().to_path_buf(),
        )
    };
    if !applied {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    }
    tokio::task::spawn_blocking(move || crate::plugins::persist_state(&dir, &snapshot))
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "persist task panicked"))?
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("persist: {e}")))?;
    // Efecto de seguridad (material de auditoría M4): plugin YA validado como
    // conocido, así que su id es seguro para el log.
    tracing::info!(id = %p.id, "plugin (des)aprobado por el humano");
    to_value(&methods::PluginSetApprovalResult {})
}

/// `plugin.set_enabled` (M4-P3): un HUMANO activa/desactiva un plugin ya
/// aprobado. Misma barrera y misma semántica de retorno que
/// [`handle_plugin_set_approval`] (id desconocido = `INVALID_PARAMS`).
// `skip_all` sin `id = %p.id`: idéntico razonamiento que
// [`handle_plugin_set_approval`] — el id crudo del wire no va al log sin validar.
#[tracing::instrument(skip_all, fields(enabled = p.enabled))]
async fn handle_plugin_set_enabled(
    actor: &Actor,
    p: &methods::PluginSetEnabledParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may enable a plugin",
        ));
    }
    // Mismo patrón regla-2 que set_approval: muta bajo el lock, persiste fuera.
    let (applied, snapshot, dir) = {
        let mut reg = shared.plugins.lock().expect("plugins lock sano");
        let applied = reg.set_enabled_in_memory(&p.id, p.enabled);
        (
            applied,
            reg.state_snapshot(),
            reg.config_dir().to_path_buf(),
        )
    };
    if !applied {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    }
    tokio::task::spawn_blocking(move || crate::plugins::persist_state(&dir, &snapshot))
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "persist task panicked"))?
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("persist: {e}")))?;
    tracing::info!(id = %p.id, "plugin (des)activado por el humano");
    to_value(&methods::PluginSetEnabledResult {})
}

/// `plugin.run_command` (M4-P4): ejecuta un comando de un plugin YA aprobado y
/// activado. ABIERTO a cualquier conexión (no consiente nada: el humano ya
/// aprobó+activó, y el sandbox WASI vacío contiene al guest).
///
/// Regla 2 (crítica): la EJECUCIÓN (`instantiate` compila el componente WASM +
/// `run_command`) es síncrona y pesada. Se separa en dos:
/// 1. RESOLVER (barato) bajo el lock del registry: valida el consentimiento y
///    resuelve `.wasm` + capabilities. El `MutexGuard` se suelta al cerrar el
///    bloque, ANTES del `.await`.
/// 2. EJECUTAR (pesado) FUERA del lock, en un `spawn_blocking`, con un clon del
///    `Arc<PluginRuntime>` compartido.
///
/// Redacción hacia el cliente (security-reviewer M4-P4): un fallo de runtime
/// (`PluginRunError::Runtime`) puede llevar la ruta del `.wasm` o detalles
/// internos de wasmtime; JAMÁS se devuelve su `Display` crudo al cliente —
/// se responde un `INTERNAL_ERROR` genérico y el detalle va SOLO al log local.
// `skip_all` sin `id`: el id viene crudo del wire; no va al log salvo tras
// resolver (mismo criterio que set_approval/set_enabled).
#[tracing::instrument(skip_all)]
async fn handle_plugin_run_command(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginRunCommandParams = parse_params(params)?;
    // 1) Resolver bajo el lock (barato). El guard NO cruza el `.await`.
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.resolve_runnable(&p.id)
    };
    let (wasm, caps, settings) = resolved.map_err(|e| run_error_to_rpc(&e))?;

    // 2) Ejecutar fuera del lock, en spawn_blocking (regla 2). El runtime es
    // `Send+Sync` pero no `Clone`: se clona el `Arc`.
    let runtime = Arc::clone(&shared.plugin_runtime);
    let command = p.command.clone();
    let arg = p.arg.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        // P2 Task 4a: entrega `[config]` YA resuelto (Task 2) al guest, mismo
        // criterio que `PluginRegistry::run_command` (uso embebido).
        inst.set_settings(settings);
        inst.run_command(&command, &arg)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "plugin task panicked"))?
    .map_err(|e| {
        // Redacción: el detalle (posible ruta del .wasm / interno de wasmtime)
        // va SOLO al log local; al cliente, un mensaje genérico.
        tracing::warn!(id = %p.id, error = %e, "el runtime del plugin falló");
        RpcError::protocol(codes::INTERNAL_ERROR, "plugin runtime failed")
    })?;
    to_value(&methods::PluginRunCommandResult { output })
}

/// Mapea el veredicto de consentimiento de [`crate::plugins::resolve_runnable`]
/// (nunca `Runtime`, que se maneja aparte con redacción) a un `RpcError`. Los
/// mensajes de estos variantes NO revelan rutas (llevan el id, no el path;
/// coherente con la redacción de `list()`), así que son seguros de propagar.
fn run_error_to_rpc(e: &crate::plugins::PluginRunError) -> RpcError {
    use crate::plugins::PluginRunError as E;
    match e {
        // Id inexistente o sin binario: error de PARÁMETRO (el cliente pidió
        // algo que no existe/no es ejecutable).
        E::Unknown(_) | E::NoBinary(_) => RpcError::protocol(codes::INVALID_PARAMS, e.to_string()),
        // Sin aprobar / desactivado: la petición no es válida en este estado
        // (el humano no ha consentido). INVALID_REQUEST con mensaje claro.
        E::NotApproved(_) | E::Disabled(_) => {
            RpcError::protocol(codes::INVALID_REQUEST, e.to_string())
        }
        // No debería llegar aquí (resolve_runnable no ejecuta), pero por si el
        // tipo evoluciona: redactado, jamás el Display crudo.
        E::Runtime(_) => RpcError::protocol(codes::INTERNAL_ERROR, "plugin runtime failed"),
    }
}

/// `plugin.preview` (M4-P5): renderiza el archivo `p.path` con el PRIMER
/// previewer consentido cuyo mimetype (adivinado por extensión) case, o devuelve
/// `preview: None` si ninguno aplica. ABIERTO como `run_command` (previsualizar
/// no consiente nada). Tres fases, separadas para NO cruzar el `MutexGuard` por
/// un `.await` (regla 2):
///
/// 1. RESOLVER (barato) bajo el lock del registry: adivina el mimetype y elige
///    el previewer. El guard se suelta al cerrar el bloque, ANTES de todo await.
///    Ninguno → `preview: None` (NO error: el frontend cae a la vista cruda).
/// 2. LEER los bytes del archivo ACOTADOS a [`PREVIEW_MAX_BYTES`](crate::plugins::PREVIEW_MAX_BYTES)
///    vía el engine (async, fuera del lock). Un archivo ilegible (`NotFound`…)
///    es un error HONESTO que se propaga — no un preview vacío.
/// 3. EJECUTAR (pesado, compila WASM) FUERA del lock, en un `spawn_blocking`,
///    con un clon del `Arc<PluginRuntime>` compartido.
///
/// Redacción hacia el cliente (security-reviewer M4-P4/P5): un fallo de runtime
/// puede llevar la ruta del `.wasm` o detalles de wasmtime; JAMÁS se devuelve su
/// `Display` crudo — `INTERNAL_ERROR` genérico + el detalle SOLO al log local.
///
/// ABIERTO (no solo-User) A SABIENDAS y ACOPLADO a `fs.read`: este handler lee
/// el archivo con la autoridad del daemon, igual que `fs.read`, que HOY es
/// abierto para agentes. Como el preview devuelve una transformación con
/// pérdida del primer MiB, es lectura estrictamente INFERIOR a la de `fs.read`
/// crudo (sin escalada; security-reviewer M4-P5). INVARIANTE (cumplida en #80):
/// `plugin.preview` gatea con el MISMO [`read_gate`] que `fs.read` — un agente
/// solo previsualiza bajo su scope; si no, sería el bypass de lectura.
// `skip_all`: `p.path` va a los campos redactados de las capas inferiores, no al
// span de este handler (mismo criterio que run_command).
#[tracing::instrument(skip_all)]
async fn handle_plugin_preview(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginPreviewParams = parse_params(params)?;
    // Gate de lectura (#80): preview LEE el archivo con la autoridad del daemon;
    // sin este gate un agente sin scope exfiltraría contenido esquivando fs.read.
    read_gate(actor, &p.path, shared)?;
    // El mimetype es `&'static str` (heurística por extensión, no lee bytes).
    let mime = crate::plugins::guess_mimetype(&p.path);
    // 1) Resolver bajo el lock (barato). El guard NO cruza el `.await`.
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.resolve_previewer(mime)
    };
    let Some((id, name, wasm, caps, settings)) = resolved else {
        // Ningún previewer consentido casa el mimetype: NO es error. El
        // frontend cae a la vista cruda. Los bytes ni se leen.
        return to_value(&methods::PluginPreviewResult { preview: None });
    };

    // 2) Leer los bytes ACOTADOS vía el engine (async, fuera del lock). Un
    // archivo ilegible (NotFound, permisos…) es un error honesto que se propaga,
    // no un preview silenciosamente vacío. El `len` acota en el provider; se
    // trunca por si algún provider entrega de más.
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(crate::plugins::PREVIEW_MAX_BYTES),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));

    // 2.5) §6.2 (#29): decodifica a TEXTO como el camino EMBEBIDO
    // (`Backend::plugin_preview`) — el guest jamás debe asumir UTF-8 sobre
    // bytes crudos. `lossy` (#101) marca cuándo la decodificación produjo `�`.
    // (Paridad de comportamiento embebido↔daemon, regla 7 — antes este handler
    // pasaba los bytes crudos al guest.)
    let (content, lossy) = crate::plugins::decode_for_preview(bytes);

    // 3) Ejecutar fuera del lock, en spawn_blocking (regla 2). El runtime es
    // `Send+Sync` pero no `Clone`: se clona el `Arc`. `mime` es `&'static` → se
    // mueve tal cual al closure.
    let runtime = Arc::clone(&shared.plugin_runtime);
    let output = tokio::task::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        // P2 Task 4a: entrega `[config]` YA resuelto (Task 2) al previewer,
        // igual que `handle_plugin_run_command` ya hace para comandos.
        inst.set_settings(settings);
        inst.render_preview(mime, &content)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "preview task panicked"))?
    .map_err(|e| {
        // Redacción: el detalle (posible ruta del .wasm / interno de wasmtime)
        // va SOLO al log local; al cliente, un mensaje genérico.
        tracing::warn!(plugin = %id, error = %e, "preview runtime failed");
        RpcError::protocol(codes::INTERNAL_ERROR, "preview runtime failed")
    })?;
    to_value(&methods::PluginPreviewResult {
        preview: Some(methods::PluginPreview {
            plugin_id: id,
            plugin_name: name,
            output,
            lossy,
        }),
    })
}

/// `plugin.preview_styled` (G3a, ADR 0037): gemelo CON ESTILO de
/// [`handle_plugin_preview`]. Mismos tres pasos y el mismo gate de lectura
/// (#80) — ABIERTO como su gemelo plano, previsualizar no consiente nada.
///
/// Difiere del gemelo plano en el paso 3 (ejecución) y en su redacción de
/// fallos: un fallo del RUNTIME en `render-styled` (trap, error de lógica
/// del guest, o los topes de la tabla ADR 0037 excedidos vía
/// `RuntimeError::StyledPreviewTooLarge`) responde `preview: None`, NUNCA
/// `INTERNAL_ERROR` — el preview con estilo es un ENRIQUECIMIENTO sobre el
/// plano (ADR 0037: "un cliente re-valida y cae a `plugin.preview` si se
/// violan [los topes]"; aquí el SERVER ya se adelanta con el mismo criterio
/// para no obligar a un cliente a distinguir "no hay preview" de "el
/// preview falló" cuando el resultado observable —caer al plano— es
/// idéntico). El detalle del fallo va SOLO al log local (igual redacción
/// que el gemelo plano).
///
/// `role` de cada [`methods::SpanWire`] viaja SIN VALIDAR: `norte-core`
/// (headless) no depende de `norte-theme` — ver el rustdoc de
/// [`crate::plugins::to_wire_lines`] y de `Backend::plugin_preview_styled`
/// para el razonamiento completo de esa frontera.
#[tracing::instrument(skip_all)]
async fn handle_plugin_preview_styled(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginPreviewStyledParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let mime = crate::plugins::guess_mimetype(&p.path);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.resolve_previewer(mime)
    };
    let Some((id, name, wasm, caps, settings)) = resolved else {
        return to_value(&methods::PluginPreviewStyledResult { preview: None });
    };

    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(crate::plugins::PREVIEW_MAX_BYTES),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));

    // §6.2 (#29): decodifica a TEXTO (paridad con el embebido, regla 7);
    // `lossy` (#101) al frontend para el aviso.
    let (content, lossy) = crate::plugins::decode_for_preview(bytes);

    let runtime = Arc::clone(&shared.plugin_runtime);
    let outcome = tokio::task::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        inst.set_settings(settings);
        inst.render_styled_preview(mime, &content)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "styled preview task panicked"))?;

    let lines = match outcome {
        Ok(lines) => lines,
        Err(e) => {
            tracing::warn!(
                plugin = %id,
                error = %e,
                "render-styled falló: preview:None (cae a plugin.preview)"
            );
            return to_value(&methods::PluginPreviewStyledResult { preview: None });
        }
    };
    to_value(&methods::PluginPreviewStyledResult {
        preview: Some(methods::PluginPreviewStyled {
            plugin_id: id,
            plugin_name: name,
            lines: crate::plugins::to_wire_lines(lines),
            lossy,
        }),
    })
}

/// Gate de LECTURA para agentes sobre un LOTE de rutas (G3b): mismo
/// criterio que [`read_gate`] aplicado a CADA elemento de `paths`,
/// cortando en la PRIMERA denegación (fail-fast, no acumula parcial). Un
/// `Actor::User` sigue sin sandboxear (una sola comprobación barata
/// bastaría, pero recorrer todas mantiene el código simétrico y no cambia
/// el resultado); un `Actor::Agent` decora/valora columnas SOLO sobre
/// entradas que ya podía listar bajo su scope — sin este gate, un agente
/// fuera de scope podría usar `plugin.decorate`/`plugin.column_values`
/// como un oráculo de existencia/nombre de rutas ajenas a su sandbox.
fn read_gate_all(
    actor: &Actor,
    paths: &[norte_proto::VPath],
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    for path in paths {
        read_gate(actor, path, shared)?;
    }
    Ok(())
}

/// `plugin.decorate` (G3b, ADR 0037 decisión 2): la SUPERPOSICIÓN de TODOS
/// los plugins `decorator` APROBADOS y ACTIVADOS sobre `params.paths`
/// (batched, POSICIONAL 1:1 — ver el rustdoc de
/// [`norte_proto::methods::PluginDecorateResult`]). ABIERTO como sus
/// gemelos `plugin.preview*` (decorar no consiente nada) pero con el MISMO
/// gate de lectura (#80) que ellos, extendido a TODO el lote
/// ([`read_gate_all`]): sin él, un agente exfiltraría existencia/nombres de
/// rutas fuera de su scope pidiendo decoraciones sobre ellas.
///
/// Fail-closed POR PLUGIN (nunca por lote): un plugin que no instancia,
/// trapea, o rompe el contrato posicional se OMITE del resultado con aviso
/// en el log local — el resto de la página se pinta igual. `params.paths`
/// vacío responde `{plugins: []}` sin resolver el catálogo.
#[tracing::instrument(skip_all)]
async fn handle_plugin_decorate(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginDecorateParams = parse_params(params)?;
    read_gate_all(actor, &p.paths, shared)?;
    if p.paths.is_empty() {
        return to_value(&methods::PluginDecorateResult {
            plugins: Vec::new(),
        });
    }
    let expected_len = p.paths.len();
    let entries = crate::plugins::paths_to_basenames(&p.paths);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.resolve_decorators()
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let plugins = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for (id, _name, wasm, caps, settings) in resolved {
            let Ok(mut inst) = runtime.instantiate_decorator(&wasm, caps) else {
                tracing::warn!(plugin = %id, "decorator: fallo al instanciar, se omite del lote");
                continue;
            };
            inst.set_settings(settings);
            let Ok(raw) = inst.decorate(&entries) else {
                tracing::warn!(plugin = %id, "decorator: fallo al ejecutar decorate, se omite del lote");
                continue;
            };
            let Some(decorations) = crate::plugins::decorations_to_wire_checked(raw, expected_len)
            else {
                tracing::warn!(
                    plugin = %id,
                    "decorator: longitud no casa el contrato posicional, se omite del lote"
                );
                continue;
            };
            out.push(methods::PluginDecorations {
                plugin_id: id,
                decorations,
            });
        }
        out
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "decorate task panicked"))?;
    to_value(&methods::PluginDecorateResult { plugins })
}

/// `plugin.column_values` (G3b, ADR 0037 decisión 2): valores de la columna
/// `params.column_id` para `params.paths`, del ÚNICO plugin `columns` que
/// la declara ([`crate::PluginRegistry::resolve_columns`], primero-que-casa
/// — a diferencia de `plugin.decorate`). Mismo gate de lectura por lote y
/// mismo contrato de entradas (basenames) que `handle_plugin_decorate`.
///
/// Fail-closed: si el plugin no instancia, trapea, o rompe el contrato
/// posicional, el resultado es un vector de `None` del tamaño de
/// `params.paths` (celda vacía para toda la página) en vez de un error.
#[tracing::instrument(skip_all)]
async fn handle_plugin_column_values(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginColumnValuesParams = parse_params(params)?;
    read_gate_all(actor, &p.paths, shared)?;
    if p.paths.is_empty() {
        return to_value(&methods::PluginColumnValuesResult { values: Vec::new() });
    }
    let expected_len = p.paths.len();
    let entries = crate::plugins::paths_to_basenames(&p.paths);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.resolve_columns(&p.column_id)
    };
    let Some((id, _name, wasm, caps, settings)) = resolved else {
        return to_value(&methods::PluginColumnValuesResult {
            values: vec![None; expected_len],
        });
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let column_id = p.column_id;
    let values = tokio::task::spawn_blocking(move || {
        let Ok(mut inst) = runtime.instantiate_columns(&wasm, caps) else {
            tracing::warn!(plugin = %id, "columns: fallo al instanciar, celdas vacías");
            return vec![None; expected_len];
        };
        inst.set_settings(settings);
        let Ok(raw) = inst.column_values(&column_id, &entries) else {
            tracing::warn!(plugin = %id, "columns: fallo al ejecutar, celdas vacías");
            return vec![None; expected_len];
        };
        crate::plugins::column_values_checked(raw, expected_len).unwrap_or_else(|| {
            tracing::warn!(
                plugin = %id,
                "columns: longitud no casa el contrato posicional, celdas vacías"
            );
            vec![None; expected_len]
        })
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "column_values task panicked"))?;
    to_value(&methods::PluginColumnValuesResult { values })
}

/// `plugin.get_config` (0.28.0, G3c, ADR 0037): esquema `[config]` + valor
/// EFECTIVO de `id`, uno por clave. ABIERTO a cualquier conexión (leer un
/// esquema/valor no consiente nada, mismo criterio que `plugin.preview*`/
/// `plugin.decorate`). `id` desconocido responde `keys: []` (mismo criterio
/// indulgente que `plugin.list` con un catálogo vacío — nunca un error).
/// Barato: solo lee bajo el lock, sin `spawn_blocking` (a diferencia de
/// `plugin.decorate`/`column_values`, que instancian WASM).
#[tracing::instrument(skip_all)]
async fn handle_plugin_get_config(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginGetConfigParams = parse_params(params)?;
    let keys = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.config_keys(&p.id).unwrap_or_default()
    };
    let keys = keys
        .into_iter()
        .map(|(key, spec, value)| crate::plugins::config_key_to_wire(key, &spec, value))
        .collect();
    to_value(&methods::PluginGetConfigResult { keys })
}

/// `plugin.set_config` (0.28.0, G3c, ADR 0037): persiste UN valor de
/// `[config]`, validado contra el ESQUEMA del manifiesto (la MISMA
/// validación que `config.toml`, vía `PluginRegistry::set_config`). Ajustes
/// de plugin son DATOS DE USUARIO, no un acto de consentimiento de
/// capabilities — pero SIGUE siendo humano-only (mismo criterio que
/// [`handle_plugin_set_approval`]/[`handle_plugin_set_enabled`]: un agente
/// no reconfigura un plugin por su cuenta). Un id/clave desconocidos o un
/// valor inválido son `INVALID_PARAMS`; NADA se persiste en ese caso
/// (`PluginRegistry::set_config` valida ANTES de escribir).
// `skip_all` sin `id`/`key`: crudos del wire, no validados aún (mismo
// criterio que set_approval/set_enabled — solo se loguean tras confirmar).
#[tracing::instrument(skip_all)]
async fn handle_plugin_set_config(
    actor: &Actor,
    p: &methods::PluginSetConfigParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may change a plugin's settings",
        ));
    }
    let id = p.id.clone();
    let key = p.key.clone();
    let value = p.value.clone();
    // El lock se sostiene DURANTE la escritura (a diferencia de
    // set_approval/set_enabled, que lo sueltan antes de persistir):
    // deliberado — serializa el read-modify-write de `config.toml` para
    // ESTE plugin frente a un `set_config` concurrente sobre otra clave del
    // mismo plugin, que si no podría perder una escritura (dos
    // lecturas-modificaciones-escrituras de `config.toml` entrelazadas).
    // Sigue corriendo en `spawn_blocking` (regla 2: la escritura + el
    // re-`resolve_settings` son I/O síncrona), así que el reactor async
    // nunca bloquea — solo un hilo de la pool bloqueante sostiene el lock.
    let shared = Arc::clone(shared);
    tokio::task::spawn_blocking(move || {
        let mut reg = shared.plugins.lock().expect("plugins lock sano");
        reg.set_config(&id, &key, &value)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "set_config task panicked"))?
    .map_err(|e| RpcError::protocol(codes::INVALID_PARAMS, e.to_string()))?;
    tracing::info!(id = %p.id, key = %p.key, "ajuste de plugin cambiado por el humano");
    to_value(&methods::PluginSetConfigResult {})
}

/// Instante de expiración de un scope a partir de su `ttl_ms`: clamp a
/// `[1, MAX_SCOPE_TTL_MS]` (ni 0 = ya-expirado inútil, ni cuasi-perpetuo) y
/// suma saturante (jamás panica por overflow del reloj).
fn scope_deadline(ttl_ms: u64) -> Instant {
    let ms = ttl_ms.clamp(1, MAX_SCOPE_TTL_MS);
    let now = Instant::now();
    now.checked_add(Duration::from_millis(ms)).unwrap_or(now)
}

/// Gate de LECTURA para agentes (cierra #80). Fuente ÚNICA del criterio que
/// hoy consultan `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities` y `fs.search`:
/// un `Actor::Agent` solo lee bajo un scope VIVO de su sesión
/// ([`ScopeRegistry::covers_read`], membresía de raíz independiente de op —
/// leer es estrictamente menos que cualquier mutación); un `User` (humano) no
/// se sandboxea; CUALQUIER otro actor (`Plugin` hoy, o variantes futuras) se
/// deniega SIN excepción (default-deny para lo que aún no sabemos gobernar; de
/// ahí el `allow` del lint —nombrar `Plugin` dejaría pasar sin gate una
/// variante futura—). El veredicto es `PolicyDenied` con la categoría gruesa
/// del vocabulario cerrado ([`DenyReason::rule_id`]), jamás la regla concreta
/// ni el `path` (sin fuga en el error ni en la traza de auditoría M3-5).
///
/// Simétrico con el gate de MUTACIONES de M3 (`Engine::gate`): la IA es un
/// ciudadano, no un dueño (spec §1.5).
fn read_gate(
    actor: &Actor,
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    use crate::policy::{DenyReason, ScopeVerdict};
    #[allow(clippy::match_wildcard_for_single_variants)]
    let denied: Option<DenyReason> = match actor {
        Actor::User => None,
        Actor::Agent { session } => {
            match shared.scopes.covers_read(session, path, Instant::now()) {
                ScopeVerdict::Within => None,
                ScopeVerdict::Expired => Some(DenyReason::ScopeExpired),
                ScopeVerdict::OutOfScope => Some(DenyReason::OutOfScope),
            }
        }
        _ => Some(DenyReason::OutOfScope),
    };
    if let Some(reason) = denied {
        // Trazable (auditoría M3-5), como el gate de mutaciones. Solo la
        // categoría gruesa y el actor: jamás el path denegado.
        tracing::warn!(
            rule = reason.rule_id(),
            "lectura sin scope: denegada (default-deny)"
        );
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        }));
    }
    Ok(())
}

/// `fs.search` (0.18.0, live search): búsqueda recursiva de nombre/contenido
/// bajo `root` como Task cancelable. Los HITS llegan por `search.hits` SOLO a
/// la conexión `conn_id` que la lanzó (envío dirigido, jamás broadcast — son
/// suyos).
///
/// GATE DE LECTURA PARA AGENTES (security, liveSearch T4): `fs.search`
/// amplifica la lectura — una sola llamada sobre `/` exfiltraría previews de
/// TODO el árbol. Por eso un `Actor::Agent` solo busca si `root` cae bajo un
/// scope VIVO de su sesión ([`ScopeRegistry::covers_read`]); fuera de él es
/// `PolicyDenied` con la categoría gruesa del vocabulario cerrado
/// ([`DenyReason::rule_id`]), jamás la regla concreta. Un `User` (humano) no se
/// sandboxea: simetría con `fs.list`/`fs.read`, que HOY siguen abiertos para
/// agentes — deuda #80 (M3 solo gateó mutaciones; search es el primer read
/// acotado).
///
/// Criterios inválidos (glob/regex que no compilan, cero criterios, ejes
/// excluyentes) → `INVALID_PARAMS` SIN crear Task, con el diagnóstico del
/// compilador (es el propio input del requester, no una fuga). Muerte del peer
/// a mitad: la Task ya está registrada y se gobierna por `task.cancel` como una
/// copia (#64 acota la ventana del dispatch dropeado); la bomba de hits muere
/// sola cuando el walker cierra el canal.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_search(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsSearchParams = parse_params(params)?;

    // Gate de lectura (default-deny para agentes fuera de scope): el mismo
    // [`read_gate`] que `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities` (#80).
    read_gate(actor, &p.root, shared)?;

    // Validación de criterios ANTES de la Task: compila los matchers para
    // recuperar el diagnóstico saneado y responder INVALID_PARAMS sin crear
    // Task (`search_as` los recompila —barato— y devolvería un error opaco). El
    // detalle es el mensaje del compilador de glob/regex: input del requester.
    if let Err(e) = crate::search::SearchMatchers::compile(&p) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("invalid search criteria: {e}"),
        ));
    }

    let (handle, mut rx) = shared
        .engine
        .search_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANTE (#64): CERO `.await` entre el submit del engine (dentro de
    // `search_as`) y este register — la Task jamás corre FUERA de `shared.tasks`
    // (con task.list/cancel y contando contra los topes). Quien añada un await
    // aquí rompe esa garantía.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // Bomba de HITS: drena el canal del walker y enruta cada lote como
    // `search.hits` SOLO al dueño. Muere sola cuando el walker cierra `tx`
    // (terminal, cancel o receptor —el propio dueño— desaparecido).
    let shared_pump = Arc::clone(shared);
    tokio::spawn(async move {
        while let Some(hits) = rx.recv().await {
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::SEARCH_HITS.into(),
                params: serde_json::to_value(&hits).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice()));
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// Las familias `fs.*`/`task.*` del dispatch (separadas por tamaño). El
/// `actor` viene de la conexión (M3-3b): las mutaciones se journalizan y
/// evalúan bajo él.
async fn dispatch_fs_task(
    req: Request,
    conn_id: u64,
    actor: crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        // fs.search (0.18.0): los HITS son del que la lanzó → necesita conn_id
        // para el envío dirigido (jamás broadcast).
        methods::FS_SEARCH => handle_fs_search(req.params, conn_id, &actor, shared).await,
        methods::FS_STAT => {
            let p: methods::FsStatParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80
            let entry = shared.engine.stat(&p.path).await.map_err(RpcError::from)?;
            to_value(&methods::FsStatResult { entry })
        }
        // index.query (0.25.0, M4): lectura directa del índice.
        methods::INDEX_QUERY => {
            let p: methods::IndexQueryParams = parse_params(req.params)?;
            read_gate(&actor, &p.root, shared)?; // #80
            let hits = shared
                .engine
                .index_query_as(&p.root, &p.text, p.limit, actor.clone())
                .await
                .map_err(RpcError::from)?;
            let hits = hits
                .into_iter()
                .map(|h| methods::IndexHit {
                    path: h.path,
                    kind: h.kind,
                    size: h.size,
                    mtime_ms: h.mtime_ms,
                })
                .collect();
            to_value(&methods::IndexQueryResult { hits })
        }
        // index.build (0.25.0, M4): Task. El resultado (indexed/removed) NO se
        // reenvía por wire aún (task completa = hecho); un fetch de report es
        // deuda análoga a `policy.undo_report`.
        methods::INDEX_BUILD => {
            let p: methods::IndexBuildParams = parse_params(req.params)?;
            // Gate de LECTURA (#80): el build camina el subárbol y sus paths
            // salen por `task.progress.current` al owner — un agente fuera de
            // scope enumeraría un árbol arbitrario. Se gatea igual que fs.search /
            // index.query (security/rust BLOCKER de la review M4).
            read_gate(&actor, &p.root, shared)?;
            let (handle, _report) = shared
                .engine
                .index_build_as(p.root, actor.clone())
                .await
                .map_err(RpcError::from)?;
            let task_id = register_task_id(shared, handle, actor.clone())?;
            to_value(&methods::FsTaskResult { task_id })
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
                .copy_with_as(&p.from, &p.to, opts, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // INVARIANTE (#64): CERO `.await` entre el submit del engine y
            // este register — un dispatch dropeado por la muerte del peer
            // jamás deja una Task corriendo FUERA de `shared.tasks` (sin
            // task.list/cancel, sin contar contra MAX_LIVE_TASKS). Quien
            // añada un await aquí rompe esa garantía.
            register_task(shared, handle, actor)
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
                .move_with_as(&p.from, &p.to, opts, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        methods::FS_DELETE => {
            let p: methods::FsDeleteParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .delete_with_as(&p.path, p.mode, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        _ => dispatch_task_family(req, actor, shared).await,
    }
}

/// El resto del dispatch: `task.*` y los métodos de solo-lectura de 0.5.0.
/// El `actor` viene de la conexión: gobierna la visibilidad de `task.list`,
/// el alcance de `task.cancel` y quién puede bendecir host keys (#66).
async fn dispatch_task_family(
    req: Request,
    actor: crate::journal::Actor,
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
            //
            // Visibilidad por actor (#66): un humano lo ve TODO; un agente
            // SOLO sus tasks — `current` lleva paths de otros actores.
            let mut tasks: Vec<norte_proto::TaskProgress> = shared
                .recent
                .lock()
                .expect("recent lock sano")
                .iter()
                .filter(|(_, owner)| may_observe(&actor, owner))
                .map(|(p, _)| p.clone())
                .collect();
            tasks.extend(
                shared
                    .tasks
                    .lock()
                    .expect("tasks lock sano")
                    .values()
                    .filter(|t| may_observe(&actor, &t.owner))
                    .map(|t| t.handle.progress().borrow().clone()),
            );
            to_value(&methods::TaskListResult { tasks })
        }
        methods::FS_READ => {
            let p: methods::FsReadParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80
            dispatch_fs_read(p, shared).await
        }
        methods::FS_CAPABILITIES => {
            let p: methods::FsCapabilitiesParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80: revela existencia/tipo
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
            if let Some(task) = shared
                .tasks
                .lock()
                .expect("tasks lock sano")
                .get(&p.task_id.get())
            {
                // Gate de actor (#66): un agente solo cancela lo SUYO. Una
                // task ajena se trata como desconocida — mismo ack, sin
                // filtrar existencia. El intento sí se traza (material de
                // auditoría M3-5), como el grant de scope.
                if may_observe(&actor, &task.owner) {
                    task.handle.cancel();
                } else {
                    tracing::warn!(
                        task_id = p.task_id.get(),
                        actor = ?actor,
                        "task.cancel sobre task ajena: ignorado por el gate de actor"
                    );
                }
            }
            to_value(&methods::TaskCancelResult {})
        }
        methods::CONNECTION_TRUST_HOST_KEY => {
            // Aceptar un fingerprint bajo TOFU es una decisión de confianza
            // HUMANA, como `grant_scope`/`decide`/`undo_session` (#66): un
            // agente jamás bendice la identidad de un host.
            if !matches!(actor, Actor::User) {
                return Err(RpcError::protocol(
                    codes::INVALID_REQUEST,
                    "only a human (non-agent) connection may trust a host key",
                ));
            }
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
fn register_task(
    shared: &Arc<Shared>,
    handle: TaskHandle,
    owner: Actor,
) -> Result<serde_json::Value, RpcError> {
    let task_id = register_task_id(shared, handle, owner)?;
    to_value(&methods::FsTaskResult { task_id })
}

/// Registra la task y arranca su bomba de progreso: cada snapshot (≤30 Hz)
/// sale como `task.progress` a los humanos y al dueño (#66); el estado
/// terminal jamás se pierde por el rate-limit (se difunde con el mismo
/// enrutado por dueño) y desregistra la task. Devuelve el `TaskId` (los
/// métodos con result propio lo envuelven ellos, M3-4).
fn register_task_id(
    shared: &Arc<Shared>,
    handle: TaskHandle,
    owner: Actor,
) -> Result<TaskId, RpcError> {
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
        // Sub-tope por clase (#70): las tasks de agentes (todas las sesiones)
        // no agotan el cupo global — el humano conserva su headroom.
        if !matches!(owner, Actor::User)
            && tasks
                .values()
                .filter(|t| !matches!(t.owner, Actor::User))
                .count()
                >= MAX_LIVE_TASKS_AGENTS
        {
            handle.cancel();
            return Err(RpcError::protocol(
                codes::OVERLOADED,
                format!("too many live agent tasks (max {MAX_LIVE_TASKS_AGENTS}); retry later"),
            ));
        }
        tasks.insert(
            task_id.get(),
            RegisteredTask {
                handle,
                owner: owner.clone(),
            },
        );
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
                push_recent(&mut recent, snapshot.clone(), &owner);
            }
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::TASK_PROGRESS.into(),
                params: serde_json::to_value(&snapshot).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                shared_pump.broadcast_task_progress(&Arc::from(frame.into_boxed_slice()), &owner);
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
                    push_recent(&mut recent, last.clone(), &owner);
                }
                let notif = Notification {
                    jsonrpc: norte_proto::wire::JsonRpcVersion,
                    method: methods::TASK_PROGRESS.into(),
                    params: serde_json::to_value(&last).ok(),
                };
                if let Ok(frame) = encode_frame(&notif) {
                    shared_pump
                        .broadcast_task_progress(&Arc::from(frame.into_boxed_slice()), &owner);
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
    use std::os::unix::fs::PermissionsExt;

    use super::{DirIdentity, is_default_tmp_fallback, peer_allowed, prepare_socket_dir};

    /// `send_to_conn` es envío DIRIGIDO (los hits de una búsqueda son del que la
    /// lanzó): solo la conexión destino recibe; una conexión desconocida es
    /// no-op; una conexión cuyo receptor murió se retira del mapa (mismo criterio
    /// de expulsión que el broadcast — no acumula backlog).
    #[test]
    fn send_to_conn_solo_al_destino_y_retira_los_muertos() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, send_to_conn_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        let (tx1, mut rx1) = mpsc::channel::<Arc<[u8]>>(4);
        let (tx2, mut rx2) = mpsc::channel::<Arc<[u8]>>(4);
        subs.lock().expect("lock").insert(
            1u64,
            Subscriber {
                tx: tx1,
                actor: Actor::User,
            },
        );
        subs.lock().expect("lock").insert(
            2u64,
            Subscriber {
                tx: tx2,
                actor: Actor::User,
            },
        );
        let frame: Arc<[u8]> = Arc::from(vec![1u8, 2, 3].into_boxed_slice());

        // Solo la conexión 1 recibe.
        send_to_conn_impl(&subs, 1, &frame);
        assert!(rx1.try_recv().is_ok(), "el destino recibe");
        assert!(rx2.try_recv().is_err(), "el otro NO recibe");

        // Conexión desconocida: no-op sin panic, sin tocar el mapa.
        send_to_conn_impl(&subs, 99, &frame);
        assert_eq!(subs.lock().expect("lock").len(), 2);

        // Receptor muerto: la conexión se retira del mapa.
        drop(rx1);
        send_to_conn_impl(&subs, 1, &frame);
        assert!(
            !subs.lock().expect("lock").contains_key(&1),
            "la conexión con receptor cerrado se retira"
        );
        assert!(subs.lock().expect("lock").contains_key(&2), "la viva sigue");
    }

    /// La política de admisión es EXACTAMENTE mismo-uid: ni root entra.
    #[test]
    fn peer_allowed_solo_mismo_uid() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(0, 1000), "root NO es el usuario del daemon");
    }

    /// #34.2 (TOCTOU): la identidad del dir se captura y un REEMPLAZO del dir
    /// entre prepare y bind (mismo path, otro inode) se detecta.
    #[test]
    fn dir_identity_detecta_reemplazo_del_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sub = tmp.path().join("sock-dir");
        std::fs::create_dir(&sub).expect("mkdir");
        let id = prepare_socket_dir(&sub).expect("dir válido");
        // Sin cambios: la identidad casa.
        assert!(id.verify_unchanged(&sub).is_ok());
        // Reemplazo (rm + recreate) = nuevo inode → detectado.
        std::fs::remove_dir(&sub).expect("rmdir");
        std::fs::create_dir(&sub).expect("recreate");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(
            id.verify_unchanged(&sub).is_err(),
            "un dir reemplazado bajo el mismo path NO debe pasar"
        );
    }

    /// La captura de identidad es estable entre llamadas al MISMO dir.
    #[test]
    fn dir_identity_estable_para_el_mismo_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = DirIdentity::of(tmp.path()).expect("stat a");
        let b = DirIdentity::of(tmp.path()).expect("stat b");
        assert_eq!(a, b);
    }

    /// #34.1 (squat /tmp): solo el fallback `/tmp/norte-<uid>/…` (XDG vacío)
    /// merece el mensaje accionable; un `XDG_RUNTIME_DIR` real o un `--socket`
    /// explícito, no.
    #[test]
    fn detecta_solo_el_fallback_de_tmp() {
        use std::path::Path;
        assert!(is_default_tmp_fallback(
            Path::new("/tmp/norte-1000/daemon.sock"),
            true
        ));
        // No es fallback si el path NO vino por defecto (--socket explícito).
        assert!(!is_default_tmp_fallback(
            Path::new("/tmp/norte-1000/daemon.sock"),
            false
        ));
        // Ni un XDG real que resultara vivir bajo /run.
        assert!(!is_default_tmp_fallback(
            Path::new("/run/user/1000/norte/daemon.sock"),
            true
        ));
    }
}
