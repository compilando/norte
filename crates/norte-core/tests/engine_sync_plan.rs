//! Integración `Engine::sync_plan_as` (tarea 8 del plan de sincronización de
//! directorios): la Task de [`TaskKind::SyncPlan`], el tee al spool y al lote,
//! el cierre con `sync.plan_done` y los rechazos que no llegan a ser Task.
//!
//! El transductor es de `norte-sync` y ya tiene sus tests contra filas hechas a
//! mano y contra `compare()` real; el spool es de `sync::spool` y tiene los
//! suyos. Aquí se prueba SOLO lo que el core añade al juntarlos: el lote, el
//! contador, la retención, la identidad de las raíces y el final.
//!
//! No hay journal que comprobar: planificar no escribe un byte en ninguno de los
//! dos árboles (regla dura 4 no aplica; quien escribe es `sync.apply`).

use std::sync::Arc;

use bytes::Bytes;
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    RelPath, SYNC_MAX_BLOCKERS_REPORTED, SYNC_MAX_INCLUDE, SYNC_STEPS_MAX_BATCH,
    SyncCompareOptions, SyncMode, SyncPlanDone, SyncPlanParams, SyncStep, SyncStepKind,
};
use norte_proto::{Error as ProtoError, RootOverlap, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

fn rel(wire: &str) -> RelPath {
    RelPath::parse_wire(wire).expect("rel válido")
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

/// Engine + `MemProvider` + EL spool bajo un tempdir propio.
///
/// El `TempDir` se devuelve para que viva lo que dure el test: al soltarlo se
/// borra el directorio de spools con él.
fn setup() -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(dir.path()));
    (engine, mem, dir)
}

fn params(source: &str, dest: &str) -> SyncPlanParams {
    SyncPlanParams {
        source: vp(source),
        dest: vp(dest),
        mode: SyncMode::Update,
        compare: SyncCompareOptions::default(),
        on_unknown: norte_proto::methods::OnUnknown::Copy,
        include: None,
    }
}

/// Los pasos de todos los lotes y el cierre, en el orden en que salieron.
struct Planned {
    batches: Vec<Vec<SyncStep>>,
    done: Option<SyncPlanDone>,
    state: TaskState,
}

impl Planned {
    fn steps(&self) -> Vec<SyncStep> {
        self.batches.iter().flatten().cloned().collect()
    }

    fn done(&self) -> &SyncPlanDone {
        self.done
            .as_ref()
            .expect("el plan cerró con sync.plan_done")
    }
}

/// Lanza el plan y drena su canal hasta el cierre.
async fn plan(engine: &Engine, p: SyncPlanParams) -> Planned {
    let (handle, mut rx) = engine
        .sync_plan_as(p, 1, Actor::User)
        .await
        .expect("sync.plan");
    let mut batches = Vec::new();
    let mut done = None;
    while let Some(event) = rx.recv().await {
        match event {
            SyncPlanEvent::Steps(batch) => {
                assert!(
                    done.is_none(),
                    "un lote DESPUÉS del cierre: sync.plan_done tiene que ser el último"
                );
                assert_eq!(batch.task_id, handle.id(), "el lote lleva SU task_id");
                batches.push(batch.steps);
            }
            SyncPlanEvent::Done(d) => {
                assert!(done.is_none(), "dos cierres para un plan");
                assert_eq!(d.task_id, handle.id());
                done = Some(d);
            }
        }
    }
    let state = handle.join().await;
    Planned {
        batches,
        done,
        state,
    }
}

/// Cuántos ficheros hay en el directorio de spools (el `.part` incluido).
fn spooled(engine: &Engine) -> usize {
    let spool = engine.spool().expect("hay spool");
    match std::fs::read_dir(spool.dir()) {
        Ok(rd) => rd.count(),
        // Nunca se creó: cero.
        Err(_) => 0,
    }
}

/// Dos ficheros en el origen y un destino vacío: dos copias, sin bloqueos.
async fn simple(mem: &MemProvider) {
    mkdir(mem, "mem:///s").await;
    mkdir(mem, "mem:///d").await;
    write_file(mem, "mem:///s/a.txt", b"aaa").await;
    write_file(mem, "mem:///s/b.txt", b"bbbb").await;
}

