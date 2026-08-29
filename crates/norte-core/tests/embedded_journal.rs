//! #167/#177: el engine embebido lleva journal, y lo abre en la PRIMERA
//! mutación — no al arrancar.
//!
//! Lo que se pinea aquí es la diferencia entre las dos cosas. Abrirlo al
//! arrancar toma el lock EXCLUSIVO de `SQLite` sobre `journal.db` para toda la
//! vida del proceso, así que un `ntc` NAVEGANDO impedía arrancar al daemon (y
//! con él a `norte mcp serve`) y le negaba la lectura a `norte audit`. Abrirlo
//! en la primera mutación conserva el registro sin conservar el estorbo.

use std::path::Path;
use std::sync::{Arc, Mutex};

use norte_core::embedded::{JournalStatus, JournalWarningSink, LazyJournal, NoJournal};
use norte_core::journal::Actor;
use norte_core::{Engine, SqliteJournal};
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

/// Sink de avisos que los GUARDA: el test necesita contarlos, no verlos.
#[derive(Default)]
struct Avisos(Mutex<Vec<NoJournal>>);

impl JournalWarningSink for Avisos {
    fn on_no_journal(&self, why: &NoJournal) {
        self.0.lock().expect("lock de avisos").push(why.clone());
    }

    /// Este sink solo cuenta pérdidas; las recuperaciones las mira `Estados`.
    fn on_journal_recovered(&self) {}
}

impl Avisos {
    fn vistos(&self) -> Vec<NoJournal> {
        self.0.lock().expect("lock de avisos").clone()
    }
}

/// Un engine embebido sobre `dir` como directorio de estado, con un
/// `MemProvider` registrado para poder mutar algo.
fn engine_perezoso(dir: &Path) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let lazy = Arc::new(LazyJournal::in_state_dir(dir));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// El fichero que se disputa.
fn journal_path(dir: &Path) -> std::path::PathBuf {
    dir.join("journal.db")
}

/// **La regresión de #177.** Una sesión que solo NAVEGA no toca el journal, así
/// que el daemon y el `norte audit` pueden abrirlo mientras ella vive.
///
/// La lista de lecturas no es decorativa: es la superficie por la que un TUI
/// pasa antes de mutar nada, y cada una de ellas resolviendo el journal
/// devolvería el bug. La última —`sync_apply` sin spool— es la más frágil de
/// todas: en `Engine::sync_apply_as` el spool se comprueba ANTES que el
/// journal, y basta invertir esas dos líneas para que abrir el diálogo de
/// sincronización le quite el fichero al daemon.
#[tokio::test]
async fn navegar_no_toma_el_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = engine_perezoso(dir.path());
    mem.mkdir(&vp("mem:///sub")).await.expect("fixture");

    engine.stat(&vp("mem:///")).await.ok();
    engine.list(&vp("mem:///")).await.ok();
    engine.capabilities(&vp("mem:///")).await.ok();
    engine
        .rename_batch_plan(&vp("mem:///"), &[("sub".into(), "otro".into())])
        .await
        .ok();
    // `sync.apply` sin spool tiene que negarse SIN abrir el journal.
    let hash = norte_proto::methods::PlanHash::parse(&"0".repeat(64)).expect("hash");
    assert!(
        matches!(
            engine.sync_apply_as(&hash, 0, Actor::User).await,
            Err(norte_proto::Error::Unsupported)
        ),
        "sin spool no se aplica"
    );

    assert!(!lazy.attempted(), "navegar no abre el journal");

    // Y esto es lo que antes fallaba: el daemon arrancando sobre el mismo
    // directorio de estado, o un `norte audit` leyendo.
    SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el journal sigue libre mientras la sesión solo navega");
}

/// La primera mutación SÍ lo abre, y queda registrada (regla dura 4, #167).
#[tokio::test]
async fn la_primera_mutacion_abre_el_journal_y_deja_fila() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_perezoso(dir.path());

    let h = engine.mkdir(&vp("mem:///nuevo")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    assert!(lazy.attempted(), "la mutación abre el journal");
    let journal = lazy.get().await.expect("esta sesión es la dueña");
    let entries = journal.journal().entries().await.expect("entries");
    assert_eq!(entries.len(), 1, "la mutación dejó su fila: {entries:?}");

    // Y ahora sí es suyo: el segundo en llegar se queda fuera. Con plazo CORTO a
    // propósito: el de por omisión son cinco segundos esperando un lock que este
    // test sabe que nadie va a soltar.
    assert!(
        SqliteJournal::open_with_busy_timeout(
            &journal_path(dir.path()),
            std::time::Duration::from_millis(250)
        )
        .await
        .is_err(),
        "tras mutar, esta sesión es la dueña del fichero"
    );
}

/// El aviso sale CUANDO se necesita el journal, no antes, y UNA sola vez por
/// sesión — y la mutación sigue adelante (hoy, #178).
#[tokio::test]
async fn el_aviso_sale_en_la_primera_mutacion_y_una_sola_vez() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Otro proceso (aquí: otro handle) se lleva el lock ANTES.
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, lazy, _mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    engine.stat(&vp("mem:///")).await.ok();
    assert!(
        avisos.vistos().is_empty(),
        "navegar no puede avisar de nada: el journal ni se ha pedido"
    );

    for n in 0..2 {
        let h = engine
            .mkdir(&vp(&format!("mem:///d{n}")))
            .await
            .expect("mkdir");
        assert_eq!(
            h.join().await,
            TaskState::Completed,
            "sin journal se sigue mutando (#178), no se rompe la sesión"
        );
    }

    assert_eq!(
        avisos.vistos(),
        vec![NoJournal::Busy],
        "un aviso, en la primera mutación, y no uno por mutación"
    );
}

