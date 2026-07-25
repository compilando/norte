//! [`LocalProvider`]: el filesystem local detrás del contrato [`Provider`].
//!
//! Regla dura 2 de `CLAUDE.md`: NADA de I/O bloqueante en contexto async —
//! toda syscall va por `spawn_blocking`; los streams entregan por canal
//! `mpsc` acotado (64), así el hilo bloqueante se libera entre chunks y
//! soltar el stream cancela el productor.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio_stream::wrappers::ReceiverStream;

use crate::native_path::{os_to_bytes, to_native};

/// Tamaño de chunk de lectura (alineado con el copy engine: 256 KiB).
const READ_CHUNK: usize = 256 * 1024;

/// Provider del filesystem local, enraizado en un directorio nativo.
///
/// El `VPath` raíz (`file:///`) se mapea a `base`; cada segmento es un
/// componente nativo (bytes intactos; en Windows, WTF-8 validado en la
/// frontera y paths SIEMPRE con prefijo `\\?\`).
///
/// Las capabilities se sondean LAZY, al inicio de la primera operación
/// async y dentro de `spawn_blocking` (issue #5 + regla 2): construir el
/// provider jamás muta `base`, y el runtime jamás se bloquea con la sonda.
/// Si el sondeo no puede decidir (base no escribible y sin API de
/// plataforma) cae al default del OS.
pub struct LocalProvider {
    base: PathBuf,
    caps: std::sync::Arc<std::sync::OnceLock<Capabilities>>,
    /// Mantiene vivo un recurso externo (p. ej. el `TempDir` de un test).
    _guard: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl LocalProvider {
    /// Provider enraizado en `base` (debe ser un directorio existente).
    ///
    /// No sondea nada: el sondeo de capabilities es lazy (arranca la primera
    /// operación async, en `spawn_blocking`, una sola vez).
    #[must_use]
    pub fn rooted(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        // Verbatim (`\\?\`) exige path absoluto y normalizado; en Windows,
        // `absolute` usa GetFullPathNameW (separadores y `..` resueltos).
        let base = std::path::absolute(&base).unwrap_or(base);
        Self {
            base,
            caps: std::sync::Arc::new(std::sync::OnceLock::new()),
            _guard: None,
        }
    }

    /// Sondea capabilities UNA vez, dentro de `spawn_blocking` (regla 2:
    /// nada de I/O bloqueante en el runtime): lo llama cada operación async
    /// antes de tocar el FS. Sondeadas ya, es una lectura atómica gratis.
    async fn ensure_caps(&self) {
        if self.caps.get().is_some() {
            return;
        }
        let caps = std::sync::Arc::clone(&self.caps);
        let base = self.base.clone();
        let _ = blocking(move || {
            caps.get_or_init(|| probe_capabilities(&base));
            Ok(())
        })
        .await;
    }

    /// Adjunta un guard que vive tanto como el provider (para tests que
    /// enraízan en un `TempDir`).
    #[doc(hidden)]
    #[must_use]
    #[allow(clippy::used_underscore_binding)] // el campo existe solo por su Drop
    pub fn with_guard(mut self, guard: Box<dyn std::any::Any + Send + Sync>) -> Self {
        self._guard = Some(guard);
        self
    }

    /// Provider que sirve TODO el filesystem del OS: unix se enraíza en `/`;
    /// Windows usa base vacía (el primer segmento del `VPath` es la unidad,
    /// p. ej. `C:`) y capabilities por defecto del OS sin sondeo (la raíz no
    /// es escribible y la sensibilidad varía por volumen).
    #[must_use]
    pub fn os_root() -> Self {
        if cfg!(windows) {
            let s = Self {
                base: PathBuf::new(),
                caps: std::sync::Arc::new(std::sync::OnceLock::new()),
                _guard: None,
            };
            let _ = s.caps.set(Capabilities {
                flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
                max_path: Some(32767),
            });
            s
        } else {
            let s = Self::rooted("/");
            // La raíz del OS no se sondea (corriendo como root, la sonda se
            // ESCRIBIRÍA en `/`): defaults del OS, como la rama Windows.
            let _ = s.caps.set(default_capabilities());
            s
        }
    }

    /// La raíz de este provider: `file:///`.
    ///
    /// # Panics
    /// Nunca: el scheme es constante y válido.
    #[must_use]
    pub fn root() -> VPath {
        VPath::root(Scheme::new("file").expect("scheme constante válido"), None)
    }

    fn native(&self, p: &VPath) -> Result<PathBuf, Error> {
        // Este provider solo sirve `file://` sin authority: cualquier otra
        // cosa es un path de OTRO provider — servirlo sería corrupción.
        if p.scheme() != "file" || p.authority().is_some() {
            return Err(Error::InvalidPath);
        }
        to_native(&self.base, p)
    }
}

/// Papelera nativa. macOS: `NSFileManager` (headless, sin prompts TCC) —
/// el default del crate sería Finder vía osascript: colgaría la task en
/// un prompt de Automation y muere en CI (hallazgo B1, ADR 0009).
#[cfg(target_os = "macos")]
fn trash_delete(p: &Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(p)
}

/// Papelera nativa (freedesktop / Recycle Bin).
#[cfg(not(target_os = "macos"))]
fn trash_delete(p: &Path) -> Result<(), trash::Error> {
    trash::delete(p)
}

/// Contador de staging: junto al pid hace único el nombre del `.norte-partial`
/// (dos writes al mismo destino jamás comparten staging, y un archivo REAL
/// del usuario llamado `x.norte-partial` jamás se toca).
static PARTIAL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Prefijo de todo staging de norte (write efímero y resume estable): lo
/// usa el GC para reconocer parciales (ADR 0012).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// Longitud (en chars hex) del hash del nombre estable: 32 = 128 bits.
const STABLE_HASH_HEX: usize = 32;

/// Path del staging ESTABLE de resume para el destino `p`: mismo
/// directorio, nombre `.norte-partial.<sha256-128-del-nombre-final>` — 47
/// bytes (no roza `NAME_MAX`) y reencontrable entre invocaciones Y entre
/// versiones de Rust. SHA-256 truncado a 128 bits: colisión accidental
/// imposible (birthday 2^64) y adversarial 2^64 (nombres desde un archivo
/// no confiable) — hallazgo H1/H3 del encoding-auditor. Hashea los BYTES
/// crudos del nombre (regla 1), jamás lo decodifica.
fn stable_partial_vpath(p: &VPath) -> Result<VPath, Error> {
    use sha2::{Digest, Sha256};
    let name = p.file_name().ok_or(Error::InvalidPath)?;
    let digest = Sha256::digest(name.as_bytes());
    let mut hex = String::with_capacity(STABLE_HASH_HEX);
    for b in &digest[..STABLE_HASH_HEX / 2] {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    let partial_name = format!("{PARTIAL_PREFIX}{hex}").into_bytes();
    let seg = Segment::new(partial_name).map_err(|_| Error::InvalidPath)?;
    p.with_file_name(seg).ok_or(Error::InvalidPath)
}

/// ¿`name` (bytes) tiene la FORMA de un staging de norte? Estrecho a las
/// dos formas conocidas — NO al prefijo suelto (H2 del encoding-auditor:
/// un archivo real del usuario `.norte-partial.backup` NO debe barrerse):
/// - estable: prefijo + exactamente 32 hex.
/// - efímero: prefijo + 16 hex + `.` + <pid> + `-` + <seq>.
fn is_norte_partial(name: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(PARTIAL_PREFIX.as_bytes()) else {
        return false;
    };
    let is_hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    // Estable: 32 hex y nada más.
    if rest.len() == STABLE_HASH_HEX && rest.iter().all(is_hex) {
        return true;
    }
    // Efímero: <16 hex>.<pid>-<seq>, todo dígitos/hex y separadores.
    let Some(dot) = rest.iter().position(|&b| b == b'.') else {
        return false;
    };
    let (hash, tail) = rest.split_at(dot);
    if hash.len() != 16 || !hash.iter().all(is_hex) {
        return false;
    }
    // tail = ".<pid>-<seq>": dígitos, un '-', dígitos.
    let tail = &tail[1..];
    let Some(dash) = tail.iter().position(|&b| b == b'-') else {
        return false;
    };
    let (pid, seq) = tail.split_at(dash);
    !pid.is_empty()
        && pid.iter().all(u8::is_ascii_digit)
        && seq.len() > 1
        && seq[1..].iter().all(u8::is_ascii_digit)
}

/// Mapea un error de OS a la taxonomía del protocolo (spec §17.7): los
/// frontends renderizan por categoría, jamás parsean strings de OS.
fn map_io(e: &std::io::Error) -> Error {
    use std::io::ErrorKind as K;
    // EILSEQ: el FS rechaza los BYTES del nombre (APFS exige UTF-8 válido).
    // std lo deja en `Uncategorized`, así que se mira el errno crudo. Con el
    // staging corto (issue #4) este rechazo llega en el rename de commit —
    // sin este mapeo sería un `Io` opaco (regresión cazada en CI de macOS).
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::EILSEQ) {
        return Error::InvalidPath;
    }
    match e.kind() {
        K::NotFound => Error::NotFound,
        K::PermissionDenied => Error::PermissionDenied,
        K::AlreadyExists => Error::Conflict {
            conflict: ConflictKind::Exists,
        },
        K::DirectoryNotEmpty | K::NotADirectory | K::IsADirectory => Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        },
        K::StorageFull | K::QuotaExceeded => Error::NoSpace,
        // EXDEV: el FS no puede renombrar entre dispositivos — Unsupported
        // dispara la degradación del move a copy+delete en el engine.
        K::CrossesDevices => Error::Unsupported,
        // InvalidFilename = ENAMETOOLONG / nombre inválido para el FS:
        // problema del PATH (el frontend debe decir "nombre demasiado
        // largo", no "error de I/O").
        K::InvalidFilename | K::InvalidInput => Error::InvalidPath,
        K::Interrupted | K::TimedOut | K::WouldBlock => Error::Io { retryable: true },
        _ => Error::Io { retryable: false },
    }
}

