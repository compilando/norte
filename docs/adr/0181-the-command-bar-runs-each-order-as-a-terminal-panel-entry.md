# 0181 - The command bar runs each order as a terminal-panel entry

- Status: accepted
- Date: 2026-10-13
- Decision makers: Oscar González
- Related: ADR 0084 (the shell behind the panels), ADR 0153 (the subshell is
  moved through a mailbox, not by typing), ADR 0176 (the terminal panel holds
  several shells), ADR 0177 (user commands), spec
  `docs/superpowers/specs/2026-10-13-terminal-here-command-bar-tui-icons-design.md`.

## Context and problem statement

The reader asked for Krusader's single-line command bar: a line under the
panels that runs what is typed in the focused pane's folder. The terminal had
a command-line MODAL that suspended the TUI and ran `$SHELL -c`; the window had
nothing (the parity matrix said "the palette is its answer"). The reader chose
"run it in the terminal panel", which both frontends have.

The obvious way to run an order "in the terminal panel" is to type it into the
shell that is already there. ADR 0153 removed exactly that: typing into a line
editor is only safe with an empty buffer, and no shell can be asked whether its
buffer is empty — type-ahead and zsh's `push-line` leave text there, and the
order concatenates onto it (`rm -rf tmpdir` + the order).

## Considered options

1. **Type the order into the panel's current shell.** Shares the shell's
   state (aliases, variables, `cd`), and it is what a terminal user expects.
   Reopens ADR 0153's injection channel.
2. **One-shot, suspended, like the old modal.** Safe; the window cannot do
   it, and the output vanishes under the listing.
3. **Each order as a NEW terminal-panel entry running `$SHELL -c`.** Safe
   (nothing is typed into a running editor), the output stays visible, the
   same in both frontends, and the user-commands runner (ADR 0177) already
   starts programs as entries.

## Decision

Option 3.

- `norte_frontend::command_bar` is the shared model: the line, its cursor,
  the history (capped at 200, no blanks, no consecutive repeats), completion
  against the pane's names, and `quote_for`, which turns a name's bytes into
  one ASCII word for bash/zsh (`$'…'` with octal escapes), fish (`\XHH`) or
  POSIX sh. A pasted newline never submits.
- Enter refuses off disk with the shared availability table's reason (the
  same one "terminal here" uses). Otherwise the order runs as `$SHELL -c
  <order>` in the pane's folder, as a new terminal-panel entry titled with
  the order and **kept after a clean exit** (`Terminals::keep_on_success`):
  the reader typed it to see its output.
- `pane.command-line` gives the bar the keyboard in both frontends;
  `[ui] command_bar = true` keeps it on screen. There is no separate toggle
  command: the setting pins it and the key calls it up.
- The history lives in `<state dir>/command-history` (mode 0600, temp +
  rename), shared by both frontends; saving happens off the UI loop.
- The window gets it through bridge 113 (`CommandBarView`,
  `command_bar_focus`, `command_bar_paste`); the host owns the line and the
  renderer reserves a bottom row like the panel bar's.

## Consequences

- Good: Krusader's bar in both frontends with no new injection channel; the
  old modal and its suspend path for one-shot orders are gone.
- Good: an order's output is never lost under a repaint.
- Bad: `-c` reads no rc file, so the reader's aliases and functions are not
  available, and a `cd` in an order changes nothing afterwards. The help says
  so; that state belongs to the subshell (`app.toggle-panels`).
- Bad: every order leaves an entry in the panel until it is closed.
