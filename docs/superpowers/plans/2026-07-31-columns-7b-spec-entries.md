# Columns block 7b — `[[ui.columns.spec]]` + picker format cycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-column presentation config (`[[ui.columns.spec]]`: width, align, format, header) parsed, merged, honored by both renderers and reported by doctor; plus `f` in the TUI picker cycling the column's format and persisting it.

**Architecture:** Config keeps ids open but vocabularies closed (sort precedent: invalid value = load error naming the path). `ColumnsSettings::resolve` folds spec entries into a per-column `ColumnStyle` (width override → `layout_items_for`; format → `builtin_cell`; header → sanitized-and-capped custom label; align → renderers). Scheme-level `spec` entries win over global ones (spec Layer 4). The picker gains one verb (`dialog.cycle-format`, key `f`) and persists changed formats as global `[[ui.columns.spec]]` entries through a new replace-by-id ArrayOfTables writer (the `persist_hotlist_add` precedent).

**Tech Stack:** serde + schemars (schema golden `NORTE_UPDATE_SCHEMA=1`), toml_edit ArrayOfTables, existing 7a picker/persist machinery.

**Spec:** `docs/superpowers/specs/2026-07-24-columns-design.md` Layer 4 (`[[ui.columns.spec]]`) + Layer 6. Issue #108.

**Scope decisions (recorded):**
- **Width cycling (`w`) in the picker is DEFERRED**: `w` is taken by `dialog.newer`, and cycling width policies without numeric entry (fixed↔auto↔flex) is near-useless without a number editor — width stays config-file-only for now. The picker cycles **format** only (`f`, free in `[dialog]` — verify in all three presets). Recorded as a deviation from Layer 6's literal "w cycles width policy".
- **Align IS honored** (left/right) — small cost in both renderers, and `deny_unknown_fields` would otherwise turn the spec's own documented example into a load error.
- Format vocabulary is closed and validated at LOAD (`exact|iec|si|relative|iso` — typo = load error, sort precedent). Whether a format FITS the column's kind (`iec` on mtime) is frontend knowledge → resolve-time diagnostic (`bad_specs`) + default format, reported by doctor as `columns-bad-spec`. Same for a spec id that doesn't parse.
- Custom `header` is config text: sanitized via the existing `sanitize_header` and capped at 24 chars at RESOLVE (single choke point); renderers keep their width-aware truncation on top.
- The picker persists format changes as **global** spec entries (spec applies "wherever that column appears"); per-scheme spec entries are file-only.
- Formats for `mode`/attr columns arrive with block 2; today the vocab maps: Size → exact/iec/si, Mtime → relative/iso, Name/Kind → no format (spec `format` on them = kind-mismatch diagnostic).

---

### Task 1: config — parse, merge, validate `[[ui.columns.spec]]`

**Files:**
- Modify: `crates/norte-config/src/schema.rs` (after `SchemeColumnsSection`, :218-225)
- Modify: `crates/norte-config/src/load.rs` (`ColumnsConfig` :385, `merge_ui_columns` :594, validation fns near `parse_sort_section`)
- Modify: `crates/norte-config/src/lib.rs` (re-exports)

- [ ] **Step 1: Failing tests** (next to `ui_columns_carga_valida_y_fusiona`, load.rs:1114 — copy its Layers harness)

