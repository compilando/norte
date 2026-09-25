//! The destination's anchor through the EMBEDDED path (#301, ADR 0073/0076).
//!
//! `engine_dest_anchor` tests that the ENGINE refuses when someone passes it
//! the anchor. This tests the other half, which was missing: that
//! `Backend::Embedded` PASSES it — remembering, as the SDK does over the wire,
//! the identity of every directory it itself listed.
//!
//! It matters because `ntc` runs embedded by default (`--daemon` is the
//! exception), so without this the frontend with the most reason for the
//! check — the one that launches `$EDITOR` on the file `fs.create` just
//! created — was exactly the one that lacked it.
//!
//! `file://` and not `MemProvider` for the same reason as the other file:
//! without links to follow nor node identity to compare there is nothing to
//! test.
#![cfg(unix)]

use std::sync::Arc;

use norte_core::backend::Backend;
use norte_core::{Engine, TransferOptions};
use norte_proto::{TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

struct Tree {
    backend: Backend,
    dir: tempfile::TempDir,
}

/// A source, a real `d/sub` destination, and an `outside/` an attacker would
/// want to divert the write to.
fn tree() -> Tree {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("source.txt"), b"content").expect("source");
    std::fs::create_dir(dir.path().join("d")).expect("destination");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("outside")).expect("outside");
    // With a client's anchor memory, which is what `embedded::engine_in`
    // installs and therefore what a frontend's engine has (#317). An engine
    // without it anchors nothing — its own test pins that down.
    let engine = Engine::new().with_client_anchors();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Tree {
        backend: Backend::Embedded(Arc::new(engine)),
        dir,
    }
}

/// What a PANE does when opening a directory: list it and retain its anchor.
///
/// Both things, and separately, because the backend does not anchor
/// everything it lists: the side tree and a script's `fs.list` also go
/// through `list`, and since remembering overwrites, either of them would
/// re-bless the pane's anchor with whatever it saw at that moment (#301).
async fn list_like_a_pane(backend: &Backend, dir: &str) {
    backend.list(&vp(dir)).await.expect("lists");
    backend.remember_listing_anchor(&vp(dir)).await;
}

/// Removes the real directory and leaves a link to `outside` with its name:
/// the whole attack, in two syscalls.
fn replace_with_link(a: &Tree) {
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove the real one");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("plant the link");
}

/// The EXACT shape of the rejection, same as in `engine_dest_anchor`:
/// frontends paint by category, so the category is the contract.
#[track_caller]
fn refused(state: &TaskState) {
    assert!(
        matches!(
            state,
            TaskState::Failed {
                error: norte_proto::Error::Conflict {
                    conflict: norte_proto::ConflictKind::EscapesRoot
                }
            }
        ),
        "it had to refuse by destination identity, was {state:?}"
    );
}

