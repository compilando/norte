//! Lo que el host le pide al mundo, en la forma MÁS pequeña que le sirve.
//!
//! No es una segunda fachada del SDK: es la lista corta de cosas que el
//! controlador necesita, y existe por una razón concreta —que sus tests sean
//! deterministas sin daemon—. Todo lo demás se le pide al
//! [`norte_client::RemoteBackend`] directamente.

use std::sync::Arc;

use futures::future::BoxFuture;
use norte_proto::{DeleteMode, Entry, Error, TaskId, TaskProgress, VPath, methods};
use tokio::sync::watch;

/// Una Task en marcha, en la forma mínima que el host necesita: su id, su
/// progreso y cómo pedirle que pare.
///
/// No es el `RemoteTask` del SDK a propósito. El host solo necesita estas
/// tres cosas, y pedirlas así es lo que permite que un test las fabrique sin
/// daemon —que es donde se comprueban las reglas que de verdad importan: que
/// un estado terminal no se pierda y que cancelar sea idempotente—.
pub struct HostTask {
    /// Id de la task en el daemon.
    pub id: TaskId,
    /// Snapshots vivos del progreso.
    pub progress: watch::Receiver<TaskProgress>,
    /// Pide la cancelación cooperativa. Llamarla dos veces no es un error:
    /// cancelar es idempotente por contrato.
    pub cancel: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for HostTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A mano porque una función no es `Debug`, y `finish_non_exhaustive`
        // lo DICE en vez de dar a entender que la task son dos campos.
        f.debug_struct("HostTask")
            .field("id", &self.id)
            .field("progress", &self.progress.borrow().state)
            .finish_non_exhaustive()
    }
}

/// Lo que el controlador necesita saber pedir.
///
/// Objeto-seguro a propósito (futuros en caja): el host guarda un
/// `Arc<dyn HostBackend>` y un test mete el suyo sin genéricos que se
/// propaguen por toda la API.
pub trait HostBackend: Send + Sync + 'static {
    /// El listado COMPLETO de un directorio.
    ///
    /// Completo y no paginado a propósito en esta fase: la política de
    /// drenaje por páginas es la tarea 2.3, y meterla antes de tener el
    /// controlador sería decidirla sin nadie que la use.
    fn list(&self, dir: VPath) -> BoxFuture<'static, Result<Vec<Entry>, Error>>;

    /// La sesión de UI y si ESTA conexión es su dueña (ADR 0059).
    ///
    /// El core la guarda y la versiona pero no la lee: el documento es de los
    /// frontends, y por eso viaja como JSON opaco.
    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>>;

    /// Escribe la sesión sobre la revisión que se leyó. Devuelve la nueva.
    ///
    /// Un `Conflict` significa que otra ventana escribió en medio: se relee,
    /// jamás se pisa.
    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>>;

    /// Borra UNA entrada: a la papelera o permanente. Devuelve la Task ya
    /// encolada — el desenlace llega por su progreso, no por esta llamada.
    ///
    /// Una por entrada y no un lote porque el método del wire es así; un
    /// borrado de varias marcas son varias Tasks, y el tablero las enseña
    /// todas.
    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>>;
}

/// El backend de verdad: el SDK.
impl HostBackend for norte_client::RemoteBackend {
    fn list(&self, dir: VPath) -> BoxFuture<'static, Result<Vec<Entry>, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            use futures::StreamExt as _;
            let (mut stream, _total) = backend.list_stream(&dir, Vec::new()).await?;
            let mut out = Vec::new();
            while let Some(e) = stream.next().await {
                out.push(e?);
            }
            Ok(out)
        })
    }

    fn session_get(&self) -> BoxFuture<'static, Result<(methods::Session, bool), Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_get().await })
    }

    fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        let backend = self.clone();
        Box::pin(async move { backend.session_put(version, revision, body).await })
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        let backend = self.clone();
        Box::pin(async move {
            let task = backend.delete(&path, mode).await?;
            let canceller = task.canceller();
            Ok(HostTask {
                id: task.id(),
                progress: task.progress(),
                cancel: Arc::new(move || canceller.cancel()),
            })
        })
    }
}
