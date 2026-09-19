//! [`Backend`]: la MISMA superficie para el core embebido y el daemon
//! (fase 3 M2). La regla 7 lo hace posible: los frontends solo cambian de
//! transporte, jamás de lógica.
//!
//! - Embebido: passthrough a [`Engine`] (el default de arranque
//!   instantáneo).
//! - Remoto (solo unix, ADR 0011): JSON-RPC contra el daemon, con bomba de
//!   notificaciones (`task.progress` → un `watch` por task), tasks
//!   FORÁNEAS (encoladas por OTROS frontends) entregadas por canal, resync
//!   vía `task.list` y reconexión con aviso.

use std::sync::Arc;

use norte_proto::{Error, TaskId, TaskProgress, TaskState};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::Engine;
use crate::scheduler::TaskHandle;

mod archive;
mod connect;
mod events;
mod fs;
mod host;
mod index;
mod journal;
mod plugins;
mod rename;
mod session;
mod sync;

/// Proyecta un `IndexHit` del core (norte-index) al tipo del protocolo (M4).
fn index_hit_to_proto(h: norte_index::IndexHit) -> norte_proto::methods::IndexHit {
    norte_proto::methods::IndexHit {
        path: h.path,
        kind: h.kind,
        size: h.size,
        mtime_ms: h.mtime_ms,
    }
}

/// Proyecta un [`crate::volumes::VolumeKind`] al tipo del protocolo (0.37.0,
/// #131). Los dos tipos NO comparten definición (`norte-proto` no puede
/// depender de `norte-core`, la dependencia va al revés): un `match`
/// exhaustivo aquí es lo que mantiene el mapeo honesto — un kind nuevo en el
/// core rompe la compilación de esta función en vez de degradar en silencio.
///
/// `pub(crate)`: el handler de `host.volumes` en `daemon::server` reutiliza
/// esta misma función (con [`volume_to_proto`]) en vez de duplicar el
/// `match` — a diferencia de `index_hit_to_proto`, cuyo mapeo es tan trivial
/// (cuatro campos sin ramas) que duplicarlo en el daemon no arriesga nada;
/// aquí SÍ hay un `match` de variantes, y dos copias son dos sitios que
/// olvidar al añadir una.
pub(crate) fn volume_kind_to_proto(
    k: crate::volumes::VolumeKind,
) -> norte_proto::methods::VolumeKind {
    match k {
        crate::volumes::VolumeKind::Fixed => norte_proto::methods::VolumeKind::Fixed,
        crate::volumes::VolumeKind::Removable => norte_proto::methods::VolumeKind::Removable,
        crate::volumes::VolumeKind::Network => norte_proto::methods::VolumeKind::Network,
        crate::volumes::VolumeKind::Pseudo => norte_proto::methods::VolumeKind::Pseudo,
        crate::volumes::VolumeKind::Unknown => norte_proto::methods::VolumeKind::Unknown,
    }
}

/// Proyecta un [`crate::volumes::Volume`] al tipo del protocolo (0.37.0,
/// #131). `pub(crate)`: ver el rustdoc de [`volume_kind_to_proto`].
pub(crate) fn volume_to_proto(v: crate::volumes::Volume) -> norte_proto::methods::Volume {
    norte_proto::methods::Volume {
        mount: v.mount,
        label: v.label,
        fs_type: v.fs_type,
        kind: volume_kind_to_proto(v.kind),
        total_bytes: v.total_bytes,
        free_bytes: v.free_bytes,
        read_only: v.read_only,
    }
}

/// Timeout de llamadas de IA: el proveedor (modelo remoto) tarda
/// legítimamente mucho más que un fs.*. Acota AMBOS brazos de
/// [`Backend::ai_rename_plan`] (embebido y remoto — cancel-on-drop en el
/// remoto igualmente).
const AI_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

/// El tipo de stream que devuelve [`Backend::list_stream`] (ADR 0017),
/// re-exportado para que los frontends lo nombren sin depender de `norte-vfs`.
pub use norte_vfs::EntryStream;

/// Una task en marcha, venga del scheduler embebido o del daemon.
///
/// NO es `Clone` a propósito: [`Self::join`] consume el handle, y dos dueños
/// de la espera son dos sitios que creen que van a ver el desenlace. Lo que sí
/// se puede repartir es OBSERVARLA — ver [`Self::observer`] (#173).
pub struct TaskRef {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: TaskCanceller,
}

