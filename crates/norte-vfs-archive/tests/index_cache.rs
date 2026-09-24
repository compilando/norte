//! #61.1: N concurrent operations over the same cold container build ONE
//! index, not N (single-flight).

mod common;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_proto::VPath;
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

/// Like `common::zip_provider_with_limits`, but also returns the inner
/// `Arc<MemProvider>` so its `Faults` can be read (the harness helper
/// hides it behind `Arc<dyn Provider>`).
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
async fn concurrent_indexing_coalesces_into_one_build() {
    let bytes = ZipSmith::new().file(b"a.txt", b"hi").build();

    // Baseline: a single cold build alone.
    let (provider, root, mem) = zip_provider_with_mem(&bytes).await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(5)));
    let _: Vec<_> = provider
        .list(&root)
        .await
        .expect("cold list")
        .collect()
        .await;
    let baseline = mem.faults().read_calls();
    assert!(baseline > 0);

    // A FRESH provider (empty cache), same bytes: 8 concurrent lists. The
    // per-op latency guarantees the overlap (all 8 reach the miss before
    // the first one finishes).
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
        "8 concurrent cold lists = the reads of a SINGLE build (single-flight)"
    );
}

/// #61.3: a hot `read` reuses the `ZipArchive` `index_for` already
/// parsed — it doesn't re-materialize the central directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_hot_read_does_not_reparse_the_central_directory() {
    // a.txt lives in the ProviderReader's block 0 (BLOCK=256 KiB); the
    // 300 KB filler pushes the central directory into the tail (block >=1).
    // Without caching, ZipArchive::new would reread the tail on EVERY
    // read -> delta >= 2.
    let filler: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let bytes = norte_testkit::ZipSmith::new()
        .file(b"a.txt", b"hi")
        .file(b"filler.bin", &filler)
        .build();
    assert!(
        bytes.len() > 262_144,
        "the central directory must fall outside block 0"
    );

    let (mem, path) = common::seed_container(b"fixture.zip", &bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        Limits::default(),
    );
    let entry_path = root.join(norte_proto::Segment::new(b"a.txt".to_vec()).expect("seg"));

    // Warms up the index (and with it, the cached CD).
    provider.stat(&entry_path).await.expect("stat");
    let before = mem.faults().read_calls();
    let chunks: Vec<_> = provider
        .read(&entry_path, None)
        .await
        .expect("read")
        .collect()
        .await;
    assert!(chunks.iter().all(Result::is_ok));
    let delta = mem.faults().read_calls() - before;
    assert_eq!(delta, 1, "a hot read = only the data block, no CD");
}

/// #59 (formerly #61 MAJOR-2): `max_cd_bytes` is now OBSOLETE — the CD is
/// parsed in streaming at index time and the zip locator is
/// self-contained, so a `read` never rereads the central directory, not
/// even with a tiny ceiling. Every hot read costs exactly the data block
/// (the local header + `a.txt`'s data live in the reader's block 0).
#[tokio::test(flavor = "multi_thread")]
async fn max_cd_bytes_is_obsolete_read_never_rereads_the_cd() {
    // Same fixture as the test above: the CD falls outside block 0.
    let filler: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let bytes = norte_testkit::ZipSmith::new()
        .file(b"a.txt", b"hi")
        .file(b"filler.bin", &filler)
        .build();
    assert!(bytes.len() > 262_144);

    let (mem, path) = common::seed_container(b"fixture.zip", &bytes).await;
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    // A tiny ceiling on purpose (the real CD is around ~110 bytes): before
    // #59 this forced the `ZipArchive` to reopen on every read; now the
    // field isn't consulted and the cost is identical to the cached path.
    let limits = Limits {
        #[allow(deprecated)] // pins the obsolete field (#59)
        max_cd_bytes: 80,
        ..Limits::default()
    };
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::Zip,
        "zip+mem",
        limits,
    );
    let entry_path = root.join(norte_proto::Segment::new(b"a.txt".to_vec()).expect("seg"));

    // Warms up the INDEX (the only structure cached since #59).
    provider.stat(&entry_path).await.expect("stat");

    for attempt in 0..2 {
        let before = mem.faults().read_calls();
        let mut stream = provider.read(&entry_path, None).await.expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(out, b"hi", "correct content with a tiny max_cd_bytes");
        let delta = mem.faults().read_calls() - before;
        assert_eq!(
            delta, 1,
            "attempt {attempt}: only the data block — the CD is never \
             reread on a read (#59, self-contained locator)"
        );
    }
}
