//! [`ObjectProvider`]: el trait [`Provider`] sobre un [`opendal::Operator`]
//! (ADR 0016). Object storage no tiene directorios: se modelan como marker
//! objects (`clave/`) + sondeo de prefijo, con precedencia fichero > dir.

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use norte_proto::{
    Authority, ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error,
    Scheme, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider, trash};
use opendal::{ErrorKind, Metadata, Operator};

/// Límite de S3 para la longitud de la KEY COMPLETA: 1024 BYTES de UTF-8. El
/// presupuesto EFECTIVO para el path del provider se calcula en [`new`]
/// restando el prefijo `root` del `Operator` (que opendal antepone a cada key
/// antes de enviarla) — sin ese descuento, un `Operator` con `root` largo
/// dejaría pasar keys que el servidor rechaza a media operación con un error
/// ambiguo. No hay límite por segmento (a diferencia del `NAME_MAX` de un FS
/// POSIX): un segmento de 300 bytes es una key S3 legal.
const MAX_KEY_BYTES: usize = 1024;

/// Chunk del writer multiparte: 8 MiB (mínimo S3 = 5 MiB; opendal bufferiza
/// hasta esto antes de subir una parte — objetos menores van por `PutObject`).
const WRITE_CHUNK: usize = 8 * 1024 * 1024;

/// Provider VFS sobre object storage (ADR 0016).
///
/// El [`Operator`] llega YA configurado (bucket/region/endpoint/credenciales
/// se resuelven en `norte-connect`, fase 7d): este provider jamás ve un
/// secreto. Semántica S3 sobre el contrato del trait:
///
/// - **Directorios** = marker objects (`clave/`) + sondeo de prefijo;
///   precedencia fichero > dir (S3 permite que `x` y `x/` coexistan; desde
///   este provider es imposible crearlo porque `write`/`mkdir` se comprueban
///   mutuamente).
/// - **Keys UTF-8-only** (límite del protocolo S3, no de la librería): un
///   nombre no representable es [`Error::InvalidPath`], regla 1.
/// - **`rename` NO es atómico y es O(n)** en directorios (copy-all luego
///   delete-all: un fallo a mitad deja duplicados, jamás pérdida) — por eso
///   no se declara `RENAME_ATOMIC`.
/// - **Resume**: diferido (ADR 0016 F) — opendal 0.58 no expone reanudar un
///   multipart upload; `open_resumable` hereda el default `(write, 0)` y
///   cancelar deja el destino limpio (sin `.norte-partial` remoto).
pub struct ObjectProvider {
    op: Operator,
    scheme: String,
    /// Bytes disponibles para el path del provider = 1024 − prefijo `root` del
    /// `Operator` − 1 (la variante dir añade `/`). Calculado en [`new`].
    key_budget: usize,
    /// El backend anuncia `copy` server-side (S3 sí; otro `Operator` podría
    /// no). Gatea `SERVER_COPY`: sin él, declararlo haría que el engine
    /// fallara en duro un fichero (`Some(Err(Unsupported))`, sin fallback a
    /// streaming) donde el streaming habría funcionado.
    server_copy: bool,
    /// Papelera lógica `.norte-trash/` activa (opt-in por conexión, ADR
    /// 0019). Off por defecto → no declara `TRASH` → borrado permanente.
    logical_trash: bool,
}

impl ObjectProvider {
    /// Provider sobre un `Operator` ya configurado, para `scheme` (`"s3"`).
    ///
    /// La raíz del `Operator` (bucket + prefijo `root` del builder) es la
    /// raíz del provider: aquí no hay `base` — la fija quien lo construye.
    #[must_use]
    pub fn new(op: Operator, scheme: impl Into<String>) -> Self {
        // El `root` del Operator (p. ej. `/equipo/proyecto/`) se antepone a
        // cada key ANTES de enviarla al servidor (la `/` inicial no cuenta —
        // opendal la recorta). Se descuenta del presupuesto de 1024 para no
        // dejar pasar keys que el servidor rechazaría a media operación.
        let root_prefix = op.info().root().trim_start_matches('/').len();
        // −1: reserva el `/` final de la variante directorio.
        let key_budget = MAX_KEY_BYTES.saturating_sub(root_prefix).saturating_sub(1);
        let server_copy = op.info().capability().copy;
        Self {
            op,
            scheme: scheme.into(),
            key_budget,
            server_copy,
            logical_trash: false,
        }
    }

    /// Activa/desactiva la papelera lógica `.norte-trash/` (ADR 0019).
    /// Sin ella el provider no declara `TRASH` y `trash()` da `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }

    /// Lee y valida la `.norte-info` de una entrada de papelera: `true` solo si
    /// decodifica exactamente a `p` (misma víctima). Distingue NUESTRA entrada
    /// de una colisión ajena con el mismo id (#99, review rust MAJOR). Ausente,
    /// ilegible o de otra víctima = `false`.
    async fn trash_info_matches(&self, info: &VPath, p: &VPath) -> bool {
        let Ok(mut stream) = self.read(info, None).await else {
            return false;
        };
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => buf.extend_from_slice(&b),
                Err(_) => return false,
            }
        }
        trash::info_decode(&buf, p).is_ok_and(|i| i.original == *p)
    }

    /// Crea el marker `dir` tolerando que ya exista (idempotente): útil para
    /// `.norte-trash/` bajo concurrencia entre sesiones.
    async fn ensure_dir_idempotent(&self, dir: &VPath) -> Result<(), Error> {
        match self.mkdir(dir).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => Ok(()),
            Err(e) => match self.stat_kind(&self.key(dir)?).await? {
                Some((EntryKind::Dir, _)) => Ok(()),
                _ => Err(e),
            },
        }
    }

    /// La raíz de este provider para un `scheme`/`authority` (`s3://bucket/`).
    ///
    /// # Panics
    /// Si `scheme` no es un scheme válido (`[a-z][a-z0-9+.-]*`). Los llamantes
    /// pasan una constante (`"s3"`), así que en la práctica nunca ocurre.
    #[must_use]
    pub fn root(scheme: &str, authority: Authority) -> VPath {
        VPath::root(Scheme::new(scheme).expect("scheme válido"), Some(authority))
    }

    /// Traduce un [`VPath`] a la key del objeto (sin `/` final). Los
    /// segmentos son BYTES; las keys S3 son UTF-8 — un nombre no
    /// representable es [`Error::InvalidPath`] (rechazo LIMPIO, regla 1,
    /// ADR 0016 D). La key se construye SIEMPRE así, nunca desde una ecoada.
    ///
    /// Sin filtro CRLF (a diferencia de FTP): S3 viaja por HTTP firmado
    /// (sigv4 cubre el path), no hay protocolo de líneas que inyectar.
    fn key(&self, p: &VPath) -> Result<String, Error> {
        if p.scheme() != self.scheme {
            return Err(Error::InvalidPath);
        }
        let mut out = String::new();
        for seg in p.segments() {
            let name = std::str::from_utf8(seg).map_err(|_| Error::InvalidPath)?;
            if name.contains('/') || name == "." || name == ".." {
                return Err(Error::InvalidPath);
            }
            // U+FFFD a ESCRIBIR: S3-legal (UTF-8), pero `list` lo usa como
            // centinela de "bytes perdidos en decodificación lossy" y corta
            // el listado ante él. Crearlo dejaría el directorio padre
            // ilistable (self-DoS) — se rechaza aquí para que write y list
            // sean simétricos (deuda: distinguir U+FFFD legítimo, #37).
            if name.contains('\u{FFFD}') {
                return Err(Error::InvalidPath);
            }
            // El `normalize_path` de opendal-core hace `path.trim()`: un
            // nombre con whitespace Unicode inicial/final se RENOMBRARÍA en
            // silencio ("file " → "file") en TODOS los backends — corrupción
            // de bytes, regla 1. Rechazo fail-loud uniforme por segmento
            // (S3 los permite; deuda upstream registrada, issue #48).
            if name.trim() != name {
                return Err(Error::InvalidPath);
            }
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(name);
        }
        if out.len() > self.key_budget {
            return Err(Error::InvalidPath);
        }
        Ok(out)
    }

    /// La key en su forma DIRECTORIO (`clave/`); la raíz es `""` (opendal
    /// lista la raíz del Operator con el path vacío).
    fn dir_key(&self, p: &VPath) -> Result<String, Error> {
        let k = self.key(p)?;
        if k.is_empty() {
            Ok(k)
        } else {
            Ok(format!("{k}/"))
        }
    }

    /// `stat` interno: fichero primero, dir (marker o prefijo con hijos)
    /// después. `None` = no existe. La precedencia fichero > dir está
    /// documentada en el ADR 0016 C.
    async fn stat_kind(&self, key: &str) -> Result<Option<(EntryKind, Metadata)>, Error> {
        match self.op.stat(key).await {
            // services-fs (harness del contrato) responde al stat SIN barra
            // de un directorio real con mode=DIR; S3 solo con ficheros.
            Ok(m) if m.mode().is_dir() => return Ok(Some((EntryKind::Dir, m))),
            Ok(m) => return Ok(Some((EntryKind::File, m))),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            // stat de un dir por la key sin barra: algunos backends lo
            // señalan en vez de NotFound — cae al sondeo de dir de abajo.
            Err(e) if e.kind() == ErrorKind::IsADirectory => {}
            Err(e) => return Err(map_err(&e)),
        }
        // ¿Dir? En S3 `stat("clave/")` es el sondeo del CompleteLayer de
        // opendal: marker O prefijo con hijos (list limit 1); en fs, el stat
        // real del directorio. Asimetría deliberada fs/S3: bajo un fichero
        // (`a.txt/hijo`) el harness fs da TypeMismatch (ENOTDIR) y S3 da
        // NotFound — el contrato solo ejercita el primero.
        match self.op.stat(&format!("{key}/")).await {
            Ok(m) => Ok(Some((EntryKind::Dir, m))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(map_err(&e)),
        }
    }

    /// ¿Existe `p` el padre de la key como directorio? La raíz siempre existe.
    async fn parent_dir_exists(&self, p: &VPath) -> Result<bool, Error> {
        let Some(parent) = p.parent() else {
            // Sin padre = `p` es la raíz; su "padre" no aplica.
            return Ok(true);
        };
        if parent.parent().is_none() {
            return Ok(true); // el padre es la raíz del provider
        }
        let key = self.key(&parent)?;
        Ok(matches!(
            self.stat_kind(&key).await?,
            Some((EntryKind::Dir, _))
        ))
    }

    /// Comprueba destino libre (ni fichero ni dir) → si no, `Conflict`.
    async fn ensure_absent(&self, key: &str) -> Result<(), Error> {
        if self.stat_kind(key).await?.is_some() {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(())
    }
}

impl std::fmt::Debug for ObjectProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectProvider")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

/// Sufijo VALIDADO de una entrada listada relativo a `from_dir` (#49): la
/// contención anti-servidor-mentiroso del rename de prefijo — `None` = el
/// propio dir listado (se salta); `Err(InvalidPath)` = la entrada sale del
/// prefijo o trae componentes ilegales (lossy `�`, `.`/`..`/vacío). Las keys
/// de copy/delete se RECONSTRUYEN siempre desde este sufijo, jamás se ecoa
/// el path del servidor.
fn validated_suffix<'a>(from_dir: &str, path: &'a str) -> Result<Option<&'a str>, Error> {
    if path == from_dir {
        return Ok(None);
    }
    let Some(suffix) = path.strip_prefix(from_dir) else {
        return Err(Error::InvalidPath);
    };
    if suffix.contains('\u{FFFD}') {
        return Err(Error::InvalidPath);
    }
    // Cada segmento del sufijo (recursivo → puede llevar `/`; un subdir
    // acaba en `/`) debe ser un nombre legal.
    let trimmed = suffix.strip_suffix('/').unwrap_or(suffix);
    if trimmed.is_empty()
        || trimmed
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(Error::InvalidPath);
    }
    Ok(Some(suffix))
}

