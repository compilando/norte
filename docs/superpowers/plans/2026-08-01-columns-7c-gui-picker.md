# Columns block 7c — GUI column picker panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The GUI gets the column picker as an overlay panel over the shared `ColumnsPicker` model — toggle/reorder/sort/format-cycle, Enter applies in-session and persists, closing the last #108 block.

**Architecture:** Mirror the GUI's established overlay precedent (`palette_view.rs`): a new `columns_view.rs` owns a thin pure wrapper (`on_key` → `ColumnsOutcome`), `main.rs` owns `render_columns_picker` + the async persist via the `cx.background_spawn` idiom (`commit_settings_write` precedent — no tokio on the render thread). `"pane.columns"` joins the GUI `COMMANDS`; alt+c arrives free from the shared presets. Apply order copies the TUI: session first (`apply_picked` + `apply_format` + re-seed pane sorts), disk second (one background task: `persist_columns` + N× `persist_column_format`). Applying the picker CLEARS `sort_override` for panes whose scheme the save targets — the persisted sort supersedes the session click. The scrim does not occlude in GPUI, so `on_sort_click` gains the picker guard.

**Tech Stack:** GPUI (raw `on_key_down`, no actions! macros), `norte_frontend::columns_picker`, `norte_config::{persist_columns, persist_column_format, PersistSort}`, Fluent.

**Spec:** `docs/superpowers/specs/2026-07-24-columns-design.md` Layer 6. Issue #108. Block-6 deferrals: sort persistence lands here (picker persists; header click stays session-only, same as TUI); the header **context menu** is NOT built — the panel is the operation surface for toggle/cycle-format; recorded as a further deferral in the close-out comment.

**Scope decisions (recorded):**
- Key handling is raw (GUI overlay precedent: palette/settings/extensions match keystrokes directly) — the TUI's `dialog.*` keymap verbs are not resolved in the GUI. Keys mirror the TUI defaults: up/down, space/e toggle, shift+up/shift+down (and K/J) reorder, s sort (ctrl+s also accepted), f format, Enter apply, Esc discard.
- Opaque ids (`attr:`/`plugin:`/junk) render masked, preserved verbatim — same #73 contract as the TUI picker.
- `norte-gui` is outside the workspace: gates are `just gui-ci` + `just check-gui`; the i18n additions touch the workspace → `just ci-fast` at close.

---

### Task 1: `columns_view.rs` — pure wrapper + key mapping + tests

**Files:**
- Create: `crates/norte-gui/src/columns_view.rs`
- Modify: `crates/norte-gui/src/main.rs` (mod declaration next to `mod palette_view;`)

