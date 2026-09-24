//! **A copy that failed cannot leave an undo that deletes what YOU put
//! there.**
//!
//! This comes from #369, which a reviewer found while looking at #362's fix
//! (ADR 0151). The chain is short and all its damage is in the last link:
//!
//! 1. the copy writes through the destination root's descriptor, and the
//!    journal notes the LOGICAL path (`Mutation::Created(/destination/f0001)`);
//! 2. if that folder was deleted — in norte, a `rename` to the trash — the
//!    bytes go to the trash while the journal keeps noting
//!    `/destination/...`;
//! 3. since ADR 0151 the task FAILS, which is correct, and those entries stay
//!    there describing files that are not at those paths;
//! 4. the natural thing after "the destination folder is no longer there" is
//!    to recreate it and repeat the copy. Now those paths DO exist, and what
//!    is inside them is the good copy;
//! 5. undoing that failed batch deletes the good copy.
//!
//! In other words: a delete caused by an operation that never happened.
//!
//! The answer is ADR 0152: a `created` entry notes the IDENTITY of what it
//! created, and its undo refuses when what is at that path is not that. The
//! identity is asked for through the destination root's DESCRIPTOR, not by
//! path, because in this specific case the path no longer leads there —
//! asking by path leaves without identity exactly the entries that need it.
//!
//! This runs against the REAL filesystem, like `engine_destino_que_desaparece`
//! and for the same reason: what makes the case possible is a descriptor that
//! survives a `rename`, and `MemProvider` has no descriptors.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Engine, Journal, SqliteJournal};
use norte_proto::{CollisionPolicy, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// Enough for the copy to stay alive when the test steps in.
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

/// Leaves a half-done copy with the destination deleted, and returns the
/// journal.
///
/// This is the starting state for both tests: what is left after the reader
/// deletes the destination folder while it was being copied to.
async fn copy_with_the_destination_deleted(
    dir: &std::path::Path,
) -> (Engine, Arc<SqliteJournal>, TaskState) {
    let source = dir.join("source");
    std::fs::create_dir(&source).expect("source");
    for i in 0..FILES {
        std::fs::write(source.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("file");
    }

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    // `with_journal` and not `with_observer`: the latter records but does not
    // open undo's door, and this test needs to be able to undo.
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir)) as Arc<dyn Provider>
    );

    // A mutation BEFORE the copy, acting as the cut point for undo:
    // `undo_after` requires the cut to NAME an entry that exists, and refuses
    // a zero — which is what a stale cursor gives and would select someone's
    // entire history.
    engine
        .mkdir(&vp("file:///mark"))
        .await
        .expect("mark")
        .join()
        .await;

    let handle = engine
        .copy_with_as(
            &vp("file:///source"),
            &vp("file:///destination"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            norte_core::Actor::User,
        )
        .await
        .expect("enqueues");

    let destination = dir.join("destination");
    assert!(
        wait(|| std::fs::read_dir(&destination).is_ok_and(|d| d.count() > 0)).await,
        "the copy never got to start"
    );
    let when_deleted = std::fs::read_dir(&destination)
        .expect("destination")
        .count();
    std::fs::rename(&destination, dir.join("trash")).expect("to the trash");
    assert!(
        when_deleted < FILES,
        "the copy had already finished when deleting ({when_deleted} of {FILES}): \
         this test proved nothing"
    );

    let state = tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("the task got stuck");
    (engine, journal, state)
}

/// **And a copy that went WELL still undoes entirely.**
///
/// The identity check has two different readers: the copy notes what it sees
/// through the root's DESCRIPTOR (`fstatat`) and undo reads by PATH
/// (`symlink_metadata`). If those two stopped agreeing, every undo of a local
/// copy would block, and the symptom would be an undo saying something else is
/// there — with the suite green, because the other undo tests run against
/// `mem://`, where both sides call the SAME function and the asymmetry does
/// not exist.
///
/// So this test does not test a feature, it tests that two ways of looking at
/// the same inode still agree. It runs against disk for that reason.
#[tokio::test]
async fn a_copy_that_went_well_undoes_entirely() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    std::fs::create_dir(&source).expect("source");
    for i in 0..3 {
        std::fs::write(source.join(format!("f{i}")), b"x").expect("file");
    }

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );

    engine
        .mkdir(&vp("file:///mark"))
        .await
        .expect("mark")
        .join()
        .await;
    let state = engine
        .copy_with_as(
            &vp("file:///source"),
            &vp("file:///destination"),
            norte_core::TransferOptions::default(),
            norte_core::Actor::User,
        )
        .await
        .expect("enqueues")
        .join()
        .await;
    assert!(
        matches!(state, TaskState::Completed),
        "the copy had to go well, was {state:?}"
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("destination"))
            .expect("destination")
            .count(),
        3
    );

    let cut = journal
        .journal()
        .entries()
        .await
        .expect("entries")
        .first()
        .expect("the mark is there")
        .seq;
    let (handle, report) = engine.undo_after(cut, None).await.expect("undo");
    let _ = tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("undo got stuck");
    let r = report.lock().expect("lock").clone();

    assert!(
        r.blocked.is_none(),
        "nobody touched anything: undo had no reason to stop. {r:?}"
    );
    assert!(
        r.undone >= 4,
        "the three files and their folder had to come back: {r:?}"
    );
    assert!(
        !dir.path().join("destination").exists(),
        "and the destination had to end up undone: {r:?}"
    );
}

