//! The attribute sheet: which rows describe what is under the cursor.
//!
//! It used to live TWICE — `norte-tui/src/ui/panels.rs` and
//! `norte-ui-host/src/controller/places.rs` — with the same field order, the
//! same size format and the same attribute walk, copied by hand. Two copies
//! of a presentation rule diverge, and these already had: the window marked a
//! hostile attribute value and the TUI did not, because the fix was applied
//! to only one.
//!
//! It is here once. The frontends PAINT it; neither decides what goes inside.
//!
//! It asks for nothing: everything comes out of the [`Entry`] the listing
//! already had. A pane that follows the cursor and also requests data for
//! every row is how going down a directory turns into a storm of requests.

use norte_i18n::{Lang, t_in};
use norte_proto::{AttrCatalog, Entry, EntryKind};

use crate::columns::{ColumnId, ColumnStyle, header_label_in, styled_cell};

/// A sheet row: a label, a value, and whether the value carried bytes that
/// had to be masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The label, already translated — or the header the catalogue gives the
    /// attribute.
    pub label: String,
    /// The value, already formatted and sanitized.
    pub value: String,
    /// The value carried bytes that could not be painted. Whoever shows it
    /// MARKS it, the same as the equivalent column.
    pub hostile: bool,
}

/// The rows that describe `entry`.
///
/// `parent_row` says whether what is under the cursor is the `..` row. On it
/// the sheet is NOT named like the parent directory: it is named `..`, as in
/// the listing, and adds where it leads. Describing the row with the
/// parent's name would suggest the cursor is on the parent, which is exactly
/// what the row is not.
///
/// `catalog` is the entry's schema attribute catalogue, if known: attributes
/// the provider already brought go through the SAME door as their equivalent
/// column, so the sheet and the column cannot disagree about what an
/// attribute is worth.
///
/// ```
/// use norte_frontend::metadata::sheet;
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let e = Entry {
///     path: VPath::parse("mem:///home/readme.txt").unwrap(),
///     kind: EntryKind::File,
///     size: Some(12),
///     mtime_ms: None,
///     attrs: std::collections::BTreeMap::new(),
/// };
/// let rows = sheet(&e, false, None, norte_i18n::Lang::En);
/// assert_eq!(rows[0].value, "readme.txt");
/// ```
#[must_use]
pub fn sheet(
    entry: &Entry,
    parent_row: bool,
    catalog: Option<&AttrCatalog>,
    lang: Lang,
) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut field = |key: &str, value: String, hostile: bool| {
        fields.push(Field {
            label: t_in(lang, key),
            value,
            hostile,
        });
    };

    if parent_row {
        // `..` is the name the listing paints on that row, and it is
        // repeated here: the sheet and the row it describes read the same.
        field("metadata-name", "..".to_owned(), false);
        field("metadata-kind", t_in(lang, "metadata-kind-dir"), false);
        let (target, hostile) = crate::display::path_display(&entry.path);
        field("metadata-target", target, hostile);
        // No size, no date, no attributes: the synthetic `Entry` does not
        // carry them — they are not this directory's — and filling them in
        // would be answering for the parent without having looked at it.
        return fields;
    }

    let name = entry
        .path
        .file_name()
        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
    let (displayable, hostile) = crate::display::display_name(&name);
    field("metadata-name", displayable, hostile);
    field(
        "metadata-kind",
        t_in(
            lang,
            match entry.kind {
                EntryKind::Dir => "metadata-kind-dir",
                EntryKind::File => "metadata-kind-file",
                EntryKind::Symlink => "metadata-kind-symlink",
                EntryKind::Other => "metadata-kind-other",
            },
        ),
        false,
    );
    if let Some(n) = entry.size {
        // Both the human-readable and the exact one: "1.2 MiB" is no use for
        // comparing and `1258291` is no use for reading.
        field(
            "metadata-size",
            format!("{} ({n})", crate::human_bytes_short(n)),
            false,
        );
    }
    if let Some(ms) = entry.mtime_ms {
        field(
            "metadata-mtime",
            crate::columns::format_mtime(ms, crate::columns::TimeFormat::Iso, ms),
            false,
        );
    }
    let now = entry.mtime_ms.unwrap_or(0);
    for attr in entry.attrs.keys() {
        let col = ColumnId::Attr(attr.clone());
        let style = ColumnStyle::default_for_id(&col, catalog);
        let Some(cell) = styled_cell(entry, &col, now, &style) else {
            continue;
        };
        // The mark is taken from the RAW value, not the already-formatted
        // cell: `styled_cell` masks internally and does not return the flag,
        // and asking the already-masked value again answers nothing — U+FFFD
        // is not a terminal hazard, so an already-converted value declares
        // itself faithful.
        let hostile = match entry.attrs.get(attr) {
            Some(norte_proto::AttrValue::Text(t)) => crate::display::display_name(t.as_bytes()).1,
            Some(norte_proto::AttrValue::Bytes(b)) => crate::display::display_name(b).1,
            // The rest are numbers or timestamps norte formats: there is no
            // third-party text to mask. ENUMERATED and not `_`: the day
            // `AttrValue` gains a variant carrying text, this has to be a
            // compile error and not a silent declaration that someone else's
            // bytes are faithful.
            Some(
                norte_proto::AttrValue::Uint(_)
                | norte_proto::AttrValue::Int(_)
                | norte_proto::AttrValue::TimeMs(_)
                | norte_proto::AttrValue::Bool(_)
                | norte_proto::AttrValue::Unknown,
            )
            | None => false,
        };
        fields.push(Field {
            // `_in` and not the global one: whoever passes `lang` does so
            // because theirs has no reason to be the process's, and half a
            // translated sheet is worse than none.
            label: header_label_in(&col, &style, catalog, lang),
            value: cell,
            hostile,
        });
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{AttrValue, VPath};

    fn entry(wire: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("test vpath"),
            kind,
            size: None,
            mtime_ms: None,
        }
    }

    fn labels(rows: &[Field]) -> Vec<&str> {
        rows.iter().map(|f| f.label.as_str()).collect()
    }

    /// A file: name, kind, human-readable AND exact size, and an ISO date.
    #[test]
    fn a_file_carries_name_kind_size_and_date() {
        let mut e = entry("mem:///home/readme.txt", EntryKind::File);
        e.size = Some(1_258_291);
        e.mtime_ms = Some(1_700_000_000_000);
        let rows = sheet(&e, false, None, Lang::En);
        assert_eq!(labels(&rows), ["Name", "Kind", "Size", "Modified"]);
        assert_eq!(rows[0].value, "readme.txt");
        assert_eq!(rows[1].value, "file");
        assert!(
            rows[2].value.contains("(1258291)"),
            "the exact number goes next to the human-readable one: {}",
            rows[2].value
        );
    }

    /// With no size or date, no empty row is invented: the row is not there.
    #[test]
    fn what_is_unknown_does_not_appear() {
        let e = entry("mem:///home/dir", EntryKind::Dir);
        let rows = sheet(&e, false, None, Lang::En);
        assert_eq!(labels(&rows), ["Name", "Kind"]);
        assert_eq!(rows[1].value, "folder");
    }

    /// The `..` row is described as `..` and says WHERE it leads.
    ///
    /// The bug this closes: the sheet used to name it with the parent's
    /// basename — "oscar" while standing in `/home/oscar/Downloads` — or,
    /// before that, did not describe it at all.
    #[test]
    fn the_parent_row_is_named_two_dots_and_says_where_it_leads() {
        let e = entry("mem:///home", EntryKind::Dir);
        let rows = sheet(&e, true, None, Lang::En);
        assert_eq!(labels(&rows), ["Name", "Kind", "Leads to"]);
        assert_eq!(rows[0].value, "..", "not the parent's name");
        assert_eq!(rows[1].value, "folder");
        assert_eq!(
            rows[2].value, "⟨mem⟩/home",
            "the SAME form as the listing's header, not the wire one"
        );
    }

    /// A name that is not UTF-8 arrives masked AND marked.
    #[test]
    fn a_hostile_name_comes_marked() {
        let e = entry("mem:///home/%FF%FE", EntryKind::File);
        let rows = sheet(&e, false, None, Lang::En);
        assert!(rows[0].hostile, "{:?}", rows[0]);
        assert!(
            !rows[1].hostile,
            "the kind is written by norte: it is never hostile"
        );
    }

    /// And a hostile attribute VALUE too.
    ///
    /// This is the one that was wrong in the TUI: the window marked it, the
    /// TUI did not, and the equivalent COLUMN did in both. The sheet said the
    /// bytes were faithful while the column next to it said they were not.
    #[test]
    fn a_hostile_attribute_value_is_also_marked() {
        let mut e = entry("mem:///home/x", EntryKind::File);
        e.attrs
            .insert("owner".to_owned(), AttrValue::Bytes(b"\xff\xfe".to_vec()));
        let rows = sheet(&e, false, None, Lang::En);
        let attr = rows
            .last()
            .expect("the attribute comes after the fixed ones");
        assert!(attr.hostile, "{attr:?}");
    }
}
