//! #219/#218 end to end against the REAL filesystem: copying A SINGLE leaf
//! does not escape its destination either, neither by writing nor by deleting.
//!
//! This goes with `file://` on purpose, for the same reason as
//! `engine_sync_confined`: `MemProvider` has no intermediate components to
//! follow nor an `openat` to refuse with, so the hole only exists — and can
//! only be shown closed — against a real filesystem.
//!
//! What is tested here is the product's MOST COMMON operation. Until #219,
//! `ops::copy_task` used `Destination::unconfined` for a lone leaf, on the
//! argument that "a leaf hangs off no approved tree". It hangs off one: its
//! destination DIRECTORY, which is what the pane showed and what the dialog
//! names.
//!
//! **#219 is NARROWED, not closed, and the test that says so is below.** A
//! link already in place by the time the core looks for the first time still
//! diverts the copy, because from the core it is identical to a legitimate
//! `~/copies -> /mnt/disk/copies`. What is gained is that the directory is
//! resolved ONCE instead of three times plus the retries, and that a later
//! swap no longer diverts anything.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine};
use norte_proto::{CollisionPolicy, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

struct Tree {
    engine: Engine,
    dir: tempfile::TempDir,
}

/// A source, a destination with a `sub/` inside, and an `outside/` sibling of
/// the destination — all three under the provider's root, which is what makes
/// this a DESTINATION leak and not a failure of the provider's root.
fn tree() -> Tree {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("source.txt"), b"content").expect("source");
    std::fs::create_dir(dir.path().join("d")).expect("destination");
    std::fs::create_dir(dir.path().join("outside")).expect("outside");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Tree { engine, dir }
}

async fn copy(a: &Tree, to: &str, policy: CollisionPolicy) -> TaskState {
    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///source.txt"),
            &vp(to),
            norte_core::TransferOptions {
                on_collision: policy,
                ..norte_core::TransferOptions::default()
            },
            Actor::User,
        )
        .await
        .expect("enqueues");
    handle.join().await
}

/// **The residue of #219, written so nobody reads more into it than there is.**
///
/// With `d/sub -> outside` ALREADY planted by the time the core looks for the
/// first time, the copy DOES land outside. It is not an oversight: from the
/// core, that link and a legitimate `~/copies -> /mnt/disk/copies` are
/// indistinguishable — both resolve elsewhere — and rejecting both would break
/// copying to `/tmp` on macOS and to half a usrmerge distribution.
///
/// What #219 does buy for a leaf is that, once that directory is resolved
/// ONCE, staging and its publication both go through the descriptor: where
/// before there were three resolutions of `dest/sub` plus one for each of the
/// three retries, now there is one. A LATER swap no longer diverts anything.
///
/// Closing the other half needs the identity observed when APPROVING — the
/// listing the human looked at — to travel with the request, and that is wire.
#[tokio::test]
async fn a_link_already_planted_at_the_destination_cannot_be_told_apart_by_the_core() {
    let a = tree();
    let outside = a.dir.path().join("outside");
    std::os::unix::fs::symlink(&outside, a.dir.path().join("d/sub")).expect("link");

    let state = copy(&a, "file:///d/sub/loot.txt", CollisionPolicy::Fail).await;

    assert_eq!(
        state,
        TaskState::Completed,
        "the copy happens: the core cannot know this link is not legitimate"
    );
    assert!(
        outside.join("loot.txt").exists(),
        "and it lands wherever the link points — which is what a link does"
    );
}

/// And the normal case still works: a leaf to an honest destination copies.
/// Confinement cannot cost the operation it exists to protect.
#[tokio::test]
async fn a_copy_of_a_leaf_to_an_honest_destination_still_works() {
    let a = tree();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("real sub");

    let state = copy(&a, "file:///d/sub/copy.txt", CollisionPolicy::Fail).await;

    assert_eq!(state, TaskState::Completed, "{state:?}");
    assert_eq!(
        std::fs::read(a.dir.path().join("d/sub/copy.txt")).expect("arrived"),
        b"content"
    );
}

/// And `Overwrite` over an honest destination replaces, which is what it
/// promises.
#[tokio::test]
async fn a_copy_with_overwrite_to_an_honest_destination_replaces() {
    let a = tree();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("real sub");
    let destination = a.dir.path().join("d/sub/copy.txt");
    std::fs::write(&destination, b"the old one").expect("prior");

    let state = copy(&a, "file:///d/sub/copy.txt", CollisionPolicy::Overwrite).await;

    assert_eq!(state, TaskState::Completed, "{state:?}");
    assert_eq!(std::fs::read(&destination).expect("arrived"), b"content");
}