/// Mapea el error de opendal a la taxonomía del protocolo (spec §17.7).
fn map_err(e: &opendal::Error) -> Error {
    match e.kind() {
        ErrorKind::NotFound => Error::NotFound,
        ErrorKind::PermissionDenied => Error::PermissionDenied,
        // Conditional write fallido (If-None-Match): el destino apareció.
        ErrorKind::ConditionNotMatch | ErrorKind::AlreadyExists => Error::Conflict {
            conflict: ConflictKind::Exists,
        },
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        },
        ErrorKind::RateLimited => Error::Io { retryable: true },
        ErrorKind::Unsupported => Error::Unsupported,
        // `is_temporary`: opendal marca así los errores de red/servicio que
        // merecen reintento (el backoff cancelable es del engine).
        _ if e.is_temporary() => Error::ProviderUnavailable { retryable: true },
        _ => Error::Io { retryable: false },
    }
}

/// Catálogo de attrs del provider object (#108 bloque 2).
///
/// `s3.etag` viaja en `ListObjectsV2` Y en `HeadObject` (gratis en ambos);
/// `s3.content_type` SOLO llega en stat (`HeadObject`) — en listados va
/// ausente, que es contrato-legal ("absence means absence"; la hidratación
/// de la UI re-statea la entrada enfocada). `s3.storage_class` es imposible
/// con opendal 0.58 (lo descarta al parsear el XML) — deuda con issue
/// propio en el cierre del bloque.
fn catalogo_s3() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty: AttrType::Text,
            hint: AttrHint::Opaque,
        };
        vec![mk("s3.etag", "ETag"), mk("s3.content_type", "Content-Type")]
    });
    &CAT
}

/// Materializa los attrs pedidos desde la metadata de opendal. Un valor
/// sobre el tope de Text se OMITE (jamás se trunca en silencio: la celda
/// ausente es honesta, la recortada mentiría).
fn attrs_from_meta(
    m: &Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if req.is_empty() {
        return out;
    }
    let mut texto = |id: &str, v: Option<&str>| {
        if let Some(s) = v
            && req.wants(id)
            && s.len() <= norte_proto::ATTR_TEXT_MAX
        {
            out.insert(id.to_owned(), AttrValue::Text(s.to_owned()));
        }
    };
    texto("s3.etag", m.etag());
    texto("s3.content_type", m.content_type());
    out
}

