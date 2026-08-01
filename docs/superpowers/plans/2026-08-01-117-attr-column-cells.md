# #117 — attr:/plugin: column cells in the pane — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render `attr:` column cells in both frontends (values already travel since #108 block 2), request the configured attr ids on listings, retire the `columns-no-renderer` doctor finding for `attr:`, and make the picker offer provider columns from the advertised catalog.

**Architecture:** Generalize the render funnel in `norte-frontend::columns` from `Builtin` to `ColumnId` (layout → widths → style → cell), with attr cells formatted by the **value's own tag** refined by the catalog's `AttrHint` (per ADR 0039: the value tag decides at render time; the hint only picks defaults). Frontends cache the `AttrCatalog` per scheme (one `fs.capabilities` per new scheme) and pass configured attr ids into `Backend::list_stream_with`/`list_with_skipped_attrs` (both already exist since block 2). `plugin:` columns stay out of the funnel and keep their doctor finding.

**Tech Stack:** Rust, ratatui (TUI), GPUI (GUI), Fluent i18n, existing `norte-proto` 0.30 attr types. **No proto change, no version bump.**

**Scope notes (decisions locked here):**
- **Sorted-but-hidden rule (spec Layer 7)** is vacuously satisfied today: the sort vocabulary (`SortColumnKey`) is closed to `name|size|mtime`, all of which live on the base `Entry`. No extra wire request is needed. Sorting by attr columns is NOT in this issue's scope — record that in the closing comment.
- `fs.stat` attr requests get no consumer here (cells come from listings; `refresh_panes` re-lists). Skip `stat_with` wiring.
- Attr default width is `Fixed(12)` uniformly; users tune via the existing `[[ui.columns.spec]]` width overrides, which already key by id string and ride the generalized funnel for free.
- `AttrValue::Unknown` renders as `"?"` (one bad cell costs one cell — never blank, blank means *absent*).
- `AttrValue::Bytes` renders through `norte_frontend::display_name` (lossy-with-U+FFFD) then the cell sanitizer; the hostile *badge* stays a name-cell convention, cells are text-only.

---

### Task 1: Model — `ColumnId`-typed funnel + attr cells + doctor retirement

**Files:**
- Modify: `crates/norte-frontend/src/columns.rs`
- Modify: `crates/norte-frontend/src/columns_picker.rs` (only if `style_for` call sites need the wrapper — they should not; `style_for(scheme, Builtin)` keeps its signature)
- Modify: `crates/norte-tui/src/ui.rs` (mechanical: tuple type changes, header labels via new helper, catalog param = `None`)
- Modify: `crates/norte-gui/src/main.rs` (same, mechanical)
- Modify: `crates/norte-cli/src/doctor.rs` (finding text + test id)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: inline `#[cfg(test)]` mods in `columns.rs`, existing tests in `ui.rs`/`doctor.rs`

- [ ] **Step 1.1: Write the failing model tests** (append to the existing test mods in `crates/norte-frontend/src/columns.rs`):

```rust
#[cfg(test)]
mod attr_funnel_tests {
    use super::*;
    use norte_proto::attrs::{AttrHint, AttrInfo, AttrType, AttrValue};

    fn catalog() -> norte_proto::AttrCatalog {
        norte_proto::AttrCatalog::new(vec![
            AttrInfo {
                id: "mem.mode".into(),
                label: "Mode".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
            AttrInfo {
                id: "mem.owner".into(),
                label: "Owner\u{202e}evil".into(),
                ty: AttrType::Bytes,
                hint: AttrHint::Identity,
            },
        ])
    }

    fn entry_with(attrs: &[(&str, AttrValue)]) -> norte_proto::Entry {
        let mut e = crate::pane::tests_entry_helper(); // if no helper exists, build the Entry literal used by the other columns tests
        for (k, v) in attrs {
            e.attrs.insert((*k).to_owned(), v.clone());
        }
        e
    }

    #[test]
    fn layout_items_for_incluye_attrs_y_deduplica() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.default_columns = Some(vec![
            "name".into(),
            "attr:mem.mode".into(),
            "attr:mem.mode".into(), // dup: una sola columna
            "size".into(),
        ]);
        let st = ColumnsSettings::resolve(&cfg);
        let items = st.layout_items_for("file");
        let ids: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(ids, vec!["name", "attr:mem.mode", "size"]);
        // attr default: Fixed(12), no-nombre.
        let attr = &items[1].1;
        assert_eq!(attr.policy, WidthPolicy::Fixed(12));
        assert!(!attr.is_name);
    }

    #[test]
    fn attr_ids_for_devuelve_los_configurados_del_scheme() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.default_columns = Some(vec!["name".into(), "attr:mem.mode".into()]);
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.attr_ids_for("file"), vec!["mem.mode".to_owned()]);
        // sin attrs configurados → vacío (no se paga el wire).
        let st2 = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert!(st2.attr_ids_for("file").is_empty());
    }

    #[test]
    fn attr_ya_no_es_unrenderable_plugin_si() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.default_columns = Some(vec![
            "name".into(),
            "attr:posix.mode".into(),
            "plugin:git/branch".into(),
        ]);
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.unrenderable, vec!["plugin:git/branch".to_owned()]);
    }

    #[test]
    fn styled_cell_attr_por_tag_del_valor_con_hint() {
        let cat = catalog();
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.default_columns = Some(vec!["name".into(), "attr:mem.mode".into()]);
        let st = ColumnsSettings::resolve(&cfg);
        let id = ColumnId::Attr("mem.mode".into());
        let style = st.style_for_id("file", &id, Some(&cat));
        assert_eq!(style.hint, AttrHint::Mode);
        assert_eq!(style.align, Align::Right);
        let e = entry_with(&[("mem.mode", AttrValue::Uint(0o100_644))]);
        assert_eq!(
            styled_cell(&e, &id, 0, &style).as_deref(),
            Some("-rw-r--r--")
        );
        // Ausente → None (blanco), jamás un valor fabricado.
        let vacio = entry_with(&[]);
        assert_eq!(styled_cell(&vacio, &id, 0, &style), None);
        // Unknown → "?" (una celda mala cuesta una celda).
        let raro = entry_with(&[("mem.mode", AttrValue::Unknown)]);
        assert_eq!(styled_cell(&raro, &id, 0, &style).as_deref(), Some("?"));
    }

    #[test]
    fn attr_text_y_bytes_hostiles_se_enmascaran() {
        let id = ColumnId::Attr("mem.note".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let e = entry_with(&[(
            "mem.note",
            AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()),
        )]);
        let cell = styled_cell(&e, &id, 0, &style).expect("celda");
        assert!(!cell.chars().any(norte_encoding::is_terminal_hazard), "{cell:?}");
        let id2 = ColumnId::Attr("mem.owner".into());
        let e2 = entry_with(&[("mem.owner", AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()))]);
        let cell2 = styled_cell(&e2, &id2, 0, &style).expect("celda");
        assert!(!cell2.chars().any(norte_encoding::is_terminal_hazard), "{cell2:?}");
        assert!(cell2.contains('\u{FFFD}'), "lossy marcado: {cell2:?}");
    }

    #[test]
    fn header_label_fluent_catalogo_o_id_enmascarado() {
        let cat = catalog();
        // Primera-parte: clave Fluent (existe col-attr-posix-mode).
        let id = ColumnId::Attr("posix.mode".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let h = header_label(&id, &style, None);
        assert_ne!(h, "col-attr-posix-mode", "clave Fluent debe existir");
        assert!(!h.starts_with("col-attr-"), "{h:?}");
        // Desconocido con catálogo: label del provider ENMASCARADO.
        let id2 = ColumnId::Attr("mem.owner".into());
        let h2 = header_label(&id2, &ColumnStyle::default_for_id(&id2, Some(&cat)), Some(&cat));
        assert!(!h2.chars().any(norte_encoding::is_terminal_hazard), "{h2:?}");
        // Desconocido sin catálogo: el id (charset seguro tras sanitize).
        let id3 = ColumnId::Attr("mem.stamp".into());
        let h3 = header_label(&id3, &ColumnStyle::default_for_id(&id3, None), None);
        assert_eq!(h3, "mem.stamp");
        // El header custom del spec GANA siempre.
        let mut st = ColumnStyle::default_for_id(&id3, None);
        st.header = Some("Custom".into());
        assert_eq!(header_label(&id3, &st, None), "Custom");
    }

    #[test]
    fn attr_cell_todos_los_tags() {
        let opaco = ColumnStyle::default_for_id(&ColumnId::Attr("x.y".into()), None);
        let e = |v: AttrValue| entry_with(&[("x.y", v)]);
        let id = ColumnId::Attr("x.y".into());
        assert_eq!(styled_cell(&e(AttrValue::Uint(42)), &id, 0, &opaco).as_deref(), Some("42"));
        assert_eq!(styled_cell(&e(AttrValue::Int(-5)), &id, 0, &opaco).as_deref(), Some("-5"));
        assert_eq!(styled_cell(&e(AttrValue::Bool(true)), &id, 0, &opaco), Some(norte_i18n::t("col-cell-yes")));
        // TimeMs siempre formatea como tiempo, con o sin hint.
        let t = styled_cell(&e(AttrValue::TimeMs(0)), &id, 60_000, &opaco).expect("celda");
        assert!(!t.is_empty());
    }
}
```

