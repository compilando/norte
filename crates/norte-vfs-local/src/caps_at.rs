//! Capabilities de UNA ubicación, no del backend entero (ADR 0054, #153/#145).
//!
//! Una máquina monta a la vez ext4, exFAT, SMB y un `+F`; `LocalProvider`
//! sirve todo eso tras un solo `file://`, así que preguntar por el provider
//! —que es lo que hacía `capabilities()`— responde por el mount de `base` y
//! calla lo que pasa en cualquier otro. Aquí se pregunta por el DIRECTORIO.
//!
//! **La escalera es de solo lectura primero, y no por pulcritud.**
//! `capabilities_at` se llama sobre cada raíz que alguien compara o
//! sincroniza, y la sonda histórica CREA un fichero (`.norte-probe-…`): en un
//! mount de solo lectura falla y no distingue «no escribible» de «no pliega»,
//! y en el directorio de otro la ven los watchers y las copias de seguridad.
//! El orden es syscall que no muta → sonda de escritura solo si nada respondió
//! y el directorio admite escritura.
//!
//! Ninguna respuesta se inventa: lo que la plataforma no sabe decir sale como
//! `None` y el llamante se queda con lo que el provider declara (ADR 0054 —
//! `Capabilities` no sabe decir «no lo sé», y su degradación es el
//! comportamiento de siempre).

use std::path::Path;

/// Lo que una sonda averiguó sobre UN directorio.
///
/// `case_sensitive: None` = ninguna rama de la escalera supo responder; el
/// llamante conserva la declaración del provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LocationCaps {
    /// ¿`Foo` y `foo` son dos nombres en este directorio?
    pub(crate) case_sensitive: Option<bool>,
    /// ¿El plegado de este directorio EXPANDE (`ß` → `ss`)? Solo lo hace la
    /// casefold de ext4/f2fs, y solo si el directorio lleva el flag.
    pub(crate) full_fold: bool,
}

/// Sondea `dir` y responde lo que la plataforma sepa decir. BLOQUEANTE: va
/// dentro de `spawn_blocking` (regla dura 2).
///
/// `probe_write` es la sonda de escritura del provider, pasada como argumento
/// para que este módulo no dependa del orden de declaración de `provider.rs` —
/// y para que el último peldaño se pueda desactivar en un test sin tocar los
/// demás.
pub(crate) fn probe_location(
    dir: &Path,
    probe_write: impl FnOnce(&Path) -> Option<bool>,
) -> LocationCaps {
    let mut caps = LocationCaps::default();

    if let Some(fs) = fs_probe(dir) {
        caps = fs;
    }

    if caps.case_sensitive.is_none() {
        caps.case_sensitive = probe_write(dir);
    }

    caps
}

/// Peldaños de plataforma que NO mutan nada. `None` = esta plataforma (o este
/// filesystem) no sabe responder sin escribir.
#[cfg(target_os = "linux")]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    linux::probe(dir)
}

/// macOS responde por VOLUMEN y sin escribir desde siempre (`pathconf`);
/// ningún filesystem de Apple expande al plegar.
#[cfg(target_os = "macos")]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    Some(LocationCaps {
        case_sensitive: Some(macos::case_sensitive(dir)?),
        full_fold: false,
    })
}

