//! #274: renaming by changing ONLY the case (or the normalization) on a
//! folding volume is real work, not an impossible operation.
//!
//! The batch planner already treats it that way
//! (`a_case_only_rename_is_real_work_on_a_case_insensitive_directory`), and
//! the issue said the single-item rename answered something else.
//!
//! **What these tests establish is that the path the issue names is not
//! taken.** `ops::move_task` over the SAME provider goes to
//! `rename_with_policy` and never consults `same_node` at any point — the
//! only two calls to `same_node` are in `copy_task` and in `move_by_copy`, and
//! the second is only reached when the rename returns `Unsupported` (EXDEV
//! across mounts). So a `Foo.txt → foo.txt` within the same directory runs.
//!
//! **And that was only half true, because the double was more permissive than
//! any disk.** `MemProvider::rename` allowed `a → A` when the destination
//! resolved to the source itself, modeling APFS's `rename(2)`. norte does not
//! rename with `rename(2)`: it renames WITHOUT OVERWRITING —
//! `renameat2(RENAME_NOREPLACE)`, `renamex_np(RENAME_EXCL)`, `MoveFileExW`
//! without replace — and there a destination that resolves to the same node
//! EXISTS, so the rename fails with `EEXIST`. With
//! `MemProvider::with_folding_noreplace` (#274) the double answers what the
//! disk answers, and then it shows: the rename is refused with "already
//! exists", and with `Overwrite` the sequence was *delete the destination* —
//! which is the file itself — and rename afterward something that is no
//! longer there.
//!
//! What remains is the NARROW case that still cannot be set up: a move that
//! degrades to copy+delete over EXDEV on a folding volume. It needs two real
//! mounts, one of them folding.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_proto::{CapabilityFlags, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write(mem: &MemProvider, wire: &str) {
    let mut sink = mem.write(&vp(wire)).await.expect("write");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
}

/// A provider that does NOT tell the case apart, like APFS, NTFS or exFAT.
async fn folding_engine() -> Engine {
    mount(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ))
    .await
    .0
}

/// The same, but whose no-overwrite rename sees the fold: what a disk does
/// (#274). Also returns the provider, to be able to look at what was left.
async fn folding_on_rename_engine() -> (Engine, Arc<MemProvider>) {
    mount(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    )
    .await
}

async fn mount(mem: MemProvider) -> (Engine, Arc<MemProvider>) {
    let mem = Arc::new(mem);
    mem.mkdir(&vp("mem:///home")).await.expect("home");
    write(&mem, "mem:///home/Foo.txt").await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// What names are in `mem:///home`, in BYTES and sorted by bytes.
///
/// Not `String`: two different names that collapsed to the same `U+FFFD`
/// would pass an `assert_eq!` without anyone noticing, and this is precisely
/// a test about names (rule 1).
async fn names(mem: &MemProvider) -> Vec<Vec<u8>> {
    use futures::StreamExt as _;
    let mut s = mem.list(&vp("mem:///home")).await.expect("lists");
    let mut out = Vec::new();
    while let Some(e) = s.next().await {
        let e = e.expect("entry");
        out.push(e.path.file_name().expect("leaf").as_bytes().to_vec());
    }
    out.sort();
    out
}

/// The same over a real disk directory.
fn names_in(dir: &std::path::Path) -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut out: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().as_bytes().to_vec())
        .collect();
    out.sort();
    out
}

/// `Foo.txt → foo.txt` on a folding volume: the BYTES change, so it is a real
/// rename. Rejecting it leaves the reader with no way out — the window has no
/// collision-with-retry modal the way the terminal does.
#[tokio::test]
async fn a_rename_that_only_changes_the_case_is_real_work() {
    let engine = folding_engine().await;
    let handle = engine
        .move_(&vp("mem:///home/Foo.txt"), &vp("mem:///home/foo.txt"))
        .await
        .expect("enqueues");
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "changing the case is not renaming something onto itself"
    );
}

/// And moving something exactly onto itself is NOT an error: it is a no-op,
/// which is what `rename(2)` promises when the two paths name the same file.
/// This is pinned here because it is what tells apart this case from the one
/// the "inside itself" guard does have to reject, and because a future change
/// that turned it into an error would break the rename that changes nothing.
#[tokio::test]
async fn a_rename_onto_itself_is_a_no_op_not_an_error() {
    let engine = folding_engine().await;
    let handle = engine
        .move_(&vp("mem:///home/Foo.txt"), &vp("mem:///home/Foo.txt"))
        .await
        .expect("enqueues");
    assert_eq!(handle.join().await, TaskState::Completed);
}

/// **The case from the issue, with the double answering what a disk answers.**
///
/// Renaming WITHOUT OVERWRITING sees the fold, so the destination "already
/// exists" — and it is the file itself. Rejecting it leaves the reader with no
/// way out: the window has no collision-with-retry modal the way the terminal
/// does, and the spelling was the only thing that was meant to change.
#[tokio::test]
async fn with_a_rename_that_sees_the_fold_changing_the_case_is_still_work() {
    let (engine, mem) = folding_on_rename_engine().await;
    let handle = engine
        .move_(&vp("mem:///home/Foo.txt"), &vp("mem:///home/foo.txt"))
        .await
        .expect("enqueues");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names(&mem).await,
        vec![b"foo.txt".to_vec()],
        "one file, with the new spelling"
    );
}

