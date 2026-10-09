# Window context menu Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A right click (or Shift+F10 / the Menu key) on a listing row, the empty listing, a column header, a Places row or a tree branch opens a host-owned menu at the pointer whose entries run exactly what their keys run.

**Architecture:** A pure model in `norte-frontend::context_menu` says WHICH entries a surface has; `norte-frontend::availability::verdict` (unchanged) says whether each can run. `norte-ui-host` keeps the open menu on `State` (like the menu bar's `self.menu`), projects a `ContextMenuView` by patch, resolves keys and clicks against what IT has open, and dispatches catalogue commands through `effect_of` (the key's path) or a small set of host verbs for Places/tree/header. The Tauri renderer only paints and reports pointer events.

**Tech Stack:** Rust (`norte-frontend`, `norte-ui-host`, `norte-tui` for the key code), Fluent (`norte-i18n`), TypeScript webview (`crates/norte-gui-tauri/ui`, vitest).

**Spec:** `docs/superpowers/specs/2026-10-09-context-menu-design.md`

## Global Constraints

- `norte-proto` NOT touched. `BRIDGE_VERSION` (`crates/norte-ui-host/src/bridge.rs:601`, now 106) bumps ONCE to **107**, in Task 3; `crates/norte-gui-tauri/ui/src/types.ts:13` follows in Task 6.
- Every entry that names a catalogue command runs through `crate::commands::effect_of(cmd, 1)` → `apply_effect`. No second dispatcher.
- Availability comes ONLY from `norte_frontend::availability::verdict(cmd, &facts)` plus "not in `commands::all_with(self.effects)`" (→ disabled, `reason-unavailable`). No new availability criterion in the menu.
- Marks rule (kept from GPUI): marked row → acts on the marks; unmarked row → `pane.clear_marks()` then cursor to it. Irrecoverable, Esc included.
- Core row entries are identical and in identical order on every row.
- Names shown in the header are the already-masked display name, elided at 40 **chars** (never bytes) with a trailing `…`.
- AI section is always present in the empty-area menu (no local "AI configured" fact); AI stays user-triggered.
- Hiding a column is window-only, like the column selector: `norte.toml` is NOT written.
- Bash cannot write files (hook): use Edit/Write. `rg`, not `grep`. Commit with `git commit -F <file>` (write the message with Write into the scratchpad). Commit trailer:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01FCsHBTHJNupNembek7fuc6`.
- Loop per task: `just t <crate>` (+ `just c` if lint surface). `just ci-fast` ONCE after Task 4. `just gui-ci` after Task 6. The push of `main` at the end is the final gate. Never `sleep`; never re-run the gate as a debugger.
- Public struct gained a field → `cargo test -p <crate> --doc`. Doc link written → `cargo doc -p <crate> --no-deps`.
- Branch: `feat/gui-context-menu` (already created; the spec is committed on it).

## Review Focus

1. **The keyboard Menu key in the webview fires BOTH a `keydown` and a DOM `contextmenu` event** on the focused element. Expected: ONE menu, anchored to the cursor row (from the host's `pane.context-menu`), not a second one at (0,0) from the row listener. The renderer's `keydown` already `preventDefault`s, which suppresses the keyboard `contextmenu` in WebKit/Chromium; pin it with a vitest that dispatches a `contextmenu` with `button: 0, clientX: 0, clientY: 0` right after a `ContextMenu` keydown and asserts no `context_menu_row` was sent. Test in Task 6.
2. **Right click on a row of the NON-active listing.** Expected: the panel is focused (capturing `mousedown`) and the menu opens for that row, not refused as stale. Host test with two slots in Task 3: dispatch `FocusSlot` then `ContextMenuRow` for the other slot → menu open, `header` names that row.
3. **The listing re-lists under an open menu** (a copy finishes and refreshes). Expected: choosing an entry whose target vanished runs nothing and the menu closes. Fingerprint test in Task 4.
4. **Right click while a dialog is up.** Expected: no menu, native menu still suppressed, dialog keeps the keys. Host test in Task 4; renderer test in Task 6.
5. **A hostile file name** (`\u{202e}gpj.exe`, 300 chars). Expected: header shows the masked display name elided at 40 chars + `…`. Host test in Task 3.

---

### Task 1: The Menu key and `pane.context-menu` in the catalogue

**Files:**
- Modify: `crates/norte-frontend/src/keymap/chord.rs` (`KeyCode` ~l.10, `Display` ~l.260, label table ~l.564, parser ~l.661)
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (the every-key cross-product list ~l.634, the label pairs ~l.1504)
- Modify: `crates/norte-frontend/src/subshell.rs` (~l.657: key → bytes)
- Modify: `crates/norte-tui/src/keymap.rs` (~l.48 and ~l.96: crossterm ↔ `KeyCode`)
- Modify: `crates/norte-ui-host/src/keys.rs` (`canonical_name` ~l.115)
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs` (a `live(...)` line next to `pane.properties`)
- Modify: `crates/norte-frontend/presets/keymap/{orthodox,cua,norton,far,krusader,total-commander,vim}.toml`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: in-file `mod tests` of `chord.rs` and `keys.rs`

**Interfaces:**
- Produces: `KeyCode::Menu` (chord token `"menu"`, label `"Menu"`); command id `"pane.context-menu"` (status Live, `counts: false`, effect `Inert`); Fluent keys `help-cmd-pane-context-menu`, `menu-item-pane-context-menu`.

- [ ] **Step 1: Failing tests.** In `chord.rs` tests:

```rust
/// The Menu key (the one between AltGr and Ctrl) is a keymap key: it opens
/// the context menu from the keyboard.
#[test]
fn the_menu_key_parses_and_prints() {
    let c = parse_chord("menu").expect("parses");
    assert_eq!(c.key, KeyCode::Menu);
    assert_eq!(c.to_string(), "menu");
    let s = parse_chord("shift+f10").expect("parses");
    assert_eq!((s.key, s.mods.shift), (KeyCode::F(10), true));
}
```

In `crates/norte-ui-host/src/keys.rs` tests (next to the PageUp/PageDown one, ~l.358):

```rust
/// The browser calls it `ContextMenu`; the keymap calls it `menu`.
#[test]
fn the_browsers_context_menu_key_is_menu() {
    for dom in ["ContextMenu", "contextmenu", "menu"] {
        assert_eq!(canonical_name(dom).as_deref(), Some("menu"), "{dom}");
    }
}
```

(Use the exact names of the parse function and `Chord` fields that `chord.rs` already exposes — `rg -n "pub fn parse" crates/norte-frontend/src/keymap/chord.rs`.)

