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

## Versionado del WIT (review protocol-guardian)

El WIT es un contrato de wire (Component Model). Disciplina:

- **Un cambio de la interfaz bumpea el paquete.** Añadir `provider` +
  `norte-provider` subió `norte:plugin@0.1.0 → 0.2.0`; el camino de escritura
  (writer resource + make-dir/remove/rename) subió `0.2.0 → 0.3.0`. Para
  previewer/command/host-log es ADITIVO. Para un GUEST de `provider`, en cambio,
  añadir items a una interfaz EXPORTADA es ROMPEDOR (un guest a 0.2.0 no
  satisface el world 0.3.0); es seguro porque `provider` es pre-release sin
  guests publicados y el bump de minor bajo 0.x codifica "ruptura permitida" —
  el único guest se bumpea en lockstep.
- **`provider` está en EVOLUCIÓN** (stage 3 añade escritura, hace crecer `caps`
  y `vfs-error`, y sus `record`/`enum` WIT no son forward-extensibles). Idealmente
  viviría en su **propio paquete** (`norte:provider@…`) para versionar
  independiente del contrato ESTABLE de previewer/command. El wit-parser en el
  árbol (0.239) **no soporta paquetes anidados** en un mismo fichero ni un
  segundo paquete suelto en el mismo directorio; separarlo exige el layout
  `wit/deps/`. Decisión: se pospone la separación a **stage 3**, cuando la
  interfaz se estabiliza y el churn cesa; hasta entonces comparte
  `norte:plugin` y cada cambio de `provider` bumpea el paquete común (coste
  aceptado: previewer/command no fijan versión, así que un bump no rompe sus
  guests).
- **`vfs-error` es un `enum` CERRADO**, no el `Unknown` forward-compatible de
  `norte_proto::Error`: añadir una variante es un cambio de wire. Por eso se
  incluyen ya las variantes relevantes al camino de lectura (`cursor-expired`,
  `provider-unavailable`, `loop`, `conflict`, `no-space`) — para no tener que
  romper el wire al mapearlas. El flag `retryable` de `io`/`provider-unavailable`
  se difiere a stage 3 (exigirá migrar el `enum` a un `variant`).
- **Cursor OPACO** (`list<u8>`), no un índice `u32`: un token de continuación
  remoto (S3/FTP) o el cursor expirable de #27 no caben en un entero.

## Staging

- **Stage 1 (this ADR):** interface + projection design; decisions recorded.
- **Stage 2a (WIT + host wire) — DONE** (commit `ee2737e`): the `provider` WIT
  interface + `norte-provider` world + `ProviderInstance` host wrappers + a
  `provider-mem` read-only guest + a wire round-trip E2E (paths/names as bytes,
  hostile-name byte-exact). Reviewers protocol-guardian/encoding/rust applied.
- **Stage 2b-read — DONE:** the `PluginProvider` adapter (`norte-core`) that
  `impl`s `norte_vfs::Provider`, reassembling the async streams from the
  bounded guest calls (paginated `list`, ranged `read`), and a target-gated
  E2E that runs the full **read** contract (the same checks as
  `readonly_provider_contract!`) against it — green. Finding: a WIT name not
  representable as a `VPath` `Segment` (e.g. contains `/`) is OMITTED by the
  adapter (like archive providers, #93); counting it via `list_skipped` is
  stage-2b debt.
- **Stage 2b-write — DONE:** the write path — a `writer` resource in the WIT
  (`write`/`commit`/`abort`) + `open-writer`/`make-dir`/`remove`/`rename`
  (package bumped to `0.3.0`), the host `ProviderInstance` resource wiring
  (`ResourceAny` handle + `writer_drop`), a `PluginByteSink` projecting the
  transactional `ByteSink` onto the guest resource, and the adapter's mutations
  now DELEGATED (a read-only guest returns `Unsupported`). A writable
  `provider-mem-rw` guest + a write E2E (write/commit staging, abort,
  mkdir/remove/rename, byte-exact hostile content) — green. Remaining before
  #30's contract-complete: run the full RW `provider_contract!` macro (deferred
  — it needs the wasm target at test time and can't SKIP; the write E2E mirrors
  its checks) and wire `list_skipped` for the dropped non-`Segment` names.
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

## Amendment 2026-08-08: the shared package is split, and the blocker is gone

This ADR recorded a known debt: `provider` shared the `norte:plugin` package
with `previewer`, `command`, `decorator` and `columns`, so any bump to it
renamed every interface in the package and every previously compiled `.wasm`
stopped instantiating — on the import side, verified empirically twice in the
WIT header's own comments. The blocker on fixing it was recorded as
`wit-parser` 0.239 not supporting nested `wit/deps/` packages.

**That blocker is lifted.** The tree is on `wit-parser` 0.251, which resolves a
`wit/deps/` layout without complaint; the split's first attempt failed only
because cross-package references need an explicit version
(`import norte:host/host-log@0.1.0;`, not `import norte:host/host-log;`).

The WIT is now three packages:

- `norte:host@0.1.0` — `host-log` and `host-config`, the two doors every world
  imports. The leaf of the graph, versioned apart and deliberately almost
  never bumped.
- `norte:plugin@0.7.0` — `previewer`, `command`, `decorator`, `columns` and
  their three worlds.
- `norte:provider@0.1.0` — the `provider` interface and the `norte-provider`
  world. It starts at 0.1.0 rather than inheriting 0.6.0: it is a new package,
  and its history stays in the header it came from.

ADR 0041 decision 3 is what made this urgent rather than tidy: the gaps in
`provider` — server-side copy, trash, attributes, resume, cancellation — close
when a real plugin needs them, so this interface is going to keep moving. Split,
that movement no longer renames `norte:plugin/previewer`.

The split is itself the last break of every guest at once, since `host-log` and
`host-config` changed package. Every guest in `examples-wasm/` and the embedded
`crates/norte-core/resources/ftp-provider.wasm` were recompiled in the same
change. Guest-side `wit_bindgen::generate!` needed `generate_all` added: it
refuses to guess what to do with imports from outside the world's own package.

`crates/norte-plugin-host/tests/wit_packages.rs` guards the structure, because
nothing else can. Every guest here is recompiled from the current WIT on every
build, so the suite is exactly as green with one package as with three — the
damage from merging them back would land on somebody else's already-compiled
artefact, outside this repository.
