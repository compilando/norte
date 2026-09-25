//! The destination directory's anchor (#295, ADR 0073) against the REAL
//! filesystem.
//!
//! This is the half ADR 0072 left open: a link **already planted** by the time
//! the core looks for the first time. From inside the core, that link and a
//! legitimate `~/copies -> /mnt/disk/copies` are identical — both resolve to
//! somewhere else — so whoever tells them apart has to be whoever LOOKED: the
//! client that listed the directory and retained its identity.
//!
//! This goes with `file://` for the same reason as `engine_leaf_confined`:
//! `MemProvider` has no links to follow nor node identity to compare.
#![cfg(unix)]

use std::sync::Arc;

use norte_core::{Actor, Engine, TransferOptions};
use norte_proto::{CollisionPolicy, DirAnchor, TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

struct Tree {
    engine: Engine,
    dir: tempfile::TempDir,
}

/// A source (file and tree), a destination `d/` with a REAL `sub/` inside, and
/// an `outside/` sibling that an attacker would want to divert the write to.
fn tree() -> Tree {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("source.txt"), b"content").expect("source");
    std::fs::create_dir(dir.path().join("tree")).expect("source tree");
    std::fs::write(dir.path().join("tree/leaf.txt"), b"leaf").expect("leaf");
    std::fs::create_dir(dir.path().join("d")).expect("destination");
    std::fs::create_dir(dir.path().join("d/sub")).expect("sub");
    std::fs::create_dir(dir.path().join("outside")).expect("outside");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(dir.path())) as Arc<dyn Provider>
    );
    Tree { engine, dir }
}

/// What the client does when LISTING: retain the identity of what it looks at.
async fn anchor(a: &Tree, dir: &str) -> DirAnchor {
    a.engine
        .dir_anchor(&vp(dir))
        .await
        .expect("ask")
        .expect("the local provider knows how to identify nodes")
}

/// The EXACT shape of the rejection: a conflict that says "this escapes the
/// approved root", not an I/O error nor a `NotFound`. Frontends paint by
/// category, so the category is the contract.
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

async fn copy(a: &Tree, from: &str, to: &str, anchor: Option<DirAnchor>) -> TaskState {
    let handle = a
        .engine
        .copy_anchored(
            &vp(from),
            &vp(to),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
            anchor,
        )
        .await
        .expect("enqueues");
    handle.join().await
}

/// **The case from the issue.** The human listed `d/sub` — a real directory —
/// and approved copying there. By the time the copy runs, `d/sub` is a link to
/// `outside`. The core still cannot tell it apart from a legitimate link; the
/// anchor can, because it does not speak of links but of NODES.
#[tokio::test]
async fn a_destination_replaced_by_a_link_no_longer_receives_the_copy() {
    let a = tree();
    let seen = anchor(&a, "file:///d/sub").await;

    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove the real one");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("plant the link");

    let state = copy(
        &a,
        "file:///source.txt",
        "file:///d/sub/loot.txt",
        Some(seen),
    )
    .await;

    refused(&state);
    assert!(
        !a.dir.path().join("outside/loot.txt").exists(),
        "and NOTHING landed on the other side of the link"
    );
}

/// Without an anchor, the 0.53 behavior: the copy happens and lands wherever
/// the link points. This exists so the difference comes from the ANCHOR and
/// not from something else that changed at the same time.
#[tokio::test]
async fn without_an_anchor_the_same_link_still_diverts_the_copy() {
    let a = tree();
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("link");

    let state = copy(&a, "file:///source.txt", "file:///d/sub/loot.txt", None).await;

    assert_eq!(state, TaskState::Completed);
    assert!(
        a.dir.path().join("outside/loot.txt").exists(),
        "the ADR 0072 residue, intact when nobody sends an anchor"
    );
}

