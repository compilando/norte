# Writing archives: pack, unpack, test, split and join — design

> Issue **#132**, the last of the K2b capability gaps and the largest. Four
> keyboard presets bind five commands norte does not have: `pane.pack`,
> `pane.unpack`, `pane.test-archive`, `pane.split-file` and
> `pane.combine-files`, all `Planned` in the catalogue under
> `keymap-reason-archive-write`.

## The one fact that shapes everything

`norte-vfs-archive` is **READ_ONLY by design** (ADR 0018), and that is not the
obstacle it looks like. Of the five commands, exactly **one** wants to mutate
the inside of a container, and it is not on the list: nobody here is adding a
file to an existing zip. What the five actually ask for is:

| command | what it really is |
| --- | --- |
| pack | read a set of entries through their provider, **write one new file** |
| unpack | read entries through the archive provider, **copy them out** |
| test | read every entry to the end and check what the format promises |
| split | read one file, write N new files |
| join | read N files, write one new file |

Not one of them writes *inside* a container. So the archive provider stays
read-only, ADR 0018 stands untouched, and none of this is a provider
capability: it is **four core operations plus one that already exists**.

## Unpack already works, and that is the cheapest finding in this design

The copy engine takes an archive path as a SOURCE today — `engine_archive.rs`
only refuses the opposite direction, and refuses it in the destination's
`write`. So unpacking is `fs.copy` from `<container>#/` to the other panel's
directory: the existing engine, the existing journal entries, the existing
undo, the existing conflict policy, the existing cancellation.

**`pane.unpack` therefore adds no wire method.** It is a TUI command that
resolves the entry under the cursor to its interior root and issues the copy
the user could have issued by hand. Everything that already works for copy —
`.norte-partial` on cancel, per-entry conflict resolution, the journal — works
for unpack because it *is* copy.

The one thing it must not do is pretend: unpacking into a read-only panel, or
of an entry that is not a container, is refused with the reason, not with a
generic failure.

## Three new methods, and why each cannot be a client loop

Proto **0.50.0**, additive: three methods, three params types, two result
types. The bump is minor; every addition is a new method, no existing shape
changes.

### `archive.pack`

```
ArchivePackParams { sources: Vec<VPath>, dest: VPath, format: ArchiveFormat,
                    level: Option<u8>, base: VPath }
```

A **Task** (rule 3), cancellable, journalled (rule 4) as one `created` entry
for `dest` with `Reversal::Delete` — packing makes exactly one node, and
deleting it is a complete undo.

`format` is **explicit on the wire and never inferred there**. The TUI derives
the default from the name the user typed and shows which format it will write;
what travels is the decision, not a filename to guess from. `base` is the
directory the stored names are relative to — without it, "pack these three
marks" has no defined name for each entry, and a client picking one silently
would give two clients two different archives from the same request.

Formats: `zip` (deflate, `store` for empty entries), `tar`, `tar.gz`. **Not
rar** — it is delegated and read-only (ADR 0056) — and not 7z, which has no
reader here either.

**Names inside the archive are bytes** (rule 1). ZIP's general-purpose bit 11
is set when, and only when, the stored name is valid UTF-8; a name that is not
gets its raw bytes with the bit clear. Note what that bit is and is not for
here: `zip_cd.rs` keeps `name_raw` verbatim and never decodes by the flag, so
our own round-trip is byte-exact either way. The bit is written for **other**
tools, which do decode by it, and setting it on a non-UTF-8 name would be a
lie that turns the user's filename into replacement characters in every unzip
on the planet. The hostile corpus is the test, in both directions.

Cancellation leaves **no unmarked partial**: the destination is written to
`<name>.norte-partial` and renamed on completion, the same contract the copy
engine gives.

### `archive.test`

```
ArchiveTestParams { path: VPath }
ArchiveTestResult { entries: u64, failed: Vec<ArchiveTestFailure>, truncated: bool }
```

A cancellable Task that reads **every entry to its end** and checks what the
format actually promises: the CRC-32 in a zip's local header/data descriptor,
the gzip trailer's CRC and ISIZE for `tar.gz`, and for plain `tar` that every
declared size is reachable — a tar has no checksum for content, and saying
"ok" without one would be a claim the format cannot support. That distinction
travels: a per-format `checked` field says what was verified, so "passed" never
means more than it can.

Writes nothing, so no journal entry. Read gate only.

Failures are a **bounded list** with a `truncated` flag, for the same reason
compare streams rows: an archive where every entry is corrupt must not cost the
client a gigabyte of report.

### `file.split` and `file.combine`

```
FileSplitParams   { path: VPath, part_bytes: u64, dest_dir: VPath }
FileCombineParams { first: VPath, dest: VPath }
```

Both are Tasks, both journalled as `created` per node written with
`Reversal::Delete`.

Split writes `name.001`, `name.002`, … beside each other in `dest_dir` — the
Total Commander convention, which is what the users of these keys have. A part
count over 999 is refused **before writing anything** rather than discovered at
part 1000, because a naming scheme that runs out mid-operation leaves a set
nobody can join.

Combine takes the FIRST part and finds the rest by that convention. It refuses
a gap (`.003` missing) rather than joining across it, and refuses when a part
other than the last is a different size from the first — a silently
short-joined file is a corrupt file that looks fine.

`.crc` companion files are **not written and not required**. TC writes one;
reading it would be nice and is not what these keys are for. Noted as follow-up
rather than smuggled in.

## What the TUI adds

Five commands move from `Planned` to `live` in the shared catalogue, which is
what makes the four presets stop saying "not built".

- **pack** (`Alt+F5` TC, `Alt+Shift+P` Krusader, `Shift+F1` Far): a dialog with
  the destination name (defaulted from the source directory), the format
  derived from the name, and the level. Over the marks, or the entry under the
  cursor when there are none — the same rule every other bulk gesture here
  follows.
- **unpack** (`Alt+F6`, `Alt+Shift+U`, `Shift+F2`): confirm the destination —
  the other panel — and issue the copy.
- **test** (`Alt+Shift+F9`, `Alt+Shift+E`, `Shift+F3`): a progress task and a
  result dialog listing what failed.
- **split** (`Ctrl+P` Krusader): part size, with the usual sizes offered.
- **join**: over the `.001` under the cursor.

All five refuse a read-only or remote-without-write panel with the reason, and
all five are `Actor::User` gestures — nothing here is reachable by an agent in
this pass, because an agent that can write an archive can write a file, and
that decision belongs to the policy surface, not to this issue.

## What is deliberately not here

- **Adding to, or updating, an existing archive.** That is the one thing that
  would need a writable archive provider and a new ADR. No preset key asks for
  it.
- **GUI and CLI surfaces.** #132 is five keyboard commands; the GUI has its own
  gap list. Follow-up.
- **rar and 7z packing.** Delegation is read-only by design.
- **Encrypted archives** in either direction.

## Tests that decide whether this is done

1. A hostile-corpus name (the invalid-UTF-8 ones) survives pack → read-back
   through the existing archive provider, byte-exact, in zip and in tar.
2. A zip written here is readable by `zip_cd.rs` — the reader in this repo is
   the round-trip oracle, and bit 11 is asserted both ways.
3. Cancelling a pack of a big tree leaves no unmarked file in the destination.
4. `archive.test` fails a zip whose CRC was flipped by one byte, and says which
   entry.
5. `archive.test` on a plain tar reports what it could NOT check.
6. Split of a file that is an exact multiple of the part size writes no empty
   trailing part; join of that set is byte-identical to the original.
7. Join refuses a gap and refuses a short middle part.
8. Every mutation lands in the journal and `undo` removes exactly what it made.
