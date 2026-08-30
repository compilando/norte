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
//! # La ventana de propiedad se abre y se cierra más de una vez (#179)
//!
//! El dueño lo era MIENTRAS VIVÍA, y el veredicto se decidía UNA vez. Las dos
//! cosas eran la misma: una ventana de propiedad que solo sabía abrirse.
//!
//! - **Se reintenta, con freno.** Si en la primera mutación el journal era de
//!   otro, esta sesión vuelve a intentarlo — como mucho una vez cada
//!   [`FRENO_TRAS_FALLO`], para no pagar `ESPERA_POR_EL_LOCK` por mutación.
//!   El ocupante suele ser de paso (otro `norte cp` de un script, un `norte
//!   audit`, un daemon reiniciándose) y un cuarto de segundo de solape marcaba
//!   una sesión de tres horas. Reintentar tras un fallo es seguro: un intento
//!   FALLIDO no construye ningún `ChainState`.
//! - **Y se suelta**, con [`LazyJournal::release`], que cierra el pool y
//!   devuelve el fichero. Solo cuando nadie más sostiene el handle: abrir un
//!   segundo sobre el mismo fichero sería este proceso quitándose el journal a
//!   sí mismo. La POLÍTICA es [`LazyJournal::release_if_idle`], y vive aquí y
//!   no en el frontend porque el reloj de «cuándo se usó» es de esta ventana y
//!   porque decidir y cerrar tienen que pasar bajo el MISMO lock; el frontend
//!   solo elige cada cuánto preguntar (el TUI, en su tick de sesión).
//! - **Reabrir RELEE la cadena, y eso no es negociable.** `ChainState`
//!   (`last_seq`, `last_hash`) sale del fichero en CADA adquisición porque
//!   [`crate::journal::Journal`] se construye de nuevo: un par viejo choca
//!   contra la PK de `seq`, y como `last_seq` solo avanza al acertar, fallarían
//!   TODAS las mutaciones siguientes — un efecto aplicado sin su fila, en
//!   bucle. Por eso `release` DESTRUYE el handle en vez de guardarlo.
//! - **Cada transición llega al sink**, la recuperación incluida
//!   ([`JournalStatus`]). El TUI pinta un indicador permanente de «esta sesión
//!   NO queda registrada» y una sesión que volviera a registrar en silencio lo
//!   convertiría en mentira.
//!
//! # `Failed` falla en CERRADO, `Busy` no (#178)
//!
//! Los dos motivos tenían la MISMA consecuencia —sin journal, un aviso, y a
//! seguir—, así que la clasificación no compraba nada y fallaba en ABIERTO
//! justo donde el daemon falla en cerrado (`daemon run` aborta con esa misma
//! entrada). Cualquiera con escritura en el directorio de estado desactivaba el
//! registro de todas las sesiones embebidas —`ntc`, `norte cp/mv/rm/mkdir` y,
//! la valiosa, `norte ai rename --yes`— en silencio y para siempre, detrás de
//! un aviso al que el usuario está entrenado a no hacer caso porque también
//! salta en el caso benigno. Y la pereza de #177 SUBÍA su gravedad: una sesión
//! que aún no había mutado no tenía nada, así que un ocupante que abriera el
//! fichero una vez y se durmiera condenaba también a las ya arrancadas.
//!
//! Ahora se separan, y el reparto es el que impide que el arreglo sea peor que
//! el agujero:
//!
//! - **`Failed` REHÚSA**, con
//!   [`Error::JournalUnavailable`](norte_proto::Error::JournalUnavailable), y
//!   lo hace ANTES del efecto: en el gate de [`crate::Engine`], no en el
//!   observer. Cuando el observer corre la mutación ya ocurrió, así que fallar
//!   ahí no la desharía — solo diría que falló algo que funcionó.
//! - **`Busy` SIGUE**, sin registro y avisando. Rehusar aquí convertiría «hay
//!   un daemon» en «el gestor de ficheros no funciona», y un ocupante de paso
//!   tumbaría una sesión de tres horas: la misma razón por la que existe el
//!   reintento de #179, que además es lo que cura este caso solo.
//!
//! No hay `--no-journal` que lo salte, y es a propósito: la salida es arreglar
//! o quitar el fichero, que es lo que dice el mensaje. Una bandera para «muta
//! sin registrar» acaba en un alias, y con ella el agujero vuelve entero.
//!
//! # El veredicto se fija POR OPERACIÓN, no por mutación (#205)
//!
//! Que la ventana sepa reabrirse abrió un filo que antes no existía: con el
//! journal preguntado por MUTACIÓN, un `copy_tree` que empieza con el fichero
//! ocupado y dura más que [`FRENO_TRAS_FALLO`] empezaba a registrar a mitad —
//! las primeras k entradas sin fila, las n-k siguientes con ella, dentro de UNA
//! Task y UN actor. Y entonces `undo` desanda la cola registrada y deja la
//! cabeza que no lo está: media copia deshecha, sin poder nombrar la otra
//! mitad, porque de ella no hay filas. **«No quedó registrado» se arregla a
//! mano; «quedó registrado a medias» es una trampa**, y era peor que el agujero
//! que el reintento vino a cerrar.
//!
//! Lo cierra [`MutationObserver::pin_for_task`](crate::observer::MutationObserver::pin_for_task):
//! el cuerpo de cada Task que muta resuelve el journal UNA vez, antes del
//! primer efecto, y se queda con lo que le salga —el handle, o un no-op— para
//! todas sus mutaciones. La operación vuelve a quedar entera dentro o entera
//! fuera, que es lo que era cuando el veredicto duraba toda la sesión.
//!
//! Dos corolarios que conviene tener escritos:
//!
//! - **el aviso de recuperación habla de lo que EMPIEZA**, no de «a partir de
//!   ahora»: una operación en vuelo conserva el veredicto con el que arrancó, y
//!   una frase que prometiera lo contrario sería falsa justo para ella;
//! - **mientras una Task muta, [`LazyJournal::release`] contesta `false`**,
//!   porque el handle fijado es un `Arc` vivo. Eso protege la ventana FIJADO →
//!   última fila, que es donde hay filas que perder. La que va del gate al
//!   fijado no la protege nadie —el gate suelta su `Arc` y la Task puede
//!   esperar en la cola— y ahí soltar no rompe nada, pero puede dejar la
//!   operación entera sin registrar si otro gana la reapertura;
//! - **el fijado vuelve a mirar si el journal es ILEGIBLE**, y rehúsa la Task
//!   si lo es (#178). El gate mira antes de encolar y esto mira al empezar de
//!   verdad; entre los dos caben treinta segundos en los que un fichero puede
//!   corromperse.
//!
//! # Lo que este mecanismo NO cubre
//!
//! - **Nadie suelta el journal por su cuenta.** [`LazyJournal::release`] es la
//!   primitiva; no hay temporizador de ociosidad que la llame, así que una
//!   sesión que mutó a las 09:00 sigue siendo la dueña hasta que el frontend
//!   decida soltar. Esa política es la mitad de #179 que no vive aquí.
//! - **`gc_partials` no pasa por el gate**, así que barre sus propios
//!   `.norte-partial` aunque el journal esté ilegible. Es basura de este
//!   proceso, no datos del usuario, y nunca llevó fila.
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
    /// pasa por Fluent (`msg-journal-busy` / `msg-journal-refused`).
    ///
    /// **Las dos frases dicen cosas DISTINTAS desde #178**, y confundirlas es
    /// el defecto que esa issue existe para no repetir: `Busy` es «esto pasó y
    /// no quedó anotado», `Failed` es «esto NO ha pasado».
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Busy => "otro proceso tiene el journal (un daemon, u otra sesión embebida): las \
                           mutaciones de ESTA sesión no quedan registradas (#167)"
                .to_owned(),
            Self::Failed(motivo) => format!(
                "el journal no se pudo abrir ({motivo}): esta sesión REHÚSA mutar mientras \
                 siga así, porque nada quedaría registrado ni se podría deshacer. Arregla \
                 lo que nombra el motivo —el directorio o el fichero— y vuelve a intentarlo \
                 (#178)"
            ),
        }
    }
}

