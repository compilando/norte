//! Servidor del daemon (ADR 0011): UDS + `SO_PEERCRED`, dispatch de
//! `initialize`/`fs.*`/`task.*`/`daemon.shutdown`, broadcast de
//! `task.progress` y shutdown por inactividad o petición.

use std::collections::HashMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
/// Planes de sincronización RETENIDOS a la vez por UNA conexión (ADR 0049).
///
/// No es un tope de concurrencia —de eso ya se encarga [`MAX_LIVE_TASKS`]—, sino
/// de acumulación: un plan aprobado sobrevive a su Task durante
/// `SYNC_PLAN_TTL_MS`, y es un fichero con el listado relativo de dos árboles
/// enteros. Sin tope, planificar en bucle variando `include` (cada selección da
/// otro digest, o sea otro fichero) llena el directorio de estado, que es el
/// mismo en el que vive `journal.db`; quedarse sin disco ahí no es una molestia,
/// es la regla 4 dejando de ser satisfacible.
///
/// Generoso a propósito: un frontend tiene un plan por panel y a lo sumo una
/// comparación previa que todavía mira.
const MAX_RETAINED_SYNC_PLANS: usize = 16;
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
/// Tope del prompt de `ai.rename_plan` (security review M4-IA): los tokens de
/// ENTRADA son el coste del proveedor; el frame de 16 MiB no es un límite.
const MAX_AI_INSTRUCTION_BYTES: usize = 4 * 1024;
/// Tope de la query de `index.search_semantic` (mismo cinturón que la
/// instrucción de `ai.rename_plan`).
const MAX_AI_QUERY_BYTES: usize = MAX_AI_INSTRUCTION_BYTES;

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
    /// Dónde vive la sesión de UI (L2): `<state_dir>/session.json` y su lock.
    /// `None` = **no se persiste nada** — ni se toma el lock ni se arranca el
    /// escritor—, que es lo que quiere un test y lo que jamás debe pasarle al
    /// `state_dir` real por descuido. El binario pasa
    /// [`norte_config::dirs::state_dir`] explícitamente.
    pub state_dir: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            idle_timeout: Some(Duration::from_mins(5)),
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
            state_dir: None,
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
    /// Conexiones que son DUEÑAS de al menos un feed dirigido vivo
    /// (`compare.rows`, `search.hits`, `sync.steps`), con cuántos (#155). Una
    /// conexión de esta lista NO se expulsa del mapa de suscriptores porque su
    /// outbox se llene: pierde el FRAME, jamás la suscripción — ver
    /// [`Shared::broadcast_where`].
    directed_feeds: Mutex<HashMap<u64, u32>>,
    /// La sesión de UI del daemon (L2): UN documento, con su revisión y su
    /// conexión dueña. `Arc` porque el volcado a disco la mira desde otra
    /// task. El core la guarda y no la lee (ADR 0058).
    ui_session: Arc<crate::ui_session::SessionStore>,
    /// Si lo que se escriba en esa sesión va a LLEGAR al disco: hay
    /// `state_dir`, este core tiene el lock, y el fichero que había no es de
    /// una versión más nueva.
    ///
    /// Es lo que `session.get` contesta como `owner`, y no solo «te la
    /// quedaste»: para el cliente las dos cosas son la misma pregunta —«¿mis
    /// escrituras se guardan?»— y contestar que sí cuando no hay escritor es
    /// prometer una pantalla que se pierde entera y en silencio.
    ///
    /// **Atómico y no un `bool`, y eso es #237.** Se calculaba UNA vez en el
    /// bind, así que un daemon que arrancaba mientras otro core tenía el lock
    /// contestaba `owner: false` el resto de su vida — también horas después
    /// de que el otro se hubiera ido y el fichero llevara libre desde
    /// entonces. Lo vuelve a intentar [`session_writer`], que es quien tiene
    /// dónde correr, y lo enciende desde ahí. `Arc` por lo mismo que
    /// `session_flush`: el escritor NO retiene el `Shared`, que lo mantendría
    /// vivo.
    session_persists: Arc<AtomicBool>,
    /// Despierta al escritor de la sesión fuera de su tick: la última
    /// conexión que se va no debería dejar un segundo de pantalla sin volcar.
    /// `Arc` porque el escritor NO retiene el `Shared` (lo mantendría vivo).
    session_flush: Arc<tokio::sync::Notify>,
    /// Runtime WASM compartido para ejecutar comandos de plugin (M4-P4). Se
    /// construye UNA vez en el bind (arranca un hilo "ticker" de época) y se
    /// reutiliza entre `plugin.run_command`. `Arc` porque `PluginRuntime` es
    /// `Send+Sync` pero NO `Clone` (posee el `JoinHandle` del ticker): el
    /// handler clona el `Arc` y ejecuta la instanciación+ejecución (pesada,
    /// síncrona) en un `spawn_blocking`, jamás en el reactor con un lock tomado
    /// (regla 2).
    plugin_runtime: Arc<norte_plugin_host::PluginRuntime>,
    /// Instancias de columnas VIVAS entre páginas (#224). Cuelga de aquí por
    /// lo mismo que el runtime: es estado de proceso, y la instancia que sirvió
    /// la página 1 es la que tiene el índice del proyecto ya parseado cuando
    /// llega la 2. El backend embebido tiene el suyo, y es el MISMO tipo — uno
    /// con pool y otro sin él sería la asimetría de #165/#201/#181 otra vez.
    column_pool: Arc<crate::plugins::ColumnPool>,
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

/// El derecho de una conexión a no ser expulsada mientras su feed dirigido
/// vive (#155). Se cuenta, no se marca: una conexión puede tener a la vez una
/// búsqueda y una comparación, y la primera en terminar no puede quitarle el
/// derecho a la otra.
struct DirectedFeed {
    shared: Arc<Shared>,
    conn_id: u64,
}

impl Drop for DirectedFeed {
    fn drop(&mut self) {
        let mut feeds = self
            .shared
            .directed_feeds
            .lock()
            .expect("directed feeds lock sano");
        if let Some(n) = feeds.get_mut(&self.conn_id) {
            *n -= 1;
            if *n == 0 {
                feeds.remove(&self.conn_id);
            }
        }
    }
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

