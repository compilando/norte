# GUI-a navegación dual-pane — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** dual-pane navegable read-only en `norte-gui` (foco, cursor, cd/enter/parent, quick search por tipeo, teclado+ratón), extrayendo la lógica de presentación compartida a un crate `norte-frontend` que la TUI también consume sin duplicar.

**Architecture:** crate lib nuevo `norte-frontend` (sin deps de UI) con `display_name`/`path_display`/`sort_entries`/`nav::QuickSearch`/`PaneState`; la TUI se refactoriza para consumirlo (`just ci` verde en cada paso); `norte-gui` (EXCLUIDO del workspace, GPUI) construye el dual-pane encima del scaffold del spike. Spec: `docs/superpowers/specs/2026-07-19-gui-a-navegacion-dual-pane-design.md`.

**Tech Stack:** norte-frontend (norte-proto + norte-encoding + unicode-normalization); norte-gui (GPUI rev f14fea9, norte-core RemoteBackend, norte-theme, theme_map); scaffold en crates/norte-gui.

**Convenciones (cada task):** TDD donde hay lógica pura; `just ci` verde tras cada task que toca el WORKSPACE (T1-T3 tocan norte-tui/norte-frontend, van al gate); norte-gui (T4) se verifica con `cargo build`/`cargo run` DENTRO de su dir (excluido, no en `just ci`) + verificación manual contra daemon. Español; commits convencionales `feat(frontend):`/`refactor(tui):`/`feat(gui):`.

**Datos verificados:**
- `norte-tui::app`: `sort_entries(&mut [Entry])` (usa privados `nfc_key`, `name_bytes`); `display_name(bytes) -> (String, bool)` (usa `must_mask` = `norte_encoding::is_terminal_hazard(c)`); `path_display(&VPath) -> (String, bool)` (prefijo `⟨scheme authority⟩/` + segmentos por display_name). Call-sites: app.rs, main.rs, ui.rs, viewer.rs.
- `norte-tui::nav`: `Mode{Filter,Jump}`, `matches(query, entries) -> Vec<usize>`, `QuickSearch` (se van); `History`, `Hotlist` (se QUEDAN — GUI-a no los usa).
- `norte-tui::ui::HOSTILE_BADGE` — el badge lo aplica el RENDER (se queda en la TUI); `display_name`/`path_display` solo devuelven el bool, no el badge.
- Deps de lo extraído: norte-proto, norte-encoding, unicode-normalization (todas workspace, permisivas — norte-frontend Apache/MIT).
- scaffold norte-gui: `backend_task.rs` (RemoteBackend::connect + list vía oneshot), `theme_map::to_gpui_rgba`, `main.rs` (ventana + un pane). GPUI: `gpui`+`gpui_platform`, trait `Render`, `div()`, `.text_color(Rgba)`, `cx.spawn`, `cx.new`, key/mouse handlers.

---

### Task 1: crate `norte-frontend` + mover saneado y sort

**Files:**
- Create: `crates/norte-frontend/Cargo.toml`, `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/src/display.rs`, `crates/norte-frontend/src/sort.rs`
- Modify: `Cargo.toml` (workspace members), `crates/norte-tui/Cargo.toml` (+ dep), `crates/norte-tui/src/app.rs` (borra lo movido, re-exporta), call-sites (main.rs, ui.rs, viewer.rs)

- [ ] **Step 1: scaffold del crate** — usa `/new-crate norte-frontend` (skill, licencia Apache-2.0 OR MIT, lints del workspace) o a mano copiando el patrón de `crates/norte-theme/Cargo.toml`. Miembro del workspace (en `[workspace] members`). Deps: `norte-proto.workspace = true`, `norte-encoding.workspace = true`, `unicode-normalization.workspace = true`. `#![forbid(unsafe_code)]` + `#![warn(missing_docs)]`.

- [ ] **Step 2: mover display + sort con sus tests** — a `display.rs`: `display_name`, `path_display`, y `fn must_mask(c) -> bool { norte_encoding::is_terminal_hazard(c) }` (privado). A `sort.rs`: `sort_entries` + los privados `nfc_key`, `name_bytes`. Copia TAMBIÉN los tests que los cubren de `app.rs` (`grep -n "fn .*display\|fn .*sort\|path_display_jamas\|path_display_calca" crates/norte-tui/src/app.rs` en el mod tests). `lib.rs`: `pub use display::{display_name, path_display}; pub use sort::sort_entries;` + rustdoc del crate («lógica de presentación PURA compartida por los frontends — sin deps de UI»).