```rust
#[test]
fn ui_columns_spec_carga_valida_y_precedencia_scheme() {
    // capa única: spec global para size (si + header) y kind (align left);
    // scheme sftp con su propio spec de size (exact) que GANA.
    let toml = r#"
[[ui.columns.spec]]
id = "size"
format = "si"
header = "Peso"
width = { fixed = 9 }

[[ui.columns.spec]]
id = "kind"
align = "left"

[[ui.columns.scheme.sftp.spec]]
id = "size"
format = "exact"
"#;
    let cfg = carga_una_capa(toml); // helper del harness existente
    let g = cfg.ui_columns.specs.get("size").expect("spec global size");
    assert_eq!(g.format.as_deref(), Some("si"));
    assert_eq!(g.header.as_deref(), Some("Peso"));
    assert_eq!(g.width, Some(WidthChoice::Fixed(9)));
    assert_eq!(
        cfg.ui_columns.specs.get("kind").and_then(|s| s.align),
        Some(AlignChoice::Left)
    );
    let sc = cfg.ui_columns.schemes.get("sftp").expect("scheme");
    assert_eq!(
        sc.specs.get("size").and_then(|s| s.format.as_deref()),
        Some("exact")
    );
}

#[test]
fn ui_columns_spec_vocabularios_cerrados_fallan_al_cargar() {
    for toml in [
        "[[ui.columns.spec]]\nid = \"size\"\nformat = \"sise\"\n",
        "[[ui.columns.spec]]\nid = \"size\"\nalign = \"middle\"\n",
        "[[ui.columns.spec]]\nid = \"size\"\nwidth = \"anchisimo\"\n",
        "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 0 }\n",
        "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 200 }\n",
        "[[ui.columns.spec]]\nformat = \"iec\"\n", // sin id
    ] {
        assert!(carga_una_capa_result(toml).is_err(), "debió fallar: {toml}");
    }
}

#[test]
fn ui_columns_spec_merge_por_id_ultimo_gana_por_campo() {
    // capa 1 define size {format=iec, header=A}; capa 2 re-define size
    // {format=si} → format de la 2, header de la 1 (last-wins POR CAMPO,
    // mismo criterio que el resto de [ui.columns]).
    /* dos capas con el harness Layers de ui_columns_carga_valida_y_fusiona */
}
```

(Adapt helper names to the real harness — the existing columns test shows how to build one- and two-layer `Layers`; if there is no `carga_una_capa`, inline what that test does.)

- [ ] **Step 2: Run** `cargo nextest run -p norte-config ui_columns_spec` → compile failure.

- [ ] **Step 3: schema.rs** — serde surface:

```rust
/// One `[[ui.columns.spec]]` entry (#108 block 7b): per-column
/// presentation. Keyed by `id`; a scheme block may carry its own `spec`
/// entries that win for panes on that scheme.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ColumnSpecSection {
    /// Column id this entry styles (required).
    pub id: String,
    /// `"auto"` | `{ fixed = n }` | `{ min = n, weight = m }` — cells,
    /// validated to `[1, 64]` at load.
    #[serde(default)]
    pub width: Option<WidthSection>,
    /// `"left"` | `"right"` — closed, validated at load.
    #[serde(default)]
    pub align: Option<String>,
    /// `"exact"` | `"iec"` | `"si"` | `"relative"` | `"iso"` — closed,
    /// validated at load; whether it FITS the column is the frontend's
    /// call (doctor reports mismatches).
    #[serde(default)]
    pub format: Option<String>,
    /// Custom header label (free text; the frontend sanitizes and caps).
    #[serde(default)]
    pub header: Option<String>,
}

/// The `width` of a spec entry.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum WidthSection {
    /// `"auto"` (any other string is a load error).
    Keyword(String),
    /// `{ fixed = n }`.
    Fixed {
        /// Cells.
        fixed: u16,
    },
    /// `{ min = n, weight = m }`.
    Flex {
        /// Floor in cells.
        min: u16,
        /// Share weight (0 = never grows).
        #[serde(default)]
        weight: u16,
    },
}
```

Add `#[serde(default)] pub spec: Option<Vec<ColumnSpecSection>>` to BOTH `UiColumnsSection` and `SchemeColumnsSection`.

- [ ] **Step 4: load.rs** — resolved types + validation + merge:

```rust
/// Width elegido en un spec (#108 7b), ya validado a `[1, 64]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthChoice {
    /// Ancho de la celda más ancha de la página (techo del frontend).
    Auto,
    /// Fijo en celdas.
    Fixed(u16),
    /// Reparto por peso con suelo.
    Flex {
        /// Suelo en celdas.
        min: u16,
        /// Peso del reparto.
        weight: u16,
    },
}

/// Align elegido en un spec (#108 7b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignChoice {
    /// Izquierda.
    Left,
    /// Derecha.
    Right,
}