- [ ] **Step 2:** `just t norte-frontend` and `just t norte-ui-host` → both fail (no `KeyCode::Menu`, `canonical_name` returns `None`).
- [ ] **Step 3: Implement.**
  - `KeyCode::Menu` with rustdoc "The Menu (context menu) key." Add an arm everywhere the compiler points: `Display` → `"menu"`; parser `"menu" => KeyCode::Menu`; label table `"menu" => "Menu"`; the cross-product list in `keymap/mod.rs`; the label pair `("menu", "Menu")`; `subshell.rs` → `b"\x1b[29~".to_vec()` (xterm's Menu); TUI `CtCode::Menu => KeyCode::Menu` and `KeyCode::Menu => CtCode::Menu` (crossterm has `KeyCode::Menu`).
  - `canonical_name`: `"contextmenu" | "menu" => "menu",`.
  - Catalogue: `live("pane.context-menu", false, Inert),` with a comment: "Opens the context menu on the focused thing (the window). The TUI has no context menu: it answers 'not here'."
  - Presets: in each of the seven, in the listing section next to `pane.properties` (or next to `pane.view` where properties is unbound), add
    `{ on = ["shift+f10", "menu"], run = "pane.context-menu" },`. If a preset test rejects a chord (some presets document reserved keys), follow that test's message.
  - Fluent, both files, next to `menu-item-pane-properties` / `help-cmd-pane-properties`:
    - en: `menu-item-pane-context-menu = Context menu` / `help-cmd-pane-context-menu = open the context menu on the focused item`
    - es: `menu-item-pane-context-menu = Menú contextual` / `help-cmd-pane-context-menu = abre el menú contextual sobre el elemento enfocado`
  - The TUI: `crates/norte-tui/src/keymap.rs` maps names to `Command` (~l.279). Do NOT add a variant. Run `just t norte-tui`; if a guard requires every Live command to be either implemented or listed as not-in-this-frontend, add `pane.context-menu` to that list with the reason "the window's context menu".
- [ ] **Step 4:** `just t norte-frontend`, `just t norte-ui-host`, `just t norte-tui`, `just t norte-i18n` green. Regenerate any help golden the catalogue change touches with the env var the failing test names (`NORTE_BLESS=1` for host goldens). `cargo test -p norte-frontend --doc`.
- [ ] **Step 5:** Commit `feat(keys): the Menu key, and pane.context-menu in the catalogue`.

---

### Task 2: `norte-frontend::context_menu` — the shared model

**Files:**
- Create: `crates/norte-frontend/src/context_menu.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (`pub mod context_menu;` next to `pub mod availability;`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl` (the `ctx-*` keys)

**Interfaces:**
- Produces:

```rust
pub enum Surface {
    Row(RowTarget),
    Empty { remote: bool },
    Header { hideable: bool },
    Place(PlaceTarget),
    Branch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowTarget {
    pub count: usize,
    pub kind: Option<norte_proto::EntryKind>, // None when count > 1
    pub archive: bool,                         // single entry, nav::archive_root_for(e).is_some()
    pub all_files: bool,                       // every target entry is EntryKind::File
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceTarget { Drive, Favorite, SectionHeader }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb { OpenHere, OpenInOther, OpenInNewTab, CopyPath, AddFavorite, RemoveFavorite, ToggleFold, SortByColumn, HideColumn }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action { Command(&'static str), Verb(Verb) }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub action: Action,
    pub label_key: &'static str,
    /// Starts a section: Some(key) titled, Some("") a bare rule, None continues.
    pub section: Option<&'static str>,
}
pub fn entries(surface: &Surface) -> Vec<Entry>;
/// `Some(n)` when `s` is longer than 40 chars: first 40 chars + `…`.
pub fn elide(s: &str) -> String;
pub const TARGET_MAX_CHARS: usize = 40;
```

- [ ] **Step 1: Failing tests** in the new file's `mod tests`:

```rust
use super::*;
use norte_proto::EntryKind;

fn row(count: usize, kind: Option<EntryKind>, archive: bool, all_files: bool) -> Surface {
    Surface::Row(RowTarget { count, kind, archive, all_files })
}
fn commands(s: &Surface) -> Vec<&'static str> {
    entries(s).iter().filter_map(|e| match e.action {
        Action::Command(c) => Some(c),
        Action::Verb(_) => None,
    }).collect()
}
const CORE_TAIL: [&str; 8] = [
    "pane.view", "pane.edit", "pane.copy", "pane.move",
    "pane.rename", "pane.delete", "pane.copy-path", "pane.properties",
];

/// The core is the SAME nine entries, in the same order, on every row: the
/// hand learns it once.
#[test]
fn the_core_is_identical_on_every_row() {
    for s in [
        row(1, Some(EntryKind::File), false, true),
        row(1, Some(EntryKind::Dir), false, false),
        row(1, Some(EntryKind::File), true, true),
        row(5, None, false, false),
    ] {
        let c = commands(&s);
        assert!(matches!(c[0], "nav.enter" | "pane.open"), "{c:?}");
        assert_eq!(&c[1..9], &CORE_TAIL, "{c:?}");
    }
}

#[test]
fn open_enters_a_directory_or_an_archive_and_opens_a_file() {
    assert_eq!(commands(&row(1, Some(EntryKind::Dir), false, false))[0], "nav.enter");
    assert_eq!(commands(&row(1, Some(EntryKind::Symlink), false, false))[0], "nav.enter");
    assert_eq!(commands(&row(1, Some(EntryKind::File), true, true))[0], "nav.enter");
    assert_eq!(commands(&row(1, Some(EntryKind::File), false, true))[0], "pane.open");
    // Several: `pane.open` (the verdict dims nothing; the dispatch refuses
    // a multiple target with its own message).
    assert_eq!(commands(&row(3, None, false, true))[0], "pane.open");
}

#[test]
fn contextual_sections_appear_only_where_they_apply() {
    let file = commands(&row(1, Some(EntryKind::File), false, true));
    assert!(!file.contains(&"pane.unpack") && !file.contains(&"pane.dir-size"));
    assert!(file.contains(&"pane.pack") && file.contains(&"pane.checksum"));

    let zip = commands(&row(1, Some(EntryKind::File), true, true));
    assert!(zip.contains(&"pane.unpack") && zip.contains(&"pane.test-archive"));
    assert!(!zip.contains(&"pane.pack"), "an archive is not packed again");

    let dir = commands(&row(1, Some(EntryKind::Dir), false, false));
    for c in ["pane.dir-size", "pane.compare-dirs", "pane.sync-dirs"] {
        assert!(dir.contains(&c), "{c}");
    }

    let two_files = commands(&row(2, None, false, true));
    assert!(two_files.contains(&"pane.compare-files") && two_files.contains(&"pane.rename-batch"));
    let two_mixed = commands(&row(2, None, false, false));
    assert!(!two_mixed.contains(&"pane.compare-files"));
    let three = commands(&row(3, None, false, true));
    assert!(!three.contains(&"pane.compare-files") && three.contains(&"pane.rename-batch"));
}

/// AI rename, organize and disconnect act on the whole folder: never on a row.
#[test]
fn folder_wide_commands_are_not_on_a_row() {
    for s in [row(1, Some(EntryKind::File), false, true), row(4, None, false, false)] {
        let c = commands(&s);
        for no in ["pane.ai-rename", "pane.organize", "pane.disconnect"] {
            assert!(!c.contains(&no), "{no} on a row");
        }
    }
}

#[test]
fn the_empty_area_has_ai_always_and_disconnect_only_when_remote() {
    let local = commands(&Surface::Empty { remote: false });
    for c in ["pane.mkdir", "pane.edit-new", "pane.refresh", "pane.toggle-hidden",
              "pane.sort-menu", "pane.columns", "mark.all", "mark.invert",
              "pane.ai-rename", "pane.organize"] {
        assert!(local.contains(&c), "{c}");
    }
    assert!(!local.contains(&"pane.disconnect"));
    assert!(commands(&Surface::Empty { remote: true }).contains(&"pane.disconnect"));
}

#[test]
fn header_places_and_branch_are_verbs() {
    let verbs = |s: &Surface| -> Vec<Verb> {
        entries(s).iter().filter_map(|e| match e.action {
            Action::Verb(v) => Some(v), Action::Command(_) => None,
        }).collect()
    };
    assert_eq!(verbs(&Surface::Header { hideable: true }), [Verb::SortByColumn, Verb::HideColumn]);
    assert_eq!(commands(&Surface::Header { hideable: true }), ["pane.columns"]);
    let base = [Verb::OpenHere, Verb::OpenInOther, Verb::OpenInNewTab, Verb::CopyPath];
    assert_eq!(verbs(&Surface::Place(PlaceTarget::Drive)), [&base[..], &[Verb::AddFavorite]].concat());
    assert_eq!(verbs(&Surface::Place(PlaceTarget::Favorite)), [&base[..], &[Verb::RemoveFavorite]].concat());
    assert_eq!(verbs(&Surface::Place(PlaceTarget::SectionHeader)), [Verb::ToggleFold]);
    assert_eq!(verbs(&Surface::Branch), [&base[..], &[Verb::AddFavorite, Verb::ToggleFold]].concat());
}

/// Every label and section key exists in BOTH locales.
#[test]
fn every_key_is_translated() {
    let surfaces = [
        row(1, Some(EntryKind::File), false, true), row(1, Some(EntryKind::Dir), false, false),
        row(1, Some(EntryKind::File), true, true), row(2, None, false, true),
        Surface::Empty { remote: true }, Surface::Header { hideable: true },
        Surface::Place(PlaceTarget::Drive), Surface::Place(PlaceTarget::Favorite),
        Surface::Place(PlaceTarget::SectionHeader), Surface::Branch,
    ];
    for s in &surfaces {
        for e in entries(s) {
            for key in std::iter::once(e.label_key).chain(e.section.filter(|k| !k.is_empty())) {
                for lang in ["en", "es"] {
                    let t = norte_i18n::t_in(lang, key);
                    assert_ne!(t, key, "{key} missing in {lang}");
                }
            }
        }
    }
}

#[test]
fn elide_counts_chars_not_bytes() {
    assert_eq!(elide("corto"), "corto");
    let long = "ñ".repeat(50);
    let e = elide(&long);
    assert_eq!(e.chars().count(), 41);
    assert!(e.ends_with('…'));
}
```

(If `norte_i18n::t_in` takes a different language type, use what `controller/views.rs` passes as `self.lang`; `rg -n "pub fn t_in" crates/norte-i18n/src`.)

- [ ] **Step 2:** `just t norte-frontend` → fails to compile.
- [ ] **Step 3: Implement.** Module docs: what it decides (which entries) and what it does not (whether they run → `availability`; labels → Fluent; dispatch → the frontend). Build with small helpers `cmd(id, label, section)` / `verb(v, label, section)`. Order and sections exactly as the spec's **Entries** section:
  - Row core: `Open` (`nav.enter` when `count == 1 && (kind ∈ {Dir, Symlink} || archive)`, else `pane.open`; label `menu-item-pane-open`… use `ctx-open`), `pane.view`, `pane.edit`, rule, `pane.copy`, `pane.move`, `pane.rename`, `pane.delete`, rule, `pane.copy-path`, `pane.properties`. Labels: the existing `menu-item-<id with . → ->` keys (they exist for all these; verify with `rg -n "^menu-item-pane-(view|edit|copy|move|rename|delete|copy-path|properties) " crates/norte-i18n/i18n/en.ftl`); `ctx-open` is new.
  - Then, in this order: Archive (`ctx-section-archive`: unpack, test-archive) if `count == 1 && archive`; Directory (`ctx-section-directory`: dir-size, compare-dirs, sync-dirs) if `count == 1 && kind == Some(Dir)`; Selection (`ctx-section-selection`: rename-batch, then compare-files if `count == 2 && all_files`) if `count >= 2`; `pane.pack` (bare rule) if `!archive`; More (`ctx-section-more`: checksum, chmod).
  - Empty: sections `ctx-section-folder` (mkdir, edit-new), `ctx-section-view` (refresh, toggle-hidden, sort-menu, columns), `ctx-section-marks` (mark.all, mark.invert), `ctx-section-ai` (ai-rename, organize), and `ctx-section-connection` (disconnect) only if `remote`.
  - Header: `SortByColumn` (`ctx-sort-by-column`), `HideColumn` (`ctx-hide-column`), rule, `pane.columns`. (`hideable` is consumed by the host for availability; the list does not change — core rule.)
  - Place/Branch: `ctx-open-here`, `ctx-open-in-other`, `ctx-open-in-new-tab`, `ctx-copy-path`, then rule + `ctx-add-favorite` / `ctx-remove-favorite`; `ctx-toggle-fold`.
  - Fluent keys (both files, a `## Context menu` block next to the old `gui-menu-*` ones, which stay):

    | key | en | es |
    | --- | --- | --- |
    | `ctx-open` | Open | Abrir |
    | `ctx-open-here` | Open | Abrir |
    | `ctx-open-in-other` | Open in the other pane | Abrir en el otro panel |
    | `ctx-open-in-new-tab` | Open in a new tab | Abrir en una pestaña nueva |
    | `ctx-copy-path` | Copy path | Copiar ruta |
    | `ctx-add-favorite` | Add to favorites… | Añadir a favoritos… |
    | `ctx-remove-favorite` | Remove from favorites | Quitar de favoritos |
    | `ctx-toggle-fold` | Fold / unfold | Plegar / desplegar |
    | `ctx-sort-by-column` | Sort by this column | Ordenar por esta columna |
    | `ctx-hide-column` | Hide this column | Ocultar esta columna |
    | `ctx-section-archive` | Archive | Archivo comprimido |
    | `ctx-section-directory` | Folder | Carpeta |
    | `ctx-section-selection` | Selection | Selección |
    | `ctx-section-more` | More | Más |
    | `ctx-section-folder` | New | Nuevo |
    | `ctx-section-view` | View | Vista |
    | `ctx-section-marks` | Marks | Marcas |
    | `ctx-section-ai` | AI | IA |
    | `ctx-section-connection` | Connection | Conexión |
    | `ctx-acts-on-dir` | in { $dir } | en { $dir } |
    | `ctx-acts-on-column` | column { $column } | columna { $column } |

    The row header reuses `gui-menu-acts-on` / `gui-menu-target-marks`.
- [ ] **Step 4:** `just t norte-frontend`, `just t norte-i18n` green; `just c`; `cargo test -p norte-frontend --doc`.
- [ ] **Step 5:** Commit `feat(frontend): the context menu's shared model`.

---

### Task 3: Host — open, project and close (listing surfaces) + bridge 107

**Files:**
- Create: `crates/norte-ui-host/src/controller/context_menu.rs` (methods of `State`, same header as `controller/menu.rs`: `#[allow(clippy::wildcard_imports)] use super::*;`)
- Modify: `crates/norte-ui-host/src/controller/mod.rs` (declare the module; `State.context_menu: Option<ContextMenu>`; init `None` ~l.3430; dispatch arms ~l.3897)
- Modify: `crates/norte-ui-host/src/action.rs` (new `UiAction` variants)
- Modify: `crates/norte-ui-host/src/dto.rs` (`ContextMenuView`, `ContextItemView`, `ViewChange::ContextMenu`, `ViewSnapshot.context_menu`)
- Modify: `crates/norte-ui-host/src/controller/views.rs` (`snapshot()` fills it)
- Modify: `crates/norte-ui-host/src/bridge.rs` (`BRIDGE_VERSION = 107` + the version note)
- Modify: `crates/norte-ui-host/tests/golden.rs` + `tests/golden/*.json` (regenerated with `NORTE_BLESS=1`)
- Test: create `crates/norte-ui-host/tests/controller/context_menu.rs`; register `mod context_menu;` in `tests/controller/main.rs`

**Interfaces:**
- Consumes: Task 2's `context_menu::{Surface, RowTarget, Entry, Action, entries, elide}`; `availability::{verdict, reason_key}`.
- Produces (Rust):

```rust
// action.rs — inside UiAction
ContextMenuRow { slot_id: u32, key: RowKey, generation: u64, x: i32, y: i32 },
ContextMenuEmpty { slot_id: u32, x: i32, y: i32 },
ContextMenuHeader { slot_id: u32, column: String, x: i32, y: i32 },
ContextMenuPlace { row: u32, generation: u64, x: i32, y: i32 },   // wired in Task 5
ContextMenuBranch { row: u32, generation: u64, x: i32, y: i32 },  // wired in Task 5
ContextMenuPointRow { row: u32 },
ContextMenuActivateRow { row: u32 },                              // wired in Task 4
ContextMenuClose,

// dto.rs
pub struct ContextMenuView { pub header: String, pub items: Vec<ContextItemView>, pub cursor: u64, pub x: Option<i32>, pub y: Option<i32> }
pub struct ContextItemView { pub label: String, pub chord: String, pub enabled: bool, pub reason: String, pub section: Option<String>, pub role: String }
// ViewChange
ContextMenu { context_menu: Option<ContextMenuView> },
// ViewSnapshot
#[serde(default)] pub context_menu: Option<ContextMenuView>,

// controller/context_menu.rs
pub(super) struct ContextMenu {
    pub(super) surface: norte_frontend::context_menu::Surface,
    pub(super) entries: Vec<norte_frontend::context_menu::Entry>,
    pub(super) facts: norte_frontend::availability::Facts,
    pub(super) cursor: usize,
    pub(super) slot: u32,
    pub(super) header: String,
    pub(super) fingerprint: Option<Fingerprint>,
    pub(super) subject: Subject,
    pub(super) anchor: Option<(i32, i32)>,
}
pub(super) struct Fingerprint { pub(super) entry: Option<Vec<u8>>, pub(super) marks: usize }
pub(super) enum Subject { Listing, Column(String), Place(usize), Branch(usize) }
impl State {
    pub(super) fn open_row_menu(&mut self, slot_id: u32, key: RowKey, generation: u64, anchor: Option<(i32, i32)>) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>);
    pub(super) fn open_empty_menu(&mut self, slot_id: u32, anchor: Option<(i32, i32)>) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>);
    pub(super) fn open_header_menu(&mut self, slot_id: u32, column: &str, anchor: Option<(i32, i32)>) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>);
    pub(super) fn point_in_context_menu(&mut self, row: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>);
    pub(super) fn close_context_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>);
    pub(super) fn vista_context_menu(&self) -> Option<crate::dto::ContextMenuView>;
    /// Drops it with no patch (for paths that already send one).
    pub(super) fn forget_context_menu(&mut self);
}
```

- [ ] **Step 1: Failing tests** in `tests/controller/context_menu.rs` (follow `gestures.rs` for helpers: `host`, `host_tree`, `fake_tree`, `snapshot`, `snapshot_until`; the `..` row is OFF in `test_settings()`):

```rust
//! The window's context menu (spec 2026-10-09).
use super::*;
use norte_ui_host::dto::ContextMenuView;

fn browser(s: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    s.slots.iter().find_map(|v| match v { SlotView::Browser(b) => Some(&**b), _ => None }).expect("a listing")
}
async fn open_on(h: &UiHost, sub: &mut norte_ui_host::controller::UiSubscription, row: usize) -> ContextMenuView {
    let s = snapshot(h, sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ContextMenuRow { slot_id: b.slot_id, key: b.rows[row].key, generation: b.generation, x: 10, y: 20 })
        .await.expect("host alive");
    snapshot_until(h, sub, "the menu open", |s| s.context_menu.clone()).await
}

#[tokio::test]
async fn an_unmarked_row_drops_the_marks_and_names_itself() {
    let (h, _) = host(vec!["a.txt", "b.txt", "c.txt"]).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    for r in [0, 1] {
        h.dispatch(UiAction::ToggleMark { slot_id: b.slot_id, key: b.rows[r].key, generation: b.generation })
            .await.expect("host alive");
    }
    let m = open_on(&h, &mut sub, 2).await;
    assert!(m.header.contains("c.txt"), "{}", m.header);
    assert_eq!((m.x, m.y), (Some(10), Some(20)));
    let after = snapshot(&h, &mut sub).await;
    assert_eq!(browser(&after).marks, 0, "the marks went");
    assert_eq!(browser(&after).cursor, Some(browser(&after).rows[2].key));
}

#[tokio::test]
async fn a_marked_row_keeps_the_marks_and_says_how_many() {
    let (h, _) = host(vec!["a.txt", "b.txt", "c.txt"]).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    for r in [0, 1] {
        h.dispatch(UiAction::ToggleMark { slot_id: b.slot_id, key: b.rows[r].key, generation: b.generation })
            .await.expect("host alive");
    }
    let m = open_on(&h, &mut sub, 1).await;
    assert!(m.header.contains('2'), "{}", m.header);
    assert_eq!(browser(&snapshot(&h, &mut sub).await).marks, 2);
}

#[tokio::test]
async fn the_core_comes_first_with_chords_and_reasons() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    assert!(m.items.len() >= 9);
    let copy = &m.items[3];
    assert_eq!(copy.chord, "F5", "orthodox preset");
    assert!(copy.enabled && copy.reason.is_empty());
    let delete = &m.items[6];
    assert_eq!(delete.role, "destructive");
}

#[tokio::test]
async fn a_stale_generation_opens_nothing() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    let ack = h.dispatch(UiAction::ContextMenuRow { slot_id: b.slot_id, key: b.rows[0].key, generation: b.generation + 99, x: 0, y: 0 })
        .await.expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

#[tokio::test]
async fn the_empty_area_touches_no_marks() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ToggleMark { slot_id: b.slot_id, key: b.rows[0].key, generation: b.generation }).await.expect("host alive");
    h.dispatch(UiAction::ContextMenuEmpty { slot_id: b.slot_id, x: 5, y: 5 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(m.items.iter().any(|i| i.role == "ai"), "the AI section is there");
    assert_eq!(browser(&snapshot(&h, &mut sub).await).marks, 1);
}

#[tokio::test]
async fn the_name_column_cannot_be_hidden_and_says_why() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let b = browser(&snapshot(&h, &mut sub).await).slot_id;
    h.dispatch(UiAction::ContextMenuHeader { slot_id: b, column: "name".into(), x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    let hide = &m.items[1];
    assert!(!hide.enabled && !hide.reason.is_empty());
}

#[tokio::test]
async fn closing_and_pointing() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuPointRow { row: 3 }).await.expect("host alive");
    assert_eq!(snapshot(&h, &mut sub).await.context_menu.expect("open").cursor, 3);
    h.dispatch(UiAction::ContextMenuClose).await.expect("host alive");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

/// Review Focus 5.
#[tokio::test]
async fn a_hostile_long_name_is_masked_and_elided() {
    let name: &'static str = Box::leak(format!("\u{202e}{}.txt", "x".repeat(300)).into_boxed_str());
    let (h, _) = host(vec![name]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    assert!(!m.header.contains('\u{202e}'), "masked");
    assert!(m.header.contains('…'), "elided: {}", m.header);
}
```

Review Focus 2 (the `orthodox` layout places two listings; `compare.rs:80` uses it):

```rust
#[tokio::test]
async fn a_row_of_the_other_listing_opens_after_its_focus() {
    let (h, _) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let other = s.slots.iter().find_map(|v| match v {
        SlotView::Browser(b) if Some(b.slot_id) != s.focus => Some((**b).clone()),
        _ => None,
    }).expect("a second listing");
    h.dispatch(UiAction::FocusSlot { slot_id: other.slot_id }).await.expect("host alive");
    let ack = h.dispatch(UiAction::ContextMenuRow {
        slot_id: other.slot_id, key: other.rows[1].key, generation: other.generation, x: 1, y: 1,
    }).await.expect("host alive");
    assert!(!matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(m.header.contains("notas.txt"), "{}", m.header);
}
```

Add the golden case: in `tests/golden.rs`, one `ContextMenuRow` action and one `ViewChange::ContextMenu { context_menu: Some(..) }` change, following the bridge-106 cases already there; regenerate with `NORTE_BLESS=1 just t norte-ui-host`.

- [ ] **Step 2:** `just t norte-ui-host` → fails to compile.
- [ ] **Step 3: Implement.**
  - `dto.rs`: the two structs with the derives `MenuView` has; `ViewChange::ContextMenu` after `Menu`; `ViewSnapshot.context_menu` with `#[serde(default)]` and rustdoc "bridge 107".
  - `action.rs`: the variants with rustdoc; row/column variants say why the generation travels (copy the `PlaceActivateRow` wording).
  - `bridge.rs`: `BRIDGE_VERSION = 107` and a `- **107**:` note listing the actions, `ViewChange::ContextMenu`, `ViewSnapshot.context_menu`.
  - `open_row_menu`: (1) `self.something_keeps_the_keys()` → `(self.applied(), vec![])` and no menu. (2) `let Some(i) = self.row_of(slot_id, key, generation) else { stale }`. (3) If the entry at `i` is the `..` row (`rg -n "parent_entry|is_parent" crates/norte-frontend/src/pane/mod.rs` for the predicate), delegate to `open_empty_menu`. (4) `let marked = pane.is_marked(entry)`; if `!marked` → `pane.clear_marks()`; `pane.set_cursor(i)`. (5) Target: `count = if marked { pane.marks_len() } else { 1 }`; `kind`/`archive` from the single entry (`norte_frontend::nav::archive_root_for(e).is_some()`); `all_files` over `marked_paths()`' entries (or the single one). (6) `facts = self.facts()` AFTER the change. (7) Header: `gui-menu-acts-on` with `target` = `elide(display name)` or `gui-menu-target-marks { n }` — display name via the same masking `browser()` uses for `RowView.display_name` (`rg -n "display_name:" src/controller/views.rs`). (8) Fingerprint `{ entry: cursor_entry().map(|e| e.path.name_bytes()), marks: marks_len() }` (use whatever byte accessor `VPath` has for the last component). (9) `self.forget_menu()` (menu bar) if open. (10) Return a patch with the rows change AND `ViewChange::ContextMenu` (one envelope: `self.parche(vec![rows…, ContextMenu{..}])` — look at `parche_rows()` to build the rows change, or send two envelopes in order).
  - `open_empty_menu`: `remote` = base scheme (after last `+`) of `pane.dir().scheme()` ≠ `"file"`; header `ctx-acts-on-dir { dir }` with the masked path display; `fingerprint: None`; marks untouched.
  - `open_header_menu`: validate `slot_id == self.active()` (else stale); `hideable = column != "name"`; header `ctx-acts-on-column` with the column's header label from `self.columns`; `subject: Subject::Column(column.to_owned())`.
  - `vista_context_menu`: per entry: label `t_in(lang, label_key)`; for `Action::Command(c)`: chord `palette::first_chord(c, &self.effective).unwrap_or_default()`, availability = if `!commands::all_with(self.effects).contains(&c)` → disabled `reason-unavailable`, else `verdict(c, &facts)`; role `menu::role(c).as_str()`. For `Action::Verb(v)`: chord `""`, role `"normal"`, enabled except `Verb::HideColumn` when `Surface::Header { hideable: false }` (reason `reason-wrong-target`). `reason` = `t_in(lang, reason_key(r))` or `""`. Section: `None` → `None`, `Some("")` → `Some(String::new())`, `Some(k)` → translated. All strings through `clamp_display`.
  - `point_in_context_menu`: clamp-free set (out of range → stale, like `point_in_menu`).
  - `close_context_menu` / `forget_context_menu`.
  - Dispatch arms in `mod.rs` next to `UiAction::MenuClose`; `ContextMenuPlace`/`Branch`/`ActivateRow` return `(Self::stale(StaleAction::Modal), Vec::new())` until Tasks 4–5 (each with a `// Task N` comment removed when wired).
  - `snapshot()`: `context_menu: self.vista_context_menu()`.
- [ ] **Step 4:** `just t norte-ui-host` green; `cargo test -p norte-ui-host --doc`; `just c`.
- [ ] **Step 5:** Commit `feat(host): the context menu opens, shows and closes (bridge 107)`.

---

### Task 4: Host — running entries, keys, staleness, `pane.context-menu`

**Files:**
- Modify: `crates/norte-ui-host/src/controller/context_menu.rs`
- Modify: `crates/norte-ui-host/src/controller/mod.rs` (route keys; `something_keeps_the_keys`; close on modal open)
- Modify: `crates/norte-ui-host/src/commands.rs` (`"pane.context-menu"` in `IMPLEMENTED`, `Effect::ContextMenu`, `effect_of` arm)
- Modify: `crates/norte-ui-host/src/controller/effects.rs` (`Effect::ContextMenu` arm)
- Test: `crates/norte-ui-host/tests/controller/context_menu.rs`

**Interfaces:**
- Consumes: Task 3's `ContextMenu`, `Fingerprint`, `open_row_menu`, `open_empty_menu`, `vista_context_menu`, `forget_context_menu`.
- Produces: `State::activate_context_menu(&mut self, row: u32, backend, mailbox)`, `State::key_in_context_menu(&mut self, k: &KeyInput, backend, mailbox)`, `State::run_context_entry(&mut self, entry: Entry, backend, mailbox)` (Task 5 extends its `Verb` arm), `Effect::ContextMenu`.

- [ ] **Step 1: Failing tests** (append):

```rust
fn row_of_command(m: &ContextMenuView, label_from: &str) -> u32 {
    // Labels are translated (locale "es"): find by the catalogue label.
    let want = norte_i18n::t_in("es", &format!("menu-item-{}", label_from.replace('.', "-")));
    u32::try_from(m.items.iter().position(|i| i.label == want).expect(label_from)).expect("fits")
}

/// Choosing "rename" opens the SAME dialog shift+F6 opens.
#[tokio::test]
async fn an_entry_runs_what_its_key_runs() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_of_command(&m, "pane.rename") })
        .await.expect("host alive");
    let s = snapshot_until(&h, &mut sub, "the rename dialog", |s| (!s.dialogs.is_empty()).then(|| s.clone())).await;
    assert!(s.context_menu.is_none(), "closed before the effect");
    let by_key = {
        let (h2, _) = host(vec!["a.txt"]).await;
        let mut sub2 = h2.subscribe();
        h2.dispatch(key_mod("F6", false, true)).await.expect("host alive");
        snapshot_until(&h2, &mut sub2, "dialog", |s| s.dialogs.first().map(|d| d.title_key.clone())).await
    };
    assert_eq!(s.dialogs[0].title_key, by_key);
}

/// A read-only WINDOW (`host_solo_read`, effects ReadOnly): Move is not in
/// `all_with(effects)`, so it travels disabled, and clicking it does nothing.
#[tokio::test]
async fn a_disabled_entry_runs_nothing_and_stays_open() {
    let (h, _) = host_solo_read(fake_tree()).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 1).await; // notas.txt
    let mv = row_of_command(&m, "pane.move");
    assert!(!m.items[mv as usize].enabled && !m.items[mv as usize].reason.is_empty());
    h.dispatch(UiAction::ContextMenuActivateRow { row: mv }).await.expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_some(), "still open");
    assert!(s.dialogs.is_empty(), "nothing ran");
}

/// Review Focus 3.
#[tokio::test]
async fn a_target_that_changed_runs_nothing() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    // Change the target under the menu: mark another row through the key path
    // the menu does not own (ToggleMark is a renderer action that bypasses it).
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ToggleMark { slot_id: b.slot_id, key: b.rows[1].key, generation: b.generation })
        .await.expect("host alive");
    let ack = h.dispatch(UiAction::ContextMenuActivateRow { row: row_of_command(&m, "pane.rename") })
        .await.expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_none() && s.dialogs.is_empty());
}

#[tokio::test]
async fn keys_walk_and_run_the_menu_and_escape_closes() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    assert_eq!(snapshot(&h, &mut sub).await.context_menu.expect("open").cursor, 1);
    h.dispatch(press("Escape")).await.expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_none());
    assert_eq!(browser(&s).cursor, Some(browser(&s).rows[0].key), "Escape did not leave the panel");
}

/// Review Focus 4.
#[tokio::test]
async fn under_a_dialog_nothing_opens() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("F6", false, true)).await.expect("host alive"); // rename dialog
    let _ = snapshot_until(&h, &mut sub, "dialog", |s| (!s.dialogs.is_empty()).then_some(())).await;
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ContextMenuRow { slot_id: b.slot_id, key: b.rows[0].key, generation: b.generation, x: 0, y: 0 })
        .await.expect("host alive");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

#[tokio::test]
async fn a_dialog_opening_closes_the_menu() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    // A command that opens a dialog WITHOUT going through the menu.
    run_by_palette(&h, &mut sub, "pane.mkdir").await;
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

#[tokio::test]
async fn shift_f10_opens_on_the_cursor_row_with_no_anchor() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(key_mod("F10", false, true)).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert_eq!((m.x, m.y), (None, None));
    assert!(m.header.contains("b.txt"), "{}", m.header);
}
```

Fill `a_disabled_entry_runs_nothing_and_stays_open` with real code once you have found how the Fake reports read-only (it must NOT stay a comment — if the Fake has no read-only knob, add `Fake::read_only(scheme)` in `tests/backend_fake/mod.rs` returning `Capabilities { flags: CapabilityFlags::READ_ONLY, .. }` for that scheme, following the existing capability plumbing).

- [ ] **Step 2:** `just t norte-ui-host` → new tests fail.
- [ ] **Step 3: Implement.**
  - `activate_context_menu`: no menu → stale Modal. `row` out of range → stale Generation. Entry disabled (recompute the item's availability exactly as `vista_context_menu` does — factor a `fn availability_of(&self, m: &ContextMenu, e: &Entry) -> Availability`) → `(self.applied(), vec![])`, menu stays. Row surfaces: compare the stored fingerprint with the CURRENT `{cursor entry bytes, marks_len()}` of `m.slot`; mismatch (or `m.slot != self.active()`) → `forget_context_menu()`, send the close patch, return `Stale(Generation)`. Else `let e = m.entries[row]; self.run_context_entry(e, …)`.
  - `run_context_entry`: `forget_context_menu()`; `closing = self.parche(vec![ViewChange::ContextMenu { context_menu: None }])`; `Action::Command(c)` → `effect_of(c, 1)` → `apply_effect` (else `no_implemented(c)`); `Action::Verb(_)` → `self.no_implemented("context-menu verb")` placeholder REPLACED in Task 5 (leave a `// Task 5` comment). Return `[closing, rest…]`.
  - `key_in_context_menu`: `Escape|esc` close; `ArrowUp|up` / `ArrowDown|down` cycle with wrap; `Enter|enter` → `activate_context_menu(cursor)`; anything else swallowed `(self.applied(), vec![])`. Route it in `mod.rs` where `key_in_menu` is routed (`rg -n "key_in_menu" src/controller/mod.rs`), BEFORE the menu bar and the listing.
  - `something_keeps_the_keys()`: include `self.context_menu.is_some()` ONLY for the key routing; the open path checks the modal/dialog/help state BEFORE this field (opening a second context menu replaces the first). Read the function first: if adding the field there would make `open_*` refuse to replace an open menu, split the check into a local `modal_has_the_keys()` used by `open_*`.
  - Close on modal open: in `parche` users that push dialogs is too wide; instead call `self.forget_context_menu()` at the top of `apply_effect` when `self.context_menu.is_some()` AND the effect did not come from `run_context_entry` (which already forgot it) — and include `ViewChange::ContextMenu { context_menu: None }` in that first patch. Also forget it in `open_menu`/`expand_menu`/`toggle_menu` and in the palette/help openers by calling a single `self.drop_context_menu_into(&mut changes)` helper.
  - `Effect::ContextMenu` in `commands.rs` (`"pane.context-menu" => Effect::ContextMenu`, in `IMPLEMENTED`); in `effects.rs`: if the Places bar has focus (`places_have_focus()`) → Task 5's `open_place_menu(cursor, None)`; else if the tree has focus → Task 5's `open_branch_menu(cursor, None)`; else the active listing: cursor row → `open_row_menu(active, key_of_cursor, epoch, None)`; empty listing → `open_empty_menu(active, None)`. (Until Task 5, Places/tree fall through to the listing; leave the branches with a `// Task 5` comment.)
- [ ] **Step 4:** `just t norte-ui-host` green; `just c`. Then **`just ci-fast` (the plan's mid-point run)**.
- [ ] **Step 5:** Commit `feat(host): context menu entries run their key's command`.

---

### Task 5: Host — Places, tree and header verbs

**Files:**
- Modify: `crates/norte-ui-host/src/controller/context_menu.rs`
- Modify: `crates/norte-ui-host/src/controller/selectors.rs` (extract `remove_favorite_named(&mut self, name: String, mailbox)` from `remove_favorite` ~l.816; `remove_favorite` calls it)
- Modify: `crates/norte-ui-host/src/controller/mod.rs` (wire `ContextMenuPlace`/`ContextMenuBranch`)
- Modify: `crates/norte-ui-host/src/controller/effects.rs` (Task 4's `// Task 5` branches)
- Test: `crates/norte-ui-host/tests/controller/context_menu.rs`

**Interfaces:**
- Consumes: Task 3/4 state and `run_context_entry`.
- Produces: `open_place_menu(&mut self, row: u32, generation: Option<u64>, anchor: Option<(i32,i32)>)`, `open_branch_menu(&mut self, row: u32, generation: Option<u64>, anchor: Option<(i32,i32)>)` (`generation: None` = from the keyboard, use the current one); `run_verb(&mut self, verb: Verb, subject: Subject, backend, mailbox)`.

- [ ] **Step 1: Failing tests** (append; Places/tree need a layout that places them — copy the setup from the existing Places/tree tests: `rg -n "PlaceActivateRow|TreeActivateRow" tests/controller/*.rs`):

Helpers already in the test binary: `host_full`, `places(&snap)`, `volume(..)`, `primer_listing`
(`attributes_processes.rs:719`), `fake_tree`, `host_tree`, `tree_with_branches`, `tree_of`
(`sync.rs:2320`), `native_effects()` (`sync.rs:1676`). `t(key)` below is
`norte_i18n::t_in(norte_i18n::Lang::Es, key)`.

```rust
fn t(key: &str) -> String { norte_i18n::t_in(norte_i18n::Lang::Es, key) }
fn row_labelled(m: &ContextMenuView, key: &str) -> u32 {
    let want = t(key);
    u32::try_from(m.items.iter().position(|i| i.label == want).expect(key)).expect("fits")
}
/// A host with the places bar and one drive at `mem:///otro`; returns the drive's row and generation.
async fn with_a_drive() -> (UiHost, norte_ui_host::controller::UiSubscription, u32, u64) {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumes = vec![volume("mem:///otro", "ext4", false)];
    let (h, _) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();
    let (row, generation) = snapshot_until(&h, &mut sub, "the drive row", |s| {
        let v = places(s)?;
        let i = v.rows.iter().position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))?;
        Some((u32::try_from(i).expect("fits"), v.generation))
    }).await;
    (h, sub, row, generation)
}

#[tokio::test]
async fn copy_path_of_a_place_copies_that_path() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let mut native = h.native_effects();
    h.dispatch(UiAction::ContextMenuPlace { row, generation, x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-copy-path") }).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await.expect("an effect").expect("channel alive");
    match effect {
        norte_ui_host::dto::NativeEffect::CopyBytes { bytes, count } => {
            assert_eq!(count, 1);
            assert!(String::from_utf8_lossy(&bytes).contains("otro"), "{bytes:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn open_here_from_a_place_is_the_click() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    h.dispatch(UiAction::ContextMenuPlace { row, generation, x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-open-here") }).await.expect("host alive");
    snapshot_until(&h, &mut sub, "the listing went to the drive", |s| {
        primer_listing(s).path_display.contains("otro").then_some(())
    }).await;
}

#[tokio::test]
async fn open_in_other_with_no_other_pane_says_so() {
    // `host_full` places ONE listing: there is no destination, and the
    // refusal is the transfer one (`host-no-other-slot` or
    // `host-no-target-designated`), not a silent nothing.
    let (h, mut sub, row, generation) = with_a_drive().await;
    h.dispatch(UiAction::ContextMenuPlace { row, generation, x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    let ack = h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-open-in-other") })
        .await.expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key.starts_with("host-no-")), "{ack:?}");
    assert!(!primer_listing(&snapshot(&h, &mut sub).await).path_display.contains("otro"));
}

#[tokio::test]
async fn a_branch_opens_in_a_new_tab() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    let generation = tree_of(&s).generation;
    h.dispatch(UiAction::ContextMenuBranch { row: 1, generation, x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-open-in-new-tab") }).await.expect("host alive");
    snapshot_until(&h, &mut sub, "a second tab listing docs", |s| {
        let two_tabs = s.layout.tabs.iter().any(|g| g.tabs.len() == 2);
        let docs = s.slots.iter().any(|v| matches!(v, SlotView::Browser(b) if b.path_display.ends_with("/casa/docs")));
        (two_tabs && docs).then_some(())
    }).await;
}

#[tokio::test]
async fn adding_a_branch_to_favorites_asks_its_name() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    h.dispatch(UiAction::ContextMenuBranch { row: 1, generation: tree_of(&s).generation, x: 0, y: 0 })
        .await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-add-favorite") }).await.expect("host alive");
    snapshot_until(&h, &mut sub, "the name dialog", |s| {
        s.dialogs.iter().any(|d| d.title_key == "modal-hotlist-name-title").then_some(())
    }).await;
}

#[tokio::test]
async fn fold_from_the_menu_is_the_twisty() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    let before = tree_of(&s).total;
    h.dispatch(UiAction::ContextMenuBranch { row: 0, generation: tree_of(&s).generation, x: 0, y: 0 })
        .await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-toggle-fold") }).await.expect("host alive");
    snapshot_until(&h, &mut sub, "the root folded", |s| (tree_of(s).total < before).then_some(())).await;
}

#[tokio::test]
async fn sort_and_hide_from_the_header() {
    let (h, _) = host(vec!["a.txt", "bb.txt"]).await;
    let mut sub = h.subscribe();
    let slot = browser(&snapshot(&h, &mut sub).await).slot_id;
    for (key, check) in [("ctx-sort-by-column", 0), ("ctx-hide-column", 1)] {
        h.dispatch(UiAction::ContextMenuHeader { slot_id: slot, column: "size".into(), x: 0, y: 0 })
            .await.expect("host alive");
        let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
        h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, key) }).await.expect("host alive");
        if check == 0 {
            snapshot_until(&h, &mut sub, "sorted by size", |s| {
                browser(s).columns.iter().any(|c| c.id == "size" && c.sort.is_some()).then_some(())
            }).await;
        } else {
            snapshot_until(&h, &mut sub, "size hidden", |s| {
                (!browser(s).columns.iter().any(|c| c.id == "size")).then_some(())
            }).await;
        }
    }
}

