//! `Engine::compare_as` integration (C6 of the directory-compare plan): the
//! [`TaskKind::Compare`] Task, the coalesced-batch pump, progress that counts
//! ROWS, and clean cancellation.
//!
//! The cascade, the pairing and the walk belong to `norte-compare` and already
//! have their tests against `MemProvider`; here only what the core adds is
//! tested: the batch, the counter and the ending.
//!
//! There is no journal to check: comparing does not write a single byte (hard
//! rule 4 does not apply).

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
    VPath::parse(wire).expect("valid wire")
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

/// Engine + in-memory `MemProvider` registered under `mem`.
fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Base params: two roots, default criteria (no hash).
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

/// Two TWIN trees of `n` files under `mem:///l` and `mem:///r`.
async fn twin_trees(mem: &MemProvider, n: usize) {
    mkdir(mem, "mem:///l").await;
    mkdir(mem, "mem:///r").await;
    for i in 0..n {
        write_file(mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
}

/// Drains the channel until it closes; returns the BATCHES exactly as they
/// arrived (the batch's shape is what is being tested, not just its content).
async fn drain(mut rx: Receiver<CompareRowsBatch>) -> Vec<CompareRowsBatch> {
    let mut out = Vec::new();
    while let Some(b) = rx.recv().await {
        out.push(b);
    }
    out
}

// 1 ───────────────────────────────────────────────────────────────────────
/// Batches are bounded and COALESCED, the same contract as `search.hits`: a
/// thousand rows cannot turn into a thousand frames.
#[tokio::test]
async fn rows_arrive_in_bounded_coalesced_batches() {
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
        "a batch above the cap: {:?}",
        batches.iter().map(|b| b.rows.len()).collect::<Vec<_>>()
    );
    let rows: usize = batches.iter().map(|b| b.rows.len()).sum();
    assert_eq!(rows, 1_000, "one row per pair");
    assert!(
        batches.len() < 1_000,
        "one frame per row is not coalescing: {} batches",
        batches.len()
    );
    assert!(
        batches.iter().all(|b| b.task_id == id),
        "every batch carries the task_id of ITS comparison"
    );
}

// 2 ───────────────────────────────────────────────────────────────────────
/// Hard rule 3 at the Task boundary: cancelling ends in `Cancelled` — the
/// engine's `Err` is ONLY that, not a failure — and the batches stop.
///
/// Nothing to clean up: comparing does not write.
#[tokio::test]
async fn cancelling_the_task_stops_the_batches_and_ends_in_cancelled() {
    let (engine, mem) = setup();
    twin_trees(&mem, 3_000).await;

    let (h, mut rx) = engine
        .compare_as(params("mem:///l", "mem:///r"), Actor::User)
        .await
        .expect("compare");
    let first = rx.recv().await.expect("at least one batch");
    assert!(!first.rows.is_empty());
    h.cancel();

    let mut seen = first.rows.len();
    while let Some(b) = rx.recv().await {
        seen += b.rows.len();
    }
    assert!(
        seen < 3_000,
        "batches kept arriving after the cancel: {seen}"
    );
    assert_eq!(h.join().await, TaskState::Cancelled);
}

// 3 ───────────────────────────────────────────────────────────────────────
/// `entries_done` counts emitted ROWS, and it is a CONTRACT (C1): it is the
/// only signal by which a client detects that it missed a `compare.rows` —
/// there is no `max_hits` to count against here as there is in `fs.search`.
#[tokio::test]
async fn progress_counts_the_emitted_rows() {
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
        .map(|b| u64::try_from(b.rows.len()).expect("fits"))
        .sum();
    assert_eq!(rows, 10);
    assert_eq!(
        progress.borrow().entries_done,
        rows,
        "the last snapshot has to match what was emitted"
    );
}

// 4 ───────────────────────────────────────────────────────────────────────
/// The verdicts are the engine's, with no translation along the way: two
/// identical trees are all `Same`.
#[tokio::test]
async fn two_identical_trees_are_all_same() {
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
        "the daemon cannot emit inconsistent rows: {rows:#?}"
    );
}

// 5 ───────────────────────────────────────────────────────────────────────
/// Comparing a root against itself never becomes a Task: it is the caller's
/// error, and it is rejected before creating one.
#[tokio::test]
async fn comparing_a_root_against_itself_creates_no_task() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///d").await;

    let Err(err) = engine
        .compare_as(params("mem:///d", "mem:///d"), Actor::User)
        .await
    else {
        panic!("comparing a root against itself had to be rejected");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "was {err:?}");
}

// 6 ───────────────────────────────────────────────────────────────────────
/// The engine ACCEPTS `follow_symlinks` and IGNORES it; whoever receives it
/// from the wire has to reject it instead of silently serving a walk different
/// from the one requested. The engine is the first public boundary, so it is
/// the one that rejects it.
#[tokio::test]
async fn follow_symlinks_is_rejected_instead_of_ignored() {
    let (engine, mem) = setup();
    twin_trees(&mem, 1).await;

    let mut p = params("mem:///l", "mem:///r");
    p.follow_symlinks = true;
    let Err(err) = engine.compare_as(p, Actor::User).await else {
        panic!("follow_symlinks had to be rejected");
    };
    assert!(matches!(err, ProtoError::Unsupported), "was {err:?}");
}

// ADR 0054 ────────────────────────────────────────────────────────────────
/// #153/#145: the comparison folds the way THE ROOT folds, not the way the
/// provider folds. `MemProvider` declares `CASE_SENSITIVE` for itself and only
/// hyphenates a `+F` for the right root; if `fs.compare` asked the provider —
/// which it used to — this pair would come out as two orphans.
#[tokio::test]
async fn the_comparison_folds_the_way_the_root_folds() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///l").await;
    mkdir(&mem, "mem:///r").await;

    let fixture = |id: &str| {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
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
        "one pair, not two orphans: {:?}",
        rows.iter().map(|r| r.verdict).collect::<Vec<_>>()
    );
    assert!(
        rows[0].left.is_some() && rows[0].right.is_some(),
        "and the row carries both sides"
    );
}
