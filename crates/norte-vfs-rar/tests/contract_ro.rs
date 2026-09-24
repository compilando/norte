//! `readonly_provider_contract!` over a `.rar` forged with [`RarSmith`] and
//! read by whichever delegate is installed.
//!
//! This suite NEEDS `7z` or `unrar` on the machine: without either there is
//! no way to read a RAR and nothing to check against. It fails saying so
//! instead of passing green having tested nothing.

use std::sync::OnceLock;

use norte_proto::{Scheme, Segment, VPath};
use norte_testkit::RarSmith;
use norte_vfs_rar::{Delegate, RarLimits, RarProvider};

/// Corpus names that a LINE-BASED listing cannot carry back.
///
/// This is not a weakness of the index: the delegate's output is line-based
/// text and a name with `\n` (or a `\r` accompanied by a `\n`) cannot be
/// reconstructed without guessing where it ends. The provider's rule is to
/// skip them and count them, so they are excluded here from the contract —
/// which demands a byte-exact round-trip — and
/// `listing::tests::a_name_with_a_line_break_is_skipped_and_counted` pins the
/// boundary.
///
/// `archive_marker_literal` (`!`) is excluded for the same reason as in the
/// zip/tar suite: it is ADR 0018's marker, unaddressable by design.
fn no_representable(id: &str, bytes: &[u8]) -> bool {
    id == "archive_marker_literal" || bytes.contains(&b'\n') || bytes.contains(&b'\r')
}

/// Hostile names the fixture's `.rar` DOES carry.
fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .filter(|n| !no_representable(&n.id, &n.bytes))
        .map(|n| n.bytes)
        .collect()
}

/// The canonical tree the RO macro requires, forged as RAR5.
fn canonical_rar() -> Vec<u8> {
    let mut smith = RarSmith::new()
        .dir(b"docs")
        .file(b"docs/hello.txt", b"hello norte\n")
        .dir(b"docs/sub")
        .file(b"docs/sub/nested.bin", b"\x00\x01\x02\xff")
        .file(b"empty.txt", b"")
        .dir(b"hostile");
    for name in hostile_names() {
        let mut full = b"hostile/".to_vec();
        full.extend_from_slice(&name);
        smith = smith.file(&full, &name);
    }
    smith.build()
}

/// The fixture's `.rar`, written ONCE per process: the delegate needs a real
/// path, so the temp file must outlive the tests.
fn fixture() -> &'static std::path::Path {
    static FIXTURE: OnceLock<(tempfile::TempDir, std::path::PathBuf)> = OnceLock::new();
    let (_dir, path) = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("contract.rar");
        std::fs::write(&path, canonical_rar()).expect("write the fixture");
        (dir, path)
    });
    path
}

fn fresh() -> RarProvider {
    let delegate = Delegate::discover().expect(
        "this suite needs `7z` or `unrar` installed: without a delegate there is no RAR to read",
    );
    RarProvider::new(fixture().to_path_buf(), delegate, RarLimits::default())
}

fn root() -> VPath {
    use std::os::unix::ffi::OsStrExt;
    let mut outer = VPath::root(Scheme::new("file").expect("scheme"), None);
    for comp in fixture().components().skip(1) {
        outer = outer.join(Segment::new(comp.as_os_str().as_bytes().to_vec()).expect("segment"));
    }
    VPath::archive_compose("rar", &outer, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod rar_ro,
    factory: fresh(),
    root: root(),
    hostile_names: hostile_names(),
}