/// Un sink instalado DESPUÉS de que el intento ya haya fallado recibe el aviso
/// igual: si no, una sesión sin registro se quedaría muda por una carrera de
/// arranque, que es justo el fallo que #177 llama «peor que hoy».
#[tokio::test]
async fn un_sink_tardio_recibe_el_aviso_pendiente() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, lazy, _mem) = engine_perezoso(dir.path());
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);
    assert_eq!(
        avisos.vistos(),
        vec![NoJournal::Busy],
        "el aviso pendiente se le entrega al primer sink que aparezca"
    );
}

/// **El nudo de #177.** `undo_session` pregunta si este engine tiene journal
/// ANTES de que haya mutado nada. Con una caché perezosa solo en el observer,
/// contestaría `Unsupported` sobre un engine que abriría el journal sin
/// problema; el accessor perezoso lo abre bajo demanda y contesta la verdad.
#[tokio::test]
async fn el_undo_abre_el_journal_bajo_demanda() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_perezoso(dir.path());

    let (h, _report) = engine
        .undo_session(Actor::User)
        .await
        .expect("un engine perezoso sobre un journal libre SÍ puede deshacer");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.attempted(), "deshacer necesitaba el journal: lo abrió");
}

/// Y si el journal es de otro, `undo_session` dice que no — lo mismo que decía
/// antes: sin cadena no hay nada que revertir.
#[tokio::test]
async fn el_undo_sin_journal_sigue_siendo_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");
    let (engine, _lazy, _mem) = engine_perezoso(dir.path());

    assert!(
        matches!(
            engine.undo_session(Actor::User).await,
            Err(norte_proto::Error::Unsupported)
        ),
        "sin cadena no hay undo"
    );
}

/// El veredicto se RECUERDA entre mutaciones: dentro de la ventana del freno,
/// una sesión que se encontró el journal ocupado no vuelve a pagar la espera
/// del lock por cada mutación (#179 pide el reintento, no el reintento en cada
/// fila). Lo que sí cambia respecto de #177 es que la decisión ya no es para
/// siempre: ver `un_ocupante_de_paso_no_condena_la_sesion`.
#[tokio::test]
async fn el_veredicto_se_recuerda_dentro_del_freno() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");
    let (engine, lazy, _mem) = engine_perezoso(dir.path());

    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.get().await.is_none(), "el lock era de otro");

    drop(dueno);
    let h = engine.mkdir(&vp("mem:///d2")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        lazy.attempts(),
        1,
        "el freno de 30 s no había pasado: ni un intento más"
    );
}

/// El motivo que NO es el lock llega como [`NoJournal::Failed`] con su texto —
/// la rama que acaba en la barra de estado de la TUI, y la única que
/// stringifica un error del core para enseñárselo a alguien.
///
/// Lo que hace la mutación con ese motivo es de #178 y lo pinea
/// `un_journal_ilegible_rehusa_la_mutacion`; lo que se comprueba aquí es la
/// CLASIFICACIÓN, que es de lo que cuelga todo lo demás.
#[tokio::test]
async fn un_journal_que_no_se_puede_abrir_no_es_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Un DIRECTORIO donde va el fichero: `SQLite` no puede abrirlo, y no por
    // culpa de ningún lock.
    std::fs::create_dir(journal_path(dir.path())).expect("ocupar el nombre");

    let (_engine, lazy, _mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    assert!(lazy.get().await.is_none(), "no se pudo abrir");
    match avisos.vistos().as_slice() {
        [NoJournal::Failed(motivo)] => assert!(!motivo.is_empty(), "el motivo se cuenta"),
        otro => panic!("esto no es el lock de nadie: {otro:?}"),
    }
}

/// Dos mutaciones a la vez comparten UN intento de apertura y UN aviso.
///
/// Es la razón de que la celda sea un `OnceCell` y no un `Option` bajo mutex:
/// dos intentos concurrentes serían dos handles del mismo fichero, y el segundo
/// se vería `Busy` contra el lock del PRIMERO — un proceso negándose a
/// journalizar por culpa de sí mismo.
#[tokio::test]
async fn dos_mutaciones_a_la_vez_comparten_el_intento() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, lazy, _mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    let (pa, pb) = (vp("mem:///a"), vp("mem:///b"));
    let (a, b) = tokio::join!(engine.mkdir(&pa), engine.mkdir(&pb));
    assert_eq!(a.expect("mkdir a").join().await, TaskState::Completed);
    assert_eq!(b.expect("mkdir b").join().await, TaskState::Completed);
    assert_eq!(
        avisos.vistos(),
        vec![NoJournal::Busy],
        "un intento, un aviso, aunque las mutaciones vengan a la vez"
    );
}

/// `ensure_journal` es el contrato del que depende `norte ai rename` para poder
/// decir «esto no se va a poder deshacer» ANTES de preguntar.
#[tokio::test]
async fn ensure_journal_lo_abre_y_contesta_la_verdad() {
    let libre = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_perezoso(libre.path());
    assert!(engine.ensure_journal().await, "el fichero estaba libre");
    assert!(lazy.attempted(), "y lo abrió sin que nadie mutara nada");

    let ocupado = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(ocupado.path()))
        .await
        .expect("el primero se lo lleva");
    let (engine, _lazy, _mem) = engine_perezoso(ocupado.path());
    assert!(!engine.ensure_journal().await, "el fichero era de otro");
}

/// El cableado que usa la TUI: el aviso sale del core y llega por el canal del
/// `Backend`, y un engine que NO puede quedarse sin journal no entrega canal
/// (uno que nunca sonaría le haría creerse cubierto).
#[tokio::test]
async fn el_backend_entrega_el_aviso_por_su_canal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, _lazy, _mem) = engine_perezoso(dir.path());
    let mut backend = norte_core::backend::Backend::Embedded(Arc::new(engine));
    let mut rx = backend
        .take_journal_warnings()
        .expect("un engine embebido perezoso sí puede quedarse sin journal");

    let norte_core::backend::Backend::Embedded(engine) = &backend else {
        unreachable!("es el embebido")
    };
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        rx.try_recv(),
        Ok(JournalStatus::Lost(NoJournal::Busy)),
        "el aviso llegó al canal"
    );

    let mut sin_journal = norte_core::backend::Backend::Embedded(Arc::new(Engine::new()));
    assert!(
        sin_journal.take_journal_warnings().is_none(),
        "un engine que no journaliza nada no puede prometer avisar de ello"
    );
}

