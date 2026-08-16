//! Escribir bajo una raíz sin poder salirse de ella (#164, ADR 0054).
//!
//! El agujero que cierra: una sincronización valida sus dos raíces y después
//! compone `dest_root + rel` paso a paso. Si un componente INTERMEDIO de ese
//! relativo es un symlink que apunta fuera, la escritura aterriza fuera y
//! ninguna de las tres comprobaciones de solape lo ve — todas razonan sobre las
//! raíces, y las raíces están bien.
//!
//! **Componer la ruta y comprobarla antes de abrir es TOCTOU por
//! construcción**, así que aquí no se compone ninguna: el caller abre la raíz
//! UNA vez y a partir de ahí direcciona segmentos relativos. Lo que se sostiene
//! es un descriptor de directorio, y un descriptor no se puede sustituir por un
//! symlink entre dos syscalls.
//!
//! # Qué se prohíbe exactamente
//!
//! Salirse de la raíz, y esa es la regla completa — **no** «que haya
//! symlinks». Un symlink RELATIVO que apunta a otro sitio dentro de la raíz se
//! sigue, porque prohibirlo rompería árboles legítimos (un `dst/data ->
//! almacen` corriente) sin ganar seguridad ninguna.
//!
//! Uno ABSOLUTO se rechaza aunque su destino caiga dentro. No es una decisión
//! de este módulo: es lo que hace `RESOLVE_BENEATH` —para el kernel una ruta
//! absoluta ya empieza fuera de la raíz—, y el paseo de emulación la copia
//! porque las dos ramas tienen que dar el MISMO veredicto. Un confinamiento
//! que significara una cosa con `openat2` y otra sin él no sería una garantía,
//! sería una lotería de versión de kernel.
//!
//! # Cómo, por plataforma
//!
//! | dónde | cómo |
//! | --- | --- |
//! | Linux ≥5.6 | `openat2(RESOLVE_BENEATH)` — lo garantiza el kernel, paso a paso |
//! | Linux <5.6, seccomp (`ENOSYS`/`EPERM`), macOS | paseo componente a componente: `openat(O_NOFOLLOW)` relativo al fd anterior, y el `ELOOP` que eso da ES la respuesta a «¿era un symlink?» — absoluto se rechaza, relativo se sigue y se comprueba por contención SOBRE EL DESCRIPTOR |
//!
//! Las dos ramas dan el mismo veredicto en todo lo que importa, con una
//! diferencia conocida y en la dirección segura: `openat2` rechaza un symlink
//! relativo que SALE y vuelve a entrar (`../../raiz/dentro`) porque mira el
//! camino, y el paseo lo acepta porque mira dónde acaba. Y la subida de
//! [`is_beneath`] es best-effort bajo renombrados concurrentes, mientras que
//! `openat2` no tiene esa ventana: donde el kernel sabe hacerlo, es el kernel
//! quien lo hace.
//! | Windows | no hay `openat`: este módulo no existe ahí y `open_root` responde `Unsupported` |

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use norte_proto::{ConflictKind, Entry, EntryKind, Error, Segment};

/// Tope de niveles que sube la comprobación de contención antes de rendirse.
/// Un árbol de más de 256 niveles ya no es un árbol de ficheros de nadie.
const MAX_CLIMB: usize = 256;

/// Una raíz abierta, y el único sitio desde el que se direcciona bajo ella.
///
/// El `fd` es el objeto: mientras viva, apunta al directorio que se abrió,
/// aunque alguien renombre o sustituya la ruta por la que se abrió.
#[derive(Debug)]
pub(crate) struct LocalRoot {
    fd: OwnedFd,
}

impl LocalRoot {
    /// Abre `dir` como raíz confinada.
    ///
    /// BLOQUEANTE: va dentro de `spawn_blocking` (regla dura 2).
    #[allow(unsafe_code)]
    pub(crate) fn open(dir: &Path) -> Result<Self, Error> {
        use std::os::unix::ffi::OsStrExt as _;
        let c = CString::new(dir.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath)?;
        // SAFETY: `c` es una CString NUL-terminada viva durante toda la
        // llamada. `O_PATH` no lee ni escribe nada: solo nombra el nodo, que es
        // todo lo que hace falta para usarlo como dirfd de los `*at`.
        let raw = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `raw` es un fd recién abierto y todavía sin dueño; `OwnedFd`
        // pasa a ser el único que lo cierra.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw) },
        })
    }

    /// El directorio PADRE de `rel`, abierto bajo la raíz y sin haber podido
    /// salirse de ella. `rel` vacío es la propia raíz.
    ///
    /// Devuelve el fd del padre y el último segmento, que es el nombre sobre el
    /// que operar con un `*at` de un solo componente — y un solo componente no
    /// puede escaparse de ningún sitio.
    pub(crate) fn parent_of<'a>(
        &self,
        rel: &'a [Segment],
    ) -> Result<(OwnedFd, &'a Segment), Error> {
        let (last, parents) = rel.split_last().ok_or(Error::InvalidPath)?;
        let fd = self.resolve_dir(parents)?;
        Ok((fd, last))
    }

    /// Abre el directorio `parents` bajo la raíz, confinado.
    fn resolve_dir(&self, parents: &[Segment]) -> Result<OwnedFd, Error> {
        if parents.is_empty() {
            return dup(self.fd.as_raw_fd());
        }
        #[cfg(target_os = "linux")]
        if !force_walk() {
            match openat2_beneath(self.fd.as_raw_fd(), parents) {
                // `ENOSYS` = kernel <5.6 o un seccomp que no lo deja pasar: el
                // paseo hace lo mismo, una syscall por componente.
                Err(e) if is_enosys(&e) => {}
                other => return other.map_err(|e| map_errno(&e)),
            }
        }
        walk_beneath(self.fd.as_raw_fd(), parents)
    }
}

