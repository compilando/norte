//! `Engine::sync_apply_as` integration (task 9 of the directory-sync plan):
//! the approved plan runs, every effect reaches the journal under ONE batch,
//! and what did not happen comes out in the report instead of killing the
//! Task.
//!
//! The executor's corners — what the revalidation compares, which entry an
//! overwrite without a trash leaves, what happens if the plan stops being
//! read — are in `sync::exec`'s unit tests. Here the WIRING is tested: that
//! the roots come from the spool and not from the request, that the gate runs
//! over them WHEN APPLYING, that the batch exists and groups, and that the
//! plan is spent no matter what happens.
//!
//! From 13 onward, what that batch is worth: `Engine::undo_session` over the
//! entries the REAL executor wrote (task 11). A batch that groups but does not
//! undo is not an undoable unit, it is a label.

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
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn read_file(mem: &MemProvider, wire: &str) -> Vec<u8> {
    let mut stream = mem.read(&vp(wire), None).await.expect("read opens");
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

/// Engine with a journal, a spool and a `MemProvider` with the capabilities a
/// test asks for. `MemProvider::new()` does NOT declare a trash, so the path
/// with a trash has to be requested by hand — just like real life, where
/// `file://` has one and a bucket does not.
async fn harness(flags: CapabilityFlags) -> Harness {
    harness_with(flags, true).await
}

/// The same, choosing the trash's kind. `logical` = the provider says WHERE it
/// left what it buried (`reversal_ref`); without it, it behaves like the
/// system's native trash, which gives no restore handle — and that is the case
/// where undoing an overwrite cannot get it right.
async fn harness_with(flags: CapabilityFlags, logical: bool) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    // LOGICAL trash: the testkit's default one makes the subtree "vanish"
    // with no recoverable destination (like the OS's native one), and then
    // there is no `reversal_ref` to check. What is being tested here is that
    // undo receives WHERE it was buried when the provider knows how to say so.
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

/// The capabilities of a destination WITH a trash.
fn with_trash() -> CapabilityFlags {
    CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING | CapabilityFlags::TRASH
}

/// And one without it (a bucket, an SFTP).
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

/// Plans and drains the channel until closing; returns the `sync.plan_done`.
async fn plan(h: &Harness, mode: SyncMode) -> SyncPlanDone {
    let (handle, mut rx) = h
        .engine
        .sync_plan_as(params(mode), 1, Actor::User)
        .await
        .expect("sync.plan accepted");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Completed);
    done.expect("the plan closed with sync.plan_done")
}

/// Applies `hash` and waits for the end. Returns the terminal state and the report.
async fn apply(h: &Harness, hash: &PlanHash) -> (TaskState, SyncReportResult) {
    let (handle, report) = h
        .engine
        .sync_apply_as(hash, 1, Actor::User)
        .await
        .expect("sync.apply accepted");
    let state = handle.join().await;
    let report = report.lock().expect("report lock").clone();
    (state, report)
}

async fn entries(h: &Harness) -> Vec<JournalEntry> {
    h.journal.journal().entries().await.expect("entries")
}

/// Undoes the human's session and waits for the end.
async fn undo(h: &Harness) -> (TaskState, UndoReport) {
    let (handle, report) = h
        .engine
        .undo_session(Actor::User)
        .await
        .expect("undo accepted");
    let state = handle.join().await;
    let report = report.lock().expect("undo report lock").clone();
    (state, report)
}

/// The base tree: a source orphan, a pair that differs and a destination
/// orphan (which only `Mirror` looks at).
async fn seed(h: &Harness) {
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///s/new")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/new/a.txt", b"new-a").await;
    write_file(&h.mem, "mem:///s/common.txt", b"longer-source").await;
    write_file(&h.mem, "mem:///d/common.txt", b"destination").await;
    write_file(&h.mem, "mem:///d/extra.txt", b"extra").await;
}

