//! Column cells contributed by a `columns` plugin (G3c: the GUI-side half
//! of the G3b deferral — `PluginInfo::columns` discovery landed in G3c
//! too, see [`norte_proto::methods::PluginColumnInfo`]). Masking for a
//! column's HEADER and its per-entry cell values, same untrusted-text
//! criterion the crate's decoration module already applies to a badge — a
//! `columns` plugin's `header` and cell text are THIRD-PARTY, never
//! painted raw.

use std::collections::HashMap;

use norte_proto::VPath;

/// Cap on a cell value AFTER masking, in CHARACTERS — same "short and
/// monospaced" criterion as a badge
/// ([`crate::decoration::BADGE_MAX_CHARS`]), but a column cell needs a bit
/// more room (e.g. `"modified 2h ago"`). The actual FIXED render width is
/// each frontend's call; this cap is an anti-DoS backstop, not the paint
/// width.
pub const COLUMN_VALUE_MAX_CHARS: usize = 32;

/// Masks a column HEADER ([`norte_proto::methods::PluginColumnInfo::header`],
/// plugin text — untrusted).
#[must_use]
pub fn sanitize_header(header: &str) -> String {
    crate::display_name(header.as_bytes()).0
}

/// Masks and truncates ONE cell value to [`COLUMN_VALUE_MAX_CHARS`]
/// characters AFTER masking (truncating before masking could cut a
/// multi-byte hazard mid-sequence); `None` (the column doesn't apply to
/// that entry) and an all-hazard value that masks to empty both collapse
/// to `None` — a frontend paints nothing for either, same as a badge.
#[must_use]
pub fn sanitize_cell(value: Option<&str>) -> Option<String> {
    value.and_then(|v| {
        let masked = crate::display_name(v.as_bytes()).0;
        let truncated: String = masked.chars().take(COLUMN_VALUE_MAX_CHARS).collect();
        (!truncated.is_empty()).then_some(truncated)
    })
}

/// Flattens the POSITIONAL `values` (1:1 with `paths`, the
/// `PLUGIN_COLUMN_VALUES` wire contract) into a `HashMap<VPath, String>` of
/// already-sanitized cells, one entry per path that got a non-empty cell —
/// same "flatten the wire's positional contract to a map keyed by path"
/// shape as [`crate::merge_decorations`]. `paths`/`values` are walked with
/// `zip` (stops at the shorter): defense in depth if a remote daemon broke
/// the 1:1 contract (already validated server-side, but a client never
/// trusts blindly).
#[must_use]
pub fn sanitize_column_values(
    paths: &[VPath],
    values: &[Option<String>],
) -> HashMap<VPath, String> {
    paths
        .iter()
        .zip(values.iter())
        .filter_map(|(p, v)| sanitize_cell(v.as_deref()).map(|s| (p.clone(), s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn sanitize_header_enmascara_hostil() {
        let hostil = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let header = String::from_utf8_lossy(&hostil.bytes).into_owned();
        let out = sanitize_header(&header);
        assert!(!out.chars().any(norte_encoding::is_terminal_hazard));
    }

    #[test]
    fn sanitize_cell_none_pasa_a_none() {
        assert_eq!(sanitize_cell(None), None);
    }

    #[test]
    fn sanitize_cell_trunca_tras_enmascarar() {
        let largo = "a".repeat(1000);
        let out = sanitize_cell(Some(&largo)).unwrap();
        assert_eq!(out.chars().count(), COLUMN_VALUE_MAX_CHARS);
    }

    #[test]
    fn sanitize_cell_vacio_tras_enmascarar_es_none() {
        assert_eq!(sanitize_cell(Some("")), None);
    }

    #[test]
    fn sanitize_column_values_posicional_con_none_intercalado() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs"), vp("mem:///c.rs")];
        let values = vec![Some("modified".to_owned()), None, Some(String::new())];
        let map = sanitize_column_values(&paths, &values);
        assert_eq!(
            map.get(&vp("mem:///a.rs")).map(String::as_str),
            Some("modified")
        );
        assert!(!map.contains_key(&vp("mem:///b.rs")));
        assert!(
            !map.contains_key(&vp("mem:///c.rs")),
            "cadena vacía tras enmascarar no entra en el mapa"
        );
    }

    #[test]
    fn sanitize_column_values_longitudes_distintas_no_panica() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs")];
        let values = vec![Some("x".to_owned())];
        let map = sanitize_column_values(&paths, &values);
        assert_eq!(map.len(), 1);
    }
}