#[tokio::test]
async fn shift_f10_in_places_opens_on_its_cursor() {
    let (h, mut sub, _, _) = with_a_drive().await;
    let places_slot = places(&snapshot(&h, &mut sub).await).expect("placed").slot_id;
    h.dispatch(UiAction::FocusSlot { slot_id: places_slot }).await.expect("host alive");
    h.dispatch(key_mod("F10", false, true)).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert_eq!(m.x, None);
    assert!(m.items.iter().any(|i| i.label == t("ctx-open-here")));
}
```

And the favorite removal: a `full` layout (places placed), one hotlist entry in the settings
AND on disk in a temp user layer (the write dir `write_dir()` resolves to; `place` is
`settings_extensions.rs:112`):

```rust
#[tokio::test]
async fn removing_a_favorite_from_places() {
    let tmp = tempfile::tempdir().expect("tmp");
    norte_config::persist_hotlist_add(tmp.path(), "proyectos", "mem:///proyectos").expect("seeded");
    let mut settings = test_settings();
    settings.common.hotlist = vec![norte_config::HotlistItem {
        name: "proyectos".to_owned(),
        target: norte_proto::VPath::parse("mem:///proyectos").map_err(|_| "err".to_owned()),
    }];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///proyectos", vec![(b"p.txt".to_vec(), false)]);
    let (h, _) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings,
        paths: norte_ui_host::settings::HostPaths {
            config_layers: vec![(norte_ui_host::settings::ConfigLayer::User, place(tmp.path().to_path_buf()))],
            state_dir: None,
            logs_dir: None,
            socket: None,
        },
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    }).await.expect("starts");
    let mut sub = h.subscribe();
    let fav = snapshot_until(&h, &mut sub, "the favorite row", |s| {
        let v = places(s)?;
        let i = v.rows.iter().position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))?;
        Some((u32::try_from(i).expect("fits"), v.generation))
    }).await;
    h.dispatch(UiAction::ContextMenuPlace { row: fav.0, generation: fav.1, x: 0, y: 0 }).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    h.dispatch(UiAction::ContextMenuActivateRow { row: row_labelled(&m, "ctx-remove-favorite") }).await.expect("host alive");
    snapshot_until(&h, &mut sub, "the favorite is gone", |s| {
        (!places(s)?.rows.iter().any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))).then_some(())
    }).await;
}
```

(If `HostPaths` has more fields than these four, add them as `None`/defaults; if `tempfile` is not a
dev-dependency of `norte-ui-host`, use the temp-dir helper the `terminals.rs` tests use for `_tmp`.)
Field names (`v.generation`, `tree_of(..).total`,
`s.layout.tabs[..].tabs`, `places(..)?.slot_id`) are the DTOs' — if one differs, the compiler says
which; keep the assertion's meaning.

- [ ] **Step 2:** `just t norte-ui-host` → fail.
- [ ] **Step 3: Implement.**
  - `open_place_menu`: generation check against `self.gen_places`; row in range; `state.set_cursor(row)`; surface from the `PlaceRow`: `Header` → `SectionHeader`, `Drive` → `Drive`, `Favorite` → `Favorite`; header = the row's label (masked, elided); `subject: Subject::Place(row)`; `slot = self.places_slot()`. For a favorite with a broken target, the three Open verbs are disabled with the broken reason (extend `availability_of`: `Subject::Place(i)` whose row is `Favorite { target: Err(key), .. }` → disabled, reason = `t_in(lang, key)`; this is the ONLY verb veto besides `HideColumn`).
  - `open_branch_menu`: same against `self.gen_branches`, `tree.set_cursor(row)`, header = the branch's display path.
  - `run_verb(verb, subject)`: resolve the subject's path NOW (places: `state.rows()[i]` → `Drive { mount, .. }` / `Favorite { target: Ok(v), .. }`; tree: `tree.set_cursor(i); tree.selected()`); a vanished row → stale.
    - `OpenHere`: places → `activate_place_at_cursor`; branch → `touch_branch(i, self.gen_branches, true, …)`.
    - `OpenInOther`: `match self.slot_dest() { Ok(d) => navigate_slot(d, &path, Trail::Record, …), Err(key) => (Unavailable{reason_key: key}, self.say(key)) }`.
    - `OpenInNewTab`: `tab_new(…)` (refocus the listing first if focus is on places/tree, as `click_layout_button` does), then `navigate(&path, Trail::Record, …)` — the new tab is the active slot after `tab_new`.
    - `CopyPath`: `clipboard_bytes(&[path])` → `self.native(NativeEffect::CopyBytes { bytes, count: 1 })` (else `without_desktop()`), message `msg-paths-copied { n: 1 }`.
    - `AddFavorite`: `request_favorite_of(path)`.
    - `RemoveFavorite`: `remove_favorite_named(name, mailbox)` with the RAW `name` from `PlaceRow::Favorite`.
    - `ToggleFold`: places → `set_cursor(i)` + the header branch of `activate_place_at_cursor` (extract `fold_place_at_cursor`); branch → `touch_branch(i, gen, false, …)`.
    - `SortByColumn`: `sort_by(self.active(), &column)`.
    - `HideColumn`: the slot scheme's current ids from `self.columns` minus `column` → `self.columns.apply_picked(Some(scheme)?, &ids, None)` (match the selector's `scheme_target` convention: `rg -n "scheme_target" src`), then the footprint/`re_list` loop from `apply_columns`, then a snapshot. Extract that loop into `fn relist_changed_footprints(&mut self, before, backend, mailbox)` and use it from both.
  - Replace Task 4's `// Task 5` placeholders (dispatch arms, `Effect::ContextMenu` branches, `run_context_entry`'s verb arm).
