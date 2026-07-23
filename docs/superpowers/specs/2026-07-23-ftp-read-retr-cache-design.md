# FTP guest RETR cache — design (#30 debt M1)

**Date:** 2026-07-23
**Status:** approved
**Related:** ADR 0033 (FTP provider as plugin), ADR 0032 (WIT provider interface)

## Problem

The FTP provider guest (`crates/norte-plugin-host/examples-wasm/ftp-provider/src/lib.rs`)
projects the WIT `provider` interface. The host adapter (`PluginProvider::read`,
`crates/norte-core/src/plugin_provider.rs`) reads a file by calling
`read(segments, offset, len)` in a loop with 64 KiB chunks and an increasing
offset. Today each call runs a full `stat` + `REST offset` + `RETR` + drain-to-EOF
cycle, so reading an N-byte file in chunks costs O(N²) transfer (each chunk
re-downloads from its offset to EOF and discards the tail). On a no-MLSD server it
also runs one `LIST` of the parent per chunk. This makes FTP effectively unusable
beyond small files — a regression versus the native provider's single streaming RETR.

## Decision: guest-side RETR cache (no WIT change)

Keep the WIT `read(segments, offset, len)` signature unchanged. Optimise the guest
so sequential reads reuse one live RETR data connection. No wire change, no version
bump, no protocol-guardian gate, no churn to the mem guests or the adapter — the
improvement is transparent behind `read`.

### Why not a WIT `reader` resource

A streaming `reader` resource (mirror of `writer`) was considered and rejected: it
requires a breaking WIT bump (0.4.0→0.5.0), rewrites the read path in all three
guests plus the adapter, and — critically — holding a RETR open across host calls
leaves the FTP `226` transfer reply pending on the control connection. If the host
interleaves a `stat`/`list` on the same session between two `read` calls, the
pending reply desyncs the connection. Guarding that would force the adapter to hold
the instance mutex for the whole stream lifetime across `spawn_blocking`/await
boundaries. The cache approach below achieves the same O(N) and defeats the
interleaving hazard by construction, entirely guest-side.

## Components (all in `ftp-provider/src/lib.rs`)

### 1. Cache state on `Session`

```rust
struct CachedRead {
    /// Remote path of the RETR in progress.
    remote: String,
    /// The next offset this stream will deliver (bytes already consumed + start).
    next_offset: u64,
    /// The live RETR data connection (impl Read). suppaftp `DataStream<TcpStream>`.
    reader: DataStream<TcpStream>,
}
// Session gains: cached_read: Option<CachedRead>
```

`DataStream<T>` is what `retr_as_stream` returns; on wasm the transport `T` is the
guest's TCP stream type. The struct holds no borrow of `Session.ftp` (the data
connection is independent of the control connection).

### 2. `flush_cached_read(s: &mut Session)`

Drains the cached reader to EOF (best-effort, bounded reads discarding bytes), calls
`s.ftp.finalize_retr_stream(reader)` to consume the pending transfer reply and reset
suppaftp's data-connection state, then sets `s.cached_read = None`. Idempotent:
`None` is a no-op. Never returns an error (best-effort cleanup; a broken control
connection surfaces on the next real op).

### 3. `read(segments, offset, len)`

1. `let remote = remote(&s.base, &segments)?;`
2. Cache hit iff `s.cached_read` is `Some` with the same `remote` **and**
   `next_offset == offset`. On hit, reuse its `reader`.
3. On miss: `flush_cached_read(s)`; `stat_remote` (reject dir → `Conflict`, absent →
   `NotFound`); if `offset > 0`, `resume_transfer(offset)`; `retr_as_stream(remote)`;
   install `CachedRead { remote, next_offset: offset, reader }`.
4. Read up to `len` bytes from the cached reader into `out`. Advance
   `s.cached_read.next_offset += out.len()`.
5. If a read returned 0 before reaching `len` (EOF): `flush_cached_read(s)` (clean
   finalize) and return `out`.
6. Otherwise leave the cache live and return `out`.
7. On a read **error** mid-stream: `flush_cached_read(s)` (best-effort) then return
   `VfsError::Io` — never leave a half-consumed reader that could desync.

Because step 2 requires an exact offset match, a random-access read (viewer seeking
backward, or a different path) is a miss that flushes and re-RETRs — correct, and no
worse than today.

### 4. Flush before every control command

Every op that issues an FTP control command must call `flush_cached_read(s)` first,
so a pending `226` from an in-progress RETR is never interleaved: `stat`, `list_dir`,
`open_writer`, `make_dir`, `remove`, `rename`, and `FtpWriter::{write, commit,
abort}`. `configure` starts a fresh session so it has no cache to flush. This is the
invariant that makes the cache safe: **no control command is ever sent while a RETR
reply is pending.**

## Data flow

The adapter's `read` loop (`plugin_provider.rs`) calls `read(off, want)` with
`off = previous_off + bytes_returned`, exactly matching `next_offset`. Sequential
reads therefore hit the cache and drain one RETR → O(N). A bounded range read
(`ByteRange` with `len`) reads until the adapter's limit and stops dropping the
stream; the cache stays live until the next op flushes it — bounded, never
desyncing.

## Error handling

- Read error mid-stream → flush + `Io` (step 7).
- Cache miss stat/RETR errors → mapped as today (`map_err`), no cache installed.
- Abandoned stream (host stops mid-read, e.g. cancellation): the cached RETR stays
  open until the next provider op flushes it. Bounded window; never desyncs because
  every op flushes first. On provider drop the whole instance (and its sockets) tear
  down.

## Testing

- The existing shared `provider_contract!` (44 cases, `ftp_provider_contract.rs`)
  must stay green — the change is behaviourally transparent.
- New guest-level e2e in `norte-plugin-host/tests/` driving `ProviderInstance`
  against in-process libunftp:
  - **Sequential read reuses one RETR**: seed a file larger than one adapter chunk
    (e.g. 512 KiB), read it fully via the adapter, assert byte-exact content. (A
    RETR-count assertion needs a server hook; correctness + the interleave test below
    are the load-bearing checks.)
  - **Interleave safety**: read half a file, drop the stream, `stat` a different
    path, then read the file again fully → byte-exact, session not desynced.
  - **Range read then op**: bounded read of a slice, then a `list_dir` → both
    succeed (the cache flush before `list_dir` leaves the control clean).
- A cancellation/clean test: start a read, drop it mid-stream, run another op → the
  next op flushes the orphaned RETR without error.

## Non-goals

- No WIT change, no version bump, no adapter change, no mem-guest change.
- The wasm32 4 GiB size ceiling (ADR 0033 H2) is a separate debt, untouched here.
- Same-host FTP→FTP copy (needs a dedicated read connection, #39 B1) stays a
  separate debt; a single-connection guest serialises read and write, which the
  cache flush keeps safe (a write flushes any pending read).
