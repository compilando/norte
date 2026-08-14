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
