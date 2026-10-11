# 0177 - User commands run programs on a key

- Status: accepted
- Date: 2026-10-10
- Decision makers: Oscar González
- Related: ADR 0010 (extension boundaries), ADR 0026 (embedded Lua and
  project trust), ADR 0077 (same command, same meaning in both frontends), ADR 0082
  (what norte hands to a program it does not own), ADR 0110 (Lua stays in the
  terminal), ADR 0176 (the terminal panel holds several shells), CLAUDE.md
  hard rules 1 and 9, spec
  `docs/superpowers/specs/2026-10-10-user-commands-design.md`.

## Context and problem statement

Krusader's "user actions" let a person bind a program of their own to a key
and run it on the selection. norte has the pieces but not the door. The
keymap accepts catalogue commands and `lua:<name>` and rejects anything else.
`openers.toml` already expands argv templates byte-safely, but only to open a
file, from a menu.

Lua is the obvious door, and it is closed: it is TUI-only and frozen (ADR
0110 §1 and §4), and the window has no host for it. WASM command plugins
(ADR 0010) are governed and run in both frontends, but "run `oxipng` on what I
marked" should not need a compiled plugin and a manifest.

## Options

**A. Reopen Lua.** Move it to a shared crate and host it in the window, then
write the commands in `init.lua`.

- Good: one scripting story.
- Bad: ADR 0110 counted the cost: a rewrite of the host, `!Send` futures in
  the window's loop, and an API that grows business logic into a script
  host. For a person who wants to run a program on a file.

**B. WASM command plugins.** Write a plugin per command.

- Good: governed, in both frontends, already there.
- Bad: a build step and a manifest to run one program, and a plugin has no
  way to name the program it would run without asking for the capability to
  spawn anything.

**C. `commands.toml`, argv only.** A declarative file of programs, each with
an argument template, bound to a key as `user:<id>` or found in the search
box.

- Good: no new language; the shape is `openers.toml`'s, which users and the
  expander already know; both frontends read the same file.
- Bad: it runs programs with the user's rights, so what it may not do has to
  be stated and tested.

## Decision

**Option C.** The rules:

- **argv only.** No implicit shell. `argv[0]` is a program, looked up on
  `PATH` when the command runs, never expanded. Field codes are `%f`, `%F`
  and `%d`, and only as a whole element of `argv`, one argument per file, so
  names stay bytes (hard rule 1). Any other `%x` is rejected at load. A user
  who wants a shell writes `sh -c '…' sh %F` and the files arrive as
  arguments, never inside the script.
- **Every layer but Project.** System, user and configuration-profile layers
  are read; a project's `commands.toml` is ignored, and so is a `user:`
  binding in a project's keymap (the project is discarded and reported, as
  for `lua:`). A folder that was downloaded must not choose what runs with
  the user's rights (ADR 0026).
- **An unknown id loads and is reported.** A binding to an id that no file
  defines is not fatal: it is reported on the status bar at start-up and on
  a profile switch (window) or a reload (terminal), and by `norte doctor` as
  `user:<id>`, and pressing it says there is no such command. A file that does
  not parse is a configuration error and the previous configuration stays.
- **Never in MCP or agent sessions, and no daemon method.** The commands run
  arbitrary programs in the user's session, outside the journal and the
  policy, so exposing one would hand an agent the filesystem directly (hard
  rule 9). `norte-mcp` does not depend on `norte-frontend`, no tool names
  `user:`, and a call to one is an unknown tool; tests pin all three.
- **Background output goes to the log panel**, masked, and the exit to the
  status bar, not to the processes panel: a detached child is not a task of
  the daemon, and the panel would promise a cancel it cannot keep. A
  background command runs in its own process group with stdin closed, so it
  cannot ask the user anything: a program that reads `/dev/tty` (ssh, sudo,
  gpg) is stopped instead of taking the TUI's keystrokes; programs that
  prompt use `run_in = "terminal"`. That mode runs the program itself, with
  no shell, as a new entry of the terminal panel (ADR 0176); the entry closes
  when it succeeds and stays open when it fails.
- **One runner, both frontends.** The model, the templates, the refusals
  (remote panes, no targets, too long, program missing) and the box rows live
  in `norte-frontend`; the TUI and the window only start the process and
  paint the result (ADR 0077). A read-only window does not run them.

This does NOT reopen ADR 0110 §4: no Lua surface grows, and `lua:<name>`
bindings behave as before. The new door is a different one.

No undo is promised: a command is a program norte does not own (ADR 0082),
and what it did to the files is not in the journal.

## Consequences

- Good: Krusader's user actions in both frontends, with no new language and
  no new capability for plugins or agents.
- Good: the file travels between machines, because a missing program is a
  refusal at run time, not a load error.
- Bad: a background command that is still writing output when norte quits
  is stopped at its next write (its pipes close); for long jobs use `run_in =
  "terminal"` or redirect its output inside `sh -c`. A command that never
  exits is the user's to stop.
- Bad: a background command that prompts on `/dev/tty` is stopped and never
  finishes on its own; its status-bar line does not come.
- Bad: the window reloads `commands.toml` late: on a profile switch, a
  settings change or a restart, because it has no file watcher; the terminal
  reloads on save.
- Bad: with `confirm = false` a key press runs a program at once, on the
  user's rights, and norte cannot take it back.
- Not done: per-command keys inside `commands.toml`, interactive
  placeholders, remote execution.
