//! [`ConfinedRoot`]: leer BAJO un directorio sin poder salirse de él, con
//! presupuesto.
//!
//! Existe para la capacidad `location` del plugin-host (ADR 0057): un guest
//! aprobado recibe un token opaco, no una ruta, y lee lo que hay debajo del
//! directorio que el panel está listando. Nada de esto puede vivir fuera de
//! este crate — la regla dura 2 dice que solo aquí se toca `std::fs`, y la
//! confinación es la del kernel ([`LocalRoot`], `openat2(RESOLVE_BENEATH)`,
//! el mismo mecanismo que cerró #164), no una comprobación de cadenas.
//!
//! # Qué garantiza, exactamente
//!
//! - **No se sale.** Un `..` INTERIOR es legítimo (`sub/../f`); uno que suba
//!   por encima de la raíz es [`LocationError::Escapes`], y también lo es una
//!   ruta absoluta — para `RESOLVE_BENEATH` una ruta absoluta ya empieza
//!   fuera.
//! - **El último componente no se sigue.** Se abre con `O_NOFOLLOW`: un
//!   symlink final es un `Symlink` que se puede `stat`, jamás un fichero que
//!   se lee sin saber a dónde apunta. Los componentes INTERMEDIOS los gobierna
//!   [`LocalRoot`] con el criterio de #164 (se siguen si no salen).
//! - **Se paga por llamada, y también cuando falla.** Si un error no gastara
//!   presupuesto, sondear el árbol fallando a propósito sería gratis.
//! - **No se entra en una raíz protegida que caiga DENTRO** (#238). Confinar
//!   acota por arriba y no dice nada de lo que hay debajo: con la raíz en
//!   `$XDG_CONFIG_HOME` —un directorio que un humano lista sin pensarlo— el
//!   guest leía `norte/secrets.age`, `norte/journal.db` y
//!   `norte/connections.toml`, y con la raíz en `/` leía el disco entero. El
//!   veto es por `(dev, ino)` de cada directorio del camino, no por comparar
//!   cadenas: un symlink que apunte a la raíz protegida da el mismo inodo.
//!
//! No hay `write` ni lo va a haber: la capacidad es de LECTURA.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Mutex;

use norte_proto::Segment;

use crate::confined::LocalRoot;

/// Topes de una sesión de ubicación. Todos fail-closed.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Tope de bytes de UNA lectura. Superarlo es [`LocationError::TooLarge`],
    /// nunca un fichero recortado en silencio.
    pub max_read_bytes: u64,
    /// Tope de llamadas de la sesión entera, fallidas incluidas.
    pub max_calls: u32,
    /// Tope de bytes ACUMULADOS que la sesión llega a entregar.
    pub max_total_bytes: u64,
    /// Tope de entradas que devuelve un `list`.
    pub max_list_entries: u32,
}

impl Default for Bounds {
    /// Lo que le basta a un lector de estado de git y poco más: el índice de
    /// un repositorio grande son unos pocos MB.
    fn default() -> Self {
        Self {
            max_read_bytes: 16 * 1024 * 1024,
            max_calls: 4_096,
            max_total_bytes: 64 * 1024 * 1024,
            max_list_entries: 4_096,
        }
    }
}

/// Qué es una entrada. Deliberadamente grueso: al guest le sobra con esto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationKind {
    /// Fichero regular.
    File,
    /// Directorio.
    Dir,
    /// Enlace simbólico (NO seguido).
    Symlink,
    /// Cualquier otra cosa (fifo, socket, dispositivo).
    Other,
}

/// Lo que `stat` devuelve: exactamente los campos que el índice de git guarda,
/// porque comparar solo `mtime` es cómo se pierde un cambio hecho dentro del
/// mismo segundo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationMeta {
    /// Qué clase de nodo es.
    pub kind: LocationKind,
    /// Tamaño en bytes.
    pub size: u64,
    /// mtime, segundos.
    pub mtime_sec: i64,
    /// mtime, nanosegundos.
    pub mtime_nsec: u32,
    /// ctime, segundos.
    pub ctime_sec: i64,
    /// ctime, nanosegundos.
    pub ctime_nsec: u32,
    /// Número de inodo.
    pub ino: u64,
    /// Dispositivo.
    pub dev: u64,
    /// Modo (permisos + tipo), tal cual lo da el sistema.
    pub mode: u32,
}

/// Una entrada de un `list`: nombre en BYTES crudos (regla dura 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationDirent {
    /// El nombre, sin decodificar.
    pub name: Vec<u8>,
    /// Qué es, si el `readdir` lo dijo; `Other` cuando no.
    pub kind: LocationKind,
}

