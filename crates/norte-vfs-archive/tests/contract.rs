//! `readonly_provider_contract!` over tar AND zip (`TarSmith`/`ZipSmith` +
//! `MemProvider`): the full RO suite against the provider's real logic
//! (vpath split, index, read) with no host FS nor Docker.

mod common;

use norte_proto::VPath;
use norte_testkit::TarSmith;
use norte_vfs_archive::ArchiveProvider;

/// Names ADR 0018's ADDRESSING can't represent inside an archive, and
/// which therefore can't enter a contract that demands a byte-exact
/// round trip.
///
/// Today there's one: a component equal to `!`, the marker that
/// separates the container from the inside. `ArchiveIndex` rejects it on
/// purpose and with its reason written down
/// (`crates/norte-vfs-archive/src/index.rs`, "`!` component (ADR 0018
/// marker, unaddressable)"), and `VPath::archive_split` cuts at the first
/// segment matching it — so an entry like that, if admitted, would be
/// addressable as something ELSE. The design choice is to skip it, i.e.
/// fail closed, and
/// `index::tests::omits_traversal_absolutes_and_the_marker` pins it down
/// — so excluding it HERE doesn't hide it: the boundary still has a test
/// asserting it, and this comment says where.
///
/// The `archive_marker_literal` fixture uncovered this when it entered
/// the corpus (#169): the contract feeds the WHOLE corpus, so an
/// addressing boundary shows up here as a missing round trip. Excluding
/// it by `id` and not by bytes leaves it said which one it is and why,
/// instead of hiding it.
fn not_representable_in_an_archive(id: &str) -> bool {
    id == "archive_marker_literal"
}

/// Corpus names that fit in a ustar header (`TarSmith` doesn't forge a
/// GNU longname): ≤ 100 bytes. The long ones are covered by the zip suite
/// (phase 8e).
fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .filter(|n| !not_representable_in_an_archive(&n.id))
        .map(|n| n.bytes)
        .filter(|b| {
            // `hostile/` + name has to fit in the header's 100 bytes.
            b.len() <= 100 - "hostile/".len()
        })
        .collect()
}

/// The canonical tree the RO macro requires, forged as a tar.
fn canonical_tar() -> Vec<u8> {
    let mut smith = TarSmith::new()
        .dir(b"docs")
        .file(b"docs/hello.txt", b"hola norte\n")
        .file(b"docs/sub/nested.bin", b"\x00\x01\x02\xff")
        .file(b"vacio.txt", b"")
        .dir(b"hostile");
    for name in hostile_names() {
        let mut full = b"hostile/".to_vec();
        full.extend_from_slice(&name);
        smith = smith.file(&full, &name);
    }
    smith.build()
}

fn fresh() -> ArchiveProvider {
    // The macro evaluates `factory` inside tokio's async test; seeding
    // the Mem is pure async (no IO/timers) — the futures executor is
    // enough and doesn't step on tokio's runtime.
    futures::executor::block_on(async {
        let (provider, _) = common::tar_provider(&canonical_tar()).await;
        provider
    })
}

fn root() -> VPath {
    let path = norte_testkit::MemProvider::root()
        .join(norte_proto::Segment::new(b"fixture.tar".to_vec()).expect("seg"));
    VPath::archive_compose("tar", &path, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod tar_ro,
    factory: fresh(),
    root: root(),
    hostile_names: hostile_names(),
}

// ---------- tar+gz (#55, ADR 0028): same canonical tree, gzipped ----------

fn fresh_targz() -> ArchiveProvider {
    futures::executor::block_on(async {
        let gz = common::gzip(&canonical_tar());
        let (provider, _) = common::targz_provider(&gz).await;
        provider
    })
}

fn targz_root() -> VPath {
    let path = norte_testkit::MemProvider::root()
        .join(norte_proto::Segment::new(b"fixture.tar.gz".to_vec()).expect("seg"));
    VPath::archive_compose("tar+gz", &path, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod targz_ro,
    factory: fresh_targz(),
    root: targz_root(),
    hostile_names: hostile_names(),
}

// ---------- zip: the FULL hostile corpus (without the 100-byte filter) ----------

fn zip_hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .filter(|n| !not_representable_in_an_archive(&n.id))
        .map(|n| n.bytes)
        .collect()
}

fn canonical_zip() -> Vec<u8> {
    let mut smith = norte_testkit::ZipSmith::new()
        .dir(b"docs")
        .file(b"docs/hello.txt", b"hola norte\n")
        .file(b"docs/sub/nested.bin", b"\x00\x01\x02\xff")
        .file(b"vacio.txt", b"")
        .dir(b"hostile");
    for name in zip_hostile_names() {
        let mut full = b"hostile/".to_vec();
        full.extend_from_slice(&name);
        smith = smith.file(&full, &name);
    }
    smith.build()
}

fn fresh_zip() -> ArchiveProvider {
    futures::executor::block_on(async {
        let (provider, _) = common::zip_provider(&canonical_zip()).await;
        provider
    })
}

fn zip_root() -> VPath {
    let path = norte_testkit::MemProvider::root()
        .join(norte_proto::Segment::new(b"fixture.zip".to_vec()).expect("seg"));
    VPath::archive_compose("zip", &path, &[]).expect("compose")
}

norte_vfs::readonly_provider_contract! {
    mod zip_ro,
    factory: fresh_zip(),
    root: zip_root(),
    hostile_names: zip_hostile_names(),
}
