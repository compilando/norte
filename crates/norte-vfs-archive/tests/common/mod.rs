//! Shared utilities: seeding a container in a `MemProvider` and wrapping
//! it in `ArchiveProvider` (pure composition, no host FS — the first
//! provider suite that's 100% portable, ADR 0018).
//!
//! Every test binary compiles this whole module; not all of them use
//! every helper.
#![allow(dead_code)]

use std::sync::Arc;

use bytes::Bytes;
use norte_proto::{Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

/// Writes `bytes` as `mem:///<name>` on a fresh `MemProvider`.
pub async fn seed_container(name: &[u8], bytes: &[u8]) -> (Arc<MemProvider>, VPath) {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(name.to_vec()).expect("seg"));
    write_file(mem.as_ref(), &path, bytes).await;
    (mem, path)
}

/// Writes (or rewrites) a file on the Mem via the contractual path.
pub async fn write_file(mem: &MemProvider, path: &VPath, bytes: &[u8]) {
    if mem.stat(path).await.is_ok() {
        mem.remove(path).await.expect("previous remove");
    }
    let mut sink = mem.write(path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// A tar provider over a freshly seeded container + the inner root.
pub async fn tar_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    tar_provider_with_limits(bytes, Limits::default()).await
}

/// A zip provider over a freshly seeded container + the inner root.
pub async fn zip_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    zip_provider_with_limits(bytes, Limits::default()).await
}

/// Like [`zip_provider`] with custom limits (bomb tests).
pub async fn zip_provider_with_limits(bytes: &[u8], limits: Limits) -> (ArchiveProvider, VPath) {
    let (mem, path) = seed_container(b"fixture.zip", bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(mem, Format::Zip, "zip+mem", limits);
    (provider, root)
}

/// Like [`tar_provider`] with custom limits (bomb tests).
pub async fn tar_provider_with_limits(bytes: &[u8], limits: Limits) -> (ArchiveProvider, VPath) {
    let (mem, path) = seed_container(b"fixture.tar", bytes).await;
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(mem, Format::Tar, "tar+mem", limits);
    (provider, root)
}

/// Gzips already-built bytes (e.g. a `TarSmith` tar) into a single gzip
/// member (#55, ADR 0028).
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).expect("write gz");
    enc.finish().expect("finish gz")
}

/// A tar.gz provider over a freshly seeded `gzip(tar_bytes)` container +
/// the inner root (#55, ADR 0028).
pub async fn targz_provider(gz_bytes: &[u8]) -> (ArchiveProvider, VPath) {
    targz_provider_with_limits(gz_bytes, Limits::default()).await
}

/// Like [`targz_provider`] with custom limits (bomb/cancellation tests).
pub async fn targz_provider_with_limits(
    gz_bytes: &[u8],
    limits: Limits,
) -> (ArchiveProvider, VPath) {
    let (mem, path) = seed_container(b"fixture.tar.gz", gz_bytes).await;
    let root = VPath::archive_compose("tar+gz", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(mem, Format::TarGz, "tar+gz+mem", limits);
    (provider, root)
}