/// `dup` de un fd, para que «la raíz misma» se devuelva con el mismo tipo que
/// cualquier otro directorio resuelto.
#[allow(unsafe_code)]
fn dup(fd: RawFd) -> Result<OwnedFd, Error> {
    // SAFETY: `fd` está vivo (lo sostiene el `OwnedFd` del llamante) y
    // `F_DUPFD_CLOEXEC` devuelve un fd nuevo del que nadie más es dueño.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` es un fd recién creado y sin dueño.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `openat2` con `RESOLVE_BENEATH`: el kernel rechaza cualquier resolución que
/// se salga de `root`, symlink intermedio incluido, sin que nadie tenga que
/// comprobar nada entre dos llamadas.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn openat2_beneath(root: RawFd, parents: &[Segment]) -> Result<OwnedFd, std::io::Error> {
    // El número de `openat2` es 437 en toda arquitectura Linux que este
    // workspace compila; `libc` no expone la constante para x86_64-gnu.
    const SYS_OPENAT2: libc::c_long = 437;

    /// `struct open_how` de `<linux/openat2.h>`. `libc` la declara
    /// `#[non_exhaustive]`, así que no se puede construir desde fuera del
    /// crate; la ABI es de tres `u64` y está CONGELADA por diseño (el kernel
    /// la extiende añadiendo campos al final y comprobando el tamaño que se le
    /// pasa, que es justo lo que hace la llamada de abajo).
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let joined = join(parents);
    let c = CString::new(joined).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        // BENEATH prohíbe SALIRSE; los symlinks que no salen se siguen, que es
        // lo que hace el paseo de emulación con su comprobación de contención.
        // NO_MAGICLINKS cierra `/proc/*/fd/*`, que sí es un escape con forma de
        // ruta corriente.
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: `c` vive durante toda la llamada; `how` es un `open_how`
    // completo, propio y alineado, y se pasa su tamaño exacto como exige la
    // ABI extensible de `openat2`. El retorno se comprueba antes de usarse.
    let raw = unsafe {
        libc::syscall(
            SYS_OPENAT2,
            root,
            c.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let raw = RawFd::try_from(raw).map_err(|_| std::io::Error::from_raw_os_error(libc::EBADF))?;
    // SAFETY: `raw` es un fd recién abierto por el kernel y sin dueño.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// El paseo: un `openat` por componente, siempre relativo al fd anterior.
///
/// Nunca hay una ruta que recomponer, así que la garantía es la misma que la
/// del kernel aunque el mecanismo sea otro. Un componente que es symlink se
/// SIGUE y después se comprueba que lo seguido cae bajo la raíz — la
/// alternativa (negarse a seguir ningún symlink) sería más estricta que
/// `RESOLVE_BENEATH` y rompería árboles legítimos.
///
/// **Se abre SIEMPRE con `O_NOFOLLOW` y el symlink se descubre por el `ELOOP`
/// que eso produce**, jamás preguntando antes si el nombre es un symlink.
/// Preguntar primero y abrir después son dos syscalls sobre un NOMBRE, y entre
/// las dos cabe una sustitución: la revisión de esta fase encontró justo eso —
/// el componente que el `lstat` había visto como directorio se abría sin
/// `O_NOFOLLOW` y sin comprobar contención, así que un cambiazo ganado en esa
/// rendija mandaba el resto del paseo fuera de la raíz. Un `openat` que falla
/// no ha abierto nada, y lo que se comprueba después es siempre el DESCRIPTOR
/// ya abierto, no un nombre.
fn walk_beneath(root: RawFd, parents: &[Segment]) -> Result<OwnedFd, Error> {
    let root_id = node_id_of(root)?;
    let mut current = dup(root)?;
    for seg in parents {
        let name = CString::new(seg.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
        match openat_dir_nofollow(current.as_raw_fd(), &name) {
            Ok(next) => current = next,
            // Con `O_NOFOLLOW` un symlink final sale como `ELOOP`… salvo que
            // también vaya `O_DIRECTORY`, y entonces el kernel prefiere
            // contestar `ENOTDIR` —que es además la respuesta legítima para un
            // fichero corriente en medio de la ruta—. Así que los dos errnos
            // significan «quizá era un symlink», y quien lo desempata es el
            // `readlinkat` de abajo: si no lo era, el error original vale.
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                // Un symlink ABSOLUTO se rechaza aunque apunte dentro. Es lo
                // que hace `RESOLVE_BENEATH` —para el kernel una ruta absoluta
                // ya «empieza» fuera de la raíz— y las dos ramas tienen que dar
                // el mismo veredicto o el confinamiento significaría una cosa
                // en un kernel y otra en el de al lado.
                match link_target_is_absolute(current.as_raw_fd(), &name)? {
                    // No era un symlink: un fichero corriente donde la ruta
                    // pedía un directorio. Su error es el que vale.
                    None => return Err(map_errno(&e)),
                    Some(true) => {
                        return Err(Error::Conflict {
                            conflict: ConflictKind::EscapesRoot,
                        });
                    }
                    Some(false) => {}
                }
                // Seguirlo: aquí sí se abre sin `O_NOFOLLOW`, y da igual que
                // entre el `ELOOP` y esto lo sustituyan por otro symlink —lo
                // que se comprueba a continuación es el descriptor que salga,
                // no el nombre por el que se pidió—.
                let next = openat_dir(current.as_raw_fd(), &name)?;
                if !is_beneath(next.as_raw_fd(), root_id)? {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::EscapesRoot,
                    });
                }
                current = next;
            }
            Err(e) => return Err(map_errno(&e)),
        }
    }
    Ok(current)
}

/// ¿El destino CRUDO del symlink `name` empieza por `/`?
///
/// Se leen los bytes tal cual y no se resuelve nada (regla 1): lo único que se
/// pregunta es si es absoluto.
///
/// `Ok(None)` = `name` NO es un symlink (`EINVAL` de `readlinkat`), que es
/// también la forma de desempatar el `ENOTDIR` de un `openat(O_NOFOLLOW |
/// O_DIRECTORY)`: ese errno lo produce tanto un symlink como un fichero
/// corriente, y solo esto los separa.
#[allow(unsafe_code)]
fn link_target_is_absolute(dir: RawFd, name: &CString) -> Result<Option<bool>, Error> {
    let mut buf = [0i8; 2];
    // SAFETY: `dir` vive, `name` es NUL-terminada y viva, y `buf` tiene sitio
    // para los bytes que se piden. `readlinkat` NO termina en NUL: por eso solo
    // se mira lo que dice haber escrito.
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINVAL) {
            return Ok(None);
        }
        return Err(map_errno(&e));
    }
    Ok(Some(n > 0 && buf[0] == i8::try_from(b'/').unwrap_or(0)))
}