- [ ] **Step 4:** `just t norte-ui-host` green; `just c`.
- [ ] **Step 5:** Commit `feat(host): context menu verbs for places, the tree and the header`.

---

### Task 6: Renderer — paint, report, suppress the native menu

**Files:**
- Create: `crates/norte-gui-tauri/ui/src/render/contextmenu.ts`
- Modify: `crates/norte-gui-tauri/ui/src/types.ts` (`BRIDGE_VERSION = 107`; `ContextMenuView`, `ContextItemView`; `ViewSnapshot.context_menu?: ContextMenuView | null`; the `ViewChange` member; the `UiAction` members)
- Modify: `crates/norte-gui-tauri/ui/src/session.ts` (`case "context_menu": s.context_menu = c.context_menu; return true;`)
- Modify: `crates/norte-gui-tauri/ui/src/render.ts` (`layer("context_menu", view.context_menu ?? null, this.paintContextMenu)`; the listing `mousedown` button guard; row/empty/header `contextmenu` listeners in `wire`)
- Modify: `crates/norte-gui-tauri/ui/src/render/menus.ts` (extract `placeInsideWindow(box, x, y)` from `popupMenu`; `popupMenu` uses it; opening a `popupMenu` sends `context_menu_close` if a host menu is painted)
- Modify: `crates/norte-gui-tauri/ui/src/render/places.ts` (`contextmenu` on `.places-row` and `li.tree-row`)
- Modify: `crates/norte-gui-tauri/ui/src/main.ts` (document-level `contextmenu` suppression)
- Modify: `crates/norte-gui-tauri/ui/src/style.css` (`.context-menu` reusing the `.tab-menu` tokens; section title; disabled + reason line; `[aria-selected="true"]` cursor)
- Test: `crates/norte-gui-tauri/ui/tests/render.test.ts`, `tests/session.test.ts`

