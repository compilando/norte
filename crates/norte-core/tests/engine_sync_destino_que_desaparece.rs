//! **What happens if a SYNC's destination disappears halfway** (#368).
//!
//! The twin of `engine_destino_que_desaparece`, and the exact same mechanism:
//! `sync.apply` opens its destination root ONCE per task (#164) and writes
//! through that descriptor. Deleting in norte means moving to the trash, i.e.
//! a `rename`, and a `rename` does not invalidate a descriptor: the directory
//! stays alive with the same inode somewhere else, so the sync kept filling
//! it and the report said it went well.
//!
//! **This matters more here than in a copy**, and that is the reason this debt
//! was not left for later: a sync is precisely the operation that gets set in
//! motion against a destination nobody is watching.
//!
//! This runs against the REAL filesystem and not against `MemProvider` for the
//! same reason as its twin: what makes the failure possible is a descriptor,
//! and `MemProvider` has none.
//!
//! Both cases are needed and are not the same. One leaves the path EMPTY,
//! which is detected because it does not resolve; the other leaves ANOTHER
//! directory in its place, which resolves perfectly and only identity
//! comparison catches it.

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
    VPath::parse(wire).expect("valid wire")
}

/// How many files the source carries.
///
/// Enough for the sync to stay alive when the test steps in. It is not a
/// deadline in disguise: no time is waited, it waits to SEE that something has
/// already landed, and there is work left behind that moment.
const FILES: usize = 4000;

/// Polls for a FACT, not a deadline. See `engine_destino_que_desaparece`.
async fn wait(mut cond: impl FnMut() -> bool) -> bool {
    let until = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while tokio::time::Instant::now() < until {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    false
}

/// A local engine WITH a journal and spool, a full source and an empty
/// destination.
///
/// The journal is not decoration: `sync.apply` refuses without it (hard rule
/// 4), so a bare engine would answer `Unsupported` and the test would measure
/// that instead.
async fn tree() -> (tempfile::TempDir, tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    // The spool lives OUTSIDE the tree the test manipulates: inside, it would
    // be one more entry the plan would have to look at.
    let spool = tempfile::tempdir().expect("spool");
    let source = dir.path().join("source");
    std::fs::create_dir(&source).expect("source");
    std::fs::create_dir(dir.path().join("destination")).expect("destination");
    for i in 0..FILES {
        std::fs::write(source.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("file");
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

/// Plans the whole sync and returns its hash.
async fn plan(engine: &Engine) -> PlanHash {
    plan_from(engine, "file:///source").await
}

/// The same, choosing the source: one test needs it to be the one that pauses.
async fn plan_from(engine: &Engine, source: &str) -> PlanHash {
    let (handle, mut rx) = engine
        .sync_plan_as(
            SyncPlanParams {
                source: vp(source),
                dest: vp("file:///destination"),
                mode: SyncMode::Mirror,
                compare: SyncCompareOptions::default(),
                on_unknown: OnUnknown::Copy,
                include: None,
            },
            1,
            Actor::User,
        )
        .await
        .expect("sync.plan accepted");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "the plan goes well"
    );
    done.expect("plan_done").plan_hash
}

/// Launches `sync.apply` and waits until it is REALLY writing.
///
/// Returns the handle, the report and how many files were in the destination
/// at that moment — which is what later proves the test arrived in time.
async fn applying(
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
        .expect("sync.apply accepted");
    let destination = dir.join("destination");
    assert!(
        wait(|| std::fs::read_dir(&destination).is_ok_and(|d| d.count() > 0)).await,
        "the sync never got to write anything"
    );
    // Measured RIGHT BEFORE stepping in: after the `rename` the task keeps
    // filling that same folder through the descriptor, so counting it later
    // would give the total no matter what reality said at the right instant.
    //
    // And there is no `await` between this count and the test's `rename`,
    // which is what makes it reliable: under `#[tokio::test]`'s
    // `current_thread`, the task cannot advance while the test body does
    // synchronous I/O. A `flavor = "multi_thread"` would break that without
    // warning — hence it being written down and not just assumed.
    let how_many = std::fs::read_dir(&destination)
        .expect("destination")
        .count();
    (handle, report, how_many)
}

/// That the test stepped in with the task still alive. Without this, a test
/// that arrives late passes having proved nothing.
fn in_time(how_many: usize) {
    assert!(
        how_many < FILES,
        "the sync had already finished by the time of deleting ({how_many} of {FILES}): \
         this test proved nothing"
    );
}

/// Waits for the outcome without being able to hang: the symptom being
/// chased is a task that never ends, and a bare `join()` would turn it into a
/// hung test instead of a red one.
async fn outcome(handle: norte_core::TaskHandle) -> TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(2), handle.join())
        .await
        .expect("the task got stuck instead of finishing")
}

fn gone(state: &TaskState, what_happened: &str) {
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "{what_happened}: the sync cannot keep writing where nobody is going \
         to look and say it went well. Was {state:?}"
    );
}

