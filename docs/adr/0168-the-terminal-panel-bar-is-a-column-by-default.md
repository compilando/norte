# 0168 — The terminal's panel bar is a column by default

- Status: accepted
- Date: 2026-10-05
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. Config: `[ui] panel_bar_position
  = "auto"` changes meaning in the terminal.
- Related: ADR 0131 (the activity bar), ADR 0140 (the terminal's column)

## Context and problem statement

ADR 0131 made `auto` answer differently per frontend: a row on top in the
terminal, because width is short there, and a column on the left in the
window. With ADR 0140 the terminal's column draws the same icons as the
window's, and the owner wants both frontends to look alike out of the box.

## Decision

**`auto` is a column in the terminal too.** The terminal's answer for
`auto` flips; `top` keeps the row for whoever wants it back. The variant
stays: it is still "each frontend's own answer", and a frontend may answer
differently again without a config change.

**Open and closed must differ in more than `bold`/`dim`.** Many terminals
honour neither on one glyph, and vscode-dark gives `Title` and `Regular`
the same colour. An open panel now carries a `▎` rule in `Muted` (the focus
colour when it has the keyboard), and a closed one paints in `Muted`,
dimmed, falling back to `Regular` on a theme without it. The window's
column follows the same scale: a side line when open, a shade when
focused.

## Consequences

- Existing installs without `panel_bar_position` get the column. The
  listing loses three columns of width; at 80 columns the name column
  narrows accordingly.
- Tests that count cells from the frame's edge pin `top`; the default
  screen's snapshots show the column.
