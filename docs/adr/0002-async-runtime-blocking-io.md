# 0002 - Async runtime and blocking I/O

- Status: accepted
- Date: 2026-07-08
- Decision makers: Oscar González

## Context

The specification requires asynchronous, cancellable I/O and a UI that never
blocks. Native filesystem APIs on all three supported operating systems are
synchronous, so the project needs an async runtime and a safe bridge for
blocking filesystem work.

## Options considered

1. **Multi-threaded Tokio with `spawn_blocking` for local filesystems**
   - Uses a mature ecosystem, including `CancellationToken`, streams, and
     testcontainers.
   - Keeps synchronous filesystem work off async worker threads.
   - Provides one model across all supported operating systems.
   - Adds a small thread handoff to each operation, which is negligible beside
     real I/O.
2. **Tokio with `tokio-uring` on Linux**
   - May reduce system calls and copies on modern Linux.
   - Creates two platform-specific code paths immediately and complicates
     cancellation while the API continues to evolve.
3. **smol or async-std**
   - Offers a smaller ecosystem and no compensating project-specific advantage.

## Decision

Use the multi-threaded Tokio runtime throughout the workspace. All local
filesystem access runs through `spawn_blocking` and remains inside
`norte-vfs-local`; other crates do not call `std::fs` directly.

Providers bridge blocking producers to async consumers with bounded channels to
provide backpressure. Cancellation is cooperative through
`tokio_util::sync::CancellationToken`, checked inside every task's inner loop.

`tokio-uring` remains a possible feature-gated optimization if benchmarks show
a meaningful benefit.

### The deferred benchmark was run, and the answer is no (#12)

Measured 2026-08-31 on Linux (`crates/norte-vfs-local/benches/local_io.rs`,
`just bench`), listing a directory of 20 000 entries:

| | per listing | per entry |
| --- | --- | --- |
| `std::fs::read_dir` + `file_type` — the system's own floor | 3.67 ms | 183 ns |
| the real path: `spawn_blocking` + bounded channel + one `Entry` per row | 23.83 ms | 1191 ns |

**85% of the provider's listing time is not system calls.** The floor is
measured doing exactly what the provider does — `read_dir` plus `file_type`,
which on Linux comes from the dirent's `d_type` without an extra call — so the
gap is `Entry` construction, the channel, and the handoff to the blocking pool.
io_uring attacks the other 15%, and only part of it: it batches submissions, it
does not stop us allocating a `VPath` per row.

Reading is even clearer: the same harness reads 256 MiB through the provider at
**5.0 GiB/s**, which is page cache and an order of magnitude above any device
this would run on. The read path is not what limits a copy.

So the feature gate is **not opened**. The line worth optimising, if listing
ever needs to be faster, is the 85% — and that is ordinary Rust in
`norte-vfs-local`, not a second I/O engine with its own cancellation semantics
and a second platform-specific code path. Re-run the benchmark before revisiting
this: the conclusion is a measurement, not an opinion, and it expires if the
listing path changes shape.

## Consequences

- The workspace has one concurrency model and can test timers with
  `tokio::time::pause`.
- The blocking pool is bounded and configurable. Lazy streams release their
  blocking thread between chunks.
- Peak Linux performance is NOT meaningfully below an io_uring implementation:
  the benchmark above says system calls are 15% of a listing and that the read
  path already runs an order of magnitude faster than any device. #12 is closed
  on that measurement rather than on the guess this line used to carry.
- Tokio becomes a structural dependency across the asynchronous codebase.
