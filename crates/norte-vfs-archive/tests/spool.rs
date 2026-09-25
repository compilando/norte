//! Spool for hot tar.gz files (#95.1): from the second read of the same
//! container onward, the provider decompresses the whole stream ONCE into
//! an anonymous tempfile and subsequent reads are local seeks — zero
//! container reads (verified with the `MemProvider`'s Faults counter).
//! Generation, zero budget and over-budget degrade to the usual
//! forward-decode, byte-exact.

mod common;

use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{ByteRange, Segment, VPath};
use norte_testkit::{MemProvider, TarSmith};
use norte_vfs::Provider;
use norte_vfs_archive::{ArchiveProvider, Format, Limits};

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("seg")
}

/// xorshift32 noise: genuinely incompressible — the resulting gz is
/// ~proportional to the decompressed size, so a forward-decode read pays
/// for several 256 KiB `ProviderReader` blocks (measurable via Faults).
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xFF) as u8
        })
        .collect()
}

async fn read_all(p: &ArchiveProvider, f: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    let mut stream = p.read(f, range).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    out
}

/// A container with TWO noise entries (a.bin crosses several 256 KiB
/// blocks) + a tar.gz provider with `limits`, keeping the Mem (Faults) and
/// the container's path (to rewrite it).
async fn setup(
    data_a: &[u8],
    data_b: &[u8],
    limits: Limits,
) -> (Arc<MemProvider>, ArchiveProvider, VPath, VPath) {
    let tar = TarSmith::new()
        .file(b"a.bin", data_a)
        .file(b"b.bin", data_b)
        .build();
    let gz = common::gzip(&tar);
    let (mem, container) = common::seed_container(b"fixture.tar.gz", &gz).await;
    let root = VPath::archive_compose("tar+gz", &container, &[]).expect("compose");
    let provider = ArchiveProvider::with_limits(
        Arc::clone(&mem) as Arc<dyn Provider>,
        Format::TarGz,
        "tar+gz+mem",
        limits,
    );
    (mem, provider, root, container)
}

/// #95.1's core: two complete reads warm up the container and build the
/// spool; the THIRD one pays NOT A SINGLE container read (a `read_calls`
/// delta ≤ 1) and is still byte-exact.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_hot_read_uses_the_spool() {
    let data_a = noise(600_000, 0x2545_F491);
    let data_b = noise(50_000, 0x9E37_79B9);
    let (mem, p, root, _) = setup(&data_a, &data_b, Limits::default()).await;
    let f = root.join(seg(b"a.bin"));

    let first = read_all(&p, &f, None).await;
    assert_eq!(first, data_a, "first read byte-exact");
    // Second read: crosses the heat threshold — builds the spool and
    // serves from it WITHIN the same read.
    assert_eq!(read_all(&p, &f, None).await, first, "second == first");

    let before = mem.faults().read_calls();
    let third = read_all(&p, &f, None).await;
    let delta = mem.faults().read_calls() - before;
    assert_eq!(third, first, "third read byte-exact");
    assert!(
        delta <= 1,
        "the third read must come from the spool, not the container \
         (read_calls delta = {delta})"
    );
}

/// Mutating the container invalidates the spool: the later read serves
/// the NEW content (never the stale spool) and the heat starts from zero.
#[tokio::test(flavor = "multi_thread")]
async fn the_spool_respects_generation() {
    let data_a = noise(400_000, 0xDEAD_BEE5);
    let data_b = noise(30_000, 0x0BAD_F00D);
    let (mem, p, root, container) = setup(&data_a, &data_b, Limits::default()).await;
    let f = root.join(seg(b"a.bin"));

    // Warms up until the old generation's spool gets built.
    assert_eq!(read_all(&p, &f, None).await, data_a);
    assert_eq!(read_all(&p, &f, None).await, data_a);

    // Rewrites the container: same entry name, different content and
    // SIZE (the generation surely changes even if the mtime is coarse).
    let data_a2 = noise(500_000, 0x1234_5678);
    let tar2 = TarSmith::new()
        .file(b"a.bin", &data_a2)
        .file(b"b.bin", &data_b)
        .build();
    common::write_file(mem.as_ref(), &container, &common::gzip(&tar2)).await;

    assert_eq!(
        read_all(&p, &f, None).await,
        data_a2,
        "after mutating the container the NEW content is served, not the stale spool"
    );
    // And the new generation's spool works again once reheated.
    assert_eq!(read_all(&p, &f, None).await, data_a2);
    let before = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a2);
    assert!(
        mem.faults().read_calls() - before <= 1,
        "the new generation gets spooled just like the old one"
    );
}