/// `Entry` de un fichero a partir de la metadata de opendal.
fn file_entry(path: VPath, m: &Metadata, req: &norte_vfs::AttrRequest) -> Entry {
    Entry {
        attrs: attrs_from_meta(m, req),
        path,
        kind: EntryKind::File,
        size: Some(m.content_length()),
        mtime_ms: m.last_modified().map(jiff_ms),
    }
}

fn jiff_ms(ts: opendal::raw::Timestamp) -> i64 {
    ts.into_inner().as_millisecond()
}

#[async_trait]
impl Provider for ObjectProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn capabilities(&self) -> Capabilities {
        // Honestas (ADR 0016 H): keys UTF-8 byte-exactas → case-sensitive y
        // case-preserving. SERVER_COPY = CopyObject (fase 7c, primer provider
        // del repo que lo implementa) SOLO si el backend anuncia `copy` — sin
        // ese gate, un backend sin copia haría fallar en duro un fichero que
        // el streaming habría copiado. NO declara: APPEND/RANDOM_WRITE (S3 no
        // tiene), SYMLINKS, RENAME_ATOMIC (copy+delete O(n)). TRASH solo si la
        // conexión activó la papelera lógica `.norte-trash/` (ADR 0019): la
        // relocalización reusa el rename copy-all→delete-all.
        let mut flags = CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING;
        if self.server_copy {
            flags |= CapabilityFlags::SERVER_COPY;
        }
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            // El presupuesto EFECTIVO (1024 − prefijo root − 1), no el límite
            // bruto de S3: lo que `key()` acepta = lo que el core pre-valida.
            max_path: u32::try_from(self.key_budget).ok(),
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        catalogo_s3()
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            // La raíz del provider (el bucket) siempre existe como dir.
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            });
        }
        match self.stat_kind(&key).await? {
            Some((EntryKind::File, m)) => Ok(file_entry(p.clone(), &m, &opt.attrs)),
            Some((_, _)) => Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: p.clone(),
                // El mtime de un marker no describe el "directorio" (los
                // objetos de dentro cambian sin tocarlo): None honesto.
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            }),
            None => Err(Error::NotFound),
        }
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        let dir = self.dir_key(p)?;
        if !dir.is_empty() {
            // NotFound / no-dir van en el Result, no como primer item.
            match self.stat_kind(self.key(p)?.as_str()).await? {
                Some((EntryKind::Dir, _)) => {}
                Some(_) => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    });
                }
                None => return Err(Error::NotFound),
            }
        }
        let lister = self.op.lister(&dir).await.map_err(|e| map_err(&e))?;
        let base = p.clone();
        let self_key = dir;
        // Arc: la petición es solo-lectura y el closure clona POR ENTRADA —
        // sin Arc cada objeto listado pagaría un Vec<String> nuevo.
        let req = std::sync::Arc::new(opt.attrs.clone());
        // Stream PEREZOSO: opendal pagina con su ContinuationToken por debajo
        // (punto de contacto con la paginación por cursor, ADR 0017).
        let stream = lister.map_err(|e| map_err(&e)).try_filter_map(move |oe| {
            let base = base.clone();
            let self_key = self_key.clone();
            let req = std::sync::Arc::clone(&req);
            async move {
                let path = oe.path();
                // opendal devuelve el propio dir listado como entrada; al
                // listar la RAÍZ (self_key vacío) la emite como `/`.
                if path == self_key || (self_key.is_empty() && path == "/") {
                    return Ok(None);
                }
                let is_dir = oe.metadata().mode().is_dir();
                // El nombre DEBE colgar del prefijo pedido. Un servidor
                // mentiroso que ecoe una key fuera de `self_key` (`otra`,
                // `../x`) NO se acepta con fallback al path completo: corta
                // fail-loud (jamás un Entry fantasma bajo `base`).
                let Some(rest) = path.strip_prefix(self_key.as_str()) else {
                    return Err(Error::InvalidPath);
                };
                let name = rest.trim_end_matches('/');
                // `""` (de un `dir//x` con segmento vacío), `.` y `..` son
                // keys S3 legales que el modelo de dirs no sabe representar:
                // se CORTA (no se ocultan en silencio — serían datos
                // invisibles en una migración dirigida por list).
                if name.is_empty() || name == "." || name == ".." {
                    return Err(Error::InvalidPath);
                }
                // Un nombre del backend con `/` (subprefijo inesperado, o un
                // `/` inyectado para escapar del dir) o U+FFFD (bytes
                // originales perdidos en una decodificación lossy) corta el
                // listado: rechazo fail-loud, regla 1.
                if name.contains('/') || name.contains('\u{FFFD}') {
                    return Err(Error::InvalidPath);
                }
                let seg = Segment::new(name.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
                let child = base.join(seg);
                let entry = if is_dir {
                    Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: child,
                        kind: EntryKind::Dir,
                        size: None,
                        mtime_ms: None,
                    }
                } else {
                    file_entry(child, oe.metadata(), &req)
                };
                Ok(Some(entry))
            }
        });
        Ok(stream.boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let key = self.key(p)?;
        // Rechaza dirs (leerlos es error) y ausentes (NotFound).
        let size = match self.stat_kind(&key).await? {
            None => return Err(Error::NotFound),
            Some((EntryKind::Dir, _)) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            Some((_, m)) => m.content_length(),
        };
        // Semántica pread del trait: offset > EOF = stream vacío y len se
        // recorta a EOF. Se CLAMPEA aquí con el size del stat (los objetos
        // son inmutables) en vez de confiar en el 416 del servidor: el
        // reader de opendal exige un fin dentro del fichero o falla a mitad
        // de stream.
        let (offset, len) = match range {
            Some(ByteRange { offset, len }) => (offset, len),
            None => (0, None),
        };
        let start = offset.min(size);
        let avail = size - start;
        let want = len.map_or(avail, |l| l.min(avail));
        if want == 0 {
            return Ok(futures::stream::empty().boxed());
        }
        let reader = self.op.reader(&key).await.map_err(|e| map_err(&e))?;
        match reader
            .into_bytes_stream(opendal::BytesRange::new(start, Some(want)))
            .await
        {
            Ok(s) => Ok(s
                .map_ok(Bytes::from)
                // Un corte de red A MITAD de descarga conserva la marca
                // `retryable` (el engine reintenta con backoff) — como el
                // `map_io` de local; solo los transitorios de transporte.
                .map_err(|e| Error::Io {
                    retryable: matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ),
                })
                .boxed()),
            // Cinturón para el clamp de arriba (no debería dispararse con
            // objetos inmutables): 416 = stream vacío, no error.
            Err(e) if e.kind() == ErrorKind::RangeNotSatisfied => {
                Ok(futures::stream::empty().boxed())
            }
            Err(e) => Err(map_err(&e)),
        }
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        if !self.parent_dir_exists(p).await? {
            return Err(Error::NotFound);
        }
        // Create-new: el contrato exige `Conflict` AL ABRIR. Este stat-check
        // upfront se mantiene SIEMPRE como cinturón (ADR 0016 E): contra un
        // servidor que ignore If-None-Match la garantía degrada a esta
        // comprobación (racy, nivel ftp), nunca a sobrescritura sin chequeo.
        self.ensure_absent(&key).await?;
        // El staging invisible es el propio multipart upload (o el PutObject
        // bufferizado): nada existe en la key hasta `close()`. If-None-Match
        // viaja en el commit → create-new race-free en servidores honestos.
        let mut w = self.op.writer_with(&key).chunk(WRITE_CHUNK);
        if self.op.info().capability().write_with_if_not_exists {
            w = w.if_not_exists(true);
        }
        let writer = w.await.map_err(|e| map_err(&e))?;
        Ok(Box::new(ObjectSink {
            writer: Some(writer),
        }))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            // La raíz ya existe.
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        if !self.parent_dir_exists(p).await? {
            return Err(Error::NotFound);
        }
        self.ensure_absent(&key).await?;
        // En S3 el CompleteLayer de opendal escribe el marker (`clave/`
        // vacío); en fs es el mkdir real. El `mkdir -p` implícito de algunos
        // backends es inofensivo: el padre ya se validó arriba.
        self.op
            .create_dir(&format!("{key}/"))
            .await
            .map_err(|e| map_err(&e))
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let key = self.key(p)?;
        if key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // stat previo: el delete de opendal es idempotente y mentiría con el
        // NotFound honesto que el contrato exige.
        match self.stat_kind(&key).await? {
            None => Err(Error::NotFound),
            Some((EntryKind::File, _)) => self.op.delete(&key).await.map_err(|e| map_err(&e)),
            Some(_) => {
                let dir = format!("{key}/");
                // Dir con hijos se niega (remove NO recursivo). El propio
                // marker no cuenta como hijo.
                let mut lister = self.op.lister(&dir).await.map_err(|e| map_err(&e))?;
                while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
                    if oe.path() != dir {
                        // Dir con hijos, como local (`DirectoryNotEmpty`).
                        return Err(Error::Conflict {
                            conflict: ConflictKind::TypeMismatch,
                        });
                    }
                }
                self.op.delete(&dir).await.map_err(|e| map_err(&e))
            }
        }
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_key = self.key(from)?;
        let to_key = self.key(to)?;
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // Un dir NO puede renombrarse dentro de su propio subárbol
        // (`a` → `a/b`): S3 lo "ejecutaría" reubicando y borrando el marker
        // origen, y el harness fs fallaría a media operación (ENOTEMPTY).
        // Se rechaza limpio, como el EINVAL de POSIX en local.
        if to_key == from_key || to_key.starts_with(&format!("{from_key}/")) {
            return Err(Error::InvalidPath);
        }
        let src = self.stat_kind(&from_key).await?.ok_or(Error::NotFound)?;
        if !self.parent_dir_exists(to).await? {
            return Err(Error::NotFound);
        }
        // Destino libre (fichero y dir): comprobación racy documentada (S3
        // no tiene rename; CopyObject sobrescribiría en silencio).
        self.ensure_absent(&to_key).await?;
        if src.0 == EntryKind::File {
            self.op
                .copy(&from_key, &to_key)
                .await
                .map_err(|e| map_err(&e))?;
            return self.op.delete(&from_key).await.map_err(|e| map_err(&e));
        }
        // Dir = prefijo entero: copy-all EN STREAMING (#49: sin materializar
        // el prefijo — pico de memoria O(dirs), no O(objetos)) y LUEGO
        // borrado con re-list + deleter batcheado. El invariante se
        // conserva: la fase de copia DRENA el lister entero antes del primer
        // delete — un fallo a mitad deja duplicados, jamás pérdida.
        //
        // Sufijos VALIDADOS relativos a `from_dir`, NUNCA la key ecoada: un
        // servidor mentiroso podría listar keys fuera del prefijo y hacer
        // que copy/delete operen (y BORREN) fuera del árbol — misma
        // contención que `list()`. Por eso el borrado NO usa
        // `Operator::remove_all` (borra lo que el servidor ecoe, sin
        // contención): re-lista y RECONSTRUYE cada key desde el sufijo
        // validado, con el `Deleter` de opendal batcheando por debajo
        // (DeleteObjects en S3).
        let from_dir = format!("{from_key}/");
        let to_dir = format!("{to_key}/");
        // Fase 0 — PRE-VALIDACIÓN streaming (BLOCKER histórico de los
        // reviewers: un listado hostil corta con CERO mutaciones): se drena
        // el lister validando cada sufijo SIN copiar nada — O(1) de memoria,
        // un sweep de LIST extra (n/1000 requests) frente a n copies. Una
        // entrada hostil COLADA entre este sweep y la copia corta a mitad de
        // la fase 1: deja duplicados (jamás pérdida) y jamás una key ecoada.
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let _ = validated_suffix(&from_dir, oe.path())?;
        }
        // Fase 1 — copia streaming (dirs = create_dir en destino; el
        // recursivo de services-fs incluye subdirs, el de S3 markers como
        // objetos con mode DIR — ambos van por create_dir).
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let Some(suffix) = validated_suffix(&from_dir, oe.path())? else {
                continue; // el propio dir listado
            };
            if oe.metadata().mode().is_dir() {
                self.op
                    .create_dir(&format!("{to_dir}{suffix}"))
                    .await
                    .map_err(|e| map_err(&e))?;
            } else {
                self.op
                    .copy(&format!("{from_dir}{suffix}"), &format!("{to_dir}{suffix}"))
                    .await
                    .map_err(|e| map_err(&e))?;
            }
        }
        self.op.create_dir(&to_dir).await.map_err(|e| map_err(&e))?;
        // Fase 2 — borrado con re-list: ficheros primero (batcheados),
        // dirs después en profundidad inversa (un dir de fs solo se borra
        // vacío; O(dirs) en memoria, la fracción diminuta del árbol).
        //
        // Guard del colado: un fichero solo se borra si su DESTINO existe —
        // una KEY NUEVA colada por otro cliente entre las dos fases no fue
        // copiada y queda SIN MOVER en el origen. OJO al overclaim (review
        // #49 MAJOR-2): una SOBRESCRITURA colada de una key YA copiada sí se
        // pierde (el dst v1 existe → se borra el src v2) — inherente al
        // rename no atómico; el conditional delete por ETag sería el fix
        // fino donde el backend lo soporte.
        //
        // Presupuesto de requests (documentado a propósito): n HEAD
        // secuenciales + n/1000 DeleteObjects + 3 sweeps de LIST. El HEAD
        // por fichero es el precio del guard; la ganancia de #49 es la
        // MEMORIA O(dirs) y el batch del delete, no menos round-trips.
        let mut lister = self
            .op
            .lister_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| map_err(&e))?;
        // Un `?` (o el drop del future, regla 3) suelta el `deleter` sin
        // close(): los deletes encolados sin flush se DESCARTAN — quedan
        // duplicados en el origen, jamás pérdida (el invariante de siempre).
        let mut deleter = self.op.deleter().await.map_err(|e| map_err(&e))?;
        let mut dirs: Vec<String> = Vec::new();
        while let Some(oe) = lister.try_next().await.map_err(|e| map_err(&e))? {
            let Some(suffix) = validated_suffix(&from_dir, oe.path())? else {
                continue;
            };
            if oe.metadata().mode().is_dir() {
                dirs.push(suffix.to_owned());
                continue;
            }
            let dst = format!("{to_dir}{suffix}");
            if !self.op.exists(&dst).await.map_err(|e| map_err(&e))? {
                // escape_debug: el sufijo es legal pero puede llevar
                // controles/ANSI — jamás crudo al log (convención VPath
                // redactado).
                tracing::warn!(
                    suffix = %suffix.escape_debug(),
                    "objeto colado durante el rename de prefijo: queda SIN MOVER en el origen"
                );
                continue;
            }
            deleter
                .delete(format!("{from_dir}{suffix}"))
                .await
                .map_err(|e| map_err(&e))?;
        }
        // Flush del batch de ficheros ANTES de tocar dirs (fs exige vacío).
        deleter.close().await.map_err(|e| map_err(&e))?;
        dirs.sort_by_key(|s| std::cmp::Reverse(s.len()));
        for suffix in dirs {
            self.op
                .delete(&format!("{from_dir}{suffix}"))
                .await
                .map_err(|e| map_err(&e))?;
        }
        self.op.delete(&from_dir).await.map_err(|e| map_err(&e))
    }

    async fn trash(&self, p: &VPath, id: &trash::TrashId) -> Result<Option<VPath>, Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // Entrada DETERMINISTA a partir del id del engine (#99). `plan` valida
        // `p` (rechaza papelerizar la propia papelera, ADR 0019) y da la raíz.
        let paths = trash::plan(p, &id.as_segment())?;
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;

        // Idempotencia: víctima ausente + payload determinista presente = esta
        // op ya aplicó en un intento transitorio anterior → devuelve el payload
        // (recupera el `reversal_ref`). Sin payload = `NotFound` genuino.
        match self.stat(p).await {
            Ok(_) => {}
            Err(Error::NotFound) => {
                return match self.stat(&paths.payload).await {
                    // Solo NUESTRA entrada (la `.norte-info` decodifica a `p`)
                    // se reclama; una ajena con el mismo id es colisión (review
                    // rust MAJOR).
                    Ok(_) if self.trash_info_matches(&paths.info, p).await => {
                        Ok(Some(paths.payload))
                    }
                    Ok(_) => Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    }),
                    Err(Error::NotFound) => Err(Error::NotFound),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        }

        self.ensure_dir_idempotent(&trash_root).await?;

        // La entrada `<id>/`: `Conflict::Exists` es NUESTRO parcial (info
        // ausente o decodifica a `p`) → sigue; una info AJENA con el mismo id
        // es colisión REAL (id fijo) → se propaga sin pisar sus metadatos
        // (review rust MAJOR).
        match self.mkdir(&paths.dir).await {
            Ok(()) => {}
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => match self.stat(&paths.info).await {
                Err(Error::NotFound) => {}
                Ok(_) if self.trash_info_matches(&paths.info, p).await => {}
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::Exists,
                    });
                }
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        }

        // `.norte-info` ANTES de mover: si el rename falla, el origen queda
        // intacto o recuperable (copiado a la papelera), nunca un payload sin
        // metadatos. `deleted_ms` del id (estable en cada reintento).
        let info = trash::info_encode(p, id.deleted_ms());
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Mueve el árbol reutilizando el rename AUDITADO: copy-all →
        // delete-all, keys reconstruidas desde sufijos validados (contención
        // de servidor hostil), sin pérdida ante interrupción (ADR 0019/0016).
        self.rename(p, &paths.payload).await?;
        // Papelera LÓGICA: el payload ES la ruta recuperable → reversal_ref.
        Ok(Some(paths.payload))
    }

    async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<(), Error>> {
        // Object storage SÍ tiene copia server-side (CopyObject) — el engine
        // la prefiere a leer+reescribir. Aplica solo a UN objeto (fichero);
        // el copy de un árbol lo orquesta el engine con list+copy_native por
        // hoja. `Some(_)`: el engine solo llama aquí con SERVER_COPY y src==dst
        // (mismo provider por puntero), así que la copia nativa siempre aplica;
        // un error se propaga tal cual (el engine NO cae a streaming).
        Some(self.copy_object(from, to).await)
    }
}

