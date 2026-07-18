# Navegación TC (quick search, historial, hotlist) — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** quick search `/` (filtro incremental default, salto por config), historial de dirs por pane `Alt+↓`, hotlist persistida `Ctrl+D` — frontend puro, cero protocolo.

**Architecture:** lógica pura en `norte-tui/src/nav.rs` (testeable sin terminal); estado en `Pane` (quick search) y `App` (popups, patrón ThemePicker/PickerAction existente); persistencia hotlist en `norte.toml` de usuario vía `toml_edit` (patrón `persist_ui_theme_to`). Spec: `docs/superpowers/specs/2026-07-18-navegacion-tc-design.md`.

**Tech Stack:** unicode-normalization (ya dep), toml_edit (ya dep), patrón popup ThemePicker, Fluent en/es con test de paridad.

**Convenciones (cada task):** TDD rojo→verde; clippy `-p norte-tui -- -D warnings` + `cargo fmt --all`; commit convencional; español en comentarios/tests; sin unwrap fuera de tests; strings UI por Fluent. Realidades del código actual (verificadas): `Pane { dir, entries, cursor, loading }` (app.rs:13) con `extend_listing`/`finish_listing`; `ThemePicker` + `PickerAction` + `theme_picker_input` (app.rs:302+); `COMMANDS` (keymap.rs:517) y `help_id` con test de paridad que OBLIGA a `help-cmd-*` en en/es; presets en `crates/norte-tui/src/keymap_presets/{orthodox,vim,cua}.toml`; `'/'` NO está bindeada en ningún preset (verificado con grep); `NorteToml` con `deny_unknown_fields` (config.rs); deuda #75 (capas posicionales) — la capa proyecto es la ÚLTIMA de `Layers.dirs`.

---

### Task 1: `nav.rs` — QuickSearch puro

**Files:**
- Create: `crates/norte-tui/src/nav.rs` (+ `pub mod nav;` en lib.rs, orden alfabético)
- Test: mod `#[cfg(test)]` en el fichero

- [ ] **Step 1: tests rojos**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Entry, EntryKind, VPath};

    fn e(wire: &str) -> Entry {
        Entry {
            path: VPath::parse(wire).expect("wire"),
            kind: EntryKind::File,
            size: None,
            mtime: None, // ajustar a los campos REALES de Entry (mirar proto)
        }
    }

    #[test]
    fn filtro_substring_case_insensitive() {
        let entries = vec![e("mem:///Proyectos"), e("mem:///readme.md"), e("mem:///PROBE")];
        let m = matches(b"pro", &entries);
        assert_eq!(m, vec![0, 2], "Proyectos y PROBE casan; readme no");
    }

    #[test]
    fn filtro_nfc_casa_con_nfd() {
        // "año" en NFC como aguja; entrada con nombre en NFD (a + n + ̃ + o).
        let nfd = "an\u{0303}o.txt";
        let entries = vec![e(&format!("mem:///{nfd}"))];
        assert_eq!(matches("año".as_bytes(), &entries), vec![0], "NFD casa con aguja NFC");
    }

    #[test]
    fn bytes_no_utf8_no_rompen_y_no_casan_en_falso() {
        let entries = vec![e("mem:///%FF%FE"), e("mem:///normal.txt")];
        assert_eq!(matches(b"norm", &entries), vec![1]);
        // La entrada hostil sigue filtrable por lo que su lossy muestra (�):
        let _ = matches("\u{FFFD}".as_bytes(), &entries); // no panica
    }

    #[test]
    fn estado_filtro_navega_y_confirma() {
        let entries = vec![e("mem:///a1"), e("mem:///b"), e("mem:///a2")];
        let mut q = QuickSearch::new(Mode::Filter);
        q.push_char('a', &entries);
        assert_eq!(q.visible(), &[0, 2]);
        q.down();
        assert_eq!(q.selected_entry_index(), Some(2), "segundo match");
        q.backspace(&entries);
        assert_eq!(q.visible(), &[0, 1, 2], "query vacía = todo visible");
    }

    #[test]
    fn modo_salto_tab_con_wrap() {
        let entries = vec![e("mem:///ab"), e("mem:///zz"), e("mem:///ac")];
        let mut q = QuickSearch::new(Mode::Jump);
        q.push_char('a', &entries);
        assert_eq!(q.selected_entry_index(), Some(0));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(2));
        q.next_match();
        assert_eq!(q.selected_entry_index(), Some(0), "wrap");
    }

    #[test]
    fn reaplicar_tras_lote_nuevo_conserva_seleccion_si_sobrevive() {
        let mut entries = vec![e("mem:///a1")];
        let mut q = QuickSearch::new(Mode::Filter);
        q.push_char('a', &entries);
        entries.push(e("mem:///a2")); // llega un lote del fill
        q.refresh(&entries);
        assert_eq!(q.visible(), &[0, 1]);
        assert_eq!(q.selected_entry_index(), Some(0), "la selección no salta");
    }
}
```

- [ ] **Step 2: rojo** — `cargo nextest run -p norte-tui nav::` → FAIL compilación.

- [ ] **Step 3: implementación**

```rust
//! Navegación TC (spec 2026-07-18): lógica PURA del quick search — sin
//! terminal, sin App. El match es UX de tipeo sobre el nombre lossy
//! normalizado a NFC y case-plegado (trampa macOS NFD, CLAUDE.md); la
//! IDENTIDAD de las entradas sigue siendo el VPath en bytes — operar usa
//! siempre `entries[índice_real]`.