// ---------------------------------------------------------------------------
// #179: la ventana de propiedad se abre y se cierra más de una vez.
// ---------------------------------------------------------------------------

/// Sink que apunta TODO lo que le llega, pérdidas y recuperaciones.
#[derive(Default)]
struct Estados(Mutex<Vec<JournalStatus>>);

impl JournalWarningSink for Estados {
    fn on_no_journal(&self, why: &NoJournal) {
        self.0
            .lock()
            .expect("lock de estados")
            .push(JournalStatus::Lost(why.clone()));
    }

    fn on_journal_recovered(&self) {
        self.0
            .lock()
            .expect("lock de estados")
            .push(JournalStatus::Recovered);
    }

    fn on_journal_squatted(&self) {
        self.0
            .lock()
            .expect("lock de estados")
            .push(JournalStatus::Squatted);
    }
}

impl Estados {
    fn vistos(&self) -> Vec<JournalStatus> {
        self.0.lock().expect("lock de estados").clone()
    }
}

/// Como [`engine_perezoso`], con el freno de reintento que el test necesite.
fn engine_con_freno(
    dir: &Path,
    freno: std::time::Duration,
) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let lazy = Arc::new(LazyJournal::with_retry_brake(dir, freno));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// **#179.1.** El ocupante de la primera mutación era de paso, y la sesión
/// vuelve a registrar en cuanto suelta: una superposición de un cuarto de
/// segundo dejaba marcada una sesión de tres horas.
#[tokio::test]
async fn un_ocupante_de_paso_no_condena_la_sesion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);
    let avisos = Arc::new(Estados::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.get().await.is_none(), "el lock era de otro");

    // El de paso suelta.
    dueno.close().await;

    let h = engine.mkdir(&vp("mem:///d2")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    let journal = lazy
        .get()
        .await
        .expect("el fichero quedó libre y se reintentó");
    let entries = journal.journal().entries().await.expect("entries");
    assert_eq!(
        entries.len(),
        1,
        "la mutación de después del reintento SÍ quedó registrada: {entries:?}"
    );
    assert_eq!(
        avisos.vistos(),
        vec![
            JournalStatus::Lost(NoJournal::Busy),
            JournalStatus::Recovered
        ],
        "el indicador permanente del frontend tiene que poder apagarse"
    );
}

/// Un probe de presencia de daemon que contesta lo que el test le diga (#203).
struct DaemonDice(bool);

impl norte_core::embedded::DaemonPresence for DaemonDice {
    fn any_daemon_listening(&self) -> bool {
        self.0
    }
}

/// **#203.** Un `Busy` que lleva minutos Y sin daemon escuchando deja de
/// parecerse al caso benigno.
///
/// Es la mitad que el aviso genérico no podía dar: `Busy` sale igual cuando hay
/// un daemon vivo —lo normal— que cuando alguien retiene `journal.db` con un
/// `begin exclusive`, y un aviso que sale siempre no lo mira nadie.
///
/// El plazo se inyecta a cero, así que aquí sube en el PRIMER intento; en
/// producción son cinco minutos y los primeros avisos son los de siempre. Lo
/// que este test fija es el veredicto, no el reloj — el reloj lo fija su
/// gemelo de abajo, y ninguno de los dos duerme.
#[tokio::test]
async fn un_busy_persistente_sin_daemon_se_dice_distinto() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let estados = Arc::new(Estados::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonDice(false)))
            .with_suspicion_delay(std::time::Duration::ZERO),
    );
    lazy.set_warning_sink(Arc::clone(&estados) as Arc<dyn JournalWarningSink>);

    assert!(lazy.resolve().await.is_err());
    assert_eq!(
        estados.vistos(),
        vec![JournalStatus::Squatted],
        "sin daemon y con el plazo cumplido, la frase es la fuerte"
    );

    // Y no se repite: un indicador que parpadea es un indicador que se ignora.
    assert!(lazy.resolve().await.is_err());
    assert!(lazy.resolve().await.is_err());
    assert_eq!(estados.vistos().len(), 1);
}

/// Y ANTES del plazo no sube, por muchos intentos que se hagan: el plazo es lo
/// que separa «un daemon tardando en arrancar» de «alguien retiene tu
/// journal».
///
/// Medido en veredictos y no en reloj — el plazo se pone a una hora, así que
/// ningún intento de este test puede cumplirlo por lento que vaya la máquina.
#[tokio::test]
async fn antes_del_plazo_el_aviso_sigue_siendo_el_de_siempre() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let estados = Arc::new(Estados::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonDice(false)))
            .with_suspicion_delay(std::time::Duration::from_hours(1)),
    );
    lazy.set_warning_sink(Arc::clone(&estados) as Arc<dyn JournalWarningSink>);

    for _ in 0..3 {
        assert!(lazy.resolve().await.is_err());
    }
    assert_eq!(estados.vistos(), vec![JournalStatus::Lost(NoJournal::Busy)]);
}

/// Con un daemon escuchando NO sube, por mucho que dure: ése es el caso
/// benigno, y confundirlo es exactamente el ruido que #203 viene a quitar.
#[tokio::test]
async fn un_busy_con_daemon_vivo_se_queda_en_el_aviso_de_siempre() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let estados = Arc::new(Estados::default());
    let lazy = Arc::new(
        LazyJournal::with_retry_brake(dir.path(), std::time::Duration::ZERO)
            .with_daemon_presence(Arc::new(DaemonDice(true)))
            .with_suspicion_delay(std::time::Duration::ZERO),
    );
    lazy.set_warning_sink(Arc::clone(&estados) as Arc<dyn JournalWarningSink>);

    for _ in 0..3 {
        assert!(lazy.resolve().await.is_err());
    }
    assert_eq!(
        estados.vistos(),
        vec![JournalStatus::Lost(NoJournal::Busy)],
        "hay un daemon: es el caso corriente y se dice una vez"
    );
}