/// Por qué una lectura confinada no se pudo servir.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocationError {
    /// La ruta relativa se sale de la raíz (o es absoluta, que para
    /// `RESOLVE_BENEATH` es lo mismo).
    #[error("path escapes the confined root")]
    Escapes,
    /// No existe.
    #[error("not found")]
    NotFound,
    /// Denegado: o el sistema operativo lo denegó, o el camino entra en una
    /// raíz protegida (#238).
    ///
    /// **Las dos cosas dan el MISMO error**, y a propósito: al guest no se le
    /// dice qué directorios existen y son intocables. Un error distinto sería
    /// un oráculo de dónde vive el journal.
    #[error("permission denied")]
    Denied,
    /// El fichero pasa de `max_read_bytes`. No se recorta: se dice.
    #[error("entry is larger than the read bound")]
    TooLarge,
    /// La sesión agotó su presupuesto de llamadas o de bytes.
    #[error("location budget exhausted")]
    Budget,
    /// El tipo de nodo no admite esta operación (leer un directorio, listar un
    /// fichero).
    #[error("wrong node type for this operation")]
    TypeMismatch,
    /// Cualquier otro fallo de I/O.
    #[error("i/o error")]
    Io,
}

/// Lo gastado por una sesión.
#[derive(Debug, Default)]
struct Spent {
    calls: u32,
    bytes: u64,
}

/// Lectura acotada BAJO un directorio.
#[derive(Debug)]
pub struct ConfinedRoot {
    root: LocalRoot,
    bounds: Bounds,
    spent: Mutex<Spent>,
    /// `(dev, ino)` de las raíces protegidas que caen dentro de esta raíz
    /// (#238). Se resuelven UNA vez, al abrir: son directorios de este proceso
    /// y no se mueven bajo nuestros pies mientras dura una llamada.
    forbidden: Vec<(u64, u64)>,
}

