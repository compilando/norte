# 0176 - The terminal panel holds several shells

- Status: accepted
- Date: 2026-10-09
- Decision makers: Oscar González
- Related: ADR 0084 (the shell is a process), ADR 0170 (open panels are
  shared), ADR 0077 (same command, same meaning), spec
  `docs/superpowers/specs/2026-10-09-terminal-instances-design.md` (the
  design record; this ADR states the decisions).

## Context and problem statement

The terminal panel (#362) held one shell per frontend. The reader wanted
VS Code's model: open more, switch, close one, name and mark them, choose
which shell. Four questions had more than one answer.

## Options

**Where the shells live**

1. **One `terminal` slot with N shells inside** — the layout tree is
   unchanged; the list is the panel's own state.
2. **One slot per shell**, as tabs of the layout tree (ADR 0134). Reuses the
   tab machinery, but ADR 0170 shares the *open panels* between the TUI and
   the window, so three shells in the window become three shell-less slots
   in the TUI; and the tick, resize and key routing all assume one slot of
   the kind.

**What names a tab**

1. **The program's OSC 0/2 title**, falling back to the shell profile's
   name; a name the reader gives wins over both.
2. **The foreground process** (VS Code's `${process}`): `tcgetpgrp` plus
   `/proc` on Linux and something else on each other OS, and a `std::fs`
   read outside `norte-vfs-local`.

**Which chords**

1. **VS Code's `ctrl+pgup/pgdn`** for next/prev.
2. **`ctrl+alt+n/w/pgup/pgdn`**, the family `layout.terminal`'s
   `ctrl+alt+s` already uses.

**Where the shells come from**

1. **`terminal.toml`**, every layer but the project one.
2. Any layer, like `norte.toml`.

## Decision

One slot, N shells (option 1): the panel is one, as in VS Code, and nothing
about the shared layout changes. The rules — close moves right then left,
exit 0 leaves the list, any other code stays with its last screen, output
behind marks the entry — live once in `norte-frontend::terminals`, tick
included, so both frontends obey the same ones.

The tab says the reader's name, else the program's OSC title, else the shell
profile. The title is foreign text: control and bidi characters are
stripped and it is capped at 128 characters.

Chords are `ctrl+alt+n/w/pgup/pgdn` in all seven presets. Far, Total
Commander and Krusader attest `ctrl+pgup` as "parent directory", and one
family for seven presets beats one per preset. Krusader has no
`terminal.new` chord: its `ctrl+alt+n` is `pane.tab-new`. New-with-profile,
rename and decorate have no chord anywhere — inside the panel every chord is
one taken from the shell.

Shell profiles are read from the system, user and configuration-profile
layers, never `./.norte`: a shell profile names a program norte runs, the
same rule as `openers.toml`. `program` must be absolute. They are called
**shell profiles** everywhere, because "profile" already means a
configuration profile.

## Consequences

- Good: the TUI and the window show the same list by construction; the
  layout file, ADR 0170 and the proto are untouched (bridge 106 only).
- Good: closing the slot or an instance kills shells off the event loop — a
  shell ignoring SIGHUP no longer freezes the screen for it.
- Bad: a program that sets no title leaves its tab named after the shell, not
  after what runs in it.
- Bad: a `vim` in the panel keeps `ctrl+pgup/pgdn`, but loses
  `ctrl+alt+pgup/pgdn`; and VS Code's muscle memory does not carry over.
- Bad: the TUI paints a tab's colour but not its icon: its icons come from a
  plugin decorator, and there is no switch to honour.
- Not done: split groups, restoring the shells of a saved session, moving a
  terminal to its own window.
