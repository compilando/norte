//! Batch rename end to end inside the core: engine → planner → executor →
//! journal, over `MemProvider`.
//!
//! The unit tests in `rename::exec` pin the transaction's corners (a journal
//! write that does not land, a rollback the provider refuses, cancellation
//! between steps). What these prove is the WIRING: that the engine really
//! re-plans, really binds the token to the directory, really gates once for
//! every path the batch touches, and that the whole thing arrives at the
//! journal as one group.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_core::journal::{Journal, SqliteJournal};
use norte_proto::{Error, Segment, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

/// `TaskHandle` has no `Debug`, so `expect_err` cannot be used on a submit.
fn refusal<T>(r: Result<T, Error>) -> Error {
    match r {
        Ok(_) => panic!("se esperaba un rechazo y la tarea se aceptó"),
        Err(e) => e,
    }
}

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segment")
}

fn pairs(v: &[(&[u8], &[u8])]) -> Vec<(Vec<u8>, Vec<u8>)> {
    v.iter().map(|(f, t)| (f.to_vec(), t.to_vec())).collect()
}

/// Seeds a file whose CONTENT is its original name, so a test can tell which
/// file ended up where — names alone would pass even if nothing moved.
async fn write_file(mem: &MemProvider, path: &VPath, content: &[u8]) {
    let mut sink = mem.write(path).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(mem: &MemProvider, path: &VPath) -> Vec<u8> {
    let mut stream = mem.read(path, None).await.expect("read abre");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

/// Engine with a journal and a `MemProvider` seeded with `names` in its root.
async fn engine_with(names: &[&[u8]]) -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>, VPath) {
    let provider = Arc::new(MemProvider::new());
    let dir = MemProvider::root();
    for n in names {
        write_file(&provider, &dir.join(seg(n)), n).await;
    }
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(Arc::clone(&provider) as Arc<dyn Provider>);
    (engine, provider, journal, dir)
}

/// Applies `from → to` inside `dir` and journals it as the executor would,
/// optionally inside `batch`. Lets a test build a journal whose entry ORDER is
/// exact instead of racing two tasks for it.
async fn rename_and_record(
    provider: &MemProvider,
    journal: &SqliteJournal,
    dir: &VPath,
    from: &[u8],
    to: &[u8],
    batch: Option<i64>,
) {
    let (src, dst) = (dir.join(seg(from)), dir.join(seg(to)));
    provider.rename(&src, &dst).await.expect("rename");
    let (path, path_to) = (dst.to_wire().into_bytes(), src.to_wire().into_bytes());
    journal
        .journal()
        .record_entry(&norte_core::NewEntry {
            op: "renamed",
            path: &path,
            path_to: Some(&path_to),
            reversal: norte_core::Reversal::RenameBack,
            reversal_ref: None,
            actor: &norte_core::journal::Actor::User,
            undoes_seq: None,
            batch_id: batch,
        })
        .await
        .expect("record");
}

/// The directory's base names, sorted.
async fn names_in(provider: &MemProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = provider.list(dir).await.expect("list");
    let mut v = Vec::new();
    while let Some(e) = stream.next().await {
        let e = e.expect("entry");
        if let Some(n) = e.path.file_name() {
            v.push(n.as_bytes().to_vec());
        }
    }
    v.sort();
    v
}

/// A permutation — the case the per-move loop could never do — lands, and it
/// lands as ONE journal batch.
#[tokio::test]
async fn a_permutation_lands_and_is_one_batch() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    assert!(plan.executable(), "{:?}", plan.plan().collisions);
    let (handle, report) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"a".to_vec(), b"b".to_vec()],
    );
    // The permutation actually swapped: the file seeded as `a` answers to `b`.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"b"))).await,
        b"a".to_vec()
    );
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"b".to_vec()
    );

    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.applied, 3, "dos renames más el rodeo");
    assert_eq!(r.rolled_back, 0);
    assert!(r.stuck.is_none());

    let es = journal.journal().entries().await.expect("entries");
    let batches: std::collections::HashSet<_> = es.iter().filter_map(|e| e.batch_id).collect();
    assert_eq!(batches.len(), 1, "un lote para toda la permutación");
    // La DIRECCIÓN, no solo el recuento. `undo.rs` revierte un `"renamed"` con
    // `rename(entry.path → entry.path_to)`: con los dos campos cambiados, la
    // cadena queda perfectamente íntegra y el undo de la sesión renombra todo
    // al revés. Contar entradas no lo ve; comparar bytes sí.
    let wire = |n: &[u8]| dir.join(seg(n)).to_wire().into_bytes();
    for (entry, step) in es.iter().zip(plan.plan().steps.iter()) {
        assert_eq!(entry.op, "renamed");
        assert_eq!(entry.reversal, "rename_back");
        assert_eq!(
            (entry.path.clone(), entry.path_to.clone()),
            (wire(&step.to), Some(wire(&step.from))),
            "path = DESTINO, path_to = ORIGEN",
        );
    }
    assert_eq!(
        es.iter().filter(|e| e.batch_id.is_some()).count(),
        3,
        "dos renames más el rodeo, todos journalizados",
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
}

