//! PURE model of the columns picker (#108 block 7a): state and transitions
//! with no IO and no render — the TUI wraps it in an overlay and the GUI (7c)
//! in a panel, same machine. Rule 7: the logic lives here.

use crate::columns::{Builtin, ColumnId, ColumnsSettings, sort_column};
use crate::sort::SortSpec;

/// One row of the picker: the id in Display form (what gets persisted), its
/// builtin if it is one (Fluent label + sortable), and whether it is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    /// Id as it travels to the config (`"size"`, `"attr:posix.mode"`…).
    pub id: String,
    /// `Some` for builtins (localized label, sort); `None` for attr/plugin
    /// (label in [`Self::label`], #117) and for ids that do not parse — they
    /// are shown and preserved.
    pub builtin: Option<Builtin>,
    /// Active = appears in the persisted list.
    pub enabled: bool,
    /// The row's CURRENT format (#108 7b): closed ASCII vocabulary
    /// (`"iec"`…); `Some` for Size/Mtime and for attrs with hint Size/
    /// Timestamp/Mode (#117) — everything else admits no format.
    pub format: Option<String>,
    /// LOCKED (m2 review 7b): a spec FROM THE SCHEME fixes the format — the
    /// picker only writes the GLOBAL spec, which the override would keep
    /// masking (a lying toast + leaking into other schemes). The row still
    /// shows its format but the cycle is a no-op and `finish` never emits
    /// it.
    pub format_locked: bool,
    /// Display label for NON-builtin rows (#117): [`crate::columns::header_label`]'s
    /// answer at open time (localized / already-masked catalogue label /
    /// sanitized id). `None` for builtins (live Fluent) and for ids that do
    /// not parse (frontends fall back to the raw id, which they mask
    /// themselves).
    pub label: Option<String>,
    /// The catalogue's hint at open time (attrs): decides the format cycle's
    /// table. `Opaque` = no cycle.
    pub hint: norte_proto::attrs::AttrHint,
    /// The format at OPEN time: [`ColumnsPicker::finish`] only emits the
    /// CHANGED ones. Private: born equal to `format` and never touched
    /// afterwards.
    opened_format: Option<String>,
}

/// The result of confirming the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// Enabled ids, in paint order.
    pub ids: Vec<String>,
    /// The chosen sort order.
    pub sort: SortSpec,
    /// `Some(scheme)` if the save goes to the scheme's override; `None` =
    /// default.
    pub scheme_target: Option<String>,
    /// Formats CHANGED relative to opening (#108 7b), as `(id, format)`:
    /// only what was touched travels to disk — one spec per column,
    /// replacement by id (`persist_column_format`).
    pub formats: Vec<(String, String)>,
}