    /// Difunde a TODAS las conexiones. Hoy solo `daemon.going_away`: que este
    /// daemon se vaya le pasa igual a un agente que a un humano, y los dos
    /// tienen que decidir lo mismo (volver o no).
    fn broadcast_all(&self, frame: &Arc<[u8]>) {
        self.broadcast_where(frame, |_| true);
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
    /// murió o ya no está suscrita.
    ///
    /// Una outbox LLENA pierde el frame y NO la suscripción (#155): la
    /// expulsión era irreversible —la entrada solo se inserta en
    /// `initialize`— y se llevaba por delante el `task.progress` terminal, que
    /// es justo la señal con la que el cliente detecta que le faltan filas. El
    /// backlog sigue acotado por el canal, que es quien lo acotaba de verdad;
    /// lo que se pierde son frames, y eso el contrato ya sabe decirlo. Una
    /// outbox CERRADA sí retira la entrada: ahí no hay nadie a quien proteger.
    ///
    /// Devuelve `false` si el frame NO se entregó. Sirve para que una bomba de
    /// feed dirigido deje de producir: seguir comparando dos árboles durante
    /// una hora para un dueño que no lee es trabajo tirado y un permiso del
    /// scheduler retenido.
    fn send_to_conn(&self, conn_id: u64, frame: &Arc<[u8]>) -> bool {
        send_to_conn_impl(&self.subscribers, conn_id, frame)
    }

    /// Marca `conn_id` como dueño de un feed dirigido vivo hasta que el guard
    /// se dropee (#155).
    fn feed_guard(self: &Arc<Self>, conn_id: u64) -> DirectedFeed {
        *self
            .directed_feeds
            .lock()
            .expect("directed feeds lock sano")
            .entry(conn_id)
            .or_insert(0) += 1;
        DirectedFeed {
            shared: Arc::clone(self),
            conn_id,
        }
    }

    fn broadcast_where(&self, frame: &Arc<[u8]>, wants: impl Fn(&Subscriber) -> bool) {
        // Quién NO se expulsa por una outbox llena (#155): el dueño de un feed
        // dirigido vivo. Se lee ANTES de tomar el lock de suscriptores — anidar
        // los dos por cada frame de broadcast es un orden de bloqueo que no
        // hace falta inventar.
        let feeds: Vec<u64> = self
            .directed_feeds
            .lock()
            .expect("directed feeds lock sano")
            .keys()
            .copied()
            .collect();
        broadcast_impl(&self.subscribers, &feeds, frame, wants);
    }
}

/// Núcleo testeable de [`Shared::broadcast_where`]: manda `frame` a cada
/// suscriptor que `wants` acepte y RETIRA al que tenga el receptor muerto.
///
/// Una outbox LLENA expulsa —el backlog de un cliente lento jamás crece sin
/// límite (M1)— SALVO que la conexión esté en `feeds`, es decir sea dueña de
/// un feed dirigido vivo (#155): a ésa la expulsión le quitaría también el
/// `task.progress` terminal de su propia task, que es la señal con la que
/// comprueba si le llegaron todas las filas. Pierde el frame y sigue
/// suscrita.
fn broadcast_impl(
    subs: &Mutex<HashMap<u64, Subscriber>>,
    feeds: &[u64],
    frame: &Arc<[u8]>,
    wants: impl Fn(&Subscriber) -> bool,
) {
    let mut subs = subs.lock().expect("subscribers lock sano");
    subs.retain(|conn, s| {
        // Un suscriptor excluido de ESTA notif conserva su suscripción.
        if !wants(s) {
            return true;
        }
        match s.tx.try_send(Arc::clone(frame)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if feeds.contains(conn) {
                    tracing::warn!(
                        conn,
                        "dueño de un feed dirigido sin drenar: se pierde el frame, no la suscripción"
                    );
                    return true;
                }
                tracing::warn!(conn, "suscriptor sin drenar: expulsado del broadcast");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    });
}

/// Núcleo testeable de [`Shared::send_to_conn`]: envía `frame` a la conexión
/// `conn_id` de `subs` (si existe) y RETIRA la entrada solo si su receptor
/// MURIÓ. Una outbox llena pierde el frame y conserva la suscripción (#155).
///
/// `true` = entregado.
fn send_to_conn_impl(
    subs: &Mutex<HashMap<u64, Subscriber>>,
    conn_id: u64,
    frame: &Arc<[u8]>,
) -> bool {
    let mut subs = subs.lock().expect("subscribers lock sano");
    let (delivered, remove) = match subs.get(&conn_id) {
        None => return false,
        Some(s) => match s.tx.try_send(Arc::clone(frame)) {
            Ok(()) => (true, false),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    conn = conn_id,
                    "dueño de un feed dirigido sin drenar: se pierde el frame, no la suscripción"
                );
                (false, false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => (false, true),
        },
    };
    if remove {
        subs.remove(&conn_id);
    }
    delivered
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
    /// El escritor de la sesión, si hay dónde escribirla.
    ///
    /// **El derecho a escribir —el lock— vive DENTRO de la task** desde #237:
    /// es ella quien lo toma tarde si al arrancar lo tenía otro core, así que
    /// tenerlo aquí sería tenerlo en dos sitios. Se espera a que termine en el
    /// apagado ordenado, y al terminar suelta el lock: el sucesor de un relevo
    /// tiene que encontrar el fichero ya escrito y el lock ya libre. Si el
    /// daemon se va por cualquier otro camino, su `Drop` la aborta (ver
    /// [`SessionWriter`]).
    session_writer: Option<SessionWriter>,
}

/// El escritor de la sesión, que se PARA si su daemon muere sin apagarse.
///
/// **El `abort` no cancela un `spawn_blocking` en vuelo.** Si cae justo
/// mientras `flush_session` espera su escritura, esa escritura termina, pero
/// el estado del escritor —y con él el lock— se dropea ya: la escritura puede
/// aterrizar después de que un sucesor haya tomado el lock. Es anterior a #237
/// y esa versión lo tenía peor (soltaba el `session_lock` ANTES de abortar);
/// no se alcanza desde el binario, que siempre espera a `run()` hasta el
/// final, solo desde un `Daemon` dropeado en un test o en un empotrador.
///
/// Dropear un `JoinHandle` de tokio DESLIGA la task, no la para. Sin este
/// envoltorio, un daemon que se va por un camino que no es el apagado ordenado
/// —un `accept` que falla, un bind que se dropea, un test que abandona— suelta
/// su `session_lock` y deja la task viva: un proceso escribiendo el fichero
/// SIN el derecho a escribirlo, que es exactamente el segundo escritor que
/// todo esto existe para que no haya.
struct SessionWriter(Option<tokio::task::JoinHandle<()>>);

impl SessionWriter {
    /// El handle, para ESPERARLO en el apagado ordenado. Lo que queda ya no
    /// aborta nada.
    fn take(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        self.0.take()
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
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
    ///
    /// # Policy
    /// Un engine sobre el que nadie llamó a
    /// [`Engine::with_policy`](crate::Engine::with_policy) gatea con `AllowAll`:
    /// este bind lo AVISA por `warn!` y sigue (#166). No se rechaza porque
    /// «permisivo a propósito» es una configuración legítima; lo que no puede
    /// ser es indistinguible de un olvido.
    #[tracing::instrument(skip(engine, scopes, approvals, cfg))]
    pub async fn bind_with_policy(
        engine: Arc<Engine>,
        scopes: ScopeRegistry,
        approvals: Arc<DaemonApprovalResolver>,
        cfg: DaemonConfig,
    ) -> Result<Self, DaemonError> {
        // #166: el gate del engine es `AllowAll` mientras nadie instale una
        // policy, y un daemon sobre ese engine no gatea NADA — ni siquiera a un
        // agente. Ningún binario nuestro llega aquí así (`daemon run` instala
        // `ScopedPolicy`), pero un embebedor o un harness sí puede, y el hueco
        // no tiene hoy ni una línea de log. `sync.apply` es lo que cambia las
        // consecuencias: una llamada, un hash, y un `Mirror` reescribe y borra.
        if !engine.has_explicit_policy() {
            tracing::warn!(
                "daemon montado sobre un engine SIN policy: toda operación de \
                 todo actor pasa (AllowAll por omisión). Instala una policy con \
                 Engine::with_policy antes de bind (#166)."
            );
        }
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

        // La sesión de UI (L2): el lock y la carga son I/O síncrono, así que
        // van a un pool blocking (regla 2). Sin `state_dir` no se persiste
        // nada, que es lo que quiere un test; con él, quien no consigue el
        // lock arranca CON la pantalla y sin escritor —clona y corre suelto—.
        let (session_lock, sesion, escribible) = open_session(cfg.state_dir.clone()).await?;
        // Persistir es las TRES cosas a la vez: hay dónde, se tiene el
        // derecho, y lo que hay en disco no es de un binario más nuevo. Lo que
        // se calcula aquí es el ARRANQUE, no la vida entera (#237): al que le
        // falta solo el lock lo vuelve a intentar el escritor.
        // `Release`/`Acquire` y no `Relaxed`: el escritor ADOPTA el documento
        // (bajo el mutex del almacén) y solo después enciende esta bandera, y
        // el handler de `session.get` lee la bandera y solo después el
        // documento. Con `Relaxed` nada ata esos dos pares, así que un cliente
        // podía recibir `owner: true` con la revisión de ANTES de la adopción,
        // escribir contra ella y llevarse un `Conflict` que no tenía por qué
        // existir. Se corrige gratis: en x86 son las mismas instrucciones.
        let session_persists = Arc::new(AtomicBool::new(
            session_lock.is_some() && cfg.state_dir.is_some() && escribible,
        ));
        let session_flush = Arc::new(tokio::sync::Notify::new());

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
            column_pool: Arc::new(crate::plugins::ColumnPool::default()),
            directed_feeds: Mutex::new(HashMap::new()),
            ui_session: Arc::new(crate::ui_session::SessionStore::new(sesion)),
            session_persists: Arc::clone(&session_persists),
            session_flush: Arc::clone(&session_flush),
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
        // El escritor existe siempre que haya DÓNDE escribir, y es él quien
        // decide si de verdad escribe: arranca con el lock si el bind lo
        // consiguió, y sin él lo vuelve a intentar (#237). Lo que sigue
        // valiendo es el gate: un core suelto —o uno que encontró una sesión
        // de un binario más nuevo— tiene la pantalla y NO la escribe. Sin eso,
        // «no se lee» acababa siendo «se pisa un segundo después», que es
        // justo lo contrario de lo que promete.
        let session_writer = cfg.state_dir.map(|dir| {
            SessionWriter(Some(tokio::spawn(session_writer(
                Arc::clone(&shared.ui_session),
                dir,
                session_flush,
                shared.shutdown.clone(),
                session_persists,
                EstadoEscritura::inicial(session_lock, escribible),
            ))))
        });
        Ok(Self {
            listener,
            socket_path,
            shared,
            idle_timeout: cfg.idle_timeout,
            session_writer,
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
    pub async fn run(mut self) -> Result<(), DaemonError> {
        let shared = Arc::clone(&self.shared);
        let mut idle_since = tokio::time::Instant::now();
        // Un `accept` que falla NO sale por `?`: saliendo por ahí se saltaría
        // el apagado ordenado de abajo, y lo que queda detrás es un escritor
        // de sesión SUELTO —el `JoinHandle` se dropea sin abortar, o sea que
        // la task sigue— escribiendo el fichero con el lock ya soltado. El
        // error se guarda y se devuelve DESPUÉS de apagar.
        let mut fallo: Option<std::io::Error> = None;
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(v) => v,
                        Err(e) => { fallo = Some(e); break }
                    };
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

        // La sesión de UI se vuelca y el lock se suelta AQUÍ, antes de retirar
        // la ruta del socket — que es lo que le da permiso al sucesor de un
        // relevo para arrancar. Al revés, el sucesor encontraría el lock
        // todavía tomado y correría suelto: un relevo dejaría al daemon nuevo
        // sin poder guardar nada, que es justo lo contrario de lo que un
        // relevo promete. El token se cancela también aquí porque el camino de
        // inactividad sale del bucle sin pasar por `daemon.shutdown`.
        // CERRAR la sesión antes del último volcado, no después: entre el
        // volcado y el cierre de las conexiones cabe un `put`, y sellar bajo el
        // mismo lock que la mutación es lo único que impide contestarle
        // `Ok(revision)` sobre un fichero que ya no va a escribir nadie.
        //
        // Y el sello va antes del `cancel`, no después (revisión de #237): el
        // `cancel` ARMA la rama de apagado del escritor, que hace su volcado
        // final y termina — en un runtime multihilo eso puede ocurrir antes de
        // que se ejecute la línea siguiente, y un `put` que caiga en esa
        // ventana recibe `Ok(revision)` por un cuerpo que ya no escribe nadie.
        // Que es exactamente lo que el párrafo de arriba dice que no pasa.
        shared.ui_session.seal();
        shared.shutdown.cancel();
        // Esperar al escritor es esperar al último volcado Y a que suelte el
        // lock: los dos viven dentro de la task desde #237.
        if let Some(mut writer) = self.session_writer.take()
            && let Some(handle) = writer.take()
        {
            let _ = handle.await;
        }

        // Fase de apagado: nada de clientes nuevos (el listener muere con
        // el drop); hard = cancelar tasks; graceful = esperarlas — y si el
        // hard llega DURANTE la espera (segunda señal), se cancelan ya.
        drop(self.listener);
        // **La ruta se retira AQUÍ, antes de drenar, y el orden importa desde
        // que existe el relevo (roadmap ítem 10).**
        //
        // Con el listener muerto y el fichero todavía en su sitio, un cliente
        // que reconecte recibe `ECONNREFUSED` y —solo tras un relevo, porque
        // solo entonces tiene permiso— arranca el reemplazo. El reemplazo ve
        // una ruta rancia, la borra, y enlaza la suya. Cuando este daemon
        // terminara de drenar, su `remove_file` borraría el socket DEL
        // REEMPLAZO: se quedaría escuchando en un inodo sin nombre, y como el
        // permiso de arranque es de un solo uso, nadie lo volvería a levantar.
        //
        // Borrando antes, la ventana pasa de «lo que dure el drenaje» a
        // microsegundos, y lo que este daemon borra es siempre suyo. Nadie
        // pierde nada: con el listener ya muerto, la ruta solo servía para dar
        // `ECONNREFUSED` en vez de `NotFound`.
        let socket_path = self.socket_path.clone();
        let socket_para_borrar = socket_path.clone();
        // Regla 2: ni un unlink síncrono en el runtime.
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(socket_para_borrar)).await;
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
        let _ = socket_path;
        tracing::info!("daemon apagado");
        match fallo {
            Some(e) => Err(DaemonError::Io(e)),
            None => Ok(()),
        }
    }

    /// Token que CANCELA las tasks vivas además de apagar (segunda señal
    /// del binario, o `stop --hard`).
    #[must_use]
    pub fn hard_shutdown_token(&self) -> CancellationToken {
        self.shared.hard_shutdown.clone()
    }
}

/// Toma el lock de la sesión de UI y la carga (L2).
///
/// Sin `state_dir` no se persiste nada: ni lock ni fichero. Con él, el lock
/// decide quién ESCRIBE —quien no lo consigue arranca igual, con la misma
/// pantalla, y no la escribe nunca— y la carga nunca falla: sus desenlaces
/// malos dan una sesión vacía y un aviso.
///
/// Todo el I/O va a un pool blocking (regla 2).
async fn open_session(
    state_dir: Option<PathBuf>,
) -> Result<
    (
        Option<crate::ui_session::disk::SessionLock>,
        norte_proto::methods::Session,
        bool,
    ),
    DaemonError,
> {
    let Some(dir) = state_dir else {
        return Ok((None, norte_proto::methods::Session::default(), false));
    };
    tokio::task::spawn_blocking(move || {
        // Un lock que no se puede ni intentar (permisos, disco lleno) NO
        // impide arrancar: deja al core suelto, que es la degradación que ya
        // existe para el segundo core.
        let lock = crate::ui_session::disk::lock(&dir).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "no se pudo tomar el lock de la sesión de UI");
            None
        });
        let r = crate::ui_session::disk::load_or_default(&dir);
        (lock, r.session, r.writable)
    })
    .await
    .map_err(|e| DaemonError::Io(std::io::Error::other(e)))
}

/// El único escritor de la sesión de UI: coalesce los cambios y los vuelca.
///
/// Un tick por segundo, y en cada uno **solo si hay algo sucio**. Coalescer es
/// el punto entero: el cursor se mueve en cada flecha y esto es un fichero, no
/// una base de datos. `session_flush` lo adelanta cuando se va la última
/// conexión, y el token lo termina — con un último volcado, que es el del
/// relevo y la razón de ser de todo esto.
///
/// Un fallo de escritura es un `warn!`, la marca de sucio VUELVE a ponerse y
/// el siguiente tick reintenta: perder una sesión es una tarde mala, y tumbar
/// el daemon por ella es peor. (Reintentar de verdad es lo que hace
/// `mark_dirty`; sin él la marca ya estaba limpia y el aviso era todo lo que
/// pasaba.)
async fn session_writer(
    store: Arc<crate::ui_session::SessionStore>,
    dir: PathBuf,
    flush: Arc<tokio::sync::Notify>,
    stop: CancellationToken,
    persiste: Arc<AtomicBool>,
    inicial: EstadoEscritura,
) {
    let mut estado = inicial;
    if matches!(estado, EstadoEscritura::Rendida) {
        // El bind tenía el lock y el fichero era de un binario más nuevo: se
        // soltó al construir el estado y no se vuelve a intentar. La task
        // TERMINA aquí en vez de girar un temporizador por segundo durante
        // toda la vida del daemon para no hacer nada con él.
        persiste.store(false, Ordering::Release);
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // El reintento va en el TICK y solo aquí: es el único de los
                // tres despertares que ocurre pase lo que pase. Con el apagado
                // ya pedido no se intenta: tomar el lock justo entonces cuesta
                // una adquisición y un volcado en el peor momento —un relevo,
                // donde el sucesor está sondeando ese mismo lock— y `select!`
                // puede elegir esta rama con el token ya cancelado.
                if !stop.is_cancelled() {
                    estado = estado.reintenta(&dir, &store, &persiste).await;
                }
                if estado.escribe() {
                    flush_session(&store, &dir).await;
                }
            }
            () = flush.notified() => {
                if estado.escribe() {
                    flush_session(&store, &dir).await;
                }
            }
            () = stop.cancelled() => {
                // El último, y por eso el que importa: aquí es donde la
                // pantalla sobrevive a un relevo.
                if estado.escribe() {
                    flush_session(&store, &dir).await;
                }
                break;
            }
        }
    }
    // Explícito: el lock se suelta al terminar la task, y el apagado ordenado
    // la ESPERA justo por esto — el sucesor del relevo tiene que encontrar el
    // fichero escrito y el lock libre.
    drop(estado);
}

/// El derecho a escribir la sesión, visto por el escritor (#237).
///
/// Tres estados y no un `Option<SessionLock>`, porque «todavía no lo tengo» y
/// «no lo voy a tener nunca» son decisiones distintas: la primera se reintenta
/// cada tick y la segunda no se reintenta jamás.
enum EstadoEscritura {
    /// Con el lock: esta instancia es la que escribe. El lock no se LEE nunca
    /// —vale por su `Drop`, que es soltarlo—, de ahí el nombre.
    Duena {
        /// El derecho, vivo mientras dure el estado.
        _lock: crate::ui_session::disk::SessionLock,
    },
    /// Sin el lock, y volviéndolo a intentar. `avisado` para que el aviso
    /// salga una vez y no una por segundo; `ticks` cuenta los intentos para
    /// espaciarlos ([`EstadoEscritura::toca_intentar`]).
    Suelta { avisado: bool, ticks: u32 },
    /// En disco hay una sesión de un binario MÁS NUEVO. No se pisa y no se
    /// reintenta en toda la vida del proceso.
    ///
    /// **Y eso no es gratis**: si el fichero del futuro se borra o lo
    /// reemplaza después uno legible —una vuelta atrás de versión, un `rm` a
    /// mano—, este daemon sigue contestando `owner: false` y cada ventana suya
    /// sigue diciendo «no se está guardando» hasta que se reinicie. Se acepta
    /// porque es lo mismo que hace el `rendido` del brazo embebido, y porque
    /// un binario más nuevo en marcha es la situación normal de ese estado.
    Rendida,
}

impl EstadoEscritura {
    /// El estado con el que arranca el escritor, a partir de lo que consiguió
    /// el bind.
    ///
    /// Con el lock tomado pero un fichero del futuro se suelta AQUÍ: retenerlo
    /// dejaría el fichero de rehén de un core que no puede escribirlo, que es
    /// lo que hacía el daemon antes de #237.
    fn inicial(lock: Option<crate::ui_session::disk::SessionLock>, escribible: bool) -> Self {
        match (lock, escribible) {
            (Some(lock), true) => Self::Duena { _lock: lock },
            (Some(lock), false) => {
                drop(lock);
                Self::Rendida
            }
            (None, _) => Self::Suelta {
                avisado: false,
                ticks: 0,
            },
        }
    }

    /// ¿Escribe este proceso?
    fn escribe(&self) -> bool {
        matches!(self, Self::Duena { .. })
    }

    /// Cuántos ticks se intenta el lock uno por segundo antes de espaciar.
    ///
    /// El caso que importa es un RELEVO: el daemon viejo se va segundos
    /// después de que arranque el nuevo, y ahí un segundo de latencia es la
    /// diferencia entre guardar la pantalla y perderla. Pasado ese minuto, lo
    /// que hay es un core ajeno que puede durar horas, y seguir sondeando cada
    /// segundo es un `mkdir`+`open`+`flock` por segundo para siempre.
    const RAFAGA: u32 = 60;
    /// Cadencia después de la ráfaga: la misma que el brazo embebido.
    const ESPACIADO: u32 = 30;

    /// ¿Toca intentarlo en este tick?
    fn toca_intentar(ticks: u32) -> bool {
        ticks < Self::RAFAGA || ticks.is_multiple_of(Self::ESPACIADO)
    }

