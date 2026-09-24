//! `Engine::sync_plan_as` integration (task 8 of the directory-sync plan): the
//! [`TaskKind::SyncPlan`] Task, the tee to the spool and to the batch, closing
//! with `sync.plan_done`, and the rejections that never become a Task.
//!
//! The transducer belongs to `norte-sync` and already has its tests against
//! hand-made rows and against a real `compare()`; the spool belongs to
//! `sync::spool` and has its own too. Here only what the core adds by joining
//! them is tested: the batch, the counter, the retention, the roots' identity
//! and the ending.
//!
//! There is no journal to check: planning writes not a single byte to either
//! tree (hard rule 4 does not apply; `sync.apply` is the one that writes).

use std::sync::Arc;

use bytes::Bytes;
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    DestTrash, RelPath, SYNC_MAX_BLOCKERS_REPORTED, SYNC_MAX_INCLUDE, SYNC_STEPS_MAX_BATCH,
    StepReversal, SyncBlockerKind, SyncCompareOptions, SyncMode, SyncPlanDone, SyncPlanParams,
    SyncStep, SyncStepKind,
};
use norte_proto::{Error as ProtoError, RootOverlap, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

fn rel(wire: &str) -> RelPath {
    RelPath::parse_wire(wire).expect("valid rel")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

/// Engine + `MemProvider` + THE spool under its own tempdir.
///
/// The `TempDir` is returned so it lives as long as the test does: dropping it
/// deletes the spool directory with it.
fn setup() -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    setup_with(MemProvider::new())
}

/// The same setup over an already-configured provider: what changes between
/// the variants is the destination's TRASH, which is the only thing that
/// decides whether a plan can be undone.
fn setup_with(mem: MemProvider) -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    let mem = Arc::new(mem);
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

/// The steps from every batch and the closing event, in the order they came out.
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
            .expect("the plan closed with sync.plan_done")
    }
}

/// Launches the plan and drains its channel until closing.
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
                    "a batch AFTER closing: sync.plan_done has to be last"
                );
                assert_eq!(batch.task_id, handle.id(), "the batch carries ITS task_id");
                batches.push(batch.steps);
            }
            SyncPlanEvent::Done(d) => {
                assert!(done.is_none(), "two closings for one plan");
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

/// How many files are in the spool directory (the `.part` included).
fn spooled(engine: &Engine) -> usize {
    let spool = engine.spool().expect("there is a spool");
    match std::fs::read_dir(spool.dir()) {
        Ok(rd) => rd.count(),
        // Never created: zero.
        Err(_) => 0,
    }
}

/// Two files at the source and an empty destination: two copies, no blockers.
async fn simple(mem: &MemProvider) {
    mkdir(mem, "mem:///s").await;
    mkdir(mem, "mem:///d").await;
    write_file(mem, "mem:///s/a.txt", b"aaa").await;
    write_file(mem, "mem:///s/b.txt", b"bbbb").await;
}

// 1 ───────────────────────────────────────────────────────────────────────
/// Batches are BOUNDED and coalesced, the same contract as `compare.rows`:
/// half a million steps cannot turn into half a million frames.
#[tokio::test]
async fn steps_arrive_in_bounded_coalesced_batches() {
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
        "a batch above the cap: {:?}",
        out.batches.iter().map(Vec::len).collect::<Vec<_>>()
    );
    assert_eq!(out.steps().len(), 600, "one step per source file");
    assert!(
        out.batches.len() < 600,
        "one frame per step is not coalescing: {} batches",
        out.batches.len()
    );
    assert_eq!(out.done().counts.copy, 600);
}

// 2 ───────────────────────────────────────────────────────────────────────
/// The plan CLOSES with its hash, its counters and its verdict, and all of
/// that comes from the spool's summary — it is not recomputed here nor in the
/// daemon.
#[tokio::test]
async fn the_plan_closes_with_hash_counters_and_executable() {
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
        "the core cannot emit inconsistent steps: {:#?}",
        out.steps()
    );
    assert!(
        out.steps()
            .iter()
            .all(|s| s.kind == SyncStepKind::Copy && s.reversal.is_some())
    );
}