/// Builds a row, seeding `format`/`opened_format` from the scheme's RESOLVED
/// style (`style_for` already folds global spec ← scheme, through the single
/// table [`crate::columns::format_name`], m3) — one single place for the
/// "born equal" invariant — and the scheme lock (m2).
fn make_row(
    id: String,
    builtin: Option<Builtin>,
    enabled: bool,
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> PickerRow {
    // #117: a single parse per row — attr and plugin stop being opaque: the
    // resolved style carries its hint (the cycle's table) and `header_label`
    // its label (already masked). Ids that do NOT parse stay opaque.
    let parsed = id.parse::<ColumnId>().ok();
    let (hint, format, format_locked, label) = match &parsed {
        Some(cid) => {
            let style = settings.style_for_id(scheme, cid, catalog);
            let hint = style.hint;
            let format = crate::columns::format_name_id(cid, &style).map(str::to_owned);
            let locked = settings.format_pinned_by_scheme_id(scheme, cid);
            let label = matches!(cid, ColumnId::Attr(_) | ColumnId::Plugin { .. })
                .then(|| crate::columns::header_label(cid, &style, catalog));
            (hint, format, locked, label)
        }
        None => (norte_proto::attrs::AttrHint::Opaque, None, false, None),
    };
    PickerRow {
        id,
        builtin,
        enabled,
        format_locked,
        label,
        hint,
        opened_format: format.clone(),
        format,
    }
}

/// The picker's state. `open` starts from the pane's EFFECTIVE set (scheme
/// override > default) and closes the list with the rest of the builtin
/// catalogue disabled. `name` comes first and is immutable (it can neither be
/// toggled nor moved off the head: the first column IS the name by the
/// render's contract).
#[derive(Debug, Clone)]
pub struct ColumnsPicker {
    rows: Vec<PickerRow>,
    cursor: usize,
    sort: SortSpec,
    scheme: String,
    scheme_override: bool,
}

/// The id of the permissions column, as written in `[ui.columns]`.
const PERMISSIONS_ID: &str = "attr:posix.mode";

impl ColumnsPicker {
    /// Builds the picker for the pane on `scheme` with its current order,
    /// with no provider catalogue ([`Self::open_with_catalog`] with `None`).
    #[must_use]
    pub fn open(settings: &ColumnsSettings, scheme: &str, current_sort: SortSpec) -> Self {
        Self::open_with_catalog(settings, scheme, current_sort, None, &[])
    }

    /// Builds the picker with the provider's attr catalogue (#117): attrs
    /// ANNOUNCED and not configured are offered disabled at the end — the
    /// picker OFFERS, it does not impose — and attr rows gain a hint (format
    /// cycle) and a label from the catalogue.
    #[must_use]
    pub fn open_with_catalog(
        settings: &ColumnsSettings,
        scheme: &str,
        current_sort: SortSpec,
        catalog: Option<&norte_proto::AttrCatalog>,
        plugins: &[norte_proto::methods::PluginInfo],
    ) -> Self {
        let raw = settings.raw_ids_for(scheme);
        let mut rows: Vec<PickerRow> = Vec::new();
        for id in &raw {
            let builtin = match id.parse::<ColumnId>() {
                Ok(ColumnId::Builtin(b)) => Some(b),
                _ => None,
            };
            // Dedup ONLY of builtins (same criterion as layout_items_for).
            // OPAQUE duplicates are PRESERVED on purpose: they are the
            // user's own config intent and the picker never cleans it up
            // (`doctor` reports it) — do not "fix" this `if` to dedup them.
            if builtin.is_some() && rows.iter().any(|r| r.builtin == builtin) {
                continue;
            }
            rows.push(make_row(
                id.clone(),
                builtin,
                true,
                settings,
                scheme,
                catalog,
            ));
        }
        // name first and immutable (the render's contract).
        if let Some(pos) = rows.iter().position(|r| r.builtin == Some(Builtin::Name)) {
            let name = rows.remove(pos);
            rows.insert(0, name);
        } else {
            rows.insert(
                0,
                make_row(
                    ColumnId::Builtin(Builtin::Name).to_string(),
                    Some(Builtin::Name),
                    true,
                    settings,
                    scheme,
                    catalog,
                ),
            );
        }
        // The PERMISSIONS column the listing itself supplies (spec
        // 2026-09-20), as an ENABLED row.
        //
        // `raw_ids_for` does not bring it —there is no catalogue where it
        // looks— and without this the picker said it was off while the
        // listing was painting it, and confirming without touching anything
        // erased it forever: writing the explicit list means the default set
        // never runs again. A dialog that erases what it showed as on is
        // worse than one that does not offer the column.
        if crate::columns::sets_permission_column(settings, scheme, catalog)
            && !rows.iter().any(|r| r.id == PERMISSIONS_ID)
        {
            rows.push(make_row(
                PERMISSIONS_ID.to_owned(),
                None,
                true,
                settings,
                scheme,
                catalog,
            ));
        }
        // Remaining catalogue, disabled, in canonical order.
        for b in [Builtin::Size, Builtin::Mtime, Builtin::Kind] {
            if !rows.iter().any(|r| r.builtin == Some(b)) {
                rows.push(make_row(
                    ColumnId::Builtin(b).to_string(),
                    Some(b),
                    false,
                    settings,
                    scheme,
                    catalog,
                ));
            }
        }
        // PROVIDER catalogue (#117): attrs announced and not configured,
        // disabled, after the builtins — the picker OFFERS, it does not
        // impose.
        if let Some(cat) = catalog {
            for info in cat {
                let id = format!("attr:{}", info.id);
                if !rows.iter().any(|r| r.id == id) {
                    rows.push(make_row(id, None, false, settings, scheme, catalog));
                }
            }
        }
        // Columns DECLARED by plugins (#120), same criterion as the attrs:
        // the unconfigured ones are offered disabled at the end.
        //
        // Only from approved AND enabled plugins: offering the column of one
        // the human has not consented to would be inviting them to configure
        // something the host is going to refuse to serve, and the resulting
        // row would paint blank with no explanation.
        //
        // The offered id carries the plugin inside (`plugin:{p}/{c}`) — it
        // is the form `[ui.columns]` stores and, since 0.35.0, the one that
        // unambiguously identifies which plugin serves the column when two
        // declare the same bare id.
        for p in plugins {
            if !p.approved || !p.enabled {
                continue;
            }
            for c in &p.columns {
                let id = format!("plugin:{}/{}", p.id, c.id);
                if !rows.iter().any(|r| r.id == id) {
                    rows.push(make_row(id, None, false, settings, scheme, catalog));
                }
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

    /// Rows in paint order.
    #[must_use]
    pub fn rows(&self) -> &[PickerRow] {
        &self.rows
    }

    /// The row under the cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The sort order in progress (applied on confirm, not before).
    #[must_use]
    pub fn sort(&self) -> SortSpec {
        self.sort.clone()
    }

    /// The scheme of the pane that opened the picker.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Will the save go to the scheme's override?
    #[must_use]
    pub fn scheme_override(&self) -> bool {
        self.scheme_override
    }

    /// Cursor up (saturating).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Cursor down (saturating).
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// Toggles the row under the cursor. `name` is immutable.
    pub fn toggle(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(r) = self.rows.get_mut(self.cursor) {
            r.enabled = !r.enabled;
        }
    }

    /// Moves the row under the cursor up one spot (never above `name`); the
    /// cursor follows it.
    pub fn move_up(&mut self) {
        if self.cursor > 1 {
            self.rows.swap(self.cursor, self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Moves the row under the cursor down one spot; the cursor follows it.
    pub fn move_down(&mut self) {
        if self.cursor >= 1 && self.cursor + 1 < self.rows.len() {
            self.rows.swap(self.cursor, self.cursor + 1);
            self.cursor += 1;
        }
    }

    /// Sorts by the column under the cursor with the header click's
    /// semantics ([`SortSpec::after_click`]); a no-op on non-sortable rows.
    pub fn sort_current(&mut self) {
        if let Some(sc) = self
            .rows
            .get(self.cursor)
            .and_then(|r| r.builtin)
            .and_then(sort_column)
        {
            self.sort = self.sort.clone().after_click(sc);
        }
    }

    /// Cycles the row under the cursor's format through its closed
    /// vocabulary (#108 7b, single table [`crate::columns::next_format_id`]):
    /// size `iec→si→exact→iec`, mtime `relative→iso→relative`, attrs
    /// through their hint's table (#117: Mode `rwx→octal→rwx`…); a no-op on
    /// name/kind/opaque rows (no format) and on rows LOCKED by a scheme spec
    /// (m2 — see [`PickerRow::format_locked`]).
    pub fn cycle_format(&mut self) {
        let Some(r) = self.rows.get_mut(self.cursor) else {
            return;
        };
        if r.format_locked {
            return;
        }
        let Ok(cid) = r.id.parse::<ColumnId>() else {
            return;
        };
        let Some(cur) = r.format.as_deref() else {
            return;
        };
        if let Some(next) = crate::columns::next_format_id(&cid, r.hint, cur) {
            r.format = Some(next.to_owned());
        }
    }

    /// The row under the cursor's current format (`None` = admits no format:
    /// name/kind/opaque).
    #[must_use]
    pub fn format_of_cursor(&self) -> Option<String> {
        self.rows.get(self.cursor).and_then(|r| r.format.clone())
    }

    /// The result to apply/persist on confirm.
    #[must_use]
    pub fn finish(&self) -> Picked {
        Picked {
            ids: self
                .rows
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.id.clone())
                .collect(),
            sort: self.sort.clone(),
            scheme_target: self.scheme_override.then(|| self.scheme.clone()),
            // Locked ones are never emitted (m2): the cycle is already a
            // no-op on them — the extra filter is defense in depth.
            formats: self
                .rows
                .iter()
                .filter(|r| !r.format_locked && r.format != r.opened_format)
                .filter_map(|r| r.format.clone().map(|f| (r.id.clone(), f)))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::{Builtin, ColumnsSettings};
    use crate::sort::{SortColumn, SortDir, SortSpec};

    fn empty_settings() -> ColumnsSettings {
        ColumnsSettings::resolve(&norte_config::ColumnsConfig::default())
    }

    /// A catalogue like the local provider's: announces POSIX mode with its
    /// hint.
    fn catalog_with_mode() -> norte_proto::AttrCatalog {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        norte_proto::AttrCatalog::new(vec![AttrInfo {
            id: "posix.mode".into(),
            label: "Mode".into(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        }])
    }

    /// Opening the picker and confirming it WITHOUT touching anything does
    /// not change the listing (spec 2026-09-20).
    ///
    /// This is the bug the review found, and it is one of the costly ones:
    /// the permissions column is supplied by the listing, not the config, so
    /// the picker could not see it and showed it as off. Confirming writes
    /// the explicit list of what is on, and from then on the default set
    /// NEVER runs again. So entering to turn on "kind" erased the
    /// permissions forever and without saying anything.
    #[test]
    fn confirming_without_touching_anything_does_not_erase_the_permissions() {
        let s = empty_settings();
        let cat = catalog_with_mode();
        let p = ColumnsPicker::open_with_catalog(&s, "file", SortSpec::default(), Some(&cat), &[]);
        assert!(
            p.rows()
                .iter()
                .any(|r| r.id == "attr:posix.mode" && r.enabled),
            "the picker shows it ON, which is how it is: {:?}",
            p.rows()
                .iter()
                .map(|r| (&r.id, r.enabled))
                .collect::<Vec<_>>()
        );
        assert!(
            p.finish().ids.iter().any(|id| id == "attr:posix.mode"),
            "and confirming keeps it"
        );
    }

    /// Where the provider does NOT have POSIX permissions, the picker offers
    /// it off, like any other attribute that is not set.
    #[test]
    fn with_no_catalogue_it_does_not_turn_on_by_itself() {
        let s = empty_settings();
        let p = ColumnsPicker::open_with_catalog(&s, "file", SortSpec::default(), None, &[]);
        assert!(
            !p.finish().ids.iter().any(|id| id == "attr:posix.mode"),
            "the listing does not paint it, so the picker does not assume it is set either"
        );
    }

    #[test]
    fn cycle_format_cycles_the_columns_vocabulary() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.down(); // size (default format iec)
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("exact"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("iec")); // full circle
        // name/kind/opaque: no-op.
        p.up();
        p.cycle_format();
        assert_eq!(p.format_of_cursor(), None);
    }

    /// m2 review 7b: a spec FROM THE SCHEME with a format LOCKS the cycle —
    /// the global spec we would write would end up masked by the override (a
    /// lying toast) and leak into other schemes. The row keeps showing the
    /// current format.
    #[test]
    fn a_format_fixed_by_the_scheme_locks_the_cycle() {
        let cfg = norte_config::ColumnsConfig {
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    specs: [(
                        "size".to_owned(),
                        norte_config::ColumnSpec {
                            format: Some("exact".to_owned()),
                            ..Default::default()
                        },
                    )]
                    .into(),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "sftp", SortSpec::default());
        p.down(); // size
        assert_eq!(
            p.format_of_cursor().as_deref(),
            Some("exact"),
            "the row shows the scheme's resolved format"
        );
        assert!(p.rows()[p.cursor()].format_locked, "scheme lock");
        p.cycle_format();
        assert_eq!(
            p.format_of_cursor().as_deref(),
            Some("exact"),
            "locked: the cycle is a no-op"
        );
        assert!(
            p.finish().formats.is_empty(),
            "a locked one is never emitted"
        );
        // The SAME builtin on another scheme is still free (the lock is per
        // scheme, not global).
        let mut free = ColumnsPicker::open(&s, "file", SortSpec::default());
        free.down();
        assert!(!free.rows()[free.cursor()].format_locked);
        free.cycle_format();
        assert_eq!(free.format_of_cursor().as_deref(), Some("si"));
    }

    #[test]
    fn finish_carries_only_the_changed_formats() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.down();
        p.cycle_format(); // size → si
        let picked = p.finish();
        assert_eq!(picked.formats, vec![("size".to_owned(), "si".to_owned())]);
        // no changes → empty
        let p2 = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        assert!(p2.finish().formats.is_empty());
        // Cycling and RETURNING to the opening value is also "no changes":
        // the diff is against opened_format, not a "touched" flag.
        p.cycle_format(); // si → exact
        p.cycle_format(); // exact → iec (opening value)
        assert!(p.finish().formats.is_empty());
    }

    /// The seed comes from the scheme's RESOLVED style, not the default:
    /// with a `format = "si"` spec in config, the first cycle goes
    /// si→exact.
    #[test]
    fn open_seeds_the_format_from_the_resolved_style() {
        let cfg = norte_config::ColumnsConfig {
            specs: [(
                "size".to_owned(),
                norte_config::ColumnSpec {
                    format: Some("si".to_owned()),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "file", SortSpec::default());
        p.down(); // size
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("exact"));
        // And finish emits the change RELATIVE to the opening (si→exact).
        assert_eq!(
            p.finish().formats,
            vec![("size".to_owned(), "exact".to_owned())]
        );
    }

    #[test]
    fn it_opens_with_the_effective_set_and_the_remaining_catalogue() {
        let p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        // Effective default: name+size+mtime enabled; kind present disabled.
        let states: Vec<(Option<Builtin>, bool)> =
            p.rows().iter().map(|r| (r.builtin, r.enabled)).collect();
        assert_eq!(
            states,
            vec![
                (Some(Builtin::Name), true),
                (Some(Builtin::Size), true),
                (Some(Builtin::Mtime), true),
                (Some(Builtin::Kind), false),
            ]
        );
        assert!(
            !p.scheme_override(),
            "with no config there is no scheme override"
        );
    }

    #[test]
    fn toggle_turns_off_and_on_but_name_is_immutable() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.toggle(); // cursor 0 = name
        assert!(p.rows()[0].enabled, "name never turns off");
        p.down();
        p.toggle();
        assert!(!p.rows()[1].enabled, "size turns off");
        p.toggle();
        assert!(p.rows()[1].enabled);
    }

    #[test]
    fn moving_reorders_but_never_above_name() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.down(); // size
        p.down(); // mtime
        p.move_up(); // mtime <-> size
        let order: Vec<Option<Builtin>> = p.rows().iter().map(|r| r.builtin).collect();
        assert_eq!(order[1], Some(Builtin::Mtime));
        assert_eq!(order[2], Some(Builtin::Size));
        assert_eq!(p.cursor(), 1, "the cursor follows the moved row");
        p.move_up(); // already touching name: no-op
        assert_eq!(p.rows()[0].builtin, Some(Builtin::Name));
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn sort_current_applies_after_click_to_the_row() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.down(); // size
        p.sort_current();
        assert_eq!(p.sort().column, SortColumn::Size);
        assert_eq!(p.sort().dir, SortDir::Asc);
        p.sort_current(); // second time reverses it
        assert_eq!(p.sort().dir, SortDir::Desc);
    }

    #[test]
    fn sort_current_on_a_non_sortable_row_is_a_noop() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        for _ in 0..3 {
            p.down(); // kind
        }
        let before = p.sort();
        p.sort_current();
        assert_eq!(p.sort(), before, "kind is not sortable");
    }

    #[test]
    fn finish_emits_only_the_enabled_ones_in_order() {
        let mut p = ColumnsPicker::open(&empty_settings(), "file", SortSpec::default());
        p.down();
        p.toggle(); // size out
        let picked = p.finish();
        assert_eq!(picked.ids, vec!["name".to_owned(), "mtime".to_owned()]);
        assert_eq!(picked.scheme_target, None, "no override → default");
    }

    #[test]
    fn opaque_ids_are_preserved_and_travel_whole() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:posix.mode".into(),
                "size".into(),
                "does not parse!".into(),
            ]),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let mut p = ColumnsPicker::open(&s, "file", SortSpec::default());
        // The opaque ones are there, enabled, in their position; the
        // remaining catalogue (mtime, kind) closes the list disabled.
        let ids: Vec<&str> = p.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids[..4],
            ["name", "attr:posix.mode", "size", "does not parse!"]
        );
        let picked = p.finish();
        assert!(picked.ids.contains(&"attr:posix.mode".to_owned()));
        assert!(picked.ids.contains(&"does not parse!".to_owned()));
        // And they are toggleable: turning off the attr removes it from the
        // result.
        p.down();
        p.toggle();
        assert!(!p.finish().ids.contains(&"attr:posix.mode".to_owned()));
    }

    #[test]
    fn open_with_catalog_offers_unconfigured_attrs() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![
            AttrInfo {
                id: "posix.mode".into(),
                label: "Mode".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
            AttrInfo {
                id: "posix.uid".into(),
                label: "UID".into(),
                ty: AttrType::Uint,
                hint: AttrHint::Identity,
            },
        ]);
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:posix.mode".into()]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let mut p =
            ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        // The configured one stays enabled; the announced-but-unconfigured
        // one appears disabled at the end, exactly ONCE.
        let uid: Vec<_> = p
            .rows()
            .iter()
            .filter(|r| r.id == "attr:posix.uid")
            .collect();
        assert_eq!(uid.len(), 1);
        assert!(!uid[0].enabled);
        assert_eq!(
            p.rows()
                .iter()
                .filter(|r| r.id == "attr:posix.mode")
                .count(),
            1
        );
        // The attr row with hint Mode cycles format: rwx → octal.
        let mode = p
            .rows()
            .iter()
            .position(|r| r.id == "attr:posix.mode")
            .unwrap();
        assert_eq!(p.rows()[mode].format.as_deref(), Some("rwx"));
        while p.cursor() != mode {
            p.down();
        }
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("octal"));
        p.cycle_format();
        assert_eq!(p.format_of_cursor().as_deref(), Some("rwx"), "full circle");
        // And the announced Identity one admits no format (no table for its
        // hint).
        while p.cursor() + 1 < p.rows().len() {
            p.down();
        }
        assert_eq!(p.rows()[p.cursor()].id, "attr:posix.uid");
        p.cycle_format();
        assert_eq!(p.format_of_cursor(), None);
    }

    #[test]
    fn open_with_no_catalogue_keeps_the_historical_behaviour() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let a = ColumnsPicker::open(&st, "file", SortSpec::default());
        let b = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &[]);
        assert_eq!(a.rows(), b.rows());
    }

    /// #117 review task 4 (a): a row OFFERED from the catalogue (born
    /// disabled) can be turned on and its id travels in `finish().ids` —
    /// offering without being able to choose would be a fake picker.
    #[test]
    fn an_offered_catalogue_row_turns_on_and_travels_in_finish() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![AttrInfo {
            id: "posix.uid".into(),
            label: "UID".into(),
            ty: AttrType::Uint,
            hint: AttrHint::Identity,
        }]);
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let mut p =
            ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        assert!(!p.finish().ids.contains(&"attr:posix.uid".to_owned()));
        while p.rows()[p.cursor()].id != "attr:posix.uid" {
            p.down();
        }
        p.toggle();
        assert!(p.rows()[p.cursor()].enabled);
        assert!(p.finish().ids.contains(&"attr:posix.uid".to_owned()));
    }

    /// #117 review task 4 (b): a HOSTILE label from the catalogue arrives
    /// ALREADY masked at `PickerRow.label` (`header_label` is the choke
    /// point; the frontends re-masking it is a belt, not the defense).
    #[test]
    fn a_hostile_catalogue_label_arrives_masked() {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        let cat = norte_proto::AttrCatalog::new(vec![AttrInfo {
            // `mem.` is not a first-party namespace: with no Fluent key, the
            // catalogue's label is what gets shown.
            id: "mem.owner".into(),
            label: "Owner\u{202e}evil".into(),
            ty: AttrType::Bytes,
            hint: AttrHint::Identity,
        }]);
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), Some(&cat), &[]);
        let row = p
            .rows()
            .iter()
            .find(|r| r.id == "attr:mem.owner")
            .expect("offered row");
        let label = row.label.as_deref().expect("catalogue label");
        assert!(
            !label.chars().any(norte_encoding::is_terminal_hazard),
            "raw hazard in label: {label:?}"
        );
        assert!(
            label.contains("Owner"),
            "keeps the harmless part: {label:?}"
        );
    }

    #[test]
    fn a_scheme_with_an_override_points_at_the_scheme() {
        let cfg = norte_config::ColumnsConfig {
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    columns: Some(vec!["name".into(), "mtime".into()]),
                    sort: None,
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let p = ColumnsPicker::open(&s, "sftp", SortSpec::default());
        assert!(p.scheme_override());
        assert_eq!(p.finish().scheme_target.as_deref(), Some("sftp"));
        let enabled: Vec<&str> = p
            .rows()
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(enabled, ["name", "mtime"]);
    }
}

