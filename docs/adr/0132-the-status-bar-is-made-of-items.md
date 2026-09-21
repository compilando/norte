# 0132 — The status bar is made of items

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **85** — `ViewSnapshot.status_items`, the
  `status_items` change, the `status_item_activate { id }` action.
- Config: new `[ui] status_items` (a list of ids).
- Related: ADR 0131 (the activity bar, same VS Code pass), ADR 0077 (parity),
  ADR 0069 (a click never carries a command)

## Context and problem statement

VS Code's status bar is the part of its chrome people most often wish other
tools had: small facts on the right — line and column, encoding, language,
branch — each one clickable, each one removable. Beside it, norte's status
bar was two different things:

- in the terminal, one line built as a chain of priorities (a wait beats a
  drag beats a message beats a live search beats the Lua hook beats the path),
  with the position `3/120` and what is marked glued to the path;
- in the window, a bar that was **empty** when nothing was happening, since
  the path already lives in each pane's header.

Neither was configurable, and the only clickable thing on it was the unread
notices badge.

## Decision

**The bar has two halves, and only the right one is configurable.**

### The left half does not change

Messages, waits, drag hints, search status and above all the WARNINGS
(listing incomplete, names reinterpreted, marks pruned, detached session,
unjournaled session) stay where they were, in the same order. A warning a
configuration could remove would not be a warning. When space is short, the
right half gives way to a persistent warning before the warning is cut.

### The right half is items

`[ui] status_items` lists, in screen order, any of:

| id | text | click |
| --- | --- | --- |
| `position` | `3/120` (none with a filter active) | — |
| `marks` | `2 marked, 4 MiB` (none without marks) | — |
| `sort` | `Name ↑` | `pane.sort-menu` |
| `encoding` | `UTF-8` / `CP437` | `pane.names-encoding` |
| `tasks` | `⟳ 2` (none without tasks) | `layout.processes` |
| `notices` | `!3` (none without notices) | `layout.log` |

Absent means all six. An unknown or repeated id is refused when the file is
loaded, and — the half that matters — by the settings editor before it
writes, since a bad list written to `norte.toml` would make the next load
fail. That refusal needed a new `SettingsEditError::Invalid { key }`.

An item with nothing to say takes no room, not even a separator.

### One model, both frontends

`norte_frontend::statusbar` decides what each item says, its priority, its
command and which fit (`fit`: drop the lowest priority until they fit, keep
the configured order). `StatusInput::from_pane` reads the facts from the
shared `PaneState`, so the filter rule for `position` exists once. The TUI
gives the right half at most half the width; the host does the same over the
width the window declared, and sends the list already cut.

A click returns the item's **id**, not an index — the list moves with the
cursor and the width, as settings rows move with a search (the same reason
`settings_set` goes by id). The host resolves the command against its own
list of now and runs it through the same dispatch as the key.

### The notices badge becomes an item

It was special-cased in both frontends and hid itself while a message was
showing. It is now the `notices` item, and a message on the left no longer
hides it: they are different halves.

## Consequences

- The terminal's idle line changes shape: the path on the left, the items on
  the right. Forty-seven TUI snapshots moved, all on that line.
- The window's status bar finally says something when nothing is happening.
- Not in this version: items contributed by plugins (it crosses WIT and
  needs `security-reviewer`), hiding an item with a right click in the
  window, and a `git.branch` item (its source is not cheap per frame). A
  `layout` item was also left out: the active layout's name is not tracked
  anywhere today.