/// The closing event says which TRASH the destination has, which is the only
/// thing that tells apart a plan that can be undone from an identical one
/// that cannot.
///
/// This test's two plans have the same steps over the same files; what
/// changes is whether undo is going to return anything. Without this field
/// the approval dialog could not say so (spec 2, task 12).
#[tokio::test]
async fn the_closing_event_says_which_trash_the_destination_has() {
    // `MemProvider` declares a trash and does not promise to restore: it is
    // macOS's and Windows's MUTE trash, which returns not even one copy.
    let (engine, mem, _dir) = setup();
    simple(&mem).await;
    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.done().dest_trash, DestTrash::Opaque);
    assert!(
        out.steps()
            .iter()
            .all(|s| s.reversal == Some(StepReversal::Irreversible)),
        "with a mute trash not even one copy comes back: {:#?}",
        out.steps()
    );

    // And with the logical trash, the SAME comparison undoes entirely.
    let (engine, mem, _dir2) = setup_with(MemProvider::new().with_logical_trash());
    simple(&mem).await;
    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.done().dest_trash, DestTrash::Restorable);
    assert!(
        out.steps()
            .iter()
            .all(|s| s.reversal == Some(StepReversal::Delete))
    );

    // And with NO trash at all — a bucket, an SFTP —, which is the case the
    // field exists for: the copy still announces `delete` on the wire and
    // undo is going to skip it, so the closing event is the ONLY thing that
    // tells this plan apart from the one above. Both carry the same steps
    // with the same reversal.
    let no_trash = norte_proto::CapabilityFlags::CASE_SENSITIVE
        | norte_proto::CapabilityFlags::CASE_PRESERVING;
    let (engine, mem, _dir3) = setup_with(MemProvider::with_flags(no_trash));
    simple(&mem).await;
    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.done().dest_trash, DestTrash::Absent);
    assert!(
        out.steps()
            .iter()
            .all(|s| s.reversal == Some(StepReversal::Delete)),
        "the copy announces `delete` with no trash to come back from: {:#?}",
        out.steps()
    );
}

// 3 ───────────────────────────────────────────────────────────────────────
/// A directory against a file of the same name is a structural blocker
/// (`TypeMismatchDir`): replacing a tree with a file deserves a human, and the
/// plan stops being executable.
#[tokio::test]
async fn a_blocker_leaves_the_plan_not_executable() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    write_file(&mem, "mem:///s/x", b"I am a file").await;
    mkdir(&mem, "mem:///d/x").await;

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    let done = out.done();
    assert!(!done.executable, "a blocker CANNOT be approved");
    assert_eq!(done.blockers.len(), 1, "{:#?}", done.blockers);
    assert_eq!(done.blockers_total, 1);
}

// 4 ───────────────────────────────────────────────────────────────────────
/// The blocker LIST is trimmed; the TOTAL is not. A human needs to know there
/// are 266 even if only 256 can be shown.
#[tokio::test]
async fn blockers_are_trimmed_but_the_total_is_not() {
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
/// Overlapping roots: rejected BEFORE walking anything, and the error says
/// WHICH one is inside which — it is not the same for whoever paints it.
#[tokio::test]
async fn overlapping_roots_are_rejected_before_walking() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///a/sub").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///a", "mem:///a/sub"), 1, Actor::User)
        .await
    else {
        panic!("the destination is inside the source");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::DestInsideSource
            }
        ),
        "was {err:?}"
    );

    let Err(err) = engine
        .sync_plan_as(params("mem:///a/sub", "mem:///a"), 1, Actor::User)
        .await
    else {
        panic!("the source is inside the destination");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::SourceInsideDest
            }
        ),
        "was {err:?}"
    );
    assert_eq!(spooled(&engine), 0, "a rejection creates no Task nor spool");
}

// 6 ───────────────────────────────────────────────────────────────────────
/// Two EQUAL roots are not "one inside the other": they are the same one, and
/// the error says so instead of picking a side by convention.
#[tokio::test]
async fn identical_roots_say_so_instead_of_naming_a_side() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///a").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///a", "mem:///a"), 1, Actor::User)
        .await
    else {
        panic!("a root against itself");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::Same
            }
        ),
        "was {err:?}"
    );
}

// 7 ───────────────────────────────────────────────────────────────────────
/// And the half the structural check CANNOT see: a root that is a symlink to
/// the same directory as the other. The two `VPath`s are different byte for
/// byte and the walk's rows all hang from the symlinked root, so neither
/// structural equality nor the walk's guard catches it. `node_id` catches it.
#[tokio::test]
async fn a_symlinked_root_against_its_destination_is_the_same_tree() {
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
        panic!("both roots are the same directory");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::Same
            }
        ),
        "was {err:?}"
    );
}

