//! #61.1: N operaciones concurrentes sobre el mismo contenedor frío
//! construyen UN índice, no N (single-flight).

mod common;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_proto::VPath;
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

/// Como `common::zip_provider_with_limits`, pero devolviendo también el
/// `Arc<MemProvider>` interior para poder leer sus `Faults` (el helper del
/// harness lo esconde detrás de `Arc<dyn Provider>`).
async fn zip_provider_with_mem(bytes: &[u8]) -> (ArchiveProvider, VPath, Arc<MemProvider>) {
    let (mem, path) = common::seed_container(b"fixture.zip", bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    (provider, root, mem)
}

#[tokio::test(flavor = "multi_thread")]
async fn indexado_concurrente_coalesce_en_un_build() {
    let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();

    // Baseline: un build frío en solitario.
    let (provider, root, mem) = zip_provider_with_mem(&bytes).await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(5)));
    let _: Vec<_> = provider
        .list(&root)
        .await
        .expect("list fría")
        .collect()
        .await;
    let baseline = mem.faults().read_calls();
    assert!(baseline > 0);

    // Provider FRESCO (caché vacía), mismos bytes: 8 lists concurrentes.
    // La latencia por op garantiza el solapamiento (los 8 llegan al miss
    // antes de que el primero termine).
    let (provider2, root2, mem2) = zip_provider_with_mem(&bytes).await;
    mem2.faults()
        .set_latency_per_op(Some(Duration::from_millis(5)));
    let p = Arc::new(provider2);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = Arc::clone(&p);
        let root2 = root2.clone();
        handles.push(tokio::spawn(async move {
            let _: Vec<_> = p.list(&root2).await.expect("list").collect().await;
        }));
    }
    for h in handles {
        h.await.expect("join");
    }
    assert_eq!(
        mem2.faults().read_calls(),
        baseline,
        "8 lists concurrentes frías = los reads de UN solo build (single-flight)"
    );
}