/// Corre un productor de stream en `spawn_blocking` protegido contra panics:
/// un panic a mitad NO puede pasar por fin-de-stream limpio (sería un listado
/// o lectura truncados en silencio) — el consumidor recibe `Internal{panic}`.
fn spawn_guarded_producer<T: Send + 'static>(
    tx: tokio::sync::mpsc::Sender<Result<T, Error>>,
    body: impl FnOnce(&tokio::sync::mpsc::Sender<Result<T, Error>>) + Send + 'static,
) {
    tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&tx)));
        if result.is_err() {
            let _ = tx.blocking_send(Err(Error::Internal { panic: true }));
        }
    });
}

/// Ejecuta I/O bloqueante; un panic dentro se supervisa y NO tumba el proceso
/// (política de panics de la spec §17.7).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::Internal { panic: true })?
}

fn mtime_ms(md: &std::fs::Metadata) -> Option<i64> {
    let t = md.modified().ok()?;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok(),
        Err(e) => i64::try_from(e.duration().as_millis()).ok().map(|v| -v),
    }
}

fn entry_from(path: VPath, md: &std::fs::Metadata) -> Entry {
    let ft = md.file_type();
    let (kind, size) = if ft.is_symlink() {
        (EntryKind::Symlink, None)
    } else if ft.is_dir() {
        (EntryKind::Dir, None)
    } else if ft.is_file() {
        (EntryKind::File, Some(md.len()))
    } else {
        (EntryKind::Other, None)
    };
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path,
        kind,
        size,
        mtime_ms: mtime_ms(md),
    }
}

/// Ante una colisión ya confirmada: ¿el nombre EXACTO (bytes) está en el
/// directorio, una variante de normalización Unicode (macOS NFD, issue #8)
/// o una variante de caja? La colisión se evalúa contra el FS destino.
fn collision_kind_for(path: &Path) -> ConflictKind {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return ConflictKind::Exists;
    };
    match std::fs::read_dir(parent) {
        Ok(rd) => {
            // Precedencia: byte-exacto > caja > normalización — en NTFS
            // (insensible a caja, sensible a normalización) el EEXIST real
            // viene de la variante de caja aunque haya un dirent NFD cerca.
            let mut case_hit = false;
            let mut norm_hit = false;
            for d in rd.flatten() {
                let dn = d.file_name();
                if dn == name {
                    return ConflictKind::Exists;
                }
                if !case_hit && case_eq_os(&dn, name) {
                    case_hit = true;
                }
                if !norm_hit && nfc_eq_os(&dn, name) {
                    norm_hit = true;
                }
            }
            if case_hit {
                ConflictKind::CaseCollision
            } else if norm_hit {
                ConflictKind::Normalization
            } else {
                ConflictKind::CaseCollision
            }
        }
        Err(_) => ConflictKind::Exists,
    }
}

/// ¿Variante solo-de-caja? (lowercase Unicode de std; el fold real del FS
/// puede ser más ancho — suficiente como etiqueta para el frontend).
fn case_eq_os(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a != b && a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// ¿Misma forma NFC? Solo comparable si ambos nombres son UTF-8 válido
/// (la normalización no está definida sobre bytes arbitrarios).
fn nfc_eq_os(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    use unicode_normalization::UnicodeNormalization;
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.nfc().eq(b.nfc()),
        _ => false,
    }
}

