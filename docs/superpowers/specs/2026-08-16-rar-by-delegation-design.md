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
pub enum Delegate { Unrar(PathBuf), SevenZip(PathBuf) }
```

Discovery probes `PATH` once and caches the result for the process:
`unrar` first, then `7z`, then `7zz`. Configuration
(`[archive] rar_delegate = "auto" | "<absolute path>"`) can pin one; a pinned
path that does not exist is an error at use, named.

Absent delegate is not a panic and not a silent empty listing: `Unsupported`
whose message names what to install. That sentence is the feature — a `.rar`
that opens to nothing teaches the user nothing.

### Rule 9, concretely

Every invocation, both for listing and for reading:

- absolute executable path, no shell, no `sh -c`;
- arguments terminated by `--` before any archive path or entry name;
- `stdin` = `null`, plus `-p-` (unrar) / `-p` with an empty password (7z), so an
  encrypted archive **cannot** block the daemon waiting on a password;
- `-inul` (unrar) to keep the tool's own messages off the data stream;
- `cwd` = an empty directory under the state directory, never the user's tree,
  so a delegate that decides to write relative paths writes nowhere interesting;
- a minimal environment;
- `kill_on_drop(true)`, a wall-clock timeout, and a semaphore bounding
  concurrent children;
- cancellation through the task's `CancellationToken` kills the child (hard rule
  3 — the clean-cancellation test is part of the work).

### Listing

`unrar vt -p- -- <archive>` (or `7z l -slt -p -- <archive>`), **stdout read as
bytes**. Never `String`, never `to_str()`: hard rule 1 does not stop being true
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

`unrar p -inul -p- -- <archive> <name>` streams one entry to stdout. A ranged
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

## What has to be measured, not decided

**The encoding of the names the delegate prints.** RAR stores names in UTF-16
or in an OEM code page depending on how they were written, and what `unrar`
prints depends on that *and* on the child's locale. This design refuses to
state the contract from memory. The plan's first task builds hostile fixtures
(a UTF-8 name, a CP437 name, a name with an invalid byte) and records what
comes back under `LANG=C` and under a UTF-8 locale; the answer decides whether
the child gets a forced locale and whether any transcoding happens at all.
Whatever it is, the bytes reach `VPath` as bytes.

## Tests

- **Fixtures**, committed to the `norte-testkit` corpus (they cannot be
  generated — the compressor is the non-free half): a small archive with
  hostile names, one solid archive, one encrypted archive, one truncated file.
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