impl ObjectProvider {
    /// `CopyObject` de un fichero (ADR 0016 G). Destino existente → `Conflict`
    /// (jamás sobrescritura silenciosa, misma política que `write`). El
    /// `ensure_absent` previo NO es solo cinturón: `If-None-Match: *` sobre la
    /// key `to` NO ve un DIRECTORIO destino (marker `to/` ni prefijo con
    /// hijos), así que el sondeo file+dir de `stat_kind` es el ÚNICO guard
    /// contra copiar un fichero `to` que aliasa el dir `to/`. Para el destino
    /// FICHERO: con `copy_with_if_not_exists` es race-free; si el backend no
    /// lo soporta degrada a ese check (racy, mismo nivel aceptado en write/ftp).
    async fn copy_object(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_key = self.key(from)?;
        let to_key = self.key(to)?;
        if from_key.is_empty() || to_key.is_empty() {
            return Err(Error::InvalidPath);
        }
        // copy_native es de objeto único: un directorio origen es TypeMismatch
        // (el engine copia árboles hoja a hoja, nunca pasa un dir aquí).
        match self.stat_kind(&from_key).await? {
            None => return Err(Error::NotFound),
            Some((EntryKind::Dir, _)) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            Some(_) => {}
        }
        if !self.parent_dir_exists(to).await? {
            return Err(Error::NotFound);
        }
        // Cinturón: el stat-check upfront cubre los backends que ignoran
        // If-None-Match (degrada a nivel racy, jamás a sobrescritura).
        self.ensure_absent(&to_key).await?;
        if self.op.info().capability().copy_with_if_not_exists {
            self.op
                .copy_with(&from_key, &to_key)
                .if_not_exists(true)
                .await
                .map(|_| ())
                .map_err(|e| map_err(&e))
        } else {
            self.op
                .copy(&from_key, &to_key)
                .await
                .map(|_| ())
                .map_err(|e| map_err(&e))
        }
    }
}

