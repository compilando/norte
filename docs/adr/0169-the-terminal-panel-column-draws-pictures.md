# 0169 — The terminal's panel column draws pictures

- Status: accepted
- Date: 2026-10-05
- Decision makers: Oscar González
- Protocol: unchanged. Config: unchanged (`[ui] images` gains a reader).
- Related: ADR 0118 (pixels outside ratatui), ADR 0140 (the column's
  glyphs), ADR 0168 (the column is the default); spec
  `docs/superpowers/specs/2026-10-05-panel-rail-big-icons-design.md`

## Context and problem statement

The column draws one cell per panel. The owner wanted the icons visibly
bigger, close to the window's, on any terminal that can paint graphics,
degrading by itself to today's glyphs.

## Decision

**Pictures, not scaled text.** The icons are the window's SVGs, now in
`norte-frontend/assets/panel-icons` and read by both frontends,
rasterised with `resvg` and placed through kitty's graphics protocol at
2×2 cells. Kitty's text sizing (OSC 66) was the other candidate: it only
exists in kitty, which also speaks the graphics protocol, so it adds no
terminal and draws font glyphs instead of the window's icons.

**Chosen by probe and by `[ui] images`.** `rail_icons::backend` derives the
backend each frame from the existing startup probe and `[ui] images`:
`off`/`blocks` keep the glyphs, `kitty` trusts the reader. No new key.

**One geometry.** `geometry::rail_layout` decides big (four cells wide, two
rows per button) or small, for painting, mouse, the body's offset and
the pixels. It goes small when the buttons do not fit at two rows, never
mid-frame on a failure — a failed icon leaves its slot blank.

**Hidden under anything painted over the body.** Kitty draws pixels above
text. Icons are placed only when the column is visible
(`panel_bar_visible`) and nothing is over the body
(`something_above_the_viewer`), the spec's buffer check replaced by the
answers that already decide painting and clicking.

**Their own ids, erased at the ADR 0118 sites.** A reserved range
(`rail_icons::ID_BASE`, 64 ids) never meets the viewer's; exit, suspend
and the panic hook delete the range, and write nothing on a terminal
that never got one.

## Consequences

- tmux without passthrough, and terminals without kitty graphics, keep
  the glyph column exactly as before.
- One more dependency (`resvg`, default features off). It brings
  `arrayref` (BSD-2-Clause), allowed in `deny.toml` for that crate only.
- Sixel (foot, xterm, Windows Terminal…) is phase F2 of the spec: one
  more `RailBackend` arm.