/// **#179, el freno.** Reintentar no puede costar `ESPERA_POR_EL_LOCK` por
/// mutación. Se mide en INTENTOS, no en reloj: «tardó menos de X» mide la carga
/// de la máquina tanto como el código.
#[tokio::test]
async fn el_reintento_lleva_freno() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::from_hours(1));

    for n in 0..3 {
        let h = engine
            .mkdir(&vp(&format!("mem:///d{n}")))
            .await
            .expect("mkdir");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    assert_eq!(
        lazy.attempts(),
        1,
        "dentro de la ventana del freno no se vuelve a pagar la espera del lock"
    );
}

/// **La trampa del `ChainState`, y la razón de que esto no sea pequeño.**
///
/// Reabrir un journal que este proceso YA TUVO obliga a releer `last_seq` y
/// `last_hash` del fichero. Con el par viejo, el insert choca contra la PK de
/// `seq` — y como `last_seq` solo avanza al acertar, fallan TODAS las
/// mutaciones siguientes: efecto aplicado sin fila, en bucle.
#[tokio::test]
async fn reabrir_relee_la_cadena_del_fichero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);

    let h = engine.mkdir(&vp("mem:///uno")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(lazy.release().await, "nadie más tiene el handle");

    // OTRO escritor avanza la cadena mientras esta sesión no la tiene.
    {
        let otro = SqliteJournal::open(&journal_path(dir.path()))
            .await
            .expect("soltado de verdad: el fichero está libre");
        otro.journal()
            .record(
                "created",
                b"mem:///de-otro",
                None,
                norte_core::journal::Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("la fila del otro");
        otro.close().await;
    }

    let h = engine.mkdir(&vp("mem:///dos")).await.expect("mkdir");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "la mutación de después de reabrir NO puede chocar con la PK de seq"
    );

    let journal = lazy.get().await.expect("reabierto");
    let entries = journal.journal().entries().await.expect("entries");
    let seqs: Vec<i64> = entries.iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        vec![1, 2, 3],
        "la cadena sigue al OTRO escritor: {entries:?}"
    );
    assert_eq!(
        entries[2].path.as_slice(),
        b"mem:///dos",
        "y la última es la nuestra: {entries:?}"
    );
}

/// Dos mutaciones a la vez sobre un journal LIBRE abren UN handle, no dos: el
/// segundo se vería `Busy` contra el lock del primero — un proceso negándose a
/// journalizar por culpa de sí mismo.
#[tokio::test]
async fn dos_mutaciones_a_la_vez_abren_un_solo_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);
    let avisos = Arc::new(Estados::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    let (pa, pb) = (vp("mem:///a"), vp("mem:///b"));
    let (a, b) = tokio::join!(engine.mkdir(&pa), engine.mkdir(&pb));
    assert_eq!(a.expect("mkdir a").join().await, TaskState::Completed);
    assert_eq!(b.expect("mkdir b").join().await, TaskState::Completed);

    assert_eq!(lazy.attempts(), 1, "un intento, no uno por mutación");
    assert!(
        avisos.vistos().is_empty(),
        "nada que avisar: se abrió a la primera"
    );
    let journal = lazy.get().await.expect("dueña");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        2,
        "las dos mutaciones quedaron registradas"
    );
}

/// Soltar con alguien más sosteniendo el handle NO suelta: abrir un segundo
/// handle sobre el mismo fichero sería este proceso quitándose el journal a sí
/// mismo.
/// #179, la política: se suelta cuando lleva un rato SIN USARSE, y no antes.
///
/// El proceso tomaba el journal en la primera mutación y no lo devolvía hasta
/// salir: una copia a las 09:00 dejaba a `norte daemon run` y a `norte audit`
/// sin poder abrir el fichero en todo el día.
#[tokio::test]
async fn soltar_por_ocioso_espera_a_que_lo_este() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);

    let h = engine.mkdir(&vp("mem:///uno")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    // Recién usado: no se suelta, y sigue siendo nuestro.
    assert!(
        !lazy
            .release_if_idle(std::time::Duration::from_mins(1))
            .await,
        "acaba de usarse: soltarlo sería soltar lo que alguien pidió hace un instante"
    );
    assert!(
        SqliteJournal::open(&journal_path(dir.path()))
            .await
            .is_err(),
        "y el fichero sigue ocupado por esta sesión"
    );

    // Con el umbral a cero, lo está por definición.
    assert!(lazy.release_if_idle(std::time::Duration::ZERO).await);
    {
        let otro = SqliteJournal::open(&journal_path(dir.path()))
            .await
            .expect("soltado de verdad: el fichero está libre");
        otro.close().await;
    }

    // Y la ventana se REABRE sola en la siguiente mutación, releyendo la
    // cadena — que es lo que hace que soltar sea seguro.
    let h = engine.mkdir(&vp("mem:///dos")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        !lazy
            .release_if_idle(std::time::Duration::from_mins(1))
            .await,
        "vuelve a ser nuestro"
    );
}

/// Sin haberlo tenido nunca, «soltar por ocioso» contesta que el fichero está
/// libre: no hay nada que soltar, y decir `false` haría que el llamante
/// creyera que lo tiene.
#[tokio::test]
async fn soltar_por_ocioso_sin_haberlo_tomado_es_cierto() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);
    assert!(lazy.release_if_idle(std::time::Duration::ZERO).await);
}

#[tokio::test]
async fn soltar_con_el_handle_prestado_no_suelta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::ZERO);
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    let prestado = lazy.get().await.expect("dueña");
    assert!(!lazy.release().await, "hay un Arc vivo por ahí");
    drop(prestado);
    assert!(lazy.release().await, "ya no");
}

