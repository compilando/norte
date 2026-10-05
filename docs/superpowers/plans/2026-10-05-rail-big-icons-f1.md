# Big rail icons, phase F1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a terminal that speaks kitty graphics, the TUI's panel column draws each panel as the window's own SVG icon at 2×2 cells; everywhere else it stays today's one-cell glyph.

**Architecture:** The panel SVGs move to one shared folder read by both frontends. A `RailBackend` chosen at startup and a single `rail_layout` decide big vs small for painting, mouse and placement. The run loop rasterises with `resvg` and places PNGs through kitty's protocol after `terminal.draw`, the ADR 0118 pattern, with a reserved id range erased at the four existing sites.

**Tech Stack:** Rust (ratatui, crossterm), `resvg` (new), kitty graphics protocol; TypeScript (vite `?raw`) in the window.

**Spec:** `docs/superpowers/specs/2026-10-05-panel-rail-big-icons-design.md`

## Global Constraints

- Detection by probe only (`kitty_graphics::supported()`), never by `TERM`.
- `[ui] images = "off"` or `"blocks"` forces the glyph column; no new config key.
- `[ui] panel_bar_style = "letters"` forces the glyph column.
- Big column: width 4 (rule, 2-cell icon, badge), 2 rows per button; small: width 3, today's `rail_rows`.
- A per-icon failure leaves that slot blank for the frame; never switches size mid-session.
- Rail kitty ids live in a reserved range distinct from the viewer's ids.
- Hard rules: no `unwrap`/`expect` outside tests without an invariant comment; no `std::fs` in the TUI; new dependency justified in the PR (rule 8).
- Every key/i18n string stays as is: no new user-facing strings except the CHANGELOG and help paragraph.
- Tests: `just t <crate>`; never `cargo test` for the suite. `cargo test -p <crate> --doc` when touching documented items. Window: `just gui-ci`.

## Review Focus

1. A dropdown menu, which-key, help or palette over the column: the icons must vanish (kitty pixels sit ABOVE text). Pinned in Task 4 (`no_icons_under_an_overlay`).
2. Resizing the terminal shorter than 2 rows per button: the column drops to small without leaving old pixels behind. Pinned in Task 4 (`shrinking_drops_to_glyphs_and_erases`).
3. Quitting, `ctrl+z` and a panic with icons placed: no icon left on the shell's screen. Pinned in Task 4 (erase sites call `rail_icons::delete_all`, tested by escape content).
4. Theme switch at runtime (`F11`/settings): the colours follow; a stale-colour image must not stay. Pinned in Task 4 (`a_new_colour_replaces_the_old_image`).
5. A plugin panel kind with no SVG: it keeps its letter in a big column (centred in the 2×2 slot), never a blank. Pinned in Task 2 (`a_kind_without_svg_paints_its_letter_when_big`).

---

### Task 1: One home for the panel icons

**Files:**
- Create: `crates/norte-frontend/assets/panel-icons/{places,tree,viewer,processes,metadata,log,disk-map,timeline}.svg`
- Modify: `crates/norte-frontend/src/chrome/panelbar.rs` (add `icon_svg`, `button_count`)
- Modify: `crates/norte-gui-tauri/ui/src/render/icons.ts` (panel kinds read the files)
- Modify: `crates/norte-gui-tauri/ui/vite.config.ts` (allow `../../norte-frontend/assets` if vite refuses it)
- Test: `crates/norte-frontend/src/chrome/panelbar.rs` (unit), `crates/norte-gui-tauri/ui/tests/render.test.ts`

**Interfaces:**
- Produces: `norte_frontend::panelbar::icon_svg(kind: &str) -> Option<&'static str>`; `norte_frontend::panelbar::button_count(reg: &KindRegistry) -> usize`.