impl ConfinedRoot {
    /// Abre `dir` como raíz confinada.
    ///
    /// BLOQUEANTE: va dentro de `spawn_blocking` (regla dura 2).
    ///
    /// # Errors
    ///
    /// [`LocationError::NotFound`] o [`LocationError::Denied`] si el
    /// directorio no se puede abrir, y `Denied` también si `dir` **es** una de
    /// las raíces protegidas.
    ///
    /// `protected` son directorios que no se abren aunque caigan dentro de la
    /// raíz (#238): confinar acota por arriba y no dice absolutamente nada de
    /// lo que hay debajo, así que sin esto una raíz perfectamente inocente —el
    /// directorio de configuración del usuario, que se lista sin pensarlo—
    /// contenía el journal, los secretos y el fichero de conexiones. Una ruta
    /// protegida que no exista o que no esté dentro no cuesta nada: se ignora.
    ///
    /// ```
    /// # use norte_vfs_local::{Bounds, ConfinedRoot};
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("f"), b"hola").unwrap();
    /// std::fs::create_dir(dir.path().join("privado")).unwrap();
    /// std::fs::write(dir.path().join("privado/x"), b"secreto").unwrap();
    /// let vetado = dir.path().join("privado");
    /// let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[vetado]).unwrap();
    /// assert_eq!(root.read(b"f").unwrap(), b"hola");
    /// assert!(root.read(b"privado/x").is_err(), "la raíz protegida no se atraviesa");
    /// ```
    pub fn open(
        dir: &Path,
        bounds: Bounds,
        protected: &[std::path::PathBuf],
    ) -> Result<Self, LocationError> {
        Self::open_verified(dir, bounds, protected, None)
    }

    /// Como [`Self::open`], exigiendo que lo que se abra sea el nodo que el
    /// llamante YA miró (#241).
    ///
    /// `expect` es el `(dev, ino)` que el llamante observó cuando decidió que
    /// esta ruta era la raíz. Entre aquella mirada y este `open` hay una
    /// ventana: la ruta se resuelve otra vez desde `/`, siguiendo enlaces y
    /// sin confinar, así que renombrar un componente por medio cambiaba la
    /// raíz por la que quisiera quien pudo renombrarlo. Con el nodo esperado,
    /// una raíz que ha cambiado bajo los pies se rehúsa en vez de servirse.
    ///
    /// `None` es «no lo miré antes», que es lo que hace [`Self::open`].
    ///
    /// # Errors
    ///
    /// Lo que devuelva [`Self::open`], y [`LocationError::Denied`] si el nodo
    /// abierto no es el esperado.
    pub fn open_verified(
        dir: &Path,
        bounds: Bounds,
        protected: &[std::path::PathBuf],
        expect: Option<(u64, u64)>,
    ) -> Result<Self, LocationError> {
        let root = LocalRoot::open(dir).map_err(|e| from_proto(&e))?;
        if let Some(esperado) = expect {
            let abierto = crate::confined::node_id_of(root.raw_fd()).map_err(|e| from_proto(&e))?;
            if abierto != esperado {
                return Err(LocationError::Denied);
            }
        }
        // Se resuelven por `(dev, ino)` y no por prefijo de ruta: comparar
        // cadenas lo rodea un symlink, y la raíz que se abre aquí puede haber
        // llegado por uno.
        let forbidden: Vec<(u64, u64)> = protected
            .iter()
            .filter_map(|p| {
                let fd = LocalRoot::open(p).ok()?;
                crate::confined::node_id_of(fd.raw_fd()).ok()
            })
            .collect();
        let yo = crate::confined::node_id_of(root.raw_fd()).map_err(|e| from_proto(&e))?;
        if forbidden.contains(&yo) {
            // La raíz MISMA está protegida. El acuñador ya lo comprueba por
            // ruta, pero esa comprobación es de cadenas y ésta de inodos.
            return Err(LocationError::Denied);
        }
        Ok(Self {
            root,
            bounds,
            spent: Mutex::new(Spent::default()),
            forbidden,
        })
    }

    /// Rechaza un camino que ATRAVIESE o TERMINE en una raíz protegida (#238).
    ///
    /// Comprueba cada prefijo, no solo el destino: sin eso, `norte/sub/x`
    /// pasaría por encima de un veto sobre `norte`. Es una resolución por
    /// prefijo, o sea O(n²) en syscalls sobre la profundidad de la ruta — que
    /// aquí es dos o tres componentes, y la alternativa (pasear componente a
    /// componente por nuestra cuenta) es reimplementar lo que
    /// `openat2(RESOLVE_BENEATH)` hace bien.
    ///
    /// Sin raíces protegidas dentro no cuesta ni una syscall.
    fn ensure_allowed(&self, comps: &[Segment]) -> Result<(), LocationError> {
        if self.forbidden.is_empty() {
            return Ok(());
        }
        for hasta in 1..=comps.len() {
            let Ok(fd) = self.root.resolve_dir(&comps[..hasta]) else {
                // No resuelve como directorio: o no existe, o es un fichero.
                // En ninguno de los dos casos es una raíz protegida por la que
                // se pueda pasar, y el error de verdad lo dará el llamante.
                break;
            };
            let id = crate::confined::node_id_of(fd.as_raw_fd()).map_err(|e| from_proto(&e))?;
            if self.forbidden.contains(&id) {
                return Err(LocationError::Denied);
            }
        }
        Ok(())
    }

    /// Lee un fichero bajo la raíz, entero.
    ///
    /// # Errors
    ///
    /// Las de [`LocationError`]: `Escapes` si la ruta sale, `TooLarge` si pasa
    /// de `max_read_bytes`, `Budget` si la sesión se acabó.
    pub fn read(&self, rel: &[u8]) -> Result<Vec<u8>, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        let file = openat_read(dir.as_raw_fd(), name.as_ref())?;
        let meta = file.metadata().map_err(|e| from_io(&e))?;
        if !meta.is_file() {
            return Err(LocationError::TypeMismatch);
        }
        if meta.len() > self.bounds.max_read_bytes {
            return Err(LocationError::TooLarge);
        }
        self.charge_bytes(meta.len())?;
        // `take` y no `read_to_end` a pelo (#240): el tope y el cobro salían
        // los dos de `st_size`, y `st_size` puede mentir —un fichero al que
        // otro proceso le está añadiendo, o cualquier cosa en un FUSE que el
        // usuario controla—. Sin el `take`, ese fichero entraba entero en la
        // memoria del host y el presupuesto de la sesión contaba de menos.
        let tope = self.bounds.max_read_bytes;
        let mut buf = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        let leidos = std::io::Read::read_to_end(&mut std::io::Read::take(file, tope), &mut buf)
            .map_err(|e| from_io(&e))?;
        let leidos = u64::try_from(leidos).unwrap_or(u64::MAX);
        if leidos > tope {
            return Err(LocationError::TooLarge);
        }
        if leidos == tope && meta.len() < tope {
            // Creció mientras se leía hasta pasarse del tope. Recortar en
            // silencio sería entregar medio fichero como si fuera entero.
            return Err(LocationError::TooLarge);
        }
        // Lo que de verdad se entregó, si resultó ser más de lo que decía el
        // `stat`. El cobro no puede quedarse corto.
        self.charge_bytes(leidos.saturating_sub(meta.len()))?;
        Ok(buf)
    }

    /// Lee como mucho los primeros `max` bytes de un fichero bajo la raíz:
    /// lo que una cabecera necesita (`norte:location@0.2.0`, demo D2).
    ///
    /// Cobra SOLO lo que devuelve, y `max` se acota además por
    /// `max_read_bytes`: un guest no puede pedir «los primeros 4 GiB». Un
    /// fichero más corto que `max` llega entero, y eso no es un error —el
    /// guest que necesite saber si se cortó tiene `stat`.
    ///
    /// # Errors
    ///
    /// Las de [`LocationError`]: `Escapes` si la ruta sale, `TypeMismatch`
    /// si no es un fichero regular, `Budget` si la sesión se acabó.
    pub fn read_prefix(&self, rel: &[u8], max: u64) -> Result<Vec<u8>, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        let file = openat_read(dir.as_raw_fd(), name.as_ref())?;
        let meta = file.metadata().map_err(|e| from_io(&e))?;
        if !meta.is_file() {
            return Err(LocationError::TypeMismatch);
        }
        let tope = max.min(self.bounds.max_read_bytes);
        // Se cobra ANTES de leer, por lo que se va a leer como mucho: un
        // sondeo que falla no puede ser gratis (#240), y `st_size` puede
        // mentir, así que el tope y no el tamaño.
        self.charge_bytes(tope.min(meta.len()))?;
        let mut buf = Vec::with_capacity(usize::try_from(tope.min(meta.len())).unwrap_or(0));
        let leidos = std::io::Read::read_to_end(&mut std::io::Read::take(file, tope), &mut buf)
            .map_err(|e| from_io(&e))?;
        // Lo que de verdad se entregó por encima de lo cobrado (un fichero
        // que creció bajo el `stat`): el cobro no puede quedarse corto.
        let leidos = u64::try_from(leidos).unwrap_or(u64::MAX);
        self.charge_bytes(leidos.saturating_sub(tope.min(meta.len())))?;
        Ok(buf)
    }

    /// `lstat` de una entrada bajo la raíz: el símbolo NO se sigue.
    ///
    /// # Errors
    ///
    /// Las de [`LocationError`].
    ///
    /// # Panics
    ///
    /// Nunca: el `CString` de `"."` es una constante sin NUL interior.
    pub fn stat(&self, rel: &[u8]) -> Result<LocationMeta, LocationError> {
        self.charge_call()?;
        let (parent, name) = Self::split(rel)?;
        self.ensure_allowed(&Self::components(rel)?)?;
        let dir = self.dir_fd(&parent)?;
        match name {
            Some(name) => fstatat_nofollow(dir.as_raw_fd(), &name),
            // La raíz misma: `fstatat` con nombre vacío y `AT_EMPTY_PATH`
            // sería otra syscall más; el `.` de un dirfd ya es ella.
            None => fstatat_nofollow(dir.as_raw_fd(), &CString::new(".").expect("`.` sin NUL")),
        }
    }

    /// Lista un directorio bajo la raíz. Nombres en bytes crudos, `.` y `..`
    /// excluidos.
    ///
    /// # Errors
    ///
    /// Las de [`LocationError`]; `TypeMismatch` si `rel` no es un directorio.
    pub fn list(&self, rel: &[u8]) -> Result<Vec<LocationDirent>, LocationError> {
        self.charge_call()?;
        let comps = Self::components(rel)?;
        self.ensure_allowed(&comps)?;
        let dir = self.dir_fd(&comps)?;
        readdir_all(dir.as_raw_fd(), self.bounds.max_list_entries)
    }

    /// Cobra una llamada. Se cobra ANTES de trabajar y también cuando el
    /// trabajo va a fallar: un sondeo que falla a propósito no puede ser
    /// gratis.
    fn charge_call(&self) -> Result<(), LocationError> {
        let mut spent = self.spent.lock().expect("spent lock sano");
        if spent.calls >= self.bounds.max_calls {
            return Err(LocationError::Budget);
        }
        spent.calls += 1;
        Ok(())
    }

    fn charge_bytes(&self, n: u64) -> Result<(), LocationError> {
        let mut spent = self.spent.lock().expect("spent lock sano");
        let total = spent.bytes.saturating_add(n);
        if total > self.bounds.max_total_bytes {
            return Err(LocationError::Budget);
        }
        spent.bytes = total;
        Ok(())
    }

    /// Trocea una ruta relativa en segmentos, resolviendo `.` y `..` de forma
    /// LÉXICA.
    ///
    /// Léxica y no por el kernel a propósito: los dos veredictos coinciden en
    /// lo único que se promete —no salir—, porque cada segmento resultante se
    /// abre igualmente bajo [`LocalRoot`]. Lo que la versión léxica evita es
    /// tener que mandarle al kernel una ruta que este módulo no ha mirado.
    fn components(rel: &[u8]) -> Result<Vec<Segment>, LocationError> {
        if rel.first() == Some(&b'/') {
            return Err(LocationError::Escapes);
        }
        let mut out: Vec<Segment> = Vec::new();
        for comp in rel.split(|b| *b == b'/') {
            match comp {
                b"" | b"." => {}
                b".." => {
                    if out.pop().is_none() {
                        return Err(LocationError::Escapes);
                    }
                }
                other => out.push(Segment::new(other.to_vec()).map_err(|_| LocationError::Io)?),
            }
        }
        Ok(out)
    }

    /// Los componentes del PADRE y el último nombre ya en `CString`. `None` =
    /// la ruta es la raíz misma.
    fn split(rel: &[u8]) -> Result<(Vec<Segment>, Option<CString>), LocationError> {
        let mut comps = Self::components(rel)?;
        let Some(last) = comps.pop() else {
            return Ok((comps, None));
        };
        let name = CString::new(last.as_bytes().to_vec()).map_err(|_| LocationError::Io)?;
        Ok((comps, Some(name)))
    }

    fn dir_fd(&self, comps: &[Segment]) -> Result<OwnedFd, LocationError> {
        self.root.resolve_dir(comps).map_err(|e| from_proto(&e))
    }
}