/// Contador de sondas: junto al pid hace único el nombre de cada sonda de
/// caja (restos de un crash o archivos del usuario jamás interfieren).
static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sensibilidad a la caja vía API del OS, sin mutar nada: `pathconf`
/// `_PC_CASE_SENSITIVE` (macOS; por-volumen). `None` = indeterminado.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn case_sensitivity_from_os(base: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(base.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` es una CString NUL-terminada viva durante toda la llamada;
    // `_PC_CASE_SENSITIVE` es constante de la ABI. El resultado se valida en
    // `tests/local.rs::capabilities_are_probed` sobre el FS real de CI.
    let rc = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    match rc {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Resto de OS: sin API fiable — se usa la sonda de escritura.
#[cfg(not(target_os = "macos"))]
fn case_sensitivity_from_os(_base: &Path) -> Option<bool> {
    None
}

/// Sondeo de sensibilidad por escritura: crea una sonda de nombre ÚNICO
/// (pid + secuencia) terminada en `-A` y comprueba si la variante `-a`
/// resuelve al MISMO archivo — identidad `(dev, ino)`, no `exists()`: un
/// archivo ajeno homónimo o un symlink mentirían (issue #5).
/// `None` si `base` no es escribible o el sondeo es indeterminado.
fn probe_case_sensitivity(base: &Path) -> Option<bool> {
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    // verbatim: la sonda debe funcionar también bajo paths >260 en Windows
    // (misma promesa que el resto del provider).
    let upper = crate::native_path::verbatim(base.join(format!(".norte-probe-{pid}-{seq}-A")));
    let lower = crate::native_path::verbatim(base.join(format!(".norte-probe-{pid}-{seq}-a")));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&upper)
        .ok()?;
    let sensitive = match (file.metadata(), std::fs::symlink_metadata(&lower)) {
        (Ok(upper_md), Ok(lower_md)) => Some(!probe_same_file(&upper_md, &lower_md)),
        (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
        _ => None,
    };
    drop(file);
    let _ = std::fs::remove_file(&upper);
    sensitive
}

/// ¿La variante en minúscula de la sonda ES el propio archivo de sonda?
#[cfg(unix)]
fn probe_same_file(upper_md: &std::fs::Metadata, lower_md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    upper_md.dev() == lower_md.dev() && upper_md.ino() == lower_md.ino()
}

/// Windows: std no expone la identidad real, pero la sonda es de nombre
/// único (pid + secuencia) — que la variante "exista" ya significa que el
/// FS pliega la caja. Suficiente aquí; no lo sería para `rename`.
#[cfg(windows)]
fn probe_same_file(_upper_md: &std::fs::Metadata, _lower_md: &std::fs::Metadata) -> bool {
    true
}

/// Identidad real del nodo en unix: `(dev, ino)` de lstat/stat según
/// `follow`. Es la misma identidad que ya usan la sonda de caja y el
/// case-rename (`same_node`) — aquí se expone por el trait (issue #16).
#[cfg(unix)]
fn node_id_native(
    p: &Path,
    follow: norte_vfs::FollowLinks,
) -> Result<Option<norte_vfs::NodeId>, Error> {
    use std::os::unix::fs::MetadataExt;
    let md = match follow {
        norte_vfs::FollowLinks::No => std::fs::symlink_metadata(p),
        norte_vfs::FollowLinks::Yes => std::fs::metadata(p),
    }
    .map_err(|e| map_io(&e))?;
    Ok(Some(norte_vfs::NodeId {
        volume: md.dev(),
        index: u128::from(md.ino()),
    }))
}

/// Identidad real del nodo en Windows: `FILE_ID_INFO` (serial de volumen
/// u64 + FileId de 128 bits, cubre ReFS) vía `GetFileInformationByHandleEx`.
/// Si el volumen no lo soporta (FAT32, SMB antiguo), degrada a `Ok(None)` —
/// "no hay identidad estable aquí" es la respuesta honesta del contrato,
/// jamás un id inventado.
#[cfg(windows)]
#[allow(unsafe_code)]
fn node_id_native(
    p: &Path,
    follow: norte_vfs::FollowLinks,
) -> Result<Option<norte_vfs::NodeId>, Error> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128, FILE_ID_INFO,
        FileIdInfo, GetFileInformationByHandleEx,
    };

    // access_mode 0 = solo consultar metadatos (ni read ni write: funciona
    // incluso sin permiso de lectura). BACKUP_SEMANTICS es obligatorio para
    // abrir directorios; OPEN_REPARSE_POINT da la identidad del PROPIO link
    // (semántica lstat) cuando follow = No.
    let mut opts = std::fs::OpenOptions::new();
    opts.access_mode(0);
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if follow == norte_vfs::FollowLinks::No {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    opts.custom_flags(flags);
    let file = opts.open(p).map_err(|e| map_io(&e))?;

    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    // SAFETY: el handle es válido y vive durante toda la llamada (file no se
    // suelta antes); el buffer es exactamente un FILE_ID_INFO y el tamaño
    // pasado es size_of del mismo tipo. Contrato verificado en el test
    // `node_id_identifica_el_mismo_archivo` (y la suite contractual de
    // node_id) sobre el FS real de la CI de Windows.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).expect("tamaño fijo pequeño"),
        )
    };
    if ok == 0 {
        // El volumen no sabe dar FileId de 128 bits: sin identidad estable.
        return Ok(None);
    }
    Ok(Some(norte_vfs::NodeId {
        volume: info.VolumeSerialNumber,
        index: u128::from_le_bytes(info.FileId.Identifier),
    }))
}

/// Kind efectivo de un symlink a crear cuando el caller pasó `Unknown`
/// (issue #18): resuelve el target RELATIVO AL PADRE del link en este FS,
/// siguiendo la cadena (metadata). Target roto o indeterminable degrada a
/// `File` — el mismo default que un `mklink` sin `/D`. Solo Windows lo
/// consulta (unix ignora el kind); se compila en todos los OS para poder
/// testearlo en cualquier CI.
#[cfg_attr(unix, allow(dead_code))]
fn effective_symlink_kind(
    link: &Path,
    target: &std::ffi::OsStr,
    kind: norte_vfs::SymlinkKind,
) -> norte_vfs::SymlinkKind {
    match kind {
        norte_vfs::SymlinkKind::Unknown => {
            let resolved = match link.parent() {
                // join con target absoluto LO respeta (semántica de Path).
                Some(parent) => parent.join(target),
                None => std::path::PathBuf::from(target),
            };
            // `link` llega verbatim (`\\?\`) en Windows y bajo verbatim el
            // kernel NO pliega `..` ni convierte `/`: normalizar léxicamente
            // (GetFullPathNameW vía `absolute`) y re-aplicar verbatim antes
            // de sondear, o un target relativo con `..` degradaría a File
            // aunque apunte a un dir (hallazgo del encoding-auditor).
            // Target drive-relative (`C:foo`): irresoluble sin el CWD de
            // aquella unidad — degrada a File, documentado.
            let resolved =
                crate::native_path::verbatim(std::path::absolute(&resolved).unwrap_or(resolved));
            match std::fs::metadata(&resolved) {
                Ok(md) if md.is_dir() => norte_vfs::SymlinkKind::Dir,
                _ => norte_vfs::SymlinkKind::File,
            }
        }
        explicit => explicit,
    }
}

/// Crea el symlink nativo. Pre-chequeo de colisión no hace falta: el
/// syscall falla con EEXIST atómicamente.
#[cfg(unix)]
fn make_symlink(
    target: &std::ffi::OsStr,
    link: &Path,
    _kind: norte_vfs::SymlinkKind,
) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, link).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Conflict {
                conflict: collision_kind_for(link),
            }
        } else {
            map_io(&e)
        }
    })
}

