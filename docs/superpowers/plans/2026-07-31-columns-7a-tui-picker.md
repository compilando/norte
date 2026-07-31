# Columns block 7a — TUI column picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A keyboard-driven column picker in the TUI (`alt+c` → overlay): toggle columns on/off, reorder them, pick the sort column/direction, Enter applies to the session AND persists to `norte.toml`, Esc discards.

**Architecture:** The pure picker model (`ColumnsPicker`) lives in `norte-frontend` (rule 7 — the GUI panel in 7c reuses it). The TUI wraps it as an `App` overlay following the `ThemePicker` precedent (NOT a `Modal` variant — the codebase's `Modal` enum is confirmation-shaped; `ThemePicker` is the list-with-cursor precedent, deviation from the spec's literal "Modal::Columns" recorded). Keys resolve through the keymap's `dialog` screen with a per-overlay ALLOWLIST (single source for dispatch and hints, #24 pattern). Persistence goes through a new nested-table writer in `norte-config` (`persist_set` cannot write `[ui.columns]`).

**Tech Stack:** Rust, ratatui, `toml_edit`, Fluent, existing `ColumnsSettings`/`SortSpec::after_click` from blocks 4/6.

**Spec:** `docs/superpowers/specs/2026-07-24-columns-design.md` Layer 6 + decomposition item 7. Issue #108.

