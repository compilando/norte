# 0173 — Panels open at their normal place; a timeline row shows its details

- Status: accepted
- Date: 2026-10-08
- Decision makers: Oscar González
- Protocol: unchanged. Bridge 104 (`TimelineRowView.seq`,
  `timeline_show_row { slot_id, seq }`). Session gains `panel_docks` (serde
  default, old sessions read fine).
- Related: ADR 0170 (open panels shared), 0171 (dock caps), 0121
  (timeline), 0134 (panel groups)

## Context and problem statement

A study of where each panel opened (2026-10-08) found the two frontends
disagreeing. The terminal put the timeline, the terminal and the disk
map at the bottom; the window's table listed five kinds and sent every
other one to a 30-cell column on the right, so the timeline — a list of
long lines — came out tall and narrow. And a panel the reader moved
forgot it the moment it closed.

The timeline row, too, shows only the time, the verb and the path tail:
who exactly, the full date, the destination, the batch and whether undo
will touch it were nowhere.

## Decision

1. **One table, both frontends** (`layout::default_dock`), after VS
   Code: left for what you navigate with (places 16, tree 24); right for
   what describes the cursor (viewer at half, details 30); bottom for
   lists and drawings (processes 8; log, terminal, timeline, disk map
   12). An unknown kind (a plugin's) keeps the old right column.
2. **Memory beats the table.** Where each kind was last
   (`layout::docks_in`, read back from the tree before it changes) is
   saved in the session per frontend — the terminal under the profile
   key, the window under its own `@window` key, as layouts already are
   (ADR 0139) — and `dock_for` uses it. The default applies only to a
   kind the reader never placed. Only a real dock is remembered: a
   panel tabbed with a listing, or in a weighted split beside one, sits
   where the listing does and has no place to remember (it would reopen
   at half the screen). A bottom or top size, remembered or not, is
   capped against a short screen on opening (ADR 0171).
3. **A timeline row's details**: Space (like Quick Look) or a click opens
   a read-only report built by `timeline::details` — date and time, who,
   what, both paths whole, the batch, whether undo brings it back and why
   not, and the journal number. Enter keeps meaning "undo back to here".
   The window's click names the slot and the row by `seq`, not position:
   the list grows at the top. Each path goes alone on its line as a
   `ReportLine::Path`, and the journal's own text (verb, actor) goes
   through `display_name`: an agent writes it. In the window the key is
   the preset's mark key, which is Space in five presets and Insert in
   far and norton; no new binding. A report the reader opened takes its first answer
   (`open_report(.., asked = true)`); one that opens on its own still
   swallows a type-ahead key.

## Alternatives not taken

- **Fixing only the window's table.** Leaves the memory gap, and the
  next kind added to one table would drift again.
- **Per-row details travelling in every `TimelineRowView`.** Ten lines
  per row for a popup opened once; the host builds them on demand.
- **Double click only.** A single click in a read-only list has nothing
  else to do; double click stays harmless (the second opens the same).

## Consequences

- A session written by this version carries `panel_docks`; older
  versions ignore it.
- Sizes are remembered with the edge (a resized bottom dock comes back at
  its size).
- "Where the reader left it" is really "where it last was": a layout
  picked from the presets also updates the memory.