use norte_proto::Entry;
use unicode_normalization::UnicodeNormalization;

/// Modo del quick search (`[ui] quick_search`, default filtro).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// El listado se reduce a los matches.
    #[default]
    Filter,
    /// El cursor salta entre matches; el listado no cambia.
    Jump,
}

/// Nombre → clave de comparación: lossy del último segmento, NFC, lowercase.
fn fold(name: &[u8]) -> String {
    String::from_utf8_lossy(name).nfc().flat_map(char::to_lowercase).collect()
}

/// Índices de `entries` cuyo nombre contiene `query` (misma normalización
/// en ambos lados). Query en bytes (viene del input tal cual).
#[must_use]
pub fn matches(query: &[u8], entries: &[Entry]) -> Vec<usize> {
    let q = fold(query);
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let name = e.path.file_name().map_or(&b""[..], |s| s.as_bytes());
            fold(name).contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}
```

(`file_name()`/`as_bytes()`: los MISMOS accessors que usa `lua/fs.rs:140` — verificar nombre exacto de la API de Segment.)

`QuickSearch`:
```rust
/// Estado vivo del quick search de UN pane.
#[derive(Debug)]
pub struct QuickSearch {
    query: Vec<u8>,
    mode: Mode,
    /// Índices REALES en `entries` que casan (query vacía = todos).
    visible: Vec<usize>,
    /// Posición de la selección DENTRO de `visible`.
    pos: usize,
}
```
con: `new(mode)`, `push_char(c: char, entries)` (append utf8 + recompute), `backspace(entries)`, `refresh(entries)` (recompute conservando la selección: recuerda el índice real seleccionado y re-búscalo en el nuevo `visible`; si murió, clamp), `down()/up()` (mueven `pos` con clamp), `next_match()` (Jump: `pos = (pos+1) % visible.len()`, noop si vacío), `visible() -> &[usize]`, `selected_entry_index() -> Option<usize>` (`visible.get(pos)`), `query_display() -> String` (lossy para pintar), `mode()`.

- [ ] **Step 4: verde** + clippy + fmt.
- [ ] **Step 5: Commit** — `feat(tui): nav::QuickSearch — filtro/salto puro NFC-insensitive (navTC T1)`

---

### Task 2: `nav.rs` — History + Hotlist + config

**Files:**
- Modify: `crates/norte-tui/src/nav.rs`, `crates/norte-tui/src/config.rs`
- Test: mods de test en ambos

- [ ] **Step 1: tests rojos** (en nav.rs y config.rs)

```rust
// nav.rs
#[test]
fn historial_push_dedup_tope_y_retirada() {
    let mut h = History::default();
    for i in 0..40 {
        h.push(vp(&format!("mem:///d{i}")));
    }
    assert_eq!(h.entries().len(), 30, "tope");
    assert_eq!(h.entries()[0], vp("mem:///d39"), "más reciente primero");
    h.push(vp("mem:///d39"));
    assert_eq!(h.entries().len(), 30, "dedup consecutivo");
    h.remove(&vp("mem:///d39"));
    assert!(!h.entries().contains(&vp("mem:///d39")), "retirada tras NotFound");
}

