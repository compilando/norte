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

### 3. A malformed value degrades; it does not break the listing

`AttrValue` carries data, so `#[serde(other)]` cannot express a catch-all
variant. `AttrValue` therefore implements `Deserialize` by hand — the same
route `CapabilityFlags` already takes.

The rule is uniform, because "one bad cell costs one cell" is the entire point
of the type: **at the value level, EVERY malformed payload becomes
`AttrValue::Unknown`, and nothing is a hard error.** That covers an
unrecognised tag (a protocol-N+1 daemon, the ADR 0004 contract applied at value
granularity), a payload of the wrong JSON type (`{"uint": "33188"}`), a
`null`, a non-object, an empty object, an undecodable base64 payload, and a
value over the caps of §5. Errors at value level never propagate: the
containing message fails only when the JSON itself is unparseable or `attrs` is
not an object at all, which serde decides on the parent type.

An object carrying two or more KNOWN tags degrades too, rather than picking
one. JSON objects are unordered (RFC 8259 §4), so "the first key wins" would
make `{"uint":1,"bool":true}` and `{"bool":true,"uint":1}` — the same document —
parse differently, and any relay that round-trips through a sorted map could
flip the result. Deserialisation therefore looks every known tag up by name and
requires exactly one; a conforming peer never emits more.

`bytes_b64` is emitted as RFC 4648 §4 (standard alphabet, padding required) and
accepted in the unpadded and URL-safe forms as well: a producer using Go's
`RawStdEncoding` would otherwise lose every `Bytes` cell silently, and widening
what a reader accepts is backward-compatible.

`AttrValue::Unknown` serialises as `{"unknown": null}`. A conforming daemon
never emits it; it exists so a value that was read can be written back without
panicking, exactly as `EntryKind::Other` round-trips as `"other"`.

### 4. Attribute ids are namespaced and validated, labels are untrusted

An id is namespaced by construction, not just by convention: at least one
`.`, every `.`-separated segment non-empty and matching `[a-z0-9_-]`, at most
64 bytes total. `posix.mode`, `sftp.owner`, `s3.storage_class`, and
`archive.packed_size` are valid; a bare word with no dot (`mode`) or a dot
with an empty segment on either side (`posix.`, `.mode`) is not. There is no
central registry — a provider owns its namespace. Both peers validate; a
malformed id in a request is `-32602`. The rule starts strict on purpose:
relaxing a validator later is backward-compatible, tightening it after 0.30
ships is not.

On the RECEIVE side the rule is enforced by the type, not left to callers — a
contract that lived only in prose is one every consumer would have to remember,
and an id becomes a configuration id and a map lookup downstream. Both receive
sides deserialise through a hand-written visitor, and neither ever errors:

- `Entry.attrs` drops a malformed key and bounds the map at 16 entries, keeping
  the smallest ids in byte order, instead of failing the entry. What that buys
  precisely: the SET of ids that survives does not depend on the order the peer
  serialised its keys in; a key repeated within the same object resolves
  last-wins, as it would in any JSON parser.
- `FsCapabilitiesResult.attrs` drops an `AttrInfo` whose id is malformed — so a
  malformed id can never be discovered and therefore never requested — and
  truncates the catalog at the 64 of §5, keeping the FIRST descriptors in wire
  order and draining the rest without materialising them. The order is the
  provider's and it is meaningful (a column picker paints the catalog in it),
  so unlike the entry map it is never sorted. A descriptor that is not a
  well-formed `AttrInfo` at all (a missing `label`) is still serde's hard
  error: that peer is broken rather than newer, whereas an unknown `type` or
  `hint` already degrades to its `Unknown` variant.

The two REQUEST fields (`FsListParams.attrs`, `FsStatParams.attrs`) are the
deliberate exception: they carry data this peer is SENDING, so they get no
filtering deserialiser. Silently dropping a bad id there would turn "the client
asked for `../etc/passwd`" into "the client asked for nothing", hiding a
caller's bug and making the daemon-side `-32602` untestable.

The filter is one-directional on purpose. Serialisation is not filtered and the
fields are public, so an `Entry` or a catalog built in-process with a malformed
id emits it and decodes back different: a producer's bug must stay visible at
the boundary that validates it (daemon-side, block 2) rather than being
laundered by the serialiser, which would also make that validation untestable.

Both the caps and the id shape travel in the published JSON Schema (ADR 0038),
generated from the same constants the code applies: the artifact is the only
thing a third-party implementer reads, and an open `object`/`array` there would
tell them that 100 arbitrary ids are legal.

`AttrInfo::label` and any `Text`/`Bytes` value is **third-party text**: an SFTP
server controls `sftp.owner`, and a WASM provider plugin controls its own
labels. Frontends mask both through `norte_frontend::display_name` exactly as
they already mask a plugin's column header, and render `Bytes` through the
lossy-with-badge path used for non-UTF-8 filenames. The bytes themselves are
preserved (hard rule 1); only the rendering is lossy.

### 5. Caps

At most 16 requested ids per call, id ≤ 64 bytes, `AttrInfo::label` ≤ 64 bytes,
`Text` ≤ 256 bytes, `Bytes` ≤ 256 bytes decoded. The ceiling a listing page can
add is therefore bounded and predictable.

The value caps are enforced **at decode**, by `AttrValue` itself, in the only
way §3 allows: an over-cap `Text` or `Bytes` degrades that cell to `Unknown`
rather than failing the entry. A `bytes_b64` payload is rejected on the length
of its base64 TEXT before it is decoded, so an oversized value never allocates
the buffer it asks for. The daemon additionally enforces the caps on emit, so a
conforming peer never puts a client in that position. Requesting an unknown id is **not** an error: it comes
back absent, so a client holding a stale catalog degrades instead of failing.
Symmetrically, a `fs.capabilities` catalog is capped at 64 advertised
`AttrInfo` entries; exceeding it is **not** an error either — a client
TRUNCATES the advertised vector rather than rejecting the response, because a
fat catalog is a buggy provider, not a broken peer.

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