#[cfg(windows)]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    Some(LocationCaps {
        case_sensitive: Some(windows::case_sensitive(dir)?),
        full_fold: false,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn fs_probe(_dir: &Path) -> Option<LocationCaps> {
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::path::Path;

    use super::LocationCaps;

    /// `FS_CASEFOLD_FL` de `<linux/fs.h>`: el directorio está en `+F`.
    const FS_CASEFOLD_FL: libc::c_long = 0x4000_0000;
    /// `FS_IOC_GETFLAGS`: `_IOR('f', 1, long)`.
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;

    // Magics de `<linux/magic.h>`. Solo están los filesystems cuya respuesta
    // se conoce SIN escribir; cualquier otro cae al peldaño siguiente.
    const EXT4: i64 = 0xEF53;
    const F2FS: i64 = 0xF2F5_2010;
    const MSDOS: i64 = 0x4d44;
    const EXFAT: i64 = 0x2011_BAB0;
    const NTFS: i64 = 0x5346_544e;
    const NTFS3: i64 = 0x7366_746E;

    /// La respuesta de Linux, o `None` si este filesystem no la da sin
    /// escribir.
    ///
    /// **El flag `+F` NO se lee solo.** `FS_IOC_GETFLAGS` lo contesta también
    /// un vfat, que no tiene casefold y aun así no distingue caja: leer «sin
    /// `FS_CASEFOLD_FL`» como «distingue caja» convertiría cada FAT montado en
    /// un ext4 a ojos del comparador. El flag solo decide donde significa algo
    /// —ext4 y f2fs—, y el resto se responde por familia de filesystem.
    pub(super) fn probe(dir: &Path) -> Option<LocationCaps> {
        match fs_type(dir)? {
            EXT4 | F2FS => {
                let casefold = directory_is_casefold(dir)?;
                Some(LocationCaps {
                    case_sensitive: Some(!casefold),
                    full_fold: casefold,
                })
            }
            // La familia FAT y los dos NTFS del kernel no distinguen caja y no
            // expanden al plegar. Es una respuesta que también vale en un
            // mount de solo lectura, que es donde la sonda de escritura calla.
            MSDOS | EXFAT | NTFS | NTFS3 => Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: false,
            }),
            // tmpfs, btrfs, xfs, nfs, cifs, fuse…: o depende de opciones de
            // montaje (cifs) o no hay constante fiable. Lo sabe la sonda de
            // escritura, y si tampoco puede, se declara lo del provider.
            _ => None,
        }
    }

    /// `statfs.f_type` de `dir`. `None` si la llamada falla.
    #[allow(unsafe_code)]
    fn fs_type(dir: &Path) -> Option<i64> {
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: `c` es una CString NUL-terminada viva durante toda la
        // llamada; `statfs` escribe un `struct statfs` completo en el puntero,
        // y `buf` es exactamente eso, propio y alineado. Solo se lee tras
        // comprobar que la llamada devolvió 0.
        let rc = unsafe { libc::statfs(c.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: `statfs` devolvió 0, así que dejó `buf` inicializado.
        let st = unsafe { buf.assume_init() };
        // El tipo de `f_type` cambia con la arquitectura y la libc (`i64` en
        // glibc/x86_64, `u32` en algunas musl de 32 bits), así que la
        // conversión es redundante SOLO en el objetivo que compila hoy.
        #[allow(clippy::useless_conversion)]
        i64::try_from(st.f_type).ok()
    }

    /// ¿Este DIRECTORIO lleva el flag casefold (`chattr +F`)?
    ///
    /// `None` = el filesystem no responde a este ioctl o el directorio no se
    /// pudo abrir. Solo se pregunta donde el flag significa algo.
    #[allow(unsafe_code)]
    fn directory_is_casefold(dir: &Path) -> Option<bool> {
        // O_PATH no vale: el ioctl exige un fd de verdad. O_RDONLY sobre un
        // directorio no lee nada y no lo muta.
        let file = std::fs::File::open(dir).ok()?;
        let mut flags: libc::c_long = 0;
        // SAFETY: `file` está vivo durante toda la llamada y su fd es válido;
        // `FS_IOC_GETFLAGS` escribe un `long` en el puntero que se le pasa, y
        // `flags` es exactamente un `long` propio y alineado. El valor de
        // retorno se comprueba antes de leerlo.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_GETFLAGS, &raw mut flags) };
        if rc != 0 {
            return None;
        }
        Some(flags & FS_CASEFOLD_FL != 0)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// `pathconf(_PC_CASE_SENSITIVE)`, por VOLUMEN y sin mutar nada.
    #[allow(unsafe_code)]
    pub(super) fn case_sensitive(dir: &Path) -> Option<bool> {
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` es una CString NUL-terminada viva durante toda la
        // llamada; `_PC_CASE_SENSITIVE` es constante de la ABI. Se valida en
        // `tests/local.rs` contra el FS real.
        let rc = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
        match rc {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::path::Path;

    /// `FILE_CASE_SENSITIVE_SEARCH` de `lpFileSystemFlags`.
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;

    /// ¿Distingue caja el volumen que sostiene `dir`?
    ///
    /// Es del VOLUMEN: el flag por-directorio de WSL
    /// (`FILE_CASE_SENSITIVE_INFORMATION`) no se consulta aquí, y su ausencia
    /// se comporta como cualquier otro `None` de esta escalera.
    ///
    /// `None` si `dir` no empieza por una letra de unidad (una ruta UNC lo
    /// hace) o si `GetVolumeInformationW` no respondió.
    pub(super) fn case_sensitive(dir: &Path) -> Option<bool> {
        let letter = drive_letter(dir)?;
        let info = crate::mounts_windows::volume_info(letter)?;
        Some(info.flags & FILE_CASE_SENSITIVE_SEARCH != 0)
    }

    /// Letra de unidad de una ruta nativa, con o sin prefijo verbatim.
    fn drive_letter(dir: &Path) -> Option<u8> {
        // Bytes, jamás `to_str` (regla dura 1): una ruta de Windows es WTF-8
        // en este crate y puede no ser UTF-8 válido, y la letra de unidad se
        // lee igual de bien byte a byte.
        let bytes = dir.as_os_str().as_encoded_bytes();
        let bytes = bytes.strip_prefix(br"\\?\").unwrap_or(bytes);
        let letter = *bytes.first()?;
        (bytes.get(1) == Some(&b':') && letter.is_ascii_alphabetic())
            .then(|| letter.to_ascii_uppercase())
    }
}