impl TaskRef {
    /// SOLO para tests de frontends (#85): un `TaskRef` SINTÉTICO respaldado
    /// por un `watch` del propio test — permite testear la lógica
    /// async/stateful de un frontend (síntesis de terminal en muerte de
    /// conexión, de-registro del canceller, read-after-write) sin engine ni
    /// daemon. El canceller es un token suelto (cancel = no-op observable
    /// vía `token.is_cancelled()` si el test conserva el clon). No es API
    /// estable: `doc(hidden)`, puede cambiar sin bump.
    #[doc(hidden)]
    #[must_use]
    pub fn synthetic_for_tests(id: TaskId, rx: watch::Receiver<TaskProgress>) -> Self {
        Self {
            id,
            rx,
            canceller: TaskCanceller::Embedded(CancellationToken::new()),
        }
    }

    /// Id de la task.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Snapshots vivos (mismo contrato que el `watch` del scheduler: el
    /// estado terminal siempre se publica salvo pérdida de conexión).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Handle clonable para cancelar desde otra task (Ctrl-C del CLI).
    #[must_use]
    pub fn canceller(&self) -> TaskCanceller {
        self.canceller.clone()
    }

    /// Una vista CLONABLE de esta task: id, progreso y cancelación, sin la
    /// espera (#173).
    ///
    /// Existe para el tablero de tasks de un frontend, que necesita PINTAR el
    /// progreso y PEDIR la cancelación, no poseer la task. Antes tenía que
    /// quedarse el [`TaskRef`] entero, así que quien lo lanzaba se quedaba sin
    /// él — y por eso una sincronización APLICÁNDOSE, que solo se puede parar
    /// desde su panel, no aparecía en el tablero: la operación más destructiva
    /// del programa era la única invisible.
    ///
    /// Que dos sitios puedan cancelar no rompe nada: [`TaskCanceller`] ya era
    /// clonable, y una cancelación es idempotente y cooperativa. Lo que sigue
    /// teniendo un solo dueño es la ESPERA.
    #[must_use]
    pub fn observer(&self) -> TaskObserver {
        TaskObserver {
            id: self.id,
            rx: self.rx.clone(),
            canceller: self.canceller.clone(),
        }
    }

    /// Petición de cancelación cooperativa.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    /// Espera el estado terminal. Si la conexión con el daemon muere sin
    /// desenlace conocido, devuelve `Failed{ProviderUnavailable}` — lo
    /// honesto que se puede decir desde fuera.
    pub async fn join(mut self) -> TaskState {
        loop {
            let state = self.rx.borrow().state.clone();
            if state.is_terminal() {
                return state;
            }
            if self.rx.changed().await.is_err() {
                let last = self.rx.borrow().state.clone();
                if last.is_terminal() {
                    return last;
                }
                return TaskState::Failed {
                    error: Error::ProviderUnavailable { retryable: true },
                };
            }
        }
    }

    fn from_handle(handle: &TaskHandle) -> Self {
        Self {
            id: handle.id(),
            rx: handle.progress(),
            canceller: TaskCanceller::Embedded(handle.cancel_token()),
        }
    }
}

/// Vista clonable de una task viva: lo que hace falta para PINTARLA y
/// PARARLA, sin poseerla ([`TaskRef::observer`], #173).
///
/// ```
/// use norte_core::backend::TaskRef;
/// use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
///
/// let (_tx, rx) = tokio::sync::watch::channel(TaskProgress {
///     task_id: TaskId::new(7),
///     kind: TaskKind::Sync,
///     state: TaskState::Running,
///     bytes_done: 0,
///     bytes_total: None,
///     entries_done: 0,
///     entries_total: None,
///     unreadable: None,
///     unvisited: None,
///     current: None,
/// });
/// let task = TaskRef::synthetic_for_tests(TaskId::new(7), rx);
/// let observador = task.observer();
/// // Dos observadores de la MISMA task, y la task sigue siendo de quien la lanzó.
/// assert_eq!(observador.clone().id(), task.id());
/// assert!(!observador.progress().borrow().state.is_terminal());
/// ```
#[derive(Clone)]
pub struct TaskObserver {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: TaskCanceller,
}

