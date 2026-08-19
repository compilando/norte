# 0060 - Writing an archive is not writing into one

- Status: accepted
- Date: 2026-08-19
- Decision makers: Oscar González
- Related: ADR 0018 (archives as virtual directories, read-only), ADR 0056 (rar
  by delegation), ADR 0028 (composed schemes), the design
  (`docs/superpowers/specs/2026-08-19-archive-write-design.md`), protocol
  0.50.0, issue #132, hard rules 1, 3 and 4.

## Context and problem statement

Four keyboard presets bind five commands norte did not have: pack, unpack,
test, split and join. They sat in the shared catalogue as `Planned` under
#132 — the last such family, and the largest.

The obvious reading is that they need a writable archive provider, and that
ADR 0018 (`norte-vfs-archive` is `READ_ONLY`) is in the way. That reading is
wrong, and getting it right is the whole decision.

Of the five, exactly **one** would mutate the inside of a container — adding a
file to an existing zip — and no preset key asks for it. What the five actually
ask for is: read a set of entries and write **one new file**; read entries out
of a container and write them out; read every entry and check it; read one file
and write N; read N and write one. Not one of them writes inside a container.

## Decision

**The archive provider stays read-only. Writing archives is four core
operations, and a fifth that already existed.**

### 1. The format writers live in `norte-vfs-archive` and know no provider

`write::ArchiveWriter` is a pure encoder: metadata and bytes in, archive bytes
out, `begin` → `data`* → `end` per entry and `take` whenever the caller wants
what has been produced. It has no `Provider`, no I/O and no async.

It lives in the archive crate because that is where the format knowledge is —
the zip central directory parser is ten metres away, and it is the reader that
validates what the writer emits. It is incremental because the bytes arrive in
chunks from an async provider and the destination may be remote: neither the
archive nor a single entry is ever held whole in memory.

### 2. Unpacking adds no method, because the copy engine already does it

The copy engine takes an archive interior as a SOURCE today; only the
destination side refuses. So `pane.unpack` issues `fs.copy` from
`<container>/!/`, and inherits the journal, the undo, the collision policy and
the cancellation that copying already has. A second implementation would have
been a second set of those, differing in the details nobody tests.

### 3. The format travels explicit on the wire

`archive.pack` carries an `ArchiveFormat`, and the server never infers it from
the destination name. The frontend derives the default from what the user
typed and **shows it before they confirm**. Inferring server-side would be
deciding for them silently, and two clients with two heuristics would produce
two different archives from the same request.

The same params carry a `base`: the directory the stored names are relative to.
Without it, "pack these three marks" has no defined name per entry.

### 4. ZIP's bit 11 is written for other people's tools, and it tells the truth

Our own reader keeps `name_raw` verbatim and never decodes by the flag, so the
round trip is byte-exact either way. Bit 11 is set when, and only when, the
stored name is valid UTF-8, because every other unzip in the world decodes by
it — setting it on a name that is not UTF-8 turns the user's filename into
replacement characters everywhere else (hard rule 1).

### 5. "It passed" says what was checked

`archive.test` returns `checked`: `crc` for a zip, `gzip-crc` for a `tar.gz`,
`sizes` for a plain tar. A tar carries no content checksum at all, and
answering a bare "passed" over one would claim more than the format can
support. The verification itself is the reader's — reading a zip entry whole
already verifies its CRC — so the operation walks and collects rather than
holding a second opinion about the same integrity.

### 6. Every mutation is a Task, journalled, and leaves nothing half-made

Pack, split and join are cancellable Tasks (rule 3) and journal one `Created`
per node with `Reversal::Delete` (rule 4). The destination is written through a
`ByteSink`, so a cancelled pack leaves the destination **clean** — an archive
that is half-written still looks like an archive, which is worse than no file
at all. The journal entry is emitted after the commit, when the node exists.

Refusals happen before anything is written: a source that does not hang from
`base`, a destination that already exists, a split that would need more than
999 pieces, a join with a gap or with a short middle piece.

## Consequences

- ADR 0018 is untouched: `norte-vfs-archive` answers `Unsupported` to every
  mutation, and no path in this work asks it for one.
- **rar and 7z are not writable**, and the dialog says so rather than writing a
  zip with a rar name. Delegation is read-only by design (ADR 0056).
- Protocol 0.50.0 is additive: four new methods, one report method, and four
  `TaskKind`s. Old clients do not form them; a new client against an old daemon
  gets `Unsupported`, not a generic failure.
- **The catalogue has no `Planned` commands left.** Every command a preset
  names is one norte has. The machinery that dims a promised capability stays —
  the constructor, the reason, the issue number — because the next promise will
  need it, and several tests that used a `Planned` command as their example of
  "unavailable" now use the other kind: a live command this frontend does not
  implement.
- Symlinks are omitted from a pack with a warning, the same way the reader
  omits what it cannot represent. Storing the target would copy the pointed-to
  file without saying so; storing the link needs an entry kind the writer does
  not have yet.
- An entry whose name would be the `!` marker is refused: the reader's index
  omits that component (ADR 0018), so writing it would produce an entry norte
  cannot address afterwards.
