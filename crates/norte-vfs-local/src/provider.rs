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

use norte_vfs::native::{to_native, verbatim};
use norte_vfs::wtf8::os_to_bytes;

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
    /// Capabilities sondeadas POR DIRECTORIO (ADR 0054), con la identidad del
    /// directorio como clave (`dev`, `ino`, `ctime`): dos rutas al mismo sitio
    /// son una entrada, un `..` o un symlink no multiplican el sondeo, y un
    /// inodo reutilizado no hereda la respuesta del difunto. Acotado, con desalojo del
    /// más antiguo — una sesión larga no puede acabar con un mapa de todos los
    /// directorios que visitó.
    caps_at: std::sync::Arc<std::sync::Mutex<CapsAtCache>>,
    /// Sustituto de `$XDG_DATA_HOME` para la papelera freedesktop; `None` =
    /// resolver del entorno, que es lo que hace producción.
    trash_home: Option<PathBuf>,
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
            caps_at: std::sync::Arc::new(std::sync::Mutex::new(CapsAtCache::default())),
            trash_home: None,
            _guard: None,
        }
    }

    /// Fuerza el paseo componente a componente aunque el kernel tenga
    /// `openat2`, mientras el guard viva (costura de test): es la única forma
    /// de ejercitar las dos ramas del confinamiento en una misma máquina.
    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    #[must_use]
    pub fn force_component_walk_for_test() -> crate::confined::ForceComponentWalk {
        crate::confined::ForceComponentWalk::new()
    }

    /// Cuántas veces se ha SONDEADO de verdad una ubicación (costura de test:
    /// lo que la caché ahorra no se ve de ninguna otra forma).
    #[doc(hidden)]
    #[must_use]
    pub fn caps_at_probe_count(&self) -> u64 {
        self.caps_at.lock().expect("caps_at lock sano").probes
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

    /// Sustituye `$XDG_DATA_HOME` para la papelera freedesktop: la papelera
    /// "home" pasa a ser `<dir>/Trash`.
    ///
    /// Es una COSTURA DE TEST, y existe porque un test no puede tocar la
    /// papelera de verdad del desarrollador ni averiguar en qué dispositivo
    /// vive: `std::env::set_var` es `unsafe` en la edición 2024 (prohibido
    /// fuera de los usos justificados de la regla 5) y además es global al
    /// proceso. Producción no la llama y resuelve del entorno.
    ///
    /// `dir` debe ser ABSOLUTO —una raíz de papelera relativa al cwd no es una
    /// raíz— y, para que [`Provider::trash`] pueda NOMBRAR su destino, debe
    /// caer bajo la raíz de este provider.
    #[doc(hidden)]
    #[must_use]
    pub fn with_trash_home(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        // Una raíz relativa se ignoraría silenciosamente aguas abajo y la
        // papelera acabaría siendo la del montaje de la víctima, que es un
        // fallo desconcertante en un test (MINOR-3 del encoding-auditor).
        debug_assert!(dir.is_absolute(), "la raíz de la papelera es absoluta");
        self.trash_home = Some(dir);
        self
    }

    /// El `VPath` de un path NATIVO bajo la raíz de este provider — la inversa
    /// de [`Self::native`].
    ///
    /// `None` si el path no cuelga de la raíz: entonces este provider NO puede
    /// nombrarlo, y quien pregunte se tiene que quedar sin ruta en vez de
    /// recibir una que no resuelve. Pasa con un provider enraizado (los tests)
    /// cuya papelera cae fuera; con `os_root`, que es lo que registra el
    /// daemon, la raíz es `/` y no pasa nunca.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    fn vpath_of(&self, native: &Path) -> Option<VPath> {
        use std::path::Component;
        let rel = native.strip_prefix(&self.base).ok()?;
        let mut out = Self::root();
        for comp in rel.components() {
            let Component::Normal(os) = comp else {
                return None;
            };
            out = out.join(Segment::new(os_to_bytes(os)).ok()?);
        }
        Some(out)
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
                caps_at: std::sync::Arc::new(std::sync::Mutex::new(CapsAtCache::default())),
                trash_home: None,
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

/// Papelera nativa delegada al crate `trash` (Recycle Bin, y las unix que no
/// son freedesktop). En freedesktop NO existe: la papelera la implementa
/// [`crate::trash_fdo`], que además sabe decir dónde dejó el fichero.
#[cfg(not(any(
    target_os = "macos",
    all(unix, not(target_os = "ios"), not(target_os = "android")),
)))]
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
    let name = p.file_name().ok_or(Error::InvalidPath)?;
    let seg = Segment::new(stable_partial_name(name.as_bytes())).map_err(|_| Error::InvalidPath)?;
    p.with_file_name(seg).ok_or(Error::InvalidPath)
}

