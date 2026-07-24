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

/// El tipo de stream que devuelve [`Backend::list_stream`] (ADR 0017),
/// re-exportado para que los frontends lo nombren sin depender de `norte-vfs`.
pub use norte_vfs::EntryStream;

/// Una task en marcha, venga del scheduler embebido o del daemon.
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
    /// `take_approvals`, `take_degraded`) son del PRIMER dueño — un clon
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
        match self {
            Self::Embedded(engine) => {
                let stream = engine.list(dir).await?;
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
            Self::Remote(r) => r.list_stream(dir).await,
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
        let (mut stream, skipped) = self.list_stream(dir).await?;
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

    /// Metadatos de un nodo (`fs.stat`).
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
        match self {
            Self::Embedded(engine) => engine.stat(path).await,
            #[cfg(unix)]
            Self::Remote(r) => r.stat(path).await,
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
    /// Taxonomía del protocolo; `Unsupported` en embebido (sin journal).
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
    /// Taxonomía del protocolo; `Unsupported` en embebido (sin journal, como
    /// [`Backend::undo_session`]).
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
                let content = crate::plugins::decode_for_preview(bytes);

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
                let content = crate::plugins::decode_for_preview(bytes);

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
                let values = tokio::task::spawn_blocking(
                    move || -> Result<Vec<Option<String>>, Error> {
                        let reg = crate::PluginRegistry::discover(&dir)
                            .map_err(|_| Error::Io { retryable: false })?;
                        let Some((id, _name, wasm, caps, settings)) =
                            reg.resolve_columns(&column_id_owned)
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
            Self::Remote(r) => r.plugin_column_values(column_id, paths).await,
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
        self, ClientInfo, ConnectionDegraded, FsCapabilitiesParams, FsCapabilitiesResult,
        FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams, FsReadParams,
        FsReadResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
        PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult, PolicyPendingResult,
        SearchHits, TaskCancelParams, TaskCancelResult, TaskListParams, TaskListResult,
    };
    use norte_proto::{
        ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskKind, TaskProgress,
        TaskState, VPath,
    };
    use norte_vfs::EntryStream;
    use tokio::sync::{mpsc, watch};

    use super::{ConnEvent, TaskCanceller, TaskRef};
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
    /// Buffer del canal de hits de una `fs.search` remota. Absorbe el burst de
    /// lotes que ya coalesció el daemon (`SEARCH_HITS_MAX_BATCH` entries por
    /// lote) mientras el frontend drena; holgado para que `try_send` no
    /// descarte por backpressure en el caso normal.
    const SEARCH_HITS_BUF: usize = 64;
    /// Tope de lotes de `search.hits` retenidos SIN route (carrera de arranque:
    /// un lote puede adelantar al registro del route). Acota la memoria ante un
    /// daemon que emita hits de `task_id`s que este proceso jamás registró.
    const SEARCH_PENDING_CAP: usize = 64;
    /// Gracia tras el terminal de una búsqueda antes de retirar su route. En el
    /// daemon la bomba de hits y la de progreso son tasks INDEPENDIENTES que
    /// escriben al mismo sink: un `search.hits` puede llegar tras el
    /// `task.progress` terminal. La gracia deja que esos lotes rezagados aún se
    /// enruten; pasada, el sender se suelta y `rx` se cierra (parida con el
    /// embebido). Retirar en seco al terminal perdería el lote rezagado.
    const SEARCH_ROUTE_GRACE: Duration = Duration::from_millis(500);

    /// Enrutado de los lotes de `search.hits` de las búsquedas VIVAS de este
    /// proceso, por `task_id`. Todo detrás de UN Mutex para que registrar el
    /// route (drenar lo pendiente + insertar) sea ATÓMICO frente a la bomba —
    /// sin ventana en la que un lote se pierda entre el drenaje y el insert.
    #[derive(Default)]
    struct SearchRoutes {
        /// `task_id` → sender del `rx` que devolvió [`RemoteBackend::search`].
        routes: HashMap<u64, mpsc::Sender<SearchHits>>,
        /// Lotes llegados ANTES de que su route se registrara (carrera de
        /// arranque): el registro los drena en orden. Acotado por
        /// [`SEARCH_PENDING_CAP`] lotes en total.
        pending: HashMap<u64, Vec<SearchHits>>,
        /// `task_id`s cuyo `task.progress` TERMINAL ya se vio. El terminal puede
        /// ADELANTAR al registro del route (el frame sale del daemon antes que
        /// la respuesta de `fs.search`, y en el cliente la bomba y `search`
        /// corren en paralelo): sin esto, la retirada del route se perdería y el
        /// `rx` no se cerraría jamás. Espejo del anillo `finished` de `own_task`.
        /// Acotado; una entrada se limpia al retirar su route.
        terminated: std::collections::HashSet<u64>,
    }

    impl SearchRoutes {
        /// Lotes pendientes retenidos en total (todos los `task_id`).
        fn pending_len(&self) -> usize {
            self.pending.values().map(Vec::len).sum()
        }

        /// Recuerda que `id` llegó a terminal (acotado: un backstop borra todo
        /// si crece sin límite, jamás memoria ilimitada ante un daemon hostil).
        /// Devuelve `true` solo en la INSERCIÓN nueva: el caller agenda la
        /// retirada una única vez, sin duplicarla ante terminales repetidos
        /// (bomba + resync de `task.list`). El set SOLO acumula terminales de
        /// BÚSQUEDA (ver `route`), así que el tope 256 es realista (haría falta
        /// 256 búsquedas concurrentes para que el `clear` borre una marca viva).
        fn mark_terminated(&mut self, id: u64) -> bool {
            if self.terminated.len() >= 256 {
                self.terminated.clear();
            }
            self.terminated.insert(id)
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
        search_routes: Mutex<SearchRoutes>,
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
            let (foreign_tx, foreign_rx) = mpsc::unbounded_channel();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
            let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();
            let backend = Self {
                inner: Arc::new(Inner {
                    socket,
                    spawn_cmd,
                    client_info,
                    client: tokio::sync::RwLock::new(None),
                    watches: Mutex::new(HashMap::new()),
                    finished: Mutex::new(std::collections::VecDeque::new()),
                    foreign_tx,
                    events_tx,
                    approvals_tx,
                    degraded_tx,
                    seen_approvals: Mutex::new(std::collections::HashSet::new()),
                    search_routes: Mutex::new(SearchRoutes::default()),
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
            // Resync de aprobaciones de policy pendientes (M3-3b T5): un Ask
            // difundido ANTES de esta conexión no se pierde. Best-effort: un
            // daemon N-1 (sin `policy.pending`) responde METHOD_NOT_FOUND y
            // no pasa nada; el TTL de una pendiente sin ver la deniega solo.
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
            // Búsqueda terminal: programa la retirada de su route TRAS la gracia
            // (deja pasar los `search.hits` rezagados; ver `SEARCH_ROUTE_GRACE`).
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
            if snapshot.state.is_terminal() && snapshot.kind == TaskKind::Search {
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
                    schedule_search_route_removal(&self.inner, id.get());
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
        /// Solo lo usan las MUTACIONES (fs.copy/move/delete): son las únicas
        /// que el daemon envuelve en su brazo de cancelación (#72).
        async fn call_timed_guarded<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
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
                CALL_TIMEOUT,
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
            };
            Ok((
                futures::stream::try_unfold(state, page_step).boxed(),
                skipped,
            ))
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

        pub(super) async fn stat(&self, path: &VPath) -> Result<Entry, Error> {
            let r: FsStatResult = self
                .call_timed(methods::FS_STAT, &FsStatParams { path: path.clone() })
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
            let (tx, rx) = mpsc::channel::<SearchHits>(SEARCH_HITS_BUF);
            let (already_terminal, discarded) = {
                let mut sr = self
                    .inner
                    .search_routes
                    .lock()
                    .expect("search_routes lock sano");
                // Drena los lotes que se adelantaron al registro (en orden). El
                // buffer se dimensiona para absorber el arranque; si aun así se
                // llenara, un lote de UI se pierde (honesto). El log va DESPUÉS
                // de soltar el guard: este lock lo toma también la bomba (ruta
                // caliente) y no debe esperar por un `tracing::warn!`.
                let mut discarded = 0usize;
                if let Some(early) = sr.pending.remove(&id.get()) {
                    for hits in early {
                        if tx.try_send(hits).is_err() {
                            discarded += 1;
                        }
                    }
                }
                sr.routes.insert(id.get(), tx);
                // ¿El terminal ADELANTÓ al registro? Entonces `route` no pudo
                // programar la retirada (aún no había route): la programa `search`.
                (sr.terminated.contains(&id.get()), discarded)
            };
            if discarded > 0 {
                tracing::warn!(
                    task_id = id.get(),
                    discarded,
                    "search.hits de arranque descartados (buffer del cliente lleno)"
                );
            }
            if already_terminal {
                schedule_search_route_removal(&self.inner, id.get());
            }
            Ok((self.own_task(id, TaskKind::Search), rx))
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

    /// Enruta UN lote de `search.hits` a su búsqueda por `task_id` (live
    /// search T5). Si el route existe, envía; `Closed` (el frontend soltó su
    /// `rx`) retira el route; `Full` descarta el lote con aviso (backpressure:
    /// el frontend va por detrás — los hits son un feed de UI, no dato
    /// autoritativo). Sin route todavía (carrera de arranque), lo retiene en
    /// `pending` acotado para que `RemoteBackend::search` lo drene al
    /// registrar; `task_id` desconocido con `pending` lleno = descarte con
    /// traza (un daemon no debería emitir hits de búsquedas que no lanzamos).
    fn route_search_hits(inner: &Arc<Inner>, hits: SearchHits) {
        let id = hits.task_id.get();
        let mut sr = inner.search_routes.lock().expect("search_routes lock sano");
        if let Some(tx) = sr.routes.get(&id) {
            match tx.try_send(hits) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // El frontend soltó su Receiver: el route ya no sirve.
                    sr.routes.remove(&id);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        task_id = id,
                        "search.hits: buffer del cliente lleno, lote descartado (backpressure)"
                    );
                }
            }
        } else if sr.pending_len() < SEARCH_PENDING_CAP {
            sr.pending.entry(id).or_default().push(hits);
        } else {
            tracing::debug!(
                task_id = id,
                "search.hits sin route y pending lleno: descartado"
            );
        }
    }

    /// Programa la retirada del route de una búsqueda terminal tras
    /// [`SEARCH_ROUTE_GRACE`]. Sostiene un [`Weak`] (no mantiene vivo a
    /// `Inner`): si el backend ya murió, no hay nada que limpiar. Al retirar el
    /// sender, el `rx` del frontend se cierra (fin del stream de hits).
    fn schedule_search_route_removal(inner: &Arc<Inner>, id: u64) {
        let weak = Arc::downgrade(inner);
        tokio::spawn(async move {
            tokio::time::sleep(SEARCH_ROUTE_GRACE).await;
            if let Some(inner) = weak.upgrade() {
                let mut sr = inner.search_routes.lock().expect("search_routes lock sano");
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
                    route_search_hits(&inner, hits);
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
            // El stream de hits de una búsqueda NO sobrevive a la reconexión:
            // la bomba de hits del daemon apuntaba al `conn_id` viejo (muerto).
            // Suelta todos los routes → los `rx` de las búsquedas en vuelo se
            // cierran (el frontend infiere el fin por el terminal de la Task,
            // reconciliado por el resync de `task.list`).
            {
                let mut sr = backend
                    .inner
                    .search_routes
                    .lock()
                    .expect("search_routes lock sano");
                sr.routes.clear();
                sr.pending.clear();
                sr.terminated.clear();
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

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_inner() -> Arc<Inner> {
            let (foreign_tx, _fr) = mpsc::unbounded_channel();
            let (events_tx, _er) = mpsc::unbounded_channel();
            let (approvals_tx, _ar) = mpsc::unbounded_channel();
            let (degraded_tx, _dr) = mpsc::unbounded_channel();
            Arc::new(Inner {
                socket: PathBuf::from("/nonexistent/test.sock"),
                spawn_cmd: None,
                client_info: ClientInfo {
                    name: "test".into(),
                    version: "0".into(),
                },
                client: tokio::sync::RwLock::new(None),
                watches: Mutex::new(HashMap::new()),
                finished: Mutex::new(std::collections::VecDeque::new()),
                foreign_tx,
                events_tx,
                approvals_tx,
                degraded_tx,
                seen_approvals: Mutex::new(std::collections::HashSet::new()),
                search_routes: Mutex::new(SearchRoutes::default()),
            })
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
            let mut sr = SearchRoutes::default();
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
                client: tokio::sync::RwLock::new(None),
                watches: Mutex::new(HashMap::new()),
                finished: Mutex::new(std::collections::VecDeque::new()),
                foreign_tx,
                events_tx,
                approvals_tx,
                degraded_tx,
                seen_approvals: Mutex::new(std::collections::HashSet::new()),
                search_routes: Mutex::new(SearchRoutes::default()),
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
}