// 1 ───────────────────────────────────────────────────────────────────────
/// The whole path: a `CreateDir`, a `Copy` and an `Overwrite` with a trash.
/// The copy lands, the overwrite buries before writing, and ALL of it shares a
/// `batch_id` — which is what turns it into an undoable unit.
#[tokio::test]
async fn an_approved_plan_runs_and_ends_up_in_a_single_batch() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    assert!(done.executable, "the plan is approvable");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert_eq!(report.done, 3, "createdir + copy + overwrite");
    assert!(report.batch_id.is_some(), "undo needs it");
    // #170: the report is self-sufficient. Whoever reads it may not be who
    // applied it — a reconnection, another connection, a `plan_done` that was
    // dropped — and without this it could not know whether anything it just
    // read comes back.
    assert_eq!(report.dest_trash, DestTrash::Restorable);
    assert_eq!(
        report.dest_trash, done.dest_trash,
        "the report cannot say a different trash than the one that was approved"
    );

    assert_eq!(read_file(&h.mem, "mem:///d/new/a.txt").await, b"new-a");
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"longer-source"
    );
    // `Update` deletes nothing: the destination orphan is still there.
    assert!(exists(&h.mem, "mem:///d/extra.txt").await);

    let es = entries(&h).await;
    let batch = report.batch_id;
    assert!(
        es.iter().all(|e| e.batch_id == batch),
        "every entry of the batch: {es:?}"
    );
    // The overwrite is `trashed` + `created`, in that order: undo walks
    // descending `seq`, so it deletes what was created BEFORE restoring what
    // was buried.
    let over: Vec<&JournalEntry> = es
        .iter()
        .filter(|e| e.path == b"mem:///d/common.txt")
        .collect();
    assert_eq!(over.len(), 2, "{over:?}");
    assert_eq!(over[0].op, "trashed");
    assert_eq!(over[0].reversal, "restore_trash");
    assert!(
        over[0].reversal_ref.is_some(),
        "undo needs to know WHERE it was buried"
    );
    assert_eq!(over[1].op, "created");
    assert_eq!(over[1].reversal, "delete");
    assert!(over[1].seq > over[0].seq);
}

// 2 ───────────────────────────────────────────────────────────────────────
/// Without a trash, the overwrite leaves ONE entry and is declared
/// irreversible. This is the row of the table a plan has to show BEFORE
/// anyone approves it, and the dialog's counter comes from the same place.
#[tokio::test]
async fn an_overwrite_without_a_trash_is_declared_irreversible() {
    let h = harness(without_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"source").await;
    write_file(&h.mem, "mem:///d/a.txt", b"longer-destination").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 1, "the dialog shows it");
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 1);
    // #170's contrast, and the reason the field has no default: this report
    // and test 1's are the same report except for this key, and one undoes
    // entirely and the other undoes nothing.
    assert_eq!(report.dest_trash, DestTrash::Absent);
    assert_eq!(report.dest_trash, done.dest_trash);
    assert_eq!(read_file(&h.mem, "mem:///d/a.txt").await, b"source");

    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "a single entry: {es:?}");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].reversal, "irreversible");
}

// 3 ───────────────────────────────────────────────────────────────────────
/// `Mirror` deletes the destination's orphan, and with a trash it is ONE
/// `trashed` for the whole tree: one entry, one thing to restore.
#[tokio::test]
async fn mirror_buries_the_destinations_orphan_as_one_piece() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d/extra")).await.expect("mkdir");
    write_file(&h.mem, "mem:///d/extra/x.txt", b"x").await;
    write_file(&h.mem, "mem:///d/extra/y.txt", b"y").await;

    let done = plan(&h, SyncMode::Mirror).await;
    assert_eq!(done.counts.delete_tree, 1);
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 1);
    assert!(!exists(&h.mem, "mem:///d/extra").await);
    assert!(!exists(&h.mem, "mem:///d/extra/x.txt").await);

    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "ONE entry for the whole tree: {es:?}");
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].path, b"mem:///d/extra");
}

