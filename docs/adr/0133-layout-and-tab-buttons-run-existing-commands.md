# 0133 — Layout and tab buttons run existing commands

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **86** — `ViewSnapshot.layout_buttons`
  (`ChromeButtonView`), the `layout_button_activate { id }` action and the
  `tab_action { slot_id, verb }` action.
- Related: ADR 0131 and 0132 (the same VS Code pass), ADR 0069 (a click
  never carries a command), ADR 0077 (parity)

## Context and problem statement

VS Code puts a handful of layout toggles at the top right, and every editor
group has its own tab bar with a close button per tab and actions at the
end. norte had the commands — `layout.split-h`, `layout.split-v`,
`layout.equalize`, `layout.pick`, `pane.tab-new`, `pane.tab-close` — but in
the window they were reachable only by key, menu or palette. The terminal
already had `[+]` and `[x]` on its tab strip; the window had neither.

## Decision

**Buttons, not new commands.** Every button runs a command that already
exists, through the same dispatch as its key. Nothing new to bind, so the
seven keymap presets are untouched.

### Layout buttons: one table, two paintings

`norte_frontend::layoutbar::BUTTONS` lists the four (split side by side,
split top and bottom, equalize, pick a layout) with a stable id, a command
and an ASCII glyph. The terminal paints the glyphs (`[|] [-] [=] [#]`) at
the right edge of the menu bar, only if they fit whole beside the titles —
a menu title is worth more than a button — and with the menu's key hint
moved to their left. The window paints an icon per id (in-tree strokes, as
in ADR 0131), with the menu entry's name and the live chord in the tooltip.
The name is the menu entry's own (`menu-item-*`): one wording per command.

A click returns the id. The host looks it up in the same table.

### Tab buttons act on the group that was clicked

`tab_action { slot_id, verb }` first selects that tab — the group clicked
takes the focus, as the terminal's `apply_tab_zone` does — then runs
`pane.tab-new` or `pane.tab-close`. In the window each tab carries a `×`
(always visible on the active one, on hover on the others, and taking its
room either way so the label does not move), and the group ends with a `+`
that opens behind the group's active tab. The `×` stops the click from also
selecting the tab: one gesture, one command.

## Consequences

- In the terminal, the four buttons appear from roughly 100 columns in
  Spanish; below that the titles keep the room. Two TUI snapshots moved.
- The TUI routes a click on the menu row to a layout button before the menu:
  both live on row 0.
- Not done: VS Code's toggles for the side bars and the bottom panel. In
  norte those are panels, and the activity bar (ADR 0131) already toggles
  them; a second set of toggles would be two surfaces for one decision.