**Scope decisions (recorded):**
- Block 7 splits: **7a** (this plan: TUI picker — toggle, reorder, sort, persist), **7b** (`[[ui.columns.spec]]` config + the picker's `w`/`f` width/format cycles — that config surface was deferred from block 4 and does not exist yet), **7c** (GUI panel over the same model + GUI keymap entry). Each lands under the 400-net-line convention.
- `dirs_first` is not editable in the picker v1 (config-only); the sort key reuses `SortSpec::after_click` — pressing sort on the row under the cursor behaves exactly like clicking that GUI header.
- Non-builtin configured ids (`attr:*`, `plugin:*`, and even unparseable junk) are **preserved**: they appear as inert-but-reorderable/toggleable rows and survive a save verbatim. The picker must never silently clean the user's config intent (doctor is who reports them).
- Save target: if the resolved config already has a scheme entry for the pane's scheme, the picker writes that scheme's `columns`+`sort`; otherwise it writes `[ui.columns] default`+`sort`. One rule, stated in the overlay title.
- Two latent gaps found in exploration get fixed here because the picker trips over them: (a) `reload_config` never refreshes `app.columns` (hot-reload of `[ui.columns]` is dead today); (b) `layout_items_for` only forces `name` first when it is MISSING — a config listing name mid-list would misrender the TUI (which budgets `widths.first()` as the name). The shared model will normalize name-to-front always.

---

### Task 1: `ColumnsPicker` pure model in norte-frontend

**Files:**
- Create: `crates/norte-frontend/src/columns_picker.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (register `pub mod columns_picker;` + re-exports if the crate re-exports siblings — follow how `columns`/`sort` are exposed)
- Modify: `crates/norte-frontend/src/columns.rs` (new `ColumnsSettings` accessors + name-first normalization + `apply_picked`)

- [ ] **Step 1: Write the failing tests** (new file's `#[cfg(test)] mod tests`)

```rust
use super::*;
use crate::columns::{Builtin, ColumnsSettings};
use crate::sort::{SortColumn, SortDir, SortSpec};

fn settings_vacios() -> ColumnsSettings {
    ColumnsSettings::resolve(&norte_config::ColumnsConfig::default())
}

#[test]
fn abre_con_el_set_efectivo_y_el_catalogo_restante() {
    let p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    // Default efectivo: name+size+mtime habilitadas; kind presente deshabilitada.
    let estados: Vec<(Option<Builtin>, bool)> =
        p.rows().iter().map(|r| (r.builtin, r.enabled)).collect();
    assert_eq!(
        estados,
        vec![
            (Some(Builtin::Name), true),
            (Some(Builtin::Size), true),
            (Some(Builtin::Mtime), true),
            (Some(Builtin::Kind), false),
        ]
    );
    assert!(!p.scheme_override(), "sin config no hay override de scheme");
}

#[test]
fn toggle_apaga_y_enciende_pero_name_es_inmutable() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.toggle(); // cursor 0 = name
    assert!(p.rows()[0].enabled, "name jamás se apaga");
    p.down();
    p.toggle();
    assert!(!p.rows()[1].enabled, "size se apaga");
    p.toggle();
    assert!(p.rows()[1].enabled);
}

#[test]
fn mover_reordena_pero_jamas_por_encima_de_name() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.down(); // size
    p.down(); // mtime
    p.move_up(); // mtime <-> size
    let orden: Vec<Option<Builtin>> = p.rows().iter().map(|r| r.builtin).collect();
    assert_eq!(orden[1], Some(Builtin::Mtime));
    assert_eq!(orden[2], Some(Builtin::Size));
    assert_eq!(p.cursor(), 1, "el cursor sigue a la fila movida");
    p.move_up(); // ya toca name: no-op
    assert_eq!(p.rows()[0].builtin, Some(Builtin::Name));
    assert_eq!(p.cursor(), 1);
}

#[test]
fn sort_current_aplica_after_click_sobre_la_fila() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.down(); // size
    p.sort_current();
    assert_eq!(p.sort().column, SortColumn::Size);
    assert_eq!(p.sort().dir, SortDir::Asc);
    p.sort_current(); // segunda vez invierte
    assert_eq!(p.sort().dir, SortDir::Desc);
}

#[test]
fn sort_current_en_fila_no_ordenable_es_noop() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    for _ in 0..3 {
        p.down(); // kind
    }
    let antes = p.sort();
    p.sort_current();
    assert_eq!(p.sort(), antes, "kind no es ordenable");
}

#[test]
fn finish_emite_solo_las_habilitadas_en_orden() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.down();
    p.toggle(); // size fuera
    let picked = p.finish();
    assert_eq!(picked.ids, vec!["name".to_owned(), "mtime".to_owned()]);
    assert_eq!(picked.scheme_target, None, "sin override → default");
}

#[test]
fn ids_opacos_se_preservan_y_viajan_enteros() {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec![
            "name".into(),
            "attr:posix.mode".into(),
            "size".into(),
            "no parsea!".into(),
        ]),
        ..Default::default()
    };
    let s = ColumnsSettings::resolve(&cfg);
    let mut p = ColumnsPicker::open(&s, "file", SortSpec::default());
    // Los opacos están, habilitados, en su posición; el catálogo restante
    // (mtime, kind) cierra la lista deshabilitado.
    let ids: Vec<&str> = p.rows().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids[..4], ["name", "attr:posix.mode", "size", "no parsea!"]);
    let picked = p.finish();
    assert!(picked.ids.contains(&"attr:posix.mode".to_owned()));
    assert!(picked.ids.contains(&"no parsea!".to_owned()));
    // Y son toggleables: apagar el attr lo saca del resultado.
    p.down();
    p.toggle();
    assert!(!p.finish().ids.contains(&"attr:posix.mode".to_owned()));
}

#[test]
fn scheme_con_override_apunta_al_scheme() {
    let cfg = norte_config::ColumnsConfig {
        schemes: [(
            "sftp".to_owned(),
            norte_config::SchemeColumns {
                columns: Some(vec!["name".into(), "mtime".into()]),
                sort: None,
            },
        )]
        .into(),
        ..Default::default()
    };
    let s = ColumnsSettings::resolve(&cfg);
    let p = ColumnsPicker::open(&s, "sftp", SortSpec::default());
    assert!(p.scheme_override());
    assert_eq!(p.finish().scheme_target.as_deref(), Some("sftp"));
    let habilitadas: Vec<&str> = p
        .rows()
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(habilitadas, ["name", "mtime"]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-frontend columns_picker`
Expected: compile error (module/types missing).

- [ ] **Step 3: Add the `ColumnsSettings` accessors** (in `columns.rs`, near `layout_items_for`)

```rust
/// La lista de ids CONFIGURADA efectiva para `scheme` en forma Display,
/// override del scheme > default > set built-in (#108 7a). Preserva los
/// ids sin renderer y los que no parsean: el picker los enseña y los
/// re-persiste ENTEROS — limpiar la config del usuario no es su trabajo
/// (doctor los reporta).
#[must_use]
pub fn raw_ids_for(&self, scheme: &str) -> Vec<String> {
    if let Some(ids) = self.raw_schemes.get(scheme).and_then(|(c, _)| c.as_ref()) {
        return ids.clone();
    }
    if let Some(ids) = &self.raw_default {
        return ids.clone();
    }
    default_layout_items()
        .iter()
        .map(|(b, _)| ColumnId::Builtin(*b).to_string())
        .collect()
}

/// ¿Tiene `scheme` una entrada propia en la config (columns o sort)?
/// Decide el TARGET del picker: con entrada, el save escribe el scheme;
/// sin ella, el default (#108 7a — una regla, dicha en el título).
#[must_use]
pub fn has_scheme_entry(&self, scheme: &str) -> bool {
    self.schemes.contains_key(scheme)
}

/// Aplica el resultado del picker EN MEMORIA (#108 7a): mismas semánticas
/// que el write-back a disco (`persist_columns`) para que la sesión y el
/// fichero no diverjan mientras llega el hot-reload.
pub fn apply_picked(&mut self, target: Option<&str>, ids: &[String], sort: crate::sort::SortSpec) {
    let parsed = parse_ids(ids);
    match target {
        Some(s) => {
            self.raw_schemes
                .entry(s.to_owned())
                .or_default()
                .0 = Some(ids.to_vec());
            self.schemes.insert(s.to_owned(), (Some(parsed), Some(sort)));
        }
        None => {
            self.raw_default = Some(ids.to_vec());
            self.default_set = Some(parsed);
            self.default_sort = sort;
        }
    }
}
```

This requires keeping the RAW id lists at resolve time: add private fields `raw_default: Option<Vec<String>>` and `raw_schemes: BTreeMap<String, (Option<Vec<String>>, ())>` — simplest concrete shape: store `raw_default: Option<Vec<String>>` plus reuse the existing `schemes` map for parsed data and a parallel `raw_schemes: BTreeMap<String, Option<Vec<String>>>` for raw lists (adjust the code above accordingly; keep ONE obvious representation, populate both in `resolve`). `apply_picked` for a scheme keeps `raw_schemes` and `schemes` in step.

Also normalize name-first in `layout_items_for` (gap b): after building `out`, if `name` exists but is not at index 0, move it to the front (`out.remove(pos)` + `insert(0, ..)`); update its rustdoc («el nombre jamás desaparece NI deja de ir primero — la TUI presupuesta la primera columna como el nombre») and add a test:

```rust
#[test]
fn layout_items_normaliza_name_al_frente() {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["size".into(), "name".into()]),
        ..Default::default()
    };
    let s = ColumnsSettings::resolve(&cfg);
    let items = s.layout_items_for("file");
    assert_eq!(items[0].0, Builtin::Name);
    assert_eq!(items[1].0, Builtin::Size);
}
```

- [ ] **Step 4: Implement `columns_picker.rs`**

```rust
//! Modelo PURO del picker de columnas (#108 bloque 7a): estado y
//! transiciones sin IO ni render — la TUI lo envuelve en un overlay y la
//! GUI (7c) en un panel, misma máquina. Regla 7: la lógica vive aquí.

use crate::columns::{Builtin, ColumnId, ColumnsSettings, sort_column};
use crate::sort::SortSpec;

/// Una fila del picker: el id en forma Display (lo que se persiste),
/// su builtin si lo es (etiqueta Fluent + ordenable), y si está activa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    /// Id tal cual viaja a la config (`"size"`, `"attr:posix.mode"`…).
    pub id: String,
    /// `Some` para los builtin (etiqueta localizada, sort); `None` para
    /// ids sin renderer o que no parsean — se enseñan y preservan.
    pub builtin: Option<Builtin>,
    /// Activa = aparece en la lista persistida.
    pub enabled: bool,
}

/// Resultado de confirmar el picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// Ids habilitados, en orden de pintado.
    pub ids: Vec<String>,
    /// El orden elegido.
    pub sort: SortSpec,
    /// `Some(scheme)` si el save va al override del scheme; `None` = default.
    pub scheme_target: Option<String>,
}

/// Estado del picker. `open` parte del set EFECTIVO del pane (override del
/// scheme > default) y cierra la lista con el resto del catálogo builtin
/// deshabilitado. `name` va primero y es inmutable (ni toggle ni desplazable
/// de la cabeza: la primera columna ES el nombre por contrato del render).
#[derive(Debug, Clone)]
pub struct ColumnsPicker {
    rows: Vec<PickerRow>,
    cursor: usize,
    sort: SortSpec,
    scheme: String,
    scheme_override: bool,
}

impl ColumnsPicker {
    /// Construye el picker para el pane en `scheme` con su orden actual.
    #[must_use]
    pub fn open(settings: &ColumnsSettings, scheme: &str, current_sort: SortSpec) -> Self {
        let raw = settings.raw_ids_for(scheme);
        let mut rows: Vec<PickerRow> = Vec::new();
        for id in &raw {
            let builtin = match id.parse::<ColumnId>() {
                Ok(ColumnId::Builtin(b)) => Some(b),
                _ => None,
            };
            if builtin.is_some() && rows.iter().any(|r| r.builtin == builtin) {
                continue; // dedup builtin, mismo criterio que layout_items_for
            }
            rows.push(PickerRow { id: id.clone(), builtin, enabled: true });
        }
        // name primero e inmutable (contrato del render).
        if let Some(pos) = rows.iter().position(|r| r.builtin == Some(Builtin::Name)) {
            let name = rows.remove(pos);
            rows.insert(0, name);
        } else {
            rows.insert(
                0,
                PickerRow {
                    id: ColumnId::Builtin(Builtin::Name).to_string(),
                    builtin: Some(Builtin::Name),
                    enabled: true,
                },
            );
        }
        // Catálogo restante, deshabilitado, en orden canónico.
        for b in [Builtin::Size, Builtin::Mtime, Builtin::Kind] {
            if !rows.iter().any(|r| r.builtin == Some(b)) {
                rows.push(PickerRow {
                    id: ColumnId::Builtin(b).to_string(),
                    builtin: Some(b),
                    enabled: false,
                });
            }
        }
        Self {
            rows,
            cursor: 0,
            sort: current_sort,
            scheme: scheme.to_owned(),
            scheme_override: settings.has_scheme_entry(scheme),
        }
    }

    /// Filas en orden de pintado.
    #[must_use]
    pub fn rows(&self) -> &[PickerRow] { &self.rows }
    /// Fila bajo el cursor.
    #[must_use]
    pub fn cursor(&self) -> usize { self.cursor }
    /// El orden en curso (se aplica al confirmar, no antes).
    #[must_use]
    pub fn sort(&self) -> SortSpec { self.sort }
    /// Scheme del pane que abrió el picker.
    #[must_use]
    pub fn scheme(&self) -> &str { &self.scheme }
    /// ¿El save irá al override del scheme?
    #[must_use]
    pub fn scheme_override(&self) -> bool { self.scheme_override }

    /// Cursor arriba (saturante).
    pub fn up(&mut self) { self.cursor = self.cursor.saturating_sub(1); }
    /// Cursor abajo (saturante).
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// Activa/desactiva la fila bajo el cursor. `name` es inmutable.
    pub fn toggle(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(r) = self.rows.get_mut(self.cursor) {
            r.enabled = !r.enabled;
        }
    }

    /// Sube la fila bajo el cursor un puesto (jamás por encima de `name`);
    /// el cursor la sigue.
    pub fn move_up(&mut self) {
        if self.cursor > 1 {
            self.rows.swap(self.cursor, self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Baja la fila bajo el cursor un puesto; el cursor la sigue.
    pub fn move_down(&mut self) {
        if self.cursor >= 1 && self.cursor + 1 < self.rows.len() {
            self.rows.swap(self.cursor, self.cursor + 1);
            self.cursor += 1;
        }
    }

    /// Ordena por la columna bajo el cursor con la semántica del click de
    /// cabecera ([`SortSpec::after_click`]); no-op en filas no ordenables.
    pub fn sort_current(&mut self) {
        if let Some(sc) = self.rows.get(self.cursor).and_then(|r| r.builtin).and_then(sort_column) {
            self.sort = self.sort.after_click(sc);
        }
    }

    /// El resultado a aplicar/persistir al confirmar.
    #[must_use]
    pub fn finish(&self) -> Picked {
        Picked {
            ids: self
                .rows
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.id.clone())
                .collect(),
            sort: self.sort,
            scheme_target: self.scheme_override.then(|| self.scheme.clone()),
        }
    }
}
```

- [ ] **Step 5: Run to verify green**

Run: `cargo nextest run -p norte-frontend columns_picker columns::`
Expected: all new tests pass, existing columns tests untouched.

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p norte-frontend --all-targets -- -D warnings; echo EXIT=$?` (EXIT=0), `cargo fmt --all`.

```bash
git add crates/norte-frontend/src/columns_picker.rs crates/norte-frontend/src/columns.rs crates/norte-frontend/src/lib.rs
git commit -m "feat(frontend): ColumnsPicker pure model + settings write-back (#108 block 7a)

The picker state machine is frontend-shared (rule 7: the TUI overlay
now, the GUI panel in 7c). Rows preserve non-builtin ids verbatim —
cleaning the user's config is doctor's job, not the picker's. name is
pinned first and immutable, matching the render contract, and
layout_items_for now normalizes name to the front always (a mid-list
name misrendered the TUI, which budgets the first width as the name).
apply_picked mirrors the on-disk write-back so session and file cannot
diverge while the hot-reload lands.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `persist_columns` nested writer in norte-config

**Files:**
- Modify: `crates/norte-config/src/load.rs` (new helper + tests)
- Modify: `crates/norte-config/src/lib.rs` (re-export, following `persist_set`)

`persist_set` writes flat `[section] key = scalar`; `[ui.columns]` is nested and `default` is an ARRAY (first array value ever persisted — needs its own round-trip test). Field names MUST match what `merge_ui_columns`/`SortSection` parse — read `crates/norte-config/src/schema.rs:182-229` first and use those exact key names (`default`, `sort` with its real subfield names — verify whether the loader reads `dir = "asc"|"desc"` or `descending`, and `dirs_first`; serialize what `load` parses, pinned by the round-trip test).

- [ ] **Step 1: Write the failing tests** (alongside the existing `persist_set` tests in `load.rs`)

```rust
#[test]
fn persist_columns_default_round_tripea_por_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    persist_columns(
        dir.path(),
        None,
        &["name".to_owned(), "mtime".to_owned(), "attr:posix.mode".to_owned()],
        PersistSort { column: "mtime", descending: true, dirs_first: true },
    )
    .expect("escritura");
    let layers = Layers { dirs: vec![(dir.path().to_path_buf(), LayerKind::User)] };
    let cfg = load_common(&layers).expect("load");
    assert_eq!(
        cfg.ui_columns.default_columns.as_deref(),
        Some(&["name".to_owned(), "mtime".to_owned(), "attr:posix.mode".to_owned()][..])
    );
    let sort = cfg.ui_columns.sort.expect("sort persistido");
    assert_eq!(sort.column, SortColumnKey::Mtime);
    assert!(sort.descending);
}

#[test]
fn persist_columns_scheme_escribe_el_override_y_preserva_comentarios() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("norte.toml"),
        "# mi config\n[ui]\ntheme = \"default\"\n",
    )
    .expect("seed");
    persist_columns(
        dir.path(),
        Some("sftp"),
        &["name".to_owned(), "size".to_owned()],
        PersistSort { column: "size", descending: false, dirs_first: true },
    )
    .expect("escritura");
    let texto = std::fs::read_to_string(dir.path().join("norte.toml")).expect("leer");
    assert!(texto.contains("# mi config"), "comentarios preservados");
    assert!(texto.contains("theme = \"default\""), "lo previo intacto");
    let layers = Layers { dirs: vec![(dir.path().to_path_buf(), LayerKind::User)] };
    let cfg = load_common(&layers).expect("load");
    let sc = cfg.ui_columns.schemes.get("sftp").expect("override sftp");
    assert_eq!(sc.columns.as_deref(), Some(&["name".to_owned(), "size".to_owned()][..]));
}

#[test]
fn persist_columns_rechaza_ui_no_tabla_sin_panico() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").expect("seed");
    let err = persist_columns(
        dir.path(),
        None,
        &["name".to_owned()],
        PersistSort { column: "name", descending: false, dirs_first: true },
    )
    .expect_err("forma inesperada");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
```

Adapt helper-struct/type names to the crate's idiom (`Layers`, `LayerKind`, the actual load entry point — the existing `ui_columns_carga_valida_y_fusiona` test at `load.rs:1114` shows the loading harness to copy; if `SortChoice` uses different field names, mirror them in `PersistSort`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-config persist_columns`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
/// El `sort` a persistir (#108 7a) — espejo consciente de `SortChoice`:
/// este writer serializa EXACTAMENTE lo que `load` parsea (pineado por el
/// round-trip test de al lado).
#[derive(Debug, Clone, Copy)]
pub struct PersistSort<'a> {
    /// `"name"` | `"size"` | `"mtime"` (vocabulario cerrado del load).
    pub column: &'a str,
    /// `true` = descendente.
    pub descending: bool,
    /// Directorios primero.
    pub dirs_first: bool,
}