// 7 bis ───────────────────────────────────────────────────────────────────
/// And the OTHER half neither of the two sees: the containment that only
/// exists if the case is folded. `mem:///Data` against `mem:///data/backup`
/// are not equal byte for byte, one does not hang from the other byte for
/// byte, and their `node_id`s are different because they are different
/// directories — but on a folding volume they are the SAME directory named
/// twice, meaning the plan would copy a tree inside itself. This is the result
/// the whole overlap apparatus exists to prevent.
#[tokio::test]
async fn containment_only_visible_by_folding_the_case_is_also_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    // Without `CASE_SENSITIVE`: APFS, NTFS, an ext4 `+F`.
    let mem = Arc::new(MemProvider::with_flags(
        norte_proto::CapabilityFlags::CASE_PRESERVING | norte_proto::CapabilityFlags::RENAME_ATOMIC,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(dir.path()));
    mkdir(&mem, "mem:///data").await;
    mkdir(&mem, "mem:///data/backup").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///Data", "mem:///data/backup"), 1, Actor::User)
        .await
    else {
        panic!("the destination hangs from the source as soon as the case is folded");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::DestInsideSource
            }
        ),
        "was {err:?}"
    );

    // And the other way around, and the two spellings of the SAME root.
    let Err(err) = engine
        .sync_plan_as(params("mem:///data/backup", "mem:///DATA"), 1, Actor::User)
        .await
    else {
        panic!("the source hangs from the destination");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::SourceInsideDest
            }
        ),
        "was {err:?}"
    );
    let Err(err) = engine
        .sync_plan_as(params("mem:///Data", "mem:///dAtA"), 1, Actor::User)
        .await
    else {
        panic!("they are the same root written twice");
    };
    assert!(
        matches!(
            err,
            ProtoError::OverlappingRoots {
                relation: RootOverlap::Same
            }
        ),
        "was {err:?}"
    );
}

// 7 ter ───────────────────────────────────────────────────────────────────
/// And the fold is NOT applied where it does not belong: with both sides
/// distinguishing case, `mem:///Data` and `mem:///data/backup` really are two
/// different trees and the plan is served. Rejecting it would deny a
/// legitimate sync on ext4, which is the half of the decision the test above
/// cannot show.
#[tokio::test]
async fn with_both_sides_case_sensitive_there_is_no_overlap_to_fold() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///Data").await;
    mkdir(&mem, "mem:///data").await;
    mkdir(&mem, "mem:///data/backup").await;

    let planned = plan(&engine, params("mem:///Data", "mem:///data/backup")).await;
    assert_eq!(planned.state, TaskState::Completed);
}

// 8 ───────────────────────────────────────────────────────────────────────
/// Two `compare` fields are not the caller's in `sync.plan`. Sending them is a
/// rejection, never a value the core silently overrides: serving a walk
/// different from the one requested is worse than not offering it.
#[tokio::test]
async fn the_caller_does_not_set_the_planners_options() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let mut p = params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("follow_symlinks is not silently served");
    };
    assert!(matches!(err, ProtoError::Unsupported), "was {err:?}");

    let mut p = params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(norte_proto::methods::DescendSide::Right);
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("descend_orphans is set by the planner");
    };
    assert!(matches!(err, ProtoError::Unsupported), "was {err:?}");
    assert_eq!(spooled(&engine), 0);
}

// 9 ───────────────────────────────────────────────────────────────────────
/// An `include` above the cap is REFUSED. Trimming it silently would sync
/// something nobody asked for, and the human would approve it believing they
/// saw it whole.
#[tokio::test]
async fn an_include_above_the_cap_is_refused_instead_of_trimmed() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("x"); SYNC_MAX_INCLUDE + 1]);
    let Err(err) = engine.sync_plan_as(p, 1, Actor::User).await else {
        panic!("above the cap");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "was {err:?}");
}

// 10 ──────────────────────────────────────────────────────────────────────
/// `include` trims what COMES OUT, and the hash goes with the trimmed set: two
/// plans of the same tree with different selections cannot share a digest, or
/// approving one would authorize the other.
#[tokio::test]
async fn include_trims_the_steps_and_the_hash_goes_with_them() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///s/sub").await;
    write_file(&mem, "mem:///s/a.txt", b"a").await;
    write_file(&mem, "mem:///s/sub/inside.txt", b"b").await;

    let everything = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(everything.state, TaskState::Completed);
    assert_eq!(everything.done().counts.copy, 2, "a.txt and sub/inside.txt");

    // Only the folder: it drags its content and leaves `a.txt` out.
    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("sub")]);
    let part = plan(&engine, p).await;
    assert_eq!(part.state, TaskState::Completed);
    let rels: Vec<String> = part.steps().iter().map(|s| s.rel.to_wire()).collect();
    assert_eq!(rels, vec!["sub".to_owned(), "sub/inside.txt".to_owned()]);
    assert_eq!(part.done().counts.copy, 1);
    assert_eq!(part.done().counts.create_dir, 1);
    assert_ne!(
        part.done().plan_hash,
        everything.done().plan_hash,
        "two different selections cannot share a hash"
    );
}