- [ ] **Step 1: Write the SVG files.** One per kind, copied from `SHAPES` in `icons.ts` (paths and circles verbatim). Template:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"/></svg>
```

The root attributes are for `resvg`; the window's CSS (`.panelbar-icon`) still wins there.

- [ ] **Step 2: Failing test in `panelbar.rs`:**

```rust
#[test]
fn every_toggle_kind_has_a_shared_svg() {
    for (kind, _) in TOGGLES {
        let svg = icon_svg(kind).unwrap_or_else(|| panic!("{kind} has no svg"));
        assert!(svg.starts_with("<svg") && svg.contains("currentColor"), "{kind}");
    }
    assert_eq!(icon_svg("a-plugin-kind"), None);
}
```

Run: `just t norte-frontend` → FAIL (`icon_svg` not found).

- [ ] **Step 3: Implement.**

```rust
/// The panel's icon as SVG source, shared with the window
/// (`assets/panel-icons`); `None` for a kind without one (a plugin's).
#[must_use]
pub fn icon_svg(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "places" => include_str!("../../assets/panel-icons/places.svg"),
        // … one arm per file …
        _ => return None,
    })
}

/// How many buttons the bar has: the registry's, whatever their state.
#[must_use]
pub fn button_count(reg: &KindRegistry) -> usize {
    reg.decls().filter(|d| es_button(d)).count()
}
```

Run: `just t norte-frontend` → PASS. Then `cargo test -p norte-frontend --doc`.

- [ ] **Step 4: The window reads the files.** In `icons.ts`, remove the eight panel entries from `SHAPES` and add:

```ts
import places from "../../../../norte-frontend/assets/panel-icons/places.svg?raw";
// … one import per file …
const PANEL_SVG: Record<string, string> = { places, tree, viewer, processes, metadata, log, "disk-map": diskMap, timeline };
```

In `icon()`, when `PANEL_SVG[kind]` exists: parse it with `new DOMParser().parseFromString(src, "image/svg+xml")`, create the same `<svg class="panelbar-icon" viewBox aria-hidden focusable>` as today and append the parsed root's children (`doc.importNode(child, true)`). The `fs:favorite` star keeps its own entry. If vite rejects the path, add `server: { fs: { allow: [".."] } }` scoped to the repo in `vite.config.ts`; add `declare module "*.svg?raw"` if `tsc` needs it.

- [ ] **Step 5: Window test.** In `render.test.ts`, assert `icon(document, "places")` has one `path` child with the star's `d`, and `icon(document, "metadata")` has a `circle`. Run `just gui-ci` → PASS.

- [ ] **Step 6: Commit** `refactor(frontend): panel icons live in one shared folder`.

---

### Task 2: `RailBackend` and `rail_layout`

**Files:**
- Create: `crates/norte-tui/src/rail_icons.rs` (backend enum and choice; later tasks add raster and escapes)
- Modify: `crates/norte-tui/src/lib.rs` (module), `crates/norte-tui/src/app.rs` (field), `crates/norte-tui/src/main.rs` (set at startup), `crates/norte-tui/src/config_reload.rs` (recompute)
- Modify: `crates/norte-tui/src/ui/geometry.rs` (`rail_layout`, width), `crates/norte-tui/src/ui/chrome.rs` (`draw_rail`, `panel_bar_zones`)
- Test: `crates/norte-tui/src/ui/geometry.rs` (unit), `crates/norte-tui/tests/mouse.rs`, `crates/norte-tui/tests/theme_render.rs`

**Interfaces:**
- Consumes: `button_count` (Task 1), `kitty_graphics::supported()`.
- Produces:

```rust
// rail_icons.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailBackend { KittyGraphics, #[default] Glyph }
pub fn choose(images: norte_config::Images, kitty: bool) -> RailBackend;

// app.rs
pub rail_backend: RailBackend,   // Glyph in App::new; main/config_reload set it

// geometry.rs
pub(crate) struct RailSlot { pub y: u16, pub height: u16 }
pub(crate) struct RailLayout { pub big: bool, pub width: u16, pub slots: Vec<RailSlot> }
pub(crate) fn rail_layout(app: &App, height: u16, top: u16) -> RailLayout;
```

- [ ] **Step 1: Failing tests (geometry.rs `#[cfg(test)]`):**