/// A symlink as a leaf goes the same way as a file: copying it is CREATING one
/// at the destination, and that creation composes a path just like the other
/// two. This checks that the confined path does not break it.
#[tokio::test]
async fn a_copy_of_a_leaf_symlink_to_an_honest_destination_works() {
    let a = tree();
    std::os::unix::fs::symlink("source.txt", a.dir.path().join("link")).expect("link");
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("real sub");

    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///link"),
            &vp("file:///d/sub/copy"),
            norte_core::TransferOptions::default(),
            Actor::User,
        )
        .await
        .expect("enqueues");

    assert_eq!(handle.join().await, TaskState::Completed);
    let meta = std::fs::symlink_metadata(a.dir.path().join("d/sub/copy")).expect("arrived");
    assert!(
        meta.file_type().is_symlink(),
        "and it is still a link: `Preserve` does not dereference it"
    );
}

/// #218 at the engine level: with `Overwrite`, the `stat` that DECIDES and the
/// `remove` that EXECUTES both go through the descriptor when there is a root.
///
/// What this test proves from here is that the confined path does the right
/// thing. That it REFUSES to escape is proven by the provider, where the
/// descriptor is observable:
/// `norte-vfs-local/tests/confined.rs`, `un_symlink_intermedio_no_redirige_un_borrado_fuera_de_la_raiz`.
#[tokio::test]
async fn a_copy_with_overwrite_deletes_and_replaces_through_the_descriptor() {
    let a = tree();
    std::fs::create_dir(a.dir.path().join("d/sub")).expect("real sub");
    let destination = a.dir.path().join("d/sub/loot.txt");
    std::fs::write(&destination, b"the old one").expect("prior");

    let state = copy(&a, "file:///d/sub/loot.txt", CollisionPolicy::Overwrite).await;

    assert_eq!(state, TaskState::Completed, "{state:?}");
    assert_eq!(std::fs::read(&destination).expect("arrived"), b"content");
}

/// A destination directory that IS a link still works (#219).
///
/// `~/copies -> /mnt/disk/copies` is a legitimate, ordinary destination; on
/// macOS so are `/tmp`, `/var` and `/etc`, and on a Linux with usrmerge so are
/// `/bin` and `/lib`. The first version of this change rejected all of them
/// with `EscapesRoot` — the identity check sees the link on one side and the
/// real directory on the other — and the security review uncovered it. Now it
/// is confined the same way and the only thing skipped is that check, which
/// over a link could say nothing anyway.
#[tokio::test]
async fn a_destination_directory_that_is_a_link_still_works() {
    let a = tree();
    let real = a.dir.path().join("storage");
    std::fs::create_dir(&real).expect("storage");
    std::os::unix::fs::symlink(&real, a.dir.path().join("d/linked")).expect("legitimate link");

    let state = copy(&a, "file:///d/linked/copy.txt", CollisionPolicy::Fail).await;

    assert_eq!(
        state,
        TaskState::Completed,
        "a legitimate `~/copies -> /mnt/disk` cannot stop working: {state:?}"
    );
    assert_eq!(
        std::fs::read(real.join("copy.txt")).expect("arrived at the real place"),
        b"content"
    );
}

/// And a RECURSIVE copy into a linked directory does not break either: its
/// root is opened by `copy_tree` over the `to` it just created, and since
/// `open_leaf_root` lives INSIDE the leaf's arms it no longer inherits theirs
/// nor pays their three syscalls.
#[tokio::test]
async fn a_tree_into_a_linked_directory_still_works() {
    let a = tree();
    std::fs::create_dir(a.dir.path().join("tree")).expect("tree");
    std::fs::write(a.dir.path().join("tree/leaf.txt"), b"inside").expect("leaf");
    let real = a.dir.path().join("storage");
    std::fs::create_dir(&real).expect("storage");
    std::os::unix::fs::symlink(&real, a.dir.path().join("d/linked")).expect("legitimate link");

    let handle = a
        .engine
        .copy_with_as(
            &vp("file:///tree"),
            &vp("file:///d/linked/copy"),
            norte_core::TransferOptions::default(),
            Actor::User,
        )
        .await
        .expect("enqueues");

    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        std::fs::read(real.join("copy/leaf.txt")).expect("the tree arrived"),
        b"inside"
    );
}