// 4 ───────────────────────────────────────────────────────────────────────
/// **The whole reason the revalidation `stat` exists.** If the destination
/// stopped looking like what the plan noted, the step is NOT run: it comes out
/// as `Conflict` and the bytes that were there stay there. It is the only
/// thing standing between the plan's TTL and a lost file.
#[tokio::test]
async fn a_destination_that_changed_under_the_plan_is_a_conflict_not_a_write() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"source").await;
    write_file(&h.mem, "mem:///d/a.txt", b"destination").await;

    let done = plan(&h, SyncMode::Update).await;
    // Someone gets there before us. (A provider's `write` is create-new, so
    // truly replacing means removing and putting back.)
    h.mem.remove(&vp("mem:///d/a.txt")).await.expect("remove");
    write_file(&h.mem, "mem:///d/a.txt", b"someone got here first").await;

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(
        state,
        TaskState::Completed,
        "a failure does not kill the Task"
    );
    assert_eq!(report.done, 0);
    assert_eq!(report.failed, 1);
    assert_eq!(
        report.failures[0].cause,
        norte_proto::methods::SyncFailureCause::Conflict
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/a.txt").await,
        b"someone got here first",
        "NOTHING was written"
    );
    assert!(entries(&h).await.is_empty(), "not even journalled");
}

// 5 ───────────────────────────────────────────────────────────────────────
/// A step that fails is a ROW in the report and the walk continues: step
/// 40,000 of 500,000 cannot take the remaining 460,000 down with it.
#[tokio::test]
async fn a_failure_in_the_middle_does_not_kill_the_task() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"a").await;
    write_file(&h.mem, "mem:///s/b.txt", b"b").await;
    write_file(&h.mem, "mem:///s/c.txt", b"c").await;

    let done = plan(&h, SyncMode::Update).await;
    // `b.txt` stops existing at the source between approving and applying.
    h.mem.remove(&vp("mem:///s/b.txt")).await.expect("remove");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.done, 2);
    assert_eq!(report.failed, 1);
    assert_eq!(report.failures[0].rel.to_wire(), "b.txt");
    // #195: the class of the step that failed. Here it is a `Copy`, whose
    // `rel` hangs from the SOURCE — which is what the hostile row of the test
    // below does NOT do.
    assert_eq!(report.failures[0].kind, SyncStepKind::Copy);
    assert!(exists(&h.mem, "mem:///d/c.txt").await, "the walk continued");
}

// 5b ──────────────────────────────────────────────────────────────────────
/// **The most common hostile row of a `Mirror`, and the whole reason for
/// #195**: a `DeleteTree` that does not happen. Its `rel` hangs from the
/// DESTINATION, it carries no `dest_rel` — the delete is already spelled out
/// the way the destination spells it — and until 0.41.0 the report carried
/// nothing to tell it apart from a failed `Copy`, whose `rel` hangs from the
/// source. A pane that painted it under the source column would send the
/// operator to look at the tree that was never touched.
#[tokio::test]
async fn a_failing_delete_tree_is_recognized_by_its_class() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d/extra")).await.expect("mkdir");
    write_file(&h.mem, "mem:///d/extra/x.txt", b"x").await;

    let done = plan(&h, SyncMode::Mirror).await;
    assert_eq!(done.counts.delete_tree, 1);
    // The tree disappears between approving and applying: the revalidation
    // catches it and the step comes out as a report row instead of touching
    // anything.
    h.mem
        .remove(&vp("mem:///d/extra/x.txt"))
        .await
        .expect("remove");
    h.mem.remove(&vp("mem:///d/extra")).await.expect("remove");

    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 1, "{:?}", report.failures);
    let failure = &report.failures[0];
    assert_eq!(failure.kind, SyncStepKind::DeleteTree);
    assert_eq!(failure.rel.to_wire(), "extra");
    assert_eq!(
        failure.dest_rel, None,
        "a delete is already spelled out the way the destination spells it: \
         without this class, the row had NOTHING saying which root its `rel` hangs from"
    );
}