- [ ] **Step 1: Write the pure view** (mirror `palette_view.rs`'s header-doc style):

```rust
//! Estado PURO del picker de columnas del GUI (#108 bloque 7c): envuelve el
//! `ColumnsPicker` compartido de norte-frontend (misma máquina que la TUI,
//! regla 7) y mapea teclas → verbos. Sin GPUI: testeable a secas. El render
//! y la persistencia viven en main.rs (precedente palette_view/settings_view).

use norte_frontend::columns_picker::{ColumnsPicker, Picked};

/// Resultado de una tecla sobre el panel.
#[derive(Debug, PartialEq)]
pub enum ColumnsOutcome {
    /// Consumida (o ignorada): el panel sigue abierto.
    None,
    /// Cerrar SIN aplicar (Esc — descarta, como la TUI).
    Close,
    /// Enter: aplicar en sesión y persistir.
    Apply(Picked),
}

/// El panel: el modelo compartido más nada (el cursor/las filas viven en él).
#[derive(Debug, Clone)]
pub struct ColumnsView {
    pub picker: ColumnsPicker,
}

impl ColumnsView {
    pub fn new(picker: ColumnsPicker) -> Self {
        Self { picker }
    }

    /// Teclas del panel — mismas que los defaults `dialog.*` de la TUI:
    /// up/down mueven cursor; space/e toggle; shift+up/down y K/J reordenan;
    /// s (o ctrl+s) ordena por la columna del cursor; f cicla formato;
    /// enter aplica; escape descarta. Todo lo demás se consume sin efecto
    /// (un overlay modal no deja pasar teclas al pane de abajo).
    pub fn on_key(&mut self, key: &str, shift: bool, ctrl: bool) -> ColumnsOutcome {
        match (key, shift, ctrl) {
            ("escape", _, _) => ColumnsOutcome::Close,
            ("enter", _, _) => ColumnsOutcome::Apply(self.picker.finish()),
            ("up", true, _) => self.picker.move_up(),
            ("down", true, _) => self.picker.move_down(),
            ("up", false, _) => self.picker.up(),
            ("down", false, _) => self.picker.down(),
            ("space" | "e", _, _) => self.picker.toggle(),
            // K/J llegan como "k"/"j" + shift en GPUI.
            ("k", true, _) => self.picker.move_up(),
            ("j", true, _) => self.picker.move_down(),
            ("s", _, _) => self.picker.sort_current(),
            ("f", false, false) => self.picker.cycle_format(),
            _ => {}
        }
        ColumnsOutcome::None
    }
}
```

Note the early-return arms (`escape`/`enter`) vs unit arms — the match returns `()` for movement arms and the function ends with `ColumnsOutcome::None`; write it as two matches or a `return` in the first two arms (compiler drives; keep it simple and clippy-clean).

- [ ] **Step 2: Unit tests in the same file** (pure — no GPUI):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::columns::ColumnsSettings;
    use norte_frontend::sort::SortSpec;

    fn view() -> ColumnsView {
        let settings = ColumnsSettings::default();
        ColumnsView::new(ColumnsPicker::open(&settings, "file", SortSpec::default()))
    }

    #[test]
    fn esc_descarta_enter_aplica() {
        let mut v = view();
        assert_eq!(v.on_key("escape", false, false), ColumnsOutcome::Close);
        let mut v = view();
        let ColumnsOutcome::Apply(picked) = v.on_key("enter", false, false) else {
            panic!("enter debe aplicar");
        };
        // El set default siempre contiene name primero (pineado).
        assert_eq!(picked.ids.first().map(String::as_str), Some("name"));
    }

    #[test]
    fn toggle_y_reorden_mueven_el_modelo() {
        let mut v = view();
        v.on_key("down", false, false); // cursor a la fila 1
        let id = v.picker.rows()[1].id.clone();
        let antes = v.picker.rows()[1].enabled;
        v.on_key("space", false, false);
        assert_eq!(v.picker.rows()[1].enabled, !antes, "toggle {id}");
        v.on_key("down", true, false); // shift+down reordena
        assert_eq!(v.picker.rows()[2].id, id, "reordenada hacia abajo");
    }

    #[test]
    fn sort_y_formato_delegan() {
        let mut v = view();
        v.on_key("down", false, false);
        v.on_key("s", false, false);
        let sorted = v.picker.sort();
        // after_click sobre la columna de la fila 1 (size en el default).
        assert_ne!(sorted, SortSpec::default());
        let antes = v.picker.format_of_cursor();
        v.on_key("f", false, false);
        assert_ne!(v.picker.format_of_cursor(), antes, "f cicla el formato");
    }

    #[test]
    fn teclas_ajenas_se_consumen_sin_efecto() {
        let mut v = view();
        let filas = v.picker.rows().to_vec().len();
        assert_eq!(v.on_key("x", false, false), ColumnsOutcome::None);
        assert_eq!(v.picker.rows().len(), filas);
    }
}
```

(Adjust constructor calls to the real signatures — `ColumnsSettings::default()`/`SortSpec::default()` exist per block 3/4; if `default()` is missing use `resolve` over a default `ColumnsConfig`.)

- [ ] **Step 3: Run** — `cd crates/norte-gui && cargo nextest run columns_view` → PASS.
- [ ] **Step 4: Commit** — `feat(gui): columns_view — pure picker panel state (#108 block 7c)`

### Task 2: command + wiring — open, key chain, guards

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs` (`COMMANDS` :16)
- Modify: `crates/norte-gui/src/main.rs` (state field, `run_command` :1256, `on_key` :1836, chord-suppression :4809, `on_sort_click` :2098)

- [ ] **Step 1:** Add `"pane.columns"` to `COMMANDS` (alphabetical/grouped placement as the list is organized). The existing keymap test (`keymap.rs:391-419`) pins that it resolves in all presets — run it: `cargo nextest run -p norte-gui keymap` (from `crates/norte-gui`). Expected: PASS (alt+c lives in the three shared presets already).
- [ ] **Step 2:** `main.rs` state + open:

```rust
/// Picker de columnas (#108 bloque 7c): overlay sobre el modelo compartido.
columns_picker: Option<columns_view::ColumnsView>,
```

```rust
fn open_columns_picker(&mut self) {
    let f = self.focus;
    let scheme = self.panes[f].dir().scheme().to_owned();
    // Sort VIVO del pane (ya pliega sort_override), como la TUI.
    let sort = self.panes[f].sort();
    self.columns_picker = Some(columns_view::ColumnsView::new(
        norte_frontend::columns_picker::ColumnsPicker::open(&self.column_settings, &scheme, sort),
    ));
}
```

`run_command` arm: `"pane.columns" => self.open_columns_picker(),`.

- [ ] **Step 3:** Key chain in `on_key` — insert the picker branch after `extensions`, before `palette` (a picker open eats everything):

```rust
if self.columns_picker.is_some() {
    self.on_columns_key(&ks, cx);
    cx.notify();
    return;
}
```

with:

```rust
fn on_columns_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
    // Mismo gate de modificadores que la palette: platform/alt fuera
    // (ctrl pasa: ctrl+s es sinónimo de sort, precedente TUI).
    if ks.modifiers.platform || ks.modifiers.alt {
        return;
    }
    let Some(view) = self.columns_picker.as_mut() else {
        return;
    };
    match view.on_key(ks.key.as_str(), ks.modifiers.shift, ks.modifiers.control) {
        columns_view::ColumnsOutcome::None => {}
        columns_view::ColumnsOutcome::Close => self.columns_picker = None,
        columns_view::ColumnsOutcome::Apply(picked) => {
            self.columns_picker = None;
            self.apply_picked_columns(picked, cx);
        }
    }
}
```

- [ ] **Step 4:** Guards — `on_sort_click` (`main.rs:2098`): extend `if self.modal.is_some()` to `if self.modal.is_some() || self.columns_picker.is_some()` (scrim does not occlude, block-6 note). Pending-chord suppression list (`:4809-4815`): add `|| self.columns_picker.is_some()`.
- [ ] **Step 5:** `cargo check` (in norte-gui) — `apply_picked_columns` doesn't exist yet; stub it `fn apply_picked_columns(&mut self, _picked: …, _cx: …) {}` to keep the commit green, real body in Task 4. Run `cargo nextest run` in norte-gui → PASS.
- [ ] **Step 6: Commit** — `feat(gui): pane.columns opens the picker overlay — alt+c (#108 block 7c)`

### Task 3: render + i18n

**Files:**
- Modify: `crates/norte-gui/src/main.rs` (render fn + overlay mount at :4834 area)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `es.ftl` (hint key)

- [ ] **Step 1:** Fluent keys (workspace crates — en + es):

```ftl
columns-picker-hint-gui = Space toggle · Shift+↑/↓ move · S sort · F format · Enter apply · Esc close
```

(es: `columns-picker-hint-gui = Espacio activa · Shift+↑/↓ mueve · S ordena · F formato · Enter aplica · Esc cierra`.) Title reuses the existing `columns-picker-title`/`columns-picker-target-default`.

- [ ] **Step 2:** `render_columns_picker(&self, view: &columns_view::ColumnsView, chrome: &ChromeColors) -> impl IntoElement` — mirror `render_palette` (`main.rs:3051`): `.w(px(560.0))`, header = `ta("columns-picker-title", target)` where target = scheme if `scheme_override()` else `t("columns-picker-target-default")`; list `.max_h(px(420.0))` `Role::List`, one row per `PickerRow`: `[x]`/`[ ]` checkbox glyph, label = Fluent `col-header-*` for builtins else the id **masked** (same helper the GUI uses for hostile text; if none is imported yet, `norte_frontend::columns::sanitize_header` or the frontend masking fn the pane names use — locate at implementation, do NOT print raw), sort arrow `▲/▼` when `sort_column(builtin) == view.picker.sort().column`, ` · <format>` suffix when `Some` (closed ASCII vocab, safe raw), dimmed when `format_locked`. Cursor row: `aria_selected(true)` + background highlight, `Role::ListItem`. Footer: `t("columns-picker-hint-gui")`.
- [ ] **Step 3:** Mount in `impl Render` next to the palette overlay (`main.rs:4834-4849` pattern): centered, `bg(rgba(0x000000aa))`, painted BEFORE the modal overlay (modal wins on top, same relative order as the TUI chain).
- [ ] **Step 4:** `cargo check` + visual smoke via `just gui-demo` if a display is available; otherwise the render compiles and the liveness smoke in gui tests covers panics.
- [ ] **Step 5: Commit** — `feat(gui,i18n): render the columns picker panel (#108 block 7c)`

### Task 4: apply + persist + sort_override interaction

**Files:**
- Modify: `crates/norte-gui/src/main.rs` (real `apply_picked_columns`)

- [ ] **Step 1:** Implement, copying the TUI order (session first, disk second) and the GUI persist idiom (`commit_settings_write`, `main.rs:1393-1443`):

```rust
/// Aplica el resultado del picker (#108 bloque 7c): sesión PRIMERO
/// (settings + sort de los panes), disco DESPUÉS (un solo background task:
/// persist_columns + persist_column_format), toast con el desenlace.
fn apply_picked_columns(
    &mut self,
    picked: norte_frontend::columns_picker::Picked,
    cx: &mut Context<Self>,
) {
    // Sesión: settings compartidos + formatos + re-seed del sort por pane.
    self.column_settings
        .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
    for (id, fmt) in &picked.formats {
        self.column_settings.apply_format(id, fmt);
    }
    for pane in 0..self.panes.len() {
        let scheme_matches = picked
            .scheme_target
            .as_deref()
            .is_none_or(|s| self.panes[pane].dir().scheme() == s);
        if scheme_matches {
            // El sort persistido SUPERSEDE el click de sesión (deuda b6).
            self.sort_override[pane] = None;
            let spec = self
                .column_settings
                .sort_for(self.panes[pane].dir().scheme());
            self.panes[pane].set_sort(spec);
        }
    }
    // Disco, fuera del hilo de render (regla 2; precedente commit_settings_write).
    let Some(dir) = norte_config::user_config_dir() else {
        self.toast(norte_i18n::t("msg-settings-no-config-dir"), cx);
        return;
    };
    let scheme = picked.scheme_target.clone();
    let ids = picked.ids.clone();
    let sort = picked.sort;
    let formats = picked.formats.clone();
    cx.spawn(async move |this, cx| {
        let outcome = cx
            .background_spawn(async move {
                norte_config::persist_columns(
                    &dir,
                    scheme.as_deref(),
                    &ids,
                    norte_config::PersistSort {
                        column: match sort.column { /* mismo mapeo que la TUI */ },
                        descending: sort.dir == norte_frontend::sort::SortDir::Desc,
                        dirs_first: sort.dirs_first,
                    },
                )
                .and_then(|_| {
                    for (id, fmt) in &formats {
                        norte_config::persist_column_format(&dir, id, fmt)?;
                    }
                    Ok(std::path::PathBuf::new())
                })
            })
            .await;
        this.update(cx, |view, cx| match outcome {
            Ok(_) => view.toast(norte_i18n::t("msg-columns-saved"), cx),
            Err(e) => view.toast_settings_error(&e, cx), // misma ruta que settings
        })
    })
    .detach();
}
```

(`toast`/`toast_settings_error` are placeholders for whatever the GUI's real status/toast mechanism is — reuse the exact functions `apply_settings_write_result` uses; locate at implementation. Error text goes through the existing `io_error_category` masking, never raw OS Display.)

- [ ] **Step 2:** Run `cargo nextest run` + `cargo clippy --all-targets -- -D warnings` in norte-gui → PASS.
- [ ] **Step 3: Commit** — `feat(gui): picker applies in-session and persists columns+formats (#108 block 7c)`

### Task 5: gates + reviews + close #108

- [ ] **Step 1:** `just gui-ci` (nextest + clippy + fmt in norte-gui) → EXIT=0. `just check-gui` → EXIT=0.
- [ ] **Step 2:** Workspace side (i18n touched): `just ci-fast` → EXIT=0.
- [ ] **Step 3:** Reviews: **rust-reviewer** (GUI diff vs hard rules — blocking IO off render thread, rule 7 nothing decisional in the GUI) and **encoding-auditor** (opaque ids masked in the panel, no raw hostile text in GPUI labels). Apply findings, targeted re-test, `fix(gui)` commit.
- [ ] **Step 4:** Close-out comment on #108: block 7c done; remaining deferrals restated (header context menu, width cycling, per-scheme spec persistence, #113, attr:-column rendering pending block 2's data now being available). Update memory. Push.
