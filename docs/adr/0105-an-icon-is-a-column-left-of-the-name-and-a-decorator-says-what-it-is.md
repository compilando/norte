# 0105 — An icon is a column left of the name, and a decorator is told what an entry is

- Status: accepted
- Date: 2026-09-10
- Decision makers: Oscar González
- Related: ADR 0037 (plugin data-out v2: decorators are data the host paints),
  ADR 0077 (a presentation decision is taken once, in the shared layer), ADR
  0094 (a WIT package bumps when one of its interfaces changes; a guest built
  against another version is listed as such), ADR 0104 (the extension
  manager)

## Context

The `file-icons` decorator was shipped as "a badge per row saying what kind
of file it is". Seen on a real listing it was wrong in three ways at once:

1. **The glyph was painted after the name**, in the place git-status paints
   its `M`. An icon that trails the file name is not an icon; it is
   punctuation. The reader expected what every file manager with icons does:
   a fixed-width column to the LEFT of the name.
2. **Directories had no icon.** A decorator received the basenames of the
   page and nothing else. `main.rs` got a crab; `/bcds` got nothing, because
   no name says "folder". A folder icon is the one icon the reader most
   expects.
3. **One row, one glyph.** `merge_decorations` kept the first plugin's badge
   for a path and dropped the rest, a documented MVP decision from G3b. With
   `file-icons` and `git-status` both consented, whichever came first in the
   catalogue silenced the other. The two say different things — what a row
   IS, and what STATE it is in — and there was no way to show both.

Also found on the way: the table did not know spreadsheets or slides.

## Decision

**1. An icon and a badge are two SLOTS of a row, not two candidates for one.**
The icon slot is a fixed-width column left of the name (two cells and a
separator in the terminal; a three-cell span in the window); the badge slot
is the git-status place right of the name, unchanged. A row can carry one of
each, from two different plugins. Within a slot the first plugin in catalogue
order wins, as before.

The slot is declared by the **manifest**, not chosen by the guest:
`[[contributions.decorator]] slot = "icon"` (default `badge`). Where a glyph
is painted is a decision about the screen, and the manifest is what the
human approved; it enters the approval digest only when it is `icon`, so no
existing decorator changes its anchor. It travels to the frontend as
`PluginDecorations::slot` (protocol 0.72.0, additive, absent = `badge`).

**2. The column opens for the whole listing, or not at all.** If any visible
row has an icon, every row gets the column — empty where there is no icon —
so the names stay aligned; the terminal shifts its "Name" header by the
same width. If no row has an icon, nothing changes: a listing without an
icon decorator looks exactly as it did. Decided by the pane
(`PaneState::any_icon`) and by the renderer per slot, not per row.

**3. A decorator is told what an entry is.** `norte:plugin` 0.9.0 → 0.10.0:
`decorate` receives `list<entry>` — `{name: list<u8>, kind: entry-kind}` —
instead of `list<list<u8>>`, with `entry-kind` = file, dir, symlink, other.
The kind comes from the listing the frontend already holds and travels as
`PluginDecorateParams::kinds`, positional with `paths`; a client that does
not send it (0.71) gets `other` for every entry, which a guest treats as a
file — so a folder goes without icon, and nothing breaks.

This is an interface change and therefore a package bump (ADR 0094): every
guest of the package is rebuilt (`just plugins force`) and re-approved in the
manager, and a guest of another version is listed as such with both
versions. The embedded FTP provider is rebuilt with it. The alternative —
smuggling the kind in the name, or letting the host invent a folder glyph
when the icon plugin says nothing — was rejected: the first corrupts a name,
the second is the host choosing a plugin's glyphs.

**4. `file-icons` becomes what its name says.** `slot = "icon"`; a folder
gets the folder icon whatever it is called, except the few names that mean
more (`.git`); a symlink gets the link icon whatever it points at, because
the listing shows the link; a file goes by its name, and the table gains
spreadsheets, slides and office documents. The `ascii` style keeps one or two
characters per kind.

## Consequences

- WIT `norte:plugin` 0.10.0; protocol 0.71 → 0.72, additive, N/N-1 window
  documented; bridge 61 → 62 (`RowView.icon`, `icon_hostile`). Goldens,
  schema and snapshots regenerated; the daemon, embedded backend, SDK, both
  frontends and the fake backend carry the kinds.
- The shared merge keeps one `Decoration` per path with both slots; the
  masking and the eight-character cap apply to the icon exactly as to the
  badge, and an icon carries no theme role — it is painted in the entry's
  own colour, because it says what the row is, not what state it is in.
- The terminal reserves three cells; an icon wider than two is cut to two,
  because the column is what keeps the names aligned. The window's cell is
  three character widths and hides overflow for the same reason.
- Whether the column is open is decided ONCE, in the host, from the pane's
  decorations (`any_icon`), and travels to the window as
  `BrowserSlotView::icon_column`; the renderer does not re-derive it from
  the visible rows, or scrolling into a page without icons would close the
  column and shift every name (ADR 0077).
- Three emoji in the table (`🖼`, `⚙`, `📽`) are text-presentation by default:
  `unicode-width` measures them at one cell and a terminal paints two. They
  carried VS16 (U+FE0F) until #374: terminals disagree on a VS16 cell, and a
  row painted one cell off until its next repaint. They are now `📷`, `🔩`
  and `📈`, wide on their own, and a test pins every emoji to ONE codepoint
  of two cells.
- The terminal paints an icon that was masked without a mark, as it already
  did for a masked badge; the window appends its `△`. Written down here as a
  known asymmetry, inherited rather than opened.
- Not done: a per-extension icon in the manager (manifests carry none), and
  icons in the tree side panel and the pickers, which list names through
  other paths.
