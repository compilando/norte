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
/// Las capabilities se sondean en la construcción (crea y borra un archivo
/// de prueba en `base`; si `base` no es escribible cae al default del OS).
pub struct LocalProvider {
    base: PathBuf,
    caps: Capabilities,
    /// Mantiene vivo un recurso externo (p. ej. el `TempDir` de un test).
    _guard: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl LocalProvider {
    /// Provider enraizado en `base` (debe ser un directorio existente).
    ///
    /// Hace I/O bloqueante (normaliza `base` y sondea capabilities creando y
    /// borrando un archivo de prueba): llámalo en arranque/setup, no en un
    /// hot path async.
    #[must_use]
    pub fn rooted(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        // Verbatim (`\\?\`) exige path absoluto y normalizado; en Windows,
        // `absolute` usa GetFullPathNameW (separadores y `..` resueltos).
        let base = std::path::absolute(&base).unwrap_or(base);
        let caps = probe_capabilities(&base);
        Self {
            base,
            caps,
            _guard: None,
        }
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
            let mut flags = CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING;
            let _ = &mut flags;
            Self {
                base: PathBuf::new(),
                caps: Capabilities {
                    flags,
                    max_path: Some(32767),
                },
                _guard: None,
            }
        } else {
            Self::rooted("/")
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
        K::InvalidInput => Error::InvalidPath,
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

/// Sondeo de sensibilidad a la caja del FS que aloja `base`: crea
/// `.norte-probe-cs-A` y comprueba si `.norte-probe-cs-a` "existe".
/// `None` si `base` no es escribible (se usa el default del OS).
fn probe_case_sensitivity(base: &Path) -> Option<bool> {
    let upper = base.join(".norte-probe-cs-A");
    let lower = base.join(".norte-probe-cs-a");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&upper)
        .ok()?;
    let sensitive = !lower.exists();
    let _ = std::fs::remove_file(&upper);
    Some(sensitive)
}

fn probe_capabilities(base: &Path) -> Capabilities {
    let mut flags = CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING;
    if cfg!(unix) {
        // Crear symlinks en Windows exige privilegio: no se declara en M0.
        flags |= CapabilityFlags::SYMLINKS;
    }
    let default_sensitive = cfg!(all(unix, not(target_os = "macos")));
    if probe_case_sensitivity(base).unwrap_or(default_sensitive) {
        flags |= CapabilityFlags::CASE_SENSITIVE;
    }
    Capabilities {
        flags,
        // Con prefijo verbatim, el límite real de Windows es 32767 UTF-16.
        max_path: cfg!(windows).then_some(32767),
    }
}

#[async_trait]
impl Provider for LocalProvider {
    // La firma del trait es `-> &str`; devolver un literal aquí es correcto.
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "file"
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let native = self.native(p)?;
        let vpath = p.clone();
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            Ok(entry_from(vpath, &md))
        })
        .await
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
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
        let final_native = self.native(p)?;
        // Staging junto al destino, con sufijo ÚNICO (pid + secuencia):
        // `<nombre>.norte-partial.<pid>-<n>`. Único porque (a) un archivo
        // real del usuario llamado `x.norte-partial` jamás debe tocarse y
        // (b) dos writes concurrentes al mismo destino no pueden compartir
        // staging. Los huérfanos de un crash los recoge el journal (M3).
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        let seq = PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut partial_name = name.as_bytes().to_vec();
        partial_name
            .extend_from_slice(format!(".norte-partial.{}-{seq}", std::process::id()).as_bytes());
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
        let nf = self.native(from)?;
        let nt = self.native(to)?;
        blocking(move || {
            let from_md = std::fs::symlink_metadata(&nf).map_err(|e| map_io(&e))?;
            match std::fs::symlink_metadata(&nt) {
                Ok(to_md) => {
                    // En FS case-insensitive el "destino" puede ser el propio
                    // origen con otra caja: rename de caja permitido.
                    if !same_node(&from_md, &to_md) {
                        return Err(Error::Conflict {
                            conflict: collision_kind_for(&nt),
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(&e)),
            }
            std::fs::rename(&nf, &nt).map_err(|e| map_io(&e))
        })
        .await
    }
}

/// ¿Puede el rename proceder aunque el destino "exista"? Solo si el destino
/// ES el propio origen con otra caja (case-rename en FS insensitive) — y con
/// un único dirent: entre dos hardlinks del mismo inode, `rename(2)` es un
/// no-op con éxito que el journal registraría como un move que no ocurrió.
#[cfg(unix)]
fn same_node(from_md: &std::fs::Metadata, to_md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    from_md.dev() == to_md.dev() && from_md.ino() == to_md.ino() && to_md.nlink() == 1
}

/// Windows: std no expone la identidad real del archivo (VolumeSerial +
/// FileIndex exigirían `windows-sys`), y una heurística por nombre machacaría
/// archivos DISTINTOS en directorios NTFS case-sensitive (los que crea WSL).
/// M0 elige seguridad: el case-rename en Windows devuelve `Conflict` (deuda M1).
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
            // Re-chequeo de colisión: pudo aparecer algo entre write() y commit().
            match std::fs::symlink_metadata(&final_path) {
                Ok(_) => {
                    let kind = collision_kind_for(&final_path);
                    let _ = std::fs::remove_file(&partial);
                    return Err(Error::Conflict { conflict: kind });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    return Err(map_io(&e));
                }
            }
            std::fs::rename(&partial, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                map_io(&e)
            })
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
