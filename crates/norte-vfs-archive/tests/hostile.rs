//! Hostile cases for the tar provider: traversal, corruption, entry
//! bombs, duplicates, symlinks, cache invalidation and scheme wiring.

mod common;

use futures::StreamExt;
use norte_proto::{ByteRange, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_testkit::{MemProvider, TarSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

async fn list_names(p: &ArchiveProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = p
        .list(dir)
        .await
        .expect("list")
        .map(|e| {
            e.expect("entry ok")
                .path
                .file_name()
                .expect("has a name")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    names
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

#[tokio::test]
async fn traversal_and_absolutes_are_omitted_from_the_tree() {
    let tar = TarSmith::new()
        .file(b"../evil", b"slip")
        .file(b"ok.txt", b"fine")
        .build();
    // `/abs` and `a/../b` can't be forged with TarSmith (100 bytes yes,
    // but the `tar` crate emits them as-is): forged manually via the name.
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(list_names(&p, &root).await, vec![b"ok.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"ok.txt")), None).await,
        b"fine"
    );
}

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

/// #93: `list_skipped` exposes the container index's `skipped` —
/// `Some(n)` with the hostile ones counted, `Some(0)` in a clean tar.
/// It's what the frontend signals as a badge ("N entries omitted").
#[tokio::test]
async fn list_skipped_exposes_the_indexs_omitted_entries() {
    let hostile = TarSmith::new()
        .file(b"../evil", b"slip")
        .file(b"ok.txt", b"fine")
        .build();
    let (p, root) = common::tar_provider(&hostile).await;
    assert_eq!(
        p.list_skipped(&root).await.expect("list_skipped"),
        Some(1),
        "the omitted traversal entry must be counted"
    );
    // Also from a subpath of the container (the total is PER CONTAINER).
    assert_eq!(
        p.list_skipped(&root.join(seg(b"ok.txt")))
            .await
            .expect("ok"),
        Some(1)
    );

    let clean = TarSmith::new().file(b"a.txt", b"x").build();
    let (p, root) = common::tar_provider(&clean).await;
    assert_eq!(p.list_skipped(&root).await.expect("ok"), Some(0));
}

#[tokio::test]
async fn a_truncated_tar_is_corrupt() {
    let mut tar = TarSmith::new().file(b"big.bin", &[7u8; 2000]).build();
    tar.truncate(700); // cuts mid-data + no closing blocks
    let (p, root) = common::tar_provider(&tar).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[tokio::test]
async fn non_tar_garbage_is_corrupt() {
    let (p, root) = common::tar_provider(b"this is not a tar\x00\x01").await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[tokio::test]
async fn max_entries_cuts_the_indexing() {
    let tar = TarSmith::new()
        .file(b"a/one", b"1")
        .file(b"a/two", b"2")
        .file(b"a/three", b"3")
        .build();
    // max_entries=3: implicit `a` + 2 files use up the whole budget.
    let limits = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    let (p, root) = common::tar_provider_with_limits(&tar, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("expected LimitExceeded(entries), got {other:?}"),
    }
}

#[tokio::test]
async fn duplicate_last_one_wins() {
    let tar = TarSmith::new()
        .file(b"x.txt", b"first")
        .file(b"x.txt", b"second!")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let f = root.join(seg(b"x.txt"));
    assert_eq!(p.stat(&f).await.expect("stat").size, Some(7));
    assert_eq!(read_all(&p, &f, None).await, b"second!");
}

#[tokio::test]
async fn symlink_lstat_and_read_link() {
    let tar = TarSmith::new()
        .file(b"docs/real.txt", b"content")
        .symlink(b"lnk", b"docs/real.txt")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let lnk = root.join(seg(b"lnk"));
    assert_eq!(p.stat(&lnk).await.expect("stat").kind, EntryKind::Symlink);
    assert_eq!(
        p.read_link(&lnk).await.expect("read_link"),
        b"docs/real.txt"
    );
    // `read` on the link: TypeMismatch (lstat, never follow it).
    match p.read(&lnk, None).await {
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }) => {}
        other => panic!("expected TypeMismatch, got {:?}", other.err()),
    }
}

#[tokio::test]
async fn range_passthrough_with_the_correct_offset() {
    // Two files: the second one does NOT start at 0 inside the tar — the
    // requested range gets translated to the container's real offset.
    let tar = TarSmith::new()
        .file(b"first.bin", &[0xAA; 600])
        .file(b"second.bin", b"0123456789")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let f = root.join(seg(b"second.bin"));
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 2,
                len: Some(3)
            })
        )
        .await,
        b"234"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 8,
                len: None
            })
        )
        .await,
        b"89"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 99,
                len: Some(1)
            })
        )
        .await,
        b"",
        "past-EOF of the ENTRY (not of the container): empty stream"
    );
}

