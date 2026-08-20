//! Lo que el host le pide al mundo, en la forma MÁS pequeña que le sirve.
//!
//! No es una segunda fachada del SDK: es la lista corta de cosas que el
//! controlador necesita, y existe por una razón concreta —que sus tests sean
//! deterministas sin daemon—. Todo lo demás se le pide al
//! [`norte_client::RemoteBackend`] directamente.

use futures::future::BoxFuture;
use norte_proto::{Entry, Error, VPath};

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
}
