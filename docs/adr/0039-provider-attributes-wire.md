# 0039 - Provider attributes on the wire

- Status: accepted
- Date: 2026-07-24
- Decision makers: Oscar González
- Related: spec §5 (`Capabilities`), §6.1 (listing presentation); ADR 0004
  (wire evolution: an unknown value degrades, it never breaks), ADR 0017
  (cursor pagination), ADR 0037 (plugin columns), ADR 0038 (protocol JSON
  Schema gate). Design: `docs/superpowers/specs/2026-07-24-columns-design.md`.

## Context

`Entry` carries `path`, `kind`, `size` and `mtime_ms`. Every other piece of
metadata a real provider knows — POSIX mode/uid/gid on local and sftp, the
owner and group strings an SFTP server sends, an S3 object's storage class and
etag, the compression method and packed size of an archive member — has no way
to reach a frontend. Configurable columns (the design this ADR serves) need
exactly that data, and each protocol has a different set of it.

Plugin columns (ADR 0037) do not solve this: a `columns` plugin runs in a WASM
sandbox with no access to the provider, so it cannot read an SFTP mode bit.

Three shapes were considered:

1. Provider-formatted strings. Trivial, but the value arrives pre-rendered:
   sorting a "1.2 GiB" column is lexicographic nonsense and the user loses all
   control over the format.
2. A closed set of new typed `Entry` fields (`mode`, `uid`, `etag`, …). Types
   survive, extensibility does not — the next provider needs another wire bump.
3. A namespaced, typed, provider-declared attribute map. Extensible, sortable,
   formattable.

## Decision

### 1. Attributes are declared, requested, and typed

- `FsCapabilitiesResult` gains `attrs: Vec<AttrInfo>`, where `AttrInfo` is
  `{ id, label, type, hint }`. This is discovery: a client learns which
  attributes exist for a provider from the call it already makes.
- `FsListParams` and `FsStatParams` gain `attrs: Vec<String>` — the client asks
  for the ids it will paint. Nothing is delivered unrequested.
- `Entry` gains `attrs: BTreeMap<String, AttrValue>`, where `AttrValue` is
  `Uint | Int | Text | Bytes | TimeMs | Bool | Unknown`.

`AttrType` (the declared type) and `AttrHint` (`Size`, `Timestamp`, `Mode`,
`Identity`, `Opaque` — the default format and alignment a frontend should pick)
are separate: two `Uint` attributes are formatted very differently depending on
whether they are a byte count or a permission word.

### 2. On demand, not always

The data providers publish is already in the response they parse (`statx`, the
SFTP attribute record, `ListObjectsV2`, the ZIP/TAR header), so the cost is
materialising and serialising it, not fetching it. A listing of 100 000 entries
must not pay for columns nobody displays, and the lazy `d_type` fast path in
`norte-vfs-local` (#52) must stay reachable. Requested-ids-only keeps both.

### 3. An unknown value degrades; it does not break the listing

`AttrValue` carries data, so `#[serde(other)]` cannot express a catch-all
variant. `AttrValue` therefore implements `Deserialize` by hand — the same
route `CapabilityFlags` already takes — and maps an unrecognised variant tag to
`AttrValue::Unknown`. A protocol-N+1 daemon that adds a variant degrades one
cell in one entry rather than failing the whole page, which is the ADR 0004
contract applied at value granularity.

A malformed base64 payload in `bytes_b64` degrades the same way, for the same
reason: one corrupt cell must not cost the user their listing. A malformed
*envelope* (a non-object, or an object with no keys) is still a hard error —
that is a broken peer, not a newer one.

`AttrValue::Unknown` serialises as `{"unknown": null}`. A conforming daemon
never emits it; it exists so a value that was read can be written back without
panicking, exactly as `EntryKind::Other` round-trips as `"other"`.

### 4. Attribute ids are namespaced and validated, labels are untrusted

An id matches `[a-z0-9._-]{1,64}` and is namespaced by its origin:
`posix.mode`, `sftp.owner`, `s3.storage_class`, `archive.packed_size`. There is
no central registry — a provider owns its namespace. Both peers validate;
a malformed id in a request is `-32602`.

`AttrInfo::label` and any `Text`/`Bytes` value is **third-party text**: an SFTP
server controls `sftp.owner`, and a WASM provider plugin controls its own
labels. Frontends mask both through `norte_frontend::display_name` exactly as
they already mask a plugin's column header, and render `Bytes` through the
lossy-with-badge path used for non-UTF-8 filenames. The bytes themselves are
preserved (hard rule 1); only the rendering is lossy.

### 5. Caps

Enforced by the server, re-validated by the client: at most 16 requested ids
per call, id ≤ 64 bytes, `AttrInfo::label` ≤ 64 bytes, `Text` ≤ 256 bytes,
`Bytes` ≤ 256 bytes decoded. The ceiling a listing page can add is therefore
bounded and predictable. Requesting an unknown id is **not** an error: it comes
back absent, so a client holding a stale catalog degrades instead of failing.

## Consequences

- Protocol 0.30.0. Purely additive: all three fields are
  `skip_serializing_if`-guarded, so a 0.29 peer emits and receives exactly
  today's bytes. The N/N-1 window moves to N=0.30.x / N-1=0.29.x.
- `norte-proto` gains a `base64` dependency (0.22, already a vetted workspace
  dependency used by `norte-core` for `fs.read`). It is needed because
  `AttrValue::Bytes` owns its decode: leaving the value as a base64 `String`
  would push error handling onto every consumer and invite a second, divergent
  decode path.
- Adding a field to `Entry` touches every struct literal in the workspace
  (~120, mostly tests). Mechanical, compiler-driven, one commit.
- This block ships no producer: no provider advertises an attribute and the
  daemon ignores requested ids. That is honest under the contract — absence is
  a valid answer — and keeps the wire change reviewable on its own.
