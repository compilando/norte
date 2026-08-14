//! Integración `Engine::sync_apply_as` (tarea 9 del plan de sincronización de
//! directorios): el plan aprobado se ejecuta, cada efecto llega al journal bajo
//! UN lote, y lo que no ocurrió sale en el informe en vez de matar la Task.
//!
//! Los rincones del ejecutor —qué compara la revalidación, qué entrada deja una
//! sobrescritura sin papelera, qué pasa si el plan deja de leerse— están en los
//! tests unitarios de `sync::exec`. Aquí se prueba el CABLEADO: que las raíces
//! salen del spool y no de la petición, que el gate corre sobre ellas AL
//! APLICAR, que el lote existe y agrupa, y que el plan se gasta pase lo que pase.
//!
//! Del 13 en adelante, lo que ese lote vale: `Engine::undo_session` sobre las
//! entradas que el ejecutor REAL escribió (tarea 11). Un lote que agrupa pero no
//! se deshace no es una unidad deshacible, es una etiqueta.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::journal::{Journal, JournalEntry, SqliteJournal};
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine, UndoReport};
use norte_proto::methods::{
    DestTrash, OnUnknown, PlanHash, SyncCompareOptions, SyncMode, SyncPlanDone, SyncPlanParams,
    SyncReportResult, SyncStepKind,
};
use norte_proto::{CapabilityFlags, Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

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

async fn read_file(mem: &MemProvider, wire: &str) -> Vec<u8> {
    let mut stream = mem.read(&vp(wire), None).await.expect("read abre");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

async fn exists(mem: &MemProvider, wire: &str) -> bool {
    mem.stat(&vp(wire)).await.is_ok()
}

struct Harness {
    engine: Engine,
    mem: Arc<MemProvider>,
    journal: Arc<SqliteJournal>,
    _dir: tempfile::TempDir,
}

/// Engine con journal, spool y un `MemProvider` con las capacidades que el test
/// pida. `MemProvider::new()` NO declara papelera, así que el camino con
/// papelera hay que pedirlo a mano — igual que en la vida real, donde `file://`
/// la tiene y un bucket no.
async fn harness(flags: CapabilityFlags) -> Harness {
    harness_with(flags, true).await
}

/// Igual, eligiendo la clase de papelera. `logical` = el provider dice DÓNDE
/// dejó lo que enterró (`reversal_ref`); sin ella se comporta como la papelera
/// nativa del sistema, que no da handle de restauración — y ese es el caso que
/// el undo de una sobrescritura no puede acertar.
async fn harness_with(flags: CapabilityFlags, logical: bool) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    // Papelera LÓGICA: la de serie del testkit hace «desaparecer» el subárbol
    // sin destino recuperable (como la nativa del OS), y entonces no hay
    // `reversal_ref` que comprobar. Lo que se quiere probar aquí es que el undo
    // recibe DÓNDE se enterró cuando el provider lo sabe decir.
    let mem = Arc::new(if logical {
        MemProvider::with_flags(flags).with_logical_trash()
    } else {
        MemProvider::with_flags(flags)
    });
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(dir.path()));
    Harness {
        engine,
        mem,
        journal,
        _dir: dir,
    }
}

/// Las capacidades de un destino CON papelera.
fn with_trash() -> CapabilityFlags {
    CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING | CapabilityFlags::TRASH
}

/// Y las de uno sin ella (un bucket, un SFTP).
fn without_trash() -> CapabilityFlags {
    CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING
}

fn params(mode: SyncMode) -> SyncPlanParams {
    SyncPlanParams {
        source: vp("mem:///s"),
        dest: vp("mem:///d"),
        mode,
        compare: SyncCompareOptions::default(),
        on_unknown: OnUnknown::Copy,
        include: None,
    }
}

/// Planifica y drena el canal hasta el cierre; devuelve el `sync.plan_done`.
async fn plan(h: &Harness, mode: SyncMode) -> SyncPlanDone {
    let (handle, mut rx) = h
        .engine
        .sync_plan_as(params(mode), 1, Actor::User)
        .await
        .expect("sync.plan aceptado");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Completed);
    done.expect("el plan cerró con sync.plan_done")
}

