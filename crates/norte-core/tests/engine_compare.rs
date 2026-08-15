//! Integración `Engine::compare_as` (C6 del plan de comparación de
//! directorios): la Task de [`TaskKind::Compare`], la bomba de lotes
//! coalescidos, el progreso que cuenta FILAS y la cancelación limpia.
//!
//! La cascada, el emparejamiento y el walk son de `norte-compare` y ya tienen
//! sus tests contra `MemProvider`; aquí se prueba SOLO lo que el core añade:
//! el lote, el contador y el final.
//!
//! No hay nada de journal que comprobar: la comparación no escribe un byte
//! (regla dura 4 no aplica).

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    COMPARE_ROWS_MAX_BATCH, CompareCriteria, CompareRowsBatch, CompareVerdict, FsCompareParams,
};
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use tokio::sync::mpsc::Receiver;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

/// Engine + `MemProvider` in-memory registrado bajo `mem`.
fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Params base: dos raíces, criterios por defecto (sin hash).
fn params(left: &str, right: &str) -> FsCompareParams {
    FsCompareParams {
        left: vp(left),
        right: vp(right),
        criteria: CompareCriteria::default(),
        max_depth: None,
        mtime_tolerance_ms: 2000,
        follow_symlinks: false,
        descend_orphans: None,
    }
}

/// Dos árboles GEMELOS de `n` ficheros bajo `mem:///l` y `mem:///r`.
async fn twin_trees(mem: &MemProvider, n: usize) {
    mkdir(mem, "mem:///l").await;
    mkdir(mem, "mem:///r").await;
    for i in 0..n {
        write_file(mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
}

/// Drena el canal hasta el cierre; devuelve los LOTES tal cual llegaron (la
/// forma del lote es lo que se está probando, no solo su contenido).
async fn drain(mut rx: Receiver<CompareRowsBatch>) -> Vec<CompareRowsBatch> {
    let mut out = Vec::new();
    while let Some(b) = rx.recv().await {
        out.push(b);
    }
    out
}

// 1 ───────────────────────────────────────────────────────────────────────
/// Los lotes van acotados y COALESCIDOS, el mismo contrato que `search.hits`:
/// mil filas no pueden convertirse en mil frames.
#[tokio::test]
async fn las_filas_llegan_en_lotes_acotados_y_coalescidos() {
    let (engine, mem) = setup();
    twin_trees(&mem, 1_000).await;

    let (h, rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let id = h.id();
    let batches = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);

    assert!(
        batches
            .iter()
            .all(|b| b.rows.len() <= COMPARE_ROWS_MAX_BATCH),
        "lote por encima del tope: {:?}",
        batches.iter().map(|b| b.rows.len()).collect::<Vec<_>>()
    );
    let rows: usize = batches.iter().map(|b| b.rows.len()).sum();
    assert_eq!(rows, 1_000, "una fila por pareja");
    assert!(
        batches.len() < 1_000,
        "una frame por fila no es coalescer: {} lotes",
        batches.len()
    );
    assert!(
        batches.iter().all(|b| b.task_id == id),
        "todos los lotes llevan el task_id de SU comparación"
    );
}

// 2 ───────────────────────────────────────────────────────────────────────
/// Regla dura 3 en la frontera de la Task: cancelar termina en `Cancelled`
/// —el `Err` del motor es SOLO eso, no un fallo— y los lotes paran.
///
/// Nada que limpiar: la comparación no escribe.
#[tokio::test]
async fn cancelar_la_task_para_los_lotes_y_termina_en_cancelled() {
    let (engine, mem) = setup();
    twin_trees(&mem, 3_000).await;

    let (h, mut rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let first = rx.recv().await.expect("al menos un lote");
    assert!(!first.rows.is_empty());
    h.cancel();

    let mut seen = first.rows.len();
    while let Some(b) = rx.recv().await {
        seen += b.rows.len();
    }
    assert!(
        seen < 3_000,
        "los lotes siguieron llegando tras el cancel: {seen}"
    );
    assert_eq!(h.join().await, TaskState::Cancelled);
}

// 3 ───────────────────────────────────────────────────────────────────────
/// `entries_done` cuenta FILAS emitidas, y es CONTRATO (C1): es la única señal
/// con la que un cliente detecta que se le perdió un `compare.rows` — aquí no
/// hay `max_hits` contra el que contar como en `fs.search`.
#[tokio::test]
async fn el_progreso_cuenta_las_filas_emitidas() {
    let (engine, mem) = setup();
    twin_trees(&mem, 10).await;

    let (h, rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let progress = h.progress();
    let batches = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);

    let rows: u64 = batches
        .iter()
        .map(|b| u64::try_from(b.rows.len()).expect("cabe"))
        .sum();
    assert_eq!(rows, 10);
    assert_eq!(
        progress.borrow().entries_done,
        rows,
        "el último snapshot tiene que cuadrar con lo emitido"
    );
}

// 4 ───────────────────────────────────────────────────────────────────────
/// Los veredictos son los del motor, sin traducción por el camino: dos árboles
/// idénticos son todo `Same`.
#[tokio::test]
async fn dos_arboles_identicos_son_todo_same() {
    let (engine, mem) = setup();
    twin_trees(&mem, 3).await;

    let (h, rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let batches = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);

    let rows: Vec<_> = batches.into_iter().flat_map(|b| b.rows).collect();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|r| r.verdict == CompareVerdict::Same),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .all(|r| r.sides_are_consistent() && r.reason_is_consistent()),
        "el daemon no puede emitir filas incoherentes: {rows:#?}"
    );
}

// 5 ───────────────────────────────────────────────────────────────────────
/// Comparar una raíz contra sí misma no llega a ser Task: es un error de quien
/// llama, y se rechaza antes de crearla.
#[tokio::test]
async fn comparar_una_raiz_contra_si_misma_no_crea_task() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///d").await;

    let Err(err) = engine
        .compare_as(params("mem:///d", "mem:///d"), Actor::User)
        .await
    else {
        panic!("comparar una raíz consigo misma tenía que rechazarse");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "fue {err:?}");
}