// 1 ───────────────────────────────────────────────────────────────────────
/// Los lotes van ACOTADOS y coalescidos, el mismo contrato que `compare.rows`:
/// medio millón de pasos no puede convertirse en medio millón de frames.
#[tokio::test]
async fn los_pasos_llegan_en_lotes_acotados_y_coalescidos() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    for i in 0..600 {
        write_file(&mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    assert!(
        out.batches.iter().all(|b| b.len() <= SYNC_STEPS_MAX_BATCH),
        "lote por encima del tope: {:?}",
        out.batches.iter().map(Vec::len).collect::<Vec<_>>()
    );
    assert_eq!(out.steps().len(), 600, "un paso por fichero del origen");
    assert!(
        out.batches.len() < 600,
        "una frame por paso no es coalescer: {} lotes",
        out.batches.len()
    );
    assert_eq!(out.done().counts.copy, 600);
}

// 2 ───────────────────────────────────────────────────────────────────────
/// El plan CIERRA con su hash, sus contadores y su veredicto, y todo eso sale
/// del resumen del spool — no se recalcula aquí ni en el daemon.
#[tokio::test]
async fn el_plan_cierra_con_hash_contadores_y_executable() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    let done = out.done();
    assert!(done.executable);
    assert!(done.blockers.is_empty());
    assert_eq!(done.blockers_total, 0);
    assert_eq!(done.counts.copy, 2);
    assert_eq!(
        done.plan_hash.as_str().len(),
        norte_proto::methods::PLAN_HASH_LEN
    );
    assert!(
        out.steps().iter().all(SyncStep::shape_is_consistent),
        "el core no puede emitir pasos incoherentes: {:#?}",
        out.steps()
    );
    assert!(
        out.steps()
            .iter()
            .all(|s| s.kind == SyncStepKind::Copy && s.reversal.is_some())
    );
}

// 3 ───────────────────────────────────────────────────────────────────────
/// Un directorio contra un fichero del mismo nombre es un bloqueo estructural
/// (`TypeMismatchDir`): reemplazar un árbol por un fichero merece un humano, y
/// el plan deja de ser ejecutable.
#[tokio::test]
async fn un_bloqueo_deja_el_plan_no_ejecutable() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    write_file(&mem, "mem:///s/x", b"soy un fichero").await;
    mkdir(&mem, "mem:///d/x").await;

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    let done = out.done();
    assert!(!done.executable, "un bloqueo NO se puede aprobar");
    assert_eq!(done.blockers.len(), 1, "{:#?}", done.blockers);
    assert_eq!(done.blockers_total, 1);
}

// 4 ───────────────────────────────────────────────────────────────────────
/// La LISTA de bloqueos se recorta; el TOTAL no. Un humano necesita saber que
/// hay 266 aunque solo se le puedan enseñar 256.
#[tokio::test]
async fn los_bloqueos_se_recortan_pero_el_total_no() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    let total = SYNC_MAX_BLOCKERS_REPORTED + 10;
    for i in 0..total {
        write_file(&mem, &format!("mem:///s/x{i}"), b"f").await;
        mkdir(&mem, &format!("mem:///d/x{i}")).await;
    }

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    let done = out.done();
    assert!(!done.executable);
    assert_eq!(done.blockers.len(), SYNC_MAX_BLOCKERS_REPORTED);
    assert_eq!(done.blockers_total, total as u64);
}

// 5 ───────────────────────────────────────────────────────────────────────
/// Raíces solapadas: se rechazan ANTES de recorrer nada, y el error dice CUÁL
/// está dentro de cuál — no es lo mismo para quien lo pinta.
#[tokio::test]
async fn raices_solapadas_se_rechazan_antes_de_recorrer() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///a/sub").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///a", "mem:///a/sub"), 1, Actor::User)
        .await
    else {
        panic!("el destino está dentro del origen");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::DestInsideSource
            }
        ),
        "fue {err:?}"
    );

    let Err(err) = engine
        .sync_plan_as(params("mem:///a/sub", "mem:///a"), 1, Actor::User)
        .await
    else {
        panic!("el origen está dentro del destino");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::SourceInsideDest
            }
        ),
        "fue {err:?}"
    );
    assert_eq!(spooled(&engine), 0, "un rechazo no crea Task ni spool");
}

// 6 ───────────────────────────────────────────────────────────────────────
/// Dos raíces IGUALES no son «una dentro de la otra»: son la misma, y el error
/// lo dice en vez de elegir un lado por convenio.
#[tokio::test]
async fn raices_identicas_lo_dicen_en_vez_de_nombrar_un_lado() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///a").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///a", "mem:///a"), 1, Actor::User)
        .await
    else {
        panic!("una raíz contra sí misma");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::Same
            }
        ),
        "fue {err:?}"
    );
}