/// Aplica `hash` y espera al final. Devuelve el estado terminal y el informe.
async fn apply(h: &Harness, hash: &PlanHash) -> (TaskState, SyncReportResult) {
    let (handle, report) = h
        .engine
        .sync_apply_as(hash, 1, Actor::User)
        .await
        .expect("sync.apply aceptado");
    let state = handle.join().await;
    let report = report.lock().expect("report lock").clone();
    (state, report)
}

async fn entries(h: &Harness) -> Vec<JournalEntry> {
    h.journal.journal().entries().await.expect("entries")
}

/// Deshace la sesión del humano y espera al final.
async fn undo(h: &Harness) -> (TaskState, UndoReport) {
    let (handle, report) = h
        .engine
        .undo_session(Actor::User)
        .await
        .expect("undo aceptado");
    let state = handle.join().await;
    let report = report.lock().expect("undo report lock").clone();
    (state, report)
}

/// El árbol base: un huérfano del origen, una pareja que difiere y un huérfano
/// del destino (que solo `Mirror` mira).
async fn seed(h: &Harness) {
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///s/nueva")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/nueva/a.txt", b"nueva-a").await;
    write_file(&h.mem, "mem:///s/comun.txt", b"origen-mas-largo").await;
    write_file(&h.mem, "mem:///d/comun.txt", b"destino").await;
    write_file(&h.mem, "mem:///d/sobra.txt", b"sobra").await;
}

// 1 ───────────────────────────────────────────────────────────────────────
/// El camino entero: un `CreateDir`, una `Copy` y un `Overwrite` con papelera.
/// La copia aterriza, la sobrescritura entierra antes de escribir, y TODO
/// comparte un `batch_id` — que es lo que lo convierte en una unidad deshacible.
#[tokio::test]
async fn un_plan_aprobado_se_ejecuta_y_queda_en_un_solo_lote() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    assert!(done.executable, "el plan es aprobable");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert_eq!(report.done, 3, "createdir + copy + overwrite");
    assert!(report.batch_id.is_some(), "el undo lo necesita");
    // #170: el informe se basta solo. Quien lo lee puede no ser quien aplicó
    // —una reconexión, otra conexión, un `plan_done` que se soltó—, y sin esto
    // no podía saber si algo de lo que acaba de leer vuelve.
    assert_eq!(report.dest_trash, DestTrash::Restorable);
    assert_eq!(
        report.dest_trash, done.dest_trash,
        "el informe no puede decir otra papelera que la que se aprobó"
    );

    assert_eq!(read_file(&h.mem, "mem:///d/nueva/a.txt").await, b"nueva-a");
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"origen-mas-largo"
    );
    // `Update` no borra: el huérfano del destino sigue ahí.
    assert!(exists(&h.mem, "mem:///d/sobra.txt").await);

    let es = entries(&h).await;
    let batch = report.batch_id;
    assert!(
        es.iter().all(|e| e.batch_id == batch),
        "todas las entradas del lote: {es:?}"
    );
    // La sobrescritura es `trashed` + `created`, en ese orden: el undo recorre
    // `seq` descendente, así que borra lo creado ANTES de restaurar lo enterrado.
    let sobre: Vec<&JournalEntry> = es
        .iter()
        .filter(|e| e.path == b"mem:///d/comun.txt")
        .collect();
    assert_eq!(sobre.len(), 2, "{sobre:?}");
    assert_eq!(sobre[0].op, "trashed");
    assert_eq!(sobre[0].reversal, "restore_trash");
    assert!(
        sobre[0].reversal_ref.is_some(),
        "el undo necesita saber DÓNDE se enterró"
    );
    assert_eq!(sobre[1].op, "created");
    assert_eq!(sobre[1].reversal, "delete");
    assert!(sobre[1].seq > sobre[0].seq);
}