    /// Vuelve a intentar el lock si aún no se tiene (#237).
    ///
    /// Cada segundo durante el primer minuto y cada treinta después
    /// ([`Self::toca_intentar`]). El aviso sale una sola vez.
    ///
    /// **El fichero se lee SOLO con el lock ya en la mano.** Leerlo antes de
    /// saber si se consiguió —que es lo que hacía la primera versión— es un
    /// `read` y un parseo de hasta un mega por segundo cuyo resultado se tira,
    /// y peor: `load_or_default` AVISA de un fichero corrupto o del futuro, así
    /// que un daemon permanentemente suelto escribía ese `warn!` una vez por
    /// segundo para siempre, enterrando todo lo demás del log. El brazo
    /// embebido nunca lo hizo así.
    ///
    /// Al conseguirlo tarde se re-lee el fichero, exactamente como el brazo
    /// embebido: si lo escribió un binario más nuevo se suelta el lock recién
    /// tomado y se abandona; si lo escribió otro core de esta versión, su
    /// documento es el vigente y se adopta ENTERO —cuerpo incluido—, o esta
    /// instancia contestaría su propia pantalla con el número del otro y lo
    /// que el otro guardó desaparecería sin que nada lo notara.
    async fn reintenta(
        self,
        dir: &Path,
        store: &Arc<crate::ui_session::SessionStore>,
        persiste: &Arc<AtomicBool>,
    ) -> Self {
        let Self::Suelta { avisado, ticks } = self else {
            return self;
        };
        let siguiente = ticks.saturating_add(1);
        if !Self::toca_intentar(ticks) {
            return Self::Suelta {
                avisado,
                ticks: siguiente,
            };
        }
        let d = dir.to_path_buf();
        // Regla 2: el lock y la lectura son I/O de disco.
        let intento = tokio::task::spawn_blocking(move || {
            let Some(lock) = crate::ui_session::disk::lock(&d)? else {
                return Ok::<_, std::io::Error>(None);
            };
            // Con el lock puesto, y no antes.
            let recuperada = crate::ui_session::disk::load_or_default(&d);
            Ok(Some((lock, recuperada)))
        })
        .await;
        let tomado = match intento {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                if !avisado {
                    tracing::warn!(error = %e, "no se pudo tomar el lock de la sesión de UI");
                }
                return Self::Suelta {
                    avisado: true,
                    ticks: siguiente,
                };
            }
            Err(e) => {
                if !avisado {
                    tracing::warn!(error = %e, "el intento de lock de la sesión de UI se cayó");
                }
                return Self::Suelta {
                    avisado: true,
                    ticks: siguiente,
                };
            }
        };
        let Some((lock, recuperada)) = tomado else {
            return Self::Suelta {
                avisado,
                ticks: siguiente,
            };
        };
        if !recuperada.writable {
            drop(lock);
            persiste.store(false, Ordering::Release);
            tracing::warn!("la sesión de UI en disco es de un binario más nuevo: no se escribe");
            return Self::Rendida;
        }
        store.adopt_from_disk(recuperada.session);
        persiste.store(true, Ordering::Release);
        tracing::info!("la sesión de UI quedó libre: este daemon vuelve a guardarla");
        Self::Duena { _lock: lock }
    }
}

/// Vuelca la sesión si hay algo que volcar. Nada sucio = ni un `open`, que es
/// lo que hace barato despertarse cada segundo.
async fn flush_session(store: &Arc<crate::ui_session::SessionStore>, dir: &Path) {
    let Some(session) = store.take_dirty() else {
        return;
    };
    let dir = dir.to_path_buf();
    // Regla 2: la escritura es I/O de disco y va a un pool blocking.
    let escrito =
        tokio::task::spawn_blocking(move || crate::ui_session::disk::write(&dir, &session)).await;
    match escrito {
        Ok(Ok(())) => {}
        // Lo sucio se lo llevó `take_dirty`, así que un fallo SIN volver a
        // marcarlo no se reintenta nunca: el tick siguiente no vería nada que
        // hacer y la pantalla se perdería por un `ENOSPC` de un segundo.
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "no se pudo escribir la sesión de UI; se reintenta");
            store.mark_dirty();
        }
        Err(e) => {
            tracing::warn!(error = %e, "el volcado de la sesión de UI se cayó; se reintenta");
            store.mark_dirty();
        }
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
        // Con la última conexión fuera no hay nadie a quien servir, y lo que
        // acaba de dejar puesto no debería esperar al siguiente tick.
        if shared.connections.fetch_sub(1, Ordering::SeqCst) == 1 {
            shared.session_flush.notify_one();
        }
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
    /// El ancla del directorio listado (#295), capturada al ABRIR por el mismo
    /// motivo que `skipped`: una página no es un directorio distinto, y el
    /// cliente puede engancharse en cualquiera.
    dir_anchor: Option<norte_proto::DirAnchor>,
    /// Petición de attrs RESUELTA al abrir el listado (#108 bloque 2): el
    /// stream nació con ella, así que las continuaciones la reusan para el
    /// cinturón de emisión (los `attrs` de una continuación se ignoran).
    attrs: norte_vfs::AttrRequest,
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

/// Valida la petición de attrs del wire (#108 bloque 2, ADR 0039 §4): id
/// malformado o más de `ATTRS_MAX_REQUEST` (el deserializador materializa
/// 16+1 como testigo) = `-32602`. Pedir un id VÁLIDO pero desconocido NO es
/// error (viene ausente — un cliente con catálogo rancio degrada).
fn validate_attr_request(ids: &[String]) -> Result<(), RpcError> {
    if ids.len() > norte_proto::ATTRS_MAX_REQUEST {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!(
                "attrs: at most {} ids per call",
                norte_proto::ATTRS_MAX_REQUEST
            ),
        ));
    }
    if let Some(bad) = ids.iter().find(|id| !norte_proto::is_valid_attr_id(id)) {
        // `escape_debug`: el id inválido es entrada hostil — jamás crudo en
        // un mensaje de error (controles, RTL, invisibles).
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("attrs: malformed id \"{}\"", bad.escape_debug()),
        ));
    }
    Ok(())
}

/// Valida `p.attrs` y lo cruza con el catálogo del provider de `path`:
/// devuelve la petición que de verdad viaja al provider. Un id válido pero
/// NO anunciado se cae aquí — el daemon solo reenvía ids que el provider
/// anuncia, jamás inventa celdas (ADR 0039 §1).
///
/// Dos resoluciones de provider (catálogo aquí, `list_with`/`stat_with` en
/// el handler): si la conexión se remapea entre ambas, la petición filtrada
/// puede no casar con el catálogo nuevo — degrada a AUSENCIA, que es
/// contrato-legal. No "arreglar" con un lookup único bajo lock.
async fn resolve_attr_request(
    ids: &[String],
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<norte_vfs::AttrRequest, RpcError> {
    validate_attr_request(ids)?;
    if ids.is_empty() {
        return Ok(norte_vfs::AttrRequest::default());
    }
    let advertised = shared
        .engine
        .attr_catalog(path)
        .await
        .map_err(RpcError::from)?;
    Ok(norte_vfs::AttrRequest::sanitized(
        ids.iter()
            .filter(|id| advertised.iter().any(|a| &a.id == *id))
            .cloned(),
    ))
}

/// Cinturón de emisión (ADR 0039 §5): delega en el belt compartido de
/// `AttrRequest` — mismo filtro que aplica el backend embebido, así ninguna
/// ruta (wire o in-process) emite ids no pedidos, valores sobre tope o
/// `Unknown`.
fn enforce_attr_caps(entry: &mut norte_proto::Entry, allowed: &norte_vfs::AttrRequest) {
    allowed.retain_conforming(entry);
}

/// El handler de `fs.stat` (#108 bloque 2): valida la petición de attrs, la
/// cruza con lo anunciado, materializa y aplica el cinturón de emisión. El
/// `read_gate` (#80) lo aplica el arm del dispatch.
async fn handle_fs_stat(
    p: methods::FsStatParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let request = resolve_attr_request(&p.attrs, &p.path, shared).await?;
    let mut entry = shared
        .engine
        .stat_with(
            &p.path,
            &norte_vfs::ListOptions {
                attrs: request.clone(),
            },
        )
        .await
        .map_err(RpcError::from)?;
    enforce_attr_caps(&mut entry, &request);
    to_value(&methods::FsStatResult { entry })
}

/// El handler de `fs.list` con paginación por cursor (ADR 0017). Cláusula ADR
/// 0004: sin `cursor` NI `limit` drena el listado COMPLETO con `next_cursor:
/// null` (un cliente 0.7 recibe exactamente lo de antes).
///
/// Attrs (#108 bloque 2): la petición se valida y resuelve AL ABRIR; una
/// continuación por cursor IGNORA `p.attrs` (el stream retenido nació con
/// sus opciones — re-mandarlos no cambia nada, la validación sí corre).
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

    // La validación de attrs corre SIEMPRE (también con cursor: un id
    // malformado es -32602 aunque la continuación no lo use).
    validate_attr_request(&p.attrs)?;

    // Continuación: el cursor es el id opaco de un listado retenido.
    if let Some(cur) = &p.cursor {
        return continue_listing(cur, &p.path, cap, now, conn, entries).await;
    }

    // Listado NUEVO (sin cursor): solo ids anunciados viajan al provider.
    let request = resolve_attr_request(&p.attrs, &p.path, shared).await?;
    let opt = norte_vfs::ListOptions {
        attrs: request.clone(),
    };
    let mut stream = shared
        .engine
        .list_with(&p.path, &opt)
        .await
        .map_err(RpcError::from)?;
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
    // El ancla del directorio (#295), también UNA vez al abrir. Un fallo aquí
    // NO tumba el listado: degrada a `None`, que es lo que dice un provider
    // que no sabe dar identidad, y entonces el cliente no manda ancla y la
    // escritura se comporta como en 0.53. Ir al revés —negarse a listar
    // porque no se puede anclar— dejaría sin listar un bucket entero por una
    // comprobación que ese destino no puede dar.
    let dir_anchor = shared.engine.dir_anchor(&p.path).await.unwrap_or_else(|e| {
        tracing::debug!(error = %e, "no se pudo anclar el directorio listado (#295)");
        None
    });
    match drain_page(&mut stream, cap, &mut entries).await {
        Ok(Drained::Done) => {
            for e in &mut entries {
                enforce_attr_caps(e, &request);
            }
            to_value(&methods::FsListResult {
                entries,
                next_cursor: None,
                skipped,
                dir_anchor,
            })
        }
        Ok(Drained::More) => {
            // Presión GLOBAL (M1): por encima del tope no se retiene — se drena
            // el resto EN LÍNEA y se devuelve completo (libera el hilo blocking
            // del productor al instante). Degrada a listado-completo, nunca
            // agota el pool ni trunca.
            if shared.open_listings.load(Ordering::SeqCst) >= GLOBAL_MAX_LISTINGS {
                match drain_page(&mut stream, None, &mut entries).await {
                    Ok(_) => {
                        for e in &mut entries {
                            enforce_attr_caps(e, &request);
                        }
                        return to_value(&methods::FsListResult {
                            entries,
                            next_cursor: None,
                            skipped,
                            dir_anchor,
                        });
                    }
                    Err(e) => return Err(RpcError::from(e)),
                }
            }
            conn.evict_if_full();
            shared.open_listings.fetch_add(1, Ordering::SeqCst);
            let id = conn.next_listing_id;
            conn.next_listing_id += 1;
            for e in &mut entries {
                enforce_attr_caps(e, &request);
            }
            conn.listings.insert(
                id,
                OpenListing {
                    path: p.path,
                    stream,
                    last_used: now,
                    skipped,
                    dir_anchor: dir_anchor.clone(),
                    attrs: request,
                    _guard: ListingGuard {
                        global: Arc::clone(&shared.open_listings),
                    },
                },
            );
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
                skipped,
                dir_anchor,
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
    let (drained, skipped, dir_anchor) = {
        let listing = conn.listings.get_mut(&id).ok_or_else(cursor_expired)?;
        if listing.path != *path {
            return Err(RpcError::protocol(
                codes::INVALID_PARAMS,
                "cursor does not belong to this path",
            ));
        }
        let skipped = listing.skipped;
        // El ancla del ABRIR, repetida en cada página (#295): volver a
        // preguntarla aquí contestaría por el directorio de AHORA, que es
        // justo lo que el ancla existe para no confundir con el de entonces.
        let dir_anchor = listing.dir_anchor.clone();
        let drained = drain_page(&mut listing.stream, cap, &mut entries).await;
        // Cinturón de emisión con la petición del ABRIR (#108 bloque 2).
        let allowed = listing.attrs.clone();
        for e in &mut entries {
            enforce_attr_caps(e, &allowed);
        }
        (drained, skipped, dir_anchor)
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
                dir_anchor,
            })
        }
        Ok(Drained::Done) => {
            conn.listings.remove(&id);
            to_value(&methods::FsListResult {
                entries,
                next_cursor: None,
                skipped,
                dir_anchor,
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
    // La dueña de la sesión de UI la SUELTA al irse: sin esto, un cliente que
    // muere deja la pantalla de rehén y el siguiente terminal corre suelto
    // para siempre. Soltar lo ajeno es un no-op (no desaloja a nadie).
    shared.ui_session.release(conn_id);
    drop_sync_plans(shared, conn_id).await;
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
                methods::FS_COPY
                    | methods::FS_MOVE
                    | methods::FS_DELETE
                    | methods::FS_MKDIR
                    | methods::FS_CREATE
                    | methods::AI_RENAME_PLAN
                    | methods::INDEX_SEARCH_SEMANTIC
                    // 0.36.0: `fs.rename_batch` gatea el lote ENTERO antes de
                    // reservar nada, así que puede quedarse suspendido en un Ask
                    // exactamente igual que un fs.move; y `fs.rename_batch_plan`
                    // lista y planifica un directorio dentro del despacho, como
                    // `ai.rename_plan`. Retirar cualquiera de los dos es seguro
                    // por construcción: el gate muere PRE-efecto, y entre el
                    // submit del engine y el register no hay ningún `.await`
                    // (#64).
                    //
                    // Retirar es retirar la RESPUESTA, no el trabajo: el
                    // planificador ya corre en `spawn_blocking` y dropear su
                    // `JoinHandle` no lo para — termina solo, acotado por
                    // `RENAME_BATCH_MAX_LISTING` y por el tope de parejas. El
                    // cliente deja de esperar; la CPU ya gastada no vuelve.
                    | methods::FS_RENAME_BATCH
                    | methods::FS_RENAME_BATCH_PLAN
                    // 0.40.0: `sync.apply` gatea las DOS raíces del plan antes
                    // de escribir un byte, así que se suspende en un `ask`
                    // igual que un fs.copy — y su espera es la más cara de
                    // todas, porque el cliente no tiene nada que hacer mientras
                    // tanto. Retirarlo es seguro: el gate muere PRE-efecto, y el
                    // derecho a aplicar el plan —que `Spool::open` ya se
                    // cobró— lo devuelve el `Drop` de `ApplyClaim` en el
                    // engine, que existe exactamente para este camino.
                    | methods::SYNC_APPLY
                    // #248: las LECTURAS puras, y por otra razón. Aquí no hay
                    // efecto que dejar a medias —una lectura no escribe nada,
                    // así que dropear su dispatch no puede dejar rastro—; lo
                    // que se libera es la CONEXIÓN. `serve_connection` despacha
                    // en serie, así que un `fs.list` que el cliente abandonó
                    // (el presupuesto de cinco segundos del arranque, #235)
                    // seguía corriendo contra un provider colgado y TODAS las
                    // peticiones siguientes esperaban detrás de él, muriendo
                    // una a una en su `CALL_TIMEOUT` de 30 s. El cliente ya
                    // manda el `rpc.cancel` (guard de `call_timed_guarded`);
                    // sin este brazo no lo escuchaba nadie.
                    | methods::FS_LIST
                    | methods::FS_STAT
                    | methods::FS_READ
                    | methods::FS_CAPABILITIES
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

/// Se lleva los PLANES de sincronización que dejó retenidos una conexión que se
/// cierra (ADR 0049, tercera de las cuatro muertes del spool).
///
/// Un plan aprobable pertenece a la conexión que lo produjo, así que sin ella no
/// lo puede aplicar nadie; y lo que quedaría en disco es un listado relativo de
/// dos árboles enteros bajo el directorio de estado.
///
/// El DERECHO a aplicarlo se olvida en memoria aunque el borrado falle, así que
/// un fallo aquí deja basura en disco y no un plan vivo: se registra y no rompe
/// el cierre de la conexión.
async fn drop_sync_plans(shared: &Arc<Shared>, conn_id: u64) {
    let Some(spool) = shared.engine.spool() else {
        return;
    };
    match spool.drop_connection(conn_id).await {
        Ok(report) if report.is_clean() => {}
        Ok(report) => tracing::warn!(
            conn = conn_id,
            failed = report.failed,
            "planes de sync que no se dejaron borrar al cerrar la conexión"
        ),
        Err(e) => tracing::warn!(conn = conn_id, error = %e, "barrido de planes de sync"),
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
        methods::SESSION_GET => handle_session_get(&conn.actor, conn_id, shared),
        methods::SESSION_PUT => {
            let p: methods::SessionPutParams = parse_params(req.params)?;
            handle_session_put(&conn.actor, conn_id, p, shared)
        }
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
        // host.volumes (0.37.0, #131): enumeración de los volúmenes del HOST,
        // SOLO para una conexión User (diseño §C de
        // `2026-08-10-volumes-design.md`) — la tabla de montaje nombra los
        // discos, servidores y medios extraíbles del humano, y un agente bajo
        // scope no lo necesita para nada. El gate va ANTES del parseo (mismo
        // criterio que `index.embed`/`index.search_semantic`/`ai.rename_plan`,
        // MAJOR de la review V2): un agente ve `PolicyDenied` sea cual sea la
        // validez de sus params, y jamás un `INVALID_PARAMS` que le dejara
        // distinguir "vedado" de "params malos" fuzzeando el único campo.
        // Sin params definidos más allá del bool con default: null/ausente se
        // acepta (ADR 0004), mismo patrón que `task.list`/`plugin.list`.
        methods::HOST_VOLUMES => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::HostVolumesParams = parse_params(
                req.params
                    .filter(|v| !v.is_null())
                    .or_else(|| Some(serde_json::json!({}))),
            )?;
            handle_host_volumes(p).await
        }
        // `connection.list` (0.56.0, #264): los nombres de `connections.toml`,
        // para que un frontend ofrezca un selector sin leer ese fichero él
        // mismo — leerlo le costaría la pila de red entera.
        //
        // El gate va ANTES del parseo, mismo criterio que `host.volumes` y por
        // el mismo motivo: la lista nombra los servidores del humano, y un
        // agente no distingue «vedado» de «params malos» fuzzeando nada. Sin
        // params definidos: null o ausente se acepta (ADR 0004).
        methods::CONNECTION_LIST => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            handle_connection_list().await
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
        // plugin.help (H3e): ABIERTO, mismo criterio que plugin.list.
        methods::PLUGIN_HELP => handle_plugin_help(req.params, shared).await,
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
    // Un RELEVO con tasks vivas se rehúsa AQUÍ, que es el único momento en el
    // que queda alguien a quien contestar: la respuesta de este método sale en
    // el acto, así que una negativa decidida después de esperar no tendría
    // destinatario — y para entonces el listener ya habría dejado de aceptar,
    // con lo que «rehusar» significaría volver a aceptar.
    //
    // Esperar y NO cancelar es lo que separa este eje de `graceful`: una copia
    // muerta a mitad de árbol es justo el estropicio que el journal tiene luego
    // que limpiar. Quien de verdad quiera cancelarlas ya tiene `graceful:
    // false`, y no se abre una segunda puerta a la misma habitación.
    // Contar y APAGAR bajo el mismo lock: entre soltarlo y cancelar, otra
    // conexión puede registrar una task, y entonces el relevo empezaría
    // igualmente con una copia viva — que es exactamente lo que se está
    // rehusando. El lock es de `std` y todo lo que hay dentro es síncrono.
    let vivas = shared.tasks.lock().expect("tasks lock sano");
    if p.mode == methods::ShutdownMode::Handover && !vivas.is_empty() {
        let cuantas = vivas.len();
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            format!(
                "a handover needs an idle daemon: {cuantas} task(s) still running. \
                 Wait for them; a handover never cancels a task, not even with \
                 graceful:false — use a plain stop for that"
            ),
        ));
    }
    // El aviso va ANTES de dejar de aceptar, o no llega a nadie: `shutdown`
    // corta el bucle de accept y las conexiones se van detrás.
    //
    // A TODAS las conexiones, no solo a las humanas: la sesión de un agente
    // muere con este daemon igual que la de un humano, y necesita saber si
    // volver. Es información sobre el TRANSPORTE, no sobre gobierno.
    let aviso = methods::DaemonGoingAway {
        reconnect: p.mode == methods::ShutdownMode::Handover,
    };
    if let Ok(params) = serde_json::to_value(aviso) {
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::DAEMON_GOING_AWAY.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            shared.broadcast_all(&Arc::from(frame.into_boxed_slice()));
        }
    }
    if !p.graceful {
        shared.hard_shutdown.cancel();
    }
    shared.shutdown.cancel();
    drop(vivas);
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

/// `session.get` (L2, 0.48.0) — la pantalla que el cliente dejó, y si ESTA
/// conexión es la que puede escribirla.
///
/// Reclamar la propiedad es parte de LEER, y no un método aparte: quien
/// arranca lee, y quien lee es el candidato natural a escribir. La primera
/// conexión humana se la queda; las siguientes reciben la misma copia y corren
/// sueltas —abrir un segundo terminal da lo que el lector esperaba, y nunca
/// hay dos escritores sobre un estado—.
///
/// SOLO humanos, mismo criterio que `daemon.shutdown` y `policy.pending`: una
/// sesión de agente no tiene pantalla que guardar.
fn handle_session_get(
    actor: &Actor,
    conn_id: u64,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection has a UI session",
        ));
    }
    // Dueña Y con dónde escribir: un daemon sin `state_dir`, sin el lock, o
    // que encontró en disco una sesión de un binario más nuevo, acepta `put`
    // en memoria y no persiste nada. Decir `owner: true` ahí sería mandar al
    // cliente a escribir cada segundo una pantalla que se pierde al salir, sin
    // un solo aviso — y el brazo embebido ya contestaba lo correcto, así que
    // era además la MISMA pregunta con dos respuestas.
    let owner = shared.ui_session.claim(conn_id) && shared.session_persists.load(Ordering::Acquire);
    to_value(&methods::SessionGetResult {
        session: shared.ui_session.get(),
        owner,
    })
}

