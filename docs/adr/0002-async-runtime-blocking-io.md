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

## Consequences

- The workspace has one concurrency model and can test timers with
  `tokio::time::pause`.
- The blocking pool is bounded and configurable. Lazy streams release their
  blocking thread between chunks.
- Peak Linux performance may remain below an io_uring implementation until the
  deferred benchmark is run.
- Tokio becomes a structural dependency across the asynchronous codebase.
