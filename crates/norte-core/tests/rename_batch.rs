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
    assert!(plan.executable(), "{:?}", plan.plan.collisions);
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
    assert!(plan.executable(), "{:?}", plan.plan.collisions);
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
        plan.plan, other.plan,
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
    assert_eq!(plan.plan.steps.last().expect("un paso").from, b"c".to_vec());
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
/// The margin is structural rather than a slept-at guess: the batch is twelve
/// steps of injected latency, so after the first destination appears there are
/// eleven more `rename`s of ≥10 ms each still to go, and every one of them is
/// preceded by a check of the token.
#[tokio::test]
async fn a_cancelled_batch_rolls_back() {
    let names: Vec<Vec<u8>> = (0..12u8).map(|i| format!("f{i}").into_bytes()).collect();
    let refs: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    let (engine, provider, _j, dir) = engine_with(&refs).await;
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
    let first_dest = dir.join(seg(&plan.plan.steps[0].to));

    provider
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(10)));
    let (handle, _report) = engine
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
}
