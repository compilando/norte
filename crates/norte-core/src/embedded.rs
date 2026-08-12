//! El journal del proceso EMBEBIDO (#167), abierto en la PRIMERA mutación
//! (#177).
//!
//! La regla dura 4 —«toda mutación pasa por el journal»— se afirmaba en
//! `CLAUDE.md` y se cumplía en un solo método: `sync.apply` exige journal y
//! rehúsa sin él, mientras copiar, mover, borrar, renombrar y enterrar por el
//! transporte embebido no registraban nada. Este módulo es la otra mitad: el
//! TUI y el CLI sin daemon usan EL journal del directorio de estado, el mismo
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
//! # Por qué PEREZOSO
//!
//! Abrirlo en el arranque hacía dueño del fichero a un proceso que a lo mejor
//! no escribe una fila en toda su vida, y el dueño lo es MIENTRAS VIVE: un
//! `ntc` navegando impedía arrancar el daemon —y con él `norte mcp serve`, o
//! sea el gobierno de los agentes— y le negaba la lectura a `norte audit`
//! (#177). Con [`LazyJournal`] el lock se toma en la primera mutación, que es
//! el primer instante en que hace falta y el único en que el estorbo se
//! justifica. Corolario que ordena el resto del módulo: la definición de «este
//! proceso quiere el journal» dejó de ser una lista de subcomandos que había
//! que mantener a mano y pasó a ser la que de verdad importa — **emite una
//! [`Mutation`](crate::observer::Mutation), o pide la cadena para deshacerla**.
//!
//! El aviso viaja con esa misma pereza: no hay nada que avisar hasta que se
//! intenta abrir. Por eso [`LazyJournal::set_warning_sink`] existe — el TUI no
//! tiene subscriber de `tracing` y el aviso tiene que llegar A LA PANTALLA, en
//! la sesión, cuando ocurre.
//!
//! # Lo que este mecanismo NO cubre
//!
//! - **El dueño lo es MIENTRAS VIVE.** La pereza mueve el instante en que se
//!   toma el lock, no cuánto dura: un TUI que copia un fichero a las 09:00 se
//!   queda `journal.db` hasta que se cierra, y en ese rato `norte daemon run`
//!   no arranca y `norte audit` no lee. Lo que #177 quitó es pagar ese precio
//!   por una sesión que solo navega, que era el caso común; el residual es
//!   soltarlo al quedar ocioso, y eso tiene issue propia (#179) porque reabrir
//!   NO es gratis: `ChainState` (`last_seq`, `last_hash`) hay que releerlo del
//!   fichero en cada reapertura — uno viejo choca contra la PK de `seq` y a
//!   partir de ahí falla toda mutación de ese proceso.
//! - **Una vez decidido, decidido.** Si en la primera mutación el journal era
//!   de otro, la sesión sigue sin registro aunque el otro suelte el lock un
//!   segundo después, y eso incluye al que lo tenía solo de paso (otro `norte
//!   cp` de un script, un `norte audit`, un daemon reiniciándose). Las razones
//!   son que reintentar en cada mutación pagaría `ESPERA_POR_EL_LOCK` por
//!   cada una, y que «esta sesión no queda registrada» ya se le dijo al
//!   usuario y hay un indicador permanente enseñándolo: cambiarlo por detrás
//!   convierte ese indicador en mentira. Lo que **no** es la razón —y así lo
//!   decía este párrafo antes— es el `ChainState`: un intento FALLIDO no
//!   construye ninguno, así que reabrir tras un fallo es seguro y lo único
//!   que hace falta para permitirlo es un reintento con freno y un aviso de
//!   recuperación (#179).
//! - **`Failed` sigue adelante igual que `Busy`.** Un journal corrupto, sin
//!   permisos, o OCUPADO A PROPÓSITO por alguien con acceso al directorio de
//!   estado, deja la sesión sin registro con solo un aviso, mientras que el
//!   daemon con esa misma entrada se niega a arrancar. Hacerlo fallar en
//!   cerrado (con un `--no-journal` explícito para el que sepa lo que hace) es
//!   una decisión de producto pendiente (#178), y la pereza SUBE su gravedad,
//!   de dos maneras que conviene tener escritas:
//!   1. Antes, quien arrancaba con el fichero libre se lo quedaba y era
//!      inmune a un ocupante posterior. Ahora una sesión que aún no ha mutado
//!      no tiene nada, así que un ocupante que abra el fichero UNA vez y se
//!      duerma condena a todas las sesiones embebidas que aún no hayan mutado
//!      —incluidas las ya arrancadas— y la decisión se cachea para siempre.
//!   2. La configuración que #177 hace posible —daemon y `ntc` a la vez— deja
//!      al `ntc` sin registro TODA su vida, por diseño y no por accidente. Que
//!      eso se vea es del indicador permanente del frontend, no de este
//!      módulo; si alguien lo quita, esto vuelve a ser silencioso.
//! - **El directorio de estado tiene que ser LOCAL.** WAL + `EXCLUSIVE` sobre
//!   NFS/SMB depende de un `fcntl` que esos sistemas no siempre respetan, y ahí
//!   dos máquinas pueden creerse dueñas del mismo fichero a la vez.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Por qué esta sesión NO queda registrada en el journal.
///
/// `#[non_exhaustive]`: un motivo nuevo no debería romper a quien haga `match`
/// (el TUI lo mapea a Fluent, y su rama `_` es la red).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoJournal {
    /// Otro proceso —un daemon, u otro embebido que ya mutó— tiene el lock
    /// exclusivo.
    Busy,
    /// No se pudo abrir por una razón que no es el lock (permisos, disco, DB
    /// corrupta, una DB de una era anterior a la cadena de hoy). Lleva el texto
    /// del error: [`crate::journal::JournalError`] no es `Clone` y esto viaja
    /// por un canal hasta la pantalla.
    Failed(String),
}

