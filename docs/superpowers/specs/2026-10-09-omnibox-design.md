# One search box for places and commands (VS Code style) — design

- Date: 2026-10-09
- Status: approved in conversation, pending spec review
- Frontends: both. The model is shared (`norte-frontend`); the TUI and the
  window (`norte-ui-host` + webview) paint it.

## Goal

norte has two search boxes that do overlapping jobs:

- the **palette** (`app.palette`: Ctrl+P in five presets, **Alt+P in
  Krusader and Total Commander**) lists every command with its first chord;
- **go anywhere** (`app.goto`, Ctrl+G) mixes a typed path, history, popular
  places, favorites, connections, commands (without their chord) and the
  name index.

Neither ranks: the palette keeps substring matches and falls back to a
subsequence; goto keeps subsequence matches in source order. Neither shows
which letters matched. The palette never dims a command that cannot run
here (`enabled: true` always, `views.rs:601`). And in the window the search
box in the title bar (the "command center") only exists with the custom
title bar: with `[ui] titlebar = "native"` there is no box at all.

Success: ONE box, as in VS Code. Alt+P (Krusader) opens it with `>` typed and
it is the command palette; Ctrl+G or a click on the box opens it empty and it
is go-anywhere; deleting the `>` switches without closing. Results are ranked
by a fuzzy score with the matched letters highlighted, a command row shows
its category and its key, and a command that cannot run here is dimmed with
the reason. The window shows the box in the menu bar when the title bar is
native.

## Decisions taken

| question | answer |
| --- | --- |
| one box or two | one: goto absorbs the palette |
| how the mode is chosen | the query's first character: none = places, `>` = commands, `?` = help |
| what each key opens | `app.palette` → box with `>`; `app.goto` → box empty |
| prefixes in this batch | `>` and `?` only (`@` tabs and `#` index files: out of scope) |
| matching | an own fuzzy scorer in `norte-frontend` (no new dependency) |
| command rows | `Category: Name ······ chord`, dimmed + reason when not runnable here |
| category | the command id's namespace (`pane.` → Panel …), translated |
| window box | in the menu bar when the title bar is native; unchanged with the custom one |
| wire | `norte-proto` untouched; bridge 107 → 108 |

### Why a scorer of our own