/// **And with a SINGLE step, which is what pins down the final check.**
///
/// With four thousand steps all three checks fire — the one on opening, the
/// periodic one and the final one — so removing any one of them lets the
/// other two catch it: the tests below test the trio, not the pieces. With one
/// step there is no periodic one and the opening one already passed, so the
/// only thing left between "I wrote" and "it went well" is the last one. This
/// is the one its own rustdoc calls "the only moment where lying closes
/// everything".
#[tokio::test]
async fn with_a_single_step_the_final_check_is_the_one_that_catches_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spool = tempfile::tempdir().expect("spool");
    std::fs::create_dir(dir.path().join("destination")).expect("destination");

    // The source is the one that can be PAUSED, so the destination goes away
    // with the single step in flight: if it went away earlier, `open_root`
    // would catch it with a `NotFound` and this test would not prove what it
    // claims to prove.
    let mem = Arc::new(norte_testkit::MemProvider::new());
    mem.mkdir(&vp("slow:///source")).await.expect("source");
    {
        let mut sink = mem.write(&vp("slow:///source/one")).await.expect("write");
        sink.write(bytes::Bytes::from(vec![b'x'; 64 * 1024]))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    let (source, mut mando) = OrigenAPeticion::nuevo(Arc::clone(&mem));

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    engine.register_provider(source);
    engine.set_spool(Spool::new(spool.path()));

    let hash = plan_from(&engine, "slow:///source").await;
    let (handle, _report) = engine
        .sync_apply_as(&hash, 1, Actor::User)
        .await
        .expect("sync.apply accepted");

    assert!(
        mando.empezo().await,
        "the sync never got to read anything: this test proved nothing"
    );
    std::fs::rename(dir.path().join("destination"), dir.path().join("trash"))
        .expect("to the trash");
    mando.sigue();

    gone(&outcome(handle).await, "a single-step plan");
}

/// **The destination folder gets deleted: the task fails and says so.**
#[tokio::test]
async fn a_sync_whose_destination_gets_deleted_fails() {
    let (dir, _spool, engine) = tree().await;
    let hash = plan(&engine).await;
    let (handle, _report, how_many) = applying(dir.path(), &engine, &hash).await;

    std::fs::rename(dir.path().join("destination"), dir.path().join("trash"))
        .expect("to the trash");
    in_time(how_many);

    gone(&outcome(handle).await, "the path no longer resolves");
}

/// **And if ANOTHER folder with the same name appears, it also fails.**
///
/// This is the one that cannot pass a plain existence check: the path
/// resolves, and the only thing that disproves the situation is that the node
/// is not the one that was opened.
#[tokio::test]
async fn if_another_folder_appears_where_the_destination_was_the_sync_stops() {
    let (dir, _spool, engine) = tree().await;
    let hash = plan(&engine).await;
    let (handle, _report, how_many) = applying(dir.path(), &engine, &hash).await;

    let destination = dir.path().join("destination");
    std::fs::rename(&destination, dir.path().join("trash")).expect("to the trash");
    std::fs::create_dir(&destination).expect("the new one");
    in_time(how_many);

    gone(
        &outcome(handle).await,
        "the path leads to ANOTHER directory",
    );
    // And what the reader sees in their new folder is what they put there: nothing.
    assert_eq!(
        std::fs::read_dir(&destination)
            .expect("the new one")
            .count(),
        0,
        "not a single file was written into the new folder"
    );
}