/// `session.put` (L2, 0.48.0) — reemplaza la sesión entera.
///
/// Cuatro negativas, y cada una dice algo distinto al cliente:
/// [`norte_proto::Error::Cancelled`] si el daemon se está apagando (no hay a
/// dónde escribir ya; contra el sucesor del relevo la misma escritura vale),
/// [`norte_proto::Error::PermissionDenied`] si no es la dueña (releer no
/// arregla nada: esta conexión no escribe nunca),
/// [`norte_proto::Error::Conflict`] con
/// [`norte_proto::ConflictKind::StaleRevision`] si trae una revisión pasada
/// (releer SÍ lo arregla) y [`norte_proto::Error::LimitExceeded`] con
/// [`norte_proto::Error::LIMIT_SESSION_BODY`] si el cuerpo pasa del tope
/// (releer no; tirar historial y reintentar, sí). En los tres casos lo
/// almacenado se queda exactamente como estaba.
/// Por qué un `session.put` no llega a la sesión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPutVeto {
    /// Una sesión de agente no tiene pantalla que guardar.
    NoEsHumano,
    /// Otra conexión es la dueña.
    NoEsLaDuena,
    /// Este CORE no escribe: sin `state_dir`, sin el lock, o con una sesión de
    /// un binario más nuevo en disco.
    SinEscritor,
}

impl From<SessionPutVeto> for RpcError {
    fn from(v: SessionPutVeto) -> Self {
        match v {
            SessionPutVeto::NoEsHumano => Self::protocol(
                codes::INVALID_REQUEST,
                "only a human (non-agent) connection has a UI session",
            ),
            // La MISMA taxonomía que «no eres la dueña», y a propósito: al
            // cliente le da igual cuál de las dos cosas le falta, y las dos se
            // arreglan igual —volver a preguntar—. La distinción vive en el
            // log, no en el wire.
            SessionPutVeto::NoEsLaDuena | SessionPutVeto::SinEscritor => {
                Self::from(norte_proto::Error::PermissionDenied)
            }
        }
    }
}

/// Quién puede escribir la sesión, en el orden en que se pregunta.
///
/// Pura y aparte del handler porque las dos condiciones interesantes son
/// CARRERAS —el apagado y el relevo de dueña— y una carrera no se prueba
/// provocándola: se prueba decidiendo sobre los mismos cuatro datos. Es lo que
/// se hizo con el gate de SIGINT del CLI (#212).
///
/// El orden no es cosmético. Humano primero, porque un agente no debería ni
/// enterarse de si hay dueña. Y la propiedad antes de la revisión y del tope,
/// porque a quien no manda las otras dos respuestas le darían consejos falsos
/// («re-lee», «recorta») sobre una escritura que jamás se va a aceptar.
///
/// El apagado NO está aquí (#233): comprobarlo contra el token, fuera del lock
/// que protege la mutación, dejaba la ventana que pretendía cerrar. Lo cierra
/// [`crate::ui_session::SessionStore::seal`], bajo el mismo lock que el `put`.
///
/// **`persiste` es el tercero, y lo añadió la revisión de #237.** El brazo
/// embebido ya rehusaba el `put` de un proceso suelto —«no escribe NI en
/// memoria»—; el daemon lo aceptaba y contestaba una revisión nueva, y eso
/// dejó de ser inocuo en cuanto su escritor pudo tomar el lock TARDE: un
/// cuerpo aceptado mientras estaba suelto, con la revisión ya por delante de
/// la del disco, sobrevivía a `adopt_from_disk` (que declina cuando la local
/// va por delante, dejando la marca de sucio puesta) y se publicaba encima de
/// la pantalla del otro core en el mismo tick. Y como la revisión solo sube,
/// nada podía detectarlo después.
fn session_put_veto(
    es_humano: bool,
    duena: Option<u64>,
    conn_id: u64,
    persiste: bool,
) -> Option<SessionPutVeto> {
    if !es_humano {
        return Some(SessionPutVeto::NoEsHumano);
    }
    if duena != Some(conn_id) {
        return Some(SessionPutVeto::NoEsLaDuena);
    }
    if !persiste {
        return Some(SessionPutVeto::SinEscritor);
    }
    None
}

fn handle_session_put(
    actor: &Actor,
    conn_id: u64,
    p: methods::SessionPutParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if let Some(veto) = session_put_veto(
        matches!(actor, Actor::User),
        shared.ui_session.owner(),
        conn_id,
        shared.session_persists.load(Ordering::Acquire),
    ) {
        return Err(veto.into());
    }
    match shared.ui_session.put(p.version, p.revision, p.body) {
        Ok(revision) => to_value(&methods::SessionPutResult { revision }),
        Err(crate::ui_session::PutError::Conflict { current }) => {
            // Con TAXONOMÍA en `data` (#182): sin ella el cliente lee
            // «internal error» y no sabe que releer lo arregla. La revisión
            // vigente NO viaja en el error — se pide con `session.get`, que es
            // el mismo viaje que hay que hacer de todas formas para saber
            // contra qué cuerpo se estaba escribiendo.
            tracing::debug!(current, "session.put con revisión rancia");
            Err(RpcError::from(norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::StaleRevision,
            }))
        }
        // La sesión ya se cerró: el cliente sí mandaba y su escritura llegó
        // tarde, así que `Cancelled` — contra el sucesor de un relevo, la misma
        // escritura vale.
        Err(crate::ui_session::PutError::Sealed) => {
            tracing::debug!("session.put tras el cierre de la sesión");
            Err(RpcError::from(norte_proto::Error::Cancelled))
        }
        Err(crate::ui_session::PutError::TooLarge { bytes }) => {
            tracing::warn!(bytes, "session.put por encima del tope");
            Err(RpcError::from(norte_proto::Error::LimitExceeded {
                limit: norte_proto::Error::LIMIT_SESSION_BODY.to_owned(),
            }))
        }
        // `Unsupported` y no `InvalidPath`: lo que falta no es un parámetro
        // bien formado, es un core capaz de leer ese esquema — «tu daemon es
        // más viejo», que es exactamente lo que ese error dice en el resto del
        // wire (#247).
        Err(crate::ui_session::PutError::UnknownSchema { version, known }) => {
            tracing::warn!(version, known, "session.put de un esquema desconocido");
            Err(RpcError::from(norte_proto::Error::Unsupported))
        }
    }
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
    // Los tres modos de fallo viajan DISTINTOS (#279): «tu clic no llegó»,
    // «llegaste tarde» y «esa aprobación no es de este daemon» piden respuestas
    // distintas de quien mira la pantalla, y antes se colapsaban en un
    // `INVALID_PARAMS` con el motivo dentro de un `message` en inglés que
    // ningún frontend puede clasificar.
    use crate::daemon::approvals::Decision;
    let reason = match shared.approvals.decide(p.approval_id, p.approve) {
        Decision::Aplicada => {
            // Efecto de seguridad (material de auditoría M3-5): quién decidió qué.
            tracing::info!("aprobación de policy decidida por el humano");
            return to_value(&methods::PolicyDecideResult {});
        }
        Decision::Vencida => "expired",
        Decision::YaDecidida => "already-decided",
        Decision::Desconocida => "unknown",
    };
    Err(RpcError::from(norte_proto::Error::ApprovalGone {
        reason: reason.to_owned(),
    }))
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
        // 0.36.0: deshacer un LOTE puede quedarse a medias, y eso no es un
        // `blocked` — `blocked` dice «paré y el árbol está consistente». Sin
        // estos dos campos, el humano REMOTO cuyo undo dejó un directorio medio
        // renombrado veía exactamente lo mismo que uno que fue bien.
        batch_stuck: snapshot
            .batch_stuck
            .as_ref()
            .map(crate::rename::stuck_to_proto),
        compensations_lost: snapshot.compensations_lost,
        // #171: lo que la policy denegó unidad a unidad. Va aparte de
        // `blocked` porque dice lo contrario que él — el undo NO paró.
        denied: snapshot
            .denied
            .into_iter()
            .map(|(seq, error)| methods::UndoBlocked { seq, error })
            .collect(),
        denied_total: snapshot.denied_total,
    })
}

