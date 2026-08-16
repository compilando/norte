# RAR, read-only, by delegation: design

> Roadmap post-alpha item **11**; product decision 5 of the specification
> (`docs/spec/norte-spec.md:339`). Read-only RAR through an installed `unrar`
> or `7z`, with no non-free code in the dependency graph. The delegate is an
> external program, so hard rule 9 decides the shape: it gets one archive path,
> one entry name and a pipe, never the user's filesystem.

## Why this is not just another archive format

`norte-vfs-archive` parses ZIP and TAR in-process, over an `Arc<dyn Provider>`,
reading through the inner provider's ranged-read API (ADR 0018). Nothing in
that shape survives contact with RAR:

| ADR 0018 assumes | RAR by delegation gives |
| --- | --- |
| an in-process parser | a child process |
| bytes from any inner provider | a **local path**, or nothing |
| ranged reads at any offset | one forward stream per invocation |
| names read from a byte structure | names printed by a program, in some encoding |

So this is a different provider with a different constructor, not a fourth
`Format` in the existing one. What it *does* share is the addressing (`ADR
0018`'s compound scheme and `!` marker), the index cache, the read-only
capability set, and the rule that an unsafe entry is skipped-and-counted rather
than fatal.

## Scope

**In:** listing a `.rar` as a directory tree, reading entry contents,
`rar+file://…/!/…` addressing, honest failure when no delegate is installed,
honest failure for an encrypted entry, cancellation that kills the child.

**Deliberately out:**

- **Writing.** `READ_ONLY`, every mutation returns `Unsupported`, same as ADR
  0018.
- **Non-local inner providers.** `rar+sftp://…` is refused with a reason. The
  delegate needs a real path; spilling a remote archive to a temporary file is
  a download the user did not ask for. Debt issue, not v1.
- **Nesting.** `rar` inside another archive layer is not local either, so the
  same refusal covers it. No `zip+rar+file://`.
- **Password prompts.** An encrypted entry is listed and refuses to read. norte
  never asks the delegate to ask the user.
- **Bundling a delegate.** If `unrar` is not installed, the answer is a sentence
  naming it, not a vendored binary.

## Approaches considered

**A fourth `Format` inside `norte-vfs-archive`** — rejected. That crate's
contract is "never opens the local filesystem directly" and "reads go through
the inner provider". A `Command` in it would falsify both sentences for every
existing format's reader.

**A WASM plugin provider** (the `norte:provider` world exists, ADR 0032/0033) —
rejected, and it is the interesting rejection: `exec` is permanently `none` for
plugins (`crates/norte-plugin-host/src/capability.rs:3`), so a sandboxed guest
cannot spawn `unrar` by construction. Delegation to an external program is a
*core* privilege precisely because the sandbox forbids it.

**A dedicated crate that takes a path** — chosen. `norte-vfs-rar` never sees an
`Arc<dyn Provider>`, so it cannot know about other providers; the "local only"
rule lives where the decision is actually made, in the engine's dispatch.

## Architecture

### Crate

`crates/norte-vfs-rar`, `MIT OR Apache-2.0` like its siblings, `unsafe`
forbidden, scaffolded with the `new-crate` skill and wired into the read-only
provider contract suite as ADR 0018 requires of an archive provider.

```rust
pub struct RarProvider { /* archive path, delegate, index cache, limits */ }
impl RarProvider {
    pub fn new(archive: PathBuf, delegate: Delegate, limits: RarLimits) -> Self;
}
```

The constructor takes an `OsString`-shaped local path. There is no inner
provider, no scheme composition inside the crate, and therefore no way for it
to reach a remote byte.

### Dispatch, and where "local only" is enforced

`Engine::provider_for` (`crates/norte-core/src/engine.rs:690`) already splits a
compound scheme and matches the format token. `rar` joins that match:

```rust
"rar" => {
    // The delegate needs a real path. Anything else is refused HERE,
    // before a provider is composed, with a reason a human can act on.
    let Some(path) = local_os_path(&aref.outer) else {
        return Err(Error::Unsupported);
    };
    ...
}
```

`aref.outer` must have scheme `file` and no authority. An outer that is itself
an archive layer fails the same test. The nesting cap that already runs above
this point (`#56`) is unaffected.

`ARCHIVE_FORMATS` in `crates/norte-proto/src/vpath.rs` gains `"rar"`, which is
the wire change: the whitelist decides which schemes a client may form. The
longest-match grammar is untouched — `rar` contains no `+`.

### The delegate

```rust
pub enum Delegate { SevenZip(PathBuf), Unrar(PathBuf) }
```

Discovery probes `PATH` once and caches the result for the process: **`7z`
first, then `7zz`, then `unrar`**. That order is measured, not taste — see the
table below: `unrar`'s text output cannot carry a name that is not valid UTF-8,
and `7z`'s can.

Configuration (`[archive] rar_delegate = "auto" | "<absolute path>"`) can pin
one; a pinned path that does not exist is an error at use, named.

**This key is user-layer only, never project.** `[archive]` already carries that
rule for its limits (`crates/norte-config/src/load.rs:1365`, "never from
Project") and here it is sharper: a key naming an executable, honoured from a
`.norte.toml` inside a repository, is arbitrary code execution on `cd`. The
loader must refuse it from the project layer the same way it refuses a raised
limit.

Absent delegate is not a panic and not a silent empty listing: `Unsupported`
whose message names what to install. That sentence is the feature — a `.rar`
that opens to nothing teaches the user nothing.

### Rule 9, concretely

Every invocation, both for listing and for reading:

- absolute executable path, no shell, no `sh -c`;
- arguments terminated by `--` before any archive path or entry name;
- `stdin` = `null`, plus `-p` with an empty password (7z) / `-p-` (unrar), so an
  encrypted archive **cannot** block the daemon waiting on a password;
- `-bd -y` (7z) / `-inul` (unrar) to keep the tool's own messages and prompts off
  the data stream;
- `cwd` = an empty directory under the state directory, never the user's tree,
  so a delegate that decides to write relative paths writes nowhere interesting;
- a minimal environment;
- `kill_on_drop(true)`, a wall-clock timeout, and a semaphore bounding
  concurrent children;
- cancellation through the task's `CancellationToken` kills the child (hard rule
  3 — the clean-cancellation test is part of the work).

### Listing

`7z l -slt -p -- <archive>` (or `unrar vt -p- -- <archive>`), **stdout read as
bytes**. The `-slt` output starts with a header block describing the archive
itself — the first `Path =` line is the `.rar` file, not an entry — so the parse
begins after the separator line. Never `String`, never `to_str()`: hard rule 1 does not stop being true
because the bytes arrived through a pipe.

The parse yields per entry: raw name bytes, size, mtime, directory flag,
encrypted flag, solid flag. The index is cached per (path, mtime, size), the
same invalidation ADR 0018 uses, with the same bounds on entry count, name
length and depth, and the same treatment of structurally unsafe names
(absolute, `..`, NUL, `!`, empty component): **skipped with a warning and
counted**, never fatal for the whole archive.

One RAR-specific skip: a name containing `\n` or `\r` cannot be recovered from a
line-oriented listing without guessing. Those entries are skipped and counted
too. Guessing here would mean showing the user a file that is not the file.

### The wildcard trap, and why it does not need a guess

`unrar p <archive> <name>` treats `<name>` as a **pattern**: an entry literally
called `report*.txt` would extract every `report….txt` in the archive, and the
stream would look perfectly healthy. 7z globs too. Neither has a "this is a
literal name" switch.

The fix uses what we already hold. The full index is in memory, so at read time
the requested name is matched **as a glob against our own index**: if it selects
more than one entry, the read is refused (`Unsupported`, "the name is ambiguous
for the delegate"). Exact, cheap, and decided before a process starts — as
opposed to inspecting the output stream and hoping to notice.

### Reads

`7z e -so -bd -y -p -- <archive> <name>` (or `unrar p -inul -p- -- <archive>
<name>`) streams one entry to stdout. A ranged
read skips the prefix and stops early, killing the child; a backwards seek is a
new process. Sequential readers therefore cost one process, random access costs
one per jump, and that asymmetry is documented rather than hidden.

Solid archives decompress every entry preceding the requested one. Listing is
unaffected; a read of a late entry in a solid archive is slow by construction.
v1 pays it and says so; a "the archive is solid, reads are sequential" note in
the index is what a later optimisation would hang from.

### Errors

| situation | answer |
| --- | --- |
| no delegate installed | `Unsupported`, message names `unrar`/`7z` |
| encrypted entry | listed; read returns `Unsupported`, "encrypted" |
| corrupt archive / limit exceeded | non-retryable I/O error (ADR 0018) |
| ambiguous name (glob) | `Unsupported`, "ambiguous for the delegate" |
| delegate exits non-zero | non-retryable I/O error carrying its stderr, truncated |
| non-local inner | `Unsupported`, refused in the engine before composition |

Capabilities advertised: `READ_ONLY`, `CASE_SENSITIVE`, `CASE_PRESERVING`.

## Wire

`ARCHIVE_FORMATS` gains `"rar"` → protocol minor bump, golden tests updated,
`protocol-guardian` review mandatory (a scheme whitelist is exactly the surface
where an older core and a newer proto meet: the existing `_ =>` arm in
`provider_for` already answers "my proto knows this format and I do not" with
`Unsupported`, honestly).

## Configuration

```toml
[archive]
rar_delegate = "auto"     # or an absolute path
```

Nothing else. Concurrency and timeout bounds live with the other archive limits
already in the engine.

## What was measured

The delegates are installed on the development machine, and the questions this
design refused to answer from memory were answered with a real archive before
the plan was written. A minimal RAR5 writer that stores entries uncompressed
(the container format is documented; the *compression* is the proprietary half,
and storing raw bytes does not touch it) produced a fixture with a UTF-8 name, a
name holding the invalid bytes `\xa4\xa5`, and the pair `star*name.txt` /
`starXname.txt`.

| question | `unrar` 7.23 | `7z` |
| --- | --- | --- |
| non-UTF-8 name in the listing | **truncated at the first invalid byte** — `cp437-\xa4\xa5.txt` prints as `cp437-`, extension and all lost | **raw bytes preserved** in `l -slt`: `cp437-\244\245.txt` |
| non-UTF-8 name as an argument | unusable — the name it printed is not the name | accepted verbatim; `e -so` returns that entry's content |
| entry name treated as a pattern | **yes** — `star?name.txt` extracts two entries | **yes** — same |
| single entry to stdout | `p -inul` | `e -so` |

Two consequences, both now in this design:

1. **`7z` is the preferred delegate and `unrar` is the fallback**, which is the
   reverse of what this spec said before the measurement. Under `unrar`, an
   entry whose name is not valid UTF-8 is skipped-and-counted, because the name
   it prints is not a name that can be asked for again. Under `7z` it is a
   normal entry.
2. **The wildcard trap is confirmed on both**, so the glob-against-our-own-index
   refusal is not a precaution against a hypothetical; it is required.

What is still open is only the RAR4 side: these fixtures are RAR5, where names
are UTF-8 by format. A RAR4 archive with an OEM-code-page name is what a decade
of downloads actually contains, and no writer here can produce one. The plan
carries it as an explicit gap rather than a claim.

## Tests

- **Fixtures are generated, not committed.** `norte-testkit` gains a minimal
  RAR5 writer for **stored** entries — a documented container around raw bytes,
  never the proprietary compressor — so the hostile corpus reaches RAR the same
  way it reaches ZIP and TAR, and the canonical tree the read-only contract
  needs can be seeded at all. Prototyped and verified against the real `unrar`
  and `7z` before this plan.
- **What the writer cannot make** is a solid archive and an encrypted one. Solid
  and encrypted flags are covered by parser unit tests over recorded delegate
  output; "an encrypted entry never blocks on stdin" is covered by a stub
  delegate that would hang if stdin were open.
- **Read-only provider contract** (ADR 0018), the one that asserts every
  mutation returns `Unsupported`.
- **Cancellation**: a read of a large entry is cancelled and the child is gone.
- **The wildcard refusal**, with an entry named `a*b` next to `axb`.
- **No delegate**: the error names the executable.
- **Encrypted**: listing shows it, read refuses, and nothing ever waits on
  stdin — asserted with a timeout, because "does not hang" is the property.
- Tests needing a delegate **retire with a message** when none is installed,
  the same convention as the two-device trash test.

## ADR

New ADR: *a provider may delegate to an external program*. It records the rule-9
boundary (path + entry + pipe, empty cwd, closed stdin, no shell), why the
sandbox cannot host this (plugins have `exec = none` permanently), why the local
restriction is enforced in the engine rather than in the crate, and the
skipped-and-counted rule extended to names a line-oriented listing cannot carry.

## Definition of done

`norte-vfs-rar` with the contract suite green, engine dispatch with the local-only
refusal, proto whitelist bumped with goldens, config knob documented, fixtures in
the corpus, ADR written, changelog entry, `just ci` green.
