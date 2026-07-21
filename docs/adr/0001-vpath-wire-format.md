# 0001 - VPath representation and wire format

- Status: accepted
- Date: 2026-07-08
- Decision makers: Oscar González

## Context

Filenames are not necessarily UTF-8. Linux permits arbitrary bytes except `/`
and NUL, Windows uses UTF-16 and may contain unpaired surrogates, and macOS
normalizes names to NFD. `VPath` must represent every valid name without loss,
travel over JSON-RPC (whose strings must be valid UTF-8), and round-trip the
original bytes exactly.

## Options considered

1. **Internal bytes with RFC 3986-style percent encoding on the wire**
   - Keeps the wire value as one readable `scheme://...` string.
   - Leaves ordinary UTF-8 paths readable in logs and fixtures.
   - Supports property testing of exact byte round trips.
   - Requires well-tested escaping rules, including literal `%` and malformed
     escapes.
2. **Internal bytes with tagged base64 for each segment**
   - Makes parsing straightforward.
   - Replaces a single path string with an array or object and makes logs and
     fixtures opaque.
3. **WTF-8 directly in a JSON string**
   - Not viable. JSON requires valid UTF-8, so unpaired surrogates and arbitrary
     bytes do not survive `serde_json` without rejection or replacement.
   - A binary protocol such as MessagePack could carry it, but binary encoding
     is optional and cannot define the base representation.

## Decision

Use internal bytes and percent encoding on the wire.

- **Internal form:**
  `VPath { scheme, authority: Option<Authority>, segments: Vec<Segment> }`,
  where `Segment(Vec<u8>)` contains raw bytes. Unix stores operating-system
  bytes directly. Windows uses the WTF-8 form returned by
  `OsStr::as_encoded_bytes()`, preserving `OsString` surrogates.
- **Construction invariants:** a segment is non-empty, contains neither NUL nor
  `/`, and is not the literal `.` or `..`. Schemes match
  `[a-z][a-z0-9+.-]*`. Invalid input is rejected rather than silently cleaned.
- **Authority:** `Authority(String)` contains non-empty printable ASCII
  (`0x21..=0x7e`) excluding `/` and `%`. Since protocol 0.8.0, the userinfo
  portion before the last `@` also rejects `:` to prevent inline passwords such
  as `user:pass@host`. Colons remain valid in `host:port` and bracketed IPv6
  addresses. The authority is not percent encoded, so the first `/` after
  `://` unambiguously begins the path. Providers convert non-ASCII hostnames to
  punycode where needed.
- **Wire form:** one `scheme://authority/segment/...` string. Valid UTF-8
  sequences remain literal. Bytes outside valid sequences use uppercase `%XX`;
  a literal `%` becomes `%25`. C0 controls and DEL are escaped even when part
  of valid UTF-8, preventing terminal injection and log forging. Decoding a
  malformed escape is an error. Non-canonical escapes such as `%41` for `A` are
  accepted; the guaranteed round trip is bytes to wire to bytes.
- **Isolation:** the codec lives in `norte-proto/src/wire/vpath_codec.rs`. No
  other module implements these escaping rules.
- **Display:** `display_lossy()` uses the deliberately non-parseable form
  `⟨scheme authority⟩/segment/...`. Lossy UTF-8 displays `�` for undecodable
  bytes and control characters, so raw controls never reach a terminal.
- **Native conversion:** `VPath` to `PathBuf` conversion belongs to
  `norte-vfs-local`, not the protocol crate. This confines operating-system
  conditionals and `from_encoded_bytes_unchecked`. On Windows, bytes received
  over the network must be validated as WTF-8 before that unchecked function is
  called; its guarantee only applies to bytes originating from a local
  `OsString`.

## Consequences

- Clients and agents see readable paths in the common case without corrupting
  uncommon names. Golden and property tests stabilize the representation.
- A future MessagePack encoding can carry raw bytes without changing this
  design.
- Every producer and consumer must use the protocol codec. The core returns a
  typed error when a client constructs invalid escapes by concatenating strings.
- Literal `%` characters in real filenames are always escaped on the wire.
