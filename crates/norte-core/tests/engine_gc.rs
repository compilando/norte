//! `Engine::gc_partials` (#11, ADR 0012): dispatches the `.norte-partial`
//! staging sweep to the provider that serves the path. Point-in-time, no Task,
//! no journal. No local staging provider → no-op.

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
    assert_eq!(n, 0, "a provider without local staging: no-op");
}

#[tokio::test]
async fn gc_partials_dispatches_to_local_and_sweeps() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Orphaned staging in its exact stable FORM (prefix + 32 hex).
    std::fs::write(
        dir.path()
            .join(".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c"),
        b"x",
    )
    .expect("plant partial");
    // A real user file with the prefix must NOT be swept.
    std::fs::write(dir.path().join(".norte-partial.backup"), b"keep").expect("plant backup");

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);

    // older_than = ZERO → any age (>=0) qualifies: deterministic.
    let n = engine
        .gc_partials(&LocalProvider::root(), Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(n, 1, "sweeps only the staging file with the exact form");
    assert!(
        dir.path().join(".norte-partial.backup").exists(),
        "the user's backup is respected"
    );
}