/// `host.volumes` (0.37.0, #131): enumeración de los volúmenes del HOST. El
/// gate de actor (SOLO `User` — diseño §C de `2026-08-10-volumes-design.md`)
/// vive en el brazo de `dispatch` que llama a esta función, ANTES del
/// parseo de params (ver el comentario de ese brazo): esta función solo
/// corre para una conexión ya autorizada, así que no vuelve a comprobar el
/// actor. No hay `Provider`/engine que consultar —
/// [`crate::volumes::enumerate`] es una función libre del HOST (diseño §A).
async fn handle_host_volumes(p: methods::HostVolumesParams) -> Result<serde_json::Value, RpcError> {
    let volumes = crate::volumes::enumerate(p.include_pseudo)
        .await
        .map_err(|_| RpcError::from(norte_proto::Error::Io { retryable: false }))?;
    to_value(&methods::HostVolumesResult {
        volumes: volumes
            .into_iter()
            .map(crate::backend::volume_to_proto)
            .collect(),
    })
}

/// `connection.list` (0.56.0, #264): las conexiones nombradas del daemon.
///
/// Lee el `connections.toml` del DAEMON, que es lo que hace útil el método: el
/// frontend no lo tiene y no debería tenerlo. Un fichero que no está es una
/// lista vacía —no tener conexiones es lo normal el primer día—, y uno que no
/// parsea es un error, porque decir «no tienes ninguna» cuando hay una coma de
/// más sería mentir sobre lo que el usuario escribió.
///
/// Jamás un secreto: lo que sale es el par `(nombre, url)` tal como está
/// escrito, y las credenciales se REFERENCIAN (ADR 0015).
async fn handle_connection_list() -> Result<serde_json::Value, RpcError> {
    let dir = crate::connect::config_dir();
    let conexiones = crate::connect::named_connections(&dir)
        .await
        .map_err(RpcError::from)?;
    to_value(&methods::ConnectionListResult {
        connections: conexiones
            .into_iter()
            .map(|(name, url)| methods::ConnectionEntry { name, url })
            .collect(),
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
    let (applied, rancio, snapshot, dir) = {
        let mut reg = shared.plugins.lock().expect("plugins lock sano");
        // Lo que se CONCEDE tiene que ser lo que el humano LEYÓ (#282). Este
        // daemon descubre el catálogo una vez al arrancar, así que hoy la
        // ventana está cerrada por accidente; la comprobación la hace real, y
        // el `Backend` embebido —que redescubre en cada llamada— la necesita
        // de verdad.
        //
        // Solo al APROBAR: revocar no concede nada, y rehusar una revocación
        // por un ancla rancia dejaría vivo el permiso que alguien quita.
        //
        // Un id DESCONOCIDO no es un ancla rancia: `manifest_digest` devuelve
        // `None` para los dos casos, y contestar «el manifiesto cambió» a
        // quien nombró un plugin que no existe es un diagnóstico equivocado
        // sobre el error más común de un cliente mal escrito. Se pregunta
        // primero si se conoce.
        let conocido = reg.manifest_digest(&p.id).is_some();
        let rancio = p.approved
            && conocido
            && p.expected_digest
                .as_ref()
                .is_some_and(|esperado| reg.manifest_digest(&p.id).as_ref() != Some(esperado));
        let applied = !rancio && reg.set_approval_in_memory(&p.id, p.approved);
        (
            applied,
            rancio,
            reg.state_snapshot(),
            reg.config_dir().to_path_buf(),
        )
    };
    if rancio {
        // NO `INVALID_PARAMS`: ése es el código de «ese plugin no existe» tres
        // líneas más abajo, y un cliente que reciba los dos iguales no puede
        // distinguir «vuelve a leerlo y aprueba» de «ese id no está». Es la
        // misma forma que `session.put` con una revisión rancia, y usa la
        // misma variante: `ConflictKind::StaleRevision`.
        return Err(RpcError::from(norte_proto::Error::Conflict {
            conflict: norte_proto::ConflictKind::StaleRevision,
        }));
    }
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

/// La ubicación que se le acuña a un plugin de columnas: el directorio padre
/// de la página, **si el actor podría leerlo él mismo** (#239).
///
/// Función aparte y con el permiso como predicado por lo mismo que
/// `send_to_conn_impl`: la decisión se prueba sin levantar un `Shared`, y lo
/// que hay que fijar es que el padre pasa por una puerta —antes no pasaba por
/// ninguna, y el comentario del handler afirmaba lo contrario.
fn location_permitida(
    primero: Option<&norte_proto::VPath>,
    permitido: impl Fn(&norte_proto::VPath) -> bool,
) -> Option<norte_proto::VPath> {
    let padre = primero.and_then(norte_proto::VPath::parent)?;
    permitido(&padre).then_some(padre)
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
        // `plugin_id` presente = ESE plugin o ninguno (#120). Ausente = cliente
        // 0.34: se conserva el primero-que-case de antes.
        reg.resolve_columns_of(p.plugin_id.as_deref(), &p.column_id)
    };
    let Some(resolved) = resolved else {
        return to_value(&methods::PluginColumnValuesResult {
            values: vec![None; expected_len],
        });
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let column_id = p.column_id;
    // La UBICACIÓN es el directorio padre de la página (ADR 0057), y **pasa su
    // propio gate de lectura** (#239).
    //
    // El comentario que había aquí decía que el padre venía gatado «de arriba»
    // porque los paths lo estaban. No es verdad: `read_gate_all` mira los
    // PATHS, y el padre de un path en scope puede estar fuera. Y una raíz de
    // scope está en su propio scope —hay un test que lo fija—, así que un
    // agente con scope sobre `file:///home/u/work` pedía columnas SOBRE esa
    // raíz y el plugin recibía una raíz confinada sobre `file:///home/u`: el
    // home entero, un nivel por encima de su sandbox, y sin necesidad de la
    // subida al marcador (que ya iba desactivada para agentes). Con un scope
    // que apunta a un fichero suelto, el directorio que lo contiene.
    //
    // Sin permiso NO se acuña: el plugin corre sin ubicación y su columna sale
    // en blanco. Es una degradación honesta — negar la llamada entera
    // convertiría la columna en un oráculo de qué directorios existen fuera
    // del scope, que es justo la fuga que este gate cierra.
    let location = location_permitida(p.paths.first(), |padre| {
        let permitido = read_gate(actor, padre, shared).is_ok();
        if !permitido {
            tracing::debug!("ubicación fuera de scope: el plugin corre sin ella");
        }
        permitido
    });
    let climb = matches!(actor, Actor::User);
    let pool = Arc::clone(&shared.column_pool);
    let values = tokio::task::spawn_blocking(move || {
        pool.column_values(
            &runtime,
            resolved,
            &column_id,
            location.as_ref(),
            // Solo el humano sube a buscar la raíz del proyecto: un agente o
            // un plugin están acotados a su scope, y subir por encima de él es
            // exactamente lo que el gate de lectura impide.
            climb,
            &entries,
            expected_len,
        )
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

/// `plugin.help` (H3e, 0.34.0): la página de ayuda de un plugin, para CUALQUIER
/// conexión — mismo criterio que `plugin.list`/`plugin.get_config`: leer
/// documentación no consiente nada.
///
/// El `id` es una CLAVE contra el catálogo, jamás un componente de ruta
/// (`PluginRegistry` lo resuelve contra los plugins descubiertos), así que un
/// id con `../` falla el lookup en vez de salir del directorio. Un id
/// desconocido es `INVALID_PARAMS`, mismo trato que `plugin.set_approval` da a
/// un plugin fantasma.
///
/// Ni un solo syscall bajo el lock: aquí hay un fichero de hasta
/// [`methods::PLUGIN_HELP_MAX_BYTES`] que puede vivir en un montaje lento u
/// hostil, y tanto la guarda de escape (tres syscalls) como la lectura dentro
/// del handler async sosteniendo el `std::Mutex` del registro violarían la regla
/// 2 y encolarían a todas las demás conexiones detrás. Por eso el lock solo
/// resuelve el id contra el catálogo —memoria pura— y devuelve un
/// `HelpJob` OPACO; verificar y leer ocurre en `spawn_blocking`, con el lock ya
/// soltado. El trabajo es opaco a propósito: este handler nunca llega a tener
/// una ruta que pudiera re-derivar del id del wire.
// `skip_all` SIN el id: viene crudo del wire y no debe llegar al log antes de
// validarse contra el catálogo (mismo criterio que `handle_plugin_set_approval`).
#[tracing::instrument(skip_all)]
async fn handle_plugin_help(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginHelpParams = parse_params(params)?;
    // El lock se libera al cerrar el bloque, ANTES de cualquier `.await`.
    let job = {
        let reg = shared.plugins.lock().expect("plugins lock sano");
        reg.help_job(&p.id)
    };
    let Some(job) = job else {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    };
    // `HelpJob::read` es también quien TOPA la lectura (`max_bytes + 1`): un
    // `help.md` disperso de 100 GiB no puede convertir esta llamada en una
    // reserva de 100 GiB. Ver su rustdoc.
    let page = tokio::task::spawn_blocking(move || job.read())
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "plugin help task panicked"))?;
    to_value(&page)
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

/// Gate de CONTENIDO para agentes (`fs.compare` con el rung de hash, C6):
/// [`read_gate`] más la exigencia de que el scope conceda una op que maneje
/// BYTES ([`ScopeRegistry::covers_content`](crate::policy::ScopeRegistry::covers_content)).
///
/// Se aplica ADEMÁS del gate de lectura, jamás en su lugar: la lectura decide
/// si el actor puede mirar el subtree, esta decide si puede hacer que UNA
/// llamada lea los dos árboles ENTEROS. Un `User` (humano) no se sandboxea y
/// cualquier actor que no sea `User`/`Agent` se deniega, igual que allí.
///
/// El veredicto es el mismo `PolicyDenied` con la categoría gruesa: quien
/// pide de más no se entera de qué puerta le faltaba, solo de que le falta
/// scope (sin fuga en el error ni en la traza).
fn content_gate(
    actor: &Actor,
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    use crate::policy::{DenyReason, ScopeVerdict};
    #[allow(clippy::match_wildcard_for_single_variants)]
    let denied: Option<DenyReason> = match actor {
        Actor::User => None,
        Actor::Agent { session } => {
            match shared.scopes.covers_content(session, path, Instant::now()) {
                ScopeVerdict::Within => None,
                ScopeVerdict::Expired => Some(DenyReason::ScopeExpired),
                ScopeVerdict::OutOfScope => Some(DenyReason::OutOfScope),
            }
        }
        _ => Some(DenyReason::OutOfScope),
    };
    if let Some(reason) = denied {
        tracing::warn!(
            rule = reason.rule_id(),
            "lectura de CONTENIDO sin scope: denegada (default-deny)"
        );
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        }));
    }
    Ok(())
}

/// `connection.close` (0.49.0, #140): suelta la sesión remota de una ruta.
///
/// Gate de LECTURA sobre la ruta, que es el mismo criterio que para mirarla:
/// cerrar una conexión no destruye datos —la siguiente operación reconecta—
/// pero sí interrumpe a quien la estuviera usando, y quien no puede ni leer ahí
/// no tiene por qué poder hacer eso.
///
/// SOLO humanos: desconectar es una decisión de quien está delante. Un agente
/// que pudiera cerrar la sesión de su humano tendría una palanca de denegación
/// de servicio gratis, sin que le sirva para nada de lo suyo.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_connection_close(
    actor: &Actor,
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ConnectionCloseParams = parse_params(params)?;
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection closes a session",
        ));
    }
    read_gate(actor, &p.path, shared)?;
    let closed = shared.engine.close_connection(&p.path);
    to_value(&methods::ConnectionCloseResult { closed })
}

