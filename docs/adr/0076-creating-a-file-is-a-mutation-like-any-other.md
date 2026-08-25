# 0076 - Creating a file is a mutation like any other

- Status: accepted
- Date: 2026-08-25
- Decision makers: Oscar González
- Related: ADR 0009 (delete as a Task), ADR 0011 (daemon), ADR 0066 (D10 all
  effects through the daemon, D11 an unprivileged webview), issues #290, #132,
  #104.

## Context and problem statement

`pane.edit-new` — create a file and open it for editing — was the last command
of #290 the window could not do, and the reason was not the gesture. It was
that **the protocol had no way to create a file.**

`fs.mkdir` creates a directory. `fs.copy` writes one that already exists
somewhere else. `archive.unpack` and `file.combine` write files, but only as
the product of reading others. Nothing said "an empty file, here, by this
name".

The TUI never needed one: `pane.edit-new` launches `$EDITOR` with the pane's
directory as cwd and lets the editor create the file when the reader saves. A
window has no terminal to hand a process to, and creating the file from the
window's own process would bypass the daemon — which ADR 0066 D10 forbids and
which would leave the mutation out of the journal (hard rule 4).

So the choice was: leave the command out with its reason, or add the method.

## Decision

**Add `fs.create`, protocol 0.57.0.** It creates ONE empty file as a Task,
exactly as `fs.mkdir` creates one directory, and it carries every rule that
comes with being a mutation: journal `Created` with its undo, a policy gate
(`PolicyOp::Create`), progress, and cancellation.

### It fails if the destination exists

There is no reading of "create" that means "empty whatever is there". A method
that truncates in silence is data loss with an innocent name, so a taken name
is `Conflict { Exists }` — the same answer `fs.mkdir` gives.

**Where that exclusivity comes from is not the same, though, and this ADR said
it wrong at first.** `mkdir` is atomic everywhere because `mkdir(2)` fails with
`EEXIST`. Creating a file is open-then-commit, in two steps, so the atomicity is
not a theorem of the core: each provider supplies it separately, and
`Provider::write` already contracts create-new for exactly this reason. The
local provider publishes with an atomic `rename_noreplace` — its own comment
says "no TOCTOU window". The object provider uses `If-None-Match`.
`MemProvider` revalidates under its lock. **SFTP cannot**: v3 has no atomic
rename, and there the window is real and documented in that provider.

The `stat` in `create_task` is therefore *not* what stops us overwriting
anything. It is the same early check `mkdir_task` does, for the same reason: a
clean `Conflict` before a staging file is created, and never claiming a
pre-existing node as ours. The first draft of this ADR described it as the only
protection and filed a `create_new` on the trait as future debt; the review
caught both. That method already exists, and it is `write`.

The consequence worth writing down: the day a new provider implements `write`
with a truncate, it violates a contract its own rustdoc already states, and
`fs.create` is the caller that loses data for it.

### `PolicyOp::Create` is its own permission

Not folded into `Mkdir`. Letting something create folders is not letting it
create files, and a rule that said `mkdir` and granted both would be one nobody
wrote.

### The window creates, then opens

The gesture asks for a name, creates the file, and only on the task's
**successful** outcome hands the path to the desktop through the native effects
channel. Opening earlier would launch an editor over a file that is not there
yet — and what that editor shows (an empty buffer that creates the file on
save) would look exactly like success.

It is refused on a remote pane, and said *before* the name is typed: what opens
afterwards is the desktop application, and `xdg-open` cannot be given an
`sftp://`. Saying it once the name is written arrives too late.

### It carries `dest_anchor`, and `fs.mkdir` does not

The first draft left the anchor of ADR 0073 out by symmetry with `fs.mkdir`,
without thinking about it. The review pushed back with a better argument than
the one for leaving it out, and it stands:

**`fs.create` is the only method on the wire whose success hands a path to a
program outside norte.** A frontend creates the file in order to open it with
the desktop's editor. Put a symlink in place between the listing and the
confirmation, and what is lost is not an empty file — it is the whole editing
session the human types afterwards, into a directory they were never looking
at. That is exactly the case ADR 0073 exists for, and it is *more* acute here
than in a copy, not less.

Creating a directory in the wrong place is a misplaced empty directory. Creating
a file in the wrong place is an invitation to write into it.

The field is optional and omitted, so a caller who did not list the directory
behaves exactly as before; `norte-client` remembers the anchor of every
directory it lists and sends it on its own, so every frontend gains the check
without a line of code.

## Consequences

- Protocol **0.57.0**: `FS_CREATE`, `FsCreateParams`, `TaskKind::Create`.
  Additive — a 0.56 client does not call the method and degrades the new kind
  to `Unknown` through its `serde(other)`. The window shifts because against a
  0.56 daemon a terminal-less frontend cannot offer "edit a new one" at all.
- No provider work was needed: `Provider::write` already exists and publishes
  by rename, so an empty file is a sink opened and committed with nothing
  written. This is the first caller that uses it for its emptiness rather than
  for its content.
- `norte-core` grows `Engine::create_file`/`create_file_as` and
  `ops::create_task`, modelled directly on `mkdir_task`.
- The window's fake backend had to be taught to hand over a task that is
  *running* and finishes afterwards. The others hand over one already terminal
  with the sender dropped, which no real backend does — the host pumps the
  channel's *changes*, so a dead channel never delivers an outcome, and the
  outcome is the whole point here.
