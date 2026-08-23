//! Conversión `VPath` ↔ paths nativos y prefijo `\\?\` (paths >260, nombres
//! reservados, trailing dots/spaces).
//!
//! Vive AQUÍ y no en el provider local porque son reglas de la FORMA de una
//! ruta, no acceso al disco, y hay dos frontends que las necesitan sin querer
//! un provider: el terminal para pasarle una ruta a una herramienta de shell,
//! y la ventana gráfica para saber dónde arranca. Tenerlas en
//! `norte-vfs-local` obligaba a los dos a arrastrar el único crate del
//! proyecto que puede usar `unsafe`, con `openat2` y `ConfinedRoot` dentro, a
//! un proceso cuyo único transporte es un socket — justo lo contrario de lo
//! que promete la ADR 0066 (#254). Tampoco caben en `norte-proto`: eso es el
//! wire, y un `Path` del sistema no cruza ningún cable.
//!
//! Frontera de seguridad (ADR 0001): en Windows los bytes de un segmento se
//! validan como WTF-8 y se DECODIFICAN a UTF-16 (`OsStringExt::from_wide`) —
//! cero `unsafe`: la reconstrucción unchecked de `OsStr` queda prohibida
//! porque su contrato ("bytes de `as_encoded_bytes` de la misma versión de
//! Rust") no cubre bytes llegados del wire.

#[cfg(unix)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use norte_proto::{Error, VPath};

/// Bytes crudos de un `OsStr` (la forma que guarda `Segment`).
///
/// Unix: los bytes del OS tal cual. Windows: WTF-8 (`as_encoded_bytes`).
///
/// Hoisted to [`norte_vfs::wtf8::os_to_bytes`] (2026-08-10-volumes.md task
/// V4): `norte-core::volumes::windows` needs the exact same conversion for
/// `Volume::label`, and `norte-proto`/`norte-core` cannot depend on this
/// crate (wrong direction), so the one function both sides need now lives in
/// `norte-vfs`, which both already depend on. This is a thin re-export so the
/// two call sites below do not have to spell the other crate's path.
pub(crate) use crate::wtf8::os_to_bytes;

/// Reconstruye un `OsString` desde los bytes de un segmento.
///
/// # Errors
/// [`Error::InvalidPath`] en Windows si los bytes no son WTF-8 válido
/// (imposible como nombre de archivo Windows; además la reconstrucción
/// unchecked sería unsound).
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // firma común con la variante Windows, que sí falla
pub fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::unix::ffi::OsStrExt;
    // Unix: cualquier byte es válido en un nombre; conversión segura 1:1.
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

/// Reconstruye un `OsString` desde los bytes de un segmento (Windows: WTF-8
/// validado → UTF-16 → `from_wide`, sin `unsafe`).
///
/// # Errors
/// [`Error::InvalidPath`] si los bytes no son WTF-8 válido, o si contienen
/// `\` (separador también bajo `\\?\`: un segmento produciría DOS
/// componentes) o `:` (Alternate Data Stream de NTFS: los datos acabarían
/// escondidos en un stream que `list` jamás devuelve).
#[cfg(windows)]
pub fn bytes_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.contains(&b'\\') || bytes.contains(&b':') {
        return Err(Error::InvalidPath);
    }
    let wide = norte_vfs::wtf8::decode_to_wide(bytes).ok_or(Error::InvalidPath)?;
    Ok(OsString::from_wide(&wide))
}

/// Destino de un symlink → `OsString`. Unix: bytes tal cual. El target NO
/// es un segmento: no se le aplican las restricciones de `bytes_to_os`.
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // firma común con la variante Windows
pub fn link_target_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::unix::ffi::OsStrExt;
    Ok(OsStr::from_bytes(bytes).to_os_string())
}

/// Destino de un symlink → `OsString` (Windows): WTF-8 validado, SIN las
/// restricciones de segmento — un target legítimo contiene `\` y `:`.
#[cfg(windows)]
pub fn link_target_to_os(bytes: &[u8]) -> Result<OsString, Error> {
    use std::os::windows::ffi::OsStringExt;
    let wide = norte_vfs::wtf8::decode_to_wide(bytes).ok_or(Error::InvalidPath)?;
    Ok(OsString::from_wide(&wide))
}

