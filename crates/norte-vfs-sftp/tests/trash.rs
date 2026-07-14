//! Papelera lógica `.norte-trash/` de sftp (fase 9b, ADR 0019) contra el
//! servidor sftp in-process.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_sftp::SftpProvider;

/// Provider fresco sobre tempdir + servidor in-process, con la papelera
/// lógica en el estado pedido.
async fn fresh(logical_trash: bool) -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    std::mem::forget(dir);
    SftpProvider::new(session, "/").with_logical_trash(logical_trash)
}

/// La raíz remota del cliente (`/`), con authority de test.
fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false).await;
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true).await;
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}