impl TaskObserver {
    /// Id de la task observada.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Snapshots vivos (el mismo `watch` que [`TaskRef::progress`]).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Pide la cancelación cooperativa. Idempotente, y sobre una task ya
    /// terminada no hace nada.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }
}

/// Cancelación clonable de una [`TaskRef`].
#[derive(Clone)]
pub enum TaskCanceller {
    /// Token del scheduler embebido.
    Embedded(CancellationToken),
    /// `task.cancel` contra el daemon (fire-and-forget: la confirmación
    /// real llega por `task.progress`, contrato del método). El asa la
    /// fabrica el SDK ([`norte_client::RemoteTaskCanceller`], ADR 0066).
    #[cfg(unix)]
    Remote(norte_client::RemoteTaskCanceller),
}

impl From<norte_client::RemoteTask> for TaskRef {
    /// Una task del daemon vista como [`TaskRef`]: el core no distingue de
    /// dónde vino, y por eso `Backend` puede devolver lo mismo desde el
    /// engine embebido y desde el SDK (ADR 0066).
    fn from(t: norte_client::RemoteTask) -> Self {
        let id = t.id();
        let rx = t.progress();
        let canceller = TaskCanceller::Remote(t.canceller());
        Self { id, rx, canceller }
    }
}

impl From<crate::engine::TransferOptions> for norte_client::TransferOptions {
    /// Las opciones del ENGINE, como las pone en los params un cliente
    /// remoto. Campo a campo y sin `..Default::default()` a propósito: un
    /// campo nuevo en cualquiera de las dos structs tiene que romper aquí y
    /// no perderse en el viaje.
    fn from(o: crate::engine::TransferOptions) -> Self {
        let crate::engine::TransferOptions {
            on_collision,
            symlinks,
            resume,
            verify,
        } = o;
        Self {
            on_collision,
            symlinks,
            resume,
            verify,
        }
    }
}

impl TaskCanceller {
    /// Dispara la cancelación cooperativa.
    pub fn cancel(&self) {
        match self {
            Self::Embedded(token) => token.cancel(),
            #[cfg(unix)]
            Self::Remote(canceller) => canceller.cancel(),
        }
    }
}

/// Evento de conexión del backend remoto (para la barra de mensajes).
///
/// Lo define el SDK ([`norte_client::ConnEvent`], ADR 0066) y se re-exporta
/// aquí: lo produce la reconexión, que vive allí, y lo consume un frontend,
/// que nombra `norte_core::backend`.
pub use norte_client::ConnEvent;

/// Observer de avisos de conexión (#44) que los reenvía por un canal: la vía
/// del `Backend::Embedded` para que una CLI/TUI EN PROCESO surface la
/// degradación igual que en modo daemon (donde el observer difunde por wire).
/// Mapea el `ConnectionWarning` del core al `ConnectionDegraded` del wire.
struct ChannelConnectionObserver {
    tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionDegraded>,
    /// El observer que ya estaba en la ranura, si lo había.
    ///
    /// La ranura del engine es de UNO y los dos canales se toman por separado,
    /// así que el segundo en instalarse tiene que seguir llamando al primero.
    /// Sin esto, `take_failed` después de `take_degraded` dejaba el canal de
    /// degradación mudo — y mudo en silencio, que es la peor forma.
    previo: Option<Arc<dyn crate::connect::ConnectionObserver>>,
}

impl crate::connect::ConnectionObserver for ChannelConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let _ = self.tx.send(norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        });
        if let Some(p) = &self.previo {
            p.on_connection_warning(w);
        }
    }

    fn on_connection_failure(&self, f: &crate::connect::ConnectionFailure) {
        if let Some(p) = &self.previo {
            p.on_connection_failure(f);
        }
    }
}

/// Gemelo del de arriba para los fallos (#322): una conexión que NO se abrió.
///
/// Dos observers y no uno con dos canales porque los dos `take_*` son
/// independientes: un frontend puede querer el aviso de seguridad y no el
/// diagnóstico, o al revés, y forzar los dos a la vez convertiría a uno en la
/// condición del otro.
struct ChannelFailureObserver {
    tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionFailed>,
    previo: Option<Arc<dyn crate::connect::ConnectionObserver>>,
}

