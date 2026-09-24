//! Containment of a HOSTILE SFTP server (ADR 0013, threat model §14): a
//! server that injects trap names (`../../`) and symlinks outside the base
//! must not be able to make the provider escape the root or corrupt.
//!
//! Linux-only (same as `contract.rs`): the in-process server is backed by
//! the HOST's FS and is only faithful on POSIX (case-sensitive,
//! byte-preserving); a raw non-UTF8 fixture (`caf\xE9`) cannot even be
//! planted on APFS (EILSEQ). The provider is OS-agnostic; the nightly
//! openssh covers real POSIX.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, ConflictKind, EntryKind, Error, VPath};
use norte_vfs::{FollowLinks, Provider};
use norte_vfs_sftp::SftpProvider;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("sftp://test:22{p}")).expect("valid wire")
}

async fn provider(base: &std::path::Path, mode: common::Mode) -> SftpProvider {
    let session = common::connect(base, mode).await;
    SftpProvider::new(session, "/")
}

/// A server that injects `../../escape` into every `readdir`: the provider
/// REJECTS the entry with `/` (never reconstructs it as a child) and cuts
/// the listing short — the rest of the legitimate entries are not served
/// blindly.
#[tokio::test]
async fn readdir_with_trap_name_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("legit.txt"), b"ok").unwrap();
    let p = provider(dir.path(), common::Mode::Hostile).await;

    let mut stream = p.list(&vp("/")).await.expect("list opens");
    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                // No served entry contains `/` or `..` in its name.
                let name = entry.path.file_name().expect("has a name");
                assert!(
                    !name.as_bytes().contains(&b'/'),
                    "an entry with `/` would escape the base"
                );
                assert_ne!(name.as_bytes(), b"..");
            }
            Err(Error::InvalidPath) => {
                // The trap name `../../escape` is rejected: containment OK.
                saw_error = true;
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert!(
        saw_error,
        "the hostile server injected `../../escape` and it should have been rejected"
    );
}

/// The provider ALWAYS builds paths from validated segments: even if the
/// server lies in a listing, a later `stat` uses the path the client built,
/// never an echoed one — there is no escape.
#[tokio::test]
async fn stat_uses_its_own_path_not_the_echoed_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("f.txt"), b"data").unwrap();
    let p = provider(dir.path(), common::Mode::Hostile).await;

    // Stat of a legitimate child works (the client builds the path).
    let e = p
        .stat(&vp("/f.txt"))
        .await
        .expect("stat of the legit child");
    assert_eq!(e.kind, EntryKind::File);
    // A VPath never admits `..` as a segment, so the client cannot request
    // `sftp://test:22/../etc` — there is no way to build it.
    assert!(VPath::parse("sftp://test:22/../etc").is_err());
}

/// A trap symlink pointing OUTSIDE the base (`/etc/passwd`) is seen as a
/// symlink (lstat, never followed); `read_link` gives the raw target bytes
/// without resolving it, and since `node_id` is `None`, the engine's
/// `Follow` cannot walk it.
#[tokio::test]
async fn trap_symlink_is_not_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink("/etc/passwd", dir.path().join("trap")).unwrap();
    let p = provider(dir.path(), common::Mode::Honest).await;

    let e = p.stat(&vp("/trap")).await.expect("stat of the symlink");
    assert_eq!(e.kind, EntryKind::Symlink, "seen as a LINK, not followed");
    // read_link gives the RAW target, unresolved.
    let target = p.read_link(&vp("/trap")).await.expect("read_link");
    assert_eq!(target, b"/etc/passwd");
    // node_id is None → the engine cannot follow dir-symlinks (containment).
    assert_eq!(
        p.node_id(&vp("/trap"), FollowLinks::No).await.unwrap(),
        None
    );
    // And the provider NEVER read /etc/passwd's contents.
}

/// The provider never leaves its base: paths are composed under it and the
/// test server rebases everything inside the tempdir. Writing creates the
/// file INSIDE it, not on the test runner's FS.
#[tokio::test]
async fn write_stays_inside_the_base() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    let mut sink = p.write(&vp("/new.bin")).await.expect("write opens");
    sink.write(Bytes::from_static(b"content")).await.unwrap();
    sink.commit().await.expect("commit");
    // The file appears INSIDE the tempdir, nowhere else.
    assert_eq!(
        std::fs::read(dir.path().join("new.bin")).unwrap(),
        b"content"
    );
}