impl NoJournal {
    /// La frase para el humano, sin traducir.
    ///
    /// Es la del log y la del CLI. El TUI NO la usa salvo como red: su interfaz
    /// pasa por Fluent (`msg-journal-busy` / `msg-journal-unavailable`).
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Busy => "otro proceso tiene el journal (un daemon, u otra sesión embebida): las \
                           mutaciones de ESTA sesión no quedan registradas (#167)"
                .to_owned(),
            Self::Failed(motivo) => format!(
                "el journal no se pudo abrir ({motivo}): las mutaciones de ESTA sesión no \
                 quedan registradas (#167)"
            ),
        }
    }
}

/// Quien recibe el aviso de que esta sesión se quedó sin journal.
///
/// Lo implementa el frontend. El TUI empuja por un canal hasta el run loop, que
/// lo pinta EN la sesión: un `eprintln!` lo tapa la pantalla alternativa un
/// segundo después y la sesión dura horas.
///
/// **Se llama con un lock interno tomado y desde dentro de una mutación**: la
/// implementación tiene que ser corta y no bloquear (un `send` a un canal
/// ilimitado, guardar en un `Mutex`), y no puede volver a entrar en el
/// [`LazyJournal`] que la llamó.
pub trait JournalWarningSink: Send + Sync {
    /// Esta sesión no tiene journal, y este es el motivo. Exactamente una vez
    /// por sesión.
    ///
    /// **No paniquees aquí.** Esto corre dentro del `on_mutation` de una
    /// mutación que ya se aplicó, así que un panic haría fallar la Task de una
    /// operación que funcionó.
    fn on_no_journal(&self, why: &NoJournal);
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

/// El journal de `<state>/journal.db` abierto EN LA PRIMERA MUTACIÓN (#177).
///
/// Es a la vez el [`MutationObserver`](crate::observer::MutationObserver) del
/// engine y su fuente de lectura para el undo, y esas dos caras comparten UNA
/// celda: si fueran dos, el undo podría abrir un segundo handle sobre el mismo
/// fichero —o contestar «sin journal» sobre un fichero libre— y la cadena
/// dejaría de tener un dueño único.
///
/// ```
/// # let rt = tokio::runtime::Builder::new_current_thread()
/// #     .enable_all().build().expect("runtime");
/// // El tempdir se crea FUERA del runtime: crearlo es I/O bloqueante y dentro
/// // de un contexto async sería justo lo que prohíbe la regla dura 2.
/// let dir = tempfile::tempdir().expect("tempdir");
/// let lazy = norte_core::embedded::LazyJournal::in_state_dir(dir.path());
/// // Recién construido no ha tocado el disco: el daemon todavía puede abrirlo.
/// assert!(!lazy.attempted());
/// # rt.block_on(async {
/// // Y pedirlo lo abre.
/// assert!(lazy.get().await.is_some());
/// assert!(lazy.attempted());
/// # });
/// ```
pub struct LazyJournal {
    /// El fichero. Se guarda resuelto para no depender de que el directorio de
    /// estado siga siendo el mismo cuando por fin se abra.
    path: PathBuf,
    /// El resultado del ÚNICO intento de apertura.
    ///
    /// [`tokio::sync::OnceCell`] y no un `Mutex<Option<..>>`: dos mutaciones
    /// concurrentes tienen que compartir el intento, no hacer uno cada una, y
    /// abrir es `async`. Su garantía —el inicializador corre exactamente una
    /// vez— es también la que hace que el aviso salga exactamente una vez.
    cell: tokio::sync::OnceCell<Result<Arc<crate::journal::SqliteJournal>, NoJournal>>,
    /// A dónde va el aviso, y el aviso que espera a que haya dónde.
    sink: Mutex<SinkSlot>,
}

/// El sink y el aviso PENDIENTE, bajo un solo lock.
///
/// Dos campos y un lock, y no dos locks ni un `OnceLock` para el sink, porque
/// la única propiedad que importa es atómica entre los dos: instalar un sink y
/// entregarle lo pendiente no puede entrelazarse con «resolver y avisar», o el
/// aviso se pierde (sink instalado un instante tarde) o se duplica. Perderlo es
/// el fallo que #177 llama «peor que hoy»: una sesión que muta sin registro y
/// sin decirlo.
#[derive(Default)]
struct SinkSlot {
    sink: Option<Arc<dyn JournalWarningSink>>,
    pendiente: Option<NoJournal>,
}

impl LazyJournal {
    /// El journal `<state_dir>/journal.db`, todavía SIN abrir.
    ///
    /// No toca el disco: construirlo es gratis y no le quita el fichero a
    /// nadie. Ese es el punto de #177.
    #[must_use]
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("journal.db"),
            cell: tokio::sync::OnceCell::new(),
            sink: Mutex::new(SinkSlot::default()),
        }
    }

    /// Instala a quien recibe el aviso, y le entrega el que ya hubiera.
    ///
    /// Lo segundo importa: el sink lo instala el arranque del frontend, y un
    /// arranque que se cruce con una mutación temprana no puede dejar muda a
    /// una sesión sin registro.
    ///
    /// **Una sola vez por proceso.** Un segundo sink reemplaza al primero —el
    /// primer receptor se queda mudo para siempre— y encima ya no encuentra el
    /// aviso pendiente, que el primero se llevó. Quien lo instala es el
    /// arranque del frontend, vía [`crate::backend::Backend::take_journal_warnings`],
    /// que es de un solo dueño por el mismo motivo que sus canales hermanos.
    ///
    /// # Panics
    /// Si el lock interno está envenenado (otro hilo hizo panic sosteniéndolo)
    /// — irrecuperable, mismo criterio que el resto de locks del engine.
    pub fn set_warning_sink(&self, sink: Arc<dyn JournalWarningSink>) {
        let mut slot = self.sink.lock().expect("lock del sink sano");
        if let Some(why) = slot.pendiente.take() {
            sink.on_no_journal(&why);
        }
        slot.sink = Some(sink);
    }

    /// ¿Se ha intentado ya abrir el journal?
    ///
    /// «Intentado», no «conseguido»: lo que responde es si este proceso ya
    /// decidió. Para tests y para quien quiera saber si el lock está en juego.
    #[must_use]
    pub fn attempted(&self) -> bool {
        self.cell.get().is_some()
    }

    /// El journal de esta sesión, abriéndolo si es la primera vez que se pide.
    ///
    /// `None` = esta sesión NO queda registrada, y el motivo ya se avisó
    /// (exactamente una vez, aquí dentro).
    pub async fn get(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        self.resolve().await.as_ref().ok().map(Arc::clone)
    }

    /// El intento único.
    async fn resolve(&self) -> &Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        self.cell
            .get_or_init(|| async {
                let r = match crate::journal::SqliteJournal::open_with_busy_timeout(
                    &self.path,
                    ESPERA_POR_EL_LOCK,
                )
                .await
                {
                    Ok(j) => Ok(Arc::new(j)),
                    Err(e) if es_lock_ocupado(&e) => Err(NoJournal::Busy),
                    Err(e) => Err(NoJournal::Failed(e.to_string())),
                };
                if let Err(why) = &r {
                    self.avisar(why);
                }
                r
            })
            .await
    }

    /// Emite el aviso EXACTAMENTE UNA VEZ, por el canal que corresponda.
    ///
    /// **Quien instala sink se hace cargo de la entrega**, y por eso el `warn!`
    /// es el `else` y no un añadido: los dos frontends avisan por caminos
    /// distintos —el TUI a la pantalla (no instala subscriber de `tracing`, así
    /// que ahí un `warn!` se descarta mudo) y el CLI a stderr por su propio
    /// sink, que llega diga lo que diga `RUST_LOG`— y emitir por los dos a la
    /// vez le enseñaría al usuario del CLI la misma frase dos veces. El `warn!`
    /// cubre al que no instala ninguno (un embebedor de la biblioteca, o una
    /// mutación que se adelante al arranque del frontend): lo que no puede
    /// pasar es que esto sea MUDO.
    ///
    /// # Panics
    /// Si el lock interno está envenenado (otro hilo hizo panic sosteniéndolo)
    /// — irrecuperable, mismo criterio que el resto de locks del engine.
    ///
    /// Un `sink` que paniquee propaga desde aquí hasta el `on_mutation` y hace
    /// fallar la Task de una mutación que YA se aplicó. Es responsabilidad de
    /// quien lo implementa (ver [`JournalWarningSink`]); a cambio, un aviso
    /// tragado en silencio sería peor.
    fn avisar(&self, why: &NoJournal) {
        let mut slot = self.sink.lock().expect("lock del sink sano");
        if let Some(s) = &slot.sink {
            s.on_no_journal(why);
        } else {
            tracing::warn!(motivo = %why.text(), "sesión embebida SIN journal");
            slot.pendiente = Some(why.clone());
        }
    }
}