/// **And with `Overwrite` the file is NOT lost.**
///
/// This is the one that bites: the `Overwrite` arm deleted the destination
/// before renaming, and on a folding volume the destination IS the source. The
/// sequence was to delete the file and rename afterward something that was no
/// longer there — a `Foo.txt → foo.txt` that takes the file down with it.
#[tokio::test]
async fn with_overwrite_a_case_change_does_not_take_the_file_down() {
    use norte_core::TransferOptions;

    let (engine, mem) = folding_on_rename_engine().await;
    let handle = engine
        .move_with(
            &vp("mem:///home/Foo.txt"),
            &vp("mem:///home/foo.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("enqueues");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names(&mem).await,
        vec![b"foo.txt".to_vec()],
        "the file is still there, with the new spelling"
    );
}

/// With `Skip`, on the other hand, there is nothing to skip: the "collision"
/// is the file itself, so the rename happens all the same. Skipping it would
/// answer "there was already one there" about itself.
#[tokio::test]
async fn with_skip_the_case_change_is_not_skipped_either() {
    use norte_core::TransferOptions;

    let (engine, mem) = folding_on_rename_engine().await;
    let handle = engine
        .move_with(
            &vp("mem:///home/Foo.txt"),
            &vp("mem:///home/foo.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Skip,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("enqueues");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(names(&mem).await, vec![b"foo.txt".to_vec()]);
}

/// **A HARDLINK is not a spelling change**, even though it shares an inode.
///
/// `NodeId` is `(device, inode)`, so two different directory entries linked to
/// the same file give the same id. Deciding by identity ALONE routed
/// `mv a.txt b.txt` through the spelling path: a step to the intermediate
/// name, a clash with `b.txt` — which is still there —, a rollback, and a
/// `Conflict` where `Overwrite` was the right thing. That is why the guard
/// also requires the leaves to fold to the same key.
///
/// This runs against a REAL disk because `MemProvider` cannot do hardlinks.
#[tokio::test]
async fn a_hardlink_does_not_take_the_spelling_path() {
    use norte_core::TransferOptions;

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), b"content").expect("a");
    std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).expect("link");

    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    let handle = engine
        .move_with(
            &vp("file:///a.txt"),
            &vp("file:///b.txt"),
            TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..TransferOptions::default()
            },
        )
        .await
        .expect("enqueues");
    let state = handle.join().await;

    // What CANNOT happen: that the machine name is left lying around.
    let left = names_in(dir.path());
    assert!(
        !left.iter().any(|n| n.starts_with(b".norte-rename-")),
        "no residue from the detour: {left:?}"
    );
    assert!(
        matches!(state, TaskState::Completed),
        "with Overwrite, a hardlink is a collision under policy, not a detour: {state:?}"
    );
}

/// **Cancelling between the two steps does not leave the file with the
/// detour's name.**
///
/// The window is not cancelable on purpose: with the task's token, cancelling
/// right after the first rename made the second one *and the rollback* come
/// out without trying anything, leaving the file with a name the reader did
/// not write and answering `Cancelled` — and here a cancelled task means "the
/// tree is as it was". It is the same decision as `rename::exec`: cancellation
/// is checked BETWEEN operations, never inside one.
#[tokio::test]
async fn cancelling_between_the_two_steps_does_not_leave_the_detours_name() {
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    );
    mem.mkdir(&vp("mem:///home")).await.expect("home");
    write(&mem, "mem:///home/Foo.txt").await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .move_(&vp("mem:///home/Foo.txt"), &vp("mem:///home/foo.txt"))
        .await
        .expect("enqueues");
    // The first rename is the detour's: as soon as it applies, it cancels.
    mem.faults().cancel_after_renames(1, handle.cancel_token());
    let state = handle.join().await;

    let left = names(&mem).await;
    assert!(
        !left.iter().any(|n| n.starts_with(b".norte-rename-")),
        "not even cancelling leaves the detour's name: {left:?}"
    );
    assert_eq!(
        state,
        TaskState::Completed,
        "the window between the two steps is not cancelable"
    );
}

/// A name right at the 255-byte limit can also have its case changed.
///
/// The intermediate name goes by PREFIX and does not embed the leaf: with a
/// suffix, a 250-byte leaf gave `ENAMETOOLONG` on the first step and the
/// spelling change was impossible. This is what the corpus's `name_max_255`
/// fixture has been saying since it exists.
#[tokio::test]
async fn a_leaf_at_the_255_limit_also_changes_case() {
    let long = "A".repeat(251);
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .with_folding_noreplace(),
    );
    mem.mkdir(&vp("mem:///home")).await.expect("home");
    write(&mem, &format!("mem:///home/{long}.txt")).await;
    let engine = Engine::new();
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    let handle = engine
        .move_(
            &vp(&format!("mem:///home/{long}.txt")),
            &vp(&format!("mem:///home/{}.txt", long.to_lowercase())),
        )
        .await
        .expect("enqueues");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names(&mem).await,
        vec![format!("{}.txt", long.to_lowercase()).into_bytes()]
    );
}

/// And a REAL collision is still a collision: two different files, and the
/// destination is not touched. Without this, "treat it as a case change"
/// would come to mean "overwrite whatever is there".
#[tokio::test]
async fn a_real_collision_still_fails() {
    let (engine, mem) = folding_on_rename_engine().await;
    write(&mem, "mem:///home/other.txt").await;
    let handle = engine
        .move_(&vp("mem:///home/Foo.txt"), &vp("mem:///home/other.txt"))
        .await
        .expect("enqueues");
    assert!(
        matches!(handle.join().await, TaskState::Failed { .. }),
        "two different files still collide"
    );
    assert_eq!(
        names(&mem).await,
        vec![b"Foo.txt".to_vec(), b"other.txt".to_vec()]
    );
}
