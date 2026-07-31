# Columns block 6 — GUI render (header, cells, clickable sort) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The GUI pane renders the configured column set (name + size/mtime/kind cells) from the shared `norte_frontend::columns` model, with a header row whose builtin labels carry the ▲/▼ sort indicator and are clickable to change the sort.

**Architecture:** Same `layout()` as the TUI, in monospace cells: the GUI measures the mono font's advance once per frame, converts the pane's approximate inner width to cells, and runs the exact same `column_widths()` the TUI uses (hoisted into `norte-frontend` so both frontends paint the same set). Cells are fixed-px divs after the name (`flex_1` name absorbs the remainder — the GUI's elastic equivalent of the TUI's computed name width). A click on a sortable header applies `SortSpec::after_click` to the pane and records a per-pane session override that survives `cd`.

**Tech Stack:** Rust, GPUI (pinned rev `f14fea9`, `cx.text_system().advance()` for the mono cell width), `norte-frontend` shared model, Fluent keys `col-header-*` (already present).

**Spec:** `docs/superpowers/specs/2026-07-24-columns-design.md`, Layer 5 GUI + decomposition item 6 ("GUI render — header, cells, clickable sort"). Issue #108.

**Scope decisions (recorded):**
- The spec's Layer-5 GUI sentence also mentions a header **context menu** (toggle column, cycle format). That is the picker's operation surface — it ships with block 7 ("GUI panel over the same model"), not here. Block 6 is exactly what the decomposition names: header, cells, clickable sort.
- **Persistence of a clicked sort is deferred to block 7** (the picker owns "applies and persists through `persist_set`"). Here a click is a session override, per pane, honored across `cd`.
- The GUI cannot know a pane's laid-out width at `render_pane` time (flex decides it later). Approximation: `viewport/2 − fixed chrome`. `layout()` only decides *which* optional columns fit and their widths; the name absorbs any error via `flex_1`, so a few cells of error never misalign anything — worst case a column drops slightly early on a narrow window.
- Builtin cells go **after** the decoration badge and **before** the G3c plugin cells, mirroring the TUI order (name-block first, metadata after). Plugin headers (already sanitized at `columns_ready`) join the new header row over their fixed 96-px cells — that closes the currently unused `_header` in `render_row`.

---

### Task 1: `SortSpec::after_click` (shared click semantics)

**Files:**
- Modify: `crates/norte-frontend/src/sort.rs`

The GUI header needs it now; the block-7 pickers (TUI modal, GUI panel) reuse it. Semantics: clicking the active column flips the direction; clicking another column sorts by it ascending; `dirs_first` never changes on click.

- [ ] **Step 1: Write the failing tests** (in the existing `#[cfg(test)] mod tests` of `sort.rs`)

```rust
#[test]
fn after_click_misma_columna_invierte_la_direccion() {
    let s = SortSpec { column: SortColumn::Size, dir: SortDir::Asc, dirs_first: true };
    let t = s.after_click(SortColumn::Size);
    assert_eq!(t, SortSpec { column: SortColumn::Size, dir: SortDir::Desc, dirs_first: true });
    // Y el segundo click vuelve a Asc.
    assert_eq!(t.after_click(SortColumn::Size).dir, SortDir::Asc);
}

#[test]
fn after_click_columna_nueva_asc_y_dirs_first_intacto() {
    let s = SortSpec { column: SortColumn::Name, dir: SortDir::Desc, dirs_first: false };
    let t = s.after_click(SortColumn::Mtime);
    assert_eq!(t, SortSpec { column: SortColumn::Mtime, dir: SortDir::Asc, dirs_first: false });
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p norte-frontend after_click`
Expected: compile error, `after_click` not found.

- [ ] **Step 3: Implement**

In the `impl SortSpec` block (add one if there is none besides `Default`):

