//! **What happens if the destination disappears WHILE copying.**
//!
//! A reader reported it: they started copying a big folder and, with the
//! progress bar running, deleted the destination folder. Reproducing it
//! turned out to be worse than they described — the task did not just sit
//! there, it finished saying **"completed"**, and the files were in the
//! trash.
//!
//! This runs against the REAL filesystem and not against `MemProvider`, for
//! the same reason as `engine_leaf_confined`: what makes the failure possible
//! is that the copy addresses through a DESCRIPTOR of the destination root,
//! opened once (#164), and a descriptor is not a path. `MemProvider` has no
//! descriptors to reproduce it with.
//!
//! And norte's delete goes to the trash, i.e. a `rename` (`trash_fdo::do_rename`).
//! That is where the difference lies that turns it into a silent failure
//! instead of an error: a `rename` **does not invalidate** the descriptor.
//! The directory keeps existing, with the same inode, somewhere else — and
//! the copy keeps filling it, where nobody is going to look.
//!
//! **The delete does not need to come from norte**, and that is what decides
//! the design: the folder can be deleted from another manager, with an `rm`
//! in a terminal, or from another machine over the same mount. Refusing to
//! delete it from within norte would close nothing; detecting it and saying
//! so would.
//!
//! The two tests here attack the two paths by which it is detected, and both
//! are needed: one leaves the path EMPTY (it resolves to nothing) and the
//! other leaves ANOTHER directory in its place, which is the only case where
//! identity comparison is what saves it.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine};
use norte_proto::{CollisionPolicy, ConflictKind, Error, TaskState, VPath};
use norte_vfs::Provider;

mod origin_on_request;
use origin_on_request::{Command, OnRequestSource};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// How many files the source carries.
///
/// Enough for the copy to stay alive when the test steps in. It is not a
/// deadline in disguise: the test does not wait a fixed time, it waits to SEE
/// that the copy started, and with four thousand files there is work left
/// behind that moment.
///
/// They are 1 KiB each because what is being tested triggers per ENTRY, not
/// per byte: making them bigger only makes the test's setup more expensive.
const FILES: usize = 4000;

/// Waits until `cond` is true, or gives up.
///
/// Polls instead of sleeping a fixed while, which is what this repository
/// asks for: what is being waited on is an observable FACT — that the first
/// file has landed — not that time has passed.
///
/// Thirty seconds and not ten: below this deadline there is the queuing, the
/// plan and the hydration, which does one `stat` per entry one at a time.
/// With the machine loaded by the rest of the suite, ten was this file's
/// tightest number and the first one that would have gone red for no reason.
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

/// The common setup: a source with many files and an engine that serves it.
fn tree() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    std::fs::create_dir(&source).expect("source");
    for i in 0..FILES {
        std::fs::write(source.join(format!("f{i:04}")), vec![b'x'; 1024]).expect("file");
    }
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    (dir, engine)
}

