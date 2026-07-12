//! Servidor del daemon (ADR 0011): UDS + `SO_PEERCRED`, dispatch de
//! `initialize`/`fs.*`/`task.*`/`daemon.shutdown`, broadcast de
//! `task.progress` y shutdown por inactividad o petición.

use std::collections::HashMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use crate::Engine;
use crate::engine::TransferOptions;
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

/// Configuración del daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path del socket; `None` = [`super::default_socket_path`].
    pub socket_path: Option<PathBuf>,
    /// Apagado tras este tiempo sin clientes NI tasks. `None` = nunca.
    pub idle_timeout: Option<Duration>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            idle_timeout: Some(Duration::from_mins(5)),
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
    subscribers: Mutex<HashMap<u64, mpsc::Sender<Arc<[u8]>>>>,
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
}

impl Shared {
    fn idle(&self) -> bool {
        self.connections.load(Ordering::SeqCst) == 0
            && self.tasks.lock().expect("tasks lock sano").is_empty()
    }

    fn broadcast(&self, frame: &Arc<[u8]>) {
        let mut subs = self.subscribers.lock().expect("subscribers lock sano");
        // try_send: el que tiene la outbox llena pierde la suscripción (y
        // pronto la conexión, cuando su próximo response tampoco quepa) —
        // el backlog de un cliente lento jamás crece sin límite.
        subs.retain(|conn, tx| match tx.try_send(Arc::clone(frame)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(conn, "suscriptor sin drenar: expulsado del broadcast");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
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
        Ok(Self {
            listener,
            socket_path,
            shared: Arc::new(Shared {
                engine,
                tasks: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(HashMap::new()),
                next_conn: AtomicUsize::new(0),
                connections: AtomicUsize::new(0),
                shutdown: CancellationToken::new(),
                hard_shutdown: CancellationToken::new(),
                uid,
            }),
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
    let mut initialized = false;
    let mut parse_errors = 0u32;
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
            handle_value(value, conn_id, &tx, &mut initialized, shared).await;
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
    initialized: &mut bool,
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
            let was_initialized = *initialized;
            // El dispatch respeta el shutdown (regla 3): un fs.list
            // gigante no retiene el apagado.
            let response = tokio::select! {
                r = dispatch(req, initialized, shared) => r,
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
            if !was_initialized && *initialized {
                shared
                    .subscribers
                    .lock()
                    .expect("subscribers lock sano")
                    .insert(conn_id, tx.clone());
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
    initialized: &mut bool,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !*initialized && req.method != methods::INITIALIZE {
        return Err(RpcError::protocol(
            codes::NOT_INITIALIZED,
            "initialize required first",
        ));
    }
    match req.method.as_str() {
        methods::INITIALIZE => {
            // Repetirlo es un error de protocolo (como LSP): renegociar a
            // mitad de sesión no significa nada.
            if *initialized {
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
            *initialized = true;
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
        _ => dispatch_fs_task(req, shared).await,
    }
}

/// Las familias `fs.*`/`task.*` del dispatch (separadas por tamaño).
async fn dispatch_fs_task(
    req: Request,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::FS_LIST => {
            let p: methods::FsListParams = parse_params(req.params)?;
            let mut stream = shared.engine.list(&p.path).await.map_err(RpcError::from)?;
            let mut entries = Vec::new();
            while let Some(item) = stream.next().await {
                entries.push(item.map_err(RpcError::from)?);
            }
            to_value(&methods::FsListResult { entries })
        }
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
            };
            let handle = shared
                .engine
                .copy_with(&p.from, &p.to, opts)
                .map_err(RpcError::from)?;
            register_task(shared, handle)
        }
        methods::FS_MOVE => {
            let p: methods::FsMoveParams = parse_params(req.params)?;
            let opts = TransferOptions {
                on_collision: p.on_collision,
                symlinks: p.symlinks,
            };
            let handle = shared
                .engine
                .move_with(&p.from, &p.to, opts)
                .map_err(RpcError::from)?;
            register_task(shared, handle)
        }
        methods::FS_DELETE => {
            let p: methods::FsDeleteParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .delete_with(&p.path, p.mode)
                .map_err(RpcError::from)?;
            register_task(shared, handle)
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
        other => Err(RpcError::protocol(
            codes::METHOD_NOT_FOUND,
            format!("unknown method: {other}"),
        )),
    }
}

/// Registra la task y arranca su bomba de progreso: cada snapshot (≤30 Hz)
/// sale como `task.progress` a TODOS los clientes; el estado terminal se
/// difunde SIEMPRE y desregistra la task.
fn register_task(shared: &Arc<Shared>, handle: TaskHandle) -> Result<serde_json::Value, RpcError> {
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
        shared_pump
            .tasks
            .lock()
            .expect("tasks lock sano")
            .remove(&task_id.get());
    });

    to_value(&methods::FsTaskResult { task_id })
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
