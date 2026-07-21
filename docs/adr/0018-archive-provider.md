# 0018 - Read-only archives as virtual directories

- Status: accepted
- Date: 2026-07-13
- Decision makers: Oscar González
- Related: specification sections 5, 6.1, and 14; ADRs 0001, 0005, and 0016

## Context

M2 needs read-only ZIP and TAR navigation. An archive lives inside another VFS
provider, so its paths must remain stable without daemon mount state, reads must
go through the inner provider, hostile entry names must not enable traversal,
and indexing/decompression must be bounded.

## Decision

### Addressing

Use a compound scheme and `!` path segment:

```text
zip+file:///home/user/archive.zip/!/docs/readme.txt
tar+sftp://user@host/path/archive.tar/!/src/lib.rs
```

`VPath::archive_compose` and `archive_split` operate on parsed segments. A
scheme is compound only when its longest registered archive-format prefix
matches; legitimate schemes containing `+` remain ordinary. Split at the first
literal `!`. Reject composition when either side already contains a `!`, reject
a compound scheme without the marker, and reject nested archive formats in v1.
Never split the wire string directly because `%21` is equivalent to `!`.

This representation is stateless, survives restarts, and reserves a compatible
path for future right-to-left nesting.

### Provider composition

Construct `ArchiveProvider` with an `Arc<dyn Provider>` for the outer path.
Perform all reads through the inner provider's ranged-read API. Run synchronous
ZIP/TAR parsers in `spawn_blocking` over an adapter. The archive provider never
opens the local filesystem directly.

### Entry names

Preserve raw entry bytes. Reject structurally invalid or unsafe names: absolute
paths, empty components, `.`, `..`, NUL, `!`, excessive depth, or excessive
length. Skip unsafe entries with a warning and count them, rather than making an
otherwise useful archive unreadable. For duplicates, the last ZIP entry wins;
for file/directory ambiguity, the directory wins. Warn in both cases.

### Limits and formats

Bound the index to 500,000 entries, names to 4,096 bytes, and depth to 64.
Return a non-retryable I/O error for limit violations or corruption. v1 supports
plain TAR and stored/deflated ZIP. Encrypted or unsupported ZIP entries remain
visible but return `Unsupported` when read. ADR 0028 later adds TAR.GZ; 7z,
RAR, nesting, and writes remain separate work.

### Read-only capability

Add `READ_ONLY` and bump the protocol from 0.8.0 to 0.9.0 so capable clients can
disable mutations before a round trip. Older clients ignore the flag and safely
receive `Unsupported`.

## Implementation details

- Put the implementation in permissively licensed `norte-vfs-archive` with
  unsafe code forbidden and no concrete-provider dependencies.
- Cache at most eight indexes per provider, keyed by canonical outer wire path
  and invalidated by outer mtime and size. Treat a missing mtime as always stale.
- Advertise `READ_ONLY`, `CASE_SENSITIVE`, and `CASE_PRESERVING`.
- Add a dedicated read-only provider contract that uses a pre-seeded tree and
  verifies every mutation returns `Unsupported`.
- Plain TAR reads use contiguous ranged passthrough. ZIP reads use a blocking
  decoder and bounded channel; dropping the stream cancels production.
- List TAR symlinks as symlinks, return their raw target through `read_link`,
  and reject content reads with `TypeMismatch`.

## Consequences

Archive bookmarks work across sessions and any outer provider—local, SFTP, S3,
or memory—without changing VPath's grammar. Raw bytes avoid CP437/UTF-8
round-trip corruption and the full portable contract runs over `MemProvider`.

The in-band `!` convention makes a real outer component named `!` unusable as a
container. Skipped hostile entries are not visible until frontends expose the
count. Remote ZIP range reads can be chatty, partly mitigated by index and block
caches. The `zip` and `tar` dependencies are preferred to security-sensitive
hand-written parsers.
