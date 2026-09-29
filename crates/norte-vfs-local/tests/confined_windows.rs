//! Writes that can't escape their root, on Windows (#217, ADR 0160).
//!
//! `tests/confined.rs` is the contract; these are its cases with a
//! junction as the hostile component (a directory symlink needs a privilege
//! an ordinary user lacks, a junction does not). One verdict differs, on
//! the safe side: a link inside the root that stays inside is refused here
//! too.

#![cfg(windows)]

use bytes::Bytes;
use norte_proto::{ConflictKind, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("valid segment")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

/// Provider rooted in a tempdir, with `dest/` inside and a sibling `outside/`.
fn scenario() -> (LocalProvider, VPath, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let inside = base.join("dest");
    let outside = base.join("outside");
    std::fs::create_dir(&inside).expect("dest");
    std::fs::create_dir(&outside).expect("outside");
    let p = LocalProvider::rooted(base).with_guard(Box::new(dir));
    let root_path = child(&LocalProvider::root(), b"dest");
    (p, root_path, inside, outside)
}

fn junction(link: &std::path::Path, target: &std::path::Path) {
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .status()
        .expect("cmd")
        .success();
    assert!(ok, "mklink /J");
}

async fn write_to(
    root: &dyn norte_vfs::ConfinedRoot,
    rel: &[Segment],
    bytes: &[u8],
) -> Result<(), Error> {
    let mut sink = root.write(rel).await?;
    sink.write(Bytes::copy_from_slice(bytes)).await?;
    sink.commit().await
}

fn escapes(e: &Error) -> bool {
    matches!(
        e,
        Error::Conflict {
            conflict: ConflictKind::EscapesRoot
        }
    )
}

/// #164 on Windows: the intermediate component is a junction pointing
/// outside, and neither the write nor the mkdir lands there.
#[tokio::test]
async fn an_intermediate_junction_does_not_redirect_writes_outside() {
    let (p, root_path, inside, outside) = scenario();
    junction(&inside.join("sub"), &outside);
    let root = p.open_root(&root_path).await.expect("confined root");

    let err = write_to(root.as_ref(), &[seg(b"sub"), seg(b"loot.txt")], b"x")
        .await
        .expect_err("refused");
    assert!(escapes(&err), "answered {err:?}");
    let err = root
        .mkdir(&[seg(b"sub"), seg(b"new")])
        .await
        .expect_err("refused");
    assert!(escapes(&err), "answered {err:?}");
    let err = root
        .remove(&[seg(b"sub"), seg(b"victim")])
        .await
        .expect_err("refused");
    assert!(escapes(&err), "answered {err:?}");

    assert!(!outside.join("loot.txt").exists() && !outside.join("new").exists());
}

/// ADR 0160: unlike unix, a link that stays inside is not crossed either.
#[tokio::test]
async fn a_junction_that_stays_inside_is_refused_too() {
    let (p, root_path, inside, _outside) = scenario();
    std::fs::create_dir(inside.join("real")).expect("real");
    junction(&inside.join("sub"), &inside.join("real"));
    let root = p.open_root(&root_path).await.expect("confined root");

    let err = write_to(root.as_ref(), &[seg(b"sub"), seg(b"x")], b"x")
        .await
        .expect_err("refused");
    assert!(escapes(&err), "answered {err:?}");
    assert!(!inside.join("real").join("x").exists());
}

/// The ordinary case: nested mkdir, write, stat, identity.
#[tokio::test]
async fn a_normal_nested_write_works() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    root.mkdir(&[seg(b"a")]).await.expect("mkdir");
    root.mkdir(&[seg(b"a"), seg(b"b")]).await.expect("mkdir");
    write_to(
        root.as_ref(),
        &[seg(b"a"), seg(b"b"), seg(b"f.txt")],
        b"hello",
    )
    .await
    .expect("write");

    assert_eq!(
        std::fs::read(inside.join("a/b/f.txt")).expect("read"),
        b"hello"
    );
    let st = root
        .stat(&[seg(b"a"), seg(b"b"), seg(b"f.txt")])
        .await
        .expect("stat");
    assert_eq!(st.size, Some(5));
    let by_root = root
        .node_id(&[seg(b"a"), seg(b"b"), seg(b"f.txt")])
        .await
        .expect("id");
    let by_path = p
        .node_id(
            &child(&child(&child(&root_path, b"a"), b"b"), b"f.txt"),
            norte_vfs::FollowLinks::No,
        )
        .await
        .expect("id");
    assert!(by_root.is_some() && by_root == by_path);
    assert!(root.root_id().await.expect("root id").is_some());
    let err = root.mkdir(&[seg(b"a")]).await.expect_err("exists");
    assert!(matches!(
        err,
        Error::Conflict {
            conflict: ConflictKind::Exists
        }
    ));
}

/// The publish goes through the directory HANDLE: a junction swapped in
/// for that directory between write and commit does not divert it.
///
/// Measured: Windows refuses to rename a directory while our staging is
/// open inside it (the staging does not share delete), so the swap itself
/// fails — a stronger answer than unix can give. Both branches are pinned,
/// in case a filesystem ever allows it.
#[tokio::test]
async fn publication_is_not_diverted_by_a_junction_slipped_in_midway() {
    let (p, root_path, inside, outside) = scenario();
    std::fs::create_dir(inside.join("sub")).expect("sub");
    let root = p.open_root(&root_path).await.expect("confined root");
    let mut sink = root
        .write(&[seg(b"sub"), seg(b"f.txt")])
        .await
        .expect("open");
    sink.write(Bytes::from_static(b"data"))
        .await
        .expect("write");

    let swapped = std::fs::rename(inside.join("sub"), inside.join("moved")).is_ok();
    if swapped {
        junction(&inside.join("sub"), &outside);
    }
    sink.commit().await.expect("commit");

    assert!(!outside.join("f.txt").exists(), "not outside");
    let landed = if swapped { "moved" } else { "sub" };
    assert_eq!(
        std::fs::read(inside.join(landed).join("f.txt")).expect("read"),
        b"data"
    );
}

