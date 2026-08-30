# 0082 - What norte hands to a program it does not own

- Status: accepted
- Date: 2026-08-30
- Decision makers: Oscar González
- Related: ADR 0072 (a hostile link is indistinguishable from a legitimate one),
  ADR 0073 (the destination anchor), ADR 0076 (`fs.create` carries an anchor),
  ADR 0077 (the same command means the same thing in both frontends), issues
  #301, #302, #303.

## Context and problem statement

Three findings from the security review of ADR 0077 were recorded there under
"what this does not close", and they are one family: **every one of them is
norte handing something to a program it does not own.**

- **#302** — the editor is launched with the browsed directory as `cwd` and a
  program name nobody resolved. On unix `Command::current_dir` is applied
  BEFORE the program is resolved, so `EDITOR=vim` with a `.` (or an empty
  component) in `PATH` executes a file called `vim` out of the directory the
  reader just walked into. Extract a hostile archive, enter it, press F4.
- **#303** — norte creates the empty file and then launches `$EDITOR` on it
  (TUI) or hands it to the desktop (window). It ANNOUNCES the name by creating
  it, and anyone who can write in that directory can see it appear, unlink it
  and leave a symlink in its place before the editor starts.
- **#301** — the destination anchor of ADR 0073 was only ever filled by
  `norte-client`, over the wire. `ntc` runs the EMBEDDED backend by default, so
  the frontend with the strongest reason for the check — the one that launches
  `$EDITOR` on what `fs.create` just created, which is the justification ADR
  0076 gave for putting the anchor on that method — was the one not getting it.

## Decision

### The program is resolved before the child gets a directory

`openers::resolve_program` already resolved a program to an ABSOLUTE path,
skipping relative and empty `PATH` entries, and already carried the reasoning:
probe and launch must look at the same directories. What was missing is that
the TUI's launch sites did not use it — they gave `Command::new` the bare name.
Now every child launched with a `cwd` gets the resolved absolute path, and if
the program cannot be resolved **it is not launched**: falling back to the raw
name would hand the lookup back to `execvp` with the directory already changed,
which is the hole itself.

`resolve_program` takes an `OsStr` now, because `$EDITOR` is an environment
variable and a variable is bytes (rule 1).

**The `$SHELL` guard is NOT copied to `$EDITOR`.** `login_shell_from` refuses a
relative `$SHELL` outright; a relative `$EDITOR` is what everybody's shell
profile says. Same hazard, different guard, and the guard belongs at the launch
because that is where the `cwd` is known.

### The window between creating a name and opening it is narrowed, not closed

Before launching the editor, both frontends ask what is at that path and refuse
anything that is not a regular file. The question goes through the backend
(`fs.stat`, which is `lstat` — it describes the link and never its target), not
through a `std::fs` call in the run loop: blocking I/O in the executor is hard
rule 2, and the backend may be the daemon.

**This narrows and does not close**, and saying so is part of the decision:
between the `stat` and the `exec` there is still a gap. Closing it properly
would mean opening the file once and handing the descriptor to the child, and
no editor interface here accepts one. What it buys is real anyway: in the TUI
the window used to be a full re-listing of both panels — seconds on a remote
pane — and is now one round trip to the core.

The refusal says the same thing for all three causes (a link, a directory, a
path with nothing at it). Naming which one would confirm to whoever planted the
link that their link is in place.

### The embedded backend remembers what it listed

`Backend::Embedded` now keeps the anchor of every directory it lists and passes
it to `create_file`, `copy` and `move` — the same thing the SDK does over the
wire, so the same gesture gets the same check whether `ntc` runs embedded or
against a daemon.

The cache lives in the `Engine` and not in `Backend`, because `Backend::
Embedded` is an `Arc` of the engine and nothing else: two clones of it — the
one a panel lists with and the one a copy is launched from — share nothing
else, so a per-clone memory would never see what the other listed. It is the
equivalent of the SDK's `Inner`. **The daemon neither writes nor reads it**: its
clients bring their own anchor in the request, which is the one belonging to
whoever actually listed.

The cache is duplicated in `norte-core` rather than shared with the SDK's. Forty
lines and a cap are cheaper than tying the embedded path — which exists to work
WITHOUT a daemon — to the crate that talks to one.

## Consequences

- An editor that is not installed now fails with "not found in PATH" instead of
  the OS's ENOENT several frames later. The honest user sees no other change:
  an absolute `$EDITOR` and a normal `PATH` resolve exactly as before.
- A `PATH` whose entries are all relative can no longer launch anything from
  norte. That is the fix, not a side effect.
- `pane.edit-new` costs one extra round trip to the core before the editor
  opens.
- Every anchored operation from `ntc` now carries an anchor for a directory the
  reader actually listed, so a destination swapped between listing and writing
  is refused with `Conflict{EscapesRoot}` — the same answer the daemon path
  gives. A destination nobody listed still behaves as it did in 0.53: there is
  no anchor to send and the check does not happen.
- Listing through the embedded backend now costs one `node_id` per listing,
  which is what the daemon already paid on every `fs.list`.

## Alternatives considered

- **Not giving the editor the panel's directory as `cwd`.** It would close
  #302 too, and it would take away the reason the `cwd` is there: `:w
  other.txt` landing beside the file being edited. Resolving the program keeps
  both.
- **Accepting #303 as documented** (it already is, in the `viewer.md` help
  topic, next to the sentence that says what norte does govern). Rejected: the
  TUI's window was wider than the property suggests — `on_tick` armed the
  editor and then re-listed both panels in the same tick — and one `stat` is a
  cheap way to make it as narrow as the property allows.
- **Putting the anchor cache in the `Engine` for the daemon too.** Rejected: an
  engine that serves many clients would be remembering "what was listed" for
  all of them at once, and the anchor exists precisely to say who looked.
