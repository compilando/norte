//! El backend REMOTO: los métodos tipados que un frontend usa de verdad.
//!
//! Por debajo hay un [`crate::rpc::Client`] y, por encima, la misma superficie
//! que el backend embebido del core presenta a un frontend. Lo que vive aquí y
//! no en el `Client` es todo lo que hace falta para que hablar con un daemon
//! se parezca a llamar a una función: reconexión con resincronización, un
//! registro de tasks, el enrutado de los lotes de búsqueda/comparación/
//! sincronización, y el fan-out de eventos de conexión y de aprobaciones.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt as _;
use norte_proto::methods::{
    self, ClientInfo, CompareRowsBatch, ConnectionDegraded, FsCapabilitiesParams,
    FsCapabilitiesResult, FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams,
    FsReadParams, FsReadResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult, PolicyPendingResult,
    SearchHits, TaskCancelParams, TaskCancelResult, TaskListParams, TaskListResult,
};
use norte_proto::{
    ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskKind, TaskProgress, TaskState,
    VPath,
};
use tokio::sync::{mpsc, watch};

mod calls;
mod paging;
mod routes;

use calls::{CALL_TIMEOUT, CancelOnAbandon, to_taxonomy};
use paging::{LIST_PAGE, PageState, page_step};
use routes::{BatchRoutes, OnFull, register_route, route_batch, schedule_route_removal};

use crate::rpc::{Client, ClientError};
use crate::task::{RemoteTask, RemoteTaskCanceller};
use crate::types::{AI_CALL_TIMEOUT, ConnEvent, EntryStream, SyncPlanEvent, TransferOptions};

/// Backoff de reconexión (se recorre y se queda en el último).
const RECONNECT_BACKOFF_MS: &[u64] = &[250, 500, 1000, 2000, 5000];

/// Cuánto vale un permiso de arranque tras un `daemon.going_away`.
///
/// Tiene que cubrir lo que tarda el daemon viejo en SALIR DEL PROCESO —no
/// en contestar—, porque hasta entonces retiene el lock exclusivo de
/// `journal.db` y el reemplazo aborta al abrirlo. Treinta segundos son de
/// sobra para eso, y siguen siendo poco para lo que la regla protege: que
/// un daemon que el usuario paró no resucite más tarde.
const HANDOVER_SPAWN_WINDOW: Duration = Duration::from_secs(30);