/// Path nativo de `p` bajo `base`: `base/<seg1>/<seg2>/…`.
///
/// En Windows el resultado va SIEMPRE con prefijo verbatim `\\?\`
/// (paths largos de más de 260, `CON`/`NUL`, trailing dots/spaces intactos).
/// Caso especial Windows:
/// `base` vacío = "raíz del OS" — el PRIMER segmento es el prefijo de unidad
/// (`C:`) y se le restituye su separador (evita el path drive-relative
/// `C:Users` que produciría un `push` ingenuo).
pub fn to_native(base: &Path, p: &VPath) -> Result<PathBuf, Error> {
    let mut segs = p.segments();
    let mut out = if cfg!(windows) && base.as_os_str().is_empty() {
        let Some(first) = segs.next() else {
            return Err(Error::InvalidPath);
        };
        // El primer segmento es el prefijo de la raíz del OS: unidad (`C:`)
        // o UNC/verbatim (`\\server\share`, `\\?\…`). No pasa por
        // bytes_to_os (que rechaza `\`/`:` como separador/ADS en nombres):
        // el prefijo es el único sitio donde son legales.
        os_root_base(first)?
    } else {
        base.to_path_buf()
    };
    for seg in segs {
        out.push(bytes_to_os(seg)?);
    }
    Ok(verbatim(out))
}

/// Base `PathBuf` de la raíz del OS Windows desde el primer segmento del
/// `VPath`: unidad `X:` (con su separador restituido, evita el path
/// drive-relative `C:Users`) o prefijo UNC/verbatim `\\…` (#22 — un cwd
/// `\\server\share` ya no aborta el arranque; [`verbatim`] lo normaliza
/// luego a `\\?\UNC\…`). El prefijo se reconstruye SIN las restricciones de
/// segmento porque legítimamente contiene `\` y `:`.
fn os_root_base(bytes: &[u8]) -> Result<PathBuf, Error> {
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
        let mut drive = OsString::from(s);
        drive.push(std::path::MAIN_SEPARATOR_STR);
        return Ok(PathBuf::from(drive));
    }
    if is_bare_windows_prefix(bytes) {
        // WTF-8 → OsString SIN las restricciones de segmento (el prefijo
        // lleva `\` y `:` legítimamente); reutiliza el decodificador sin
        // restricciones de `link_target_to_os`. La FORMA ya la validó
        // `is_bare_windows_prefix` (una sola componente Prefix, no gorda).
        return Ok(PathBuf::from(link_target_to_os(bytes)?));
    }
    Err(Error::InvalidPath)
}

/// `true` si los bytes son EXACTAMENTE un prefijo de raíz Windows «desnudo»
/// (una sola componente `Prefix`, SIN cola de path): UNC `\\server\share`,
/// verbatim-disk `\\?\C:` o verbatim-UNC `\\?\UNC\server\share`. Reconocedor
/// a nivel de bytes — compilado y testeado en todo OS; en Windows estos
/// bytes vienen de `Component::Prefix::as_os_str`.
///
/// Rechaza a propósito (más estricto que `std`, review #22):
/// - el namespace de DISPOSITIVO `\\.\…` (I/O de disco/pipe crudo, no
///   navegación — regresión de mínimo privilegio),
/// - un primer segmento GORDO con cola (`\\?\C:\Windows\…`): expandiría a
///   varias componentes nativas saltándose el guard por-segmento de
///   `bytes_to_os` (que rechaza `\`/`:`),
/// - formas verbatim raras (Volume GUID): fail-closed a `InvalidPath`, jamás
///   un path nativo malformado.
fn is_bare_windows_prefix(bytes: &[u8]) -> bool {
    if let Some(rest) = bytes.strip_prefix(br"\\?\") {
        // Verbatim-UNC `\\?\UNC\server\share`.
        if let Some(unc) = rest.strip_prefix(br"UNC\") {
            return is_bare_unc_body(unc);
        }
        // Verbatim-disk `\\?\C:` — letra de unidad + `:`, nada más.
        return rest.len() == 2 && rest[0].is_ascii_alphabetic() && rest[1] == b':';
    }
    if let Some(unc) = bytes.strip_prefix(br"\\") {
        return is_bare_unc_body(unc);
    }
    false
}