/// Escribe la selección del picker (#108 7a) en el `norte.toml` de `dir`,
/// PRESERVANDO comentarios y formato (mismo `toml_edit` y mismos guards de
/// forma que [`persist_set`], nivel a nivel): `scheme = None` fija
/// `[ui.columns] default + sort`; `Some(s)` fija
/// `[ui.columns.scheme.<s>] columns + sort`. BLOQUEANTE: I/O síncrono — el
/// caller lo envuelve en `spawn_blocking` (regla 2).
///
/// # Errors
/// [`std::io::Error`] si el TOML no parsea, un nivel existente no es tabla,
/// o falla el I/O.
pub fn persist_columns(
    dir: &std::path::Path,
    scheme: Option<&str>,
    ids: &[String],
    sort: PersistSort<'_>,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = /* read-or-new igual que persist_set, mismo error saneado */;

    // Camina/crea la cadena de tablas con guard de forma en CADA nivel
    // (mismo criterio `is_table_like` que persist_set — un nivel escalar
    // indexado en pánico tumbaría el hilo de fondo).
    fn nested<'d>(
        doc: &'d mut toml_edit::DocumentMut,
        path_disp: &std::path::Path,
        segs: &[&str],
    ) -> std::io::Result<&'d mut toml_edit::Table> {
        let mut t = doc.as_table_mut();
        for s in segs {
            if let Some(existing) = t.get(s)
                && !existing.is_table_like()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{}: [{s}] no es una tabla (forma inesperada); corrígelo o bórralo",
                        path_disp.display()
                    ),
                ));
            }
            let item = t.entry(s).or_insert_with(|| {
                let mut nt = toml_edit::Table::new();
                // Intermedia implícita: se emite el header hoja
                // ([ui.columns]), no una cadena de headers vacíos.
                nt.set_implicit(true);
                toml_edit::Item::Table(nt)
            });
            t = item
                .as_table_mut()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "tabla"))?;
        }
        Ok(t)
    }

    let segs: Vec<&str> = match scheme {
        None => vec!["ui", "columns"],
        Some(s) => vec!["ui", "columns", "scheme", s],
    };
    let table = nested(&mut doc, &path, &segs)?;
    table.set_implicit(false); // la hoja SÍ se emite aunque quede solo con arrays

    let mut arr = toml_edit::Array::new();
    for id in ids {
        arr.push(id.as_str());
    }
    let list_key = if scheme.is_none() { "default" } else { "columns" };
    table[list_key] = toml_edit::Item::Value(toml_edit::Value::Array(arr));

    let mut sort_tbl = toml_edit::InlineTable::new();
    sort_tbl.insert("column", sort.column.into());
    /* dir/descending + dirs_first: usa los NOMBRES DE CLAVE reales que
       parsea merge_ui_columns/SortSection (verificado en Step 1) */
    table["sort"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(sort_tbl));

    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}