/// Sink de escritura sobre object storage (ADR 0016 E). El staging invisible
/// es el propio multipart upload de opendal: las partes suben en `write`
/// (bufferizadas por chunk) y NADA existe en la key final hasta el
/// `CompleteMultipartUpload`/`PutObject` del `commit`. `abort` =
/// `AbortMultipartUpload` (o descartar el buffer). Sin `.norte-partial`
/// remoto; `keep` hereda el default del trait (= abort): el resume multipart
/// está diferido (ADR 0016 F).
///
/// El `writer` es `Option` para que [`Drop`] pueda EXTRAERLO y abortar el
/// multipart huérfano: un writer soltado sin `close`/`abort` deja las partes
/// ya subidas colgando en el bucket (invisibles pero facturables) y este
/// provider no tiene resume que las reencuentre — el contrato de `ByteSink`
/// exige limpieza best-effort en `Drop`.
struct ObjectSink {
    writer: Option<opendal::Writer>,
}

#[async_trait]
impl ByteSink for ObjectSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        let w = self.writer.as_mut().ok_or(Error::Io { retryable: false })?;
        w.write(chunk).await.map_err(|e| map_err(&e))
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        // If-None-Match viaja aquí: `ConditionNotMatch` → Conflict (el
        // destino apareció entre el stat-check del open y este commit).
        let mut w = self.writer.take().ok_or(Error::Io { retryable: false })?;
        w.close().await.map(|_| ()).map_err(|e| map_err(&e))
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        let mut w = self.writer.take().ok_or(Error::Io { retryable: false })?;
        w.abort().await.map_err(|e| map_err(&e))
    }
}