/// El nombre del staging estable a partir de los BYTES del nombre final.
///
/// La mitad de [`stable_partial_vpath`] que no necesita un `VPath`, porque la
/// raíz confinada direcciona por segmentos y no tiene ninguno que darle. Una
/// sola definición: dos formas de nombrar el mismo staging serían dos ficheros
/// donde el resume espera uno.
pub(crate) fn stable_partial_name(final_name: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(final_name);
    let mut hex = String::with_capacity(STABLE_HASH_HEX);
    for b in &digest[..STABLE_HASH_HEX / 2] {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    format!("{PARTIAL_PREFIX}{hex}").into_bytes()
}

/// El modo con el que se PUBLICA lo que pasó por el staging ESTABLE (#299).
///
/// Ese staging nace `0o600` y no puede nacer de otra forma: su nombre es
/// predecible, así que mientras dure tiene que ser nuestro y de nadie más
/// (#298). Pero publicar es un `rename`, que no toca el modo, y sin esto una
/// copia REANUDADA acabaría en `0o600` mientras la misma copia sin cortes
/// acaba en `0o644`. Misma operación, dos resultados, y el reanudable es hoy
/// el camino por defecto de una hoja.
///
/// Así que se reproduce lo que habría dado un `create`: `0o666` recortado por
/// la umask. Lo que NO se hace es preservar el modo del ORIGEN —eso es lo que
/// hace `cp -p` y es una decisión de producto que norte todavía no ha tomado
/// (hoy no preserva permisos en ninguna copia); colarla aquí sería decidirla
/// por descarte dentro de un arreglo.
#[cfg(unix)]
pub(crate) fn modo_publicado() -> u32 {
    0o666 & !umask_del_proceso()
}

/// La umask del proceso, SIN cambiarla.
///
/// `umask(2)` solo la devuelve poniéndola, y eso es global al proceso: hacerlo
/// aquí sería una carrera con cualquier otra escritura en vuelo, en un daemon
/// que escribe desde muchas tasks a la vez. Linux la publica de solo lectura
/// en `/proc/self/status` (`Umask:`, desde 4.7).
///
/// Donde no se puede leer se supone `0o022`, que es la de una configuración
/// corriente y da el `0o644` de siempre. Suponer de menos —`0o000`— publicaría
/// más abierto de lo que el usuario pidió, y eso no se hace ni una vez.
#[cfg(unix)]
fn umask_del_proceso() -> u32 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for linea in status.lines() {
                if let Some(valor) = linea.strip_prefix("Umask:")
                    && let Ok(u) = u32::from_str_radix(valor.trim(), 8)
                {
                    return u;
                }
            }
        }
    }
    0o022
}

/// Le da al fichero RECIÉN PUBLICADO el modo que habría tenido si la copia no
/// se hubiera cortado (#299). No hace nada si el staging no era el estable.
///
/// Va sobre el DESCRIPTOR, que sigue apuntando al mismo inodo después del
/// rename: por ruta habría una ventana entre publicar y ajustar en la que otro
/// podría sustituir el nombre y recibir el `chmod`.
///
/// **Best-effort a propósito, y en silencio.** Un fallo aquí deja el fichero
/// en `0o600`: copiado, con sus bytes y su nombre buenos, y más restrictivo de
/// lo pedido. Convertirlo en error tiraría una copia entera por un permiso.
/// Y no se registra porque este crate no tiene `tracing` —es el único que
/// puede usar `unsafe` y se mantiene sin dependencias de instrumentación—;
/// quien quiera saberlo mira el modo del fichero.
#[cfg(unix)]
pub(crate) fn reponer_modo_publicado(file: &std::fs::File, estable: bool) {
    use std::os::fd::AsRawFd as _;

    if !estable {
        return;
    }
    // SAFETY: `file` está vivo y su fd es válido durante toda la llamada.
    // `fchmod` no toma punteros.
    #[allow(unsafe_code)]
    let _ = unsafe { libc::fchmod(file.as_raw_fd(), modo_publicado() as libc::mode_t) };
}

/// Windows no tiene modo POSIX que reponer: el fichero hereda la ACL de su
/// directorio y el staging nunca se restringió a mano.
#[cfg(windows)]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn reponer_modo_publicado(_file: &std::fs::File, _estable: bool) {}

/// Abre (o crea) el staging estable de `path` para REANUDAR, y dice cuántos
/// bytes había.
///
/// Unix mira lo que ha abierto —`O_NOFOLLOW`, fichero regular, un solo enlace,
/// nuestro, `0o600`— porque el nombre lo calcula cualquiera que sepa el nombre
/// de destino (#298).
#[cfg(unix)]
fn open_stable_staging(path: &std::path::Path) -> Result<(std::fs::File, u64), Error> {
    crate::confined::abre_staging_estable(path)
}

/// Windows: sin las comprobaciones de #298 todavía. Un reparse point plantado
/// con el nombre del staging es el mismo agujero, y ahí no se cierra con una
/// bandera de `open` — pide `NtCreateFile` con `FILE_OPEN_REPARSE_POINT`, que
/// es lo que #220 y #217 tienen abierto.
#[cfg(windows)]
fn open_stable_staging(path: &std::path::Path) -> Result<(std::fs::File, u64), Error> {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| map_io(&e))?;
    let already = file.metadata().map_err(|e| map_io(&e))?.len();
    Ok((file, already))
}

