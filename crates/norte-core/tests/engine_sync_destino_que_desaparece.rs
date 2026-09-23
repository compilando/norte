//! **Qué pasa si el destino de una SINCRONIZACIÓN desaparece a mitad** (#368).
//!
//! El gemelo de `engine_destino_que_desaparece`, y el mismo mecanismo exacto:
//! `sync.apply` abre su raíz de destino UNA vez por tarea (#164) y escribe por
//! ese descriptor. Borrar en norte es mover a la papelera, o sea un `rename`,
//! y un `rename` no invalida un descriptor: el directorio sigue vivo con su
//! mismo inodo en otro sitio, así que la sincronización seguía llenándolo y el
//! informe decía que fue bien.
//!
//! **Aquí importa más que en una copia**, y ésa es la razón de que la deuda no
//! se dejara para después: una sincronización es justamente la operación que
//! se pone en marcha contra un destino que nadie está mirando.
//!
//! Va contra el sistema de ficheros REAL y no contra `MemProvider` por lo
//! mismo que su gemelo: lo que hace posible el fallo es un descriptor, y
//! `MemProvider` no tiene.
//!
//! Los dos casos hacen falta y no son el mismo. Uno deja la ruta VACÍA, que se
//! detecta porque no resuelve; el otro deja OTRO directorio en su sitio, que
//! resuelve perfectamente y solo lo caza la comparación de identidad.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::journal::{Journal, SqliteJournal};
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    OnUnknown, PlanHash, SyncCompareOptions, SyncMode, SyncPlanParams, SyncReportResult,
};
use norte_proto::{ConflictKind, Error, TaskState, VPath};
use norte_vfs::Provider;

mod origen_a_peticion;
use origen_a_peticion::OrigenAPeticion;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Cuántos ficheros lleva el origen.
///
/// Bastantes para que la sincronización siga viva cuando el test interviene.
/// No es un plazo disfrazado: no se espera un tiempo, se espera a VER que ya
/// aterrizó algo, y detrás de ese momento queda trabajo.
const FICHEROS: usize = 4000;

/// Sondea un HECHO, no un plazo. Ver `engine_destino_que_desaparece`.
async fn espera(mut cond: impl FnMut() -> bool) -> bool {
    let hasta = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while tokio::time::Instant::now() < hasta {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

/// Un motor local CON journal y spool, un origen lleno y un destino vacío.
///
/// El journal no es decorado: `sync.apply` se niega sin él (regla dura 4), así
/// que un motor pelado contesta `Unsupported` y el test mediría eso.
async fn arbol() -> (tempfile::TempDir, tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    // El spool vive FUERA del árbol que el test manosea: dentro sería una
    // entrada más que el plan tendría que mirar.
    let spool = tempfile::tempdir().expect("spool");
    let origen = dir.path().join("origen");
    std::fs::create_dir(&origen).expect("origen");
    std::fs::create_dir(dir.path().join("destino")).expect("destino");
    for i in 0..FICHEROS {
        std::fs::write(origen.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("fichero");
    }
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    engine.set_spool(Spool::new(spool.path()));
    (dir, spool, engine)
}

/// Planifica la sincronización entera y devuelve su hash.
async fn planifica(engine: &Engine) -> PlanHash {
    planifica_desde(engine, "file:///origen").await
}

/// Lo mismo, eligiendo el origen: un test necesita que sea el que se para.
async fn planifica_desde(engine: &Engine, origen: &str) -> PlanHash {
    let (handle, mut rx) = engine
        .sync_plan_as(
            SyncPlanParams {
                source: vp(origen),
                dest: vp("file:///destino"),
                mode: SyncMode::Mirror,
                compare: SyncCompareOptions::default(),
                on_unknown: OnUnknown::Copy,
                include: None,
            },
            1,
            Actor::User,
        )
        .await
        .expect("sync.plan aceptado");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Completed, "el plan va bien");
    done.expect("plan_done").plan_hash
}

/// Lanza el `sync.apply` y espera a que esté DE VERDAD escribiendo.
///
/// Devuelve el handle, el informe y cuántos ficheros había en el destino en
/// ese momento — que es lo que después demuestra que el test llegó a tiempo.
async fn aplicando(
    dir: &std::path::Path,
    engine: &Engine,
    hash: &PlanHash,
) -> (
    norte_core::TaskHandle,
    Arc<std::sync::Mutex<SyncReportResult>>,
    usize,
) {
    let (handle, report) = engine
        .sync_apply_as(hash, 1, Actor::User)
        .await
        .expect("sync.apply aceptado");
    let destino = dir.join("destino");
    assert!(
        espera(|| std::fs::read_dir(&destino).is_ok_and(|d| d.count() > 0)).await,
        "la sincronización no llegó a escribir nada"
    );
    // Medido JUSTO ANTES de intervenir: después del `rename` la tarea sigue
    // llenando esa misma carpeta por el descriptor, así que contarlo luego
    // daría el total dijera lo que dijera la realidad en el instante bueno.
    //
    // Y no hay ningún `await` entre este conteo y el `rename` del test, que es
    // lo que lo hace fiable: bajo el `current_thread` de `#[tokio::test]` la
    // tarea no puede avanzar mientras el cuerpo del test hace E/S síncrona.
    // Un `flavor = "multi_thread"` rompería eso sin avisar — de ahí que esté
    // escrito y no solo supuesto.
    let cuantos = std::fs::read_dir(&destino).expect("destino").count();
    (handle, report, cuantos)
}

/// Que el test interviniera con la tarea todavía viva. Sin esto, un test que
/// llega tarde pasa sin haber probado nada.
fn a_tiempo(cuantos: usize) {
    assert!(
        cuantos < FICHEROS,
        "la sincronización ya había acabado al borrar ({cuantos} de {FICHEROS}): \
         este test no ha probado nada"
    );
}

/// Espera el desenlace sin poder colgarse: el síntoma que se persigue es una
/// tarea que no termina, y un `join()` pelado lo convertiría en un test
/// colgado en vez de en uno rojo.
async fn desenlace(handle: norte_core::TaskHandle) -> TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(2), handle.join())
        .await
        .expect("la tarea se quedó colgada en vez de terminar")
}

