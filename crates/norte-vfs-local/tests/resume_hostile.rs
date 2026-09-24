//! The STABLE staging of the BY-PATH route is a PREDICTABLE name (#298).
//!
//! `.norte-partial.` plus the final name's sha256-128: anyone who knows
//! where we're about to copy to can compute it. So whatever's on the
//! other side of that name may have been put there by someone else, and
//! resuming over it publishes under the legitimate name a foreign inode —
//! with its content, its owner and its permissions — or appends our bytes
//! to a victim's file.
//!
//! It's the same hole #297 closed for the CONFINED path
//! (`tests/confined.rs`), and here it had been open since ADR 0012 with
//! fewer defenses: the open was `append(true).create(true)`, which
//! follows links, checks neither the type, nor `st_nlink`, nor the owner,
//! and creates with `0o666`.
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;

use bytes::Bytes;
use norte_proto::{Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("valid segment"))
}

/// The staging's name, exactly as computed by anyone who knows the
/// destination name.
fn staging_name(final_name: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let d = Sha256::digest(final_name);
    let mut hex = String::new();
    for b in &d[..16] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    format!(".norte-partial.{hex}")
}

/// A REGULAR FILE planted with the staging's name isn't resumed.
///
/// It's given a hardlink so "this inode has another name" is observable
/// without depending on the uid: the test runs as the same user who
/// planted the file, so the owner distinguishes nothing here.
#[tokio::test]
async fn a_staging_planted_by_someone_else_is_not_resumed_by_path() {
    let (p, root, base) = provider();
    let planted = base.join(staging_name(b"big.bin"));
    std::fs::write(&planted, b"FOREIGN CONTENT").expect("planted");
    std::fs::hard_link(&planted, base.join("mine.txt")).expect("hardlink");

    let Err(err) = p.open_resumable(&child(&root, b"big.bin")).await else {
        panic!("resumed over a file that isn't ours");
    };
    assert!(
        matches!(err, Error::Conflict { .. }),
        "it has to be a conflict, not an I/O error: {err:?}"
    );
    assert_eq!(
        std::fs::read(&planted).expect("still there"),
        b"FOREIGN CONTENT",
        "and nothing got appended to it"
    );
}

/// A SYMLINK with the staging's name doesn't work either: following it
/// appends our bytes to the victim's file and then `commit` publishes it
/// under the legitimate name.
#[tokio::test]
async fn a_symlink_with_the_stagings_name_is_not_followed() {
    let (p, root, base) = provider();
    let victim = base.join("victim.txt");
    std::fs::write(&victim, b"FROM THE VICTIM").expect("victim");
    std::os::unix::fs::symlink(&victim, base.join(staging_name(b"big.bin"))).expect("symlink");

    let Err(err) = p.open_resumable(&child(&root, b"big.bin")).await else {
        panic!("followed a link to someone else's file");
    };
    assert!(
        !matches!(err, Error::NotFound),
        "the link exists: the error has to talk about it, not about something missing: {err:?}"
    );
    assert_eq!(
        std::fs::read(&victim).expect("still there"),
        b"FROM THE VICTIM",
        "and the victim untouched"
    );
}

/// A FIFO with the staging's name doesn't hang the open: without
/// `O_NONBLOCK` the `open` waits for a reader FOREVER inside the blocking
/// pool, and the cancellation token can't interrupt an in-progress `open`.
#[tokio::test]
async fn a_fifo_with_the_stagings_name_does_not_hang_the_open_by_path() {
    let (p, root, base) = provider();
    let fifo = base.join(staging_name(b"big.bin"));
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
    // SAFETY: `c` is a live, NUL-terminated CString; `mkfifo` requires no
    // privilege and only writes to the filesystem.
    let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o666) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

    let r = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        p.open_resumable(&child(&root, b"big.bin")),
    )
    .await
    .expect("the open has to return, not hang");
    assert!(r.is_err(), "a FIFO is not a staging");
}

/// The staging is created `0o600` and not `0o666`: for as long as it
/// lasts it's ours and nobody else's.
#[tokio::test]
async fn the_staging_by_path_is_created_only_for_us() {
    let (p, root, base) = provider();
    let (mut sink, _) = p
        .open_resumable(&child(&root, b"big.bin"))
        .await
        .expect("opens");
    sink.write(Bytes::from_static(b"abc")).await.expect("half");
    sink.keep().await.expect("keeps");

    let mode = std::fs::metadata(base.join(staging_name(b"big.bin")))
        .expect("staging")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "someone else's partial is neither written nor read"
    );
}

/// And the normal path still works: a staging we created gets resumed.
/// The defense can't cost the operation it exists to protect.
#[tokio::test]
async fn a_staging_of_our_own_is_resumed_by_path() {
    let (p, root, _base) = provider();
    let dest = child(&root, b"big.bin");
    let (mut sink, already) = p.open_resumable(&dest).await.expect("opens");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"abc")).await.expect("half");
    sink.keep().await.expect("keeps");

    let (_sink, already) = p.open_resumable(&dest).await.expect("reopens its own");
    assert_eq!(already, 3, "ours passes the check and gets continued");
}

/// Verifying the prefix of a file that is NOT the one about to be
/// continued verifies nothing: a foreign partial's digest isn't returned,
/// and the engine degrades to `Length` instead of trusting a prefix that
/// isn't its own.
#[tokio::test]
async fn the_digest_of_a_foreign_partial_is_not_returned() {
    let (p, root, base) = provider();
    let planted = base.join(staging_name(b"big.bin"));
    std::fs::write(&planted, b"FOREIGN CONTENT").expect("planted");
    std::fs::hard_link(&planted, base.join("mine.txt")).expect("hardlink");

    let d = p
        .partial_digest(&child(&root, b"big.bin"), 3)
        .await
        .expect("not an I/O error");
    assert!(d.is_none(), "not our partial: there's no digest to give");
}