/// A non-UTF8 name is CLEANLY rejected with `InvalidPath` (russh-sftp's
/// limitation = String; ADR 0013 D2), never lossy.
#[tokio::test]
async fn non_utf8_name_is_cleanly_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;
    let seg = norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap();
    let hostile = SftpProvider::root(Authority::new("test:22").unwrap()).join(seg);
    // Any operation with a non-UTF8 name rejects cleanly.
    assert_eq!(p.stat(&hostile).await.unwrap_err(), Error::InvalidPath);
    assert!(p.write(&hostile).await.is_err());
}
/// Resume over sftp (ADR 0012): `keep` preserves the `.norte-partial`, a
/// second open resumes from the offset and commit concatenates. (The sink's
/// clean cancellation is covered by the contract macro's
/// `contract_abort_leaves_no_trace`, which runs against this same
/// provider.)
#[tokio::test]
async fn resume_over_sftp_resumes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    // First leg: open_resumable, write "hello", keep (preserve).
    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hello")).await.unwrap();
    sink.keep().await.expect("keep");
    // The final destination does not exist yet.
    assert_eq!(p.stat(&vp("/big.bin")).await.unwrap_err(), Error::NotFound);

    // Second leg: resumes from 4 bytes.
    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 4, "resumes after what was preserved");
    sink.write(Bytes::from_static(b"world")).await.unwrap();
    sink.commit().await.expect("commit");
    // The content is the concatenation.
    let mut stream = p.read(&vp("/big.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = stream.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"helloworld");
}

/// H1 (write): the ephemeral staging has a PREDICTABLE name
/// (`.norte-partial.eph.0`). A hostile server/co-tenant pre-plants it as a
/// symlink to a file OUTSIDE the base; `write()` opens with `EXCLUDE`
/// (atomic create-new) → FAILS without following the symlink. The victim
/// stays intact (write containment — ADR 0013, threat model §14).
#[tokio::test]
async fn pre_planted_staging_symlink_is_not_followed_on_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("victim tempdir");
    let victim = outside.path().join("victim");
    std::fs::write(&victim, b"intact").unwrap();
    // The hostile party pre-plants the predictable staging as a symlink to
    // the victim.
    std::os::unix::fs::symlink(&victim, dir.path().join(".norte-partial.eph.0")).unwrap();

    let p = provider(dir.path(), common::Mode::Honest).await;
    // The open with EXCLUDE fails against the existing path → write() gives
    // an error.
    assert!(
        p.write(&vp("/dest")).await.is_err(),
        "opening a pre-planted staging (symlink) must fail, not follow it"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"intact",
        "never wrote through the symlink outside the base"
    );
}

/// H1 (resume): `open_resumable` cannot use `EXCLUDE` (it legitimately
/// reopens a partial), so it does `lstat` and REJECTS if the existing
/// staging is a symlink. Otherwise, resuming in APPEND would write into the
/// target outside the base.
#[tokio::test]
async fn pre_planted_staging_symlink_is_not_resumed() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path(), common::Mode::Honest).await;

    // First open: creates the staging (regular file), writes, keep.
    let (mut sink, _) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    sink.write(Bytes::from_static(b"hello")).await.unwrap();
    sink.keep().await.expect("keep");

    // Discovers the staging THROUGH the provider: `list` is an in-order
    // round-trip that guarantees sink1's writes were already processed
    // server-side before touching the tempdir from outside (otherwise, a
    // late WRITE from sink1 would follow the symlink we planted — an
    // artifact of the test, not the provider). The staging's name is
    // deterministic but internal; we take it from the listing instead of
    // hardcoding it.
    let mut stream = p.list(&vp("/")).await.expect("list");
    let mut staging_name = None;
    while let Some(item) = stream.next().await {
        let entry = item.expect("valid entry");
        let name = entry
            .path
            .file_name()
            .expect("has a name")
            .as_bytes()
            .to_vec();
        if name.starts_with(b".norte-partial.") {
            staging_name = Some(name);
        }
    }
    let staging_name = staging_name.expect("the preserved staging is listed");
    let staging = dir.path().join(std::ffi::OsStr::from_bytes(&staging_name));

    // The hostile party replaces the staging with a symlink outside the base.
    let outside = tempfile::tempdir().expect("victim tempdir");
    let victim = outside.path().join("victim");
    std::fs::write(&victim, b"intact").unwrap();
    std::fs::remove_file(&staging).unwrap();
    std::os::unix::fs::symlink(&victim, &staging).unwrap();

    // Second open: the staging is now a symlink → rejected.
    let rejection = p.open_resumable(&vp("/big.bin")).await;
    assert!(
        matches!(
            rejection,
            Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch
            })
        ),
        "resuming over a staging that is a symlink must be rejected"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"intact",
        "never appended through the symlink outside the base"
    );
}

/// Finding A (encoding): russh-sftp decodes the server's names with
/// `from_utf8_lossy`, so a non-UTF8 name arrives substituted by U+FFFD and
/// the original bytes are lost BELOW the boundary. The provider CLEANLY
/// REJECTS it (`InvalidPath`) instead of emitting an `Entry` with corrupt
/// bytes that would collide or point at a nonexistent file (rule 1 / ADR
/// 0013 D2).
#[tokio::test]
async fn readdir_non_utf8_name_is_not_corrupted() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().expect("tempdir");
    // `café` in Latin-1: raw byte 0xE9 — impossible to create via the provider.
    let raw = std::ffi::OsStr::from_bytes(b"caf\xE9.txt");
    std::fs::write(dir.path().join(raw), b"x").unwrap();
    let p = provider(dir.path(), common::Mode::Honest).await;

    let mut stream = p.list(&vp("/")).await.expect("list opens");
    let mut rejected = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                let name = entry.path.file_name().expect("has a name");
                assert!(
                    !name.as_bytes().windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
                    "a non-UTF8 name was silently emitted corrupt (U+FFFD)"
                );
            }
            Err(Error::InvalidPath) => rejected = true,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert!(
        rejected,
        "the non-UTF8 name should have been cleanly rejected (InvalidPath), never corrupted"
    );
}