fn se_fue(estado: &TaskState, que_paso: &str) {
    assert!(
        matches!(
            estado,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "{que_paso}: la sincronización no puede seguir escribiendo donde nadie \
         va a mirar y decir que fue bien. Fue {estado:?}"
    );
}

/// **Y con UN solo paso, que es lo que fija la comprobación del final.**
///
/// Con cuatro mil pasos disparan las tres comprobaciones —la de abrir, la
/// periódica y la del final—, así que anular una cualquiera deja que las otras
/// dos lo cacen: los tests de abajo prueban el trío, no las piezas. Con un
/// paso no hay periódica y la de abrir ya pasó, de modo que lo único que
/// queda entre «escribí» y «fue bien» es la última. Ésta es la que su propio
/// rustdoc llama «el único momento en el que mentir lo cierra todo».
#[tokio::test]
async fn con_un_solo_paso_la_comprobacion_del_final_es_la_que_lo_caza() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool = tempfile::tempdir().expect("spool");
    std::fs::create_dir(dir.path().join("destino")).expect("destino");

    // El origen es el que se puede PARAR, para que el destino se vaya con el
    // único paso en vuelo: si se fuera antes, lo cazaría `open_root` con un
    // `NotFound` y este test no probaría lo que dice probar.
    let mem = Arc::new(norte_testkit::MemProvider::new());
    mem.mkdir(&vp("lento:///origen")).await.expect("origen");
    {
        let mut sink = mem.write(&vp("lento:///origen/uno")).await.expect("write");
        sink.write(bytes::Bytes::from(vec![b'x'; 64 * 1024]))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let (origen, mut mando) = OrigenAPeticion::nuevo(Arc::clone(&mem));

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    engine.register_provider(origen);
    engine.set_spool(Spool::new(spool.path()));

    let hash = planifica_desde(&engine, "lento:///origen").await;
    let (handle, _report) = engine
        .sync_apply_as(&hash, 1, Actor::User)
        .await
        .expect("sync.apply aceptado");

    assert!(
        mando.empezo().await,
        "la sincronización no llegó a leer nada: este test no ha probado nada"
    );
    std::fs::rename(dir.path().join("destino"), dir.path().join("papelera"))
        .expect("a la papelera");
    mando.sigue();

    se_fue(&desenlace(handle).await, "un plan de un solo paso");
}

/// **La carpeta de destino se borra: la tarea falla y lo dice.**
#[tokio::test]
async fn una_sincronizacion_cuyo_destino_se_borra_falla() {
    let (dir, _spool, engine) = arbol().await;
    let hash = planifica(&engine).await;
    let (handle, _report, cuantos) = aplicando(dir.path(), &engine, &hash).await;

    std::fs::rename(dir.path().join("destino"), dir.path().join("papelera"))
        .expect("a la papelera");
    a_tiempo(cuantos);

    se_fue(&desenlace(handle).await, "la ruta ya no resuelve");
}

/// **Y si aparece OTRA carpeta con el mismo nombre, también.**
///
/// Éste es el que no puede pasar una simple comprobación de existencia: la
/// ruta resuelve, y lo único que desmiente la situación es que el nodo no es
/// el que se abrió.
#[tokio::test]
async fn si_en_el_sitio_del_destino_aparece_otra_carpeta_la_sincronizacion_para() {
    let (dir, _spool, engine) = arbol().await;
    let hash = planifica(&engine).await;
    let (handle, _report, cuantos) = aplicando(dir.path(), &engine, &hash).await;

    let destino = dir.path().join("destino");
    std::fs::rename(&destino, dir.path().join("papelera")).expect("a la papelera");
    std::fs::create_dir(&destino).expect("la nueva");
    a_tiempo(cuantos);

    se_fue(&desenlace(handle).await, "la ruta lleva a OTRO directorio");
    // Y lo que el lector ve en su carpeta nueva es lo que él puso: nada.
    assert_eq!(
        std::fs::read_dir(&destino).expect("la nueva").count(),
        0,
        "no se escribió ni un fichero en la carpeta nueva"
    );
}