#[tokio::test]
async fn invalidation_by_the_containers_generation() {
    let (mem, path) = common::seed_container(
        b"fixture.tar",
        &TarSmith::new().file(b"v1.txt", b"one").build(),
    )
    .await;
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let p = ArchiveProvider::new(std::sync::Arc::clone(&mem) as _, Format::Tar, "tar+mem");
    assert_eq!(list_names(&p, &root).await, vec![b"v1.txt".to_vec()]);
    // Rewrite the container (remove + write: new generation).
    common::write_file(
        mem.as_ref(),
        &path,
        &TarSmith::new().file(b"v2.txt", b"two!").build(),
    )
    .await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"v2.txt".to_vec()],
        "the cached index gets invalidated by (mtime,size)"
    );
}

#[tokio::test]
async fn a_missing_container_or_a_dir_fails_clean() {
    let mem = std::sync::Arc::new(MemProvider::new());
    let dir = MemProvider::root().join(seg(b"undir"));
    mem.mkdir(&dir).await.expect("mkdir");
    let p = ArchiveProvider::new(std::sync::Arc::clone(&mem) as _, Format::Tar, "tar+mem");

    let missing = VPath::archive_compose(
        "tar",
        &MemProvider::root().join(seg(b"does-not-exist.tar")),
        &[],
    )
    .expect("compose");
    assert_eq!(p.stat(&missing).await.unwrap_err(), Error::NotFound);

    let overdir = VPath::archive_compose("tar", &dir, &[]).expect("compose");
    match p.stat(&overdir).await {
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        }) => {}
        other => panic!("expected TypeMismatch over a dir, got {other:?}"),
    }
}

#[tokio::test]
async fn a_foreign_scheme_is_invalid_path() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let (p, _) = common::tar_provider(&tar).await;
    // A zip+mem path against the tar+mem provider: broken wiring = InvalidPath.
    let zip_path = VPath::parse("zip+mem:///fixture.tar/!/x").expect("parse");
    assert_eq!(p.stat(&zip_path).await.unwrap_err(), Error::InvalidPath);
    // And a path WITHOUT the marker with the right scheme: malformed = InvalidPath.
    let no_marker = VPath::parse("tar+mem:///fixture.tar").expect("parse");
    assert_eq!(p.stat(&no_marker).await.unwrap_err(), Error::InvalidPath);
}

#[tokio::test]
async fn odd_kinds_are_listed_as_other_without_read() {
    // TarSmith doesn't forge hardlinks; a tar with only dir+symlink+file
    // covers the v1 kinds. Coverage of Other = a future real tar (debt issue).
    let tar = TarSmith::new()
        .dir(b"d")
        .file(b"d/f", b"x")
        .symlink(b"s", b"d/f")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(
        list_names(&p, &root).await,
        vec![b"d".to_vec(), b"s".to_vec()]
    );
}

/// #58: a failure of the INNER provider (a network cut mid-index) is
/// genuine IO and propagates VERBATIM — it never gets disguised as
/// "corrupt tar". `Corrupt` is reserved for the format truly being broken.
#[tokio::test]
async fn a_failure_of_the_inner_provider_is_not_disguised_as_corrupt() {
    let tar = TarSmith::new().file(b"ok.txt", b"fine").build();
    let (mem, path) = common::seed_container(b"fixture.tar", &tar).await;
    let faults = mem.faults();
    let root = VPath::archive_compose("tar", &path, &[]).expect("compose");
    let p = ArchiveProvider::with_limits(mem, Format::Tar, "tar+mem", Limits::default());
    // The cut lands MID-PARSE (after the index's earlier ops: the
    // generation stat + the first reads), not before: it's the path that
    // crosses the format's `corrupt()` helpers.
    for n in 0..8u64 {
        faults.clear();
        faults.disconnect_after(n);
        match p.list(&root).await.map(|_| ()) {
            Err(Error::ProviderUnavailable { retryable: true }) | Ok(()) => {}
            other => panic!("with disconnect_after({n}) the inner IO got disguised: {other:?}"),
        }
    }
}