/// **The case from the issue, over `fs.create`.** The pane listed `d/sub`;
/// between that and the dialog's Enter, `d/sub` became a link to `outside`.
/// The file is NOT created on the other side — which is what `$EDITOR` would
/// have opened.
#[tokio::test]
async fn creating_at_a_destination_replaced_by_a_link_is_refused() {
    let a = tree();
    // What a pane does when opening the directory, and the only thing needed
    // for the anchor to exist: list it through the backend.
    list_like_a_pane(&a.backend, "file:///d/sub").await;

    replace_with_link(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notes.txt"))
        .await
        .expect("enqueues");
    refused(&task.join().await);
    assert!(
        !a.dir.path().join("outside/notes.txt").exists(),
        "and nothing landed on the other side of the link"
    );
}

/// And the same for copying: `pane.copy` over the pane that was listed.
#[tokio::test]
async fn copying_to_a_destination_replaced_by_a_link_is_refused() {
    let a = tree();
    list_like_a_pane(&a.backend, "file:///d/sub").await;

    replace_with_link(&a);

    let task = a
        .backend
        .copy(
            &vp("file:///source.txt"),
            &vp("file:///d/sub/loot.txt"),
            TransferOptions::default(),
        )
        .await
        .expect("enqueues");
    refused(&task.join().await);
    assert!(!a.dir.path().join("outside/loot.txt").exists());
}

/// And moving, which is the other anchored write.
#[tokio::test]
async fn moving_to_a_destination_replaced_by_a_link_is_refused() {
    let a = tree();
    list_like_a_pane(&a.backend, "file:///d/sub").await;

    replace_with_link(&a);

    let task = a
        .backend
        .move_(
            &vp("file:///source.txt"),
            &vp("file:///d/sub/loot.txt"),
            TransferOptions::default(),
        )
        .await
        .expect("enqueues");
    refused(&task.join().await);
    assert!(!a.dir.path().join("outside/loot.txt").exists());
    assert!(
        a.dir.path().join("source.txt").exists(),
        "and the source is still where it was: a refused move deletes nothing"
    );
}

/// **A listing that is NOT a screen does not re-bless the anchor** (#301).
///
/// The side tree asks for one branch per loop iteration, and a Lua script can
/// call `fs.list` whenever it wants. If those listings wrote to the cache, it
/// would be enough for one of them to pass over the destination AFTER the
/// swap for the human's copy to pass the check against the attacker's node.
#[tokio::test]
async fn a_listing_that_is_not_a_panes_does_not_rebless_the_anchor() {
    let a = tree();
    list_like_a_pane(&a.backend, "file:///d/sub").await;

    replace_with_link(&a);
    // The side tree passes through there and sees the link already in place.
    a.backend.list(&vp("file:///d/sub")).await.expect("lists");

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notes.txt"))
        .await
        .expect("enqueues");
    refused(&task.join().await);
}

/// Without having listed the destination there is no anchor to send, and then
/// this behaves like 0.53. This is here so the difference comes from the
/// ANCHOR and not from something else: it is the same link and the same
/// backend.
#[tokio::test]
async fn without_listing_the_destination_there_is_no_anchor_and_the_link_diverts() {
    let a = tree();
    replace_with_link(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notes.txt"))
        .await
        .expect("enqueues");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(a.dir.path().join("outside/notes.txt").exists());
}

/// And a FRONTEND's does have it, which is the other half and the one that can
/// be deleted without anything going red (#317).
///
/// `embedded::engine_in` is the only constructor that calls
/// `with_client_anchors`. Without this test, removing that call silently
/// leaves the TUI and the CLI without an anchor check.
#[test]
fn a_frontends_engine_does_anchor() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(norte_core::embedded::engine_in(dir.path()).has_client_anchors());
    assert!(
        !Engine::new().has_client_anchors(),
        "and one that does not go through there, does not"
    );
}

/// **The DAEMON's engine cannot anchor, no matter who mounts a
/// `Backend::Embedded` over it** (#317).
///
/// The anchor says who LOOKED, and that only means something in a process
/// with one client. The daemon has many, so a shared cache would pass client
/// A's listing to client B's write. The property is held by the type — the
/// memory is installed by `with_client_anchors`, and only
/// `embedded::engine_in` calls it — and this pins it down from the outside:
/// same listing and same swap, and here the write DOES happen, because
/// without an anchor it is 0.53.
#[tokio::test]
async fn the_daemons_engine_does_not_anchor_even_with_an_embedded_backend_mounted() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("d")).expect("destination");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("outside")).expect("outside");
    // As the daemon builds it: without going through `embedded::engine_in`.
    let engine = Engine::new();
    assert!(
        !engine.has_client_anchors(),
        "an engine that is not a frontend's has no anchor memory"
    );
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    let a = Tree {
        backend: Backend::Embedded(Arc::new(engine)),
        dir,
    };

    list_like_a_pane(&a.backend, "file:///d/sub").await;
    replace_with_link(&a);

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notes.txt"))
        .await
        .expect("enqueues");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(
        a.dir.path().join("outside/notes.txt").exists(),
        "no client memory means no anchor, and no anchor is the usual 0.53"
    );
}

/// The real directory, listed and untouched: the anchor does not cost the
/// operation. Without this test, "always refuse" would pass the three above.
#[tokio::test]
async fn a_destination_that_stays_the_same_lets_creation_through() {
    let a = tree();
    list_like_a_pane(&a.backend, "file:///d/sub").await;

    let task = a
        .backend
        .create_file(&vp("file:///d/sub/notes.txt"))
        .await
        .expect("enqueues");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/notes.txt").exists());
}
