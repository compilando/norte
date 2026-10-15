# Terminal here, command bar, TUI context menu and TUI icons — design

- Date: 2026-10-13
- Frontends: both (TUI and window), shared logic in `norte-frontend`
- Protocol: unchanged. Bridge: one bump (the command bar's view).
- Related: ADR 0077, 0082, 0084, 0153, 0168/0169, 0176, 0177; context-menu
  design 2026-10-09 (its "out of scope: TUI menu" is what part B closes).

## What the reader asked for

1. A right-click "Open terminal here", working across operating systems.
2. Krusader's single-line command bar, besides the terminal panel.
3. The TUI's kitty/sixel icons (with their text fallback) on more of the
   existing chrome — the layout buttons `[|] [-] [=] [/] [#]` first.

Decisions taken with the reader (2026-10-13):

| question | answer |
| --- | --- |
| TUI "terminal here" | external emulator when there is a desktop; the suspended shell otherwise |
| where the bar runs | in the terminal panel |
| remote (sftp) / inside an archive | disabled, saying why |
| scope | all four parts below |

## What exists (so nothing is built twice)

- `app.terminal` ("Terminal here") is catalogued and in the menu bar. The
  window launches an external emulator (`native.rs::terminal`, candidates in
  `shell::terminal_candidates_from`); the **TUI suspends and runs a login
  shell** instead — the two frontends disagree (ADR 0077).
- Candidates: `$TERMINAL`, `xdg-terminal-exec`, then ghostty, kitty,
  alacritty, wezterm, konsole, gnome-terminal, xterm (Linux); `open -a
  Terminal` (macOS); `wt`, `cmd` (Windows). No config key.
- The window's context menu is a shared model (`norte_frontend::context_menu`)
  projected by `norte-ui-host`; the TUI has none (`mouse.rs`, right button).
- `pane.command-line` is a TUI-only modal that suspends and runs `$SHELL -c`;
  the window has none (parity matrix: "does not apply").
- User commands with `run_in = "terminal"` (ADR 0177) already run a program
  as a new entry of the terminal panel.

## Part A — "Terminal here", everywhere

**A1. Candidates.** Add `ptyxis` (GNOME's default now), `kgx`, `foot`,
`xfce4-terminal` with their cwd flags, and `x-terminal-emulator` (Debian
alternatives) with no flag — it is an arbitrary program, same rule as
`$TERMINAL`. Order: `[ui] terminal` › `$TERMINAL` › `xdg-terminal-exec` ›
the list. The list's flags are verified against each emulator's `--help`, not
guessed.

**A2. `[ui] terminal`.** An argv list like `[ui] diff`, with `{dir}` expanded
by `openers::expand_argv` (bytes, rule 1). This is how a macOS reader picks
iTerm2 (`["open", "-a", "iTerm", "{dir}"]`) and a Windows reader picks a
profile. When set, it is the only candidate: a configured program that fails
says so instead of silently opening another one.

**A3. The TUI launches the emulator when there is a desktop.** "Desktop" is a
shared, pure function of the environment:

- Linux/BSD: `WAYLAND_DISPLAY` or `DISPLAY` set (an `ssh -X` session counts —
  the window appears where the reader is).
- macOS/Windows: neither `SSH_CONNECTION` nor `SSH_TTY` set (over ssh, `open
  -a Terminal` would open on the remote machine's screen).

Without a desktop, or when every candidate fails to start, the TUI keeps
today's suspended shell and the status bar says which one happened. The
launch is detached (own process group, stdio null) so the emulator outlives
the TUI.

**A4. Refusals with a reason.** A remote pane: `host-not-local` (exists). An
archive pane gets its own key (`host-inside-archive`, both locales): "not a
folder on this disk" is the truth for both, but the archive reader needs to
know the zip's folder is one `..` away.

**A5. Context menu (window now, TUI in part B).** `app.terminal` in the
listing's row and empty-area menus, section "open", disabled-with-reason on
remote/archive panes through `availability::verdict`. Label reused:
`menu-item-app-terminal`.

## Part B — a context menu in the TUI

The shared model and verdicts already exist; the TUI adds the surface.

- **Open**: right click on a row / empty area / column header, and the
  `pane.context-menu` command (the Menu key) on the cursor — today catalogued
  as "not here" in the TUI.
- **Paint**: a bordered popup at the click (clamped to the screen), sections
  separated by a rule, disabled entries dimmed with their reason on the
  right, the chord of each command where it has one. Same look as the
  existing TUI menus.
- **Drive**: ↑/↓/Enter/Esc and the mouse; a click outside closes it
  (ADR 0178's rule). Anything that changes the listing under it closes it —
  the same fingerprint check the host uses, since a stale row is a
  wrong-file operation.
- **Run**: a `Command` goes through the TUI's one dispatcher; the `Verb`s of
  places and tree branches through the TUI's existing handlers for those
  surfaces. No entry exists only in one frontend; a test compares the
  entries both build from one `Surface`.

## Part C — the command bar

**Not typed into a shell.** ADR 0153 removed typing into a shell's line
editor: with anything in the buffer (type-ahead, zsh's `push-line`) the order
concatenates onto it. So each order is **a new entry of the terminal panel
running `$SHELL -c '<order>'`** in the active pane's directory — the runner
user commands already use (ADR 0177). The panel opens if it is closed.

Unlike a user command, the entry does **not** close on success: the reader
typed it to see its output. It stays until closed (`✕`, `terminal.close`),
titled with the order. Consequence to state in the help: `-c` loads no rc
file, so the reader's aliases and functions are not there — the same as
Krusader's and as today's TUI modal.

**The bar.** One line under the panels (above the function-key bar in the
TUI), shown by a setting `[ui] command_bar = true|false` (default off: it
costs a row and orthodox presets already reach `pane.command-line`) and
toggled by a command `app.command-bar` in all seven presets. Contents:

- a prompt with the active pane's directory (masked display name, cut from
  the start like the breadcrumbs) and `>`;
- the input: ↑/↓ history (per session, shared by both frontends through the
  session file like the hotlist), `Tab` completes against the active pane's
  listing, a chord inserts the name under the cursor quoted for the reader's
  shell (bytes, rule 1) — Krusader's and MC's `Ctrl+Enter`;
- `Enter` runs, `Esc` returns the keyboard to the pane.

`pane.command-line` keeps its key and now focuses the bar (opening it for
that use if it is hidden). The TUI modal goes away; the window gets the same
bar, so the parity matrix entry flips from "does not apply" to a pinned
pair. On a remote or archive pane the bar is dimmed with the reason, and
`Enter` does nothing.

## Part D — TUI icons beyond the panel bar

**One drawing, two frontends**, as ADR 0169 did for panels: the shapes in
`icons.ts` for `layout:*`, `fs:*` and `ui:*` move to SVG files under
`crates/norte-frontend/assets/ui-icons/`; the window imports them `?raw`
(as it does `panel-icons`) and the TUI rasterises them through the same
kitty/sixel path.

Where, in this order:

1. **Layout buttons** on the menu bar: 2 cells wide × 1 row (a square at the
   usual cell aspect), replacing `[|]` etc.; the click zones keep the same
   cells. Fallback: today's ASCII.
2. **Places and tree rows**: folder / open folder / drive / removable /
   network / home / favourite, 2×1 before the name. Fallback: today's text.
3. **Status bar indicators**: remote, bell, search.

The 1-row-high case is new (panels are 2×2): the rasteriser takes the cell
box as a parameter. Verified in pixels on kitty and on xterm `vt340` sixel
under Xvfb, as with ADR 0169.

## Order and branches

Four branches, merged together once (one push = one gate):

1. `feat/terminal-here` — A1–A5.
2. `feat/tui-context-menu` — B (gets `app.terminal` for free).
3. `feat/command-bar` — C (bridge bump).
4. `feat/tui-chrome-icons` — D.

ADRs: one for A3 (when the TUI hands a directory to an emulator), one for C
(the command bar runs each order as a panel entry, never typed into a shell).
B and D follow existing ADRs (context-menu design, 0169).

## Testing

- A: the candidate list is a pure function — table tests per OS for the
  order, `[ui] terminal` exclusivity, `{dir}` with non-UTF-8 bytes, and the
  desktop predicate over a fake environment.
- B: the TUI and the host build the same entries for the same `Surface`;
  stale listing closes the menu; disabled entries never dispatch.
- C: an order never reaches a pty as typed input (the runner spawns
  `$SHELL -c`); a name with a quote, a newline and invalid UTF-8 inserted by
  the chord comes out as one argument in bash, zsh and fish; remote/archive
  refuse; history survives a restart.
- D: the TUI asks for the same SVG the window imports (no second drawing);
  fallback text when the terminal has no graphics; snapshot of the menu bar
  with and without graphics.
- Every new key in both locales; seven presets bind `app.command-bar` or say
  why in their header.

## Out of scope

- `terminal here` over ssh for remote panes (the reader chose "disabled").
- Running the bar's order inside an existing shell (ADR 0153).
- Icons on listing rows (ADR 0105 already owns that column).