/// Windows distingue archivo/dir en la creación; exige privilegio
/// (SeCreateSymbolicLinkPrivilege o Developer Mode) — por eso el provider
/// no declara `SYMLINKS` en Windows y este camino responde vía
/// `Unsupported` antes de llegar aquí salvo sondeos futuros.
#[cfg(windows)]
fn make_symlink(
    target: &std::ffi::OsStr,
    link: &Path,
    kind: norte_vfs::SymlinkKind,
) -> Result<(), Error> {
    let res = match effective_symlink_kind(link, target, kind) {
        norte_vfs::SymlinkKind::Dir => std::os::windows::fs::symlink_dir(target, link),
        // `Unknown` ya quedó resuelto arriba; el brazo existe por exhaustividad.
        norte_vfs::SymlinkKind::File | norte_vfs::SymlinkKind::Unknown => {
            std::os::windows::fs::symlink_file(target, link)
        }
    };
    res.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Conflict {
                conflict: collision_kind_for(link),
            }
        } else {
            map_io(&e)
        }
    })
}

/// Capabilities por defecto del OS, sin tocar el FS: lo que responde
/// `capabilities()` si aún no corrió ninguna operación async.
fn default_capabilities() -> Capabilities {
    let mut flags = CapabilityFlags::RENAME_ATOMIC
        | CapabilityFlags::CASE_PRESERVING
        // FS local: escritura en offset arbitrario y append (resume M2).
        | CapabilityFlags::APPEND
        | CapabilityFlags::RANDOM_WRITE
        // Papelera nativa en los 3 OS (crate trash, ADR 0009).
        | CapabilityFlags::TRASH;
    if cfg!(unix) {
        // Crear symlinks en Windows exige privilegio: no se declara en M0.
        flags |= CapabilityFlags::SYMLINKS;
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        flags |= CapabilityFlags::CASE_SENSITIVE;
    }
    Capabilities {
        flags,
        // Con prefijo verbatim, el límite real de Windows es 32767 UTF-16.
        max_path: cfg!(windows).then_some(32767),
    }
}

fn probe_capabilities(base: &Path) -> Capabilities {
    let mut caps = default_capabilities();
    let default_sensitive = caps.flags.contains(CapabilityFlags::CASE_SENSITIVE);
    let sensitive = case_sensitivity_from_os(base)
        .or_else(|| probe_case_sensitivity(base))
        .unwrap_or(default_sensitive);
    caps.flags.set(CapabilityFlags::CASE_SENSITIVE, sensitive);
    caps
}