```rust
#[test]
fn choose_follows_the_probe_and_the_images_switch() {
    use norte_config::Images;
    assert_eq!(choose(Images::Auto, true), RailBackend::KittyGraphics);
    assert_eq!(choose(Images::Kitty, false), RailBackend::KittyGraphics);
    assert_eq!(choose(Images::Auto, false), RailBackend::Glyph);
    assert_eq!(choose(Images::Off, true), RailBackend::Glyph);
    assert_eq!(choose(Images::Blocks, true), RailBackend::Glyph);
}

#[test]
fn big_needs_the_backend_two_rows_each_and_not_letters() {
    let mut app = test_app();               // App::new, column default
    let n = /* button_count */;
    app.rail_backend = RailBackend::KittyGraphics;
    let l = rail_layout(&app, (3 * n) as u16, 1);
    assert!(l.big && l.width == 4);
    assert!(l.slots.iter().all(|s| s.height == 2));
    assert_eq!(l.slots[0].y, 2, "air on top when it fits");
    let tight = rail_layout(&app, (2 * n) as u16, 1);
    assert!(tight.big && tight.slots[1].y == tight.slots[0].y + 2, "packed");
    assert!(!rail_layout(&app, (2 * n - 1) as u16, 1).big, "no room: small");
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    assert!(!rail_layout(&app, 100, 1).big);
    app.chrome.panel_bar_style = None;
    app.rail_backend = RailBackend::Glyph;
    let small = rail_layout(&app, 100, 1);
    assert!(!small.big && small.width == 3 && small.slots.iter().all(|s| s.height == 1));
}
```

Run `just t norte-tui` → FAIL.

- [ ] **Step 2: Implement.** `choose`: `match images { Off | Blocks => Glyph, Kitty => KittyGraphics, Auto if kitty => KittyGraphics, Auto => Glyph }`. `rail_layout(app, height, top)`: `n = button_count(registry)`; `big = app.rail_backend != Glyph && style != Letters && 2*n <= height`; if big: air (`from = 1, step = 3`) when `3*n <= height`, else `from = 0, step = 2`; slots `{ y: top + from + i*step, height: 2 }`; width 4. Else: the current `rail_rows` logic moved here (`height 1`), width 3. `RAIL_W` goes away: `panel_bar_area` computes `rail_layout(app, height, y).width`; `chrome::rail_rows` is deleted and both `panel_bar_zones` and `draw_rail` take `rail_layout(app, bar.height, bar.y).slots` — zones span `y..y+height`.

- [ ] **Step 3: Paint the big slot in `draw_rail`.** For a big slot: column x = rule, x+1..x+2 = icon, x+3 = badge. Rule `▎` (state style) on BOTH rows; icon cells are spaces in the rail's background (Task 4 places pixels over them); badge on the lower row. A kind with `icon_svg == None` paints its letter at (x+1, y) in the icon style instead of spaces — Task 4 does not place anything for it.

- [ ] **Step 4: Tests for painting and mouse parity:**

```rust
// theme_render.rs
#[test]
fn a_big_column_reserves_two_by_two_and_rules_both_rows() { /* rail_backend = KittyGraphics,
  80×40, toggle_places; find the rows whose x=0 is "▎": exactly two consecutive rows for
  places; cells (1,y),(2,y),(1,y+1),(2,y+1) are " " */ }

#[test]
fn a_kind_without_svg_paints_its_letter_when_big() { /* register a plugin kind through the
  same path tests/mouse.rs uses for plugin panels; its slot shows its letter at x=1 */ }

// mouse.rs
#[test]
fn big_rail_clicks_land_on_both_rows_of_a_button() { /* rail_backend = KittyGraphics, H=40;
  a click on (1, y) and on (1, y+1) of places' slot both run `layout.places` */ }
```

Run `just t norte-tui` → PASS (existing tests unaffected: `App::new` is `Glyph`).

- [ ] **Step 5: Wire startup.** In `main.rs`, right after `kitty_graphics::query_support()`: `app.rail_backend = rail_icons::choose(app.chrome.images(), kitty_graphics::supported());`. Same line where `config_reload.rs:98` already recomputes the image mode. `cargo check -p norte-tui --all-targets`.

- [ ] **Step 6: Commit** `feat(tui): the panel column can lay out big slots`.

---