// 2 ───────────────────────────────────────────────────────────────────────
/// Sin papelera, la sobrescritura deja UNA entrada y se declara irreversible.
/// Es la fila de la tabla que un plan tiene que enseñar ANTES de que nadie lo
/// apruebe, y el contador del diálogo sale del mismo sitio.
#[tokio::test]
async fn una_sobrescritura_sin_papelera_se_declara_irreversible() {
    let h = harness(without_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"origen").await;
    write_file(&h.mem, "mem:///d/a.txt", b"destino-mas-largo").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 1, "el diálogo lo enseña");
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 1);
    // El contraste de #170, y el motivo de que el campo no tenga default: este
    // informe y el del test 1 son el mismo informe salvo por esta clave, y uno
    // se deshace entero y el otro no se deshace nada.
    assert_eq!(report.dest_trash, DestTrash::Absent);
    assert_eq!(report.dest_trash, done.dest_trash);
    assert_eq!(read_file(&h.mem, "mem:///d/a.txt").await, b"origen");

    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "una sola entrada: {es:?}");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].reversal, "irreversible");
}

// 3 ───────────────────────────────────────────────────────────────────────
/// `Mirror` borra el huérfano del destino, y con papelera es UN `trashed` por el
/// árbol entero: una entrada, una cosa que restaurar.
#[tokio::test]
async fn mirror_entierra_el_huerfano_del_destino_de_una_pieza() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d/sobra")).await.expect("mkdir");
    write_file(&h.mem, "mem:///d/sobra/x.txt", b"x").await;
    write_file(&h.mem, "mem:///d/sobra/y.txt", b"y").await;

    let done = plan(&h, SyncMode::Mirror).await;
    assert_eq!(done.counts.delete_tree, 1);
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 1);
    assert!(!exists(&h.mem, "mem:///d/sobra").await);
    assert!(!exists(&h.mem, "mem:///d/sobra/x.txt").await);

    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "UNA entrada por el árbol entero: {es:?}");
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].path, b"mem:///d/sobra");
}

// 4 ───────────────────────────────────────────────────────────────────────
/// **El motivo entero de que exista el `stat` de revalidación.** Si el destino
/// dejó de parecerse a lo que el plan anotó, el paso NO se ejecuta: sale como
/// `Conflict` y los bytes que había siguen ahí. Es lo único que hay entre el TTL
/// del plan y un fichero perdido.
#[tokio::test]
async fn un_destino_que_cambio_bajo_el_plan_es_conflicto_y_no_escritura() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"origen").await;
    write_file(&h.mem, "mem:///d/a.txt", b"destino").await;

    let done = plan(&h, SyncMode::Update).await;
    // Alguien llega antes que nosotros. (El `write` de un provider es
    // create-new, así que sustituir de verdad es quitar y volver a poner.)
    h.mem.remove(&vp("mem:///d/a.txt")).await.expect("remove");
    write_file(&h.mem, "mem:///d/a.txt", b"alguien llego antes").await;

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed, "un fallo no mata la Task");
    assert_eq!(report.done, 0);
    assert_eq!(report.failed, 1);
    assert_eq!(
        report.failures[0].cause,
        norte_proto::methods::SyncFailureCause::Conflict
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/a.txt").await,
        b"alguien llego antes",
        "no se escribió NADA"
    );
    assert!(entries(&h).await.is_empty(), "ni se journalizó nada");
}

// 5 ───────────────────────────────────────────────────────────────────────
/// Un paso que falla es una FILA del informe y el recorrido sigue: el paso 40 000
/// de 500 000 no puede llevarse por delante los 460 000 que quedan.
#[tokio::test]
async fn un_fallo_en_mitad_no_mata_la_task() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"a").await;
    write_file(&h.mem, "mem:///s/b.txt", b"b").await;
    write_file(&h.mem, "mem:///s/c.txt", b"c").await;

    let done = plan(&h, SyncMode::Update).await;
    // `b.txt` deja de existir en el origen entre aprobar y aplicar.
    h.mem.remove(&vp("mem:///s/b.txt")).await.expect("remove");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert_eq!(report.failures[0].rel.to_wire(), "b.txt");
    // #195: la clase del paso que falló. Aquí es un `Copy`, cuyo `rel` cuelga
    // del ORIGEN — que es lo que la fila hostil del test de abajo NO hace.
    assert_eq!(report.failures[0].kind, SyncStepKind::Copy);
    assert!(
        exists(&h.mem, "mem:///d/c.txt").await,
        "el recorrido siguió"
    );
}

