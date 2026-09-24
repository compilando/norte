//! `provider_contract!` over an IN-PROCESS SFTP server (ADR 0013): the SAME
//! suite that `MemProvider` and `LocalProvider` pass, now against a REAL
//! REMOTE provider, with the hostile-names corpus. Runs in normal CI,
//! without Docker (the real openssh is a separate nightly job).
//!
//! Linux-only: the in-process server maps sftp ops onto the HOST's FS, so
//! its fidelity requires a POSIX FS (case-sensitive, byte-preserving).
//! macOS (APFS case-insensitive, rejects non-UTF8 names) and Windows (NTFS
//! case-insensitive, no POSIX symlinks) CANNOT back the faithful harness —
//! they would give a non-representative "server". The provider is
//! OS-agnostic (pure Rust, no `cfg`), so the Linux run is authoritative and
//! the nightly openssh validates a production POSIX server.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_sftp::SftpProvider;

/// Builds a fresh sftp provider over a tempdir + in-process server. The
/// block is async, so it is wrapped in its own runtime (the
/// `provider_contract!` macro evaluates `factory` in a `#[tokio::test]`
/// test).
async fn fresh() -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    // The tempdir must live as long as the provider (ephemeral tests; the OS
    // cleans up /tmp).
    std::mem::forget(dir);
    // The server maps `/` to the tempdir, so the client's REMOTE base is
    // `/` (paths are composed /segment, the server rebases them).
    SftpProvider::new(session, "/")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod sftp_inproc,
    factory: fresh().await,
    root: SftpProvider::root(Authority::new("test:22").expect("valid authority")),
    hostile_names: hostile_names(),
}

/// The same provider with the logical trash TURNED ON (ADR 0019).
async fn fresh_with_trash() -> SftpProvider {
    fresh().await.with_logical_trash(true)
}

// The whole suite, again, with the logical trash on (#168).
//
// Same reason as in `norte-vfs-object`: it is the only configuration where
// the contract branch that says "the destination exists and is restored"
// runs, i.e. the one that checks that `trash()` names what it buries, that
// `reversal_ref` is `Some` and that `restore_from` returns the exact node —
// bytes and name, non-UTF8 names included.
//
// This provider's overrides were believed correct because they had been
// READ. Object's were in that same state when the bug #168 documents was
// written.
norte_vfs::provider_contract! {
    mod sftp_inproc_papelera,
    factory: fresh_with_trash().await,
    root: SftpProvider::root(Authority::new("test:22").expect("valid authority")),
    hostile_names: hostile_names(),
}

// ---------- posix attrs (#108 block 2) ----------

#[tokio::test]
async fn attrs_posix_from_file_attributes() {
    use futures::StreamExt;
    use norte_proto::{AttrValue, Segment};
    use norte_vfs::{AttrRequest, ListOptions, Provider};

    let p = fresh().await;
    let root = SftpProvider::root(Authority::new("test:22").expect("valid authority"));
    let f = root.join(Segment::new(b"f.txt".to_vec()).expect("valid segment"));
    {
        let mut sink = p.write(&f).await.expect("write opens");
        norte_vfs::ByteSink::write(&mut *sink, bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk goes in");
        sink.commit().await.expect("commit publishes");
    }
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["posix.mode", "posix.uid", "posix.gid"].map(str::to_owned)),
    };
    let e = p.stat_with(&f, &opt).await.expect("stat_with");
    // The in-proc server serves the host's FS: mode is ALWAYS present.
    assert!(
        matches!(e.attrs.get("posix.mode"), Some(AttrValue::Uint(_))),
        "posix.mode present and Uint: {:?}",
        e.attrs
    );
    for id in ["posix.uid", "posix.gid"] {
        if let Some(v) = e.attrs.get(id) {
            assert!(matches!(v, AttrValue::Uint(_)), "{id} must be Uint");
        }
    }

    // list_with carries the same thing per entry.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    let le = s.next().await.expect("one entry").expect("ok");
    assert!(le.attrs.contains_key("posix.mode"));

    // Without requesting → nothing.
    assert!(p.stat(&f).await.expect("stat").attrs.is_empty());
}