/// Rule 1 through the whole stack: a permutation of names that are not UTF-8
/// crosses planner, executor and journal byte for byte.
#[tokio::test]
async fn a_permutation_of_hostile_names_survives_byte_for_byte() {
    let one = b"caf\xff\xfe.txt".to_vec();
    let two = b"\xed\xa0\x80-lone.bin".to_vec();
    let (engine, provider, journal, dir) = engine_with(&[&one, &two]).await;
    let ps = vec![(one.clone(), two.clone()), (two.clone(), one.clone())];
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    assert!(plan.executable(), "{:?}", plan.plan().collisions);
    let (handle, _report) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);

    let mut want = vec![one.clone(), two.clone()];
    want.sort();
    assert_eq!(names_in(&provider, &dir).await, want);
    assert_eq!(
        read_all(&provider, &dir.join(seg(&two))).await,
        one,
        "el fichero que era `one` responde ahora a `two`",
    );
    // Y el journal guarda los bytes, no una conversión lossy de ellos.
    let es = journal.journal().entries().await.expect("entries");
    let holds = |needle: &[u8]| {
        es.iter()
            .any(|e| e.path.windows(needle.len()).any(|w| w == needle))
    };
    let encoded = norte_proto::VPath::root(norte_proto::Scheme::new("mem").expect("scheme"), None)
        .join(seg(&one))
        .to_wire()
        .into_bytes();
    assert!(holds(&encoded), "el nombre hostil viaja íntegro al journal");
}

/// The stale-plan guard: the directory changed after the preview, so the
/// execution refuses instead of doing something the human did not approve.
#[tokio::test]
async fn a_drifted_directory_refuses_with_plan_stale() {
    let (engine, provider, _j, dir) = engine_with(&[b"a"]).await;
    let ps = pairs(&[(b"a", b"z")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    write_file(&provider, &dir.join(seg(b"z")), b"drift").await;
    let e = refusal(engine.rename_batch(&dir, &ps, plan.hash()).await);
    assert_eq!(e, Error::PlanStale);
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"a".to_vec(), b"z".to_vec()],
        "no se tocó nada",
    );
}

