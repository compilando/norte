# 0005 - Provider contract expansion and copy policies

- Status: accepted
- Date: 2026-07-11
- Decision makers: Oscar González
- Related: M1 phase 2, issues #6, #7, and #8; ADR 0004

## Context

M2 adds remote SFTP and object-storage providers. Adding methods to `Provider`
breaks every implementation, so the contract should grow while only
`LocalProvider` and `MemProvider` exist. The copy engine also needs the collision,
symlink, and retry policies required by the specification before the TUI builds
interactive dialogs around them.

## Options considered

### Range reads

- Add `read_range()` beside `read()`. This avoids an immediate break but leaves
  two permanent APIs and hides unsupported range reads until production.
- Change `read` to `read(&self, path, range: Option<ByteRange>)`. This breaks the
  two current implementations at the cheapest point and uses `None` for a full
  file.

### Symlinks

- A creation-only `symlink()` API cannot preserve links because the source
  target cannot be read.
- `read_link()` plus `symlink()` can carry the target as raw bytes. A target may
  be relative, absolute, broken, or non-UTF-8, so neither `String` nor `VPath`
  is suitable. Windows additionally needs a file-or-directory hint.

### Collision handling

- Implementing `Ask` immediately requires task pausing, a request/response
  channel, timeouts, and multiplexing before a UI can use them.
- The complete enum can enter the wire format now while M1 treats `Ask` like
  `Fail`. Interactive per-file resolution can then arrive without another wire
  change.

### Overwrite implementation

- Atomic replacement through `WriteOpts` is the long-term design but requires
  provider-specific semantics that M2 will define more accurately.
- `remove()` followed by a normal write is non-atomic but produces separate,
  reversible journal mutations.

## Decision

- Change the trait to
  `read(&self, path: &VPath, range: Option<ByteRange>)`, where
  `ByteRange { offset: u64, len: Option<u64> }` and `len: None` means through
  EOF. Providers without range support return `Unsupported`.
- Add `read_link(&self, path) -> Result<Vec<u8>>` and
  `symlink(&self, link, target: &[u8], kind: SymlinkKind)`, where
  `SymlinkKind` is `File` or `Dir`. Providers without symlinks return
  `Unsupported` and omit the `SYMLINKS` capability.
- Add `CollisionPolicy { Fail, Ask, Skip, Overwrite, RenameAuto, Newer }` to
  optional `fs.copy` and `fs.move` parameters, defaulting to `Fail`.
  - `Fail` and M1's initial `Ask` return `Conflict`.
  - `Skip` records progress and completes without copying the conflicting item.
  - `Overwrite` removes then writes, except for file/directory type mismatches.
  - `RenameAuto` inserts ` (n)` before the last extension, trying byte-safe
    values from 1 through 1000 before returning `Conflict`.
  - `Newer` overwrites only when the source mtime is greater. Missing or
    incomparable mtimes return `Conflict` rather than guessing.
- Add `SymlinkPolicy { Follow, Preserve, Skip }`, defaulting to `Preserve`.
  - `Preserve` copies the raw link target and requires `SYMLINKS` at the
    destination.
  - `Skip` counts and omits symlinks.
  - `Follow` copies file-link content. Directory links remain `Unsupported` in
    M1 until traversal tracks filesystem identity to prevent cycles.
- Retry provider operations on retryable `ProviderUnavailable` and `Io` errors
  up to three times, using deterministic exponential backoff from 100 ms and
  checking cancellation during the wait. M1 restarts a partial file from the
  beginning; offset resume arrives in M2. Other errors are not retried.
- Add `ConflictKind::Normalization` for byte-distinct names whose NFC forms
  collide, plus a receive-only `Unknown` fallback for future variants.
- Add the `APPEND` and `RANDOM_WRITE` capabilities. `LocalProvider` advertises
  both.
- Bump `PROTOCOL_VERSION` from 0.1.0 to 0.2.0 and add golden fixtures. Version
  0.2.0 becomes the starting point for the N/N-1 compatibility guarantee;
  there were no deployed remote 0.1 clients.

## Consequences

- M2 providers implement a stable contract, and the viewer and resume engine
  have range reads. The TUI can later add interactive `Ask` without changing
  the wire format.
- Journalled overwrite consists of reversible `Removed` and `Created` entries.
- The range signature change touches both initial providers and their tests.
- Overwrite is non-atomic until provider-specific atomic replacement is added.
- Following directory symlinks remains unsupported until cycle tracking exists.
- `RenameAuto` may perform 1,000 stats and fails loudly at `NAME_MAX` rather
  than truncating a multibyte name unsafely.
- Self-overwrite detection is conservative until providers expose stable file
  identity such as `(dev, ino)` or `FileId`.
- Only idempotent reads and full-file restarts are retried. Retrying an ambiguous
  remote mutation could duplicate effects or corrupt the journal.
