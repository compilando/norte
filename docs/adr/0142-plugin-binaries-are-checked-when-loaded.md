# 0142 — Plugin binaries are checked when they are loaded

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. WIT: unchanged.
- Related: #241 and #282 (consent is manifest plus binary digest), ADR 0141
  (the compile cache, whose review found this), ADR 0033 (embedded provider)

## Context and problem statement

Consenting to a plugin covers its manifest and the SHA-256 of its
`plugin.wasm`, taken when the catalogue discovers it. Providers compared
the bytes they read against that digest before instantiating them. Every
other kind — previewers, thumbnailers, commands, decorators, panels,
renamers, organizers, columns, hooks — got a path back from the registry
and loaded whatever was at that path when it ran. The daemon discovers its
catalogue once and keeps it, so between one discovery and the next,
anything able to write a plugin's `plugin.wasm` ran its code with the
capabilities the human had approved for the old one, network included. The
rustdoc of two resolvers even claimed the check was made.

## Decision

**What is loaded is what was approved, checked on the bytes that run.** The
runtime no longer instantiates from a path. It takes a `WasmArtifact` — the
path together with the approved digest — reads the file (bounded by the
artifact cap), hashes it, and refuses with `RuntimeError::DigestMismatch`
if the hash is not the approved one. The check is made on the same bytes
that are compiled, before the compile cache is consulted, so there is no
gap between checking and loading, and a cached compilation of the approved
version cannot serve a changed file.

The registry builds the artifact from the catalogue entry
(`verified_wasm`); an entry without a digest — a binary that could not be
read at discovery — has no artifact, which is the same as having no binary.
The column pool and the hook dispatcher, which keep live instances, compare
the whole artifact, digest included.

For a provider the mismatch maps to `PermissionDenied`, the same answer the
provider path already gave when it caught it itself.

`WasmArtifact::trusting_current` hashes whatever is at a path now. It exists
for tests and for code that is the authority of the file it names; no
production path uses it for a third party's plugin.

## Consequences

- Replacing a plugin's binary after approval makes it stop working until it
  is approved again, instead of running unasked.
- Loading a plugin costs one SHA-256 of its binary per call — milliseconds
  for a few megabytes, and computed once for both the check and the cache.
- The check is at load. An instance already built from the approved bytes —
  a live hook, a column pool entry — keeps running that code after the file
  changes; the next load of the new bytes is what is refused.
- Providers now go through the same check when they connect, which also
  bounds that read by the artifact cap; it used to read the whole file
  before comparing.