**Interfaces:**
- Consumes: the bridge 107 types from Task 3.
- Produces: `export function paintContextMenu(this: Screen, menu: ContextMenuView | null): void`; `export function placeInsideWindow(box: HTMLElement, x: number, y: number): void`.

- [ ] **Step 1: Failing tests** in `render.test.ts` (existing helpers: `mount()`, `view({})` — a listing with rows `a.txt`/`b.txt` and columns `name`/`size` —, `realCatalog()`; first move `withPlaces` out of `describe("the places sidebar")` to module scope so both describes use it):

```ts
describe("context menu", () => {
  const menuView = (over: Partial<ContextMenuView> = {}): ContextMenuView => ({
    header: "acts on a.txt",
    items: [
      { label: "Open", chord: "Enter", enabled: true, reason: "", section: null, role: "normal" },
      { label: "Move", chord: "F6", enabled: false, reason: "read-only backend", section: "", role: "normal" },
      { label: "Delete", chord: "F8", enabled: true, reason: "", section: null, role: "destructive" },
    ],
    cursor: 0, x: 30, y: 40, ...over,
  });

  it("suppresses the webview's menu everywhere but in a text field", () => {
    mount();
    const div = document.createElement("div");
    const input = document.createElement("input");
    document.body.append(div, input);
    const a = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    div.dispatchEvent(a);
    expect(a.defaultPrevented).toBe(true);
    const b = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    input.dispatchEvent(b);
    expect(b.defaultPrevented).toBe(false);
  });

  it("a right press on a row neither selects nor double-clicks", () => {
    const { screen, sent } = mount();
    screen.paint(view({})); // the existing fixture with two rows
    const row = document.querySelector(".row") as HTMLElement;
    for (let i = 0; i < 2; i++) {
      row.dispatchEvent(new MouseEvent("mousedown", { button: 2, bubbles: true, cancelable: true }));
    }
    expect(sent.filter((a) => a.action === "select_row" || a.action === "activate")).toEqual([]);
  });

  it("a right click on a row asks the host, with the row's key and generation", () => {
    const { screen, sent } = mount();
    screen.paint(view({}));
    const row = document.querySelector(".row") as HTMLElement;
    row.dispatchEvent(new MouseEvent("contextmenu", { button: 2, bubbles: true, cancelable: true, clientX: 12.6, clientY: 30.2 }));
    expect(sent.at(-1)).toEqual({ action: "context_menu_row", slot_id: 1, key: 0, generation: 1, x: 13, y: 30 });
  });

  it("below the last row it is the empty area; on a header, that column", () => {
    const { screen, sent } = mount();
    screen.paint(view({}));
    (document.querySelector(".scroller") as HTMLElement).dispatchEvent(
      new MouseEvent("contextmenu", { button: 2, bubbles: true, cancelable: true }));
    expect(sent.at(-1)).toMatchObject({ action: "context_menu_empty" });
    (document.querySelector('[data-column="size"]') as HTMLElement).dispatchEvent(
      new MouseEvent("contextmenu", { button: 2, bubbles: true, cancelable: true }));
    expect(sent.at(-1)).toMatchObject({ action: "context_menu_header", column: "size" });
  });

  it("places and tree rows ask for their own menus", () => {
    const { screen, sent } = mount();
    const v = withPlaces(0); // moved to module scope from "the places sidebar"
    v.slots = [
      ...v.slots,
      {
        kind: "tree" as const,
        slot_id: 8,
        rows: [
          { label: "home", hostile: false, depth: 0, expanded: true, children: true },
          { label: "docs", hostile: false, depth: 1, expanded: false, children: null },
        ],
        first: 40,
        total: 120,
        cursor: 40,
        generation: 3,
      },
    ];
    v.layout.placements = [
      ...v.layout.placements,
      { slot_id: 8, x: 30, y: 0, width: 30, height: 10, role: null, focus_index: 3 },
    ];
    screen.paint(v);
    const right = { button: 2, bubbles: true, cancelable: true, clientX: 5, clientY: 6 };
    document.querySelectorAll<HTMLElement>(".places-row")[1]?.dispatchEvent(new MouseEvent("contextmenu", right));
    expect(sent.at(-1)).toEqual({ action: "context_menu_place", row: 1, generation: 3, x: 5, y: 6 });
    document.querySelectorAll<HTMLElement>(".tree-row")[1]?.dispatchEvent(new MouseEvent("contextmenu", right));
    expect(sent.at(-1)).toEqual({ action: "context_menu_branch", row: 41, generation: 3, x: 5, y: 6 });
  });

  it("paints the host's menu: header, sections, disabled with its reason, the cursor", () => {
    const { screen } = mount();
    screen.paint({ ...view({}), context_menu: menuView() });
    const box = document.querySelector(".context-menu") as HTMLElement;
    expect(box.querySelector(".context-menu-header")?.textContent).toBe("acts on a.txt");
    const items = [...box.querySelectorAll(".context-menu-item")] as HTMLElement[];
    expect(items).toHaveLength(3);
    expect(items[0]?.getAttribute("aria-selected")).toBe("true");
    expect(items[1]?.getAttribute("aria-disabled")).toBe("true");
    expect(items[1]?.textContent).toContain("read-only backend");
    expect(items[2]?.dataset["role"]).toBe("destructive");
    expect(box.querySelectorAll(".context-menu-rule")).toHaveLength(1);
  });

  it("hover points, click activates, outside closes, blur closes", () => {
    const { screen, sent } = mount();
    screen.paint({ ...view({}), context_menu: menuView() });
    const items = [...document.querySelectorAll(".context-menu-item")] as HTMLElement[];
    items[2]?.dispatchEvent(new MouseEvent("mousemove", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "context_menu_point_row", row: 2 });
    items[2]?.click();
    expect(sent.at(-1)).toEqual({ action: "context_menu_activate_row", row: 2 });
    document.body.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    expect(sent.at(-1)).toEqual({ action: "context_menu_close" });
    window.dispatchEvent(new Event("blur"));
    expect(sent.at(-1)).toEqual({ action: "context_menu_close" });
  });

  it("stays inside the window near the bottom-right corner", () => {
    const { screen } = mount();
    screen.paint({ ...view({}), context_menu: menuView({ x: window.innerWidth - 1, y: window.innerHeight - 1 }) });
    const box = document.querySelector(".context-menu") as HTMLElement;
    expect(parseFloat(box.style.left)).toBeLessThan(window.innerWidth);
    expect(parseFloat(box.style.top)).toBeLessThan(window.innerHeight);
  });

  it("with no anchor it sits under the focused row", () => {
    const { screen } = mount();
    screen.paint({ ...view({}), context_menu: menuView({ x: null, y: null }) });
    expect(document.querySelector(".context-menu")).not.toBeNull();
  });

  // Review Focus 1.
  it("the keyboard Menu key does not open a second menu at the origin", () => {
    const { screen, sent } = mount();
    screen.paint(view({}));
    const row = document.querySelector(".row") as HTMLElement;
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "ContextMenu", bubbles: true, cancelable: true }));
    row.dispatchEvent(new MouseEvent("contextmenu", { button: 0, bubbles: true, cancelable: true, clientX: 0, clientY: 0 }));
    expect(sent.filter((a) => a.action === "context_menu_row")).toEqual([]);
  });
});
```