```

(The exact borrow dance with `nested` may need `doc.as_table_mut()` threading — implement to compile cleanly; the CONTRACT is what the tests pin: nested creation, comment preservation, per-level shape guard, array + inline-table sort, loader round-trip.)

- [ ] **Step 4: Run to verify green + lint + commit**

Run: `cargo nextest run -p norte-config; echo EXIT=$?` then `cargo clippy -p norte-config --all-targets -- -D warnings; echo EXIT=$?` then `cargo fmt --all`.

```bash
git add crates/norte-config/src/load.rs crates/norte-config/src/lib.rs
git commit -m "feat(config): persist_columns — nested [ui.columns] writer (#108 block 7a)

persist_set writes flat [section] key = scalar and cannot reach
[ui.columns]; this walks/creates the table chain with the same
is_table_like guard at every level (a scalar level would panic the
background write thread) and serializes the first array value the
persist layer ever writes, pinned by a loader round-trip test so the
writer can never drift from what merge_ui_columns parses.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: keymap surface — `pane.columns` + three new `dialog.*` verbs

**Files:**
- Modify: `crates/norte-tui/src/keymap.rs` (`commands!` at :100-147; `DIALOG_COMMANDS` at :150; `dialog_hint_id` mapping)
- Modify: `crates/norte-frontend/presets/keymap/orthodox.toml`, `vim.toml`, `cua.toml`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `es.ftl`