/// Abre un hijo para lectura SIN seguir un symlink final.
///
/// `O_NONBLOCK` y `O_NOCTTY` no son adorno (#240): la comprobación de tipo es
/// POSTERIOR al open, y un `open(O_RDONLY)` sobre una FIFO sin escritor se
/// queda bloqueado para siempre. Un tarball hostil puede traer una FIFO
/// llamada `.git/index` —tar las lleva y norte las extrae—, y cada pintado de
/// esa página se comía un hilo del pool bloqueante; la interrupción por época
/// de wasmtime no salva de eso, porque solo dispara en instrucciones wasm.
/// Sobre un fichero regular, `O_NONBLOCK` no cambia nada de la lectura.
fn openat_read(dir: RawFd, name: Option<&CString>) -> Result<std::fs::File, LocationError> {
    let Some(name) = name else {
        // Leer la raíz es leer un directorio.
        return Err(LocationError::TypeMismatch);
    };
    #[allow(unsafe_code)]
    // SAFETY: `dir` está vivo (lo sostiene el `OwnedFd` del llamante) y `name`
    // es una CString NUL-terminada viva durante toda la llamada. El fd que
    // devuelve el kernel no tiene dueño hasta el `from_raw_fd` de abajo.
    let raw = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY,
        )
    };
    if raw < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    #[allow(unsafe_code)]
    // SAFETY: `raw` es un fd recién abierto y sin dueño; `File` pasa a serlo.
    Ok(unsafe { std::fs::File::from_raw_fd(raw) })
}

