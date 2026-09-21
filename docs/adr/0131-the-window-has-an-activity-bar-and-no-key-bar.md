# 0131 — The window has an activity bar and no key bar

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **84** — the key bar leaves the bridge
  (`ViewSnapshot.key_bar`, the `key_bar` change, the `key_bar_activate`
  action, `KeyBarView`, `KeyCellView`); `PanelBarView` grows `vertical` and
  `PanelButtonView` grows `count`.
- Config: new `[ui] panel_bar_position = "auto" | "top" | "left"`.
- Related: ADR 0077 (parity between the terminal and the window), ADR 0106
  (the chrome is derived, not drawn), ADR 0069 (a click returns an index,
  never a command), #324 (the panel bar)

## Context and problem statement

Put beside VS Code, the window read as a terminal in a frame. From the top
it spent four full-width rows on chrome before and after the content: the
title, the menu, the panel bar (`S Sitios  V Visor  P Procesos …`), and at
the bottom the F1–F10 key bar in solid blue cells — the heaviest thing on
screen and the one that said least, since every one of its commands is in
the menu and the palette.

VS Code puts the same kind of thing — "which views exist, which is open,
which has news" — in a narrow column on the left, the activity bar, with a
counter badge on the icon that has something to say. A window is short on
height and has width to spare; a terminal is the opposite.

## Decision

**The window drops the key bar, and its panel bar becomes an activity bar
on the left. The terminal keeps both as they are, and can opt into the
column.**

### The key bar leaves the window, not the terminal

It is removed from the bridge rather than hidden by a default: a view the
window never paints would still be computed and diffed on every patch.
`[ui] key_bar` keeps governing the TUI, and its description now says
"terminal only" in both languages — a setting the window shows must not
promise something the window does not do.

### One key for the position, and `auto` is each frontend's answer

`[ui] panel_bar_position` takes `top`, `left`, or `auto` (default). `auto`
is not a third place: it resolves to `top` in the terminal and `left` in the
window, because each is short on the other dimension. The resolution for
the window happens in the HOST (`PanelBarView.vertical`); the renderer only
reserves the width the host tells it to, the same way it reserves the
height of a row. One decision, one place (ADR 0077).

`right` was left out on purpose: nobody asked for it, and every value is a
geometry both frontends must honour.

### The rail is the same bar, not a new surface

Same buttons, same order, same click-returns-an-index (ADR 0069), same
states. What changes is the painting: an icon per built-in panel kind —
drawn in-tree as plain strokes, so there is no icon set to license — and the
letter for a kind with no icon, which is what a plugin's panel gets. The
panel name moves to `aria-label` and the tooltip, since nothing visible
says it. Focus is a border on the left edge, not a filled background, so
the icon stays readable.

### Attention becomes a count

`PanelButton.attention` goes from `bool` to `u32`: the number of live tasks
for processes, the number of warning-or-worse lines retained by the log
ring. The window paints it as a badge (`99+` from a hundred); the terminal
keeps its one-cell `·`, because a count that changes the button's width
would make the row move. `LogRing::has_at_or_above` became
`count_at_or_above`: the same single pass over a bounded ring, without
cloning.

The DTO keeps `attention: bool` next to the new `count`, so a renderer that
only wants "is there something" does not have to know what zero means.

### The terminal's column

With `left`, the TUI paints a three-cell rail — ` S·`, exactly a
letter-style button — one button per row, from under the menu to above the
key bar. Body, mouse zones and painting all read the same geometry
(`panel_bar_area`), so the listing moves right and a click on the rail
cannot fall through to a pane. A terminal too narrow for the rail and a
listing beside it gets no rail.

## Consequences

- The listings gain two rows of height: the key bar's at the bottom and
  the panel bar's at the top. They give back 48 px of width to the rail.
- A panel contributed by a plugin shows its letter in the rail, not an
  icon. Letting a plugin declare an icon is a separate decision (it crosses
  WIT); ADR 0108's "a plugin names a meaning" is the likely route.
- Function keys still work in the window; only the row that advertised them
  is gone. The help and the which-key panel remain where they are listed.
- The rail has no bottom group yet (settings, connection), which VS Code
  has; those are commands, not panels, and do not fit the "index into the
  panel bar" click contract without a new action.