/// Lo que le pasó al journal de ESTA sesión, en el orden en que le pasó.
///
/// No es un estado que se consulte: es el evento que cruza hasta la pantalla.
/// Existe porque el indicador del frontend tiene que poder APAGARSE — un
/// «NO se registra» que no sabe volverse «ya sí» miente en cuanto la ventana
/// se reabre (#179).
///
/// `#[non_exhaustive]`: una transición nueva no debería romper a quien haga
/// `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalStatus {
    /// Esta sesión dejó de quedar registrada, por este motivo.
    Lost(NoJournal),
    /// Y volvió a quedarlo: la ventana se reabrió.
    Recovered,
    /// **El journal lleva [`SOSPECHA_TRAS`] ocupado y no hay daemon
    /// escuchando** (#203).
    ///
    /// [`NoJournal::Busy`] es benigno casi siempre: un daemon vivo, otro `ntc`,
    /// un `norte cp` de un script. Por eso sigue adelante y solo avisa — y por
    /// eso mismo el aviso se ignora, que es lo que hace barato el ataque:
    /// cualquiera con el mismo uid retiene el lock (`begin exclusive`) y todas
    /// las sesiones embebidas dejan de registrar, detrás de una frase que
    /// también sale cuando no pasa nada.
    ///
    /// Estos dos hechos juntos SÍ distinguen un caso del otro, y hoy nadie los
    /// juntaba: *lleva minutos ocupado* y *el socket del daemon no contesta*.
    /// No prueba que haya un atacante —un daemon muerto a mitad de una
    /// transacción larga da lo mismo— pero deja de ser el caso corriente, y
    /// eso es exactamente lo que un indicador tiene que poder decir.
    ///
    /// Lo que NO hace es rehusar la mutación. Convertir «alguien tiene tu
    /// journal» en «el gestor de ficheros no funciona» es el arreglo que #178
    /// evitó a propósito.
    Squatted,
}

/// Quien recibe los cambios de «esta sesión queda registrada» o no.
///
/// Lo implementa el frontend. El TUI empuja por un canal hasta el run loop, que
/// lo pinta EN la sesión: un `eprintln!` lo tapa la pantalla alternativa un
/// segundo después y la sesión dura horas.
///
/// **Se llama con locks internos tomados y desde dentro de una mutación**: la
/// implementación tiene que ser corta y no bloquear (un `send` a un canal
/// ilimitado, guardar en un `Mutex`), y **no puede volver a entrar en el
/// [`LazyJournal`] que la llamó** — el lock de la ventana de propiedad está
/// tomado y reentrar lo bloquearía para siempre.
pub trait JournalWarningSink: Send + Sync {
    /// Esta sesión NO queda registrada, y este es el motivo. Una vez por
    /// EPISODIO: mientras el motivo no cambie, no se repite.
    ///
    /// **No paniquees aquí.** Esto corre dentro del `on_mutation` de una
    /// mutación que ya se aplicó, así que un panic haría fallar la Task de una
    /// operación que funcionó.
    fn on_no_journal(&self, why: &NoJournal);

    /// La sesión VOLVIÓ a quedar registrada: la ventana se reabrió tras un
    /// [`Self::on_no_journal`].
    ///
    /// Sin cuerpo por omisión A PROPÓSITO: un sink que se olvide de esto deja
    /// su indicador encendido sobre una sesión que sí registra, que es
    /// exactamente la mentira que #179 vino a quitar. Que el compilador lo
    /// pregunte.
    ///
    /// No se emite en la PRIMERA apertura, que no recupera nada.
    fn on_journal_recovered(&self);