/// Un `[[ui.columns.spec]]` resuelto (#108 7b): vocabularios YA validados
/// (typo = error de carga, patrón sort); `format` queda como string —
/// si CASA con la columna lo decide el frontend (doctor reporta).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnSpec {
    /// Ancho, si el spec lo fija.
    pub width: Option<WidthChoice>,
    /// Alineación, si el spec la fija.
    pub align: Option<AlignChoice>,
    /// Formato (vocabulario global cerrado; encaje por-columna = frontend).
    pub format: Option<String>,
    /// Cabecera propia (texto libre; el frontend la sanea y capa).
    pub header: Option<String>,
}
```

`ColumnsConfig` gains `pub specs: BTreeMap<String, ColumnSpec>`; `SchemeColumns` gains `pub specs: BTreeMap<String, ColumnSpec>`. Validation fn (`parse_spec_entries(&[ColumnSpecSection], path_label) -> Result<BTreeMap<..>, ConfigError>`): empty/missing `id` = error; align ∉ {left,right} = error; format ∉ {exact,iec,si,relative,iso} = error; width `Keyword(s)` with s ≠ "auto" = error; fixed/min outside `[1,64]` = error (name the offending path like the sort validator does — read `parse_sort_section` :775 and mirror its error style). Merge: last-wins PER FIELD by id across layers (fold each entry over the accumulated map: `Some` fields overwrite, `None` fields keep); scheme maps merge by scheme key then by id the same way.

- [ ] **Step 5: Green + lint** `cargo nextest run -p norte-config; echo EXIT=$?`, clippy, fmt.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-config
git commit -m "feat(config): [[ui.columns.spec]] — per-column width/align/format/header (#108 block 7b)

Ids stay an open set; every vocabulary is closed and validated at load
(sort precedent: a typo is a load error naming the path, never a silent
skip). Whether a format fits its column is frontend knowledge — format
survives as a validated string and doctor reports the mismatches.
Scheme-level spec entries ride SchemeColumns and win at resolve. Merge
is last-wins per field by id, same criterion as the rest of
[ui.columns].

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: frontend — `ColumnStyle` resolution + formatted cells

**Files:**
- Modify: `crates/norte-frontend/src/columns.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn style_for_aplica_spec_global_y_scheme_gana() {
    let mut cfg = norte_config::ColumnsConfig::default();
    cfg.specs.insert("size".into(), norte_config::ColumnSpec {
        format: Some("si".into()),
        header: Some("Peso".into()),
        width: Some(norte_config::WidthChoice::Fixed(9)),
        ..Default::default()
    });
    let mut sc = norte_config::SchemeColumns::default();
    sc.specs.insert("size".into(), norte_config::ColumnSpec {
        format: Some("exact".into()),
        ..Default::default()
    });
    cfg.schemes.insert("sftp".into(), sc);
    let s = ColumnsSettings::resolve(&cfg);
    assert_eq!(s.style_for("file", Builtin::Size).size_format, SizeFormat::Si);
    assert_eq!(s.style_for("sftp", Builtin::Size).size_format, SizeFormat::Exact);
    // header del global sobrevive en el scheme (last-wins POR CAMPO).
    assert_eq!(s.style_for("sftp", Builtin::Size).header.as_deref(), Some("Peso"));
    // width override llega al layout.
    let items = s.layout_items_for("file");
    let size = items.iter().find(|(b, _)| *b == Builtin::Size).expect("size");
    assert_eq!(size.1.policy, WidthPolicy::Fixed(9));
}

#[test]
fn spec_formato_que_no_casa_es_diagnostico_no_aplicado() {
    let mut cfg = norte_config::ColumnsConfig::default();
    cfg.specs.insert("mtime".into(), norte_config::ColumnSpec {
        format: Some("iec".into()), // iec en un timestamp: no casa
        ..Default::default()
    });
    let s = ColumnsSettings::resolve(&cfg);
    assert_eq!(s.style_for("file", Builtin::Mtime).time_format, TimeFormat::Relative);
    assert!(s.bad_specs.iter().any(|b| b.contains("mtime")));
}

#[test]
fn spec_header_hostil_se_sanea_y_capa_al_resolver() {
    let mut cfg = norte_config::ColumnsConfig::default();
    cfg.specs.insert("size".into(), norte_config::ColumnSpec {
        header: Some(format!("A\u{202E}{}", "x".repeat(60))),
        ..Default::default()
    });
    let s = ColumnsSettings::resolve(&cfg);
    let h = s.style_for("file", Builtin::Size).header.expect("header");
    assert!(!h.chars().any(norte_encoding::is_terminal_hazard));
    assert!(h.chars().count() <= 24);
}