The `places and tree rows` case must be real code, not the comment. In `session.test.ts`, one case: a `{ change: "context_menu", context_menu: null }` patch clears `s.context_menu`. Review Focus 4 (renderer half): with `dialogs` non-empty in the painted view, a row `contextmenu` is still `defaultPrevented`.

- [ ] **Step 2:** `cd crates/norte-gui-tauri/ui && npx vitest run` → fail.
- [ ] **Step 3: Implement.**
  - `main.ts`: `doc.addEventListener("contextmenu", (e) => { const t = e.target; if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return; e.preventDefault(); });` with a comment naming WebKitGTK's Back/Reload/Inspect menu.
  - Keyboard-origin `contextmenu`: specific listeners ignore events with `e.button !== 2` (a keyboard-triggered `contextmenu` reports `button: 0`); the host opens the keyboard menu from `pane.context-menu`.
  - `render.ts` `wire`: first line of the scroller `mousedown` handler: `if (e.button !== 0) { return; }` (side buttons are handled on `mouseup` by the root listener). New `dom.scroller.addEventListener("contextmenu", …)`: `e.button !== 2` → return; `.row` hit → `context_menu_row { slot_id, key, generation: dom.generation, x: Math.round(e.clientX), y: Math.round(e.clientY) }`; otherwise → `context_menu_empty`. `dom.header.addEventListener("contextmenu", …)`: the `[data-column]` hit → `context_menu_header { column }`.
  - `places.ts`: on `.places-row` → `context_menu_place { row: i, generation }`; on `li.tree-row` → `context_menu_branch { row: first + i, generation }` (same row arithmetic as its click handler).
  - `contextmenu.ts` `paintContextMenu`: `null` → remove `.context-menu` and its listeners. Else build `<div class="context-menu" role="menu">` with `.context-menu-header` (`textContent`), then per item: a `section` non-null → `.context-menu-rule` (and a `.context-menu-section` title if non-empty) BEFORE the item; `<div class="context-menu-item" role="menuitem" data-role=… aria-selected aria-disabled>` with label, chord (`.context-menu-chord`) and, when `reason`, a `.context-menu-reason` line. All text via `textContent` (names are hostile input). `mousemove` → `context_menu_point_row` only when the row differs from `menu.cursor`; `click` → `context_menu_activate_row` (also for disabled ones: the host decides). Position: `x/y` → `placeInsideWindow(box, x, y)`; `null` → the focused listing's `[aria-selected="true"].row` (or the focused places/tree row) `getBoundingClientRect()` → `(left + 16, bottom)`. Outside `pointerdown` (capture), `blur`, `resize` → `context_menu_close`. Keys are NOT handled here.
  - `menus.ts`: extract `placeInsideWindow`; `popupMenu` calls it; at `popupMenu` open, if `document.querySelector(".context-menu")` exists, `this.send({ action: "context_menu_close" })` — `popupMenu` is not a `Screen` method, so pass a `onOpen?: () => void` from its two callers (`openTabMenu`, the terminal list) that sends it.
  - `style.css`: `.context-menu` = the `.tab-menu` rules (copy the token names, do not invent new variables — `gui-ci` fails on a variable no theme feeds); destructive uses the menu-bar dropdown's destructive token; `.context-menu-reason` smaller and dimmed.
