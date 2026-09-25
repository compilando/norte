//! #164 end to end and against the REAL filesystem: an intermediate
//! destination component that becomes an outward symlink BETWEEN approving
//! and applying does not redirect the write.
//!
//! This goes with `file://` on purpose. `MemProvider` has no intermediate
//! symlinks to follow nor an `openat` to refuse with, so the hole only
//! exists — and can only be shown closed — against a real filesystem.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::journal::{Journal, SqliteJournal};
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    OnUnknown, PlanHash, SyncCompareOptions, SyncFailureCause, SyncMode, SyncPlanDone,
    SyncPlanParams, SyncReportResult, SyncStepKind,
};
use norte_proto::{TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

struct Harness {
    engine: Engine,
    tree: tempfile::TempDir,
    _spool: tempfile::TempDir,
}

/// Source, destination and a directory OUTSIDE the destination to point at,
/// all three under the provider's root — which is what makes this a
/// DESTINATION leak and not a failure of the provider's root, which is
/// already checked separately.
async fn harness() -> Harness {
    let tree = tempfile::tempdir().expect("tree");
    let spool = tempfile::tempdir().expect("spool");
    std::fs::create_dir_all(tree.path().join("s/sub")).expect("source");
    std::fs::write(tree.path().join("s/sub/secret.txt"), b"secret").expect("file");
    std::fs::create_dir(tree.path().join("d")).expect("destination");
    std::fs::create_dir(tree.path().join("outside")).expect("outside");

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(tree.path())) as Arc<dyn Provider>,
    );
    engine.set_spool(Spool::new(spool.path()));
    Harness {
        engine,
        tree,
        _spool: spool,
    }
}

async fn plan(h: &Harness) -> SyncPlanDone {
    let params = SyncPlanParams {
        source: vp("file:///s"),
        dest: vp("file:///d"),
        mode: SyncMode::Update,
        compare: SyncCompareOptions::default(),
        on_unknown: OnUnknown::Copy,
        include: None,
    };
    let (handle, mut rx) = h
        .engine
        .sync_plan_as(params, 1, Actor::User)
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

/// The case from #164: the plan is approved against a clean destination and,
/// before applying it, someone swaps the intermediate directory for an
/// outward symlink. The copy CANNOT land there.
#[tokio::test]
async fn a_sync_does_not_follow_an_intermediate_symlink_out_of_its_destination() {
    let h = harness().await;
    let done = plan(&h).await;
    assert!(
        done.executable,
        "the plan is approved against a clean destination"
    );

    // The TTL window: between approving and applying, `d/sub` stops being the
    // directory the plan will create and becomes a bridge to `outside`.
    std::os::unix::fs::symlink(h.tree.path().join("outside"), h.tree.path().join("d/sub"))
        .expect("hostile symlink");

    let (state, report) = apply(&h, &done.plan_hash).await;

    assert_eq!(
        state,
        TaskState::Completed,
        "the Task ends, it does not blow up"
    );
    assert!(
        !h.tree.path().join("outside/secret.txt").exists(),
        "it did not write outside the destination"
    );
    // And it was counted as a conflict: the report's row says that step did
    // not happen, instead of staying quiet about a write that landed
    // somewhere else.
    assert!(
        report
            .failures
            .iter()
            .any(|f| { f.kind == SyncStepKind::Copy && f.cause == SyncFailureCause::Conflict }),
        "the copy comes out as a conflict: {:?}",
        report.failures
    );
}
