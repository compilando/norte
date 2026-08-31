# 0056 - A provider may delegate to a program it does not trust

- Status: accepted
- Date: 2026-08-16
- Decision makers: Oscar González
- Related: roadmap post-alpha item 11 and its design
  (`docs/superpowers/specs/2026-08-16-rar-by-delegation-design.md`), ADR 0018
  (archives as directories: addressing, name safety, skipped-and-counted),
  ADR 0032/0033 (the WASM plugin sandbox), hard rule 9 (nothing reaches the
  filesystem except through the core), protocol 0.47.0.

## Context and problem statement

RAR is the one archive format norte cannot read with code it is allowed to
contain. The decompressor is proprietary; the only free implementations are
bindings to that same non-free source, and the licence forbids using it to
recreate the algorithm. So "add a crate" is not available, and a decade of
downloads is `.rar`.

What *is* available is that most machines already have a program that reads RAR:
`7z` (p7zip) or `unrar`. Using it means the format problem disappears and a
security problem takes its place: norte would be handing the user's paths to a
program it did not write, did not audit, and cannot patch.

Hard rule 9 says agents and plugins never touch the filesystem directly. A
delegate is neither, and that is the gap this ADR closes.

## Decision considered and rejected: a WASM plugin

The `norte:provider` world exists and would look like the natural home. It is
the interesting rejection: `exec` is permanently `none` for plugins
(`crates/norte-plugin-host/src/capability.rs:3`), so a sandboxed guest cannot
spawn `unrar` **by construction**. Delegating to an external program is a *core*
privilege precisely because the sandbox forbids it. Any design that puts this in
a plugin has to first punch a hole in the sandbox, which is a worse decision
than the one being made here.

## Decision

**A provider may delegate to an external program, and what it hands over is a
path, an entry name and a pipe — never the filesystem.**

`crates/norte-vfs-rar` holds a local **path**, not an inner `Provider`. It
therefore cannot reach a remote byte and cannot know about other providers. The
boundary has four parts:

1. **The child is boxed in.** Absolute executable, no shell; every argument
   after `--`; `stdin` closed to `null` and an empty password on the command
   line, so an encrypted archive can never block the daemon waiting on a prompt
   nobody will answer; `stderr` captured so the tool's chatter never
   contaminates the data stream; `cwd` an empty per-process directory that is
   never the user's tree; `env_clear`, so no credentials and no `LD_PRELOAD`;
   `kill_on_drop`, a wall-clock timeout that kills, and a semaphore bounding
   concurrent children to four. Cancelling the task's token kills the child
   (hard rule 3).

2. **"Local inner only" is enforced in the engine, not in the crate.**
   `Engine::provider_for` refuses `rar+sftp://…`, `rar+mem://…` and
   `rar+zip+file://…` with `Unsupported` *before* composing anything. The crate
   could not make that judgement — it does not know what is behind a scheme —
   and materialising a remote container to a temporary file would be a download
   nobody asked for.

3. **Names stay bytes, and a name that cannot survive is skipped and counted.**
   ADR 0018's rules apply verbatim (absolute, `.`, `..`, NUL, the `!` marker,
   over-long, over-deep). RAR adds one of its own: the delegate's listing is
   line-oriented, so a name containing `\n` or `\r` cannot be recovered without
   guessing — and guessing means showing the user a file that is not that file.

4. **A name that is a pattern is refused.** Measured: both delegates treat the
   entry name as a **glob**, and neither has a "this is literal" switch. An
   entry called `star?name.txt` extracts `starXname.txt` too, and the stream
   looks perfectly healthy. Since the whole index is already in memory, the
   ambiguity is decided against our own names before any process starts.

### Why `7z` outranks `unrar`

Measured on a real fixture (`unrar` 7.23, `7z` 26.02), and this is the reason
for the order, not taste:

| | listing a non-UTF-8 name | raw bytes as an argument |
| --- | --- | --- |
| `7z -slt` | `cp437-\244\245.txt` — intact | accepted |
| `unrar vt` | `cp437-` — **truncated at the first invalid byte** | accepted |

A provider that silently drops a file's extension is not acceptable while there
is an alternative. Discovery probes `7z`, then `7zz`, then `unrar`.

### The configuration key

`[archive] rar_delegate` pins an executable. It is honoured from the System and
User layers and **never from Project**: `[archive]` already carried that rule
for its anti-bomb limits, and here it is sharper — a key that names an
executable, read from a `.norte.toml` inside a repository, is arbitrary code
execution on `cd`.

### The wire

`rar` joins `ARCHIVE_FORMATS` (protocol 0.47.0). The whitelist decides which
composed schemes a client may form, so widening it is a wire change even though
no existing message moves a byte.

## Consequences

- A `.rar` opens as a directory wherever a delegate is installed; where none is,
  the answer is `Unsupported` with a message naming what to install. A `.rar`
  that opens to nothing would teach the user nothing.
- Read-only, and it stays that way: writing RAR needs the proprietary
  compressor.
- Encrypted entries are listed but not read — the password cannot be asked for,
  and pretending the file is empty would be worse than saying no.
- Reading is O(one child process per read) and a range is served by discarding
  from the pipe, because a pipe has no seek.
- **That gap is closed** (#223, 2026-08-31). It used to read: the fixtures are
  RAR5, where names are UTF-8 by format, and a RAR4 archive with an
  OEM-code-page name — what a decade of downloads actually contains — could not
  be produced here. It can: `RarSmith::build_rar4` forges the RAR4 container
  with a STORED entry and the name in raw bytes (no `LHD_UNICODE`), which is
  the same thing the RAR5 forge already did and touches no more of the
  proprietary format than it did. No third-party binary in the repo, no licence
  or provenance question, and it can carry any name from the hostile corpus.
  Verified against real `unrar` 7.23 and `7z`.
- **And it measured the delegates**, which is what the gap was really hiding.
  On a RAR4 name in CP866, `7z -slt` hands back the OEM bytes UNTOUCHED and the
  name it prints selects the entry again; `unrar` does not — it maps them into
  a private-use range (U+E0xx behind U+FFFE). That is a *different* failure
  from the truncation already measured for non-UTF-8 RAR5 names, and both point
  the same way: on names that are not UTF-8, `unrar` is not a source of truth.
  The preference for `7z` was until now measured only over RAR5.