/// Drift that changes NO verdict does not invalidate the plan: the hash is over
/// conclusions, so an unrelated new file must not force the human to read the
/// same list twice.
#[tokio::test]
async fn an_unrelated_new_file_does_not_invalidate_the_plan() {
    let (engine, provider, _j, dir) = engine_with(&[b"a"]).await;
    let ps = pairs(&[(b"a", b"z")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    write_file(&provider, &dir.join(seg(b"unrelated")), b"x").await;
    let (handle, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"unrelated".to_vec(), b"z".to_vec()],
    );
}

/// The token is bound to the DIRECTORY: a plan approved for one directory is
/// not executable against another whose re-plan produces the same steps.
#[tokio::test]
async fn a_token_from_another_directory_is_stale() {
    let (engine, provider, _j, root) = engine_with(&[]).await;
    let (here, there) = (root.join(seg(b"here")), root.join(seg(b"there")));
    provider.mkdir(&here).await.expect("mkdir");
    provider.mkdir(&there).await.expect("mkdir");
    write_file(&provider, &here.join(seg(b"a")), b"here").await;
    write_file(&provider, &there.join(seg(b"a")), b"there").await;

    let ps = pairs(&[(b"a", b"z")]);
    let plan = engine.rename_batch_plan(&here, &ps).await.expect("plan");
    let other = engine.rename_batch_plan(&there, &ps).await.expect("plan");
    assert_eq!(
        plan.plan(),
        other.plan(),
        "el planificador es puro: mismos pasos, mismos veredictos",
    );
    assert_ne!(plan.hash(), other.hash(), "el token no es el mismo");

    let e = refusal(engine.rename_batch(&there, &ps, plan.hash()).await);
    assert_eq!(e, Error::PlanStale);
    assert_eq!(names_in(&provider, &there).await, vec![b"a".to_vec()]);
}

/// A plan with a collision is refused before any effect, with its own error.
#[tokio::test]
async fn a_colliding_plan_is_refused_before_any_effect() {
    let (engine, provider, _j, dir) = engine_with(&[b"a", b"z"]).await;
    let ps = pairs(&[(b"a", b"z")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    assert!(!plan.executable());
    let e = refusal(engine.rename_batch(&dir, &ps, plan.hash()).await);
    assert_eq!(e, Error::PlanNotExecutable);
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"a".to_vec(), b"z".to_vec()],
    );
}

/// A name that is not a directory entry never reaches the planner.
#[tokio::test]
async fn a_pair_that_is_not_a_directory_entry_is_refused() {
    let (engine, _p, _j, dir) = engine_with(&[b"a"]).await;
    for bad in [
        b"..".to_vec(),
        b"x/y".to_vec(),
        b"nul\0".to_vec(),
        Vec::new(),
    ] {
        let ps = vec![(b"a".to_vec(), bad.clone())];
        assert_eq!(
            engine
                .rename_batch_plan(&dir, &ps)
                .await
                .expect_err("refuse"),
            Error::InvalidPath,
            "{bad:?}",
        );
    }
}

/// Step k fails → the directory is IDENTICAL to how it started. This is the
/// whole point of the feature.
#[tokio::test]
async fn a_failing_step_rolls_the_whole_batch_back() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b", b"c"]).await;
    let before = names_in(&provider, &dir).await;
    let ps = pairs(&[(b"a", b"x"), (b"b", b"y"), (b"c", b"d")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    // `c → d` is the last step (independent pairs keep the caller's order).
    assert_eq!(
        plan.plan().steps.last().expect("un paso").from,
        b"c".to_vec()
    );
    provider.faults().fail_rename_at(&dir.join(seg(b"c")));

    let (handle, report) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    match handle.join().await {
        TaskState::Failed { error } => assert!(matches!(error, Error::Io { .. }), "{error:?}"),
        other => panic!("se esperaba Failed, llegó {other:?}"),
    }
    assert_eq!(
        names_in(&provider, &dir).await,
        before,
        "desandado hasta el principio",
    );
    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.applied, 2);
    assert_eq!(r.rolled_back, 2);
    assert_eq!(r.compensations_lost, 0);
    assert!(r.uncertain.is_none());
    assert_eq!(
        r.failed_pair,
        Some(2),
        "la fila `c → d` que el usuario escribió"
    );
    assert!(r.stuck.is_none());

    // The journal tells the truth: every applied step and every compensation.
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(
        es.iter().filter(|e| e.undoes_seq.is_some()).count(),
        2,
        "dos pasos aplicados, dos compensaciones",
    );
    assert_eq!(
        es.iter().filter(|e| e.batch_id.is_some()).count(),
        4,
        "los cuatro apuntes van dentro del lote",
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
        "la cadena sobrevive a un rollback",
    );
    // Y nada del lote queda pendiente de deshacer: los originales están
    // compensados, así que un `undo_session` posterior no los ve.
    assert!(
        journal
            .journal()
            .revertible_for(&norte_core::journal::Actor::User)
            .await
            .expect("revertible")
            .is_empty(),
        "un lote desandado no deja trabajo al undo",
    );
}

/// Cancellation is the same rollback: a cancelled batch leaves the tree as it
/// was (rule 3, and the same promise a cancelled copy makes).
///
/// The margin is made of STEPS, not of milliseconds. Cancelling after the first
/// destination appears leaves ~200 renames still to run, each preceded by a
/// check of the token, so losing the race would take the test process being
/// descheduled for the whole remaining tail — and the tail grows with the pair
/// count rather than with the injected latency, which is the knob somebody
/// might lower later. The rollback stays at one step whatever the count.
/// (`rename::exec`'s unit test pins the same property with no clock at all,
/// cancelling from inside the recorder; what THIS one adds is the wiring:
/// `TaskHandle::cancel` → `TaskState::Cancelled` → tree restored.)
#[tokio::test]
async fn a_cancelled_batch_rolls_back() {
    let names: Vec<Vec<u8>> = (0..200u16).map(|i| format!("f{i}").into_bytes()).collect();
    let refs: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    let (engine, provider, journal, dir) = engine_with(&refs).await;
    let before = names_in(&provider, &dir).await;
    let ps: Vec<(Vec<u8>, Vec<u8>)> = names
        .iter()
        .map(|n| {
            let mut to = b"r-".to_vec();
            to.extend_from_slice(n);
            (n.clone(), to)
        })
        .collect();
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let first_dest = dir.join(seg(&plan.plan().steps[0].to));

    provider
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(10)));
    let (handle, report) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    // No blind sleep: poll a REAL condition — the first destination existing.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while provider.stat(&first_dest).await.is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "el primer paso nunca llegó a aplicarse",
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    provider.faults().clear();
    assert_eq!(names_in(&provider, &dir).await, before);
    let r = report.lock().expect("report lock").clone();
    assert!(r.applied >= 1, "el sondeo vio aterrizar un paso");
    assert_eq!(r.rolled_back, r.applied, "y todos volvieron");
    assert!(r.stuck.is_none(), "{:?}", r.stuck);
    assert_eq!(r.compensations_lost, 0);
    // El camino cancelado corre el MISMO código de compensación que el fallido,
    // y hasta ahora nadie miraba si dejaba entradas sin compensar.
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
    assert!(
        journal
            .journal()
            .revertible_for(&norte_core::journal::Actor::User)
            .await
            .expect("revertible")
            .is_empty(),
        "un lote cancelado no deja trabajo al undo",
    );
}