#[cfg(test)]
mod plugin_offer_tests {
    use super::*;

    fn plugin(
        id: &str,
        cols: &[&str],
        approved: bool,
        enabled: bool,
    ) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.to_owned(),
            name: id.to_owned(),
            publisher: String::new(),
            version: "1.0.0".to_owned(),
            category: "columns".to_owned(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: cols
                .iter()
                .map(|c| norte_proto::methods::PluginColumnInfo {
                    id: (*c).to_owned(),
                    header: (*c).to_owned(),
                })
                .collect(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    /// A column declared by a consented plugin gets OFFERED (#120). Before
    /// this, the only way to reach it was hand-editing `[ui.columns]`, which
    /// is as good as not offering it.
    #[test]
    fn a_consented_plugins_column_is_offered() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let plugins = [plugin("org.norte.git", &["status"], true, true)];
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
        let row = p
            .rows()
            .iter()
            .find(|r| r.id == "plugin:org.norte.git/status")
            .expect("the declared column must be offered");
        assert!(
            !row.enabled,
            "it is OFFERED disabled: the picker offers, it does not impose"
        );
    }

    /// An unconsented plugin is not offered: configuring it would give a
    /// column the host refuses to serve, which would paint blank with no
    /// explanation.
    #[test]
    fn what_the_human_has_not_consented_to_is_not_offered() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        for (approved, enabled) in [(false, true), (true, false), (false, false)] {
            let plugins = [plugin("org.norte.git", &["status"], approved, enabled)];
            let p =
                ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
            assert!(
                !p.rows().iter().any(|r| r.id.starts_with("plugin:")),
                "approved={approved} enabled={enabled} must not be offered"
            );
        }
    }

    /// Two plugins can declare the SAME bare id, and the two rows have to
    /// exist separately: the offered id carries the plugin inside, which is
    /// exactly what the wire has been able to tell apart since 0.35.0
    /// (#120).
    #[test]
    fn two_plugins_with_the_same_bare_id_give_two_rows() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let plugins = [
            plugin("org.a.tool", &["status"], true, true),
            plugin("org.b.tool", &["status"], true, true),
        ];
        let p = ColumnsPicker::open_with_catalog(&st, "file", SortSpec::default(), None, &plugins);
        for id in ["plugin:org.a.tool/status", "plugin:org.b.tool/status"] {
            assert!(
                p.rows().iter().any(|r| r.id == id),
                "row `{id}` is missing: collapsing them would hide a column the \
                 user can actually configure"
            );
        }
    }
}
