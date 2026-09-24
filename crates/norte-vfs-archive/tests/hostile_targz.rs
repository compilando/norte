//! Hostile/roundtrip cases for the tar.gz provider (#55, ADR 0028): the gz
//! layer adds two risks plain tar doesn't have — it isn't seekable
//! (forward-decode O(offset) per read) and it's forward-only in the index
//! (concatenated gzip members, truncation, gzip bombs). The hostile corpus
//! of NAMES (traversal, absolutes, the `!` marker…) is already covered by
//! `readonly_provider_contract!` in `contract.rs`, reusing the same
//! gzipped canonical tree.

mod common;

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Segment, VPath};
use norte_testkit::TarSmith;
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

/// Full round trip + a range in the middle, with content that CROSSES the
/// `ProviderReader`'s 256 KiB block (the forward-decode discard rule: the
/// offset is served by discarding decompressed bytes, not with `Seek`).
#[tokio::test(flavor = "multi_thread")]
async fn a_large_roundtrip_and_a_range_in_the_middle() {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let tar = TarSmith::new()
        .file(b"first.bin", b"header")
        .file(b"big.bin", &data)
        .build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let f = root.join(seg(b"big.bin"));

    assert_eq!(
        p.stat(&f).await.expect("stat").size,
        Some(data.len() as u64)
    );
    assert_eq!(read_all(&p, &f, None).await, data, "full read byte-exact");

    // Range with an offset IN THE MIDDLE of the file (forces the forward
    // discard).
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 262_100,
                len: Some(100)
            })
        )
        .await,
        &data[262_100..262_200],
        "range in the middle, crossing the ProviderReader's 256 KiB block"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 299_995,
                len: None
            })
        )
        .await,
        &data[299_995..],
        "tail range with no len"
    );
    assert_eq!(
        read_all(
            &p,
            &f,
            Some(ByteRange {
                offset: 999_999,
                len: Some(1)
            })
        )
        .await,
        b"",
        "past-EOF of the ENTRY: empty stream"
    );
}

/// CONCATENATED gzip members (real tgz files have them — e.g. `git archive
/// | gzip` can produce several, and tools that append data too):
/// `MultiGzDecoder` decodes them as one continuous stream and the whole
/// tar gets indexed just as with a single member.
#[tokio::test]
async fn multi_member_gzip_indexes_whole() {
    let tar = TarSmith::new()
        .file(b"a.txt", b"first")
        .file(b"b.txt", b"second")
        .build();
    // Splits the PLAIN tar in half by bytes (not by entry) and gzips each
    // half separately: two concatenated gzip members that, decoded in
    // series, reproduce the original tar byte for byte.
    let mid = tar.len() / 2;
    let mut multi = common::gzip(&tar[..mid]);
    multi.extend_from_slice(&common::gzip(&tar[mid..]));

    let (p, root) = common::targz_provider(&multi).await;
    let mut names: Vec<Vec<u8>> = p
        .list(&root)
        .await
        .expect("list")
        .map(|e| {
            e.expect("ok")
                .path
                .file_name()
                .expect("name")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    names.sort();
    assert_eq!(names, vec![b"a.txt".to_vec(), b"b.txt".to_vec()]);
    assert_eq!(
        read_all(&p, &root.join(seg(b"b.txt")), None).await,
        b"second"
    );
}

/// A tar.gz container cut in half: PINS the behavior — the index fails
/// with `Corrupt` (never silent short data). Cutting the COMPRESSED
/// BYTES in half leaves the gzip decoder with an incomplete member (CRC/
/// truncated stream), or, if the cut lands inside the already-decompressed
/// tar, leaves the `tar` iterator with an entry lacking enough data to
/// skip over. Both cases are genuine decoder IO errors that `corrupt()`
/// translates into `Corrupt`.
#[tokio::test]
async fn a_truncated_tar_gz_is_corrupt() {
    let tar = TarSmith::new().file(b"big.bin", &[7u8; 4000]).build();
    let gz = common::gzip(&tar);
    let truncated = &gz[..gz.len() / 2];
    let (p, root) = common::targz_provider(truncated).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::Corrupt) => {}
        other => panic!("expected Corrupt with a truncated tar.gz, got {other:?}"),
    }
}

/// Classic bomb: a big file of zeros compresses to almost nothing. With a
/// small `max_decompressed_bytes`, the index cuts BEFORE paying for the
/// full decompression — it never hangs.
#[tokio::test]
async fn a_decompression_bomb_is_cut_by_the_limit() {
    let tar = TarSmith::new()
        .file(b"bomb.bin", &vec![0u8; 4_000_000])
        .build();
    let gz = common::gzip(&tar);
    let limits = Limits {
        max_decompressed_bytes: 4096,
        ..Limits::default()
    };
    let (p, root) = common::targz_provider_with_limits(&gz, limits).await;
    match p.list(&root).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "decompressed-bytes" => {}
        other => panic!("expected LimitExceeded(decompressed-bytes), got {other:?}"),
    }
}

/// Dropping the read stream mid-decompression: the `spawn_blocking`
/// thread ends at the next chunk (closed channel, rule 3) — nothing hangs
/// and a later re-read is still correct.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_stream_cancels_the_forward_decode() {
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 13) as u8).collect();
    let tar = TarSmith::new().file(b"huge.bin", &data).build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let f = root.join(seg(b"huge.bin"));

    let mut stream = p.read(&f, None).await.expect("read");
    let first = stream.next().await.expect("there's a chunk").expect("ok");
    assert!(!first.is_empty());
    drop(stream); // the blocking thread dies on the next send (closed channel)

    assert_eq!(
        read_all(&p, &f, None).await,
        data,
        "re-reading it whole is still intact"
    );
}