`nucleo-matcher` (Helix's) is MPL-2.0, which `deny.toml` does not allow, and
the job is small: a subsequence match scored with fzf's v1 bonuses fits in a
module with its tests. It also returns the matched positions, which is what
the highlight needs and what `is_subsequence` cannot give.

### Why the namespace and not the menu bar for the category

`chrome/menu.rs` `MENUS` puts each command in at most one menu, but only 112
of the 179 commands are in a menu. The id's namespace covers every command,
is stable, and already reads as a group (`pane.`, `mark.`, `nav.`,
`layout.`, `terminal.`, `app.`, `viewer.` …). One Fluent key per namespace.

## Architecture

### 1. Fuzzy scorer — `crates/norte-frontend/src/fuzzy.rs` (new)

```rust
pub struct Match { pub score: i32, pub positions: Vec<u32> }
pub fn score(query: &str, candidate: &str) -> Option<Match>;
```

- Case-folded through `nav::fold` (accents and case), positions in CHARS of
  the original candidate.
- A subsequence is required; score = sum of per-char points with fzf v1
  bonuses: start of the candidate, start of a word (after space, `.`, `-`,
  `_`, `/`, `:` or a lower→upper change), consecutive with the previous
  match; a penalty per gap char. An exact-case match of a char gets a small
  bonus. Ties: shorter candidate first.
- Greedy left-to-right, then one backward pass to tighten the window (fzf
  v1). No DP: candidates are short (≤ ~120 chars) and lists are ≤ a few
  hundred rows.
- Empty query → `Some(Match { score: 0, positions: [] })`.

### 2. Model — `navigation/goto.rs` becomes the omnibox

- `Mode { Places, Commands, Help }` derived from the query: `>` → Commands,
  `?` → Help, anything else → Places. The prefix is part of `query` (so
  backspace over it changes mode); matching uses the query without it and
  without leading spaces.
- `GotoRow` gains `chord: Option<String>`, `category: Option<String>`
  (already translated), `unavailable: Option<String>` (the translated
  reason), `recent: bool`, `positions: Vec<u32>` (filled by `refresh`, in
  chars of `text`).
- `refresh()` ranks with `fuzzy::score` over `text` (and over `desc` with a
  lower weight for commands, so a word in the help line still finds it).
  Places keeps its sections, ranked inside each section, capped at
  `CAP_PER_SECTION`. Commands is one flat list ranked by score; with an
  empty query, recents first (session `palette_recent`), then catalogue
  order. Help lists the prefixes first, then help topics by title.
- Places mode NO LONGER has the commands section: commands live behind `>`.
  An empty Places box shows a hint line `> commands · ? help` (one Fluent
  key) so the change is discoverable.
- What `Palette` does moves here and `overlays/palette_state.rs` is deleted:
  recents (`with_recent`), plugin rows (`extend_rows` → a `replace_section`
  on the commands list, query kept), page up/down, `selected()`. The row
  builders in `overlays/palette.rs` (`plugin_rows*`, `first_chord`,
  `rows_for_context`) stay and feed the Commands source.
- `Action` gains `SetQuery(String)` (a `?` row that inserts a prefix) and
  `Help(topic_id)` (a topic row). A command row that is unavailable returns
  `Nothing(reason)` instead of running.
- Activating a command (palette or goto, any frontend) calls
  `note_palette_recent`.

### 3. Availability and category

- `unavailable` comes from `availability::verdict(cmd, &facts)`, as the
  context menu already does (`context_menu.rs:512`): TUI facts from
  `app/caps.rs` `help_facts()`, host from `controller/help.rs` `facts()`.
  Plugin rows are never dimmed. The read-only window's dropping of mutating
  commands stays as is.
- Category: `cmd-ns-<namespace>` Fluent keys (EN + ES) for every namespace
  in the catalogue; a test walks `CATALOGUE` and fails on a namespace with
  no key. Plugin rows keep their `[label]` and get no category.

### 4. Keys

- `app.palette` opens the box with `>`; `app.goto` opens it empty. Both
  commands stay (presets, menu entries and docs keep working). Opening it
  while it is open switches the mode in place.
- Inside: typing, Backspace, ↑/↓, PgUp/PgDn, Home/End, Enter, Esc, and F1 on
  a command row opens its help topic (today TUI-only, `palette_help`; the
  window gets it too).
- Paste goes into the query in both frontends (TUI `paste.rs:124` today
  only for the palette).

### 5. TUI

- `dispatch.rs` `AppPalette`/`AppGoto` both open the same overlay with the
  initial query. `draw_palette` is removed; `draw_goto` paints the three
  modes: matched chars in `Role::Mark`, a recent dot, category dim before
  the name, chord right-aligned, unavailable rows dimmed with the reason on
  the status line when selected.
- Snapshots `snapshot_palette_open`, `snapshot_palette_hostile_plugin_row`
  and `snapshot_modal_paints_over_the_palette` are re-taken through the new
  overlay.

### 6. Window

- Host: `PaletteView` and `Effect::Palette`'s own controller go away;
  `Effect::Palette` opens goto with `>`. `GotoLineView::Row` gains `chord`,
  `category`, `unavailable`, `recent`, `positions`; `GotoView` gains `mode`
  and `hint`. Rows become clickable: `UiAction::GotoPointRow { row }` and
  `GotoActivateRow { row }`, resolved against what the host has open (the
  menu bar's rule). `key_in_palette`'s hard-coded page height (10) goes with
  it; PgUp/PgDn use the painted height.
- Bridge 108 (doc entry above `BRIDGE_VERSION`, mirrored in `types.ts`,
  `catalog.rs`, `wire_catalogue.rs`).
- Webview: `paintPalette` is removed; `paintGoto` paints the modes with
  `<mark>` around matched chars, category, chord, `data-unavailable` with a
  `title` holding the reason, and rows sending point/activate on hover and
  click (only on a row change, as the menus now do).
- The box: with `data-titlebar="custom"` it stays in the title bar. With the
  native title bar it goes into the menu bar between the last menu title
  and `.menubar-actions`, if the menu bar is shown (`[ui] menu_bar`). Label
  `Search places and commands (>)` + the chord of `app.goto`; click opens
  the box empty. Narrow window: the label goes first, then the box shrinks
  to the icon.

## Edge cases

- `>` typed in Places mode at position 0 switches mode; a `>` later in the
  query is a literal character.
- A typed path (`/`, `~`, `scheme://`) in Places keeps its row at the top,
  unranked, as today (`PathSource` is `ya_filtered`).
- The async index section keeps its own minimum (3 chars) and is ranked on
  arrival through `replace_section`.
- Plugin rows arrive late (window): the query and the cursor row survive.
- A command removed by hot reload while the box is open: activation
  resolves against the current catalogue; gone → `Nothing`.
- Help mode with no topic in the active language falls back as
  `norte_help::topic` already does.

## Testing

- `fuzzy`: word-start beats mid-word, consecutive beats scattered, exact
  case bonus, positions are char indices with accents/emoji, no match →
  `None`, empty query.
- Model: mode by prefix and backspace over it; Places has no commands
  section and shows the hint; Commands ranked, recents first on empty query;
  unavailable row → `Nothing(reason)`; `?` rows → `SetQuery`/`Help`; plugin
  rows merged keeping the query.
- Category: every catalogue namespace has an EN and ES key.
- TUI: Alt+P in the krusader preset opens with `>`; Ctrl+G empty; snapshot of
  each mode with highlighted chars.
- Host: `app.palette` → GotoView mode Commands with query `>`; click
  activate; dimmed row with reason; goldens `changes.json`/`updates.json`
  re-blessed.
- Webview: highlight marks, dimmed row title, box in the menu bar with
  `data-titlebar="native"`, not duplicated with `custom`.
- Help gate and i18n suites; help topics that describe the palette or goto
  (`grep -l palette crates/norte-help/topics`) updated.

## Out of scope

- `@` open tabs and `#` files from the index as prefixes.
- Frecency beyond the existing recent-commands list.
- A box in the TUI's chrome (it has no menu bar to hold one).