/// Abre el staging estable de `path` para LEER su prefijo, o `None` si no hay
/// uno nuestro. Mismas comprobaciones y mismo motivo que
/// [`open_stable_staging`].
#[cfg(unix)]
fn open_partial_for_digest(path: &std::path::Path) -> Result<Option<std::fs::File>, Error> {
    crate::confined::abre_parcial_verificado(path)
}

#[cfg(windows)]
fn open_partial_for_digest(path: &std::path::Path) -> Result<Option<std::fs::File>, Error> {
    match std::fs::File::open(path) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(map_io(&e)),
    }
}

/// Nombre de staging EFÍMERO para el destino `final_name`:
/// `.norte-partial.<16 hex>.<pid>-<seq>`.
///
/// pid + secuencia: (a) un archivo real del usuario jamás se toca (el
/// `O_EXCL`/`create_new` de quien lo abre además lo garantiza) y (b) dos
/// writes concurrentes al mismo destino no comparten staging. El prefijo lo
/// hace reconocible para el GC (ADR 0012) — la forma la valida
/// [`is_norte_partial`], así que quien la construya debe hacerlo AQUÍ y no en
/// una segunda copia del `format!`.
pub(crate) fn ephemeral_partial_name(final_name: &[u8]) -> Vec<u8> {
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        final_name.hash(&mut h);
        h.finish()
    };
    let seq = PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{PARTIAL_PREFIX}{hash:016x}.{}-{seq}", std::process::id()).into_bytes()
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
pub(crate) fn map_io(e: &std::io::Error) -> Error {
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
/// Lo que `capabilities_at` espera a que el filesystem conteste (#213).
///
/// El mismo número que usan las consultas de volúmenes del core
/// (`SPACE_QUERY_DEADLINE`, `ENUMERATE_DEADLINE`, `QUERY_DEADLINE`) y por el
/// mismo motivo: la escalera son un `statfs` y un `ioctl` sobre un montaje
/// vivo —microsegundos—, así que 200 ms no recorta ninguna respuesta real y
/// sí acota lo que un montaje muerto puede hacer esperar a quien pregunta.
const CAPS_AT_DEADLINE: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) async fn blocking<T: Send + 'static>(
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

fn entry_from(path: VPath, md: &std::fs::Metadata, req: &norte_vfs::AttrRequest) -> Entry {
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
        attrs: attrs_from_md(md, req),
        path,
        kind,
        size,
        mtime_ms: mtime_ms(md),
    }
}

/// Catálogo de attrs del provider local (#108 bloque 2): POSIX en unix,
/// `win.attributes` en Windows. Todo sale de la `Metadata` ya en mano —
/// cero syscalls extra sobre `stat`; en `list` exige la promoción por
/// entrada (ver `list_with`).
#[cfg(unix)]
fn catalogo_local() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        vec![
            mk("posix.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("posix.uid", "UID", AttrType::Uint, AttrHint::Identity),
            mk("posix.gid", "GID", AttrType::Uint, AttrHint::Identity),
            mk("posix.nlink", "Links", AttrType::Uint, AttrHint::Opaque),
            mk(
                "posix.ctime_ms",
                "Changed",
                AttrType::TimeMs,
                AttrHint::Timestamp,
            ),
        ]
    });
    &CAT
}

/// Ver [`catalogo_local`] (variante Windows).
#[cfg(windows)]
fn catalogo_local() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![AttrInfo {
            id: "win.attributes".to_owned(),
            label: "Attributes".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Opaque,
        }]
    });
    &CAT
}

#[cfg(not(any(unix, windows)))]
fn catalogo_local() -> &'static [norte_proto::AttrInfo] {
    &[]
}

