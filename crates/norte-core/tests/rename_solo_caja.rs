//! Renaming by changing ONLY the case, over a destination that folds (#274).
//!
//! `Foo.txt → foo.txt` on APFS, HFS+, NTFS, exFAT or an SMB share that folds
//! is real work: the filesystem keeps the new name even though both names
//! refer to the same inode. The same goes for NFD → NFC.
//!
//! The batch planner already knows this — `rename/plan.rs` has
//! `a_case_only_rename_is_real_work_on_a_case_insensitive_directory` and
//! emits a real step — so a window with two renames cannot give opposite
//! answers to the same input.

use std::sync::Arc;

use norte_core::{Actor, Engine, TransferOptions};
use norte_proto::{CapabilityFlags, CollisionPolicy, TaskState, VPath};
use norte_testkit::{MemProvider, Normalization};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// A provider that FOLDS, like APFS: without `CASE_SENSITIVE` and with
/// insensitive normalization, meaning `A.txt` and `a.txt` are the same node.
fn folding_engine() -> (Engine, Arc<MemProvider>) {
    let caps = MemProvider::new().capabilities().flags & !CapabilityFlags::CASE_SENSITIVE;
    let mem =
        Arc::new(MemProvider::with_flags(caps).with_normalization(Normalization::Insensitive));
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

async fn write(mem: &MemProvider, wire: &str, bytes: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(bytes::Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn move_(engine: &Engine, from: &str, to: &str) -> TaskState {
    let handle = engine
        .move_with_as(
            &vp(from),
            &vp(to),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("enqueues");
    handle.join().await
}

/// **The case from the issue.** Changing only the case is NOT moving something
/// onto itself: it is the rename `F2` does in any file manager.
#[tokio::test]
async fn a_rename_that_only_changes_the_case_happens() {
    let (engine, mem) = folding_engine();
    write(&mem, "mem:///Foo.txt", b"content").await;

    assert_eq!(
        move_(&engine, "mem:///Foo.txt", "mem:///foo.txt").await,
        TaskState::Completed,
        "the same node under another name is real work, not self-destruction"
    );
}

/// **The exact same path byte for byte is NOT rejected: the rename is a no-op
/// that succeeds**, just like `rename(2)` with `old` and `new` pointing at the
/// same file, which POSIX defines as success doing nothing.
///
/// It is an asymmetry with `fs.copy`, which returns `InvalidPath` for
/// `from == to` — and there it IS needed, because copying ONTO itself with
/// `Overwrite` deletes the destination before reading the source. A rename
/// reads nothing, so there is nothing to destroy. The test pins this down so
/// nobody "fixes" one of the two into the other by accident.
#[tokio::test]
async fn moving_onto_itself_is_a_no_op_that_succeeds() {
    let (engine, mem) = folding_engine();
    write(&mem, "mem:///Foo.txt", b"content").await;

    assert_eq!(
        move_(&engine, "mem:///Foo.txt", "mem:///Foo.txt").await,
        TaskState::Completed
    );
    let e = mem.stat(&vp("mem:///Foo.txt")).await.expect("still there");
    assert_eq!(e.size, Some(7), "and nothing was lost along the way");
}

/// And the file is still there, with its content: a case-only rename resolved
/// as "same thing, doing nothing" and one that deleted the source read the
/// same from outside if nobody looks at the content.
#[tokio::test]
async fn the_content_survives_a_case_only_rename() {
    let (engine, mem) = folding_engine();
    write(&mem, "mem:///Foo.txt", b"content").await;

    assert_eq!(
        move_(&engine, "mem:///Foo.txt", "mem:///foo.txt").await,
        TaskState::Completed
    );
    let e = mem.stat(&vp("mem:///foo.txt")).await.expect("still there");
    assert_eq!(e.size, Some(7));
}
