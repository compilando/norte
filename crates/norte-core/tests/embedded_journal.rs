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

use norte_core::embedded::{JournalWarningSink, LazyJournal, NoJournal};
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

/// El resultado se decide UNA vez: si el lock era de otro, esta sesión sigue
/// sin registro aunque el otro suelte — y no vuelve a pagar los 250 ms de
/// espera en cada mutación.
#[tokio::test]
async fn el_resultado_se_cachea() {
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
    assert!(
        lazy.get().await.is_none(),
        "la decisión es de la sesión, no de cada mutación"
    );
    // Y se comprueba por el FICHERO, no por el reloj: «tardó menos de X» mide
    // la carga de la máquina tanto como el código, y el margen contra los
    // 250 ms de espera del lock no daba para distinguirlos. Que el journal
    // siga libre dice exactamente lo mismo y no depende de nada.
    SqliteJournal::open(&journal_path(dir.path()))
        .await
        .expect("la sesión no reintentó: el journal sigue de quien lo quiera");
}

/// El motivo que NO es el lock llega como [`NoJournal::Failed`] con su texto —
/// la rama que acaba en la barra de estado de la TUI, y la única que
/// stringifica un error del core para enseñárselo a alguien.
#[tokio::test]
async fn un_journal_que_no_se_puede_abrir_no_es_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Un DIRECTORIO donde va el fichero: `SQLite` no puede abrirlo, y no por
    // culpa de ningún lock.
    std::fs::create_dir(journal_path(dir.path())).expect("ocupar el nombre");

    let (engine, lazy, _mem) = engine_perezoso(dir.path());
    let avisos = Arc::new(Avisos::default());
    lazy.set_warning_sink(Arc::clone(&avisos) as Arc<dyn JournalWarningSink>);

    let h = engine.mkdir(&vp("mem:///d")).await.expect("mkdir");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "un journal ilegible no rompe la sesión embebida (#178)"
    );
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
        Ok(NoJournal::Busy),
        "el aviso llegó al canal"
    );

    let mut sin_journal = norte_core::backend::Backend::Embedded(Arc::new(Engine::new()));
    assert!(
        sin_journal.take_journal_warnings().is_none(),
        "un engine que no journaliza nada no puede prometer avisar de ello"
    );
}