#[async_trait]
impl Provider for LocalProvider {
    // La firma del trait es `-> &str`; devolver un literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "file"
    }

    fn capabilities(&self) -> Capabilities {
        // Lectura pura (regla 2: aquí no se puede hacer I/O — esto se llama
        // desde contexto async). Exactas tras la primera operación async
        // (el sondeo corre ahí, en spawn_blocking); antes, default del OS.
        self.caps
            .get()
            .copied()
            .unwrap_or_else(default_capabilities)
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let vpath = p.clone();
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            Ok(entry_from(vpath, &md))
        })
        .await
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        // Validación previa síncrona: NotFound / no-dir se devuelven en el
        // Result, no como primer item del stream. `metadata` SIGUE symlinks
        // (semántica opendir): listar un dir-symlink lista su target, y un
        // link roto es NotFound — igual que el FS real por debajo.
        {
            let probe = native.clone();
            blocking(move || {
                let md = std::fs::metadata(&probe).map_err(|e| map_io(&e))?;
                if md.is_dir() {
                    Ok(())
                } else {
                    Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    })
                }
            })
            .await?;
        }
        let base_vpath = p.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Entry, Error>>(64);
        spawn_guarded_producer(tx, move |tx| {
            let rd = match std::fs::read_dir(&native) {
                Ok(rd) => rd,
                Err(e) => {
                    let _ = tx.blocking_send(Err(map_io(&e)));
                    return;
                }
            };
            for dent in rd {
                let item = dent.map_err(|e| map_io(&e)).and_then(|d| {
                    let seg = Segment::new(os_to_bytes(&d.file_name()))
                        .map_err(|_| Error::InvalidPath)?;
                    // #52: kind por d_type del readdir (std solo statea con
                    // DT_UNKNOWN); size/mtime LAZY (None = «no lo sé»,
                    // contrato de Entry) — el copy engine hidrata sus hojas
                    // (hydrate_plan) y la UI sondea la enfocada.
                    let ft = d.file_type().map_err(|e| map_io(&e))?;
                    let kind = if ft.is_symlink() {
                        EntryKind::Symlink
                    } else if ft.is_dir() {
                        EntryKind::Dir
                    } else if ft.is_file() {
                        EntryKind::File
                    } else {
                        EntryKind::Other
                    };
                    Ok(Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: base_vpath.join(seg),
                        kind,
                        size: None,
                        mtime_ms: None,
                    })
                });
                let stop = item.is_err();
                if tx.blocking_send(item).is_err() {
                    // Receptor soltado: cancelación cooperativa del listado.
                    return;
                }
                if stop {
                    return;
                }
            }
        });
        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<ByteStream, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let file = blocking(move || {
            // `metadata` SIGUE symlinks (semántica open): leer un
            // dir-symlink es TypeMismatch — el sondeo del copy engine
            // distingue así archivo/dir — y un link roto es NotFound.
            let md = std::fs::metadata(&native).map_err(|e| map_io(&e))?;
            if md.is_dir() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            // No-regulares (FIFO/socket/device): el open puede BLOQUEAR el
            // hilo indefinidamente (una FIFO sin escritor) y la cancelación
            // no puede interrumpirlo (regla 3) — rechazo honesto ANTES del
            // open. El engine los trata como Other/Unsupported igualmente.
            if !md.is_file() {
                return Err(Error::Unsupported);
            }
            let mut file = std::fs::File::open(&native).map_err(|e| map_io(&e))?;
            if let Some(r) = range {
                use std::io::Seek;
                // Semántica pread (ADR 0005): offset pasado de EOF no es
                // error — el stream simplemente termina vacío.
                file.seek(std::io::SeekFrom::Start(r.offset))
                    .map_err(|e| map_io(&e))?;
            }
            Ok(file)
        })
        .await?;
        // `None` = sin límite (hasta EOF).
        let mut remaining: Option<u64> = range.and_then(|r| r.len);
        // Buffer de 8 chunks: 2 MiB máximos retenidos si el consumidor se
        // atasca (con blocking_send el productor espera igual de bien).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Error>>(8);
        spawn_guarded_producer(tx, move |tx| {
            use std::io::Read;
            let mut file = file;
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                let want = match remaining {
                    Some(0) => return,
                    // INVARIANTE: min(n, READ_CHUNK=256Ki) siempre cabe.
                    Some(n) => usize::try_from(n.min(READ_CHUNK as u64))
                        .expect("min con READ_CHUNK cabe en usize"),
                    None => READ_CHUNK,
                };
                match file.read(&mut buf[..want]) {
                    Ok(0) => return,
                    Ok(n) => {
                        if let Some(rem) = &mut remaining {
                            *rem -= n as u64;
                        }
                        if tx
                            .blocking_send(Ok(Bytes::copy_from_slice(&buf[..n])))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let _ = tx.blocking_send(Err(map_io(&e)));
                        return;
                    }
                }
            }
        });
        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn node_id(
        &self,
        p: &VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || node_id_native(&native, follow)).await
    }

    async fn trash(&self, p: &VPath) -> Result<Option<VPath>, Error> {
        self.ensure_caps().await;
        if !self.capabilities().flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        let native = self.native(p)?;
        blocking(move || {
            // Existencia primero: el crate trash da errores variopintos.
            // (TOCTOU cosmético: si la víctima desaparece entre el stat y
            // el delete, saldrá PermissionDenied en vez de NotFound.)
            std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            trash_delete(&native).map_err(|e| match e {
                trash::Error::CouldNotAccess { .. } => Error::PermissionDenied,
                trash::Error::TargetedRoot => Error::InvalidPath,
                // "Sin papelera utilizable AQUÍ" (mount sin topdir, sin
                // $HOME…): Unsupported — el TUI reofrece PERMANENTE con
                // aviso (ADR 0009). Variantes del crate stringly: Unknown
                // es su cajón para "no pude"; upstream además panickea con
                // /proc/mounts no-UTF8 (contenido por `blocking()` como
                // Internal{panic}).
                trash::Error::Unknown { .. } => Error::Unsupported,
                _ => Error::Io { retryable: false },
            })
        })
        .await?;
        // Papelera NATIVA del OS: no exponemos una ruta estable de destino; el
        // handle de restauración se resuelve en el undo (M3-2, ADR 0009).
        Ok(None)
    }

    /// Restaura desde la papelera nativa del OS el ítem cuya ruta ORIGINAL es
    /// `original` (undo M3-2). Lista la papelera (`os_limited`), casa por ruta
    /// original el ítem más reciente (desempate estable por id) y lo restaura.
    /// Estricto: si el destino ya existe, `Conflict` (jamás pisa).
    ///
    /// SOLO freedesktop (Linux/BSD): la papelera guarda el parent CANONICALIZADO
    /// (symlinks resueltos), así que se canoniza el parent de `original` antes de
    /// casar; sin ello, una raíz colgada de un symlink nunca acertaría. Windows
    /// (prefijo verbatim vs `C:\` del shell) y macOS/iOS/Android quedan
    /// `Unsupported` por el default del trait (deuda: restore Windows/macOS).
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(original)?;
        blocking(move || {
            // Destino LIBRE (estricto: jamás pisa). symlink_metadata NO sigue el
            // link (un symlink colgante en el destino ES «ocupado»), coherente
            // con `trash()`.
            if native.symlink_metadata().is_ok() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            // La papelera almacena `parent.canonicalize().join(name)`: casa contra
            // esa MISMA forma o el match falla si la raíz cuelga de un symlink.
            let name = native.file_name().ok_or(Error::InvalidPath)?;
            let parent = native.parent().ok_or(Error::InvalidPath)?;
            let target = parent.canonicalize().map_err(|e| map_io(&e))?.join(name);

            let items = trash::os_limited::list().map_err(|_| Error::Unsupported)?;
            let pick = items
                .into_iter()
                .filter(|it| it.original_path() == target)
                // Más reciente; desempate por id → determinista bajo empate de
                // segundo (deuda: resolución de 1s no distingue trash-recrea-trash
                // en el mismo segundo — capturar el id al tirar sería exacto).
                .max_by_key(|it| (it.time_deleted, it.id.clone()))
                .ok_or(Error::NotFound)?;
            trash::os_limited::restore_all([pick]).map_err(|e| match e {
                trash::Error::RestoreCollision { .. } => Error::Conflict {
                    conflict: ConflictKind::Exists,
                },
                trash::Error::CouldNotAccess { .. } => Error::PermissionDenied,
                _ => Error::Io { retryable: false },
            })
        })
        .await
    }

    /// GC de `.norte-partial` huérfanos en el directorio `dir` (ADR 0012, #11):
    /// borra los staging cuya última modificación es anterior a `older_than`.
    /// Reconoce los parciales por su FORMA exacta (`is_norte_partial`), no por
    /// el prefijo suelto — un archivo real del usuario `.norte-partial.backup`
    /// JAMÁS se toca (H2 del encoding-auditor). Devuelve cuántos borró.
    ///
    /// No distingue un parcial de una copia VIVA (esa correlación es del
    /// journal M3): usar un `older_than` holgado (horas) para no barrer una
    /// reanudación en curso. Es una operación puntual, no una Task.
    ///
    /// # Errors
    /// [`Error`] si `dir` no se puede listar; los fallos de borrado
    /// individuales se cuentan como no-borrados, sin abortar el barrido.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        self.ensure_caps().await;
        let native = self.native(dir)?;
        blocking(move || {
            let now = std::time::SystemTime::now();
            let rd = std::fs::read_dir(&native).map_err(|e| map_io(&e))?;
            let mut removed = 0usize;
            for dent in rd.flatten() {
                let name = dent.file_name();
                if !is_norte_partial(&os_to_bytes(&name)) {
                    continue;
                }
                // Edad por mtime; sin metadata legible, se deja (conservador).
                let old = dent
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age >= older_than);
                if old && std::fs::remove_file(dent.path()).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            if !md.file_type().is_symlink() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            let target = std::fs::read_link(&native).map_err(|e| map_io(&e))?;
            // Bytes CRUDOS del target (regla 1): jamás String ni VPath.
            Ok(os_to_bytes(target.as_os_str()))
        })
        .await
    }

    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.ensure_caps().await;
        if !self
            .capabilities()
            .flags
            .contains(CapabilityFlags::SYMLINKS)
        {
            return Err(Error::Unsupported);
        }
        let native = self.native(link)?;
        let target = crate::native_path::link_target_to_os(target)?;
        blocking(move || make_symlink(&target, &native, kind)).await
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        self.ensure_caps().await;
        let final_native = self.native(p)?;
        // Staging junto al destino, con nombre CORTO y único:
        // `.norte-partial.<hash>.<pid>-<n>`. Corto porque NO deriva del
        // nombre final (242–255 bytes son legales en ext4/APFS/NTFS y un
        // sufijo daría ENAMETOOLONG, issue #4) sino de su hash. Único por
        // pid + secuencia: (a) un archivo real del usuario jamás se toca
        // (create_new además lo garantiza) y (b) dos writes concurrentes al
        // mismo destino no comparten staging. El prefijo `.norte-partial` lo
        // hace reconocible para el GC del journal (M3).
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            name.as_bytes().hash(&mut h);
            h.finish()
        };
        let seq = PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let partial_name =
            format!(".norte-partial.{hash:016x}.{}-{seq}", std::process::id()).into_bytes();
        let partial_seg = Segment::new(partial_name).map_err(|_| Error::InvalidPath)?;
        let partial_vpath = p.with_file_name(partial_seg).ok_or(Error::InvalidPath)?;
        let partial_native = self.native(&partial_vpath)?;

        let (file, partial_native, final_native) = blocking(move || {
            match std::fs::symlink_metadata(&final_native) {
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: collision_kind_for(&final_native),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(&e)),
            }
            // create_new: si aun así existe algo con este nombre, error antes
            // que tocar un archivo ajeno.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial_native)
                .map_err(|e| map_io(&e))?;
            Ok((file, partial_native, final_native))
        })
        .await?;

        Ok(Box::new(LocalSink {
            file: Some(file),
            partial: partial_native,
            final_path: final_native,
            done: false,
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use std::io::Read as _;

        use sha2::{Digest, Sha256};
        self.ensure_caps().await;
        // Mismo staging estable que open_resumable (#35): SHA-256 de sus
        // primeros `len` bytes. I/O síncrono en spawn_blocking (regla 2).
        let partial_native = self.native(&stable_partial_vpath(p)?)?;
        blocking(move || {
            let file = match std::fs::File::open(&partial_native) {
                Ok(f) => f,
                // Sin staging = sin digest (el engine degrada a Length).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(map_io(&e)),
            };
            let mut reader = file.take(len);
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut seen: u64 = 0;
            loop {
                let n = reader.read(&mut buf).map_err(|e| map_io(&e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                seen += n as u64;
            }
            // El staging es más corto que `len` (raro: `len` viene de
            // open_resumable): sin prefijo completo, degrada a Length.
            if seen < len {
                return Ok(None);
            }
            Ok(Some(hasher.finalize().into()))
        })
        .await
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        self.ensure_caps().await;
        let final_native = self.native(p)?;
        // Staging con nombre ESTABLE por destino (ADR 0012): sin pid+seq,
        // así una segunda invocación lo reencuentra y REANUDA. Sigue siendo
        // corto (deriva del hash del nombre final, no del nombre) para no
        // rozar NAME_MAX (issue #4). Prefijo `.norte-partial` reconocible
        // para el GC.
        let partial_vpath = stable_partial_vpath(p)?;
        let partial_native = self.native(&partial_vpath)?;

        let (file, already, partial_native, final_native) = blocking(move || {
            // El destino final NO debe existir todavía (mismo contrato que
            // write): si existe, la política de colisión es del core.
            match std::fs::symlink_metadata(&final_native) {
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: collision_kind_for(&final_native),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(&e)),
            }
            // Abre (o crea) el parcial en APPEND: si ya había bytes de una
            // copia previa, se reanuda tras ellos.
            let file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&partial_native)
                .map_err(|e| map_io(&e))?;
            let already = file.metadata().map_err(|e| map_io(&e))?.len();
            Ok((file, already, partial_native, final_native))
        })
        .await?;

        Ok((
            Box::new(LocalSink {
                file: Some(file),
                partial: partial_native,
                final_path: final_native,
                done: false,
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            std::fs::create_dir(&native).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::Conflict {
                        conflict: collision_kind_for(&native),
                    }
                } else {
                    map_io(&e)
                }
            })
        })
        .await
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            if md.file_type().is_dir() {
                // No recursivo: dir con hijos → Conflict (el walk es del core).
                std::fs::remove_dir(&native).map_err(|e| map_io(&e))
            } else {
                std::fs::remove_file(&native).map_err(|e| map_io(&e))
            }
        })
        .await
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let nf = self.native(from)?;
        let nt = self.native(to)?;
        blocking(move || do_rename(&nf, &nt)).await
    }
}