// 6 ───────────────────────────────────────────────────────────────────────
/// The plan is SPENT in any terminal state: `Spool::remove` runs when the
/// Task finishes, so the same hash cannot be applied again. Without that
/// call the hash would stay "applying" forever and it would not even be
/// possible to re-plan the same tree.
#[tokio::test]
async fn the_plan_is_spent_when_the_task_finishes() {
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
        "a plan is approved once"
    );
}

// 7 ───────────────────────────────────────────────────────────────────────
/// A hash this daemon never emitted is a stale plan, not an internal failure:
/// the answer is telling the client to plan again.
#[tokio::test]
async fn a_hash_this_daemon_never_emitted_is_a_stale_plan() {
    let h = harness(with_trash()).await;
    let foreign = PlanHash::parse(&"0".repeat(64)).expect("hex");
    let r = h.engine.sync_apply_as(&foreign, 1, Actor::User).await;
    assert!(matches!(r, Err(ProtoError::PlanStale)), "foreign hash");

    // And a plan from ANOTHER connection, either: the spool is tied to the
    // one that planned.
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 2, Actor::User)
        .await;
    assert!(
        matches!(r, Err(ProtoError::PlanStale)),
        "another connection"
    );
}

// 8 ───────────────────────────────────────────────────────────────────────
/// A plan with blockers does not run, and the rejection says so by name: a
/// matching hash says "this is the plan you were shown", never "this plan can
/// be run".
#[tokio::test]
async fn a_non_executable_plan_is_refused_and_does_not_stay_applying() {
    // A READ-ONLY destination: a single blocker, `DestReadOnly`.
    let h = harness(CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::READ_ONLY).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");

    let done = plan(&h, SyncMode::Update).await;
    assert!(!done.executable);
    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(matches!(r, Err(ProtoError::PlanNotExecutable)), "blocked");

    // And the right to plan was returned: re-planning the same tree works
    // again (same digest — without the rejection's `remove`, this would be a
    // visible failure).
    let another = plan(&h, SyncMode::Update).await;
    assert_eq!(another.plan_hash, done.plan_hash);
}

// 9 ───────────────────────────────────────────────────────────────────────
/// **The gate runs over the roots that come from the SPOOL, when applying.**
/// `sync.plan`'s does not do: between planning and applying a scope expires
/// and a `policy.toml` rule changes, and `sync.apply` carries no path to gate
/// with — only a hash. This policy denies EXACTLY the destination root, so
/// only a gate that pulled that root from the file could have seen it.
#[tokio::test]
async fn the_applys_gate_runs_over_the_spools_roots() {
    use norte_core::policy::{Decision, DenyReason, PolicyGate, PolicyOp};

    /// Denies every mutation that touches `mem:///d`, and only that.
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

    // Planning IS allowed: `sync.plan` has no mutation gate (the read one
    // lives in the daemon, which is the one that ties a connection to an
    // actor).
    let done = plan(&h, SyncMode::Update).await;
    assert!(done.executable);

    let r = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(
        matches!(r, Err(ProtoError::PolicyDenied { .. })),
        "the apply's gate sees it"
    );
    assert!(!exists(&h.mem, "mem:///d/new").await, "nothing was written");
    // And the right to apply was returned: a rejection does not leave the
    // hash stuck "applying" (if it did, this would be `PlanStale`).
    let another = plan(&h, SyncMode::Update).await;
    assert_eq!(another.plan_hash, done.plan_hash);
}

// 10 ──────────────────────────────────────────────────────────────────────
/// Without a journal, nothing is applied (hard rule 4): the plan promises a
/// reversal per step and only the journal can fulfill it. Fail-closed, like
/// the spool.
#[tokio::test]
async fn without_a_journal_nothing_is_applied() {
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
    assert!(matches!(r, Err(ProtoError::Unsupported)), "no journal");
}

