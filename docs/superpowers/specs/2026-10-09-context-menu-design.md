# Window context menu (right click) — design

- Date: 2026-10-09
- Status: approved in conversation, pending spec review
- Frontend: the Tauri window (`norte-gui-tauri` + `norte-ui-host`). The TUI
  gets the shared model and the catalogue entry, not a menu.

## Goal

The right button does nothing useful in the window today:

- On a listing row, `render.ts` (scroller `mousedown`) does not look at
  `e.button`: a right click moves the cursor, Shift/Ctrl+right click mark,
  a right click on the mark box toggles the mark, and **two right clicks in
  quick succession count as a double click and enter the directory**.
- Nothing calls `preventDefault` on `contextmenu`, so WebKitGTK's own menu
  (Back / Reload / Inspect) opens on top.
- Only a tab (`menus.ts`, `openTabMenu`) and the terminal list
  (`terminal.ts`) have a menu of their own.
- The help topic `mouse.md` ("The right-click menu") and the CHANGELOG still
  describe the GPUI frontend's row menu, retired in `f6bb98b8`. The window
  promises something it does not do.

Success: a right click anywhere a reader would expect a menu — a row, the
empty part of a listing, a column header, a Places row, a tree branch — opens
OUR menu at the pointer with what can be done there, every entry running the
same thing its key runs. Shift+F10 / the Menu key open the same menu from the
keyboard. The help page is true again.

## Decisions taken

| question | answer |
| --- | --- |
| who owns the menu | the HOST (state, keys, resolution); the renderer paints |
| what entries exist | a pure shared model, `norte_frontend::context_menu` |
| whether an entry can run | `norte_frontend::availability::verdict`, unchanged table |
| composition | fixed CORE in fixed order, dimmed with its reason; CONTEXTUAL sections appear only when they apply |
| marks | kept from GPUI: marked row → acts on the marks; unmarked row → **drops the pane's marks**, cursor to it |
| keyboard | new command `pane.context-menu` (Shift+F10, Menu key); ↑↓ Enter Esc inside |
| surfaces | listing row, empty area (and `..`), column header, Places row, tree branch |
| stale check | a target FINGERPRINT at open, compared at run (not the listing generation) |
| wire | `norte-proto` untouched; bridge bumps to 107 |
| TUI | catalogued command answers "not available here"; no menu in this batch |

### Why the host and not the renderer

Every key already goes to the host (`main.ts` sends them all); a menu whose
arrows were resolved in TypeScript would be the one surface where they are
not. The host also already has the pattern: the menu bar keeps `self.menu`,
projects `MenuView` by patch and resolves a click against what IT has open
(`activate_from_menu`), so a row that changed between painting and clicking
runs nothing.

### Why core + contextual, not "everything always"

GPUI's rule was "every entry always appears, only availability changes". With
the window's ~20 candidate commands that is a menu mostly grey. The core keeps
GPUI's rule — same entries, same order, on every row, so the hand learns it —
and the extras appear only where they mean something.

### Why a fingerprint, not the generation

A big directory keeps landing filler batches, each bumping the listing epoch.
Refusing on any epoch change would make the menu useless while a directory
loads. What must not change is WHAT the menu said it acts on: the entry under
the cursor and the number of marks.

## Architecture

### 1. Shared model — `crates/norte-frontend/src/context_menu.rs` (new)

Pure, no I/O, no host types. Decides WHICH entries a surface has; never
whether they run, never labels.