/// And the correct anchor does not cost the operation: same node, the copy
/// happens.
#[tokio::test]
async fn the_real_directorys_anchor_lets_the_copy_through() {
    let a = tree();
    let seen = anchor(&a, "file:///d/sub").await;

    let state = copy(
        &a,
        "file:///source.txt",
        "file:///d/sub/copy.txt",
        Some(seen),
    )
    .await;

    assert_eq!(state, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/copy.txt").exists());
}

/// **A LEGITIMATE link does not break**, which is the reason ADR 0072 could
/// not compare identities outright: on macOS `/tmp`, `/var` and `/etc` are
/// links, and on a Linux with usrmerge so are `/bin` and `/lib`.
///
/// The anchor is taken by FOLLOWING the link, so listing `link/` and listing
/// `d/sub/` give the same one: the same destination under two names cannot be
/// two destinations.
#[tokio::test]
async fn a_legitimate_link_to_the_approved_directory_passes() {
    let a = tree();
    std::os::unix::fs::symlink(a.dir.path().join("d/sub"), a.dir.path().join("link"))
        .expect("legitimate link");

    let via_link = anchor(&a, "file:///link").await;
    assert_eq!(
        via_link,
        anchor(&a, "file:///d/sub").await,
        "the same node under two names gives the same anchor"
    );

    let state = copy(
        &a,
        "file:///source.txt",
        "file:///link/copy.txt",
        Some(via_link),
    )
    .await;

    assert_eq!(state, TaskState::Completed);
    assert!(a.dir.path().join("d/sub/copy.txt").exists());
}

/// An anchor nobody issued authorizes nothing. This is the property that makes
/// the process secret matter: without it, whoever knew the format could forge
/// one.
#[tokio::test]
async fn a_made_up_anchor_does_not_authorize() {
    let a = tree();
    let state = copy(
        &a,
        "file:///source.txt",
        "file:///d/sub/copy.txt",
        Some(DirAnchor::new(
            "0123456789abcdef0123456789abcdef".to_owned(),
        )),
    )
    .await;

    refused(&state);
    assert!(!a.dir.path().join("d/sub/copy.txt").exists());
}

/// A TREE creates its destination, so the anchor is checked against the parent
/// and by path. What this proves is that it IS checked too: without this,
/// copying a directory would lose the defense that copying a file gains.
#[tokio::test]
async fn a_tree_toward_a_replaced_parent_is_not_copied_either() {
    let a = tree();
    let seen = anchor(&a, "file:///d/sub").await;

    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("link");

    let state = copy(&a, "file:///tree", "file:///d/sub/tree", Some(seen)).await;

    refused(&state);
    assert!(
        !a.dir.path().join("outside/tree").exists(),
        "not even the tree's root directory was created"
    );
}

/// And moving by copy inherits it: an `fs.move` that degrades to copy+delete
/// writes just like a copy, and also deletes the source afterward.
#[tokio::test]
async fn moving_to_a_replaced_destination_neither_writes_nor_deletes_the_source() {
    let a = tree();
    let seen = anchor(&a, "file:///d/sub").await;
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("link");

    // Cross-provider is not needed: what is being tested is the move's anchored
    // path, and `move_anchored` carries it to `move_by_copy` the same way.
    let handle = a
        .engine
        .move_anchored(
            &vp("file:///source.txt"),
            &vp("file:///d/sub/loot.txt"),
            TransferOptions {
                on_collision: CollisionPolicy::Fail,
                ..TransferOptions::default()
            },
            Actor::User,
            Some(seen),
        )
        .await
        .expect("enqueues")
        .join()
        .await;

    refused(&handle);
    assert!(
        !a.dir.path().join("outside/loot.txt").exists(),
        "it did not write to the other side"
    );
    assert!(
        a.dir.path().join("source.txt").exists(),
        "and it did not delete the source: a move that does not place does not delete"
    );
}

/// **Creating a file is also anchored** (#290), and it is where the anchor is
/// worth MORE, not less.
///
/// `fs.create` is the only wire method whose success hands a path to a program
/// OUTSIDE norte: the window creates the file to open it with the desktop's
/// editor. With the link planted between the listing and the confirmation, it
/// is not an empty file that gets lost — it is the whole editing session the
/// human writes afterward, in a directory they were not looking at.
#[tokio::test]
async fn creating_a_file_at_a_replaced_destination_is_refused() {
    let a = tree();
    let seen = anchor(&a, "file:///d/sub").await;

    // The attacker swaps `d/sub` for a link to `outside/`.
    std::fs::remove_dir(a.dir.path().join("d/sub")).expect("remove sub");
    std::os::unix::fs::symlink(a.dir.path().join("outside"), a.dir.path().join("d/sub"))
        .expect("plant the link");

    let state = a
        .engine
        .create_file_as(&vp("file:///d/sub/draft.md"), Some(seen), Actor::User)
        .await
        .expect("enqueues")
        .join()
        .await;

    refused(&state);
    assert!(
        !a.dir.path().join("outside/draft.md").exists(),
        "it created nothing on the other side of the link"
    );
}

/// And without an anchor it behaves as before #295: it is created wherever the
/// path says.
///
/// The check is an IMPROVEMENT that whoever lists can ask for, not a new
/// requirement — a `norte` against a hand-typed path keeps working.
#[tokio::test]
async fn creating_without_an_anchor_still_creates() {
    let a = tree();
    let state = a
        .engine
        .create_file_as(&vp("file:///d/sub/draft.md"), None, Actor::User)
        .await
        .expect("enqueues")
        .join()
        .await;

    assert_eq!(state, TaskState::Completed, "{state:?}");
    assert!(a.dir.path().join("d/sub/draft.md").is_file());
}