/// El motivo de un journal ilegible pasa por un saneador antes de llegar a una
/// terminal: quien puede escribir el fichero escribe parte de esa frase, y la
/// prosa de `SQLite` interpola identificadores del propio fichero.
#[tokio::test]
async fn el_motivo_de_un_journal_roto_no_lleva_controles_a_la_pantalla() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Un fichero que no es una base de datos, con un identificador hostil
    // dentro: `SQLite` lo devolverá en su mensaje.
    std::fs::write(
        journal_path(dir.path()),
        b"no soy sqlite \x1b[31m\x07 \x1b]0;pwned\x07",
    )
    .expect("fixture");

    let (_engine, lazy, _mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);
    assert!(lazy.get().await.is_none(), "no es una base de datos");

    match avisos.vistos().as_slice() {
        [NoJournal::Failed(motivo)] => {
            assert!(
                !motivo.chars().any(char::is_control),
                "ni un byte de control llega a la barra de estado: {motivo:?}"
            );
            assert!(motivo.chars().count() <= 201, "acotado: {}", motivo.len());
        }
        otro => panic!("no es el lock de nadie: {otro:?}"),
    }
}

/// `ensure_journal` se salta el freno: es lo que `norte ai rename` pregunta
/// ANTES de pedir confirmación, y contestar desde un veredicto de hace medio
/// minuto le diría al humano «esto no se registra» sobre un fichero libre.
#[tokio::test]
async fn ensure_journal_no_contesta_desde_el_freno() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");
    // Freno LARGO: una mutación normal no reintentaría en toda la sesión.
    let (engine, lazy, _mem) = engine_con_freno(dir.path(), std::time::Duration::from_hours(1));

    assert!(!engine.ensure_journal().await, "el fichero era de otro");
    dueno.close().await;
    assert!(
        engine.ensure_journal().await,
        "quedó libre: la pregunta que ve un humano no se contesta desde la caché"
    );
    assert_eq!(
        lazy.attempts(),
        2,
        "y para eso hubo que intentarlo otra vez"
    );
}

// ---------------------------------------------------------------------------
// #178: un journal ilegible falla en CERRADO; uno ocupado, no.
// ---------------------------------------------------------------------------

/// **#178.** Un `journal.db` que no es una base de datos —lo que deja cualquiera
/// con escritura en el directorio de estado— REHÚSA la mutación, con su
/// categoría propia y sin tocar nada.
///
/// Antes seguía adelante detrás de un aviso, o sea que corromper un fichero
/// desactivaba el registro de TODAS las sesiones embebidas —incluido el de
/// `norte ai rename --yes`, que es el que más falta hace— en silencio y para
/// siempre, mientras `norte daemon run` con esa misma entrada se niega a
/// arrancar. La asimetría era el bug.
#[tokio::test]
async fn un_journal_ilegible_rehusa_la_mutacion() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Un DIRECTORIO donde va el fichero: `SQLite` no puede abrirlo, y no por
    // culpa de ningún lock.
    std::fs::create_dir(journal_path(dir.path())).expect("ocupar el nombre");

    let (engine, lazy, mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Estados::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    assert!(
        matches!(
            engine.mkdir(&vp("mem:///d")).await,
            Err(norte_proto::Error::JournalUnavailable)
        ),
        "un journal ilegible para la mutación con su categoría"
    );
    assert!(
        mem.stat(&vp("mem:///d")).await.is_err(),
        "y no se tocó nada: la negativa es PREVIA al efecto"
    );
    // El motivo, con el fichero nombrado, sigue llegando por el canal de avisos
    // — que es in-process y sí puede llevar rutas.
    match avisos.vistos().as_slice() {
        [JournalStatus::Lost(NoJournal::Failed(motivo))] => {
            assert!(
                motivo.contains("journal.db"),
                "el fichero se nombra: {motivo}"
            );
        }
        otro => panic!("esto no es el lock de nadie: {otro:?}"),
    }
}

/// Y su gemela, que es la que impide que el arreglo sea peor que el agujero: un
/// journal OCUPADO deja seguir.
///
/// El ocupante habitual es benigno —un daemon vivo, otra ventana— y rehusar
/// convertiría «hay un daemon» en «el gestor de ficheros no funciona». Un
/// transitorio (otro `norte cp` de un script, un daemon reiniciándose) no puede
/// tumbar una sesión de tres horas.
#[tokio::test]
async fn un_journal_ocupado_deja_seguir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let (engine, _lazy, mem) = engine_perezoso(dir.path());
    let h = engine
        .mkdir(&vp("mem:///d"))
        .await
        .expect("ocupado NO rehúsa");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d")).await.is_ok(),
        "la mutación ocurrió"
    );
}

/// El engine SIN journal por construcción (`Engine::new()`, un embebedor de la
/// biblioteca) no queda atrapado en la negativa de #178: no tiene journal
/// perezoso, así que no hay fichero que arreglar y nunca hubo registro que
/// perder.
#[tokio::test]
async fn un_engine_sin_journal_perezoso_no_lo_echa_de_menos() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
}