Note on `entry_with`: the existing columns tests build `norte_proto::Entry` literals (see `crates/norte-frontend/src/pane.rs` tests around line 1131 — `attrs: std::collections::BTreeMap::new()`). Copy that literal shape into a local `fn entry_with` in the test mod rather than referencing a helper that does not exist; the placeholder call above (`tests_entry_helper`) must be replaced with that literal.

- [ ] **Step 1.2: Run the tests to verify they fail**

Run: `cargo nextest run -p norte-frontend attr_funnel 2>/dev/null; echo EXIT=$?`
Expected: compile errors (missing `style_for_id`, `default_for_id`, `header_label`, `attr_ids_for`, `styled_cell` arity). That counts as the failing state.

- [ ] **Step 1.3: Implement the model changes in `crates/norte-frontend/src/columns.rs`**

1. **`ModeFormat` + table** (next to `SizeFormat`/`TimeFormat`, ~line 286):

```rust
/// Formato de un word de modo POSIX (#117).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFormat {
    /// `-rw-r--r--` (tipo + rwx, setuid/sticky incluidos).
    Rwx,
    /// Octal (`644`).
    Octal,
}
```

and next to `TIME_FORMATS` (~line 938):

```rust
/// Tabla str ↔ enum de columnas con hint `Mode` — mismas reglas que
/// [`SIZE_FORMATS`]. Las cadenas entran al vocabulario global de config
/// en la tarea 4 de #117.
const MODE_FORMATS: &[(&str, ModeFormat)] =
    &[("rwx", ModeFormat::Rwx), ("octal", ModeFormat::Octal)];
```

2. **`ColumnStyle` gains two fields** (~line 892). Update BOTH constructors and every literal construction (the `estilo()` helper in `ui.rs` tests uses struct literals — switch them to `..ColumnStyle::default_for(b)` spread):

```rust
    /// Formato de modo (solo lo leen celdas attr con hint `Mode`).
    pub mode_format: ModeFormat,
    /// Hint del catálogo para columnas attr (`Opaque` si no hay catálogo
    /// o la columna es builtin — los builtin no lo leen).
    pub hint: norte_proto::attrs::AttrHint,
```

`default_for(b)` adds `mode_format: ModeFormat::Rwx, hint: norte_proto::attrs::AttrHint::Opaque,`.

3. **`default_for_id`** on `impl ColumnStyle`:

```rust
    /// Defaults de CUALQUIER columna (#117): builtin = `default_for`;
    /// attr = alineación y hint del catálogo (`Opaque`/izquierda sin él,
    /// Size/Timestamp/Mode a la derecha); plugin = texto a la izquierda.
    #[must_use]
    pub fn default_for_id(id: &ColumnId, catalog: Option<&norte_proto::AttrCatalog>) -> Self {
        use norte_proto::attrs::AttrHint;
        match id {
            ColumnId::Builtin(b) => Self::default_for(*b),
            ColumnId::Attr(aid) => {
                let hint = catalog
                    .and_then(|c| c.iter().find(|i| i.id == *aid))
                    .map_or(AttrHint::Opaque, |i| i.hint);
                Self {
                    align: match hint {
                        AttrHint::Size | AttrHint::Timestamp | AttrHint::Mode => Align::Right,
                        _ => Align::Left,
                    },
                    hint,
                    ..Self::default_for(Builtin::Kind) // iec/relative/rwx, header None
                }
            }
            ColumnId::Plugin { .. } => Self {
                align: Align::Left,
                ..Self::default_for(Builtin::Kind)
            },
        }
    }
```

(`default_for(Builtin::Kind)` is just a source of the non-align defaults; its align is overridden. If that reads too cute, spell the struct out.)

4. **`style_for_id`** — generalize the fold; `style_for` becomes a wrapper (keeps picker/tests compiling):