/// `server\share` con ambos no vacíos y SIN más `\` (exactamente dos
/// componentes). `server` no puede ser `.` (dispositivo) ni `?` (marcador
/// verbatim): esos van por otras ramas o se rechazan.
fn is_bare_unc_body(body: &[u8]) -> bool {
    let mut parts = body.split(|&b| b == b'\\');
    let (Some(server), Some(share), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !server.is_empty() && !share.is_empty() && server != b"." && server != b"?"
}

/// Convierte un `VPath` `file://` (sin authority) a su path NATIVO — la
/// inversa de [`vpath_from_native`]. La base es la raíz del OS (igual que
/// `LocalProvider::os_root`): `/` en unix; en Windows la unidad/UNC
/// que viaje en el primer segmento. Byte a byte (regla 1).
///
/// Uso: un frontend que necesita la ruta real para lanzar un programa
/// externo (opener, #28) sobre un fichero local.
///
/// # Errors
/// [`Error::InvalidPath`] si el scheme no es `file`, si lleva authority (es
/// de OTRO provider), o si algún segmento no es representable nativamente.
///
/// ```
/// use norte_proto::{Scheme, Segment, VPath};
/// let vp = VPath::root(Scheme::new("file").unwrap(), None)
///     .join(Segment::new(b"etc".to_vec()).unwrap())
///     .join(Segment::new(b"hosts".to_vec()).unwrap());
/// # #[cfg(unix)]
/// assert_eq!(
///     norte_vfs_local::vpath_to_native(&vp).unwrap(),
///     std::path::Path::new("/etc/hosts")
/// );
/// ```
pub fn vpath_to_native(p: &VPath) -> Result<PathBuf, Error> {
    if p.scheme() != "file" || p.authority().is_some() {
        return Err(Error::InvalidPath);
    }
    // Misma base que `LocalProvider::os_root`: vacía en Windows (el primer
    // segmento es la unidad/UNC), `/` en unix.
    let base = if cfg!(windows) {
        PathBuf::new()
    } else {
        PathBuf::from("/")
    };
    to_native(&base, p)
}

/// Convierte un path NATIVO absoluto a `VPath` (`file:///…`), byte a byte.
/// La inversa de la resolución de `LocalProvider::os_root`.
///
/// # Errors
/// [`Error::InvalidPath`] si el path no puede normalizarse o contiene
/// componentes no representables como segmentos.
///
/// # Panics
/// Nunca: el scheme `file` es constante y válido.
pub fn vpath_from_native(path: &Path) -> Result<VPath, Error> {
    use norte_proto::{Scheme, Segment};
    let abs = std::path::absolute(path).map_err(|_| Error::InvalidPath)?;
    let mut out = VPath::root(Scheme::new("file").expect("scheme constante válido"), None);
    for comp in abs.components() {
        use std::path::Component;
        match comp {
            Component::RootDir => {}
            Component::Prefix(pr) => {
                // Windows: la unidad (`C:`) o el UNC viajan como primer segmento.
                let seg =
                    Segment::new(os_to_bytes(pr.as_os_str())).map_err(|_| Error::InvalidPath)?;
                out = out.join(seg);
            }
            Component::Normal(os) => {
                let seg = Segment::new(os_to_bytes(os)).map_err(|_| Error::InvalidPath)?;
                out = out.join(seg);
            }
            // `absolute` no resuelve `..` contra el FS pero sí los pliega
            // lexicalmente en Windows; en unix pueden sobrevivir: rechazo.
            Component::CurDir | Component::ParentDir => return Err(Error::InvalidPath),
        }
    }
    Ok(out)
}

/// Aplica el prefijo verbatim en Windows; identidad en el resto.
#[cfg(not(windows))]
pub fn verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Aplica el prefijo verbatim en Windows; identidad en el resto.
#[cfg(windows)]
pub fn verbatim(p: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    // Ya verbatim: no tocar.
    if let Some(Component::Prefix(pr)) = p.components().next() {
        match pr.kind() {
            Prefix::Verbatim(_) | Prefix::VerbatimUNC(..) | Prefix::VerbatimDisk(_) => return p,
            Prefix::UNC(server, share) => {
                // \\server\share\… → \\?\UNC\server\share\…
                let mut out = PathBuf::from(r"\\?\UNC");
                out.push(server);
                out.push(share);
                for c in p.components() {
                    match c {
                        Component::Prefix(_) | Component::RootDir => {}
                        other => out.push(other.as_os_str()),
                    }
                }
                return out;
            }
            _ => {}
        }
    }
    let mut s = OsString::from(r"\\?\");
    s.push(p.as_os_str());
    PathBuf::from(s)
}

#[cfg(test)]
mod root_base_tests {
    use super::{is_bare_windows_prefix, os_root_base};
    use norte_proto::Error;

    #[test]
    fn acepta_solo_prefijos_desnudos_unc_y_verbatim() {
        // UNC y verbatim «desnudos» (una sola componente Prefix): aceptados.
        assert!(is_bare_windows_prefix(br"\\server\share"));
        assert!(is_bare_windows_prefix(br"\\wsl$\Ubuntu")); // \\wsl$ del issue
        assert!(is_bare_windows_prefix(br"\\?\C:"));
        assert!(is_bare_windows_prefix(br"\\?\UNC\server\share"));
    }

    #[test]
    fn rechaza_dispositivo_gordos_y_malformados() {
        // Namespace de dispositivo: I/O crudo, NO navegación (review #22).
        assert!(!is_bare_windows_prefix(br"\\.\PhysicalDrive0"));
        assert!(!is_bare_windows_prefix(br"\\.\C:"));
        // Primer segmento GORDO con cola: saltaría el guard por-segmento.
        assert!(!is_bare_windows_prefix(br"\\?\C:\Windows"));
        assert!(!is_bare_windows_prefix(br"\\server\share\dir"));
        // UNC incompleto / malformado.
        assert!(!is_bare_windows_prefix(br"\\server"));
        assert!(!is_bare_windows_prefix(br"\\"));
        assert!(!is_bare_windows_prefix(br"\single"));
        assert!(!is_bare_windows_prefix(b"C:"));
        assert!(!is_bare_windows_prefix(b"normal"));
    }

    #[test]
    fn os_root_base_acepta_unidad_y_unc_desnudo() {
        // Unidad: aceptada, con su separador restituido.
        let drive = os_root_base(b"C:").expect("unidad válida");
        assert!(drive.to_string_lossy().starts_with("C:"));
        // #22: el prefijo UNC desnudo ya no se rechaza (antes = no-arranque).
        assert!(os_root_base(br"\\server\share").is_ok());
        assert!(os_root_base(br"\\?\C:").is_ok());
        // Pero un fat/dispositivo SÍ se rechaza (mínimo privilegio).
        assert_eq!(
            os_root_base(br"\\.\PhysicalDrive0"),
            Err(Error::InvalidPath)
        );
        assert_eq!(os_root_base(br"\\?\C:\Windows"), Err(Error::InvalidPath));
    }

    /// #28 encoding: un nombre con bytes NO-UTF8 sobrevive byte a byte por el
    /// round-trip nativo → `vpath_to_native` → nativo (unix). Guard del inverso
    /// de `vpath_from_native` a nivel de fixture.
    #[cfg(unix)]
    #[test]
    fn vpath_to_native_round_trip_bytes_no_utf8() {
        use super::vpath_to_native;
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;
        let native = Path::new(OsStr::from_bytes(b"/x/\xff\xfe.txt"));
        let vpath = super::vpath_from_native(native).expect("vpath");
        let back = vpath_to_native(&vpath).expect("nativo");
        assert_eq!(back.as_os_str().as_bytes(), b"/x/\xff\xfe.txt");
    }

    #[test]
    fn os_root_base_rechaza_primer_segmento_no_prefijo() {
        // Ni unidad ni UNC: un nombre normal como raíz del OS es InvalidPath.
        assert_eq!(os_root_base(b"Users"), Err(Error::InvalidPath));
        assert_eq!(os_root_base(b"C"), Err(Error::InvalidPath));
        assert_eq!(os_root_base(b""), Err(Error::InvalidPath));
    }

    /// #22: round-trip nativo real de un cwd UNC (SOLO Windows: `to_native`
    /// bajo raíz vacía es camino `cfg!(windows)`, y la clasificación
    /// `Component::Prefix` es semántica de Windows). Bloquea la corrección
    /// cuando la CI corra en Windows; en Linux este test no compila el cuerpo.
    #[cfg(windows)]
    #[test]
    fn unc_cwd_round_trip_byte_exacto() {
        use super::{to_native, vpath_from_native};
        use std::path::Path;
        let native = Path::new(r"\\server\share\dir");
        let vpath = vpath_from_native(native).expect("vpath desde UNC");
        // El primer segmento es el prefijo desnudo, `dir` va aparte.
        let back = to_native(Path::new(""), &vpath).expect("to_native UNC");
        // verbatim() canoniza UNC → \\?\UNC\server\share\dir (mismo fichero).
        assert_eq!(back, Path::new(r"\\?\UNC\server\share\dir"));
    }
}