// 7 ───────────────────────────────────────────────────────────────────────
/// Y la mitad que la comprobación estructural NO puede ver: una raíz que es un
/// symlink al mismo directorio que la otra. Los dos `VPath` son distintos byte a
/// byte y las filas del walk cuelgan todas de la raíz symlinkeada, así que ni la
/// igualdad estructural ni el guard del walk lo cogen. Lo coge `node_id`.
#[tokio::test]
async fn una_raiz_symlinkeada_contra_su_destino_es_el_mismo_arbol() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///real").await;
    write_file(&mem, "mem:///real/a.txt", b"x").await;
    mem.symlink(&vp("mem:///alias"), b"real", norte_vfs::SymlinkKind::Dir)
        .await
        .expect("symlink");

    let Err(err) = engine
        .sync_plan_as(params("mem:///alias", "mem:///real"), 1, Actor::User)
        .await
    else {
        panic!("las dos raíces son el mismo directorio");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::Same
            }
        ),
        "fue {err:?}"
    );
}

// 8 ───────────────────────────────────────────────────────────────────────
/// Dos campos de `compare` no son del llamante en `sync.plan`. Mandarlos es un
/// rechazo, jamás un valor que el core pise en silencio: servir un recorrido
/// distinto del pedido es peor que no ofrecerlo.
#[tokio::test]
async fn el_llamante_no_fija_las_opciones_del_planificador() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let mut p = params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("follow_symlinks no se sirve en silencio");
    };
    assert!(matches!(err, ProtoError::Unsupported), "fue {err:?}");

    let mut p = params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(norte_proto::methods::DescendSide::Right);
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("descend_orphans lo fija el planificador");
    };
    assert!(matches!(err, ProtoError::Unsupported), "fue {err:?}");
    assert_eq!(spooled(&engine), 0);
}

// 9 ───────────────────────────────────────────────────────────────────────
/// Un `include` por encima del tope se REHÚSA. Recortarlo en silencio
/// sincronizaría algo que nadie pidió, y el humano lo aprobaría creyendo que lo
/// vio entero.
#[tokio::test]
async fn un_include_por_encima_del_tope_se_rehusa_en_vez_de_recortarse() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("x"); SYNC_MAX_INCLUDE + 1]);
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("por encima del tope");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "fue {err:?}");
}

// 10 ──────────────────────────────────────────────────────────────────────
/// El `include` recorta lo que SALE, y el hash va con lo recortado: dos planes
/// del mismo árbol con selecciones distintas no pueden compartir digest, o
/// aprobar uno autorizaría el otro.
#[tokio::test]
async fn el_include_recorta_los_pasos_y_el_hash_va_con_ellos() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///s/sub").await;
    write_file(&mem, "mem:///s/a.txt", b"a").await;
    write_file(&mem, "mem:///s/sub/dentro.txt", b"b").await;

    let todo = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(todo.state, TaskState::Completed);
    assert_eq!(todo.done().counts.copy, 2, "a.txt y sub/dentro.txt");

    // Solo la carpeta: arrastra su contenido y deja fuera a `a.txt`.
    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("sub")]);
    let parte = plan(&engine, p).await;
    assert_eq!(parte.state, TaskState::Completed);
    let rels: Vec<String> = parte.steps().iter().map(|s| s.rel.to_wire()).collect();
    assert_eq!(rels, vec!["sub".to_owned(), "sub/dentro.txt".to_owned()]);
    assert_eq!(parte.done().counts.copy, 1);
    assert_eq!(parte.done().counts.create_dir, 1);
    assert_ne!(
        parte.done().plan_hash,
        todo.done().plan_hash,
        "dos selecciones distintas no pueden compartir hash"
    );
}

// 10b ─────────────────────────────────────────────────────────────────────
/// Y el arrastre hacia ARRIBA, que es el que falta: el panel deja seleccionar la
/// fila del FICHERO, y sin el `CreateDir` de su carpeta el plan copiaría dentro
/// de un directorio que no existe — rompiendo además la regla que
/// `SyncStepsBatch::steps` publica («un `CreateDir` precede a toda copia dentro
/// de él»).
#[tokio::test]
async fn seleccionar_un_fichero_arrastra_el_createdir_de_su_carpeta() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///s/nueva").await;
    write_file(&mem, "mem:///s/nueva/a.txt", b"a").await;
    write_file(&mem, "mem:///s/nueva/b.txt", b"b").await;

    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("nueva/a.txt")]);
    let out = plan(&engine, p).await;
    assert_eq!(out.state, TaskState::Completed);

    let steps = out.steps();
    let rels: Vec<String> = steps.iter().map(|s| s.rel.to_wire()).collect();
    assert_eq!(rels, vec!["nueva".to_owned(), "nueva/a.txt".to_owned()]);
    assert_eq!(steps[0].kind, SyncStepKind::CreateDir);
    assert_eq!(out.done().counts.copy, 1, "b.txt no estaba seleccionado");
    assert_eq!(out.done().counts.create_dir, 1);
}

