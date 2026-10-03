//! ADR 0165: a local copy is filled by the kernel when it can. What can be
//! checked from outside is what must not change because of it: the bytes,
//! an empty file, the progress, and that the copy still lands through the
//! staging (no `.norte-partial` left, the destination is a new file).

use std::sync::Arc;

use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("valid segment"))
}

#[tokio::test]
async fn a_local_copy_keeps_its_bytes_progress_and_staging() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir");
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(dir.path().join("src/big"), &big).expect("write");
    std::fs::write(dir.path().join("src/empty"), b"").expect("write");

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);
    let root = LocalProvider::root();

    let handle = engine
        .copy(&child(&root, b"src"), &child(&root, b"dst"))
        .await
        .expect("submit");
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(std::fs::read(dir.path().join("dst/big")).expect("big"), big);
    assert_eq!(
        std::fs::read(dir.path().join("dst/empty")).expect("empty"),
        b""
    );
    let last = rx.borrow().clone();
    assert_eq!(last.bytes_done, big.len() as u64);
    assert_eq!(last.bytes_total, Some(big.len() as u64));
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("dst"))
        .expect("dst")
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".norte-partial")
        })
        .collect();
    assert!(leftovers.is_empty(), "no staging left behind");
}
