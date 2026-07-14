//! `Engine::gc_partials` (#11, ADR 0012): despacha el barrido de staging
//! `.norte-partial` al provider que sirve el path. Puntual, sin Task, sin
//! journal. Sin provider de staging local → no-op.

use std::sync::Arc;
use std::time::Duration;

use norte_core::Engine;
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

#[tokio::test]
async fn gc_partials_noop_on_provider_without_local_staging() {
    let engine = Engine::new();
    engine.register_provider(Arc::new(MemProvider::new()) as Arc<dyn Provider>);
    let n = engine
        .gc_partials(&MemProvider::root(), Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(n, 0, "provider sin staging local: no-op");
}

#[tokio::test]
async fn gc_partials_dispatches_to_local_and_sweeps() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Staging huérfano en su FORMA estable exacta (prefijo + 32 hex).
    std::fs::write(
        dir.path()
            .join(".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c"),
        b"x",
    )
    .expect("plant partial");
    // Un archivo real del usuario con el prefijo NO debe barrerse.
    std::fs::write(dir.path().join(".norte-partial.backup"), b"keep").expect("plant backup");

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);

    // older_than = ZERO → cualquier antigüedad (>=0) califica: determinista.
    let n = engine
        .gc_partials(&LocalProvider::root(), Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(n, 1, "barre solo el staging con forma exacta");
    assert!(
        dir.path().join(".norte-partial.backup").exists(),
        "el backup del usuario se respeta"
    );
}