/// Rename con contrato no-replace: la colisión la detecta el PROPIO rename
/// (atómico, sin ventana check→rename). Un destino existente solo se tolera
/// si es el origen con otra caja (case-rename en FS insensitive).
fn do_rename(nf: &Path, nt: &Path) -> Result<(), Error> {
    match rename_noreplace(nf, nt) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let from_md = std::fs::symlink_metadata(nf).map_err(|e| map_io(&e))?;
            let to_md = std::fs::symlink_metadata(nt).map_err(|e| map_io(&e))?;
            if same_node(&from_md, &to_md) {
                // Case-rename del propio origen: rename plano. La ventana
                // que reabre es mínima y solo en este camino (el destino ES
                // este mismo inode, verificado por (dev,ino)).
                std::fs::rename(nf, nt).map_err(|e| map_io(&e))
            } else {
                Err(Error::Conflict {
                    conflict: collision_kind_for(nt),
                })
            }
        }
        Err(e) if noreplace_unsupported(&e) => checked_rename(nf, nt),
        Err(e) => Err(map_io(&e)),
    }
}

/// Fallback para FS sin primitiva no-replace (NFS viejo, EINVAL/ENOSYS):
/// el check→rename de M0, con su ventana TOCTOU documentada.
fn checked_rename(nf: &Path, nt: &Path) -> Result<(), Error> {
    let from_md = std::fs::symlink_metadata(nf).map_err(|e| map_io(&e))?;
    match std::fs::symlink_metadata(nt) {
        Ok(to_md) => {
            if !same_node(&from_md, &to_md) {
                return Err(Error::Conflict {
                    conflict: collision_kind_for(nt),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(map_io(&e)),
    }
    std::fs::rename(nf, nt).map_err(|e| map_io(&e))
}

/// ¿El error dice "este FS/kernel no sabe hacer rename no-replace"?
/// (EINVAL/ENOSYS/ENOTSUP). Distinto de EXDEV (degradar a copy+delete) y de
/// EEXIST (colisión real): aquí se degrada a check→rename.
///
/// EINVAL es ambiguo: `renameat2` también lo devuelve para "destino dentro
/// del origen". El fallback re-falla igual por `std::fs::rename` (resultado
/// correcto, solo syscalls extra) y el core ya pre-filtra descendientes.
fn noreplace_unsupported(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
    )
}

/// Rename que NUNCA reemplaza un destino existente, atómico en el FS:
/// `renameat2(RENAME_NOREPLACE)`. Errores relevantes: `AlreadyExists`
/// (destino ocupado), `CrossesDevices` (EXDEV), `InvalidInput`/`Unsupported`
/// (FS o kernel sin soporte del flag — el caller degrada).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: `f` y `t` son CStrings NUL-terminadas vivas durante toda la
    // llamada; `AT_FDCWD` y `RENAME_NOREPLACE` son constantes de la ABI.
    // Contrato testeado en `tests::rename_noreplace_jamas_pisa_el_destino`.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Rename no-replace atómico de macOS: `renamex_np(RENAME_EXCL)`.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: `f` y `t` son CStrings NUL-terminadas vivas durante toda la
    // llamada; `RENAME_EXCL` es constante de la ABI. Contrato testeado en
    // `tests::rename_noreplace_jamas_pisa_el_destino`.
    let rc = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Rename no-replace de Windows: `MoveFileExW` con flags 0 — sin
/// `MOVEFILE_REPLACE_EXISTING` (no-replace) y sin `MOVEFILE_COPY_ALLOWED`
/// (cross-volumen → `ERROR_NOT_SAME_DEVICE`, jamás una copia silenciosa no
/// cancelable). El case-rename del propio archivo SÍ procede: es la vía
/// estándar de NTFS para cambiar la caja (issue #2, sin heurística).
#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let f: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let t: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Un NUL interior (posible en un `base` arbitrario del caller, no en
    // segmentos) truncaría el wide-string y renombraría OTRO path.
    if f[..f.len() - 1].contains(&0) || t[..t.len() - 1].contains(&0) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: `f` y `t` son buffers UTF-16 NUL-terminados (NUL interior
    // rechazado arriba) vivos durante toda la llamada. Contrato testeado en
    // `tests::rename_noreplace_jamas_pisa_el_destino`.
    let rc =
        unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0) };
    if rc == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Resto de unix (fuera de la matriz de CI): sin primitiva no-replace
/// portable — emulación check→rename con ventana TOCTOU.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// ¿Puede el rename proceder aunque el destino "exista"? Solo si el destino
/// ES el propio origen con otra caja (case-rename en FS insensitive) — y con
/// un único dirent: entre dos hardlinks del mismo inode, `rename(2)` es un
/// no-op con éxito que el journal registraría como un move que no ocurrió.
#[cfg(unix)]
fn same_node(from_md: &std::fs::Metadata, to_md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // La guarda de nlink solo aplica a archivos: un dir no puede tener
    // hardlinks (su nlink es 2 + subdirs) y bloquearía el case-rename de
    // directorios en FS insensitive.
    from_md.dev() == to_md.dev()
        && from_md.ino() == to_md.ino()
        && (to_md.is_dir() || to_md.nlink() == 1)
}

/// Windows: el case-rename del propio archivo lo resuelve YA el
/// `rename_noreplace` (`MoveFileExW` lo permite sin `REPLACE_EXISTING`), así
/// que llegar a `EEXIST` significa colisión real — `false` sin heurística
/// (una por nombre machacaría archivos DISTINTOS en directorios NTFS
/// case-sensitive, los que crea WSL). Cierra la deuda del issue #2.
#[cfg(windows)]
fn same_node(_from_md: &std::fs::Metadata, _to_md: &std::fs::Metadata) -> bool {
    false
}

struct LocalSink {
    file: Option<std::fs::File>,
    partial: PathBuf,
    final_path: PathBuf,
    /// `true` cuando commit/abort ya se ocuparon del staging (Drop no toca nada).
    done: bool,
}

#[async_trait]
impl ByteSink for LocalSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let mut file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let (file, res) = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let res = file.write_all(&chunk).map_err(|e| map_io(&e));
            (file, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.file = Some(file);
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let partial = self.partial.clone();
        let final_path = self.final_path.clone();
        let res = blocking(move || {
            file.sync_all().map_err(|e| map_io(&e))?;
            drop(file);
            // No-replace atómico: la colisión aparecida entre write() y
            // commit() la detecta el PROPIO rename, sin ventana TOCTOU.
            match rename_noreplace(&partial, &final_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let kind = collision_kind_for(&final_path);
                    let _ = std::fs::remove_file(&partial);
                    Err(Error::Conflict { conflict: kind })
                }
                Err(e) if noreplace_unsupported(&e) => {
                    // FS sin no-replace: check→rename de M0 (mejor esfuerzo).
                    match std::fs::symlink_metadata(&final_path) {
                        Ok(_) => {
                            let kind = collision_kind_for(&final_path);
                            let _ = std::fs::remove_file(&partial);
                            Err(Error::Conflict { conflict: kind })
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            std::fs::rename(&partial, &final_path).map_err(|e| {
                                let _ = std::fs::remove_file(&partial);
                                map_io(&e)
                            })
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&partial);
                            Err(map_io(&e))
                        }
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    Err(map_io(&e))
                }
            }
        })
        .await;
        // Éxito o error "limpio": el closure ya se ocupó del staging. Si el
        // closure PANICÓ (Internal), deja que Drop intente la limpieza.
        if !matches!(res, Err(Error::Internal { .. })) {
            self.done = true;
        }
        res
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        self.done = true;
        let partial = self.partial.clone();
        blocking(move || match std::fs::remove_file(&partial) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(map_io(&e)),
        })
        .await
    }

    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        // Conserva el staging para un open_resumable posterior (ADR 0012):
        // durabiliza (fsync) y NO renombra ni borra. `done` evita que Drop
        // lo barra.
        let file = self.file.take();
        self.done = true;
        blocking(move || {
            if let Some(f) = file {
                f.sync_all().map_err(|e| map_io(&e))?;
            }
            Ok(())
        })
        .await
    }
}

