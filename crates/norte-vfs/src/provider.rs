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
use norte_proto::{ByteRange, Capabilities, Entry, Error, VPath};

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
///     async fn read(
///         &self,
///         _p: &VPath,
///         _range: Option<norte_proto::ByteRange>,
///     ) -> Result<ByteStream, Error> {
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

    /// Contenido de un archivo como stream de chunks. `range: None` = el
    /// archivo completo; con rango, desde `offset` hasta `len` bytes (o EOF,
    /// lo que llegue antes). `offset` más allá de EOF: stream vacío, no
    /// error (semántica de `pread`). Lo exige el resume de M2 y el viewer
    /// (ADR 0005).
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error>;

    /// Identidad REAL del nodo, si el backend la conoce: `(dev, ino)` en
    /// unix, `(volumen, FileId)` en Windows, clave interna en providers
    /// sintéticos. Es la base de los guards anti-autodestrucción del copy
    /// engine y del visited set contra ciclos de symlinks (spec §17.9).
    ///
    /// `follow` elige entre la identidad del PROPIO nodo (semántica lstat,
    /// coherente con [`Self::stat`]) o la de su destino resuelto; sobre un
    /// nodo que no es symlink ambas coinciden.
    ///
    /// `Ok(None)` (default) = este backend no tiene identidad estable
    /// (object storage, ftp): el caller degrada a heurísticas conservadoras
    /// y las features que EXIGEN identidad (seguir dir-symlinks) responden
    /// `Unsupported`.
    ///
    /// Errores: [`Error::NotFound`] si `p` no existe — o si es un symlink
    /// roto con [`FollowLinks::Yes`].
    async fn node_id(&self, p: &VPath, follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        let _ = (p, follow);
        Ok(None)
    }

    /// Bytes CRUDOS del destino de un symlink (relativo o absoluto, quizá
    /// roto, quizá no-UTF8 — jamás se valida como `VPath` ni se resuelve).
    ///
    /// Errores: [`Error::NotFound`] si `p` no existe;
    /// [`Error::Conflict`] (`TypeMismatch`) si existe pero no es symlink;
    /// [`Error::Unsupported`] si el provider no sabe de symlinks (default).
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let _ = p;
        Err(Error::Unsupported)
    }

    /// Mueve `p` (árbol entero si es dir) a la PAPELERA del provider —
    /// recuperable (ADR 0009). Solo con la capability `TRASH`; sin ella:
    /// [`Error::Unsupported`] (default) — el engine JAMÁS degrada a
    /// borrado permanente por su cuenta.
    ///
    /// Devuelve `Some(dest)` con el destino recuperable cuando la papelera es
    /// LÓGICA (`.norte-trash/<id>/payload`) — el core lo persiste como
    /// `reversal_ref` para el undo. `None` si es la papelera NATIVA del OS (sin
    /// ruta estable expuesta) o una papelera "vanish" de test.
    ///
    /// Excepciones de plataforma conocidas (ADR 0009, issues #25/#26):
    /// Windows puede DESTRUIR ítems no reciclables (auto-respuesta del
    /// nuke warning); freedesktop cross-device degrada a copy+delete
    /// interno (potencialmente largo e incancelable a mitad).
    async fn trash(&self, p: &VPath) -> Result<Option<VPath>, Error> {
        let _ = p;
        Err(Error::Unsupported)
    }

    /// GC de staging `.norte-partial` huérfano (ADR 0012, #11) en el directorio
    /// `dir`: borra los parciales cuya antigüedad supera `older_than`. Los
    /// reconoce por su FORMA exacta, no por prefijo suelto — un archivo real
    /// `.norte-partial.backup` JAMÁS se toca. Devuelve cuántos borró.
    ///
    /// Default no-op (`Ok(0)`): solo los providers con staging LOCAL lo
    /// implementan. NO es una mutación de usuario → no pasa por el journal.
    ///
    /// # Errors
    /// [`Error`] si `dir` no se puede listar; los fallos de borrado
    /// individuales se cuentan como no-borrados, sin abortar el barrido.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let _ = (dir, older_than);
        Ok(0)
    }

    /// Restaura desde la papelera NATIVA del OS el ítem cuya ruta original es
    /// `original` (undo M3-2 de un `Trashed` sin `reversal_ref`, ADR 0009).
    /// Default `Unsupported`. Solo el provider local lo implementa: casa por
    /// ruta original el ítem MÁS RECIENTE y lo restaura. Falla limpio si la
    /// plataforma no lista la papelera, no hay match, o el destino está ocupado.
    ///
    /// # Errors
    /// [`Error::Unsupported`] (default y plataformas sin listado de papelera);
    /// [`Error::NotFound`] sin match; [`Error::Conflict`] destino ocupado.
    async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
        let _ = original;
        Err(Error::Unsupported)
    }

    /// Crea un symlink en `link` apuntando a `target` (bytes crudos, tal
    /// cual — el provider no los interpreta). `kind` distingue archivo/dir
    /// donde el OS lo exige (Windows); unix lo ignora.
    ///
    /// Si `link` ya existe: [`Error::Conflict`]. Providers sin symlinks:
    /// [`Error::Unsupported`] (default) y SIN la capability `SYMLINKS`.
    async fn symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        let _ = (link, target, kind);
        Err(Error::Unsupported)
    }

    /// Abre un sink de escritura para un archivo NUEVO. Si el destino ya
    /// existe: [`Error::Conflict`] — la política de sobrescritura es del core,
    /// no del provider. Los bytes no son visibles en el path final hasta
    /// [`ByteSink::commit`].
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error>;

    /// Abre un sink que REANUDA una escritura previa a `p` (ADR 0012):
    /// devuelve el sink y cuántos bytes YA hay durables en el staging
    /// (`0` = empieza de cero). El sink AÑADE después de esos bytes; el
    /// engine lee el origen desde ese offset.
    ///
    /// La reanudación cross-invocación exige un staging con nombre ESTABLE
    /// por destino (un provider que lo soporte lo reencuentra). Default:
    /// `(write(p), 0)` — sin reanudación, empieza de cero (correcto y
    /// seguro; el engine recopia entero).
    ///
    /// El destino final debe seguir sin existir: si ya existe, mismo
    /// [`Error::Conflict`] que [`Self::write`].
    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        Ok((self.write(p).await?, 0))
    }

    /// Crea UN directorio (el padre debe existir; `mkdir -p` lo compone el core).
    /// Si ya existe: [`Error::Conflict`].
    async fn mkdir(&self, p: &VPath) -> Result<(), Error>;

    /// Borra UN nodo: archivo, symlink o directorio VACÍO (el walk post-order
    /// es del core). Directorio no vacío: [`Error::Conflict`].
    ///
    /// GARANTÍA (contractual): sobre un symlink borra EL LINK, jamás su
    /// target (semántica lstat/unlink). El copy engine confía en esto para
    /// que mover un árbol con links expandidos no destruya los targets
    /// (issue #19).
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
    ///
    /// **Contrato de cancelación (#51, regla 3):** el caller puede DROPEAR
    /// este future a medias (el engine lo racea contra su token). El
    /// implementador garantiza que un drop jamás deja en el destino un
    /// parcial VISIBLE sin marcar (S3 cumple: `CopyObject` es atómico y un
    /// multipart incompleto no publica objeto). Un efecto que complete
    /// server-side DESPUÉS del drop es ambigüedad aceptada (el engine la
    /// documenta, familia #32). OJO con implementaciones sobre
    /// `spawn_blocking` (reflink local futuro): el drop del future NO
    /// detiene el hilo — la copia correría hasta el final SIEMPRE y el
    /// "después del drop" pasaría de raza rara a caso determinista; esa
    /// implementación necesita su propio punto de cancelación.
    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        let _ = (from, to);
        None
    }
}