```rust
/// El resultado de un click en la cabecera de `col` (#108 b6): la columna
/// activa invierte su dirección; una columna nueva ordena por ella
/// ASCENDENTE. `dirs_first` jamás cambia por click — es una preferencia,
/// no un criterio de columna. Compartido: la cabecera de la GUI hoy, los
/// pickers de ambos frontends en el bloque 7.
#[must_use]
pub fn after_click(self, col: SortColumn) -> Self {
    if self.column == col {
        let dir = match self.dir {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        };
        Self { dir, ..self }
    } else {
        Self { column: col, dir: SortDir::Asc, ..self }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p norte-frontend after_click`
Expected: 2 passed.

---

### Task 2: hoist `column_widths` + `sort_column` into the shared model

**Files:**
- Modify: `crates/norte-frontend/src/columns.rs`
- Modify: `crates/norte-tui/src/ui.rs` (delete the local `column_widths`, reuse the shared one; replace the inline `activa` match with `sort_column`)

Both frontends must paint the same set from the same layout — that is the spec's Layer-5 sentence. The TUI already has the function (`ui.rs:281`); it moves, it does not fork.

- [ ] **Step 1: Write the failing test** (in `columns.rs` tests)

```rust
#[test]
fn column_widths_conjunto_default_a_80_celdas() {
    let s = ColumnsSettings::default();
    let w = column_widths(&s, "file", 80);
    let cols: Vec<Builtin> = w.iter().map(|(b, _)| *b).collect();
    assert_eq!(cols, vec![Builtin::Name, Builtin::Size, Builtin::Mtime]);
    // El nombre absorbe el resto: suma == disponible.
    assert_eq!(w.iter().map(|(_, x)| *x).sum::<u16>(), 80);
}

#[test]
fn column_widths_estrecho_solo_nombre() {
    let s = ColumnsSettings::default();
    let w = column_widths(&s, "file", 12);
    assert_eq!(w.iter().map(|(b, _)| *b).collect::<Vec<_>>(), vec![Builtin::Name]);
}

#[test]
fn sort_column_mapea_builtins_ordenables() {
    use crate::sort::SortColumn;
    assert_eq!(sort_column(Builtin::Name), Some(SortColumn::Name));
    assert_eq!(sort_column(Builtin::Size), Some(SortColumn::Size));
    assert_eq!(sort_column(Builtin::Mtime), Some(SortColumn::Mtime));
    assert_eq!(sort_column(Builtin::Kind), None);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p norte-frontend columns::`
Expected: compile error (`column_widths`/`sort_column` not found).

- [ ] **Step 3: Implement in `columns.rs`**

Move the TUI function verbatim (it is already pure) and add the mapping:

```rust
/// Anchos de las columnas (#108) para un ancho interior en CELDAS:
/// `(builtin, ancho)` de las columnas VIVAS de `settings` para `scheme`,
/// en orden de pintado — una columna sin sitio no aparece. Compartido
/// TUI/GUI: ambos frontends pintan el MISMO conjunto del mismo [`layout`].
pub fn column_widths(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
) -> Vec<(Builtin, u16)> {
    let set = settings.layout_items_for(scheme);
    let items: Vec<_> = set.iter().map(|(_, it)| *it).collect();
    let placed = layout(inner_width, &items);
    set.iter()
        .zip(placed)
        .filter_map(|((b, _), w)| w.map(|w| (*b, w)))
        .collect()
}

/// La columna de orden que corresponde a un builtin, si es ordenable.
/// `Kind` no lo es (no hay `SortColumn::Kind`): su cabecera no lleva
/// flecha ni es clicable.
pub fn sort_column(b: Builtin) -> Option<crate::sort::SortColumn> {
    use crate::sort::SortColumn;
    match b {
        Builtin::Name => Some(SortColumn::Name),
        Builtin::Size => Some(SortColumn::Size),
        Builtin::Mtime => Some(SortColumn::Mtime),
        Builtin::Kind => None,
    }
}
```

- [ ] **Step 4: Refactor the TUI to consume it**

In `crates/norte-tui/src/ui.rs`:
- Delete the local `fn column_widths` (lines ~279–293) and change its call site (`ui.rs:1658`) to `norte_frontend::columns::column_widths(settings, pane.dir().scheme(), inner_w)`.
- In `column_header_line`, replace the `activa` match:

```rust
let activa = norte_frontend::columns::sort_column(*col) == Some(sort.column);
```

- [ ] **Step 5: Run frontend + TUI tests (snapshots must NOT change)**

Run: `cargo nextest run -p norte-frontend && cargo nextest run -p norte-tui`
Expected: all pass, zero `.snap.new` files (`git status` clean of snapshots — this is a pure move).

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy -p norte-frontend -p norte-tui --all-targets -- -D warnings; echo EXIT=$?` (capture the real exit — never pipe to tail) and `cargo fmt --all`.

```bash
git add crates/norte-frontend/src/sort.rs crates/norte-frontend/src/columns.rs crates/norte-tui/src/ui.rs
git commit -m "feat(frontend,tui): shared column_widths + SortSpec::after_click (#108 block 6 prep)

The GUI paints the same column set as the TUI from the same layout(),
so the width computation moves into the shared model instead of forking.
after_click carries the header-click semantics both frontends' pickers
will reuse; Kind maps to no SortColumn on purpose (not sortable).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: GUI cell geometry — mono advance, inner cells, per-frame widths

**Files:**
- Modify: `crates/norte-gui/src/main.rs`

- [ ] **Step 1: Write the failing test** (in the `#[cfg(test)]` mod of `main.rs`)

```rust
/// #108 b6: celdas interiores aproximadas del pane — viewport/2 menos el
/// chrome fijo (bordes + padding) y el canalón de marca, a suelo 0.
#[test]
fn pane_inner_cells_aritmetica_y_suelos() {
    // 1280px de ventana, celda de 8.4px, canalón de 14px:
    // (640 − 8 − 14) / 8.4 = 73.5… → 73.
    assert_eq!(pane_inner_cells(1280.0, 8.4, 14.0), 73);
    // Ventana absurda de 10px: jamás pánico, 0 celdas.
    assert_eq!(pane_inner_cells(10.0, 8.4, 14.0), 0);
    // Celda no-positiva (advance imposible): 0, no división por cero.
    assert_eq!(pane_inner_cells(1280.0, 0.0, 14.0), 0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd crates/norte-gui && cargo nextest run pane_inner_cells`
Expected: compile error, `pane_inner_cells` not found.

- [ ] **Step 3: Implement the helper** (top-level fn near `FontSet`)

```rust
/// Chrome horizontal fijo de un pane (#108 b6): `border_2` a ambos lados
/// (2px × 2) + `px(sp::S)` de padding a ambos lados. El canalón de marca va
/// aparte (depende de `fonts.size`).
const PANE_CHROME_PX: f32 = 4.0 + 2.0 * sp::S;

/// Celdas mono que caben en el interior de un pane (#108 b6), aproximando
/// el ancho del pane como viewport/2 (los dos panes son `flex_1` iguales).
/// PURA a propósito (testeable sin `TextSystem`): el caller mide `ch` con
/// `cx.text_system().advance(…, '0')`. El error de aproximación lo absorbe
/// el nombre (`flex_1`) — `layout()` solo decide qué columnas CABEN.
fn pane_inner_cells(viewport_w: f32, ch: f32, gutter: f32) -> u16 {
    if ch <= 0.0 {
        return 0;
    }
    let inner = viewport_w / 2.0 - PANE_CHROME_PX - gutter;
    if inner <= 0.0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = (inner / ch).floor() as u16;
    cells
}
```

(If `sp::S` is not a `const f32` usable in const context, make `PANE_CHROME_PX` a `fn pane_chrome_px() -> f32` instead — same arithmetic; check `mod sp` at `main.rs:118` first.)

- [ ] **Step 4: Run to verify it passes**

Run: `cd crates/norte-gui && cargo nextest run pane_inner_cells`
Expected: PASS.

- [ ] **Step 5: Thread widths/now/ch through `render_pane`**

