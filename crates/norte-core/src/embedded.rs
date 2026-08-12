//! El journal del proceso EMBEBIDO (#167).
//!
//! La regla dura 4 —«toda mutación pasa por el journal»— se afirmaba en
//! `CLAUDE.md` y se cumplía en un solo método: `sync.apply` exige journal y
//! rehúsa sin él, mientras copiar, mover, borrar, renombrar y enterrar por el
//! transporte embebido no registraban nada. Este módulo es la otra mitad: el
//! TUI y el CLI sin daemon abren EL journal del directorio de estado, el mismo
//! que abriría el daemon.
//!
//! # Un solo escritor, y quién se lo queda
//!
//! La cadena de hashes del journal asume un dueño único (spec §4, ADR 0024), y
//! quien lo impone no es una convención: [`crate::journal::Journal::open`] abre
//! `SQLite` con `locking_mode=EXCLUSIVE`. Un segundo proceso —otro TUI embebido, o
//! el daemon corriendo— pierde la carrera al abrir con `database is locked`.
//!
//! Ese caso NO impide arrancar. Un `norte cp` que deja de funcionar porque hay
//! un daemon vivo sería peor que la falta de registro que arregla esto: se
//! sigue sin journal, se avisa, y el TUI ya atenúa lo que exige journal
//! (`pane.sync-dirs`) con su razón visible.
//!
//! # Lo que este mecanismo NO cubre
//!
//! - **El dueño lo es mientras vive.** Un TUI embebido se queda el lock toda la
//!   sesión, así que un `norte daemon run` posterior no arranca y un `norte
//!   audit` no puede leer. Abrirlo PEREZOSAMENTE —en la primera mutación— es lo
//!   que quitaría casi todo ese coste, y es trabajo aparte (#177).
//! - **`Unavailable` sigue adelante igual que `Busy`.** Un journal corrupto, sin
//!   permisos, o OCUPADO A PROPÓSITO por alguien con acceso al directorio de
//!   estado, deja la sesión sin registro con solo un aviso, mientras que el
//!   daemon con esa misma entrada se niega a arrancar. Hacerlo fallar en
//!   cerrado (con un `--no-journal` explícito para el que sepa lo que hace) es
//!   una decisión de producto pendiente (#178).
//! - **El directorio de estado tiene que ser LOCAL.** WAL + `EXCLUSIVE` sobre
//!   NFS/SMB depende de un `fcntl` que esos sistemas no siempre respetan, y ahí
//!   dos máquinas pueden creerse dueñas del mismo fichero a la vez.

use std::path::Path;
use std::sync::Arc;

/// Qué journal le tocó a este proceso embebido.
///
/// `#[non_exhaustive]`: un cuarto estado (por ejemplo «abierto en solo lectura»)
/// no debería romper a quien haga `match`.
#[non_exhaustive]
pub enum EmbeddedJournal {
    /// Este proceso es el dueño único: sus mutaciones QUEDAN REGISTRADAS en la
    /// cadena.
    ///
    /// Registradas, no deshacibles desde aquí: ningún comando embebido expone
    /// hoy un undo (`Backend::Embedded::undo_session` responde `Unsupported`, y
    /// `norte undo` habla con el daemon). Lo que #167 entrega es el RASTRO —que
    /// es la regla dura 4— y con él la posibilidad de deshacer cuando exista el
    /// comando que lo haga.
    Owned(Arc<crate::journal::SqliteJournal>),
    /// Otro proceso —un daemon, u otro embebido— tiene el lock exclusivo. Se
    /// sigue sin registro, avisando.
    Busy,
    /// El journal no se pudo abrir por una razón que no es el lock (permisos,
    /// disco, DB corrupta, una DB de una era anterior a la cadena de hoy). Se
    /// sigue sin registro, avisando, con el motivo.
    Unavailable(crate::journal::JournalError),
}

/// A mano y no derivado: [`crate::journal::SqliteJournal`] no es `Debug` (lleva
/// dentro el pool de `sqlx`), y de todas formas lo único que un mensaje de test
/// o de log necesita de este tipo es CUÁL de los tres casos salió.
impl std::fmt::Debug for EmbeddedJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owned(_) => f.write_str("Owned(..)"),
            Self::Busy => f.write_str("Busy"),
            Self::Unavailable(motivo) => write!(f, "Unavailable({motivo})"),
        }
    }
}