/// `fs.dir_size` (0.49.0, #139): cuánto ocupa lo que se pida, como Task.
///
/// Gate de LECTURA sobre CADA raíz, y antes de validar nada más: un actor sin
/// derechos sobre lo que pide no llega a saber si su petición era además
/// incorrecta. Recorrer un árbol revela su FORMA —cuántas cosas hay y cómo se
/// llaman los directorios por los que se baja—, que es exactamente lo que un
/// listado revela y por eso es el mismo gate.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_dir_size(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsDirSizeParams = parse_params(params)?;
    for path in &p.paths {
        read_gate(actor, path, shared)?;
    }
    if p.paths.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.dir_size: paths must not be empty",
        ));
    }
    let handle = shared
        .engine
        .dir_size_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANTE (#64): CERO `.await` entre el submit del engine (dentro de
    // `dir_size_as`) y este register — la Task jamás corre FUERA de
    // `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `archive.pack` (0.50.0, #132): fabrica un archivo como Task.
///
/// El gate de MUTACIÓN lo hace el engine (misma op que una copia: se leen las
/// fuentes y se escribe el destino). Aquí va el de LECTURA sobre las fuentes,
/// por lo mismo que en `fs.dir_size`: sin él, un agente fuera de scope
/// enumeraría un árbol ajeno a través de los errores de esta llamada.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_archive_pack(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchivePackParams = parse_params(params)?;
    if p.sources.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "archive.pack: sources must not be empty",
        ));
    }
    for path in &p.sources {
        read_gate(actor, path, shared)?;
    }
    let handle = shared
        .engine
        .pack_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANTE (#64): CERO `.await` entre el submit y este register.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `archive.pack_report` (0.58.0, #250): qué guardó ese empaquetado que no
/// sobrevive a salir de aquí.
///
/// Gemelo exacto de `archive.test_report`, visibilidad incluida: solo lo ve
/// quien lanzó la Task, y un id de otro actor se contesta igual que uno que no
/// existe.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_archive_pack_report(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchivePackReportParams = parse_params(params)?;
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, informe) = shared
        .engine
        .archive_pack_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        tracing::warn!(actor = ?actor, "archive.pack_report de otro actor");
        return Err(unknown());
    }
    to_value(&informe)
}

/// `archive.test` (0.50.0, #132): comprueba un archivo como Task.
///
/// No muta, así que solo gate de LECTURA. El informe se recoge después con
/// [`methods::ARCHIVE_TEST_REPORT`], igual que el de un lote de renames: una
/// Task no devuelve valor, y «qué entrada está corrupta» no cabe en un
/// `Failed`.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_archive_test(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchiveTestParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let (handle, _informe) = shared
        .engine
        .test_archive_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `archive.test_report` (0.50.0, #132): el informe de un test ya lanzado.
///
/// Gemelo exacto de `fs.rename_batch_report`, VISIBILIDAD incluida: solo lo ve
/// quien lanzó la Task, y un id de otro actor se contesta igual que uno que no
/// existe — decir «existe pero no es tuyo» ya sería contar algo.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_archive_test_report(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchiveTestReportParams = parse_params(params)?;
    // Una sola respuesta para las tres situaciones —desalojado del anillo,
    // nunca fue un test, es de otro actor—, y la tercera es la razón:
    // separarla confirmaría que la task de otro existió.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, informe) = shared
        .engine
        .archive_test_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Material de auditoría, como en sus gemelos: preguntar por informes
        // ajenos deja rastro aunque la respuesta no diga nada.
        tracing::warn!(actor = ?actor, "archive.test_report de otro actor");
        return Err(unknown());
    }
    to_value(&informe)
}