// 5b ──────────────────────────────────────────────────────────────────────
/// **La fila hostil más común de un `Mirror`, y todo el motivo de #195**: un
/// `DeleteTree` que no ocurre. Su `rel` cuelga del DESTINO, no lleva `dest_rel`
/// —el borrado ya está deletreado como el destino lo deletrea— y hasta 0.41.0
/// el informe no traía nada con lo que distinguirla de un `Copy` fallido, cuyo
/// `rel` cuelga del origen. Un panel que la pintase bajo la columna del origen
/// manda al operador a mirar el árbol que no se ha tocado.
#[tokio::test]
async fn un_delete_tree_que_falla_se_reconoce_por_su_clase() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d/sobra")).await.expect("mkdir");
    write_file(&h.mem, "mem:///d/sobra/x.txt", b"x").await;

    let done = plan(&h, SyncMode::Mirror).await;
    assert_eq!(done.counts.delete_tree, 1);
    // El árbol desaparece entre aprobar y aplicar: la revalidación lo caza y el
    // paso sale como fila del informe en vez de tocar nada.
    h.mem
        .remove(&vp("mem:///d/sobra/x.txt"))
        .await
        .expect("remove");
    h.mem.remove(&vp("mem:///d/sobra")).await.expect("remove");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 1, "{:?}", report.failures);
    let fallo = &report.failures[0];
    assert_eq!(fallo.kind, SyncStepKind::DeleteTree);
    assert_eq!(fallo.rel.to_wire(), "sobra");
    assert_eq!(
        fallo.dest_rel, None,
        "un borrado ya viene deletreado como el destino: sin esta clase, la fila \
         no tenía NADA que dijera de qué raíz cuelga su `rel`"
    );
}

// 6 ───────────────────────────────────────────────────────────────────────
/// El plan se GASTA en cualquier estado terminal: `Spool::remove` corre al
/// acabar la Task, así que el mismo hash ya no se puede volver a aplicar. Sin
/// esa llamada el hash se quedaría «aplicándose» y ni siquiera se podría
/// replanificar el mismo árbol.
#[tokio::test]
async fn el_plan_se_gasta_al_terminar_la_task() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, _) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);

    let again = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(
        matches!(again, Err(ProtoError::PlanStale)),
        "un plan se aprueba una vez"
    );
}

// 7 ───────────────────────────────────────────────────────────────────────
/// Un hash que este daemon no emitió es un plan rancio, no un fallo interno: la
/// respuesta le está diciendo al cliente que vuelva a planificar.
#[tokio::test]
async fn un_hash_que_este_daemon_no_emitio_es_plan_rancio() {
    let h = harness(with_trash()).await;
    let ajeno = PlanHash::parse(&"0".repeat(64)).expect("hex");
    let r = h.engine.sync_apply_as(&ajeno, 1, Actor::User).await;
    assert!(matches!(r, Err(ProtoError::PlanStale)), "hash ajeno");

    // Y un plan de OTRA conexión tampoco: el spool está atado a la que planificó.
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 2, Actor::User)
        .await;
    assert!(matches!(r, Err(ProtoError::PlanStale)), "otra conexión");
}

// 8 ───────────────────────────────────────────────────────────────────────
/// Un plan con bloqueos no se ejecuta, y el rechazo lo dice con su nombre: un
/// hash que case dice «este es el plan que se te enseñó», jamás «este plan se
/// puede ejecutar».
#[tokio::test]
async fn un_plan_no_ejecutable_se_rehusa_y_no_se_queda_aplicandose() {
    // Destino de SOLO LECTURA: un único bloqueo, `DestReadOnly`.
    let h = harness(CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::READ_ONLY).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");

    let done = plan(&h, SyncMode::Update).await;
    assert!(!done.executable);
    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(matches!(r, Err(ProtoError::PlanNotExecutable)), "bloqueado");

    // Y el derecho se devolvió: replanificar el mismo árbol vuelve a funcionar
    // (mismo digest — sin el `remove` del rechazo, esto sería un fallo visible).
    let otra = plan(&h, SyncMode::Update).await;
    assert_eq!(otra.plan_hash, done.plan_hash);
}

