# 0180 - "Terminal here" opens an emulator when there is a desktop

- Status: accepted
- Date: 2026-10-13
- Decision makers: Oscar González
- Related: ADR 0077 (the same command means the same thing in both
  frontends), ADR 0082 (what norte hands to a program it does not own),
  spec `docs/superpowers/specs/2026-10-13-terminal-here-command-bar-tui-icons-design.md`.

## Context and problem statement

`app.terminal` ("Terminal here") did two different things. The window opened
an external terminal emulator in the pane's directory; the TUI suspended
itself and ran a login shell there. Same name, same menu entry, two results,
which is what ADR 0077 forbids. The reader asked for "open a terminal here"
on the right-click menu, which means an emulator window.

The TUI is not always where a window can appear: over ssh there is no screen
of the reader's to put one on. And "is there a screen" is not one question
across systems. On Linux a display variable says it, and an `ssh -X` session
has one that does reach the reader. On macOS and Windows there is always a
screen, but over ssh it is the remote machine's: `open -a Terminal` would
open a window in front of nobody.

## Considered options

1. **Keep the TUI's suspended shell.** No change; the reader is already in
   a terminal. Leaves the two frontends disagreeing, and the right-click
   entry would mean two things.
2. **Always an emulator, refuse without a desktop.** One meaning, but over
   ssh the key becomes useless where a shell is exactly what helps.
3. **An emulator when there is a desktop, the suspended shell otherwise.**
   The window's meaning wherever it can be kept, the old behaviour where it
   cannot, and said when it falls back.

## Decision

Option 3.

- `norte_frontend::shell::has_desktop` decides, shared by the handoff's
  `windowed` fact and by "terminal here": on Linux/BSD `WAYLAND_DISPLAY` or
  `DISPLAY` set and not empty; on macOS and Windows neither `SSH_CONNECTION`
  nor `SSH_TTY` set. The macOS/Windows rule is new for the handoff too: it
  used to look only at display variables, which those systems do not set.
- With a desktop, the TUI tries the same candidates as the window
  (`[ui] terminal`, else `$TERMINAL`, `xdg-terminal-exec` and the known
  list) and starts the first that resolves with
  `norte_frontend::shell::spawn_detached`: own process group (the hangup of
  the reader's terminal does not take the emulator with it), a console of
  its own on Windows, null stdio, reaped by a thread.
- No desktop: the suspended login shell, as before. A desktop where no
  emulator starts: the same shell, and the status bar says it is the
  fallback (`msg-terminal-fallback`) so a missing emulator is not mistaken
  for the intended behaviour.
- Remote and archive panes keep refusing with their reason; there is no
  local directory to sit in. Every launcher gets its directory from
  `shell::shell_dir` (on Windows: no verbatim `\\?\`, reserved names
  refused), which is also what `Facts::pane_on_disk` asks, so the dimmed
  menu entry and the refusal agree.
- **No Windows candidate carries the directory in its argv.** Windows
  Terminal re-parses its own command line and splits on `;` even inside a
  quoted argument: `wt -d C:\x\a;calc` ran `calc`. It gets `-d .`, and the
  directory travels only as the process's cwd (corpus `win_wt_semicolon`).
  A user's `[ui] terminal = ["wt", "-d", "%d"]` reintroduces the problem;
  the settings help says to use `.`.

## Consequences

- Good: one meaning for `app.terminal` in both frontends and in the
  right-click menu; `[ui] terminal` applies to both.
- Good: over ssh the key still gives a shell, as before.
- Bad: on a Linux machine with a stale `DISPLAY` (a dead X server) the TUI
  tries emulators that fail to connect; `spawn` succeeds and the window
  never appears. The emulator's own failure is not observed — the same
  limitation the window has always had.
- Bad: resolving the candidates stats `PATH` on the TUI's loop turn; a
  handful of `stat`s, the price `pane.open` already pays.