/// ¿Sigue vivo el permiso de arranque?
///
/// Función aparte y pura para poder probar la caducidad sin gastar treinta
/// segundos de reloj.
fn spawn_allowed(hasta: Option<std::time::Instant>, ahora: std::time::Instant) -> bool {
    hasta.is_some_and(|d| ahora < d)
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
    /// **Caduca por TIEMPO, no por intentos**, y la diferencia es la que hay
    /// entre que el relevo funcione y que no.
    ///
    /// La primera versión lo gastaba en el primer intento de reconexión,
    /// que llega a los 250 ms. Para entonces el daemon viejo TODAVÍA no ha
    /// salido del proceso, y con él retiene el lock exclusivo de
    /// `journal.db` — que es lo primero que abre el reemplazo, y aborta si
    /// no puede. Así que el relevo moría en ese lock, el permiso se iba con
    /// el intento fallido, y ningún intento posterior volvía a arrancar
    /// nada: sesión muerta para siempre tras una actualización rutinaria,
    /// que es exactamente el fallo que esta feature existe para evitar.
    /// Lo encontró la revisión de seguridad de esta fase.
    ///
    /// Lo que la regla de no-resucitar quiere es que la licencia no
    /// sobreviva «hasta la semana que viene», y eso es una cota de TIEMPO.
    /// Dentro de la ventana se arranca tantas veces como haga falta;
    /// pasada, «el daemon no está» vuelve a significar lo de siempre.
    handover_until: Mutex<Option<std::time::Instant>>,
    client: tokio::sync::RwLock<Option<Arc<Client>>>,
    watches: Mutex<HashMap<u64, watch::Sender<TaskProgress>>>,
    /// Desenlaces vistos SIN watch receptor (el broadcast terminal
    /// puede adelantar a la response del método que creó la task):
    /// `own_task` los consulta para no esperar un progreso que ya pasó.
    finished: Mutex<std::collections::VecDeque<TaskProgress>>,
    foreign_tx: mpsc::UnboundedSender<RemoteTask>,
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
    sync_routes: Mutex<BatchRoutes<SyncPlanEvent>>,
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
    foreign_rx: Mutex<Option<mpsc::UnboundedReceiver<RemoteTask>>>,
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
                handover_until: Mutex::new(None),
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
        let _ = self.inner.foreign_tx.send(RemoteTask::new(
            id,
            rx,
            RemoteTaskCanceller::new(self.clone(), id),
        ));
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
    ///
    /// **También lo usan las LECTURAS** (`fs.list`, `fs.stat`, `fs.read`,
    /// `fs.capabilities`) desde #248, y ahí no protege un efecto a medias
    /// —una lectura no deja ninguno— sino la CONEXIÓN: `serve_connection`
    /// despacha en serie, así que una lectura abandonada —el presupuesto
    /// de cinco segundos de la sesión (#235), un future dropeado— dejaba a
    /// todas las peticiones siguientes esperando detrás de ella, cada una
    /// muriendo en su propio [`CALL_TIMEOUT`] de 30 s. La TUI arrancaba,
    /// se veía, y no servía para nada sin decirlo.
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn list_stream(
        &self,
        dir: &VPath,
        attrs: Vec<String>,
    ) -> Result<(EntryStream, Option<u64>), Error> {
        // Primera página síncrona: un `NotFound`/`TypeMismatch` sale en el
        // Result, no como primer item del stream (paridad con el embebido).
        let first: FsListResult = self
            .call_timed_guarded(
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

    /// Las capacidades de la localización, sin el catálogo de atributos.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
        Ok(self.capabilities_full(path).await?.capabilities)
    }

    /// El catálogo de atributos que la localización sabe reportar (#117).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        Ok(self.capabilities_full(path).await?.attrs)
    }

    /// `fs.capabilities` completo: capacidades MÁS catálogo de atributos, en
    /// un solo viaje (los dos accesores de arriba salen de aquí).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn capabilities_full(&self, path: &VPath) -> Result<FsCapabilitiesResult, Error> {
        self.call_timed_guarded(
            methods::FS_CAPABILITIES,
            &FsCapabilitiesParams { path: path.clone() },
        )
        .await
    }

    /// `fs.stat` de una entrada, pidiendo los atributos indicados.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn stat(&self, path: &VPath, attrs: Vec<String>) -> Result<Entry, Error> {
        let r: FsStatResult = self
            .call_timed_guarded(
                methods::FS_STAT,
                &FsStatParams {
                    path: path.clone(),
                    attrs,
                },
            )
            .await?;
        Ok(r.entry)
    }

    /// `connection.trust_host_key`: confía en la clave de host que el TOFU
    /// acaba de enseñar (#45, ADR 0015).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn trust_host_key(
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

    /// `fs.read` de un rango de bytes.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn read(&self, path: &VPath, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
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
                .call_timed_guarded(
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

    /// Copia o mueve, según `mover`: una Task del daemon en los dos casos.
    ///
    /// # Errors
    /// Lo que responda el daemon al ENCOLAR (el desenlace llega por progreso).
    pub async fn transfer(
        &self,
        method: &str,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<RemoteTask, Error> {
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

    /// `index.build`: indexa un root como Task.
    ///
    /// # Errors
    /// Lo que responda el daemon al encolar.
    pub async fn index_build(&self, root: &VPath) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::INDEX_BUILD,
                &methods::IndexBuildParams { root: root.clone() },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Index))
    }

    /// `index.query`: búsqueda por nombre contra el índice.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn index_query(
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

    /// `index.embed`: calcula los embeddings de un root como Task (M4-IA-2).
    ///
    /// # Errors
    /// Lo que responda el daemon al encolar.
    pub async fn index_embed(&self, root: &VPath) -> Result<RemoteTask, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn index_search_semantic(
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn ai_rename_plan(
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

    /// `fs.delete` de un lote: papelera o permanente según `mode`.
    ///
    /// # Errors
    /// Lo que responda el daemon al encolar.
    pub async fn delete(&self, path: &VPath, mode: DeleteMode) -> Result<RemoteTask, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn rename_batch_plan(
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[methods::RenamePair],
        plan_hash: &methods::PlanHash,
    ) -> Result<RemoteTask, Error> {
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

    /// Como [`Self::call_timed`], traduciendo `METHOD_NOT_FOUND` a
    /// [`Error::Unsupported`] (#247).
    ///
    /// Es la primitiva de «degrada Y dilo»: contra un daemon más viejo,
    /// «tu daemon no sabe de esto» y un fallo de verdad no son lo mismo, y
    /// un `session.get` que devuelve un error genérico deja al frontend
    /// diciendo que la sesión falló cuando lo que pasa es que no la hay.
    async fn call_no_method_is_unsupported<P, R>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, Error>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let client = self.client().await?;
        match tokio::time::timeout(CALL_TIMEOUT, client.call::<_, R>(method, params)).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `fs.rename_batch_report` (0.36.0): informe del lote. Un daemon N-1
    /// sin el método responde `METHOD_NOT_FOUND` → `Unsupported`, para que
    /// el caller lo distinga de un fallo REAL — mismo criterio que
    /// `policy.undo_report` (#71): el informe es la ÚNICA señal de que un
    /// lote dejó el directorio a medias, y no se degrada en silencio.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn rename_batch_report(
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

    /// `fs.mkdir`, como Task (la mutación pasa por journal y policy igual).
    ///
    /// # Errors
    /// Lo que responda el daemon al encolar.
    pub async fn mkdir(&self, path: &VPath) -> Result<RemoteTask, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn search(
        &self,
        params: FsSearchParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<SearchHits>), Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<CompareRowsBatch>), Error> {
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

    /// `connection.close` (0.49.0, #140): suelta la sesión de esa ruta.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn close_connection(&self, path: &norte_proto::VPath) -> Result<bool, Error> {
        let client = self.client().await?;
        let params = methods::ConnectionCloseParams { path: path.clone() };
        let call =
            client.call::<_, methods::ConnectionCloseResult>(methods::CONNECTION_CLOSE, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map(|r| r.closed).map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `fs.dir_size` (0.49.0, #139): lanza la Task y devuelve su
    /// referencia. Sin canal: lo que hay que escuchar es el progreso, que
    /// ya llega por la suscripción de siempre.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn dir_size(&self, params: methods::FsDirSizeParams) -> Result<RemoteTask, Error> {
        let client = self.client().await?;
        let call = client.call::<_, FsTaskResult>(methods::FS_DIR_SIZE, &params);
        let result: FsTaskResult = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                return Err(Error::Unsupported);
            }
            Ok(res) => res.map_err(to_taxonomy)?,
            Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
        };
        Ok(self.own_task(result.task_id, TaskKind::DirSize))
    }

    /// `archive.pack` (0.50.0, #132).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn pack(&self, params: methods::ArchivePackParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::ARCHIVE_PACK, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Pack))
    }

    /// `archive.test` (0.50.0, #132).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::ARCHIVE_TEST, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::TestArchive))
    }

    /// `archive.test_report` (0.50.0, #132): el informe, cuando la Task ya
    /// ha terminado (o antes, parcial).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn archive_test_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<methods::ArchiveTestResult, Error> {
        self.call_maybe_unknown(
            methods::ARCHIVE_TEST_REPORT,
            &methods::ArchiveTestReportParams { task_id },
        )
        .await
    }

    /// `file.split` (0.50.0, #132).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn split_file(&self, params: methods::FileSplitParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FILE_SPLIT, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Split))
    }

    /// `file.combine` (0.50.0, #132).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FILE_COMBINE, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Combine))
    }

    /// `sync.plan` (0.40.0, ADR 0049): lanza la Task y devuelve el `rx` por
    /// el que la bomba enruta los DOS eventos del plan (`sync.steps` y el
    /// `sync.plan_done` que lo cierra) de ESTE `task_id`. Mismo ciclo de
    /// vida que [`Self::compare`], incluida la carrera de arranque.
    ///
    /// Un daemon N-1 sin el método contesta `METHOD_NOT_FOUND` →
    /// [`Error::Unsupported`], para que el frontend distinga «tu daemon es
    /// más viejo» de un fallo real.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<SyncPlanEvent>), Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn sync_apply(&self, plan_hash: &methods::PlanHash) -> Result<RemoteTask, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn sync_report(&self, task_id: TaskId) -> Result<methods::SyncReportResult, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn undo_session(&self, session: &str) -> Result<RemoteTask, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Result<methods::PolicyUndoReportResult, Error> {
        let client = self.client().await?;
        let params = methods::PolicyUndoReportParams { task_id };
        let call =
            client.call::<_, methods::PolicyUndoReportResult>(methods::POLICY_UNDO_REPORT, &params);
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
    fn own_task(&self, id: TaskId, kind: TaskKind) -> RemoteTask {
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
            return RemoteTask::new(id, rx, RemoteTaskCanceller::new(self.clone(), id));
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
        RemoteTask::new(id, rx, RemoteTaskCanceller::new(self.clone(), id))
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

    /// El canal de tasks AJENAS (las que otro cliente lanzó y este observa).
    /// Es del PRIMER dueño: un clon del backend no debe llamarlo.
    ///
    /// # Panics
    /// Si el estado interno está envenenado por un panic previo.
    pub fn take_foreign_tasks(&self) -> Option<mpsc::UnboundedReceiver<RemoteTask>> {
        self.foreign_rx.lock().expect("foreign_rx lock sano").take()
    }

    /// El canal de eventos de conexión (perdida / restaurada). Del primer
    /// dueño, como el resto de los `take_*`.
    ///
    /// # Panics
    /// Si el estado interno está envenenado por un panic previo.
    pub fn take_conn_events(&self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        self.events_rx.lock().expect("events_rx lock sano").take()
    }

    /// El canal de aprobaciones pendientes de policy. Del primer dueño.
    ///
    /// # Panics
    /// Si el estado interno está envenenado por un panic previo.
    pub fn take_approvals(&self) -> Option<mpsc::UnboundedReceiver<PolicyApprovalRequired>> {
        self.approvals_rx
            .lock()
            .expect("approvals_rx lock sano")
            .take()
    }

    /// Se lleva el receptor de avisos `connection.degraded` (#44). Uno solo
    /// (el primer dueño), como los otros `take_*`.
    ///
    /// # Panics
    /// Si el estado interno está envenenado por un panic previo.
    pub fn take_degraded(&self) -> Option<mpsc::UnboundedReceiver<ConnectionDegraded>> {
        self.degraded_rx
            .lock()
            .expect("degraded_rx lock sano")
            .take()
    }

    /// `policy.decide` contra el daemon (M3-3b T5).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn policy_decide(&self, approval_id: u64, approve: bool) -> Result<(), Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugins_list(&self) -> Result<methods::PluginListResult, Error> {
        self.call_timed(methods::PLUGIN_LIST, &methods::PluginListParams {})
            .await
    }

    /// `host.volumes` contra el daemon (0.37.0, #131). El gate por actor
    /// vive server-side (diseño §C): una conexión de agente ve
    /// [`Error::PolicyDenied`] aquí, no un fallo de transporte.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn volumes(&self, include_pseudo: bool) -> Result<Vec<methods::Volume>, Error> {
        let result: methods::HostVolumesResult = self
            .call_timed(
                methods::HOST_VOLUMES,
                &methods::HostVolumesParams { include_pseudo },
            )
            .await?;
        Ok(result.volumes)
    }

    /// `session.get` contra el daemon (L2): la pantalla y si ESTA conexión
    /// es la dueña.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn session_get(&self) -> Result<(methods::Session, bool), Error> {
        let r: methods::SessionGetResult = self
            .call_no_method_is_unsupported(methods::SESSION_GET, &serde_json::json!({}))
            .await?;
        Ok((r.session, r.owner))
    }

    /// `session.put` contra el daemon (L2): devuelve la revisión NUEVA.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, Error> {
        let r: methods::SessionPutResult = self
            .call_no_method_is_unsupported(
                methods::SESSION_PUT,
                &methods::SessionPutParams {
                    version,
                    revision,
                    body,
                },
            )
            .await?;
        Ok(r.revision)
    }

    /// `plugin.set_approval` contra el daemon (M4-P3).
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugins_set_approval(&self, id: &str, approved: bool) -> Result<(), Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_run_command(
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_preview(
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
    ) -> Result<Option<methods::PluginPreviewStyled>, Error> {
        let client = self.client().await?;
        let params = methods::PluginPreviewStyledParams { path: path.clone() };
        let call = client
            .call::<_, methods::PluginPreviewStyledResult>(methods::PLUGIN_PREVIEW_STYLED, &params);
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_decorate(
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_column_values(
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
        let call = client
            .call::<_, methods::PluginColumnValuesResult>(methods::PLUGIN_COLUMN_VALUES, &params);
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_get_config(
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
    /// El markdown NO está enmascarado (ver `Backend::plugin_help` del core):
    /// se parsea antes de pintarlo, nunca se vuelca en crudo a un terminal
    /// ni a un log.
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_help(&self, id: &str) -> Result<methods::PluginHelpResult, Error> {
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
    ///
    /// # Errors
    /// Lo que responda el daemon.
    pub async fn plugin_set_config(&self, id: &str, key: &str, value: &str) -> Result<(), Error> {
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
                // Malformada = descartada CON traza, y SIN tocar el
                // permiso: «no se entiende» no es «no vuelvas».
                let Some(params) = n.params else {
                    tracing::warn!("daemon.going_away sin params: descartada");
                    continue;
                };
                let aviso = match serde_json::from_value::<methods::DaemonGoingAway>(params) {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::warn!(error = %e, "daemon.going_away malformada");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                // Un backend de AGENTE no arranca daemons: su `spawn_cmd`
                // es `None` por construcción, así que no se le guarda un
                // permiso que no puede usar. Hoy es redundante; mañana, si
                // alguien le diera comando de arranque, esto es lo único
                // que impediría que una notificación del daemon hiciera que
                // el puente MCP lance procesos.
                if inner.agent_session.is_some() {
                    continue;
                }
                *inner.handover_until.lock().expect("handover lock sano") = aviso
                    .reconnect
                    .then(|| std::time::Instant::now() + HANDOVER_SPAWN_WINDOW);
                tracing::info!(reconnect = aviso.reconnect, "el daemon avisa de que se va");
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
                    serde_json::from_value::<methods::SyncPlanDone>(params).map(SyncPlanEvent::Done)
                } else {
                    serde_json::from_value::<methods::SyncStepsBatch>(params)
                        .map(SyncPlanEvent::Steps)
                };
                let event = match event {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::debug!(error = %e, "evento de sync malformado: descartado");
                        continue;
                    }
                };
                let id = match &event {
                    SyncPlanEvent::Steps(b) => b.task_id.get(),
                    SyncPlanEvent::Done(d) => d.task_id.get(),
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
            // ¿Sigue vivo el permiso de arranque? Por TIEMPO, no por
            // intentos (ver `Inner::handover_until`): dentro de la ventana
            // se puede insistir, que es lo que deja sobrevivir al lock del
            // journal que el daemon viejo todavía retiene.
            let relevo = {
                let mut hasta = backend
                    .inner
                    .handover_until
                    .lock()
                    .expect("handover lock sano");
                let vivo = spawn_allowed(*hasta, std::time::Instant::now());
                if !vivo {
                    // Caducado: se limpia para no volver a mirarlo.
                    *hasta = None;
                }
                vivo
            };
            match backend.establish(relevo).await {
                Ok(rx) => {
                    let _ = backend.inner.events_tx.send(ConnEvent::Restored);
                    break rx;
                }
                // Daemon incompatible de versión: reintentar es fútil.
                Err(e) if crate::rpc::is_version_mismatch(&e) => return,
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
            handover_until: Mutex::new(None),
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

    /// El permiso de arranque caduca por TIEMPO y no por intentos.
    ///
    /// Es la corrección de un fallo que la revisión de seguridad encontró y
    /// que anulaba la feature entera: gastándolo en el primer intento —a
    /// los 250 ms— el reemplazo todavía no puede arrancar, porque el daemon
    /// viejo no ha salido del proceso y retiene el lock de `journal.db`. El
    /// intento fallaba, el permiso se iba con él, y la sesión quedaba
    /// muerta para siempre tras una actualización normal.
    #[test]
    fn el_permiso_de_arranque_caduca_por_tiempo() {
        let ahora = std::time::Instant::now();
        // Sin aviso, jamás: es la regla de no resucitar un daemon parado.
        assert!(!spawn_allowed(None, ahora));
        // Dentro de la ventana, tantas veces como haga falta — que es lo
        // que deja sobrevivir al lock del journal.
        let hasta = ahora + HANDOVER_SPAWN_WINDOW;
        assert!(spawn_allowed(Some(hasta), ahora));
        assert!(spawn_allowed(Some(hasta), ahora + Duration::from_secs(29)));
        // Pasada, «el daemon no está» vuelve a significar lo de siempre.
        assert!(!spawn_allowed(Some(hasta), ahora + Duration::from_secs(31)));
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
            handover_until: Mutex::new(None),
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
}