// 9 ───────────────────────────────────────────────────────────────────────
/// **El gate corre sobre las raíces que salen del SPOOL, al aplicar.** El de
/// `sync.plan` no vale: entre planificar y aplicar caduca un scope y cambia una
/// regla de `policy.toml`, y `sync.apply` no lleva ninguna ruta con la que
/// gatear — solo un hash. La policy de aquí deniega EXACTAMENTE la raíz de
/// destino, así que solo puede haberla visto un gate que sacara esa raíz del
/// fichero.
#[tokio::test]
async fn el_gate_del_apply_corre_sobre_las_raices_del_spool() {
    use norte_core::policy::{Decision, DenyReason, PolicyGate, PolicyOp};

    /// Deniega toda mutación que toque `mem:///d`, y solo esa.
    struct DenyDest;
    impl PolicyGate for DenyDest {
        fn evaluate(&self, _actor: &Actor, _op: PolicyOp, paths: &[&VPath]) -> Decision {
            if paths.iter().any(|p| p.to_wire().starts_with("mem:///d")) {
                Decision::Deny(DenyReason::OutOfScope)
            } else {
                Decision::Allow
            }
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal))
        .with_policy(Arc::new(DenyDest), Arc::new(norte_core::approval::DenyAll));
    let mem = Arc::new(MemProvider::with_flags(with_trash()).with_logical_trash());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(dir.path()));
    let h = Harness {
        engine,
        mem,
        journal,
        _dir: dir,
    };
    seed(&h).await;

    // Planificar SÍ se puede: `sync.plan` no tiene gate de mutación (el de
    // lectura vive en el daemon, que es quien ata una conexión a un actor).
    let done = plan(&h, SyncMode::Update).await;
    assert!(done.executable);

    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(
        matches!(r, Err(ProtoError::PolicyDenied { .. })),
        "el gate del apply lo ve"
    );
    assert!(
        !exists(&h.mem, "mem:///d/nueva").await,
        "no se escribió nada"
    );
    // Y el derecho a aplicar se devolvió: un rechazo no deja el hash atascado
    // en «aplicándose» (si lo dejara, esto sería `PlanStale`).
    let otra = plan(&h, SyncMode::Update).await;
    assert_eq!(otra.plan_hash, done.plan_hash);
}

// 10 ──────────────────────────────────────────────────────────────────────
/// Sin journal no se aplica (regla dura 4): el plan promete una reversa por paso
/// y solo el journal la puede cumplir. Fail-closed, como el spool.
#[tokio::test]
async fn sin_journal_no_se_aplica() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(with_trash()).with_logical_trash());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(dir.path()));
    mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///s/a.txt", b"a").await;

    let (handle, mut rx) = engine
        .sync_plan_as(params(SyncMode::Update), 1, Actor::User)
        .await
        .expect("sync.plan");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    handle.join().await;
    let done = done.expect("plan_done");

    let r = engine.sync_apply_as(&done.plan_hash, 1, Actor::User).await;
    assert!(matches!(r, Err(ProtoError::Unsupported)), "sin journal");
}

// 11 ──────────────────────────────────────────────────────────────────────
/// Cancelar deja el lote CERRADO y deshacible: lo aplicado antes del corte está
/// journalizado bajo su `batch_id`, y no se desanda nada (media sincronización
/// es un estado real). Y el plan se gasta igual.
#[tokio::test]
async fn cancelar_deja_un_lote_cerrado_y_gasta_el_plan() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    for i in 0..200 {
        write_file(&h.mem, &format!("mem:///s/f{i:03}.txt"), b"x").await;
    }
    let done = plan(&h, SyncMode::Update).await;

    let (handle, report) = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await
        .expect("sync.apply");
    handle.cancel();
    let state = handle.join().await;
    assert_eq!(state, TaskState::Cancelled);

    let report = report.lock().expect("lock").clone();
    let es = entries(&h).await;
    assert_eq!(
        es.len() as u64,
        report.done,
        "cada paso hecho dejó su entrada"
    );
    assert!(
        es.iter().all(|e| e.batch_id == report.batch_id),
        "un solo lote"
    );
    // El plan se gastó: la cancelación es un estado terminal como cualquier otro.
    let again = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(matches!(again, Err(ProtoError::PlanStale)));
}