// ---- session undo consumes the batch as ONE block --------------------------

/// Undo of a batch is ONE step for the user: the permutation goes back whole.
///
/// It is also the case a per-entry undo can never do. Walking the three
/// entries of a swap in LIFO order reverts `T → b` first, which frees nothing
/// and puts the file back under a temporary name — and the next entry then
/// finds its destination occupied. The batch has to reach the undo as one item.
#[tokio::test]
async fn undoing_a_session_reverts_the_whole_batch() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let before = names_in(&provider, &dir).await;
    let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let (applied, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);

    let (handle, report) = engine
        .undo_session(norte_core::journal::Actor::User)
        .await
        .expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(names_in(&provider, &dir).await, before);
    // Y cada fichero volvió a SU nombre, no solo el conjunto de nombres.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"a".to_vec()
    );
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"b"))).await,
        b"b".to_vec()
    );
    let r = report.lock().expect("report lock").clone();
    assert!(r.blocked.is_none(), "{:?}", r.blocked);
    assert_eq!(r.undone, 3, "las tres entradas del lote, deshechas");

    // El undo es a su vez UN lote, y compensa las tres entradas originales, así
    // que un segundo undo no tiene nada que hacer.
    let es = journal.journal().entries().await.expect("entries");
    let batches: std::collections::HashSet<_> = es.iter().filter_map(|e| e.batch_id).collect();
    assert_eq!(batches.len(), 2, "el lote original y el que lo deshace");
    assert_eq!(
        es.iter().filter(|e| e.undoes_seq.is_some()).count(),
        3,
        "una compensación por entrada original",
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
    assert!(
        journal
            .journal()
            .revertible_for(&norte_core::journal::Actor::User)
            .await
            .expect("revertible")
            .is_empty(),
        "un lote deshecho no deja trabajo a un undo posterior",
    );
}

/// All or nothing: if ONE member of the batch cannot be reverted, the batch is
/// not touched at all — no half-undo.
#[tokio::test]
async fn a_batch_whose_member_is_blocked_is_left_untouched() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let ps = pairs(&[(b"a", b"x"), (b"b", b"y")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let (applied, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);
    // Alguien devolvió `a` a mano: revertir `x → a` lo pisaría.
    write_file(&provider, &dir.join(seg(b"a")), b"mine").await;
    let after_squat = names_in(&provider, &dir).await;

    let (handle, report) = engine
        .undo_session(norte_core::journal::Actor::User)
        .await
        .expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        after_squat,
        "el lote bloqueado no se deshace a medias",
    );
    // Y `y` sigue siendo `y`: el miembro que SÍ se podía revertir tampoco se
    // tocó. Esta es la propiedad; el listado de nombres por sí solo la deja
    // pasar si alguien revierte `y → b` y luego se queda atascado en `x`.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"y"))).await,
        b"b".to_vec(),
    );
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"mine".to_vec(),
        "el fichero del usuario sigue intacto",
    );
    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.undone, 0, "nada del lote se deshizo");
    let (seq, err) = r.blocked.expect("el lote se bloquea");
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    // El `seq` señala la entrada CONCRETA que no se puede revertir, no el lote
    // entero: es lo que el humano tiene que ir a mirar.
    let es = journal.journal().entries().await.expect("entries");
    let blocked_entry = es.iter().find(|e| e.seq == seq).expect("la entrada");
    assert_eq!(
        blocked_entry.path,
        dir.join(seg(b"x")).to_wire().into_bytes(),
        "la entrada bloqueada es la de `a → x`",
    );

    // Nada se compensó, así que el lote sigue pendiente entero: un undo
    // posterior (una vez quitado el ocupante) lo encuentra completo.
    assert_eq!(
        journal
            .journal()
            .revertible_for(&norte_core::journal::Actor::User)
            .await
            .expect("revertible")
            .len(),
        2,
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
}