```rust
pub enum Surface {
    /// A listing row. `target` is what the menu acts on, already decided.
    Row(RowTarget),
    /// The listing below its last row, or the `..` row.
    Empty { remote: bool },
    /// A column header. `hideable` is false for `name`. The column id
    /// itself lives in the host's `Subject::Column`, not in the model.
    Header { hideable: bool },
    /// A Places row.
    Place(PlaceTarget),   // Drive | Favorite | SectionHeader
    /// A tree branch. Fold/unfold is one toggle entry, so no state here.
    Branch,
}

pub struct RowTarget {
    /// How many entries: 1 for an unmarked row, n for the marks.
    pub count: usize,
    /// The kind of the single entry; `None` when count > 1.
    pub kind: Option<EntryKind>,
    /// The single entry is an archive this frontend can compose
    /// (`norte_frontend::nav::archive_root_for(e).is_some()`, the table
    /// both frontends already share for Enter and `pane.unpack`).
    pub archive: bool,
    /// All marked entries are regular files (for "compare files").
    pub all_files: bool,
}

pub enum Action {
    /// A catalogue command, dispatched through `effect_of` like its key.
    Command(&'static str),
    /// Not a pane command: a verb on the clicked thing.
    Verb(Verb),
}

pub enum Verb {
    OpenHere, OpenInOther, OpenInNewTab, CopyPath,
    AddFavorite, RemoveFavorite, ToggleFold,
    SortByColumn, HideColumn,
}

pub struct Entry {
    pub action: Action,
    /// Fluent key of the label.
    pub label_key: &'static str,
    /// Fluent key of the section this entry STARTS, `Some("")` for a bare
    /// rule, `None` to stay in the previous one (same shape as the menu bar).
    pub section: Option<&'static str>,
}
// The role (`normal` | `destructive` | `ai`) is NOT in the model: the host
// derives it per command with `norte_frontend::menu::role(id)`, the menu
// bar's own source (ADR 0126); verbs are `normal`.

pub fn entries(surface: &Surface) -> Vec<Entry>;
```

`Open` on a row is ONE entry whose command depends on the kind: `nav.enter`
for a directory, symlink or archive, `pane.open` for anything else. The
entry is still in the same place on every row.

### 2. Host — `crates/norte-ui-host/src/controller/context_menu.rs` (new)

State on `State`:

```rust
pub(super) struct ContextMenu {
    surface: Surface,
    entries: Vec<Entry>,
    /// Frozen at open: the verdicts do not change under the pointer.
    facts: Facts,
    cursor: usize,
    /// The slot it was opened on (listing, places or tree).
    slot: u32,
    /// Row surfaces only: what it said it acts on.
    fingerprint: Option<Fingerprint>, // { cursor_entry_name: Vec<u8>, marks: usize }
    /// Places/tree/header: the row or column it was opened on.
    subject: Subject,
    /// Pixel anchor from the renderer, or `None` from the keyboard.
    anchor: Option<(i32, i32)>,
}
```

**Open** (one action per surface: `context_menu_row`, `_empty`, `_header`,
`_place`, `_branch` — "an open action" below):

