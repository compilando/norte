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

**The raster has the slot's proportions.** Kitty stretches an image to
the `c`×`r` cells it is given, and cells are about 1:2: a square raster
came out as a tall oval. The canvas is 2×2 cells in pixels
(`TIOCGWINSZ`, 1:2 if the terminal does not say) with the icon square in
its middle. A resize redraws with ED 2, which drops every placement, so
the loop forgets what it placed and places it again.

**Closed is halfway to the background.** The glyph column dims closed
icons with `dim`, which pixels do not get, and most presets define no
`muted`; the closed colour is blended 50% toward the background.

**Hidden under anything painted over the body.** Kitty draws pixels above
text. Icons are placed only when the column is visible
(`panel_bar_visible`) and nothing is over the body
(`something_above_the_viewer`), the spec's buffer check replaced by the
answers that already decide painting and clicking. While they are
withheld the slot shows the one-cell glyph, not an empty square.

**Their own ids, erased at the ADR 0118 sites.** A reserved range
(`rail_icons::ID_BASE`, 64 ids) never meets the viewer's; exit, suspend
and the panic hook delete the range, and write nothing on a terminal
that never got one.

**Sixel too (F2).** The probe's DA1 reply listing attribute `4` makes
`RailBackend::Sixel`, when kitty graphics is absent and the terminal
reports its cell size in pixels — sixel paints pixel for pixel, so the
canvas is the slot exactly. Eight levels from the rail's background to
the stroke colour need no quantizer, and level 0 is not painted: the
cell's own background shows through, whatever the terminal's palette
makes of it. A sixel image is cells: there is nothing to delete and
nothing to erase on exit, but ratatui does not rewrite cells it believes
unchanged, so taking an icon down — or putting a new one in its cells —
repaints them first from the frame just drawn. The column's last row
stays free in sixel: an image ending on the screen's last row scrolls
the screen. Checked in xterm `-ti vt340` under Xvfb, with a custom
palette, and through tmux 3.7.

**The cell size comes from the terminal** (`CSI 16 t`, in the same
probe), with the window size divided by the grid as the fallback.

**Kitty images are hidden, not deleted, under an overlay** (`d=i`) and
re-placed (`a=p`) when it closes; the image id is the column slot's.

## Consequences

- Terminals without kitty graphics or sixel keep the glyph column
  exactly as before; so does tmux without sixel or passthrough.
- One more dependency (`resvg`, default features off). It brings
  `arrayref` (BSD-2-Clause), allowed in `deny.toml` for that crate only.
- On Windows the probe does not run (it reads `/dev/tty`), so Windows
  Terminal keeps the glyphs although it speaks sixel and answers
  `CSI 16 t`; porting the probe to the Windows console is what is left.