// 6 ───────────────────────────────────────────────────────────────────────
/// El motor ACEPTA `follow_symlinks` y lo IGNORA; quien lo recibe del wire
/// tiene que rechazarlo en vez de servir en silencio un recorrido distinto del
/// pedido. El engine es la primera frontera pública, así que lo rechaza él.
#[tokio::test]
async fn follow_symlinks_se_rechaza_en_vez_de_ignorarse() {
    let (engine, mem) = setup();
    twin_trees(&mem, 1).await;

    let mut p = params("mem:///l", "mem:///r");
    p.follow_symlinks = true;
    let Err(err) = engine.compare_as(p, Actor::User).await else {
        panic!("follow_symlinks tenía que rechazarse");
    };
    assert!(matches!(err, ProtoError::Unsupported), "fue {err:?}");
}

// ADR 0054 ────────────────────────────────────────────────────────────────
/// #153/#145: la comparación pliega como pliega LA RAÍZ, no como pliega el
/// provider. El `MemProvider` declara `CASE_SENSITIVE` para sí mismo y guioniza
/// un `+F` solo para la raíz derecha; si `fs.compare` preguntase por el
/// provider —lo que hacía—, esta pareja saldría como dos huérfanas.
#[tokio::test]
async fn la_comparacion_pliega_como_pliega_la_raiz() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///l").await;
    mkdir(&mem, "mem:///r").await;

    let fixture = |id: &str| {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} en el corpus"))
            .bytes
    };
    let zett = String::from_utf8(fixture("ext4_full_fold_es_zett")).expect("UTF-8");
    let ss = String::from_utf8(fixture("ext4_full_fold_ss")).expect("UTF-8");
    write_file(&mem, &format!("mem:///l/{zett}"), b"x").await;
    write_file(&mem, &format!("mem:///r/{ss}"), b"x").await;

    mem.set_caps_at(
        &vp("mem:///r"),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::CASE_PRESERVING
                | norte_proto::CapabilityFlags::FULL_FOLD,
            max_path: None,
        },
    );

    let (h, rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let rows: Vec<_> = drain(rx).await.into_iter().flat_map(|b| b.rows).collect();
    assert_eq!(h.join().await, TaskState::Completed);

    assert_eq!(
        rows.len(),
        1,
        "una pareja, no dos huérfanas: {:?}",
        rows.iter().map(|r| r.verdict).collect::<Vec<_>>()
    );
    assert!(
        rows[0].left.is_some() && rows[0].right.is_some(),
        "y la fila lleva los dos lados"
    );
}