// 12 ──────────────────────────────────────────────────────────────────────
/// Un `Skip` no toca nada y no journaliza nada: cuenta en `skipped`, no en
/// `done`, y el árbol de destino queda como estaba.
#[tokio::test]
async fn un_skip_no_toca_nada_ni_deja_entrada() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///s/oscura")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/oscura/x.txt", b"x").await;
    write_file(&h.mem, "mem:///s/a.txt", b"a").await;
    // Un directorio del ORIGEN que no se deja listar: la comparación emite una
    // fila de error y el transductor la convierte en un `Skip` `Unreadable`.
    h.mem.faults().fail_list_at(&vp("mem:///s/oscura"));

    let done = plan(&h, SyncMode::Update).await;
    assert!(
        done.counts.skip >= 1,
        "hay al menos un Skip: {:?}",
        done.counts
    );
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(
        report.skipped, done.counts.skip,
        "los Skip se cuentan aparte"
    );
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(
        !exists(&h.mem, "mem:///d/oscura/x.txt").await,
        "un Skip no escribe"
    );
    assert_eq!(
        entries(&h).await.len() as u64,
        report.done,
        "solo lo hecho deja entrada"
    );
}

// 13 ──────────────────────────────────────────────────────────────────────
/// **El lote es deshacible de verdad** (tarea 11). Se aplica el plan entero y
/// se deshace la sesión: el árbol vuelve exactamente a como estaba, incluida la
/// pareja del `Overwrite` —que solo sale bien si el undo borra lo CREADO antes
/// de restaurar lo ENTERRADO— y el directorio, que se vacía antes de irse.
#[tokio::test]
async fn deshacer_una_sincronizacion_devuelve_el_arbol() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.blocked, None, "nada bloqueó");
    assert_eq!(undone.undone, 4, "trashed + created + createdir + copy");
    assert_eq!(undone.skipped_irreversible, 0);
    assert!(undone.unreverted_paths.is_empty(), "todo volvió");

    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"destino",
        "lo enterrado volvió a su sitio",
    );
    assert!(
        !exists(&h.mem, "mem:///d/nueva/a.txt").await,
        "la copia se fue"
    );
    assert!(
        !exists(&h.mem, "mem:///d/nueva").await,
        "y el directorio detrás de ella"
    );
    assert!(
        exists(&h.mem, "mem:///d/sobra.txt").await,
        "lo que la sincronización no tocó, el undo tampoco"
    );

    // Segundo undo: todo compensado, nada que hacer.
    let (state, again) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(again.undone, 0);
}

// 14 ──────────────────────────────────────────────────────────────────────
/// El undo de una sincronización es a su vez UN LOTE: todas las compensaciones
/// comparten un `batch_id` FRESCO y cada una dice a qué `seq` compensa. Sin lo
/// primero, deshacer el undo se partiría en cuatro unidades; sin lo segundo,
/// las compensaciones parecerían mutaciones nuevas y deshacibles.
#[tokio::test]
async fn el_undo_de_una_sincronizacion_es_a_su_vez_un_lote() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (_state, report) = apply(&h, &done.plan_hash).await;
    let aplicado = report.batch_id.expect("el lote de la ida");

    let (state, _undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);

    let comp: Vec<JournalEntry> = entries(&h)
        .await
        .into_iter()
        .filter(|e| e.undoes_seq.is_some())
        .collect();
    assert_eq!(comp.len(), 4, "una compensación por entrada: {comp:?}");
    let lote = comp[0].batch_id.expect("las compensaciones van en lote");
    assert!(
        comp.iter().all(|e| e.batch_id == Some(lote)),
        "todas bajo el MISMO lote: {comp:?}",
    );
    assert_ne!(lote, aplicado, "y un lote FRESCO, no el de la ida");
}