`render_pane` (main.rs:2087) gains `window: &Window` (the caller `render` at :4431 already has it — update the two call sites for pane 0 and 1). At the top of `render_pane`, after `let pane = …`:

```rust
// #108 b6: geometría de columnas del frame — el advance del mono ('0';
// en una monoespaciada todo glifo simple mide la celda), las celdas
// interiores aproximadas y el MISMO column_widths() que pinta la TUI.
let ts = cx.text_system();
let font_id = ts.resolve_font(&self.fonts.mono);
let ch: f32 = ts
    .advance(font_id, self.fonts.size, '0')
    .map_or(f32::from(self.fonts.size) * 0.6, |a| f32::from(a.width));
let cells = pane_inner_cells(
    f32::from(window.viewport_size().width),
    ch,
    f32::from(self.fonts.size),
);
let widths = norte_frontend::columns::column_widths(
    &self.column_settings,
    pane.dir().scheme(),
    cells,
);
let now_ms: i64 = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
```

Clone `widths` into the `cx.processor` closure (alongside `chrome_owned`) and pass `&widths_owned, now_ms, ch` to `render_row`. Keep a second copy outside the closure for the header row (Task 5).

- [ ] **Step 6: Compile**

Run: `cd crates/norte-gui && cargo check --locked`
Expected: clean (render_row does not use them yet — prefix the new params with `_` until Task 4, or land Tasks 3+4 together before checking; either is fine, but the commit comes after Task 5).

---

### Task 4: builtin cells in `render_row`

**Files:**
- Modify: `crates/norte-gui/src/main.rs` (`render_row`, :2362)

- [ ] **Step 1: Extend the signature**

```rust
fn render_row(
    &self,
    pane: usize,
    idx: usize,
    entry: &Entry,
    highlighted: bool,
    marked: bool,
    chrome: &ChromeColors,
    widths: &[(norte_frontend::columns::Builtin, u16)],
    now_ms: i64,
    ch: f32,
    window: &Window,
    cx: &mut Context<Self>,
) -> AnyElement {
```

(`#[allow(clippy::too_many_arguments)]` is already on the fn.)

- [ ] **Step 2: Paint the cells** — insert AFTER the decoration-badge `if let` (:2449–2456) and BEFORE the G3c plugin-cells `for` (:2465):

```rust
// #108 b6: celdas builtin tras el bloque del nombre (canalón+nombre+
// badge, que absorbe el resto vía flex_1) y ANTES de las celdas de
// plugin (G3c) — mismo orden que la TUI. Ancho FIJO en px = celdas de
// layout() × advance del mono; contenido a la DERECHA con ≥1 celda de
// separador (pl), presupuesto idéntico al de la TUI (el ancho INCLUYE
// el separador). Ausencia (`None` de builtin_cell — el size de un dir,
// un mtime desconocido) = celda en blanco, jamás un 0 fabricado. Color:
// el de la fila a alfa reducido — el mismo "dim relativo" que el
// fallback del badge de decoración (GPUI no tiene Modifier::DIM).
for (col, w) in widths.iter().filter(|(b, _)| !matches!(b, norte_frontend::columns::Builtin::Name)) {
    let cell = norte_frontend::columns::builtin_cell(entry, *col, now_ms).unwrap_or_default();
    row = row.child(
        div()
            .flex_none()
            .w(px(f32::from(*w) * ch))
            .pl(px(ch))
            .flex()
            .flex_row()
            .justify_end()
            .overflow_hidden()
            .text_color(gpui::Rgba { a: 0.55, ..color })
            .child(div().truncate().child(SharedString::from(cell))),
    );
}
```

Check the exact signature of `builtin_cell` (`norte-frontend/src/columns.rs:1004`) before wiring: `builtin_cell(entry: &norte_proto::Entry, col: Builtin, now_ms: i64) -> Option<String>`.

Note: when `highlighted && chrome.sel_fg.is_some()`, the row override (:2489) sets the row's text color AFTER these children are built — child `text_color` wins in GPUI (nearest ancestor refinement), so cells stay dim over `sel_bg`. That matches the TUI (cells are DIM inside the selection too). No extra code needed; just verify visually in the smoke.