impl Drop for ObjectSink {
    fn drop(&mut self) {
        // Contrato de ByteSink: soltar sin commit/abort = abort best-effort.
        // El multipart no es un unlink síncrono (como local) sino una llamada
        // de red — se lanza en la runtime actual si la hay; sin runtime
        // (drop fuera de tokio) las partes quedan para la lifecycle rule
        // `AbortIncompleteMultipartUpload` del bucket (recomendada en 7d).
        if let Some(mut w) = self.writer.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = w.abort().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validated_suffix;
    use norte_proto::Error;

    /// #49: la contención del rename, caso a caso — el helper es la única
    /// puerta por la que un path ecoado se convierte en key de operación.
    #[test]
    fn validated_suffix_contiene_lo_hostil() {
        let d = "src/";
        // El propio dir listado: se salta, no es error.
        assert_eq!(validated_suffix(d, "src/"), Ok(None));
        // Legales: fichero, subdir (con `/` final), anidado.
        assert_eq!(validated_suffix(d, "src/a.txt"), Ok(Some("a.txt")));
        assert_eq!(validated_suffix(d, "src/sub/"), Ok(Some("sub/")));
        assert_eq!(validated_suffix(d, "src/sub/b"), Ok(Some("sub/b")));
        // Hostiles: fuera del prefijo, absoluto, traversal, segmento
        // vacío, lossy.
        for hostil in [
            "otra/x",
            "/etc/passwd",
            "src/../victima",
            "src/a/../b",
            "src//oculto",
            "src/./x",
            "src/caf\u{FFFD}.txt",
            "src",
        ] {
            assert_eq!(
                validated_suffix(d, hostil),
                Err(Error::InvalidPath),
                "{hostil}"
            );
        }
    }
}