### Task 3: Rasterise an icon

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies] resvg`), `crates/norte-tui/Cargo.toml`
- Modify: `crates/norte-tui/src/rail_icons.rs`, `crates/norte-tui/src/theme.rs` (`role_rgb`)
- Test: `crates/norte-tui/src/rail_icons.rs` (unit)

**Interfaces:**
- Consumes: `icon_svg` (Task 1).
- Produces:

```rust
// theme.rs
impl TuiTheme { pub fn role_rgb(&self, role: Role) -> Option<[u8; 3]>; }   // the theme's raw fg
// rail_icons.rs
pub const RASTER_PX: u32 = 64;
pub fn png(kind: &str, rgb: [u8; 3]) -> Option<std::sync::Arc<Vec<u8>>>;  // cached by (kind, rgb)
```

- [ ] **Step 1: Dependency.** `resvg = { version = "0.48", default-features = false }` in the workspace; `resvg.workspace = true` in norte-tui. Check `cargo tree -p norte-tui -e features -i tiny-skia` shows the PNG encoder (`png-format`); if not, add `tiny-skia = { version = "<resvg's>", default-features = false, features = ["std", "png-format"] }`. No `text`/`system-fonts` features: icons have no text. Note size in the commit for the PR's rule-8 paragraph (`cargo tree -p resvg --no-default-features | wc -l`).

- [ ] **Step 2: Failing test:**

```rust
#[test]
fn the_star_rasterises_in_the_requested_colour() {
    let bytes = png("places", [0xff, 0x00, 0x00]).expect("places has an svg");
    let pix = resvg::tiny_skia::Pixmap::decode_png(&bytes).expect("valid png");
    assert_eq!((pix.width(), pix.height()), (RASTER_PX, RASTER_PX));
    // A point ON the star's top stroke (12, 3.5 in the 24 grid).
    let p = pix.pixel(RASTER_PX / 2, RASTER_PX * 35 / 240 + 1).expect("inside");
    assert!(p.red() > 200 && p.green() < 40 && p.alpha() > 200, "{p:?}");
    assert!(png("a-plugin-kind", [0, 0, 0]).is_none());
}
```

Run `just t norte-tui` → FAIL.

- [ ] **Step 3: Implement.** `png`: look up a `Mutex<HashMap<(String,[u8;3]), Arc<Vec<u8>>>>` (`OnceLock`); on miss, `icon_svg(kind)?.replace("currentColor", &format!("#{r:02x}{g:02x}{b:02x}"))`, `usvg::Tree::from_str(&src, &usvg::Options::default()).ok()?`, `Pixmap::new(RASTER_PX, RASTER_PX)?`, `resvg::render(&tree, Transform::from_scale(RASTER_PX as f32 / 24.0, …), &mut pixmap.as_mut())`, `pixmap.encode_png().ok()?`. A poisoned mutex: `lock().unwrap_or_else(PoisonError::into_inner)`. Failures log at `debug` and return `None`. `role_rgb`: `self.theme.style(role).fg.map(|c| [c.r, c.g, c.b])`.

- [ ] **Step 4:** `just t norte-tui` → PASS; `just c`.

- [ ] **Step 5: Commit** `feat(tui): rasterise a panel icon with resvg` (body: the rule-8 justification — benefit, size, maintenance, alternatives: hand-written stroker, `image`+font glyphs).

---

### Task 4: Place, replace and erase the icons

**Files:**
- Modify: `crates/norte-tui/src/rail_icons.rs` (desired set, escapes, placed state), `crates/norte-tui/src/ui.rs` (`rail_icons_to_place`), `crates/norte-tui/src/event_loop.rs` (after the viewer block), `crates/norte-tui/src/suspend.rs:382`, `crates/norte-tui/src/tty.rs:115,145`
- Test: `crates/norte-tui/src/rail_icons.rs` (unit), `crates/norte-tui/tests/theme_render.rs`

**Interfaces:**
- Consumes: `rail_layout`, `png`, `role_rgb`, `kitty_graphics::escape_place` (its `PLACEMENT`/`C=1`/`q=2` conventions).
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailIcon { pub kind: String, pub rgb: [u8; 3], pub rect: Rect }  // rect = the 2×2 icon cells
pub const ID_BASE: u32 = 0x4E52_0000;            // "NR" — viewer ids count up from 1
pub fn id_for(slot_index: usize) -> u32;         // ID_BASE + index
pub fn sync(out: &mut impl Write, want: &[RailIcon]);   // diff against PLACED, write escapes
pub fn delete_all(out: &mut impl Write);         // `a=d,d=R,x=ID_BASE,y=ID_BASE+63`, forget state
// ui.rs
pub fn rail_icons_to_place(app: &App, area: Rect) -> Vec<RailIcon>;
```

- [ ] **Step 1: Failing tests:**