/// A mano y no derivado: [`crate::journal::SqliteJournal`] no es `Debug` (lleva
/// dentro el pool de `sqlx`), y lo único que un mensaje de test o de log
/// necesita de este tipo es en qué punto está la decisión.
impl std::fmt::Debug for LazyJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let estado = match self.cell.get() {
            None => "sin abrir".to_owned(),
            Some(Ok(_)) => "dueño".to_owned(),
            Some(Err(why)) => format!("sin journal ({why:?})"),
        };
        f.debug_struct("LazyJournal")
            .field("path", &self.path)
            .field("estado", &estado)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl crate::observer::MutationObserver for LazyJournal {
    /// La costura de la regla dura 4 en el proceso embebido, y el disparador de
    /// la apertura.
    ///
    /// Sin journal devuelve `Ok(())`: la mutación YA OCURRIÓ (el observer se
    /// llama después del efecto), así que fallar aquí no la desharía, solo
    /// diría que falló algo que funcionó. Lo que NO es aceptable es que además
    /// sea muda, y por eso el aviso está en `LazyJournal::resolve`, que corre
    /// exactamente una vez y avisa antes de que este `Ok(())` vuelva.
    async fn on_mutation(
        &self,
        mutation: &crate::observer::Mutation<'_>,
        actor: &crate::journal::Actor,
    ) -> Result<(), norte_proto::Error> {
        match self.get().await {
            Some(j) => {
                crate::observer::MutationObserver::on_mutation(j.as_ref(), mutation, actor).await
            }
            None => Ok(()),
        }
    }
}