/// `fstatat` sin seguir el symlink final.
fn fstatat_nofollow(dir: RawFd, name: &CString) -> Result<LocationMeta, LocationError> {
    let mut st: libc::stat = unsafe_zeroed_stat();
    #[allow(unsafe_code)]
    // SAFETY: `dir` vive, `name` es NUL-terminada y viva, y `st` es una `stat`
    // propia y alineada que el kernel rellena entera.
    let rc = unsafe { libc::fstatat(dir, name.as_ptr(), &raw mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if rc < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    Ok(meta_from_stat(&st))
}

/// Una `libc::stat` a cero, que es lo que el kernel espera recibir.
#[allow(unsafe_code)]
fn unsafe_zeroed_stat() -> libc::stat {
    // SAFETY: `libc::stat` es un POD de enteros: el patrón todo-ceros es un
    // valor válido, y el kernel lo sobreescribe entero antes de que se lea.
    unsafe { std::mem::zeroed() }
}

// `useless_conversion` es cierto SOLO en este target: los tipos de
// `libc::stat` cambian de anchura entre arquitecturas (`time_t` de 32 bits
// sigue existiendo), y un `as` que trunca un inodo convierte dos ficheros
// distintos en el mismo. La conversión se queda.
#[allow(clippy::useless_conversion)]
fn meta_from_stat(st: &libc::stat) -> LocationMeta {
    let mode = st.st_mode;
    let kind = match mode & libc::S_IFMT {
        libc::S_IFREG => LocationKind::File,
        libc::S_IFDIR => LocationKind::Dir,
        libc::S_IFLNK => LocationKind::Symlink,
        _ => LocationKind::Other,
    };
    LocationMeta {
        kind,
        size: u64::try_from(st.st_size).unwrap_or(0),
        mtime_sec: i64::try_from(st.st_mtime).unwrap_or(0),
        mtime_nsec: u32::try_from(st.st_mtime_nsec).unwrap_or(0),
        ctime_sec: i64::try_from(st.st_ctime).unwrap_or(0),
        ctime_nsec: u32::try_from(st.st_ctime_nsec).unwrap_or(0),
        ino: u64::try_from(st.st_ino).unwrap_or(0),
        dev: u64::try_from(st.st_dev).unwrap_or(0),
        mode: u32::try_from(mode).unwrap_or(0),
    }
}

/// `readdir` sobre un dirfd, acotado.
fn readdir_all(dir: RawFd, max: u32) -> Result<Vec<LocationDirent>, LocationError> {
    // `fdopendir` se queda con el fd (lo cierra `closedir`), así que se le da
    // un duplicado ABIERTO PARA LEER: el de `LocalRoot` es `O_PATH`, que no
    // sirve para recorrer.
    #[allow(unsafe_code)]
    // SAFETY: `dir` vive durante la llamada; `"."` es una constante
    // NUL-terminada. El fd devuelto no tiene dueño hasta el `fdopendir`.
    let raw = unsafe {
        libc::openat(
            dir,
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    #[allow(unsafe_code)]
    // SAFETY: `raw` es un fd de directorio recién abierto y sin dueño;
    // `fdopendir` pasa a ser su dueño y `closedir` lo cierra al final.
    let dirp = unsafe { libc::fdopendir(raw) };
    if dirp.is_null() {
        let err = std::io::Error::last_os_error();
        #[allow(unsafe_code)]
        // SAFETY: `fdopendir` falló, así que el fd sigue siendo nuestro.
        unsafe {
            libc::close(raw)
        };
        return Err(from_io(&err));
    }
    let mut out = Vec::new();
    loop {
        // POSIX: `readdir` devuelve NULL al acabar Y al fallar, y la única
        // forma de distinguirlas es poner errno a 0 antes. Sin esto, un errno
        // viejo de cualquier llamada anterior se leería como un directorio
        // roto — o al revés, un fallo real pasaría por fin de directorio.
        clear_errno();
        #[allow(unsafe_code)]
        // SAFETY: `dirp` es un DIR* vivo, propiedad de esta función hasta el
        // `closedir` de abajo.
        let entry = unsafe { libc::readdir(dirp) };
        if entry.is_null() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                #[allow(unsafe_code)]
                // SAFETY: `dirp` sigue vivo y esta es su única liberación en
                // este camino.
                unsafe {
                    libc::closedir(dirp)
                };
                return Err(from_io(&err));
            }
            break;
        }
        #[allow(unsafe_code)]
        // SAFETY: `entry` es un puntero válido a una `dirent` propiedad del
        // DIR*, viva hasta el siguiente `readdir`, y solo se lee aquí.
        let (name, d_type) = unsafe {
            let name = std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                .to_bytes()
                .to_vec();
            (name, (*entry).d_type)
        };
        if name == b"." || name == b".." {
            continue;
        }
        out.push(LocationDirent {
            name,
            kind: kind_from_d_type(d_type),
        });
        if u32::try_from(out.len()).unwrap_or(u32::MAX) >= max {
            break;
        }
    }
    #[allow(unsafe_code)]
    // SAFETY: `dirp` sigue vivo y esta es su única liberación.
    unsafe {
        libc::closedir(dirp)
    };
    Ok(out)
}

/// Pone `errno` a cero. Lo exige POSIX antes de un `readdir` cuyo NULL haya
/// que interpretar.
#[allow(unsafe_code)]
fn clear_errno() {
    // SAFETY: `errno` es thread-local y el puntero que devuelven estas dos
    // funciones apunta a él; escribirle un 0 es la forma que define POSIX.
    unsafe {
        #[cfg(target_os = "linux")]
        {
            *libc::__errno_location() = 0;
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            *libc::__error() = 0;
        }
    }
}

fn kind_from_d_type(d_type: u8) -> LocationKind {
    match d_type {
        libc::DT_REG => LocationKind::File,
        libc::DT_DIR => LocationKind::Dir,
        libc::DT_LNK => LocationKind::Symlink,
        _ => LocationKind::Other,
    }
}

fn from_io(e: &std::io::Error) -> LocationError {
    match e.raw_os_error() {
        Some(libc::ENOENT) => LocationError::NotFound,
        Some(libc::EACCES | libc::EPERM) => LocationError::Denied,
        // `ELOOP` = symlink que no se sigue; `EXDEV` = lo que devuelve
        // `openat2(RESOLVE_BENEATH)` cuando la resolución se sale.
        Some(libc::ELOOP | libc::EXDEV) => LocationError::Escapes,
        Some(libc::EISDIR | libc::ENOTDIR) => LocationError::TypeMismatch,
        _ => LocationError::Io,
    }
}

fn from_proto(e: &norte_proto::Error) -> LocationError {
    use norte_proto::{ConflictKind, Error};
    match e {
        Error::NotFound => LocationError::NotFound,
        Error::PermissionDenied => LocationError::Denied,
        Error::Conflict {
            conflict: ConflictKind::EscapesRoot,
        } => LocationError::Escapes,
        Error::Conflict { .. } => LocationError::TypeMismatch,
        _ => LocationError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #240: una FIFO no cuelga al lector.
    ///
    /// La comprobación de tipo es posterior al open, así que sin `O_NONBLOCK`
    /// un `open(O_RDONLY)` sobre una FIFO sin escritor se queda ahí para
    /// siempre — un hilo del pool bloqueante por página pintada, y la
    /// interrupción por época de wasmtime no llega a enterarse.
    #[test]
    fn una_fifo_no_bloquea_al_abrirla() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("index");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes().to_vec()).unwrap();
        #[allow(unsafe_code)]
        // SAFETY: `c` vive durante toda la llamada y es una ruta NUL-terminada
        // dentro de un tempdir recién creado.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        // Sin `O_NONBLOCK` este assert no falla: no termina.
        assert_eq!(root.read(b"index"), Err(LocationError::TypeMismatch));
    }

    /// `read_prefix` (norte:location 0.2.0): entrega y COBRA como mucho `max`
    /// bytes. Una columna sobre cien vídeos cuesta cien cabeceras y no cien
    /// vídeos, y el presupuesto de la sesión lo refleja.
    #[test]
    fn read_prefix_entrega_y_cobra_solo_la_cabecera() {
        let dir = tempfile::tempdir().unwrap();
        let contenido: Vec<u8> = (0..100u8).collect();
        std::fs::write(dir.path().join("pista.mp3"), &contenido).unwrap();
        std::fs::create_dir(dir.path().join("carpeta")).unwrap();

        let bounds = Bounds {
            max_total_bytes: 60,
            ..Bounds::default()
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(
            root.read_prefix(b"pista.mp3", 50).unwrap(),
            contenido[..50],
            "los primeros 50"
        );
        // Un segundo prefijo de 50 no cabe en los 60 de la sesión: se cobró
        // lo entregado, no lo que el fichero mide.
        assert_eq!(
            root.read_prefix(b"pista.mp3", 50),
            Err(LocationError::Budget)
        );

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(
            root.read_prefix(b"pista.mp3", 1000).unwrap(),
            contenido,
            "más corto que `max`: entero, sin error"
        );
        assert_eq!(
            root.read_prefix(b"carpeta", 10),
            Err(LocationError::TypeMismatch)
        );
        assert_eq!(
            root.read_prefix(b"../fuera", 10),
            Err(LocationError::Escapes)
        );
    }

    /// #238: la raíz protegida que cae DENTRO no se atraviesa, ni a un nivel
    /// ni a tres.
    ///
    /// Es el caso real y no hace falta nada hostil para llegar a él: con el
    /// panel en `$XDG_CONFIG_HOME` la raíz confinada era ese directorio, y
    /// `norte/` —el journal, los secretos, las conexiones— estaba debajo.
    #[test]
    fn una_raiz_protegida_de_dentro_no_se_atraviesa() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("norte/hondo")).unwrap();
        std::fs::write(dir.path().join("norte/secretos.age"), b"nope").unwrap();
        std::fs::write(dir.path().join("norte/hondo/x"), b"tampoco").unwrap();
        std::fs::write(dir.path().join("libre.txt"), b"si").unwrap();
        let vetada = dir.path().join("norte");

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[vetada]).unwrap();
        assert_eq!(root.read(b"libre.txt").unwrap(), b"si", "lo demás se lee");
        assert_eq!(
            root.read(b"norte/secretos.age"),
            Err(LocationError::Denied),
            "un nivel"
        );
        assert_eq!(
            root.read(b"norte/hondo/x"),
            Err(LocationError::Denied),
            "y tres: se comprueba CADA prefijo, no solo el destino"
        );
        assert_eq!(root.list(b"norte"), Err(LocationError::Denied));
        assert_eq!(root.stat(b"norte"), Err(LocationError::Denied));
        assert_eq!(
            root.stat(b"norte/secretos.age"),
            Err(LocationError::Denied),
            "ni se confirma que exista"
        );
    }

    /// Y el veto es por INODO, así que un symlink que apunte a la raíz
    /// protegida da lo mismo: comparar cadenas es lo que un symlink rodea.
    #[test]
    fn un_symlink_a_la_raiz_protegida_tampoco_entra() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("norte")).unwrap();
        std::fs::write(dir.path().join("norte/secretos.age"), b"nope").unwrap();
        std::os::unix::fs::symlink("norte", dir.path().join("atajo")).unwrap();
        let vetada = dir.path().join("norte");

        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[vetada]).unwrap();
        assert_eq!(
            root.read(b"atajo/secretos.age"),
            Err(LocationError::Denied),
            "el atajo resuelve al mismo inodo"
        );
    }

    /// La raíz MISMA protegida no se abre. El acuñador ya lo comprueba por
    /// ruta; esto lo comprueba por inodo, que es lo que un symlink no engaña.
    #[test]
    fn la_raiz_protegida_no_se_abre_como_raiz() {
        let dir = tempfile::tempdir().unwrap();
        let ella = dir.path().to_path_buf();
        assert_eq!(
            ConfinedRoot::open(dir.path(), Bounds::default(), &[ella]).err(),
            Some(LocationError::Denied)
        );
    }

    /// Una raíz protegida que no existe o que está FUERA no cuesta nada y no
    /// veta nada: el proceso declara las suyas una vez y muchas no aplican.
    #[test]
    fn una_protegida_ausente_o_de_fuera_no_estorba() {
        let fuera = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"si").unwrap();
        let root = ConfinedRoot::open(
            dir.path(),
            Bounds::default(),
            &[fuera.path().to_path_buf(), dir.path().join("no-existe")],
        )
        .unwrap();
        assert_eq!(root.read(b"f").unwrap(), b"si");
    }

    #[test]
    fn un_dotdot_no_sale_de_la_raiz() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dentro.txt"), b"si").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(
            root.read(b"sub/../dentro.txt").is_ok(),
            "un `..` INTERIOR es legítimo"
        );
        assert_eq!(root.read(b"../fuera.txt"), Err(LocationError::Escapes));
    }

    #[test]
    fn un_symlink_que_apunta_fuera_no_se_sigue() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secreto"), b"nope").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secreto"), dir.path().join("escape"))
            .unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(
            root.read(b"escape").is_err(),
            "un symlink no es una puerta trasera"
        );
        // Verlo SÍ se puede: es una entrada del directorio como otra.
        assert_eq!(root.stat(b"escape").unwrap().kind, LocationKind::Symlink);
    }

    #[test]
    fn un_symlink_intermedio_que_sale_lo_rechaza_la_raiz() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("d")).unwrap();
        std::fs::write(outside.path().join("d/secreto"), b"nope").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("d"), dir.path().join("puerta")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert!(root.read(b"puerta/secreto").is_err());
    }

    #[test]
    fn una_ruta_absoluta_no_es_relativa() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"/etc/passwd"), Err(LocationError::Escapes));
    }

    #[test]
    fn cada_tope_corta_en_su_borde() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("grande.bin"), vec![0u8; 4096]).unwrap();
        let bounds = Bounds {
            max_read_bytes: 1024,
            max_calls: 2,
            max_total_bytes: 2048,
            max_list_entries: 8,
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(root.read(b"grande.bin"), Err(LocationError::TooLarge));
        // El presupuesto de LLAMADAS se consume aunque la lectura falle: si
        // no, un guest sondea el árbol gratis fallando a propósito.
        root.stat(b"grande.bin").ok();
        assert_eq!(root.stat(b"grande.bin"), Err(LocationError::Budget));
    }

    #[test]
    fn el_presupuesto_de_bytes_corta_la_sesion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![b'x'; 100]).unwrap();
        std::fs::write(dir.path().join("b"), vec![b'y'; 100]).unwrap();
        let bounds = Bounds {
            max_total_bytes: 150,
            ..Bounds::default()
        };
        let root = ConfinedRoot::open(dir.path(), bounds, &[]).unwrap();
        assert_eq!(root.read(b"a").unwrap().len(), 100);
        assert_eq!(root.read(b"b"), Err(LocationError::Budget));
    }

    #[test]
    fn stat_trae_lo_que_git_guarda_en_su_indice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"x").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        let m = root.stat(b"f").unwrap();
        assert_eq!(m.size, 1);
        assert_eq!(m.kind, LocationKind::File);
        assert!(
            m.ino != 0 && m.dev != 0,
            "git compara ino/dev, no solo mtime"
        );
        assert!(m.mtime_sec > 0, "y mtime con nanosegundos");
    }

    #[test]
    fn list_da_los_nombres_en_bytes_crudos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        {
            use std::os::unix::ffi::OsStrExt as _;
            let raro = std::ffi::OsStr::from_bytes(b"no\xffutf8");
            std::fs::write(dir.path().join(raro), b"").unwrap();
        }
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        let mut names: Vec<Vec<u8>> = root
            .list(b"")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![b"a.txt".to_vec(), b"no\xffutf8".to_vec(), b"sub".to_vec()]
        );
        let sub = root.list(b"sub").unwrap();
        assert!(sub.is_empty(), "un directorio vacío lista vacío, no falla");
    }

    #[test]
    fn listar_un_fichero_es_type_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), b"").unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.list(b"f"), Err(LocationError::TypeMismatch));
    }

    #[test]
    fn leer_un_directorio_es_type_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let root = ConfinedRoot::open(dir.path(), Bounds::default(), &[]).unwrap();
        assert_eq!(root.read(b"d"), Err(LocationError::TypeMismatch));
    }
}
