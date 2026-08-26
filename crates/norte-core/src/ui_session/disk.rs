//! La sesión de UI en disco: un JSON por usuario, escrito entero y de una vez.
//!
//! El fichero es `<state_dir>/session.json`, al lado de `journal.db` y de
//! `logs/` —`state_dir()` YA termina en `norte`, así que aquí no se vuelve a
//! añadir—. Se escribe por temporal y `rename`, que es lo que hace que un
//! corte de luz a mitad de volcado deje la sesión ANTERIOR y no media.
//!
//! Cargar tiene cuatro desenlaces y ninguno es un panic: no hay fichero
//! (primer arranque), se cargó, está corrupto, o viene de una versión que este
//! binario no sabe leer. Los dos últimos NO se pisan a la ligera y NO se
//! cuentan citando el contenido: una sesión lleva las rutas por las que el
//! lector se mueve, y una ruta no va a un log por un error de parseo.

use std::path::{Path, PathBuf};

use norte_proto::methods::{SESSION_BODY_MAX, Session};

/// La versión de cuerpo más alta que este binario sabe entregarle a su
/// frontend.
///
/// Es el ÚNICO número del fichero que el core mira, y mirarlo no es leer el
/// cuerpo: lo compara, no lo interpreta. Un fichero con una versión mayor no
/// se carga y —esto es lo que importa— no se pisa: perder la sesión que
/// escribió un binario más nuevo no se recupera, y el precio de respetarla es
/// arrancar una vez desde la configuración.
///
/// Va del brazo de `norte_frontend::session::SCHEMA_VERSION`, que es quien le
/// da significado al cuerpo. Los dos números viven en crates distintos porque
/// el core NO puede depender del frontend; que no se separen lo comprueba un
/// test en `norte-tui`, el único crate que ve los dos
/// (`las_dos_versiones_de_esquema_van_del_brazo`).
pub const SCHEMA_VERSION: u32 = 2;

/// Lo más grande que se acepta LEER del disco.
///
/// `put` topa el cuerpo en [`SESSION_BODY_MAX`]; sin un tope simétrico al
/// cargar, un fichero de varios GB —que cualquier proceso del mismo uid puede
/// dejar mientras nadie tiene el lock— se leería entero a memoria en el
/// arranque. El margen es para el sobre (`version`, `revision` y las llaves).
const MAX_FILE_BYTES: u64 = SESSION_BODY_MAX as u64 + 4096;

/// Distingue el temporal de DOS volcados del mismo proceso, para que
/// `create_new` no se encuentre nunca el suyo propio.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Qué se encontró al cargar.
#[derive(Debug)]
pub enum LoadOutcome {
    /// No hay fichero. Primer arranque, y no es un fallo.
    Fresh,
    /// La sesión que había.
    Loaded(Session),
    /// Había fichero y no se pudo leer. `reason` es DIAGNÓSTICO —categoría y
    /// posición—, jamás el contenido.
    Corrupt {
        /// Qué falló, sin citar un solo byte del fichero.
        reason: String,
    },
    /// El fichero lo escribió un binario más nuevo.
    FromTheFuture {
        /// La versión que traía, para poder decirla en el aviso.
        version: u32,
    },
}

/// Dónde vive la sesión: `<state_dir>/session.json`.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("session.json")
}

/// Carga la sesión de `<state_dir>`.
///
/// Nunca falla: los cuatro desenlaces son [`LoadOutcome`], porque los tres que
/// no son «cargada» tienen la misma respuesta razonable —arrancar desde la
/// configuración— y solo se diferencian en lo que hay que decirle al humano.
#[must_use]
pub fn load(state_dir: &Path) -> LoadOutcome {
    let file = path(state_dir);
    // El tope ANTES de leer: el tamaño es del inodo, no del contenido, así que
    // decirlo no cita un solo byte del fichero.
    if let Ok(m) = std::fs::metadata(&file)
        && m.len() > MAX_FILE_BYTES
    {
        return LoadOutcome::Corrupt {
            reason: format!(
                "ocupa {} bytes y el tope de carga es {MAX_FILE_BYTES}",
                m.len()
            ),
        };
    }
    let raw = match std::fs::read(&file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::Fresh,
        // Un fichero que existe y no se deja leer NO es un primer arranque: si
        // se tratara como tal, el siguiente volcado lo pisaría.
        Err(e) => {
            return LoadOutcome::Corrupt {
                reason: format!("no se pudo leer: {}", e.kind()),
            };
        }
    };
    // Primero SOLO la versión: rehusar una futura no puede depender de que el
    // resto de su forma le encaje a este binario.
    match serde_json::from_slice::<VersionOnly>(&raw) {
        Ok(VersionOnly { version }) if version > SCHEMA_VERSION => {
            return LoadOutcome::FromTheFuture { version };
        }
        Ok(_) => {}
        Err(e) => {
            return LoadOutcome::Corrupt {
                reason: diagnose(&e),
            };
        }
    }
    match serde_json::from_slice::<Session>(&raw) {
        Ok(session) => LoadOutcome::Loaded(session),
        Err(e) => LoadOutcome::Corrupt {
            reason: diagnose(&e),
        },
    }
}