/// **What the journal noted is not where it says — but it says WHAT it was.**
///
/// This test is not the damage, it is the premise: after the failure there are
/// `created` entries pointing at empty paths, and what saves them from being a
/// trap is that each one notes the identity of what it created. The one below
/// shows what that is for.
///
/// The identity has to be in ALL of them, and that is where this test bites:
/// the obvious implementation — asking `Provider::node_id(path)` right after
/// publishing — leaves without identity the ones written after the delete,
/// which are most of them and exactly the ones that matter.
#[tokio::test]
async fn a_failed_copy_leaves_entries_pointing_at_empty_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_engine, journal, state) = copy_with_the_destination_deleted(dir.path()).await;
    assert!(
        matches!(state, TaskState::Failed { .. }),
        "the task has to fail (ADR 0151), was {state:?}"
    );

    let entries = journal.journal().entries().await.expect("entries");
    let created: Vec<&norte_core::JournalEntry> = entries
        .iter()
        .filter(|e| e.op == "created" && e.path.starts_with(b"file:///destination/"))
        .collect();
    assert!(
        !created.is_empty(),
        "without entries there is nothing to prove: the copy never noted anything"
    );
    // And none of those paths has anything: the bytes are in the trash.
    assert_eq!(
        std::fs::read_dir(dir.path().join("destination"))
            .ok()
            .map(Iterator::count),
        None,
        "the destination folder does not exist, and the journal says it created files inside"
    );
    // And each one says WHAT it created, or its `delete` is a blind delete.
    let without_identity = created
        .iter()
        .filter(|e| e.reversal == "delete" && e.reversal_ref.is_none())
        .count();
    assert_eq!(
        without_identity,
        0,
        "{without_identity} of {} entries promise a `delete` without saying \
         WHAT they created: that undo deletes whatever is at that path, which \
         is #369's damage",
        created.len()
    );
}

/// **And undoing it cannot take the GOOD copy down with it.**
///
/// This is the damage, with the realistic gesture: the copy fails, the reader
/// recreates the folder and repeats it. Now those paths have the good copy
/// inside. Undoing the failed batch must not touch it.
#[tokio::test]
async fn undoing_the_failed_copy_does_not_delete_what_was_put_there_afterward() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (engine, journal, _state) = copy_with_the_destination_deleted(dir.path()).await;

    // The reader recreates the folder and REPEATS the copy, which is what
    // anyone does upon reading "the destination folder is no longer there".
    // This is simulated by putting a file at EVERY path the journal noted.
    //
    // That all of them are there is what makes this the real case and not a
    // lab one: undo walks from newest to oldest and STOPS at the first path it
    // cannot find. With only one recreated, it stops at the second and
    // touches nothing — the file is saved by a failure's ordering, not
    // because anyone is protecting it. With the copy repeated there is
    // nothing to stop it.
    let destination = dir.path().join("destination");
    std::fs::create_dir(&destination).expect("the new one");
    let entries = journal.journal().entries().await.expect("entries");
    let created: Vec<Vec<u8>> = entries
        .iter()
        .filter(|e| e.op == "created" && e.path.starts_with(b"file:///destination/"))
        .map(|e| e.path.clone())
        .collect();
    assert!(
        !created.is_empty(),
        "without entries there is nothing to undo"
    );
    for p in &created {
        let name = std::str::from_utf8(&p[b"file:///destination/".len()..]).expect("utf8");
        std::fs::write(
            destination.join(name),
            b"the GOOD copy, from the second time",
        )
        .expect("repeated");
    }
    let theirs = destination
        .join(std::str::from_utf8(&created[0][b"file:///destination/".len()..]).expect("utf8"));

    // And undoes that batch: the cut is the mark, i.e. the whole copy.
    let entries = journal.journal().entries().await.expect("entries");
    let cut = entries.first().expect("the mark is there").seq;
    let (handle, report) = engine.undo_after(cut, None).await.expect("undo");
    let _ = tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("undo got stuck");
    let r = report.lock().expect("lock").clone();

    assert!(
        theirs.exists(),
        "undoing a copy that FAILED took down a file that copy never wrote: \
         the reader put it there when repeating it. Report: {r:?}"
    );
    // And stated the right way round: nothing reverted from those paths, all
    // COUNTED, and the session does NOT stop (#371). That it does not stop
    // matters as much as that it does not delete: if this blocked, editing a
    // single copied file with any editor that saves atomically — vim, VS
    // Code, `sed -i`, they all change the inode — would leave the whole copy
    // undone because of that one file.
    assert_eq!(
        r.undone, 0,
        "there was nothing to undo at those paths: {r:?}"
    );
    // One more than the files: the `destination` folder itself is also a
    // `created`, and the one that exists now was created by the reader —
    // another node, same answer.
    assert_eq!(
        r.skipped_not_ours,
        created.len() as u64 + 1,
        "ALL of them are skipped and it says how many, not just the first: {r:?}"
    );
    assert!(
        r.blocked.is_none(),
        "and it does not stop: what is there was put by the reader, it is not \
         a divergence nobody can explain. {r:?}"
    );
    // Not one, not "almost none": since it no longer stops at the first one,
    // this really checks the check looks at all of them.
    for p in &created {
        let name = std::str::from_utf8(&p[b"file:///destination/".len()..]).expect("utf8");
        assert!(
            destination.join(name).exists(),
            "undo took down {name}, which the reader put there: {r:?}"
        );
    }
}
