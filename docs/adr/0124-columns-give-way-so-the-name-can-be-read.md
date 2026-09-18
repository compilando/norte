# 0124 — Columns give way so the name can be read

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged at 73 — no DTO field moves; the
  window receives fewer columns and shorter cell text, both of which it
  already accepted.
- Related: #108 (the shared column layout), ADR 0077 (the same rule in both
  frontends), ADR 0107 (column widths by dragging)

## Context and problem statement

`columns::layout` gave every column its width and handed the name whatever
was left. It dropped a column only when not even the name's floor
(`NAME_MIN`, 10 cells) fitted. In the case that actually happens — two panes
of ~50 cells, with Type, Size and Date — the name kept 14–16 cells and every
screenshot read `Ca….png`. The listing still showed its type, which the icon,
the colour and the `/` already say.

The layout never looked at what was in the directory, so it had no way to tell
"there is room" from "there is room to read".

## Decision

**1. The name has a target.** It is the width that covers 80% of the names in
the listing, plus what goes before the name in a row (gutter, badge, icons).
The target is capped at 3/5 of the pane, so one enormous name does not strip
the rest. It is the 80th percentile rather than the maximum for the same
reason.

**2. The other columns give way, one rung at a time, and stop as soon as the
name reaches its target:**

1. hide Type (the icon, colour and `/` already say it),
2. short date: `22:19` today, `09-16` this year, `2025` before,
3. short size: `80K`, `1.3M`, `512B`,
4. hide Date,
5. hide Size.

A wide pane climbs no rungs. The short formats (`SizeFormat::Short`,
`TimeFormat::Short`) fit in five cells and are not configuration words: only
the fitting puts them there.

**3. What the user touched does not move.** A column with a `width` in its
spec (what dragging a border writes) is not hidden or shortened. A column with
a `format` is not shortened. A name with a fixed width switches the whole
ladder off. `attr:` and `plugin:` columns are not on the ladder, because
someone asked for them on purpose.

**4. One rule, two painters.** `norte_frontend::columns::fitted_columns` is
pure and shared. The terminal calls it from `pane_columns`, which is also where
the mouse finds column borders: two computations would let a drag grab the
neighbouring column. The window host calls it from `ajuste_de`, with the cell
width the layout gives the slot, and builds the header and every row of a batch
from the same result.

**5. Measure when the listing changes, not when painting.** `PaneState` keeps
the 80th percentile, recomputed in `listing_moved` from a 65-bucket histogram.
It allocates nothing for a UTF-8 name. Measuring per frame would mean walking
the whole directory on every key, which is the mistake the idle-CPU fix
removed.

**6. In the window, header and rows travel together.** The fit changes on
paths that send no header (a paged fill, a resized slot). `parche()` compares
each slot's fit with the last one sent, as it already does for the panel bar.
When it differs, it sends the header AND the rows. A cell whose header has gone
would be painted with no width.

## Consequences

- The screenshot case: the left pane shows full names with Size and Date; the
  right one shows full `Captura de pantalla ….png` names with a short date.
- The window's own pixel-based drop (`descartarLasQueNoCaben`) stays as a
  safety net. The host now removes columns before it would have to.
- An empty listing has no names to read, so it keeps all its columns.
- The short formats are a candidate for the configuration vocabulary
  (`format = "short"`), which this ADR does not add.

## Alternatives considered

- **Raise `NAME_MIN`.** One number for every directory: too much for `bin`,
  too little for screenshots.
- **Drop from the right, as before, only earlier.** Removes Type in the
  screenshot case, but still loses the date before trying a shorter one, and
  in a pane that lists Type last it happens to be right only by accident.
- **Let the renderer decide in the window.** It knows the pixels, but then
  the terminal and the window would fit by two rules (ADR 0077), and shortened
  text has to be formatted where cells are formatted — in the host.