/// Solo la versión, para decidir si el fichero es del futuro antes de mirar
/// nada más.
#[derive(serde::Deserialize)]
struct VersionOnly {
    version: u32,
}

/// El error de serde, contado SIN su mensaje: `Display` de `serde_json` cita
/// el valor que no encajó («invalid type: string "…"»), y ese valor sale del
/// fichero de sesión, que lleva rutas. Categoría y posición bastan para
/// diagnosticar y no filtran nada.
fn diagnose(e: &serde_json::Error) -> String {
    let que = match e.classify() {
        serde_json::error::Category::Io => "i/o",
        serde_json::error::Category::Syntax => "JSON mal formado",
        serde_json::error::Category::Data => "forma inesperada",
        serde_json::error::Category::Eof => "se acaba antes de tiempo",
    };
    format!("{que} en línea {} columna {}", e.line(), e.column())
}

/// Escribe la sesión en `<state_dir>`, entera y de una vez.
///
/// Temporal + `rename`: un corte a mitad deja la sesión ANTERIOR intacta, que
/// es la única alternativa aceptable a la nueva. El `sync_all` va ANTES del
/// rename porque renombrar un fichero cuyos datos siguen en el page cache es
/// exactamente cómo se publica un fichero vacío.
///
/// # Errors
///
/// Cualquier fallo de I/O al crear el directorio, escribir el temporal o
/// renombrarlo. El llamante AVISA y sigue: no poder guardar la pantalla no es
/// razón para tumbar la sesión que la produjo.
pub fn write(state_dir: &Path, session: &Session) -> std::io::Result<()> {
    ensure_dir(state_dir)?;
    let file = path(state_dir);
    // El temporal es del MISMO directorio a propósito: `rename` solo es
    // atómico dentro de un sistema de ficheros. Y lleva el pid en el nombre
    // porque un nombre fijo es un fichero que puede estar YA ahí: `mode` solo
    // se aplica al CREAR, así que un `.tmp` heredado se publicaría con los
    // permisos —o el destino de enlace— que otro le dejó puestos.
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = file.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    // El fichero ES el documento del wire, sin sobre: la misma forma que
    // `session.get` devuelve. Un formato de fichero aparte sería una segunda
    // cosa que versionar para no ganar nada — lo único que hay que saber de
    // este fichero es qué versión de cuerpo lleva, y eso ya viaja dentro.
    let bytes = serde_json::to_vec(session).map_err(std::io::Error::other)?;
    // Un temporal a medias no se queda de recuerdo: el siguiente volcado con
    // este pid lo encontraría y `create_new` fallaría para siempre.
    if let Err(e) = write_private(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Y el `rename` también se sincroniza: los datos del temporal estaban en
    // disco, pero el NOMBRE nuevo vive en el directorio, y un corte de luz sin
    // esto deja el nombre viejo apuntando a lo de antes.
    sync_dir(state_dir);
    Ok(())
}

/// Sincroniza el directorio para que el `rename` sobreviva a un corte.
///
/// Silencioso a propósito: un `fsync` de directorio que no se puede hacer
/// —algunos sistemas de ficheros de red— no invalida un volcado que ya está
/// escrito y renombrado.
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Lo que se recuperó del disco, y si se puede escribir encima.
///
/// El segundo campo es la mitad que importa: «no se lee» y «no se pisa» son
/// decisiones distintas, y la primera sin la segunda es exactamente cómo un
/// binario viejo se come la sesión de uno nuevo —arranca vacío, el cliente
/// escribe contra la revisión 0, y el volcado siguiente publica ese vacío
/// encima del fichero que no supo leer—.
#[derive(Debug)]
pub struct Restored {
    /// La sesión recuperada, o una vacía si no había ninguna legible.
    pub session: Session,
    /// Si este proceso puede volcar sobre el fichero que había.
    pub writable: bool,
}

/// La sesión de `state_dir`, contando en voz alta lo que no sea «cargada».
///
/// Tres de los cuatro desenlaces arrancan desde la configuración y se
/// diferencian solo en lo que hay que decirle al humano, así que esa elección
/// no es del llamante: lo único suyo es dónde vive el estado. Lo comparten el
/// daemon y el frontend embebido, que si no dirían dos cosas distintas del
/// mismo fichero.
///
/// El CUARTO —un fichero del futuro— además prohíbe escribir, y eso sí tiene
/// que subir hasta quien decide si hay escritor: la promesa de ADR 0059 no es
/// «no se lee», es «no se pierde».
///
/// Los avisos son la mitad del valor de estos desenlaces: sin ellos, «la
/// pantalla salió en blanco» es indistinguible de «nunca se guardó».
///
/// Síncrono: los dos llamantes lo invocan dentro de un `spawn_blocking`
/// (regla 2).
#[must_use]
pub fn load_or_default(state_dir: &Path) -> Restored {
    match load(state_dir) {
        LoadOutcome::Loaded(session) => Restored {
            session,
            writable: true,
        },
        LoadOutcome::Fresh => Restored {
            session: Session::default(),
            writable: true,
        },
        // Un fichero ILEGIBLE sí se pisa, y es lo contrario de una excepción:
        // lo que llevaba dentro ya está perdido, y no volver a escribir nunca
        // dejaría al lector sin sesión para siempre por un corte de luz de
        // hace un mes.
        LoadOutcome::Corrupt { reason } => {
            tracing::warn!(%reason, "sesión de UI ilegible: se arranca desde la configuración");
            Restored {
                session: Session::default(),
                writable: true,
            }
        }
        LoadOutcome::FromTheFuture { version } => {
            tracing::warn!(
                version,
                conocida = SCHEMA_VERSION,
                "sesión de UI de una versión más nueva: no se lee y NO se escribe"
            );
            Restored {
                session: Session::default(),
                writable: false,
            }
        }
    }
}

/// El derecho a ESCRIBIR la sesión de este `state_dir`, mientras viva.
///
/// Lo suelta su `Drop`, y el SO lo suelta igual si el proceso muere de golpe:
/// un core que se cuelga no deja el fichero bloqueado para siempre.
#[derive(Debug)]
pub struct SessionLock {
    /// El descriptor bloqueado. El lock ES este handle: cerrarlo lo suelta.
    _file: std::fs::File,
}

/// Intenta tomar el derecho a escribir la sesión de `state_dir`.
///
/// `Ok(None)` = lo tiene otro proceso; quien no lo consigue corre SUELTO —
/// carga la pantalla, la usa, y no escribe—. Es `try_lock` y no `lock` a
/// propósito: esperar colgaría un arranque detrás de un core que está vivo y
/// no piensa soltarlo.
///
/// El lock va sobre un `session.json.lock` hermano y JAMÁS sobre el fichero
/// mismo, por lo que ya documenta `lock_config_file` en `norte-config`: el
/// escritor reemplaza el fichero por `rename`, así que un lock sobre él sería
/// un lock sobre un inodo que deja de ser el fichero en cuanto alguien
/// escribe.
///
/// # Errors
///
/// Fallos de I/O al crear el directorio o abrir el fichero de lock.
pub fn lock(state_dir: &Path) -> std::io::Result<Option<SessionLock>> {
    ensure_dir(state_dir)?;
    let mut name = path(state_dir).into_os_string();
    name.push(".lock");
    // SIN truncar (la misma razón que en `norte-config`): `CREATE_ALWAYS`
    // sobre un lockfile que otro proceso tiene tomado puede fallar en Windows
    // en vez de llegar al intento de lock, que es justo la contención que esto
    // existe para resolver.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    // Vacío y sin secretos, pero al lado de un fichero 0600: un lockfile
    // 0644 solo cuenta a quien mire que aquí hay una sesión.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let handle = opts.open(PathBuf::from(name))?;
    match handle.try_lock() {
        Ok(()) => Ok(Some(SessionLock { _file: handle })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Crea `<state_dir>` con permisos de solo-el-dueño, y ESTRECHA el que ya
/// estuviera abierto.
///
/// `DirBuilder::mode` solo se aplica al crear, así que un directorio de estado
/// heredado de una versión anterior —o creado por otro subsistema con otro
/// umask— se quedaría a 0755 con la sesión dentro. Es el mismo agujero que
/// `create_dir_locked` repara en `logging`.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if let Ok(md) = std::fs::metadata(dir)
            && md.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Escribe `bytes` en `file`, creándolo NUEVO a 0600 y sincronizándolo antes
/// de devolver: quien llama va a renombrarlo acto seguido.
///
/// `create_new` y no `create`: el modo de `open` solo manda cuando el fichero
/// se CREA, así que abrir uno que ya estaba es publicar los permisos —o el
/// enlace— que le dejó puestos quien lo dejó ahí. Con `O_NOFOLLOW` encima, un
/// symlink en el sitio del temporal es un error y no una escritura a donde
/// apunte.
fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // El modo va en el `open`, no en un `set_permissions` posterior: entre
    // crear a 0644 y ajustarlo hay una ventana en la que otro usuario del
    // sistema puede abrirlo, y lo que hay dentro son las rutas del lector.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
        // rustix y no libc, por la regla 5: la constante es la misma y viene
        // sin `unsafe` (ver el Cargo.toml de este crate).
        if let Ok(nofollow) = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()) {
            opts.custom_flags(nofollow);
        }
    }
    let mut f = opts.open(file)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sesion(v: u32, rev: u64) -> Session {
        Session {
            version: v,
            revision: rev,
            body: serde_json::json!({ "a": 1 }),
        }
    }

    /// Sin fichero no hay sesión, y eso NO es un fallo: es un primer arranque.
    #[test]
    fn sin_fichero_es_un_primer_arranque() {
        let d = tempfile::tempdir().expect("tmp");
        assert!(matches!(load(d.path()), LoadOutcome::Fresh));
    }

    /// Escribir y volver a leer devuelve la misma sesión, revisión incluida.
    #[test]
    fn round_trip_por_disco() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 9)).expect("escribe");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("cargada")
        };
        assert_eq!(s.revision, 9);
        assert_eq!(s.version, 1);
    }

    /// Un kind que ningún binario declara sobrevive al viaje por disco: el
    /// cuerpo es opaco también aquí.
    #[test]
    fn un_kind_desconocido_sobrevive_al_disco() {
        let d = tempfile::tempdir().expect("tmp");
        let body = serde_json::json!({ "layouts": { "default": {
            "kind": "kind-de-otro-binario", "params": { "x": [1, 2] } } } });
        write(
            d.path(),
            &Session {
                version: 1,
                revision: 1,
                body: body.clone(),
            },
        )
        .expect("escribe");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("cargada")
        };
        assert_eq!(s.body, body);
    }

    /// Un fichero corrupto NO es una pantalla en blanco: es un diagnóstico y
    /// un arranque desde la config. Y el diagnóstico no cita el CONTENIDO: un
    /// fichero de sesión lleva rutas, y una ruta no va a un log por un error
    /// de parseo.
    #[test]
    fn un_fichero_corrupto_es_diagnostico_no_pantalla_en_blanco() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(path(d.path()).parent().expect("padre")).expect("mkdir");
        std::fs::write(path(d.path()), b"{ esto no es json").expect("escribe");
        let LoadOutcome::Corrupt { reason } = load(d.path()) else {
            panic!("corrupta")
        };
        assert!(!reason.is_empty());
        assert!(!reason.contains("esto no es json"), "{reason}");
    }

    /// Una sesión de una versión FUTURA no se pisa. Perder una sesión nueva
    /// contra un binario viejo no se recupera.
    #[test]
    fn una_version_del_futuro_no_se_pisa() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(SCHEMA_VERSION + 1, 1)).expect("escribe");
        let LoadOutcome::FromTheFuture { version } = load(d.path()) else {
            panic!("del futuro")
        };
        assert_eq!(version, SCHEMA_VERSION + 1);
        // Y lo que hace que «no se pisa» sea verdad y no una frase: el
        // desenlace SUBE hasta quien decide si hay escritor.
        let r = load_or_default(d.path());
        assert!(!r.writable, "un fichero del futuro no se escribe");
        assert_eq!(r.session.revision, 0, "y tampoco se lee");
    }

    /// Un fichero ILEGIBLE sí se pisa: lo que llevaba ya está perdido, y no
    /// volver a escribir jamás dejaría al lector sin sesión para siempre.
    #[test]
    fn un_fichero_corrupto_si_se_pisa() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(path(d.path()), b"{ esto no es json").expect("escribe");
        assert!(load_or_default(d.path()).writable);
    }

    /// Un fichero enorme no se lee a memoria: se rehúsa por tamaño, y el
    /// diagnóstico dice bytes, que son del inodo y no del contenido.
    #[test]
    fn un_fichero_gigante_no_se_carga() {
        let d = tempfile::tempdir().expect("tmp");
        let gordo = vec![b'x'; usize::try_from(MAX_FILE_BYTES).expect("cabe") + 1];
        std::fs::write(path(d.path()), &gordo).expect("escribe");
        let LoadOutcome::Corrupt { reason } = load(d.path()) else {
            panic!("por tamaño")
        };
        assert!(reason.contains("bytes"), "{reason}");
    }

    /// El directorio de estado se ESTRECHA aunque ya existiera abierto: la
    /// sesión vive dentro y `mode` solo manda al crear.
    #[cfg(unix)]
    #[test]
    fn el_directorio_heredado_se_estrecha() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().expect("tmp");
        let dir = d.path().join("estado");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        write(&dir, &sesion(1, 1)).expect("escribe");
        let modo = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(modo & 0o077, 0, "modo {modo:o}");
    }

    /// Un `.tmp` que ya estaba —de otro usuario, o apuntando a otro sitio— no
    /// se reutiliza: el nombre lleva pid y secuencia, y el `open` es
    /// `create_new`.
    #[test]
    fn un_temporal_heredado_no_se_reutiliza() {
        let d = tempfile::tempdir().expect("tmp");
        let viejo = path(d.path()).with_extension("json.tmp");
        std::fs::write(&viejo, b"de otro").expect("escribe");
        write(d.path(), &sesion(1, 1)).expect("escribe igual");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("cargada")
        };
        assert_eq!(s.revision, 1);
        assert_eq!(std::fs::read(&viejo).expect("sigue"), b"de otro");
    }

    /// La escritura es atómica: no deja `.tmp` detrás.
    #[test]
    fn escribir_no_deja_temporales() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 1)).expect("escribe");
        write(d.path(), &sesion(1, 2)).expect("reescribe");
        let dir = path(d.path()).parent().expect("padre").to_owned();
        let restos: Vec<_> = std::fs::read_dir(&dir)
            .expect("lee")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp") || n.ends_with('~'))
            .collect();
        assert!(restos.is_empty(), "restos: {restos:?}");
    }

    /// Un segundo core sobre el mismo estado NO escribe: clona y corre suelto.
    /// Y al irse el primero, el siguiente sí lo toma — que es lo que hace que
    /// un relevo no deje al sucesor sin poder guardar nada.
    #[test]
    fn un_segundo_core_no_escribe_sobre_el_estado_ajeno() {
        let d = tempfile::tempdir().expect("tmp");
        let uno = lock(d.path()).expect("lock").expect("libre");
        assert!(
            lock(d.path()).expect("lock").is_none(),
            "el segundo no la toma"
        );
        drop(uno);
        assert!(
            lock(d.path()).expect("lock").is_some(),
            "al soltarla, el siguiente sí"
        );
    }

    /// El fichero no lo puede leer cualquiera: la sesión lleva las rutas por
    /// las que el lector se mueve.
    #[cfg(unix)]
    #[test]
    fn el_fichero_es_solo_del_dueno() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &sesion(1, 1)).expect("escribe");
        let modo = std::fs::metadata(path(d.path()))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(modo & 0o077, 0, "modo {modo:o}");
    }
}