// 10b ─────────────────────────────────────────────────────────────────────
/// And the drag UPWARD, which is the one that is missing: the pane lets the
/// FILE'S row be selected, and without its folder's `CreateDir` the plan would
/// copy inside a directory that does not exist — also breaking the rule
/// `SyncStepsBatch::steps` publishes ("a `CreateDir` precedes every copy
/// inside it").
#[tokio::test]
async fn selecting_a_file_drags_its_folders_createdir() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    mkdir(&mem, "mem:///s/new").await;
    write_file(&mem, "mem:///s/new/a.txt", b"a").await;
    write_file(&mem, "mem:///s/new/b.txt", b"b").await;

    let mut p = params("mem:///s", "mem:///d");
    p.include = Some(vec![rel("new/a.txt")]);
    let out = plan(&engine, p).await;
    assert_eq!(out.state, TaskState::Completed);

    let steps = out.steps();
    let rels: Vec<String> = steps.iter().map(|s| s.rel.to_wire()).collect();
    assert_eq!(rels, vec!["new".to_owned(), "new/a.txt".to_owned()]);
    assert_eq!(steps[0].kind, SyncStepKind::CreateDir);
    assert_eq!(out.done().counts.copy, 1, "b.txt was not selected");
    assert_eq!(out.done().counts.create_dir, 1);
}