/// `openat` de un componente como directorio, SIN seguir un symlink final.
///
/// Devuelve el `io::Error` crudo y no la taxonomía: quien llama necesita
/// distinguir el `ELOOP` de «esto es un symlink» del resto, y `map_errno` los
/// funde a propósito en un solo veredicto.
#[allow(unsafe_code)]
fn openat_dir_nofollow(dir: RawFd, name: &CString) -> Result<OwnedFd, std::io::Error> {
    // SAFETY: `dir` está vivo y `name` es una CString NUL-terminada viva
    // durante toda la llamada. `O_PATH` no lee ni escribe.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` es un fd recién abierto y sin dueño.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `openat` de un componente como directorio SIGUIENDO el symlink, que es lo
/// que se quiere una vez sabido que lo es y que su destino es relativo. Lo que
/// salga se comprueba con [`is_beneath`] antes de usarse para nada.
#[allow(unsafe_code)]
fn openat_dir(dir: RawFd, name: &std::ffi::CStr) -> Result<OwnedFd, Error> {
    // SAFETY: `dir` está vivo y `name` es una CString NUL-terminada viva
    // durante toda la llamada. `O_PATH` no lee ni escribe.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` es un fd recién abierto y sin dueño.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// ¿Se llega desde `fd` hasta `root_id` subiendo por `..`?
///
/// La comprobación se hace sobre el DESCRIPTOR ya abierto, no sobre una ruta,
/// así que lo que se valida es exactamente el objeto que se va a usar después,
/// y no un nombre que pueda apuntar a otra cosa para cuando se abra.
///
/// **La subida en sí es best-effort bajo renombrados concurrentes**, y eso hay
/// que decirlo en vez de dejarlo suponer: si alguien mueve el directorio ya
/// abierto FUERA de la raíz entre este cálculo y el `mkdirat`/`openat` que
/// venga después, el veredicto se calculó sobre un árbol que ya no es. Hace
/// falta permiso de escritura DENTRO de la raíz para intentarlo, y `openat2`
/// —que es el camino de serie en Linux— no tiene esa ventana porque lo resuelve
/// el kernel de una pieza. `libpathrs` documenta la misma limitación para la
/// misma emulación.
fn is_beneath(fd: RawFd, root_id: (u64, u64)) -> Result<bool, Error> {
    if node_id_of(fd)? == root_id {
        return Ok(true);
    }
    let dotdot = c"..";
    let mut current = dup(fd)?;
    for _ in 0..MAX_CLIMB {
        let parent = openat_dir(current.as_raw_fd(), dotdot)?;
        let parent_id = node_id_of(parent.as_raw_fd())?;
        if parent_id == root_id {
            return Ok(true);
        }
        // La raíz del filesystem es su propio padre: se acabó el camino.
        if parent_id == node_id_of(current.as_raw_fd())? {
            return Ok(false);
        }
        current = parent;
    }
    Ok(false)
}

/// `(dev, ino)` de un fd abierto.
#[allow(unsafe_code)]
fn node_id_of(fd: RawFd) -> Result<(u64, u64), Error> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` está vivo y `st` es un `stat` propio y alineado que la
    // llamada rellena entero. Solo se lee tras comprobar el retorno.
    //
    // `fstat` sobre un fd `O_PATH` es una de las pocas operaciones que la
    // documentación de `open(2)` permite explícitamente sobre él.
    let rc = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if rc != 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `fstat` devolvió 0, así que dejó `st` inicializado.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::useless_conversion)] // `dev_t`/`ino_t` cambian por plataforma
    Ok((u64::from(st.st_dev), u64::try_from(st.st_ino).unwrap_or(0)))
}

/// Los segmentos unidos por `/`, que es lo único que `openat2` sabe recibir.
/// No es «componer una ruta»: la resolución sigue siendo relativa al fd de la
/// raíz y el kernel es quien impide salirse de ella.
#[cfg(target_os = "linux")]
fn join(parents: &[Segment]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, seg) in parents.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(seg.as_bytes());
    }
    out
}

/// ¿Dice este error que `openat2` no está disponible?
///
/// `ENOSYS` es el kernel <5.6. `EPERM` es lo que contesta un filtro seccomp que
/// no conoce el syscall (el perfil por defecto de Docker, durante años), y por
/// eso cuenta: es la MISMA situación —no hay `openat2`— dicha de otra forma.
/// Que un `EPERM` real de otra procedencia caiga aquí no abre nada: el paseo
/// confina igual y volverá a fallar con el mismo `EPERM` si lo era de verdad.
#[cfg(target_os = "linux")]
fn is_enosys(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOSYS) || e.raw_os_error() == Some(libc::EPERM)
}

/// Traduce el errno de una resolución confinada.
///
/// `EXDEV` es lo que contesta `openat2(RESOLVE_BENEATH)` cuando la resolución
/// se habría salido, y `ELOOP` lo que contesta un `O_NOFOLLOW`: los dos son el
/// mismo veredicto y NO son `NotFound`, porque un caller que ve `NotFound`
/// responde creando el padre — justo la operación que esto existe para impedir.
fn map_errno(e: &std::io::Error) -> Error {
    match e.raw_os_error() {
        Some(libc::EXDEV | libc::ELOOP) => Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        },
        _ => crate::provider::map_io(e),
    }
}

/// Costura de test: fuerza el paseo aunque el kernel tenga `openat2`, para que
/// el camino de emulación se ejercite en la misma máquina que el otro.
#[cfg(target_os = "linux")]
fn force_walk() -> bool {
    FORCE_WALK.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(target_os = "linux")]
static FORCE_WALK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Guard que fuerza el paseo mientras vive (costura de test).
#[cfg(target_os = "linux")]
#[doc(hidden)]
#[derive(Debug)]
pub struct ForceComponentWalk(());

#[cfg(target_os = "linux")]
impl ForceComponentWalk {
    pub(crate) fn new() -> Self {
        FORCE_WALK.store(true, std::sync::atomic::Ordering::Relaxed);
        Self(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ForceComponentWalk {
    fn drop(&mut self) {
        FORCE_WALK.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---------- operaciones bajo la raíz ----------

impl LocalRoot {
    /// `mkdir` de `rel` bajo la raíz.
    #[allow(unsafe_code)]
    pub(crate) fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        // SAFETY: `dir` vive mientras dura la llamada y `c` es una CString
        // NUL-terminada viva también. `0o777` lo recorta la umask del proceso,
        // igual que hace `std::fs::create_dir`.
        let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o777) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            return Err(map_errno(&e));
        }
        Ok(())
    }

    /// `symlink` de `rel` bajo la raíz, apuntando a `target`.
    ///
    /// Lo confinado es DÓNDE cae el link: el `symlinkat` va contra el
    /// descriptor del padre ya resuelto, así que un componente intermedio que
    /// sea un puente hacia fuera no puede llevárselo. A dónde APUNTA no se
    /// toca —se copian los bytes del origen tal cual, como hace
    /// `Provider::symlink`—: recortar el target sería inventarse un enlace
    /// distinto del que se está copiando.
    #[allow(unsafe_code)]
    pub(crate) fn symlink(&self, rel: &[Segment], target: &[u8]) -> Result<(), Error> {
        let (dir, name) = self.parent_of(rel)?;
        let name = cstring(name)?;
        // Un target con un NUL dentro no es una ruta que ningún unix sostenga.
        let target = CString::new(target.to_vec()).map_err(|_| Error::InvalidPath)?;
        // SAFETY: `dir` vive durante toda la llamada y las dos CStrings son
        // NUL-terminadas y vivas también.
        let rc = unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            return Err(map_errno(&e));
        }
        Ok(())
    }

    /// `lstat` de `rel` bajo la raíz: describe el LINK, jamás su destino
    /// (mismo contrato que `Provider::stat`).
    #[allow(unsafe_code)]
    pub(crate) fn stat(&self, rel: &[Segment], path: norte_proto::VPath) -> Result<Entry, Error> {
        let (dir, name) = self.parent_of(rel)?;
        let c = cstring(name)?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `dir` y `c` viven durante la llamada; `st` es un `stat`
        // propio y alineado que se rellena entero. Solo se lee tras el 0.
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                c.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(map_errno(&std::io::Error::last_os_error()));
        }
        // SAFETY: `fstatat` devolvió 0, así que dejó `st` inicializado.
        let st = unsafe { st.assume_init() };
        Ok(entry_from_stat(path, &st))
    }

    /// Abre un sink para `rel`: el staging se crea con `openat` en el
    /// directorio ya resuelto y se publica con `renameat` en ESE MISMO
    /// descriptor, así que la publicación va confinada igual que la escritura.
    /// Entre abrir y publicar nadie puede colar un symlink que mande el rename
    /// a otro sitio, porque no hay ninguna ruta que volver a resolver.
    pub(crate) fn open_write(&self, rel: &[Segment]) -> Result<ConfinedStaging, Error> {
        let (dir, name) = self.parent_of(rel)?;
        let final_name = cstring(name)?;
        // `Provider::write` promete un archivo NUEVO y lo dice AL ABRIR: sin
        // esto, un destino ocupado no se sabría hasta el `commit`, o sea
        // después de haber transferido el fichero entero para nada.
        //
        // No es LA garantía —esa es el `RENAME_NOREPLACE` de la publicación,
        // que no tiene ventana—, y por eso una carrera perdida aquí no pierde
        // nada: sale como conflicto un momento más tarde.
        //
        // Único matiz frente al camino por ruta: allí `collision_kind_for`
        // relee el directorio y distingue una colisión de CAJA o de
        // normalización; aquí se contesta `Exists` a secas. Nada del core
        // decide por esa distinción (solo se pinta), y releer el directorio
        // sería recorrer lo que este camino existe para no recorrer.
        if !name_is_free(dir.as_raw_fd(), &final_name)? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        let staging_name = ephemeral_staging_name(name.as_bytes());
        let file = create_exclusive(dir.as_raw_fd(), &staging_name)?;
        Ok(ConfinedStaging {
            dir,
            file,
            staging: staging_name,
            final_name,
        })
    }
}

/// Un staging abierto bajo una raíz confinada, con todo lo que su publicación
/// necesita: el descriptor del directorio, el nombre temporal y el definitivo.
#[derive(Debug)]
pub(crate) struct ConfinedStaging {
    pub(crate) dir: OwnedFd,
    pub(crate) file: std::fs::File,
    pub(crate) staging: CString,
    pub(crate) final_name: CString,
}

/// Publica el staging sobre su nombre definitivo, no-replace y en el mismo
/// descriptor de directorio.
#[allow(unsafe_code)]
pub(crate) fn publish(dir: RawFd, staging: &CString, final_name: &CString) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `dir` vive y las dos CStrings son NUL-terminadas y vivas.
        // `RENAME_NOREPLACE` hace que la colisión la detecte el PROPIO rename,
        // sin ventana entre comprobar y renombrar.
        let rc = unsafe {
            libc::renameat2(
                dir,
                staging.as_ptr(),
                dir,
                final_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        // `EINVAL`/`ENOSYS`/`EOPNOTSUPP`: filesystem sin no-replace (algunos
        // FUSE, NFS viejos). Se degrada al `renameat` llano precedido de un
        // `faccessat`, que es la ventana que M0 ya documenta — y sigue siendo
        // confinada, que es lo que este módulo garantiza.
        if !matches!(
            e.raw_os_error(),
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
        ) {
            return Err(rename_error(&e));
        }
    }
    plain_rename(dir, staging, final_name)
}

/// ¿Está LIBRE ese nombre en este directorio? Mira el NODO, no lo que apunte:
/// un symlink roto ocupa el nombre igual que un fichero.
///
/// `Ok(false)` = ocupado. `Ok(true)` = libre, y solo lo dice el `ENOENT`:
/// cualquier otro errno es «no lo sé» y sale como `Err`, porque quien pregunta
/// lo hace justo antes de un rename que PISA. La versión anterior usaba
/// `faccessat(F_OK, AT_SYMLINK_NOFOLLOW)` y leía cualquier fallo como «libre»,
/// que es fail-OPEN: ese flag necesita `faccessat2` (kernel 5.8+) y en un
/// kernel viejo, o en una libc que no lo use, contesta `EINVAL` para siempre —
/// con lo que el nombre parecía libre siempre y el rename degradado destruía lo
/// que hubiera. Lo encontró la revisión de seguridad de esta fase.
#[allow(unsafe_code)]
fn name_is_free(dir: RawFd, name: &CString) -> Result<bool, Error> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` vive, `name` es NUL-terminada y viva, y `st` es un `stat`
    // propio y alineado. Solo se leería tras un 0, y aquí ni se lee.
    let rc = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(false);
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        return Ok(true);
    }
    Err(map_errno(&e))
}