/// Lo que para el lote bloqueado es el `feasible` de `undo.rs`, y NO el rechazo
/// del provider.
///
/// `a_batch_whose_member_is_blocked_is_left_untouched` no distingue las dos
/// cosas: sobre un `MemProvider` que ya rechaza un destino ocupado, borrar la
/// simulación entera deja ese test verde igual, porque el paso que sobra falla
/// en el provider y el ejecutor desanda lo que llevaba. El árbol acaba en el
/// mismo sitio por un camino peor — pasando de verdad por el estado medio
/// deshecho que el todo-o-nada existe para no visitar nunca.
///
/// Aquí el provider PISA (`rename_clobbers`, el posix-rename de sftp y el
/// copy+delete de object): el cinturón de abajo desaparece y el fichero del
/// usuario se destruye si la unidad llega a ejecutarse. Sin red debajo, lo
/// único que puede dejar el directorio intacto es haber respondido ANTES de
/// tocar el provider.
#[tokio::test]
async fn a_blocked_batch_is_stopped_by_the_gate_not_by_the_provider() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let user = norte_core::journal::Actor::User;
    let ps = pairs(&[(b"a", b"x"), (b"b", b"y")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let (applied, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);
    // Alguien devolvió `a` a mano: revertir `x → a` lo pisaría.
    write_file(&provider, &dir.join(seg(b"a")), b"mine").await;
    // …y este provider PISA en vez de rechazar. El no-clobber ya no lo pone él.
    provider.faults().rename_clobbers(true);
    let after_squat = names_in(&provider, &dir).await;

    let (handle, report) = engine.undo_session(user.clone()).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        after_squat,
        "el lote bloqueado no se deshace a medias",
    );
    // LA propiedad: el fichero del usuario sigue vivo. Con la simulación
    // neutralizada, el paso `x → a` lo habría BORRADO — el `mine` se pierde y
    // en su sitio queda el contenido de `a`.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"mine".to_vec(),
        "el fichero del usuario sigue intacto: nadie llegó a renombrar encima",
    );
    // Y el miembro que SÍ se podía revertir tampoco se tocó.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"y"))).await,
        b"b".to_vec(),
    );
    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.undone, 0, "nada del lote se deshizo");
    assert!(r.batch_stuck.is_none(), "{:?}", r.batch_stuck);
    assert_eq!(r.compensations_lost, 0);
    let (_seq, err) = r.blocked.expect("el lote se bloquea");
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    // El lote sigue pendiente ENTERO: no se aplicó ni se compensó ni un paso.
    assert_eq!(
        journal
            .journal()
            .revertible_for(&user)
            .await
            .expect("revertible")
            .len(),
        2,
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
}

/// Un lote PARTIDO por una mutación intercalada, con un miembro bloqueado: o
/// vuelve entero o no vuelve nada.
///
/// Es la mitad que `a_batch_interleaved_with_another_mutation_is_still_one_unit`
/// no llega a probar. Allí la suelta es inocua, así que agrupar por entradas
/// CONTIGUAS parte el lote en dos y aun así el resultado sale bien: los
/// fragmentos se aplican en el mismo orden LIFO global y cada uno cabe por su
/// cuenta. El agrupado por contigüidad solo se delata cuando un fragmento cabe
/// y el otro no — y entonces el primero ya se aplicó.
///
/// Con el ocupante en `a`, agrupar por contigüidad devuelve `y → b` (fragmento
/// nuevo, viable) y se atasca luego en `x → a` (fragmento viejo): media
/// permutación deshecha, que es el estado que toda esta funcionalidad existe
/// para impedir. Agrupando por `batch_id` la unidad se juzga entera y no se
/// toca nada.
///
/// El entrelazado se FABRICA (efectos reales + apuntes a mano) en vez de
/// provocarse con dos tasks concurrentes: la carrera que interesa es un
/// instante, y perseguirla con relojes es un test que un día se pone rojo por
/// carga.
#[tokio::test]
async fn an_interleaved_batch_with_a_blocked_member_is_never_half_undone() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b", b"lone"]).await;
    let user = norte_core::journal::Actor::User;
    let batch = journal.journal().alloc_batch().await.expect("alloc");

    // Lote de dos renames independientes, con la suelta EN MEDIO.
    rename_and_record(&provider, &journal, &dir, b"a", b"x", Some(batch)).await;
    rename_and_record(&provider, &journal, &dir, b"lone", b"lone2", None).await;
    rename_and_record(&provider, &journal, &dir, b"b", b"y", Some(batch)).await;
    // Alguien devolvió `a` a mano: el miembro más VIEJO del lote ya no cabe.
    write_file(&provider, &dir.join(seg(b"a")), b"mine").await;
    let before = names_in(&provider, &dir).await;

    let (handle, report) = engine.undo_session(user.clone()).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        before,
        "el lote partido no se deshace a medias",
    );
    // El miembro NUEVO del lote es el que un agrupado por contigüidad habría
    // revertido solo: `y` tiene que seguir siendo `y`.
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"y"))).await,
        b"b".to_vec(),
        "el fragmento viable del lote tampoco se toca",
    );
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"mine".to_vec(),
    );
    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.undone, 0, "nada se deshizo: ni el lote ni la suelta");
    let (_seq, err) = r.blocked.expect("el lote se bloquea");
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    // Las tres siguen pendientes: las dos del lote y la suelta, que el LIFO
    // estricto no llega a tocar porque la unidad de arriba se bloqueó.
    assert_eq!(
        journal
            .journal()
            .revertible_for(&user)
            .await
            .expect("revertible")
            .len(),
        3,
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
}