// 11 ──────────────────────────────────────────────────────────────────────
/// Cancelling leaves the batch CLOSED and undoable: what was applied before
/// the cut is journalled under its `batch_id`, and nothing is unwound (half a
/// sync is a real state). And the plan is spent all the same.
#[tokio::test]
async fn cancelling_leaves_a_closed_batch_and_spends_the_plan() {
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
        "every step done left its entry"
    );
    assert!(
        es.iter().all(|e| e.batch_id == report.batch_id),
        "a single batch"
    );
    // The plan was spent: cancellation is a terminal state like any other.
    let again = h
        .engine
        .sync_apply_as(&done.plan_hash, 1, Actor::User)
        .await;
    assert!(matches!(again, Err(ProtoError::PlanStale)));
}

// 12 ──────────────────────────────────────────────────────────────────────
/// A `Skip` touches nothing and journals nothing: it counts in `skipped`, not
/// in `done`, and the destination tree stays as it was.
#[tokio::test]
async fn a_skip_touches_nothing_and_leaves_no_entry() {
    let h = harness(with_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///s/dark")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/dark/x.txt", b"x").await;
    write_file(&h.mem, "mem:///s/a.txt", b"a").await;
    // A SOURCE directory that cannot be listed: the comparison emits an error
    // row and the transducer turns it into an `Unreadable` `Skip`.
    h.mem.faults().fail_list_at(&vp("mem:///s/dark"));

    let done = plan(&h, SyncMode::Update).await;
    assert!(
        done.counts.skip >= 1,
        "there is at least one Skip: {:?}",
        done.counts
    );
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(
        report.skipped, done.counts.skip,
        "Skips are counted separately"
    );
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(
        !exists(&h.mem, "mem:///d/dark/x.txt").await,
        "a Skip writes nothing"
    );
    assert_eq!(
        entries(&h).await.len() as u64,
        report.done,
        "only what was done leaves an entry"
    );
}

// 13 ──────────────────────────────────────────────────────────────────────
/// **The batch is really undoable** (task 11). The whole plan is applied and
/// the session undone: the tree goes back exactly to how it was, including the
/// `Overwrite` pair — which only comes out right if undo deletes what was
/// CREATED before restoring what was BURIED — and the directory, which is
/// emptied before it leaves.
#[tokio::test]
async fn undoing_a_sync_returns_the_tree() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.blocked, None, "nothing blocked");
    assert_eq!(undone.undone, 4, "trashed + created + createdir + copy");
    assert_eq!(undone.skipped_irreversible, 0);
    assert!(undone.unreverted_paths.is_empty(), "everything came back");

    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"destination",
        "what was buried went back to its place",
    );
    assert!(
        !exists(&h.mem, "mem:///d/new/a.txt").await,
        "the copy is gone"
    );
    assert!(
        !exists(&h.mem, "mem:///d/new").await,
        "and the directory behind it"
    );
    assert!(
        exists(&h.mem, "mem:///d/extra.txt").await,
        "what the sync did not touch, undo did not either"
    );

    // Second undo: everything compensated, nothing to do.
    let (state, again) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(again.undone, 0);
}

// 14 ──────────────────────────────────────────────────────────────────────
/// A sync's undo is in turn ONE BATCH: all the compensations share a FRESH
/// `batch_id` and each says which `seq` it compensates. Without the first,
/// undoing the undo would split into four units; without the second, the
/// compensations would look like new, undoable mutations.
#[tokio::test]
async fn a_syncs_undo_is_in_turn_one_batch() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (_state, report) = apply(&h, &done.plan_hash).await;
    let applied = report.batch_id.expect("the forward batch");

    let (state, _undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);

    let comp: Vec<JournalEntry> = entries(&h)
        .await
        .into_iter()
        .filter(|e| e.undoes_seq.is_some())
        .collect();
    assert_eq!(comp.len(), 4, "one compensation per entry: {comp:?}");
    let batch = comp[0].batch_id.expect("the compensations go in a batch");
    assert!(
        comp.iter().all(|e| e.batch_id == Some(batch)),
        "all under the SAME batch: {comp:?}",
    );
    assert_ne!(batch, applied, "and a FRESH batch, not the forward one");
}

