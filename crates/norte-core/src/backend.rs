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
    /// real llega por `task.progress`, contrato del método).
    #[cfg(unix)]
    Remote {
        /// Conexión con el daemon.
        backend: remote::RemoteBackend,
        /// Task a cancelar.
        id: TaskId,
    },
}

impl TaskCanceller {
    /// Dispara la cancelación cooperativa.
    pub fn cancel(&self) {
        match self {
            Self::Embedded(token) => token.cancel(),
            #[cfg(unix)]
            Self::Remote { backend, id } => backend.spawn_cancel(*id),
        }
    }
}

/// Evento de conexión del backend remoto (para la barra de mensajes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// La conexión con el daemon se perdió; reconectando en background.
    Lost,
    /// Reconectado (y resincronizado vía `task.list`).
    Restored,
}

/// Observer de avisos de conexión (#44) que los reenvía por un canal: la vía
/// del `Backend::Embedded` para que una CLI/TUI EN PROCESO surface la
/// degradación igual que en modo daemon (donde el observer difunde por wire).
/// Mapea el `ConnectionWarning` del core al `ConnectionDegraded` del wire.
struct ChannelConnectionObserver {
    tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionDegraded>,
}

impl crate::connect::ConnectionObserver for ChannelConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let _ = self.tx.send(norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        });
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

/// El core detrás de una única superficie (regla 7).
pub enum Backend {
    /// Core in-process: arranque instantáneo, sin daemon.
    Embedded(Arc<Engine>),
    /// Contra el daemon UDS (ADR 0011).
    #[cfg(unix)]
    Remote(remote::RemoteBackend),
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

    /// Abre ya el journal (si hace falta) y dice si ESTA sesión queda
    /// registrada — la pregunta que un frontend hace justo antes de mutar y
    /// necesita CONTESTAR al humano antes del sí, no después (`norte ai
    /// rename`, y desde esta tarea `norte sync`).
    ///
    /// - Embebido: delega en [`Engine::ensure_journal`], que toma el lock
    ///   perezoso AQUÍ (no en la primera mutación) y lo conserva hasta que
    ///   alguien lo suelte (`LazyJournal::release`, que hoy no llama nadie por
    ///   su cuenta — #179).
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