/// Un undo de lote que se cae DESPUÉS de aplicar un paso deja el lote otra vez
/// deshacible: el ejecutor desanda lo que llevaba y las compensaciones que
/// escribió dejan de valer.
///
/// La trampa que esto pincha es de contabilidad, no de ficheros. El undo del
/// paso ya aplicado escribió una compensación `C` que TAPA su entrada original;
/// al desandarse, se escribe `D` que compensa a `C`. Con la condición ingenua
/// («existe alguna compensación») la original quedaba tapada para siempre por
/// una `C` que ya no vale: el árbol seguía con el lote aplicado y el journal
/// decía que no había nada que deshacer. Silencioso e irrecuperable.
#[tokio::test]
async fn an_undo_that_rolls_itself_back_leaves_the_batch_revertible() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let user = norte_core::journal::Actor::User;
    let ps = pairs(&[(b"a", b"x"), (b"b", b"y")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let (applied, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);
    let after_batch = names_in(&provider, &dir).await;

    // El segundo paso del undo (`x → a`) falla; el primero (`y → b`) ya se
    // aplicó y se desanda. `feasible` no lo ve: el ocupante no existe, es el
    // provider el que se cae.
    provider.faults().fail_rename_at(&dir.join(seg(b"x")));
    let (handle, report) = engine.undo_session(user.clone()).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    let r = report.lock().expect("report lock").clone();
    assert_eq!(r.undone, 0, "el lote no se deshizo");
    assert!(r.blocked.is_some());
    assert!(r.batch_stuck.is_none(), "{:?}", r.batch_stuck);
    assert_eq!(
        names_in(&provider, &dir).await,
        after_batch,
        "el ejecutor devolvió el paso que había aplicado",
    );

    // LA propiedad: el lote sigue pendiente ENTERO.
    let rev = journal
        .journal()
        .revertible_for(&user)
        .await
        .expect("revertible");
    assert_eq!(
        rev.len(),
        2,
        "las dos entradas del lote siguen por deshacer"
    );

    // Y quitado el fallo, el undo lo termina — sin haber pasado nunca por un
    // estado medio deshecho.
    provider.faults().clear();
    let (handle, report) = engine.undo_session(user.clone()).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert!(report.lock().expect("report lock").blocked.is_none());
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"a".to_vec(), b"b".to_vec()],
    );
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
    );
}

/// Un lote y una mutación suelta ENTRELAZADOS en el journal: el lote se sigue
/// consumiendo entero.
///
/// El agrupado no puede ser «entradas consecutivas». El scheduler corre hasta
/// cuatro tasks por provider, así que otra mutación del MISMO actor puede
/// aterrizar entre dos entradas del lote; con agrupado por contigüidad el lote
/// se partiría en dos unidades y la primera lo dejaría a medias — que es
/// exactamente lo que esta función existe para impedir.
///
/// El entrelazado se FABRICA (efectos reales sobre el provider + apuntes a
/// mano) en vez de provocarse con dos tasks concurrentes: la carrera que
/// interesa es un instante, y un test que la persiga con relojes es un test
/// que un día se pone rojo por carga.
#[tokio::test]
async fn a_batch_interleaved_with_another_mutation_is_still_one_unit() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b", b"lone"]).await;
    let user = norte_core::journal::Actor::User;
    let batch = journal.journal().alloc_batch().await.expect("alloc");
    let temp = b".norte-rename-tmp";
    let step = |from: &'static [u8], to: &'static [u8], b: Option<i64>| {
        rename_and_record(&provider, &journal, &dir, from, to, b)
    };

    // Permutación `a ↔ b` aplicada a mano, con la suelta EN MEDIO.
    step(b"a", temp, Some(batch)).await;
    // …y aquí se cuela la mutación suelta, entre la primera y la última del lote.
    step(b"lone", b"lone2", None).await;
    step(b"b", b"a", Some(batch)).await;
    step(temp, b"b", Some(batch)).await;

    let (handle, report) = engine.undo_session(user.clone()).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    let r = report.lock().expect("report lock").clone();
    assert!(r.blocked.is_none(), "{:?}", r.blocked);
    assert_eq!(r.undone, 4, "las tres del lote más la suelta");
    let mut want = vec![b"a".to_vec(), b"b".to_vec(), b"lone".to_vec()];
    want.sort();
    assert_eq!(names_in(&provider, &dir).await, want);
    assert_eq!(
        read_all(&provider, &dir.join(seg(b"a"))).await,
        b"a".to_vec(),
        "la permutación volvió del todo",
    );
    assert!(
        journal
            .journal()
            .revertible_for(&user)
            .await
            .expect("revertible")
            .is_empty(),
    );
}