/// Publica sin poder pisar, allí donde `renameat2` no está.
///
/// **Primero se intenta `linkat` + `unlinkat`, que SÍ es no-replace atómico y
/// es portable.** `link` falla con `EEXIST` si el destino existe, y eso lo
/// decide el kernel sin ventana ninguna — que es la propiedad entera. Solo si
/// el filesystem no sabe enlazar (`EPERM`/`EOPNOTSUPP`/`EMLINK`, y `EXDEV` no
/// puede pasar porque los dos nombres cuelgan del MISMO descriptor) se cae al
/// `renameat` llano precedido de la comprobación, que es la ventana que M0 ya
/// documenta.
///
/// Importa más de lo que parece: en macOS no hay `renameat2`, así que este era
/// el ÚNICO camino de publicación de todo el módulo, y la revisión de seguridad
/// señaló que la comprobación previa dejaba a cada publicación de macOS con una
/// rendija en la que se pisa un fichero ajeno sin decir nada.
#[allow(unsafe_code)]
fn plain_rename(dir: RawFd, staging: &CString, final_name: &CString) -> Result<(), Error> {
    // SAFETY: `dir` vive y las dos CStrings son NUL-terminadas y vivas. Sin
    // `AT_SYMLINK_FOLLOW`: se enlaza el staging, jamás lo que apuntara.
    let rc = unsafe { libc::linkat(dir, staging.as_ptr(), dir, final_name.as_ptr(), 0) };
    if rc == 0 {
        // Publicado. El staging sobra: su borrado es best-effort porque el
        // fichero YA está en su sitio y fallar aquí sería fallar después de
        // haber tenido éxito.
        let _ = discard(dir, staging);
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    if !matches!(
        e.raw_os_error(),
        Some(libc::EPERM | libc::EOPNOTSUPP | libc::EMLINK)
    ) {
        return Err(rename_error(&e));
    }
    // Sin enlaces duros: queda el rename llano, y su comprobación previa.
    // `name_is_free` falla en duda, así que un errno raro NO se lee como
    // «libre» y no se pisa nada por no saber.
    if !name_is_free(dir, final_name)? {
        return Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        });
    }
    // SAFETY: `dir` vive y las dos CStrings son NUL-terminadas y vivas.
    let rc = unsafe { libc::renameat(dir, staging.as_ptr(), dir, final_name.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(rename_error(&std::io::Error::last_os_error()))
    }
}

