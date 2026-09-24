//! The benchmark ADR 0002 deferred (#12): does `tokio-uring` have anything
//! to gain here?
//!
//! The ADR chose multi-threaded Tokio with `spawn_blocking` for all local
//! I/O and left `tokio-uring` as "optimization after a benchmark that
//! justifies it". This is that benchmark, and it's written to answer the
//! REAL question, which isn't "how long does it take" but **where does the
//! time go**:
//!
//! - if KERNEL work dominates (reading the directory, moving the bytes),
//!   `io_uring` doesn't remove it: it makes the same calls with a
//!   different wrapper;
//! - if what norte puts ON TOP dominates — the jump to the blocking pool,
//!   the backpressured channel, building an `Entry` per row — `io_uring`
//!   doesn't remove that either, because none of it is a system call.
//!
//! That's why it measures the two halves separately instead of a single
//! number: the RAW listing (`std::fs::read_dir` bare, the system's floor)
//! against the provider's listing (the same work plus everything norte
//! adds). The DIFFERENCE is the ceiling of what any I/O engine change
//! could trim, and it's measured without writing a second implementation
//! — which is what the benchmark exists to decide.
//!
//! `just bench` runs it. Criterion prints means and they're compared by
//! eye: a hard time gate in CI is flakiness, not a measurement.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use futures::StreamExt as _;
use norte_proto::VPath;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

/// How many entries the large directory has.
///
/// The issue asked for 100,000. It stays at 20,000 because the number
/// being sought is the cost PER ENTRY, which flattens out well before
/// that: five times as many files give the same answer and multiply by
/// five how long each of criterion's `iter`s takes — it repeats them —
/// and how much of whoever runs it's tempdir it takes up.
const ENTRIES: usize = 20_000;

/// A directory with [`ENTRIES`] EMPTY files.
///
/// Empty on purpose: what's measured is enumerating, not reading. A byte
/// of content would put the cost of opening into the listing's count.
fn big_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..ENTRIES {
        std::fs::write(dir.path().join(format!("f{i:06}.txt")), b"").expect("write");
    }
    dir
}

fn listing(c: &mut Criterion) {
    let dir = big_dir();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let mut g = c.benchmark_group("listing");
    // Fewer samples than the default: each one walks 20,000 entries.
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(15));

    // THE FLOOR: what the system costs, with no norte in between. It's not
    // an alternative that could be used — it gives no `Entry`, no
    // cancellation, no backpressure — it's the reference the one below is
    // read against.
    //
    // Calls `file_type()` because the provider ALSO calls it, and on Linux
    // it comes out of the dirent's `d_type` with no extra system call.
    // Without this line the floor would have come out cheaper than it
    // really is and the measured overhead would have been inflated in
    // favor of the conclusion.
    g.bench_function("raw_read_dir", |b| {
        b.iter(|| {
            let mut n = 0usize;
            for d in std::fs::read_dir(dir.path()).expect("read_dir").flatten() {
                black_box(d.file_type().expect("file_type"));
                n += 1;
            }
            black_box(n)
        });
    });

    // THE REAL PATH: `spawn_blocking` + bounded channel + one `Entry` per
    // row. The difference from the one above is the ceiling ADR 0002 and
    // issue #12 talk about.
    g.bench_function("provider_list", |b| {
        let provider = LocalProvider::rooted(dir.path().to_path_buf());
        let root = VPath::parse("file:///").expect("valid wire");
        b.iter(|| {
            rt.block_on(async {
                let mut s = provider.list(&root).await.expect("list");
                let mut n = 0usize;
                while let Some(entry) = s.next().await {
                    // Counted and dropped: what's measured is producing
                    // it, and accumulating them would put the cost of a
                    // 20,000-`Entry` `Vec` into the listing's count.
                    black_box(&entry.expect("entry"));
                    n += 1;
                }
                black_box(n)
            })
        });
    });
    g.finish();
}

/// Size of the copy. The issue said 10 GiB; criterion REPEATS every
/// measurement, so ten gibibytes per iteration is tens of minutes and a
/// full disk. 256 MiB gives the same number — MB/s — because the cost is
/// linear once the file doesn't fit in the page cache.
const COPY_SIZE: usize = 256 * 1024 * 1024;

fn copy(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source.bin");
    // Content that's NEITHER compressible NOR all zeros: a file of zeros
    // can be resolved by the filesystem without moving a byte, and then
    // this would measure creating a hole.
    let block: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    {
        use std::io::Write as _;
        let f = std::fs::File::create(&source).expect("create");
        let mut w = std::io::BufWriter::new(f);
        for _ in 0..(COPY_SIZE / block.len()) {
            w.write_all(&block).expect("write");
        }
        w.flush().expect("flush");
    }
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let provider = LocalProvider::rooted(dir.path().to_path_buf());
    let from = VPath::parse("file:///source.bin").expect("valid wire");

    let mut g = c.benchmark_group("copy");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(30));
    g.throughput(criterion::Throughput::Bytes(COPY_SIZE as u64));
    // Read through the provider and drop the bytes: measures the READ
    // path, which is the half a different I/O engine could change.
    // Writing would also bring in the destination's `fsync`, which
    // belongs to the kernel and nobody else.
    g.bench_function("provider_read", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut s = provider.read(&from, None).await.expect("read");
                let mut n = 0u64;
                while let Some(chunk) = s.next().await {
                    n += chunk.expect("chunk").len() as u64;
                }
                black_box(n)
            })
        });
    });
    g.finish();
}

criterion_group!(benches, listing, copy);
criterion_main!(benches);