/// Garbage after the gzip (trailing garbage, NOT a valid gzip member):
/// PINS `MultiGzDecoder`'s behavior — it stops cleanly at the last valid
/// member (the garbage is ignored), it doesn't propagate an error.
/// Documented: if a crate upgrade changes this, the test goes red with a
/// warning.
#[tokio::test]
async fn garbage_after_the_gzip_is_ignored() {
    let tar = TarSmith::new().file(b"x.txt", b"content").build();
    let mut gz = common::gzip(&tar);
    gz.extend_from_slice(b"this is not a valid gzip member, it is garbage");
    let (p, root) = common::targz_provider(&gz).await;
    assert_eq!(
        read_all(&p, &root.join(seg(b"x.txt")), None).await,
        b"content",
        "the valid content still reads fine; the garbage after the last member is ignored"
    );
}

/// #58 (same criterion as tar/zip) + FIX-3 (rust MINOR-2, #55 review): a
/// GENUINE failure of the INNER provider (disconnection mid-index)
/// propagates VERBATIM even after crossing flate2 + tar-rs (which may
/// rewrap the original `io::Error`) — it never gets disguised as
/// `Corrupt`.
#[tokio::test]
async fn a_failure_of_the_inner_provider_is_not_disguised_as_corrupt() {
    let tar = TarSmith::new().file(b"ok.txt", b"fine").build();
    let gz = common::gzip(&tar);
    let (mem, path) = common::seed_container(b"fixture.tar.gz", &gz).await;
    let faults = mem.faults();
    let root = VPath::archive_compose("tar+gz", &path, &[]).expect("compose");
    let p = ArchiveProvider::with_limits(mem, Format::TarGz, "tar+gz+mem", Limits::default());
    // The cut lands at different points of the parsing (index +
    // decompression); in NONE of them should it look disguised as
    // "corrupt tar.gz".
    for n in 0..8u64 {
        faults.clear();
        faults.disconnect_after(n);
        match p.list(&root).await.map(|_| ()) {
            Err(Error::ProviderUnavailable { retryable: true }) | Ok(()) => {}
            other => panic!("with disconnect_after({n}) the inner IO got disguised: {other:?}"),
        }
    }
}

/// FIX-2 (security MAJOR, #55 review): the forward-decode's concurrency
/// semaphore QUEUES the excess reads, it never rejects them. Fires
/// `GZ_READ_CONCURRENCY` (4) + 2 concurrent reads of the SAME entry with
/// latency injected into the inner provider (simulates the real pinned
/// thread without needing a giant container) and checks that ALL of them
/// complete with the correct content.
#[tokio::test(flavor = "multi_thread")]
async fn gz_read_concurrency_queues_rather_than_rejects() {
    let tar = TarSmith::new().file(b"a.txt", b"short content").build();
    let gz = common::gzip(&tar);
    let (mem, path) = common::seed_container(b"fixture.tar.gz", &gz).await;
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(15)));
    let root = VPath::archive_compose("tar+gz", &path, &[]).expect("compose");
    let p = Arc::new(ArchiveProvider::with_limits(
        mem,
        Format::TarGz,
        "tar+gz+mem",
        Limits::default(),
    ));
    let f = root.join(seg(b"a.txt"));

    let mut handles = Vec::new();
    for _ in 0..6u32 {
        let p = Arc::clone(&p);
        let f = f.clone();
        handles.push(tokio::spawn(async move { read_all(&p, &f, None).await }));
    }
    for h in handles {
        assert_eq!(
            h.await.expect("join"),
            b"short content",
            "every read above the concurrency ceiling must QUEUE and complete, not fail"
        );
    }
}

/// flate2's real Content-Encoding via `write::GzEncoder` in several
/// `write_all` calls (not just one contiguous buffer) — smoke test that
/// the `common::gzip` helper doesn't depend on writing it all at once.
#[tokio::test]
async fn gzip_in_parts_produces_the_same_result() {
    let tar = TarSmith::new().file(b"p.txt", b"parts").build();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&tar[..tar.len() / 2]).expect("part 1");
    enc.write_all(&tar[tar.len() / 2..]).expect("part 2");
    let gz = enc.finish().expect("finish");
    let (p, root) = common::targz_provider(&gz).await;
    assert_eq!(
        read_all(&p, &root.join(seg(b"p.txt")), None).await,
        b"parts"
    );
}

/// #60: GNU longname ALSO via the tar.gz path (sequential `entries()`,
/// no Seek) — `classify_entry` is shared but the iterator isn't: the
/// corpus's 255-byte name lists byte-exact and reads.
#[tokio::test]
async fn gnu_longname_roundtrips_via_targz() {
    // MEDIUM from the audit: the MULTIBYTE variant (85×あ) — truncating to
    // 100 splits a UTF-8 sequence (100 = 33×3+1): if the crate used the
    // header's name instead of the longname, the listing would come out
    // with a different invalid-UTF8 name and the read would fail — a
    // built-in canary.
    let long_name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255_multibyte")
        .expect("corpus fixture")
        .bytes;
    let tar = TarSmith::new()
        .file_gnu_longname(&long_name, b"gz-long")
        .build();
    let gz = common::gzip(&tar);
    let (p, root) = common::targz_provider(&gz).await;
    let name_seg = norte_proto::Segment::new(long_name).expect("seg");
    let f = root.join(name_seg);
    let mut stream = p.read(&f, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"gz-long");
}