/// **La afirmación entera de #178, y lo único que la sostiene.** TODO punto de
/// mutación del engine pasa por el gate del journal.
///
/// Sin esto, la cobertura la daba un solo `mkdir`: un `Engine::hardlink_as`
/// futuro que se olvidara del gate no rompería ningún test y reabriría el
/// agujero por la puerta nueva, en silencio. La lista es la de los ocho
/// llamadores de `gate`, y crece con ellos.
#[tokio::test]
async fn toda_mutacion_pasa_por_el_gate_del_journal() {
    use norte_proto::Error as E;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(journal_path(dir.path())).expect("ocupar el nombre con un directorio");
    let (engine, _lazy, mem) = engine_perezoso(dir.path());
    // Un árbol con algo que copiar, mover, renombrar y borrar.
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    {
        let mut sink = mem.write(&vp("mem:///d/a.txt")).await.expect("write");
        sink.write(bytes::Bytes::from_static(b"vivo"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    let (from, to) = (vp("mem:///d/a.txt"), vp("mem:///d/b.txt"));
    // El plan de renombrados se pide de verdad: `rename_batch` compara el hash
    // ANTES del gate, así que uno inventado saldría por `PlanStale` y no
    // probaría nada del journal. (Que el orden sea ese no es un problema:
    // comparar no toca el árbol.)
    let pares: Vec<(Vec<u8>, Vec<u8>)> = vec![(b"a.txt".to_vec(), b"c.txt".to_vec())];
    let plan = engine
        .rename_batch_plan(&vp("mem:///d"), &pares)
        .await
        .expect("planificar es LEER: no pasa por el gate del journal");
    let rehusado: Vec<(&str, Result<(), norte_proto::Error>)> = vec![
        ("copy", engine.copy(&from, &to).await.map(|_| ())),
        ("move", engine.move_(&from, &to).await.map(|_| ())),
        ("mkdir", engine.mkdir(&vp("mem:///nuevo")).await.map(|_| ())),
        (
            "delete",
            engine.delete(&vp("mem:///d/a.txt")).await.map(|_| ()),
        ),
        (
            "rename_batch",
            engine
                .rename_batch(&vp("mem:///d"), &pares, plan.hash())
                .await
                .map(|_| ()),
        ),
        (
            "undo_session",
            engine.undo_session(Actor::User).await.map(|_| ()),
        ),
        // #314: la novena. El pin existe justo para que la que llega no se
        // olvide, y esta llegó — así que aquí está.
        (
            "set_mode",
            engine
                .set_mode(norte_proto::methods::FsSetModeParams {
                    paths: vec![vp("mem:///d/a.txt")],
                    mode: 0o600,
                })
                .await
                .map(|_| ()),
        ),
    ];
    for (nombre, r) in rehusado {
        assert!(
            matches!(r, Err(E::JournalUnavailable)),
            "{nombre} tiene que pasar por el gate del journal: {r:?}"
        );
    }

    // Y nada de eso tocó el árbol: la negativa es PREVIA al efecto.
    assert!(
        mem.stat(&from).await.is_ok(),
        "el fichero sigue donde estaba"
    );
    assert!(mem.stat(&to).await.is_err(), "no se creó el destino");
    assert!(
        mem.stat(&vp("mem:///nuevo")).await.is_err(),
        "no se creó el directorio"
    );
}

// ---------------------------------------------------------------------------
// #205: una operación queda ENTERA dentro del journal, o entera fuera.
// ---------------------------------------------------------------------------

/// Un `MemProvider` que SUELTA el journal en cuanto borra el primer nodo.
///
/// Es el reintento de #179 disparándose a mitad de una operación larga, sin
/// carreras: el ocupante deja el fichero justo entre la primera entrada y la
/// segunda, que es la ventana exacta en la que una Task podía empezar a
/// registrar por el medio.
struct SueltaElJournalAlBorrar {
    inner: Arc<MemProvider>,
    dueno: tokio::sync::Mutex<Option<SqliteJournal>>,
    /// Borrados vistos. Se suelta en el SEGUNDO, no en el primero, y ahí está
    /// la gracia: la fila de la primera entrada ya se intentó (y no llegó, el
    /// fichero era de otro), así que lo que queda es una operación con la
    /// cabeza sin registrar y la cola registrada — la mitad y mitad exacta que
    /// #205 describe, no un cambio de veredicto antes de empezar.
    vistos: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl Provider for SueltaElJournalAlBorrar {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await?;
        if self
            .vistos
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            == 1
            && let Some(j) = self.dueno.lock().await.take()
        {
            // De verdad, y esperando: soltar sin cerrar dejaría el lock puesto
            // un rato indefinido y el test dependería del reloj.
            j.close().await;
        }
        Ok(())
    }
}

/// **#205.** Una operación que empieza SIN journal se queda sin journal entera,
/// aunque el fichero se libere a mitad.
///
/// Sin fijar el veredicto, `ops` lo preguntaba por MUTACIÓN: la primera entrada
/// no dejaba fila, el ocupante soltaba, y las siguientes sí — media operación
/// registrada dentro de UNA Task y UN actor. `undo_session` desanda entonces la
/// cola registrada y deja la cabeza que no lo está, sin poder nombrar lo que se
/// dejó, porque de eso no hay filas.
///
/// «No quedó registrado» se arregla a mano; «quedó registrado a medias» es una
/// trampa, y la abrió el reintento de #179.
#[tokio::test]
async fn una_operacion_no_queda_registrada_a_medias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    // Freno CERO: sin fijar el veredicto, la segunda entrada reintentaría y
    // encontraría el fichero libre. Es lo que hace al test discriminante.
    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir.path(),
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let provider = Arc::new(SueltaElJournalAlBorrar {
        inner: Arc::clone(&mem),
        dueno: tokio::sync::Mutex::new(Some(dueno)),
        vistos: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);

    // Un borrado permanente del árbol: cuatro entradas, una mutación cada una.
    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d")).await.is_err(),
        "el árbol se borró entero: el efecto no depende del journal"
    );

    // Y el fichero quedó libre a mitad, así que ahora esta sesión sí lo abre.
    let journal = lazy.get().await.expect("el ocupante lo soltó");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        0,
        "la operación empezó sin journal: NINGUNA de sus entradas quedó \
         registrada, ni siquiera las de después de que el fichero se liberara. \
         Sin fijar el veredicto son 3 de 4 — cabeza sin registrar, cola \
         registrada — que es la operación que el undo deshace a medias"
    );
}

/// Y la otra mitad de la misma propiedad: una operación que empieza CON journal
/// registra todas sus entradas.
///
/// Las dos juntas son «entera dentro o entera fuera». Sin esta, fijar el
/// veredicto en `NoopObserver` para todo pasaría el test de arriba.
#[tokio::test]
async fn una_operacion_que_empieza_con_journal_lo_registra_todo() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = engine_perezoso(dir.path());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let journal = lazy.get().await.expect("dueña");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        4,
        "tres ficheros y su directorio: la operación entera"
    );
}