- [ ] **Step 3: Compile + existing GUI tests**

Run: `cd crates/norte-gui && cargo nextest run`
Expected: all existing tests pass.

---

### Task 5: header row + clickable sort + session override

**Files:**
- Modify: `crates/norte-gui/src/main.rs`

- [ ] **Step 1: State — per-pane sort override**

Add to `struct NorteGui` (near `column_settings`, :124):

```rust
/// Orden elegido por click en la cabecera (#108 b6), POR PANE y de
/// SESIÓN: sobrevive al cd (gana a `column_settings.sort_for`) pero no
/// se persiste — persistir es del picker (bloque 7, `persist_set`).
sort_override: [Option<norte_frontend::SortSpec>; 2],
```

Initialize `sort_override: [None, None]` in BOTH constructor literals (:500 and :570 — the two `NorteGui { … }` blocks).

In `cd()` (:629, the `let spec = self.column_settings.sort_for(dir.scheme());` at :638):

```rust
let spec = self.sort_override[pane].unwrap_or_else(|| self.column_settings.sort_for(dir.scheme()));
```

(The startup seeding at :547 stays as-is — overrides are `None` there by construction.)

- [ ] **Step 2: Click handler**

New method next to `on_pane_scroll` (:2056):

```rust
/// Click en una cabecera ordenable (#108 b6): aplica `after_click` al
/// orden ACTUAL del pane (no al de config — dos clicks seguidos deben
/// alternar), re-ordena in place (`set_sort` re-ancla cursor y quick) y
/// recuerda la elección para los próximos cd de este pane.
fn on_sort_click(&mut self, pane: usize, col: norte_frontend::SortColumn, cx: &mut Context<Self>) {
    let spec = self.panes[pane].sort().after_click(col);
    self.panes[pane].set_sort(spec);
    self.sort_override[pane] = Some(spec);
    self.focus = pane;
    cx.notify();
}
```

- [ ] **Step 3: Header row in `render_pane`** — insert right BEFORE `col = col.child(list);` (:2267), using the `widths` computed in Task 3:

```rust
// #108 b6: fila de cabeceras de columna sobre el listado — mono (misma
// geometría de celda que las filas), dim, con ▲/▼ en la columna del
// orden activo. Las cabeceras ordenables son botones (`Role::Button`,
// cursor pointer, hover); `Kind` y las columnas de plugin no (sin
// SortColumn). El canalón de marca se replica como hueco fijo para que
// la cabecera del nombre arranque donde arranca el nombre.
{
    let sort = pane.sort();
    let dim = gpui::Rgba { a: 0.55, ..chrome.fg };
    let arrow = if sort.dir == norte_frontend::SortDir::Asc { "▲" } else { "▼" };
    let mut header_row = div()
        .id(format!("col-header-{i}"))
        .flex_none()
        .h(self.fonts.row_h)
        .px(px(sp::S))
        .flex()
        .flex_row()
        .items_center()
        .font(self.fonts.mono.clone())
        .text_color(dim)
        .child(div().flex_none().w(self.fonts.size).child(SharedString::from("")));
    for (k, (col, w)) in widths.iter().enumerate() {
        let is_name = matches!(col, norte_frontend::columns::Builtin::Name);
        let label_key = match col {
            norte_frontend::columns::Builtin::Name => "col-header-name",
            norte_frontend::columns::Builtin::Size => "col-header-size",
            norte_frontend::columns::Builtin::Mtime => "col-header-mtime",
            norte_frontend::columns::Builtin::Kind => "col-header-kind",
        };
        let sortable = norte_frontend::columns::sort_column(*col);
        let active = sortable == Some(sort.column);
        let label = if active {
            format!("{}{arrow}", norte_i18n::t(label_key))
        } else {
            norte_i18n::t(label_key)
        };
        let mut cell = div()
            .id(format!("col-header-{i}-{k}"))
            .overflow_hidden()
            .child(div().truncate().child(SharedString::from(label)));
        cell = if is_name {
            cell.flex_1()
        } else {
            cell.flex_none()
                .w(px(f32::from(*w) * ch))
                .pl(px(ch))
                .flex()
                .flex_row()
                .justify_end()
        };
        if let Some(sc) = sortable {
            cell = cell
                .role(gpui::Role::Button)
                .aria_label(norte_i18n::t(label_key))
                .cursor_pointer()
                .hover(|s| s.bg(chrome.hover_bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev: &MouseDownEvent, _w, cx| {
                        this.on_sort_click(i, sc, cx);
                    }),
                );
        }
        header_row = header_row.child(cell);
    }
    // Cabeceras de las columnas de plugin (G3c): misma geometría fija de
    // 96px que sus celdas, no ordenables. Ya saneadas en columns_ready.
    for (_id, header) in &self.columns[i] {
        header_row = header_row.child(
            div()
                .flex_none()
                .pl(px(sp::XS))
                .w(px(96.0))
                .truncate()
                .child(SharedString::from(header.clone())),
        );
    }
    col = col.child(header_row);
}
```

