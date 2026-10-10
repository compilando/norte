# Search panel, phase A: live, every match, the viewer at the line — design

- Date: 2026-10-09
- Status: approved in conversation, pending spec review
- Frontends: both. A new panel kind in the shared registry; the model lives
  in `norte-frontend`, the TUI paints it in cells, the window in the webview.
- Phases: **A (this spec)** panel + live search + every match highlighted +
  viewer at the line; **B** "Open as listing" (a shared virtual listing,
  replacing the TUI's Alt+F7 pane takeover); **C** history in the session
  and named searches in `searches.toml`. B and C get their own specs.

## Goal

Searching a tree today is a form and a list. In the window the form is a
dialog (ADR 0143) and the results a modal listbox of names; in the TUI the
results take over the focused pane. Neither shows WHERE a file matched beyond
one line, and the engine could not tell them anyway:

- `fs.search` stops at the first hit per file (`norte-core/src/search.rs`
  `search_content` 1030-1037, doc at 364); `MatchInfo` carries the line of
  that first hit and a preview that is the first 160 chars of the line
  (`trim_preview` 1313), so a match at column 200 is not visible.
- No column or range: nothing to highlight.
- The viewer cannot open at a line or highlight anything, and it reads only
  the first 256 KiB (`VISOR_CAP`, `VIEW_CAP`).
- The window does not cancel the previous search when a new one starts
  (`controller/search.rs` `launch_search` 302 overwrites `self.search`; the
  old walk keeps running, its batches dropped by epoch). Harmless with a
  dialog; fatal with search-as-you-type.

Success: a **Search** panel in the rail of both frontends. Typing searches
after a 300 ms pause, cancelling the previous run. Results arrive live as a
tree folder → file → matching lines, each line with the match highlighted
and the preview centred on it. A click (Enter) on a line opens norte's viewer
at that line with the match highlighted; on a file, the pane goes to its
folder with the cursor on it.

## Decisions taken

| question | answer |
| --- | --- |
| where | a panel kind `search`, docked left by default (ADR 0173), in both frontends |
| when it searches | 300 ms after the last edit; Enter forces; the previous task is cancelled |
| matches per file | up to 50 shown, then a `+N more` row; the request asks for 51 |
| protocol | 0.85 → 0.86, additive: `max_matches_per_file`, `MatchInfo.ranges`, `MatchInfo.offset`, `SearchHits.all_matches`, `SearchHits.more` |
| old daemon | returns one match per file, no ranges: the panel shows it unhighlighted |
| preview | ~160 chars, a window CENTRED on the first range of the line |
| the viewer | opens at a line with ranges highlighted; a line past the 256 KiB cap loads the window that contains it |
| root | the focused pane's folder when the panel opens; shown, with a button to re-root at the current one |
| the task board | the panel owns ONE entry, replaced on relaunch, not one per keystroke |
| Alt+F7 / the dialog | stay as they are until phase B |
| wire | protocol 0.86; bridge 107 → 108 (or next free) |

## Architecture

### 1. Engine and protocol — `norte-core/src/search.rs`, `norte-proto`

- `FsSearchParams.max_matches_per_file: Option<u32>` (`skip_serializing_if`,
  default `None` = today's behaviour, 1). Capped by the daemon at 1000.
- `MatchInfo` gains:
  - `offset: Option<u64>` — byte offset of the START of the matching line in
    the file (what the viewer needs to load a window past its cap);
  - `ranges: Option<Vec<(u32, u32)>>` — `[start, end)` in CHARS of
    `preview`, after trimming and masking.
- `SearchHits` gains `more: Option<Vec<u32>>`, aligned with `entries`: how
  many further matches a file had beyond the cap (0 when none). Additive,
  keeps the 1:1 alignment of `matches` (which stays `Option<Vec<MatchInfo>>`
  of the FIRST match, for old clients) and adds
  `all_matches: Option<Vec<Vec<MatchInfo>>>` aligned the same way, present
  only when `max_matches_per_file` was sent.
- Byte path (`scan_bytes` 1100): keep scanning after a hit until the cap;
  the hit position gives the in-line byte column, converted to chars over
  the decoded line. Decode path (`scan_decode_lines` 1198): `find_iter`
  instead of `is_match`. `whole_word` and `case_sensitive` apply as today.
- `trim_preview`: centre a ~160-char window on the first range, `…` at the
  cut ends, ranges shifted; hazard masking unchanged (ranges computed AFTER
  masking so they index what is painted).
- Version: `PROTOCOL_VERSION = "0.86.0"` with its `# 0.86.0` rustdoc
  section; `golden_types.rs`, fixtures and `docs/schema/proto.schema.json`
  (ADR 0038) regenerated. An older daemon ignores the new field and the
  client degrades (one match, no ranges): no `Unsupported` refusal, because
  the result set is the same — only less detailed.

### 2. Shared model — `norte-frontend/src/search_panel.rs` (new)

- `SearchPanel { form: SearchForm, root: VPath, run: Option<Run>, tree: Vec<FolderNode>, cursor, collapsed: BTreeSet<VPath>, debounce_deadline: Option<Instant> }`.
- `FolderNode { dir, files: Vec<FileNode> }`, `FileNode { entry, matches: Vec<MatchLine>, more: u32 }`,
  `MatchLine { line: u64, offset: Option<u64>, preview: String, ranges: Vec<(u32,u32)> }`.
- Pure transitions: `edit(field, value, now)` sets the deadline;
  `due(now) -> Option<FsSearchParams>` fires once; `apply_hits(epoch, SearchHits)`
  groups by parent folder keeping arrival order of folders and sorting files
  by name inside; `rows()` flattens to `Row::{Folder{dir, count}, File{..}, Line{..}, More{n}}`
  honouring `collapsed`; `activate(cursor) -> Activation::{Toggle(dir), GoTo(entry), View{path, line, offset, ranges}}`.
- Status: `N in M folders`, `searching…`, `stopped at 2000 results`, errors —
  through the shared `search_status::Outcome`.
- The form is today's `SearchForm` (`norte-frontend/src/search.rs`) and its
  `params()` mapping (479); the panel adds `max_matches_per_file = 51`.
  An empty name AND empty content runs nothing and clears the tree.

### 3. Panel kind — `layout/kinds.rs`, `chrome/panelbar.rs`, `layout/docks.rs`

- `KindRegistry::builtin()` gains `search`; `default_dock` → `Left`.
- Rail button: shared SVG `assets/panel-icons/search.svg`, one-cell icon,
  `panelbar-search` Fluent key EN+ES, letter not repeated — the panelbar
  guards (`every_builtin_button_has_a_shared_svg` 513, `…one_cell_icon` 539,
  `every_panel_has_a_short_name_in_both_languages` 764, `letters_are_not_repeated`
  788) and the TUI's `every_builtin_kind_paints_a_picture_when_big`.
- Command `layout.search` (catalogue, Live, both frontends) toggles it; it
  appears in the menu bar under Find and in the palette. Bound in presets
  where a key is free (Ctrl+Shift+F in cua/vim/orthodox; Krusader keeps
  Alt+F7 for its dialog until phase B).

### 4. Debounce and cancellation — both frontends

- The frontends tick the model (TUI event loop timer; host's existing tick):
  when `due(now)` yields params, CANCEL the running task (`task.cancel`,
  fire-and-forget, `RemoteTaskCanceller`) and launch; batches carry the
  epoch, stale ones dropped.
- Host fix independent of the panel: `launch_search` cancels the previous
  task too (the dialog benefits).
- Task board: the panel's runs share one board entry (`TaskNew` once,
  then the entry is re-pointed at the new task id), so typing does not fill
  the strip or the processes panel, nor approach `MAX_LIVE_TASKS` (512).

### 5. Viewer at a line — `view/viewer.rs`, host `controller/viewer.rs`, TUI `viewer_open.rs`

- `ViewerTarget { line: u64, offset: Option<u64>, ranges: Vec<(u32,u32)> }`
  passed to `request_viewer_of` / `open_viewer`.
- Within the cap: read as today, `scroll = line - 1` (clamped), highlight
  the ranges on that line (re-found in the decoded line by the matched text,
  since preview ranges are relative to a trimmed window).
- Past the cap with an `offset`: read a 256 KiB window starting a few lines
  before `offset`; the viewer shows "lines from N" in its title and line
  numbers continue from the known line.
- The highlight survives scrolling and clears on Esc or a new file. Plugin
  styled previews (`open_visor` 387) skip the highlight.
- Host DTO: viewer view gains `highlight: Option<{row, ranges}>` (bridge bump).

### 6. TUI paint

- The panel draws: root line, name and content inputs with `[Aa] [ab] [.*]`
  toggles (F-keys as in the Alt+F7 dialog, and mouse), a `filters (n)` fold
  with the other fields, the status line, then the tree with `▸/▾`, counts,
  line numbers dim, matches in `Role::Mark`. Keys: Tab between inputs and
  tree, ↑/↓, ←/→ fold, Enter activate, Esc back to the pane.

### 7. Window paint

- `SlotView::Search(SearchPanelView)` (bridge bump), projected from the
  shared model; webview `render/searchpanel.ts`: real `<input>`s (typing
  goes to the host as field edits, the existing `UiAction::DialogField`
  shape reused or a sibling action), toggles as buttons, a collapsible
  filters section, the tree with `<mark>` ranges, rows clickable; virtual
  scrolling past ~500 rows. Repaint follows the menus' rule (in-place
  cursor moves, no rebuild per hover).

## Edge cases

- Old daemon (0.85): no ranges, one match per file, no `more`: lines shown
  without highlight, no `+N` row. No error.
- Binary files: today's engine rules; no preview → the file row only.
- A file with 10 000 matches: 50 lines + `+9950 more`; activating `more`
  opens the viewer at the 51st match's line.
- Lines longer than `LINE_MATCH_CAP` (1 MiB): matched truncated as today;
  ranges only inside the evaluated part.
- Regex invalid while typing: no launch, the field shows the error
  (`field_unreadable`), the previous results stay dimmed.
- Root deleted or unmounted while searching: the task fails; status shows
  the error; the tree keeps what arrived.
- The panel closed while running: the task is cancelled.
- Two windows/frontends: each panel owns its run; nothing is shared but the
  open-panels set (ADR 0170).

## Testing

- Engine: N matches per file in byte and decode paths, cap + `more`,
  ranges after masking and centring, UTF-8 multibyte and UTF-16 ranges in
  chars, `whole_word`/case, a match past column 160 visible in the preview.
- Protocol: golden types, schema, an 0.85 client against 0.86 (old shape),
  a 0.86 client against an 0.85 fixture (degrades).
- Model: debounce fires once, edit during the pause postpones, cancel on
  relaunch, epoch drops stale, grouping/order, collapse, `+N`, activation
  kinds, empty query clears.
- Viewer: open at a line inside the cap with highlight; past the cap via
  offset; plugin preview skips highlight.
- Host: dialog relaunch cancels the previous task (regression for the bug);
  one board entry across relaunches; SlotView goldens.
- TUI: snapshot of the panel with highlighted lines, keys, the rail guard.
- Webview: tree render with marks, inputs send edits, clicks, in-place
  cursor; panel kind guards.
- Help topic for the panel (EN+ES), help gate, i18n suites, CHANGELOG.

## Out of scope (phases B and C)

- "Open as listing" and the TUI's Alt+F7 pane takeover → B.
- Search history and `searches.toml` (+ adding it to the config watcher's
  `CONFIG_FILES`) → C.
- Replace-in-files.
