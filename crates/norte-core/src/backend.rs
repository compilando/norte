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

use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskProgress, TaskState, VPath,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::Engine;
use crate::engine::TransferOptions;
use crate::scheduler::TaskHandle;

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

    /// Listado de un directorio como STREAM perezoso (ADR 0017). Embebido =
    /// el stream del engine tal cual; remoto = primera página EAGER (paridad
    /// de errores: `NotFound`/`TypeMismatch` en el `Result`, no como primer
    /// item) + páginas siguientes por cursor. Soltar el stream lo cancela.
    ///
    /// Devuelve además las omitidas del CONTENEDOR (#93): entradas que su
    /// índice descartó por nombres hostiles/límites (providers archive) y que
    /// por tanto JAMÁS saldrán del stream. `None` = no aplica (el backend
    /// lista todo lo que existe). Disponible al abrir en ambos modos: el
    /// embebido consulta el índice ya caliente; el remoto lo trae la primera
    /// página (todas la repiten).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream(&self, dir: &VPath) -> Result<(EntryStream, Option<u64>), Error> {
        self.list_stream_with(dir, &[]).await
    }

    /// [`Backend::list_stream`] pidiendo atributos por entrada (#108 bloque
    /// 2). `attrs` son ids del catálogo (`Backend::attr_catalog`); un id no
    /// anunciado viene ausente, jamás es error.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_stream_with(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(EntryStream, Option<u64>), Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let stream = engine.list_with(dir, &opt).await?;
                // Mismo cinturón de emisión que el daemon (ADR 0039 §5): un
                // provider con bug no cuela ids no pedidos ni valores sobre
                // tope por la ruta in-process.
                let belt = opt.attrs.clone();
                let stream = stream
                    .map(move |item| {
                        item.map(|mut e| {
                            belt.retain_conforming(&mut e);
                            e
                        })
                    })
                    .boxed();
                // Best-effort: un fallo aquí no tumba un listado que ya abrió
                // (mismo contrato que el daemon) — degrada a "desconocido",
                // pero JAMÁS en silencio (el punto de #93 es la señal).
                let skipped = engine.list_skipped(dir).await.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "list_skipped falló; omitidas = desconocido");
                    None
                });
                Ok((stream, skipped))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.list_stream(dir, attrs.to_vec()).await,
        }
    }

    /// Listado COMPLETO (drena [`Backend::list_stream`]). El `ls` remoto de un
    /// dir gigante ya no arriesga el `CALL_TIMEOUT` ni un frame monstruoso: son
    /// N páginas acotadas por debajo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list(&self, dir: &VPath) -> Result<Vec<Entry>, Error> {
        Ok(self.list_with_skipped(dir).await?.0)
    }

    /// [`Backend::list`] + las omitidas del contenedor (#93) — para frontends
    /// que quieran señalizarlas (`ls` de la CLI).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped(&self, dir: &VPath) -> Result<(Vec<Entry>, Option<u64>), Error> {
        self.list_with_skipped_attrs(dir, &[]).await
    }

    /// [`Backend::list_with_skipped`] pidiendo atributos por entrada (#108
    /// bloque 2) — el `ls --attrs` de la CLI.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list_with_skipped_attrs(
        &self,
        dir: &VPath,
        attrs: &[String],
    ) -> Result<(Vec<Entry>, Option<u64>), Error> {
        let (mut stream, skipped) = self.list_stream_with(dir, attrs).await?;
        let mut entries = Vec::new();
        while let Some(item) = stream.next().await {
            entries.push(item?);
        }
        Ok((entries, skipped))
    }

    /// Lectura de PRESENTACIÓN (viewer): junta el rango pedido en memoria.
    /// El caller acota (`len`) — esto no es el camino de las copias.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn read(&self, path: &VPath, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
        match self {
            Self::Embedded(engine) => {
                let mut stream = engine.read(path, range).await?;
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(out)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.read(path, range).await,
        }
    }

    /// Capabilities del provider que sirve `path` (F8/papelera, ADR 0009).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
        match self {
            Self::Embedded(engine) => engine.capabilities(path).await,
            #[cfg(unix)]
            Self::Remote(r) => r.capabilities(path).await,
        }
    }

    /// Catálogo de attrs del provider que sirve `path` (#108 bloque 2),
    /// SIEMPRE saneado: el embebido pasa por `Engine::attr_catalog`
    /// (`AttrCatalog::new`, ADR 0039 §4) y el remoto por el deserializador
    /// del wire (mismo saneo por el tipo).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        match self {
            Self::Embedded(engine) => engine.attr_catalog(path).await,
            #[cfg(unix)]
            Self::Remote(r) => r.attr_catalog(path).await,
        }
    }

    /// Both halves of `fs.capabilities` for `path`: the capability flags AND
    /// the attribute catalogue, in ONE round trip.
    ///
    /// [`Self::capabilities`] and [`Self::attr_catalog`] each throw the other
    /// half of that response away, so a frontend that wants both — the TUI
    /// caches the catalogue for its columns and the flags to answer
    /// "read-only?" without asking again — paid two round trips for one
    /// message. Remote mode makes a single `fs.capabilities` call here;
    /// embedded mode asks the engine twice, which is two provider lookups
    /// instead of one and no extra round trip on the wire. Not "no I/O at
    /// all": both halves go through `Engine::provider_for`, which for a remote
    /// scheme can resolve or establish the connection first — true of
    /// `file://`, false of an embedded `sftp` pane.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn capabilities_and_attrs(
        &self,
        path: &VPath,
    ) -> Result<(Capabilities, norte_proto::AttrCatalog), Error> {
        match self {
            Self::Embedded(engine) => Ok((
                engine.capabilities(path).await?,
                engine.attr_catalog(path).await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => {
                let full = r.capabilities_full(path).await?;
                Ok((full.capabilities, full.attrs))
            }
        }
    }

    /// Metadatos de un nodo (`fs.stat`).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
        self.stat_attrs(path, &[]).await
    }

    /// [`Backend::stat`] pidiendo atributos por entrada (#108 bloque 2).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat_attrs(&self, path: &VPath, attrs: &[String]) -> Result<Entry, Error> {
        match self {
            Self::Embedded(engine) => {
                let opt = norte_vfs::ListOptions {
                    attrs: norte_vfs::AttrRequest::sanitized(attrs.to_vec()),
                };
                let mut entry = engine.stat_with(path, &opt).await?;
                // Mismo cinturón de emisión que el daemon (ADR 0039 §5).
                opt.attrs.retain_conforming(&mut entry);
                Ok(entry)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.stat(path, attrs.to_vec()).await,
        }
    }

    /// Retiene el ancla de un directorio que un PANEL acaba de listar (#301,
    /// ADR 0073), para que la escritura que venga después pueda decir «el
    /// destino era ESE».
    ///
    /// Es lo que el daemon pone en la respuesta de `fs.list` y el SDK guarda
    /// por su cuenta. Aquí no hay wire, así que lo guarda el engine — y sin
    /// esto `ntc`, que corre embebido por DEFECTO, hacía toda operación
    /// anclada SIN ancla: la comprobación que ADR 0076 pidió justo para
    /// `fs.create` no la tenía el único frontend que lanza un `$EDITOR` sobre
    /// lo creado.
    ///
    /// # Se llama a mano, y ese es el punto
    ///
    /// No lo hace `list_stream_with`, que es el embudo de TODOS los listados:
    /// por ahí pasan el árbol lateral (una rama por vuelta del bucle) y el
    /// `fs.list` de un script Lua, y como recordar SOBRESCRIBE, cualquiera de
    /// ellos rebendecía el ancla del panel con el nodo que viera en ese
    /// momento. El ancla dice **quién miró**; un listado que no es una
    /// pantalla no ha mirado nadie.
    ///
    /// Contra el daemon no hace nada: allí el ancla la manda el listado en su
    /// respuesta y la guarda el SDK, que es de quien listó de verdad.
    ///
    /// Best-effort: un provider que no sabe dar identidad de nodo (un bucket,
    /// un SFTP sin extensiones) no puede impedir un listado, y un fallo BORRA
    /// la que hubiera —mandar una vieja sería que la escritura se rechazara a
    /// sí misma—, así que la escritura siguiente se comporta como en 0.53.
    pub async fn remember_listing_anchor(&self, dir: &VPath) {
        match self {
            Self::Embedded(engine) => {
                let ancla = engine.dir_anchor(dir).await.unwrap_or_else(|e| {
                    tracing::debug!(error = %e, "dir_anchor falló; sin ancla para este listado");
                    None
                });
                engine.remember_dir_anchor(dir, ancla);
            }
            #[cfg(unix)]
            Self::Remote(_) => {}
        }
    }

    /// El ancla retenida del directorio en el que `destino` va a escribirse
    /// (#301).
    ///
    /// `destino` es la ruta EXACTA de lo que se escribe, así que lo que se
    /// busca es su PADRE: es el directorio que el humano listó y aprobó. La
    /// misma cuenta que hace el SDK en el camino remoto.
    ///
    /// `None` —nadie listó ese directorio en esta sesión, o su provider no
    /// sabe dar identidad de nodo— se comporta exactamente como 0.53: se
    /// confina igual y esa comprobación no ocurre.
    fn ancla_del_destino(engine: &Engine, destino: &VPath) -> Option<norte_proto::DirAnchor> {
        engine.remembered_dir_anchor(&destino.parent()?)
    }

    /// Copia como task.
    ///
    /// El ancla del directorio DESTINO viaja con la operación cuando este
    /// backend lo listó (#301, ADR 0073) — igual que la pone el SDK en el
    /// camino remoto, y por el mismo motivo: entre listar y escribir, ese
    /// directorio puede haber dejado de ser el nodo que el humano miraba.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn copy(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .copy_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::ancla_del_destino(engine, to),
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Copy, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Move como task. Con el ancla del destino, como [`Self::copy`].
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn move_(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .move_anchored(
                        from,
                        to,
                        opts,
                        crate::journal::Actor::User,
                        Self::ancla_del_destino(engine, to),
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r
                .transfer(norte_client::Transfer::Move, from, to, opts.into())
                .await
                .map(TaskRef::from),
        }
    }

    /// Borrado como task (papelera o permanente, ADR 0009).
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn delete(&self, path: &VPath, mode: DeleteMode) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                Ok(TaskRef::from_handle(&engine.delete_with(path, mode).await?))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.delete(path, mode).await.map(TaskRef::from),
        }
    }

    /// Creación de UN directorio como Task (#104, F7). Sin `-p`; destino
    /// ocupado = `Conflict{Exists}`.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn mkdir(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.mkdir(path).await?)),
            #[cfg(unix)]
            Self::Remote(r) => r.mkdir(path).await.map(TaskRef::from),
        }
    }

    /// Creación de UN fichero VACÍO como Task (#290). Destino ocupado =
    /// `Conflict{Exists}`; la exclusividad la aporta el provider (atómica en
    /// local y en objetos, con ventana en SFTP v3).
    ///
    /// Con el ancla del directorio, como [`Self::copy`] — y aquí es donde más
    /// falta hace (#301): `fs.create` es el único método cuyo éxito entrega
    /// una ruta a un programa de FUERA de norte (`$EDITOR`), que es el motivo
    /// con el que ADR 0076 justificó ponerle ancla.
    ///
    /// # Errors
    /// Taxonomía del protocolo.
    pub async fn create_file(&self, path: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(
                &engine
                    .create_file_as(
                        path,
                        Self::ancla_del_destino(engine, path),
                        crate::journal::Actor::User,
                    )
                    .await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => r.create_file(path).await.map(TaskRef::from),
        }
    }

    /// Cambia los permisos POSIX de un lote de rutas como Task (#314).
    ///
    /// Muta: journal con reversa —el modo anterior— y gate de política. Una
    /// ubicación sin permisos POSIX responde `Unsupported` y no cambia nada.
    ///
    /// # Errors
    /// Taxonomía del protocolo: [`Error::InvalidPath`] sin rutas, por encima
    /// del tope o con bits que no son de permiso; [`Error::PolicyDenied`];
    /// [`Error::Unsupported`].
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.set_mode(params).await?)),
            #[cfg(unix)]
            Self::Remote(r) => r.set_mode(params).await.map(TaskRef::from),
        }
    }

    /// El plan REVISABLE de un lote de renames dentro de `dir` (spec §17, ADR
    /// 0042). NO muta nada: ni Task, ni journal.
    ///
    /// Lo que se manda es INTENCIÓN — parejas de nombres base. El orden, los
    /// temporales y los veredictos los decide el core (regla dura 7), y el
    /// `plan_hash` que vuelve es el que hay que devolver a
    /// [`Backend::rename_batch`] para ejecutar EXACTAMENTE lo que se enseñó.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si un nombre no es una entrada de directorio
    /// legal o si hay más de `FS_RENAME_BATCH_MAX_PAIRS` parejas;
    /// [`Error::Unsupported`] sin provider o con uno de solo lectura;
    /// [`Error::LimitExceeded`] en un directorio inabarcable;
    /// [`Error::PolicyDenied`] del gate de lectura (remoto, agente sin scope);
    /// taxonomía del protocolo.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
    ) -> Result<norte_proto::methods::FsRenameBatchPlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let plan = engine.rename_batch_plan(dir, &raw).await?;
                crate::rename::plan_to_proto(&plan)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_plan(dir, pairs).await,
        }
    }

    /// Ejecuta el lote aprobado como UNA Task y UNA unidad deshacible del
    /// journal (spec §17, ADR 0042).
    ///
    /// `plan_hash` es el token de FRESCURA de [`Backend::rename_batch_plan`],
    /// atado al directorio. El core re-planifica el directorio TAL COMO ESTÁ
    /// AHORA y compara: si derivó, esto es [`Error::PlanStale`] y no se toca
    /// nada. No es una prueba de aprobación —el digest es público y calculable
    /// sin haber pedido el plan—: garantiza QUÉ se ejecuta, no que alguien lo
    /// mirara. El informe de lo que
    /// pasó se pide con [`Backend::rename_batch_report`] — la Task terminal
    /// cuenta la causa, no lo que se quedó a medias.
    ///
    /// # Errors
    /// [`Error::PlanStale`] si el directorio derivó desde el plan;
    /// [`Error::PlanNotExecutable`] si el plan aprobado tenía colisiones;
    /// [`Error::PolicyDenied`] del gate de mutación; más las de
    /// [`Backend::rename_batch_plan`].
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let (handle, _report) = engine.rename_batch(dir, &raw, plan_hash).await?;
                // El informe queda en el anillo del engine, que es de donde lo
                // lee `rename_batch_report`: los dos brazos se piden igual.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .rename_batch(dir, pairs, plan_hash)
                .await
                .map(TaskRef::from),
        }
    }

    /// El informe de un lote ya lanzado (`fs.rename_batch_report`, 0.36.0):
    /// cuántos pasos se aplicaron, cuántos se deshicieron y —lo que ningún
    /// error pelado puede decir— QUÉ paso se quedó aplicado y bajo qué nombre.
    ///
    /// Míralo también cuando la Task diga `cancelled`: cancelar un lote lo
    /// deshace, y un rollback también puede atascarse.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese `task_id` nunca fue un lote de este proceso
    /// o si el anillo ya lo desalojó — y el brazo remoto contesta lo MISMO,
    /// porque el daemon manda esa categoría y no un `-32602` sin taxonomía;
    /// [`Error::Unsupported`] contra un daemon N-1 que no conoce el método;
    /// taxonomía del protocolo.
    pub async fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsRenameBatchReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .rename_batch_report(task_id)
                .map(|(_owner, r)| crate::rename::report_to_proto(&r))
                // Embebido no hay actor que comprobar: este `Backend` ES el
                // humano en proceso (mismo criterio que
                // `plugins_set_approval`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_report(task_id).await,
        }
    }

    /// (Re)construye el índice de `root` como Task (M4, ADR 0034).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; taxonomía del protocolo.
    pub async fn index_build(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .index_build_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_build(root).await.map(TaskRef::from),
        }
    }

    /// Consulta el índice de `root` por `text` (M4). Devuelve hits del protocolo.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; taxonomía del protocolo.
    pub async fn index_query(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
    ) -> Result<Vec<norte_proto::methods::IndexHit>, Error> {
        match self {
            Self::Embedded(engine) => {
                let hits = engine
                    .index_query_as(root, text, limit, crate::journal::Actor::User)
                    .await?;
                Ok(hits.into_iter().map(index_hit_to_proto).collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_query(root, text, limit).await,
        }
    }

    /// Genera embeddings de los ficheros ya indexados de `root` como Task
    /// (M4-IA-2). Requiere `index.build` previo del MISMO root.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice o sin proveedor de embeddings;
    /// [`Error::NotFound`] sin `index.build` previo (en la RESPUESTA, no en
    /// el join); [`Error::PolicyDenied`] del gate de IA; taxonomía del
    /// protocolo.
    pub async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .index_embed_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_embed(root).await.map(TaskRef::from),
        }
    }

    /// Búsqueda semántica sobre los embeddings del índice (M4-IA-2):
    /// `root = None` busca en todos los roots. AMBOS brazos acotados por
    /// `AI_CALL_TIMEOUT` (el embed de la query va al proveedor), como
    /// [`Backend::ai_rename_plan`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice o sin proveedor de embeddings;
    /// [`Error::PolicyDenied`] del gate de IA;
    /// [`Error::ProviderUnavailable`] (retryable) al agotar el timeout;
    /// taxonomía del protocolo.
    pub async fn index_search_semantic(
        &self,
        root: Option<&VPath>,
        query: &str,
        k: u32,
    ) -> Result<Vec<norte_proto::methods::SemanticHit>, Error> {
        match self {
            Self::Embedded(engine) => {
                let hits = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.index_search_semantic(root, query, k),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(hits
                    .into_iter()
                    .map(|(path, score)| norte_proto::methods::SemanticHit { path, score })
                    .collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_search_semantic(root, query, k).await,
        }
    }

    /// Plan de rename revisable de `dir` vía IA (M4-IA, ADR 0031). NO muta:
    /// aplicar el plan son N [`Backend::move_`] gobernados. AMBOS brazos
    /// están acotados por `AI_CALL_TIMEOUT` (2 min): un endpoint de proveedor en
    /// dead-air jamás cuelga el frontend embebido ni el remoto.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin proveedor de IA; [`Error::PolicyDenied`]
    /// del gate de IA (off, local-only, denied prefix);
    /// [`Error::ProviderUnavailable`] (retryable) al agotar el timeout;
    /// taxonomía del protocolo para fallos del proveedor.
    /// `names` son los basenames MARCADOS (#121). Vacío = el directorio
    /// entero, que es lo que este método hacía: con la selección de primera
    /// clase, pedir un plan sobre cinco ficheros mandaba los mil del
    /// directorio al proveedor.
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_rename_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(crate::ai::ai_plan_to_proto(plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_rename_plan(dir, instruction, names).await,
        }
    }

    /// Plan de ORGANIZAR por IA (0.77.0, fase 8): revisable, no muta nada.
    ///
    /// # Errors
    /// `Unsupported` sin proveedor; el gate de IA con su motivo; la taxonomía
    /// del protocolo. `ProviderUnavailable` al agotar el timeout, como su
    /// hermano.
    pub async fn ai_organize_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiOrganizePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_organize_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(norte_proto::methods::AiOrganizePlanResult {
                    moves: plan.moves,
                    refused: None,
                })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_organize_plan(dir, instruction, names).await,
        }
    }

    /// Aplica un plan de organizar (0.77.0, fase 8): crea las carpetas y
    /// mueve, como UN lote deshacible.
    ///
    /// # Errors
    /// `PlanStale` si el token no es el del plan revisado; `InvalidPath` si
    /// algún destino se sale del directorio; la taxonomía del protocolo.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[norte_proto::methods::OrganizeMove],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .organize(dir, moves, plan_hash, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.organize(dir, moves, plan_hash).await.map(TaskRef::from),
        }
    }

    /// GC de staging `.norte-partial` huérfano bajo `dir` (#11, ADR 0012):
    /// operación PUNTUAL, no una Task ni una mutación del journal. Devuelve
    /// cuántos barrió.
    ///
    /// # Errors
    /// En `Remote` es [`Error::Unsupported`]: no existe (aún) un método de
    /// wire para el GC — exponerlo exige un cambio de protocolo, diferido
    /// hasta que haya demanda. En `Embedded`, los del provider.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        match self {
            Self::Embedded(engine) => engine.gc_partials(dir, older_than).await,
            #[cfg(unix)]
            Self::Remote(_) => Err(Error::Unsupported),
        }
    }

    /// Volúmenes del host (`host.volumes`, 0.37.0, #131): mount point, tipo
    /// de filesystem, kind y espacio libre/total. `include_pseudo` es el
    /// toggle "mostrar todo" del picker (diseño §E de
    /// `2026-08-10-volumes-design.md`).
    ///
    /// Embebido: llama a [`crate::volumes::enumerate`] directamente — un
    /// volumen es del HOST, no de un provider, así que no hay engine que
    /// consultar (diseño §A). SIN gate de actor: un core embebido no tiene
    /// conexión ni daemon, así que quien lo llama YA es el humano sentado
    /// delante — no hay superficie remota que sandboxear.
    ///
    /// Remoto: `host.volumes` contra el daemon, que SÍ gatea por actor de
    /// conexión (diseño §C) — una conexión de agente ve
    /// [`Error::PolicyDenied`].
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn volumes(
        &self,
        include_pseudo: bool,
    ) -> Result<Vec<norte_proto::methods::Volume>, Error> {
        match self {
            Self::Embedded(_) => {
                let volumes = crate::volumes::enumerate(include_pseudo)
                    .await
                    .map_err(|_| Error::Io { retryable: false })?;
                Ok(volumes.into_iter().map(volume_to_proto).collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.volumes(include_pseudo).await,
        }
    }

    /// La sesión de UI guardada y si ESTA superficie puede escribirla (L2).
    ///
    /// Contra el daemon es `session.get`. En EMBEBIDO no hay socket, así que
    /// el proceso es su propio almacén: el fichero de `<state_dir>` y el mismo
    /// lock que usa el daemon, tomado una vez por proceso. Sin `state_dir` —un
    /// entorno sin `HOME`— se sirve una sesión vacía que nadie escribe, que es
    /// exactamente lo que hoy hace un arranque sin sesión guardada.
    ///
    /// # Errors
    ///
    /// Lo que devuelva el transporte. Un fallo NO es motivo para no arrancar:
    /// el llamante sigue con la pantalla de la configuración.
    pub async fn session_get(&self) -> Result<(norte_proto::methods::Session, bool), Error> {
        match self {
            Self::Embedded(_) => Ok(crate::embedded::session_get().await),
            #[cfg(unix)]
            Self::Remote(r) => r.session_get().await,
        }
    }

    /// Reemplaza la sesión de UI y devuelve la revisión NUEVA (L2).
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] si la revisión venía rancia (re-lee y reintenta),
    /// [`Error::LimitExceeded`] si el cuerpo pasa del tope, y lo que dé el
    /// transporte en lo demás.
    pub async fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, Error> {
        match self {
            Self::Embedded(_) => crate::embedded::session_put(version, revision, body).await,
            #[cfg(unix)]
            Self::Remote(r) => r.session_put(version, revision, body).await,
        }
    }

    /// Búsqueda viva (`fs.search`, live search): devuelve la Task
    /// ([`TaskRef`], cancelable con `TaskRef::cancel`) y el STREAM de lotes de
    /// hits ([`norte_proto::methods::SearchHits`]).
    ///
    /// El humano de un frontend es siempre `User` (sin sandbox): el embebido
    /// lo pasa tal cual a [`Engine::search_as`]; el remoto lo lanza contra el
    /// daemon, que fija el actor server-side por la conexión.
    ///
    /// # Ciclo de vida del canal de hits
    /// - **Embebido:** el walker del engine cierra el `tx` al terminar, así que
    ///   `rx` se cierra solo (drena hasta `None`).
    /// - **Remoto:** la bomba del `RemoteBackend` enruta cada notificación
    ///   `search.hits` por `task_id` a este `rx`. El route se retira —cerrando
    ///   `rx`— cuando la Task llega a terminal (con una gracia que cubre la
    ///   carrera hits-vs-terminal; ver `RemoteBackend::search`). En ambos casos
    ///   el criterio de "búsqueda terminada" es el estado terminal de la
    ///   [`TaskRef`]; el cierre de `rx` es la señal cómoda de que ya no llegan
    ///   más lotes.
    ///
    /// # Errors
    /// Criterios inválidos (cero criterios, o glob y regex del mismo eje) →
    /// [`Error::InvalidPath`] embebido / `INVALID_PARAMS` del daemon; resto,
    /// taxonomía del protocolo; daemon caído = `ProviderUnavailable`.
    pub async fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> Result<(TaskRef, mpsc::Receiver<norte_proto::methods::SearchHits>), Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .search_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.search(params).await.map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Fabrica un archivo (`archive.pack`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] sin fuentes, y lo que devuelva el core. Un
    /// daemon N-1 sin el método contesta `METHOD_NOT_FOUND` →
    /// [`Error::Unsupported`].
    pub async fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> Result<TaskRef, Error> {
        if params.sources.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                // El informe (#250) se recoge por `archive_pack_report`: aquí
                // solo viaja el handle.
                let handle = engine.pack_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.pack(params).await.map(TaskRef::from),
        }
    }

    /// Comprueba un archivo (`archive.test`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] si el nombre no es de un formato conocido, y lo
    /// que devuelva el core.
    pub async fn test_archive(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _) = engine
                    .test_archive_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.test_archive(params).await.map(TaskRef::from),
        }
    }

    /// El informe de un `archive.test` ya lanzado (0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] si ese id no fue un test de esta instancia, si el
    /// anillo ya lo desalojó o si es de otro actor — las tres con la misma
    /// respuesta, que es lo que hace el daemon.
    pub async fn archive_test_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchiveTestResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_test_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.archive_test_report(task_id).await,
        }
    }

    /// El informe de un `archive.pack` (0.58.0, #250): qué guardó ese
    /// empaquetado que no sobrevive a salir de aquí.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un empaquetado o si el anillo ya
    /// lo desalojó; contra un daemon N-1, lo que responda él.
    pub async fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchivePackReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_pack_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.archive_pack_report(task_id).await,
        }
    }

    /// Parte un fichero en trozos (`file.split`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Lo que devuelva el core: trozo demasiado pequeño, demasiados trozos, o
    /// un fallo de I/O.
    pub async fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine.split_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.split_file(params).await.map(TaskRef::from),
        }
    }

    /// Junta los trozos de un split (`file.combine`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Lo que devuelva el core: un hueco en la numeración, un trozo intermedio
    /// corto, o un fallo de I/O.
    pub async fn combine_files(
        &self,
        params: norte_proto::methods::FileCombineParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .combine_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.combine_files(params).await.map(TaskRef::from),
        }
    }

    /// Comparación de dos árboles (`fs.compare`, 0.39.0, ADR 0048): devuelve
    /// la Task ([`TaskRef`], cancelable) y el STREAM de lotes de filas
    /// ([`norte_proto::methods::CompareRowsBatch`]).
    ///
    /// Mismo ciclo de vida del canal que [`Self::search`]: embebido, el walk
    /// cierra el `tx` al terminar; remoto, la bomba enruta cada `compare.rows`
    /// por `task_id` y el route se retira tras el terminal (con la misma
    /// gracia). El criterio de "comparación terminada" es el estado terminal
    /// de la [`TaskRef`]; el cierre del `rx` es la señal cómoda.
    ///
    /// **No muta nada**: sin journal, sin undo (regla dura 4 no aplica).
    ///
    /// # Cuándo están TODAS las filas
    /// El cierre del `rx` NO significa «llegaron todas»: una notificación se
    /// puede perder (el daemon expulsa a un suscriptor que no drena, la bomba
    /// del cliente descarta un lote si su buffer se llena, y una reconexión
    /// suelta los routes cerrando el `rx` de forma indistinguible de un final
    /// limpio). La señal es
    /// [`TaskProgress::entries_done`](norte_proto::TaskProgress::entries_done),
    /// que en una Task [`TaskKind::Compare`](norte_proto::TaskKind::Compare)
    /// cuenta FILAS emitidas: se comparan las recibidas con ese número, y
    /// **DESPUÉS de que el `rx` se cierre**, no al llegar el snapshot terminal
    /// —la bomba de filas y la de progreso son tasks distintas, así que el
    /// terminal puede adelantar al último lote—. Quien vaya a ESCRIBIR a
    /// partir de estas filas (el plan de sincronización de la spec 2) tiene
    /// que hacer esa comprobación.
    ///
    /// # Errors
    /// Dos raíces iguales → [`Error::InvalidPath`]; `follow_symlinks: true` →
    /// [`Error::Unsupported`]. Los dos se comprueban AQUÍ, antes de elegir
    /// brazo, para que el embebido y el remoto contesten lo mismo: el daemon
    /// los rechaza con `-32602` pelado (es su contrato publicado) y
    /// `to_taxonomy` convertiría eso en `Internal`, o sea la misma respuesta
    /// que da un provider que panica. El daemon los sigue comprobando por su
    /// cuenta: aquello es la frontera, esto es la paridad de las dos vías
    /// (mismo criterio que `check_pairs_cap`).
    ///
    /// Un daemon N-1 (0.38.x) sin el método responde `METHOD_NOT_FOUND`, que
    /// se entrega como [`Error::Unsupported`] — «tu daemon es más viejo», no
    /// un fallo real. Resto, taxonomía del protocolo; daemon caído =
    /// `ProviderUnavailable`.
    /// Cierra la sesión remota de `path` (#140). `false` = no había ninguna.
    ///
    /// # Errors
    ///
    /// Lo que devuelva el transporte. Un daemon N-1 sin el método contesta
    /// `METHOD_NOT_FOUND` → [`Error::Unsupported`].
    pub async fn close_connection(&self, path: &norte_proto::VPath) -> Result<bool, Error> {
        match self {
            Self::Embedded(engine) => Ok(engine.close_connection(path)),
            #[cfg(unix)]
            Self::Remote(r) => r.close_connection(path).await,
        }
    }

    /// Cuánto ocupa lo que se le pase, como Task (`fs.dir_size`, 0.49.0,
    /// #139).
    ///
    /// El TOTAL no vuelve por aquí: viaja en el progreso de la Task
    /// (`bytes_done`/`entries_done`), que es lo que el frontend ya escucha para
    /// pintar cualquier otra. El último snapshot es el resultado.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] sin rutas, y lo que devuelva el core. Un daemon
    /// N-1 sin el método contesta `METHOD_NOT_FOUND` → [`Error::Unsupported`],
    /// para que el frontend distinga «tu daemon es más viejo» de un fallo real.
    pub async fn dir_size(
        &self,
        params: norte_proto::methods::FsDirSizeParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_size_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.dir_size(params).await.map(TaskRef::from),
        }
    }

    /// El digest del contenido de un lote de ficheros (`fs.checksum`, 0.59.0,
    /// #311): devuelve la Task, y los digests se recogen con
    /// [`Self::checksum_report`].
    ///
    /// **No muta nada**: leer no es escribir (regla dura 4 no aplica).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con la lista vacía; la taxonomía del protocolo
    /// para el resto.
    pub async fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> Result<TaskRef, Error> {
        if params.paths.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .checksum_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.checksum(params).await.map(TaskRef::from),
        }
    }

    /// Los digests que lleva calculados esa Task (`fs.checksum_report`,
    /// 0.59.0, #311). SNAPSHOT: parcial mientras corre, definitivo cuando la
    /// Task es terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un lote de sumas de esta
    /// instancia o si el anillo ya lo desalojó.
    pub async fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsChecksumReportResult, Error> {
        match self {
            // Embebido no hay actor que comprobar: este `Backend` ES el humano
            // en proceso (mismo criterio que `rename_batch_report`).
            Self::Embedded(engine) => engine
                .checksum_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.checksum_report(task_id).await,
        }
    }

    /// De qué está hecho un directorio, hijo a hijo (`fs.dir_usage`, 0.75.0,
    /// fase 4): devuelve la Task, y el mapa se recoge con
    /// [`Self::dir_usage_report`].
    ///
    /// **No muta nada**: medir no es escribir (regla dura 4 no aplica).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con `depth` en cero o por encima de
    /// [`DIR_USAGE_MAX_DEPTH`](norte_proto::methods::DIR_USAGE_MAX_DEPTH). Los
    /// dos se comprueban AQUÍ, antes de elegir brazo, para que el embebido y el
    /// remoto contesten lo mismo — la lección de `check_pairs_cap`. El daemon
    /// los sigue comprobando por su cuenta: aquello es la frontera, esto es la
    /// paridad de las dos vías.
    ///
    /// **Lo que NO se comprueba aquí es hasta dónde sabe bajar el servidor.**
    /// Que hoy solo se sirva `depth: 1` es una capacidad del daemon, no el
    /// contrato del tipo: cablearla en el cliente haría que un `Backend` 0.75
    /// rechazara por su cuenta un `depth: 2` que un daemon 0.76 sí sirve, sin
    /// llegar a preguntárselo. Eso lo contesta quien lo sabe, y llega como
    /// [`Error::Unsupported`].
    ///
    /// Un daemon N-1 sin el método contesta `METHOD_NOT_FOUND` → también
    /// [`Error::Unsupported`]: quien necesite distinguir «no conoce el método»
    /// de «esa profundidad no se sirve» lo sabe por la `depth` que pidió.
    pub async fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> Result<TaskRef, Error> {
        if params.depth == 0 || params.depth > norte_proto::methods::DIR_USAGE_MAX_DEPTH {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .dir_usage_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.dir_usage(params).await.map(TaskRef::from),
        }
    }

    /// El mapa que lleva medido esa Task (`fs.dir_usage_report`, 0.75.0, fase
    /// 4). SNAPSHOT: parcial mientras corre, definitivo cuando la Task es
    /// terminal.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un mapa de esta instancia o si
    /// el anillo ya lo desalojó.
    pub async fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsDirUsageReportResult, Error> {
        match self {
            // Embebido no hay actor que comprobar: este `Backend` ES el humano
            // en proceso (mismo criterio que `checksum_report`).
            Self::Embedded(engine) => engine
                .dir_usage_report(task_id)
                .map(|(_owner, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.dir_usage_report(task_id).await,
        }
    }

    /// Comparación de dos árboles (`fs.compare`, 0.39.0, ADR 0048): devuelve
    /// la Task ([`TaskRef`], cancelable) y el STREAM de lotes de filas
    /// ([`norte_proto::methods::CompareRowsBatch`]).
    ///
    /// Mismo ciclo de vida del canal que [`Self::search`]: embebido, el walk
    /// cierra el `tx` al terminar; remoto, la bomba enruta cada `compare.rows`
    /// por `task_id` y el route se retira tras el terminal (con la misma
    /// gracia). El criterio de "comparación terminada" es el estado terminal
    /// de la [`TaskRef`]; el cierre del `rx` es la señal cómoda.
    ///
    /// **No muta nada**: sin journal, sin undo (regla dura 4 no aplica).
    ///
    /// # Cuándo están TODAS las filas
    /// El cierre del `rx` NO significa «llegaron todas»: una notificación se
    /// puede perder (el daemon expulsa a un suscriptor que no drena, la bomba
    /// del cliente descarta un lote si su buffer se llena, y una reconexión
    /// suelta los routes cerrando el `rx` de forma indistinguible de un final
    /// limpio). La señal es
    /// [`TaskProgress::entries_done`](norte_proto::TaskProgress::entries_done),
    /// que en una Task [`TaskKind::Compare`](norte_proto::TaskKind::Compare)
    /// cuenta FILAS emitidas: se comparan las recibidas con ese número, y
    /// **DESPUÉS de que el `rx` se cierre**, no al llegar el snapshot terminal
    /// —la bomba de filas y la de progreso son tasks distintas, así que el
    /// terminal puede adelantar al último lote—. Quien vaya a ESCRIBIR a
    /// partir de estas filas (el plan de sincronización de la spec 2) tiene
    /// que hacer esa comprobación.
    ///
    /// # Errors
    /// Dos raíces iguales → [`Error::InvalidPath`]; `follow_symlinks: true` →
    /// [`Error::Unsupported`]. Los dos se comprueban AQUÍ, antes de elegir
    /// brazo, para que el embebido y el remoto contesten lo mismo: el daemon
    /// los rechaza con `-32602` pelado (es su contrato publicado) y
    /// `to_taxonomy` convertiría eso en `Internal`, o sea la misma respuesta
    /// que da un provider que panica. El daemon los sigue comprobando por su
    /// cuenta: aquello es la frontera, esto es la paridad de las dos vías
    /// (mismo criterio que `check_pairs_cap`).
    ///
    /// Un daemon N-1 (0.38.x) sin el método responde `METHOD_NOT_FOUND`, que
    /// se entrega como [`Error::Unsupported`] — «tu daemon es más viejo», no
    /// un fallo real. Resto, taxonomía del protocolo; daemon caído =
    /// `ProviderUnavailable`.
    pub async fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> Result<
        (
            TaskRef,
            mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
        ),
        Error,
    > {
        if params.follow_symlinks {
            return Err(Error::Unsupported);
        }
        if params.left == params.right {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .compare_as(params, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .compare(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Planifica una sincronización de un sentido (`sync.plan`, 0.40.0, ADR
    /// 0049): devuelve la Task ([`TaskRef`], cancelable) y el STREAM de eventos
    /// del plan — lotes de pasos acotados y, al final, el
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) que lo CIERRA y trae
    /// el `plan_hash`.
    ///
    /// **No muta nada**: por debajo es una comparación con una decisión por
    /// fila. Quien escribe es [`Self::sync_apply`], y solo con el hash que llega
    /// aquí.
    ///
    /// # El orden de los eventos es el del canal
    /// `sync.plan_done` llega SIEMPRE después del último lote de pasos, en los
    /// dos brazos: el core mete ambos en un `mpsc` y la bomba del cliente los
    /// enruta al mismo `rx`. Un cierre que llegara antes que un lote sería un
    /// cliente aprobando el hash de un plan que todavía estaba llegando.
    ///
    /// # Cuándo están TODOS los pasos
    /// El `sync.plan_done` es la señal, y su ausencia es la protección: sin él
    /// no hay `plan_hash`, y sin `plan_hash` no se puede aplicar nada. Las TRES
    /// formas de perder un lote fallan por ese lado:
    ///
    /// 1. el daemon expulsa a quien no drena su outbox → su bomba para y sus
    ///    planes retenidos se barren;
    /// 2. una reconexión suelta los routes → el `rx` se cierra;
    /// 3. **el buffer de este proceso se llena** porque quien consume el `rx` va
    ///    más lento que el daemon. Este es el único que un cliente se hace a sí
    ///    mismo, y por eso el enrutado CIERRA el feed en vez de descartar el
    ///    lote (`OnFull::CloseFeed`): descartarlo y entregar el cierre detrás
    ///    —que es lo que hacen `search.hits` y `compare.rows`, donde un lote es
    ///    pintura— dejaría a un humano aprobando un hash que cubre pasos que
    ///    nunca vio.
    ///
    /// Aun así, quien pinte estos pasos debería cuadrarlos:
    /// `SyncPlanDone::counts` suma el plan ENTERO (`create_dir + copy +
    /// overwrite + delete_tree + skip`), así que comparar esa suma con los pasos
    /// recibidos detecta cualquier pérdida futura sin depender de que el
    /// transporte la señale. `TaskProgress::entries_done` cuenta lo mismo desde
    /// el otro lado.
    ///
    /// # El plan queda RETENIDO
    /// Aprobar cuesta un fichero en el directorio de estado del daemon, vivo
    /// durante [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS) y
    /// atado a esta conexión. Hay un tope de planes retenidos por conexión:
    /// pasado, el daemon contesta `OVERLOADED` sin taxonomía —la petición es
    /// válida, el momento no— y este brazo lo entrega como
    /// [`Error::Internal`], igual que el resto de los `-32602`/`-32603` pelados
    /// del daemon. No se puede adelantar aquí porque solo el daemon sabe cuántos
    /// planes retiene esta conexión.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si `compare.follow_symlinks` o
    /// `compare.descend_orphans` vienen puestos (ninguno de los dos es del
    /// llamante: el planificador fija el segundo al lado del origen);
    /// [`Error::InvalidPath`] si `include` pasa de
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE). Los tres se
    /// comprueban AQUÍ, antes de elegir brazo, por lo mismo que en
    /// [`Self::compare`]: el daemon los rechaza con `-32602` pelado y
    /// `to_taxonomy` convertiría eso en `Internal`, o sea la misma respuesta que
    /// da un provider que panica. El engine los sigue comprobando por su cuenta.
    ///
    /// Adelantarlos cambia el ORDEN de dos rechazos, y conviene saberlo: contra
    /// un engine sin spool, esto contesta por el parámetro (`InvalidPath`)
    /// donde el engine habría contestado por la retención (`Unsupported`); y
    /// contra un daemon, un agente sin scope recibe la queja del parámetro desde
    /// su propio proceso en vez del `PolicyDenied` del daemon, que gatea antes
    /// de validar. Ninguno filtra nada —estas tres comprobaciones no miran las
    /// rutas— y es la misma asimetría que [`Self::compare`] ya tiene.
    ///
    /// Además: [`Error::OverlappingRoots`] si las dos raíces se solapan (esa sí
    /// es categoría del wire y viene del engine, sin copia aquí),
    /// [`Error::Unsupported`] si el daemon no tiene spool instalado o es un
    /// daemon N-1 sin el método; resto, taxonomía del protocolo.
    pub async fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
    ) -> Result<(TaskRef, mpsc::Receiver<crate::sync::SyncPlanEvent>), Error> {
        if params.compare.follow_symlinks || params.compare.descend_orphans.is_some() {
            return Err(Error::Unsupported);
        }
        if params
            .include
            .as_ref()
            .is_some_and(|inc| inc.len() > norte_proto::methods::SYNC_MAX_INCLUDE)
        {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                let (handle, rx) = engine
                    .sync_plan_as(params, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                Ok((TaskRef::from_handle(&handle), rx))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .sync_plan(params)
                .await
                .map(|(t, rx)| (TaskRef::from(t), rx)),
        }
    }

    /// Ejecuta el plan APROBADO que `plan_hash` nombra (`sync.apply`, 0.40.0,
    /// ADR 0049) como UNA Task y UN lote deshacible del journal.
    ///
    /// **El hash es el único parámetro**, y esa es la garantía: no hay forma de
    /// pedir que se ejecute algo distinto de lo que [`Self::sync_plan`] enseñó.
    /// Las dos raíces, el modo y los criterios salen del plan retenido.
    ///
    /// **El plan se gasta**: aplicarlo lo consume, pase lo que pase. Un segundo
    /// `sync_apply` del mismo hash es [`Error::PlanStale`], que es verdad.
    ///
    /// Qué pasó de verdad se pide con [`Self::sync_report`]: un paso que falla
    /// es una FILA del informe y no el final de la Task, así que el estado
    /// terminal no cuenta ni la mitad.
    ///
    /// # Errors
    /// [`Error::PlanStale`] si el hash no nombra un plan vivo de este proceso
    /// (no existe, caducó, está manipulado o ya se aplicó);
    /// [`Error::PlanNotExecutable`] si el plan traía bloqueos;
    /// [`Error::PolicyDenied`] del gate, que corre sobre las raíces leídas del
    /// plan y AHORA, no cuando se planificó; [`Error::Unsupported`] sin spool o
    /// sin journal (aplicar sin journal sería enterrar sin dejar vuelta atrás,
    /// regla dura 4), o contra un daemon N-1; resto, taxonomía del protocolo.
    pub async fn sync_apply(
        &self,
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .sync_apply_as(plan_hash, EMBEDDED_CONN_ID, crate::journal::Actor::User)
                    .await?;
                // El informe queda en el anillo del engine, que es de donde lo
                // lee `sync_report`: los dos brazos se piden igual.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.sync_apply(plan_hash).await.map(TaskRef::from),
        }
    }

    /// El informe de una aplicación ya lanzada (`sync.report`, 0.40.0): cuántos
    /// pasos se ejecutaron, cuántos fallaron y por qué —con la ruta de cada
    /// uno— y bajo qué lote del journal quedó lo que sí se aplicó.
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes.
    /// Míralo también cuando diga `cancelled`: lo aplicado hasta el corte se
    /// queda, journalizado — media sincronización es un estado real.
    ///
    /// # Quién ve qué
    /// El daemon sirve el informe a quien podría ver la Task: su dueño, o
    /// cualquier conexión HUMANA. Un `Backend::Remote` abierto con
    /// [`remote::RemoteBackend::connect`] es humano, y por él se ven también
    /// los informes de las aplicaciones de los AGENTES — deliberado, y la
    /// simetría del undo: un humano que gobierna el daemon puede leer lo que un
    /// agente hizo. Uno abierto con
    /// [`remote::RemoteBackend::connect_as_agent`] NO lo es (lo estrenó el
    /// puente MCP): ve lo suyo y nada más, y para él «no es tuya» y «no existe»
    /// son la misma respuesta.
    ///
    /// Nótese la asimetría, que no es un descuido: la AUTORIZACIÓN (el plan) va
    /// por conexión, y su informe por ACTOR. Dos conexiones humanas son el mismo
    /// `Actor::User`, así que una lee el informe de la otra aunque no pudiera
    /// aplicar su plan.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese `task_id` nunca fue una aplicación de este
    /// proceso, si el anillo ya lo desalojó, o si el que pregunta no podía verla.
    /// [`Error::Unsupported`] contra un daemon N-1; resto, taxonomía del
    /// protocolo.
    pub async fn sync_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::SyncReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .sync_report(task_id)
                .map(|(_owner, r)| r)
                // Embebido no hay actor que comprobar: este `Backend` ES el
                // humano en proceso (mismo criterio que `rename_batch_report`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.sync_report(task_id).await,
        }
    }

    /// Registra la host key de `host:port` tras la confirmación EXPLÍCITA
    /// del usuario (flujo TOFU: un `Error::HostKeyUnknown` trajo el
    /// fingerprint, el frontend lo mostró y el usuario aceptó — ADR 0015 D).
    /// El core re-verifica el fingerprint contra la clave real del host
    /// antes de registrar (anti-TOCTOU).
    ///
    /// # Errors
    /// Taxonomía del protocolo ([`Error::HostKeyMismatch`] si el host ya no
    /// presenta esa clave).
    /// `algo` viaja informativo en el wire (la identidad que se confirma es
    /// el fingerprint); pásalo tal cual llegó en el `HostKeyUnknown`.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        algo: &str,
        fingerprint: &str,
    ) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => {
                let _ = algo; // el engine confirma por fingerprint
                engine.trust_host_key(host, port, fingerprint).await
            }
            #[cfg(unix)]
            Self::Remote(r) => r.trust_host_key(host, port, algo, fingerprint).await,
        }
    }

    /// Entrega al core el secreto de `conn` que el humano acaba de teclear,
    /// tras un [`Error::SecretNeeded`] (#325). Vive en memoria, en el proceso
    /// que tiene el engine, y hasta que ese proceso pare: no se persiste en
    /// ningún sitio.
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::Unsupported`] si no hay conector.
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => engine.provide_secret(conn, secret).await,
            #[cfg(unix)]
            Self::Remote(r) => r.provide_secret(conn, secret).await,
        }
    }

    /// Canal de tasks FORÁNEAS (encoladas por otros frontends de la misma
    /// sesión). `None` en embebido o si ya se tomó. Solo el dueño original de
    /// la conexión debe llamarlo; un clon (scripting) no.
    pub fn take_foreign_tasks(&mut self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => {
                // El SDK entrega tasks REMOTAS; un frontend habla de
                // `TaskRef` y no quiere saber de dónde vino. El puente es
                // una task de reenvío porque un canal no se puede mapear en
                // el sitio: muere cuando muere el canal de origen, así que
                // no sobrevive a la conexión que lo alimentaba.
                let mut origen = r.take_foreign_tasks()?;
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(async move {
                    while let Some(t) = origen.recv().await {
                        if tx.send(TaskRef::from(t)).is_err() {
                            break;
                        }
                    }
                });
                Some(rx)
            }
        }
    }

    /// Canal de eventos de conexión (aviso de reconexión). `None` en
    /// embebido o si ya se tomó. Solo el dueño original de la conexión debe
    /// llamarlo; un clon (scripting) no.
    pub fn take_conn_events(&mut self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_conn_events(),
        }
    }

    /// Canal de aprobaciones de policy pendientes (M3-3b T5): cada
    /// `policy.approval_required` del daemon (y el resync por
    /// `policy.pending` al (re)conectar) llega aquí para que el frontend
    /// pregunte al humano. `None` en embebido (sin agentes que aprobar por
    /// esta vía) o si ya se tomó. Solo el dueño original de la conexión debe
    /// llamarlo; un clon (scripting) no.
    pub fn take_approvals(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_approvals(),
        }
    }

    /// Receptor de avisos `connection.degraded` (#44). En `Remote` viene del
    /// pump del daemon; en `Embedded` INSTALA un observer en el engine que
    /// empuja a un canal — así AMBOS modos surfacean la degradación de forma
    /// uniforme (rust MAJOR M1 + security m1: antes el embebido era silencioso).
    /// One-shot por su naturaleza (instala/toma una vez); en `Embedded` el
    /// aviso es SÍNCRONO (el observer dispara dentro del `provider_for` del
    /// comando en curso), así que un drenado posterior lo ve sin carrera.
    pub fn take_degraded(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                // Encadenando: la ranura es de UNO y este `take_*` no puede
                // dejar mudo al del otro hecho (#322).
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelConnectionObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_degraded(),
        }
    }

    /// Receptor de fallos `connection.failed` (#322): POR QUÉ una conexión NO
    /// se abrió. Gemelo de [`Backend::take_degraded`] y con el mismo trato en
    /// los dos brazos — en `Remote` viene del pump del daemon, en `Embedded`
    /// instala un observer.
    ///
    /// Existe en `Embedded` y no solo en `Remote` porque el diagnóstico se
    /// perdía en los DOS: en el daemon se quedaba en su log, y en el embebido
    /// salía por el stderr del propio proceso — que en la TUI se lo come la
    /// pantalla alternativa. Un fallo que se diagnostica o no según el
    /// transporte es la peor forma de que dependa.
    ///
    /// One-shot en `Remote`, donde el receptor se lo lleva el primer dueño. En
    /// `Embedded` NO lo es —igual que [`Backend::take_degraded`]—: cada
    /// llamada encadena otro observer y devuelve otro receptor, y el que nadie
    /// drene es un canal sin techo que solo crece. Llámalo UNA vez, en el
    /// arranque.
    ///
    /// Por `&self` y no `&mut self` como [`Backend::take_degraded`]: ninguna
    /// de las dos ramas lo necesitaba, y `norte connect` —el comando que se
    /// teclea justo para diagnosticar esto— tiene el backend por referencia
    /// compartida. Pedir `&mut` habría dejado fuera al único sitio donde el
    /// humano está preguntando explícitamente «¿por qué no entra?».
    pub fn take_failed(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine.chain_connection_observer(|previo| {
                    Arc::new(ChannelFailureObserver { tx, previo })
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_failed(),
        }
    }

    /// Receptor de avisos `plugin.notice` (0.69.0, ADR 0100): la frase de un
    /// plugin `hook` sobre una mutación ya registrada, o que los hooks de un
    /// plugin se apagaron tras tres fallos. En `Remote` viene del pump del
    /// daemon; en `Embedded` ARRANCA el despachador de hooks sobre el journal
    /// de este engine y le da un canal — así ambos modos corren los mismos
    /// hooks y surfacean lo mismo. One-shot, como [`Backend::take_failed`]:
    /// el engine tiene UN hueco para el despachador y el segundo que lo pida
    /// recibe `None`, en vez de arrancar otro que pisara al primero.
    ///
    /// `None` también en `Embedded` si el engine no lleva journal (sin filas
    /// no hay hooks) o si el runtime WASM no se pudo crear: sin runtime no
    /// corre ningún plugin, y tampoco un hook (fail-closed, con traza). El
    /// despachador muere solo cuando el receptor devuelto se suelta.
    ///
    /// # Panics
    /// Fuera de un runtime de tokio: arranca tasks.
    pub fn take_plugin_notices(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>> {
        match self {
            Self::Embedded(engine) => {
                if !engine.has_journal() || !engine.claim_hooks_slot() {
                    return None;
                }
                let runtime = match norte_plugin_host::PluginRuntime::new() {
                    Ok(r) => Arc::new(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "hooks: sin runtime de plugins, no corren");
                        return None;
                    }
                };
                let (tx, rx) = mpsc::unbounded_channel();
                // Enchufar el journal es `async` (el perezoso guarda el
                // extremo bajo su lock) y leer `policy.toml` es I/O (regla 2):
                // las dos cosas en una task. Una mutación que se adelante
                // queda sin hook, y es el arranque: no hay ninguna. Sin token
                // de cancelación propio: la vida del despachador embebido es
                // la del receptor (`is_closed`), y el proceso que lo hospeda
                // termina con él.
                let engine = Arc::clone(engine);
                tokio::spawn(async move {
                    // Las reglas del humano valen también aquí (ADR 0101): el
                    // engine embebido no lleva gate, así que el despachador
                    // las mira para el actor `plugin`. Un fichero ilegible se
                    // dice y equivale a ninguno.
                    let policy = tokio::task::spawn_blocking(crate::PolicyConfig::load)
                        .await
                        .ok()
                        .and_then(|r| match r {
                            Ok(p) => Some(Arc::new(p)),
                            Err(e) => {
                                tracing::warn!(error = %e, "hooks: policy.toml ilegible, sin reglas");
                                None
                            }
                        });
                    let (sender, _task) = crate::hooks::spawn_dispatcher(
                        crate::connect::config_dir(),
                        runtime,
                        Arc::new(ChannelHookSink { tx }),
                        tokio_util::sync::CancellationToken::new(),
                        Some(crate::hooks::SidecarWriter {
                            engine: Arc::downgrade(&engine),
                            scopes: None,
                            policy,
                        }),
                    );
                    engine.enable_hooks(sender).await;
                });
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_plugin_notices(),
        }
    }

    /// Receptor del aviso «esta sesión NO queda registrada en el journal»
    /// (#167/#177). Solo `Embedded` puede quedarse sin journal —el daemon se
    /// niega a arrancar sin él—, así que en `Remote` es `None`.
    ///
    /// Como [`Backend::take_degraded`], en `Embedded` INSTALA el sink en el
    /// engine en vez de tomar un canal ya hecho. Llamarlo en el arranque, antes
    /// de la primera mutación; y si una mutación se adelanta igual, el aviso no
    /// se pierde (el `LazyJournal` lo retiene hasta que hay sink).
    ///
    /// Llegan PÉRDIDAS Y RECUPERACIONES (#179): la ventana de propiedad se
    /// puede reabrir, así que un frontend que solo escuche
    /// [`JournalStatus::Lost`](crate::embedded::JournalStatus::Lost) acaba
    /// pintando «esta sesión no se registra» sobre una que sí.
    ///
    /// `None` también si el engine embebido no lleva journal perezoso —uno
    /// construido con `Engine::new()`, que no journaliza NADA y nunca va a
    /// avisar de ello—: devolver un canal ahí sería decirle al frontend que
    /// está cubierto por un aviso que no puede llegar.
    ///
    /// UNA sola vez, como sus hermanos: un segundo sink deja mudo al primer
    /// receptor (ver [`crate::embedded::LazyJournal::set_warning_sink`]).
    pub fn take_journal_warnings(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<crate::embedded::JournalStatus>> {
        match self {
            Self::Embedded(engine) => {
                let (tx, rx) = mpsc::unbounded_channel();
                engine
                    .set_journal_warning_sink(Arc::new(ChannelJournalSink { tx }))
                    .then_some(rx)
            }
            #[cfg(unix)]
            Self::Remote(_) => None,
        }
    }

    /// Resuelve una aprobación pendiente (`policy.decide`, M3-3b T5).
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` en embebido (las aprobaciones
    /// solo llegan por el canal del daemon, así que aquí no hay qué decidir).
    pub async fn policy_decide(&self, approval_id: u64, approve: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.policy_decide(approval_id, approve).await,
        }
    }

    /// Deshace la sesión completa de un agente como Task (`policy.undo_session`,
    /// M3-4). Solo tiene sentido contra el daemon (dueño del journal).
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` en embebido. Desde #167 el
    /// embebido sí puede tener journal, pero deshacer LA SESIÓN DE UN AGENTE es
    /// del daemon: los agentes se gobiernan ahí y es ahí donde existen sus
    /// sesiones.
    pub async fn undo_session(&self, session: &str) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.undo_session(session).await.map(TaskRef::from),
        }
    }

    /// Una página de la línea de tiempo del journal (`journal.list`, 0.76.0,
    /// fase 7): de la más nueva hacia atrás, `before_seq` exclusivo.
    ///
    /// Funciona EMBEBIDO, al contrario que [`Backend::undo_session`]: lo que
    /// aquél no puede contestar sin daemon son las sesiones de agente, y esto
    /// es el journal de esta máquina, que el engine embebido tiene delante.
    ///
    /// # Errors
    /// Taxonomía del protocolo. `Unsupported` sin journal; contra un daemon,
    /// `PolicyDenied` si la conexión no es humana.
    pub async fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<norte_proto::methods::JournalListResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let limit = limit.clamp(1, norte_proto::methods::JOURNAL_LIST_MAX_PAGE);
                let entries = engine.journal_page(before_seq, limit, actor_kind).await?;
                // El MISMO cálculo de cursor que el daemon, porque es la
                // misma función: dos copias de esta expresión es lo que la
                // revisión de protocolo señaló, y ninguna podía ponerse roja
                // por su cuenta.
                Ok(crate::journal::page_to_wire(&entries, limit))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.journal_list(before_seq, limit, actor_kind).await,
        }
    }

    /// Deshace lo que el humano hizo DESPUÉS de `seq` (`journal.undo_after`,
    /// 0.76.0, fase 7). La entrada señalada se queda.
    ///
    /// También embebido, por lo mismo que [`Backend::journal_list`]: deshacer
    /// lo propio no necesita daemon. El informe se lee como el de cualquier
    /// undo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` sin journal.
    pub async fn undo_after(&self, seq: i64) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine.undo_after(seq).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.undo_after(seq).await.map(TaskRef::from),
        }
    }

    /// Informe de una Task de undo (`policy.undo_report`, #71): qué se
    /// deshizo, qué se saltó y por qué, dónde se bloqueó el LIFO. Snapshot;
    /// definitivo cuando la Task es terminal.
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` en embebido, por la misma razón
    /// que [`Backend::undo_session`]: la sesión que se deshace es de un agente,
    /// y los agentes viven en el daemon.
    pub async fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::PolicyUndoReportResult, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.undo_report(task_id).await,
        }
    }

    /// Catálogo de plugins descubiertos + su estado aprobado/activado (M4-P3).
    /// Embebido: descubre de [`crate::connect::config_dir`] bajo demanda (el
    /// estado vive en `plugins-state.toml`, no en memoria — no hay que retener
    /// un registro entre llamadas); remoto: `plugin.list` contra el daemon.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn plugins_list(&self) -> Result<norte_proto::methods::PluginListResult, Error> {
        match self {
            Self::Embedded(_) => {
                // Registro EFÍMERO por-llamada (I/O sync → spawn_blocking,
                // regla 2). La verdad vive en el fichero de estado.
                let dir = crate::connect::config_dir();
                tokio::task::spawn_blocking(move || {
                    crate::PluginRegistry::discover(&dir).map(|r| r.list())
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_list().await,
        }
    }

    /// Aprueba (o revoca) las capabilities de un plugin (M4-P3). Embebido:
    /// discover + `set_approval` + persiste, todo en `spawn_blocking`.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// El gate "SOLO un humano aprueba" vive en la capa WIRE (el daemon, que ata
    /// la conexión a un [`crate::journal::Actor`]). Este `Backend` embebido es la
    /// API in-proceso del frontend HUMANO y NO recibe `Actor`: aprobar por aquí
    /// es, por construcción, un acto del humano. Si algún día se cablea un bridge
    /// de agente a un `Backend` embebido, habría que replicar el gate AQUÍ (no
    /// existe hoy y no debe introducirse sin ese gate).
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id es desconocido; taxonomía del protocolo en
    /// lo demás.
    pub async fn plugins_set_approval(
        &self,
        id: &str,
        approved: bool,
        expected_digest: Option<&str>,
    ) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let esperado = expected_digest.map(ToOwned::to_owned);
                let applied = tokio::task::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    // La comprobación de #282 importa MÁS aquí que en el
                    // daemon: éste descubre el catálogo una vez al arrancar,
                    // así que su ventana está cerrada por accidente. Este
                    // `discover` corre en CADA llamada, o sea que el
                    // `plugin.toml` que se lee ahora puede no ser el que el
                    // humano leyó hace un momento.
                    // Un id desconocido NO es un ancla rancia: los dos dan
                    // `None` aquí, y confundirlos daría «el manifiesto
                    // cambió» a quien nombró un plugin que no existe.
                    if approved
                        && let Some(actual) = reg.manifest_digest(&id)
                        && let Some(esperado) = &esperado
                        && &actual != esperado
                    {
                        return Ok(None);
                    }
                    reg.set_approval(&id, approved).map(Some)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })?;
                match applied {
                    Some(true) => Ok(()),
                    Some(false) => Err(Error::NotFound),
                    // El manifiesto cambió bajo los pies: no se concede, y se
                    // dice con la variante que significa exactamente eso —la
                    // misma que `session.put` con una revisión rancia—. No
                    // `Exists`, que se lee como «el destino ya está» y aquí no
                    // significa nada.
                    None => Err(Error::Conflict {
                        conflict: norte_proto::ConflictKind::StaleRevision,
                    }),
                }
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_set_approval(id, approved, expected_digest).await,
        }
    }

    /// Activa/desactiva un plugin ya aprobado (M4-P3). Semántica idéntica a
    /// [`Self::plugins_set_approval`].
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate "solo humano" vive en la
    /// capa wire (daemon con `Actor`); este Backend embebido es la API del
    /// frontend humano y no recibe `Actor`. Un futuro bridge de agente a un
    /// Backend embebido tendría que replicar el gate aquí.
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id es desconocido; taxonomía del protocolo en
    /// lo demás.
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let applied = tokio::task::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    reg.set_enabled(&id, enabled)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
                .map_err(|_| Error::Io { retryable: false })?;
                if applied {
                    Ok(())
                } else {
                    Err(Error::NotFound)
                }
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_set_enabled(id, enabled).await,
        }
    }

    /// Desinstala un plugin (0.71.0, ADR 0104): borra su directorio y deja su
    /// estado apagado y sin aprobar. Devuelve si tenía consentimiento, que
    /// es lo que acaba de dejar de existir.
    ///
    /// Embebido: [`crate::plugins::uninstall`] en `spawn_blocking`, lo mismo
    /// que hace la CLI. Remoto: `plugin.uninstall` contra el daemon, que
    /// además lo olvida en su registro en memoria.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate «solo humano» vive
    /// en la capa wire.
    ///
    /// # Errors
    /// [`Error::NotFound`] si el id no es un id o no está instalado;
    /// [`Error::Io`] si el borrado o el estado fallan.
    pub async fn plugins_uninstall(&self, id: &str) -> Result<bool, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let informe =
                    tokio::task::spawn_blocking(move || crate::plugins::uninstall(&dir, &id))
                        .await
                        .map_err(|_| Error::Internal { panic: true })?
                        .map_err(|e| {
                            use crate::plugins::UninstallError as U;
                            match e {
                                U::InvalidId | U::NotInstalled(_) => Error::NotFound,
                                U::Io(_) => Error::Io { retryable: false },
                            }
                        })?;
                Ok(informe.was_approved)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugins_uninstall(id).await.map(|r| r.was_approved),
        }
    }

    /// Ejecuta un comando de un plugin YA aprobado y activado (M4-P4) y devuelve
    /// su salida. Embebido: registro EFÍMERO por-llamada + un `PluginRuntime`
    /// nuevo, TODO en `spawn_blocking` (la instanciación compila WASM: pesada y
    /// síncrona, regla 2). Remoto: `plugin.run_command` contra el daemon.
    ///
    /// El coste de crear el runtime por-llamada se acepta igual que el registro
    /// efímero de [`Self::plugins_list`]: el modo embebido es un frontend humano
    /// puntual, no un servidor de plugins de alta frecuencia (ese es el daemon,
    /// que sí reutiliza un runtime compartido).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`. Un fallo del runtime del plugin se
    /// entrega REDACTADO (`Internal`), sin filtrar rutas ni detalles internos.
    pub async fn plugin_run_command(
        &self,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let command = command.to_owned();
                let arg = arg.to_owned();
                tokio::task::spawn_blocking(move || -> Result<String, Error> {
                    let reg = crate::PluginRegistry::discover(&dir)
                        .map_err(|_| Error::Io { retryable: false })?;
                    let runtime = norte_plugin_host::PluginRuntime::new()
                        .map_err(|_| Error::Internal { panic: false })?;
                    reg.run_command(&runtime, &id, &command, &arg)
                        .map_err(|e| run_error_to_taxonomy(&e))
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_run_command(id, command, arg).await,
        }
    }

    /// Previsualiza `path` con el PRIMER previewer consentido cuyo mimetype
    /// (adivinado por extensión) case, o devuelve `preview: None` si ninguno
    /// aplica (el frontend cae a la vista cruda). Un archivo ilegible es un error
    /// honesto, no un preview vacío.
    ///
    /// Embebido: registro EFÍMERO por-llamada + resolución del previewer +
    /// `PluginRuntime` nuevo, con la lectura de bytes acotada intercalada. La
    /// resolución (discover, IO) y la ejecución (instancia WASM, síncrona) van en
    /// `spawn_blocking` (regla 2); la lectura de bytes usa el engine async entre
    /// medias. El coste del registro/runtime efímero se acepta igual que en
    /// [`Self::plugin_run_command`]: el modo embebido es un frontend humano
    /// puntual, no un servidor de plugins de alta frecuencia (ese es el daemon,
    /// que reutiliza un registro y un runtime compartidos). Remoto:
    /// `plugin.preview` contra el daemon.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`. Un fallo del runtime del plugin se
    /// entrega REDACTADO (`Internal`), sin filtrar rutas ni detalles internos.
    pub async fn plugin_preview(
        &self,
        path: &VPath,
    ) -> Result<norte_proto::methods::PluginPreviewResult, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolver el previewer (discover = IO) en spawn_blocking.
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = tokio::task::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_previewer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(norte_proto::methods::PluginPreviewResult { preview: None });
                };

                // 2) Leer los bytes ACOTADOS vía el engine (async). Un archivo
                // ilegible es un error honesto que se propaga.
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::PREVIEW_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
                        break;
                    }
                }
                let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));

                // 2.5) §6.2 (#29): el previewer recibe TEXTO ya decodificado
                // por la detección del core — jamás bytes crudos sobre los que
                // asumir UTF-8 (lógica testeada en `plugins::decode_for_preview`).
                // `lossy` (#101) viaja al frontend para el aviso «via …».
                let (content, lossy) = crate::plugins::decode_for_preview(bytes);

                // 3) Instanciar + renderizar (síncrono, WASM) en spawn_blocking.
                let mime_owned = mime.to_owned();
                let output = tokio::task::spawn_blocking(move || -> Result<String, Error> {
                    let runtime = norte_plugin_host::PluginRuntime::new()
                        .map_err(|_| Error::Internal { panic: false })?;
                    let mut inst = runtime
                        .instantiate(&wasm, caps)
                        .map_err(|_| Error::Internal { panic: false })?;
                    // P2 Task 4a: entrega `[config]` YA resuelto al previewer,
                    // mismo criterio que `plugin_run_command` (vía
                    // `PluginRegistry::run_command`, Task 3).
                    inst.set_settings(settings);
                    inst.render_preview(&mime_owned, &content)
                        .map_err(|_| Error::Internal { panic: false })
                })
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(norte_proto::methods::PluginPreviewResult {
                    preview: Some(norte_proto::methods::PluginPreview {
                        plugin_id: id,
                        plugin_name: name,
                        output,
                        lossy,
                    }),
                })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_preview(path).await,
        }
    }

    /// Previsualiza `path` CON ESTILO (G3a, ADR 0037): el gemelo de
    /// [`Self::plugin_preview`] que devuelve líneas de spans en vez de una
    /// cadena plana. Mismos pasos 1 (resolver) y 2 (leer bytes) que
    /// `plugin_preview` — sus errores se PROPAGAN igual (un archivo
    /// ilegible sigue siendo un fallo honesto). Difiere en el paso 3
    /// (ejecución): invoca `render-styled` en vez de `render`, y —a
    /// diferencia de `plugin_preview`— CUALQUIER fallo del runtime EN ESE
    /// PASO (trap, error de lógica del guest, o los topes de la tabla ADR
    /// 0037 excedidos vía `RuntimeError::StyledPreviewTooLarge`) degrada a
    /// `Ok(None)` en vez de propagarse: el preview con estilo es un
    /// ENRIQUECIMIENTO sobre el plano, nunca debe bloquear el archivo — el
    /// caller cae a [`Self::plugin_preview`] (plano), que a su vez cae a la
    /// vista cruda si tampoco aplica ninguno.
    ///
    /// Límite de responsabilidad sobre `SpanWire::role` (ADR 0037 decisión
    /// 3, enmienda): viaja SIN VALIDAR desde aquí. `norte-core` es headless
    /// y NO depende de `norte-theme` (dueño del conjunto cerrado `Role`);
    /// añadir esa dependencia estructural solo para validar un `String` que
    /// de todos modos ya llega acotado en tamaño (topes del wire, aplicados
    /// en `render_styled_preview`) no se justifica (regla 8) cuando el
    /// FRONTEND —que sí conoce el tema y es quien PINTA— es el único que
    /// puede resolver un `role` a un color, y por tanto el único lugar
    /// donde un nombre desconocido tiene un significado operable (`None`,
    /// sin color) en vez de un dato inerte. Un `role` no reconocido nunca
    /// debe usarse por un frontend como clave de lookup sin pasar antes por
    /// `norte_theme::Role::from_kebab_requestable` (ver `norte-frontend::
    /// viewer::Viewer::with_plugin_preview_styled`, que es donde eso ocurre).
    /// `_requestable` y no `from_kebab` a secas: desde la spec 2026-09-11 el
    /// vocabulario que un plugin puede nombrar es el de SIGNIFICADO, no el
    /// cromo ni el estado de la ventana.
    ///
    /// # Errors
    /// Igual que [`Self::plugin_preview`] para resolución/lectura; jamás por
    /// un fallo de EJECUCIÓN del guest (ver arriba: degrada a `Ok(None)`).
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
        columns: Option<u32>,
    ) -> Result<Option<norte_proto::methods::PluginPreviewStyled>, Error> {
        match self {
            Self::Embedded(engine) => {
                // 1) Resolver el previewer (discover = IO) en spawn_blocking.
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = tokio::task::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_previewer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(None);
                };

                // 2) Leer los bytes ACOTADOS vía el engine (async), igual que
                // `plugin_preview`. Un archivo ilegible se propaga honesto.
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::PREVIEW_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
                        break;
                    }
                }
                let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));
                let (content, lossy) = crate::plugins::decode_for_preview(bytes);

                // 3) Instanciar + `render-styled` (síncrono, WASM) en
                // spawn_blocking. Cualquier `RuntimeError` aquí (trap, guest,
                // o tope excedido) degrada a `Ok(None)` — ver rustdoc.
                let mime_owned = mime.to_owned();
                let outcome = tokio::task::spawn_blocking(move || {
                    let runtime = norte_plugin_host::PluginRuntime::new()?;
                    let mut inst = runtime.instantiate(&wasm, caps)?;
                    inst.set_settings(settings);
                    inst.render_styled_preview(
                        &mime_owned,
                        &content,
                        crate::plugins::clamp_preview_columns(columns),
                    )
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?;

                let lines = match outcome {
                    Ok(lines) => lines,
                    Err(e) => {
                        tracing::debug!(
                            plugin = %id,
                            error = %e,
                            "render-styled falló: cae a plugin_preview (plano)"
                        );
                        return Ok(None);
                    }
                };
                Ok(Some(norte_proto::methods::PluginPreviewStyled {
                    plugin_id: id,
                    plugin_name: name,
                    lines: crate::plugins::to_wire_lines(lines),
                    lossy,
                }))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_preview_styled(path, columns).await,
        }
    }

    /// `plugin.thumbnail` (ADR 0107): la miniatura de `path` por el primer
    /// plugin de miniaturas consentido cuyo mimetype casa, o `None` si no
    /// hay ninguno o el que hay no supo — cosmético y fail-soft, como la
    /// preview con estilo. Embebido: resolver y leer aquí, correr el guest
    /// en `spawn_blocking`. Remoto: el daemon hace lo mismo.
    ///
    /// # Errors
    /// Los de la lectura del fichero; jamás por un fallo del guest.
    pub async fn plugin_thumbnail(
        &self,
        path: &VPath,
        max_edge: u32,
    ) -> Result<Option<norte_proto::methods::PluginThumbnail>, Error> {
        match self {
            Self::Embedded(engine) => {
                let dir = crate::connect::config_dir();
                let mime = crate::plugins::guess_mimetype(path);
                let resolved = tokio::task::spawn_blocking(
                    move || -> Result<Option<crate::plugins::ResolvedPreviewer>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg.resolve_thumbnailer(mime))
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                let Some((id, name, wasm, caps, settings)) = resolved else {
                    return Ok(None);
                };
                let range = ByteRange {
                    offset: 0,
                    len: Some(crate::plugins::THUMBNAIL_MAX_BYTES),
                };
                let mut stream = engine.read(path, Some(range)).await?;
                let mut bytes: Vec<u8> = Vec::new();
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk?);
                    if bytes.len() as u64 >= crate::plugins::THUMBNAIL_MAX_BYTES {
                        break;
                    }
                }
                let cap =
                    usize::try_from(crate::plugins::THUMBNAIL_MAX_BYTES).unwrap_or(usize::MAX);
                bytes.truncate(cap.min(bytes.len()));
                let outcome = tokio::task::spawn_blocking(move || {
                    let runtime = norte_plugin_host::PluginRuntime::new()?;
                    let mut inst = runtime.instantiate_thumbnail(&wasm, caps)?;
                    inst.set_settings(settings);
                    inst.render_thumbnail(mime, &bytes, max_edge)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?;
                let thumb = match outcome {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!(plugin = %id, error = %e, "thumbnail falló: sin miniatura");
                        return Ok(None);
                    }
                };
                Ok(Some(norte_proto::methods::PluginThumbnail {
                    plugin_id: id,
                    plugin_name: name,
                    mimetype: thumb.mimetype.to_owned(),
                    bytes: thumb.bytes,
                    width: thumb.width,
                    height: thumb.height,
                }))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_thumbnail(path, max_edge).await,
        }
    }

    /// Decora `paths` (G3b, ADR 0037 decisión 2): la SUPERPOSICIÓN de TODOS
    /// los plugins `decorator` APROBADOS y ACTIVADOS ([`crate::PluginRegistry::
    /// resolve_decorators`], plural — a diferencia del previewer que elige
    /// el primero), batched — UNA llamada por plugin sobre la página
    /// ENTERA, nunca entrada por entrada. `paths` son las rutas VISIBLES de
    /// la página actual, en el orden en que se listan; cada
    /// `PluginDecorations::decorations` es POSICIONAL 1:1 con `paths`. Los
    /// `entries` que cruzan al guest son BASENAMES
    /// (`crate::plugins::paths_to_basenames`) — un decorator ve el
    /// nombre de cada entrada, no dónde vive en el árbol (privacidad, ver
    /// el rustdoc de esa función).
    ///
    /// Fail-closed POR PLUGIN, nunca por lote entero: si un plugin no
    /// instancia, su runtime trapea, o devuelve una longitud que no casa
    /// `paths.len()` (violación de contrato,
    /// `crate::plugins::decorations_to_wire_checked`), ESE plugin se
    /// OMITE del resultado (con aviso en el log local) — el resto de
    /// plugins y el resto de la página se pintan igual, mismo criterio de
    /// degradación por-plugin que `plugin_preview`/`plugin_preview_styled`
    /// (un enriquecimiento nunca debe bloquear el listado). `paths` vacío
    /// devuelve `Ok(vec![])` sin resolver el catálogo (nada que decorar).
    ///
    /// # Errors
    /// Solo por fallos de INFRAESTRUCTURA del propio `Backend` (I/O al
    /// descubrir el catálogo, panic real de `spawn_blocking`) — jamás por
    /// un plugin individual que falla (degrada, ver arriba).
    pub async fn plugin_decorate(
        &self,
        paths: &[VPath],
        kinds: &[norte_proto::EntryKind],
    ) -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let entries = crate::plugins::paths_to_entries(paths, kinds);
                let expected_len = paths.len();
                let plugins = tokio::task::spawn_blocking(
                    move || -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let runtime = norte_plugin_host::PluginRuntime::new()
                            .map_err(|_| Error::Internal { panic: false })?;
                        let mut out = Vec::new();
                        for ((id, _name, wasm, caps, settings), slot) in reg.resolve_decorators() {
                            let Ok(mut inst) = runtime.instantiate_decorator(&wasm, caps) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: fallo al instanciar, se omite del lote"
                                );
                                continue;
                            };
                            inst.set_settings(settings);
                            let Ok(raw) = inst.decorate(&entries) else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: fallo al ejecutar decorate, se omite del lote"
                                );
                                continue;
                            };
                            let Some(decorations) =
                                crate::plugins::decorations_to_wire_checked(raw, expected_len)
                            else {
                                tracing::warn!(
                                    plugin = %id,
                                    "decorator: longitud no casa el contrato posicional, se omite del lote"
                                );
                                continue;
                            };
                            out.push(norte_proto::methods::PluginDecorations {
                                plugin_id: id,
                                slot: crate::plugins::slot_to_wire(slot),
                                decorations,
                            });
                        }
                        Ok(out)
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(plugins)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_decorate(paths, kinds).await,
        }
    }

    /// El plan de renombrado que PROPONE un plugin `renamer` (C3, ADR 0095)
    /// para `names` en `dir`: el mismo resultado que [`Self::ai_rename_plan`],
    /// que es lo que hace que los frontends lo revisen y ejecuten por el
    /// camino que ya tienen.
    ///
    /// Si el guest rehúsa no es un error: `entries` vacío y `refused` con
    /// su frase, ya enmascarada y acotada (#332).
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese plugin/renamer no está consentido;
    /// [`Error::Io`] si el guest no corre. En `Remote`, lo que conteste el
    /// daemon.
    pub async fn plugin_rename_plan(
        &self,
        plugin_id: &str,
        renamer_id: &str,
        dir: &VPath,
        names: &[String],
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(_) => {
                let cfg = crate::connect::config_dir();
                let (plugin_id, renamer_id) = (plugin_id.to_owned(), renamer_id.to_owned());
                let (dir, names) = (dir.clone(), names.to_vec());
                tokio::task::spawn_blocking(move || {
                    let reg = crate::PluginRegistry::discover(&cfg)
                        .map_err(|_| Error::Io { retryable: false })?;
                    let Some(resolved) = reg.resolve_renamer(&plugin_id, &renamer_id) else {
                        return Err(Error::NotFound);
                    };
                    let (runtime, _) = columnas_de_proceso()?;
                    // Embebido: quien pide es la persona, el mismo caso que
                    // `Actor::User` en el daemon — sube hasta el marcador.
                    match crate::plugins::run_rename_plan(
                        runtime,
                        resolved,
                        &renamer_id,
                        Some(&dir),
                        true,
                        &names,
                    ) {
                        crate::plugins::RenamePlanOutcome::Plan(entries) => {
                            Ok(norte_proto::methods::AiRenamePlanResult {
                                entries,
                                refused: None,
                            })
                        }
                        // Rehusar no es un error (#332): es un plan vacío con
                        // motivo, y el motivo es texto de un tercero que se
                        // enmascara y acota antes de enseñarse.
                        crate::plugins::RenamePlanOutcome::Refused(frase) => {
                            tracing::info!(plugin = %plugin_id, motivo = %frase, "renamer: rehusó");
                            Ok(norte_proto::methods::AiRenamePlanResult {
                                entries: Vec::new(),
                                refused: Some(crate::plugins::guest_reason(&frase)),
                            })
                        }
                        crate::plugins::RenamePlanOutcome::Failed => {
                            Err(Error::Io { retryable: false })
                        }
                    }
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => {
                r.plugin_rename_plan(plugin_id, renamer_id, dir, names)
                    .await
            }
        }
    }

    /// Valores de la columna `column_id` para `paths` (G3b, ADR 0037
    /// decisión 2): a diferencia de [`Self::plugin_decorate`] (superposición
    /// de TODOS los decorators), como mucho UN plugin `columns` aporta
    /// `column_id` ([`crate::PluginRegistry::resolve_columns`],
    /// primero-que-casa). Mismo contrato de entradas (basenames) que
    /// `plugin_decorate`. Fail-closed: si el ÚNICO plugin que aporta la
    /// columna no instancia, trapea, o rompe el contrato posicional, el
    /// resultado es un vector de `None` del tamaño de `paths` (celda vacía
    /// para toda la página) en vez de un error — una columna sin datos es
    /// un enriquecimiento perdido, no un fallo del listado. `paths` vacío
    /// devuelve `Ok(vec![])`.
    ///
    /// # Errors
    /// Solo por fallos de INFRAESTRUCTURA (igual que [`Self::plugin_decorate`]).
    pub async fn plugin_column_values(
        &self,
        plugin_id: &str,
        column_id: &str,
        paths: &[VPath],
    ) -> Result<Vec<Option<String>>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let entries = crate::plugins::paths_to_basenames(paths);
                let expected_len = paths.len();
                // El directorio padre de la página: la ubicación que el guest
                // puede leer si se le aprobó (ADR 0057).
                let location = paths.first().and_then(norte_proto::VPath::parent);
                let column_id_owned = column_id.to_owned();
                let plugin_id_owned = plugin_id.to_owned();
                let values =
                    tokio::task::spawn_blocking(move || -> Result<Vec<Option<String>>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        // ESE plugin o ninguno (#120): dos plugins consentidos
                        // que declaren el mismo id bare no pueden servirse el
                        // uno por el otro.
                        let Some(resolved) =
                            reg.resolve_columns_of(Some(&plugin_id_owned), &column_id_owned)
                        else {
                            return Ok(vec![None; expected_len]);
                        };
                        // El motor y el pool son de PROCESO, como en el daemon
                        // (#224). Construir un `PluginRuntime` por llamada no
                        // solo compila el motor otra vez: arranca y para un
                        // hilo ticker de época por página pintada.
                        let (runtime, pool) = columnas_de_proceso()?;
                        // MISMA función que el daemon: la capacidad de
                        // ubicación no puede significar una cosa aquí y otra
                        // allí.
                        Ok(pool.column_values(
                            runtime,
                            resolved,
                            &column_id_owned,
                            location.as_ref(),
                            // Embebido: quien mira es la persona que abrió el
                            // panel, el mismo caso que `Actor::User`.
                            true,
                            &entries,
                            expected_len,
                        ))
                    })
                    .await
                    .map_err(|_| Error::Internal { panic: true })??;
                Ok(values)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_column_values(plugin_id, column_id, paths).await,
        }
    }

    /// El marco que un plugin pinta para su panel (0.74.0, fase 3).
    ///
    /// Embebido TAMBIÉN, y no solo con daemon: el mismo `ntc` no puede enseñar
    /// un panel de git cuando hay daemon y una caja vacía cuando no lo hay.
    /// Lo que corre es la misma función que el daemon (`render_panel_blocking`),
    /// con el mismo mint y la misma vida de la sesión de ubicación.
    ///
    /// `climb` es `true` aquí porque embebido quien mira es la persona que
    /// abrió el panel — el caso de `Actor::User` en el daemon—, así que subir a
    /// buscar la raíz del proyecto (`.git`) es lo correcto.
    ///
    /// Fail-soft: sin plugin que lo pinte, o si el guest falla, no hay marco.
    ///
    /// Embebido cuesta un DESCUBRIMIENTO del catálogo por llamada —leer el
    /// directorio de plugins y parsear cada `plugin.toml`—, donde el daemon
    /// tiene su registro en memoria. Se paga por cambio de contexto (otro
    /// directorio, otra fila, otro tamaño), no por frame, porque una firma que
    /// ya se pidió o que ya volvió vacía no se repite.
    ///
    /// # Errors
    /// Taxonomía del protocolo. Con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`; embebido, `Io{retryable:false}`
    /// si el catálogo no se puede leer e `Internal` si el motor de wasm no
    /// arranca o la tarea bloqueante se cae.
    pub async fn plugin_panel_render(
        &self,
        params: norte_proto::methods::PluginPanelRenderParams,
    ) -> Result<Option<norte_proto::methods::PanelFrame>, Error> {
        match self {
            Self::Embedded(_) => {
                let dir_cfg = crate::connect::config_dir();
                tokio::task::spawn_blocking(
                    move || -> Result<Option<norte_proto::methods::PanelFrame>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir_cfg)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let Some(resuelto) = reg.resolve_panel(&params.plugin_id, &params.kind)
                        else {
                            return Ok(None);
                        };
                        // El motor es de PROCESO, como en el daemon (#224):
                        // construir uno por repintado compila el motor otra vez
                        // y arranca un hilo de época por frame.
                        let (runtime, _) = columnas_de_proceso()?;
                        let contexto = norte_plugin_host::panel_iface::PanelContext {
                            cols: params.cols,
                            rows: params.rows,
                            lang: params.lang.clone(),
                            cursor_name: params.cursor_name.clone(),
                        };
                        let evento = crate::plugins::panel_event_to_host(&params.event);
                        let state = params.state.clone().unwrap_or_default();
                        match crate::plugins::render_panel_blocking(
                            runtime,
                            resuelto,
                            &crate::plugins::PanelCall {
                                dir: &params.dir,
                                climb: true,
                                kind: &params.kind,
                                contexto: &contexto,
                                state: &state,
                                evento: &evento,
                            },
                        ) {
                            Ok((id, marco)) => {
                                Ok(Some(crate::plugins::panel_frame_to_wire(id, marco)))
                            }
                            // Un guest que falla deja el panel sin marco, no la
                            // pantalla con un error.
                            Err(_) => Ok(None),
                        }
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_panel_render(params).await,
        }
    }

    /// Esquema `[config]` + valores efectivos de `id` (0.28.0, G3c, ADR
    /// 0037): cierra la deuda P2 (`settings` era host-only). Id desconocido
    /// devuelve `keys: []` (mismo criterio indulgente que
    /// [`Self::plugins_list`] con un catálogo vacío). Embebido: registro
    /// EFÍMERO por-llamada en `spawn_blocking` (regla 2), igual criterio de
    /// coste que [`Self::plugins_list`].
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn plugin_get_config(
        &self,
        id: &str,
    ) -> Result<norte_proto::methods::PluginGetConfigResult, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let keys = tokio::task::spawn_blocking(
                    move || -> Result<Vec<norte_proto::methods::PluginConfigKeyWire>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        Ok(reg
                            .config_keys(&id)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(key, spec, value)| {
                                crate::plugins::config_key_to_wire(key, &spec, value)
                            })
                            .collect())
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(norte_proto::methods::PluginGetConfigResult { keys })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_get_config(id).await,
        }
    }

    /// La página de ayuda de un plugin (H3e, 0.34.0), ya acotada por el host a
    /// [`norte_proto::methods::PLUGIN_HELP_MAX_BYTES`]. Embebido: registro
    /// EFÍMERO por llamada en `spawn_blocking` (regla 2), mismo criterio de
    /// coste que [`Self::plugin_get_config`].
    ///
    /// El markdown que devuelve NO está enmascarado: lleva verbatim los peligros
    /// de terminal que el plugin escribiera (ESC, controles C0, anulaciones
    /// bidi). Se parsea con `norte_help::parse_untrusted`, que enmascara al
    /// construir el modelo; nunca se pinta ni se loguea en crudo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    ///
    /// Un `id` que no está en el catálogo NO da el mismo error por los dos
    /// caminos, y conviene saberlo: embebido es [`Error::NotFound`], mientras
    /// que el daemon responde `INVALID_PARAMS` sin taxonomía en `data`, que
    /// `to_taxonomy` entrega como `Internal{panic:false}`. Un frontend que
    /// quiera distinguir "plugin desconocido" de "el daemon tuvo un problema"
    /// no puede hacerlo por el wire; lo correcto en ambos casos es tratarlo
    /// como "no hay página" y seguir pintando.
    pub async fn plugin_help(
        &self,
        id: &str,
    ) -> Result<norte_proto::methods::PluginHelpResult, Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                tokio::task::spawn_blocking(
                    move || -> Result<norte_proto::methods::PluginHelpResult, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        reg.help_of(&id).ok_or(Error::NotFound)
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_help(id).await,
        }
    }

    /// Persiste UN valor de `[config]` para `id`, validado contra el
    /// esquema del manifiesto (0.28.0, G3c, ADR 0037). Embebido: registro
    /// EFÍMERO por-llamada + `PluginRegistry::set_config` (valida, persiste,
    /// re-resuelve), TODO en `spawn_blocking`.
    ///
    /// # Invariante de seguridad (defensa en profundidad)
    /// Igual que [`Self::plugins_set_approval`]: el gate "solo humano" vive
    /// en la capa wire (daemon con `Actor`); este `Backend` embebido es la
    /// API del frontend humano y no recibe `Actor`.
    ///
    /// # Errors
    /// [`Error::NotFound`] si `id` es desconocido; taxonomía del protocolo
    /// en lo demás (un valor inválido o clave desconocida llega como un
    /// error genérico — el caller debe validar client-side ANTES de llamar,
    /// que es lo que hacen TUI/GUI).
    pub async fn plugin_set_config(&self, id: &str, key: &str, value: &str) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let key = key.to_owned();
                let value = value.to_owned();
                tokio::task::spawn_blocking(move || -> Result<(), Error> {
                    let mut reg = crate::PluginRegistry::discover(&dir)
                        .map_err(|_| Error::Io { retryable: false })?;
                    reg.set_config(&id, &key, &value)
                        .map_err(|e| config_set_error_to_taxonomy(&e))
                })
                .await
                .map_err(|_| Error::Internal { panic: true })?
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_set_config(id, key, value).await,
        }
    }

    /// El registro del DAEMON desde `cursor` (`log.tail`, 0.65.0, #328,
    /// ADR 0092).
    ///
    /// `cursor: None` es «dame lo que haya» y NO es lo mismo que cero: contra
    /// un anillo que ya dio la vuelta, un cero reportaría un `lost` falso en
    /// la primera vuelta. Después se encadena el `next` que llegó.
    ///
    /// # Errors
    /// Taxonomía del protocolo. En `Embedded` es siempre
    /// [`Error::Unsupported`], y eso NO es una carencia: el anillo del core
    /// embebido está en ESTE proceso, así que ya es el que el frontend lee —
    /// no hay una segunda fuente que ofrecer. En `Remote`, un daemon de la
    /// misma versión compilado sin la feature `logging` contesta lo mismo, y
    /// esa respuesta no puede cambiar mientras ese daemon viva.
    ///
    /// Las dos respuestas se escriben igual y **no significan lo mismo**, así
    /// que quien pinta un panel decide con [`Self::is_remote`] antes de
    /// preguntar: sin daemon no hay nada de lo que informar, y una frase sobre
    /// «este daemon» donde no hay ninguno es peor que el silencio.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_tail(None, 10)),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> Result<norte_proto::methods::LogTailResult, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.log_tail(cursor, max).await,
        }
    }

    /// Sube el nivel que el anillo del daemon captura (`log.level`, 0.65.0,
    /// #328) y devuelve el que de verdad quedó puesto.
    ///
    /// Su anillo es SUYO: es global a todos sus clientes y nunca baja, así
    /// que lo pedido y lo puesto no tienen por qué coincidir — de ahí que
    /// esto devuelva un nivel en vez de un `()`.
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::Unsupported`] en `Embedded` y contra
    /// un daemon sin registro que servir (ver [`Self::log_tail`]).
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_level("debug")),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_level(&self, level: &str) -> Result<String, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.log_level(level).await,
        }
    }
}

/// Mapea un fallo de [`crate::PluginRegistry::set_config`] a la taxonomía
/// del protocolo (modo EMBEBIDO): un id desconocido es honestamente
/// `NotFound` (mismo criterio que [`Backend::plugins_set_approval`]); el
/// resto (clave desconocida / valor inválido / I/O) se REDACTA a `Internal`
/// — el caller (TUI/GUI) valida client-side ANTES de llamar, así que este
/// camino solo se pisa por un valor que se coló esa barrera (backstop, no
/// UX primaria).
fn config_set_error_to_taxonomy(e: &crate::plugins::PluginConfigSetError) -> Error {
    use crate::plugins::PluginConfigSetError as E;
    match e {
        E::Unknown(_) => Error::NotFound,
        E::UnknownKey(_) | E::Invalid(_) | E::Io(_) => Error::Internal { panic: false },
    }
}

/// Mapea el fallo de ejecución de un plugin a la taxonomía del protocolo (modo
/// EMBEBIDO). Un fallo de runtime se REDACTA a `Internal` (jamás el `Display`
/// crudo, que puede llevar la ruta del `.wasm` o detalles de wasmtime): la
/// misma política que el daemon aplica en el wire (security-reviewer M4-P4).
fn run_error_to_taxonomy(e: &crate::plugins::PluginRunError) -> Error {
    use crate::plugins::PluginRunError as E;
    match e {
        // Id inexistente / sin binario / no consentido: el nodo pedido no está
        // disponible para ejecución. `NotFound` es la categoría honesta y NO
        // revela rutas (los mensajes de estos variantes llevan el id, no el path).
        E::Unknown(_) | E::NoBinary(_) | E::NotApproved(_) | E::Disabled(_) => Error::NotFound,
        // Un kind sin `command`: ese plugin no tiene la capacidad que se le
        // pide, que es lo que `Unsupported` dice (el daemon lo devuelve como
        // `INVALID_PARAMS`, con el mismo sentido).
        E::NotRunnable(_) => Error::Unsupported,
        // Fallo del runtime: redactado, sin filtrar el detalle al frontend.
        E::Runtime(_) => Error::Internal { panic: false },
    }
}

/// El motor WASM y el pool de instancias de columnas del PROCESO, para el
/// backend embebido (#224).
///
/// El daemon los cuelga de su `Shared`; aquí no hay dónde, porque el backend
/// embebido redescubre el registro en cada llamada y no tiene estado propio.
/// Un `static` es lo que hace que las dos mitades del mismo binario —`ntc`
/// embebido y `ntc --daemon`— paguen lo mismo por la misma página.
///
/// Se construye una vez y no se suelta: `PluginRuntime` posee el hilo ticker
/// que hace avanzar la época del motor, o sea el reloj con el que un guest en
/// bucle trapa (regla dura 3). Un runtime por llamada arrancaba y paraba ese
/// hilo por página pintada.
///
/// # Errors
/// Si el motor wasmtime no se puede configurar en esta plataforma. El fallo se
/// recuerda: reintentarlo por cada página sería pagar el fallo N veces para
/// llegar al mismo sitio.
/// El motor de wasm y el pool de columnas de ESTE proceso.
///
/// Lo usan las columnas y, desde la fase 3, los paneles de plugin, que se
/// quedan solo con el motor: construir un `PluginRuntime` por llamada compila
/// el motor otra vez y arranca un hilo de época por pintado. El nombre dice
/// «columnas» por quién llegó primero.
fn columnas_de_proceso() -> Result<
    (
        &'static norte_plugin_host::PluginRuntime,
        &'static crate::plugins::ColumnPool,
    ),
    Error,
> {
    struct Columnas {
        runtime: norte_plugin_host::PluginRuntime,
        pool: crate::plugins::ColumnPool,
    }
    static COLUMNAS: std::sync::OnceLock<Option<Columnas>> = std::sync::OnceLock::new();
    let cel = COLUMNAS.get_or_init(|| {
        norte_plugin_host::PluginRuntime::new()
            .ok()
            .map(|runtime| Columnas {
                runtime,
                pool: crate::plugins::ColumnPool::default(),
            })
    });
    match cel {
        Some(c) => Ok((&c.runtime, &c.pool)),
        None => Err(Error::Internal { panic: false }),
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