```rust
    /// El estilo efectivo de CUALQUIER columna en `scheme` (#117):
    /// defaults ← spec global ← spec del scheme, campo a campo. El formato
    /// str se pliega por la tabla que lo contenga (size/time/mode) — en
    /// builtins resolve ya validó el encaje; en attrs el hint decide qué
    /// campo se LEE al pintar, así que plegar los tres es inocuo.
    #[must_use]
    pub fn style_for_id(
        &self,
        scheme: &str,
        id: &ColumnId,
        catalog: Option<&norte_proto::AttrCatalog>,
    ) -> ColumnStyle {
        let key = id.to_string();
        let mut style = ColumnStyle::default_for_id(id, catalog);
        let global = self.specs_global.get(&key);
        let scoped = self.specs_schemes.get(scheme).and_then(|m| m.get(&key));
        for spec in [global, scoped].into_iter().flatten() {
            if let Some(a) = spec.align {
                style.align = match a {
                    norte_config::AlignChoice::Left => Align::Left,
                    norte_config::AlignChoice::Right => Align::Right,
                };
            }
            if let Some(fmt) = spec.format.as_deref() {
                if let Some((_, f)) = SIZE_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.size_format = *f;
                } else if let Some((_, f)) = TIME_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.time_format = *f;
                } else if let Some((_, f)) = MODE_FORMATS.iter().find(|(s, _)| *s == fmt) {
                    style.mode_format = *f;
                }
            }
            if spec.header.is_some() {
                style.header.clone_from(&spec.header);
            }
        }
        style
    }

    /// [`Self::style_for_id`] para un builtin — la firma histórica (7b).
    #[must_use]
    pub fn style_for(&self, scheme: &str, builtin: Builtin) -> ColumnStyle {
        self.style_for_id(scheme, &ColumnId::Builtin(builtin), None)
    }
```

Delete the old `style_for` body (the per-builtin format match is subsumed).

**Guard rail:** builtins must not accept mode formats — that is already enforced at `resolve` time by `format_fits` (a `rwx` on `size` lands in `bad_specs` and the field is stripped), so the permissive fold above never sees it. Keep `format_fits` builtin-only in this task.

5. **`layout_items_for` → `Vec<(ColumnId, LayoutItem)>`** (~line 1221). Rewrite:

```rust
    /// Los items de layout para un pane en `scheme`, en orden de pintado
    /// (#117: builtins Y attrs; los `plugin:` configurados se SALTAN — sin
    /// renderer aún, doctor los nombra). Dedup por id; el nombre jamás
    /// desaparece ni deja de ir primero. El `width` de un
    /// `[[ui.columns.spec]]` (global ← scheme) SUSTITUYE la política del
    /// item — sobre cualquier columna, también attrs.
    #[must_use]
    pub fn layout_items_for(&self, scheme: &str) -> Vec<(ColumnId, LayoutItem)> {
        let ids = self
            .schemes
            .get(scheme)
            .and_then(|(c, _)| c.as_ref())
            .or(self.default_set.as_ref());
        let mut out = match ids {
            None => default_layout_items()
                .into_iter()
                .map(|(b, it)| (ColumnId::Builtin(b), it))
                .collect::<Vec<_>>(),
            Some(ids) => {
                let mut out: Vec<(ColumnId, LayoutItem)> = Vec::new();
                for id in ids {
                    match id {
                        ColumnId::Plugin { .. } => continue,
                        _ if out.iter().any(|(x, _)| x == id) => continue,
                        ColumnId::Builtin(b) => out.push((id.clone(), builtin_layout_item(*b))),
                        ColumnId::Attr(_) => out.push((id.clone(), attr_layout_item())),
                    }
                }
                let name = ColumnId::Builtin(Builtin::Name);
                match out.iter().position(|(id, _)| *id == name) {
                    Some(pos) if pos > 0 => {
                        let n = out.remove(pos);
                        out.insert(0, n);
                    }
                    Some(_) => {}
                    None => out.insert(0, (name, builtin_layout_item(Builtin::Name))),
                }
                out
            }
        };
        self.apply_width_overrides(scheme, &mut out);
        out
    }
```

`apply_width_overrides` changes its signature to `items: &mut [(ColumnId, LayoutItem)]` and its key line to `let key = id.to_string();` — the body is otherwise unchanged. Add:

```rust
/// Item de layout por defecto de una columna attr (#117): `Fixed(12)`
/// (separador incluido) — el ancho fino se ajusta con el width override
/// del spec, que llega gratis por `apply_width_overrides`.
#[must_use]
pub fn attr_layout_item() -> LayoutItem {
    LayoutItem {
        policy: WidthPolicy::Fixed(12),
        measured: 0,
        is_name: false,
    }
}
```

6. **`column_widths` → `Vec<(ColumnId, u16)>`** — only the tuple type changes; body identical.

7. **`sort_column_id`** next to `sort_column`:

```rust
/// [`sort_column`] para cualquier id: attr/plugin no son ordenables (el
/// vocabulario de sort es cerrado: name/size/mtime — spec Layer 7 nota
/// #117).
#[must_use]
pub fn sort_column_id(id: &ColumnId) -> Option<crate::sort::SortColumn> {
    match id {
        ColumnId::Builtin(b) => sort_column(*b),
        ColumnId::Attr(_) | ColumnId::Plugin { .. } => None,
    }
}
```

8. **`styled_cell` takes `&ColumnId`**; `builtin_cell` keeps its signature:

```rust
#[must_use]
pub fn styled_cell(
    entry: &norte_proto::Entry,
    col: &ColumnId,
    now_ms: i64,
    style: &ColumnStyle,
) -> Option<String> {
    match col {
        ColumnId::Builtin(b) => match b {
            Builtin::Name => None, // el nombre lo pinta el frontend
            Builtin::Kind => Some(norte_i18n::t(match entry.kind {
                norte_proto::EntryKind::Dir => "col-kind-dir",
                norte_proto::EntryKind::File => "col-kind-file",
                norte_proto::EntryKind::Symlink => "col-kind-symlink",
                norte_proto::EntryKind::Other => "col-kind-other",
            })),
            Builtin::Size => entry.size.map(|n| format_size(n, style.size_format)),
            Builtin::Mtime => entry
                .mtime_ms
                .map(|ms| format_mtime(ms, style.time_format, now_ms)),
        },
        ColumnId::Attr(id) => entry.attrs.get(id).and_then(|v| attr_cell(v, style, now_ms)),
        ColumnId::Plugin { .. } => None,
    }
}

#[must_use]
pub fn builtin_cell(entry: &norte_proto::Entry, col: Builtin, now_ms: i64) -> Option<String> {
    styled_cell(entry, &ColumnId::Builtin(col), now_ms, &ColumnStyle::default_for(col))
}
```

9. **`attr_cell`** (private) + `format_mode`:

```rust
/// Celda de un valor attr (#117): el TAG del valor decide (ADR 0039 §1 —
/// jamás coaccionado al tipo declarado); el hint del estilo refina los
/// numéricos. Text/Bytes son de TERCEROS: enmascarados y capados por
/// [`sanitize_cell`]; Bytes pasa antes por el lossy MARCADO de
/// [`crate::display_name`] (regla 1: los bytes originales no se tocan).
fn attr_cell(v: &norte_proto::AttrValue, style: &ColumnStyle, now_ms: i64) -> Option<String> {
    use norte_proto::attrs::{AttrHint, AttrValue};
    match v {
        AttrValue::Uint(n) => Some(match style.hint {
            AttrHint::Size => format_size(*n, style.size_format),
            AttrHint::Mode => format_mode(*n, style.mode_format),
            AttrHint::Timestamp => i64::try_from(*n)
                .map_or_else(|_| n.to_string(), |ms| format_mtime(ms, style.time_format, now_ms)),
            _ => n.to_string(),
        }),
        AttrValue::Int(i) => Some(match style.hint {
            AttrHint::Timestamp => format_mtime(*i, style.time_format, now_ms),
            _ => i.to_string(),
        }),
        AttrValue::TimeMs(ms) => Some(format_mtime(*ms, style.time_format, now_ms)),
        AttrValue::Text(s) => sanitize_cell(Some(s)),
        AttrValue::Bytes(b) => {
            let (shown, _hostil) = crate::display_name(b);
            sanitize_cell(Some(&shown))
        }
        AttrValue::Bool(b) => Some(norte_i18n::t(if *b { "col-cell-yes" } else { "col-cell-no" })),
        // Una celda mala cuesta una celda: visible, jamás blanco (blanco =
        // AUSENTE).
        AttrValue::Unknown => Some("?".to_owned()),
    }
}

/// Modo POSIX según formato. Un valor que no cabe en u32 no es un modo:
/// decimal crudo, jamás un panic ni un truncado silencioso.
fn format_mode(n: u64, fmt: ModeFormat) -> String {
    match u32::try_from(n) {
        Ok(m) => match fmt {
            ModeFormat::Rwx => format_mode_rwx(m),
            ModeFormat::Octal => format_mode_octal(m),
        },
        Err(_) => n.to_string(),
    }
}
```

(Check the existing `format_mode_rwx(0o100_644)` output in its tests — the assertion in Step 1.1 (`-rw-r--r--`) must match what the shipped formatter produces; adjust the test literal to the real output, not the formatter to the test.)

10. **`header_label`** (shared by both frontends — replaces the TUI-local match):

```rust
/// Etiqueta de cabecera de CUALQUIER columna (#117), compartida TUI/GUI:
/// el `header` custom del spec gana (YA saneado al resolver); builtin →
/// Fluent; attr → Fluent por id de primera parte (`col-attr-posix-mode`),
/// si no el label del catálogo ENMASCARADO, si no el id saneado. `t()`
/// devuelve la clave cuando falta: se detecta comparando.
#[must_use]
pub fn header_label(
    id: &ColumnId,
    style: &ColumnStyle,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> String {
    if let Some(h) = &style.header {
        return h.clone();
    }
    match id {
        ColumnId::Builtin(b) => norte_i18n::t(match b {
            Builtin::Name => "col-header-name",
            Builtin::Size => "col-header-size",
            Builtin::Mtime => "col-header-mtime",
            Builtin::Kind => "col-header-kind",
        }),
        ColumnId::Attr(aid) => {
            let key = format!("col-attr-{}", aid.replace(['.', '_'], "-"));
            let loc = norte_i18n::t(&key);
            if loc != key {
                return loc;
            }
            if let Some(info) = catalog.and_then(|c| c.iter().find(|i| i.id == *aid)) {
                let sane: String = sanitize_header(&info.label)
                    .chars()
                    .take(HEADER_MAX_CHARS)
                    .collect();
                if !sane.is_empty() {
                    return sane;
                }
            }
            sanitize_header(aid).chars().take(HEADER_MAX_CHARS).collect()
        }
        ColumnId::Plugin { plugin, column } => sanitize_header(&format!("{plugin}/{column}"))
            .chars()
            .take(HEADER_MAX_CHARS)
            .collect(),
    }
}
```

11. **`attr_ids_for`** on `impl ColumnsSettings`:

```rust
    /// Los ids attr CONFIGURADOS y renderizables de `scheme` (#117): lo
    /// que el pane pide en `fs.list`. Dedup del funnel; capado a
    /// [`norte_proto::ATTRS_MAX_REQUEST`] (el daemon rechazaría más).
    #[must_use]
    pub fn attr_ids_for(&self, scheme: &str) -> Vec<String> {
        self.layout_items_for(scheme)
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Attr(a) => Some(a),
                _ => None,
            })
            .take(norte_proto::ATTRS_MAX_REQUEST)
            .collect()
    }
```

12. **`collect_diagnostics`**: the `Ok(ColumnId::Attr(_) | ColumnId::Plugin { .. })` arm splits — `Attr` becomes renderable:

```rust
                Ok(ColumnId::Plugin { .. }) => {
                    if !self.unrenderable.contains(raw) {
                        self.unrenderable.push(raw.clone());
                    }
                }
                Ok(ColumnId::Builtin(_) | ColumnId::Attr(_)) => {}
```

Update the rustdoc of `ColumnsSettings` (`unrenderable` now = `plugin:` only) and of `sanitize_specs` (its behavior is unchanged — attr formats still pass through; the guard is that only `MODE_FORMATS`/`SIZE_FORMATS`/`TIME_FORMATS` words fold, unknown words keep defaults).

