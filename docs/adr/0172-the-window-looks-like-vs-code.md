# 0172 — The window follows VS Code's chrome

- Status: accepted
- Date: 2026-10-08
- Decision makers: Oscar González
- Protocol: unchanged. Bridge 100 (`dock_slot`) and 101
  (`PanelBarView.footer`, `activity_activate`). Amends ADR 0136's default.
- Related: ADR 0133 (layout buttons), 0136 (custom title bar), 0138
  (moving panes), 0069 (the renderer does not dispatch)

## Context and problem statement

Compared side by side with VS Code (2026-10-08), the window worked but
read as rough. Two rows of chrome, nothing at the foot of the activity
bar, hard borders, panes that could not become a full-height column, a
`/` glued to every folder, the provider's name on every local path, and a
toast like a selected row.

## Decision

1. **Drop on the window's edge takes the whole side**
   (`Node::dock_outer`, both frontends). Dropped on a pane, a pane could
   only land beside or under that pane. The band is 24 px in the window
   (a third of that on top, where the titles sit) and the outermost cell
   in the terminal.
2. **Our own title bar by default** (`[ui] titlebar = "custom"`), with a
   **command centre** that opens "go to". ADR 0136 kept the native bar as
   the default so as not to fight window managers. Two rows of chrome
   against VS Code's one was the first thing the comparison showed, and
   VS Code itself defaults to its own bar on Linux. `native` stays.
3. **Settings and help at the foot of the activity column**, as VS
   Code's gear. Like the command centre, the renderer names a button from
   a closed list (`panelbar::footer_command`), never a command (ADR 0069).
   The terminal keeps F1 and F9 and does not paint this foot: its column
   is short, and those keys are always there.
4. **Cards, row icons, badges, cards for notifications.** Panes get a
   one-pixel gutter, rounded corners, and an inner outline. The border
   stays one pixel, so the cell grid holds. Rows without plugin icons get
   the Tree's folder and a page icon. The local root crumb reads `/`. The
   header's notices become right-aligned badges. The footer is fitted to
   the pane by the shared priority rule. The toast becomes a wrapping
   card.

## Consequences

- A window manager that wants its own decorations needs
  `[ui] titlebar = "native"`.
- Panels a restored layout brings now start at boot like opened ones. The
  terminal's shell only does so in a LOCAL directory the session
  restored: a directory from the command line ("Open with norte" on a
  downloaded folder) would run the shell's prompt hooks (git, direnv)
  there without a gesture, so that panel waits for its key.
- The "whole top" is never offered to a drag that started on a top
  title: those titles sit on the top edge, and a title nudged sideways
  restacked its pane across the width.
- Not done: a "maximize panel" header action (no such command yet), and
  status bar items beyond the remote indicator (no git branch in the
  core).