/// Un provider que intenta SOLTAR el journal justo cuando la Task está
/// mutando, y apunta lo que le contestaron.
///
/// El instante importa y por eso se pregunta desde aquí dentro: entre que el
/// engine despacha la Task y que su cuerpo fija el veredicto no hay nadie
/// sosteniendo el handle, y soltar AHÍ es inofensivo (todavía no hay efecto, y
/// el `pin` lo vuelve a abrir). La ventana que importa es la otra, la que va
/// del `pin` a la última fila, y solo se alcanza desde dentro del efecto.
struct SueltaMientrasMuta {
    inner: Arc<MemProvider>,
    lazy: Arc<LazyJournal>,
    solto: Mutex<Option<bool>>,
}

#[async_trait::async_trait]
impl Provider for SueltaMientrasMuta {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        // El efecto ya está a punto de ocurrir y su fila todavía no existe:
        // ESTA es la ventana.
        let r = self.lazy.release().await;
        *self.solto.lock().expect("lock") = Some(r);
        self.inner.mkdir(p).await
    }
}

/// El handle fijado se sostiene TODA la Task, así que `release` no puede cerrar
/// la ventana entre el gate de una mutación y su fila.
///
/// Es la precondición que le falta al temporizador de ociosidad de #179, y la
/// mitad de #205 que no es sobre el undo: pinchar el handle al principio la da
/// gratis, y sin ella `release` desde otro hilo dejaría un efecto sin fila y
/// sin error.
#[tokio::test]
async fn mientras_una_task_muta_el_journal_no_se_puede_soltar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = Arc::new(LazyJournal::in_state_dir(dir.path()));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    let provider = Arc::new(SueltaMientrasMuta {
        inner: Arc::clone(&mem),
        lazy: Arc::clone(&lazy),
        solto: Mutex::new(None),
    });
    engine.register_provider(Arc::clone(&provider) as Arc<dyn Provider>);

    let h = engine.mkdir(&vp("mem:///nuevo")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    assert_eq!(
        *provider.solto.lock().expect("lock"),
        Some(false),
        "con la Task a medio mutar, soltar el journal tiene que NEGARSE: \
         cerrarlo ahí dejaría este efecto sin fila y sin error"
    );
    let journal = lazy.get().await.expect("dueña");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        1,
        "y la fila llegó"
    );
}

/// Un `MemProvider` que suelta el journal tras el SEGUNDO renombrado.
///
/// El gemelo de [`SueltaElJournalAlBorrar`] para el otro camino que fija su
/// veredicto fuera de `ops`: el lote de renombrados, que cuando arranca sin
/// journal se registra por el observer crudo.
struct SueltaElJournalAlRenombrar {
    inner: Arc<MemProvider>,
    dueno: tokio::sync::Mutex<Option<SqliteJournal>>,
    vistos: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl Provider for SueltaElJournalAlRenombrar {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await?;
        if self
            .vistos
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            == 1
            && let Some(j) = self.dueno.lock().await.take()
        {
            j.close().await;
        }
        Ok(())
    }
}

/// **#205 en el lote de renombrados**, que fija su veredicto en `engine` y no
/// en `ops`, y por tanto tenía la misma grieta por su cuenta.
///
/// Cuando `rename_batch_as` no encuentra journal al empezar, registra por el
/// observer crudo. Sin fijarlo, cada paso volvía a preguntar — y las filas que
/// llegaran a mitad irían ADEMÁS sin `batch_id`, o sea que el lote que el wire
/// anuncia como una unidad deshacible quedaría medio registrado y sin agrupar.
#[tokio::test]
async fn un_lote_de_renombrados_tampoco_queda_registrado_a_medias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dueno = SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("el primero se lo lleva");

    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir.path(),
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    mem.mkdir(&vp("mem:///d")).await.expect("fixture");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("mem:///d/a{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let provider = Arc::new(SueltaElJournalAlRenombrar {
        inner: Arc::clone(&mem),
        dueno: tokio::sync::Mutex::new(Some(dueno)),
        vistos: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);

    let pares: Vec<(Vec<u8>, Vec<u8>)> = (0..3)
        .map(|n| {
            (
                format!("a{n}.txt").into_bytes(),
                format!("b{n}.txt").into_bytes(),
            )
        })
        .collect();
    let plan = engine
        .rename_batch_plan(&vp("mem:///d"), &pares)
        .await
        .expect("plan");
    let (h, _report) = engine
        .rename_batch(&vp("mem:///d"), &pares, plan.hash())
        .await
        .expect("rename_batch");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(
        mem.stat(&vp("mem:///d/b0.txt")).await.is_ok(),
        "los renombrados ocurrieron"
    );

    let journal = lazy.get().await.expect("el ocupante lo soltó a mitad");
    assert_eq!(
        journal.journal().count().await.expect("count"),
        0,
        "el lote empezó sin journal: NINGUNO de sus pasos quedó registrado, y \
         desde luego no unos sí y otros no"
    );
}

/// Arma un engine cuyo journal lo tiene otro, y que lo suelta en cuanto el
/// provider ve su primera mutación.
async fn escenario_que_suelta(dir: &Path) -> (Engine, Arc<LazyJournal>, Arc<MemProvider>) {
    let dueno = SqliteJournal::open(&journal_path(dir))
        .await
        .expect("el primero se lo lleva");
    let lazy = Arc::new(LazyJournal::with_retry_brake(
        dir,
        std::time::Duration::ZERO,
    ));
    let engine = Engine::with_lazy_journal(Arc::clone(&lazy));
    let mem = Arc::new(MemProvider::new());
    let provider = Arc::new(SueltaAlMutar {
        inner: Arc::clone(&mem),
        dueno: tokio::sync::Mutex::new(Some(dueno)),
        vistos: std::sync::atomic::AtomicU64::new(0),
    });
    engine.register_provider(provider as Arc<dyn Provider>);
    (engine, lazy, mem)
}