/// `spool_max_bytes = 0` disables the spool: the third read still pays for
/// the container (forward-decode) — and is still correct.
#[tokio::test(flavor = "multi_thread")]
async fn zero_budget_disables_the_spool() {
    let data_a = noise(600_000, 0xACED_C0DE);
    let data_b = noise(20_000, 0xFEED_FACE);
    let limits = Limits {
        spool_max_bytes: 0,
        ..Limits::default()
    };
    let (mem, p, root, _) = setup(&data_a, &data_b, limits).await;
    let f = root.join(seg(b"a.bin"));

    assert_eq!(read_all(&p, &f, None).await, data_a);
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let before = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let delta = mem.faults().read_calls() - before;
    assert!(
        delta > 1,
        "with a zero budget the third read still reads the container \
         (read_calls delta = {delta})"
    );
}

/// Decompressed size > `spool_max_bytes`: the build aborts
/// (negative-cache), there's no spool, and ALL reads stay correct via
/// forward-decode.
#[tokio::test(flavor = "multi_thread")]
async fn over_budget_does_not_spool() {
    let data_a = noise(300_000, 0x5EED_5EED);
    let data_b = noise(10_000, 0xB16B_00B5);
    let limits = Limits {
        spool_max_bytes: 1024, // the container's decompressed size exceeds it
        ..Limits::default()
    };
    let (mem, p, root, _) = setup(&data_a, &data_b, limits).await;
    let f = root.join(seg(b"a.bin"));

    assert_eq!(read_all(&p, &f, None).await, data_a);
    // The second one triggers the build, which aborts over budget and
    // degrades to forward-decode — still byte-exact.
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let before = mem.faults().read_calls();
    assert_eq!(read_all(&p, &f, None).await, data_a);
    let delta = mem.faults().read_calls() - before;
    assert!(
        delta > 1,
        "with no spool (non-spoolable), the third read pays for the container \
         (read_calls delta = {delta})"
    );
}

/// RANGED reads served from the spool: byte-exact with an offset in the
/// middle of the entry, crossing blocks, at the tail, over the OTHER entry
/// of the same container, and past-EOF — all without touching the container.
#[tokio::test(flavor = "multi_thread")]
async fn ranges_from_the_spool_are_byte_exact() {
    let data_a = noise(600_000, 0xCAFE_BABE);
    let data_b = noise(50_000, 0x8BAD_BEEF);
    let (mem, p, root, _) = setup(&data_a, &data_b, Limits::default()).await;
    let a = root.join(seg(b"a.bin"));
    let b = root.join(seg(b"b.bin"));

    // Warms up the container (heat is PER CONTAINER): the spool covers
    // both entries.
    assert_eq!(read_all(&p, &a, None).await, data_a);
    assert_eq!(read_all(&p, &a, None).await, data_a);

    let before = mem.faults().read_calls();
    let mid = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 300_123,
            len: Some(10_000),
        }),
    )
    .await;
    assert_eq!(
        mid,
        &data_a[300_123..310_123],
        "range in the middle of a.bin"
    );
    let tail = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 599_995,
            len: None,
        }),
    )
    .await;
    assert_eq!(tail, &data_a[599_995..], "tail range with no len");
    let other = read_all(
        &p,
        &b,
        Some(ByteRange {
            offset: 1_000,
            len: Some(500),
        }),
    )
    .await;
    assert_eq!(
        other,
        &data_b[1_000..1_500],
        "the container's OTHER entry also comes from the spool"
    );
    let past = read_all(
        &p,
        &a,
        Some(ByteRange {
            offset: 999_999_999,
            len: Some(1),
        }),
    )
    .await;
    assert_eq!(past, b"", "past-EOF of the ENTRY: empty stream");
    let delta = mem.faults().read_calls() - before;
    assert!(
        delta <= 1,
        "every range comes from the spool, not the container \
         (read_calls delta = {delta})"
    );
}