Adjust to the crate's actual imports (e.g. `SortDir`/`SortColumn` re-exports from `norte_frontend`; `MouseButton`/`MouseDownEvent` are already imported for the row handler). If `.hover()` on a non-`Stateful` div does not compile, the `.id(…)` already makes it stateful — keep the `.id` BEFORE `.hover`.

- [ ] **Step 4: Full GUI gate**

Run: `just gui-ci` (from repo root; it runs nextest + clippy `-D warnings` + fmt --check inside `crates/norte-gui`). Capture the real exit code (`just gui-ci; echo EXIT=$?`).
Expected: EXIT=0.

- [ ] **Step 5: Smoke run (real window)**

Run: `just run-gui` on a real directory. Verify: header row shows Name/Size/Modified over aligned cells; dir rows show a blank size cell; clicking «Size» sorts by size asc, again desc, arrow follows; clicking «Name» switches back; a `cd` keeps the clicked sort; narrow window drops mtime before size before name. Close with the quit flow.

- [ ] **Step 6: CHANGELOG + commit**

Add a CHANGELOG entry under the unreleased section mirroring the block-5 one, GUI-flavored.

```bash
git add crates/norte-gui CHANGELOG.md
git commit -m "feat(gui): default columns in the pane — cells, header, click-to-sort (#108 block 6)

Same layout() as the TUI in monospace cells: the mono advance is
measured per frame, viewport/2 approximates the pane's inner width
(flex_1 on the name absorbs the error — layout() only decides which
columns fit), and cells are fixed-px right-aligned divs whose width
includes the separator, dim via the row color at reduced alpha (the
decoration-badge precedent). The header row carries the Fluent labels,
the ▲/▼ of the active SortSpec, and the plugin-column headers (G3c)
over their 96px cells; sortable headers are buttons that apply
SortSpec::after_click and record a per-pane session override that
survives cd. Persistence and the context menu belong to the picker
(block 7).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: reviews and close

- [ ] **Step 1: rust-reviewer** over `git diff <base>..HEAD` (both commits). Apply findings; commit as `fix(gui): apply the #108 block-6 review findings`.
- [ ] **Step 2: encoding-auditor** over the GUI header/cells (surface is small: builtin formatter output is trusted; plugin headers/cells were sanitized at ingest — the auditor confirms no NEW third-party text reaches paint unmasked).
- [ ] **Step 3: Final gates**

Run: `just ci-fast; echo EXIT=$?` then `just gui-ci; echo EXIT=$?`.
Expected: both EXIT=0. (No proto/vfs/core logic touched → the full `just ci` cov gate is not required by the CI-cadence rule, but `ci-fast` covers fmt/clippy/tests/docs across the workspace.)

- [ ] **Step 4: Note deferrals on #108**: context menu + sort persistence + per-scheme override interaction → block 7; exact pane width (vs viewport/2 approximation) if it ever bites.