/// Un directorio con tres ficheros, para que la operación tenga entradas que
/// partir.
async fn arbolito(mem: &Arc<MemProvider>, raiz: &str) {
    mem.mkdir(&vp(raiz)).await.expect("mkdir");
    for n in 0..3 {
        let mut sink = mem
            .write(&vp(&format!("{raiz}/f{n}.txt")))
            .await
            .expect("write");
        sink.write(bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
}

/// Cuántas filas tiene el journal de `lazy`, que a estas alturas está libre.
async fn filas(lazy: &Arc<LazyJournal>) -> i64 {
    lazy.get()
        .await
        .expect("el ocupante lo soltó")
        .journal()
        .count()
        .await
        .expect("count")
}

/// **La regla entera de #205, y lo único que la sostiene.** TODA Task que muta
/// fija su veredicto: empieza sin journal → termina sin journal, entera.
///
/// El gemelo de `toda_mutacion_pasa_por_el_gate_del_journal` para #205, y por
/// el mismo motivo: sin él la regla vive en un comentario, y el día que alguien
/// añada un `Engine::hardlink_as` que se olvide de fijar, ningún test se entera
/// — la operación empezará a registrarse por el medio y el undo la deshará a
/// medias, en silencio.
///
/// Cada caso corre en su propio directorio de estado: lo que se afirma es que
/// el journal quedó VACÍO, y compartirlo haría que el de al lado lo llenara.
#[tokio::test]
async fn toda_task_que_muta_fija_su_veredicto() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = escenario_que_suelta(dir.path()).await;
    arbolito(&mem, "mem:///src").await;
    let h = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(filas(&lazy).await, 0, "copy_tree fija su veredicto");

    // Dentro del MISMO provider un move es UN rename, así que también prueba
    // el camino de `rename_with_policy`, que es el otro que `move_task`
    // delega.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = escenario_que_suelta(dir.path()).await;
    arbolito(&mem, "mem:///src").await;
    let h = engine
        .move_(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(filas(&lazy).await, 0, "move fija su veredicto");

    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = escenario_que_suelta(dir.path()).await;
    arbolito(&mem, "mem:///d").await;
    let h = engine
        .delete_with(&vp("mem:///d"), norte_proto::DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(filas(&lazy).await, 0, "delete fija su veredicto");

    // Una sola mutación, así que no hay mitad que partir — pero el veredicto
    // tiene que ser el del principio igual.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, _mem) = escenario_que_suelta(dir.path()).await;
    let h = engine.mkdir(&vp("mem:///nuevo")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(filas(&lazy).await, 0, "mkdir fija su veredicto");

    // #314: un lote de permisos son VARIAS mutaciones seguidas, que es el caso
    // que este test existe para cubrir — el observador se fija una vez, antes
    // de la primera, y no se vuelve a preguntar por el camino.
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, lazy, mem) = escenario_que_suelta(dir.path()).await;
    arbolito(&mem, "mem:///d").await;
    let h = engine
        .set_mode(norte_proto::methods::FsSetModeParams {
            paths: vec![vp("mem:///d/f0.txt"), vp("mem:///d/f1.txt")],
            mode: 0o600,
        })
        .await
        .expect("set_mode");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(filas(&lazy).await, 0, "set_mode fija su veredicto");
}

/// El provider de [`toda_task_que_muta_fija_su_veredicto`]: suelta el journal
/// en cuanto ve su SEGUNDA mutación, sea del tipo que sea.
struct SueltaAlMutar {
    inner: Arc<MemProvider>,
    dueno: tokio::sync::Mutex<Option<SqliteJournal>>,
    vistos: std::sync::atomic::AtomicU64,
}

impl SueltaAlMutar {
    /// Suelta en la PRIMERA mutación, no en la segunda.
    ///
    /// Aquí lo que se comprueba es que el veredicto de la Task no cambia, no
    /// dónde cae el corte — de eso se ocupa
    /// `una_operacion_no_queda_registrada_a_medias`, con su 3-de-4. Y hace
    /// falta que sea la primera: un `move` dentro del mismo provider es UN
    /// rename, así que esperando a la segunda no se soltaría nunca y el caso
    /// pasaría sin probar nada.
    async fn quizas_soltar(&self) {
        if self
            .vistos
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            == 0
            && let Some(j) = self.dueno.lock().await.take()
        {
            j.close().await;
        }
    }
}

#[async_trait::async_trait]
impl Provider for SueltaAlMutar {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
        let sink = self.inner.write(p).await?;
        self.quizas_soltar().await;
        Ok(sink)
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.mkdir(p).await?;
        self.quizas_soltar().await;
        Ok(())
    }
    async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.remove(p).await?;
        self.quizas_soltar().await;
        Ok(())
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
        self.inner.rename(from, to).await?;
        self.quizas_soltar().await;
        Ok(())
    }
    // #314: cambiar permisos es una mutación más, y sin reenviarla este doble
    // respondía `Unsupported` por el default del trait — la Task terminaba
    // «bien» sin haber mutado nada y sin soltar el journal, que es justo lo
    // contrario de lo que este test comprueba.
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), norte_proto::Error> {
        self.inner.set_mode(p, mode).await?;
        self.quizas_soltar().await;
        Ok(())
    }
    // Para que el modo ANTERIOR se pueda leer: el default del trait tira las
    // opciones y con ellas el `posix.mode` que la reversa necesita.
    async fn stat_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<norte_proto::Entry, norte_proto::Error> {
        self.inner.stat_with(p, opt).await
    }
}
