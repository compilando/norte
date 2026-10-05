# Big icons in the terminal's panel column — design

- Date: 2026-10-05
- Status: approved in conversation, pending written review
- Related: ADR 0118 (the TUI paints an image outside ratatui), ADR 0140
  (the terminal's panel column), ADR 0168 (the column is the default)

## Intent

The terminal's panel column draws each panel as one cell (★ ⋔ ◉ …). The
owner wants them visibly bigger, close to the window's activity bar, on any
terminal that can paint graphics — not only kitty — and degrading by itself,
with no setting, down to today's one-cell glyph.

Success:

- On a terminal with graphics, each icon is the window's own icon, 2×2 cells.
- Anywhere else (tmux without passthrough, plain terminals, too little
  height), the column is exactly today's, with no stray bytes on screen.
- The window and the terminal draw the same icon from the same source.

Out of scope (decided): a block-art middle tier; kitty's text sizing
(OSC 66) — every terminal that has it also speaks kitty graphics; iTerm2's
inline images — iTerm2 speaks sixel; DECDHL/DECDWL, which double a whole
line, not a column.

## Backends

```rust
enum RailBackend { KittyGraphics, Sixel, Glyph }
```

Chosen ONCE at startup, by probing, never by `TERM`:

1. `KittyGraphics` if `kitty_graphics::query_support()` said yes.
2. `Sixel` if the DA1 reply that same probe already reads carries
   attribute `4`, AND the cell size in pixels is known (below).
3. `Glyph` otherwise.

`[ui] images = "off"` forces `Glyph`: it is already the switch for
"no pixels from this terminal", and the column honours it instead of
adding a key. `"blocks"` forces `Glyph` too (half-blocks make no sense at
2×2 cells).

The enum is the extension point: OSC 66 or iTerm2 would be one more arm.

## One source of icons

The strokes in `crates/norte-gui-tauri/ui/src/render/icons.ts` move to one
`.svg` per panel kind in `crates/norte-frontend/assets/panel-icons/`
(24×24 grid, `stroke="currentColor"`, as today). The window imports them
(`?raw`); the TUI embeds them with `include_str!` through
`norte_frontend::panelbar::icon_svg(kind) -> Option<&'static str>`. A kind
with no SVG (a plugin's) keeps its letter in both frontends, as today.

## Geometry: `rail_layout`

One function, used by painting, mouse zones and placement:

```rust
struct RailLayout { big: bool, width: u16, rows: Vec<RailSlot> }
struct RailSlot { y: u16, height: u16 }   // 1 or 2 rows
```

- `big` when the backend is not `Glyph`, the style is not `letters`, and
  every button fits at 2 rows (with a blank row between two if that fits
  too, as `rail_rows` does today; packed otherwise).
- Big: width 4 — rule, 2-cell icon, badge. The `▎` rule covers both rows;
  the badge sits on the lower one.
- Not big: today's `rail_rows`, width 3.

`geometry::RAIL_W` becomes `rail_layout(..).width`; every caller that
reserves the column asks the same function.

## Rasterising

- `resvg` (pure Rust, linebender, maintained; justified in the PR per
  hard rule 8) renders an SVG with `currentColor` replaced by the state's
  colour: closed = `Muted` dimmed, open = `Title`, focused = `Title` (the
  same scale as the glyph column, ADR 0168).
- Size: kitty fits the raster to the `c`/`r` cells it is given, so the
  icon is rendered at a fixed 64 px. Sixel needs exact pixels: 2× the cell
  size from `crossterm::terminal::window_size()`; if it reports 0 pixels,
  sixel is not chosen.
- Cache keyed by (kind, colour, pixel size); a theme change or resize
  misses it naturally.
- Sixel encoding: an icon is one colour over the background, so the
  antialiased edge is blended onto the theme's background and reduced to
  8 levels — a palette of 8, no general quantizer (the reason ADR 0118
  left sixel out does not apply).

## Placing

The ADR 0118 pattern, in `event_loop` after `terminal.draw`:

- For each big slot, if the completed frame's buffer still holds the
  rail's blank cells there (no menu, modal, palette or which-key painted
  over), move the cursor and emit the icon; otherwise skip it.
- Kitty: one image id per (kind, colour) from a range reserved for the
  rail, distinct from the viewer's; placements are repositioned, not
  retransmitted, when only the position changes.
- Sixel: re-emitted when the slot's (kind, colour, position) changed or
  the cells under it were repainted.
- Erasing goes through the four existing sites (loop, suspend, exit,
  panic) — `kitty_graphics::delete_placed` learns the rail's range.

## Errors

A probe that fails or times out is "no" (existing behaviour). A raster or
write failure for one icon logs at `debug` and leaves that slot blank for
the frame — it never falls back mid-session to a different width, which
would move the panes.

## Testing

- DA1 parsing: attribute 4 present / absent / malformed.
- `rail_layout`: big vs small by backend, style and height; mouse zones
  and painting take the same slots (the existing parity test, extended).
- Placement skips a slot an overlay covers (buffer check, no tty).
- Sixel encoder: a known 2×2 raster gives the expected bytes.
- `icon_svg` exists for every built-in kind the window draws, and the
  window's renderer reads the same files (a frontend test).
- Manual: kitty, foot (sixel), tmux (glyph), with the tmux harness.

## Phases

- **F1:** shared SVGs, `rail_layout`, `KittyGraphics` + `Glyph`.
- **F2:** `Sixel`.

Each phase merges on its own and degrades correctly at any point.