// config.rs (junto a toml_diag_tests)
#[test]
fn hotlist_round_trip_preservando_comentarios() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("norte.toml"),
        "# mi config\n[ui]\ntheme = \"nord\" # tema\n",
    )
    .unwrap();
    persist_hotlist_add(dir.path(), "trabajo", "file:///home/o/work").unwrap();
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
    assert!(s.contains("# mi config"), "comentarios intactos");
    assert!(s.contains("[[hotlist]]"));
    persist_hotlist_remove(dir.path(), "trabajo").unwrap();
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
    assert!(!s.contains("trabajo"));
}

#[test]
fn hotlist_se_carga_de_todas_las_capas_menos_proyecto() {
    // dos dirs: "usuario" con una entrada, "proyecto" (última) con otra:
    // la de proyecto NO entra (un repo ajeno no inyecta favoritos).
    // montar Layers{dirs:[user_dir, proj_dir]} + load() y assert.
}

#[test]
fn hotlist_entrada_invalida_degrada_por_entrada() {
    // path no parseable como VPath wire → la entrada se conserva como
    // inválida (name + error) sin tumbar la carga; las demás viven.
}
```

- [ ] **Step 2: rojo.**
- [ ] **Step 3: implementación**
  - nav.rs: `History { deque: VecDeque<VPath> }` — `push` (dedup frente, tope `HISTORY_MAX = 30`), `entries()`, `remove(&VPath)`.
  - config.rs: `NorteToml` gana `#[serde(default)] pub hotlist: Vec<HotlistEntry>` con `HotlistEntry { name: String, path: String }` (+schema derive como el resto); `LoadedConfig` gana `pub hotlist: Vec<HotlistItem>` con `HotlistItem { name: String, target: Result<VPath, String> }` (inválido = Err(categoría estable `err-invalid-path`) — se degrada por entrada). En `load`: acumula hotlist de TODAS las capas MENOS la última (proyecto; comentario referenciando la decisión de la spec y deuda #75). `[ui] quick_search: Option<String>` («filter»|«jump», otro valor = como filter con warning… NO: valor desconocido = `ConfigError::Toml` con diagnóstico claro — config rota es error, ADR 0007) → `LoadedConfig.quick_search_mode: nav::Mode`.
  - config.rs: `persist_hotlist_add(dir, name, wire_path)` / `persist_hotlist_remove(dir, name)` con toml_edit calcando `persist_ui_theme_to` (ArrayOfTables `hotlist`; add reemplaza si el name ya existe; remove por name).
- [ ] **Step 4: verde** (los dos crates de tests: `cargo nextest run -p norte-tui nav:: config::`) + clippy + fmt.
- [ ] **Step 5: Commit** — `feat(tui): historial por pane + hotlist en config con toml_edit (navTC T2)`

---

### Task 3: comandos, presets, Fluent

**Files:**
- Modify: `crates/norte-tui/src/keymap.rs` (COMMANDS), `crates/norte-tui/src/keymap_presets/{orthodox,vim,cua}.toml`, `crates/norte-i18n/i18n/{en,es}.ftl`