13. **`format_pinned_by_scheme_id`** generic + wrapper (the picker needs it in Task 4; adding it now keeps this task the only one touching this file's core):

```rust
    /// [`Self::format_pinned_by_scheme`] para cualquier id (#117).
    #[must_use]
    pub fn format_pinned_by_scheme_id(&self, scheme: &str, id: &ColumnId) -> bool {
        let key = id.to_string();
        self.specs_schemes
            .get(scheme)
            .and_then(|m| m.get(&key))
            .is_some_and(|sp| sp.format.is_some())
    }

    #[must_use]
    pub fn format_pinned_by_scheme(&self, scheme: &str, builtin: Builtin) -> bool {
        self.format_pinned_by_scheme_id(scheme, &ColumnId::Builtin(builtin))
    }
```

Check `norte-frontend/Cargo.toml` — `norte-proto` is already a dependency (Entry is used); `norte-encoding` is used by tests (`is_terminal_hazard`) — it is already a dependency of norte-frontend (display.rs uses it); if only a dev-dependency is missing, add it.

- [ ] **Step 1.4: Add the Fluent keys** to `crates/norte-i18n/i18n/en.ftl` and `es.ftl` (there is a message-parity test — both files or it fails). Place next to the existing `col-header-*` block:

```ftl
# en.ftl
col-cell-yes = yes
col-cell-no = no
col-attr-posix-mode = Mode
col-attr-posix-uid = UID
col-attr-posix-gid = GID
col-attr-posix-nlink = Links
col-attr-posix-ctime-ms = Changed
col-attr-win-attributes = Attributes
col-attr-s3-etag = ETag
col-attr-s3-content-type = Content type
col-attr-archive-method = Method
col-attr-archive-packed-size = Packed
col-attr-archive-crc32 = CRC-32
```

```ftl
# es.ftl
col-cell-yes = sí
col-cell-no = no
col-attr-posix-mode = Modo
col-attr-posix-uid = UID
col-attr-posix-gid = GID
col-attr-posix-nlink = Enlaces
col-attr-posix-ctime-ms = Cambiado
col-attr-win-attributes = Atributos
col-attr-s3-etag = ETag
col-attr-s3-content-type = Tipo de contenido
col-attr-archive-method = Método
col-attr-archive-packed-size = Comprimido
col-attr-archive-crc32 = CRC-32
```

- [ ] **Step 1.5: Mechanical frontend updates (compile fixes only — real wiring is Tasks 2/3).**

**TUI `crates/norte-tui/src/ui.rs`:**
- `styled_columns` returns `Vec<(ColumnId, u16, ColumnStyle)>` and maps with `settings.style_for_id(scheme, &id, None)` (borrow: `column_widths` yields owned `ColumnId`, so `.map(|(id, w)| { let s = settings.style_for_id(scheme, &id, None); (id, w, s) })`).
- `column_header_line` takes `&[(ColumnId, u16, ColumnStyle)]`; the label match is replaced by `let label = norte_frontend::columns::header_label(col, style, None);` and `activa` by `sort_column_id(col) == Some(sort.column)`.
- The cell loop: `styled_cell(entry, col, now_ms, style)` (col is now `&ColumnId`).
- The `estilo()` test helper and `column_header_line_tests`: construct styles via `ColumnStyle { align, header, ..ColumnStyle::default_for(b) }` and columns as `ColumnId::Builtin(b)`.
- Picker row label (~line 643): the `match r.builtin` on `None` currently falls back to the raw id — leave as-is in this task (Task 4 adds `label`).

**GUI `crates/norte-gui/src/main.rs`:**
- The `column_widths` consumption (~line 2315) and cell paint (~line 2829): same tuple/borrow changes, `style_for_id(scheme, &id, None)`, `styled_cell(entry, col, now_ms, style)`.
- Header sortability (~line 2548): `sort_column_id(col)`.
- Header label: replace whatever per-Builtin match exists with `header_label(col, style, None)`.

**Doctor `crates/norte-cli/src/doctor.rs`:**
- Finding text (~line 127) becomes plugin-only:

```rust
            detail: format!(
                "[ui.columns] id válido sin renderer aún (las celdas de plugin llegan con el wiring de columnas de plugins, #117): {}",
                sanitize_detail(raw)
            ),
```

- The test at ~line 584: if its fixture uses an `attr:` id, change it to `plugin:demo/x` (attr ids no longer produce the finding); add an assertion that an `attr:` id does NOT produce `columns-no-renderer`.

- [ ] **Step 1.6: Run the affected suites**

Run:
```sh
cargo nextest run -p norte-frontend 2>/dev/null; echo EXIT=$?
cargo nextest run -p norte-tui 2>/dev/null; echo EXIT=$?
cargo nextest run -p norte-cli doctor 2>/dev/null; echo EXIT=$?
cargo clippy -p norte-frontend -p norte-tui -p norte-cli --all-targets 2>/dev/null; echo EXIT=$?
just check-gui; echo EXIT=$?
```
Expected: all `EXIT=0`. **Capture the exit code — never pipe clippy through `tail`** (recorded process lesson: pipes swallow the exit).

- [ ] **Step 1.7: Commit**

```bash
git add -A
git commit -m "feat(frontend): ColumnId render funnel + attr cells (#117)"
```

---

### Task 2: TUI — request attrs on listings, cache the catalog, render live values

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (catalog cache + accessor)
- Modify: `crates/norte-tui/src/main.rs` (`first_page`, `listing`, cd flow, picker-apply refresh)
- Modify: `crates/norte-tui/src/ui.rs` (pass the real catalog)
- Test: rendered-buffer hostile test in `ui.rs` tests (or the TUI's snapshot test mod, wherever the block-5 render tests live — find with `grep -rn "col-header-name\|render" crates/norte-tui/src/ui.rs | grep test`)

- [x] **Step 2.1: Write the failing render test** (in the same test mod as the block-5 rendered-buffer tests; mirror their harness — they build a `Pane` from `Entry` literals and render to a `ratatui` buffer):

```rust
#[test]
fn celdas_attr_hostiles_enmascaradas_y_ausencia_en_blanco() {
    use norte_proto::attrs::AttrValue;
    // Config: name + attr:mem.owner (Bytes no-UTF8) + attr:mem.note (RTL+ZWJ).
    let mut cfg = norte_config::ColumnsConfig::default();
    cfg.default_columns = Some(vec![
        "name".into(),
        "attr:mem.owner".into(),
        "attr:mem.note".into(),
    ]);
    let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    let mut e1 = entry("aaa"); // el constructor de entries del harness existente
    e1.attrs.insert("mem.owner".into(), AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()));
    e1.attrs.insert("mem.note".into(), AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()));
    let e2 = entry("bbb"); // SIN attrs: celdas en blanco
    // ... render con el harness del bloque 5 (draw_pane a un TestBackend) ...
    // Asserts sobre el buffer:
    // 1. ninguna celda contiene chars de norte_encoding::is_terminal_hazard
    // 2. la fila de e1 contiene '\u{FFFD}' (lossy MARCADO del owner)
    // 3. la fila de e2 pinta blancos en las columnas attr (ausencia)
    // 4. la cabecera contiene "mem.owner" (fallback id, sin catálogo en test)
}
```

(The exact harness calls come from the neighboring block-5 tests — reuse their `TestBackend`/buffer-scan helpers verbatim.)

- [x] **Step 2.2: Run it to verify it fails**

Run: `cargo nextest run -p norte-tui celdas_attr 2>/dev/null; echo EXIT=$?`
Expected: FAIL — the pane never received the attrs → blank cells everywhere (assert 2 fails), because rendering works since Task 1 but nothing requests attrs. If it fails to compile because `draw_pane` lacks the catalog param, that also counts.

- [x] **Step 2.3: Implement**

1. **`App` cache** (`crates/norte-tui/src/app.rs`): add field + accessor + setter:

```rust
    /// Catálogo de attrs por SCHEME (#117): una llamada a
    /// `fs.capabilities` por scheme nuevo y por sesión; alimenta hints y
    /// cabeceras del render y las filas del picker (tarea 4).
    pub attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>,
```

(init `attr_catalogs: std::collections::HashMap::new(),` wherever `App` is constructed) plus:

```rust
    /// El catálogo cacheado del scheme del pane enfocado, si llegó.
    #[must_use]
    pub fn attr_catalog(&self, scheme: &str) -> Option<&norte_proto::AttrCatalog> {
        self.attr_catalogs.get(scheme)
    }
```

2. **`first_page` and `listing` gain params** (`crates/norte-tui/src/main.rs` ~4121):

```rust
async fn listing(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
) -> Result<(Vec<Entry>, Option<u64>), Error> {
    backend.list_with_skipped_attrs(dir, attrs).await
}

async fn first_page(
    backend: &Backend,
    dir: &VPath,
    attrs: &[String],
    fetch_catalog: bool,
) -> Result<(Vec<Entry>, Option<EntryStream>, Option<u64>, Option<norte_proto::AttrCatalog>), Error>
{
    // El catálogo ANTES del stream (misma conexión, una vez por scheme);
    // un fallo del catálogo NO tumba el cd: sin hints se pinta Opaque.
    let catalog = if fetch_catalog {
        backend.attr_catalog(dir).await.ok()
    } else {
        None
    };
    let (mut stream, skipped) = backend.list_stream_with(dir, attrs).await?;
    let mut first = Vec::with_capacity(FIRST_PAGE);
    while first.len() < FIRST_PAGE {
        match stream.next().await {
            Some(item) => first.push(item?),
            None => return Ok((first, None, skipped, catalog)),
        }
    }
    Ok((first, Some(stream), skipped, catalog))
}
```

3. **Call sites**: `grep -n "first_page(\|listing(" crates/norte-tui/src/main.rs`. At each cd initiation the caller computes, BEFORE spawning:

```rust
let scheme = dir.scheme().to_owned();
let attrs = app.columns.attr_ids_for(&scheme);
let fetch_catalog = !app.attr_catalogs.contains_key(&scheme);
```

and threads `attrs`/`fetch_catalog` into the spawned future. The cd-result handling (where `Cd::Filling`/first page lands, ~4233) stores the returned catalog:

```rust
if let Some(cat) = catalog {
    app.attr_catalogs.insert(scheme.clone(), cat);
}
```

`refresh_panes` (~2876) passes `app.columns.attr_ids_for(pane_scheme)` to `listing` per pane.

4. **Render pass-through**: `draw_pane` gains `catalog: Option<&norte_proto::AttrCatalog>`; its caller passes `app.attr_catalog(pane.dir().scheme())`. `styled_columns` and `column_header_line` receive it instead of the Task-1 `None`.

5. **Picker-apply refresh**: `apply_picked_columns` (~1626) computes the focused pane's `attr_ids_for` before and after `app.columns.apply_picked(...)`; if changed, the caller triggers the existing refresh path (the call site at ~1614 is inside the run loop where `backend`/`events` are in scope — call `refresh_panes(app, backend, events).await` exactly as the post-mutation flow does). Simplest shape: make `apply_picked_columns` return `bool` (needs_refresh) and act at the call site.

- [x] **Step 2.4: Run the tests**

Run:
```sh
cargo nextest run -p norte-tui 2>/dev/null; echo EXIT=$?
cargo clippy -p norte-tui --all-targets -- -D warnings 2>/dev/null; echo EXIT=$?
```
Expected: `EXIT=0` both. The Step-2.1 test must now pass — the harness renders entries whose `attrs` maps are populated by the test itself (no daemon in unit tests; the request-path change is covered by compile + the existing block-2 Backend tests).

- [x] **Step 2.5: Commit**

```bash
git add -A
git commit -m "feat(tui): request configured attrs + per-scheme catalog cache (#117)"
```

---

### Task 3: GUI — same wiring through the session worker

**Files:**
- Modify: `crates/norte-gui/src/session.rs` (List carries attrs; catalog event)
- Modify: `crates/norte-gui/src/main.rs` (cache + pass catalog to render; List senders include attrs; picker-apply refresh)

- [x] **Step 3.1: Extend the session protocol** (`crates/norte-gui/src/session.rs`):

`SessionCmd::List` gains `attrs: Vec<String>` and `fetch_catalog: bool`; `SessionEvent` gains:

```rust
    /// Catálogo de attrs del scheme (#117): una vez por scheme y sesión.
    AttrCatalog {
        scheme: String,
        catalog: norte_proto::AttrCatalog,
    },
```

Worker handling (~line 365):

```rust
                    SessionCmd::List { pane, generation, dir, attrs, fetch_catalog } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            if fetch_catalog {
                                if let Ok(catalog) = backend.attr_catalog(&dir).await {
                                    let _ = tx.send(SessionEvent::AttrCatalog {
                                        scheme: dir.scheme().to_owned(),
                                        catalog,
                                    });
                                }
                            }
                            let outcome = backend
                                .list_with_skipped_attrs(&dir, &attrs)
                                .await
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::Listed { pane, generation, dir, outcome });
                        });
                    }
```

- [x] **Step 3.2: Wire the app side** (`crates/norte-gui/src/main.rs`):

- Field `attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>` (init at both construction sites, ~550/632 pattern from `columns_picker: None`).
- Every `SessionCmd::List` sender adds `attrs: self.column_settings.attr_ids_for(scheme)` and `fetch_catalog: !self.attr_catalogs.contains_key(scheme)` (find senders: `grep -n "SessionCmd::List" crates/norte-gui/src/main.rs`).
- The `SessionEvent` handler match adds `AttrCatalog { scheme, catalog } => { self.attr_catalogs.insert(scheme, catalog); cx.notify(); }`.
- The render paths from Task 1 replace their `None` catalog with `self.attr_catalogs.get(scheme)`.
- Picker apply (`apply_picked` handler ~1728): if the focused pane's `attr_ids_for` changed across the apply, re-send `SessionCmd::List` for the affected panes (same generation bump as a manual reload).

- [x] **Step 3.3: GUI test** — extend the columns render test in `main.rs`/`columns_view.rs` test mod (wherever block-6 cell tests live; `grep -n "styled_cell\|column_widths" crates/norte-gui/src/main.rs | grep -i test`): an `Entry` with a hostile `Text` attr renders masked and an entry without the attr renders blank — same assertions as the TUI test, over the GUI's cell-string builder (the GUI cell path is pure string code; no window needed).

- [x] **Step 3.4: Run the gates**

Run: `just check-gui; echo EXIT=$?` then `just gui-ci; echo EXIT=$?` (or the GUI test invocation the justfile uses).
Expected: `EXIT=0` both.

- [x] **Step 3.5: Commit**

```bash
git add -A
git commit -m "feat(gui): request configured attrs + catalog via session events (#117)"
```

---

### Task 4: Picker — catalog-aware rows + attr format cycling + config vocab

**Files:**
- Modify: `crates/norte-frontend/src/columns_picker.rs`
- Modify: `crates/norte-frontend/src/columns.rs` (format cycling by hint)
- Modify: `crates/norte-config/src/load.rs` (~line 943: format vocab)
- Modify: `docs/schema/norte.schema.json` (regenerated)
- Modify: `crates/norte-tui/src/app.rs` (`open_columns_picker` passes catalog), `crates/norte-tui/src/ui.rs` (attr row labels)
- Modify: `crates/norte-gui/src/main.rs` (`open_columns_picker`), `crates/norte-gui/src/columns_view.rs` (row labels)

- [ ] **Step 4.1: Failing picker tests** (in `columns_picker.rs` tests):

```rust
#[test]
fn open_with_catalog_ofrece_attrs_no_configurados() {
    use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
    let cat = norte_proto::AttrCatalog::new(vec![
        AttrInfo { id: "posix.mode".into(), label: "Mode".into(), ty: AttrType::Uint, hint: AttrHint::Mode },
        AttrInfo { id: "posix.uid".into(), label: "UID".into(), ty: AttrType::Uint, hint: AttrHint::Identity },
    ]);
    let mut cfg = norte_config::ColumnsConfig::default();
    cfg.default_columns = Some(vec!["name".into(), "attr:posix.mode".into()]);
    let st = ColumnsSettings::resolve(&cfg);
    let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat));
    // El configurado sigue habilitado; el anunciado no-configurado aparece
    // deshabilitado al final, UNA sola vez.
    let uid: Vec<_> = p.rows().iter().filter(|r| r.id == "attr:posix.uid").collect();
    assert_eq!(uid.len(), 1);
    assert!(!uid[0].enabled);
    assert_eq!(p.rows().iter().filter(|r| r.id == "attr:posix.mode").count(), 1);
    // La fila attr con hint Mode cicla formato: rwx → octal.
    let modo = p.rows().iter().position(|r| r.id == "attr:posix.mode").unwrap();
    assert_eq!(p.rows()[modo].format.as_deref(), Some("rwx"));
}

#[test]
fn open_sin_catalogo_conserva_la_conducta_historica() {
    let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
    let a = ColumnsPicker::open(&st, "file", SortSpec::default());
    let b = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None);
    assert_eq!(a.rows(), b.rows());
}
```

Run: `cargo nextest run -p norte-frontend open_with_catalog 2>/dev/null; echo EXIT=$?` — expect compile failure (no `open_with_catalog`).

- [ ] **Step 4.2: Model support in `columns.rs`** — format cycling by hint:

```rust
/// El siguiente formato del ciclo del picker para CUALQUIER columna
/// (#117): builtin por su tabla; attr por la tabla de su hint (Size/
/// Timestamp/Mode); el resto no admite formato.
#[must_use]
pub fn next_format_id(
    id: &ColumnId,
    hint: norte_proto::attrs::AttrHint,
    current: &str,
) -> Option<&'static str> {
    use norte_proto::attrs::AttrHint;
    fn advance<T>(tab: &'static [(&'static str, T)], cur: &str) -> Option<&'static str> {
        let i = tab.iter().position(|(s, _)| *s == cur)?;
        Some(tab[(i + 1) % tab.len()].0)
    }
    match id {
        ColumnId::Builtin(b) => next_format(*b, current),
        ColumnId::Attr(_) => match hint {
            AttrHint::Size => advance(SIZE_FORMATS, current),
            AttrHint::Timestamp => advance(TIME_FORMATS, current),
            AttrHint::Mode => advance(MODE_FORMATS, current),
            _ => None,
        },
        ColumnId::Plugin { .. } => None,
    }
}

/// El nombre-str del formato vigente para cualquier columna (#117) — seed
/// del picker, dirección enum → str, por el hint en attrs.
#[must_use]
pub fn format_name_id(
    id: &ColumnId,
    hint: norte_proto::attrs::AttrHint,
    style: &ColumnStyle,
) -> Option<&'static str> {
    use norte_proto::attrs::AttrHint;
    match id {
        ColumnId::Builtin(b) => format_name(*b, style),
        ColumnId::Attr(_) => match hint {
            AttrHint::Size => SIZE_FORMATS.iter().find(|(_, f)| *f == style.size_format).map(|(s, _)| *s),
            AttrHint::Timestamp => TIME_FORMATS.iter().find(|(_, f)| *f == style.time_format).map(|(s, _)| *s),
            AttrHint::Mode => MODE_FORMATS.iter().find(|(_, f)| *f == style.mode_format).map(|(s, _)| *s),
            _ => None,
        },
        ColumnId::Plugin { .. } => None,
    }
}
```

- [ ] **Step 4.3: Picker changes** (`columns_picker.rs`):

- `PickerRow` gains two pub fields (update every literal):

```rust
    /// Etiqueta de display para filas NO builtin (#117): la de
    /// `header_label` al abrir (localizada / label del catálogo YA
    /// enmascarado / id saneado). `None` en builtins (Fluent en vivo).
    pub label: Option<String>,
    /// Hint del catálogo al abrir (attrs): decide la tabla del ciclo de
    /// formato. `Opaque` = sin ciclo.
    pub hint: norte_proto::attrs::AttrHint,
```

- `make_row` gains `catalog: Option<&norte_proto::AttrCatalog>` and computes, for a parsed `ColumnId` (parse once at the top: `let parsed = id.parse::<ColumnId>().ok();`):
  - `hint`: from catalog lookup for `ColumnId::Attr`, else `Opaque`;
  - `format`: `parsed.and_then(|cid| format_name_id(&cid, hint, &settings.style_for_id(scheme, &cid, catalog))).map(str::to_owned)` — replacing the builtin-only seed;
  - `format_locked`: `settings.format_pinned_by_scheme_id(scheme, &cid)` for any parsed id;
  - `label`: `matches!(parsed, Some(ColumnId::Attr(_)) | Some(ColumnId::Plugin{..}))` → `Some(header_label(&cid, &style_sin_header, catalog))` where `style_sin_header` is the resolved style with `header: None` forced when the resolved header equals the raw id (keep it simple: always `header_label(&cid, &settings.style_for_id(scheme, &cid, catalog), catalog)`).
- `open` delegates: `pub fn open(...) -> Self { Self::open_with_catalog(settings, scheme, current_sort, None) }`.
- `open_with_catalog(settings, scheme, current_sort, catalog: Option<&norte_proto::AttrCatalog>)`: existing body with `make_row(..., catalog)` everywhere, plus after the builtin-catalog loop:

```rust
        // Catálogo de PROVIDER (#117): attrs anunciados y no configurados,
        // deshabilitados, tras los builtins — el picker OFRECE, no impone.
        if let Some(cat) = catalog {
            for info in cat {
                let id = format!("attr:{}", info.id);
                if !rows.iter().any(|r| r.id == id) {
                    rows.push(make_row(id, None, false, settings, scheme, catalog));
                }
            }
        }
```

  (`make_row`'s `builtin` param stays `Option<Builtin>` — attr rows pass `None` exactly like opaque rows today; the parse inside `make_row` distinguishes them.)
- `cycle_format`: replace the `builtin`-gated `next_format` call with `next_format_id(&parsed_id, row.hint, current)` (parse `row.id`; unparsable rows keep no-op). `format_locked` gate unchanged.
- `sort_current` / anything using `r.builtin` for sortability: unchanged (attr rows are not sortable — `sort_column_id` returns `None`).

- [ ] **Step 4.4: Config vocabulary + schema** (`crates/norte-config/src/load.rs` ~943):

```rust
            Some(f @ ("exact" | "iec" | "si" | "relative" | "iso" | "octal" | "rwx")) => Some(f.to_owned()),
```

Run `cargo nextest run -p norte-config 2>/dev/null; echo EXIT=$?` — if the `norte.schema.json` golden covers the format vocab it will fail with regeneration instructions; follow them (the golden lives at `docs/schema/norte.schema.json`). Also confirm the frontend pin test that `SIZE_FORMATS`/`TIME_FORMATS`/`MODE_FORMATS` strings are a subset of the config vocabulary — extend it to include `MODE_FORMATS` (it lives in `columns.rs` tests; grep `vocabulario`).

- [ ] **Step 4.5: Frontend picker call sites:**

- TUI `app.rs::open_columns_picker` (~1437): 

```rust
    pub fn open_columns_picker(&mut self) {
        let scheme = self.focused().dir().scheme().to_owned();
        let sort = self.focused().sort();
        let catalog = self.attr_catalogs.get(&scheme);
        self.columns_picker = Some(norte_frontend::columns_picker::ColumnsPicker::open_with_catalog(
            &self.columns,
            &scheme,
            sort,
            catalog,
        ));
    }
```

- TUI `ui.rs` picker row rendering (~643): non-builtin rows use `r.label.as_deref().unwrap_or(&r.id)` for display (the label was masked at construction; the id fallback is what today's code shows). `f`-cycle hint line: rows with `format.is_some()` already show the cycle affordance — attr rows now qualify automatically.
- GUI `main.rs::open_columns_picker` (~1686): same `open_with_catalog` with `self.attr_catalogs.get(scheme)`.
- GUI `columns_view.rs` `row_display`: prefer `row.label` when present (keep the existing mask+cap choke point — labels arrive pre-masked, the defensive re-mask stays per the P1 lesson: bidi unmasked in render disappears silently).

- [ ] **Step 4.6: Run everything touched**

```sh
cargo nextest run -p norte-frontend -p norte-config -p norte-tui 2>/dev/null; echo EXIT=$?
cargo clippy -p norte-frontend -p norte-config -p norte-tui --all-targets -- -D warnings 2>/dev/null; echo EXIT=$?
just check-gui; echo EXIT=$?
just gui-ci; echo EXIT=$?
```
Expected: all `EXIT=0`.

- [ ] **Step 4.7: Commit**

```bash
git add -A
git commit -m "feat(frontend,tui,gui): catalog-aware picker + attr format cycling (#117)"
```

---

### Task 5: Reviews, changelog, full gate, close

- [ ] **Step 5.1: Dispatch reviewers** on the cumulative diff (base = the commit before Task 1):
  - `rust-reviewer` — hard-rules pass over the four commits.
  - `encoding-auditor` — attr `Text`/`Bytes`/label rendering, header fallbacks, picker labels; point it at `columns.rs::attr_cell`/`header_label`, the TUI buffer test, and the GUI cell path.
  - No protocol-guardian needed: zero proto/daemon changes (verify with `git diff <base> --stat -- crates/norte-proto crates/norte-core` → empty for proto; backend.rs untouched).
  Apply findings, commit as `fix(frontend): apply the #117 review findings` (or per-crate as appropriate).

- [ ] **Step 5.2: Changelog** — add the entry alongside the block-7c one (`grep -rn "block 7c" CHANGELOG.md docs/` to find the file/format used) covering: attr cells in both frontends, per-scheme catalog cache, on-demand attr requests, doctor finding now plugin-only, picker offers provider columns, `octal`/`rwx` formats. Commit `docs(changelog): #117 attr column cells`.

- [ ] **Step 5.3: Full local gate** (frontends + config touched; no proto/vfs/core — but this is the close of the change):

```sh
just ci; echo EXIT=$?
```
Expected: `EXIT=0`. (Remember the llvm-cov staleness trap: if coverage numbers look impossible, `cargo llvm-cov clean` and re-run.)

- [ ] **Step 5.4: Close issue #117** with a comment in the style of the #108 block comments: what shipped, the sorted-but-hidden note (sort vocabulary is closed to builtins, rule vacuous — sorting by attr columns is future work, file it if wanted), `stat_with` left unwired (no consumer), plugin cells still pending (finding retained), reviews applied, gates green.

- [ ] **Step 5.5: Update memory** — `backlog-deuda-estado.md` / project state: #117 done, remaining follow-ups (#114/#115/#116/#113).

---

## Self-review notes

- **Spec coverage:** issue bullets → Task 1 (funnel + formatters + doctor), Task 2/3 (wire requests + Layer-7 note), Task 4 (catalog-aware picker). Hostile rendering pre-seeded by MemProvider corpus → Tasks 1/2 tests.
- **Type consistency:** `style_for_id(scheme, &ColumnId, Option<&AttrCatalog>)`, `styled_cell(&Entry, &ColumnId, i64, &ColumnStyle)`, `header_label(&ColumnId, &ColumnStyle, Option<&AttrCatalog>)`, `next_format_id(&ColumnId, AttrHint, &str)`, `format_name_id(&ColumnId, AttrHint, &ColumnStyle)`, `open_with_catalog(&ColumnsSettings, &str, SortSpec, Option<&AttrCatalog>)`, `attr_ids_for(&str) -> Vec<String>` — used consistently across tasks.
- **Known drift risk:** line numbers are anchors, not truth — every step names a grep to relocate. The `-rw-r--r--` literal in Step 1.1 must be checked against `format_mode_rwx`'s real output before trusting the test.
