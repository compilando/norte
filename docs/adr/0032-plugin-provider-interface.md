# 0032 - Plugin provider interface (WIT projection of the `Provider` trait)

- Status: proposed
- Date: 2026-07-22
- Decision makers: Oscar González
- Related: specification section 7.1; ADRs 0022 (plugin host) and 0010
  (core/plugin/config boundary); issue #30

## Context

Issue #30 asks to migrate `norte-vfs-ftp` to a **plugin-provider**: a VFS
provider that runs as a sandboxed WASM plugin, exercising a WIT `provider`
interface that projects the `norte_vfs::Provider` trait. FTP is the chosen
first candidate — the least "core" provider (legacy, unencrypted, its own
dependency) — and the migration is the dogfood proving a third party can write
a provider without touching the core.

Three hard mismatches make this a milestone, not a small change, and this ADR
exists to design the interface and **stage** the work so each risky surface is
reviewed on its own:

1. **Async + streaming vs. synchronous WIT.** `Provider` is `async` and
   streams: `list -> EntryStream`, `read -> ByteStream`, `write -> ByteSink`
   (see `crates/norte-vfs/src/provider.rs`). The plugin host today makes
   **synchronous** guest calls (the previewer is `render(input) -> string`,
   ADR 0022); wasmtime 46 in this tree has no async host-call story and no
   `wasi:io` async streams wired. Projecting a streaming async trait onto a
   sync component is the core design problem.
2. **Network egress.** FTP needs TCP (control + data connections). The host
   grants no network capability today; `wasi:sockets` must be wired and gated.
3. **Identical contract.** The acceptance criterion is that the shared
   `provider_contract!` suite passes **unchanged** against the plugin-backed
   provider — a working guest, not a stub.

## Decision

### The projection: paginated / ranged synchronous calls

The `Provider` stream/async surface is projected to **bounded synchronous**
calls; the host adapter (`PluginProvider`, below) reassembles streams from
bounded pages/chunks. This reuses mechanisms already in the tree:

- **`list`** → `list(path, cursor) -> (list<entry>, option<cursor>)`. Paginated,
  reusing the cursor pagination of #27 (`FsList` params/`next_cursor`). The
  adapter drives `EntryStream` by looping until the cursor is exhausted. A long
  listing is bounded per page → cancellation is granular (see below).
- **`read`** → `read(path, offset, len) -> list<u8>`. A bounded range read
  (`ByteRange` already exists in the trait). The adapter turns repeated ranged
  reads into a `ByteStream`; EOF = a short/empty chunk.
- **`write`** → a `resource writer` with `open(path) -> writer`,
  `write(chunk)`, `commit()`, `abort()` — projecting `ByteSink` (including its
  `commit`/`abort` discipline for clean-or-`.norte-partial` semantics).
- **`stat` / `mkdir` / `remove` / `rename` / `trash` / `read-link` /
  `symlink`** → straightforward sync request/response.
- **`capabilities`** → `capabilities() -> caps` (the `CapabilityFlags`
  bitflags projected as a WIT record/flags).

### Byte-honesty (regla 1) crosses the WIT as bytes, never `string`

Every path and every filename crosses the interface as `list<u8>` (raw `VPath`
segments / `Entry.name` bytes), **never** a WIT `string` — a `string` is
Component-Model UTF-8 and would force a lossy decode on hostile names. The
`preview-input.content` precedent (`list<u8>`) applies; here it is mandatory
for the whole surface. Errors project to a WIT `enum` mirroring
`norte_proto::Error` (NotFound, PermissionDenied, Conflict, …) so the taxonomy
survives round-trip.

### Cancellation via host epoch interruption

`Provider` cancellation is drop-based (dropping the stream aborts the work).
The projection makes each guest call **bounded** (one page / one chunk), and
the host cancels an in-flight call with the **epoch-deadline** mechanism
already used for the CPU budget (ADR 0022, `runtime.rs`). The adapter maps a
dropped `EntryStream`/`ByteStream` or a `CancellationToken` trip to "stop
issuing further paged calls" + epoch-cancel the current one. No new
cancellation primitive is needed.

