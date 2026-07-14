//! Papelera lógica `.norte-trash/` de object/S3 (fase 9c, ADR 0019) contra
//! el harness `services-fs` de opendal.
mod common;

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_object::ObjectProvider;

/// Provider fresco sobre un tempdir vía `services-fs`, con la papelera
/// lógica en el estado pedido.
fn fresh(logical_trash: bool) -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3").with_logical_trash(logical_trash)
}

/// La raíz del provider (`s3://norte-test/`).
fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false);
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true);
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}