/// #97: tar's passthrough (contiguous data) with a container that gets
/// TRUNCATED under the read — the inner provider ends the stream short
/// with pread semantics and WITHOUT an error (like a real FS): the reader
/// must get `Corrupt` after the partial bytes, never a silently short
/// file (parity with zip #95.4 and targz FIX-1).
#[tokio::test]
async fn a_short_tar_passthrough_is_corrupt_not_short_data() {
    use bytes::Bytes;
    use norte_proto::{ByteRange as BR, Capabilities, Entry as PEntry};
    use norte_vfs::{ByteStream, EntryStream as ES};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A delegate over Mem that, when ARMED, cuts every read stream in
    /// half of its chunks — with no error, like a container mutated out
    /// from under it.
    struct Truncating {
        inner: MemProvider,
        armed: std::sync::Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl norte_vfs::Provider for Truncating {
        #[expect(clippy::unnecessary_literal_bound, reason = "the trait's signature")]
        fn scheme(&self) -> &str {
            "mem"
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<PEntry, Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<ES, Error> {
            self.inner.list(p).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            norte_vfs::Provider::write(&self.inner, p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
            self.inner.rename(from, to).await
        }
        async fn read(&self, p: &VPath, range: Option<BR>) -> Result<ByteStream, Error> {
            let stream = self.inner.read(p, range).await?;
            if !self.armed.load(Ordering::Relaxed) {
                return Ok(stream);
            }
            // Gathers it and cuts it in HALF of the requested bytes: a clean end.
            let all: Vec<Result<Bytes, Error>> = stream.collect().await;
            let mut bytes: Vec<u8> = Vec::new();
            for c in all {
                bytes.extend_from_slice(&c.expect("chunk ok"));
            }
            bytes.truncate(bytes.len() / 2);
            Ok(futures::stream::iter(vec![Ok(Bytes::from(bytes))]).boxed())
        }
    }

    let tar = TarSmith::new().file(b"data.bin", &[7u8; 1000]).build();
    let mem = MemProvider::new();
    let root = MemProvider::root();
    let container = root.join(seg(b"c.tar"));
    {
        let mut sink = norte_vfs::Provider::write(&mem, &container)
            .await
            .expect("write opens");
        sink.write(bytes::Bytes::from(tar)).await.expect("chunk");
        sink.commit().await.expect("commit");
    }
    let armed = std::sync::Arc::new(AtomicBool::new(false));
    let provider = ArchiveProvider::new(
        std::sync::Arc::new(Truncating {
            inner: mem,
            armed: std::sync::Arc::clone(&armed),
        }),
        Format::Tar,
        "tar+mem",
    );
    let inner_path = VPath::parse("tar+mem:///c.tar/!/data.bin").expect("wire");

    // Healthy: a full round trip (the index stays hot).
    assert_eq!(provider.read(&inner_path, None).await.map(|_| ()), Ok(()));
    let mut stream = provider
        .read(&inner_path, None)
        .await
        .expect("healthy read");
    let mut total = 0usize;
    while let Some(item) = stream.next().await {
        total += item.expect("healthy chunk").len();
    }
    assert_eq!(total, 1000);
    // Review's M1: polling after Ready(None) doesn't panic (fused stream).
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());

    // Armed: the inner one cuts it in half WITHOUT an error -> Corrupt, not silence.
    armed.store(true, Ordering::Relaxed);
    let mut stream = provider.read(&inner_path, None).await.expect("read opens");
    let mut seen = 0usize;
    let mut failure = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(c) => seen += c.len(),
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    match failure {
        Some(Error::Corrupt) => assert_eq!(seen, 500, "the partial bytes arrive, then the error"),
        other => panic!("expected Corrupt after {seen} bytes, got {other:?}"),
    }
    // After the Err: a clean None and a safe poll-after-None (fused),
    // never a second Err nor a panic.
    assert!(stream.next().await.is_none());
    assert!(stream.next().await.is_none());
}

/// #60 (H6): GNU longname — the header carries the name TRUNCATED to 100
/// but the crate's `path_bytes()` applies the byte-exact longname. No
/// test compiled this path: the corpus's 255-byte name, via longname,
/// must be listed WHOLE and read.
#[tokio::test]
async fn gnu_longname_roundtrips_the_corpuss_255_byte_name() {
    let long_name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255")
        .expect("corpus fixture")
        .bytes;
    assert_eq!(long_name.len(), 255);
    let tar = TarSmith::new()
        .file_gnu_longname(&long_name, b"long-content")
        .file(b"short.txt", b"x")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let names = list_names(&p, &root).await;
    assert!(
        names.contains(&long_name),
        "the 255-byte name lists BYTE-EXACT via longname"
    );
    assert_eq!(
        read_all(&p, &root.join(seg(&long_name)), None).await,
        b"long-content"
    );
}

/// #60: pax `path=` override with NON-UTF8 bytes — the crate applies the
/// pax record's path byte-exact (real pax demands UTF-8; hostile tars don't).
#[tokio::test]
async fn pax_path_non_utf8_roundtrips() {
    let name = b"docs/caf\xe9.txt"; // é in Latin-1 inside a pax path
    let tar = TarSmith::new().file_pax_path(name, b"pax!").build();
    let (p, root) = common::tar_provider(&tar).await;
    let docs = list_names(&p, &root).await;
    assert_eq!(docs, vec![b"docs".to_vec()], "the pax path's implicit dir");
    assert_eq!(
        read_all(&p, &root.join(seg(b"docs")).join(seg(b"caf\xe9.txt")), None).await,
        b"pax!"
    );
}

/// #60: zip-slip VIA longname — a traversal that doesn't fit in the
/// ustar header (>100 bytes) arrives whole via the longname and must be
/// OMITTED just like the short one (counting in skipped), never sneaking
/// in because it came via the long path.
#[tokio::test]
async fn zip_slip_via_longname_is_omitted() {
    // MAJOR from the audit: the prefix truncated to 100 must be BENIGN —
    // the traversal lives ONLY past the truncation point. With a broken
    // longname, the benign "aaa…" header would sneak into the listing and
    // skipped would be 0: both asserts fail loudly (tested with a mutant
    // that suppresses the L).
    let mut evil = vec![b'a'; 100];
    evil.extend_from_slice(b"/../../../etc/passwd");
    let tar = TarSmith::new()
        .file_gnu_longname(&evil, b"slip")
        .file(b"ok.txt", b"fine")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    let names = list_names(&p, &root).await;
    assert!(
        !names.contains(&vec![b'a'; 100]),
        "the benign truncated name does NOT sneak in"
    );
    assert_eq!(names, vec![b"ok.txt".to_vec()]);
    assert_eq!(
        p.list_skipped(&root).await.expect("skipped"),
        Some(1),
        "the longname's traversal counts as omitted"
    );
}

/// #60 (H5, direct pin via `entry_raw`): a `pax_global_header` (typeflag
/// `g`, what `git archive` emits) does NOT ghost into the listing — the
/// crate's iterator doesn't consume it alone and `classify_entry`'s
/// filter discards it.
#[tokio::test]
async fn a_raw_pax_global_header_does_not_ghost_in() {
    let tar = TarSmith::new()
        .entry_raw(b'g', b"pax_global_header", b"23 comment=git archive\n")
        .file(b"real.txt", b"yes")
        .build();
    let (p, root) = common::tar_provider(&tar).await;
    assert_eq!(list_names(&p, &root).await, vec![b"real.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"real.txt")), None).await,
        b"yes"
    );
}

// ---------- attrs (#108 block 2): tar announces NONE ----------

#[tokio::test]
async fn attrs_tar_catalog_is_empty() {
    use norte_testkit::TarSmith;
    let tar = TarSmith::new().file(b"f.txt", b"x").build();
    let (p, _root) = common::tar_provider(&tar).await;
    assert!(norte_vfs::Provider::attrs(&p).is_empty());
}