#[test]
fn builtin_cell_honra_el_formato() {
    let e = /* Entry file con size=2048, mtime_ms conocido — copiar el
               builder de los tests existentes de builtin_cell */;
    let styled = ColumnStyle { size_format: SizeFormat::Exact, ..ColumnStyle::default_for(Builtin::Size) };
    assert_eq!(styled_cell(&e, Builtin::Size, 0, &styled).as_deref(), Some("2048"));
}
```

- [ ] **Step 2: Run** `cargo nextest run -p norte-frontend style_for` → compile failure.

- [ ] **Step 3: Implement**

```rust
/// Estilo RESUELTO de una columna (#108 7b): lo que el spec fija más los
/// defaults del builtin. Header YA saneado y capado (único choke point).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStyle {
    /// Formato de tamaño (solo lo lee Size).
    pub size_format: SizeFormat,
    /// Formato de tiempo (solo lo lee Mtime).
    pub time_format: TimeFormat,
    /// Alineación efectiva.
    pub align: Align,
    /// Cabecera propia (saneada, ≤ [`HEADER_MAX_CHARS`]); `None` = Fluent.
    pub header: Option<String>,
}

/// Tope de una cabecera custom (#108 7b).
pub const HEADER_MAX_CHARS: usize = 24;

impl ColumnStyle {
    /// Los defaults del builtin sin spec: iec/relative, derecha en las
    /// no-nombre (la convención que ya pintaban ambos frontends).
    #[must_use]
    pub fn default_for(b: Builtin) -> Self { /* Name→Left, resto Right */ }
}
```

`ColumnsSettings` gains a private `styles: BTreeMap<(Option<String>, String), ...>`-shaped store — concrete simplest form: keep `specs_global: BTreeMap<String, norte_config::ColumnSpec>` + `specs_schemes: BTreeMap<String, BTreeMap<String, ColumnSpec>>` copied at resolve, plus `pub bad_specs: Vec<String>` diagnostics; `style_for(scheme, builtin) -> ColumnStyle` folds default ← global spec ← scheme spec (per-field, `Some` wins), mapping format strings to the right enum per builtin and pushing a diagnostic (once per offending id) when the format doesn't fit (`iec|si|exact` only for Size, `relative|iso` only for Mtime, anything on Name/Kind = mismatch). Header: `sanitize_header` + char-cap 24 at resolve. Width: `layout_items_for` applies the spec width (global←scheme fold) to the `LayoutItem.policy` — except the NAME column keeps `is_name: true` and a Flex floor (a `fixed = 1` name would fight `NAME_MIN`; let `layout()`'s existing name-floor rules win, note in rustdoc). Also add `styled_cell(entry, col, now_ms, style) -> Option<String>` and reimplement `builtin_cell` as `styled_cell(entry, col, now_ms, &ColumnStyle::default_for(col))` — zero behavior change for existing callers, pinned by the existing tests.

Also: spec ids that don't parse as `ColumnId` → `bad_specs` diagnostic at resolve.

- [ ] **Step 4: Green + lint** — full `cargo nextest run -p norte-frontend; echo EXIT=$?` (existing builtin_cell/layout tests must not change), clippy, fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/columns.rs
git commit -m "feat(frontend): ColumnStyle — spec entries resolved into layout, cells, headers (#108 block 7b)

style_for folds builtin defaults <- global spec <- scheme spec per
field. Format/kind mismatches and unparseable spec ids become
diagnostics (bad_specs) with the default applied — never a silent skip,
never a startup failure. Custom headers are sanitized and capped at the
single resolve choke point. builtin_cell survives as the default-style
wrapper so no caller changes behavior.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: renderers honor style; doctor + schema golden

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (header line + `entry_item` cells)
- Modify: `crates/norte-gui/src/main.rs` (header row + `render_row` cells)
- Modify: `crates/norte-cli/src/doctor.rs` (`check_columns` :105)
- Modify: `docs/schema/norte.schema.json` (regen)
- Modify: `crates/norte-tui/tests/snapshots_ui.rs` (+1 snapshot)

- [ ] **Step 1: TUI** — `column_header_line` and `entry_item` take the settings (they already receive them or the widths; thread `&ColumnsSettings` + scheme where needed): header label = `style.header` (already sanitized) or the Fluent key; cell = `styled_cell(entry, col, now_ms, &settings.style_for(scheme, col))`; align: `Align::Left` pads right instead of left (mirror the existing right-align arithmetic — the width still INCLUDES the separator cell). Keep `column_widths` signature (it already consumes settings — the width override lands via `layout_items_for` from Task 2 for free).
- [ ] **Step 2: GUI** — same three touches in `render_pane` header loop and `render_row` cell loop: custom header label, `styled_cell`, `justify_start` vs `justify_end` from `style.align`. Widths again arrive free via `layout_items_for`.
- [ ] **Step 3: Doctor** — extend `check_columns` with `st.bad_specs` → code `columns-bad-spec`, `Severity::Warn`, masked+capped detail via the existing `sanitize_detail`; test beside the existing columns doctor test (doctor.rs:564).
- [ ] **Step 4: Snapshot** — new `snapshot_columns_spec_80x16` (or reuse the pane snapshot harness at 80x16): config with `size` spec `{format="si", header="Peso", width={fixed=9}}` + `kind` with `align="left"`; assert the rendered pane shows `Peso` in the header and an SI-formatted size; hostile header case is covered by the Task-2 unit test (resolve is the choke point) — no need to re-pin at render.
- [ ] **Step 5: Schema golden** — `NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui schema --features norte-tui/schema; echo EXIT=$?` then re-run WITHOUT the env var to confirm the golden matches (check the feature spelling against `just test`'s invocation at justfile:32). Inspect the schema diff: only additive `spec` surfaces.
- [ ] **Step 6: Gates** — `cargo nextest run -p norte-tui -p norte-cli; echo EXIT=$?`; `just gui-ci; echo EXIT=$?`; clippy workspace-touched crates; fmt.
- [ ] **Step 7: Commit**

```bash
git add crates/norte-tui crates/norte-gui crates/norte-cli docs/schema CHANGELOG.md
git commit -m "feat(tui,gui,cli): render [[ui.columns.spec]] — width/format/header/align live (#108 block 7b)