1. Validate the row/column against the generation the renderer painted
   (`row_of`, `gen_places`, the tree's generation). Stale → no menu.
2. Row surface — decide the target ONCE:
   - row marked → target = the marks; cursor moves to the row, marks kept;
   - row unmarked → **clear the pane's marks**, cursor to the row, target =
     that row. Irrecoverable, Esc included (the rule from GPUI and from
     `mouse.md`: every command prefers the marks, so keeping them would let
     the menu say "1" while the copy took eleven).
   - `..` → `Surface::Empty`, marks untouched.
3. Freeze `facts()` (after the cursor/marks change, so they describe the
   target), build entries, store the fingerprint.
4. Close any open menu-bar dropdown; refuse to open while
   `something_keeps_the_keys()` (a dialog, help) — same rule as `toggle_menu`.

**Project** (`vista_context_menu`) → `ContextMenuView` (see wire).
Each entry: label from Fluent, chord via `palette::first_chord` (empty for a
verb), `enabled` + `reason` from `verdict(command, &facts)` (verbs: always
enabled except `HideColumn` on a non-hideable column, reason
`reason-wrong-target`), section, role.

**Point / activate / close**: same shape as the menu bar.

- `context_menu_point_row { row }` moves the cursor only.
- `context_menu_activate_row { row }` resolves against the HOST's entries.
  A disabled entry: applied, nothing runs, the menu stays. A row surface
  whose fingerprint no longer matches the pane: close, `Stale`, nothing runs.
  Otherwise the CLOSE goes in its own patch FIRST, then the effect
  (`run_from_menu`'s order, for the same reason: the command may open a
  dialog that must get the keys).
- `context_menu_close` closes without running.

**Keys** (`key_in_context_menu`), routed before the listing while open:
Up/Down move (skipping nothing — disabled entries are walkable, as in the
menu bar, so their reason can be read), Enter activates, Escape closes,
anything else is swallowed. `something_keeps_the_keys()` includes it.

**Closes on its own** when any modal opens (dialog, help, palette, menu-bar
dropdown) and when the slot it was opened on goes away.

**Verbs** (host functions, all reuse existing paths):

| verb | does |
| --- | --- |
| `OpenHere` | what a click on that Places row / branch does today (`activate_place`, `TreeActivateRow`) |
| `OpenInOther` | navigates the DESTINATION slot (`slot_dest()`) to the target, `Trail::Record`; none designated → the existing `host-no-target-designated` refusal |
| `OpenInNewTab` | `tab_new` on the focused listing, then navigate the new slot |
| `CopyPath` | `NativeEffect::CopyBytes` with `clipboard_bytes(&[path])`, same message as `copy_paths` |
| `AddFavorite` | opens the existing name dialog (`request_favorite_of`, prefilled with `suggested_hotlist_name`) like the picker, rather than saving directly: a direct save would silently replace a same-named favorite |
| `RemoveFavorite` | the body of `remove_favorite`, taking the name from the Places row instead of the selector |
| `ToggleFold` | `toggle_fold` (Places header) / `TreeToggleRow` (branch) |
| `SortByColumn` | `sort_by_column` |
| `HideColumn` | the column selector's apply path (`columns.apply_picked` with the slot scheme's ids minus that one, then `re_list` where the footprint changed) — window-only, like the selector: `norte.toml` is not written |

### 3. Catalogue — `pane.context-menu`

New command in `norte_frontend::keymap::catalogue`, default chords
`shift+F10` and the `ContextMenu` key, in every preset that binds listing
keys. Host: in `IMPLEMENTED`, `Effect::ContextMenu`, which opens on the
FOCUSED thing — the cursor row of the active listing (marks rule applies:
cursor row unmarked → marks dropped), the Places cursor row, or the tree
cursor branch. TUI: catalogued, not implemented → the standard "not here"
phrase. Fluent strings in both locales. The `keys.ts` key name for the Menu
key must reach the host as a chord the keymap can parse.

### 4. Wire — bridge 107

`UiAction` (renderer → host):

```ts
| { action: "context_menu_row"; slot_id: number; key: number; generation: number; x: number; y: number }
| { action: "context_menu_empty"; slot_id: number; x: number; y: number }
| { action: "context_menu_header"; slot_id: number; column: string; x: number; y: number }
| { action: "context_menu_place"; row: number; generation: number; x: number; y: number }
| { action: "context_menu_branch"; row: number; generation: number; x: number; y: number }
| { action: "context_menu_point_row"; row: number }
| { action: "context_menu_activate_row"; row: number }
| { action: "context_menu_close" }
```

`ViewChange::ContextMenu { context_menu: ContextMenuView | null }` and
`ViewSnapshot.context_menu: ContextMenuView | null` (optional on read: an
earlier host does not send it).

```ts
interface ContextMenuView {
  /** "acts on: photo.zip" / "acts on: 11 marked" / the directory / the column. Masked, elided at 40 chars. */
  header: string;
  items: ContextItemView[];
  cursor: number;
  /** Pixels; null = anchor to the focused row's element (keyboard). */
  x: number | null;
  y: number | null;
}
interface ContextItemView {
  label: string;
  chord: string;
  enabled: boolean;
  /** Why not, already translated; "" when enabled. */
  reason: string;
  section: string | null;
  role: string;
}
```