/// Materializa los attrs pedidos desde una `Metadata` YA en mano. `mode` es
/// el `st_mode` crudo (bits de tipo incluidos); los formatters deciden la
/// presentación (octal/rwx).
fn attrs_from_md(
    md: &std::fs::Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if req.is_empty() {
        return out;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if req.wants("posix.mode") {
            out.insert(
                "posix.mode".to_owned(),
                AttrValue::Uint(u64::from(md.mode())),
            );
        }
        if req.wants("posix.uid") {
            out.insert("posix.uid".to_owned(), AttrValue::Uint(u64::from(md.uid())));
        }
        if req.wants("posix.gid") {
            out.insert("posix.gid".to_owned(), AttrValue::Uint(u64::from(md.gid())));
        }
        if req.wants("posix.nlink") {
            out.insert("posix.nlink".to_owned(), AttrValue::Uint(md.nlink()));
        }
        if req.wants("posix.ctime_ms") {
            // ctime en ms, exactamente floor(ms real): tv_nsec ∈ [0, 1e9),
            // así que también en pre-1970 la desviación es < 1ms (redondeo
            // hacia −∞). Saturante: un FUSE/imagen forjada puede devolver
            // st_ctime cerca de i64::MAX y el overflow mataría el listado.
            let ms = md
                .ctime()
                .saturating_mul(1000)
                .saturating_add(md.ctime_nsec() / 1_000_000);
            out.insert("posix.ctime_ms".to_owned(), AttrValue::TimeMs(ms));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if req.wants("win.attributes") {
            out.insert(
                "win.attributes".to_owned(),
                AttrValue::Uint(u64::from(md.file_attributes())),
            );
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = md;
    out
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
    let upper = verbatim(base.join(format!(".norte-probe-{pid}-{seq}-A")));
    let lower = verbatim(base.join(format!(".norte-probe-{pid}-{seq}-a")));
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
/// u64 + `FileId` de 128 bits, cubre `ReFS`) vía `GetFileInformationByHandleEx`.
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
            let resolved = verbatim(std::path::absolute(&resolved).unwrap_or(resolved));
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
/// (`SeCreateSymbolicLinkPrivilege` o Developer Mode) — por eso el provider
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
        // Permisos POSIX (#314): en Windows no los hay —`set_permissions` solo
        // sabe del bit de solo lectura—, y anunciarlos ahí sería prometer que
        // `fs.set_mode` hace algo que no hace.
        flags |= CapabilityFlags::POSIX_MODE;
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

/// Caché acotada de capabilities por directorio, con la identidad del nodo
/// como clave.
///
/// No es un LRU de acceso sino de INSERCIÓN: lo que hay que impedir es que un
/// recorrido largo haga crecer el mapa sin fin, y para eso basta con desalojar
/// la entrada más vieja. Un comparador toca un puñado de raíces; un indexador
/// que recorra miles pagará una syscall de más al volver sobre la primera, que
/// es más barato que la contabilidad de un LRU de verdad.
#[derive(Debug, Default)]
struct CapsAtCache {
    /// `(dev, ino)` → capabilities ya sondeadas.
    map: std::collections::HashMap<(u64, u64, i64), Capabilities>,
    /// Orden de inserción, para el desalojo.
    order: std::collections::VecDeque<(u64, u64, i64)>,
    /// Sondeos REALES (los que no salieron de aquí). Costura de test.
    probes: u64,
}

/// Techo de la caché. Un directorio ocupa decenas de bytes; 256 cubre de sobra
/// las raíces de una comparación, una sincronización y los dos paneles.
const CAPS_AT_CACHE_MAX: usize = 256;

impl CapsAtCache {
    fn get(&self, key: (u64, u64, i64)) -> Option<Capabilities> {
        self.map.get(&key).copied()
    }

    fn insert(&mut self, key: (u64, u64, i64), caps: Capabilities) {
        if self.map.insert(key, caps).is_none() {
            self.order.push_back(key);
            while self.order.len() > CAPS_AT_CACHE_MAX {
                if let Some(viejo) = self.order.pop_front() {
                    self.map.remove(&viejo);
                }
            }
        }
    }
}

/// Identidad de un directorio para la caché: `(dev, ino, ctime_nsec)`.
///
/// El `ctime` está ahí por la REUTILIZACIÓN de inodos, que es lo que hace
/// insuficiente a `(dev, ino)` solo: ext4 recicla números de inodo dentro del
/// mismo grupo de bloques, así que borrar un directorio `+F` y crear otro
/// corriente puede devolver la misma pareja y servirle la respuesta del
/// muerto — un `FULL_FOLD` falso, que empareja dos ficheros que son distintos.
/// El `ctime` cambia en toda reasignación de inodo y ya viene en la `Metadata`
/// que se acaba de leer, así que cuesta cero.
///
/// (El flag `+F` de un directorio VIVO no cambia: se hereda al crearlo, no se
/// puede poner sobre un directorio no vacío ni quitar. Lo que se invalida aquí
/// es la identidad, no el veredicto.)
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn dir_identity(md: &std::fs::Metadata) -> Option<(u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((md.dev(), md.ino(), md.ctime_nsec()))
}

/// Windows: `std` no expone la identidad del volumen ni el índice del fichero
/// desde `Metadata`, así que aquí no hay clave y cada pregunta se sondea. En
/// Windows la escalera de solo lectura no responde nada todavía (ver
/// `caps_at::windows`), así que sondear es leer una `Metadata` y poco más.
#[cfg(windows)]
fn dir_identity(_md: &std::fs::Metadata) -> Option<(u64, u64, i64)> {
    None
}

/// La clave de caché de un directorio, stateándolo.
fn dir_key(dir: &Path) -> Option<(u64, u64, i64)> {
    std::fs::metadata(dir).ok().as_ref().and_then(dir_identity)
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

    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        self.ensure_caps().await;
        let mut declared = self.capabilities();
        // Confinar es de la PLATAFORMA, no de la ubicación ni del estado del
        // árbol: en unix hay `openat` —con `openat2` o con el paseo, los dos
        // garantizan lo mismo—, y en Windows todavía no. Va antes de cualquier
        // sonda porque tiene que valer también en el camino degradado de abajo:
        // si no, `file:///destino-que-aun-no-existe` diría «no sé confinar» y
        // `file:///` diría que sí, que es una respuesta distinta para la misma
        // máquina y el caso corriente de planificar un mirror.
        declared
            .flags
            .set(CapabilityFlags::CONFINED_WRITES, cfg!(unix));
        let native = self.native(p)?;
        let cache = std::sync::Arc::clone(&self.caps_at);
        // Con PLAZO, y en un hilo desacoplado (#213). Todo lo que hay dentro
        // —el `symlink_metadata`, el `statfs` y el `ioctl` de la escalera— se
        // cuelga indefinidamente sobre un NFS o un CIFS muerto, y una syscall
        // en vuelo no se cancela. En el pool de bloqueo eso ataría una plaza
        // COMPARTIDA por montaje caído; peor todavía, `sync.plan` pregunta
        // esto dos veces ANTES de `sched.submit`, o sea fuera de toda Task y
        // de todo `CancellationToken` (regla dura 3): la RPC se quedaba colgada
        // sin nada que cancelar.
        //
        // Vencido el plazo se responde lo DECLARADO por el provider, que es la
        // degradación que el ADR 0054 ya define para «no lo sé», y no se
        // cachea nada: un montaje que vuelve contesta la próxima vez.
        let declared_on_timeout = declared;
        norte_vfs::deadline::blocking_with_deadline(
            move || {
                // La pregunta es SIEMPRE sobre el directorio que CONTIENE al
                // nombre: lo que se decide con la respuesta es si dos nombres
                // pueden coexistir ahí, y eso lo manda el directorio donde van a
                // estar. Para un fichero —o para un symlink, que es un nombre en
                // el directorio del enlace y no en el de su destino— eso es su
                // padre; para un directorio, él mismo. `symlink_metadata`, por
                // tanto, y no `metadata`.
                let Ok(md) = std::fs::symlink_metadata(&native) else {
                    // Una ruta que no está (o que no se deja mirar) NO es un error
                    // aquí: `capabilities()` jamás pudo fallar, y hacer fallar a
                    // su versión por ubicación convertiría «planificar hacia un
                    // destino que aún no existe» —el caso corriente de un mirror—
                    // en un error, además de cambiar el contrato de un método del
                    // wire ya publicado. Se declara lo del provider (ADR 0054: la
                    // degradación es el comportamiento de siempre).
                    return Ok(declared);
                };
                let dir: &Path = if md.is_dir() {
                    &native
                } else {
                    native.parent().unwrap_or(&native)
                };
                let key = dir_key(dir);

                if let Some(k) = key
                    && let Some(hit) = cache.lock().expect("caps_at lock sano").get(k)
                {
                    return Ok(hit);
                }

                let found = crate::caps_at::probe_location(dir);
                let mut caps = declared;
                if let Some(sensitive) = found.case_sensitive {
                    caps.flags.set(CapabilityFlags::CASE_SENSITIVE, sensitive);
                }
                // `None` = la escalera no supo; se deja lo declarado en vez de
                // apagar un flag que nadie contradijo.
                if let Some(full) = found.full_fold {
                    caps.flags.set(CapabilityFlags::FULL_FOLD, full);
                }
                let mut guard = cache.lock().expect("caps_at lock sano");
                guard.probes += 1;
                if let Some(k) = key {
                    guard.insert(k, caps);
                }
                Ok(caps)
            },
            CAPS_AT_DEADLINE,
        )
        .await
        .unwrap_or(Ok(declared_on_timeout))
    }

    /// Las reglas de nombre de ESTA plataforma (#163).
    ///
    /// En unix, cualquier secuencia de bytes sin `/` ni NUL — y un `Segment`
    /// ya lo garantiza, así que aquí no hay nada que rechazar.
    ///
    /// En Windows sí: los nombres de dispositivo (`CON`, `NUL`, `COM1`…) no
    /// son ficheros, los `<>:"|?*` y los controles no son legales, y un punto
    /// o un espacio FINALES los borra Win32 en silencio — con lo que el
    /// fichero que queda no es el que se pidió. `f:ads` es el peor de todos y
    /// por eso los dos puntos están en la lista: ahí no falla, escribe un
    /// flujo alternativo, y la copia dice que fue bien mientras el fichero no
    /// está.
    ///
    /// **Sin verificar en una máquina Windows**, como el resto de la deuda de
    /// esa plataforma (#217, #220, #221, #222): las reglas salen de la
    /// documentación de Win32, no de una ejecución. Lo que sí está probado es
    /// el CABLEADO —que un nombre rehusado bloquea el plan en vez de
    /// descubrirse al ejecutar—, con un provider de test que rehúsa a
    /// propósito.
    fn name_is_legal(&self, name: &[u8]) -> bool {
        /// Los nombres de dispositivo de Win32, que no son ficheros.
        const RESERVADOS: &[&[u8]] = &[
            b"CON", b"PRN", b"AUX", b"NUL", b"COM1", b"COM2", b"COM3", b"COM4", b"COM5", b"COM6",
            b"COM7", b"COM8", b"COM9", b"LPT1", b"LPT2", b"LPT3", b"LPT4", b"LPT5", b"LPT6",
            b"LPT7", b"LPT8", b"LPT9",
        ];

        if !cfg!(windows) {
            return true;
        }
        if name.is_empty() {
            return false;
        }
        // Los bytes prohibidos por Win32, más los controles.
        if name.iter().any(|b| {
            matches!(
                b,
                0..=0x1F | b'<' | b'>' | b':' | b'"' | b'|' | b'?' | b'*' | b'\\'
            )
        }) {
            return false;
        }
        // Punto o espacio finales: Win32 los quita, así que el nombre que
        // queda no es el que se pidió.
        if matches!(name.last(), Some(b'.' | b' ')) {
            return false;
        }
        // Los nombres de dispositivo, con o sin extensión detrás.
        let raiz: &[u8] = name.split(|b| *b == b'.').next().unwrap_or(name);
        !RESERVADOS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&raiz.to_ascii_uppercase()))
    }

    #[cfg(unix)]
    async fn open_root(&self, root: &VPath) -> Result<Box<dyn norte_vfs::ConfinedRoot>, Error> {
        self.ensure_caps().await;
        let native = self.native(root)?;
        let vpath = root.clone();
        blocking(move || {
            let abierta = crate::confined::LocalRoot::open(&native)?;
            Ok(
                Box::new(crate::confined::LocalConfinedRoot::new(abierta, vpath))
                    as Box<dyn norte_vfs::ConfinedRoot>,
            )
        })
        .await
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let vpath = p.clone();
        let req = opt.attrs.clone();
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            Ok(entry_from(vpath, &md, &req))
        })
        .await
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        catalogo_local()
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

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        // Sin attr anunciado en la petición: camino rápido lazy (#52) intacto.
        let advertised = catalogo_local();
        if !opt
            .attrs
            .iter()
            .any(|id| advertised.iter().any(|a| a.id == id))
        {
            return self.list(p).await;
        }
        self.ensure_caps().await;
        let native = self.native(p)?;
        // Misma validación previa síncrona que `list` (NotFound / no-dir en
        // el Result, no como primer item del stream).
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
        let req = opt.attrs.clone();
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
                    // Promoción (#108 bloque 2): attrs pedidos → un lstat por
                    // entrada (`DirEntry::metadata` NO sigue symlinks), que
                    // además hidrata size/mtime de gratis. Sigue dentro del
                    // productor bloqueante — jamás I/O en el ejecutor async.
                    //
                    // Carrera readdir→lstat: una entrada borrada entre ambos
                    // ya NO existe — se OMITE (None), no mata un listado de
                    // un dir vivo (/tmp, build dirs). Otros errores sí son
                    // fatales, como en el camino sin promoción.
                    match d.metadata() {
                        Ok(md) => Ok(Some(entry_from(base_vpath.join(seg), &md, &req))),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(e) => Err(map_io(&e)),
                    }
                });
                let item = match item {
                    Ok(None) => continue,
                    Ok(Some(entry)) => Ok(entry),
                    Err(e) => Err(e),
                };
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

    /// Freedesktop (Linux/BSD): la papelera la implementa este crate (módulo
    /// interno `trash_fdo`, la spec de freedesktop.org), y NOMBRA su destino.
    ///
    /// Es la diferencia entre poder deshacer una sobrescritura y no poder: con
    /// `Ok(None)` el journal se queda sin `reversal_ref` y el undo tiene que
    /// adivinar por ruta original, que sobre una pareja `trashed`+`created`
    /// desentierra el fichero equivocado. Aquí el destino sale de una decisión
    /// nuestra, así que se sabe.
    ///
    /// `Ok(None)` sigue siendo posible en un caso: que la papelera que toca
    /// caiga FUERA de la raíz de este provider (un provider enraizado, cosa de
    /// tests — `os_root`, que es el que registra el daemon, no puede). El
    /// efecto ya ocurrió; lo que falta es una ruta que este provider sepa
    /// resolver, y devolver una que no resuelve sería peor.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn trash(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        self.ensure_caps().await;
        if !self.capabilities().flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        let native = self.native(p)?;
        let home = self.trash_home.clone();
        let id = *id;
        let dest = blocking(move || crate::trash_fdo::trash(&native, home.as_deref(), &id)).await?;
        Ok(self.vpath_of(&dest))
    }

    /// macOS y Windows: sigue delegando en el crate `trash`, que no expone
    /// dónde puso el fichero — de ahí el `Ok(None)`, y de ahí que
    /// [`Provider::trash_restorable`] diga que no.
    ///
    /// Reimplementar la papelera de esas dos plataformas no es lo mismo que
    /// implementar una spec de tres ficheros: `NSFileManager` y la Recycle Bin
    /// son APIs con su propio índice, y falsear uno sería peor que decir la
    /// verdad (issues #25/#26).
    #[cfg(not(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )))]
    async fn trash(
        &self,
        p: &VPath,
        // La papelera NATIVA del OS no tiene destino recuperable estable: el
        // id determinista del engine (#99) no aplica aquí (dest = None; el undo
        // degrada como siempre en trash nativa). Solo lo usan las lógicas.
        _id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
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

    /// Freedesktop sí, **si además este provider sabe NOMBRAR su papelera**.
    ///
    /// No es una constante de plataforma: `trash()` devuelve la ruta traducida
    /// a `VPath`, y eso no puede nombrar lo que cae fuera de la raíz del
    /// provider. Un provider enraizado en un directorio cuya papelera queda
    /// fuera contestaría `Ok(None)` tras haber prometido que sí — y el journal
    /// se quedaría sin `reversal_ref` justo donde el plan dijo `RestoreTrash`,
    /// que es el bug entero de esta tarea con otro disfraz (MAJOR-3 del
    /// encoding-auditor, MINOR del security-reviewer). Así que la promesa se
    /// mide, y quien no puede cumplirla contesta `false`: el plan marca los
    /// pasos IRREVERSIBLES antes de que nadie apruebe (regla dura 4).
    ///
    /// `os_root`, que es lo que registra el daemon, tiene la raíz en `/` y
    /// nombra cualquier ruta.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    fn trash_restorable(&self) -> bool {
        self.base == Path::new("/")
            || crate::trash_fdo::home_trash(self.trash_home.as_deref())
                .is_some_and(|t| t.starts_with(&self.base))
    }

    /// macOS y Windows no: ahí la papelera la pone el crate `trash`, que no
    /// dice dónde deja las cosas (issues #25/#26, ADR 0009).
    #[cfg(not(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )))]
    fn trash_restorable(&self) -> bool {
        false
    }

    /// Saca de la papelera freedesktop el fichero que
    /// [`Provider::trash`] enterró, y se lleva su sidecar con él.
    ///
    /// El movimiento primero y el sidecar después, nunca al revés: un
    /// `files/x` sin su `info/x.trashinfo` no lo enseña ninguna papelera
    /// gráfica, así que borrar los metadatos y fallar luego el movimiento
    /// escondería el fichero en vez de devolverlo. Al revés lo peor que queda
    /// es un sidecar huérfano, que es cosmético.
    ///
    /// El borrado del sidecar es best-effort a propósito: el contrato de este
    /// método es "el fichero está de vuelta en `original`", y eso ya se
    /// cumplió cuando el `rename` volvió `Ok`. Devolver `Err` por no haber
    /// podido limpiar metadatos haría que el undo contase como bloqueada una
    /// entrada que sí se revirtió.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn restore_from(&self, dest: &VPath, original: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let from = self.native(dest)?;
        let to = self.native(original)?;
        blocking(move || {
            do_rename(&from, &to)?;
            crate::trash_fdo::forget_sidecar(&from);
            Ok(())
        })
        .await
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
        let target = norte_vfs::native::link_target_to_os(target)?;
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
        let partial_name = ephemeral_partial_name(name.as_bytes());
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
            // Staging recién creado: se empieza en cero.
            pos: 0,
            partial: partial_native,
            final_path: final_native,
            done: false,
            estable: false,
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
            // Sin staging NUESTRO = sin digest (el engine degrada a Length):
            // el prefijo de un fichero que no es el que se va a continuar no
            // dice nada sobre lo que se va a continuar (#298).
            let Some(file) = open_partial_for_digest(&partial_native)? else {
                return Ok(None);
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
            // copia previa, se reanuda tras ellos. Y MIRA lo que ha abierto,
            // porque este nombre es predecible (#298).
            let (file, already) = open_stable_staging(&partial_native)?;
            Ok((file, already, partial_native, final_native))
        })
        .await?;

        Ok((
            Box::new(LocalSink {
                file: Some(file),
                // REANUDANDO: la posición es lo que ya hay, y no lo que diga el
                // descriptor — se abrió con `O_APPEND`, que deja el offset en 0
                // hasta la primera escritura (ver `write_maybe_sparse`).
                pos: already,
                partial: partial_native,
                final_path: final_native,
                done: false,
                estable: true,
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

    /// #314: `chmod(2)`, en `spawn_blocking` como todo lo demás de este
    /// provider (regla 2).
    ///
    /// Solo en unix. En Windows `set_permissions` únicamente sabe del bit de
    /// solo lectura, así que fingir un modo POSIX ahí sería escribir algo que
    /// no es lo que se pidió: se responde `Unsupported`, que es lo mismo que
    /// dice la capability.
    #[cfg(unix)]
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        use std::os::unix::fs::PermissionsExt as _;

        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            // `set_permissions` SIGUE el enlace, que es lo que hace `chmod(2)`
            // y lo que espera quien lo pide desde un listado: los permisos de
            // un symlink no significan nada en Linux.
            std::fs::set_permissions(&native, std::fs::Permissions::from_mode(mode))
                .map_err(|e| map_io(&e))
        })
        .await
    }

    #[cfg(not(unix))]
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        let _ = (p, mode);
        Err(Error::Unsupported)
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
pub(crate) fn do_rename(nf: &Path, nt: &Path) -> Result<(), Error> {
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

/// Escribe `chunk` dejando AGUJERO donde es todo ceros (roadmap ítem 8).
///
/// Un chunk entero de ceros no se escribe: se extiende la longitud y se coloca
/// la posición al final. Quien decide si eso es un agujero de verdad es el
/// filesystem —ext4, XFS y APFS sí; uno que no los tenga asigna al escribir y
/// sale igual de correcto—, y lo que se lee después son los mismos bytes en los
/// dos casos.
///
/// **`pos` lo lleva el sink y NO se pregunta al descriptor**, que es la parte
/// donde esto se rompió una vez y de la peor manera. La primera versión saltaba
/// con `SeekFrom::Current` y fijaba la longitud con lo que devolviera el salto,
/// apoyándose en que el sink escribe secuencialmente desde el final. Es falso
/// para el sink REANUDADO: se abre con `O_APPEND`, y `O_APPEND` no coloca el
/// offset al final al abrir —lo deja en 0 y solo se reposiciona justo antes de
/// cada `write`—, así que sobre un parcial de N bytes el salto arrancaba de 0 y
/// el `set_len` truncaba en vez de extender. Se comía lo ya copiado, el commit
/// lo publicaba y nadie lo comprobaba. Con `pos` explícito la invariante deja
/// de ser una afirmación en un comentario y pasa a ser cierta por construcción:
/// `pos` solo crece, así que el `set_len` solo puede extender.
///
/// La longitud se fija SOBRE LA MARCHA y no en el commit: `open_resumable`
/// deriva su `already` del tamaño del staging y `partial_digest` lee sus
/// primeros bytes. Con la longitud aplazada, un parcial que acabara en agujero
/// diría tener menos bytes de los que tiene.
///
/// Límite honesto: la unidad es el CHUNK. Un agujero más pequeño que un chunk,
/// o desalineado con él, se materializa — esto no busca huecos dentro de los
/// datos, solo se abstiene de escribir los que ya vienen enteros.
///
/// Windows: `set_len` sobre un handle abierto solo para APPEND puede contestar
/// `ERROR_ACCESS_DENIED`, así que el camino de reanudación con un chunk de
/// ceros está sin verificar ahí (#222, bloqueada por CI como #220 y #221).
pub(crate) fn write_maybe_sparse(
    file: &mut std::fs::File,
    pos: &mut u64,
    chunk: &[u8],
) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write as _};
    if chunk.is_empty() {
        return Ok(());
    }
    let len = chunk.len() as u64;
    if chunk.iter().any(|&b| b != 0) {
        file.write_all(chunk)?;
    } else {
        let fin = pos.checked_add(len).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "posición desbordada")
        })?;
        // Primero extender, después colocarse: en este orden la longitud nunca
        // pasa por un valor menor del que ya tenía.
        file.set_len(fin)?;
        file.seek(SeekFrom::Start(fin))?;
    }
    *pos += len;
    Ok(())
}

