//! The mode of the file that gets PUBLISHED (#299).
//!
//! The stable staging is born `0o600` and can't be born any other way: its
//! name is predictable, so for as long as it lasts it has to be ours and
//! nobody else's (#297, #298). But publishing is a `rename`, which doesn't
//! touch the mode, so a RESUMED copy used to end up `0o600` while the same
//! uninterrupted copy ended up `0o644`. Same operation, two results — and
//! resumable has been a leaf's path since #219, i.e. the product's most
//! common one.
//!
//! The assertions are EQUALITY between the two paths and not against a
//! hand-written mode: the correct mode depends on whoever runs the test's
//! umask, and pinning `0o644` would turn a CI with a different umask red
//! for a reason that isn't the bug.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;

use bytes::Bytes;
use norte_proto::{Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("valid segment")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(seg(name))
}

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("exists")
        .permissions()
        .mode()
        & 0o777
}

/// **The bug.** A normal copy and a resumed copy publish the SAME file
/// with the same content; they have to publish it with the same mode.
#[tokio::test]
async fn a_resumed_copy_is_published_with_a_normal_copys_mode() {
    let (p, root, base) = provider();

    let normal = child(&root, b"normal.bin");
    let mut sink = p.write(&normal).await.expect("write");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let resumed = child(&root, b"resumed.bin");
    let (mut sink, already) = p.open_resumable(&resumed).await.expect("open_resumable");
    assert_eq!(already, 0, "there was no earlier partial");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        mode(&base.join("resumed.bin")),
        mode(&base.join("normal.bin")),
        "same operation, same mode: without this the resumed one stays at 0o600"
    );
}

/// A REAL resume — two sessions over the same staging — publishes the
/// same way. The case above opens the staging and publishes it in one go;
/// this one leaves it half-done, finds it again and finishes it, which is
/// what an interrupted copy does.
#[tokio::test]
async fn a_resume_in_two_stages_also_publishes_with_the_normal_mode() {
    let (p, root, base) = provider();

    let normal = child(&root, b"normal.bin");
    let mut sink = p.write(&normal).await.expect("write");
    sink.write(Bytes::from_static(b"onetwo"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let dest = child(&root, b"big.bin");
    let (mut sink, _) = p.open_resumable(&dest).await.expect("first stage");
    sink.write(Bytes::from_static(b"one")).await.expect("bytes");
    // `keep` preserves the staging for the next resume (ADR 0012).
    sink.keep().await.expect("keeps");

    let (mut sink, already) = p.open_resumable(&dest).await.expect("second stage");
    assert_eq!(already, 3, "finds the first stage's bytes again");
    sink.write(Bytes::from_static(b"two")).await.expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        std::fs::read(base.join("big.bin")).expect("read"),
        b"onetwo",
        "and the bytes are the two stages"
    );
    assert_eq!(mode(&base.join("big.bin")), mode(&base.join("normal.bin")),);
}

/// **For as long as it lasts, the staging stays OURS.** The fix can't
/// consist of creating it more open: its name is predictable, so a
/// `0o644` during the copy would let anyone read what's being copied —
/// and a half-done file, at that. The mode is restored AFTER publishing.
#[tokio::test]
async fn the_half_done_staging_is_not_relaxed() {
    let (p, root, base) = provider();
    let dest = child(&root, b"big.bin");

    let (mut sink, _) = p.open_resumable(&dest).await.expect("open_resumable");
    sink.write(Bytes::from_static(b"halfway"))
        .await
        .expect("bytes");
    sink.keep().await.expect("keeps");

    let staging = std::fs::read_dir(&base)
        .expect("list")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".norte-partial."))
        })
        .expect("the kept staging");

    assert_eq!(
        mode(&staging),
        0o600,
        "the partial is ours and nobody else's"
    );
}

/// The CONFINED path (#297) is the one a leaf has used since #219, so it's
/// the one the user really sees. Same promise.
#[tokio::test]
async fn the_confined_path_publishes_with_the_same_mode() {
    let (p, root, base) = provider();
    std::fs::create_dir(base.join("dest")).expect("dest");
    let dest_root = child(&root, b"dest");

    let croot = p.open_root(&dest_root).await.expect("confined root");
    let mut sink = croot.write(&[seg(b"normal.bin")]).await.expect("write");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    let (mut sink, _) = croot
        .open_resumable(&[seg(b"resumed.bin")])
        .await
        .expect("confined open_resumable");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("bytes");
    sink.commit().await.expect("commit");

    assert_eq!(
        mode(&base.join("dest/resumed.bin")),
        mode(&base.join("dest/normal.bin")),
    );
}