// 15 ──────────────────────────────────────────────────────────────────────
/// **Sin papelera en el destino, el undo no devuelve NADA — ni siquiera las
/// copias.** Una entrada `irreversible` no tiene qué restaurar, y un `created`
/// sin papelera no se borra permanente (#65: bajo esa ruta puede vivir ya
/// trabajo del humano). Lo que la sincronización promete como reversa `Delete`
/// se queda en promesa, así que el undo lo cuenta y lo NOMBRA en vez de fingir.
#[tokio::test]
async fn sin_papelera_el_undo_nombra_lo_que_no_puede_devolver() {
    let h = harness(without_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"nuevo").await;
    write_file(&h.mem, "mem:///s/comun.txt", b"origen-mas-largo").await;
    write_file(&h.mem, "mem:///d/comun.txt", b"destino").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 1, "la sobrescritura, y solo ella");
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed, "no rehúsa: informa");
    assert_eq!(undone.undone, 0, "nada volvió");
    assert_eq!(undone.skipped_irreversible, 1, "la sobrescritura");
    assert_eq!(
        undone.skipped_created_no_trash, 1,
        "y la COPIA, que el plan enseñaba como reversible"
    );
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/comun.txt".to_vec(), b"mem:///d/a.txt".to_vec()],
        "las dos, en el orden en que se intentaron (seq descendente)",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"origen-mas-largo"
    );
    assert!(exists(&h.mem, "mem:///d/a.txt").await, "nada se borró");
}

// 16 ──────────────────────────────────────────────────────────────────────
/// **Un paso que no vuelve no secuestra a los demás.** Es la diferencia entera
/// con el undo de un lote de renombrados: media permutación deshecha no es
/// ningún estado, media sincronización deshecha sí. Aquí alguien borra a mano
/// un fichero copiado entre aplicar y deshacer: esa entrada bloquea, y las
/// otras tres vuelven igual.
#[tokio::test]
async fn un_paso_que_no_vuelve_no_secuestra_al_resto() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);

    // Drift: el humano borra la copia por su cuenta.
    h.mem
        .remove(&vp("mem:///d/nueva/a.txt"))
        .await
        .expect("remove");

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 3, "las otras tres entradas SÍ volvieron");
    let (seq, error) = undone.blocked.expect("la que no volvió, nombrada");
    assert!(matches!(error, ProtoError::NotFound), "{error:?}");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/nueva/a.txt".to_vec()],
        "y con nombre, no solo con un seq ({seq})",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"destino",
        "la sobrescritura se deshizo aunque otra entrada bloqueara",
    );
}

// 17 ──────────────────────────────────────────────────────────────────────
/// **Una papelera que no dice dónde dejó lo que enterró lo dice ANTES, no
/// después.** Esto era el BLOCKER de la tarea 11: el plan prometía
/// `RestoreTrash`, el journal se quedaba sin `reversal_ref` y el undo casaba
/// por ruta original —eligiendo el más reciente, que para entonces era el
/// fichero que él mismo acababa de enterrar—; el usuario veía un éxito y su
/// original seguía en la papelera. Ahora el destino declara que su papelera no
/// nombra nada (`trash_restorable` en `false`) y el plan sale entero
/// `Irreversible` con su motivo, que es lo que la regla dura 4 pide: o hay
/// undo, o hay una clasificación explícita ANTES de aprobar.
///
/// Y lo que NO cambia: el borrado sigue yendo a la papelera. Perder el undo no
/// es razón para borrar permanente lo que se podía enterrar.
#[tokio::test]
async fn una_papelera_que_no_nombra_su_destino_lo_dice_en_el_plan() {
    let h = harness_with(with_trash(), false).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/comun.txt", b"origen-mas-largo").await;
    write_file(&h.mem, "mem:///d/comun.txt", b"destino").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(
        done.counts.irreversible, 1,
        "el plan lo dice antes de que nadie apruebe: {:?}",
        done.counts
    );
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "UNA entrada, irreversible: {es:?}");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].reversal.as_str(), "irreversible");
    assert!(es[0].reversal_ref.is_none());

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed, "no rehúsa: informa");
    assert_eq!(undone.undone, 0, "no hay nada que devolver…");
    assert_eq!(undone.skipped_irreversible, 1, "…y se cuenta");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/comun.txt".to_vec()],
        "nombrada UNA vez, la del fichero que el usuario quiere de vuelta",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"origen-mas-largo",
        "la ruta NO se queda vacía: lo sincronizado sigue ahí",
    );
}

