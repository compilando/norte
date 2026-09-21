# 0140 — The terminal's panel column draws icons

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. Config: `[ui] panel_bar_style`
  gains `"icons"` and `"nerd"`.
- Related: ADR 0131 (the activity bar), ADR 0105 (the icon column)

## Context and problem statement

ADR 0131 gave the terminal an optional panel column (`panel_bar_position =
"left"`), three cells wide, with each panel's access letter on its own
row. The window's column draws an icon per panel, marks the one with the
keyboard, and shows a count badge, as VS Code's activity bar does. The
terminal's column worked, but it read as a list of letters.

## Decision

**Icons, one cell each.** `panelbar::icon(kind, IconSet)` gives every
built-in panel an icon with the same subjects as the window's: ★ places,
⋔ tree, ◉ viewer, ∿ processes, ⓘ details, ≡ log, ◔ disk map, ◷ timeline.
They are Unicode symbols without emoji presentation, which a terminal draws
in one cell; a test pins that each is one cell wide and one scalar. With
`panel_bar_style = "nerd"` the column uses Nerd Font glyphs instead (Font
Awesome, in the private use area), closer to VS Code's, for a font that has
them. `"letters"` keeps the letters. `"names"` — the default — draws the
Unicode icons in the column, since names do not fit there; in a row, the
icon styles behave as `"names"`. A panel without an icon (a plugin's)
draws its letter.

**The same scale as the window.** Each row is three cells: a `▎` bar in
the focus colour on the panel with the keyboard, the icon (bright if the
panel is open, dimmed if closed), and the badge — the count in the warning
colour, `+` past nine.

**Air when it fits.** With room for all of them, one blank row sits above
the first icon and between every two, as in VS Code; otherwise they pack.
Painting and mouse zones come from the same function, so a blank row is
never a button.

## Consequences

- The terminal's column reads like the window's, with no font requirement
  by default.
- Terminals configured to draw East Asian ambiguous-width characters wide
  would misalign the column; `"letters"` is the way out, and the same is
  already true of the box-drawing borders.