- [ ] **Step 3: la TUI consume norte-frontend** — `crates/norte-tui/Cargo.toml`: `norte-frontend.workspace = true` (+ entrada en `[workspace.dependencies]` del raíz). En `app.rs`: BORRA las defs movidas; re-exporta para no tocar todos los call-sites: `pub use norte_frontend::{display_name, path_display, sort_entries};` (los call-sites que hacen `app::display_name` siguen compilando). Borra los tests movidos de app.rs. Verifica que main.rs/ui.rs/viewer.rs siguen resolviendo (usan `crate::app::display_name` etc. — el re-export los cubre).

- [ ] **Step 4: verde** — `cargo nextest run -p norte-frontend` (tests movidos) + `cargo nextest run -p norte-tui` (sin regresión) + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all`. Los tests de display/sort ahora viven UNA vez (en norte-frontend).
- [ ] **Step 5: Commit** — `feat(frontend): crate norte-frontend + saneado/sort compartidos (GUI-a T1)`

---

### Task 2: mover `QuickSearch` a `norte-frontend::nav`

**Files:**
- Create: `crates/norte-frontend/src/nav.rs`
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-tui/src/nav.rs` (se queda History/Hotlist, re-exporta QuickSearch)

- [ ] **Step 1: mover** — a `norte-frontend/src/nav.rs`: `Mode`, `matches`, `QuickSearch` (+ su impl completa: new/push_char/backspace/refresh/down/up/next_match/visible/selected_entry_index/query_display/mode/quick_confirm — TODO lo que tiene hoy) + los tests de QuickSearch/matches/fold del mod tests de norte-tui::nav (`grep -n "fn .*fold\|fn .*matches\|fn .*quick\|fn refresh_" crates/norte-tui/src/nav.rs`). `lib.rs`: `pub mod nav;` (o `pub use nav::{Mode, QuickSearch, matches};`).
- [ ] **Step 2: la TUI re-exporta** — `crates/norte-tui/src/nav.rs`: BORRA `Mode`/`matches`/`QuickSearch` + sus tests; añade `pub use norte_frontend::nav::{Mode, QuickSearch, matches};`. `History` y `Hotlist` se quedan (con sus tests). Verifica: los call-sites de QuickSearch en la TUI (app.rs Pane.quick, main.rs, keymap `lua:`?) siguen compilando por el re-export.
- [ ] **Step 3: verde** — `cargo nextest run -p norte-frontend nav` + `cargo nextest run -p norte-tui` + clippy workspace + fmt.
- [ ] **Step 4: Commit** — `refactor(frontend): QuickSearch a norte-frontend, History/Hotlist se quedan en la TUI (GUI-a T2)`

---

### Task 3: `PaneState` puro en `norte-frontend`

**Files:** Create `crates/norte-frontend/src/pane.rs`; Modify `lib.rs`

Decisión (YAGNI, anclada en la spec §2): `PaneState` es NUEVO para consumo de la GUI; el `Pane` de la TUI (con render/quick entrelazado, 2149 líneas de app.rs) NO se refactoriza en GUI-a — su lógica de cursor ya está testeada y funciona. Deuda anotada: unificar el `Pane` de la TUI sobre `PaneState` cuando toque (issue). La duplicación es la lógica de cursor (~clamps triviales), no negocio.

