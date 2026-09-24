//! #52: `LocalProvider`'s listing is lazy (kind by `d_type`,
//! `size`/`mtime_ms` left `None`). This test checks the C1↔C2 COORDINATION:
//! without `hydrate_plan` (C1, in `ops::copy_tree`), `bytes_total` would stay
//! `Some(0)` because the walk's plan carries `size: None` for every leaf
//! (source: `LocalProvider::list`, C2). With hydration, the progress
//! reflects the real size BEFORE copying and the content arrives byte-exact.

use std::sync::Arc;

use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("valid segment"))
}

#[tokio::test]
async fn copy_dir_local_bytes_total_hydrated_from_lazy_listing() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir src");
    std::fs::write(dir.path().join("src").join("a"), b"abc").expect("3 bytes");
    std::fs::write(dir.path().join("src").join("b"), b"abcd").expect("4 bytes");

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);

    let root = LocalProvider::root();
    let src = child(&root, b"src");
    let dst = child(&root, b"dst");

    let handle = engine.copy(&src, &dst).await.expect("submit");
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    let last = rx.borrow().clone();
    assert_eq!(
        last.bytes_total,
        Some(7),
        "without hydrate_plan the lazy listing would leave bytes_total at Some(0)"
    );

    assert_eq!(
        std::fs::read(dir.path().join("dst").join("a")).expect("dst/a"),
        b"abc"
    );
    assert_eq!(
        std::fs::read(dir.path().join("dst").join("b")).expect("dst/b"),
        b"abcd"
    );
}