impl crate::connect::ConnectionObserver for ChannelFailureObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        if let Some(p) = &self.previo {
            p.on_connection_warning(w);
        }
    }

    fn on_connection_failure(&self, f: &crate::connect::ConnectionFailure) {
        let _ = self.tx.send(norte_proto::methods::ConnectionFailed {
            conn: f.conn.clone(),
            scheme: f.scheme.clone(),
            host: f.host.clone(),
            reason: f.reason.wire().to_owned(),
            detail: f.detail.clone(),
        });
        if let Some(p) = &self.previo {
            p.on_connection_failure(f);
        }
    }
}

/// A dónde van los avisos de los hooks en `Backend::Embedded` (ADR 0100): a
/// un canal que el frontend drena, igual que en modo daemon los difunde el
/// wire. Sin esto el embebido correría los hooks y se tragaría sus frases.
struct ChannelHookSink {
    tx: mpsc::UnboundedSender<norte_proto::methods::PluginNotice>,
}

impl crate::hooks::HookNoticeSink for ChannelHookSink {
    fn notice(&self, n: norte_proto::methods::PluginNotice) {
        let _ = self.tx.send(n);
    }

    /// El frontend soltó el receptor: el despachador termina con él.
    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Sink de avisos del journal perezoso (#177) que los reenvía por un canal: la
/// vía para que una TUI embebida pinte EN LA SESIÓN que sus mutaciones no están
/// quedando registradas.
///
/// Gemelo de [`ChannelConnectionObserver`] y por el mismo motivo: el aviso nace
/// dentro del core, en mitad de una mutación, y el frontend no tiene forma de
/// preguntárselo a nadie después.
struct ChannelJournalSink {
    tx: mpsc::UnboundedSender<crate::embedded::JournalStatus>,
}

impl crate::embedded::JournalWarningSink for ChannelJournalSink {
    fn on_no_journal(&self, why: &crate::embedded::NoJournal) {
        let _ = self
            .tx
            .send(crate::embedded::JournalStatus::Lost(why.clone()));
    }

    fn on_journal_recovered(&self) {
        let _ = self.tx.send(crate::embedded::JournalStatus::Recovered);
    }

    fn on_journal_squatted(&self) {
        let _ = self.tx.send(crate::embedded::JournalStatus::Squatted);
    }
}

/// La «conexión» del brazo EMBEBIDO, para lo que la lleva por llave: hoy solo
/// el spool de planes de sincronización, que ata cada plan aprobado a la
/// conexión que lo produjo (ADR 0049).
///
/// In-process hay exactamente UNA, y por eso es una constante y no un contador:
/// planificar y aplicar tienen que casar, y dos ids distintos harían que un
/// `sync_apply` de este mismo `Backend` contestara `PlanStale` a su propio plan.
///
/// `u64::MAX` y no `0` deliberadamente: los `conn_id` del daemon salen de un
/// contador que arranca en cero, así que este valor no puede coincidir con
/// ninguno en un proceso que tenga las dos cosas a la vez — el spool no llegaría
/// a confundir el plan de un cliente del socket con el de un `Backend`
/// embebido. (Y el barrido por conexión compara el prefijo `"<id>-"` del nombre
/// del fichero, que tampoco colisiona.)
///
/// **Corolario, para quien exponga estos métodos:** el `plan_hash` NO es un
/// secreto —es un digest determinista de las raíces, las opciones y los pasos,
/// calculable por cualquiera que pueda leer los dos árboles—, así que la única
/// cosa que ata un plan a quien lo pidió es este `conn_id`, y aquí es una
/// constante. Todo lo que alcance este brazo comparte la misma conexión y actúa
/// como `Actor::User`, o sea sin gate de policy. `sync_plan`/`sync_apply` no
/// deben cablearse a un entorno de scripting ni al host de plugins sin un actor
/// propio: sería una escritura de árbol entero sin puerta.
const EMBEDDED_CONN_ID: u64 = u64::MAX;

/// El backend REMOTO vive en el SDK desde ADR 0066 y se re-exporta aquí
/// para que los consumidores de siempre (MCP, tests e2e) sigan nombrándolo
/// donde lo nombraban.
pub mod remote {
    pub use norte_client::remote::*;
}

/// El core detrás de una única superficie (regla 7).
pub enum Backend {
    /// Core in-process: arranque instantáneo, sin daemon.
    Embedded(Arc<Engine>),
    /// Contra el daemon UDS (ADR 0011).
    #[cfg(unix)]
    Remote(norte_client::RemoteBackend),
}

impl Clone for Backend {
    /// Clon BARATO: comparte engine/conexión (Arc interno en ambas variantes).
    /// OJO: los canales one-shot (`take_foreign_tasks`, `take_conn_events`,
    /// `take_approvals`, `take_degraded`, `take_journal_warnings`) son del
    /// PRIMER dueño — un clon
    /// (p. ej. para scripting Lua, tasks 4-5) no debe llamarlos.
    fn clone(&self) -> Self {
        match self {
            Self::Embedded(e) => Self::Embedded(Arc::clone(e)),
            #[cfg(unix)]
            Self::Remote(r) => Self::Remote(r.clone()),
        }
    }
}

impl Backend {
    /// ¿Registra este backend sus mutaciones en un journal, y por tanto se
    /// pueden deshacer?
    ///
    /// Hoy es exactamente «va contra el daemon», y contesta por lo que este
    /// tipo puede PROMETER, no por lo que un proceso concreto haya montado.
    /// El brazo embebido lleva el journal del directorio de estado desde #167
    /// (`norte_core::embedded`) pero lo abre PEREZOSO y sobre un lock
    /// exclusivo que otro proceso puede tener; y el spool que
    /// [`Self::sync_plan`] necesita no lo instala este tipo sino quien
    /// construye el engine —`norte-cli` lo hace, y solo para `norte sync`—,
    /// así que ni el journal ni el spool son ciertos por construcción. Un
    /// `true` aquí sería una promesa que este valor no puede sostener.
    ///
    /// De modo que lo que dice es: **«hay un daemon detrás»**, que es la única
    /// configuración en la que las dos cosas están garantizadas de antemano.
    /// La pregunta VIVA —«¿queda registrada ESTA sesión?», la que hay que
    /// contestarle a un humano antes de que diga que sí— es
    /// [`Self::ensure_journal`], que abre el journal para responder; ésta no
    /// abre nada.
    ///
    /// Es una pregunta sobre el TRANSPORTE y no sobre el engine porque desde
    /// fuera no hay forma de preguntárselo al engine: `Engine` no publica si
    /// tiene journal, y un frontend que lo dedujera del primer `Unsupported`
    /// se habría enterado después de enseñar un plan.
    ///
    /// Un frontend la usa para ATENUAR antes de que el lector pulse la tecla
    /// (`norte_frontend::availability::Facts::journalled`), no para saltarse
    /// ninguna comprobación: quien decide sigue siendo el core.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// assert!(!Backend::Embedded(Arc::new(Engine::new())).is_journalled());
    /// ```
    #[must_use]
    pub fn is_journalled(&self) -> bool {
        match self {
            Self::Embedded(_) => false,
            #[cfg(unix)]
            Self::Remote(_) => true,
        }
    }

    /// ¿Hay un daemon al otro lado, o el core está en ESTE proceso?
    ///
    /// La pregunta del transporte, desnuda, que es distinta de
    /// [`Self::is_journalled`]: aquella dice qué se puede PROMETER (journal y
    /// spool) y ésta dice si existe un segundo proceso del que hablar.
    ///
    /// Existe porque hay superficies que no son «puedo o no puedo» sino «hay
    /// otro sitio o no lo hay», y la primera es el panel de registro (#328).
    /// Con el core embebido hay un solo anillo —el de este proceso, que es el
    /// que el panel ya lee—, así que preguntarle al backend por el registro
    /// del daemon contesta [`Error::Unsupported`] con toda la razón, y un
    /// frontend que tratara esa respuesta como un hecho sobre un daemon
    /// acabaría diciendo «este daemon no sirve su registro» donde no hay
    /// ninguno. La respuesta correcta ahí no es otra frase: es **no
    /// preguntar**, y no mencionar a nadie.
    ///
    /// Se decide UNA vez y no cambia: el `Backend` no cambia de brazo en vida
    /// del proceso.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// assert!(!Backend::Embedded(Arc::new(Engine::new())).is_remote());
    /// ```
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        match self {
            Self::Embedded(_) => false,
            #[cfg(unix)]
            Self::Remote(_) => true,
        }
    }

    /// Abre ya el journal (si hace falta) y dice si ESTA sesión queda
    /// registrada — la pregunta que un frontend hace justo antes de mutar y
    /// necesita CONTESTAR al humano antes del sí, no después (`norte ai
    /// rename`, y desde esta tarea `norte sync`).
    ///
    /// - Embebido: delega en [`Engine::ensure_journal`], que toma el lock
    ///   perezoso AQUÍ (no en la primera mutación) y lo conserva hasta que se
    ///   suelte por ocioso ([`Self::release_journal_if_idle`], que el TUI
    ///   llama en su tick — #179).
    /// - Remoto: siempre `true`. El daemon es DUEÑO del journal y se niega a
    ///   arrancar sin uno (ver el arranque de `norte daemon run`); no hay un
    ///   viaje de ida y vuelta que hacer para saberlo, y una conexión remota
    ///   sin journal no es un estado que este proceso pueda observar ni
    ///   remediar — solo el operador del daemon puede.
    ///
    /// A diferencia de [`Self::is_journalled`] (que en el brazo embebido dice
    /// `false` a propósito, ver su rustdoc), esto SÍ abre el journal cuando
    /// puede: es la llamada de quien está a punto de mutar, no la de quien
    /// solo quiere atenuar una tecla.
    ///
    /// Un engine recién construido no tiene de dónde sacarlo, y entonces la
    /// respuesta honesta es `false` — no un error:
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(!rt.block_on(backend.ensure_journal()));
    /// ```
    pub async fn ensure_journal(&self) -> bool {
        match self {
            Self::Embedded(engine) => engine.ensure_journal().await,
            #[cfg(unix)]
            Self::Remote(_) => true,
        }
    }

    /// Suelta el journal si lleva `ocioso` sin usarse (#179).
    ///
    /// Un `ntc` que copió un fichero a las 09:00 se quedaba `journal.db` hasta
    /// salir, así que `norte daemon run` y `norte audit` no podían abrirlo en
    /// todo el día. La ventana se reabre sola en la siguiente mutación, y la
    /// reapertura RELEE la cadena — que es lo que hace que soltar sea seguro.
    ///
    /// Remoto: `true` sin hacer nada. El journal es del DAEMON, que se niega a
    /// arrancar sin uno; soltarlo desde aquí no es que sea inútil, es que no
    /// es de este proceso.
    ///
    /// # Esto NO es cancel-safe. En el CUERPO de una rama de `select!`, jamás
    /// en su condición (ver
    /// [`LazyJournal::release`](crate::embedded::LazyJournal::release)).
    pub async fn release_journal_if_idle(&self, ocioso: std::time::Duration) -> bool {
        match self {
            Self::Embedded(engine) => engine.release_journal_if_idle(ocioso).await,
            #[cfg(unix)]
            Self::Remote(_) => true,
        }
    }

    /// Lo mismo, diciendo POR QUÉ no — ver [`Engine::journal_obstacle`].
    ///
    /// `None` = esta sesión registra, o no hay ventana que perder (el daemon al
    /// otro lado de un socket, o un `Engine::new()`).
    ///
    /// Existe por lo que #178 partió en dos: con `Busy` la operación ocurriría
    /// sin registro —y el remedio es `--daemon`, hablar con quien tiene el
    /// fichero— y con `Failed` no va a ocurrir en absoluto, y ahí `--daemon` no
    /// es remedio ninguno porque el daemon se niega a arrancar con ese mismo
    /// fichero. Un `bool` manda a la mitad de los usuarios contra la pared
    /// equivocada.
    pub async fn journal_obstacle(&self) -> Option<crate::embedded::NoJournal> {
        match self {
            Self::Embedded(engine) => engine.journal_obstacle().await,
            #[cfg(unix)]
            Self::Remote(_) => None,
        }
    }

    /// Suelta los planes de sincronización que ESTE backend retiene, como hace
    /// el daemon cuando se le cae una conexión.
    ///
    /// El derecho a aplicar un plan vive en un registro EN MEMORIA que muere
    /// con el proceso, así que lo que esto se lleva no es un plan aplicable
    /// sino su fichero: un listado con las rutas relativas de los dos árboles,
    /// legible por quien pueda leer el directorio de estado. El daemon lo
    /// suelta en dos sitios —barrido al arrancar y `Spool::drop_connection` al
    /// cerrar cada conexión— y un proceso embebido no tiene ninguno de los
    /// dos: **es el llamante quien tiene que llamar a esto al salir**, por
    /// todos los caminos, incluido el que no aplicó nada.
    ///
    /// No barre el directorio entero, y no es un descuido: un proceso embebido
    /// comparte el directorio de estado con un daemon que puede estar vivo, y
    /// no tiene el lock del journal con el que demostrar que no lo está. Se
    /// lleva lo suyo y nada más.
    ///
    /// Sin spool instalado (todo el que no sea `norte sync`) y contra el
    /// daemon es un no-op: allí el dueño del spool es el daemon, y el
    /// desmontaje de la conexión ya lo hace él.
    ///
    /// No devuelve nada ni falla: un fichero que no se deja borrar queda en el
    /// `tracing::warn!` y en el código de salida del comando, que es de la
    /// sincronización y no de la limpieza (mismo criterio que el barrido de
    /// arranque del daemon, ver [`crate::sync::Spool::sweep`]).
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// // Sin spool instalado no hay nada que soltar, y decirlo no cuesta.
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// rt.block_on(backend.drop_retained_plans());
    /// ```
    pub async fn drop_retained_plans(&self) {
        match self {
            Self::Embedded(engine) => {
                let Some(spool) = engine.spool() else { return };
                match spool.drop_connection(EMBEDDED_CONN_ID).await {
                    Ok(report) if report.is_clean() => {}
                    Ok(report) => tracing::warn!(
                        failed = report.failed,
                        "quedaron spools de sincronización sin borrar"
                    ),
                    Err(e) => tracing::warn!(error = %e, "no se pudo soltar el spool"),
                }
            }
            #[cfg(unix)]
            Self::Remote(_) => {}
        }
    }
}

/// El backend remoto (solo unix, como el daemon — ADR 0011).
#[cfg(unix)]
#[cfg(test)]
mod observer_tests {
    use super::*;

