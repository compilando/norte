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

That uniformity is why the value is read with a STREAMING visitor rather than
materialised into a `serde_json::Value` first. The shorter route had two
consequences that contradicted the rule above: `serde_json`'s recursion limit
applies while BUILDING a value, so a ~250-byte payload nested 125 deep inside
one cell of one entry failed the whole page — a value-level hard error, and one
the very same bytes never caused in an unknown field, which derived serde skips
with `IgnoredAny` — and a `Value` costs several times the bytes it came from,
reintroducing exactly the amplification the bounds below exist to prevent. The
visitor knows the tag before it reads the payload, so it decodes straight into
the variant, checks the caps before copying anything, and drains everything
else iteratively.

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

An id is namespaced by construction, not just by convention: at least one `.`,
every `.`-separated segment starting with an ASCII letter and continuing in
`[a-z0-9_-]`, at most 64 bytes total —
`^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$`. `posix.mode`, `sftp.owner`,
`s3.storage_class`, `s3.etag` and `archive.packed_size` are valid; a bare word
with no dot (`mode`) or a dot with an empty segment on either side (`posix.`,
`.mode`) is not. There is no central registry — a provider owns its namespace.
Both peers validate; a malformed id in a request is `-32602`. The rule starts
strict on purpose: relaxing a validator later is backward-compatible,
tightening it after 0.30 ships is not.

The leading letter is the part of the rule that is about something other than
tidiness: it is what keeps an id from being read as a DIFFERENT kind of token
downstream. `-x.y` is argv-shaped, and block 2 takes `--attrs <id>` on the CLI;
`0.0` is float-shaped in a configuration file that keys columns by id. Digits
inside a segment stay legal, so `s3.etag` and `posix.ctime_ms` are unaffected.
The timing argument above is the whole reason this lands now rather than later:
`-.-`, `0.0`, `9-9.9-9` and `__.__` are ids no provider wants and every one of
them is free to exclude today and impossible to exclude after 0.30 ships.

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
  malformed id crossing the WIRE can never be discovered and therefore never
  requested — drops a REPEATED id keeping the FIRST, clamps an over-long label
  to the 64 bytes of §5 on a char boundary, and truncates the catalog at the 64
  advertised entries of §5, keeping the FIRST descriptors in wire order. The
  order is the provider's and it is meaningful (a column picker paints the
  catalog in it), so unlike the entry map it is never sorted. A descriptor that
  is not a well-formed `AttrInfo` at all (a missing `label`) is still serde's
  hard error: that peer is broken rather than newer, whereas an unknown `type`
  or `hint` already degrades to its `Unknown` variant.

Repeats therefore resolve in OPPOSITE directions on the two receive sides, and
the contrast is the reason: a catalog is an ordered list its provider ranked, so
"first" is a real choice and first-wins keeps it; the keys of `Entry.attrs`
arrive in a JSON object, which RFC 8259 §4 leaves unordered, so there is no
first to prefer and last-wins matches what any JSON parser would do. Keeping
both copies of an advertised id would be worse than either rule: a consumer
folding the catalog into a map and one using `find()` would render the same
bytes differently, which is a cross-frontend divergence (TUI vs GUI) from a
single response. Rejects and duplicates are discarded BEFORE a slot is taken,
so padding a catalog cannot starve a legitimate later attribute out of the 64.

Both rules live in ONE function, `attrs::sanitize_catalog`, and the catalog is
not a `Vec<AttrInfo>` but an `attrs::AttrCatalog`: a newtype whose field is
private and whose only constructor runs that function. A filter that sat only
on the deserialisation boundary would be a filter the DEFAULT configuration
never applies — an EMBEDDED backend (TUI/CLI with no daemon in between) does not
cross it — so block 2's in-process catalogs, including those from a WASM
provider plugin the threat model treats as untrusted, would reach a frontend
unvalidated whenever someone forgot the call. `PluginColumnInfo` (ADR 0037),
which validates no id and caps no length, is the standing evidence that the call
gets forgotten. Block 2's embedded path therefore MUST hand its catalog to
`AttrCatalog::new` — which is not a discipline to remember but the only way to
produce the type the field holds. `sanitize_catalog` stays public because block
2 assembles and reorders plain vectors before wrapping one.

The two REQUEST fields (`FsListParams.attrs`, `FsStatParams.attrs`) are the
deliberate exception: they carry data this peer is SENDING, so nothing about
them is filtered or deduplicated. Silently dropping a bad id there would turn
"the client asked for `../etc/passwd`" into "the client asked for nothing",
hiding a caller's bug and making the daemon-side `-32602` untestable.

Two decode-time bounds exist purely for MEMORY, and neither is validation. A
catalog stops being EXAMINED past 256 elements (4 × 64) and the rest is drained
unmaterialised: bounding only what is KEPT would let a peer sending four
million malformed descriptors be parsed in full for a result of zero. A request
keeps its first 17 (16 + 1) elements and drains the rest: a 16 MiB frame of
`["a","a",…]` is ~4 million elements and ~15× that in `String` headers, decided
before any daemon-side check can run. The `+ 1` matters — over-cap must stay
observable as `len() > 16` rather than be trimmed into legality.

For `Entry.attrs` the filter is one-directional on purpose. Serialisation is not
filtered and the field is public, so an entry built in-process with a malformed
id emits it and decodes back different: a producer's bug must stay visible at
the boundary that validates it (daemon-side, block 2) rather than being
laundered by the serialiser, which would also make that validation untestable.
The catalog has no such hole to leave open — it cannot be built dirty at all —
and the asymmetry is deliberate: an advertised id becomes a REQUESTED id, a
configuration id and a map lookup downstream, a blast radius longer than one
cell of one entry.

Both the caps and the id shape travel in the published JSON Schema (ADR 0038),
generated from the same constants the code applies: the artifact is the only
thing a third-party implementer reads, and an open `object`/`array` there would
tell them that 100 arbitrary ids are legal. That covers all four places an id
or a label appears — the `Entry.attrs` keys, `AttrInfo::id`, `AttrInfo::label`,
and the two requested-id lists — not just the map.

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

`AttrInfo::label` is enforced at decode too, but by CLAMPING (on a char
boundary) rather than dropping: the id is what a client acts on, and losing an
attribute over a cosmetic field would be the wrong trade.

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
  today's bytes. The N/N-1 window moves to N=0.30.x / N-1=0.29.x. The direction
  that has to hold is a **0.29 client against a 0.30 daemon**: it sends no
  `attrs`, receives none, and nothing changes for it. The reverse — a 0.30
  client against a 0.29 daemon — is not an attribute question at all:
  `version_compatible` rejects a client from the future outright, so that
  handshake is `VERSION_MISMATCH` before any field is looked at.
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