impl Drop for LocalSink {
    fn drop(&mut self) {
        // Contrato de ByteSink: soltar sin commit = abort best-effort. Es un
        // unlink síncrono y rápido; la limpieza GARANTIZADA es abort().
        if !self.done {
            self.file.take();
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::Error;

    use super::{map_io, rename_noreplace};

    #[test]
    fn exdev_mapea_a_unsupported() {
        // EXDEV en rename (issue #3): el FS no puede hacerlo — el engine
        // degrada el move a copy+delete. Io{retryable:false} sería un error
        // terminal opaco para el usuario.
        let e = std::io::Error::from(std::io::ErrorKind::CrossesDevices);
        assert_eq!(map_io(&e), Error::Unsupported);
    }

    #[test]
    fn enametoolong_mapea_a_invalid_path() {
        // Con el staging corto (issue #4), un nombre >NAME_MAX ya no revienta
        // al abrir el staging: el rechazo del OS llega en el stat/rename del
        // path FINAL. Es un problema del path, no de I/O: InvalidPath.
        let e = std::io::Error::from(std::io::ErrorKind::InvalidFilename);
        assert_eq!(map_io(&e), Error::InvalidPath);
    }

    /// EILSEQ (APFS rechaza nombres no-UTF8) llega como `Uncategorized`:
    /// hay que mirar el errno crudo. Mismo desplazamiento del issue #4: con
    /// el staging corto el rechazo ocurre en el rename de commit, y sin este
    /// mapeo saldría como `Io` opaco (lo cazó la CI de macOS).
    #[cfg(unix)]
    #[test]
    fn eilseq_mapea_a_invalid_path() {
        let e = std::io::Error::from_raw_os_error(libc::EILSEQ);
        assert_eq!(map_io(&e), Error::InvalidPath);
    }

    /// Test del `unsafe` de `rename_noreplace` (regla 5): el contrato
    /// no-replace se cumple en el FS real de los tres OS de CI.
    #[test]
    fn rename_noreplace_jamas_pisa_el_destino() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"origen").unwrap();
        std::fs::write(&b, b"destino").unwrap();

        let err = rename_noreplace(&a, &b).expect_err("destino ocupado");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"destino", "intacto");
        assert_eq!(std::fs::read(&a).unwrap(), b"origen", "intacto");