/// Y la contraparte que arregla el BLOCKER de verdad: una papelera que SÍ
/// nombra su destino deshace la pareja entera, sin adivinar.
#[tokio::test]
async fn una_papelera_que_nombra_su_destino_deshace_la_sobrescritura() {
    let h = harness_with(with_trash(), true).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/comun.txt", b"origen-mas-largo").await;
    write_file(&h.mem, "mem:///d/comun.txt", b"destino").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 0, "{:?}", done.counts);
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    let es = entries(&h).await;
    assert_eq!(es.len(), 2, "trashed + created: {es:?}");
    assert!(
        es[0].reversal_ref.is_some(),
        "el destino recuperable llegó al journal"
    );

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 2, "las dos mitades");
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"destino",
        "el fichero del USUARIO, no el que el undo acababa de enterrar",
    );
}

// 18 ──────────────────────────────────────────────────────────────────────
/// **Un directorio creado no se manda a la papelera con contenido ajeno
/// dentro.** El orden inverso lo deja vacío cuando todo va bien; cuando no
/// —aquí el usuario metió un fichero suyo entre aplicar y deshacer— la
/// papelera se llevaría también eso, y el informe no lo nombraría. Se bloquea
/// esa entrada y las demás siguen.
#[tokio::test]
async fn un_directorio_creado_con_contenido_ajeno_no_se_entierra() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);

    // El humano deja algo suyo dentro del directorio que la sincronización creó.
    write_file(&h.mem, "mem:///d/nueva/notas.txt", b"mias").await;

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 3, "todo menos el directorio");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/nueva".to_vec()],
        "y el que no volvió, con nombre",
    );
    assert!(
        exists(&h.mem, "mem:///d/nueva/notas.txt").await,
        "el fichero del humano sigue donde lo dejó",
    );
    assert!(
        !exists(&h.mem, "mem:///d/nueva/a.txt").await,
        "la copia se fue"
    );
}

// 19 ──────────────────────────────────────────────────────────────────────
/// Cancelar a mitad de un lote (regla 3): corte ENTRE entradas, lo compensado
/// se queda compensado, la cadena sigue íntegra y lo que faltaba sigue siendo
/// deshacible — un segundo undo lo termina. Cuánto entró en cada mitad depende
/// del reloj; que la suma sea el lote entero, no.
#[tokio::test]
async fn cancelar_el_undo_de_un_lote_lo_deja_terminable() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(apply(&h, &done.plan_hash).await.0, TaskState::Completed);
    let total = entries(&h).await.len() as u64;

    // Latencia por op → ventana determinista para cancelar antes de terminar.
    h.mem
        .faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(40)));
    let (handle, report) = h
        .engine
        .undo_session(Actor::User)
        .await
        .expect("undo aceptado");
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    handle.cancel();
    let state = handle.join().await;
    let first = report.lock().expect("undo report lock").clone();
    h.mem.faults().set_latency_per_op(None);

    assert_eq!(state, TaskState::Cancelled, "corte cooperativo limpio");
    assert!(first.blocked.is_none(), "cancelar no es bloquear");
    assert!(
        h.journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
        "la cadena del journal aguanta el corte",
    );

    let (state, second) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(
        first.undone + second.undone,
        total,
        "entre las dos mitades, el lote entero",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/comun.txt").await,
        b"destino",
        "y el árbol acaba donde estaba",
    );
    assert!(!exists(&h.mem, "mem:///d/nueva").await);
}