// 15 ──────────────────────────────────────────────────────────────────────
/// **Without a trash at the destination, undo returns NOTHING — not even the
/// copies.** An `irreversible` entry has nothing to restore, and a `created`
/// without a trash is not permanently deleted (#65: human work may already
/// live under that path). What the sync promises as a `Delete` reversal stays
/// a promise, so undo counts it and NAMES it instead of faking it.
#[tokio::test]
async fn without_a_trash_undo_names_what_it_cannot_return() {
    let h = harness(without_trash()).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/a.txt", b"new").await;
    write_file(&h.mem, "mem:///s/common.txt", b"longer-source").await;
    write_file(&h.mem, "mem:///d/common.txt", b"destination").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 1, "the overwrite, and only it");
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);

    let (state, undone) = undo(&h).await;
    assert_eq!(
        state,
        TaskState::Completed,
        "it does not refuse: it reports"
    );
    assert_eq!(undone.undone, 0, "nothing came back");
    assert_eq!(undone.skipped_irreversible, 1, "the overwrite");
    assert_eq!(
        undone.skipped_created_no_trash, 1,
        "and the COPY, which the plan showed as reversible"
    );
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/common.txt".to_vec(), b"mem:///d/a.txt".to_vec()],
        "both, in the order they were attempted (descending seq)",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"longer-source"
    );
    assert!(
        exists(&h.mem, "mem:///d/a.txt").await,
        "nothing was deleted"
    );
}

// 16 ──────────────────────────────────────────────────────────────────────
/// **A step that does not come back does not hold the others hostage.** This
/// is the whole difference from undoing a rename batch: half a permutation
/// undone is not a valid state, half a sync undone is. Here someone deletes a
/// copied file by hand between applying and undoing: that entry blocks, and
/// the other three come back all the same.
#[tokio::test]
async fn a_step_that_does_not_come_back_does_not_hold_the_rest_hostage() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);

    // Drift: the human deletes the copy on their own.
    h.mem
        .remove(&vp("mem:///d/new/a.txt"))
        .await
        .expect("remove");

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 3, "the other three entries DID come back");
    let (seq, error) = undone
        .blocked
        .expect("the one that did not come back, named");
    assert!(matches!(error, ProtoError::NotFound), "{error:?}");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/new/a.txt".to_vec()],
        "and by name, not just a seq ({seq})",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"destination",
        "the overwrite was undone even though another entry blocked",
    );
}

// 17 ──────────────────────────────────────────────────────────────────────
/// **A trash that does not say where it left what it buried says so BEFORE,
/// not after.** This was task 11's BLOCKER: the plan promised `RestoreTrash`,
/// the journal ended up without a `reversal_ref`, and undo matched by original
/// path — picking the most recent one, which by then was the file it had just
/// buried itself; the user saw a success and their original stayed in the
/// trash. Now the destination declares its trash names nothing
/// (`trash_restorable` at `false`) and the plan comes out entirely
/// `Irreversible` with its reason, which is what hard rule 4 asks for: either
/// there is undo, or there is an explicit classification BEFORE approving.
///
/// And what does NOT change: the delete still goes to the trash. Losing undo
/// is no reason to permanently delete what could be buried.
#[tokio::test]
async fn a_trash_that_does_not_name_its_destination_says_so_in_the_plan() {
    let h = harness_with(with_trash(), false).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/common.txt", b"longer-source").await;
    write_file(&h.mem, "mem:///d/common.txt", b"destination").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(
        done.counts.irreversible, 1,
        "the plan says so before anyone approves: {:?}",
        done.counts
    );
    let (state, report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    let es = entries(&h).await;
    assert_eq!(es.len(), 1, "ONE entry, irreversible: {es:?}");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].reversal.as_str(), "irreversible");
    assert!(es[0].reversal_ref.is_none());

    let (state, undone) = undo(&h).await;
    assert_eq!(
        state,
        TaskState::Completed,
        "it does not refuse: it reports"
    );
    assert_eq!(undone.undone, 0, "there is nothing to return…");
    assert_eq!(undone.skipped_irreversible, 1, "…and it is counted");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/common.txt".to_vec()],
        "named ONCE, the one for the file the user wants back",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"longer-source",
        "the path does NOT end up empty: what was synced is still there",
    );
}