    fn progreso(id: u64) -> (watch::Sender<TaskProgress>, watch::Receiver<TaskProgress>) {
        let (tx, rx) = watch::channel(TaskProgress {
            task_id: TaskId::new(id),
            kind: norte_proto::TaskKind::Sync,
            state: TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        // El emisor se devuelve para que el test lo retenga vivo: un `watch`
        // sin emisor no es lo que este test observa.
        (tx, rx)
    }

    /// #173: el observador cancela LA MISMA task, no una copia inerte. Es la
    /// propiedad de la que depende que un tablero pueda parar una
    /// sincronización que se está aplicando sin quitarle el handle a su panel.
    #[test]
    fn el_observador_cancela_la_misma_task() {
        let (_tx, rx) = progreso(7);
        let task = TaskRef::synthetic_for_tests(TaskId::new(7), rx);
        let TaskCanceller::Embedded(token) = task.canceller() else {
            panic!("un TaskRef sintético cancela con un token embebido");
        };
        assert!(!token.is_cancelled());
        let observador = task.observer();
        // Y clonado: el tablero clona su fila al reordenarla.
        observador.clone().cancel();
        assert!(token.is_cancelled(), "la cancelación llega a la task real");
        assert_eq!(observador.id(), task.id());
    }

    /// Y observar no consume: quien lanzó la task se la queda entera —
    /// incluida la ESPERA, que es lo único que sigue teniendo un solo dueño.
    #[tokio::test]
    async fn observar_no_le_quita_la_task_a_quien_la_lanzo() {
        let (tx, rx) = watch::channel(TaskProgress {
            task_id: TaskId::new(9),
            kind: norte_proto::TaskKind::Sync,
            state: TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        let task = TaskRef::synthetic_for_tests(TaskId::new(9), rx);
        let observador = task.observer();
        tx.send_modify(|p| p.state = TaskState::Completed);
        assert_eq!(
            observador.progress().borrow().state,
            TaskState::Completed,
            "el observador ve el mismo canal"
        );
        assert!(matches!(task.join().await, TaskState::Completed));
    }
}
