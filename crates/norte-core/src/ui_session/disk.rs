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

use norte_proto::methods::Session;

/// La versión de cuerpo más alta que este binario sabe entregarle a su
/// frontend.
///
/// Es el ÚNICO número del fichero que el core mira, y mirarlo no es leer el
/// cuerpo: lo compara, no lo interpreta. Un fichero con una versión mayor no
/// se carga y —esto es lo que importa— no se pisa: perder la sesión que
/// escribió un binario más nuevo no se recupera, y el precio de respetarla es
/// arrancar una vez desde la configuración.
pub const SCHEMA_VERSION: u32 = 1;

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
    // atómico dentro de un sistema de ficheros.
    let tmp = file.with_extension("json.tmp");
    // El fichero ES el documento del wire, sin sobre: la misma forma que
    // `session.get` devuelve. Un formato de fichero aparte sería una segunda
    // cosa que versionar para no ganar nada — lo único que hay que saber de
    // este fichero es qué versión de cuerpo lleva, y eso ya viaja dentro.
    let bytes = serde_json::to_vec(session).map_err(std::io::Error::other)?;
    write_private(&tmp, &bytes)?;
    std::fs::rename(&tmp, &file)
}

/// La sesión de `state_dir`, contando en voz alta lo que no sea «cargada».
///
/// Los tres desenlaces que no traen sesión tienen la MISMA respuesta —arrancar
/// desde la configuración— y solo se diferencian en lo que hay que decirle al
/// humano, así que la elección de qué hacer no es del llamante: lo único suyo
/// es dónde vive el estado. Lo comparten el daemon y el frontend embebido, que
/// si no dirían dos cosas distintas del mismo fichero.
///
/// Los avisos son la mitad del valor de estos desenlaces: sin ellos, «la
/// pantalla salió en blanco» es indistinguible de «nunca se guardó».
///
/// Síncrono: los dos llamantes lo invocan dentro de un `spawn_blocking`
/// (regla 2).
#[must_use]
pub fn load_or_default(state_dir: &Path) -> Session {
    match load(state_dir) {
        LoadOutcome::Loaded(s) => s,
        LoadOutcome::Fresh => Session::default(),
        LoadOutcome::Corrupt { reason } => {
            tracing::warn!(%reason, "sesión de UI ilegible: se arranca desde la configuración");
            Session::default()
        }
        LoadOutcome::FromTheFuture { version } => {
            tracing::warn!(
                version,
                conocida = SCHEMA_VERSION,
                "sesión de UI de una versión más nueva: no se lee y NO se pisa"
            );
            Session::default()
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
    let handle = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(name))?;
    match handle.try_lock() {
        Ok(()) => Ok(Some(SessionLock { _file: handle })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Crea `<state_dir>` con permisos de solo-el-dueño.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Escribe `bytes` en `file`, creándolo a 0600 y sincronizándolo antes de
/// devolver: quien llama va a renombrarlo acto seguido.
fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    // El modo va en el `open`, no en un `set_permissions` posterior: entre
    // crear a 0644 y ajustarlo hay una ventana en la que otro usuario del
    // sistema puede abrirlo, y lo que hay dentro son las rutas del lector.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
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
