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
    ///
    /// `None` = no se pudo averiguar, que NO es «no expande»: el llamante
    /// conserva lo que el provider declare en vez de apagar un flag que nadie
    /// contradijo.
    pub(crate) full_fold: Option<bool>,
}

/// Sondea `dir` y responde lo que la plataforma sepa decir SIN escribir nada.
/// BLOQUEANTE: va dentro de `spawn_blocking` (regla dura 2).
///
/// **Aquí no hay sonda de escritura, y es una decisión de seguridad.**
/// `capabilities_at` se responde detrás del gate de LECTURA
/// (`fs.capabilities`, `fs.compare`, `sync.plan`), así que un actor con
/// permiso de lectura sobre un directorio provocaría, si esta escalera
/// escribiera, la creación de un fichero ahí — sin pasar por el gate de
/// escritura (regla 9) y sin entrada de journal (regla 4). La sonda de
/// escritura sigue existiendo, para la raíz PROPIA del provider y una sola vez
/// (`probe_capabilities`), que es donde el provider ya tiene permiso por
/// construcción.
///
/// Consecuencia, dicha en vez de escondida: en un filesystem que esta escalera
/// no reconoce (tmpfs, btrfs, xfs, nfs, cifs, fuse…) la respuesta es «no lo
/// sé», y el llamante se queda con lo que el provider declara.
pub(crate) fn probe_location(dir: &Path) -> LocationCaps {
    fs_probe(dir).unwrap_or_default()
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
        // Ningún filesystem de Apple expande al plegar.
        full_fold: Some(false),
    })
}