/// Tipo del symlink a crear: Windows distingue archivo/directorio en la
/// creación (`CreateSymbolicLinkW`); unix lo ignora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymlinkKind {
    /// El destino es (o será) un archivo.
    File,
    /// El destino es (o será) un directorio.
    Dir,
    /// El caller NO lo sabe (p. ej. el copy engine preservando un link de
    /// otro provider, issue #18): el provider lo determina best-effort
    /// resolviendo el target EN SU PROPIO árbol — target roto o
    /// indeterminable degrada a `File` (documentado). En OS donde el kind
    /// no importa (unix) equivale a `File` sin coste alguno.
    Unknown,
}

/// ¿Resolver symlinks al calcular la identidad de un nodo?
/// (Parámetro de [`Provider::node_id`].)
///
/// ```
/// use norte_vfs::FollowLinks;
/// // `No` = identidad del propio link; `Yes` = la de su destino.
/// assert_ne!(FollowLinks::No, FollowLinks::Yes);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowLinks {
    /// Identidad del propio nodo (semántica lstat, como `stat`).
    No,
    /// Identidad del destino resuelto; symlink roto = `NotFound`.
    Yes,
}

/// Identidad real de un nodo DENTRO de un provider: comparable y hashable,
/// jamás interpretable ni serializable al wire (es un detalle del backend;
/// comparar `NodeId` de providers distintos no significa nada).
///
/// ```
/// use norte_vfs::NodeId;
/// let a = NodeId { volume: 1, index: 42 };
/// let b = NodeId { volume: 1, index: 42 };
/// assert_eq!(a, b);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    /// Dominio de unicidad del índice (device unix, serial de volumen
    /// Windows; 0 si el backend no distingue volúmenes).
    pub volume: u64,
    /// Índice del nodo dentro del volumen (`ino`; 128 bits cubren el
    /// `FileId` de `ReFS`).
    pub index: u128,
}