/// Abort, a dropped sink and an occupied destination leave nothing behind.
#[tokio::test]
async fn no_partial_is_left_and_an_occupied_name_is_refused_early() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");

    let mut sink = root.write(&[seg(b"a.txt")]).await.expect("open");
    sink.write(Bytes::from_static(b"x")).await.expect("write");
    sink.abort().await.expect("abort");
    let sink = root.write(&[seg(b"b.txt")]).await.expect("open");
    drop(sink);
    assert_eq!(std::fs::read_dir(&inside).expect("ls").count(), 0, "empty");

    std::fs::write(inside.join("taken"), b"old").expect("write");
    let err = root
        .write(&[seg(b"taken")])
        .await
        .err()
        .expect("refused at open");
    assert!(matches!(
        err,
        Error::Conflict {
            conflict: ConflictKind::Exists
        }
    ));
    assert_eq!(std::fs::read(inside.join("taken")).expect("old"), b"old");
}

/// remove: a file goes, a directory does not, a junction goes as the link.
/// rmdir: the empty goes, the full does not.
#[tokio::test]
async fn confined_removal() {
    let (p, root_path, inside, outside) = scenario();
    std::fs::write(inside.join("f"), b"x").expect("f");
    std::fs::create_dir(inside.join("d")).expect("d");
    std::fs::write(inside.join("d").join("x"), b"x").expect("d/x");
    std::fs::create_dir(inside.join("empty")).expect("empty");
    std::fs::write(outside.join("kept"), b"k").expect("kept");
    junction(&inside.join("j"), &outside);
    let root = p.open_root(&root_path).await.expect("confined root");

    root.remove(&[seg(b"f")]).await.expect("file");
    assert!(!inside.join("f").exists());
    let err = root.remove(&[seg(b"d")]).await.expect_err("a dir");
    assert!(matches!(
        err,
        Error::Conflict {
            conflict: ConflictKind::TypeMismatch
        }
    ));
    root.remove(&[seg(b"j")]).await.expect("the link");
    assert!(std::fs::symlink_metadata(inside.join("j")).is_err());
    assert_eq!(std::fs::read(outside.join("kept")).expect("kept"), b"k");

    let err = root.rmdir(&[seg(b"d")]).await.expect_err("full");
    assert!(inside.join("d").join("x").exists(), "{err:?}");
    root.rmdir(&[seg(b"empty")]).await.expect("empty");
    assert!(!inside.join("empty").exists());
}

/// A stable partial is resumed after its bytes, digested by its prefix,
/// and kept by `keep`.
#[tokio::test]
async fn a_confined_root_resumes_its_own_partial() {
    let (p, root_path, inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    assert!(root.resumes());

    let (mut sink, already) = root.open_resumable(&[seg(b"big")]).await.expect("open");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hello "))
        .await
        .expect("write");
    sink.keep().await.expect("keep");

    let digest = root
        .partial_digest(&[seg(b"big")], 6)
        .await
        .expect("digest");
    assert!(digest.is_some());
    assert_eq!(
        root.partial_digest(&[seg(b"big")], 60)
            .await
            .expect("digest"),
        None,
        "shorter than asked"
    );

    let (mut sink, already) = root.open_resumable(&[seg(b"big")]).await.expect("reopen");
    assert_eq!(already, 6);
    sink.write(Bytes::from_static(b"world"))
        .await
        .expect("write");
    sink.commit().await.expect("commit");
    assert_eq!(
        std::fs::read(inside.join("big")).expect("read"),
        b"hello world"
    );
}

/// A stable staging name planted as a junction is not resumed through.
#[tokio::test]
async fn a_planted_staging_is_not_resumed() {
    let (p, root_path, inside, outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    // Learn the stable name: keep an empty partial, see what appeared.
    let (sink, _) = root.open_resumable(&[seg(b"t")]).await.expect("open");
    sink.keep().await.expect("keep");
    let name = std::fs::read_dir(&inside)
        .expect("ls")
        .next()
        .expect("the partial")
        .expect("entry")
        .file_name();
    std::fs::remove_file(inside.join(&name)).expect("rm");
    junction(&inside.join(&name), &outside);

    let err = root
        .open_resumable(&[seg(b"t")])
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, Error::Conflict { .. }), "answered {err:?}");
    assert_eq!(std::fs::read_dir(&outside).expect("ls").count(), 0);
}

/// Windows cannot create links here: `Unsupported`, like the provider.
#[tokio::test]
async fn symlinks_are_unsupported() {
    let (p, root_path, _inside, _outside) = scenario();
    let root = p.open_root(&root_path).await.expect("confined root");
    let err = root
        .symlink(&[seg(b"l")], b"target", norte_vfs::SymlinkKind::Unknown)
        .await
        .expect_err("unsupported");
    assert!(matches!(err, Error::Unsupported));
}
