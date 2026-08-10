# Shell integration — design

**Date:** 2026-08-10
**Status:** approved
**Roadmap item:** 4 of `2026-08-07-post-alpha-roadmap.md`
**Closes:** #135 (`app.terminal`, `app.toggle-panels`, `pane.command-line`)

## Why

Item 4 is the smallest surface on the roadmap and the one that changes daily
use the most: it is what makes a terminal file manager something you stay in
rather than visit. None of it exists — no cd-on-quit, no picker mode, no way to
reach a shell without leaving.

It is also owed to the user in a stronger sense than the rest of the list. The
four imported presets K2b shipped bind `F9` and `Ctrl+O`, so a migrant from
Total Commander or Far presses them today and reads "not built yet (#135)".
Three keys are visible and dark.

## Scope

In:

- `cd-on-quit` for bash, zsh and fish.
- `ntc --pick`, a picker other tools can consume.
- `app.terminal`, `app.toggle-panels` and `pane.command-line` in the TUI, by
  suspending the interface — no embedded pty.
- `app.terminal` in the GUI, by launching the system terminal emulator.

Out, deliberately:

- **An embedded terminal inside a pane.** portable-pty plus a vt parser plus
  sizing, keyboard, colour, scrollback and a new security surface is a
  milestone, not an item. Nothing here forecloses it.
- **A persistent subshell.** Midnight Commander keeps one alive behind the
  panels, which is what makes its `Ctrl+O` a shell rather than a view of
  scrollback. Ours is the view. See "Honest limits".
- **The GUI's `Ctrl+O` and command line.** There is no host terminal under a
  GUI window to reveal.

## A. Where the frontend paints

Today `ratatui::init()` gives a `DefaultTerminal` over stdout. It moves to the
**controlling terminal**: `/dev/tty` on unix, `CONOUT$` on Windows, opened
once at start-up. Always — not only under `--pick`.

This is the one invasive part of the item, and it is invasive in a shallow way:

- A crate-local `Tui` type alias replaces `ratatui::DefaultTerminal` in the
  three signatures that name it (`main.rs:1523`, `:7770`, `:7841`).
- `ratatui::init`/`restore` are replaced by our own pair over that handle,
  including an equivalent panic hook — leaving a user in raw mode after a panic
  is worse than the panic.
- The `std::io::stdout()` call sites that drive the terminal (mouse capture,
  alternate screen, raw mode — six of them, all in `main.rs`) take the handle
  instead. `mouse.rs` already accepts `&mut impl Write` and does not change.

Two consequences worth stating:

- stdout is now free for data, which is what B needs.
- `ntc > log` no longer writes escape sequences into `log`. Today it does.

**No controlling terminal** (a cron job, a pipe on both ends) is an error with
a message and exit code 2, not a crash and not a screenful of escapes. `--help`
and `--version` still work, because they never touch the terminal.

## B. `ntc --pick`

A boolean flag in the TUI's `BOOL_FLAGS`, parsed by the shared
`norte_frontend::cli::parse`, so the GUI keeps rejecting it as unknown rather
than swallowing it.

**What it emits.** The first-class selection of the active pane if it is
non-empty; otherwise the file under the cursor. Raw bytes of each path, each
followed by a NUL. Rule 1: a path is bytes, and NUL is the only separator that
cannot occur inside one.

```sh
ntc --pick | xargs -0 vim
```

`$(ntc --pick)` is not supported and cannot be: command substitution cannot
carry a NUL and strips trailing newlines, so it corrupts any filename that ends
in one. The `shell-init` wrapper (C) offers a function for the interactive
case.

**How you accept.** A new command, `app.pick-accept`, live only when `--pick`
was passed:

- `Enter` when the cursor is **not** on a directory. `Enter` on a directory
  keeps entering it — that is navigation, and taking it away would make the
  picker unusable for reaching anything.
- `Ctrl+Enter` anywhere, which accepts the selection whatever the cursor is on.

Quitting normally (`q`, `F10`) is a cancellation.

**Exit codes.** 0 accepted and written, 1 cancelled with nothing written, 2
error (no tty, write failure). A consumer distinguishes "user chose nothing"
from "norte broke" without parsing a message.

**Remote panes** are not special here: `sftp://host/x` is a real path and a
tool that asked norte to pick something may well want it. What comes out is the
VPath as displayed, in bytes.

## C. cd-on-quit

**Flag.** `--cd-file PATH`, a value flag. Passed by the wrapper, never by a
human. Not an environment variable: a flag is visible in `ps`, cannot leak into
a child process, and is trivially testable.

**What norte writes.** On a clean quit, if the active pane is `file://`, the
directory's raw bytes followed by a NUL, truncating the file first. Otherwise
**nothing at all**, plus one line on stderr naming the pane that was active
("the active pane was `sftp://host/x`; the shell stays put"). An empty file is
the wrapper's signal to do nothing, so a norte that dies mid-write cannot move
a shell to half a path.

A crash writes nothing, because the write happens at the end. That is the
correct failure: the shell stays where it was.

**The wrapper is printed, not shipped.** `norte shell-init bash|zsh|fish`
writes it to stdout for `eval`. One source, versioned with the binary, no
package-manager file to go stale:

```sh
# bash/zsh:  eval "$(norte shell-init bash)"
# fish:      norte shell-init fish | source
```