Both renderers read style_for at the same points they already read the
shared layout: custom headers (sanitized at resolve) replace the Fluent
label, cells go through styled_cell, align picks the padding side, and
width overrides arrive through layout_items_for with no renderer
change. doctor gains columns-bad-spec (masked, capped). Schema golden
regenerated — additive only.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

(CHANGELOG: one entry for the config surface + rendering, under unreleased.)

---

### Task 4: picker `f` — cycle format + persist

**Files:**
- Modify: `crates/norte-frontend/src/columns_picker.rs` (cycle + `Picked.formats`)
- Modify: `crates/norte-config/src/load.rs` (`persist_column_format` replace-by-id writer + tests)
- Modify: `crates/norte-config/src/lib.rs`, `crates/norte-tui/src/config.rs` (re-exports)
- Modify: `crates/norte-tui/src/keymap.rs` (`dialog.cycle-format` verb), presets ×3 (`f`), `crates/norte-i18n/i18n/{en,es}.ftl` (`dialog-cmd-cycle-format`)
- Modify: `crates/norte-tui/src/app.rs` (`ALLOW_COLUMNS` + verb), `main.rs` (`on_columns_key` arm + persist in `apply_picked_columns`), `ui.rs` (row shows the format), `hints.rs` (nothing — the verb joins the printed hint only if it fits; verify the 80-col budget like de2ea07 did, filter if not)
- Modify: `crates/norte-tui/tests/columns_picker.rs` (+1 e2e), `tests/snapshots_ui.rs` (picker snapshot updates if the row text changes)

- [ ] **Step 1: Model (TDD)** — tests first in `columns_picker.rs`:

```rust
#[test]
fn cycle_format_rota_el_vocabulario_de_la_columna() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.down(); // size (formato default iec)
    p.cycle_format();
    assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
    p.cycle_format();
    assert_eq!(p.format_of_cursor().as_deref(), Some("exact"));
    p.cycle_format();
    assert_eq!(p.format_of_cursor().as_deref(), Some("iec")); // vuelta completa
    // name/kind/opacos: no-op.
    p.up();
    p.cycle_format();
    assert_eq!(p.format_of_cursor(), None);
}

#[test]
fn finish_lleva_solo_los_formatos_cambiados() {
    let mut p = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    p.down();
    p.cycle_format(); // size → si
    let picked = p.finish();
    assert_eq!(picked.formats, vec![("size".to_owned(), "si".to_owned())]);
    // sin cambios → vacío
    let p2 = ColumnsPicker::open(&settings_vacios(), "file", SortSpec::default());
    assert!(p2.finish().formats.is_empty());
}
```