/// Borra el staging. Un staging que ya no está no es un error.
#[allow(unsafe_code)]
pub(crate) fn discard(dir: RawFd, staging: &CString) -> Result<(), Error> {
    // SAFETY: `dir` vive y `staging` es NUL-terminada y viva.
    let rc = unsafe { libc::unlinkat(dir, staging.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(map_errno(&e))
}

/// Crea el staging en exclusiva: `O_EXCL` para que dos escrituras jamás
/// compartan uno, `O_NOFOLLOW` para que un symlink plantado con ese nombre no
/// redirija la creación.
#[allow(unsafe_code)]
fn create_exclusive(dir: RawFd, name: &CString) -> Result<std::fs::File, Error> {
    // SAFETY: `dir` vive y `name` es NUL-terminada y viva. El modo lo recorta
    // la umask, igual que en `std::fs::File::create`.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o666,
        )
    };
    if raw < 0 {
        return Err(map_errno(&std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` es un fd recién abierto y sin dueño; `File` pasa a serlo.
    Ok(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(raw) })
}

/// Nombre de staging efímero, con la MISMA forma que el del provider
/// (`.norte-partial.<16 hex>.<pid>-<seq>`) para que el barredor de parciales
/// lo siga reconociendo — esto es otra manera de llegar a los mismos ficheros,
/// no una segunda convención.
fn ephemeral_staging_name(final_name: &[u8]) -> CString {
    let name = crate::provider::ephemeral_partial_name(final_name);
    CString::new(name).expect("el nombre de staging es hex y puntos")
}

/// Un `Entry` desde un `stat` crudo, sin pasar por `std::fs::Metadata` (que
/// exigiría una ruta que aquí, deliberadamente, no existe).
fn entry_from_stat(path: norte_proto::VPath, st: &libc::stat) -> Entry {
    let (kind, size) = match st.st_mode & libc::S_IFMT {
        libc::S_IFLNK => (EntryKind::Symlink, None),
        libc::S_IFDIR => (EntryKind::Dir, None),
        libc::S_IFREG => (EntryKind::File, u64::try_from(st.st_size).ok()),
        _ => (EntryKind::Other, None),
    };
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path,
        kind,
        size,
        mtime_ms: mtime_ms_of(st),
    }
}

/// mtime en milisegundos UTC desde un `stat`.
fn mtime_ms_of(st: &libc::stat) -> Option<i64> {
    // Los tipos de `st_mtime`/`st_mtime_nsec` cambian con la plataforma y la
    // libc, así que la conversión es redundante SOLO en el objetivo de hoy.
    #[allow(clippy::useless_conversion)]
    let secs = i64::try_from(st.st_mtime).ok()?;
    #[allow(clippy::useless_conversion)]
    let nanos = i64::try_from(st.st_mtime_nsec).ok()?;
    secs.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

/// Un segmento como `CString`. Un segmento con un NUL dentro no es un nombre que
/// ningún filesystem unix pueda sostener.
fn cstring(seg: &Segment) -> Result<CString, Error> {
    CString::new(seg.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)
}

/// El error de un rename de publicación: un destino que ya existe es
/// `Conflict`, y el resto pasa por la taxonomía de siempre.
fn rename_error(e: &std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        return Error::Conflict {
            conflict: ConflictKind::Exists,
        };
    }
    map_errno(e)
}

// ---------- el handle que ve el core ----------

/// [`norte_vfs::ConfinedRoot`] sobre una [`LocalRoot`].
///
/// El `Arc` no es por compartir: el sink que devuelve `write` sobrevive al
/// handle si el caller lo suelta antes de hacer commit, y el descriptor del
/// directorio tiene que seguir vivo para que el `renameat` de la publicación
/// siga siendo el mismo directorio y no una ruta que se vuelve a resolver.
#[derive(Debug)]
pub(crate) struct LocalConfinedRoot {
    root: std::sync::Arc<LocalRoot>,
    /// La raíz como `VPath`, SOLO para poder nombrar lo que `stat` devuelve.
    /// Jamás se usa para resolver nada.
    vpath: norte_proto::VPath,
}

impl LocalConfinedRoot {
    pub(crate) fn new(root: LocalRoot, vpath: norte_proto::VPath) -> Self {
        Self {
            root: std::sync::Arc::new(root),
            vpath,
        }
    }

    /// El `VPath` de `rel` bajo la raíz. Es para PINTAR el resultado de un
    /// `stat`, no para abrir nada.
    fn vpath_of(&self, rel: &[Segment]) -> norte_proto::VPath {
        let mut p = self.vpath.clone();
        for seg in rel {
            p = p.join(seg.clone());
        }
        p
    }
}

#[async_trait::async_trait]
impl norte_vfs::ConfinedRoot for LocalConfinedRoot {
    async fn root_id(&self) -> Result<Option<norte_vfs::NodeId>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        crate::provider::blocking(move || {
            let (dev, ino) = node_id_of(root.fd.as_raw_fd())?;
            Ok(Some(norte_vfs::NodeId {
                volume: dev,
                index: u128::from(ino),
            }))
        })
        .await
    }

    async fn mkdir(&self, rel: &[Segment]) -> Result<(), Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.mkdir(&rel)).await
    }

    async fn write(&self, rel: &[Segment]) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        let staging = crate::provider::blocking(move || root.open_write(&rel)).await?;
        Ok(Box::new(ConfinedSink::new(staging)))
    }

    async fn symlink(
        &self,
        rel: &[Segment],
        target: &[u8],
        _kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        // `kind` es de Windows, y en Windows este módulo no existe.
        let root = std::sync::Arc::clone(&self.root);
        let rel = rel.to_vec();
        let target = target.to_vec();
        crate::provider::blocking(move || root.symlink(&rel, &target)).await
    }

    /// Esta raíz NO reanuda, y lo dice en vez de fingirlo.
    ///
    /// El default del trait contesta `(write, 0)`, que es seguro pero le
    /// promete al caller un sink cuyo `keep` conservaría algo continuable — y
    /// el de aquí no puede (ver [`ConfinedSink::keep`]). `Unsupported` es la
    /// respuesta exacta: el motor recopia entero, que es lo que iba a hacer de
    /// todas formas con `already = 0`.
    async fn open_resumable(
        &self,
        rel: &[Segment],
    ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        let _ = rel;
        Err(Error::Unsupported)
    }

    async fn stat(&self, rel: &[Segment]) -> Result<Entry, Error> {
        let root = std::sync::Arc::clone(&self.root);
        let path = self.vpath_of(rel);
        let rel = rel.to_vec();
        crate::provider::blocking(move || root.stat(&rel, path)).await
    }
}

/// El sink de una escritura confinada.
///
/// Sostiene el descriptor del directorio, no su ruta: entre `write` y `commit`
/// nadie puede sustituir un componente por un symlink y desviar la
/// publicación, porque no queda ninguna ruta que volver a resolver.
#[derive(Debug)]
struct ConfinedSink {
    dir: Option<OwnedFd>,
    file: Option<std::fs::File>,
    staging: CString,
    final_name: CString,
    /// `true` cuando commit/abort ya se ocuparon del staging (Drop no toca).
    done: bool,
}

impl ConfinedSink {
    fn new(s: ConfinedStaging) -> Self {
        Self {
            dir: Some(s.dir),
            file: Some(s.file),
            staging: s.staging,
            final_name: s.final_name,
            done: false,
        }
    }
}

#[async_trait::async_trait]
impl norte_vfs::ByteSink for ConfinedSink {
    async fn write(&mut self, chunk: bytes::Bytes) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let (file, res) = tokio::task::spawn_blocking(move || {
            let mut file = file;
            let res = crate::provider::write_maybe_sparse(&mut file, &chunk)
                .map_err(|e| crate::provider::map_io(&e));
            (file, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.file = Some(file);
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let dir = self.dir.take().ok_or(Error::Io { retryable: false })?;
        let staging = self.staging.clone();
        let final_name = self.final_name.clone();
        let res = crate::provider::blocking(move || {
            file.sync_all().map_err(|e| crate::provider::map_io(&e))?;
            drop(file);
            let out = publish(dir.as_raw_fd(), &staging, &final_name);
            if out.is_err() {
                // Un publish que no publica no deja el staging por ahí: es la
                // misma promesa del sink de siempre.
                let _ = discard(dir.as_raw_fd(), &staging);
            }
            out
        })
        .await;
        if !matches!(res, Err(Error::Internal { .. })) {
            self.done = true;
        }
        res
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        let dir = self.dir.take().ok_or(Error::Io { retryable: false })?;
        self.done = true;
        let staging = self.staging.clone();
        crate::provider::blocking(move || discard(dir.as_raw_fd(), &staging)).await
    }

    /// **Aquí `keep` BORRA, y no es una contradicción con el trait: es la
    /// única forma honesta de cumplirlo.**
    ///
    /// `keep` existe para conservar el staging y que un `open_resumable`
    /// posterior lo continúe (ADR 0012). El de esta raíz lleva un nombre
    /// EFÍMERO —pid y secuencia—, así que nadie puede volver a encontrarlo:
    /// conservarlo dejaría un `.norte-partial` por intento que ningún resume va
    /// a consumir y que el siguiente `Mirror` vería como huérfano y borraría.
    /// Sin resume que servir, conservar no conserva nada; solo ensucia.
    ///
    /// Hoy el core no llega aquí —`Dest::resumes()` es `false` para un destino
    /// confinado, así que la cancelación va por `abort`—, y precisamente por
    /// eso se cierra en el TIPO en vez de confiarlo a esa invariante, que vive
    /// en otro crate y la puede romper el siguiente que pase.
    async fn keep(self: Box<Self>) -> Result<(), Error> {
        self.abort().await
    }
}

impl Drop for ConfinedSink {
    fn drop(&mut self) {
        // Un sink soltado sin commit ni abort no deja staging detrás. Es
        // best-effort y síncrono a propósito: aquí ya no hay a quién devolverle
        // un error.
        if self.done {
            return;
        }
        self.file.take();
        if let Some(dir) = self.dir.take() {
            let _ = discard(dir.as_raw_fd(), &self.staging);
        }
    }
}
