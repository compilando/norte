//! Utilidades compartidas: sembrar un contenedor en un `MemProvider` y
//! envolverlo en el `ArchiveProvider` (composición pura, sin FS del host —
//! la primera suite de provider 100 % portable, ADR 0018).
//!
//! Cada binario de test compila este módulo entero; no todos usan todos
//! los helpers.
#![allow(dead_code)]

use std::sync::Arc;

use bytes::Bytes;
use norte_proto::{Segment, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

/// Escribe `bytes` como `mem:///<name>` en un `MemProvider` nuevo.
pub async fn seed_container(name: &[u8], bytes: &[u8]) -> (Arc<MemProvider>, VPath) {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(name.to_vec()).expect("seg"));
    write_file(mem.as_ref(), &path, bytes).await;
    (mem, path)
}

/// Escribe (o reescribe) un archivo en el Mem por el camino contractual.
pub async fn write_file(mem: &MemProvider, path: &VPath, bytes: &[u8]) {
    if mem.stat(path).await.is_ok() {
        mem.remove(path).await.expect("remove previo");
    }
    let mut sink = mem.write(path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Provider tar sobre un contenedor recién sembrado + la raíz interior.
pub async fn tar_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    tar_provider_with_limits(bytes, Limits::default()).await
}

/// Provider zip sobre un contenedor recién sembrado + la raíz interior.
pub async fn zip_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    zip_provider_with_limits(bytes, Limits::default()).await
}

/// Como [`zip_provider`] con límites propios (tests de bomba).
pub async fn zip_provider_with_limits(bytes: &[u8], limits: Limits) -> (ArchiveProvider, VPath) {
    let (mem, path) = seed_container(b"fixture.zip", bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(mem, Format::Zip, "zip+mem", limits);
    (provider, root)
}

/// Como [`tar_provider`] con límites propios (tests de bomba).
pub async fn tar_provider_with_limits(bytes: &[u8], limits: Limits) -> (ArchiveProvider, VPath) {
    let (mem, path) = seed_container(b"fixture.tar", bytes).await;
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(mem, Format::Tar, "tar+mem", limits);
    (provider, root)
}