The `bridge.rs` version note for 107 lists all of the above.

### 5. Renderer

- `render/contextmenu.ts` (new) paints `ContextMenuView` with the existing
  `.tab-menu` look (`popupMenu`'s keep-inside-the-window measuring, extracted
  so both use it). Section titles and rules like the menu-bar dropdown; a
  disabled entry dimmed with its reason as a second, smaller line.
  Hover → `context_menu_point_row`; click → `context_menu_activate_row`;
  press outside, window blur, resize, scroll of the slot → `context_menu_close`.
  Escape and arrows are NOT handled here: they are keys and go to the host.
- `x/y == null`: anchor under the element of the focused row / place / branch.
- **One** document-level `contextmenu` listener calls `preventDefault`
  unless the target is an `<input>` or `<textarea>` (those keep the native
  cut/copy/paste). The specific listeners (row, empty area, header, Places,
  tree) send their open action. The tab and terminal menus stay as they
  are (renderer-local, out of scope).
- Listing `mousedown`: `e.button !== 0` returns before select/mark/
  double-click counting. Side buttons keep their `mouseup` history handler.
- While the host menu is open, the renderer's own `popupMenu` (tabs,
  terminal) is closed, and opening one of those closes the host menu.

## Entries

`│` is a section rule. Labels are the existing `menu-item-*` keys where one
exists; new keys are named `ctx-*`.

### Listing row — core (always, in this order)

Open (`nav.enter` / `pane.open`) · View `pane.view` · Edit `pane.edit` │
Copy `pane.copy` · Move `pane.move` · Rename `pane.rename` · Delete
`pane.delete` │ Copy path `pane.copy-path` · Properties `pane.properties`

### Listing row — contextual (only when it applies)

| section | when | entries |
| --- | --- | --- |
| Archive | single target, `archive` | Extract `pane.unpack`, Test `pane.test-archive` |
| Directory | single target, a directory | Size `pane.dir-size`, Compare with other pane `pane.compare-dirs`, Synchronize `pane.sync-dirs` |
| Selection | `count >= 2` | Batch rename `pane.rename-batch`; Compare files `pane.compare-files` only when `count == 2 && all_files` |
| Pack | target is not an archive | Pack `pane.pack` |
| More | always | Checksum `pane.checksum`, Permissions `pane.chmod` |

AI rename, Organize and Disconnect are NOT on the row: they act on the whole
folder or connection, and a row menu would claim a scope they do not have.

### Empty area (and `..`)

| section | entries |
| --- | --- |
| Folder | New folder `pane.mkdir`, New file `pane.edit-new` |
| View | Refresh `pane.refresh`, Show hidden `pane.toggle-hidden`, Sort… `pane.sort-menu`, Columns… `pane.columns` |
| Marks | Mark all `mark.all`, Invert `mark.invert` |
| AI | AI rename `pane.ai-rename`, Organize `pane.organize` |
| Connection (only if the pane is remote) | Disconnect `pane.disconnect` |

The AI section is always present: the host has no local "AI is configured"
fact (the daemon decides when asked), so a missing provider is refused by the
dispatch exactly as from the key. The section never runs anything by itself:
each entry is the reader asking (AI stays opt-in, user-triggered).

"Remote" = the pane's BASE scheme (the part after the last `+`) is not
`file`: `sftp`, `s3`, `zip+sftp` are remote; `file`, `zip+file` are not —
the same test `disconnect` makes before refusing with `msg-disconnect-local`.

### Column header

Sort by this column (`SortByColumn`) · Hide this column (`HideColumn`,
disabled on Name: `reason-wrong-target`) │ Columns… `pane.columns`

### Places