    /// Copia como task.
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
                &engine.copy_with(from, to, opts).await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => {
                r.transfer(norte_proto::methods::FS_COPY, from, to, opts)
                    .await
            }
        }
    }

    /// Move como task.
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
                &engine.move_with(from, to, opts).await?,
            )),
            #[cfg(unix)]
            Self::Remote(r) => {
                r.transfer(norte_proto::methods::FS_MOVE, from, to, opts)
                    .await
            }
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
            Self::Remote(r) => r.delete(path, mode).await,
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
            Self::Remote(r) => r.mkdir(path).await,
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
            Self::Remote(r) => r.rename_batch(dir, pairs, plan_hash).await,
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
            Self::Remote(r) => r.index_build(root).await,
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
            Self::Remote(r) => r.index_embed(root).await,
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
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan =
                    tokio::time::timeout(AI_CALL_TIMEOUT, engine.ai_rename_plan(dir, instruction))
                        .await
                        .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(crate::ai::ai_plan_to_proto(plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_rename_plan(dir, instruction).await,
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
            Self::Remote(r) => r.search(params).await,
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
            Self::Remote(r) => r.compare(params).await,
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
            Self::Remote(r) => r.sync_plan(params).await,
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
            Self::Remote(r) => r.sync_apply(plan_hash).await,
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

    /// Canal de tasks FORÁNEAS (encoladas por otros frontends de la misma
    /// sesión). `None` en embebido o si ya se tomó. Solo el dueño original de
    /// la conexión debe llamarlo; un clon (scripting) no.
    pub fn take_foreign_tasks(&mut self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_foreign_tasks(),
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
                engine.set_connection_observer(Arc::new(ChannelConnectionObserver { tx }));
                Some(rx)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.take_degraded(),
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
            Self::Remote(r) => r.undo_session(session).await,
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
    pub async fn plugins_set_approval(&self, id: &str, approved: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let id = id.to_owned();
                let applied = tokio::task::spawn_blocking(move || {
                    let mut reg = crate::PluginRegistry::discover(&dir)?;
                    reg.set_approval(&id, approved)
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
            Self::Remote(r) => r.plugins_set_approval(id, approved).await,
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
    /// `norte_theme::Role::from_kebab` (ver `norte-frontend::viewer::
    /// Viewer::with_plugin_preview_styled`, que es donde eso ocurre).
    ///
    /// # Errors
    /// Igual que [`Self::plugin_preview`] para resolución/lectura; jamás por
    /// un fallo de EJECUCIÓN del guest (ver arriba: degrada a `Ok(None)`).
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
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
                    inst.render_styled_preview(&mime_owned, &content)
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
            Self::Remote(r) => r.plugin_preview_styled(path).await,
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
    ) -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Self::Embedded(_) => {
                let dir = crate::connect::config_dir();
                let entries = crate::plugins::paths_to_basenames(paths);
                let expected_len = paths.len();
                let plugins = tokio::task::spawn_blocking(
                    move || -> Result<Vec<norte_proto::methods::PluginDecorations>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let runtime = norte_plugin_host::PluginRuntime::new()
                            .map_err(|_| Error::Internal { panic: false })?;
                        let mut out = Vec::new();
                        for (id, _name, wasm, caps, settings) in reg.resolve_decorators() {
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
            Self::Remote(r) => r.plugin_decorate(paths).await,
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
                let column_id_owned = column_id.to_owned();
                let plugin_id_owned = plugin_id.to_owned();
                let values = tokio::task::spawn_blocking(
                    move || -> Result<Vec<Option<String>>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        // ESE plugin o ninguno (#120): dos plugins consentidos
                        // que declaren el mismo id bare no pueden servirse el
                        // uno por el otro.
                        let Some((id, _name, wasm, caps, settings)) =
                            reg.resolve_columns_of(Some(&plugin_id_owned), &column_id_owned)
                        else {
                            return Ok(vec![None; expected_len]);
                        };
                        let runtime = norte_plugin_host::PluginRuntime::new()
                            .map_err(|_| Error::Internal { panic: false })?;
                        let Ok(mut inst) = runtime.instantiate_columns(&wasm, caps) else {
                            tracing::warn!(plugin = %id, "columns: fallo al instanciar, celdas vacías");
                            return Ok(vec![None; expected_len]);
                        };
                        inst.set_settings(settings);
                        let Ok(raw) = inst.column_values(&column_id_owned, &entries) else {
                            tracing::warn!(plugin = %id, "columns: fallo al ejecutar, celdas vacías");
                            return Ok(vec![None; expected_len]);
                        };
                        Ok(
                            crate::plugins::column_values_checked(raw, expected_len)
                                .unwrap_or_else(|| {
                                    tracing::warn!(
                                        plugin = %id,
                                        "columns: longitud no casa el contrato posicional, celdas vacías"
                                    );
                                    vec![None; expected_len]
                                }),
                        )
                    },
                )
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                Ok(values)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.plugin_column_values(plugin_id, column_id, paths).await,
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
        // Fallo del runtime: redactado, sin filtrar el detalle al frontend.
        E::Runtime(_) => Error::Internal { panic: false },
    }
}

/// El backend remoto (solo unix, como el daemon — ADR 0011).
#[cfg(unix)]
pub mod remote {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, Weak};
    use std::time::Duration;

    use base64::Engine as _;
    use futures::StreamExt as _;
    use norte_proto::methods::{
        self, ClientInfo, CompareRowsBatch, ConnectionDegraded, FsCapabilitiesParams,
        FsCapabilitiesResult, FsCopyParams, FsDeleteParams, FsListParams, FsListResult,
        FsMoveParams, FsReadParams, FsReadResult, FsSearchParams, FsStatParams, FsStatResult,
        FsTaskResult, PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult,
        PolicyPendingResult, SearchHits, TaskCancelParams, TaskCancelResult, TaskListParams,
        TaskListResult,
    };
    use norte_proto::{
        ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskKind, TaskProgress,
        TaskState, VPath,
    };
    use norte_vfs::EntryStream;
    use tokio::sync::{mpsc, watch};

    use super::{AI_CALL_TIMEOUT, ConnEvent, TaskCanceller, TaskRef};
    use crate::daemon::{Client, ClientError};
    use crate::engine::TransferOptions;

    /// Backoff de reconexión (se recorre y se queda en el último).
    const RECONNECT_BACKOFF_MS: &[u64] = &[250, 500, 1000, 2000, 5000];
    /// Tope de una llamada RPC: un daemon vivo-pero-atascado (stat sobre un
    /// NFS muerto) jamás congela el frontend (M4 del rust-reviewer).
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);
    /// Entradas por página al listar un dir remoto (ADR 0017): acota el frame
    /// de respuesta y el tiempo de UNA llamada.
    const LIST_PAGE: u32 = 1000;
    /// Buffer del canal de lotes de un feed remoto (`search.hits` de una
    /// `fs.search`, `compare.rows` de una `fs.compare`). Absorbe el burst de
    /// lotes que ya coalesció el daemon (`SEARCH_HITS_MAX_BATCH` /
    /// `COMPARE_ROWS_MAX_BATCH` por lote) mientras el frontend drena; holgado
    /// para que `try_send` no descarte por backpressure en el caso normal.
    const BATCH_BUF: usize = 64;
    /// Tope de lotes retenidos SIN route (carrera de arranque: un lote puede
    /// adelantar al registro del route). Acota la memoria ante un daemon que
    /// emita lotes de `task_id`s que este proceso jamás registró.
    ///
    /// Es POR FEED, no global: cada `BatchRoutes` lleva su propio `pending`, y
    /// hay dos, así que lo retenido en el peor caso es el doble de este número.
    const BATCH_PENDING_CAP: usize = 64;
    /// Gracia tras el terminal de un feed antes de retirar su route. En el
    /// daemon la bomba de lotes y la de progreso son tasks INDEPENDIENTES que
    /// escriben al mismo sink: un `search.hits`/`compare.rows` puede llegar
    /// tras el `task.progress` terminal. La gracia deja que esos lotes
    /// rezagados aún se enruten; pasada, el sender se suelta y `rx` se cierra
    /// (parida con el embebido). Retirar en seco al terminal perdería el lote
    /// rezagado.
    const BATCH_ROUTE_GRACE: Duration = Duration::from_millis(500);

    /// Enrutado de los lotes de UN feed vivo (los `search.hits` de una
    /// `fs.search`, las `compare.rows` de una `fs.compare`) por `task_id`.
    /// Todo detrás de UN Mutex para que registrar el route (drenar lo
    /// pendiente + insertar) sea ATÓMICO frente a la bomba — sin ventana en la
    /// que un lote se pierda entre el drenaje y el insert.
    ///
    /// Genérico en el lote y no duplicado por feed: los dos tienen el mismo
    /// ciclo de vida (route, carrera de arranque, gracia tras el terminal) y
    /// dos copias del mismo razonamiento sutil se desincronizan.
    struct BatchRoutes<T> {
        /// `task_id` → sender del `rx` que devolvió el método que lo lanzó.
        routes: HashMap<u64, mpsc::Sender<T>>,
        /// Lotes llegados ANTES de que su route se registrara (carrera de
        /// arranque): el registro los drena en orden. Acotado por
        /// [`BATCH_PENDING_CAP`] lotes en total.
        pending: HashMap<u64, Vec<T>>,
        /// `task_id`s cuyo `task.progress` TERMINAL ya se vio. El terminal puede
        /// ADELANTAR al registro del route (el frame sale del daemon antes que
        /// la respuesta de `fs.search`, y en el cliente la bomba y `search`
        /// corren en paralelo): sin esto, la retirada del route se perdería y el
        /// `rx` no se cerraría jamás. Espejo del anillo `finished` de `own_task`.
        /// Acotado; una entrada se limpia al retirar su route.
        terminated: std::collections::HashSet<u64>,
    }

    // `derive(Default)` exigiría `T: Default`, que un lote no tiene por qué
    // ser: el mapa vacío no depende del tipo del lote.
    impl<T> Default for BatchRoutes<T> {
        fn default() -> Self {
            Self {
                routes: HashMap::new(),
                pending: HashMap::new(),
                terminated: std::collections::HashSet::new(),
            }
        }
    }

    impl<T> BatchRoutes<T> {
        /// Lotes pendientes retenidos en total (todos los `task_id`).
        fn pending_len(&self) -> usize {
            self.pending.values().map(Vec::len).sum()
        }

        /// Recuerda que `id` llegó a terminal (acotado: un backstop borra todo
        /// si crece sin límite, jamás memoria ilimitada ante un daemon hostil).
        /// Devuelve `true` solo en la INSERCIÓN nueva: el caller agenda la
        /// retirada una única vez, sin duplicarla ante terminales repetidos
        /// (bomba + resync de `task.list`). El set SOLO acumula terminales del
        /// KIND de este feed (ver `route`), así que el tope 256 es realista
        /// (harían falta 256 feeds concurrentes para que el `clear` borre una
        /// marca viva).
        fn mark_terminated(&mut self, id: u64) -> bool {
            if self.terminated.len() >= 256 {
                self.terminated.clear();
            }
            self.terminated.insert(id)
        }

        /// Suelta todo (reconexión): los `rx` en vuelo se cierran.
        fn clear(&mut self) {
            self.routes.clear();
            self.pending.clear();
            self.terminated.clear();
        }
    }

    /// Estado del `try_unfold` que pagina un listado remoto: el buffer de la
    /// página actual y el cursor de la siguiente.
    struct PageState {
        backend: RemoteBackend,
        dir: VPath,
        buffer: std::collections::VecDeque<Entry>,
        cursor: Option<String>,
        done: bool,
        /// Ids de attrs del ARRANQUE (#108 bloque 2): el daemon ignora los de
        /// una continuación (el stream retenido nació con ellos), pero se
        /// re-mandan igual — si el cursor expira y el cliente reinicia, el
        /// nuevo listado pide lo mismo.
        attrs: Vec<String>,
    }

    /// Un paso del stream paginado: sirve del buffer o pide la página siguiente.
    async fn page_step(mut st: PageState) -> Result<Option<(Entry, PageState)>, Error> {
        loop {
            if let Some(e) = st.buffer.pop_front() {
                return Ok(Some((e, st)));
            }
            if st.done {
                return Ok(None);
            }
            let cursor = st.cursor.take();
            let page: FsListResult = st
                .backend
                .call_timed(
                    methods::FS_LIST,
                    &FsListParams {
                        path: st.dir.clone(),
                        limit: Some(LIST_PAGE),
                        cursor,
                        attrs: st.attrs.clone(),
                    },
                )
                .await?;
            // Un server roto que devuelve página vacía CON next_cursor haría
            // un bucle infinito: se corta (precedente del guard de fs.read).
            if page.entries.is_empty() && page.next_cursor.is_some() {
                return Err(Error::Internal { panic: false });
            }
            st.buffer.extend(page.entries);
            match page.next_cursor {
                Some(c) => st.cursor = Some(c),
                None => st.done = true,
            }
        }
    }

    /// Mapea el error del cliente RPC a la taxonomía (el contrato de los
    /// frontends es SIEMPRE la taxonomía, spec §17.7).
    /// Guard drop-based de un submit remoto (#74): si cae ARMADO con un id
    /// capturado, notifica `rpc.cancel {id}` (sync, best-effort — `notify`
    /// solo encola el frame; canal muerto = no-op). `armed=false` tras
    /// recibir la respuesta.
    struct CancelOnAbandon {
        client: Arc<Client>,
        /// Id de la request en vuelo; `0` = aún sin asignar (el contador del
        /// [`Client`] arranca en 1 — jamás emite 0).
        id: Arc<std::sync::atomic::AtomicU64>,
        armed: bool,
    }

    impl Drop for CancelOnAbandon {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            let id = self.id.load(std::sync::atomic::Ordering::SeqCst);
            if id == 0 {
                return;
            }
            let _ = self.client.notify(
                methods::RPC_CANCEL,
                &methods::RpcCancelParams {
                    id: norte_proto::wire::RequestId::Num(id),
                },
            );
        }
    }

    fn to_taxonomy(e: ClientError) -> Error {
        match e {
            // La taxonomía viaja en data (ADR 0011): se entrega tal cual.
            ClientError::Rpc(rpc) => rpc.data.unwrap_or(Error::Internal { panic: false }),
            ClientError::Io(_) | ClientError::ConnectionClosed | ClientError::SpawnTimeout => {
                Error::ProviderUnavailable { retryable: true }
            }
            ClientError::BadResult(_) => Error::Internal { panic: false },
            ClientError::ForeignDaemon => Error::PermissionDenied,
        }
    }

    struct Inner {
        socket: PathBuf,
        /// Comando de autoarranque (`argv[0]` + args); `None` = solo connect.
        spawn_cmd: Option<Vec<std::ffi::OsString>>,
        client_info: ClientInfo,
        /// Sesión de agente declarada en el handshake, o `None` para una
        /// conexión humana. Vive AQUÍ y no solo en el `connect` porque
        /// `establish` corre también en cada RECONEXIÓN: un backend de agente
        /// que reconectara sin ella volvería como `Actor::User` — el actor se
        /// blanquearía solo, en silencio, al primer corte del daemon.
        agent_session: Option<String>,
        /// El daemon dijo que venía un RELEVO (`daemon.going_away` con
        /// `reconnect: true`), así que la próxima reconexión puede arrancarlo.
        ///
        /// Existe porque un relevo y una parada son la MISMA conexión cerrada
        /// vistas desde aquí, y la respuesta correcta es la contraria: sin esta
        /// señal, reconectar siempre resucitaría un daemon que el usuario acaba
        /// de parar, y no reconectar nunca dejaría la sesión muerta tras una
        /// actualización.
        ///
        /// Se GASTA en el intento que lo usa, salga bien o mal. Un frontend al
        /// que le dijeron que venía un relevo, que no lo encontró y se rindió,
        /// no puede llevarse esa licencia a la conexión de la semana que viene:
        /// para entonces «el daemon no está» vuelve a significar lo de siempre.
        handover_expected: std::sync::atomic::AtomicBool,
        client: tokio::sync::RwLock<Option<Arc<Client>>>,
        watches: Mutex<HashMap<u64, watch::Sender<TaskProgress>>>,
        /// Desenlaces vistos SIN watch receptor (el broadcast terminal
        /// puede adelantar a la response del método que creó la task):
        /// `own_task` los consulta para no esperar un progreso que ya pasó.
        finished: Mutex<std::collections::VecDeque<TaskProgress>>,
        foreign_tx: mpsc::UnboundedSender<TaskRef>,
        events_tx: mpsc::UnboundedSender<ConnEvent>,
        /// Aprobaciones de policy hacia el frontend (M3-3b T5): las notifs
        /// `policy.approval_required` de la bomba + el resync de
        /// `policy.pending` al (re)conectar.
        approvals_tx: mpsc::UnboundedSender<PolicyApprovalRequired>,
        /// Avisos `connection.degraded` (#44) hacia el frontend: cada notif de
        /// la bomba se reenvía por aquí (mismo patrón que `approvals_tx`).
        degraded_tx: mpsc::UnboundedSender<ConnectionDegraded>,
        /// `approval_id`s ya entregados al frontend: la entrega del daemon es
        /// at-least-once (broadcast + resync pueden solapar; cada reconexión
        /// re-lista pendientes) y un prompt de SEGURIDAD duplicado confunde
        /// (MAJOR-1 del rust-reviewer). Dedup best-effort acotado.
        seen_approvals: Mutex<std::collections::HashSet<u64>>,
        /// Enrutado de los lotes de `search.hits` por `task_id` (live search).
        /// Compartido por TODOS los clones vía `Inner`: es enrutado (no un
        /// canal one-shot `take_*`), así que una búsqueda lanzada por cualquier
        /// clon recibe sus hits por la bomba única.
        search_routes: Mutex<BatchRoutes<SearchHits>>,
        /// Lo mismo para las `compare.rows` de una `fs.compare` (0.39.0). Mapa
        /// SEPARADO y no uno compartido: los `task_id` de dos feeds distintos
        /// no colisionan, pero un mapa único obligaría a un lote-suma en el
        /// canal y el frontend tendría que filtrar lo que no pidió.
        compare_routes: Mutex<BatchRoutes<CompareRowsBatch>>,
        /// Y lo mismo para `sync.plan` (0.40.0), con una diferencia: los DOS
        /// eventos del plan —`sync.steps` y el `sync.plan_done` que lo cierra—
        /// viajan por UN canal, igual que en el daemon, porque el orden entre
        /// ellos es normativo. Con dos mapas ese orden dependería de cómo el
        /// runtime despierta dos receptores; con uno es la cola.
        sync_routes: Mutex<BatchRoutes<crate::sync::SyncPlanEvent>>,
    }

    impl Inner {
        /// Entrega una aprobación al frontend UNA sola vez por `approval_id`
        /// (la fuente es at-least-once: broadcast + resync de cada
        /// reconexión). El set se poda entero al tope — dedup best-effort
        /// (escala humana), jamás memoria sin límite.
        fn push_approval(&self, req: PolicyApprovalRequired) {
            let mut seen = self
                .seen_approvals
                .lock()
                .expect("seen_approvals lock sano");
            if seen.len() >= 1024 {
                seen.clear();
            }
            if seen.insert(req.approval_id) {
                let _ = self.approvals_tx.send(req);
            }
        }

        /// Encola un aviso `connection.degraded` (#44) hacia el frontend.
        fn push_degraded(&self, d: ConnectionDegraded) {
            let _ = self.degraded_tx.send(d);
        }
    }

    /// Conexión (auto-reconectante) con el daemon. Clonable: todos los
    /// clones comparten conexión y watches (`inner`), pero los canales
    /// one-shot de abajo son POR INSTANCIA — ver el `impl Clone` manual.
    pub struct RemoteBackend {
        inner: Arc<Inner>,
        /// Canal de tasks FORÁNEAS. `Some` solo en la instancia que aún no
        /// lo tomó; un clon nace con `None` (no puede robárselo al dueño).
        foreign_rx: Mutex<Option<mpsc::UnboundedReceiver<TaskRef>>>,
        /// Canal de eventos de conexión. Mismo invariante que `foreign_rx`.
        events_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnEvent>>>,
        /// Canal de aprobaciones de policy. Mismo invariante que `foreign_rx`.
        approvals_rx: Mutex<Option<mpsc::UnboundedReceiver<PolicyApprovalRequired>>>,
        /// Canal de avisos `connection.degraded` (#44). Mismo invariante que
        /// `foreign_rx`.
        degraded_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnectionDegraded>>>,
    }

    impl Clone for RemoteBackend {
        /// Clon ESTRUCTURAL (no derive): comparte `inner` (conexión, watches,
        /// los `_tx`) vía `Arc`, pero los receptores nacen `None`.
        /// Antes vivían dentro de `Inner` (compartido) y un clon podía
        /// `take_*` y robárselos al dueño real (p. ej. la TUI) — el `ask` de
        /// policy caducaría a `deny` en silencio sin que nadie lo viera
        /// (MAJOR del rust-reviewer sobre e408373). Los usos INTERNOS que
        /// clonan `RemoteBackend` (`TaskCanceller::Remote`, `PageState` del
        /// `list_stream`, etc.) jamás llaman `take_*`, así que `None` es
        /// también el valor correcto para ellos.
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
            }
        }
    }

    impl RemoteBackend {
        /// Conecta (arrancando el daemon si `spawn_cmd` lo permite),
        /// negocia `initialize` y resincroniza con `task.list`.
        ///
        /// # Errors
        /// Taxonomía: `ProviderUnavailable` si no hay daemon alcanzable;
        /// `Internal` ante un server incompatible.
        pub async fn connect(
            socket: PathBuf,
            spawn_cmd: Option<Vec<std::ffi::OsString>>,
            client_info: ClientInfo,
        ) -> Result<Self, Error> {
            Self::connect_inner(socket, spawn_cmd, client_info, None).await
        }

        /// Como [`Self::connect`], pero declarando `agent_session`: la
        /// conexión queda ligada al actor de agente que el daemon gobierna
        /// (`Actor::Agent { session }`), y por tanto al gate de agente —
        /// lecturas y mutaciones exigen un scope vivo de esa sesión.
        ///
        /// Existe para el puente MCP, que necesita un brazo capaz de drenar
        /// notificaciones (`fs.compare` y `sync.plan` entregan por ahí) sin
        /// dejar de ser el mismo actor que su conexión de tools. Los scopes de
        /// policy se guardan por SESIÓN (`ScopeRegistry::grant(session, …)`),
        /// no por conexión, así que los permisos concedidos valen igual en las
        /// dos. Lo que NO se comparte es el `conn_id`: un plan retenido para
        /// esta conexión no es redimible desde la otra.
        ///
        /// Sin `spawn_cmd` a propósito: un agente no arranca daemons. Si no
        /// hay uno escuchando, esto falla.
        ///
        /// # Errors
        /// Los de [`Self::connect`], más sesión rechazada por el daemon
        /// (charset `[A-Za-z0-9._-]`, 1..=64).
        pub async fn connect_as_agent(
            socket: PathBuf,
            client_info: ClientInfo,
            agent_session: String,
        ) -> Result<Self, Error> {
            Self::connect_inner(socket, None, client_info, Some(agent_session)).await
        }

        /// El cuerpo compartido de [`Self::connect`] y
        /// [`Self::connect_as_agent`]: un solo handshake, una sola bomba.
        async fn connect_inner(
            socket: PathBuf,
            spawn_cmd: Option<Vec<std::ffi::OsString>>,
            client_info: ClientInfo,
            agent_session: Option<String>,
        ) -> Result<Self, Error> {
            let (foreign_tx, foreign_rx) = mpsc::unbounded_channel();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
            let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();
            let backend = Self {
                inner: Arc::new(Inner {
                    socket,
                    spawn_cmd,
                    client_info,
                    agent_session,
                    handover_expected: std::sync::atomic::AtomicBool::new(false),
                    client: tokio::sync::RwLock::new(None),
                    watches: Mutex::new(HashMap::new()),
                    finished: Mutex::new(std::collections::VecDeque::new()),
                    foreign_tx,
                    events_tx,
                    approvals_tx,
                    degraded_tx,
                    seen_approvals: Mutex::new(std::collections::HashSet::new()),
                    search_routes: Mutex::new(BatchRoutes::default()),
                    compare_routes: Mutex::new(BatchRoutes::default()),
                    sync_routes: Mutex::new(BatchRoutes::default()),
                }),
                foreign_rx: Mutex::new(Some(foreign_rx)),
                events_rx: Mutex::new(Some(events_rx)),
                approvals_rx: Mutex::new(Some(approvals_rx)),
                degraded_rx: Mutex::new(Some(degraded_rx)),
            };
            // La 1ª conexión SÍ arranca el daemon (spawn); las reconexiones
            // NO (M3 del rust-reviewer: reconectar jamás debe resucitar un
            // daemon que el usuario acaba de parar).
            let notifications = backend.establish(true).await.map_err(to_taxonomy)?;
            // UNA task de bomba para toda la vida del backend. Sostiene un
            // Weak (no un Arc): cuando el último `RemoteBackend` externo se
            // suelta, `Inner` se libera y la bomba sale sola — sin ciclo de
            // Arc ni reconexión eterna (M2 del rust-reviewer).
            let weak = Arc::downgrade(&backend.inner);
            tokio::spawn(async move { pump_loop(weak, notifications).await });
            Ok(backend)
        }

        /// Una conexión nueva: connect(+spawn si `spawn`) → initialize →
        /// resync → reconciliación. Devuelve el receptor de notificaciones.
        async fn establish(
            &self,
            spawn: bool,
        ) -> Result<mpsc::UnboundedReceiver<norte_proto::wire::Notification>, ClientError> {
            let mut client = match (&self.inner.spawn_cmd, spawn) {
                (Some(argv), true) => {
                    let argv = argv.clone();
                    Client::connect_or_spawn(&self.inner.socket, move || {
                        let mut cmd = std::process::Command::new(&argv[0]);
                        cmd.args(&argv[1..]);
                        cmd
                    })
                    .await?
                }
                _ => Client::connect(&self.inner.socket).await?,
            };
            // El actor se re-declara en CADA conexión: el daemon lo fija en
            // el handshake y no lo recuerda de la anterior.
            match self.inner.agent_session.clone() {
                Some(session) => {
                    client
                        .initialize_as_agent(self.inner.client_info.clone(), session)
                        .await?;
                }
                None => {
                    client.initialize(self.inner.client_info.clone()).await?;
                }
            }
            let notifications = client.take_notifications();
            let client = Arc::new(client);
            *self.inner.client.write().await = Some(Arc::clone(&client));

            // A partir de AQUÍ, un fallo tiene que dejar el hueco vacío
            // (#181). Publicar el cliente antes del resync es correcto —el
            // resync se hace CON él—, pero si el resync falla y el cliente se
            // queda puesto, el llamante habla por una conexión cuyo receptor
            // de notificaciones acaba de morir con este marco: nada se
            // enruta, `task.progress` incluido, y un `TaskRef::join()` —que
            // no tiene plazo— espera para siempre un terminal que ya no puede
            // llegar. Con el hueco a `None`, el siguiente intento empieza
            // limpio y el llamante recibe un error en vez de un silencio.
            let resync = self.resync(&client).await;
            if let Err(e) = resync {
                *self.inner.client.write().await = None;
                return Err(e);
            }
            Ok(notifications)
        }

        /// El resync de una conexión recién hecha: las tasks que ya corrían,
        /// la reconciliación de huérfanas y las aprobaciones pendientes.
        ///
        /// Separado de [`Self::establish`] para que su fallo tenga UN camino
        /// de salida y no varios `?` repartidos, cada uno con su ocasión de
        /// olvidar que hay estado publicado que limpiar (#181).
        async fn resync(&self, client: &Arc<Client>) -> Result<(), ClientError> {
            // Resync: el estado de las tasks que ya corrían (o terminaron
            // mientras no estábamos — el server retiene desenlaces recientes).
            let list: TaskListResult = client.call(methods::TASK_LIST, &TaskListParams {}).await?;
            let mut live: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for snapshot in list.tasks {
                live.insert(snapshot.task_id.get());
                self.route(snapshot);
            }
            // Reconciliación (B1): una task NUESTRA en vuelo que el daemon
            // (posiblemente reiniciado y vacío) ya no conoce jamás recibiría
            // su terminal — su `join()` colgaría. Se resuelve `Failed`.
            self.fail_orphans(&live);
            // Resync de aprobaciones de policy pendientes (M3-3b T5): un Ask
            // difundido ANTES de esta conexión no se pierde. Best-effort: un
            // daemon N-1 (sin `policy.pending`) responde METHOD_NOT_FOUND y
            // no pasa nada; el TTL de una pendiente sin ver la deniega solo.
            //
            // Una conexión de AGENTE no lo pide: `policy.pending` es
            // human-only y el daemon le contesta INVALID_REQUEST, que no es
            // METHOD_NOT_FOUND y caería en el `warn!` de abajo — un aviso por
            // conexión Y POR RECONEXIÓN, para siempre, en el canal donde hay
            // que poder leer los avisos de verdad. Además no tendría sentido:
            // quien aprueba es el humano, jamás el agente.
            if self.inner.agent_session.is_some() {
                return Ok(());
            }
            match client
                .call::<_, PolicyPendingResult>(methods::POLICY_PENDING, &serde_json::json!({}))
                .await
            {
                Ok(listed) => {
                    for p in listed.pending {
                        self.inner.push_approval(PolicyApprovalRequired {
                            approval_id: p.approval_id,
                            session: p.session,
                            op: p.op,
                            paths: p.paths,
                            paths_total: p.paths_total,
                            // El TTL restante no viaja en `policy.pending`:
                            // 0 = desconocido (documentado en proto).
                            ttl_ms: 0,
                        });
                    }
                }
                // Se sigue igual en ambos casos, pero un fallo real no debe
                // confundirse en silencio con un daemon N-1 (m4 del review).
                Err(ClientError::Rpc(ref rpc))
                    if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
                {
                    tracing::debug!("daemon sin policy.pending (N-1): resync omitido");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "resync de policy.pending falló");
                }
            }
            Ok(())
        }

        /// Marca `Failed{ProviderUnavailable}` toda task con watch vivo que
        /// el daemon ya no conoce (ni viva ni recién-terminada): su
        /// desenlace se perdió con la desconexión (B1 del rust-reviewer).
        fn fail_orphans(&self, live: &std::collections::HashSet<u64>) {
            let mut watches = self.inner.watches.lock().expect("watches lock sano");
            let orphans: Vec<u64> = watches
                .keys()
                .copied()
                .filter(|id| !live.contains(id))
                .collect();
            for id in orphans {
                if let Some(sender) = watches.remove(&id) {
                    let mut last = sender.borrow().clone();
                    last.state = TaskState::Failed {
                        error: Error::ProviderUnavailable { retryable: false },
                    };
                    let _ = sender.send(last);
                }
            }
        }

        /// Rutea UN snapshot: al watch de su task, creándolo (y
        /// anunciándolo como task foránea) si es la primera vez.
        fn route(&self, snapshot: TaskProgress) {
            let id = snapshot.task_id;
            // Búsqueda terminal: programa la retirada de su route TRAS la gracia
            // (deja pasar los `search.hits` rezagados; ver `BATCH_ROUTE_GRACE`).
            // FILTRO POR KIND (obligatorio): solo se marca `terminated` para
            // terminales de BÚSQUEDA. Si se marcara todo, los terminales de
            // copy/move/delete/list ajenos llenarían el set y el `clear(256)`
            // podría borrar la marca de una búsqueda viva justo entre su
            // terminal y el registro de su route → route colgado (sender leak).
            // La marca se toma SIEMPRE bajo el lock: si el terminal adelantó al
            // registro del route (aún no hay route), `search` verá la marca y
            // programará la retirada él. Así ninguna de las dos órdenes lo deja
            // colgado. Se agenda SOLO en la inserción nueva (`mark_terminated`
            // devuelve `true`) y con route vivo: un terminal duplicado
            // (bomba + resync) no vuelve a agendar. El lock de `search_routes`
            // es independiente del de `watches`; se toma y suelta aquí, sin
            // anidar. Mejora futura: un cierre ESTRUCTURAL (sentinela «search
            // done» tras drenar los hits) evitaría la gracia por tiempo.
            //
            // `fs.compare` (0.39.0) tiene su propio mapa y el mismo trato: su
            // kind es `Compare` y sus lotes son `compare.rows`.
            if snapshot.state.is_terminal() {
                match snapshot.kind {
                    TaskKind::Search => {
                        let schedule = {
                            let mut sr = self
                                .inner
                                .search_routes
                                .lock()
                                .expect("search_routes lock sano");
                            let newly = sr.mark_terminated(id.get());
                            newly && sr.routes.contains_key(&id.get())
                        };
                        if schedule {
                            schedule_route_removal(&self.inner, id.get(), |i| &i.search_routes);
                        }
                    }
                    TaskKind::Compare => {
                        let schedule = {
                            let mut cr = self
                                .inner
                                .compare_routes
                                .lock()
                                .expect("compare_routes lock sano");
                            let newly = cr.mark_terminated(id.get());
                            newly && cr.routes.contains_key(&id.get())
                        };
                        if schedule {
                            schedule_route_removal(&self.inner, id.get(), |i| &i.compare_routes);
                        }
                    }
                    TaskKind::SyncPlan => {
                        let schedule = {
                            let mut sr = self
                                .inner
                                .sync_routes
                                .lock()
                                .expect("sync_routes lock sano");
                            let newly = sr.mark_terminated(id.get());
                            newly && sr.routes.contains_key(&id.get())
                        };
                        if schedule {
                            schedule_route_removal(&self.inner, id.get(), |i| &i.sync_routes);
                        }
                    }
                    _ => {}
                }
            }
            let mut watches = self.inner.watches.lock().expect("watches lock sano");
            if let Some(sender) = watches.get(&id.get()) {
                let terminal = snapshot.state.is_terminal();
                let _ = sender.send(snapshot.clone());
                if terminal {
                    watches.remove(&id.get());
                    self.remember_finished(snapshot);
                }
                return;
            }
            // Task nueva no pedida por este proceso: si ya llegó terminal
            // no se anuncia como foránea, pero SÍ se recuerda — el terminal
            // por broadcast puede adelantar a la response de fs.copy y
            // own_task lo necesita. El lock de watches se RETIENE durante
            // el registro (orden watches→finished en todas partes): así
            // own_task no puede colarse entre ambos y perder el desenlace.
            if snapshot.state.is_terminal() {
                self.remember_finished(snapshot);
                drop(watches);
                return;
            }
            let (sender, rx) = watch::channel(snapshot);
            watches.insert(id.get(), sender);
            drop(watches);
            let _ = self.inner.foreign_tx.send(TaskRef {
                id,
                rx,
                canceller: TaskCanceller::Remote {
                    backend: self.clone(),
                    id,
                },
            });
        }

        async fn client(&self) -> Result<Arc<Client>, Error> {
            self.inner
                .client
                .read()
                .await
                .clone()
                .ok_or(Error::ProviderUnavailable { retryable: true })
        }

        /// Una request con TOPE de tiempo (M4 del rust-reviewer): un daemon
        /// vivo-pero-atascado jamás congela al frontend — a los
        /// [`CALL_TIMEOUT`] la operación falla `ProviderUnavailable`.
        async fn call_timed<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
        where
            P: serde::Serialize,
            R: serde::de::DeserializeOwned,
        {
            let client = self.client().await?;
            match tokio::time::timeout(CALL_TIMEOUT, client.call(method, params)).await {
                Ok(res) => res.map_err(to_taxonomy),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// Como [`Self::call_timed`] pero CANCEL-ON-DROP (#74, patrón #72):
        /// si este future se dropea (o expira el timeout) con la request aún
        /// EN VUELO, envía `rpc.cancel {id}` best-effort — el dispatch del
        /// daemon muere PRE-efecto y la Task no nace huérfana sin canceller
        /// (la ventana del driver Lua que abandona el run con el submit en
        /// vuelo). Un id cuyo dispatch YA terminó es un no-op en el daemon.
        /// Lo usan las mutaciones (fs.copy/move/delete/mkdir), `index.build`
        /// y — vía [`Self::call_timed_guarded_with`] — `ai.rename_plan`. El
        /// daemon envuelve en su brazo de cancelación (#72) todos esos
        /// métodos MENOS `index.build`: para ese el guard es best-effort (el
        /// `rpc.cancel` no encuentra dispatch que cortar).
        async fn call_timed_guarded<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
        where
            P: serde::Serialize,
            R: serde::de::DeserializeOwned,
        {
            self.call_timed_guarded_with(CALL_TIMEOUT, method, params)
                .await
        }

        /// Como [`Self::call_timed_guarded`] con timeout EXPLÍCITO: la llamada
        /// de IA usa [`AI_CALL_TIMEOUT`] (un modelo remoto tarda legítimamente
        /// más que el [`CALL_TIMEOUT`] de un fs.*).
        async fn call_timed_guarded_with<P, R>(
            &self,
            timeout: Duration,
            method: &str,
            params: &P,
        ) -> Result<R, Error>
        where
            P: serde::Serialize,
            R: serde::de::DeserializeOwned,
        {
            let client = self.client().await?;
            let mut guard = CancelOnAbandon {
                client: std::sync::Arc::clone(&client),
                id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                armed: true,
            };
            let id_cell = std::sync::Arc::clone(&guard.id);
            let res = match tokio::time::timeout(
                timeout,
                client.call_tracked(method, params, |id| {
                    // El contador del Client arranca en 1: `0` = «aún sin id».
                    id_cell.store(id, std::sync::atomic::Ordering::SeqCst);
                }),
            )
            .await
            {
                Ok(res) => res.map_err(to_taxonomy),
                // Timeout: el guard queda ARMADO — el return lo dropea y el
                // rpc.cancel viaja (antes, el dispatch seguía corriendo
                // server-side sin nadie escuchando).
                Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
            };
            // Respuesta recibida (ok o error del RPC): ya no hay nada que
            // cancelar — desarmar para no cancelar un id reutilizable.
            guard.armed = false;
            res
        }

        /// Listado remoto como stream perezoso: primera página EAGER (paridad
        /// de errores) + `try_unfold` sobre el `next_cursor`. Sin deps nuevas.
        /// El `skipped` del contenedor (#93) viaja en cada página — basta el
        /// de la primera (un daemon N-1 no lo manda: `None` = desconocido).
        pub(super) async fn list_stream(
            &self,
            dir: &VPath,
            attrs: Vec<String>,
        ) -> Result<(EntryStream, Option<u64>), Error> {
            // Primera página síncrona: un `NotFound`/`TypeMismatch` sale en el
            // Result, no como primer item del stream (paridad con el embebido).
            let first: FsListResult = self
                .call_timed(
                    methods::FS_LIST,
                    &FsListParams {
                        path: dir.clone(),
                        limit: Some(LIST_PAGE),
                        cursor: None,
                        attrs: attrs.clone(),
                    },
                )
                .await?;
            let skipped = first.skipped;
            let done = first.next_cursor.is_none();
            let state = PageState {
                backend: self.clone(),
                dir: dir.clone(),
                buffer: first.entries.into(),
                cursor: first.next_cursor,
                done,
                attrs,
            };
            Ok((
                futures::stream::try_unfold(state, page_step).boxed(),
                skipped,
            ))
        }

        pub(super) async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
            Ok(self.capabilities_full(path).await?.capabilities)
        }

        pub(super) async fn attr_catalog(
            &self,
            path: &VPath,
        ) -> Result<norte_proto::AttrCatalog, Error> {
            Ok(self.capabilities_full(path).await?.attrs)
        }

        pub(super) async fn capabilities_full(
            &self,
            path: &VPath,
        ) -> Result<FsCapabilitiesResult, Error> {
            self.call_timed(
                methods::FS_CAPABILITIES,
                &FsCapabilitiesParams { path: path.clone() },
            )
            .await
        }

        pub(super) async fn stat(&self, path: &VPath, attrs: Vec<String>) -> Result<Entry, Error> {
            let r: FsStatResult = self
                .call_timed(
                    methods::FS_STAT,
                    &FsStatParams {
                        path: path.clone(),
                        attrs,
                    },
                )
                .await?;
            Ok(r.entry)
        }

        pub(super) async fn trust_host_key(
            &self,
            host: &str,
            port: Option<u16>,
            algo: &str,
            fingerprint: &str,
        ) -> Result<(), Error> {
            let r: methods::ConnectionTrustHostKeyResult = self
                .call_timed(
                    methods::CONNECTION_TRUST_HOST_KEY,
                    &methods::ConnectionTrustHostKeyParams {
                        host: host.to_string(),
                        port,
                        algo: algo.to_string(),
                        fingerprint: fingerprint.to_string(),
                    },
                )
                .await?;
            // `trusted: false` está reservado en 0.7.0 para un rechazo por
            // política de un core futuro: tratarlo como éxito dejaría al
            // usuario en un bucle de reintentos "confiados" que nunca
            // registran nada.
            if r.trusted {
                Ok(())
            } else {
                Err(Error::PolicyDenied {
                    rule: "connection.trust_host_key".to_string(),
                })
            }
        }

        pub(super) async fn read(
            &self,
            path: &VPath,
            range: Option<ByteRange>,
        ) -> Result<Vec<u8>, Error> {
            let mut offset = range.as_ref().map_or(0, |r| r.offset);
            let mut remaining = range.as_ref().and_then(|r| r.len);
            let mut out: Vec<u8> = Vec::new();
            loop {
                let want = remaining.map_or(methods::FS_READ_MAX_CHUNK, |r| {
                    r.min(methods::FS_READ_MAX_CHUNK)
                });
                if want == 0 {
                    return Ok(out);
                }
                let r: FsReadResult = self
                    .call_timed(
                        methods::FS_READ,
                        &FsReadParams {
                            path: path.clone(),
                            range: Some(ByteRange {
                                offset,
                                len: Some(want),
                            }),
                        },
                    )
                    .await?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(r.content_b64.as_bytes())
                    .map_err(|_| Error::Internal { panic: false })?;
                // Guard defensivo (m1 del rust/protocol reviewers): un server
                // que devuelva 0 bytes con eof=false haría bucle infinito.
                if bytes.is_empty() && !r.eof {
                    return Err(Error::Internal { panic: false });
                }
                offset += bytes.len() as u64;
                if let Some(rem) = &mut remaining {
                    *rem = rem.saturating_sub(bytes.len() as u64);
                }
                out.extend_from_slice(&bytes);
                if r.eof {
                    return Ok(out);
                }
            }
        }

        pub(super) async fn transfer(
            &self,
            method: &str,
            from: &VPath,
            to: &VPath,
            opts: TransferOptions,
        ) -> Result<TaskRef, Error> {
            let result: FsTaskResult = if method == methods::FS_COPY {
                self.call_timed_guarded(
                    method,
                    &FsCopyParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
                        resume: opts.resume,
                        verify: opts.verify,
                    },
                )
                .await?
            } else {
                self.call_timed_guarded(
                    method,
                    &FsMoveParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
                        resume: opts.resume,
                        verify: opts.verify,
                    },
                )
                .await?
            };
            let kind = if method == methods::FS_COPY {
                TaskKind::Copy
            } else {
                TaskKind::Move
            };
            Ok(self.own_task(result.task_id, kind))
        }

        pub(super) async fn index_build(&self, root: &VPath) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::INDEX_BUILD,
                    &methods::IndexBuildParams { root: root.clone() },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Index))
        }

        pub(super) async fn index_query(
            &self,
            root: &VPath,
            text: &str,
            limit: u32,
        ) -> Result<Vec<methods::IndexHit>, Error> {
            let r: methods::IndexQueryResult = self
                .call_timed(
                    methods::INDEX_QUERY,
                    &methods::IndexQueryParams {
                        root: root.clone(),
                        text: text.to_string(),
                        limit,
                    },
                )
                .await?;
            Ok(r.hits)
        }

        pub(super) async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::INDEX_EMBED,
                    &methods::IndexEmbedParams { root: root.clone() },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Embed))
        }

        /// `index.search_semantic` (0.33.0, M4-IA-2): respuesta directa con
        /// el timeout LARGO de IA y cancel-on-drop (el daemon la tiene en su
        /// brazo de cancelación #72 — abandonar la espera corta el dispatch).
        pub(super) async fn index_search_semantic(
            &self,
            root: Option<&VPath>,
            query: &str,
            k: u32,
        ) -> Result<Vec<methods::SemanticHit>, Error> {
            let r: methods::IndexSearchSemanticResult = self
                .call_timed_guarded_with(
                    AI_CALL_TIMEOUT,
                    methods::INDEX_SEARCH_SEMANTIC,
                    &methods::IndexSearchSemanticParams {
                        root: root.cloned(),
                        query: query.to_owned(),
                        k,
                    },
                )
                .await?;
            Ok(r.hits)
        }

        /// `ai.rename_plan` (0.32.0, M4-IA): respuesta directa con el timeout
        /// LARGO de IA y cancel-on-drop (el daemon lo tiene en su brazo de
        /// cancelación #72 — abandonar la espera corta el dispatch).
        pub(super) async fn ai_rename_plan(
            &self,
            dir: &VPath,
            instruction: &str,
        ) -> Result<methods::AiRenamePlanResult, Error> {
            self.call_timed_guarded_with(
                AI_CALL_TIMEOUT,
                methods::AI_RENAME_PLAN,
                &methods::AiRenamePlanParams {
                    dir: dir.clone(),
                    instruction: instruction.to_string(),
                },
            )
            .await
        }

        pub(super) async fn delete(
            &self,
            path: &VPath,
            mode: DeleteMode,
        ) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::FS_DELETE,
                    &FsDeleteParams {
                        path: path.clone(),
                        mode,
                    },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Delete))
        }

        /// `fs.rename_batch_plan` (0.36.0, ADR 0042): respuesta DIRECTA, sin
        /// Task. El daemon la tiene en su brazo de cancelación (#72), así que
        /// abandonar la espera corta el dispatch en vez de dejarlo planificando
        /// un directorio enorme.
        pub(super) async fn rename_batch_plan(
            &self,
            dir: &VPath,
            pairs: &[methods::RenamePair],
        ) -> Result<methods::FsRenameBatchPlanResult, Error> {
            self.call_timed_guarded(
                methods::FS_RENAME_BATCH_PLAN,
                &methods::FsRenameBatchPlanParams {
                    dir: dir.clone(),
                    pairs: pairs.to_vec(),
                },
            )
            .await
        }

        /// `fs.rename_batch` (0.36.0, ADR 0042): UNA Task para el lote entero.
        /// Se manda la MISMA intención que produjo el `plan_hash`; el orden
        /// jamás cruza el wire.
        pub(super) async fn rename_batch(
            &self,
            dir: &VPath,
            pairs: &[methods::RenamePair],
            plan_hash: &methods::PlanHash,
        ) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::FS_RENAME_BATCH,
                    &methods::FsRenameBatchParams {
                        dir: dir.clone(),
                        pairs: pairs.to_vec(),
                        plan_hash: plan_hash.clone(),
                    },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::RenameBatch))
        }

        /// `fs.rename_batch_report` (0.36.0): informe del lote. Un daemon N-1
        /// sin el método responde `METHOD_NOT_FOUND` → `Unsupported`, para que
        /// el caller lo distinga de un fallo REAL — mismo criterio que
        /// `policy.undo_report` (#71): el informe es la ÚNICA señal de que un
        /// lote dejó el directorio a medias, y no se degrada en silencio.
        pub(super) async fn rename_batch_report(
            &self,
            task_id: TaskId,
        ) -> Result<methods::FsRenameBatchReportResult, Error> {
            let client = self.client().await?;
            let params = methods::FsRenameBatchReportParams { task_id };
            let call = client.call::<_, methods::FsRenameBatchReportResult>(
                methods::FS_RENAME_BATCH_REPORT,
                &params,
            );
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Err(ClientError::Rpc(ref rpc)))
                    if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
                {
                    Err(Error::Unsupported)
                }
                Ok(res) => res.map_err(to_taxonomy),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        pub(super) async fn mkdir(&self, path: &VPath) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::FS_MKDIR,
                    &norte_proto::methods::FsMkdirParams { path: path.clone() },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Mkdir))
        }

        /// `fs.search` (live search T5): lanza la Task y devuelve el `rx` por el
        /// que la bomba enruta los lotes de `search.hits` de ESTE `task_id`.
        ///
        /// Orden ANTI-CARRERA: el route se registra ANTES de que puedan llegar
        /// más hits. La bomba (otra task) puede haber enrutado ya lotes que
        /// adelantaron a esta respuesta —el frame de `search.hits` puede salir
        /// del daemon antes que la respuesta de `fs.search`, y en el cliente la
        /// bomba y esta llamada corren en paralelo—: esos lotes se quedaron en
        /// `pending`. El registro (drenar `pending` + insertar el route) es
        /// ATÓMICO bajo el lock de `search_routes`, así que ni un lote se pierde
        /// entre ambos pasos. Es el mismo patrón con el que `own_task` cierra la
        /// carrera del terminal adelantado vía el anillo `finished`.
        pub(super) async fn search(
            &self,
            params: FsSearchParams,
        ) -> Result<(TaskRef, mpsc::Receiver<SearchHits>), Error> {
            let result: FsTaskResult = self.call_timed(methods::FS_SEARCH, &params).await?;
            let id = result.task_id;
            let rx = register_route(&self.inner, id.get(), methods::SEARCH_HITS, |i| {
                &i.search_routes
            });
            Ok((self.own_task(id, TaskKind::Search), rx))
        }

        /// `fs.compare` (0.39.0, ADR 0048): lanza la Task y devuelve el `rx`
        /// por el que la bomba enruta los lotes de `compare.rows` de ESTE
        /// `task_id`. Mismo ciclo de vida que [`Self::search`], incluida la
        /// carrera de arranque.
        ///
        /// Un daemon N-1 (0.38.x) NO tiene el método y contesta
        /// `METHOD_NOT_FOUND`: se traduce a [`Error::Unsupported`] para que el
        /// frontend distinga «tu daemon es más viejo» de un fallo real (mismo
        /// criterio que `undo_report` y que el resync de `policy.pending`).
        /// `version_compatible` acepta N-1, así que esta combinación no es
        /// hipotética.
        pub(super) async fn compare(
            &self,
            params: methods::FsCompareParams,
        ) -> Result<(TaskRef, mpsc::Receiver<CompareRowsBatch>), Error> {
            let client = self.client().await?;
            let call = client.call::<_, FsTaskResult>(methods::FS_COMPARE, &params);
            let result: FsTaskResult = match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Err(ClientError::Rpc(ref rpc)))
                    if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
                {
                    return Err(Error::Unsupported);
                }
                Ok(res) => res.map_err(to_taxonomy)?,
                Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
            };
            let id = result.task_id;
            let rx = register_route(&self.inner, id.get(), methods::COMPARE_ROWS, |i| {
                &i.compare_routes
            });
            Ok((self.own_task(id, TaskKind::Compare), rx))
        }

        /// `sync.plan` (0.40.0, ADR 0049): lanza la Task y devuelve el `rx` por
        /// el que la bomba enruta los DOS eventos del plan (`sync.steps` y el
        /// `sync.plan_done` que lo cierra) de ESTE `task_id`. Mismo ciclo de
        /// vida que [`Self::compare`], incluida la carrera de arranque.
        ///
        /// Un daemon N-1 sin el método contesta `METHOD_NOT_FOUND` →
        /// [`Error::Unsupported`], para que el frontend distinga «tu daemon es
        /// más viejo» de un fallo real.
        pub(super) async fn sync_plan(
            &self,
            params: methods::SyncPlanParams,
        ) -> Result<(TaskRef, mpsc::Receiver<crate::sync::SyncPlanEvent>), Error> {
            let result: FsTaskResult = self.call_maybe_unknown(methods::SYNC_PLAN, &params).await?;
            let id = result.task_id;
            let rx = register_route(&self.inner, id.get(), methods::SYNC_STEPS, |i| {
                &i.sync_routes
            });
            Ok((self.own_task(id, TaskKind::SyncPlan), rx))
        }

        /// `sync.apply` (0.40.0, ADR 0049): ejecuta el plan que `plan_hash`
        /// nombra. No lleva rutas — las dos raíces salen del plan retenido
        /// server-side, atado a ESTA conexión.
        pub(super) async fn sync_apply(
            &self,
            plan_hash: &methods::PlanHash,
        ) -> Result<TaskRef, Error> {
            let params = methods::SyncApplyParams {
                plan_hash: plan_hash.clone(),
            };
            // CANCEL-ON-DROP (#74), y aquí no es una precaución de más: el gate
            // de `sync.apply` puede quedarse suspendido en un `ask` de policy
            // más de lo que dura [`CALL_TIMEOUT`], y sin el guard el despacho
            // seguiría vivo server-side — el humano aprobaría un minuto después
            // y el árbol se reescribiría para un cliente que ya había desistido
            // y había recibido «el daemon no contesta». Con él, abandonar manda
            // el `rpc.cancel` que retira el gate PRE-efecto.
            //
            // Se pierde a cambio la traducción de `METHOD_NOT_FOUND`, y no
            // importa: para llegar aquí hace falta un `plan_hash`, que solo
            // puede haber salido de un `sync.plan` del MISMO daemon.
            let result: FsTaskResult = self
                .call_timed_guarded(methods::SYNC_APPLY, &params)
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Sync))
        }

        /// `sync.report` (0.40.0, ADR 0049): el informe de una aplicación ya
        /// lanzada. `NotFound` si ese id no es una aplicación que este daemon
        /// retenga — y esa es también la respuesta a un id de OTRA conexión, a
        /// propósito.
        pub(super) async fn sync_report(
            &self,
            task_id: TaskId,
        ) -> Result<methods::SyncReportResult, Error> {
            let params = methods::SyncReportParams { task_id };
            self.call_maybe_unknown(methods::SYNC_REPORT, &params).await
        }

        /// Una llamada cuyo `METHOD_NOT_FOUND` significa «tu daemon es más
        /// viejo» y se entrega como [`Error::Unsupported`], no como el
        /// `Internal` en el que `to_taxonomy` convertiría un `-32601`. Es el
        /// patrón que `compare` y `undo_report` ya escribían a mano.
        async fn call_maybe_unknown<P: serde::Serialize, R: serde::de::DeserializeOwned>(
            &self,
            method: &'static str,
            params: &P,
        ) -> Result<R, Error> {
            let client = self.client().await?;
            let call = client.call::<_, R>(method, params);
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Err(ClientError::Rpc(ref rpc)))
                    if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
                {
                    Err(Error::Unsupported)
                }
                Ok(res) => res.map_err(to_taxonomy),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// `policy.undo_session` (M3-4): un humano deshace la sesión de un
        /// agente. Corre como Task de undo con progreso/cancel como las demás.
        pub(super) async fn undo_session(&self, session: &str) -> Result<TaskRef, Error> {
            let result: methods::PolicyUndoSessionResult = self
                .call_timed(
                    methods::POLICY_UNDO_SESSION,
                    &methods::PolicyUndoSessionParams {
                        session: session.to_owned(),
                    },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Undo))
        }

        /// `policy.undo_report` (#71): informe de la Task de undo. Un daemon
        /// N-1 sin el método responde `METHOD_NOT_FOUND` → `Unsupported`, para
        /// que el caller lo distinga de un fallo REAL (el informe es la única
        /// señal de que un undo Completed se bloqueó o saltó — no se degrada
        /// en silencio; mismo criterio que el resync de `policy.pending`).
        pub(super) async fn undo_report(
            &self,
            task_id: TaskId,
        ) -> Result<methods::PolicyUndoReportResult, Error> {
            let client = self.client().await?;
            let params = methods::PolicyUndoReportParams { task_id };
            let call = client
                .call::<_, methods::PolicyUndoReportResult>(methods::POLICY_UNDO_REPORT, &params);
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(Err(ClientError::Rpc(ref rpc)))
                    if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
                {
                    Err(Error::Unsupported)
                }
                Ok(res) => res.map_err(to_taxonomy),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// Recuerda un desenlace (anillo acotado).
        fn remember_finished(&self, snapshot: TaskProgress) {
            let mut finished = self.inner.finished.lock().expect("finished lock sano");
            finished.push_back(snapshot);
            while finished.len() > 128 {
                finished.pop_front();
            }
        }

        /// `TaskRef` de una task PEDIDA por este proceso: engancha (o crea)
        /// su watch. Si la bomba llegó antes (broadcast) y ya la anunció
        /// como foránea, el frontend dedupe por id (contrato documentado);
        /// si ya llegó su TERMINAL, el watch nace resuelto.
        fn own_task(&self, id: TaskId, kind: TaskKind) -> TaskRef {
            // Orden de locks watches→finished (el mismo que route): con
            // watches retenido, un desenlace o ya está en finished o
            // llegará al watch que se crea abajo — sin ventana.
            let mut watches = self.inner.watches.lock().expect("watches lock sano");
            if let Some(done) = self
                .inner
                .finished
                .lock()
                .expect("finished lock sano")
                .iter()
                .find(|p| p.task_id == id)
                .cloned()
            {
                drop(watches);
                let (_sender, rx) = watch::channel(done);
                return TaskRef {
                    id,
                    rx,
                    canceller: TaskCanceller::Remote {
                        backend: self.clone(),
                        id,
                    },
                };
            }
            let rx = if let Some(sender) = watches.get(&id.get()) {
                sender.subscribe()
            } else {
                let initial = TaskProgress {
                    task_id: id,
                    kind,
                    state: TaskState::Pending,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 0,
                    entries_total: None,
                    current: None,
                };
                let (sender, rx) = watch::channel(initial);
                watches.insert(id.get(), sender);
                rx
            };
            TaskRef {
                id,
                rx,
                canceller: TaskCanceller::Remote {
                    backend: self.clone(),
                    id,
                },
            }
        }

        /// `task.cancel` fire-and-forget (la confirmación llega por
        /// `task.progress`, contrato del método).
        pub(crate) fn spawn_cancel(&self, id: TaskId) {
            let backend = self.clone();
            tokio::spawn(async move {
                if let Ok(client) = backend.client().await {
                    let _ = client
                        .call::<_, TaskCancelResult>(
                            methods::TASK_CANCEL,
                            &TaskCancelParams { task_id: id },
                        )
                        .await;
                }
            });
        }

        pub(super) fn take_foreign_tasks(&self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
            self.foreign_rx.lock().expect("foreign_rx lock sano").take()
        }

        pub(super) fn take_conn_events(&self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
            self.events_rx.lock().expect("events_rx lock sano").take()
        }

        pub(super) fn take_approvals(
            &self,
        ) -> Option<mpsc::UnboundedReceiver<PolicyApprovalRequired>> {
            self.approvals_rx
                .lock()
                .expect("approvals_rx lock sano")
                .take()
        }

        /// Se lleva el receptor de avisos `connection.degraded` (#44). Uno solo
        /// (el primer dueño), como los otros `take_*`.
        pub(super) fn take_degraded(&self) -> Option<mpsc::UnboundedReceiver<ConnectionDegraded>> {
            self.degraded_rx
                .lock()
                .expect("degraded_rx lock sano")
                .take()
        }

        /// `policy.decide` contra el daemon (M3-3b T5).
        pub(super) async fn policy_decide(
            &self,
            approval_id: u64,
            approve: bool,
        ) -> Result<(), Error> {
            let _: PolicyDecideResult = self
                .call_timed(
                    methods::POLICY_DECIDE,
                    &PolicyDecideParams {
                        approval_id,
                        approve,
                    },
                )
                .await?;
            Ok(())
        }

        /// `plugin.list` contra el daemon (M4-P3).
        pub(super) async fn plugins_list(&self) -> Result<methods::PluginListResult, Error> {
            self.call_timed(methods::PLUGIN_LIST, &methods::PluginListParams {})
                .await
        }

        /// `host.volumes` contra el daemon (0.37.0, #131). El gate por actor
        /// vive server-side (diseño §C): una conexión de agente ve
        /// [`Error::PolicyDenied`] aquí, no un fallo de transporte.
        pub(super) async fn volumes(
            &self,
            include_pseudo: bool,
        ) -> Result<Vec<methods::Volume>, Error> {
            let result: methods::HostVolumesResult = self
                .call_timed(
                    methods::HOST_VOLUMES,
                    &methods::HostVolumesParams { include_pseudo },
                )
                .await?;
            Ok(result.volumes)
        }

        /// `plugin.set_approval` contra el daemon (M4-P3).
        pub(super) async fn plugins_set_approval(
            &self,
            id: &str,
            approved: bool,
        ) -> Result<(), Error> {
            let _: methods::PluginSetApprovalResult = self
                .call_timed(
                    methods::PLUGIN_SET_APPROVAL,
                    &methods::PluginSetApprovalParams {
                        id: id.to_owned(),
                        approved,
                    },
                )
                .await?;
            Ok(())
        }

        /// `plugin.set_enabled` contra el daemon (M4-P3).
        pub(super) async fn plugins_set_enabled(
            &self,
            id: &str,
            enabled: bool,
        ) -> Result<(), Error> {
            let _: methods::PluginSetEnabledResult = self
                .call_timed(
                    methods::PLUGIN_SET_ENABLED,
                    &methods::PluginSetEnabledParams {
                        id: id.to_owned(),
                        enabled,
                    },
                )
                .await?;
            Ok(())
        }

        /// `plugin.run_command` contra el daemon (M4-P4): devuelve la salida del
        /// comando. El daemon ya redacta los fallos de runtime a `Internal`.
        pub(super) async fn plugin_run_command(
            &self,
            id: &str,
            command: &str,
            arg: &str,
        ) -> Result<String, Error> {
            let r: methods::PluginRunCommandResult = self
                .call_timed(
                    methods::PLUGIN_RUN_COMMAND,
                    &methods::PluginRunCommandParams {
                        id: id.to_owned(),
                        command: command.to_owned(),
                        arg: arg.to_owned(),
                    },
                )
                .await?;
            Ok(r.output)
        }

        /// `plugin.preview` contra el daemon (M4-P5): devuelve el result tal cual
        /// (la preview, o `None`). El daemon ya redacta los fallos de runtime a
        /// `INTERNAL_ERROR` y resuelve el previewer fail-closed.
        pub(super) async fn plugin_preview(
            &self,
            path: &VPath,
        ) -> Result<methods::PluginPreviewResult, Error> {
            self.call_timed(
                methods::PLUGIN_PREVIEW,
                &methods::PluginPreviewParams { path: path.clone() },
            )
            .await
        }

        /// `plugin.preview_styled` contra el daemon (G3a, ADR 0037): gemelo
        /// con estilo de [`Self::plugin_preview`]. `MethodNotFound` (-32601)
        /// es el trigger REAL dentro de la MISMA ventana 0.27 (un daemon
        /// 0.27 sin este handler aún cableado — el ADR distingue esto de
        /// `VERSION_MISMATCH`, que ni deja intentar la llamada): se traduce
        /// a `Ok(None)`, exactamente lo mismo que "ningún previewer
        /// aplica" — el caller cae a [`Self::plugin_preview`] (plano). NO
        /// se usa `call_timed` aquí (mismo motivo que `undo_report`,
        /// arriba): `call_timed`/`to_taxonomy` solo miran `rpc.data`, que
        /// para un `METHOD_NOT_FOUND` de la rama `_` del dispatch es `None`
        /// — el código -32601 se perdería. Cualquier OTRO fallo (I/O,
        /// timeout, un fallo real de runtime redactado por el daemon…) se
        /// propaga tal cual.
        pub(super) async fn plugin_preview_styled(
            &self,
            path: &VPath,
        ) -> Result<Option<methods::PluginPreviewStyled>, Error> {
            let client = self.client().await?;
            let params = methods::PluginPreviewStyledParams { path: path.clone() };
            let call = client.call::<_, methods::PluginPreviewStyledResult>(
                methods::PLUGIN_PREVIEW_STYLED,
                &params,
            );
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(res) => map_styled_preview_result(res),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// `plugin.decorate` contra el daemon (G3b, ADR 0037): `MethodNotFound`
        /// (mismos DOS triggers documentados en el ADR — un daemon 0.27 que aún
        /// no cableó el handler, o un cliente que decide no llamarlo) cae a SIN
        /// decoraciones (`Ok(vec![])`) — el listado se pinta igual, sin badges.
        /// Cualquier OTRO error se propaga. `paths` vacío no llama al wire (nada
        /// que decorar).
        pub(super) async fn plugin_decorate(
            &self,
            paths: &[VPath],
        ) -> Result<Vec<methods::PluginDecorations>, Error> {
            if paths.is_empty() {
                return Ok(Vec::new());
            }
            let client = self.client().await?;
            let params = methods::PluginDecorateParams {
                paths: paths.to_vec(),
            };
            let call =
                client.call::<_, methods::PluginDecorateResult>(methods::PLUGIN_DECORATE, &params);
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(res) => map_decorate_result(res),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// `plugin.column_values` contra el daemon (G3b, ADR 0037): mismo
        /// criterio de fallback que [`Self::plugin_decorate`], pero la forma
        /// "sin datos" es un vector de `None` del tamaño de `paths` (celda
        /// vacía por entrada), no un vector vacío — el caller espera SIEMPRE
        /// una celda por ruta (contrato posicional), incluso cuando la
        /// columna no aplica en absoluto.
        pub(super) async fn plugin_column_values(
            &self,
            plugin_id: &str,
            column_id: &str,
            paths: &[VPath],
        ) -> Result<Vec<Option<String>>, Error> {
            if paths.is_empty() {
                return Ok(Vec::new());
            }
            let client = self.client().await?;
            let params = methods::PluginColumnValuesParams {
                column_id: column_id.to_owned(),
                paths: paths.to_vec(),
                plugin_id: Some(plugin_id.to_owned()),
            };
            let call = client.call::<_, methods::PluginColumnValuesResult>(
                methods::PLUGIN_COLUMN_VALUES,
                &params,
            );
            match tokio::time::timeout(CALL_TIMEOUT, call).await {
                Ok(res) => map_column_values_result(res, paths.len()),
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            }
        }

        /// `plugin.get_config` contra el daemon (0.28.0, G3c). Sin fallback
        /// especial: un daemon N-1 (0.27, que no tiene el handler) responde
        /// `MethodNotFound`, que `call_timed`/`to_taxonomy` degradan a un
        /// error genérico — el caller (TUI/GUI) trata "no pude leer la
        /// config" como "esconde la sección de ajustes de este plugin",
        /// nunca como un crash.
        pub(super) async fn plugin_get_config(
            &self,
            id: &str,
        ) -> Result<methods::PluginGetConfigResult, Error> {
            self.call_timed(
                methods::PLUGIN_GET_CONFIG,
                &methods::PluginGetConfigParams { id: id.to_owned() },
            )
            .await
        }

        /// `plugin.help` contra el daemon (H3e, 0.34.0). Sin fallback
        /// especial: cualquier error —incluido un peer que no implemente el
        /// método y conteste `MethodNotFound`— lo degradan
        /// `call_timed`/`to_taxonomy` a un error de la taxonomía, y el frontend
        /// lo trata como "este plugin no tiene página" y sigue pintando la
        /// ayuda, nunca como un fallo. La ayuda es cosmética.
        ///
        /// El markdown NO está enmascarado (ver [`Backend::plugin_help`]):
        /// se parsea antes de pintarlo, nunca se vuelca en crudo a un terminal
        /// ni a un log.
        pub(super) async fn plugin_help(
            &self,
            id: &str,
        ) -> Result<methods::PluginHelpResult, Error> {
            self.call_timed(
                methods::PLUGIN_HELP,
                &methods::PluginHelpParams { id: id.to_owned() },
            )
            .await
        }

        /// `plugin.set_config` contra el daemon (0.28.0, G3c). Mismo
        /// criterio de fallback que [`Self::plugin_get_config`] — SIN
        /// fallback especial, un error (incluido un daemon N-1 sin el
        /// handler, o un valor rechazado por el daemon) se propaga tal cual.
        pub(super) async fn plugin_set_config(
            &self,
            id: &str,
            key: &str,
            value: &str,
        ) -> Result<(), Error> {
            let _: methods::PluginSetConfigResult = self
                .call_timed(
                    methods::PLUGIN_SET_CONFIG,
                    &methods::PluginSetConfigParams {
                        id: id.to_owned(),
                        key: key.to_owned(),
                        value: value.to_owned(),
                    },
                )
                .await?;
            Ok(())
        }
    }

    /// Traduce el `Result` crudo de `plugin.preview_styled` (G3a, ADR 0037)
    /// al contrato de [`RemoteBackend::plugin_preview_styled`]. Extraída de
    /// esa función SOLO para poder testearla sin socket (construyendo un
    /// [`ClientError::Rpc`] a mano): `METHOD_NOT_FOUND` (-32601) → `Ok(None)`
    /// (mismo destino que "ningún previewer aplica" — el caller cae al
    /// preview plano); cualquier OTRO error va por la taxonomía normal
    /// (`to_taxonomy`, que SÍ mira `rpc.data` para los `APP_ERROR`).
    fn map_styled_preview_result(
        res: Result<methods::PluginPreviewStyledResult, ClientError>,
    ) -> Result<Option<methods::PluginPreviewStyled>, Error> {
        match res {
            Ok(r) => Ok(r.preview),
            Err(ClientError::Rpc(ref rpc))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Ok(None)
            }
            Err(e) => Err(to_taxonomy(e)),
        }
    }

    /// Traduce el `Result` crudo de `plugin.decorate` (G3b, ADR 0037) al
    /// contrato de [`RemoteBackend::plugin_decorate`]: `METHOD_NOT_FOUND` →
    /// `Ok(vec![])` (sin decoraciones, mismo destino que "ningún decorator
    /// consentido"); cualquier OTRO error va por la taxonomía normal.
    fn map_decorate_result(
        res: Result<methods::PluginDecorateResult, ClientError>,
    ) -> Result<Vec<methods::PluginDecorations>, Error> {
        match res {
            Ok(r) => Ok(r.plugins),
            Err(ClientError::Rpc(ref rpc))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(to_taxonomy(e)),
        }
    }

    /// Traduce el `Result` crudo de `plugin.column_values` (G3b, ADR 0037)
    /// al contrato de [`RemoteBackend::plugin_column_values`]:
    /// `METHOD_NOT_FOUND` → `Ok(vec![None; expected_len])` (celda vacía por
    /// entrada, NUNCA un vector vacío — el caller espera SIEMPRE una celda
    /// por ruta, contrato posicional); cualquier OTRO error va por la
    /// taxonomía normal.
    fn map_column_values_result(
        res: Result<methods::PluginColumnValuesResult, ClientError>,
        expected_len: usize,
    ) -> Result<Vec<Option<String>>, Error> {
        match res {
            Ok(r) => Ok(r.values),
            Err(ClientError::Rpc(ref rpc))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Ok(vec![None; expected_len])
            }
            Err(e) => Err(to_taxonomy(e)),
        }
    }

    /// Enruta UN lote de un feed vivo (`search.hits`, `compare.rows`) a su
    /// Task por `task_id`. Si el route existe, envía; `Closed` (el frontend
    /// soltó su `rx`) retira el route; `Full` descarta el lote con aviso
    /// (backpressure: el frontend va por detrás — estos lotes son un feed de
    /// Qué hacer con un lote que no cabe en el buffer del consumidor.
    ///
    /// La diferencia no es de estilo: depende de para qué sirven los lotes.
    #[derive(Clone, Copy)]
    enum OnFull {
        /// Descartar el lote y seguir. Los hits de una búsqueda y las filas de
        /// una comparación son PINTURA: perder un lote empobrece una lista que
        /// nadie va a usar para escribir, y cerrar el feed entero castigaría
        /// más de lo que protege.
        DropBatch,
        /// Cerrar el feed. Los pasos de un plan de sincronización NO son
        /// pintura: son las operaciones que el `plan_hash` va a ejecutar, y
        /// entre ellas hay `DeleteTree` y `Overwrite`. Un lote descartado en
        /// silencio con el cierre entregado detrás dejaría a un humano
        /// aprobando un hash que cubre pasos que nunca vio — que es exactamente
        /// lo que este diseño existe para impedir. Cerrar el feed hace que el
        /// `sync.plan_done` no llegue, y sin él no hay hash con el que aprobar
        /// nada: se falla del lado seguro.
        ///
        /// (El brazo EMBEBIDO no tiene este problema: usa `send().await`, o sea
        /// contrapresión de verdad, y no pierde un paso.)
        CloseFeed,
    }

    /// UI, no dato autoritativo). Sin route todavía (carrera de arranque), lo
    /// retiene en `pending` acotado para que el método que lo lanzó lo drene
    /// al registrar; `task_id` desconocido con `pending` lleno = descarte con
    /// traza (un daemon no debería emitir lotes de Tasks que no lanzamos).
    ///
    /// `feed` es solo la etiqueta de las trazas. `on_full` decide qué pasa
    /// cuando el consumidor no drena, que es donde los feeds DEJAN de parecerse.
    fn route_batch<T>(
        routes: &Mutex<BatchRoutes<T>>,
        id: u64,
        batch: T,
        feed: &'static str,
        on_full: OnFull,
    ) {
        let mut sr = routes.lock().expect("batch routes lock sano");
        if let Some(tx) = sr.routes.get(&id) {
            match tx.try_send(batch) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // El frontend soltó su Receiver: el route ya no sirve.
                    sr.routes.remove(&id);
                }
                Err(mpsc::error::TrySendError::Full(_)) => match on_full {
                    OnFull::DropBatch => tracing::warn!(
                        task_id = id,
                        feed,
                        "buffer del cliente lleno, lote descartado (backpressure)"
                    ),
                    OnFull::CloseFeed => {
                        tracing::warn!(
                            task_id = id,
                            feed,
                            "buffer del cliente lleno: se CIERRA el feed en vez de \
                             descartar el lote"
                        );
                        // Soltar el sender cierra el `rx` del frontend. Lo que
                        // venga detrás —incluido el `sync.plan_done`— ya no se
                        // entrega, así que el cliente se queda sin `plan_hash` y
                        // no puede aprobar un plan que vio incompleto.
                        sr.routes.remove(&id);
                        sr.pending.remove(&id);
                    }
                },
            }
        } else if sr.pending_len() < BATCH_PENDING_CAP {
            sr.pending.entry(id).or_default().push(batch);
        } else {
            tracing::debug!(
                task_id = id,
                feed,
                "lote sin route y pending lleno: descartado"
            );
        }
    }

    /// Registra el route de un feed recién lanzado y devuelve su `rx`.
    ///
    /// Orden ANTI-CARRERA: el route se registra ANTES de que puedan llegar más
    /// lotes. La bomba (otra task) puede haber enrutado ya lotes que
    /// adelantaron a la respuesta del método —el frame puede salir del daemon
    /// antes que la respuesta de `fs.search`/`fs.compare`, y en el cliente la
    /// bomba y la llamada corren en paralelo—: esos lotes se quedaron en
    /// `pending`. El registro (drenar `pending` + insertar el route) es
    /// ATÓMICO bajo el lock, así que ni un lote se pierde entre ambos pasos.
    /// Es el mismo patrón con el que `own_task` cierra la carrera del terminal
    /// adelantado vía el anillo `finished`.
    ///
    /// Si el TERMINAL se adelantó al registro, `route` no pudo programar la
    /// retirada (aún no había route): la programa aquí.
    fn register_route<T: Send + 'static>(
        inner: &Arc<Inner>,
        id: u64,
        feed: &'static str,
        sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
    ) -> mpsc::Receiver<T> {
        let (tx, rx) = mpsc::channel::<T>(BATCH_BUF);
        let (already_terminal, discarded) = {
            let mut sr = sel(inner).lock().expect("batch routes lock sano");
            // Drena los lotes que se adelantaron al registro (en orden). El
            // buffer se dimensiona para absorber el arranque; si aun así se
            // llenara, un lote de UI se pierde (honesto). El log va DESPUÉS de
            // soltar el guard: este lock lo toma también la bomba (ruta
            // caliente) y no debe esperar por un `tracing::warn!`.
            let mut discarded = 0usize;
            if let Some(early) = sr.pending.remove(&id) {
                for batch in early {
                    if tx.try_send(batch).is_err() {
                        discarded += 1;
                    }
                }
            }
            sr.routes.insert(id, tx);
            (sr.terminated.contains(&id), discarded)
        };
        if discarded > 0 {
            tracing::warn!(
                task_id = id,
                discarded,
                feed,
                "lotes de arranque descartados (buffer del cliente lleno)"
            );
        }
        if already_terminal {
            schedule_route_removal(inner, id, sel);
        }
        rx
    }

    /// Programa la retirada del route de un feed terminal tras
    /// [`BATCH_ROUTE_GRACE`]. Sostiene un [`Weak`] (no mantiene vivo a
    /// `Inner`): si el backend ya murió, no hay nada que limpiar. Al retirar el
    /// sender, el `rx` del frontend se cierra (fin del stream de lotes).
    ///
    /// `sel` elige el mapa del feed dentro de `Inner` — un puntero a función,
    /// para que la task de gracia no capture nada más que el `Weak` y el id.
    fn schedule_route_removal<T: Send + 'static>(
        inner: &Arc<Inner>,
        id: u64,
        sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
    ) {
        let weak = Arc::downgrade(inner);
        tokio::spawn(async move {
            tokio::time::sleep(BATCH_ROUTE_GRACE).await;
            if let Some(inner) = weak.upgrade() {
                let mut sr = sel(&inner).lock().expect("batch routes lock sano");
                sr.routes.remove(&id);
                sr.pending.remove(&id);
                sr.terminated.remove(&id);
            }
        });
    }

    /// Bomba vitalicia de notificaciones (M2 del rust-reviewer). Sostiene
    /// un [`Weak`]: en el estado estable (bloqueada en `recv().await`) NO
    /// mantiene viva a `Inner`, así que cuando el último `RemoteBackend`
    /// externo se suelta, `Inner` se libera, el `Client` interno cierra la
    /// conexión, `recv()` devuelve `None` y la bomba SALE — sin ciclo de
    /// Arc ni reconexión eterna.
    #[allow(clippy::too_many_lines)] // tabla de despacho notif→destino + reconexión
    async fn pump_loop(
        weak: Weak<Inner>,
        mut notifications: mpsc::UnboundedReceiver<norte_proto::wire::Notification>,
    ) {
        loop {
            // Consumo: NUNCA se retiene un Arc a través del `recv().await`.
            while let Some(n) = notifications.recv().await {
                // Aprobación de policy pendiente (M3-3b T5): al frontend.
                if n.method == methods::POLICY_APPROVAL_REQUIRED {
                    // Malformada = descartada CON traza (m3 del review): el
                    // agente esperará su TTL y alguien debe poder saber por qué.
                    let Some(params) = n.params else {
                        tracing::warn!("policy.approval_required sin params: descartada");
                        continue;
                    };
                    let req = match serde_json::from_value::<PolicyApprovalRequired>(params) {
                        Ok(req) => req,
                        Err(e) => {
                            tracing::warn!(error = %e, "policy.approval_required malformada");
                            continue;
                        }
                    };
                    let Some(inner) = weak.upgrade() else { return };
                    inner.push_approval(req);
                    continue;
                }
                // Aviso de sesión degradada (#44): al frontend. Malformada =
                // descartada CON traza (mismo trato que la aprobación).
                if n.method == methods::CONNECTION_DEGRADED {
                    let Some(params) = n.params else {
                        tracing::warn!("connection.degraded sin params: descartada");
                        continue;
                    };
                    let d = match serde_json::from_value::<ConnectionDegraded>(params) {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!(error = %e, "connection.degraded malformada");
                            continue;
                        }
                    };
                    let Some(inner) = weak.upgrade() else { return };
                    inner.push_degraded(d);
                    continue;
                }
                // El daemon se va (0.46.0). Lo único que hay que quedarse es
                // si volver, y hay que quedárselo AQUÍ: cuando la conexión se
                // cierre no habrá forma de distinguir un relevo de una parada.
                if n.method == methods::DAEMON_GOING_AWAY {
                    let volver = n
                        .params
                        .and_then(|p| serde_json::from_value::<methods::DaemonGoingAway>(p).ok())
                        .is_some_and(|g| g.reconnect);
                    let Some(inner) = weak.upgrade() else { return };
                    inner
                        .handover_expected
                        .store(volver, std::sync::atomic::Ordering::SeqCst);
                    tracing::info!(reconnect = volver, "el daemon avisa de que se va");
                    continue;
                }
                // Lote de hits de una búsqueda viva (live search T5): al `rx`
                // de su `task_id`. Malformado = descartado con traza.
                if n.method == methods::SEARCH_HITS {
                    let Some(params) = n.params else {
                        tracing::debug!("search.hits sin params: descartada");
                        continue;
                    };
                    let hits = match serde_json::from_value::<SearchHits>(params) {
                        Ok(hits) => hits,
                        Err(e) => {
                            tracing::debug!(error = %e, "search.hits malformada: descartada");
                            continue;
                        }
                    };
                    let Some(inner) = weak.upgrade() else { return };
                    let id = hits.task_id.get();
                    route_batch(
                        &inner.search_routes,
                        id,
                        hits,
                        methods::SEARCH_HITS,
                        OnFull::DropBatch,
                    );
                    continue;
                }
                // Lote de filas de una comparación viva (0.39.0): mismo trato.
                if n.method == methods::COMPARE_ROWS {
                    let Some(params) = n.params else {
                        tracing::debug!("compare.rows sin params: descartada");
                        continue;
                    };
                    let rows = match serde_json::from_value::<CompareRowsBatch>(params) {
                        Ok(rows) => rows,
                        Err(e) => {
                            tracing::debug!(error = %e, "compare.rows malformada: descartada");
                            continue;
                        }
                    };
                    let Some(inner) = weak.upgrade() else { return };
                    let id = rows.task_id.get();
                    route_batch(
                        &inner.compare_routes,
                        id,
                        rows,
                        methods::COMPARE_ROWS,
                        OnFull::DropBatch,
                    );
                    continue;
                }
                // Los dos eventos de un plan vivo (0.40.0) van al MISMO `rx`,
                // envueltos en el mismo enum que devuelve el brazo embebido:
                // el orden «pasos* y después el cierre» es la cola de ese
                // canal, no una carrera entre dos mapas. Un evento malformado
                // se descarta con traza, como los otros dos feeds.
                if n.method == methods::SYNC_STEPS || n.method == methods::SYNC_PLAN_DONE {
                    let done = n.method == methods::SYNC_PLAN_DONE;
                    let Some(params) = n.params else {
                        tracing::debug!(method = %n.method, "evento de sync sin params: descartado");
                        continue;
                    };
                    let event = if done {
                        serde_json::from_value::<methods::SyncPlanDone>(params)
                            .map(crate::sync::SyncPlanEvent::Done)
                    } else {
                        serde_json::from_value::<methods::SyncStepsBatch>(params)
                            .map(crate::sync::SyncPlanEvent::Steps)
                    };
                    let event = match event {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::debug!(error = %e, "evento de sync malformado: descartado");
                            continue;
                        }
                    };
                    let id = match &event {
                        crate::sync::SyncPlanEvent::Steps(b) => b.task_id.get(),
                        crate::sync::SyncPlanEvent::Done(d) => d.task_id.get(),
                    };
                    let Some(inner) = weak.upgrade() else { return };
                    let feed = if done {
                        methods::SYNC_PLAN_DONE
                    } else {
                        methods::SYNC_STEPS
                    };
                    route_batch(&inner.sync_routes, id, event, feed, OnFull::CloseFeed);
                    continue;
                }
                if n.method != methods::TASK_PROGRESS {
                    continue;
                }
                let Some(params) = n.params else { continue };
                let Ok(snapshot) = serde_json::from_value::<TaskProgress>(params) else {
                    continue;
                };
                let Some(inner) = weak.upgrade() else { return };
                // Wrapper EFÍMERO solo para reusar `route` (&self) — jamás
                // llama take_*, así que los tres `None` son correctos.
                RemoteBackend {
                    inner,
                    foreign_rx: Mutex::new(None),
                    events_rx: Mutex::new(None),
                    approvals_rx: Mutex::new(None),
                    degraded_rx: Mutex::new(None),
                }
                .route(snapshot);
            }
            // Conexión muerta. Si ya no queda backend externo, salir.
            let Some(inner) = weak.upgrade() else { return };
            // Wrapper EFÍMERO (jamás llama take_*): los tres `None` son correctos.
            let backend = RemoteBackend {
                inner,
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
            };
            *backend.inner.client.write().await = None;
            // Los feeds vivos (hits de una búsqueda, filas de una comparación)
            // NO sobreviven a la reconexión: la bomba del daemon apuntaba al
            // `conn_id` viejo (muerto). Suelta todos los routes → los `rx` en
            // vuelo se cierran (el frontend infiere el fin por el terminal de
            // la Task, reconciliado por el resync de `task.list`).
            {
                let mut sr = backend
                    .inner
                    .search_routes
                    .lock()
                    .expect("search_routes lock sano");
                sr.clear();
            }
            {
                let mut cr = backend
                    .inner
                    .compare_routes
                    .lock()
                    .expect("compare_routes lock sano");
                cr.clear();
            }
            {
                let mut sr = backend
                    .inner
                    .sync_routes
                    .lock()
                    .expect("sync_routes lock sano");
                sr.clear();
            }
            let _ = backend.inner.events_tx.send(ConnEvent::Lost);
            drop(backend);
            // Reconexión con backoff (NO re-arranca el daemon: M3). Si el
            // daemon es incompatible de versión, reintentar es fútil: se
            // abandona (el aviso Lost ya se envió).
            let mut attempt = 0usize;
            notifications = loop {
                let delay = RECONNECT_BACKOFF_MS[attempt.min(RECONNECT_BACKOFF_MS.len() - 1)];
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let Some(inner) = weak.upgrade() else { return };
                // Wrapper EFÍMERO (jamás llama take_*): los tres `None` son correctos.
                let backend = RemoteBackend {
                    inner,
                    foreign_rx: Mutex::new(None),
                    events_rx: Mutex::new(None),
                    approvals_rx: Mutex::new(None),
                    degraded_rx: Mutex::new(None),
                };
                // El permiso de arranque se GASTA aquí, con `swap`: sea cual
                // sea el resultado, este intento es el que lo consume. Volver a
                // intentarlo con él puesto sería resucitar un daemon parado,
                // solo que más tarde.
                let relevo = backend
                    .inner
                    .handover_expected
                    .swap(false, std::sync::atomic::Ordering::SeqCst);
                match backend.establish(relevo).await {
                    Ok(rx) => {
                        let _ = backend.inner.events_tx.send(ConnEvent::Restored);
                        break rx;
                    }
                    // Daemon incompatible de versión: reintentar es fútil.
                    Err(e) if crate::daemon::is_version_mismatch(&e) => return,
                    Err(_) => {}
                }
            };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_inner() -> Arc<Inner> {
            test_inner_en(PathBuf::from("/nonexistent/test.sock"))
        }

        fn test_inner_en(socket: PathBuf) -> Arc<Inner> {
            let (foreign_tx, _fr) = mpsc::unbounded_channel();
            let (events_tx, _er) = mpsc::unbounded_channel();
            let (approvals_tx, _ar) = mpsc::unbounded_channel();
            let (degraded_tx, _dr) = mpsc::unbounded_channel();
            Arc::new(Inner {
                socket,
                spawn_cmd: None,
                client_info: ClientInfo {
                    name: "test".into(),
                    version: "0".into(),
                },
                agent_session: None,
                handover_expected: std::sync::atomic::AtomicBool::new(false),
                client: tokio::sync::RwLock::new(None),
                watches: Mutex::new(HashMap::new()),
                finished: Mutex::new(std::collections::VecDeque::new()),
                foreign_tx,
                events_tx,
                approvals_tx,
                degraded_tx,
                seen_approvals: Mutex::new(std::collections::HashSet::new()),
                search_routes: Mutex::new(BatchRoutes::default()),
                compare_routes: Mutex::new(BatchRoutes::default()),
                sync_routes: Mutex::new(BatchRoutes::default()),
            })
        }

        /// Un daemon de mentira que acepta el handshake y RECHAZA
        /// `task.list` — el estado exacto de #181, que ningún daemon de
        /// verdad sabe montar.
        ///
        /// Devuelve al soltar el listener; el test lo mantiene vivo por su
        /// `JoinHandle`.
        fn stub_que_rechaza_task_list(socket: &std::path::Path) -> tokio::task::JoinHandle<()> {
            let listener = tokio::net::UnixListener::bind(socket).expect("bind del stub");
            tokio::spawn(async move {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let mut decoder = norte_proto::wire::FrameDecoder::new();
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = match tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if decoder.push(&buf[..n]).is_err() {
                        return;
                    }
                    while let Some(frame) = decoder.next_frame() {
                        let Ok(req) = serde_json::from_slice::<norte_proto::wire::Request>(&frame)
                        else {
                            continue;
                        };
                        let resp = if req.method == norte_proto::methods::INITIALIZE {
                            norte_proto::wire::Response::ok(
                                req.id.clone(),
                                serde_json::to_value(norte_proto::methods::InitializeResult {
                                    server_info: norte_proto::methods::ServerInfo {
                                        name: "stub".into(),
                                        version: "0".into(),
                                    },
                                    protocol_version: norte_proto::methods::PROTOCOL_VERSION.into(),
                                    encodings: vec!["json".into()],
                                })
                                .expect("json"),
                            )
                        } else {
                            // `task.list` (y cualquier otra cosa) se rechaza:
                            // es lo que #181 necesita que pase DESPUÉS de que
                            // `establish` haya publicado el cliente.
                            norte_proto::wire::Response::err(
                                Some(req.id.clone()),
                                norte_proto::wire::RpcError::protocol(
                                    norte_proto::wire::codes::INTERNAL_ERROR,
                                    "el stub rechaza esto a propósito",
                                ),
                            )
                        };
                        let Ok(bytes) = norte_proto::wire::encode_frame(&resp) else {
                            return;
                        };
                        if tokio::io::AsyncWriteExt::write_all(&mut conn, &bytes)
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            })
        }

        /// #181: un resync que falla NO puede dejar publicado el cliente.
        ///
        /// Si se queda, el llamante habla por una conexión cuyo receptor de
        /// notificaciones murió con el marco de `establish`: no se enruta
        /// nada, `task.progress` incluido, y `TaskRef::join()` —que no tiene
        /// plazo— espera un terminal que ya no puede llegar. Para siempre.
        #[tokio::test]
        async fn un_resync_fallido_no_deja_el_cliente_publicado() {
            let dir = tempfile::tempdir().expect("tempdir");
            let socket = dir.path().join("stub.sock");
            let _stub = stub_que_rechaza_task_list(&socket);

            let inner = test_inner_en(socket);
            let backend = RemoteBackend {
                inner: Arc::clone(&inner),
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
            };

            let r = backend.establish(false).await;
            assert!(r.is_err(), "el resync lo rechaza el stub");
            assert!(
                inner.client.read().await.is_none(),
                "y el hueco queda VACÍO: con un cliente ahí, nadie enruta y un join() cuelga para siempre"
            );
        }

        /// El feed de `sync.plan` se CIERRA cuando el consumidor no drena, en
        /// vez de descartar el lote como hacen los otros dos.
        ///
        /// Es la diferencia que hace segura la aprobación: un lote de pasos
        /// descartado en silencio, con el `sync.plan_done` entregado detrás,
        /// dejaría a un humano aprobando un `plan_hash` que cubre `DeleteTree` y
        /// `Overwrite` que nunca vio en pantalla. Cerrando el feed no llega
        /// cierre, y sin cierre no hay hash con el que aprobar nada.
        #[test]
        fn el_feed_de_sync_se_cierra_en_vez_de_perder_un_lote() {
            let routes: Mutex<BatchRoutes<u32>> = Mutex::new(BatchRoutes::default());
            let (tx, mut rx) = mpsc::channel::<u32>(1);
            routes.lock().expect("lock").routes.insert(7, tx);

            route_batch(&routes, 7, 1, "sync.steps", OnFull::CloseFeed);
            route_batch(&routes, 7, 2, "sync.steps", OnFull::CloseFeed); // no cabe
            // El route se retiró: el `rx` ve lo que sí entró y después el fin.
            assert!(
                !routes.lock().expect("lock").routes.contains_key(&7),
                "el feed tenía que cerrarse"
            );
            assert_eq!(rx.try_recv(), Ok(1));
            assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected));
        }

        /// Y el de una búsqueda o una comparación NO: ahí un lote es pintura, y
        /// cerrar el feed entero castigaría más de lo que protege.
        #[test]
        fn el_feed_de_una_busqueda_descarta_el_lote_y_sigue() {
            let routes: Mutex<BatchRoutes<u32>> = Mutex::new(BatchRoutes::default());
            let (tx, mut rx) = mpsc::channel::<u32>(1);
            routes.lock().expect("lock").routes.insert(7, tx);

            route_batch(&routes, 7, 1, "search.hits", OnFull::DropBatch);
            route_batch(&routes, 7, 2, "search.hits", OnFull::DropBatch); // se pierde
            assert!(
                routes.lock().expect("lock").routes.contains_key(&7),
                "el feed sigue vivo"
            );
            assert_eq!(rx.try_recv(), Ok(1));
            assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        }

        fn progress(id: u64, kind: TaskKind, state: TaskState) -> TaskProgress {
            TaskProgress {
                task_id: TaskId::new(id),
                kind,
                state,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 0,
                entries_total: None,
                current: None,
            }
        }

        fn backend_for(inner: Arc<Inner>) -> RemoteBackend {
            RemoteBackend {
                inner,
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
            }
        }

        /// FIX RAÍZ del review: `route` marca `terminated` SOLO para terminales
        /// de búsqueda. Un terminal de copy/move/delete/list ajeno jamás entra
        /// (si lo hiciera, ≥256 de ellos entre el terminal de una búsqueda y el
        /// registro de su route dispararían el `clear` y perderían la marca →
        /// route colgado). Y un terminal de SEARCH sí se recuerda, para que
        /// `search` agende la retirada aunque el terminal se le adelante.
        #[test]
        fn route_solo_cuenta_terminales_de_busqueda() {
            let inner = test_inner();
            let backend = backend_for(Arc::clone(&inner));

            backend.route(progress(7, TaskKind::Copy, TaskState::Completed));
            backend.route(progress(8, TaskKind::Delete, TaskState::Completed));
            assert!(
                inner
                    .search_routes
                    .lock()
                    .expect("lock")
                    .terminated
                    .is_empty(),
                "los terminales no-search jamás entran en `terminated`"
            );

            backend.route(progress(9, TaskKind::Search, TaskState::Completed));
            assert!(
                inner
                    .search_routes
                    .lock()
                    .expect("lock")
                    .terminated
                    .contains(&9),
                "el terminal de búsqueda queda marcado para que `search` lo vea"
            );
        }

        /// H2/H3: `mark_terminated` solo devuelve `true` en la inserción nueva,
        /// así la retirada se agenda UNA vez (un terminal duplicado —bomba +
        /// resync— no vuelve a spawnear la task de gracia).
        #[test]
        fn mark_terminated_solo_true_en_insercion_nueva() {
            let mut sr = BatchRoutes::<SearchHits>::default();
            assert!(sr.mark_terminated(1), "primera vez: recién insertada");
            assert!(!sr.mark_terminated(1), "repetida: no reagenda");
            assert!(sr.mark_terminated(2), "otro id: recién insertado");
        }

        /// #44: la seam de `connection.degraded` refleja la de aprobaciones —
        /// `push_degraded` (lo que hace el arm de la bomba) entrega en el
        /// receptor que `take_degraded` se lleva UNA vez.
        #[test]
        fn degraded_push_llega_a_take_degraded() {
            let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();
            let (foreign_tx, _fr) = mpsc::unbounded_channel();
            let (events_tx, _er) = mpsc::unbounded_channel();
            let (approvals_tx, _ar) = mpsc::unbounded_channel();
            let inner = Arc::new(Inner {
                socket: PathBuf::from("/nonexistent/test.sock"),
                spawn_cmd: None,
                client_info: ClientInfo {
                    name: "test".into(),
                    version: "0".into(),
                },
                agent_session: None,
                handover_expected: std::sync::atomic::AtomicBool::new(false),
                client: tokio::sync::RwLock::new(None),
                watches: Mutex::new(HashMap::new()),
                finished: Mutex::new(std::collections::VecDeque::new()),
                foreign_tx,
                events_tx,
                approvals_tx,
                degraded_tx,
                seen_approvals: Mutex::new(std::collections::HashSet::new()),
                search_routes: Mutex::new(BatchRoutes::default()),
                compare_routes: Mutex::new(BatchRoutes::default()),
                sync_routes: Mutex::new(BatchRoutes::default()),
            });
            let backend = RemoteBackend {
                inner: Arc::clone(&inner),
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(Some(degraded_rx)),
            };

            inner.push_degraded(ConnectionDegraded {
                scheme: "ftp".into(),
                host: "example.test".into(),
                reason: "tls-auth-rejected".into(),
                detail: None,
            });

            let mut rx = backend.take_degraded().expect("primer dueño se lo lleva");
            let got = rx.try_recv().expect("el aviso llegó al receptor");
            assert_eq!(got.scheme, "ftp");
            assert_eq!(got.reason, "tls-auth-rejected");
            // Uno solo: un segundo `take_*` ve `None` (como los otros canales).
            assert!(
                backend.take_degraded().is_none(),
                "el receptor es one-shot, igual que take_approvals"
            );
        }

        /// G3a (ADR 0037): `METHOD_NOT_FOUND` (-32601) de `plugin.preview_styled`
        /// se traduce a `Ok(None)` — un daemon 0.27 sin este handler aún
        /// cableado degrada EXACTAMENTE como "ningún previewer aplica"; el
        /// caller (frontend) cae al preview plano. Construye el `ClientError`
        /// a mano (sin socket): es el trigger REAL dentro de la ventana 0.27,
        /// distinto de `VERSION_MISMATCH` (que ni deja llamar).
        #[test]
        fn plugin_preview_styled_method_not_found_es_none() {
            let err = ClientError::Rpc(norte_proto::wire::RpcError::protocol(
                norte_proto::wire::codes::METHOD_NOT_FOUND,
                "unknown method: plugin.preview_styled",
            ));
            let got = map_styled_preview_result(Err(err)).expect("METHOD_NOT_FOUND no es error");
            assert_eq!(
                got, None,
                "cae a Ok(None), como si ningún previewer aplicara"
            );
        }

        /// Un `Ok` con `preview: Some(..)` pasa tal cual.
        #[test]
        fn plugin_preview_styled_ok_pasa_la_preview() {
            let preview = methods::PluginPreviewStyled {
                plugin_id: "org.norte.demo".into(),
                plugin_name: "Demo".into(),
                lines: vec![vec![methods::SpanWire {
                    text: "hola".into(),
                    role: None,
                    fg: None,
                }]],
                lossy: false,
            };
            let got = map_styled_preview_result(Ok(methods::PluginPreviewStyledResult {
                preview: Some(preview.clone()),
            }))
            .expect("Ok pasa");
            assert_eq!(got, Some(preview));
        }

        /// Un fallo REAL (no `METHOD_NOT_FOUND`) se propaga vía `to_taxonomy`
        /// — jamás se confunde en silencio con "no hay handler todavía".
        #[test]
        fn plugin_preview_styled_otro_error_se_propaga() {
            let err = ClientError::Rpc(norte_proto::wire::RpcError::from(Error::Internal {
                panic: false,
            }));
            let got = map_styled_preview_result(Err(err));
            assert!(
                matches!(got, Err(Error::Internal { panic: false })),
                "un fallo real se propaga, no se confunde con METHOD_NOT_FOUND: {got:?}"
            );
        }

        /// V3.5 (encoding-auditor finding deferred from V3): `Volume::label`
        /// is `Option<Vec<u8>>` end to end now, so a non-UTF-8 label crosses
        /// `volume_to_proto` byte-for-byte — no `String` sits between the
        /// core type and the wire type to lose it lossily or refuse it.
        #[test]
        fn volume_to_proto_preserves_a_non_utf8_label() {
            let hostile = vec![0xFF, 0xFE, b'a'];
            let v = crate::volumes::Volume {
                mount: VPath::parse("file:///media/usb").expect("vpath de test"),
                label: Some(hostile.clone()),
                fs_type: "vfat".into(),
                kind: crate::volumes::VolumeKind::Removable,
                total_bytes: None,
                free_bytes: None,
                read_only: false,
            };
            let proto = crate::backend::volume_to_proto(v);
            assert_eq!(proto.label, Some(hostile), "los bytes cruzan sin cambiar");
        }
    }
}

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
