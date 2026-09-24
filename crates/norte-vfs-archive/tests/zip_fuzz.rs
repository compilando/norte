//! Short fuzz (proptest) of the zip provider (spec §12, threat model §14): a
//! container of ARBITRARY bytes never `panic`s — Ok or a typed `Error` — and
//! an arbitrary entry name survives byte for byte (rule 1). Runs in the PR
//! gate; nightly widens the cases.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Segment, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format};
use proptest::prelude::*;

/// A zip provider over `bytes` seeded in a Mem, with its inner root.
async fn zip_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
    let mem = Arc::new(MemProvider::new());
    let path = MemProvider::root().join(Segment::new(b"f.zip".to_vec()).expect("seg"));
    let mut sink = mem.write(&path).await.expect("write");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
    (ArchiveProvider::new(mem, Format::Zip, "zip+mem"), root)
}

/// Walks the WHOLE inner tree (recursive list), forcing the decoding of
/// each name. Returns the final names or the first error.
async fn walk(p: &ArchiveProvider, dir: &VPath) -> Result<Vec<Vec<u8>>, norte_proto::Error> {
    let mut out = Vec::new();
    let mut stream = p.list(dir).await?;
    while let Some(e) = stream.next().await {
        let e = e?;
        if let Some(name) = e.path.file_name() {
            out.push(name.as_bytes().to_vec());
        }
        if e.kind == norte_proto::EntryKind::Dir {
            out.extend(Box::pin(walk(p, &e.path)).await?);
        }
    }
    Ok(out)
}

proptest! {
    /// Arbitrary bytes as a "zip": build + list NEVER panics. The result is
    /// Ok (a valid zip by chance) or a typed Error, never a crash
    /// (anti-bomb/anti-malformed per the spec).
    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let (p, root) = zip_provider(&data).await;
            // list of the root + walk: Ok or Err, never panic.
            let _ = walk(&p, &root).await;
        });
    }
}

proptest! {
    // Fewer cases: each one builds a real zip + runtime.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// An arbitrary entry name (no `/` or `\0`, not empty, not `.`/`..`,
    /// not the reserved `!` marker) comes back from the listing BYTE FOR
    /// BYTE — bit 11/cp437 is metadata, the raw name rules (rule 1, ADR
    /// 0018).
    ///
    /// The `!` segment is left OUT on purpose: ADR 0018 reserves it as the
    /// compound scheme's marker, so `archive_compose` rejects it
    /// (`ArchiveAddressing`) and the index omits it as unaddressable
    /// (`index.rs`, "`!` component (ADR 0018 marker)"). A lone `!` isn't
    /// addressable in a compound path — a documented limitation, not a
    /// silent loss (it's logged and counted as `skipped`) —, and the
    /// generator has to respect the same invariant it already respects for
    /// `.`/`..`.
    #[test]
    fn arbitrary_name_roundtrips_byte_exact(
        raw in proptest::collection::vec(any::<u8>(), 1..40)
            .prop_filter("legal segment name (not the `!` marker)", |b| {
                !b.contains(&b'/')
                    && !b.contains(&0)
                    && b.as_slice() != b"."
                    && b.as_slice() != b".."
                    && b.as_slice() != b"!"
            }),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let zip = ZipSmith::new().file(&raw, b"content").build();
            let (p, root) = zip_provider(&zip).await;
            let names = walk(&p, &root).await.expect("a valid zip lists");
            prop_assert!(
                names.iter().any(|n| n == &raw),
                "name {raw:?} did not come back byte-exact; got {names:?}"
            );
            Ok(())
        })?;
    }
}