#[cfg(windows)]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    windows::case_sensitive(dir)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn fs_probe(_dir: &Path) -> Option<LocationCaps> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::path::Path;

    use super::LocationCaps;

    /// `FS_CASEFOLD_FL` de `<linux/fs.h>`: el directorio está en `+F`.
    const FS_CASEFOLD_FL: libc::c_uint = 0x4000_0000;

    /// `FS_IOC_GETFLAGS`, o sea `_IOR('f', 1, long)`.
    ///
    /// La codificación miente sobre el tamaño y hay que respetarla igual: el
    /// número lleva `sizeof(long)` dentro, así que en 32 bits es
    /// `0x8004_6601` y en 64 bits `0x8008_6601` — pasar el de 64 en una
    /// máquina de 32 devuelve `ENOTTY` y apagaría la detección de `+F` en
    /// silencio. Lo que el kernel ESCRIBE, en cambio, son 4 bytes en los dos
    /// casos (`ioctl_getflags` hace `put_user` de un `unsigned int`), que es
    /// por lo que el buffer de abajo es un `c_uint` y no un `c_long`.
    pub(super) fn fs_ioc_getflags() -> libc::Ioctl {
        const IOC_READ: u64 = 2;
        let size = std::mem::size_of::<libc::c_long>() as u64;
        let request = (IOC_READ << 30) | (size << 16) | (u64::from(b'f') << 8) | 1;
        request as libc::Ioctl
    }

    // Magics de `<linux/magic.h>`. Solo están los filesystems cuya respuesta
    // se conoce SIN escribir; cualquier otro cae al peldaño siguiente.
    const EXT4: i64 = 0xEF53;
    const F2FS: i64 = 0xF2F5_2010;
    const MSDOS: i64 = 0x4d44;
    const EXFAT: i64 = 0x2011_BAB0;

    /// La respuesta de Linux, o `None` si este filesystem no la da sin
    /// escribir.
    ///
    /// **El flag `+F` NO se lee solo.** `FS_IOC_GETFLAGS` lo contesta también
    /// un vfat, que no tiene casefold y aun así no distingue caja: leer «sin
    /// `FS_CASEFOLD_FL`» como «distingue caja» convertiría cada FAT montado en
    /// un ext4 a ojos del comparador. El flag solo decide donde significa algo
    /// —ext4 y f2fs—, y el resto se responde por familia de filesystem.
    pub(super) fn probe(dir: &Path) -> Option<LocationCaps> {
        probe_from_magic(fs_type(dir)?, || directory_is_casefold(dir))
    }

    /// La decisión, separada de las syscalls para que cada arma se pueda
    /// probar sin un volumen de ese tipo montado — que es la única forma de
    /// probarlas en esta máquina.
    pub(super) fn probe_from_magic(
        magic: i64,
        casefold: impl FnOnce() -> Option<bool>,
    ) -> Option<LocationCaps> {
        match magic {
            EXT4 | F2FS => {
                let casefold = casefold()?;
                Some(LocationCaps {
                    case_sensitive: Some(!casefold),
                    full_fold: Some(casefold),
                })
            }
            // vfat y exFAT no distinguen caja SIEMPRE —vfat por definición,
            // exFAT por su tabla Up-case en disco, que es 1:1 y no expande— y
            // no hay opción de montaje que lo cambie. Es una respuesta que
            // también vale en un mount de solo lectura.
            MSDOS | EXFAT => Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(false),
            }),
            // NTFS NO entra aquí, y los dos drivers son la razón: `ntfs3`
            // compara SENSIBLE salvo con `-o nocase`, y el `ntfs` legacy hace
            // justo lo contrario. Dos defaults opuestos y los dos
            // sobreescribibles al montar = lo mismo que cifs, y se responde
            // igual: no lo sé.
            //
            // tmpfs, btrfs, xfs, nfs, cifs, fuse…: o depende de opciones de
            // montaje o no hay constante fiable. El llamante se queda con lo
            // que el provider declare.
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
        let mut flags: libc::c_uint = 0;
        // SAFETY: `file` está vivo durante toda la llamada y su fd es válido.
        // El handler del kernel (`ioctl_getflags`) hace `put_user` de un
        // `unsigned int` a través de este puntero — CUATRO bytes, pese a lo que
        // diga el `long` de la codificación del número—, y `flags` es
        // exactamente un `c_uint` propio y alineado. El retorno se comprueba
        // antes de leerlo.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), fs_ioc_getflags(), &raw mut flags) };
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

    use super::LocationCaps;

    /// Windows NO tiene peldaño de solo lectura, y `FILE_CASE_SENSITIVE_SEARCH`
    /// es la razón por la que no lo tiene.
    ///
    /// Ese flag de `GetVolumeInformationW` significa «el driver del volumen
    /// SABE sostener nombres sensibles a la caja», no «aquí las búsquedas
    /// distinguen caja»: NTFS lo trae puesto y aun así el gestor de objetos de
    /// Win32 pliega por encima del filesystem. Leerlo como respuesta convertiría
    /// cada NTFS en un volumen sensible, apagaría el plegado y con él la
    /// detección de colisiones `README`/`readme` en toda la plataforma — una
    /// regresión, no una mejora, y ninguna máquina de este proyecto compila
    /// Windows para verla.
    ///
    /// La respuesta por directorio que SÍ gobierna la resolución es
    /// `FileCaseSensitiveInformation` (`GetFileInformationByHandleEx`), y hasta
    /// que exista se contesta «no lo sé»: el provider declara su default
    /// —insensible— y su raíz sigue teniendo la sonda de escritura de siempre.
    pub(super) fn case_sensitive(_dir: &Path) -> Option<LocationCaps> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Cada arma de la tabla de magics, sin un volumen de ese tipo montado —
    /// que es la única forma de probarlas en esta máquina. La sonda del
    /// casefold se inyecta: si un arma la llama cuando no debe, se ve.
    #[test]
    fn la_tabla_de_magics_responde_lo_que_dice_responder() {
        let nunca = || panic!("este filesystem no debe preguntar por el flag +F");

        // ext4/f2fs: manda el flag del DIRECTORIO.
        assert_eq!(
            linux::probe_from_magic(0xEF53, || Some(true)),
            Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(true),
            }),
            "ext4 con +F: no distingue caja y EXPANDE"
        );
        assert_eq!(
            linux::probe_from_magic(0xEF53, || Some(false)),
            Some(LocationCaps {
                case_sensitive: Some(true),
                full_fold: Some(false),
            }),
            "ext4 sin +F: distingue caja"
        );
        assert_eq!(
            linux::probe_from_magic(0xF2F5_2010, || Some(true)),
            Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(true),
            }),
            "f2fs casefold, igual que ext4"
        );
        // Un ioctl que no contesta no se inventa una respuesta.
        assert_eq!(linux::probe_from_magic(0xEF53, || None), None);

        // vfat y exFAT: respuesta fija, sin preguntar por un flag que no
        // tienen.
        for magic in [0x4d44, 0x2011_BAB0] {
            assert_eq!(
                linux::probe_from_magic(magic, nunca),
                Some(LocationCaps {
                    case_sensitive: Some(false),
                    full_fold: Some(false),
                }),
                "familia FAT: no distingue caja y no expande ({magic:#x})"
            );
        }

        // Los dos NTFS del kernel dependen de opciones de montaje y tienen
        // defaults OPUESTOS entre sí: no se contesta por ellos.
        for magic in [0x5346_544e_i64, 0x7366_746E] {
            assert_eq!(
                linux::probe_from_magic(magic, nunca),
                None,
                "NTFS no se responde de memoria ({magic:#x})"
            );
        }

        // tmpfs, btrfs, xfs, nfs, cifs, fuse: fuera de la tabla.
        for magic in [
            0x0102_1994_i64,
            0x9123_683E,
            0x5846_5342,
            0x6969,
            0xFF53_4D42,
        ] {
            assert_eq!(linux::probe_from_magic(magic, nunca), None);
        }
    }

    /// El número del ioctl lleva `sizeof(long)` dentro, y equivocarlo devuelve
    /// `ENOTTY` — o sea, apaga la detección de `+F` sin decir nada.
    #[test]
    fn el_numero_del_ioctl_es_el_de_esta_arquitectura() {
        let esperado: libc::Ioctl = if std::mem::size_of::<libc::c_long>() == 8 {
            0x8008_6601_u64 as libc::Ioctl
        } else {
            0x8004_6601_u64 as libc::Ioctl
        };
        assert_eq!(linux::fs_ioc_getflags(), esperado);
    }
}