```rust
// rail_icons.rs — `sync` against a Vec<u8> writer
#[test]
fn the_first_sync_places_and_the_second_writes_nothing() { /* want = [places at (1,2,2,2)];
  sync → output contains "a=T,i={id_for(0)}" and "c=2,r=2"; sync again with same want → empty */ }
#[test]
fn a_new_colour_replaces_the_old_image() { /* same rect, other rgb → output deletes
  "d=I,i={id}" then places again */ }
#[test]
fn shrinking_drops_to_glyphs_and_erases() { /* sync(want) then sync(&[]) → "d=I" for it */ }
#[test]
fn delete_all_clears_the_range_and_the_state() { /* contains "d=R" with both bounds;
  a later sync(want) places again */ }

// theme_render.rs
#[test]
fn no_icons_under_an_overlay() { /* rail_backend = KittyGraphics, 80×40: rail_icons_to_place
  non-empty; with app.menu open, with help open, with the palette open → empty */ }
#[test]
fn icons_sit_on_the_reserved_cells() { /* each RailIcon.rect is (x+1, slot.y, 2, 2) of
  rail_layout, and only kinds with icon_svg */ }
```

Run `just t norte-tui` → FAIL.

- [ ] **Step 2: Implement `rail_icons_to_place`:** `None` from `geometry::panel_bar_visible` → empty (covers menu, help, palette, modals, which-key — check `mouse::overlay_open` includes which-key; if not, add it there, since the click zones need it too). Else `rail_layout`; if not `big` → empty. Zip slots with `panel_buttons`; skip kinds without `icon_svg`; colour by state: Closed → `Muted` (fallback `Regular`), Open/Focused → `Title` (fallback `Regular`), via `role_rgb`; no colour → skip.

- [ ] **Step 3: Implement `sync`:** process state `static PLACED: Mutex<Vec<Option<(String,[u8;3],Rect)>>>` indexed by slot. Per slot: same as wanted → nothing; different → if something there, `escape_delete(id)`; then `MoveTo(rect.x, rect.y)` + `kitty_graphics::escape_place(id, &png, rect, None)` (it already writes `C=1`, `q=2`, `p=1`). Slots no longer wanted → `escape_delete`. A failed write: terminator `\x1b\\`, `debug!`, forget that slot (retry next frame). `delete_all`: one `\x1b_Ga=d,d=R,x={ID_BASE},y={ID_BASE+63},q=2\x1b\\`, clear `PLACED`.

- [ ] **Step 4: Wire.** In `event_loop.rs`, after the viewer block: `if app.rail_backend == RailBackend::KittyGraphics { rail_icons::sync(terminal.backend_mut(), &ui::rail_icons_to_place(app, painted_area)); }`. Add `rail_icons::delete_all(...)` next to each `kitty_graphics::delete_placed` in `suspend.rs` and `tty.rs` (restore and the panic hook). After a resize, ratatui clears the screen — kitty keeps images: `sync` compares rects, so moved slots are re-placed; unchanged rects survive the clear (images are not cells).

- [ ] **Step 5:** `just t norte-tui` → PASS; `cargo test -p norte-tui --doc`; `just c`.

- [ ] **Step 6: Commit** `feat(tui): big panel icons in kitty`.

---

### Task 5: Docs, manual check, close

**Files:**
- Create: `docs/adr/0169-the-terminal-panel-column-draws-pictures.md`, row in `docs/adr/README.md`
- Modify: `CHANGELOG.md` (Added), `crates/norte-help/topics/{en,es}/appearance.md` (one sentence each), then `NORTE_UPDATE_GOLDEN=1 just t norte-cli`

- [ ] **Step 1:** ADR 0169: decision (images not OSC 66, shared SVGs, `images` switch, reserved ids), consequences (tmux → glyphs; F2 sixel). CHANGELOG: "In kitty, the terminal's panel column shows the window's icons at double size; other terminals keep the one-cell icons." Help: same sentence in both languages.
- [ ] **Step 2: Manual check with the tmux harness** (memory `tui-harness-tmux`) — glyph column unchanged, no stray bytes; and in a real kitty: big icons, open/closed colours, F9 menu over the column hides them, resize, `ctrl+z`/`fg`, quit leaves nothing.
- [ ] **Step 3:** `just ci-fast` (the one mid-plan run), `just gui-ci`.
- [ ] **Step 4: Commit** `docs: ADR 0169, changelog and help for big rail icons`; then `just link` and `just link-gui`.
