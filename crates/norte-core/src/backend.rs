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

/// Una task en marcha, venga del scheduler embebido o del daemon.
pub struct TaskRef {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: TaskCanceller,
}

impl TaskRef {
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

/// El core detrás de una única superficie (regla 7).
pub enum Backend {
    /// Core in-process: arranque instantáneo, sin daemon.
    Embedded(Arc<Engine>),
    /// Contra el daemon UDS (ADR 0011).
    #[cfg(unix)]
    Remote(remote::RemoteBackend),
}

impl Backend {
    /// Listado completo de un directorio (el orden es del backend).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn list(&self, dir: &VPath) -> Result<Vec<Entry>, Error> {
        match self {
            Self::Embedded(engine) => {
                let mut stream = engine.list(dir).await?;
                let mut entries = Vec::new();
                while let Some(item) = stream.next().await {
                    entries.push(item?);
                }
                Ok(entries)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.list(dir).await,
        }
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
            Self::Embedded(engine) => engine.capabilities(path),
            #[cfg(unix)]
            Self::Remote(r) => r.capabilities(path).await,
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
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.copy_with(from, to, opts)?)),
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
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.move_with(from, to, opts)?)),
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
            Self::Embedded(engine) => Ok(TaskRef::from_handle(&engine.delete_with(path, mode)?)),
            #[cfg(unix)]
            Self::Remote(r) => r.delete(path, mode).await,
        }
    }

    /// Canal de tasks FORÁNEAS (encoladas por otros frontends de la misma
    /// sesión). `None` en embebido o si ya se tomó.
    pub fn take_foreign_tasks(&mut self) -> Option<mpsc::UnboundedReceiver<TaskRef>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_foreign_tasks(),
        }
    }

    /// Canal de eventos de conexión (aviso de reconexión). `None` en
    /// embebido o si ya se tomó.
    pub fn take_conn_events(&mut self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        match self {
            Self::Embedded(_) => None,
            #[cfg(unix)]
            Self::Remote(r) => r.take_conn_events(),
        }
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
    use norte_proto::methods::{
        self, ClientInfo, FsCapabilitiesParams, FsCapabilitiesResult, FsCopyParams, FsDeleteParams,
        FsListParams, FsListResult, FsMoveParams, FsReadParams, FsReadResult, FsTaskResult,
        TaskCancelParams, TaskCancelResult, TaskListParams, TaskListResult,
    };
    use norte_proto::{
        ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskKind, TaskProgress,
        TaskState, VPath,
    };
    use tokio::sync::{mpsc, watch};

    use super::{ConnEvent, TaskCanceller, TaskRef};
    use crate::daemon::{Client, ClientError};
    use crate::engine::TransferOptions;

    /// Backoff de reconexión (se recorre y se queda en el último).
    const RECONNECT_BACKOFF_MS: &[u64] = &[250, 500, 1000, 2000, 5000];
    /// Tope de una llamada RPC: un daemon vivo-pero-atascado (stat sobre un
    /// NFS muerto) jamás congela el frontend (M4 del rust-reviewer).
    const CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Mapea el error del cliente RPC a la taxonomía (el contrato de los
    /// frontends es SIEMPRE la taxonomía, spec §17.7).
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
        client: tokio::sync::RwLock<Option<Arc<Client>>>,
        watches: Mutex<HashMap<u64, watch::Sender<TaskProgress>>>,
        /// Desenlaces vistos SIN watch receptor (el broadcast terminal
        /// puede adelantar a la response del método que creó la task):
        /// `own_task` los consulta para no esperar un progreso que ya pasó.
        finished: Mutex<std::collections::VecDeque<TaskProgress>>,
        foreign_tx: mpsc::UnboundedSender<TaskRef>,
        foreign_rx: Mutex<Option<mpsc::UnboundedReceiver<TaskRef>>>,
        events_tx: mpsc::UnboundedSender<ConnEvent>,
        events_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnEvent>>>,
    }

    /// Conexión (auto-reconectante) con el daemon. Clonable: todos los
    /// clones comparten conexión, watches y canales.
    #[derive(Clone)]
    pub struct RemoteBackend {
        inner: Arc<Inner>,
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
            let (foreign_tx, foreign_rx) = mpsc::unbounded_channel();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let backend = Self {
                inner: Arc::new(Inner {
                    socket,
                    spawn_cmd,
                    client_info,
                    client: tokio::sync::RwLock::new(None),
                    watches: Mutex::new(HashMap::new()),
                    finished: Mutex::new(std::collections::VecDeque::new()),
                    foreign_tx,
                    foreign_rx: Mutex::new(Some(foreign_rx)),
                    events_tx,
                    events_rx: Mutex::new(Some(events_rx)),
                }),
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
            client.initialize(self.inner.client_info.clone()).await?;
            let notifications = client.take_notifications();
            let client = Arc::new(client);
            *self.inner.client.write().await = Some(Arc::clone(&client));

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
            Ok(notifications)
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

        pub(super) async fn list(&self, dir: &VPath) -> Result<Vec<Entry>, Error> {
            let r: FsListResult = self
                .call_timed(methods::FS_LIST, &FsListParams { path: dir.clone() })
                .await?;
            Ok(r.entries)
        }

        pub(super) async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
            let r: FsCapabilitiesResult = self
                .call_timed(
                    methods::FS_CAPABILITIES,
                    &FsCapabilitiesParams { path: path.clone() },
                )
                .await?;
            Ok(r.capabilities)
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
                self.call_timed(
                    method,
                    &FsCopyParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
                    },
                )
                .await?
            } else {
                self.call_timed(
                    method,
                    &FsMoveParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
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

        pub(super) async fn delete(
            &self,
            path: &VPath,
            mode: DeleteMode,
        ) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed(
                    methods::FS_DELETE,
                    &FsDeleteParams {
                        path: path.clone(),
                        mode,
                    },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::Delete))
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
            self.inner
                .foreign_rx
                .lock()
                .expect("foreign_rx lock sano")
                .take()
        }

        pub(super) fn take_conn_events(&self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
            self.inner
                .events_rx
                .lock()
                .expect("events_rx lock sano")
                .take()
        }
    }

    /// Bomba vitalicia de notificaciones (M2 del rust-reviewer). Sostiene
    /// un [`Weak`]: en el estado estable (bloqueada en `recv().await`) NO
    /// mantiene viva a `Inner`, así que cuando el último `RemoteBackend`
    /// externo se suelta, `Inner` se libera, el `Client` interno cierra la
    /// conexión, `recv()` devuelve `None` y la bomba SALE — sin ciclo de
    /// Arc ni reconexión eterna.
    async fn pump_loop(
        weak: Weak<Inner>,
        mut notifications: mpsc::UnboundedReceiver<norte_proto::wire::Notification>,
    ) {
        loop {
            // Consumo: NUNCA se retiene un Arc a través del `recv().await`.
            while let Some(n) = notifications.recv().await {
                if n.method != methods::TASK_PROGRESS {
                    continue;
                }
                let Some(params) = n.params else { continue };
                let Ok(snapshot) = serde_json::from_value::<TaskProgress>(params) else {
                    continue;
                };
                let Some(inner) = weak.upgrade() else { return };
                RemoteBackend { inner }.route(snapshot);
            }
            // Conexión muerta. Si ya no queda backend externo, salir.
            let Some(inner) = weak.upgrade() else { return };
            let backend = RemoteBackend { inner };
            *backend.inner.client.write().await = None;
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
                let backend = RemoteBackend { inner };
                match backend.establish(false).await {
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
}
