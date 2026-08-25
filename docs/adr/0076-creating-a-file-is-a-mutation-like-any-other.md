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
is `Conflict { Exists }` — the same answer `fs.mkdir` gives, and for the same
reason.

The check is a `stat` before the write, and it **races**: between the stat and
the sink's publishing rename, something else can appear. It ships anyway,
because the failure mode it removes (creating over the reader's file) is data
loss, and the one it leaves open (two simultaneous creations of the same name
not seeing each other) is not. Closing it properly needs a `create_new` on the
`Provider` trait, which is a different change with a different blast radius.

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