    /// El journal lleva minutos ocupado y NO hay daemon escuchando (#203):
    /// [`JournalStatus::Squatted`].
    ///
    /// Con cuerpo por omisión, al revés que [`Self::on_journal_recovered`], y
    /// la asimetría es deliberada: olvidarse de la recuperación deja un
    /// indicador MINTIENDO, mientras que olvidarse de ésta solo deja el aviso
    /// genérico —que es lo que se enseñaba hasta ahora— así que el default cae
    /// del lado de decir menos, nunca de decir algo falso.
    fn on_journal_squatted(&self) {
        self.on_no_journal(&NoJournal::Busy);
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

/// Como mucho un intento de apertura cada tanto, tras uno que falló.
///
/// El reintento de #179 es lo que salva a la sesión del ocupante de paso, pero
/// sin freno costaría `ESPERA_POR_EL_LOCK` en CADA mutación mientras el
/// ocupante siga ahí —y el ocupante habitual, un daemon, sigue ahí toda la
/// tarde—. Treinta segundos es el orden de magnitud de «reiniciar un daemon»
/// sin ser el de «notarlo al copiar».
pub const FRENO_TRAS_FALLO: std::time::Duration = std::time::Duration::from_secs(30);

/// Cuánto lleva `Busy` un journal antes de que valga la pena preguntarse si
/// quien lo tiene es un daemon (#203).
///
/// Cinco minutos, y el número tiene dos lados. Corto de más, un daemon que
/// arranca lento o una sesión de otro `ntc` se llevarían la frase fuerte, que
/// es exactamente el ruido que este issue viene a quitar. Largo de más, la
/// sesión pasa la tarde entera sin registrar detrás del aviso suave. Un
/// arranque de daemon y un `norte cp` de un script viven en segundos; cinco
/// minutos no es ninguno de los dos.
pub const SOSPECHA_TRAS: std::time::Duration = std::time::Duration::from_mins(5);

/// Quién contesta «¿hay un daemon escuchando?» (#203).
///
/// Es una inyección y no una llamada directa por dos razones, y la segunda es
/// la que manda: un test no puede levantar un daemon para probar el caso en
/// que NO lo hay, y el `LazyJournal` no tiene por qué saber dónde vive un
/// socket. El default de producción es [`DaemonSocketProbe`].
pub trait DaemonPresence: Send + Sync {
    /// `true` si algo contesta en el socket del daemon de este usuario.
    ///
    /// Un socket huérfano (el daemon murió sin limpiarlo) contesta `false`:
    /// lo que importa es si hay alguien AL OTRO LADO, no si el fichero existe.
    fn any_daemon_listening(&self) -> bool;
}

/// El probe de producción: un `connect` al socket por omisión de este uid.
///
/// Mismo criterio que el arranque del daemon usa para detectar otro vivo — un
/// `connect` lo delata, y un socket huérfano da `ECONNREFUSED`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DaemonSocketProbe;

impl DaemonPresence for DaemonSocketProbe {
    fn any_daemon_listening(&self) -> bool {
        // Síncrono y no bloqueante en la práctica: un `connect` a un socket
        // unix local resuelve en el acto, exista o no. Corre con el lock de la
        // ventana tomado, así que aquí no cabe nada más caro.
        std::os::unix::net::UnixStream::connect(crate::daemon::default_socket_path(None)).is_ok()
    }
}

/// Lo que se espera a que el pool termine de cerrarse en [`LazyJournal::release`].
///
/// Un tope y no una espera indefinida: el cierre corre con el lock de la
/// ventana tomado, y ese lock lo necesita cada mutación. Generoso a propósito —
/// cerrar es local y rápido, así que agotarlo ya es una anomalía.
const ESPERA_POR_EL_CIERRE: std::time::Duration = std::time::Duration::from_secs(5);

/// El journal de `<state>/journal.db` abierto EN LA PRIMERA MUTACIÓN (#177), y
/// reabierto cuantas veces haga falta (#179).
///
/// Es a la vez el [`MutationObserver`](crate::observer::MutationObserver) del
/// engine y su fuente de lectura para el undo, y esas dos caras comparten UNA
/// ventana: si fueran dos, el undo podría abrir un segundo handle sobre el mismo
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
/// // Soltarlo lo devuelve, y el siguiente que lo pida lo vuelve a abrir —
/// // releyendo la cadena del fichero.
/// assert!(lazy.release().await);
/// assert!(lazy.get().await.is_some());
/// # });
/// ```
pub struct LazyJournal {
    /// El fichero. Se guarda resuelto para no depender de que el directorio de
    /// estado siga siendo el mismo cuando por fin se abra.
    path: PathBuf,
    /// Cada cuánto se puede reintentar tras un intento fallido.
    freno: std::time::Duration,
    /// **La ventana de propiedad, entera y bajo UN lock**: el handle mientras
    /// se tiene, el último veredicto cuando no, y lo último que se le dijo al
    /// sink.
    ///
    /// Un [`tokio::sync::Mutex`] y no un `std`: abrir es `async` y el lock se
    /// sostiene A TRAVÉS del `await` a propósito — es lo que serializa los
    /// intentos. Sin eso, dos mutaciones concurrentes abrirían dos handles y la
    /// segunda se vería `Busy` contra el lock de la primera, o sea un proceso
    /// negándose a journalizar por culpa de sí mismo. Es la propiedad que daba
    /// gratis el `OnceCell` que había aquí antes, y la que hay que reproducir a
    /// mano ahora que el intento puede repetirse.
    estado: tokio::sync::Mutex<Ventana>,
    /// Quién contesta si hay un daemon escuchando (#203). Se consulta SOLO
    /// cuando un `Busy` ya lleva [`Self::sospecha`], que es lo que hace que el
    /// `connect` no cueste nada en el caso normal.
    presencia: Arc<dyn DaemonPresence>,
    /// Cuánto tiene que llevar ocupado un episodio antes de preguntarse por el
    /// daemon. [`SOSPECHA_TRAS`] salvo en tests.
    sospecha: std::time::Duration,
    /// A dónde van los avisos, y el aviso que espera a que haya dónde.
    ///
    /// **Orden de locks: `estado` → `sink`, y jamás al revés.** `emitir` corre
    /// SIEMPRE con `estado` tomado, y de eso depende algo que no se ve: el «qué
    /// se anunció» vive en `estado` y el «qué queda pendiente» vive aquí, o sea
    /// en dos locks distintos, y solo son coherentes porque los dos se tocan
    /// dentro de la misma sección crítica. Un `set_warning_sink` que se pusiera
    /// a leer `estado` invertiría el orden y sería un abrazo mortal.
    sink: Mutex<SinkSlot>,
    /// Intentos de apertura pagados. FUERA del lock porque
    /// [`LazyJournal::attempted`] es síncrono (lo llama un `Debug` y lo llaman
    /// los tests) y porque contar no necesita exclusión.
    intentos: std::sync::atomic::AtomicU64,
}

/// El estado de la ventana de propiedad.
#[derive(Default)]
struct Ventana {
    /// El handle MIENTRAS esta sesión es la dueña.
    ///
    /// `None` no significa «no se pudo»: significa «ahora mismo no lo tiene»,
    /// que también es el estado recién construido y el de después de un
    /// [`LazyJournal::release`].
    handle: Option<Arc<crate::journal::SqliteJournal>>,
    /// El último intento FALLIDO: cuándo y qué dijo. Lo consulta el freno.
    /// `None` mientras se tiene el handle, o antes del primer intento.
    ultimo_fallo: Option<(std::time::Instant, NoJournal)>,
    /// Lo último que se le contó al sink, para no repetirlo ni contradecirlo.
    anunciado: Option<JournalStatus>,
    /// Cuándo se usó el handle por última vez, para la política de ociosidad
    /// (#179). `None` mientras no se tiene.
    ultimo_uso: Option<std::time::Instant>,
    /// Desde cuándo lleva ocupado este EPISODIO (#203): lo pone el primer
    /// `Busy` y lo borra cualquier otra cosa —una apertura buena, un `Failed`—,
    /// porque lo que se mide es «cuánto lleva ESTE ocupante», no cuántos ha
    /// habido.
    ocupado_desde: Option<std::time::Instant>,
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
    /// Solo se retiene una PÉRDIDA. Una recuperación sin sink no tiene nada que
    /// apagar —nadie encendió nada— así que en vez de encolarse BORRA lo
    /// pendiente: entregarle a un sink tardío una pérdida que ya se recuperó
    /// sería encender un indicador para una sesión que sí registra.
    pendiente: Option<NoJournal>,
}

impl LazyJournal {
    /// El journal `<state_dir>/journal.db`, todavía SIN abrir.
    ///
    /// No toca el disco: construirlo es gratis y no le quita el fichero a
    /// nadie. Ese es el punto de #177.
    #[must_use]
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self::with_retry_brake(state_dir, FRENO_TRAS_FALLO)
    }

    /// Como [`Self::in_state_dir`], con otro freno de reintento.
    ///
    /// Existe para los tests —que no pueden esperar [`FRENO_TRAS_FALLO`] para
    /// ver un reintento, ni fiarse de un reloj para ver que NO lo hubo— y para
    /// un embebedor con otra cadencia. `Duration::ZERO` reintenta en cada
    /// mutación, con lo que eso cuesta.
    #[must_use]
    pub fn with_retry_brake(state_dir: &Path, freno: std::time::Duration) -> Self {
        Self {
            path: state_dir.join("journal.db"),
            freno,
            estado: tokio::sync::Mutex::new(Ventana::default()),
            presencia: Arc::new(DaemonSocketProbe),
            sospecha: SOSPECHA_TRAS,
            sink: Mutex::new(SinkSlot::default()),
            intentos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Con otro probe de presencia de daemon (#203).
    ///
    /// Para los tests, que necesitan el caso «lleva ocupado un buen rato y no
    /// hay daemon» sin levantar ninguno, y para un embebedor que sepa por otra
    /// vía si hay uno.
    #[must_use]
    pub fn with_daemon_presence(mut self, presencia: Arc<dyn DaemonPresence>) -> Self {
        self.presencia = presencia;
        self
    }

    /// Con otro plazo de sospecha (#203).
    ///
    /// Para los tests, que no pueden esperar [`SOSPECHA_TRAS`] ni fiarse de un
    /// reloj para ver que el plazo NO se cumplió. `Duration::ZERO` sospecha en
    /// el segundo intento del mismo episodio — el primero abre el episodio, y
    /// eso es del diseño y no del plazo.
    #[must_use]
    pub fn with_suspicion_delay(mut self, sospecha: std::time::Duration) -> Self {
        self.sospecha = sospecha;
        self
    }

    /// Instala a quien recibe los avisos, y le entrega el que ya hubiera.
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
    /// Y hay una segunda razón, más fina: lo pendiente es lo NO ENTREGADO, no
    /// el estado. Un sink que llegue después de que el primero se llevara la
    /// pérdida se cree cubierto sobre una sesión que no lo está, y más tarde
    /// recibirá una recuperación de algo que nunca enseñó.
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

    /// ¿Se ha intentado ya abrir el journal, alguna vez?
    ///
    /// «Intentado», no «conseguido» ni «ahora mismo»: lo que responde es si este
    /// proceso ya pagó una apertura. Para tests y para quien quiera saber si el
    /// lock llegó a estar en juego.
    #[must_use]
    pub fn attempted(&self) -> bool {
        self.attempts() > 0
    }

    /// Cuántas aperturas se han pagado.
    ///
    /// Es lo que mide el freno de #179 en los tests: «tardó menos de X» mide la
    /// carga de la máquina tanto como el código, y contra los 250 ms de
    /// `ESPERA_POR_EL_LOCK` el margen no daba para distinguirlos.
    #[must_use]
    pub fn attempts(&self) -> u64 {
        self.intentos.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// El journal de esta sesión, abriéndolo si ahora mismo no se tiene.
    ///
    /// `None` = esta sesión NO queda registrada, y el motivo ya se avisó (una
    /// vez por episodio, aquí dentro).
    ///
    /// **El [`Arc`] devuelto no debe sobrevivir a la operación que lo pidió.**
    /// Mientras viva, [`Self::release`] no puede soltar el fichero (devuelve
    /// `false`), así que guardarlo en un campo de vida larga convierte el
    /// release en un no-op permanente y silencioso. Los consumidores de este
    /// árbol lo sostienen exactamente lo que dura su Task, que es lo correcto.
    pub async fn get(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        self.resolve().await.ok()
    }

    /// Suelta el journal: cierra el pool y devuelve el fichero a quien lo
    /// quiera (el daemon, un `norte audit`).
    ///
    /// `true` si al volver esta sesión no lo tiene — incluido el caso de que no
    /// lo tuviera ya. `false` si alguien más sostiene un [`Arc`] del handle: ahí
    /// NO se suelta, porque cerrar el pool por debajo de una mutación en vuelo
    /// la mataría, y reabrir mientras el otro `Arc` vive abriría un SEGUNDO
    /// handle sobre el mismo fichero.
    ///
    /// La siguiente mutación lo vuelve a abrir, releyendo la cadena del fichero
    /// — que es la razón de que aquí se DESTRUYA el handle en vez de guardarlo
    /// (ver la nota del módulo sobre `ChainState`).
    ///
    /// # Precondición: NINGUNA mutación puede estar entre su gate y su fila
    ///
    /// Esto es lo que hay que resolver ANTES de llamar a esto desde un
    /// temporizador de ociosidad, y es la razón de que el temporizador no esté
    /// escrito todavía.
    ///
    /// [`crate::Engine`] comprueba el journal en el gate, ANTES del efecto, y
    /// lo escribe en el observer, DESPUÉS. Entre esos dos instantes `release`
    /// puede cerrar la ventana; si además otro proceso se lleva el fichero
    /// mientras tanto, la reapertura del observer da `Busy`, `on_mutation`
    /// contesta `Ok(())` —la mutación ya ocurrió, fallar ahí solo mentiría
    /// sobre algo que funcionó— y el efecto se queda sin fila. El aviso al
    /// frontend SÍ sale, así que no es mudo, pero la regla dura 4 se rompe.
    ///
    /// **Desde #205 la precondición se cumple sola dentro del engine**, y por
    /// eso este método puede empezar a tener llamantes: el cuerpo de cada Task
    /// que muta fija el journal al principio
    /// ([`MutationObserver::pin_for_task`](crate::observer::MutationObserver::pin_for_task))
    /// y sostiene ese `Arc` hasta el final, así que `Arc::try_unwrap` falla y
    /// esto contesta `false` mientras haya una operación en vuelo. Antes no
    /// era así: `crate::ops` tomaba y soltaba el handle POR ENTRADA, y entre
    /// dos entradas no lo sostenía nadie.
    ///
    /// Lo que sigue sin cubrir el `Arc` es un observer que no sea este —un
    /// embebedor con su propio [`crate::observer::MutationObserver`] que no
    /// implemente `pin_for_task`—, y ahí la precondición vuelve a ser del
    /// llamante.
    ///
    /// # Esto NO es cancel-safe. Córrelo entero o `tokio::spawn`-éalo.
    ///
    /// Dropear este future en el `await` del cierre deja el handle ya SACADO de
    /// la ventana y el pool cerrándose por su cuenta en el worker de `sqlx`: el
    /// siguiente `resolve` abre contra nuestra propia conexión agonizando y se
    /// lleva un `Busy` que nos hemos inventado nosotros — un aviso de «sesión
    /// sin registro» falso, y [`FRENO_TRAS_FALLO`] de mutaciones de verdad sin
    /// registrar hasta que el reintento lo cura. O sea, exactamente el bug que
    /// esta función existe para no tener.
    ///
    /// Un `tokio::select!` con esto en una rama lo dispara. Ponlo en el CUERPO
    /// de la rama, no en la condición.
    pub async fn release(&self) -> bool {
        let mut v = self.estado.lock().await;
        Self::release_bajo_lock(&mut v).await
    }

    /// Suelta el journal **si lleva `ocioso` sin usarse** (#179).
    ///
    /// Es la política que la primitiva no traía, y vive aquí y no en el
    /// frontend por dos razones: el reloj de «cuándo se usó» es de esta
    /// ventana —el frontend tendría que espiarlo—, y la decisión y el cierre
    /// tienen que ocurrir bajo el MISMO lock, o entre mirar y soltar cabe una
    /// mutación que se queda sin registro.
    ///
    /// Devuelve `true` si al volver el fichero está libre: se soltó ahora, o
    /// no lo teníamos. `false` = sigue siendo nuestro, porque aún no está
    /// ocioso o porque alguien sostiene el handle (una Task en vuelo lo fija
    /// entero, ver [`Self::release`]).
    ///
    /// Lo que esto arregla es que un `ntc` que copió un fichero a las 09:00 se
    /// quedaba `journal.db` hasta salir, así que `norte daemon run` y `norte
    /// audit` no podían abrirlo en todo el día. La reapertura RELEE la cadena,
    /// que es lo que hace que soltar sea seguro.
    ///
    /// # Esto NO es cancel-safe, por lo mismo que [`Self::release`].
    pub async fn release_if_idle(&self, ocioso: std::time::Duration) -> bool {
        let mut v = self.estado.lock().await;
        if v.handle.is_none() {
            return true;
        }
        // Sin sello de uso no se suelta: se acaba de adquirir por un camino
        // que no pasó por `resolver_bajo_lock`, y tratarlo como ocioso sería
        // soltar lo que alguien pidió hace un instante.
        let ocioso_desde = v.ultimo_uso.filter(|t| t.elapsed() >= ocioso);
        if ocioso_desde.is_none() {
            return false;
        }
        Self::release_bajo_lock(&mut v).await
    }

    /// El cuerpo de [`Self::release`], con la ventana YA tomada.
    async fn release_bajo_lock(v: &mut Ventana) -> bool {
        let Some(handle) = v.handle.take() else {
            v.ultimo_uso = None;
            return true;
        };
        match Arc::try_unwrap(handle) {
            Ok(j) => {
                // Cerrar de verdad, y esperar a que cierre: soltar el `Arc` y
                // seguir dejaría el lock del fichero puesto un rato indefinido
                // —`sqlx` cierra la conexión en su worker— y el siguiente en
                // abrir se llevaría un `Busy` inventado por nosotros.
                //
                // Con TOPE, y sosteniendo el lock de la ventana mientras tanto:
                // `on_mutation` necesita ese mismo lock, así que un cierre que
                // no volviera dejaría al proceso sin journalizar Y sin mutar,
                // mudo. El plazo convierte el cuelgue en un estado degradado
                // que además se dice.
                if tokio::time::timeout(ESPERA_POR_EL_CIERRE, j.close())
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "el journal no terminó de cerrarse a tiempo: el fichero puede seguir \
                         ocupado un rato más"
                    );
                }
                v.ultimo_uso = None;
                true
            }
            Err(vivo) => {
                v.handle = Some(vivo);
                false
            }
        }
    }

    /// Como [`Self::get`], pero SIN el freno: si ahora mismo no se tiene el
    /// journal, se intenta abrir cueste lo que cueste
    /// (`ESPERA_POR_EL_LOCK`).
    ///
    /// Para el llamante que va a enseñarle la respuesta a un humano y no puede
    /// contestar desde un veredicto de hace medio minuto — hoy
    /// [`crate::Engine::ensure_journal`], que es lo que `norte ai rename`
    /// pregunta ANTES de pedir confirmación. Para una mutación cualquiera el
    /// freno es justo lo que se quiere; aquí es lo que haría mentir a la
    /// pregunta.
    pub async fn acquire_now(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        self.resolve_now().await.ok()
    }

    /// Como [`Self::resolve`], pero SIN el freno — y en UNA sección crítica.
    ///
    /// Que sea una sola importa: soltar el lock para limpiar el veredicto y
    /// volver a tomarlo deja un hueco en el que otra mutación puede fallar y
    /// re-armar el freno, con lo que esto contestaría desde la caché que su
    /// propio contrato promete saltarse.
    ///
    /// # Errors
    /// Las de [`Self::resolve`].
    pub async fn resolve_now(&self) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        let mut v = self.estado.lock().await;
        v.ultimo_fallo = None;
        self.resolver_bajo_lock(&mut v).await
    }

    /// El journal, o el MOTIVO de que no lo haya.
    ///
    /// [`Self::get`] tira el motivo porque a un observer le da igual. Al gate
    /// de mutaciones NO le da igual: `Busy` sigue adelante y `Failed` rehúsa
    /// (#178), y ahí está toda la diferencia entre «hay un daemon vivo» y
    /// «alguien con escritura en el directorio de estado desactivó el
    /// registro».
    ///
    /// Abre si hace falta y si el freno lo permite, exactamente como `get` — y
    /// con su misma advertencia: **el [`Arc`] devuelto no debe sobrevivir a la
    /// operación que lo pidió**, o [`Self::release`] se convierte en un no-op
    /// permanente y silencioso.
    ///
    /// # Errors
    /// [`NoJournal::Busy`] si el lock lo tiene otro proceso;
    /// [`NoJournal::Failed`] con el fichero y el motivo para todo lo demás
    /// (permisos, corrupción, una DB de una era anterior a esta cadena).
    pub async fn resolve(&self) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        let mut v = self.estado.lock().await;
        self.resolver_bajo_lock(&mut v).await
    }

    /// El cuerpo de [`Self::resolve`], con la ventana YA tomada.
    async fn resolver_bajo_lock(
        &self,
        v: &mut Ventana,
    ) -> Result<Arc<crate::journal::SqliteJournal>, NoJournal> {
        if let Some(j) = &v.handle {
            v.ultimo_uso = Some(std::time::Instant::now());
            return Ok(Arc::clone(j));
        }
        // El freno, y SOLO para `Busy`. Lo que el freno ahorra es la espera del
        // lock (`ESPERA_POR_EL_LOCK`), y esa espera solo se paga cuando hay un
        // lock que esperar: un `Failed` —no existe el directorio, no hay
        // permisos, esto no es una base de datos— vuelve en el acto, así que
        // frenarlo no ahorra nada y sí cuesta lo único que importa desde #178,
        // que es CUÁNDO se entera la sesión de que el fichero ya está
        // arreglado. Con el freno puesto, un `chmod` que devuelve los permisos
        // dejaba hasta 30 s de mutaciones rehusadas sin forma de forzar el
        // reintento desde la interfaz; sin él, la siguiente operación funciona.
        // Lo mismo vale para un `Failed` TRANSITORIO (un `EMFILE` en un TUI con
        // muchas conexiones), que es el caso en que 30 s de negativa serían
        // puro daño.
        if let Some((cuando, NoJournal::Busy)) = &v.ultimo_fallo
            && cuando.elapsed() < self.freno
        {
            return Err(NoJournal::Busy);
        }
        self.intentos
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let r = match crate::journal::SqliteJournal::open_with_busy_timeout(
            &self.path,
            ESPERA_POR_EL_LOCK,
        )
        .await
        {
            Ok(j) => Ok(Arc::new(j)),
            Err(e) if es_lock_ocupado(&e) => Err(NoJournal::Busy),
            // El FICHERO va en el motivo, y no solo el error de `SQLite`: desde
            // #178 esto no es un aviso, es lo que hay que arreglar para que la
            // sesión vuelva a poder mutar, y «unable to open database file» sin
            // un path delante no le dice a nadie qué tocar. `Path::display` es
            // la conversión lossy EXPLÍCITA que pide la regla 1 — esto es texto
            // para un humano y nadie lo reparsea.
            Err(e) => Err(NoJournal::Failed(motivo_saneado(
                &e.to_string(),
                &self.path,
            ))),
        };
        match &r {
            Ok(j) => {
                v.handle = Some(Arc::clone(j));
                v.ultimo_fallo = None;
                v.ocupado_desde = None;
                self.anunciar(v, &JournalStatus::Recovered);
            }
            Err(why) => {
                let ahora = std::time::Instant::now();
                v.ultimo_fallo = Some((ahora, why.clone()));
                // El reloj del episodio (#203) es SOLO de `Busy`: un `Failed`
                // no es un ocupante, es un fichero roto, y ya rehúsa la
                // mutación por su cuenta desde #178.
                let sospechoso = match why {
                    NoJournal::Busy => {
                        let desde = *v.ocupado_desde.get_or_insert(ahora);
                        // El `connect` se paga SOLO pasado el plazo: en el caso
                        // normal —un daemon vivo, que es la mayoría de los
                        // `Busy`— este brazo no se toca nunca.
                        desde.elapsed() >= self.sospecha && !self.presencia.any_daemon_listening()
                    }
                    NoJournal::Failed(_) => {
                        v.ocupado_desde = None;
                        false
                    }
                };
                if sospechoso {
                    self.anunciar(v, &JournalStatus::Squatted);
                } else {
                    self.anunciar(v, &JournalStatus::Lost(why.clone()));
                }
            }
        }
        r
    }

    /// Emite una transición SI dice algo nuevo, y recuerda que la dijo.
    ///
    /// Dos filtros, y los dos son la diferencia entre un indicador útil y uno
    /// que se ignora: una pérdida no se repite mientras la CLASE no cambie
    /// (con el freno, eso son dos mensajes por minuto durante horas), y una
    /// recuperación no se emite si no había nada que recuperar — la PRIMERA
    /// apertura es lo normal, no una noticia.
    fn anunciar(&self, v: &mut Ventana, ev: &JournalStatus) {
        if v.anunciado.as_ref().is_some_and(|ya| misma_clase(ya, ev)) {
            return;
        }
        if matches!(ev, JournalStatus::Recovered)
            && !matches!(v.anunciado, Some(JournalStatus::Lost(_)))
        {
            v.anunciado = Some(ev.clone());
            return;
        }
        v.anunciado = Some(ev.clone());
        self.emitir(ev);
    }

    /// Lleva la transición al sink, o al log si todavía no hay sink.
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
    fn emitir(&self, ev: &JournalStatus) {
        let mut slot = self.sink.lock().expect("lock del sink sano");
        match (&slot.sink, ev) {
            (Some(s), JournalStatus::Lost(why)) => s.on_no_journal(why),
            (Some(s), JournalStatus::Recovered) => s.on_journal_recovered(),
            (Some(s), JournalStatus::Squatted) => s.on_journal_squatted(),
            (None, JournalStatus::Lost(why)) => {
                tracing::warn!(motivo = %why.text(), "sesión embebida SIN journal");
                slot.pendiente = Some(why.clone());
            }
            (None, JournalStatus::Recovered) => {
                tracing::info!("la sesión embebida volvió a tener journal");
                slot.pendiente = None;
            }
            // Sin sink, lo pendiente sigue siendo un `Busy`: es lo que un sink
            // tardío tiene que ver, y `on_journal_squatted` cae por omisión en
            // esa misma frase. Lo que sí sube de nivel es el LOG — este es el
            // renglón que un operador busca cuando pregunta por qué no hay
            // registro (#203).
            (None, JournalStatus::Squatted) => {
                tracing::warn!(
                    "el journal lleva minutos ocupado y no hay daemon escuchando:                      alguien retiene `journal.db`"
                );
                slot.pendiente = Some(NoJournal::Busy);
            }
        }
    }
}

/// A mano y no derivado: [`crate::journal::SqliteJournal`] no es `Debug` (lleva
/// dentro el pool de `sqlx`), y lo único que un mensaje de test o de log
/// necesita de este tipo es en qué punto está la decisión.
impl std::fmt::Debug for LazyJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`: formatear no puede esperar a que termine una apertura, y
        // menos aún desde un `Drop` o un log de la propia apertura.
        let estado = match self.estado.try_lock() {
            Err(_) => "abriéndose".to_owned(),
            Ok(v) => match (&v.handle, &v.ultimo_fallo) {
                (Some(_), _) => "dueño".to_owned(),
                (None, Some((_, why))) => format!("sin journal ({why:?})"),
                (None, None) if self.attempted() => "soltado".to_owned(),
                (None, None) => "sin abrir".to_owned(),
            },
        };
        f.debug_struct("LazyJournal")
            .field("path", &self.path)
            .field("estado", &estado)
            .field("intentos", &self.attempts())
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
    /// sea muda, y por eso el aviso está en `LazyJournal::resolve`, que avisa
    /// antes de que este `Ok(())` vuelva — una vez por EPISODIO desde #179, no
    /// una vez por sesión: la ventana se puede perder y recuperar varias veces
    /// y el frontend tiene que enterarse de cada cambio.
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

    /// Fija el veredicto para toda una Task (#205): o el journal, o nada, pero
    /// LO MISMO para todas sus mutaciones.
    ///
    /// Devolver el [`crate::journal::SqliteJournal`] en vez de a sí mismo es lo
    /// que quita del medio a esta ventana durante la Task: el handle ya no se
    /// vuelve a resolver, así que ni el reintento de #179 puede empezar a
    /// registrar por el medio ni un `release` puede dejar de hacerlo. Y como el
    /// `Arc` vive lo que vive la Task, `release` contesta `false` mientras
    /// tanto — que es exactamente la precondición que le falta al temporizador
    /// de ociosidad.
    ///
    /// Sin journal se fija un no-op, y no `self`: si se devolviera `self`, cada
    /// mutación volvería a preguntar y volveríamos a la mitad y mitad.
    async fn pin_for_task(
        &self,
    ) -> Result<Option<Arc<dyn crate::observer::MutationObserver>>, norte_proto::Error> {
        // `resolve` y no `get`, porque el MOTIVO decide (#178) y `get` lo tira.
        // SIN brazo comodín: un motivo nuevo rompe la compilación en vez de
        // colarse por «adelante sin registrar», igual que en el gate.
        match self.resolve().await {
            Ok(j) => Ok(Some(j as Arc<dyn crate::observer::MutationObserver>)),
            // Ocupado: la Task corre SIN registrar, entera. Es el caso benigno
            // y el que no puede tumbar una sesión.
            Err(NoJournal::Busy) => Ok(Some(Arc::new(crate::observer::NoopObserver))),
            // Ilegible: se rehúsa aquí también, y no solo en el gate. Entre el
            // gate y este punto cabe toda la cola del scheduler, y un fichero
            // puede corromperse en ese rato; sin esto, la Task borraría un
            // árbol entero en silencio. Todavía no ha ocurrido ningún efecto.
            Err(NoJournal::Failed(motivo)) => {
                tracing::error!(
                    motivo = %motivo,
                    "Task rehusada al fijar su journal: no se puede abrir (#178)"
                );
                Err(norte_proto::Error::JournalUnavailable)
            }
        }
    }
}

/// La sesión de UI de un proceso SIN daemon (L2).
///
/// El daemon guarda la pantalla en un `SessionStore` y la vuelca a
/// `<state_dir>/session.json`. Un frontend embebido no tiene daemon, así que
/// es su propio almacén: el mismo fichero y el MISMO lock, para que un
/// embebido y un daemon —o dos embebidos— no se pisen la pantalla.
///
/// Uno por proceso, y por eso es un `OnceLock`: dos almacenes vivos serían dos
/// candidatos al mismo lock dentro del mismo proceso, y el segundo se vería
/// suelto por culpa del primero.
///
/// El `state_dir` se resuelve UNA vez, así que un test que llame aquí escribe
/// el estado REAL del usuario y le deja la sesión tomada a la norte que tenga
/// abierta: para aislarlo hay que poner `XDG_STATE_HOME` antes de la primera
/// llamada (el daemon tiene `state_dir` en su config justo por esto). Un
/// cambio de `HOME` a media vida del proceso tampoco se ve.
static UI_SESSION: std::sync::OnceLock<EmbeddedSession> = std::sync::OnceLock::new();

/// El almacén embebido: la sesión viva, dónde se escribe, y el derecho a
/// escribirla.
struct EmbeddedSession {
    /// La sesión viva de ESTE proceso.
    store: Arc<crate::ui_session::SessionStore>,
    /// Dónde vive el estado, si hay dónde. Que se PUEDA escribir es otra cosa
    /// y vive en [`EmbeddedSession::escritura`].
    dir: Option<PathBuf>,
    /// El derecho a escribir, que se puede conseguir MÁS TARDE.
    ///
    /// No es del arranque, y esa era la mitad que faltaba (#234): la ventana
    /// que arranca segunda no tiene el lock, la primera se cierra un rato
    /// después, y con la decisión congelada esta ventana no volvía a escribir
    /// en toda su vida — su pantalla se moría con ella aunque el fichero
    /// estuviera libre desde hace horas.
    escritura: std::sync::Mutex<Escritura>,
    /// Serializa `take_dirty` + volcado.
    ///
    /// El daemon tiene UN escritor que espera cada volcado, así que sus
    /// escrituras salen en orden por construcción. Aquí escribe quien llama, y
    /// dos `session_put` a la vez pueden entrelazarse —A toma la revisión 1, B
    /// toma la 2, B escribe, A escribe— y dejar en disco la VIEJA mientras la
    /// memoria dice la nueva. Hoy solo llama la TUI y va en serie, así que
    /// esto es el cierre de una puerta abierta, no un incendio apagado.
    writing: std::sync::Mutex<()>,
}

/// El estado del derecho a escribir, que se reintenta cada poco.
#[derive(Default)]
struct Escritura {
    /// El derecho, si se tiene.
    lock: Option<crate::ui_session::disk::SessionLock>,
    /// Ya se avisó de que no se puede tomar. Sin esto, una ventana suelta
    /// contra un directorio de estado que no se deja abrir avisa cada treinta
    /// segundos durante todo el día: un indicador que parpadea es un indicador
    /// que nadie mira (#178).
    avisado: bool,
    /// El fichero lo escribió un binario más nuevo, así que este proceso se
    /// RINDE: no se vuelve a intentar.
    ///
    /// Reintentar tenía dos costes y ninguna ventaja: releer hasta un mega cada
    /// treinta segundos para volver a rehusarlo, y un aviso por vuelta. El
    /// arranque siguiente vuelve a mirar, que es cuando la respuesta puede
    /// haber cambiado de verdad.
    rendido: bool,
}

/// El almacén de este proceso, abriéndolo la primera vez.
///
/// Síncrono a propósito: lo llaman los dos envoltorios de abajo desde DENTRO
/// de un `spawn_blocking` (regla 2).
fn ui_session_blocking() -> &'static EmbeddedSession {
    UI_SESSION.get_or_init(|| {
        let Some(dir) = norte_config::dirs::state_dir() else {
            // Sin directorio de estado —un entorno sin `HOME`— hay pantalla
            // viva y no hay dónde guardarla. Es lo que ya pasaba antes de que
            // existiera la sesión, no un fallo nuevo.
            return EmbeddedSession {
                store: Arc::new(crate::ui_session::SessionStore::default()),
                dir: None,
                escritura: std::sync::Mutex::new(Escritura::default()),
                writing: std::sync::Mutex::new(()),
            };
        };
        let lock = toma_el_lock(&dir, true);
        let recuperada = crate::ui_session::disk::load_or_default(&dir);
        EmbeddedSession {
            dir: Some(dir),
            store: Arc::new(crate::ui_session::SessionStore::new(recuperada.session)),
            // El derecho a escribir son DOS cosas: el lock, y que lo que había
            // en disco no sea de un binario más nuevo.
            escritura: std::sync::Mutex::new(Escritura {
                lock: lock.filter(|_| recuperada.writable),
                avisado: false,
                rendido: !recuperada.writable,
            }),
            writing: std::sync::Mutex::new(()),
        }
    })
}

/// Intenta el lock de escritura de `dir`. `avisa` decide si un fallo se cuenta:
/// esto se reintenta cada treinta segundos y un aviso por vuelta es ruido, no
/// información.
fn toma_el_lock(dir: &Path, avisa: bool) -> Option<crate::ui_session::disk::SessionLock> {
    crate::ui_session::disk::lock(dir).unwrap_or_else(|e| {
        if avisa {
            tracing::warn!(error = %e, "no se pudo tomar el lock de la sesión de UI");
        }
        None
    })
}

/// ¿Puede este proceso escribir la sesión AHORA?
///
/// Si ya tiene el lock, sí. Si no, se vuelve a intentar: la ventana que lo
/// tenía puede haberse cerrado (#234). Al conseguirlo tarde se re-lee el
/// fichero por dos cosas distintas y las dos importan:
///
/// - Si mientras corríamos lo escribió un binario MÁS NUEVO, no se pisa: se
///   suelta el lock que acabamos de tomar y se sigue sin escribir.
/// - Si lo escribió otra ventana de esta misma versión, su DOCUMENTO es el
///   vigente y se adopta entero, cuerpo incluido. Quedarse solo con la revisión
///   parecía suficiente y no lo era: el cliente recibía su propio cuerpo con el
///   número del otro, su siguiente escritura encajaba sin conflicto, y lo que
///   la otra ventana hubiera guardado desaparecía sin que nada lo notara. Con
///   el cuerpo delante, el cliente decide qué conserva —él es el único que sabe
///   leerlo— y de paso el número no va hacia atrás.
///
/// Síncrono: lo llaman los dos envoltorios desde dentro de un `spawn_blocking`
/// (regla 2).
fn puede_escribir(s: &EmbeddedSession) -> bool {
    let Some(dir) = s.dir.as_ref() else {
        return false;
    };
    let mut guarda = s
        .escritura
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guarda.lock.is_some() {
        return true;
    }
    if guarda.rendido {
        return false;
    }
    let Some(lock) = toma_el_lock(dir, !guarda.avisado) else {
        guarda.avisado = true;
        return false;
    };
    let recuperada = crate::ui_session::disk::load_or_default(dir);
    if !recuperada.writable {
        // Un binario más nuevo escribió mientras estábamos sueltos: se suelta
        // el lock recién tomado y no se vuelve a intentar en todo el proceso.
        drop(lock);
        guarda.rendido = true;
        return false;
    }
    s.store.adopt_from_disk(recuperada.session);
    tracing::info!("la sesión de UI quedó libre: esta ventana vuelve a guardarla");
    guarda.lock = Some(lock);
    guarda.avisado = false;
    true
}

/// La sesión guardada y si este proceso puede escribirla.
///
/// El `conn` que reclama la propiedad es el mismo para todo el proceso: en
/// embebido no hay conexiones, hay UNA superficie.
pub async fn session_get() -> (norte_proto::methods::Session, bool) {
    tokio::task::spawn_blocking(|| {
        let s = ui_session_blocking();
        let dueño = s.store.claim(0) && puede_escribir(s);
        (s.store.get(), dueño)
    })
    .await
    // Un panic dentro del closure NO es «otra ventana tiene la sesión», que es
    // lo que el frontend pinta con `false`: se dice, y luego se degrada.
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "la lectura de la sesión de UI se cayó");
        (norte_proto::methods::Session::default(), false)
    })
}

/// Reemplaza la sesión y la vuelca, si este proceso es quien escribe.
///
/// Un proceso SUELTO —el que no consiguió el lock— no escribe NI en memoria: se
/// le contesta `PermissionDenied` antes de tocar el almacén. Aceptarlo en
/// memoria y devolver una revisión nueva era prometerle que había guardado algo
/// que no iba a ninguna parte; abrir una segunda ventana sigue sin costarle la
/// pantalla a la primera, que es lo que importaba.
///
/// **Solo para un humano.** El gate `Actor::User` de ADR 0059 lo hace el
/// handler del daemon, y aquí no hay handler: esta función y su hermana son
/// `pub`, así que quien las llame responde de que la superficie que hay
/// detrás es una persona. Hoy no hay ninguna que no lo sea —ni MCP ni los
/// plugins llegan al `Backend` embebido—, y añadir una es añadir ese gate.
///
/// # Errors
///
/// [`norte_proto::Error::PermissionDenied`] si este proceso no es quien
/// escribe (no tiene el lock, o el fichero es de un binario más nuevo),
/// [`norte_proto::Error::Conflict`] con `StaleRevision` y
/// [`norte_proto::Error::LimitExceeded`] con `LIMIT_SESSION_BODY`: las mismas
/// tres negativas que da el daemon, para que el cliente no tenga dos caminos.
pub async fn session_put(
    version: u32,
    revision: u64,
    body: serde_json::Value,
) -> Result<u64, norte_proto::Error> {
    tokio::task::spawn_blocking(move || {
        let s = ui_session_blocking();
        // No ser quien escribe se dice ANTES de tocar la memoria, y con la
        // misma negativa que da el daemon: aceptar el `put` y devolver una
        // revisión nueva le prometería al llamante que ha guardado algo que no
        // va a llegar a ningún sitio. Aquí «dueño» es el lock, porque en
        // embebido hay UNA superficie y no hay conexiones que se disputen nada.
        if !puede_escribir(s) {
            return Err(norte_proto::Error::PermissionDenied);
        }
        let Some(dir) = s.dir.as_ref() else {
            return Err(norte_proto::Error::PermissionDenied);
        };
        let rev = s.store.put(version, revision, body).map_err(|e| match e {
            crate::ui_session::PutError::Conflict { .. } => norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::StaleRevision,
            },
            crate::ui_session::PutError::TooLarge { .. } => norte_proto::Error::LimitExceeded {
                limit: norte_proto::Error::LIMIT_SESSION_BODY.to_owned(),
            },
            // Un almacén embebido no se cierra —lo cierra el apagado de un
            // daemon—, pero nombrarlo aquí es lo que hace que añadir un cierre
            // en este brazo sea un error de compilación y no un silencio.
            crate::ui_session::PutError::Sealed => norte_proto::Error::Cancelled,
            // Mismo trato que en el daemon (#247): un esquema que este core no
            // sabe leer no se escribe, porque escribirlo mata la persistencia
            // desde el arranque siguiente.
            crate::ui_session::PutError::UnknownSchema { .. } => norte_proto::Error::Unsupported,
        })?;
        // `take_dirty` y el volcado, bajo UN lock: es lo que hace que dos
        // escrituras a la vez no dejen en disco la vieja.
        let _turno = s
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sesion) = s.store.take_dirty()
            && let Err(e) = crate::ui_session::disk::write(dir, &sesion)
        {
            // Un volcado fallido no tumba nada, y se vuelve a marcar sucio
            // para que el siguiente `put` lo reintente entero.
            tracing::warn!(error = %e, "no se pudo escribir la sesión de UI; se reintenta");
            s.store.mark_dirty();
        }
        Ok(rev)
    })
    .await
    .unwrap_or(Err(norte_proto::Error::Internal { panic: true }))
}

/// El engine embebido de un frontend: journal perezoso sobre `state_dir`, y la
/// memoria de anclas de UN cliente.
///
/// Es lo que llaman `norte-tui` y `norte-cli` en vez de `Engine::new()`. No
/// abre nada todavía (ver [`LazyJournal`]), así que da igual que el comando
/// acabe mutando o no — que es lo que quitó de en medio la lista de subcomandos
/// que había que mantener a mano (#177).
///
/// **Aquí, y solo aquí, se instala [`crate::Engine::with_client_anchors`]**
/// (#317). El ancla de ADR 0073 dice quién MIRÓ, y eso solo significa algo en
/// un proceso con un cliente: el de un frontend. El engine del daemon se
/// construye por otro camino (`Engine::with_journal`) y por tanto no la tiene,
/// que es lo que impide que el listado de un cliente autorice la escritura de
/// otro.
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
        .with_client_anchors()
}

/// ¿Dicen estas dos transiciones LO MISMO para quien las pinta?
///
/// Por CLASE y no por valor, y la diferencia es una vía de spam: el texto de un
/// [`NoJournal::Failed`] lo escribe en parte quien pueda escribir `journal.db`
/// (`SQLite` interpola identificadores del fichero en su prosa), y desde #178
/// ese motivo ya no lleva freno de reintento — se reabre en cada mutación
/// rehusada. Comparando el `String` entero, un motivo que variase entre
/// intentos daría un aviso por mutación: un indicador que parpadea es un
/// indicador que se ignora, que es justo lo que #178 vino a arreglar.
///
/// Lo que se pierde es poder decir «ahora falla por otra razón», y no importa:
/// el indicador dice lo mismo en las dos («este journal no se puede abrir») y
/// el detalle viaja en el error de cada mutación rehusada.
fn misma_clase(a: &JournalStatus, b: &JournalStatus) -> bool {
    use {JournalStatus as S, NoJournal as N};
    matches!(
        (a, b),
        (S::Recovered, S::Recovered)
            | (S::Lost(N::Busy), S::Lost(N::Busy))
            | (S::Lost(N::Failed(_)), S::Lost(N::Failed(_)))
            // `Squatted` es su propia clase, y por eso SUBE desde un `Busy` ya
            // anunciado en vez de callarse: la sesión lleva media hora viendo
            // «no se está registrando» y lo que cambia ahora es que eso ya no
            // tiene una explicación inocente. Bajar de vuelta a `Busy` sí se
            // calla —un daemon que arranca no es una noticia mejor que la
            // anterior, es la misma— y la única salida hacia arriba de este
            // estado es `Recovered`.
            | (S::Squatted, S::Squatted | S::Lost(N::Busy))
    )
}

/// Tope de la RAZÓN dentro de [`NoJournal::Failed`], en caracteres.
const RAZON_MAX: usize = 160;

/// Tope de la ruta que acompaña a esa razón, en caracteres, contados por la
/// COLA.
///
/// Dos topes y no uno, y el orden importa: con un solo tope sobre
/// `"<ruta>: <razón>"`, un `NORTE_CONFIG_DIR` profundo se come el presupuesto y
/// lo que se corta es la razón — o sea el POR QUÉ, que es lo único que
/// distingue «el directorio no se puede escribir» de «esto no es una base de
/// datos», y son arreglos distintos. Y de la ruta lo que sirve es el final
/// (`…/norte/journal.db`), no el principio.
const RUTA_MAX: usize = 80;

/// El texto de un error de apertura, apto para una terminal y para un log.
///
/// Es «`<ruta>`: `<razón>`», con un presupuesto para cada mitad y las dos
/// saneadas.
///
/// **Quien puede escribir `<state>/journal.db` escribe parte de esta frase.**
/// La prosa de `SQLite` interpola identificadores del propio fichero
/// («malformed database schema (<lo que ponga el atacante>) — …»), y de aquí va
/// a la barra de estado del TUI, al stderr del CLI y al log: un byte de control
/// ahí es una secuencia de escape en el terminal de quien mira. Se quitan los
/// controles y se acota la longitud.
///
/// No sustituye a nada más: quien tiene esa escritura ya se cargó la integridad
/// del journal, y desde #178 esa sesión además no muta. Esto solo impide que la
/// avería se convierta en una inyección en la pantalla del operador.
fn motivo_saneado(razon: &str, ruta: &std::path::Path) -> String {
    format!(
        "{}: {}",
        recorta(&saneado(&ruta.display().to_string()), RUTA_MAX, Cola::Final),
        saneado_y_recortado(razon)
    )
}

/// La razón, saneada y acotada por el principio.
fn saneado_y_recortado(razon: &str) -> String {
    recorta(&saneado(razon), RAZON_MAX, Cola::Principio)
}

/// Por qué punta se recorta.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cola {
    /// Se guarda el principio (una razón se lee de izquierda a derecha).
    Principio,
    /// Se guarda el FINAL (de una ruta lo que identifica es la cola).
    Final,
}

/// Quita lo que un terminal interpretaría en vez de pintar.
///
/// Controles (que son secuencias de escape) y los reordenadores bidi de
/// Unicode: los segundos no son `char::is_control` y reordenan lo que va
/// DESPUÉS de ellos, así que un nombre con un `U+202E` dentro reescribe la
/// frase entera del operador sin cambiar un byte de lo que dice.
fn saneado(bruto: &str) -> String {
    bruto
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Acota a `max` caracteres, marcando con `…` que se cortó.
fn recorta(s: &str, max: usize, cola: Cola) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    match cola {
        Cola::Principio => s.chars().take(max).chain(std::iter::once('…')).collect(),
        Cola::Final => std::iter::once('…')
            .chain(s.chars().skip(n - max))
            .collect(),
    }
}

/// Si este error de apertura es «lo tiene otro», y no un problema de verdad.
///
/// Se mira el CÓDIGO de `SQLite`, no su prosa: `SQLITE_BUSY` (5),
/// `SQLITE_LOCKED` (6) y `SQLITE_PROTOCOL` (15), tomando el byte bajo para que
/// valgan también los extendidos (`SQLITE_BUSY_SNAPSHOT` = 261, …). El texto
/// («database is locked») es de la capa de presentación de `sqlx` y nadie lo
/// garantiza entre versiones; con la clasificación colgando de él, un bump que
/// reformatee `Display` convertiría todos los `Busy` en `Failed` sin que ningún
/// test se enterase.
///
/// El 15 está aquí desde #178 y por culpa de #178: es contención del protocolo
/// de locking de WAL, su remedio documentado es REINTENTAR, y desde que
/// `Failed` rehúsa la mutación, clasificarlo mal ya no cuesta una fila de
/// journal — cuesta una operación negada por una carrera que se resuelve sola.
///
/// Lo que NO entra, y no por olvido: los sabores de lock de `SQLITE_IOERR`
/// (`_LOCK` = 3850, `_BLOCKED` = 2826) y `SQLITE_READONLY_CANTLOCK` (520).
/// Suenan transitorios y no lo son —un `fcntl` que falla sobre NFS, un fichero
/// que de verdad es de solo lectura— y sus PRIMARIOS (10 y 8) son cajones
/// enormes que se llevarían por delante media taxonomía de I/O. Un falso
/// `Failed` cuesta una negativa que el usuario ve y puede reintentar; un falso
/// `Busy` cuesta una mutación sin registrar que nadie ve.
///
/// Deliberadamente ESTRECHO por lo mismo: ensanchar esto a «cualquier error»
/// convertiría una DB corrupta o un directorio sin permisos en un `Busy`
/// silencioso, o sea en una sesión sin registro que el operador creería
/// registrada, justo en la máquina que más lo necesita.
fn es_lock_ocupado(e: &crate::journal::JournalError) -> bool {
    let crate::journal::JournalError::Sqlx(sqlx::Error::Database(db)) = e else {
        return false;
    };
    db.code()
        .and_then(|c| c.parse::<i32>().ok())
        .is_some_and(|c| matches!(c & 0xff, 5 | 6 | 15))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un almacén embebido de laboratorio: sin el `OnceLock` del proceso, que
    /// solo se puede inicializar una vez y apuntaría al estado real.
    fn sesion_en(dir: &Path) -> EmbeddedSession {
        EmbeddedSession {
            store: Arc::new(crate::ui_session::SessionStore::default()),
            dir: Some(dir.to_path_buf()),
            escritura: std::sync::Mutex::new(Escritura::default()),
            writing: std::sync::Mutex::new(()),
        }
    }

    /// **#234**: el derecho a escribir NO es del arranque. La ventana que
    /// arrancó segunda vuelve a intentarlo, y cuando la primera se cierra,
    /// escribe.
    ///
    /// Antes de esto la decisión se congelaba en el `OnceLock` del arranque: la
    /// segunda ventana no guardaba nada en toda su vida aunque el fichero
    /// llevara horas libre, y su pantalla se moría con ella.
    #[test]
    fn el_derecho_a_escribir_se_consigue_mas_tarde() {
        let d = tempfile::tempdir().expect("tmp");
        let s = sesion_en(d.path());
        // Otra ventana lo tiene.
        let primera = crate::ui_session::disk::lock(d.path())
            .expect("lock")
            .expect("libre");
        assert!(!puede_escribir(&s), "con la dueña viva, no se escribe");
        // La dueña se va.
        drop(primera);
        assert!(puede_escribir(&s), "y al irse, esta ventana sí");
        assert!(puede_escribir(&s), "y no lo vuelve a pedir cada vez");
    }

    /// Al conseguirlo tarde se adopta la revisión del FICHERO: la otra ventana
    /// siguió subiéndola mientras estábamos sueltos, y volcar la nuestra tal
    /// cual la renumeraría hacia atrás.
    #[test]
    fn al_tomarlo_tarde_se_adopta_la_revision_del_fichero() {
        let d = tempfile::tempdir().expect("tmp");
        crate::ui_session::disk::write(
            d.path(),
            &norte_proto::methods::Session {
                version: crate::ui_session::disk::SCHEMA_VERSION,
                revision: 42,
                body: serde_json::json!({ "de": "la otra ventana" }),
            },
        )
        .expect("escribe");
        let s = sesion_en(d.path());
        assert_eq!(s.store.get().revision, 0, "la nuestra empieza de cero");
        assert!(puede_escribir(&s));
        assert_eq!(
            s.store.get().revision,
            42,
            "la revisión del fichero no puede ir hacia atrás"
        );
    }

    /// Y si mientras estábamos sueltos escribió un binario MÁS NUEVO, no se
    /// pisa: se suelta el lock recién tomado y se sigue sin escribir.
    #[test]
    fn un_fichero_del_futuro_quita_el_derecho_que_se_acababa_de_tomar() {
        let d = tempfile::tempdir().expect("tmp");
        crate::ui_session::disk::write(
            d.path(),
            &norte_proto::methods::Session {
                version: crate::ui_session::disk::SCHEMA_VERSION + 1,
                revision: 9,
                body: serde_json::json!({ "de": "un binario más nuevo" }),
            },
        )
        .expect("escribe");
        let s = sesion_en(d.path());
        assert!(!puede_escribir(&s));
        // Y el lock queda LIBRE: no se retiene un derecho que no se va a usar.
        assert!(
            crate::ui_session::disk::lock(d.path())
                .expect("lock")
                .is_some(),
            "el lock se soltó"
        );
    }
}