- [ ] **Step 1: tests rojos** (mod tests en pane.rs):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};
    fn e(w: &str, k: EntryKind) -> Entry {
        Entry { path: VPath::parse(w).unwrap(), kind: k, size: None, mtime_ms: None }
    }
    fn pane(names: &[&str]) -> PaneState {
        let es = names.iter().map(|n| e(&format!("mem:///{n}"), EntryKind::File)).collect();
        PaneState::new(VPath::parse("mem:///").unwrap(), es)
    }

    #[test]
    fn cursor_se_mueve_con_clamp() {
        let mut p = pane(&["a", "b", "c"]);
        assert_eq!(p.cursor(), 0);
        p.cursor_up(); // clamp en 0
        assert_eq!(p.cursor(), 0);
        p.cursor_down(); p.cursor_down();
        assert_eq!(p.cursor(), 2);
        p.cursor_down(); // clamp en len-1
        assert_eq!(p.cursor(), 2);
        p.home();
        assert_eq!(p.cursor(), 0);
        p.end();
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn selected_respeta_el_filtro_quick() {
        let mut p = pane(&["alfa", "beta", "alto"]);
        p.quick_start(norte_frontend::nav::Mode::Filter); // o super::nav si reexport
        p.quick_char('a');
        // "alfa" y "alto" casan; el selected es el primero filtrado.
        assert_eq!(p.selected().unwrap().path, VPath::parse("mem:///alfa").unwrap());
        p.quick_cancel();
        assert_eq!(p.selected().unwrap().path, VPath::parse("mem:///alfa").unwrap());
    }

    #[test]
    fn set_listing_resetea_cursor_y_cierra_quick() {
        let mut p = pane(&["a", "b"]);
        p.cursor_down();
        p.quick_start(norte_frontend::nav::Mode::Filter);
        p.set_listing(VPath::parse("mem:///otro").unwrap(), vec![e("mem:///otro/x", EntryKind::File)]);
        assert_eq!(p.cursor(), 0);
        assert!(p.quick_visible().is_none());
        assert_eq!(p.dir(), &VPath::parse("mem:///otro").unwrap());
    }
}
```
- [ ] **Step 2: rojo** — `cargo nextest run -p norte-frontend pane` → FAIL.
- [ ] **Step 3: implementación** (`pane.rs`): `PaneState { dir: VPath, entries: Vec<Entry>, cursor: usize, loading: bool, quick: Option<QuickSearch> }` con:
  - `new(dir, entries)`, `set_listing(dir, entries)` (reset cursor 0, `quick=None`, `loading=false`), `begin_loading(dir)` (dir nuevo, entries vacías, loading=true, quick=None).
  - `cursor_up` (saturating_sub 1), `cursor_down` (min len-1), `page_up(n)`/`page_down(n)`, `home` (0), `end` (len-1). Todos no-op si vacío.
  - `selected() -> Option<&Entry>`: si `quick` activo en modo Filter → `entries[quick.selected_entry_index()?]`; si no → `entries.get(cursor)`. (Calca la lógica del Pane de la TUI.)
  - `dir()`, `entries()`, `cursor()`, `loading()`.
  - quick: `quick_start(mode)` (con las entries actuales), `quick_char(c)`, `quick_backspace()`, `quick_up/down()`, `quick_confirm()` (fija cursor al seleccionado, cierra), `quick_cancel()`, `quick_visible() -> Option<&[usize]>`. En Filter, `selected()` respeta el filtro; en Jump, `quick_char` fija cursor. (Extrae la mecánica del Pane de la TUI — está en app.rs; cópiala, no la inventes.)
  - Contrato de refresh tras un lote nuevo NO aplica aquí (GUI-a lista el dir de una, sin fill paginado — anota que el streaming es optimización posterior).
- [ ] **Step 4: verde** + clippy + fmt.
- [ ] **Step 5: abrir issue de deuda** — `gh issue create` «[frontend/tui] unificar el Pane de la TUI sobre norte_frontend::PaneState (hoy la lógica de cursor está duplicada)» + referénciala en un comentario de pane.rs.
- [ ] **Step 6: Commit** — `feat(frontend): PaneState puro (cursor + quick) para la GUI (GUI-a T3)`

---

### Task 4: norte-gui dual-pane navegable (GPUI)

**Files:** Modify `crates/norte-gui/src/main.rs`, `crates/norte-gui/src/backend_task.rs`, `crates/norte-gui/Cargo.toml` (+ norte-frontend por path)

Trabaja DENTRO de crates/norte-gui/ (excluido). NO hay tests de render (política spike); verificación manual + un test de la fn pura input→acción si sale.

- [ ] **Step 1: dep** — `crates/norte-gui/Cargo.toml`: `norte-frontend = { path = "../norte-frontend" }`.
- [ ] **Step 2: AppState + backend** — main.rs: estado `[PaneState; 2]` (norte_frontend::PaneState) + `focus: usize` + `Theme::preset_default()` cacheado + el `RemoteBackend` compartido. `backend_task.rs`: `list(backend, dir) -> Result<Vec<Entry>, Error>` reusable (ya existe conectar+list; generaliza para llamarlo en cada cd). El resultado cruza al hilo GPUI por oneshot + `cx.spawn`/`this.update`+`cx.notify` (patrón del spike, T4 del spike lo tiene).
- [ ] **Step 3: render dual-pane** — dos columnas horizontales (los dos panes); cada una lista vertical de `sort_entries`(entries) con `display_name` + color por tipo (`theme_map`, T5 del spike) + indicador de tipo; el pane con `focus` resaltado (fondo/borde distinto); la entrada bajo el cursor resaltada; en el pane activo con quick filtrando, pinta SOLO `quick_visible()` + una línea `/{query}` al pie. Nombres hostiles: `display_name` ya sanea + si el bool hostil, prefija un badge (define un `HOSTILE_BADGE` local en norte-gui o reusa el criterio — no hay ui.rs de la TUI aquí).
- [ ] **Step 4: input** — key handlers GPUI (descubre la API del rev: `on_key_down`/`KeyDownEvent`): Tab→conmuta focus; ↑↓/Home/End/PgUp/PgDn→cursor del pane con foco; carácter imprimible→quick_char (abre quick si cerrado); Backspace→si quick activo quick_backspace, si no cd al padre (`dir.parent()` — mira la API de VPath para el padre; si es raíz, no-op); Esc→quick_cancel; Enter→si quick activo quick_confirm; si el `selected()` es dir → cd (begin_loading + list async); si file → no-op. Mouse: click en una fila→focus a ese pane + cursor a esa fila; doble-click en dir→cd; rueda→cursor/scroll. Extrae el mapeo a una fn pura `key_to_action(key, mods, quick_active) -> Action` testeable si es limpio.
- [ ] **Step 5: verificar contra daemon** — arranca `norte daemon run` (background, nota el socket); `cargo run` dentro de crates/norte-gui/ con NORTE_SOCKET/NORTE_DIR. Verifica (sin ver la ventana pero sí): conecta+lista 2 panes; con NORTE_GUI_DEBUG loguea foco/cd/cursor para confirmar que Tab/Enter/Backspace/tipeo cambian el estado correcto; error de cd (dir inexistente vía NORTE_DIR malo) → categoría sin panic. Si tienes display, confírmalo visualmente.
- [ ] **Step 6: Commit** — `feat(gui): dual-pane navegable read-only con quick search y ratón (GUI-a T4)`

---

### Task 5: cierre — reviewers + gate + push

- [ ] **Step 1: reviewers** (el controller orquesta): rust-reviewer sobre el rango (norte-frontend nuevo + el refactor de la TUI + norte-gui) — foco en que la extracción no cambió comportamiento (re-exports correctos), reglas duras, PaneState limpio. encoding-auditor sobre display/sort/QuickSearch/PaneState movidos (tocan bytes de nombres — que el saneado/NFC no se degradó al mover). Aplicar hallazgos con TDD.
- [ ] **Step 2: gate** — `just ci` EXIT=0 (norte-frontend + norte-tui en el workspace; norte-gui excluido, verificado aparte con `cargo build -p norte-gui`). OJO links rustdoc a items privados (varios cierres tropezaron ahí — revisa `[`x`]` antes).
- [ ] **Step 3: cerrar spec** — estado → IMPLEMENTADO + desviaciones (p.ej. Pane de la TUI no unificado sobre PaneState, deuda anotada).
- [ ] **Step 4: Commit + push** — `test: cierre GUI-a — reviewers + gate (GUI-a T5)` + push del rango.

---

## Self-review del plan (hecho)

- Cobertura spec: crate norte-frontend + extracción incremental (T1-T3); display/path/sort compartidos (T1); QuickSearch compartido, History/Hotlist se quedan (T2); PaneState puro testeado una vez (T3); dual-pane navegable + quick + teclado + ratón + read-only + errores-sin-panic (T4); TUI verde reusando norte-frontend (T1-T3 + T5 gate); reviewers + gate (T5). Fuera de alcance (mutaciones, keymap config, viewer, i18n, AccessKit) no tocado.
- Tipos consistentes: `display_name`/`path_display`/`sort_entries`/`QuickSearch`/`Mode`/`matches`/`PaneState` — mismas firmas en norte-frontend (T1-T3) y consumidas en norte-gui (T4) y por la TUI vía re-export. `theme_map::to_gpui_rgba` (spike) reusado en T4.
- Riesgo del refactor de la TUI acotado: re-exports mantienen los call-sites; `just ci` verde exigido tras cada task del workspace; el Pane de la TUI NO se toca (deuda anotada) — cero riesgo de romper la TUI que ya funciona.
- norte-gui sin tests de render es política del spike (documentada); la lógica pura (PaneState, key_to_action) SÍ lleva test.