It defines a function named `ntc` that shadows the binary, makes a `mktemp`
file, passes `--cd-file`, and on return reads it NUL-safely. The function must
invoke the real binary as `command ntc` (bash/zsh) / `command ntc` (fish) —
calling `ntc` from inside a function named `ntc` is infinite recursion, and it
is the one way this wrapper can fail catastrophically:

- bash/zsh: `IFS= read -r -d '' dir < "$f"` — never `$(cat "$f")`.
- fish: `set dir (string split0 < $f)`.

Empty file, or a read that yields nothing: no `cd`. The temp file is removed on
every path, including a shell that gets interrupted.

`norte doctor` reports whether the wrapper is installed, because "cd-on-quit
does nothing" is otherwise indistinguishable from "you never ran shell-init".

## D. The three keys of #135, by suspension

The mechanism already exists and is already careful. `run_opener`
(`crates/norte-tui/src/main.rs:7841`, built for #28) releases the mouse capture
first, disables raw mode, leaves the alternate screen, spawns with inherited
stdio, and restores **always** — child failure, join failure or an intermediate
syscall failure alike.

It generalises to `run_suspended(argv, cwd, wait_for_key)`. Three new callers:

**`app.terminal`** (`F9`): spawns `$SHELL`, falling back to `/bin/sh`
(`%COMSPEC%` on Windows), with the active pane's directory as cwd. The child
gets `NORTE_LEVEL` incremented, so a norte launched inside that shell can say
so rather than leave the user wondering which one they are quitting. A pane
that is not `file://` declines with a localised reason instead of opening a
shell somewhere surprising.

**`app.toggle-panels`** (`Ctrl+O`): suspends and shows the host terminal until
a key is pressed, then restores. No child process.

**`pane.command-line`**: a prompt on the bottom row; `Enter` runs
`$SHELL -c CMD` with the pane's cwd, waits for a key, restores, and refreshes
the listing. `file://` only. History is out of scope for this item.

**The journal does not see any of this, and we say so.** A shell the user
started is the user acting with their own privileges, not norte performing a
mutation; there is no actor to attribute, no reversal to record, and pretending
otherwise would put unrevertible entries in the chain. Rule 4 governs norte's
mutations. What changes on disk is picked up by the directory watcher and the
refresh on return.

## E. The GUI

Only `app.terminal`. It launches the system terminal emulator with the pane's
directory as cwd: `$TERMINAL` if set, then `xdg-terminal-exec`, then a short
probe list on unix; `open -a Terminal <dir>` on macOS; `wt` then `cmd` on
Windows. Nothing found is a message naming what was tried, not a silent
no-op — the same shape as the opener's "program missing" path.

`app.toggle-panels` and `pane.command-line` are simply not in the GUI's
implemented set, which resolves to `Availability::NotHere`
(`keymap/effective.rs:38`) — a state the engine already expresses, distinct
from `NotBuilt`. The reference sheet and which-key overlay show them greyed
with "not in this frontend" rather than an issue number, which is the truth.

## Honest limits

- `Ctrl+O` shows the terminal's scrollback, not a live shell. mc's version is
  backed by a persistent subshell it keeps alive; ours is not. Documented at
  the command, and an issue is filed for the subshell so the difference is
  tracked rather than discovered.
- Suspension hands over the whole terminal. A child that leaves it in a strange
  state (a program that dies without restoring its own modes) is restored as
  far as our own settings go — alternate screen, raw mode, mouse capture — and
  no further.
- The GUI's terminal is whatever the desktop is configured to open. If that is
  wrong, it is wrong for every application, and norte should not be the one
  guessing around it.

## Testing

- **Pure and unit-testable:** flag parsing, the cd-file writer, and the
  wrapper-emitting `shell-init` output. Cases: a path with `0xFF` bytes, a
  directory whose name ends in `\n`, a remote pane (writes nothing), a
  zero-length pre-existing file.
- **The wrappers are executed for real.** A test runs bash, zsh and fish over a
  cd-file containing hostile bytes and asserts the shell lands in the right
  directory. Skipped with a message when the shell is absent, in the style of
  the existing wasm and MinIO skips — never silently.
- **Suspension** is tested with a trivial child (`true`, `false`, and one
  killed by a signal), asserting the terminal state is restored in all three
  and that the child's status is what propagates.
- **The picker** is tested at the boundary: given a selection and a cursor,
  what bytes reach stdout and what exit code follows. NUL-terminated, not
  NUL-separated, so a single result is unambiguous.
- **Keymap:** a pin that `app.pick-accept` exists in the catalogue, and that
  the three #135 commands are `Live` after this item — the same test shape that
  pins their issue numbers today.

## Decomposition

Four tasks, in order. Each is independently useful and independently
reviewable.

1. **S1 — the terminal handle.** Section A alone: `/dev/tty`/`CONOUT$`, the `Tui`
   alias, the panic hook, the no-tty error. No user-visible feature; it is what
   makes B possible and it fixes `ntc > log` on the way.
2. **S2 — picker.** B: the flag, `app.pick-accept`, the byte-exact output, the
   exit codes.
3. **S3 — cd-on-quit.** C: `--cd-file`, `norte shell-init`, the three wrappers,
   the doctor line.
4. **S4 — the keys.** D and E: `run_suspended`, the three TUI commands, the
   GUI's external terminal, and the catalogue flip from `Planned` to `Live`.

S4 closes #135. S1 must land before S2; S3 and S4 are independent of both.

## What this unblocks

Nothing structurally — item 4 depends on nothing and nothing depends on it.
What it changes is the count of keys an imported preset shows dark: three
fewer, and the next item (3, volumes) takes two more.