struct LocalSink {
    file: Option<std::fs::File>,
    /// Bytes ya entregados a este sink, agujeros incluidos. Es el ancla de
    /// [`write_maybe_sparse`]: el descriptor NO sabe dónde está cuando se
    /// abrió con `O_APPEND` para reanudar.
    pos: u64,
    partial: PathBuf,
    final_path: PathBuf,
    /// `true` cuando commit/abort ya se ocuparon del staging (Drop no toca nada).
    done: bool,
    /// El staging es el ESTABLE, o sea que nació `0o600` (#298) y hay que
    /// darle en el `commit` el modo que habría tenido una copia sin cortes
    /// (#299). El efímero nace `0o666` recortado por la umask y no necesita
    /// nada.
    estable: bool,
}

#[async_trait]
impl ByteSink for LocalSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let mut pos = self.pos;
        let (file, pos, res) = tokio::task::spawn_blocking(move || {
            let mut file = file;
            let res = write_maybe_sparse(&mut file, &mut pos, &chunk).map_err(|e| map_io(&e));
            (file, pos, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.file = Some(file);
        self.pos = pos;
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let partial = self.partial.clone();
        let final_path = self.final_path.clone();
        let estable = self.estable;
        let res = blocking(move || {
            file.sync_all().map_err(|e| map_io(&e))?;
            // El descriptor sigue VIVO durante el rename a propósito (#299):
            // el modo se arregla DESPUÉS de publicar y sobre el fd, no sobre
            // la ruta. Al revés —relajar el `0o600` mientras todavía se llama
            // `.norte-partial`— dejaría legible por otros un staging con el
            // nombre más predecible del directorio, y por un fichero que aún
            // no es el que nadie pidió.
            // No-replace atómico: la colisión aparecida entre write() y
            // commit() la detecta el PROPIO rename, sin ventana TOCTOU.
            match rename_noreplace(&partial, &final_path) {
                Ok(()) => {
                    reponer_modo_publicado(&file, estable);
                    Ok(())
                }
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
                            })?;
                            reponer_modo_publicado(&file, estable);
                            Ok(())
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