/// And the counterpart that really fixes the BLOCKER: a trash that DOES name
/// its destination undoes the whole pair, with no guessing.
#[tokio::test]
async fn a_trash_that_names_its_destination_undoes_the_overwrite() {
    let h = harness_with(with_trash(), true).await;
    h.mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
    h.mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&h.mem, "mem:///s/common.txt", b"longer-source").await;
    write_file(&h.mem, "mem:///d/common.txt", b"destination").await;

    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(done.counts.irreversible, 0, "{:?}", done.counts);
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);
    let es = entries(&h).await;
    assert_eq!(es.len(), 2, "trashed + created: {es:?}");
    assert!(
        es[0].reversal_ref.is_some(),
        "the recoverable destination reached the journal"
    );

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 2, "both halves");
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"destination",
        "the USER's file, not the one undo had just buried",
    );
}

// 18 ──────────────────────────────────────────────────────────────────────
/// **A created directory is not sent to the trash with someone else's content
/// inside.** The reverse order leaves it empty when everything goes well;
/// when it does not — here the user put a file of their own in it between
/// applying and undoing — the trash would take that down too, and the report
/// would not name it. That entry blocks and the others continue.
#[tokio::test]
async fn a_created_directory_with_someone_elses_content_is_not_buried() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    let (state, _report) = apply(&h, &done.plan_hash).await;
    assert_eq!(state, TaskState::Completed);

    // The human leaves something of theirs inside the directory the sync created.
    write_file(&h.mem, "mem:///d/new/notes.txt", b"mine").await;

    let (state, undone) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(undone.undone, 3, "everything except the directory");
    assert_eq!(
        undone.unreverted_paths,
        vec![b"mem:///d/new".to_vec()],
        "and the one that did not come back, by name",
    );
    assert!(
        exists(&h.mem, "mem:///d/new/notes.txt").await,
        "the human's file is still where they left it",
    );
    assert!(
        !exists(&h.mem, "mem:///d/new/a.txt").await,
        "the copy is gone"
    );
}

// 19 ──────────────────────────────────────────────────────────────────────
/// Cancelling halfway through a batch (rule 3): the cut is BETWEEN entries,
/// what was compensated stays compensated, the chain stays intact and what was
/// left is still undoable — a second undo finishes it. How much went into each
/// half depends on the clock; that the sum is the whole batch does not.
#[tokio::test]
async fn cancelling_a_batchs_undo_leaves_it_finishable() {
    let h = harness(with_trash()).await;
    seed(&h).await;
    let done = plan(&h, SyncMode::Update).await;
    assert_eq!(apply(&h, &done.plan_hash).await.0, TaskState::Completed);
    let total = entries(&h).await.len() as u64;

    // Per-op latency → a deterministic window to cancel before finishing.
    h.mem
        .faults()
        // The clock cannot be paused here: sqlx's journal pool times out
        // (`PoolTimedOut`) when tokio fast-forwards time. A wide window
        // instead: 200 ms per op against a 15 ms wait, 50x of margin.
        .set_latency_per_op(Some(std::time::Duration::from_millis(200)));
    let (handle, report) = h
        .engine
        .undo_session(Actor::User)
        .await
        .expect("undo accepted");
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    handle.cancel();
    let state = handle.join().await;
    let first = report.lock().expect("undo report lock").clone();
    h.mem.faults().set_latency_per_op(None);

    assert_eq!(state, TaskState::Cancelled, "clean cooperative cut");
    assert!(first.blocked.is_none(), "cancelling is not blocking");
    assert!(
        h.journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
        "the journal's chain survives the cut",
    );

    let (state, second) = undo(&h).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(
        first.undone + second.undone,
        total,
        "between the two halves, the whole batch",
    );
    assert_eq!(
        read_file(&h.mem, "mem:///d/common.txt").await,
        b"destination",
        "and the tree ends up where it was",
    );
    assert!(!exists(&h.mem, "mem:///d/new").await);
}