// ---- the policy gate -------------------------------------------------------

/// A gate that records every path it was asked about and answers `verdict`.
struct Recording {
    seen: std::sync::Mutex<Vec<Vec<u8>>>,
    verdict: norte_core::policy::Decision,
}

impl norte_core::policy::PolicyGate for Recording {
    fn evaluate(
        &self,
        _actor: &norte_core::journal::Actor,
        _op: norte_core::policy::PolicyOp,
        paths: &[&VPath],
    ) -> norte_core::policy::Decision {
        let mut seen = self.seen.lock().expect("seen lock");
        for p in paths {
            if let Some(n) = p.file_name() {
                seen.push(n.as_bytes().to_vec());
            }
        }
        self.verdict.clone()
    }
}

async fn engine_gated(
    names: &[&[u8]],
    verdict: norte_core::policy::Decision,
) -> (Engine, Arc<MemProvider>, Arc<Recording>, VPath) {
    let provider = Arc::new(MemProvider::new());
    let dir = MemProvider::root();
    for n in names {
        write_file(&provider, &dir.join(seg(n)), n).await;
    }
    let gate = Arc::new(Recording {
        seen: std::sync::Mutex::new(Vec::new()),
        verdict,
    });
    let engine = Engine::new().with_policy(
        Arc::clone(&gate) as Arc<dyn norte_core::policy::PolicyGate>,
        Arc::new(norte_core::approval::DenyAll),
    );
    engine.register_provider(Arc::clone(&provider) as Arc<dyn Provider>);
    (engine, provider, gate, dir)
}

/// ONE gate for the whole batch, and it sees EVERY path the batch touches —
/// the planner's temporary names included, which are files created in the
/// user's directory and which no `pairs` list ever mentions.
///
/// Without this the claim is prose: a refactor that gated only the non-temp
/// steps would leave every test green while an agent scoped away from
/// `.norte-rename-*` renamed through it.
#[tokio::test]
async fn the_gate_sees_every_path_including_the_temporaries() {
    let (engine, _p, gate, dir) =
        engine_gated(&[b"a", b"b"], norte_core::policy::Decision::Allow).await;
    let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let temp = plan
        .plan()
        .steps
        .iter()
        .find(|s| s.temp && s.to.starts_with(b".norte-rename-"))
        .expect("un temporal")
        .to
        .clone();
    let (handle, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);

    let seen = gate.seen.lock().expect("seen lock").clone();
    assert!(
        seen.contains(&temp),
        "el temporal pasa por el gate: {seen:?}"
    );
    assert!(seen.contains(&b"a".to_vec()) && seen.contains(&b"b".to_vec()));
    // Un solo gate: las tres parejas de rutas llegan en UNA evaluación (2 por
    // paso × 3 pasos), no una evaluación por paso.
    assert_eq!(seen.len(), 6, "{seen:?}");
}

/// A denied batch is denied WHOLE and before any effect. The policy resolves a
/// slice to the most restrictive verdict, so one denied name stops everything.
#[tokio::test]
async fn a_denied_batch_applies_nothing() {
    let (engine, provider, _g, dir) = engine_gated(
        &[b"a", b"b"],
        norte_core::policy::Decision::Deny(norte_core::policy::DenyReason::OutOfScope),
    )
    .await;
    let before = names_in(&provider, &dir).await;
    let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let e = refusal(engine.rename_batch(&dir, &ps, plan.hash()).await);
    assert!(matches!(e, Error::PolicyDenied { .. }), "{e:?}");
    assert_eq!(names_in(&provider, &dir).await, before);
}

/// A provider whose `capabilities()` LIES until its first async operation —
/// which is not a contrivance but the shape `LocalProvider` really has: the
/// case regime is probed in `spawn_blocking` by the first async call, and until
/// that has run `capabilities()` answers `cfg!(target_os)`'s guess.
///
/// It exists to pin the ORDER inside `Engine::plan_for`. Asking for the caps
/// before listing the directory plans a case-INSENSITIVE volume as if it
/// distinguished case, which means a destination that is already taken is not
/// reported as a collision and the executor is handed a `rename` onto an
/// occupied name. The first operation on a provider is exactly the case an MCP
/// bridge or a one-shot CLI hits every time.
struct LazyCaps {
    inner: Arc<MemProvider>,
    probed: Arc<std::sync::atomic::AtomicBool>,
}