### Network: a new gated `net` capability (stage 3)

FTP requires outbound TCP. A new capability `net = "outbound"` (host-gated,
fail-closed, same discipline as `fs-read = "scoped"`) wires `wasi:sockets/tcp`
into the store **only** for provider plugins that declare it. This is the
largest new attack surface in the whole feature (a plugin opening network
connections) and gets its **own** security review + policy-engine integration
(M3) — which is exactly why it is isolated in the last stage.

### The host adapter closes the loop

`norte-plugin-host` grows a `PluginProvider` that `impl`s `norte_vfs::Provider`
by calling the guest interface (looping paged `list`, ranged `read`, driving
the `writer` resource). The **acceptance test is the existing
`provider_contract!`** macro run against `PluginProvider` — the same suite the
in-tree providers pass. Only when it is green does the in-tree crate retire.

### WIT sketch (design-stage; not yet in `norte-plugin.wit`)

```wit
interface provider {
    // Bytes, never string (regla 1). A VPath is its raw segments joined.
    type path = list<u8>;
    record entry { name: list<u8>, kind: entry-kind, size: option<u64>, /* … */ }
    enum vfs-error { not-found, permission-denied, conflict, unsupported, /* … */ }

    capabilities: func() -> caps;
    stat: func(p: path) -> result<entry, vfs-error>;
    // Paginación (#27): página + cursor opaco; el host reensambla el stream.
    list: func(p: path, cursor: option<list<u8>>)
        -> result<tuple<list<entry>, option<list<u8>>>, vfs-error>;
    // Rango acotado; el host reensambla el ByteStream. Chunk corto = EOF.
    read: func(p: path, offset: u64, len: u64) -> result<list<u8>, vfs-error>;
    mkdir: func(p: path) -> result<_, vfs-error>;
    remove: func(p: path) -> result<_, vfs-error>;
    rename: func(from: path, to: path) -> result<_, vfs-error>;
    // ByteSink como recurso: open → write* → commit | abort.
    resource writer {
        write: func(chunk: list<u8>) -> result<_, vfs-error>;
        commit: func() -> result<_, vfs-error>;
        abort: func() -> result<_, vfs-error>;
    }
    open-writer: func(p: path) -> result<writer, vfs-error>;
}
```

This interface is **not** added to the `norte-plugin` world yet: doing so pulls
in the host adapter, a guest, and the `net` capability — the staged work below.

## Staging

- **Stage 1 (this ADR):** interface + projection design; decisions recorded.
- **Stage 2 (no network):** land the `provider` WIT interface, the
  `PluginProvider` host adapter, and a **`MemProvider`-backed guest** that
  passes `provider_contract!`. Proves the async→sync projection in isolation,
  with zero network attack surface. Reviewers: protocol-guardian (the WIT is a
  wire contract), rust, encoding (paths/names as bytes round-trip).
- **Stage 3 (network + FTP):** the `net` capability + `wasi:sockets` wiring
  (own security review), port `norte-vfs-ftp`'s protocol logic into the guest,
  pass `provider_contract!` over a real FTP server, then retire the in-tree
  crate. Closes #30.

## Consequences

- The projection is **paginated-sync**, not streaming: a large read is N
  bounded host calls the adapter stitches into a `ByteStream`. Acceptable —
  throughput is bounded by page/chunk size, tuned like the existing pagination.
- `net = "outbound"` is a **major** new attack surface; isolating it in stage 3
  keeps stages 1–2 free of network risk and lets the socket grant get a
  dedicated review + policy-engine hook.
- Until stage 3 lands, `norte-vfs-ftp` stays in-tree (the current path,
  unchanged) — #30 remains open, now with a de-risked, staged design.
- If the async→sync projection proves too costly in stage 2 (round-trip
  overhead per chunk), the fallback is to wait for `wasi:io` async streams in a
  later wasmtime — but the paginated design is the pragmatic path that works
  with the runtime in the tree today.