        let c = dir.path().join("c");
        rename_noreplace(&a, &c).expect("destino libre");
        assert_eq!(std::fs::read(&c).unwrap(), b"origen");
        assert!(!a.exists());
    }

    /// Guardián del hash del nombre estable (H3 del encoding-auditor): el
    /// valor DEBE ser constante entre versiones de Rust — SHA-256 lo
    /// garantiza; un cambio de algoritmo rompería la reanudación
    /// cross-versión en silencio, así que se congela aquí.
    #[test]
    fn stable_partial_name_is_frozen() {
        use norte_proto::{Scheme, Segment, VPath};
        let dst = VPath::root(Scheme::new("file").unwrap(), None)
            .join(Segment::new(b"dst.bin".to_vec()).unwrap());
        let partial = super::stable_partial_vpath(&dst).unwrap();
        assert_eq!(
            partial.file_name().unwrap().as_bytes(),
            b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c",
            "sha256(\"dst.bin\")[..16] hex — congelado (H3)"
        );
    }

    /// `is_norte_partial` (H2): reconoce las dos formas de staging y NADA
    /// más — un archivo de usuario con el prefijo no se confunde.
    #[test]
    fn is_norte_partial_reconoce_solo_las_formas() {
        use super::is_norte_partial as f;
        // Estable: prefijo + 32 hex.
        assert!(f(b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c"));
        // Efímero: prefijo + 16 hex + .<pid>-<seq>.
        assert!(f(b".norte-partial.80dcee3a35d0eff3.12345-7"));
        // NO son staging:
        assert!(!f(b".norte-partial.backup"));
        assert!(!f(b".norte-partial.notas.txt"));
        assert!(!f(b".norte-partial.")); // vacío
        assert!(!f(b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3")); // 31 hex
        assert!(!f(b".norte-partial.ZZZZ")); // no-hex
        assert!(!f(b"otro.norte-partial.80dcee3a35d0eff397ec041e9ee27a3c")); // sin prefijo al inicio
        assert!(!f(b".norte-partial.80dcee3a35d0eff3.abc-7")); // pid no-dígito
    }

    /// `SymlinkKind::Unknown` (issue #18): el kind se resuelve contra el
    /// target REAL relativo al padre del link; roto degrada a File; los
    /// kinds explícitos pasan tal cual sin tocar el FS.
    #[test]
    fn unknown_symlink_kind_se_resuelve_contra_el_target() {
        use norte_vfs::SymlinkKind;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("archivo"), b"x").unwrap();
        let link = dir.path().join("el-link");

        let kind_of = |target: &str, kind| {
            super::effective_symlink_kind(&link, std::ffi::OsStr::new(target), kind)
        };
        assert_eq!(kind_of("subdir", SymlinkKind::Unknown), SymlinkKind::Dir);
        assert_eq!(kind_of("archivo", SymlinkKind::Unknown), SymlinkKind::File);
        assert_eq!(
            kind_of("no-existe", SymlinkKind::Unknown),
            SymlinkKind::File
        );
        // Target con `..` y con separador `/` anidado: canarios de la
        // normalización pre-verbatim en la CI de Windows (bajo `\\?\` el
        // kernel no pliega `..` ni convierte `/` — hallazgo del auditor).
        std::fs::create_dir_all(dir.path().join("inner")).unwrap();
        std::fs::create_dir_all(dir.path().join("nested").join("leaf")).unwrap();
        let inner_link = dir.path().join("inner").join("el-link");
        assert_eq!(
            super::effective_symlink_kind(
                &inner_link,
                std::ffi::OsStr::new("../subdir"),
                SymlinkKind::Unknown
            ),
            SymlinkKind::Dir,
            "target relativo con .."
        );
        assert_eq!(
            kind_of("nested/leaf", SymlinkKind::Unknown),
            SymlinkKind::Dir,
            "target anidado con separador /"
        );
        // Target ABSOLUTO: join lo respeta.
        let abs = dir.path().join("subdir");
        assert_eq!(
            super::effective_symlink_kind(&link, abs.as_os_str(), SymlinkKind::Unknown),
            SymlinkKind::Dir
        );
        // Explícito: jamás se re-resuelve (no-existe seguiría siendo Dir).
        assert_eq!(kind_of("no-existe", SymlinkKind::Dir), SymlinkKind::Dir);
    }

    /// Identidad de nodo sobre el FS real (issue #16): estable, distinta
    /// entre nodos, sobrevive al rename y — donde hay symlinks — `follow`
    /// resuelve al destino. En volúmenes sin identidad (`Ok(None)`) el test
    /// se auto-salta, igual que el contrato.
    #[tokio::test]
    async fn node_id_identifica_el_mismo_archivo() {
        use norte_vfs::{FollowLinks, Provider};
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), b"x").unwrap();
        std::fs::write(dir.path().join("b"), b"y").unwrap();
        let p = super::LocalProvider::rooted(dir.path());
        let root = super::LocalProvider::root();
        let a = root.join(norte_proto::Segment::new(b"a".to_vec()).unwrap());
        let b = root.join(norte_proto::Segment::new(b"b".to_vec()).unwrap());

        let Some(id_a) = p.node_id(&a, FollowLinks::No).await.expect("node_id a") else {
            eprintln!("skip: volumen sin identidad estable");
            return;
        };
        let id_b = p
            .node_id(&b, FollowLinks::No)
            .await
            .expect("node_id b")
            .expect("mismo volumen: o siempre o nunca");
        assert_ne!(id_a, id_b, "nodos distintos");
        assert_eq!(
            p.node_id(&a, FollowLinks::Yes).await.unwrap().unwrap(),
            id_a,
            "follow sobre un archivo normal no cambia nada"
        );

        // El rename mueve el nodo, no lo recrea.
        std::fs::rename(dir.path().join("a"), dir.path().join("c")).unwrap();
        let c = root.join(norte_proto::Segment::new(b"c".to_vec()).unwrap());
        assert_eq!(p.node_id(&c, FollowLinks::No).await.unwrap().unwrap(), id_a);

        // Inexistente: NotFound, jamás None-silencioso.
        assert_eq!(
            p.node_id(&a, FollowLinks::No).await.unwrap_err(),
            norte_proto::Error::NotFound
        );
    }

    /// Colisión por normalización (issue #8): el dirent existe en NFD (lo
    /// que escribe macOS) y el pedido llega en NFC — bytes distintos, forma
    /// NFC idéntica. Etiquetarla `CaseCollision` despistaría al frontend.
    #[test]
    fn collision_por_normalizacion_se_etiqueta() {
        use norte_proto::ConflictKind;
        let dir = tempfile::tempdir().expect("tempdir");
        let nfd = String::from_utf8(vec![0x65, 0xCC, 0x81]).unwrap(); // e + ́
        std::fs::write(dir.path().join(&nfd), b"x").unwrap();
        let nfc = String::from_utf8(vec![0xC3, 0xA9]).unwrap(); // é
        assert_eq!(
            super::collision_kind_for(&dir.path().join(&nfc)),
            ConflictKind::Normalization
        );
        // Caja distinta sin tema de normalización: sigue siendo CaseCollision.
        std::fs::write(dir.path().join("caja"), b"x").unwrap();
        assert_eq!(
            super::collision_kind_for(&dir.path().join("CAJA")),
            ConflictKind::CaseCollision
        );
    }

    /// Un path con NUL interior (posible en un `base` hostil del caller)
    /// truncaría el wide-string y renombraría OTRO path: rechazo limpio.
    #[cfg(windows)]
    #[test]
    fn rename_noreplace_rejects_interior_nul() {
        use std::os::windows::ffi::OsStringExt;
        let evil = std::path::PathBuf::from(std::ffi::OsString::from_wide(&[
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            0,
            u16::from(b'x'),
        ]));
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = dir.path().join("a");
        std::fs::write(&ok, b"x").unwrap();
        let err = rename_noreplace(&evil, &ok).expect_err("NUL interior en origen");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let err = rename_noreplace(&ok, &evil).expect_err("NUL interior en destino");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&ok).unwrap(), b"x", "nada se movió");
    }
}