/// Lo que se espera a que el lock quede libre antes de darlo por ocupado.
///
/// Corto A PROPÓSITO. Quien tiene el lock lo tiene mientras vive (un daemon, o
/// una sesión de TUI de tres horas), así que esperar no lo consigue: el plazo de
/// `sqlx` por omisión son CINCO SEGUNDOS, y con un daemon vivo eso convertía
/// cada `norte cp` embebido en cinco segundos parado antes de seguir igual, sin
/// registro. Lo que sí cabe en este plazo es la única espera que sirve de algo:
/// el hueco entre dos procesos cortos, uno cerrando y el siguiente abriendo.
const ESPERA_POR_EL_LOCK: std::time::Duration = std::time::Duration::from_millis(250);

impl EmbeddedJournal {
    /// Abre `<state>/journal.db` para este proceso.
    ///
    /// Nunca falla: las tres salidas son estados, no errores. Quien llama
    /// decide qué engine construir con cada una — ver [`Self::into_engine`].
    ///
    /// No bloquea más de [`ESPERA_POR_EL_LOCK`] si el journal ya es de otro.
    pub async fn open_in(state_dir: &Path) -> Self {
        let path = state_dir.join("journal.db");
        match crate::journal::SqliteJournal::open_with_busy_timeout(&path, ESPERA_POR_EL_LOCK).await
        {
            Ok(j) => Self::Owned(Arc::new(j)),
            Err(e) if es_lock_ocupado(&e) => Self::Busy,
            Err(e) => Self::Unavailable(e),
        }
    }

    /// El aviso que corresponde a este estado, o `None` si hay journal.
    ///
    /// Existe porque los dos frontends avisan por caminos distintos: el CLI
    /// tiene subscriber de `tracing` (`logging::init`) y el TUI NO —ahí un
    /// `warn!` se descartaría mudo y el aviso tiene que ir a la pantalla—. El
    /// TEXTO es el mismo en los dos, y por eso vive aquí una sola vez.
    ///
    /// Emitirlo es de QUIEN LLAMA, exactamente una vez: [`Self::into_engine`] no
    /// loguea nada. Con las dos cosas juntas —un `warn!` dentro y un `eprintln!`
    /// fuera— el número de avisos dependía de qué binario instala subscriber,
    /// que es justo la clase de cosa que se descubre duplicada en producción.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Owned(_) => None,
            Self::Busy => Some(
                "otro proceso tiene el journal (un daemon, u otra sesión embebida): las \
                 mutaciones de ESTA sesión no quedan registradas (#167)"
                    .to_owned(),
            ),
            Self::Unavailable(motivo) => Some(format!(
                "el journal no se pudo abrir ({motivo}): las mutaciones de ESTA sesión no \
                 quedan registradas (#167)"
            )),
        }
    }

    /// El engine que corresponde.
    ///
    /// `Owned` da [`crate::Engine::with_journal`] (cadena encadenada, y fuente
    /// para el undo cuando haya comando que lo pida); los otros dos dan
    /// [`crate::Engine::new`], que es lo que había antes de #167.
    ///
    /// No emite el aviso: eso es de quien llama, con [`Self::warning`].
    #[must_use]
    pub fn into_engine(self) -> crate::Engine {
        match self {
            Self::Owned(j) => crate::Engine::with_journal(j),
            Self::Busy | Self::Unavailable(_) => crate::Engine::new(),
        }
    }
}

/// Si este error de apertura es «lo tiene otro», y no un problema de verdad.
///
/// Se mira el CÓDIGO de `SQLite`, no su prosa: `SQLITE_BUSY` (5) y
/// `SQLITE_LOCKED` (6), tomando el byte bajo para que valgan también los
/// extendidos (`SQLITE_BUSY_SNAPSHOT` = 261, …). El texto («database is
/// locked») es de la capa de presentación de `sqlx` y nadie lo garantiza entre
/// versiones; con la clasificación colgando de él, un bump que reformatee
/// `Display` convertiría todos los `Busy` en `Unavailable` sin que ningún test
/// se enterase.
///
/// Deliberadamente ESTRECHO: ensanchar esto a «cualquier error» convertiría una
/// DB corrupta o un directorio sin permisos en un `Busy` silencioso, o sea en
/// una sesión sin registro que el operador creería registrada, justo en la
/// máquina que más lo necesita.
fn es_lock_ocupado(e: &crate::journal::JournalError) -> bool {
    let crate::journal::JournalError::Sqlx(sqlx::Error::Database(db)) = e else {
        return false;
    };
    db.code()
        .and_then(|c| c.parse::<i32>().ok())
        .is_some_and(|c| matches!(c & 0xff, 5 | 6))
}
