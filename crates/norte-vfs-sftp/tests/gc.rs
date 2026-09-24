//! sftp's `gc_partials` (#11, ADR 0012) against the in-process server:
//! sweeps orphaned `.norte-partial.*` by their exact SHAPE (stable 32-hex /
//! ephemeral `eph.<seq>`), never real user files.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::{Authority, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_sftp::SftpProvider;

fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segment")
}

#[tokio::test]
async fn gc_partials_sweeps_by_exact_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    let p = SftpProvider::new(session, "/");

    // Orphans with the TWO shapes of sftp staging…
    std::fs::write(
        base.join(".norte-partial.0123456789abcdef0123456789abcdef"),
        b"x",
    )
    .expect("stable");
    std::fs::write(base.join(".norte-partial.eph.7"), b"x").expect("ephemeral");
    // …and USER files with the prefix but not the shape (H2).
    std::fs::write(base.join(".norte-partial.backup"), b"mine").expect("user 1");
    std::fs::write(base.join(".norte-partial.eph.no-num"), b"mine").expect("user 2");
    // Another provider's shape (local: eph with pid-seq) is not touched here
    // either.
    std::fs::write(base.join(".norte-partial.0123456789abcdef.42-1"), b"?").expect("other shape");

    let removed = p
        .gc_partials(&root(), std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 2, "only the two sftp shapes are swept");
    assert!(base.join(".norte-partial.backup").exists());
    assert!(base.join(".norte-partial.eph.no-num").exists());
    assert!(base.join(".norte-partial.0123456789abcdef.42-1").exists());
    assert!(
        !base
            .join(".norte-partial.0123456789abcdef0123456789abcdef")
            .exists()
    );
    assert!(!base.join(".norte-partial.eph.7").exists());
}

/// `older_than` respects mtime: a RECENT staging file is not swept (an
/// in-progress resume survives a gc with a lenient threshold).
#[tokio::test]
async fn gc_partials_respects_older_than() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    let p = SftpProvider::new(session, "/");

    std::fs::write(base.join(".norte-partial.eph.3"), b"x").expect("staging");
    let removed = p
        .gc_partials(&root(), std::time::Duration::from_hours(1))
        .await
        .expect("gc");
    assert_eq!(removed, 0, "just touched: it stays");
    assert!(base.join(".norte-partial.eph.3").exists());
    let _ = seg(b"anchor"); // (uses the helper; the tempdir's drop cleans up)
}