/// Launches the copy and waits until it is REALLY copying.
///
/// Returns the handle and how many files were in the destination at that
/// moment, which is what later proves the test arrived in time.
async fn copying(dir: &std::path::Path, engine: &Engine) -> (norte_core::TaskHandle, usize) {
    let handle = engine
        .copy_with_as(
            &vp("file:///source"),
            &vp("file:///destination"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("enqueues");

    // Waits until there is SOMETHING inside, not a specific name. Waiting for
    // `f0000` was one of the failed attempts: the tree is walked in the order
    // `readdir` gives, which is not alphabetical, so that file could be the
    // last one — and the test would step in with the copy already finished,
    // believing it caught it starting.
    let destination = dir.join("destination");
    assert!(
        wait(|| std::fs::read_dir(&destination).is_ok_and(|d| d.count() > 0)).await,
        "the copy never got to start"
    );
    // And it is measured RIGHT BEFORE stepping in. Counting it afterward does
    // not work, and that was the first failed attempt: after the `rename` the
    // copy keeps filling that same folder through the descriptor, so the
    // counter would end up complete no matter what reality said at the
    // moment that mattered.
    let how_many = std::fs::read_dir(&destination)
        .expect("destination")
        .count();
    (handle, how_many)
}

/// Waits for the outcome WITHOUT being able to hang.
///
/// The symptom the reader described — a task that just sits there — is
/// exactly what a bare `join()` would turn into a hung test instead of a red
/// one: nextest's deadline would trip minutes later, with a timeout report
/// instead of the message written here.
async fn outcome(handle: norte_core::TaskHandle) -> TaskState {
    tokio::time::timeout(std::time::Duration::from_mins(1), handle.join())
        .await
        .expect("the task got stuck: neither completed, nor failed, nor cancelled")
}

fn in_time(how_many: usize) {
    assert!(
        how_many < FILES,
        "the copy had already finished by the time of stepping in ({how_many} of {FILES}): \
         this test proved nothing, a bigger source is needed"
    );
}

/// **Deleting the destination mid-copy does not settle as completed.**
///
/// It is deleted the way norte deletes: to the trash, which is a `rename`.
/// What the copy has open is that folder's descriptor, so after the `rename`
/// it keeps writing inside it — in the trash, where the reader put nothing
/// and is not going to look.
///
/// Here the path ends up EMPTY, so what detects the case is that the path
/// does not resolve. The test below covers the other path.
#[tokio::test]
async fn deleting_the_destination_mid_copy_does_not_settle_as_completed() {
    let (dir, engine) = tree();
    let (handle, how_many) = copying(dir.path(), &engine).await;

    // Between the count above and this `rename` the copy cannot advance: this
    // test runs on a single-thread runtime, and while the test body does
    // synchronous I/O it holds it. This is load-bearing and invisible — the
    // day someone puts `flavor = "multi_thread"` on it to speed it up, that
    // window opens and the test can fail with the wrong message.
    let trash = dir.path().join("trash");
    std::fs::rename(dir.path().join("destination"), &trash).expect("to the trash");
    in_time(how_many);

    let state = outcome(handle).await;
    assert_ne!(
        state,
        TaskState::Completed,
        "the destination stopped existing mid-copy and the task says it copied: \
         what was copied is in {}, which is not where it was asked to go",
        trash.display()
    );
    // And "failed" alone is not enough: it has to fail SAYING WHAT. Before it
    // answered a bare `NotFound`, which in the middle of a copy of thousands
    // of files reads as "something is missing from the SOURCE" — the
    // opposite of what happened.
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "it has to say the destination is gone, not {state:?}"
    );
}

/// **And if ANOTHER folder appears in its place, it does not either.**
///
/// This is the case that really tests identity comparison, and that is why it
/// is needed on top of the one above: here the path DOES resolve — there is a
/// directory at `file:///destination` — so "I cannot find the path" saves
/// nobody. The only thing telling this destination apart from the good one is
/// that its inode is not the one of the descriptor the copy has open.
///
/// Without this test, the check could be reduced to a `stat` of the path and
/// everything would stay green, while the copy keeps filling the old
/// directory.
///
/// And it is the REALISTIC case, not a contrived one: deleting the folder and
/// creating it again is exactly what someone who wanted to start from scratch
/// would do.
#[tokio::test]
async fn if_another_folder_appears_where_the_destination_was_the_copy_stops() {
    let (dir, engine) = tree();
    let (handle, how_many) = copying(dir.path(), &engine).await;

    let destination = dir.path().join("destination");
    std::fs::rename(&destination, dir.path().join("trash")).expect("to the trash");
    // And the reader creates a new folder with the same name.
    std::fs::create_dir(&destination).expect("the new one");
    in_time(how_many);

    let state = outcome(handle).await;
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "the path resolves, but to ANOTHER directory: the copy cannot keep \
         filling the old one and say it went well. Was {state:?}"
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

/// The lone-file setup: a source that can be paused, a real local destination,
/// and the engine joining them.
async fn a_paused_file(
    dir: &std::path::Path,
) -> (Engine, Arc<norte_testkit::MemProvider>, Command) {
    let mem = Arc::new(norte_testkit::MemProvider::new());
    // Several chunks to look like a real file; the pause does not depend on
    // there being more than one (see `OnRequestSource::read`).
    {
        let mut sink = mem.write(&vp("slow:///big")).await.expect("write");
        for _ in 0..8 {
            sink.write(bytes::Bytes::from(vec![b'x'; 64 * 1024]))
                .await
                .expect("chunk");
        }
        sink.commit().await.expect("commit");
    }
    let (source, command) = OnRequestSource::wrap(Arc::clone(&mem));
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir)) as Arc<dyn Provider>
    );
    engine.register_provider(source);
    (engine, mem, command)
}

/// **And MOVING a file is the case that loses data.**
///
/// The #367 review found it while looking at the fix, and it is worse than
/// what #367 closed: a move between providers copies the leaf and then
/// DELETES the source. With the destination folder deleted mid-copy, the
/// result was the bytes in the trash, the source destroyed and the task
/// saying "completed". Here the check has to run before the delete, and not
/// before the final sentence: what is behind it is an irreversible effect.
#[tokio::test]
async fn moving_a_file_to_a_disappearing_destination_does_not_delete_the_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let destination = dir.path().join("destination");
    std::fs::create_dir(&destination).expect("destination");
    let (engine, mem, mut command) = a_paused_file(dir.path()).await;

    let handle = engine
        .move_with_as(
            &vp("slow:///big"),
            &vp("file:///destination/big"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("enqueues");

    assert!(
        command.started().await,
        "the move never got to start: this test proved nothing"
    );
    std::fs::rename(&destination, dir.path().join("trash")).expect("to the trash");
    command.follows();

    let state = outcome(handle).await;
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "a move whose destination is gone cannot end up \"completed\". Was {state:?}"
    );
    // And this is the real damage, not the sentence: the source is still there.
    assert!(
        mem.stat(&vp("slow:///big")).await.is_ok(),
        "the move deleted the source after copying it into a folder that was \
         no longer there: the bytes in the trash and the file destroyed"
    );
}

/// **#367 — copying A SINGLE file has the same hole.**
///
/// `open_leaf_root` checks the root ONCE, when opening it, which is how the
/// tree behaved before ADR 0151. From there the leaf is written and published
/// through that descriptor, so deleting the destination folder while the copy
/// is running left the file in the trash and the task saying it went well.
#[tokio::test]
async fn copying_a_file_to_a_disappearing_destination_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let destination = dir.path().join("destination");
    std::fs::create_dir(&destination).expect("destination");

    let (engine, _mem, mut command) = a_paused_file(dir.path()).await;

    let handle = engine
        .copy_with_as(
            &vp("slow:///big"),
            &vp("file:///destination/big"),
            norte_core::TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("enqueues");

    // The fact, not a deadline: the copy already delivered its first chunk.
    assert!(
        command.started().await,
        "the copy never got to start: this test proved nothing"
    );
    // With the copy PAUSED halfway, the reader deletes the destination folder.
    std::fs::rename(&destination, dir.path().join("trash")).expect("to the trash");
    command.follows();

    let state = outcome(handle).await;
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: Error::Conflict {
                    conflict: ConflictKind::DestinationGone
                }
            }
        ),
        "a lone file whose destination is gone cannot end up \"completed\": \
         the bytes are in the trash. Was {state:?}"
    );
}