- [ ] **Step 1:** añade a `COMMANDS`: `"pane.quick-search"`, `"pane.history"`, `"pane.hotlist"`. (`hotlist.add` NO va en COMMANDS: solo existe dentro del popup, tecla fija como los demás overlays — #24.) El test de ayuda existente OBLIGARÁ a las claves `help-cmd-pane-quick-search`, `help-cmd-pane-history`, `help-cmd-pane-hotlist` en ambos locales — ese es el rojo.
- [ ] **Step 2:** bindings en los TRES presets (sección pane): `/` → `pane.quick-search`, `alt+down` → `pane.history`, `ctrl+d` → `ctrl+d` ¡OJO! comprueba colisiones por preset (grep en cada toml: vim usa `ctrl+d` a veces como half-page-down — si colisiona en vim, usa `ctrl+d` igualmente SOLO si está libre; si no, `b` estilo hotlist de vim… decisión: si colisiona, bindea `ctrl+b` en ese preset y anótalo). `/` en vim: es search nativo de vim — coherente.
- [ ] **Step 3:** claves Fluent (en + es, paridad):
```
help-cmd-pane-quick-search = quick search in pane (filter/jump)
help-cmd-pane-history = directory history
help-cmd-pane-hotlist = favorite directories
quicksearch-partial = (partial)
history-title = History
history-empty = no history yet
hotlist-title = Favorites
hotlist-keys = [enter] go   [a] add current   [d] delete   [esc] close
hotlist-empty = empty — 'a' adds the current directory
hotlist-name-prompt = name:
hotlist-invalid = invalid path
msg-hotlist-saved = favorite saved: { $name }
msg-hotlist-removed = favorite removed: { $name }
msg-hotlist-persist-failed = favorites not saved: { $error }
```
(es equivalentes; `$name` por `detail_for_bar` al interpolar en main.rs; `$error` = `io_error_category`.)
- [ ] **Step 4:** verde (`cargo nextest run -p norte-tui -p norte-i18n`) + clippy + fmt.
- [ ] **Step 5: Commit** — `feat(tui,i18n): comandos y bindings de navegación TC (navTC T3)`

---

### Task 4: wiring quick search

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (Pane gana `pub quick: Option<nav::QuickSearch>` + métodos), `crates/norte-tui/src/main.rs` (dispatch + captura de teclas), `crates/norte-tui/src/ui.rs` (render), `crates/norte-tui/tests/snapshots_ui.rs`

- [ ] **Step 1: tests rojos** (en app.rs, lógica sin terminal):

```rust
#[test]
fn quick_filter_redirige_seleccion_y_ops() {
    let mut p = pane_con(vec!["a1", "b", "a2"]); // helper local con Entry mínimos
    p.quick_start(nav::Mode::Filter);
    p.quick_char('a');
    assert_eq!(p.selected().unwrap().path, vp("mem:///a1"), "selected respeta el filtro");
    p.quick_down();
    assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
    p.quick_cancel();
    assert_eq!(p.selected().unwrap().path, vp("mem:///a1"), "restaurado: cursor al último real");
}

#[test]
fn extend_listing_reaplica_el_filtro() {
    let mut p = pane_con(vec!["a1"]);
    p.quick_start(nav::Mode::Filter);
    p.quick_char('a');
    p.extend_listing(vec![entry("a2"), entry("zz")]);
    assert_eq!(p.quick_visible().unwrap().len(), 2, "a2 entra, zz no");
}
```

- [ ] **Step 2: rojo.**
- [ ] **Step 3: implementación**
  - `Pane`: campo `quick`, métodos `quick_start/quick_char/quick_backspace/quick_down/quick_up/quick_next/quick_cancel/quick_confirm` (confirm: fija `cursor` al índice real seleccionado y cierra), `quick_visible() -> Option<&[usize]>`; **`selected()` pasa a respetar el filtro** (si `quick` activo en modo Filter → `entries[quick.selected_entry_index()?]`); `extend_listing`/`finish_listing` llaman `quick.refresh`. En modo Jump, `quick_char` fija `cursor = selected_entry_index()` directamente (el listado no cambia).
  - main.rs: `Resolution::Run("pane.quick-search")` → `pane.quick_start(cfg_mode)`. Con quick activo, ANTES del resolver: chars imprimibles → `quick_char`; Backspace → `quick_backspace`; ↑↓ → `quick_up/down`; Tab (Jump) → `quick_next`; Esc → `quick_cancel`; Enter → `quick_confirm` y si lo seleccionado es dir, cd (mismo camino que nav.enter — reusar el brazo). El RESTO de teclas (F5, etc.) caen al resolver con el `selected()` ya filtrado — feed-to-listbox gratis (comentario).
  - ui.rs: con quick activo, `draw_pane` pinta solo `quick_visible()` (Filter) y una línea input `/{query_display}` + contador `n/m` + `t("quicksearch-partial")` si `loading`. Query display con mask (usa `crate::app::display_name` sobre los bytes de query… la query la tecleó el usuario: pinta lossy simple, sin mask — comenta el porqué).
  - Config: `quick_search_mode` fluye de LoadedConfig a donde main.rs lo necesita (variable del run loop, como cli_preset).
- [ ] **Step 4:** snapshot nuevo `snapshot_quick_search_filtro` (pane con filtro activo). Flujo insta del repo.
- [ ] **Step 5:** verde suite entera + clippy + fmt.
- [ ] **Step 6: Commit** — `feat(tui): quick search en pane — filtro y salto (navTC T4)`

---

### Task 5: wiring historial + hotlist

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (popups), `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/ui.rs`, `crates/norte-tui/tests/snapshots_ui.rs`

- [ ] **Step 1: tests rojos** (app.rs): popup navega con PickerAction (calca los tests de ThemePicker si existen; si no, mínimos: up/down/confirm/cancel + `a` en hotlist abre input de nombre + `d` marca borrado).
- [ ] **Step 2: rojo.**
- [ ] **Step 3: implementación**
  - app.rs: `pub struct NavPopup { pub kind: NavPopupKind, items: Vec<(String, Option<VPath>)>, cursor: usize, pub name_input: Option<String> }` con `NavPopupKind { History, Hotlist }`; navegación con `PickerAction` (reusa el enum); `App.nav_popup: Option<NavPopup>`. Items: display ya saneado al construir (paths por `path_display`, badge hostil; hotlist inválida = `(name + t("hotlist-invalid"), None)`).
  - main.rs: `pane.history` → popup con `History` del pane activo (los History viven como `[History; 2]` en el run loop o campos de App — elige App, junto a los panes… `Pane` NO (History no es render); campo `App.history: [nav::History; 2]`). Push al historial: en el punto donde un cd REEMPLAZA el pane (brazos `Cd::Filling`/`Cd::Replaced` — empuja el dir ANTERIOR). `pane.hotlist` → popup desde `LoadedConfig.hotlist` (clonada al App en arranque/reload). Teclas del popup (fijas, patrón overlay): ↑↓/Enter/Esc; en Hotlist además `a` (abre `name_input`; Enter confirma → `persist_hotlist_add` en spawn_blocking + refresca la copia en App + barra `msg-hotlist-saved`; input vacío = cancela) y `d` (`persist_hotlist_remove` + barra). Enter en item con `Some(path)` → cd (mismo camino que hotlist/historial usan el flujo de cd existente; NotFound en historial → `History::remove` + barra). `name_input` activo captura imprimibles/backspace ANTES que nada.
  - Persist fallido: barra `msg-hotlist-persist-failed` + la copia en App NO se toca (consistencia con disco).
  - ui.rs: `draw_nav_popup` calcando `draw_theme_picker` (título Fluent por kind, footer `hotlist-keys` en Hotlist, línea de input si `name_input`).
- [ ] **Step 4:** snapshots `snapshot_popup_historial` y `snapshot_popup_hotlist`.
- [ ] **Step 5:** verde suite + clippy + fmt.
- [ ] **Step 6: Commit** — `feat(tui): historial Alt+↓ y hotlist Ctrl+D persistida (navTC T5)`

---

### Task 6: cierre — reviewers + gate + push

- [ ] **Step 1:** reviewers sobre el rango completo del proyecto (el controller los orquesta): rust-reviewer (reglas duras, estado en App/Pane) + encoding-auditor (fold NFC del matcher, popups con paths hostiles, hotlist wire round-trip). Aplicar hallazgos con TDD.
- [ ] **Step 2:** actualizar la spec con desviaciones reales (p.ej. binding ctrl+d en vim si colisionó) + estado → IMPLEMENTADO.
- [ ] **Step 3:** `just ci` EXIT=0 (vía nohup + log si el runner mata jobs largos — precedente del cierre M4).
- [ ] **Step 4: Commit + push** — `test(tui): cierre navegación TC — reviewers + gate (navTC T6)` y push de todo el rango.

---

## Self-review del plan (hecho)

- Cobertura spec: A1 quick search (T1+T4, ambos modos, parcial, feed-to-listbox), A2 historial (T2+T5, retirada NotFound), A3 hotlist (T2+T5, capas sin proyecto, degradación por entrada, toml_edit), comandos/presets/ayuda (T3), errores Fluent (T3/T5), tests 1-5 de la spec (T1/T2/T4/T5), reviewers (T6).
- Tipos consistentes: `nav::{Mode, QuickSearch, History, matches}`, `config::{HotlistEntry, HotlistItem, persist_hotlist_add/remove}`, `app::{NavPopup, NavPopupKind}`, métodos `quick_*` de Pane — únicos en todo el plan.
- Sin placeholders: los dos tests de config marcados con comentario-cuerpo (capas/degradación) llevan la receta exacta en el propio comentario; el implementador los materializa con el harness de Layers que ya usa `config.rs` en sus tests.