- [ ] **Step 1: Vocabulary**

In the `commands!` invocation, next to the other pane verbs: `"pane.columns" => PaneColumns,`.
In `DIALOG_COMMANDS`: add `"dialog.move-up"`, `"dialog.move-down"`, `"dialog.sort"`.
In `dialog_hint_id` (or wherever `dialog.*` → `dialog-cmd-*` is mapped): add the three.

- [ ] **Step 2: Presets** (ALL THREE files — the strict build fails otherwise, and the 1:1 test `crates/norte-tui/tests/keymap.rs:514` enforces preset/vocabulary parity)

`[pane]` array, near `alt+.`:

```toml
{ on = ["alt+c"], run = "pane.columns" },
```

`[dialog]` array:

```toml
{ on = ["shift+up"], run = "dialog.move-up" },
{ on = ["shift+down"], run = "dialog.move-down" },
{ on = ["s"], run = "dialog.sort" },
```

Before writing: `grep -n "shift+" crates/norte-frontend/presets/keymap/*.toml` to confirm how shifted chords are spelled in this keymap (there are `shift+f8`-style bindings; if `shift+up` is unprecedented, verify `parse_chord` + `chord_from_crossterm` accept it — the crossterm adapter must produce `shift`+`Up`. If arrow-with-shift doesn't survive the adapter, fall back to `ctrl+up`/`ctrl+down` and note the deviation). The spec suggests `J`/`K` for reorder; uppercase letters arrive from crossterm as `Char('J')+SHIFT` — check how the adapter normalizes case before also adding `"shift+j"`/`"shift+k"` to the `on` arrays; add them only if they resolve.

`s` in `[dialog]` is safe for text-entry modals: `dialog_action` filters by per-modal ALLOWLIST, and only `ALLOW_COLUMNS` will contain `dialog.sort` — for `Mkdir`/`TransferName`/`MarkPattern` the resolution misses and the char falls through to the text buffer. VERIFY that claim against `on_dialog_key` (`main.rs:2814`) — if resolution swallows the char before the allowlist filter, scope the `s` binding out (e.g. use `ctrl+s`) and record it.

- [ ] **Step 3: Fluent** (both `en.ftl` and `es.ftl`)

```ftl
help-cmd-pane-columns = Column picker            # es: Selector de columnas
dialog-cmd-move-up = move up                     # es: subir
dialog-cmd-move-down = move down                 # es: bajar
dialog-cmd-sort = sort by                        # es: ordenar por
columns-picker-title = Columns — { $target }     # es: Columnas — { $target }
columns-picker-target-default = all schemes      # es: todos los schemes
msg-columns-saved = Columns saved                # es: Columnas guardadas
```

(Exact wording free; keys fixed. The existing tests `todo_comando_tiene_ayuda_traducida` and `todo_dialog_command_tiene_etiqueta_traducida` fail until both locales have them.)

- [ ] **Step 4: Run the keymap test suite**

Run: `cargo nextest run -p norte-tui keymap; echo EXIT=$?`
Expected: green (vocabulary/preset/Fluent parity tests all pass). This task does NOT commit alone — it compiles but `Command::PaneColumns` has no dispatch arm yet (compile error is EXPECTED at this point if you build norte-tui: proceed straight into Task 4, they commit together).

---

### Task 4: TUI overlay — state, keys, draw, apply+persist, reload gap

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (App field, `ALLOW_COLUMNS`, `open_columns_picker`, picker input plumbing)
- Modify: `crates/norte-tui/src/hints.rs` (`DialogHints.columns`)
- Modify: `crates/norte-tui/src/main.rs` (run-loop branch, `on_columns_key`, dispatch arm, `apply_picked_columns`, reload fix)
- Modify: `crates/norte-tui/src/ui.rs` (`draw_columns_picker` + draw-order slot)

- [ ] **Step 1: App state** (`app.rs`, next to `theme_picker` at :647)

```rust
/// Overlay del picker de columnas (#108 7a): mismo patrón que
/// `theme_picker` — un Option en App, NO una variante de Modal (Modal es
/// confirmación; esto es lista con cursor). El modelo vive en
/// norte-frontend (`ColumnsPicker`, regla 7).
pub columns_picker: Option<norte_frontend::columns_picker::ColumnsPicker>,
```

Init `None` in the constructor (near :1351). Plus:

```rust
/// Abre el picker para el pane con foco (#108 7a): parte del set efectivo
/// de su scheme y de su orden VIVO (el del pane, no el de config — un
/// sort de cabecera previo no se pierde al abrir).
pub fn open_columns_picker(&mut self) {
    let pane = self.focused_pane_index(); // usa el accessor real del App
    let scheme = self.panes[pane].dir().scheme().to_owned();
    let sort = self.panes[pane].sort();
    self.columns_picker = Some(norte_frontend::columns_picker::ColumnsPicker::open(
        &self.columns,
        &scheme,
        sort,
    ));
}
```

(Adapt `focused_pane_index()` to however App names the focused-pane accessor — grep `fn focused` / `self.focus` in app.rs.)

- [ ] **Step 2: Allowlist + hints**

`app.rs` next to `ALLOW_PICKER` (:2322):

```rust
/// Claves del picker de columnas (#108 7a) — única fuente para dispatch
/// (on_columns_key) y para el hint del pie.
pub const ALLOW_COLUMNS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.confirm",
    "dialog.cancel",
];
```

`hints.rs`: add `pub columns: String` to `DialogHints` (:88) and in `build` (:113):

```rust
columns: dialog_hints(&without_navigation(ALLOW_COLUMNS), eff),
```

(`without_navigation` strips up/down from the PRINTED hint only — footer space; the keys still work.)

- [ ] **Step 3: Key handler + run-loop routing** (`main.rs`)

Route BEFORE the modal branch, next to the `theme_picker` branch at :921-922 (order among overlay branches follows the existing chain; the picker behaves like theme_picker — ctrl+c hard-quit guard included, same as every overlay handler):

```rust
} else if app.columns_picker.is_some() {
    on_columns_key(&mut app, &resolver, mods, code).await;
}
```

```rust
/// Claves del picker de columnas (#108 7a): resuelve por keymap (pantalla
/// dialog) y filtra por ALLOW_COLUMNS — misma disciplina única-fuente que
/// el resto de overlays (#24).
async fn on_columns_key(
    app: &mut App,
    resolver: &keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    let Some(cmd) = resolver.dialog_command(mods, code) else { return };
    if !crate::app::ALLOW_COLUMNS.contains(&cmd) {
        return;
    }
    let Some(p) = app.columns_picker.as_mut() else { return };
    match cmd {
        "dialog.up" => p.up(),
        "dialog.down" => p.down(),
        "dialog.toggle-enabled" => p.toggle(),
        "dialog.move-up" => p.move_up(),
        "dialog.move-down" => p.move_down(),
        "dialog.sort" => p.sort_current(),
        "dialog.cancel" => app.columns_picker = None,
        "dialog.confirm" => {
            let picked = p.finish();
            app.columns_picker = None;
            apply_picked_columns(app, picked).await;
        }
        _ => {}
    }
}
```

(`resolver.dialog_command` — use the real resolver API the other overlay handlers use; copy from `on_theme_picker_key` at :1492.)

```rust
/// Aplica el resultado del picker (#108 7a): sesión primero (settings en
/// memoria + re-sort de TODO pane en ese scheme·target), disco después
/// (`persist_columns` en spawn_blocking — regla 2), toast por status bar
/// con categoría de error, jamás el Display del SO (#73).
async fn apply_picked_columns(app: &mut App, picked: norte_frontend::columns_picker::Picked) {
    app.columns
        .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
    for i in 0..app.panes.len() {
        app.apply_scheme_sort(i);
    }
    let Some(dir) = config::user_config_dir() else {
        app.message = Some(t("msg-settings-no-config-dir"));
        return;
    };
    let ids = picked.ids.clone();
    let scheme = picked.scheme_target.clone();
    let sort = picked.sort;
    let res = tokio::task::spawn_blocking(move || {
        norte_config::persist_columns(
            &dir,
            scheme.as_deref(),
            &ids,
            norte_config::PersistSort {
                column: match sort.column {
                    norte_frontend::SortColumn::Name => "name",
                    norte_frontend::SortColumn::Size => "size",
                    norte_frontend::SortColumn::Mtime => "mtime",
                },
                descending: sort.dir == norte_frontend::SortDir::Desc,
                dirs_first: sort.dirs_first,
            },
        )
    })
    .await;
    match res {
        Ok(Ok(_)) => app.message = Some(t("msg-columns-saved")),
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-settings-save-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "tarea de fondo de persist_columns no terminó");
            app.message = Some(t("msg-settings-save-crashed"));
        }
    }
}
```

Dispatch arm (`main.rs:3483` match, modeled on `Command::AppTheme`):

```rust
Command::PaneColumns => app.open_columns_picker(),
```

Reload gap fix — in `reload_config` (insert next to `app.openers = cfg.openers.clone();` at ~:2204):

```rust
// #108 7a: [ui.columns] editado fuera también refresca la sesión (antes
// solo arrancaba); el re-sort mantiene los panes coherentes con el
// fichero — el persist del picker dispara este mismo camino y es
// idempotente con lo ya aplicado en memoria.
app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
for i in 0..app.panes.len() {
    app.apply_scheme_sort(i);
}
```

- [ ] **Step 4: Draw** (`ui.rs` — call slot right after the `theme_picker` draw at :80-82; picker paints under the modal, same as theme picker)

```rust
if let Some(p) = &app.columns_picker {
    draw_columns_picker(frame, p, theme, &app.dialog_hints.columns);
}
```

```rust
/// Overlay del picker de columnas (#108 7a): lista con cursor — checkbox,
/// etiqueta (Fluent para builtins; el id CRUDO enmascarado para los que no
/// parsean o no tienen renderer — texto de config del usuario, #73: se
/// pinta con mask_terminal_hazards) y la flecha del sort en su columna.
/// Mismo esqueleto que `draw_theme_picker` (List + ListState, hint en
/// title_bottom, ancho por contenido con suelo del footer).
fn draw_columns_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::columns_picker::ColumnsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    use norte_frontend::columns::Builtin;
    let target = if p.scheme_override() {
        p.scheme().to_owned()
    } else {
        t("columns-picker-target-default")
    };
    let titulo = ta("columns-picker-title", &[("target", &target)]);
    let items: Vec<ListItem<'_>> = p
        .rows()
        .iter()
        .map(|r| {
            let marca = if r.enabled { "[x]" } else { "[ ]" };
            let etiqueta = match r.builtin {
                Some(Builtin::Name) => t("col-header-name"),
                Some(Builtin::Size) => t("col-header-size"),
                Some(Builtin::Mtime) => t("col-header-mtime"),
                Some(Builtin::Kind) => t("col-header-kind"),
                None => norte_encoding::mask_terminal_hazards(&r.id),
            };
            let flecha = match (r.builtin.and_then(norte_frontend::columns::sort_column), p.sort()) {
                (Some(sc), s) if sc == s.column => {
                    if s.dir == norte_frontend::SortDir::Asc { " ▲" } else { " ▼" }
                }
                _ => "",
            };
            ListItem::new(format!(" {marca} {etiqueta}{flecha}"))
        })
        .collect();
    /* Clear + centered box + List con highlight de Role::Selection +
       hint en title_bottom — copiar el esqueleto de draw_theme_picker
       (ui.rs:551) incluida la cuenta de ancho por Line::width. */
}
```

(`mask_terminal_hazards` — use the real helper name/location; grep `mask_terminal_hazards` in norte-encoding/norte-frontend. The raw-id row is the ONLY third-party-ish text in this overlay; everything else is Fluent.)

- [ ] **Step 5: Compile + clippy**

Run: `cargo nextest run -p norte-tui; echo EXIT=$?` (existing suite green), `cargo clippy -p norte-tui --all-targets -- -D warnings; echo EXIT=$?`.

- [ ] **Step 6: Commit** (Tasks 3+4 together — the vocabulary without the overlay doesn't compile usefully)

```bash
git add crates/norte-tui crates/norte-frontend/presets crates/norte-i18n
git commit -m "feat(tui,frontend): column picker overlay — alt+c, reorder, sort, persist (#108 block 7a)

pane.columns opens a ThemePicker-style overlay over the shared
ColumnsPicker model: space toggles (name immutable), shift+up/down
reorders below the pinned name, s applies the header-click sort
semantics to the row under the cursor, Enter applies in-session
(settings + re-sort of both panes) and persists via persist_columns in
spawn_blocking, Esc discards. Keys resolve through the dialog screen
with ALLOW_COLUMNS as the single source for dispatch and the generated
footer hint. Three new dialog verbs (move-up/move-down/sort) join the
closed vocabulary, all three presets and both locales. reload_config
now refreshes app.columns + re-sorts (hot-reload of [ui.columns] was
dead since block 4).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: tests — snapshot, behavior, persistence e2e

**Files:**
- Modify: `crates/norte-tui/tests/snapshots_ui.rs`
- Create: `crates/norte-tui/tests/columns_picker.rs`

- [ ] **Step 1: Snapshot** (pattern: `snapshot_theme_picker_80x24` at :233 — including its loud extra assertion)

```rust
#[test]
fn snapshot_columns_picker_80x24() {
    let mut app = app_base();
    app.open_columns_picker();
    // Un id opaco hostil en la config: la fila se pinta ENMASCARADA.
    // (Construye la config vía ColumnsConfig con "attr:x\u{202E}evil" en
    // default_columns, resuelve a app.columns ANTES de open.)
    let texto = render_80x24(&app);
    let hint = &app.dialog_hints.columns;
    assert!(!hint.is_empty(), "hint generado presente");
    assert!(texto.contains(hint), "el pie no se trunca en silencio");
    assert!(!texto.contains('\u{202E}'), "el RLO de la config jamás llega crudo");
    insta::assert_snapshot!(texto);
}
```

Order the test body so the hostile config is applied before `open_columns_picker` (build `ColumnsSettings::resolve` from a literal `ColumnsConfig` with the hostile id, assign to `app.columns`). Inspect the generated `.snap` before accepting: checkbox column aligned, arrow on name, masked row visible. `git status` must show ONLY the intended `.snap` (the `*.snap.new` gitignore from block 5 protects the tree, but inspect anyway).

- [ ] **Step 2: Behavior + persistence test** (`tests/columns_picker.rs`, model on `tests/theme_picker.rs` + `tests/theme_persist.rs` for the hermetic `NORTE_CONFIG_DIR` harness)

Three tests:

```rust
/// Abrir → bajar a mtime → s (sort) → Enter: el pane queda ordenado por
/// mtime asc y el norte.toml del NORTE_CONFIG_DIR hermético contiene
/// [ui.columns] con sort.column = "mtime".
#[tokio::test]
async fn picker_ordena_y_persiste() { /* harness de theme_persist.rs */ }

/// Abrir → apagar size → Enter: layout_items_for del scheme ya no trae
/// Size y el fichero persiste default = ["name", "mtime"].
#[tokio::test]
async fn picker_toggle_persiste_la_lista() { /* ídem */ }

/// Esc descarta: ni settings ni fichero cambian.
#[tokio::test]
async fn picker_cancel_no_toca_nada() { /* ídem */ }
```

Write them against however `theme_picker.rs` drives keys (it calls the `on_*_key` handler or `App` methods directly — copy the driving style; if the tests drive `App::theme_picker_input` directly, expose the equivalent pure path and drive `ColumnsPicker` + `apply_picked_columns`).

- [ ] **Step 3: Run everything**

Run: `cargo nextest run -p norte-tui; echo EXIT=$?` and `cargo nextest run -p norte-frontend -p norte-config; echo EXIT=$?`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add crates/norte-tui/tests
git commit -m "test(tui): column picker — snapshot with hostile config id, behavior, persist e2e (#108 block 7a)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: gates, reviews, close

- [ ] **Step 1: CHANGELOG** — entry under unreleased (picker: alt+c, toggle/reorder/sort/persist; hot-reload fix; name-first normalization). Amend into the Task-4 commit or a small `docs(changelog)` commit — follow the block-6 style (CHANGELOG went with the feature commit).
- [ ] **Step 2: Gates** — `just ci-fast; echo EXIT=$?` (workspace) and `just gui-ci; echo EXIT=$?` (GUI compiles against the changed norte-frontend — `build_for_subset` skips `pane.columns`, so no GUI code change is expected; if the GUI build breaks, STOP and fix the frontend API compatibly instead).
- [ ] **Step 3: Reviewers** — rust-reviewer over the full block diff; encoding-auditor over the picker draw (the masked raw-id row + hostile-config snapshot). Apply findings as `fix(tui): apply the #108 block-7a review findings`.
- [ ] **Step 4: Deferrals on #108** — 7b (`[[ui.columns.spec]]` + w/f cycles), 7c (GUI panel + GUI keymap), dirs_first not picker-editable, `J`/`K` chords if the adapter forced the arrow-only fallback.