| row | entries |
| --- | --- |
| drive | Open · Open in other pane · Open in new tab · Copy path │ Add to favorites |
| favorite | Open · Open in other pane · Open in new tab · Copy path │ Remove from favorites |
| favorite, broken target | same, Open/Open in other/new tab disabled with the broken reason |
| section header | Fold / Unfold |

### Tree branch

Open · Open in other pane · Open in new tab · Copy path │ Add to favorites ·
Fold / Unfold

## Edge cases

- **Right click on a non-active slot**: the capturing `mousedown` already
  focuses it, so `context_menu_row` arrives for the active slot. If the
  generation is old by then → stale, no menu.
- **Right click while a dialog / help is open**: the host refuses (keys
  belong to the modal); the native menu is still suppressed.
- **Right click with a menu already open**: the old one closes and the new
  one opens at the new place (one menu at a time).
- **Target re-listed underneath** (copy finished, refresh): fingerprint check
  at run; mismatch → closes, nothing runs.
- **Hostile names**: the header uses the already-masked display name, elided
  at 40 characters by characters, never bytes.
- **Disabled entry clicked**: nothing runs, the menu stays open.
- **Read-only pane (inside an archive)**: Copy stays lit (it reads), Move /
  Rename / Delete / Pack dim with `reason-read-only` — all from `verdict`.
- **Commands the table has no arm for** (`pane.open`, `pane.edit`,
  `pane.chmod`, `pane.checksum`, …) are lit by `verdict`'s documented
  fail-open, and a wrong target is refused by the dispatch with its own
  message. Adding arms to the table is out of scope here; the menu does not
  grow a second availability criterion.
- **No destination designated** (three or more listings, none targeted):
  Copy/Move stay lit and the dispatch refuses with its own message, as from
  the key (`facts()`'s documented divergence).

## Testing

- `norte-frontend` (`context_menu.rs`): entries per surface as a table;
  the core is identical, in identical order, for every `RowTarget`; each
  contextual section appears exactly under its condition; the Connection
  section only when remote; `Open` resolves to `nav.enter` vs `pane.open`.
- `norte-ui-host` (`tests/`):
  - open on a marked row keeps the marks; on an unmarked row drops them and
    moves the cursor;
  - an entry produces the SAME effect as its key (copy, rename, mkdir);
  - disabled entry: no effect, menu stays;
  - fingerprint stale: closed, `Stale`, no effect;
  - the close patch precedes the effect's patch;
  - keys are routed to the menu while open; Escape closes;
  - a dialog opening closes the menu; opening is refused under a dialog;
  - `pane.context-menu` opens on the cursor row with `anchor: None`;
  - every verb: OpenInOther, OpenInNewTab, CopyPath, Add/RemoveFavorite,
    ToggleFold, SortByColumn, HideColumn;
  - bridge golden for 107 (`ViewChange::ContextMenu`, snapshot field, the
    new actions);
  - catalogue guard: `pane.context-menu` catalogued, its Fluent strings in
    both locales, `ctx-*` keys in both locales.
- Renderer (vitest, `tests/render.test.ts`):
  - the document `contextmenu` is prevented, except on an `<input>`;
  - a right `mousedown` on a row sends no `select_row` and two of them no
    `activate`;
  - each surface sends its open action with its fields;
  - paints a `ContextMenuView` (sections, disabled + reason, cursor);
  - hover / click / outside / blur send point / activate / close;
  - kept inside the window near the right and bottom edges;
  - `x: null` anchors to the focused row.
- Docs: rewrite `crates/norte-help/topics/{en,es}/mouse.md` "The right-click
  menu" for this menu (surfaces, keyboard, the marks rule kept), regenerate
  the help golden, CHANGELOG entry, bridge 107 note.

## Out of scope

- A context menu in the TUI (the model is shared so it can come later).
- Menus on the tab bar / terminal list beyond what exists.
- Search results, timeline, disk map, compare, organize review surfaces.
- Clipboard cut/copy/paste of FILES (norte has no file clipboard).
- Drag with the right button.