Implementation: `open` seeds each builtin row's current format from `settings.style_for(scheme, b)` (Size → its SizeFormat as `"iec"|"si"|"exact"`, Mtime → `"relative"|"iso"`); `cycle_format` rotates the row's vocab (Size: iec→si→exact→iec; Mtime: relative→iso→relative; others no-op); `format_of_cursor()` accessor; `Picked` gains `pub formats: Vec<(String, String)>` = rows whose format differs from the OPENING value (id, format).

- [ ] **Step 2: Writer (TDD in norte-config)** — `persist_column_format(dir, id: &str, format: &str) -> io::Result<PathBuf>`: read-or-create doc (same sanitized-error discipline), find `["ui"]["columns"]` table (same TableLike walk as `persist_columns` — factor the walk into a shared private helper now that there are two callers), then in its `spec` `ArrayOfTables` (create if absent — mirror `persist_hotlist_add`'s `or_insert_with(Item::ArrayOfTables)` :172 INCLUDING its shape guard) find the table whose `id` equals `id` and set `format`, or append `{ id, format }`. Tests: append-new, replace-existing-preserving-other-fields (seed an entry with `header = "Peso"` and assert it survives), round-trip through real `load` into `cfg.ui_columns.specs`, shape-guard error on `spec = 3`.
- [ ] **Step 3: Keymap surface** — `"dialog.cycle-format"` in `DIALOG_COMMANDS` + presets ×3 (`{ on = ["f"], run = "dialog.cycle-format" },` — FIRST verify `f` free in vim.toml/cua.toml `[dialog]`; if taken anywhere, pick the free key and record) + Fluent labels both locales + extend the 7a chord test (`columns_picker_chords_resuelven_via_adaptador_crossterm`) with `f`. `ALLOW_COLUMNS` += `dialog.cycle-format`. Check the printed hint still fits 80 cells with the new verb (the de2ea07 lesson — measure; filter from the PRINTED hint if it doesn't, keys still work).
- [ ] **Step 4: TUI wiring** — `on_columns_key`: `"dialog.cycle-format" => p.cycle_format()`. Row rendering in `draw_columns_picker`: append ` · {format}` to rows that HAVE a format (current value, plain ASCII vocab — no masking needed). `apply_picked_columns`: after `persist_columns`, persist each `picked.formats` entry via `spawn_blocking(persist_column_format)` (same error handling; one toast covers the lot — reuse `msg-columns-saved` on full success). In-memory: extend `ColumnsSettings::apply_picked` (or a sibling `apply_format(id, format)`) so the session sees the new format immediately — same lockstep rule as 7a.
- [ ] **Step 5: Tests** — e2e in `tests/columns_picker.rs`: open → down to size → `cycle_format` → confirm-path helpers → assert `style_for("file", Size).size_format == Si` in session AND the written `norte.toml` contains a `[[ui.columns.spec]]` (or inline equivalent) with `id = "size"`, `format = "si"`; re-`load` and assert `specs["size"].format == Some("si")`. Update the picker snapshot if row text gained ` · iec`.
- [ ] **Step 6: Gates + commit**

`cargo nextest run -p norte-config -p norte-frontend -p norte-tui; echo EXIT=$?`; clippy those three; fmt.

```bash
git add crates/norte-config crates/norte-frontend crates/norte-tui crates/norte-i18n CHANGELOG.md
git commit -m "feat(tui,frontend,config): picker cycles column format — f, persisted as spec entries (#108 block 7b)

f rotates the row's closed format vocabulary (size: iec/si/exact,
mtime: relative/iso; name/kind/opaque no-op) and finish() carries only
the CHANGED formats. persist_column_format writes a replace-by-id
[[ui.columns.spec]] entry preserving the entry's other fields (the
hotlist ArrayOfTables precedent, shared nested-walk helper). Width
cycling stays deferred: w is dialog.newer and a cycle without numeric
entry is noise — recorded deviation from Layer 6.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

(CHANGELOG: second entry for the picker key.)

---

### Task 5: gates, reviews, close

- [ ] **Step 1:** `just ci-fast; echo EXIT=$?` + `just gui-ci; echo EXIT=$?`.
- [ ] **Step 2:** rust-reviewer over the full 7b diff; encoding-auditor over: custom header path (resolve choke point + both renderers), spec-id round-trip through `persist_column_format`, doctor detail masking. Apply findings as `fix: apply the #108 block-7b review findings`.
- [ ] **Step 3:** #108 comment: 7b done; deferrals — width cycling in picker (needs numeric entry UX), per-scheme spec persistence (file-only), block 2 + 7c remain.