- [ ] **Step 4:** `npx vitest run`, `npm run lint`, `npm run fmt:check` green; then **`just gui-ci`**.
- [ ] **Step 5:** Commit `feat(gui): the context menu, painted from the host`.

---

### Task 7: Docs and changelog

**Files:**
- Modify: `crates/norte-help/topics/en/mouse.md`, `crates/norte-help/topics/es/mouse.md` ("The right-click menu" section)
- Modify: `crates/norte-cli/tests/goldens/help-en.json` (+ `help-es.json` if present) — regenerated
- Modify: `CHANGELOG.md` (Unreleased)

- [ ] **Step 1:** Rewrite "The right-click menu" (en, then the same content in es) to say: where it opens (row, empty area and `..`, column header, Places, tree); Shift+F10 / the Menu key open it on the focused item; the core entries and that extras appear only where they apply; every entry runs the same command as its key (use `{{cmd:…}}` live marks for `pane.view`, `pane.copy`, `pane.move`, `pane.rename`, `pane.delete`, `pane.context-menu`); dimmed entries show why; **the marks rule unchanged** (keep the existing "Getting there costs something" paragraph verbatim in substance); AI and disconnect live in the empty-area menu because they act on the whole folder.
- [ ] **Step 2:** `just t norte-help` and `just t norte-cli`; regenerate the help goldens with the env var the failing golden test names; re-run green.
- [ ] **Step 3:** CHANGELOG entry under Unreleased: "**Context menu in the window** (bridge 107)": surfaces, keyboard, host-owned, same commands as keys, marks rule kept, the right button no longer selects/enters, the webview's own menu is gone.
- [ ] **Step 4:** Commit `docs: the window's context menu in help and the changelog`.

---

## Finish

- [ ] `git log --oneline main..` reviewed; branch merged to `main` ONCE (`git switch main && git merge --no-ff feat/gui-context-menu`), then `git push origin main` — the pre-push hook is the gate (`ci-fast` + `gui-ci`). Do not run `ci-fast` by hand before the push.