/// El engine embebido de un frontend: journal perezoso sobre `state_dir`.
///
/// Es lo que llaman `norte-tui` y `norte-cli` en vez de `Engine::new()`. No
/// abre nada todavía (ver [`LazyJournal`]), así que da igual que el comando
/// acabe mutando o no — que es lo que quitó de en medio la lista de subcomandos
/// que había que mantener a mano (#177).
///
/// **Uno por proceso.** Cada llamada acuña un [`LazyJournal`] nuevo, o sea otro
/// candidato a dueño del MISMO fichero: dos engines de este tipo vivos a la vez
/// en un proceso acaban con el segundo viéndose `Busy` contra el lock del
/// primero — un proceso que se niega a journalizar porque se lo impide él
/// mismo. Hoy `norte-cli` tiene dos sitios donde se llama (`run` y `ai_cmd`) y
/// son excluyentes porque `Cmd::Ai` sale antes por su propio brazo; si eso
/// cambia, esto tiene que pasar a ser un `OnceLock` de proceso.
#[must_use]
pub fn engine_in(state_dir: &Path) -> crate::Engine {
    crate::Engine::with_lazy_journal(Arc::new(LazyJournal::in_state_dir(state_dir)))
}

/// Si este error de apertura es «lo tiene otro», y no un problema de verdad.
///
/// Se mira el CÓDIGO de `SQLite`, no su prosa: `SQLITE_BUSY` (5) y
/// `SQLITE_LOCKED` (6), tomando el byte bajo para que valgan también los
/// extendidos (`SQLITE_BUSY_SNAPSHOT` = 261, …). El texto («database is
/// locked») es de la capa de presentación de `sqlx` y nadie lo garantiza entre
/// versiones; con la clasificación colgando de él, un bump que reformatee
/// `Display` convertiría todos los `Busy` en `Failed` sin que ningún test se
/// enterase.
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