/// `file.split` (0.50.0, #132): parte un fichero como Task.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_file_split(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FileSplitParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let handle = shared
        .engine
        .split_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `file.combine` (0.50.0, #132): junta los trozos como Task.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_file_combine(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FileCombineParams = parse_params(params)?;
    read_gate(actor, &p.first, shared)?;
    // Y el DIRECTORIO, porque los demás trozos se derivan por convención y no
    // los nombra la petición: un scope sobre el fichero `.001` a secas no
    // cubre a sus hermanos.
    if let Some(dir) = p.first.parent() {
        read_gate(actor, &dir, shared)?;
    }
    let handle = shared
        .engine
        .combine_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `fs.compare` (0.39.0, ADR 0048): compara dos árboles como Task cancelable.
/// Las FILAS llegan por `compare.rows` SOLO a la conexión `conn_id` que la
/// lanzó (envío dirigido, jamás broadcast — mismo criterio que `search.hits`).
///
/// NO muta nada: sin journal, sin undo, no se escribe un byte. **Regla dura 4
/// no aplica** — dicho aquí para que una revisión posterior no pida una
/// entrada de journal que no significaría nada.
///
/// GATE. Comparar LEE dos árboles enteros, así que hay dos puertas:
/// - [`read_gate`] sobre **AMBAS** raíces (#80). Una sola no basta: el árbol
///   que no está bajo scope se listaría igual, y sus nombres viajarían en las
///   filas.
/// - [`content_gate`] sobre ambas **cuando `criteria.hash` está encendido**:
///   ese rung pasa cada byte de cada fichero emparejado por un sha256, que es
///   más de lo que revela un listado (ver la tensión anotada en
///   `covers_content`).
///
/// `INVALID_PARAMS` SIN crear Task, con la misma forma que los criterios
/// inválidos de `fs.search`:
/// - Dos raíces IGUALES: comparar algo contra sí mismo durante una hora no es
///   una petición, es una errata de quien llama.
/// - `follow_symlinks: true`: el motor acepta el campo y lo IGNORA, y servir
///   en silencio un recorrido distinto del pedido es peor que no ofrecerlo.
///
/// (Ambos son `-32602` pelado, así que un `Backend` remoto los entrega como
/// `Internal` mientras el embebido dice `InvalidPath`/`Unsupported`. Es la
/// misma asimetría que ya tienen los criterios de `fs.search`, y el contrato
/// publicado en `methods::FS_COMPARE` es el código, no la taxonomía.)
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_compare(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsCompareParams = parse_params(params)?;

    // Gate ANTES de validar params: un actor sin derechos sobre las raíces no
    // llega a saber si su petición era además incorrecta.
    read_gate(actor, &p.left, shared)?;
    read_gate(actor, &p.right, shared)?;
    if p.criteria.hash {
        content_gate(actor, &p.left, shared)?;
        content_gate(actor, &p.right, shared)?;
    }

    if p.left == p.right {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.compare: left and right resolve to the same root",
        ));
    }
    if p.follow_symlinks {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.compare: follow_symlinks is not supported (link targets are compared as bytes)",
        ));
    }
    // `descend_orphans` NO se valida aquí: es un `DescendSide`, que no tiene
    // `serde(other)`, así que un `"lft"` muere en `parse_params` de arriba
    // (`-32602`) y `Side::Unknown` ni siquiera es representable. Un `if` en
    // este handler habría dejado fuera el brazo EMBEBIDO, que llama al engine
    // sin pasar por aquí.

    let (handle, mut rx) = shared
        .engine
        .compare_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANTE (#64): CERO `.await` entre el submit del engine (dentro de
    // `compare_as`) y este register — la Task jamás corre FUERA de
    // `shared.tasks` (con task.list/cancel y contando contra los topes). Quien
    // añada un await aquí rompe esa garantía.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // Bomba de FILAS: drena el canal del walk y enruta cada lote como
    // `compare.rows` SOLO al dueño. Muere sola cuando el walk cierra `tx`
    // (terminal, cancel o receptor —el propio dueño— desaparecido).
    //
    // Y al revés: en cuanto un lote NO se entrega —el dueño se fue, o no
    // drenaba su outbox y el daemon lo expulsó— la bomba PARA. Al soltar `rx`,
    // el walk ve `ReceiverGone` y termina. Sin esto, una comparación de tres
    // horas seguiría leyendo dos árboles (y hasheándolos) para nadie,
    // reteniendo su permiso del scheduler frente al resto del trabajo de ese
    // scheme. `fs.compare` es el caso que lo pide: a diferencia de
    // `fs.search`, no tiene `max_hits` que lo acote.
    //
    // Y lo que ya NO pasa (#155): pararla no le cuesta la suscripción, así que
    // el `task.progress` terminal —la única señal con la que puede saber que le
    // faltan filas— le sigue llegando.
    let shared_pump = Arc::clone(shared);
    // #155: mientras esta bomba viva, su dueño no se expulsa del mapa de
    // suscriptores por una outbox llena — perdería el `task.progress` terminal
    // con el que compara filas recibidas contra `entries_done`, que es la
    // única forma que tiene de saber que la comparación le llegó entera.
    let feed = shared_pump.feed_guard(conn_id);
    tokio::spawn(async move {
        let _feed = feed;
        while let Some(rows) = rx.recv().await {
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::COMPARE_ROWS.into(),
                params: serde_json::to_value(&rows).ok(),
            };
            let Ok(frame) = encode_frame(&notif) else {
                continue;
            };
            if !shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice())) {
                tracing::debug!(conn = conn_id, "compare.rows sin dueño: se para el walk");
                break;
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.plan` (0.40.0, ADR 0049): planifica una sincronización de UN sentido
/// (`source` → `dest`) como Task cancelable. Los PASOS llegan por `sync.steps`
/// SOLO a la conexión `conn_id` que la lanzó, y el plan lo CIERRA un
/// `sync.plan_done` por el mismo camino.
///
/// NO muta: por debajo es `fs.compare` con una decisión por fila. Lo que sí hace
/// es RETENER el plan en un spool atado a esta conexión, que es lo que permite
/// que `sync.apply` no lleve más que un hash.
///
/// GATE. Planificar LEE dos árboles enteros, así que son las mismas dos puertas
/// que [`handle_fs_compare`], y por los mismos motivos:
/// - [`read_gate`] sobre **AMBAS** raíces (#80).
/// - [`content_gate`] sobre ambas **cuando `compare.criteria.hash` está
///   encendido**.
///
/// Y van **antes** de validar los params: un actor sin derechos sobre las raíces
/// no llega a saber si su petición era además incorrecta.
///
/// `INVALID_PARAMS` SIN crear Task, con la misma forma que en `fs.compare`, para
/// los dos campos de `compare` que en `sync.plan` **no son del llamante**
/// (`follow_symlinks` y `descend_orphans` — el planificador fija el segundo al
/// lado del ORIGEN) y para un `include` por encima de
/// [`methods::SYNC_MAX_INCLUDE`], que se rehúsa en vez de recortarse. El engine
/// los rechaza también, con su propia taxonomía, porque el brazo EMBEBIDO no
/// pasa por aquí.
///
/// Las raíces solapadas NO se comprueban aquí: son
/// [`Error::OverlappingRoots`](norte_proto::Error::OverlappingRoots), una
/// categoría del wire, y el engine la produce en un solo sitio para los dos
/// caminos.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_sync_plan(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::SyncPlanParams = parse_params(params)?;

    // Gate ANTES de validar params (ver la nota del doc).
    read_gate(actor, &p.source, shared)?;
    read_gate(actor, &p.dest, shared)?;
    if p.compare.criteria.hash {
        content_gate(actor, &p.source, shared)?;
        content_gate(actor, &p.dest, shared)?;
    }

    if p.compare.follow_symlinks {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "sync.plan: compare.follow_symlinks is not supported (link targets are compared as bytes)",
        ));
    }
    if p.compare.descend_orphans.is_some() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "sync.plan: compare.descend_orphans is set by the planner, not by the caller",
        ));
    }
    // La constante es el contrato: el tope se nombra, jamás se escribe.
    let max_include = methods::SYNC_MAX_INCLUDE;
    if p.include
        .as_ref()
        .is_some_and(|inc| inc.len() > max_include)
    {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("sync.plan: include has more than {max_include} paths"),
        ));
    }

    // Tope de planes RETENIDOS por conexión. Un plan aprobado es un fichero con
    // el listado relativo de dos árboles, y mientras su conexión viva la única
    // cosa que lo recoge es el TTL. Sin este tope, un cliente que planifique en
    // bucle variando `include` —cada selección da otro digest, o sea otro
    // fichero— llena el directorio de estado, que es donde vive `journal.db`.
    // `OVERLOADED` y no `-32602`: la petición es válida, el momento no (mismo
    // criterio y mismo código que el tope de Tasks vivas).
    if let Some(spool) = shared.engine.spool()
        && spool.retained_for(conn_id) >= MAX_RETAINED_SYNC_PLANS
    {
        // Con TAXONOMÍA en `data` y no solo con la frase (#182): un rechazo
        // sin taxonomía llega al cliente como `Internal { panic: false }` —
        // «internal error»— porque `to_taxonomy` no tiene otra cosa que
        // devolver, y a un agente eso le dice «vuelve a intentarlo», que es lo
        // que llenaba este mismo tope. `LimitExceeded` dice lo que pasa: el
        // plan es válido, lo que se acabó es el presupuesto.
        return Err(RpcError::from(norte_proto::Error::LimitExceeded {
            limit: norte_proto::Error::LIMIT_RETAINED_SYNC_PLANS.to_owned(),
        }));
    }

    let (handle, mut rx) = shared
        .engine
        .sync_plan_as(p, conn_id, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANTE (#64): CERO `.await` entre el submit del engine (dentro de
    // `sync_plan_as`) y este register — la Task jamás corre FUERA de
    // `shared.tasks`. Quien añada un await aquí rompe esa garantía.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // Bomba de PASOS: drena el canal del plan y enruta cada evento SOLO al
    // dueño. Un único canal trae los lotes y el cierre, así que el orden
    // «`sync.steps`* y después un `sync.plan_done`» no depende de esta bomba:
    // es la cola.
    //
    // En cuanto un evento NO se entrega —el dueño se fue, o no drenaba su
    // outbox y el daemon lo expulsó— la bomba PARA. Al soltar `rx`, la Task ve
    // que su receptor desapareció, cierra el spool como INTERRUMPIDO (no deja
    // plan aprobable) y termina. Sin esto, un plan de tres horas seguiría
    // recorriendo dos árboles para nadie, reteniendo su permiso del scheduler.
    //
    // Lo que ya NO pasa (#155): el dueño de este feed no se expulsa del mapa de
    // suscriptores mientras dure, así que conserva su `task.progress` terminal.
    // Aquí el fallo era el menos grave de los tres —un cliente sin
    // `sync.plan_done` no tiene `plan_hash` y no puede aplicar nada— pero es el
    // mismo mecanismo, y arreglarlo en dos de tres bombas es dejarlo a medias.
    let shared_pump = Arc::clone(shared);
    let feed = shared_pump.feed_guard(conn_id);
    tokio::spawn(async move {
        let _feed = feed;
        while let Some(event) = rx.recv().await {
            let (method, params) = match event {
                crate::sync::SyncPlanEvent::Steps(batch) => {
                    (methods::SYNC_STEPS, serde_json::to_value(&batch))
                }
                crate::sync::SyncPlanEvent::Done(done) => {
                    (methods::SYNC_PLAN_DONE, serde_json::to_value(&done))
                }
            };
            // A diferencia de la bomba de `fs.compare`, un fallo de
            // serialización NO manda la notificación con `params: null`: aquí un
            // lote perdido alimenta una aprobación, y un frame que ningún
            // cliente puede parsear es peor que ninguno. Es inalcanzable con
            // estos tipos (structs planos), y por eso mismo sale barato.
            let Ok(params) = params else {
                tracing::error!(method, "no se pudo serializar un evento de sync.plan");
                break;
            };
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: method.into(),
                params: Some(params),
            };
            let Ok(frame) = encode_frame(&notif) else {
                break;
            };
            if !shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice())) {
                tracing::debug!(conn = conn_id, method, "sync sin dueño: se para el plan");
                // Y se van sus planes retenidos. Este es el único observador de
                // la entrega, y llega DESPUÉS del desmontaje de la conexión: un
                // plan que cerró entre el barrido del desmontaje y este punto
                // quedaría retenido para siempre — el `sync.plan_done` cabe en
                // el buffer del canal, así que la Task lo da por entregado.
                drop_sync_plans(&shared_pump, conn_id).await;
                break;
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.apply` (0.40.0, ADR 0049): ejecuta un plan RETENIDO como Task
/// cancelable. El único parámetro es el `plan_hash`, así que por la FORMA de la
/// petición no se puede ejecutar nada que no sea lo que un humano aprobó.
///
/// # Por qué aquí NO hay gate
/// No porque no haga falta, sino porque este handler no sabe sobre qué pedirlo:
/// las dos raíces viven en el SPOOL y `sync.apply` no las lleva. El gate corre
/// dentro de [`Engine::sync_apply_as`], sobre las rutas leídas del fichero y en
/// el momento de aplicar — que es además lo correcto, porque entre planificar y
/// aplicar pasan hasta `SYNC_PLAN_TTL_MS` y un scope caduca dentro de esa
/// ventana. Un `read_gate` aquí sobre algo que no son las raíces sería teatro.
///
/// # Los rechazos son del engine, y esto no los duplica
/// `Unsupported` (sin spool o sin journal), `PlanStale` (el hash no nombra un
/// plan vivo de ESTA conexión — no existe, caducó, está manipulado, es de otra
/// conexión o ya se está aplicando), `PlanNotExecutable` (el plan traía
/// bloqueos) y `PolicyDenied` salen todos de `sync_apply_as`, porque el brazo
/// EMBEBIDO del `Backend` llama al engine sin pasar por aquí. Este handler los
/// entrega con su taxonomía intacta.
///
/// Un `plan_hash` MALFORMADO no llega hasta el engine: `PlanHash` lo rechaza al
/// deserializar, o sea `-32602`. Y eso importa — «esto no es un hash» y «el
/// mundo se movió» son hechos distintos, y contestar el segundo a quien mandó el
/// primero le miente sobre el estado del mundo.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_sync_apply(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::SyncApplyParams = parse_params(params)?;
    // El tope de Tasks vivas se mira ANTES de abrir el plan, y no solo en
    // `register_task_id`. Abrirlo se lleva el DERECHO a aplicarlo (es de un solo
    // uso) y el cuerpo de la Task lo GASTA en cualquier estado terminal, así que
    // un `OVERLOADED` de después destruye el plan aprobado: el cliente se queda
    // sin `task_id`, cada reintento de ese hash contesta `PlanStale` y la única
    // salida es volver a recorrer los dos árboles enteros. Ningún otro método
    // tiene un parámetro tan caro de reconstruir.
    //
    // Es TOCTOU —dos `sync.apply` simultáneos pueden pasar los dos y el segundo
    // morir en `register_task_id`— y aun así vale: mueve el caso normal de
    // «plan destruido» a «plan intacto, vuelve a intentarlo», que es lo que el
    // mensaje del error dice. Mismo criterio que el pre-chequeo del tope de
    // planes retenidos de `handle_sync_plan`.
    if let Some(err) = tasks_at_capacity(shared, actor) {
        return Err(err);
    }
    let (handle, _report) = shared
        .engine
        .sync_apply_as(&p.plan_hash, conn_id, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // El informe NO se retiene aquí: vive en el anillo del engine
    // (`Engine::sync_report`), que es de donde lo sirve `sync.report` — un solo
    // anillo para el socket y para el `Backend` embebido, igual que el de
    // `fs.rename_batch_report`.
    //
    // INVARIANTE (#64): CERO `.await` entre el submit del engine (dentro de
    // `sync_apply_as`) y este register — la Task jamás corre FUERA de
    // `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.report` (0.40.0, ADR 0049): qué hizo la aplicación de un plan —
/// cuántos pasos se ejecutaron, cuántos fallaron y con qué causa, y bajo qué
/// lote del journal quedó todo.
///
/// Gemelo de [`handle_rename_batch_report`], incluida su comprobación de
/// dueño: lo ve quien podría ver la Task ([`may_observe`]) — su dueño, o
/// cualquier conexión humana. Para el resto la respuesta es la MISMA que la de
/// un id desconocido, porque el informe lleva rutas relativas de dos árboles
/// ajenos y distinguir «no es tuya» de «no existe» ya sería filtrar que
/// existió.
///
/// Un id DESALOJADO del anillo ([`SYNC_REPORTS_MAX`](crate::SYNC_REPORTS_MAX))
/// contesta lo mismo que uno que nunca fue una aplicación, con la misma
/// renuncia que su gemelo documenta.
fn handle_sync_report(
    actor: &Actor,
    p: &methods::SyncReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // `NotFound` de la taxonomía, una sola respuesta para las TRES situaciones
    // —desalojado del anillo, nunca fue una aplicación, es de otro actor— y la
    // tercera es la razón: separarla confirmaría que la task de otro existió.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared.engine.sync_report(p.task_id).ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Material de auditoría (M3-5), igual que la rama denegada de
        // `task.cancel`: el que pregunta por informes ajenos deja rastro aunque
        // su respuesta no le diga nada. Ni el id ni el dueño.
        tracing::warn!(
            actor = ?actor,
            "informe de sync de otro actor: denegado (respuesta = id desconocido)"
        );
        return Err(unknown());
    }
    to_value(&report)
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
    // #155: igual que `compare.rows` — el dueño de un feed dirigido vivo
    // pierde frames, jamás la suscripción.
    let feed = shared_pump.feed_guard(conn_id);

    tokio::spawn(async move {
        let _feed = feed;
        while let Some(hits) = rx.recv().await {
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::SEARCH_HITS.into(),
                params: serde_json::to_value(&hits).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                // El desenlace se ignora A PROPÓSITO: la bomba de `fs.compare`
                // sí para cuando el dueño desaparece, pero cambiar eso aquí
                // cambiaría el comportamiento de una búsqueda viva, que no es
                // lo que este cambio venía a tocar. `fs.search` además se
                // acota con `max_hits`.
                let _delivered =
                    shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice()));
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// El tope de parejas de un lote de renames (0.36.0), aplicado EN LA FRONTERA.
///
/// La constante es el contrato ([`methods::FS_RENAME_BATCH_MAX_PAIRS`]) y el
/// daemon es donde se impone: aquí es donde llega input de un peer que puede
/// ser un agente. RECHAZA, no recorta —recortar ejecutaría un plan distinto del
/// pedido, y para el plan devolvería veredictos de un lote que nadie mandó—, y
/// rechaza ANTES de tocar el engine, que planifica en tiempo lineal sobre las
/// parejas Y sobre el listado. El engine vuelve a comprobarlo por su cuenta
/// (es API pública embebida); esta comprobación es la de la frontera, no un
/// duplicado ocioso.
///
/// El veredicto es [`Error::InvalidPath`] de la TAXONOMÍA, el mismo que
/// devuelve `plan_for` cuando la comprobación gemela del engine se dispara. Un
/// `-32602` pelado no lleva categoría en `data`, así que el `Backend` remoto lo
/// entregaría como `Internal` mientras el embebido dice `InvalidPath`: dos
/// respuestas distintas al mismo suceso según por dónde se entre.
fn check_pairs_cap(
    pairs: &[methods::RenamePair],
) -> Result<Vec<crate::rename::PairBytes>, RpcError> {
    let max = methods::FS_RENAME_BATCH_MAX_PAIRS;
    if pairs.len() > max {
        tracing::debug!(
            pairs = pairs.len(),
            max,
            "lote de renames por encima del tope"
        );
        return Err(RpcError::from(norte_proto::Error::InvalidPath));
    }
    Ok(crate::rename::pairs_from_wire(pairs))
}

/// `fs.rename_batch_report` (0.36.0): el informe de un lote ya lanzado.
///
/// Lo ve quien podría ver la Task ([`may_observe`]): su dueño, o cualquier
/// conexión humana. Para el resto la respuesta es la MISMA que la de un id
/// desconocido — el informe lleva rutas del directorio de otro actor, y
/// distinguir «no es tuya» de «no existe» ya sería filtrar que existió (mismo
/// criterio que `task.cancel`).
///
/// Un id DESALOJADO del anillo contesta lo mismo que uno que nunca fue un lote,
/// y eso sí es una renuncia: existe el precedente de
/// [`Error::CursorExpired`](norte_proto::Error::CursorExpired) (ADR 0017) para
/// «tu asa envejeció fuera de un anillo acotado del server». No se acuña
/// categoría porque hoy ningún cliente reintentaría distinto —el informe de una
/// task terminal se pide una vez, justo después— y porque separar las dos cosas
/// solo tiene sentido si además se separa de la tercera, que es justo la que no
/// puede separarse. Aditiva el día que un cliente enseñe el caso (ADR 0042 §8).
fn handle_rename_batch_report(
    actor: &Actor,
    p: &methods::FsRenameBatchReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // `NotFound` de la taxonomía, que es exactamente lo que contesta el brazo
    // embebido del `Backend` para el mismo caso. Una sola respuesta para las
    // TRES situaciones —desalojado del anillo, nunca fue un lote, es de otro
    // actor— y la tercera es la razón: separarla confirmaría que la task de
    // otro existió.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .rename_batch_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Material de auditoría (M3-5), igual que la rama denegada de
        // `task.cancel`: el que pregunta por informes ajenos deja rastro
        // aunque su respuesta no le diga nada. Ni el id ni el dueño: la
        // traza cuenta que hubo sondeo, no qué había al otro lado.
        tracing::warn!(
            actor = ?actor,
            "informe de lote de otro actor: denegado (respuesta = id desconocido)"
        );
        return Err(unknown());
    }
    to_value(&crate::rename::report_to_proto(&report))
}

/// Las familias `fs.*`/`task.*` del dispatch (separadas por tamaño). El
/// `actor` viene de la conexión (M3-3b): las mutaciones se journalizan y
/// evalúan bajo él.
// Lista plana de brazos, un método por brazo — mismo criterio que `dispatch`:
// trocearla no reduciría la complejidad real, solo la escondería.
#[allow(clippy::too_many_lines)]
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
        // fs.compare (0.39.0): las FILAS son del que la lanzó → conn_id, igual
        // que `fs.search` (envío dirigido, jamás broadcast).
        methods::FS_COMPARE => handle_fs_compare(req.params, conn_id, &actor, shared).await,
        // fs.dir_size (0.49.0, #139): sin conn_id — no enruta nada, el total
        // viaja en el progreso que ya escucha todo el mundo.
        methods::FS_DIR_SIZE => handle_fs_dir_size(req.params, &actor, shared).await,
        // 0.50.0 (#132): escribir archivos. Ninguno escribe DENTRO de un
        // contenedor — el provider de archivos sigue `READ_ONLY` (ADR 0018).
        methods::ARCHIVE_PACK => handle_archive_pack(req.params, &actor, shared).await,
        methods::ARCHIVE_TEST => handle_archive_test(req.params, &actor, shared).await,
        methods::ARCHIVE_TEST_REPORT => handle_archive_test_report(req.params, &actor, shared),
        methods::ARCHIVE_PACK_REPORT => handle_archive_pack_report(req.params, &actor, shared),
        methods::FILE_SPLIT => handle_file_split(req.params, &actor, shared).await,
        methods::FILE_COMBINE => handle_file_combine(req.params, &actor, shared).await,
        // connection.close (0.49.0, #140): humano, con gate de lectura.
        methods::CONNECTION_CLOSE => handle_connection_close(&actor, req.params, shared),
        // sync.plan (0.40.0): los PASOS son del que lo lanzó, y el plan queda
        // RETENIDO a nombre de esta conexión → conn_id por partida doble.
        methods::SYNC_PLAN => handle_sync_plan(req.params, conn_id, &actor, shared).await,
        // sync.apply (0.40.0): el plan RETENIDO se abre por `(conn_id, hash)`,
        // así que el conn_id no es para enrutar nada — es la mitad de la llave.
        methods::SYNC_APPLY => handle_sync_apply(req.params, conn_id, &actor, shared).await,
        // sync.report (0.40.0): lo que un `Failed` no puede contar — qué pasos
        // se quedaron sin aplicar y bajo qué lote está lo que sí.
        methods::SYNC_REPORT => {
            let p: methods::SyncReportParams = parse_params(req.params)?;
            handle_sync_report(&actor, &p, shared)
        }
        methods::FS_STAT => {
            let p: methods::FsStatParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80
            handle_fs_stat(p, shared).await
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
        // ai.rename_plan (0.32.0, M4-IA, ADR 0031): plan de rename revisable.
        // Respuesta DIRECTA; cancelable (#72) — la llamada al proveedor tarda.
        methods::AI_RENAME_PLAN => {
            // IA solo para el humano (M4-IA security): un agente con scope de
            // lectura NO puede quemar cuota del proveedor ni empujar basenames
            // + instrucción fuera de la máquina sin rastro (el path de lectura
            // no journaliza). El MCP tampoco expone ai.* como tool. Categoría
            // del vocabulario CERRADO de [`crate::policy::DenyReason`].
            //
            // **El gate va ANTES del parseo** (#122), como en `index.embed` y
            // por el mismo motivo: estando después, un agente distinguía
            // «params malos» de «instrucción demasiado larga» de «dentro o
            // fuera de mi scope» ANTES de que se le denegara — o sea que la
            // respuesta dependía de cosas que él controla, y eso convierte un
            // método vedado en un oráculo sobre el árbol del humano.
            if !matches!(actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::AiRenamePlanParams = parse_params(req.params)?;
            if p.instruction.len() > MAX_AI_INSTRUCTION_BYTES {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("instruction supera {MAX_AI_INSTRUCTION_BYTES} bytes"),
                ));
            }
            read_gate(&actor, &p.dir, shared)?; // #80
            let plan = shared
                .engine
                .ai_rename_plan(&p.dir, &p.instruction)
                .await
                .map_err(RpcError::from)?;
            // Mapeo core→proto compartido con `Backend::Embedded`
            // (`ai_plan_to_proto`): lossy-identidad por invariante del engine.
            to_value(&crate::ai::ai_plan_to_proto(plan))
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
        // index.embed (0.33.0, M4-IA-2): Task de embeddings del root ya
        // indexado. SOLO humano, fail-closed como ai.rename_plan: los
        // prefijos de CONTENIDO salen del proceso hacia el proveedor y el
        // path de lectura no journaliza — un agente no quema cuota ni
        // exfiltra contenido sin rastro. Categoría del vocabulario CERRADO
        // de [`crate::policy::DenyReason`].
        methods::INDEX_EMBED => {
            // El gate de actor va ANTES del parseo A PROPÓSITO (security
            // audit M4-IA-2): un agente recibe `PolicyDenied` sea cual sea
            // la validez de sus params, y jamás distingue "params malos" de
            // "vedado" — la respuesta no depende de nada que él controle.
            if !matches!(actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::IndexEmbedParams = parse_params(req.params)?;
            read_gate(&actor, &p.root, shared)?; // #80
            let handle = shared
                .engine
                .index_embed_as(p.root, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // INVARIANTE (#64): CERO `.await` entre el submit del engine
            // (dentro de `index_embed_as`) y este register.
            let task_id = register_task_id(shared, handle, actor.clone())?;
            to_value(&methods::FsTaskResult { task_id })
        }
        // index.search_semantic (0.33.0, M4-IA-2): respuesta DIRECTA,
        // cancelable con rpc.cancel (#72) — el embed de la query tarda lo
        // que tarde el proveedor. SOLO humano (la query SALE hacia el
        // proveedor), mismo criterio que index.embed / ai.rename_plan.
        methods::INDEX_SEARCH_SEMANTIC => {
            // Igual que `index.embed`: gate de actor ANTES del parseo (security
            // audit M4-IA-2), para que un agente vea siempre `PolicyDenied`.
            if !matches!(actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::IndexSearchSemanticParams = parse_params(req.params)?;
            if p.query.len() > MAX_AI_QUERY_BYTES {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("query supera {MAX_AI_QUERY_BYTES} bytes"),
                ));
            }
            if let Some(root) = &p.root {
                read_gate(&actor, root, shared)?; // #80
            }
            let hits = shared
                .engine
                .index_search_semantic(p.root.as_ref(), &p.query, p.k)
                .await
                .map_err(RpcError::from)?;
            let hits = hits
                .into_iter()
                .map(|(path, score)| methods::SemanticHit { path, score })
                .collect();
            to_value(&methods::IndexSearchSemanticResult { hits })
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
                .copy_anchored(&p.from, &p.to, opts, actor.clone(), p.dest_anchor)
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
                .move_anchored(&p.from, &p.to, opts, actor.clone(), p.dest_anchor)
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
        // fs.rename_batch_plan (0.36.0, ADR 0042): el plan REVISABLE de un lote
        // de renames. Respuesta DIRECTA: ni Task, ni journal, ni mutación —
        // pero SÍ una lectura de directorio, y por eso pasa por el `read_gate`
        // como `fs.list`/`fs.stat` (#80). Sus veredictos `External`,
        // `AbsentSource` y `AmbiguousSource` dicen qué nombres existen y cuáles
        // no: sin gate esto es un oráculo de nombres —y de gemelos NFC/NFD, que
        // `fs.list` ni siquiera expone— para un agente sin scope.
        methods::FS_RENAME_BATCH_PLAN => {
            let p: methods::FsRenameBatchPlanParams = parse_params(req.params)?;
            read_gate(&actor, &p.dir, shared)?; // #80
            let pairs = check_pairs_cap(&p.pairs)?;
            let plan = shared
                .engine
                .rename_batch_plan_as(&p.dir, &pairs, actor)
                .await
                .map_err(RpcError::from)?;
            to_value(&crate::rename::plan_to_proto(&plan).map_err(RpcError::from)?)
        }
        // fs.rename_batch (0.36.0, ADR 0042): UNA Task, UN lote del journal,
        // rollback si algo falla. El engine RE-PLANIFICA y compara el hash: lo
        // que cruza el wire es intención (`pairs`), jamás un orden.
        methods::FS_RENAME_BATCH => {
            let p: methods::FsRenameBatchParams = parse_params(req.params)?;
            // #80, y aquí NO es una precaución de más. Ejecutar EMPIEZA por
            // planificar: `rename_batch_as` lista el directorio y compara el
            // hash ANTES de llegar a su gate de mutación, así que sin este
            // gate este método es el mismo oráculo que su gemelo — y uno mejor,
            // porque el `plan_hash` es determinista y calculable offline: un
            // agente sin scope manda el hash de la hipótesis «existe X» y
            // distingue `PolicyDenied` (existía) de `PlanStale` (no existía),
            // un bit exacto por petición. Que el efecto esté gateado más
            // adentro no salva a la LECTURA que hay antes.
            read_gate(&actor, &p.dir, shared)?;
            let pairs = check_pairs_cap(&p.pairs)?;
            let (handle, _report) = shared
                .engine
                .rename_batch_as(&p.dir, &pairs, &p.plan_hash, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // El informe NO se retiene aquí: vive en el anillo del engine
            // (`Engine::rename_batch_report`), que es de donde lo sirve
            // `fs.rename_batch_report` — un solo anillo para el socket y para
            // el `Backend` embebido.
            //
            // INVARIANTE (#64): CERO `.await` entre el submit del engine y este
            // register, igual que fs.copy/fs.move.
            register_task(shared, handle, actor)
        }
        // fs.rename_batch_report (0.36.0): lo que un `Failed` no puede contar —
        // qué paso se quedó aplicado y con qué nombre.
        methods::FS_RENAME_BATCH_REPORT => {
            let p: methods::FsRenameBatchReportParams = parse_params(req.params)?;
            handle_rename_batch_report(&actor, &p, shared)
        }
        // fs.mkdir (0.31.0, #104): Task, mismo molde que delete.
        methods::FS_MKDIR => {
            let p: methods::FsMkdirParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .mkdir_as(&p.path, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        methods::FS_CREATE => {
            let p: methods::FsCreateParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .create_file_as(&p.path, p.dest_anchor, actor.clone())
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
            // Catálogo del provider (#108 bloque 2), SIEMPRE saneado:
            // `Engine::attr_catalog` pasa por `AttrCatalog::new` — único
            // camino al wire (ADR 0039 §4).
            let attrs = shared
                .engine
                .attr_catalog(&p.path)
                .await
                .map_err(RpcError::from)?;
            to_value(&methods::FsCapabilitiesResult {
                capabilities,
                attrs,
            })
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
/// El mismo tope que aplica [`register_task_id`], consultado ANTES de crear la
/// Task. `Some(err)` = no cabe.
///
/// Existe para el único método cuyo rechazo TARDÍO no es recuperable
/// (`sync.apply`: rechazar después de abrir el plan lo destruye). Es una
/// aproximación —entre esto y el registro puede colarse otra Task— y por eso NO
/// sustituye al tope de `register_task_id`, que sigue siendo la autoridad.
fn tasks_at_capacity(shared: &Arc<Shared>, owner: &Actor) -> Option<RpcError> {
    let tasks = shared.tasks.lock().expect("tasks lock sano");
    if tasks.len() >= MAX_LIVE_TASKS {
        return Some(RpcError::protocol(
            codes::OVERLOADED,
            format!("too many live tasks (max {MAX_LIVE_TASKS}); retry later"),
        ));
    }
    if !matches!(owner, Actor::User)
        && tasks
            .values()
            .filter(|t| !matches!(t.owner, Actor::User))
            .count()
            >= MAX_LIVE_TASKS_AGENTS
    {
        return Some(RpcError::protocol(
            codes::OVERLOADED,
            format!("too many live agent tasks (max {MAX_LIVE_TASKS_AGENTS}); retry later"),
        ));
    }
    None
}

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

    /// #239: la ubicación que se le da a un plugin PASA su propia puerta.
    ///
    /// Se creía gatada porque los `paths` lo estaban, y no es lo mismo: una
    /// raíz de scope está en su propio scope —`covers_read` lo fija con todas
    /// las letras—, así que pedir columnas SOBRE la raíz entregaba al plugin
    /// una raíz confinada un nivel POR ENCIMA del sandbox del agente. Con un
    /// scope que apunta a un fichero, el directorio que lo contiene.
    #[test]
    fn la_ubicacion_de_un_plugin_pasa_el_gate_de_lectura() {
        use super::location_permitida;
        use crate::policy::{Scope, ScopeRegistry, ScopeVerdict};
        use norte_proto::VPath;

        let vp = |w: &str| VPath::parse(w).expect("wire de test");
        let reg = ScopeRegistry::new();
        reg.grant(
            "s1",
            Scope::forever(
                vec![vp("mem:///home/u/work")],
                crate::policy::OpSet::of(&["copy"]),
            ),
        );
        let ahora = std::time::Instant::now();
        let permitido = |p: &VPath| reg.covers_read("s1", p, ahora) == ScopeVerdict::Within;

        // Dentro del scope: la ubicación es el padre, y se acuña.
        assert_eq!(
            location_permitida(Some(&vp("mem:///home/u/work/sub/f")), permitido),
            Some(vp("mem:///home/u/work/sub")),
        );
        // LA RAÍZ del scope: su padre es el home entero, fuera del sandbox.
        // Sin ubicación, y el plugin corre sin ella.
        assert_eq!(
            location_permitida(Some(&vp("mem:///home/u/work")), permitido),
            None,
            "el padre de la raíz del scope está FUERA del scope"
        );
        // Un humano no se sandboxea: el predicado dice que sí a todo.
        assert_eq!(
            location_permitida(Some(&vp("mem:///home/u/work")), |_| true),
            Some(vp("mem:///home/u")),
        );
        // Sin paths no hay ubicación que acuñar.
        assert_eq!(location_permitida(None, |_| true), None);
    }

    /// El veto de `session.put`: quién puede escribir, y en qué orden se
    /// pregunta.
    ///
    /// El apagado NO está aquí y esa es la mitad interesante: comprobarlo con
    /// un token, fuera del lock que protege la mutación, dejaba abierta la
    /// ventana que pretendía cerrar. Lo cierra `SessionStore::seal`, y su test
    /// vive con el almacén.
    #[test]
    fn solo_la_duena_humana_escribe_la_sesion() {
        use super::{SessionPutVeto, session_put_veto};

        assert_eq!(session_put_veto(true, Some(7), 7, true), None);
        // Un agente no llega ni a preguntar por lo demás: no debería enterarse
        // ni de si hay dueña.
        assert_eq!(
            session_put_veto(false, Some(7), 7, true),
            Some(SessionPutVeto::NoEsHumano)
        );
        assert_eq!(
            session_put_veto(true, Some(1), 7, true),
            Some(SessionPutVeto::NoEsLaDuena)
        );
        assert_eq!(
            session_put_veto(true, None, 7, true),
            Some(SessionPutVeto::NoEsLaDuena),
            "sin dueña tampoco escribe quien no la reclamó"
        );
        // Revisión de #237: la dueña de un core que NO persiste tampoco
        // escribe. Aceptarlo en memoria dejó de ser inocuo cuando el escritor
        // pudo tomar el lock tarde — el cuerpo aceptado suelto sobrevivía a la
        // adopción y se publicaba encima de la pantalla del otro core.
        assert_eq!(
            session_put_veto(true, Some(7), 7, false),
            Some(SessionPutVeto::SinEscritor),
            "sin escritor, la propiedad del almacén no basta"
        );
    }

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

    /// #155: una outbox LLENA cuesta el frame y NO la suscripción. La
    /// expulsión era irreversible —la entrada solo se inserta en
    /// `initialize`— y se llevaba con ella el `task.progress` terminal, que es
    /// justo lo que el contrato de `compare.rows` manda comparar contra las
    /// filas recibidas para saber si llegaron todas.
    #[test]
    fn una_outbox_llena_cuesta_el_frame_no_la_suscripcion() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, send_to_conn_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        let (tx, _rx) = mpsc::channel::<Arc<[u8]>>(1);
        subs.lock().expect("lock").insert(
            1u64,
            Subscriber {
                tx,
                actor: Actor::User,
            },
        );
        let frame: Arc<[u8]> = Arc::from(vec![1u8].into_boxed_slice());

        assert!(send_to_conn_impl(&subs, 1, &frame), "el primero cabe");
        assert!(
            !send_to_conn_impl(&subs, 1, &frame),
            "el segundo no cabe: no entregado"
        );
        assert!(
            subs.lock().expect("lock").contains_key(&1),
            "y sigue suscrita: sin esto pierde también su terminal"
        );
    }

    /// El broadcast SÍ sigue expulsando al que no drena —el backlog de un
    /// cliente lento no puede crecer sin límite (M1)—, salvo al dueño de un
    /// feed dirigido vivo (#155).
    #[test]
    fn el_broadcast_expulsa_al_que_no_drena_pero_no_al_dueno_de_un_feed() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, broadcast_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        // Los receptores se retienen vivos: cerrarlos sería el OTRO caso
        // (outbox muerta), y aquí lo que se prueba es la LLENA.
        let mut vivos = Vec::new();
        for conn in [1u64, 2u64] {
            let (tx, rx) = mpsc::channel::<Arc<[u8]>>(1);
            vivos.push(rx);
            subs.lock().expect("lock").insert(
                conn,
                Subscriber {
                    tx,
                    actor: Actor::User,
                },
            );
        }
        let frame: Arc<[u8]> = Arc::from(vec![1u8].into_boxed_slice());

        // El primer frame cabe en las dos.
        broadcast_impl(&subs, &[], &frame, |_| true);
        assert_eq!(subs.lock().expect("lock").len(), 2);

        // El segundo no cabe en ninguna, pero la 2 es dueña de un feed vivo.
        broadcast_impl(&subs, &[2], &frame, |_| true);
        let subs = subs.lock().expect("lock");
        assert!(
            !subs.contains_key(&1),
            "el que no drena y no tiene feed, fuera"
        );
        assert!(
            subs.contains_key(&2),
            "el dueño de un feed dirigido vivo conserva la suscripción"
        );
        drop(vivos);
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