// 11 ──────────────────────────────────────────────────────────────────────
/// Hard rule 3 at the Task boundary, and the consequence that matters: a
/// cancelled plan **leaves nothing approvable**. The partial digest of a plan
/// cut halfway would be perfectly valid for a plan that claims to sync a tree
/// that was only a third walked.
#[tokio::test]
async fn cancelling_the_plan_leaves_no_spool() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    // Enough steps to FILL the channel (capacity 8 batches of 256): with the
    // sender blocked in `send`, the plan cannot finish before the test cuts
    // it. Without that this test would be a race against the clock.
    for i in 0..3_000 {
        write_file(&mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }

    let (handle, mut rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    let first = rx.recv().await.expect("at least one event");
    assert!(matches!(first, SyncPlanEvent::Steps(_)));
    handle.cancel();

    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(done.is_none(), "a cancelled plan does NOT close");
    assert_eq!(
        spooled(&engine),
        0,
        "a cancelled plan is not an approvable plan"
    );
}

// 12 ──────────────────────────────────────────────────────────────────────
/// An owner that stops receiving also leaves no plan: what would be retained
/// is a plan nobody ever saw in full, and dropping the channel is also what
/// stops the walk.
#[tokio::test]
async fn an_owner_that_stops_receiving_leaves_no_approvable_plan() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    // Enough steps to FILL the channel (capacity 8 batches of 256): with the
    // sender blocked in `send`, the plan cannot finish before the test cuts
    // it. Without that this test would be a race against the clock.
    for i in 0..3_000 {
        write_file(&mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }

    let (handle, mut rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    rx.recv().await.expect("at least one event");
    drop(rx);

    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert_eq!(spooled(&engine), 0);
}

// 13 ──────────────────────────────────────────────────────────────────────
/// A plan that finishes leaves EXACTLY one spool, and it opens with the hash
/// that traveled in the closing event. This is the executor's precondition:
/// `sync.apply` does not carry the roots, so they come from there.
#[tokio::test]
async fn a_completed_plan_leaves_one_spool_openable_with_its_hash() {
    let (engine, mem, _dir) = setup();
    simple(&mem).await;

    let out = plan(&engine, params("mem:///s", "mem:///d")).await;
    assert_eq!(out.state, TaskState::Completed);
    assert_eq!(spooled(&engine), 1);

    let spool = engine.spool().expect("there is a spool");
    let reader = spool
        .open(1, &out.done().plan_hash)
        .await
        .expect("the plan opens with its hash");
    assert_eq!(reader.header().options.source_root, vp("mem:///s"));
    assert_eq!(reader.header().options.dest_root, vp("mem:///d"));
    assert!(reader.summary().executable);

    // And another connection does not open it, even knowing the hash.
    assert!(spool.open(2, &out.done().plan_hash).await.is_err());
}

// 13b ─────────────────────────────────────────────────────────────────────
/// A plan that emits NOT ONE step — two identical trees — never touches its
/// channel, so it cannot find out its owner left through `flush`. This is the
/// case that forces the property "a plan with no owner is not retained" to be
/// decided in the spool and not in the channel.
#[tokio::test]
async fn a_zero_step_plan_whose_owner_left_is_not_retained() {
    let (engine, mem, _dir) = setup();
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;
    write_file(&mem, "mem:///s/a.txt", b"x").await;
    write_file(&mem, "mem:///d/a.txt", b"x").await;

    let (handle, rx) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
        .expect("sync.plan");
    // The owner leaves before the Task gets to anything, and the spool finds
    // out through the connection's teardown, not through the channel.
    engine
        .spool()
        .expect("there is a spool")
        .drop_connection(1)
        .await
        .expect("drop");
    drop(rx);

    // There are TWO possible orderings and this test cannot pin down which
    // one comes out: the teardown can arrive before the Task finishes, or
    // after. Under `cargo llvm-cov` the second happens quite often, and
    // asserting `!= Completed` was asserting a race — an intermittent red,
    // which here is a bug and not noise.
    //
    // The property does NOT depend on the order, and it is the only one this
    // test exists to pin down: whatever happens, no plan from a connection
    // that is no longer there stays retained. If on top of that the Task did
    // not get to complete, that means the teardown won, which is the other
    // path and is also fine.
    let state = handle.join().await;
    assert_eq!(spooled(&engine), 0, "an ownerless plan is not retained");
    assert!(
        matches!(
            state,
            TaskState::Completed | TaskState::Failed { .. } | TaskState::Cancelled
        ),
        "the Task ends in one of the three ways, was {state:?}"
    );
}

// 14 ──────────────────────────────────────────────────────────────────────
/// Without a spool installed, nothing is planned: a plan that cannot be
/// retained cannot be applied either, and showing an approval dialog over
/// something that does not exist afterward is worse than not offering it
/// (fail-closed, like the index).
#[tokio::test]
async fn without_a_spool_installed_nothing_is_planned() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///s").await;
    mkdir(&mem, "mem:///d").await;

    let Err(err) = engine
        .sync_plan_as(params("mem:///s", "mem:///d"), 1, Actor::User)
        .await
    else {
        panic!("without retention there is no plan");
    };
    assert!(matches!(err, ProtoError::Unsupported), "was {err:?}");
}

// 15 ──────────────────────────────────────────────────────────────────────
// The three provider crossings the spec asks for BY NAME
// (`planning_into_an_archive_blocks_instead_of_attempting_and_failing`,
// `comparing_against_an_archive_source_copies_on_unknown_and_says_so`,
// `a_destination_without_a_trash_takes_the_irreversible_path_for_real`).
//
// These are the only place where `Unknown` confidence and `Irreversible`
// reversal meet REAL providers — a read-only zip with no date to trust, a
// destination that declares no trash — instead of hand-made `Capabilities`.
// The transducer already has its tables tested against synthetic rows; what
// is checked here is that what a real provider SAYS reaches the step.

/// An in-memory zip with `files` (raw name, bytes) and WITHOUT dates: the pair
/// comes out at zero, which is invalid, so the index leaves `mtime_ms: None` —
/// exactly what a real zip whose writer did not fill the field does. That is
/// where the second test's `Unknown` comes from.
fn zip_bytes(files: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut smith = norte_testkit::ZipSmith::new().undated();
    for (name, body) in files {
        smith = smith.file(name, body);
    }
    smith.build()
}

/// Engine with the usual `MemProvider` and an `a.zip` container inside it,
/// reachable as `zip+mem:///a.zip/!` (ADR 0018: composite scheme, with no
/// prior registration of the archive provider).
async fn setup_with_zip(files: &[(&[u8], &[u8])]) -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    let (engine, mem, dir) = setup();
    let bytes = zip_bytes(files);
    let mut sink = mem.write(&vp("mem:///a.zip")).await.expect("write opens");
    sink.write(Bytes::from(bytes)).await.expect("chunk");
    sink.commit().await.expect("commit");
    (engine, mem, dir)
}