// 11 ──────────────────────────────────────────────────────────────────────
/// Regla dura 3 en la frontera de la Task, y la consecuencia que importa: un
/// plan cancelado **no deja nada aprobable**. El digest parcial de un plan
/// cortado por la mitad sería perfectamente válido para un plan que dice
/// sincronizar un árbol que se recorrió un tercio.
#[tokio::test]
async fn cancelar_el_plan_no_deja_spool() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    // Bastantes pasos para LLENAR el canal (capacidad 8 lotes de 256): con el
    // emisor bloqueado en `send`, el plan no puede terminar antes de que el test
    // corte. Sin eso el test sería una carrera contra el reloj.
    for i in 0..3_000 {
        write_file(&mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }

    let (handle, mut rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    let first = rx.recv().await.expect("al menos un evento");
    assert!(matches!(first, SyncPlanEvent::Steps(_)));
    handle.cancel();

    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(done.is_none(), "un plan cancelado NO cierra");
    assert_eq!(
        spooled(&engine),
        0,
        "un plan cancelado no es un plan aprobable"
    );
}

// 12 ──────────────────────────────────────────────────────────────────────
/// El dueño que deja de recibir tampoco deja plan: lo que retendríamos sería un
/// plan que nadie llegó a ver entero, y soltar el canal es además lo que para el
/// walk.
#[tokio::test]
async fn un_dueno_que_deja_de_recibir_no_deja_plan_aprobable() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    // Bastantes pasos para LLENAR el canal (capacidad 8 lotes de 256): con el
    // emisor bloqueado en `send`, el plan no puede terminar antes de que el test
    // corte. Sin eso el test sería una carrera contra el reloj.
    for i in 0..3_000 {
        write_file(&mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }

    let (handle, mut rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    rx.recv().await.expect("al menos un evento");
    drop(rx);

    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert_eq!(spooled(&engine), 0);
}

// 13 ──────────────────────────────────────────────────────────────────────
/// Un plan que termina deja EXACTAMENTE un spool, y se abre con el hash que
/// viajó en el cierre. Es la precondición del ejecutor: `sync.apply` no lleva
/// las raíces, así que salen de ahí.
#[tokio::test]
async fn un_plan_completo_deja_un_spool_abrible_con_su_hash() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    assert_eq!(spooled(&engine), 1);

    let spool = engine.spool().expect("hay spool");
    let reader = spool
        .open(1, &out.done().plan_hash)
        .await
        .expect("el plan se abre con su hash");
    assert_eq!(reader.header().options.source_root, vp("mem:///s"));
    assert_eq!(reader.header().options.dest_root, vp("mem:///d"));
    assert!(reader.summary().executable);

    // Y otra conexión no lo abre, aunque conozca el hash.
    assert!(spool.open(2, &out.done().plan_hash).await.is_err());
}

// 13b ─────────────────────────────────────────────────────────────────────
/// Un plan que no emite NI UN paso —dos árboles idénticos— nunca toca su canal,
/// así que no puede enterarse de que su dueño se fue por la vía del `flush`. Es
/// el caso que hace que la propiedad «un plan sin dueño no queda retenido» tenga
/// que decidirse en el spool y no en el canal.
#[tokio::test]
async fn un_plan_de_cero_pasos_cuyo_dueno_se_fue_no_queda_retenido() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    write_file(&mem, "mem:///s/a.txt", b"x").await;
    write_file(&mem, "mem:///d/a.txt", b"x").await;

    let (handle, rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    // El dueño se va antes de que la Task llegue a nada, y el spool se entera
    // por el desmontaje de la conexión, no por el canal.
    engine
        .spool()
        .expect("hay spool")
        .drop_connection(1)
        .await
        .expect("drop");
    drop(rx);

    assert_ne!(handle.join().await, TaskState::Completed);
    assert_eq!(spooled(&engine), 0, "un plan sin dueño no se retiene");
}

// 14 ──────────────────────────────────────────────────────────────────────
/// Sin spool instalado no se planifica: un plan que no se puede retener tampoco
/// se puede aplicar, y enseñar un diálogo de aprobación sobre algo que después
/// no existe es peor que no ofrecerlo (fail-closed, como el índice).
#[tokio::test]
async fn sin_spool_instalado_no_se_planifica() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
    else {
        panic!("sin retención no hay plan");
    };
    assert!(matches!(err, ProtoError::Unsupported), "fue {err:?}");
}
