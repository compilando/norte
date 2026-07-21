//! #52: el listado de `LocalProvider` es lazy (kind por `d_type`,
//! `size`/`mtime_ms` en `None`). Este test prueba la COORDINACIÓN C1↔C2:
//! sin `hydrate_plan` (C1, en `ops::copy_tree`), `bytes_total` quedaría en
//! `Some(0)` porque el plan del walk trae `size: None` para cada hoja
//! (fuente: `LocalProvider::list`, C2). Con la hidratación, el progreso
//! refleja el tamaño real ANTES de copiar y el contenido llega byte-exacto.

use std::sync::Arc;

use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento válido"))
}

#[tokio::test]
async fn copy_dir_local_bytes_total_hidratado_desde_listado_lazy() {
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
        "sin hydrate_plan el listado lazy dejaría bytes_total en Some(0)"
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
