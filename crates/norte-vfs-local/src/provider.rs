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

/// Contador de staging: junto al pid hace único el nombre del `.norte-partial`
/// (dos writes al mismo destino jamás comparten staging, y un archivo REAL
/// del usuario llamado `x.norte-partial` jamás se toca).
static PARTIAL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mapea un error de OS a la taxonomía del protocolo (spec §17.7): los
/// frontends renderizan por categoría, jamás parsean strings de OS.
fn map_io(e: &std::io::Error) -> Error {
    use std::io::ErrorKind as K;
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
        path,
        kind,
        size,
        mtime_ms: mtime_ms(md),
    }
}

/// Ante una colisión ya confirmada: ¿el nombre EXACTO (bytes) está en el
/// directorio, o solo una variante de caja? Distingue `Exists` de
/// `CaseCollision` (la colisión se evalúa contra el FS destino).
fn collision_kind_for(path: &Path) -> ConflictKind {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return ConflictKind::Exists;
    };
    match std::fs::read_dir(parent) {
        Ok(rd) => {
            for d in rd.flatten() {
                if d.file_name() == name {
                    return ConflictKind::Exists;
                }
            }
            ConflictKind::CaseCollision
        }
        Err(_) => ConflictKind::Exists,
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

/// Capabilities por defecto del OS, sin tocar el FS: lo que responde
/// `capabilities()` si aún no corrió ninguna operación async.
fn default_capabilities() -> Capabilities {
    let mut flags = CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING;
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
        // Result, no como primer item del stream.
        {
            let probe = native.clone();
            blocking(move || {
                let md = std::fs::symlink_metadata(&probe).map_err(|e| map_io(&e))?;
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
                    // DirEntry::metadata NO sigue symlinks: describe el link.
                    let md = d.metadata().map_err(|e| map_io(&e))?;
                    Ok(entry_from(base_vpath.join(seg), &md))
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

    async fn read(&self, p: &VPath) -> Result<ByteStream, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let file = blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            if md.is_dir() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            std::fs::File::open(&native).map_err(|e| map_io(&e))
        })
        .await?;
        // Buffer de 8 chunks: 2 MiB máximos retenidos si el consumidor se
        // atasca (con blocking_send el productor espera igual de bien).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Error>>(8);
        spawn_guarded_producer(tx, move |tx| {
            use std::io::Read;
            let mut file = file;
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
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