/// Planning TOWARD an archive blocks: `norte-vfs-archive` is read-only, and
/// that has to come out of the PLAN — a blocker, before anyone approves
/// anything — and not from half a batch of failed writes.
///
/// And the blocker CLOSES the plan: not a single step is emitted, because the
/// step list of a plan that cannot run only serves to make someone look at it
/// and believe it will.
#[tokio::test]
async fn planning_into_an_archive_blocks_instead_of_attempting_and_failing() {
    let (engine, mem, _dir) = setup_with_zip(&[(b"inside.txt", b"x")]).await;
    mkdir(&mem, "mem:///s").await;
    write_file(&mem, "mem:///s/a.txt", b"aaa").await;

    let planned = plan(&engine, params("mem:///s", "zip+mem:///a.zip/!")).await;
    assert_eq!(planned.state, TaskState::Completed);
    let done = planned.done();
    assert!(
        !done.executable,
        "a read-only destination is not executable"
    );
    assert_eq!(done.blockers.len(), 1);
    assert_eq!(done.blockers[0].kind, SyncBlockerKind::DestReadOnly);
    assert!(
        done.blockers[0].rel.is_root(),
        "a read-only destination is not about a specific spot"
    );
    assert!(
        planned.steps().is_empty(),
        "not one step: the blocker ends the stream before pulling the first row"
    );
}

/// A SOURCE that is an archive: its dates deserve no trust (this zip's second
/// pair is invalid, so the index makes none up). The cascade answers
/// `Same`/`Unknown`, and `on_unknown: Copy` — the default — WRITES.
///
/// That it writes is not the interesting part: the interesting part is that
/// the step carries the confidence it was decided with, which is what ADR
/// 0048 promises and what lets the dialog say "this is copied because nobody
/// could verify it".
#[tokio::test]
async fn an_archive_source_copies_the_uncertain_and_says_why() {
    let (engine, mem, _dir) = setup_with_zip(&[(b"same-bytes.txt", b"12345")]).await;
    mkdir(&mem, "mem:///d").await;
    // SAME size on both sides: the size rung does not decide and the cascade
    // moves to the date one, which is the one left without an answer.
    write_file(&mem, "mem:///d/same-bytes.txt", b"54321").await;

    let planned = plan(&engine, params("zip+mem:///a.zip/!", "mem:///d")).await;
    assert_eq!(planned.state, TaskState::Completed);
    let steps = planned.steps();
    let s = steps
        .iter()
        .find(|s| s.rel == rel("same-bytes.txt"))
        .expect("the paired file has a step");
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(
        s.confidence,
        norte_proto::methods::CompareConfidence::Unknown,
        "a date that does not exist does not turn into `Probable`"
    );
    assert_eq!(s.criterion, norte_proto::methods::CompareCriterion::Mtime);
    assert!(planned.done().executable);
}

/// A destination that declares NO trash: the overwrite is `Irreversible` and
/// the plan says so with its reason, before anyone approves. Nothing
/// simulated — `MemProvider::with_flags` without `TRASH` is what a bucket or
/// an SFTP declares, and the source is the real local provider.
#[tokio::test]
async fn a_destination_without_a_trash_takes_the_irreversible_path_for_real() {
    let spool_dir = tempfile::tempdir().expect("tempdir");
    let local_dir = tempfile::tempdir().expect("local tempdir");
    std::fs::create_dir(local_dir.path().join("s")).expect("mkdir s");
    std::fs::write(local_dir.path().join("s/a.txt"), b"new source").expect("write a.txt");

    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(local_dir.path())) as Arc<dyn Provider>,
    );
    let mem = Arc::new(MemProvider::with_flags(
        norte_proto::CapabilityFlags::CASE_SENSITIVE
            | norte_proto::CapabilityFlags::CASE_PRESERVING,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(Spool::new(spool_dir.path()));
    mkdir(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/a.txt", b"old").await;

    let planned = plan(&engine, params("file:///s", "mem:///d")).await;
    assert_eq!(planned.state, TaskState::Completed);
    let done = planned.done();
    assert!(
        done.counts.irreversible > 0,
        "with no trash, overwriting is not undone: {:?}",
        done.counts
    );
    let steps = planned.steps();
    let s = steps
        .iter()
        .find(|s| s.rel == rel("a.txt"))
        .expect("the paired file has a step");
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(
        s.reversal,
        Some(norte_proto::methods::StepReversal::Irreversible)
    );
    assert_eq!(
        s.reason,
        Some(norte_proto::methods::SyncReason::NoTrashOnTarget)
    );
}
