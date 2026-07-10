//! El trait [`Provider`]: contrato único de todo backend de almacenamiento.
//!
//! Reglas del contrato (las verifica `provider_contract!` en `norte-testkit`):
//! - Nombres = bytes ([`VPath`]); un provider jamás renormaliza ni "repara"
//!   nombres en silencio.
//! - Operaciones simples: sin recursión (la hace el core), sin políticas de
//!   colisión (el core decide), sin seguir symlinks.
//! - Errores mapeados a la taxonomía [`Error`] en el borde del provider.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use norte_proto::{Capabilities, Entry, Error, VPath};

use crate::sink::ByteSink;

/// Stream de entradas de un listado (`fs.list`), perezoso y cancelable
/// soltándolo. Un error a mitad de stream termina el listado.
pub type EntryStream = BoxStream<'static, Result<Entry, Error>>;

/// Stream de contenido de un archivo, en chunks [`Bytes`] del tamaño que el
/// provider prefiera (el copy engine re-trocea si le hace falta).
pub type ByteStream = BoxStream<'static, Result<Bytes, Error>>;

/// Un backend de almacenamiento (local, sftp, s3, archive, memoria).
///
/// Objeto-seguro: el core trabaja con `Box<dyn Provider>` registrados por
/// scheme. Las operaciones compuestas (copy recursivo, move cross-provider,
/// delete de árboles) NO viven aquí: son del copy engine de `norte-core`.
///
/// ```
/// use async_trait::async_trait;
/// use norte_proto::{Capabilities, CapabilityFlags, Entry, Error, VPath};
/// use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
///
/// struct NullProvider;
///
/// #[async_trait]
/// impl Provider for NullProvider {
///     fn scheme(&self) -> &str {
///         "null"
///     }
///     fn capabilities(&self) -> Capabilities {
///         Capabilities { flags: CapabilityFlags::empty(), max_path: None }
///     }
///     async fn stat(&self, _p: &VPath) -> Result<Entry, Error> {
///         Err(Error::NotFound)
///     }
///     async fn list(&self, _p: &VPath) -> Result<EntryStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn read(&self, _p: &VPath) -> Result<ByteStream, Error> {
///         Err(Error::NotFound)
///     }
///     async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
///         Err(Error::Unsupported)
///     }
///     async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
///     async fn remove(&self, _p: &VPath) -> Result<(), Error> {
///         Err(Error::NotFound)
///     }
///     async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
///         Err(Error::Unsupported)
///     }
/// }
///
/// // Objeto-seguro: el core registra providers así.
/// let _boxed: Box<dyn Provider> = Box::new(NullProvider);
/// ```
#[async_trait]
pub trait Provider: Send + Sync {
    /// El scheme que sirve este provider (`file`, `sftp`, `mem`…).
    fn scheme(&self) -> &str;

    /// Capacidades declaradas; el core elige estrategia consultándolas.
    fn capabilities(&self) -> Capabilities;

    /// Metadatos de un nodo. Symlinks: describe el LINK (kind `Symlink`),
    /// jamás el destino.
    async fn stat(&self, p: &VPath) -> Result<Entry, Error>;

    /// Listado no recursivo de un directorio, como stream perezoso.
    /// El orden es el del backend, sin garantía.
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error>;

    /// Contenido completo de un archivo como stream de chunks.
    /// (Lectura por rango — resume — llega en M1.)
    async fn read(&self, p: &VPath) -> Result<ByteStream, Error>;

    /// Abre un sink de escritura para un archivo NUEVO. Si el destino ya
    /// existe: [`Error::Conflict`] — la política de sobrescritura es del core,
    /// no del provider. Los bytes no son visibles en el path final hasta
    /// [`ByteSink::commit`].
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error>;

    /// Crea UN directorio (el padre debe existir; `mkdir -p` lo compone el core).
    /// Si ya existe: [`Error::Conflict`].
    async fn mkdir(&self, p: &VPath) -> Result<(), Error>;

    /// Borra UN nodo: archivo, symlink o directorio VACÍO (el walk post-order
    /// es del core). Directorio no vacío: [`Error::Conflict`].
    async fn remove(&self, p: &VPath) -> Result<(), Error>;

    /// Renombra dentro de ESTE provider (cross-provider = copy+delete en el
    /// core). Atómico si la capability `RENAME_ATOMIC` está declarada.
    /// Si el destino existe: [`Error::Conflict`].
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error>;

    /// Copia server-side de UN archivo si el backend la ofrece (S3 `CopyObject`,
    /// reflink/clonefile…). `None` = "no sé hacerlo, hazlo por streaming";
    /// solo se consulta si la capability `SERVER_COPY` está declarada.
    ///
    /// Si el destino ya existe: [`Error::Conflict`] — MISMA política que
    /// [`Self::write`]. Un backend cuyo copy nativo sobrescribe por defecto
    /// (S3 `CopyObject`) DEBE chequear antes; jamás sobrescritura silenciosa.
    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        let _ = (from, to);
        None
    }
}