impl LazyCaps {
    fn probe(&self) {
        self.probed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl norte_vfs::Provider for LazyCaps {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    fn capabilities(&self) -> norte_vfs::Capabilities {
        if self.probed.load(std::sync::atomic::Ordering::SeqCst) {
            self.inner.capabilities()
        } else {
            // The unprobed guess: "this is a unix filesystem, so it
            // distinguishes case". Wrong here, and wrong on any folding volume.
            let mut caps = self.inner.capabilities();
            caps.flags |= norte_vfs::CapabilityFlags::CASE_SENSITIVE;
            caps
        }
    }

    async fn stat(&self, p: &VPath) -> Result<norte_vfs::Entry, Error> {
        self.probe();
        self.inner.stat(p).await
    }

    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.probe();
        self.inner.list(p).await
    }

    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.probe();
        self.inner.read(p, range).await
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.probe();
        self.inner.write(p).await
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.probe();
        self.inner.mkdir(p).await
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.probe();
        self.inner.remove(p).await
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.probe();
        self.inner.rename(from, to).await
    }
}

/// The FIRST plan a provider is ever asked for must use the case regime that
/// provider really has, not the one it guesses before it has looked.
///
/// `A.txt` sits in a directory that does not distinguish case, and the batch
/// aims `x.txt` at `a.txt`. That is an external collision, and there is no
/// second chance to notice it: the plan either refuses here or the executor
/// renames over a file.
#[tokio::test]
async fn the_first_plan_uses_the_probed_case_regime_not_the_guess() {
    use norte_vfs::CapabilityFlags as F;
    let inner = Arc::new(MemProvider::with_flags(
        F::RENAME_ATOMIC | F::CASE_PRESERVING,
    ));
    let dir = MemProvider::root();
    for n in [b"A.txt".as_slice(), b"x.txt".as_slice()] {
        write_file(&inner, &dir.join(seg(n)), n).await;
    }
    let probed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = LazyCaps {
        inner,
        probed: Arc::clone(&probed),
    };
    let engine = Engine::new();
    engine.register_provider(Arc::new(provider) as Arc<dyn Provider>);

    assert!(
        !probed.load(std::sync::atomic::Ordering::SeqCst),
        "the point of the test is that nothing has probed yet",
    );
    let ps = pairs(&[(b"x.txt", b"a.txt")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    assert!(
        !plan.executable(),
        "`A.txt` is in the way on a directory that folds case: {:?}",
        plan.plan().steps,
    );
    assert_eq!(
        plan.plan().collisions[0].kind,
        norte_core::rename::CollisionKind::External,
    );
}

/// ADR 0054: el undo pregunta al DIRECTORIO, igual que preguntó el plan.
///
/// Los dos juzgan el mismo lote y tienen que juzgarlo con el MISMO plegado: si
/// el plan lo calcula con el del directorio (un ext4/f2fs `+F`) y el undo con
/// el que el provider declara para sí, `feasible` opina sobre un directorio
/// que no es este. No se prueba con un escenario —un `+F` de verdad no puede
/// sostener a la vez las dos grafías, así que el caso «el inverso choca con el
/// gemelo» no se puede montar sin fingir un filesystem imposible—: se prueba
/// que la pregunta se hace, que es exactamente lo que se arregló.
#[tokio::test]
async fn el_undo_pregunta_por_el_directorio() {
    let (engine, provider, _journal, dir) = engine_with(&[b"a", b"b"]).await;
    let ps = pairs(&[(b"a", b"x"), (b"b", b"y")]);
    let plan = engine.rename_batch_plan(&dir, &ps).await.expect("plan");
    let (applied, _r) = engine
        .rename_batch(&dir, &ps, plan.hash())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);

    assert!(
        provider.was_asked_about(&dir),
        "el plan pregunta por la ubicación"
    );
    // Se olvida lo que preguntó el plan: lo que se afirma abajo es que el undo
    // vuelve a preguntar, no que alguien preguntara alguna vez.
    provider.forget_who_asked();

    let (handle, report) = engine
        .undo_session(norte_core::journal::Actor::User)
        .await
        .expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        report.lock().expect("report").undone,
        2,
        "y el undo revierte el lote entero"
    );
    assert!(
        provider.was_asked_about(&dir),
        "el undo pregunta por la MISMA ubicación que el plan"
    );
}
