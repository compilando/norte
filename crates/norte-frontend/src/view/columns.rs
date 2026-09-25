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

/// Cap on `plugin:` columns REQUESTABLE per painted listing (#117-follow-up)
/// — mirror of attrs' [`norte_proto::ATTRS_MAX_REQUEST`], but local to the
/// frontend: every plugin column costs ONE `plugin.column_values` RPC per
/// listing, so the cap bounds work, not wire. Painted == requested; the
/// overflow is diagnostic (`plugins_over_cap`, the doctor names it), never a
/// blank column.
///
/// It is the cap on the PAINTED ones. What a listing requests in total
/// ([`plugin_requests`]) adds the status bar's elements (ADR 0137), up to
/// `STATUS_PLUGINS_MAX` more: twelve calls at most.
pub const PLUGIN_COLUMNS_MAX_REQUEST: usize = 8;

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
        let mut truncated: String = masked.chars().take(COLUMN_VALUE_MAX_CHARS).collect();
        // The cut is MARKED. Without the mark, a truncated value and a
        // complete one paint identically, and whoever reads the attributes
        // sheet —which exists precisely to see the whole value— cannot know
        // which one they are looking at. This is the corpus's
        // `truncation_twins` rule: what is owed is that the cut be visible,
        // not that it fit.
        if masked.chars().nth(COLUMN_VALUE_MAX_CHARS).is_some() {
            truncated.push('…');
        }
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

/// A `plugin:` column's Display id (#117-follow-up, audit F5): goes through
/// [`ColumnId`]'s REAL `Display` — never an ad-hoc `format!` that could
/// drift from the parser (a drift = permanently blank cells with no
/// diagnostic, the side-map's key would stop matching).
#[must_use]
pub fn plugin_display_id(plugin: &str, column: &str) -> String {
    ColumnId::Plugin {
        plugin: plugin.to_owned(),
        column: column.to_owned(),
    }
    .to_string()
}

/// The plugin columns a `scheme` listing has to REQUEST: the ones painted as
/// a column plus the ones the status bar shows (`[ui] status_plugins`, ADR
/// 0137), without repeats, and in that order.
///
/// One single list for both frontends: the status element is the column's
/// value for the entry under the cursor, so it has to arrive on the SAME
/// trip as the cells —and with the same consent validation,
/// [`validated_plugin_requests`]—. With one list per frontend, one would
/// request the column and the other would show an empty element.
#[must_use]
pub fn plugin_requests(
    settings: &ColumnsSettings,
    status: &[(String, String)],
    scheme: &str,
) -> Vec<(String, String)> {
    let mut out = settings.plugin_ids_for(scheme);
    for par in status.iter().take(norte_config::load::STATUS_PLUGINS_MAX) {
        if !out.contains(par) {
            out.push(par.clone());
        }
    }
    out
}

/// Filters the CONFIGURED (plugin, column) pairs against the live catalog
/// (#117-follow-up, review MAJOR-1: a single definition for both frontends —
/// the MEMBERSHIP validation is what stops a configured id from painting the
/// column of a plugin that never declared it): a pair survives only if its
/// plugin is approved + enabled AND declares THAT column. It also DEDUPES by
/// bare column id (review MAJOR-2): the `plugin.column_values` wire resolves
/// first-match by bare id across plugins — two consented pairs with the same
/// column would serve the SAME values under two different headers
/// (misattributed data); the first is kept and the rest are left blank
/// (visible absence, never false attribution; real disambiguation = issue
/// #120).
#[must_use]
pub fn validated_plugin_requests(
    requested: &[(String, String)],
    plugins: &[norte_proto::methods::PluginInfo],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (plugin, column) in requested {
        let declared = plugins.iter().any(|p| {
            p.approved && p.enabled && p.id == *plugin && p.columns.iter().any(|c| c.id == *column)
        });
        // Dedup by the PAIR, not by the bare id (#120). Two consented
        // plugins can both declare `status`, and since 0.35.0 the wire
        // knows how to tell them apart: deduplicating by column alone would
        // silently drop the second one the user configured on purpose.
        if declared && !out.iter().any(|(p, c)| p == plugin && c == column) {
            out.push((plugin.clone(), column.clone()));
        }
    }
    out
}

#[cfg(test)]
mod validated_plugin_requests_tests {
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

    /// Membership: only the pair whose plugin (approved+enabled) declares
    /// THAT column survives — neither foreign columns nor unconsented
    /// plugins.
    #[test]
    fn filters_by_membership_and_consent() {
        let plugins = vec![
            plugin("git", &["branch"], true, true),
            plugin("other", &["status"], false, true),
            plugin("off", &["x"], true, false),
        ];
        let requested = vec![
            ("git".to_owned(), "branch".to_owned()),
            ("git".to_owned(), "status".to_owned()), // git does NOT declare status
            ("other".to_owned(), "status".to_owned()), // not approved
            ("off".to_owned(), "x".to_owned()),      // disabled
            ("ghost".to_owned(), "y".to_owned()),    // does not exist
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            vec![("git".to_owned(), "branch".to_owned())]
        );
    }

    /// Two consented plugins with the SAME bare column id: BOTH are served
    /// (#120 closed).
    ///
    /// Up to 0.35.0 the wire only carried the bare id and the host resolved
    /// to the first match, so serving both would have painted one's values
    /// under the other's header; the first was kept and the second was left
    /// blank — visible absence rather than false attribution. Now the
    /// request names the plugin, the host serves THAT one or none, and
    /// keeping only one would silently drop a column the user configured.
    #[test]
    fn bare_id_collision_serves_both_plugins() {
        let plugins = vec![
            plugin("a", &["branch"], true, true),
            plugin("b", &["branch"], true, true),
        ];
        let requested = vec![
            ("a".to_owned(), "branch".to_owned()),
            ("b".to_owned(), "branch".to_owned()),
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            requested,
            "each pair goes with its plugin: the wire already tells them apart (#120)"
        );
    }

    /// What is still deduplicated is the REPEATED pair: configuring
    /// `plugin:a/branch` twice is one column, not two requests to the same
    /// guest.
    #[test]
    fn the_repeated_pair_is_deduplicated() {
        let plugins = vec![plugin("a", &["branch"], true, true)];
        let requested = vec![
            ("a".to_owned(), "branch".to_owned()),
            ("a".to_owned(), "branch".to_owned()),
        ];
        assert_eq!(
            validated_plugin_requests(&requested, &plugins),
            vec![("a".to_owned(), "branch".to_owned())]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn sanitize_header_masks_hostile() {
        let hostile = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("corpus fixture");
        let header = String::from_utf8_lossy(&hostile.bytes).into_owned();
        let out = sanitize_header(&header);
        assert!(!out.chars().any(norte_encoding::is_terminal_hazard));
    }

    #[test]
    fn sanitize_cell_none_becomes_none() {
        assert_eq!(sanitize_cell(None), None);
    }

    /// Truncates after masking, and MARKS the cut.
    ///
    /// The mark is not cosmetic: the attributes sheet exists to see the
    /// whole value, and without it a truncated value and a complete one
    /// paint identically.
    #[test]
    fn sanitize_cell_truncates_after_masking_and_marks_the_cut() {
        let long = "a".repeat(1000);
        let out = sanitize_cell(Some(&long)).unwrap();
        assert!(out.ends_with('…'), "the cut is visible: {out}");
        assert_eq!(
            out.chars().count(),
            COLUMN_VALUE_MAX_CHARS + 1,
            "the cap's characters plus the mark"
        );

        // One that fits EXACTLY is not marked: there is nothing cut to say.
        let exact = "a".repeat(COLUMN_VALUE_MAX_CHARS);
        let out = sanitize_cell(Some(&exact)).unwrap();
        assert_eq!(out, exact);
    }

    #[test]
    fn sanitize_cell_empty_after_masking_is_none() {
        assert_eq!(sanitize_cell(Some("")), None);
    }

    #[test]
    fn sanitize_column_values_positional_with_none_interleaved() {
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
            "an empty string after masking does not enter the map"
        );
    }

    #[test]
    fn sanitize_column_values_different_lengths_does_not_panic() {
        let paths = vec![vp("mem:///a.rs"), vp("mem:///b.rs")];
        let values = vec![Some("x".to_owned())];
        let map = sanitize_column_values(&paths, &values);
        assert_eq!(map.len(), 1);
    }
}

// ---------------------------------------------------------------------------
// #108 block 3.2: the shared columns MODEL (spec 2026-07-24, L1). Types +
// layout + formatters; catalog/config/picker arrive in blocks 4/6/7. All
// pure: the frontends only paint (rule 7).
// ---------------------------------------------------------------------------

/// A built-in column, derived from the `Entry` as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// The name (last segment). Never dropped in the layout.
    Name,
    /// `Entry.size`.
    Size,
    /// `Entry.mtime_ms`.
    Mtime,
    /// `Entry.kind`, as localized text.
    Kind,
}

/// A column's identity (#108): built-in, provider attribute
/// (`attr:posix.mode`, block 2) or plugin column
/// (`plugin:git-status/branch`, ADR 0037). A STABLE config string form via
/// `FromStr`/`Display` (pinned round-trip).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnId {
    /// Built-in (`"name"`, `"size"`, `"mtime"`, `"kind"`).
    Builtin(Builtin),
    /// A provider attribute by namespaced id (`"attr:<id>"`).
    Attr(String),
    /// A plugin column (`"plugin:<plugin>/<column>"`).
    Plugin {
        /// The plugin's reverse-DNS id.
        plugin: String,
        /// The column's id within the plugin.
        column: String,
    },
}

/// A column id that fails to parse (#108): a DIAGNOSTIC value — never a
/// panic and never a silent drop (the doctor reports it, block 4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid column id: {reason}")]
pub struct ColumnIdError {
    /// Why it fails to parse (NEUTRAL text: it does not interpolate the
    /// user's input — the doctor assembles the full diagnostic with the
    /// source).
    pub reason: &'static str,
}

impl std::str::FromStr for ColumnId {
    type Err = ColumnIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => return Ok(Self::Builtin(Builtin::Name)),
            "size" => return Ok(Self::Builtin(Builtin::Size)),
            "mtime" => return Ok(Self::Builtin(Builtin::Mtime)),
            "kind" => return Ok(Self::Builtin(Builtin::Kind)),
            _ => {}
        }
        if let Some(attr) = s.strip_prefix("attr:") {
            if attr.is_empty() {
                return Err(ColumnIdError {
                    reason: "attr: with no id",
                });
            }
            return Ok(Self::Attr(attr.to_owned()));
        }
        if let Some(rest) = s.strip_prefix("plugin:") {
            let Some((plugin, column)) = rest.split_once('/') else {
                return Err(ColumnIdError {
                    reason: "plugin: with no '/' between plugin and column",
                });
            };
            if plugin.is_empty() || column.is_empty() {
                return Err(ColumnIdError {
                    reason: "plugin: empty id or column",
                });
            }
            return Ok(Self::Plugin {
                plugin: plugin.to_owned(),
                column: column.to_owned(),
            });
        }
        Err(ColumnIdError {
            reason: "neither built-in nor attr:/plugin:",
        })
    }
}

impl std::fmt::Display for ColumnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin(Builtin::Name) => f.write_str("name"),
            Self::Builtin(Builtin::Size) => f.write_str("size"),
            Self::Builtin(Builtin::Mtime) => f.write_str("mtime"),
            Self::Builtin(Builtin::Kind) => f.write_str("kind"),
            Self::Attr(id) => write!(f, "attr:{id}"),
            Self::Plugin { plugin, column } => write!(f, "plugin:{plugin}/{column}"),
        }
    }
}

/// A column's width policy (terminal cells).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthPolicy {
    /// Fixed width.
    Fixed(u16),
    /// The current page's widest cell, capped at [`AUTO_CEILING`].
    Auto,
    /// Splits the leftover space by weight, never below `min`.
    Flex {
        /// Floor in cells.
        min: u16,
        /// The split's relative weight.
        weight: u16,
    },
}

/// An `Auto` column's cap (render anti-DoS: a hostile mile-long cell does
/// not steal the pane).
pub const AUTO_CEILING: u16 = 32;

/// The NAME's floor: never dropped and never goes below this.
pub const NAME_MIN: u16 = 10;

/// A cell's alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Left (text).
    Left,
    /// Right (numbers).
    Right,
}

/// How a cell that does not fit is truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncate {
    /// Tail cut off.
    End,
    /// Middle ellipsis (paths/names).
    Middle,
}

/// Size format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeFormat {
    /// Raw digits, no separators — locale-free, stable in snapshots.
    Exact,
    /// Binary (`KiB`/`MiB`), via [`crate::human_bytes`].
    Iec,
    /// Decimal (`kB`/`MB`).
    Si,
    /// Short, binary and with no space (`80K`, `1.3M`, `512B`): never more
    /// than five cells. Not a configuration format: it is the one
    /// [`fitted_columns`] sets when the name needs the room.
    Short,
}

/// Time format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// "2h ago", via Fluent (`col-time-*`). Needs the caller's `now`.
    Relative,
    /// RFC 3339 UTC to the minute (`2026-07-31T09:41Z`) — locale-free.
    Iso,
    /// LOCAL time with the precision the distance calls for (spec
    /// 2026-09-10): `14:02` if today, `09-10 14:02` if this year,
    /// `2025-09-10` if earlier. Numeric on purpose: fits in 11 cells in any
    /// language and compares at a glance. Needs the caller's `now` AND the
    /// zone ([`format_mtime_tz`] to fix it; [`format_mtime_in`] uses the
    /// system's).
    Smart,
    /// [`Self::Smart`]'s in five cells: `14:02` if today, `09-10` if this
    /// year, `2025` if earlier. Like [`SizeFormat::Short`], it is set by
    /// [`fitted_columns`], not configuration.
    Short,
}

/// A POSIX mode word's format (#117).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeFormat {
    /// `-rw-r--r--` (type + rwx, setuid/sticky included).
    Rwx,
    /// Octal (`644`).
    Octal,
}

/// A [`layout`] entry: policy + the page's measurement (for `Auto`) +
/// whether it is the NAME column (never dropped).
#[derive(Debug, Clone, Copy)]
pub struct LayoutItem {
    /// Width policy.
    pub policy: WidthPolicy,
    /// Cells of the widest cell measured on the page (only `Auto` reads it).
    pub measured: u16,
    /// Is it the name column?
    pub is_name: bool,
}

/// Splits `available` cells among the columns (#108 L1, pure and shared by
/// both frontends). `None` = column DROPPED (did not fit). Rules, all
/// pinned:
/// 1. `Fixed` takes its width; `Auto` the measurement capped at
///    [`AUTO_CEILING`]; `Flex` starts from its `min` and the leftover is
///    split by weight.
/// 2. If the total does not fit, columns are dropped from the RIGHTMOST one
///    of LOWEST weight (`Fixed`/`Auto` count as weight 0) until it fits.
/// 3. The NAME is never dropped and never goes below [`NAME_MIN`] (if even
///    that does not fit, it gets `available.max(1)` — with `available == 0`
///    it returns 1: a zero-width name cannot be painted).
/// 4. The sum of returned widths is ≤ `available` EXCEPT for rule 3's name
///    floor exception; every returned width is ≥ 1.
#[must_use]
pub fn layout(available: u16, items: &[LayoutItem]) -> Vec<Option<u16>> {
    debug_assert!(
        items.iter().filter(|it| it.is_name).count() <= 1,
        "at most ONE name column (review m2)"
    );
    let mut alive: Vec<bool> = items.iter().map(|_| true).collect();
    loop {
        // Each alive column's base.
        let base: Vec<u16> = items
            .iter()
            .map(|it| {
                // The name's floor applies under ANY policy (review m2):
                // rule 3's contract is not Flex-only.
                let floor = if it.is_name { NAME_MIN } else { 1 };
                match it.policy {
                    WidthPolicy::Fixed(w) => w.max(floor),
                    WidthPolicy::Auto => it.measured.clamp(floor, AUTO_CEILING.max(floor)),
                    WidthPolicy::Flex { min, .. } => min.max(floor),
                }
            })
            .collect();
        let total: u32 = base
            .iter()
            .zip(&alive)
            .filter(|(_, a)| **a)
            .map(|(w, _)| u32::from(*w))
            .sum();
        if total <= u32::from(available) {
            // Fits: split the leftover among the alive Flex ones by weight.
            let leftover = u32::from(available) - total;
            let total_weight: u64 = items
                .iter()
                .zip(&alive)
                .filter(|(_, a)| **a)
                .map(|(it, _)| match it.policy {
                    WidthPolicy::Flex { weight, .. } => u64::from(weight),
                    _ => 0,
                })
                .sum();
            let mut out = Vec::with_capacity(items.len());
            let mut allotted = 0u64;
            let mut weight_seen = 0u64;
            for (i, it) in items.iter().enumerate() {
                if !alive[i] {
                    out.push(None);
                    continue;
                }
                let extra = match it.policy {
                    WidthPolicy::Flex { weight, .. } if total_weight > 0 => {
                        weight_seen += u64::from(weight);
                        // Cumulative split with no lost remainder. In u64
                        // (review M1): leftover(≤65535) × accumulated
                        // weights (no cap: config/plugins) overflowed u32
                        // with large weights — a panic in debug, garbage
                        // widths in release.
                        let up_to = u64::from(leftover) * weight_seen / total_weight;
                        let e = up_to - allotted;
                        allotted = up_to;
                        e
                    }
                    _ => 0,
                };
                let w = u64::from(base[i]) + extra;
                out.push(Some(u16::try_from(w).unwrap_or(u16::MAX)));
            }
            return out;
        }
        // Does not fit: drop the rightmost one of lowest weight (never the
        // name). If only the name is left, give it all the available space.
        let victim = items
            .iter()
            .enumerate()
            .filter(|(i, it)| alive[*i] && !it.is_name)
            .min_by_key(|(i, it)| {
                let weight = match it.policy {
                    WidthPolicy::Flex { weight, .. } => weight,
                    _ => 0,
                };
                (weight, std::cmp::Reverse(*i))
            })
            .map(|(i, _)| i);
        match victim {
            Some(i) => alive[i] = false,
            None => {
                // Only the name (or nothing) is alive: everything for it.
                return items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| (alive[i] && it.is_name).then_some(available.max(1)))
                    .collect();
            }
        }
    }
}

/// Size by format (#108). `Exact` is raw digits; `Iec` reuses
/// [`crate::human_bytes`]; `Si` decimal with one digit.
#[must_use]
pub fn format_size(n: u64, fmt: SizeFormat) -> String {
    match fmt {
        SizeFormat::Exact => n.to_string(),
        SizeFormat::Iec => crate::human_bytes(n),
        SizeFormat::Si => {
            const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
            if n < 1000 {
                return format!("{n} B");
            }
            #[expect(clippy::cast_precision_loss, reason = "magnitudes far from 2^53")]
            let mut value = n as f64 / 1000.0;
            let mut unit = 0usize;
            while (value * 10.0).round() >= 10000.0 && unit + 1 < UNITS.len() {
                value /= 1000.0;
                unit += 1;
            }
            format!("{value:.1} {}", UNITS[unit])
        }
        SizeFormat::Short => short_size(n),
    }
}

/// [`SizeFormat::Short`]: `512B`, `9.5K`, `80K`, `1.3M`. One decimal digit
/// only below 10, which is where the reading changes; rounding that reaches
/// 1000 bumps the unit, so it never goes past five cells.
fn short_size(n: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if n < 1024 {
        return format!("{n}B");
    }
    #[expect(clippy::cast_precision_loss, reason = "magnitudes far from 2^53")]
    let mut value = n as f64 / 1024.0;
    let mut unit = 0usize;
    while value.round() >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if (value * 10.0).round() < 100.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

/// Time by format (#108). `now_ms` is supplied by the caller (a pure fn —
/// testable and stable in snapshots); negative pre-1970 values are valid.
///
/// In the AMBIENT language. A wrapper over [`format_mtime_in`] for whoever
/// has no `lang` to pass; **a window always has one**, and calling this one
/// from it painted every date cell in the listing in the PROCESS's
/// language, under a header in the host's.
#[must_use]
pub fn format_mtime(mtime_ms: i64, fmt: TimeFormat, now_ms: i64) -> String {
    format_mtime_in(mtime_ms, fmt, now_ms, norte_i18n::active())
}

/// [`format_mtime`] in a GIVEN language.
///
/// The relative branch is the one that translates, and it cannot be dodged
/// with configuration: the window ignores `time-format`, so it is always
/// live.
#[must_use]
pub fn format_mtime_in(
    mtime_ms: i64,
    fmt: TimeFormat,
    now_ms: i64,
    lang: norte_i18n::Lang,
) -> String {
    format_mtime_tz(mtime_ms, fmt, now_ms, lang, &jiff::tz::TimeZone::system())
}

/// [`format_mtime_in`] with the given ZONE — the one [`TimeFormat::Smart`]
/// needs to know what "today" is. The frontends pass the system's; tests,
/// a fixed one, so a snapshot does not depend on the machine.
#[must_use]
pub fn format_mtime_tz(
    mtime_ms: i64,
    fmt: TimeFormat,
    now_ms: i64,
    lang: norte_i18n::Lang,
    tz: &jiff::tz::TimeZone,
) -> String {
    match fmt {
        TimeFormat::Iso => iso_utc_minutes(mtime_ms),
        TimeFormat::Smart => smart_local(mtime_ms, now_ms, tz),
        TimeFormat::Short => short_local(mtime_ms, now_ms, tz),
        TimeFormat::Relative => {
            let delta_s = (now_ms.saturating_sub(mtime_ms)) / 1000;
            if delta_s < 60 {
                return norte_i18n::t_in(lang, "col-time-now");
            }
            let (n, key) = if delta_s < 3600 {
                (delta_s / 60, "col-time-min")
            } else if delta_s < 86_400 {
                (delta_s / 3600, "col-time-hour")
            } else if delta_s < 365 * 86_400 {
                (delta_s / 86_400, "col-time-day")
            } else {
                (delta_s / (365 * 86_400), "col-time-year")
            };
            norte_i18n::ta_in(lang, key, &[("n", &n.to_string())])
        }
    }
}

/// [`TimeFormat::Smart`]: local time with the precision the distance calls
/// for. An instant outside `jiff`'s range (±9999 years: garbage mtime from
/// a hostile provider) falls back to ISO UTC, which knows how to paint any
/// `i64` — never a panic nor an empty cell.
fn smart_local(mtime_ms: i64, now_ms: i64, tz: &jiff::tz::TimeZone) -> String {
    let (Ok(ts), Ok(now)) = (
        jiff::Timestamp::from_millisecond(mtime_ms),
        jiff::Timestamp::from_millisecond(now_ms),
    ) else {
        return iso_utc_minutes(mtime_ms);
    };
    let z = ts.to_zoned(tz.clone());
    let n = now.to_zoned(tz.clone());
    if z.date() == n.date() {
        format!("{:02}:{:02}", z.hour(), z.minute())
    } else if z.year() == n.year() {
        format!(
            "{:02}-{:02} {:02}:{:02}",
            z.month(),
            z.day(),
            z.hour(),
            z.minute()
        )
    } else {
        format!("{:04}-{:02}-{:02}", z.year(), z.month(), z.day())
    }
}

/// [`TimeFormat::Short`]: the part of [`smart_local`] that distinguishes
/// that distance, and nothing more. Outside `jiff`'s range, the ISO's year.
fn short_local(mtime_ms: i64, now_ms: i64, tz: &jiff::tz::TimeZone) -> String {
    let (Ok(ts), Ok(now)) = (
        jiff::Timestamp::from_millisecond(mtime_ms),
        jiff::Timestamp::from_millisecond(now_ms),
    ) else {
        // The ISO's YEAR, with its sign: `iso_utc_minutes` puts the sign
        // outside the width, and cutting to four characters left `-000`
        // for any negative year.
        let iso = iso_utc_minutes(mtime_ms);
        let end = iso
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '-')
            .map_or(iso.len(), |(i, _)| i);
        return iso[..end].to_owned();
    };
    let z = ts.to_zoned(tz.clone());
    let n = now.to_zoned(tz.clone());
    if z.date() == n.date() {
        format!("{:02}:{:02}", z.hour(), z.minute())
    } else if z.year() == n.year() {
        format!("{:02}-{:02}", z.month(), z.day())
    } else {
        format!("{:04}", z.year())
    }
}

/// RFC 3339 UTC to the minute, with no external calendar dependency: a
/// civil-days algorithm (Howard Hinnant) over the epoch. Pinned against
/// known dates, negatives included.
fn iso_utc_minutes(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (hour, min) = (sod / 3600, (sod % 3600) / 60);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year_base = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_shift = (5 * doy + 2) / 153;
    let day = doy - (153 * month_shift + 2) / 5 + 1;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    };
    let year = if month <= 2 { year_base + 1 } else { year_base };
    // Negative years (garbage mtime from a corrupt file): expanded ISO 8601
    // form `-0005-…` — plain `{:04}` would count the sign inside the width
    // (review m4).
    if year < 0 {
        format!(
            "-{:04}-{month:02}-{day:02}T{hour:02}:{min:02}Z",
            year.unsigned_abs()
        )
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}Z")
    }
}

/// A POSIX mode in octal (`0644`) — for block 2 (attrs); it lives here so
/// the formatters are born together and tested.
#[must_use]
pub fn format_mode_octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// A POSIX mode, `ls`-style (`-rw-r--r--`).
///
/// The first letter is the node's CLASS, with the seven `S_IFMT` defines
/// and an eighth it does not define: `?`.
///
/// The `?` is the encoding audit's finding, and it is the one that matters.
/// A mode whose type bits are zero —what an SFTP server that only reports
/// permissions sends, and what `MemProvider` emits— is not a regular file:
/// it is a mode that does not say which class it is. Painting it `-` showed
/// a directory as a file on the same row where the icon and the `/` said it
/// was a directory, and of two surfaces that contradict each other, this
/// one was the one lying. `?` is a question the reader can act on; `-` was
/// a wrong answer.
///
/// ```
/// use norte_frontend::columns::format_mode_rwx;
/// assert_eq!(format_mode_rwx(0o100_644), "-rw-r--r--");
/// assert_eq!(format_mode_rwx(0o040_755), "drwxr-xr-x");
/// assert_eq!(format_mode_rwx(0o010_644), "prw-r--r--", "fifo");
/// assert_eq!(format_mode_rwx(0o020_666), "crw-rw-rw-", "character device");
/// assert_eq!(format_mode_rwx(0o060_660), "brw-rw----", "block device");
/// assert_eq!(format_mode_rwx(0o140_755), "srwxr-xr-x", "socket");
/// // With no class bits: it is NOT a regular file, it's a mode that doesn't say.
/// assert_eq!(format_mode_rwx(0o644), "?rw-r--r--");
/// // And always ten cells, whatever the input.
/// assert_eq!(format_mode_rwx(u32::MAX).chars().count(), 10);
/// ```
#[must_use]
pub fn format_mode_rwx(mode: u32) -> String {
    let kind = match mode & 0o170_000 {
        0o140_000 => 's',
        0o120_000 => 'l',
        0o100_000 => '-',
        0o060_000 => 'b',
        0o040_000 => 'd',
        0o020_000 => 'c',
        0o010_000 => 'p',
        _ => '?',
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    // (shift, special bit, letter with x, letter without x): setuid/setgid/
    // sticky like real `ls` (review m5) — a setuid is never painted ordinary.
    for (shift, special, low, up) in [
        (6u32, 0o4000u32, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        out.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let x = bits & 0o1 != 0;
        out.push(match (mode & special != 0, x) {
            (true, true) => low,
            (true, false) => up,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    out
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::sort::{SortColumn, SortDir};

    #[test]
    fn column_widths_defaults_the_set_to_80_cells() {
        let s = ColumnsSettings::default();
        // With no catalog there is no permissions column: the listing sets
        // it, and the listing does not show it until it knows the backend
        // answers it.
        let w = column_widths(&s, "file", 80, None);
        let cols: Vec<ColumnId> = w.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            cols,
            vec![
                ColumnId::Builtin(Builtin::Name),
                ColumnId::Builtin(Builtin::Size),
                ColumnId::Builtin(Builtin::Mtime),
            ]
        );
        // The name absorbs the rest: sum == available.
        assert_eq!(w.iter().map(|(_, x)| *x).sum::<u16>(), 80);
    }

    #[test]
    fn column_widths_narrow_only_name() {
        let s = ColumnsSettings::default();
        let w = column_widths(&s, "file", 12, None);
        assert_eq!(
            w.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            vec![ColumnId::Builtin(Builtin::Name)]
        );
    }

    #[test]
    fn sort_column_maps_sortable_builtins() {
        use crate::sort::SortColumn;
        assert_eq!(sort_column(Builtin::Name), Some(SortColumn::Name));
        assert_eq!(sort_column(Builtin::Size), Some(SortColumn::Size));
        assert_eq!(sort_column(Builtin::Mtime), Some(SortColumn::Mtime));
        assert_eq!(sort_column(Builtin::Kind), None);
    }

    /// ADR 0144: an `attr:` sorts by its id; a `plugin:` still does not sort
    /// (its values arrive after the listing).
    #[test]
    fn sort_column_id_sorts_attributes_and_not_plugins() {
        use crate::sort::SortColumn;
        let attr: ColumnId = "attr:posix.uid".parse().expect("id");
        assert_eq!(
            sort_column_id(&attr),
            Some(SortColumn::Attr("posix.uid".to_owned()))
        );
        let plugin: ColumnId = "plugin:git/status".parse().expect("id");
        assert_eq!(sort_column_id(&plugin), None);
    }

    #[test]
    fn resolve_parses_diagnoses_and_resolves_by_scheme() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "size".into(),
                "rota!!".into(),
                "attr:posix.mode".into(),
            ]),
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Mtime,
                descending: true,
                dirs_first: true,
            }),
            schemes: [(
                "sftp".to_owned(),
                norte_config::SchemeColumns {
                    columns: Some(vec!["name".into(), "kind".into()]),
                    sort: None,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            st.invalid,
            vec!["rota!!".to_owned()],
            "diagnostic, not a silent drop"
        );
        // #117 and follow-up: attr: and plugin: are both painted — the only
        // cap diagnostic present here must be empty.
        assert!(st.plugins_over_cap.is_empty(), "{:?}", st.plugins_over_cap);

        // Default: size + attr → name PREPENDED (never without a name).
        let items = st.layout_items_for("file");
        let cols: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(cols, vec!["name", "size", "attr:posix.mode"]);

        // Scheme: replaces the whole list.
        let items = st.layout_items_for("sftp");
        let cols: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(cols, vec!["name", "kind"]);

        // Sort: global mtime/desc; sftp inherits the global (no override).
        let s = st.sort_for("file");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
        let s = st.sort_for("sftp");
        assert_eq!((s.column, s.dir), (SortColumn::Mtime, SortDir::Desc));
    }

    /// The TWO width tables say the same thing: the stock one
    /// (`default_layout_items`) and the one used by configured
    /// `[ui.columns]` (`builtin_layout_item`). They diverged once —12 and
    /// 10 for the date— and the configured column came out truncated
    /// ("09-10 20:", 2026-09-11).
    #[test]
    fn the_two_width_tables_agree() {
        for (b, item) in default_layout_items() {
            assert_eq!(
                builtin_layout_item(b).policy,
                item.policy,
                "{b:?}: `builtin_layout_item` differs from `default_layout_items`"
            );
        }
    }

    #[test]
    fn layout_items_normalizes_name_to_the_front() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["size".into(), "name".into()]),
            ..Default::default()
        };
        let s = ColumnsSettings::resolve(&cfg);
        let items = s.layout_items_for("file");
        assert_eq!(items[0].0, ColumnId::Builtin(Builtin::Name));
        assert_eq!(items[1].0, ColumnId::Builtin(Builtin::Size));
    }

    #[test]
    fn with_no_config_everything_is_default() {
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert_eq!(st.sort_for("file"), crate::sort::SortSpec::default());
        let items = st.layout_items_for("file");
        assert_eq!(items.len(), 4, "name+size+mtime+permissions");
        // And on a scheme with no POSIX permissions, the usual three: the
        // fourth is not placed where the backend cannot answer it.
        assert_eq!(st.layout_items_for("s3").len(), 3, "name+size+mtime");
        assert!(st.invalid.is_empty() && st.plugins_over_cap.is_empty());
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;
    use std::str::FromStr as _;

    #[test]
    fn column_id_round_trip_and_errors() {
        for s in [
            "name",
            "size",
            "mtime",
            "kind",
            "attr:posix.mode",
            "plugin:git-status/branch",
        ] {
            let id = ColumnId::from_str(s).expect(s);
            assert_eq!(id.to_string(), s, "round-trip");
        }
        for bad in [
            "",
            "sise",
            "attr:",
            "plugin:",
            "plugin:solo",
            "plugin:/col",
            "plugin:p/",
        ] {
            assert!(ColumnId::from_str(bad).is_err(), "{bad:?} must fail");
        }
    }

    fn it(policy: WidthPolicy, measured: u16, is_name: bool) -> LayoutItem {
        LayoutItem {
            policy,
            measured,
            is_name,
        }
    }

    #[test]
    fn layout_splits_and_drops_from_the_right() {
        // name flex + size fixed 9 + mtime fixed 12, in 80 cells.
        let items = [
            it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
            it(WidthPolicy::Fixed(9), 0, false),
            it(WidthPolicy::Fixed(12), 0, false),
        ];
        let w = layout(80, &items);
        assert_eq!(w[1], Some(9));
        assert_eq!(w[2], Some(12));
        assert_eq!(w[0], Some(80 - 9 - 12), "the name absorbs the leftover");

        // In 25 cells the three do not fit: the RIGHTMOST one (weight 0) drops.
        let w = layout(25, &items);
        assert_eq!(w[2], None, "the rightmost one of lowest weight drops");
        assert_eq!(w[1], Some(9));
        assert_eq!(w[0], Some(16));

        // In 12 cells only the name survives, with everything.
        let w = layout(12, &items);
        assert_eq!(w, vec![Some(12), None, None]);
    }

    /// Sweep: sum ≤ available, name never dropped, widths ≥ 1.
    #[test]
    fn layout_invariants_under_sweep() {
        let policies = [
            WidthPolicy::Fixed(7),
            WidthPolicy::Auto,
            WidthPolicy::Flex { min: 4, weight: 2 },
        ];
        for avail in [0u16, 1, 5, 10, 20, 40, 79, 80, 200] {
            for p1 in policies {
                for p2 in policies {
                    let items = [
                        it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
                        it(p1, 15, false),
                        it(p2, 3, false),
                    ];
                    let w = layout(avail, &items);
                    assert!(w[0].is_some(), "name alive: avail={avail} {p1:?} {p2:?}");
                    let sum: u32 = w.iter().flatten().map(|x| u32::from(*x)).sum();
                    // The one documented exception: the name's floor can
                    // exceed a tiny available (rule 3).
                    if avail >= NAME_MIN {
                        assert!(sum <= u32::from(avail.max(1)), "sum {sum} > {avail}");
                    }
                    assert!(w.iter().flatten().all(|x| *x >= 1));
                }
            }
        }
    }

    #[test]
    fn format_size_boundaries() {
        assert_eq!(format_size(0, SizeFormat::Exact), "0");
        assert_eq!(
            format_size(u64::MAX, SizeFormat::Exact),
            u64::MAX.to_string()
        );
        assert_eq!(format_size(1536, SizeFormat::Iec), "1.5 KiB");
        assert_eq!(format_size(999, SizeFormat::Si), "999 B");
        assert_eq!(format_size(1000, SizeFormat::Si), "1.0 kB");
        assert_eq!(
            format_size(999_950, SizeFormat::Si),
            "1.0 MB",
            "rounded promotion"
        );
        assert_eq!(format_size(u64::MAX, SizeFormat::Si), "18.4 EB");
    }

    #[test]
    fn iso_utc_known_dates() {
        assert_eq!(iso_utc_minutes(0), "1970-01-01T00:00Z");
        assert_eq!(iso_utc_minutes(1_720_000_000_000), "2024-07-03T09:46Z");
        // Pre-1970 (negative): 1969-12-31 23:59.
        assert_eq!(iso_utc_minutes(-60_000), "1969-12-31T23:59Z");
        // Leap year.
        assert_eq!(iso_utc_minutes(951_782_400_000), "2000-02-29T00:00Z");
    }

    #[test]
    fn posix_modes() {
        assert_eq!(format_mode_octal(0o100_644), "0644");
        assert_eq!(format_mode_rwx(0o100_644), "-rw-r--r--");
        assert_eq!(format_mode_rwx(0o040_755), "drwxr-xr-x");
        assert_eq!(format_mode_rwx(0o120_777), "lrwxrwxrwx");
        // Review m5: setuid/setgid/sticky like ls — never ordinary.
        assert_eq!(format_mode_rwx(0o104_755), "-rwsr-xr-x");
        assert_eq!(format_mode_rwx(0o102_745), "-rwxr-Sr-x");
        assert_eq!(format_mode_rwx(0o041_775), "drwxrwxr-t");
        assert_eq!(format_mode_rwx(0o041_774), "drwxrwxr-T");
    }

    /// The canonical mode corpus, against the formatter (spec 2026-09-20).
    ///
    /// It lives in `norte-testkit` and not here because the column stopped
    /// being optional: the same corpus has to be usable by the local
    /// provider's tests and the window's, and a table copied in three
    /// places drifts apart at the first new mode.
    #[test]
    fn the_mode_corpus_paints_whole() {
        for m in norte_testkit::corpus::posix_modes() {
            let painted = format_mode_rwx(u32::try_from(m.mode).expect("fits in u32"));
            assert_eq!(painted, m.rwx, "{}: {}", m.id, m.why);
            assert_eq!(painted.chars().count(), 10, "{} is not ten wide", m.id);
        }
    }

    /// `no_type_bits`'s twin IS a regular file, and the two must be
    /// distinguishable. That was the bug: both painted `-`.
    #[test]
    fn a_mode_with_no_class_is_not_confused_with_a_file() {
        let corpus = norte_testkit::corpus::posix_modes();
        let sin = corpus
            .iter()
            .find(|m| m.id == "no_type_bits")
            .expect("the corpus brings it");
        let twin = sin.twin.expect("the collision needs two");
        let a = format_mode_rwx(u32::try_from(sin.mode).expect("fits"));
        let b = format_mode_rwx(u32::try_from(twin).expect("fits"));
        assert_ne!(a, b, "a directory over SFTP read as a regular file");
        assert!(a.starts_with('?'), "the missing class is a QUESTION: {a}");
        assert!(b.starts_with('-'), "the one that is there is asserted: {b}");
    }

    /// Review m3: available=0 → the name gets 1 (unpaintable at 0), the
    /// documented exception to rule 3/4. And m1: huge weights do not
    /// overflow (u64).
    #[test]
    fn layout_zero_and_huge_weight_edges() {
        let items = [
            it(WidthPolicy::Flex { min: 10, weight: 1 }, 0, true),
            it(WidthPolicy::Fixed(9), 0, false),
        ];
        assert_eq!(layout(0, &items), vec![Some(1), None]);

        let huge = [
            it(
                WidthPolicy::Flex {
                    min: 10,
                    weight: u16::MAX,
                },
                0,
                true,
            ),
            it(
                WidthPolicy::Flex {
                    min: 4,
                    weight: u16::MAX,
                },
                0,
                false,
            ),
            it(
                WidthPolicy::Flex {
                    min: 4,
                    weight: u16::MAX,
                },
                0,
                false,
            ),
        ];
        let w = layout(u16::MAX, &huge);
        let sum: u32 = w.iter().flatten().map(|x| u32::from(*x)).sum();
        assert!(
            u16::try_from(sum).is_ok(),
            "no overflow from the split: {w:?}"
        );
    }

    /// Review m4: negative year in expanded ISO form, width 4 + sign.
    #[test]
    fn iso_utc_negative_year() {
        // ~ -63_113_904_000_000 ms ≈ year -31 (approx); pins the FORMAT.
        let s = iso_utc_minutes(-63_200_000_000_000);
        assert!(s.starts_with('-'), "{s}");
        let year_part = &s[1..5];
        assert!(year_part.chars().all(|c| c.is_ascii_digit()), "{s}");
    }

    /// `Relative` via Fluent, injected `now`: pure and stable.
    #[test]
    fn format_mtime_relative_and_iso() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let now = 1_720_000_000_000i64;
        assert_eq!(format_mtime(now - 30_000, TimeFormat::Relative, now), "now");
        assert_eq!(
            format_mtime(now - 5 * 60_000, TimeFormat::Relative, now),
            "5m ago"
        );
        assert_eq!(
            format_mtime(now - 3 * 3_600_000, TimeFormat::Relative, now),
            "3h ago"
        );
        // Future (skewed clock): saturates to "now", never a panic.
        assert_eq!(format_mtime(now + 10_000, TimeFormat::Relative, now), "now");
        assert_eq!(format_mtime(now, TimeFormat::Iso, now), "2024-07-03T09:46Z");
    }

    /// `Smart` (spec 2026-09-10): three precisions depending on distance, in
    /// the GIVEN zone — here UTC+2, so "today" is decided in local time and
    /// not UTC (at 23:30 UTC on July 2nd it is 01:30 on the 3rd in Madrid).
    #[test]
    fn smart_is_local_time_with_three_precisions() {
        let tz = jiff::tz::TimeZone::fixed(jiff::tz::offset(2));
        let en = norte_i18n::Lang::En;
        let now = 1_720_000_000_000; // 2024-07-03T09:46:40Z → 11:46 local
        let f = |ms| format_mtime_tz(ms, TimeFormat::Smart, now, en, &tz);
        assert_eq!(f(now), "11:46", "today: only the time, local");
        // 23:30Z on July 2nd = 01:30 on the 3rd locally: STILL today.
        assert_eq!(f(1_719_963_000_000), "01:30");
        // 21:30Z on July 2nd = 23:30 on the 2nd: yesterday → month-day and time.
        assert_eq!(f(1_719_955_800_000), "07-02 23:30");
        // A different year: only the date.
        assert_eq!(f(951_782_400_000), "2000-02-29");
        assert!(f(now).len() <= 11 && f(1_719_955_800_000).len() <= 11);
        // Outside jiff's range: falls back to ISO UTC, which paints anything.
        assert!(f(i64::MIN).ends_with('Z'));
        assert!(f(i64::MAX).ends_with('Z'));
    }

    /// #117 encoding-audit L3: EXTREME times (garbage mtime from a hostile
    /// provider) — never a panic, always a well-shaped string.
    #[test]
    fn extreme_times_with_no_panic_and_a_shape() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        // ISO at both ends of the range: RFC-3339 form (expanded year on
        // the negative side), never empty.
        let min = iso_utc_minutes(i64::MIN);
        assert!(min.starts_with('-') && min.ends_with('Z'), "{min}");
        let max = iso_utc_minutes(i64::MAX);
        assert!(max.ends_with('Z') && max.contains('T'), "{max}");
        // Relative with a saturating delta in both directions: remote past =
        // years; remote future (negative delta) = "now".
        let past = format_mtime(i64::MIN, TimeFormat::Relative, i64::MAX);
        assert!(past.contains('y'), "{past}");
        assert_eq!(
            format_mtime(i64::MAX, TimeFormat::Relative, i64::MIN),
            "now"
        );
    }
}

/// The DEFAULT column set (#108 L4): `name`, `size`, `mtime` with the
/// formats from the spec's hint (size → iec/right, mtime → relative).
/// Block 4 (config `[ui.columns]`) will replace it with the user's; until
/// then both frontends paint this.
#[must_use]
pub fn default_layout_items() -> Vec<(Builtin, LayoutItem)> {
    vec![
        (
            Builtin::Name,
            LayoutItem {
                policy: WidthPolicy::Flex { min: 10, weight: 1 },
                measured: 0,
                is_name: true,
            },
        ),
        // Non-name columns' widths INCLUDE their separator (1 cell to the
        // left): the layout budgets the row's TOTAL width — without this,
        // the last column overflowed the pane and the terminal truncated
        // it. 11 = "1023.9 GiB" (10) + separator; 12 = "09-10 14:02" (11,
        // this year's `Smart`) + separator — "364d ago" (9) fits with room
        // to spare.
        (
            Builtin::Size,
            LayoutItem {
                policy: WidthPolicy::Fixed(11),
                measured: 0,
                is_name: false,
            },
        ),
        (
            Builtin::Mtime,
            LayoutItem {
                policy: WidthPolicy::Fixed(12),
                measured: 0,
                is_name: false,
            },
        ),
    ]
}

/// The attribute that carries an entry's POSIX mode, and that the
/// PERMISSIONS column paints as `drwxr-xr-x` (spec 2026-09-20).
///
/// It is the same id `norte-vfs-local` and `norte-vfs-sftp` announce in
/// their attribute catalog; it is written here once so the default column
/// and its ladder rung name the SAME thing.
pub const POSIX_MODE_ATTR: &str = "posix.mode";

/// The schemes whose listing shows the permissions column WITHOUT anyone
/// asking for it (spec 2026-09-20): the two that have real POSIX
/// permissions.
///
/// The list is short and explicit on purpose. The alternative —always
/// showing it and leaving it empty where the backend does not answer—
/// spends the name's width on an object bucket or inside a `.zip` to say
/// nothing, which is exactly what ADR 0124 came to fix.
const SCHEMES_WITH_PERMISSIONS: &[&str] = &["file", "sftp"];

/// A custom header's cap (#108 7b), in characters AFTER masking.
pub const HEADER_MAX_CHARS: usize = 24;

/// A column's RESOLVED style (#108 7b): what the spec fixes plus the
/// builtin's defaults. `header` arrives ALREADY sanitized and capped at
/// [`HEADER_MAX_CHARS`] — the only choke point is
/// [`ColumnsSettings::resolve`], never the render (which runs per frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnStyle {
    /// Size format (only `Size` reads it).
    pub size_format: SizeFormat,
    /// Time format (only `Mtime` reads it).
    pub time_format: TimeFormat,
    /// Mode format (only attr cells with hint `Mode` read it).
    pub mode_format: ModeFormat,
    /// Catalog hint for attr columns (`Opaque` if there is no catalog or
    /// the column is builtin — builtins do not read it).
    pub hint: norte_proto::attrs::AttrHint,
    /// Effective alignment.
    pub align: Align,
    /// Custom header (sanitized, ≤ [`HEADER_MAX_CHARS`]); `None` = the
    /// usual Fluent label.
    pub header: Option<String>,
}

impl ColumnStyle {
    /// The same style in short format, if `compact` (what
    /// [`Fitted::compact`] says). Touches both formats: each column reads
    /// its own.
    #[must_use]
    pub fn compacted(mut self, compact: bool) -> Self {
        if compact {
            self.size_format = SizeFormat::Short;
            self.time_format = TimeFormat::Short;
        }
        self
    }

    /// The builtin's defaults with no spec: iec/relative, name on the left
    /// and the rest on the right — the convention both frontends already
    /// painted.
    #[must_use]
    pub fn default_for(b: Builtin) -> Self {
        Self {
            size_format: SizeFormat::Iec,
            time_format: TimeFormat::Relative,
            mode_format: ModeFormat::Rwx,
            hint: norte_proto::attrs::AttrHint::Opaque,
            align: if b == Builtin::Name {
                Align::Left
            } else {
                Align::Right
            },
            header: None,
        }
    }

    /// Defaults for ANY column (#117): builtin = `default_for`; attr =
    /// alignment and hint from the catalog (`Opaque`/left with none,
    /// Size/Timestamp/Mode on the right); plugin = text on the left.
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
                    // `default_for(Kind)` only contributes the non-align
                    // fields (iec/relative/rwx, header None); its align is
                    // overridden.
                    ..Self::default_for(Builtin::Kind)
                }
            }
            ColumnId::Plugin { .. } => Self {
                align: Align::Left,
                ..Self::default_for(Builtin::Kind)
            },
        }
    }
}

/// The SINGLE str ↔ enum table for `Size`'s format vocabulary (#108 7b m3),
/// in the picker's cycle ORDER. Consumed by `format_fits`, `next_format`,
/// `format_name` and `style_for` — there used to be FOUR copies (config
/// aside); block 2 (attrs) extends tables, not scattered matches. The
/// strings must be a subset of the global vocabulary config accepts
/// (`norte-config/src/load.rs`, the spec's parse) — pinned by a test.
const SIZE_FORMATS: &[(&str, SizeFormat)] = &[
    ("iec", SizeFormat::Iec),
    ("si", SizeFormat::Si),
    ("exact", SizeFormat::Exact),
];

/// str ↔ `Mtime` enum table — same rules as [`SIZE_FORMATS`].
const TIME_FORMATS: &[(&str, TimeFormat)] = &[
    ("relative", TimeFormat::Relative),
    ("iso", TimeFormat::Iso),
    ("smart", TimeFormat::Smart),
];

/// [`TimeFormat`] of a `[ui] date_format` (spec 2026-09-10).
#[must_use]
pub fn time_format_of(f: norte_config::DateFormat) -> TimeFormat {
    match f {
        norte_config::DateFormat::Smart => TimeFormat::Smart,
        norte_config::DateFormat::Relative => TimeFormat::Relative,
        norte_config::DateFormat::Iso => TimeFormat::Iso,
    }
}

/// str ↔ enum table for columns with the `Mode` hint — same rules as
/// [`SIZE_FORMATS`]. The strings enter the global config vocabulary in
/// task 4 of #117.
const MODE_FORMATS: &[(&str, ModeFormat)] =
    &[("rwx", ModeFormat::Rwx), ("octal", ModeFormat::Octal)];

/// Does `fmt` (a global vocabulary ALREADY validated in config) match the
/// column? ONLY builtins: `Name`/`Kind` admit no format at all. Attr formats
/// deliberately do NOT go through here (#117): they dispatch by the
/// catalog's hint when folding (`style_for_id`) and a word that does not
/// match keeps the default — do not extend this function for attrs.
fn format_fits(b: Builtin, fmt: &str) -> bool {
    match b {
        Builtin::Size => SIZE_FORMATS.iter().any(|(s, _)| *s == fmt),
        Builtin::Mtime => TIME_FORMATS.iter().any(|(s, _)| *s == fmt),
        Builtin::Name | Builtin::Kind => false,
    }
}

/// The picker cycle's next format (#108 7b): rotates the builtin's table;
/// `None` = the column admits no format (name/kind) or `current` is not in
/// the table.
#[must_use]
pub fn next_format(b: Builtin, current: &str) -> Option<&'static str> {
    fn advance<T>(tab: &'static [(&'static str, T)], cur: &str) -> Option<&'static str> {
        let i = tab.iter().position(|(s, _)| *s == cur)?;
        Some(tab[(i + 1) % tab.len()].0)
    }
    match b {
        Builtin::Size => advance(SIZE_FORMATS, current),
        Builtin::Mtime => advance(TIME_FORMATS, current),
        Builtin::Name | Builtin::Kind => None,
    }
}

/// The picker cycle's next format for ANY column (#117): builtin by its
/// table; attr by its hint's table (Size/Timestamp/Mode); the rest admit no
/// format.
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

/// The str-name of the current format for any column (#117) — the picker's
/// seed, enum → str direction, by the style's OWN hint in attrs
/// (`style.hint`: a single source, no param that could get out of sync).
#[must_use]
pub fn format_name_id(id: &ColumnId, style: &ColumnStyle) -> Option<&'static str> {
    use norte_proto::attrs::AttrHint;
    match id {
        ColumnId::Builtin(b) => format_name(*b, style),
        ColumnId::Attr(_) => match style.hint {
            AttrHint::Size => SIZE_FORMATS
                .iter()
                .find(|(_, f)| *f == style.size_format)
                .map(|(s, _)| *s),
            AttrHint::Timestamp => TIME_FORMATS
                .iter()
                .find(|(_, f)| *f == style.time_format)
                .map(|(s, _)| *s),
            AttrHint::Mode => MODE_FORMATS
                .iter()
                .find(|(_, f)| *f == style.mode_format)
                .map(|(s, _)| *s),
            _ => None,
        },
        ColumnId::Plugin { .. } => None,
    }
}

/// The str-name of a style's current format (the picker's seed): the same
/// table, in the enum → str direction.
#[must_use]
pub fn format_name(b: Builtin, style: &ColumnStyle) -> Option<&'static str> {
    match b {
        Builtin::Size => SIZE_FORMATS
            .iter()
            .find(|(_, f)| *f == style.size_format)
            .map(|(s, _)| *s),
        Builtin::Mtime => TIME_FORMATS
            .iter()
            .find(|(_, f)| *f == style.time_format)
            .map(|(s, _)| *s),
        Builtin::Name | Builtin::Kind => None,
    }
}

/// RESOLVED columns config (#108 block 4): parsed ids, mapped sort,
/// per-scheme overrides. Ids that fail to parse go to
/// [`ColumnsSettings::invalid`] — they are skipped when painting and
/// `norte doctor` reports them (never a silent drop nor a startup error).
/// Both `attr:` (#117) and `plugin:` (follow-up) are painted through the
/// generalized funnel, each family with its own painted == requested cap.
#[derive(Debug, Clone, Default)]
pub struct ColumnsSettings {
    default_set: Option<Vec<ColumnId>>,
    default_sort: crate::sort::SortSpec,
    /// `[ui] date_format` (spec 2026-09-10): the format for time columns
    /// when no spec sets it. `None` = the usual one (`relative`), which is
    /// what a test or `doctor` `ColumnsSettings` wants: `smart`'s local time
    /// depends on the machine, and a snapshot cannot. The frontends receive
    /// it via [`Self::with_date_format`].
    default_time: Option<TimeFormat>,
    schemes:
        std::collections::BTreeMap<String, (Option<Vec<ColumnId>>, Option<crate::sort::SortSpec>)>,
    /// The RAW `default` list exactly as it came from config (#108 7a): the
    /// picker preserves and re-persists ids that fail to parse or have no
    /// renderer — parallel to `default_set`, [`Self::apply_picked`] keeps
    /// them in step.
    raw_default: Option<Vec<String>>,
    /// Raw lists per scheme; there is an entry only if the scheme
    /// configured `columns` (a sort-only override does not list here).
    /// Parallel to `schemes`.
    raw_schemes: std::collections::BTreeMap<String, Vec<String>>,
    /// Global specs kept at resolve time (#108 7b), ALREADY sanitized:
    /// header masked and capped at [`HEADER_MAX_CHARS`], formats that do
    /// not match their column removed (and diagnosed in
    /// [`Self::bad_specs`]). The single choke point — [`Self::style_for`]
    /// only folds fields.
    specs_global: std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    /// Per-scheme specs, same sanitizing; when folded they WIN over the
    /// global ones field by field.
    specs_schemes: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    >,
    /// The label its MANIFEST gave each plugin column, by column id
    /// (`plugin:acme.git/status`), already sanitized. It does not come from
    /// configuration but from the live plugin catalog, so the frontend
    /// installs it when it receives it ([`Self::apply_plugin_headers`]);
    /// without it the header would show the id, which is what the reader
    /// recognises least.
    plugin_headers: std::collections::BTreeMap<String, String>,
    /// Configured ids that do NOT parse (diagnostic for the doctor).
    pub invalid: Vec<String>,
    /// Configured `plugin:` ids BEYOND the request cap
    /// ([`PLUGIN_COLUMNS_MAX_REQUEST`]) in some list: the funnel does not
    /// paint them and the pane does not request them (painted == requested);
    /// the doctor names them (`columns-plugins-over-cap`).
    pub plugins_over_cap: Vec<String>,
    /// Configured `attr:` ids BEYOND the request cap
    /// ([`norte_proto::ATTRS_MAX_REQUEST`]) in some list (#117 review): the
    /// funnel does not paint them and the pane does not request them —
    /// painted == requested, never a permanently blank column; the doctor
    /// names them (`columns-attrs-over-cap`).
    pub attrs_over_cap: Vec<String>,
    /// `attr:` ids that parse as a column but whose attr id is NOT legal on
    /// the wire (#117 encoding-audit M1,
    /// [`norte_proto::attrs::is_valid_attr_id`]: lowercase with a namespace
    /// — `attr:Posix.Mode` parses and the daemon would still reject it with
    /// -32602, bringing down the WHOLE `fs.list`). The funnel skips them and
    /// the pane does not request them (painted == requested, never a blank
    /// column nor a dead listing); the raw form is preserved for
    /// picker/persist and the doctor names them
    /// (`columns-attr-id-not-wire-safe`).
    pub attrs_not_wire_safe: Vec<String>,
    /// `[[ui.columns.spec]]` specs with an impossible id or a format that
    /// does not match their column (#108 7b): the default is applied and
    /// the doctor reports it (`columns-bad-spec`) — never a silent drop nor
    /// a startup failure.
    pub bad_specs: Vec<String>,
}

impl ColumnsSettings {
    /// Resolves the raw config (#108). Never fails: what is invalid
    /// accumulates as diagnostics.
    #[must_use]
    pub fn resolve(cfg: &norte_config::ColumnsConfig) -> Self {
        let mut out = Self {
            default_sort: map_sort(cfg.sort.as_ref()),
            ..Self::default()
        };
        out.default_set = cfg.default_columns.as_ref().map(|ids| parse_ids(ids));
        out.raw_default.clone_from(&cfg.default_columns);
        for (scheme, sc) in &cfg.schemes {
            let cols = sc.columns.as_ref().map(|ids| parse_ids(ids));
            let sort = sc.sort.as_ref().map(|s| map_sort(Some(s)));
            out.schemes.insert(scheme.clone(), (cols, sort));
            if let Some(raw) = &sc.columns {
                out.raw_schemes.insert(scheme.clone(), raw.clone());
            }
        }
        if let Some(ids) = &cfg.default_columns {
            out.collect_diagnostics(ids);
        }
        for sc in cfg.schemes.values() {
            if let Some(ids) = &sc.columns {
                out.collect_diagnostics(ids);
            }
        }
        // #108 7b: keep the SANITIZED specs here (single choke point) —
        // `style_for` runs per frame and must not sanitize or diagnose again.
        out.specs_global = out.sanitize_specs(&cfg.specs);
        for (scheme, sc) in &cfg.schemes {
            if !sc.specs.is_empty() {
                let sane = out.sanitize_specs(&sc.specs);
                out.specs_schemes.insert(scheme.clone(), sane);
            }
        }
        out
    }

    /// Sanitizes a specs map at resolve time (#108 7b): impossible id →
    /// [`Self::bad_specs`] and dropped; a format that does not match its
    /// builtin → [`Self::bad_specs`] and the field removed (the default
    /// will be applied); header masked ([`sanitize_header`]) and capped at
    /// [`HEADER_MAX_CHARS`] (empty after masking = `None`, falls back to
    /// Fluent). `attr:`/`plugin:` ids are kept as-is (#117): their formats
    /// pass unvalidated here — when folding (`style_for_id`) only
    /// [`MODE_FORMATS`]/[`SIZE_FORMATS`]/[`TIME_FORMATS`]'s words match, an
    /// unknown one keeps the default.
    fn sanitize_specs(
        &mut self,
        specs: &std::collections::BTreeMap<String, norte_config::ColumnSpec>,
    ) -> std::collections::BTreeMap<String, norte_config::ColumnSpec> {
        let mut out = std::collections::BTreeMap::new();
        for (raw, spec) in specs {
            let mut spec = spec.clone();
            match raw.parse::<ColumnId>() {
                Err(_) => {
                    self.push_bad_spec(raw);
                    continue; // impossible id: the whole spec is diagnostic
                }
                Ok(ColumnId::Builtin(b)) => {
                    if let Some(fmt) = spec.format.as_deref()
                        && !format_fits(b, fmt)
                    {
                        self.push_bad_spec(raw);
                        spec.format = None; // default, never the broken spec
                    }
                }
                Ok(ColumnId::Attr(_) | ColumnId::Plugin { .. }) => {}
            }
            if let Some(h) = spec.header.as_deref() {
                let sane: String = sanitize_header(h).chars().take(HEADER_MAX_CHARS).collect();
                spec.header = (!sane.is_empty()).then_some(sane);
            }
            out.insert(raw.clone(), spec);
        }
        out
    }

    /// Adds a spec diagnostic, deduplicated by id (one entry per offending
    /// id, whether it comes from the global map or a scheme).
    fn push_bad_spec(&mut self, raw: &str) {
        if !self.bad_specs.iter().any(|b| b == raw) {
            self.bad_specs.push(raw.to_owned());
        }
    }

    /// The effective style of ANY column in `scheme` (#117): defaults ←
    /// global spec ← the scheme's spec, field by field. The str format is
    /// folded by whichever table contains it (size/time/mode) — in
    /// builtins, resolve already validated the match; in attrs, the hint
    /// decides which field is READ when painting, so folding all three is
    /// harmless.
    #[must_use]
    pub fn style_for_id(
        &self,
        scheme: &str,
        id: &ColumnId,
        catalog: Option<&norte_proto::AttrCatalog>,
    ) -> ColumnStyle {
        let key = id.to_string();
        let mut style = ColumnStyle::default_for_id(id, catalog);
        if let Some(t) = self.default_time {
            style.time_format = t;
        }
        // The manifest's label, BELOW the specs: `[ui.columns] header`
        // still rules, and the id only shows up when there is neither one
        // nor the other.
        if matches!(id, ColumnId::Plugin { .. }) {
            style
                .header
                .clone_from(&self.plugin_headers.get(&key).cloned());
        }
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

    /// Sets the default time format from `[ui] date_format` (spec
    /// 2026-09-10). A column spec still wins.
    #[must_use]
    pub fn with_date_format(mut self, f: norte_config::DateFormat) -> Self {
        self.default_time = Some(time_format_of(f));
        self
    }

    /// [`Self::style_for_id`] for a builtin — the historical signature (7b).
    #[must_use]
    pub fn style_for(&self, scheme: &str, builtin: Builtin) -> ColumnStyle {
        self.style_for_id(scheme, &ColumnId::Builtin(builtin), None)
    }

    /// Applies IN MEMORY a format chosen in the picker (#108 7b): updates
    /// (or creates) the retained `specs_global` entry for `id` — the same
    /// session↔disk lockstep as [`Self::apply_picked`] against
    /// `persist_column_format`, so `style_for` sees it instantly without
    /// waiting for the hot-reload. `format` arrives from the picker's closed
    /// vocabulary (already matches its column): no re-sanitizing. A spec of
    /// the SAME id at the scheme level still wins (per-scheme persistence =
    /// deferred).
    pub fn apply_format(&mut self, id: &str, format: &str) {
        self.specs_global.entry(id.to_owned()).or_default().format = Some(format.to_owned());
    }

    /// Installs the labels MANIFESTS give plugin columns (spec 2026-09-11):
    /// `{ "plugin:acme.git/status": "Status" }`, already sanitized by
    /// whoever received them from the catalog. Both frontends call here with
    /// what their column probe already requests, so the header is named the
    /// same in both — `[ui.columns] header` still ranks above it.
    ///
    /// Only ADDS: a catalog that arrives without a column does not erase its
    /// label, because the catalog is requested in batches and a batch is
    /// not the whole truth.
    pub fn apply_plugin_headers(
        &mut self,
        headers: impl IntoIterator<Item = (String, String)>,
    ) -> bool {
        let mut changed = false;
        for (id, label) in headers {
            if self.plugin_headers.get(&id) != Some(&label) {
                self.plugin_headers.insert(id, label);
                changed = true;
            }
        }
        changed
    }

    /// Applies IN MEMORY a fixed width (spec 2026-09-11, V2: dragging a
    /// header's edge): the counterpart of `persist_column_width`, with the
    /// same session↔disk lockstep as [`Self::apply_format`]. `cells` is
    /// clamped here to `[1, 64]`, the range the loader accepts: a width
    /// outside it written to disk would leave the WHOLE `norte.toml`
    /// unable to load.
    pub fn apply_width(&mut self, id: &str, cells: u16) -> u16 {
        let cells = cells.clamp(1, 64);
        self.specs_global.entry(id.to_owned()).or_default().width =
            Some(norte_config::WidthChoice::Fixed(cells));
        cells
    }

    /// Does a spec AT THE SCHEME level fix `builtin`'s format? (#108 7b m2).
    /// With such an override, cycling in the picker would write the GLOBAL
    /// spec that the scheme's own would keep masking — session and disk
    /// "consistent" but invisible (a lying toast) and leaking into other
    /// schemes: the picker BLOCKS those rows. The retained format is
    /// already validated against its column in [`Self::resolve`].
    #[must_use]
    pub fn format_pinned_by_scheme(&self, scheme: &str, builtin: Builtin) -> bool {
        self.format_pinned_by_scheme_id(scheme, &ColumnId::Builtin(builtin))
    }

    /// [`Self::format_pinned_by_scheme`] for any id (#117).
    #[must_use]
    pub fn format_pinned_by_scheme_id(&self, scheme: &str, id: &ColumnId) -> bool {
        let key = id.to_string();
        self.specs_schemes
            .get(scheme)
            .and_then(|m| m.get(&key))
            .is_some_and(|sp| sp.format.is_some())
    }

    /// Does `id` have a `width` in some spec (global or the scheme's)? It
    /// is what dragging an edge writes, so it means "the user touched it",
    /// and [`fitted_columns`] does not move it.
    #[must_use]
    pub fn width_pinned(&self, scheme: &str, id: &ColumnId) -> bool {
        self.spec_field(scheme, id, |s| s.width.is_some())
    }

    /// The list of columns someone WROTE for `scheme`, if any: the
    /// scheme's if it has one, and otherwise the global one.
    ///
    /// A single source for a question that used to be asked in two places
    /// with the same copied expression (`layout_items_for` and the
    /// predicate below). Two copies that agree today are an invariant held
    /// up by a paste, and the first one touched pulls them apart.
    fn configured_ids(&self, scheme: &str) -> Option<&Vec<ColumnId>> {
        self.schemes
            .get(scheme)
            .and_then(|(c, _)| c.as_ref())
            .or(self.default_set.as_ref())
    }

    /// Does this scheme paint a column list someone WROTE, instead of the
    /// default set? (spec 2026-09-20)
    ///
    /// Asked by [`fitted_columns`]'s ladder —the permissions column yields
    /// room when norte set it and not when the user did— and by the column
    /// selector, which needs to know whether to show it turned on.
    #[must_use]
    pub fn has_user_columns(&self, scheme: &str) -> bool {
        self.configured_ids(scheme).is_some()
    }

    /// Does `id` have a `format` in some spec (global or the scheme's)?
    /// Whoever chose a format does not want it swapped for the short one.
    #[must_use]
    pub fn format_pinned(&self, scheme: &str, id: &ColumnId) -> bool {
        self.spec_field(scheme, id, |s| s.format.is_some())
    }

    fn spec_field(
        &self,
        scheme: &str,
        id: &ColumnId,
        field: impl Fn(&norte_config::ColumnSpec) -> bool,
    ) -> bool {
        let key = id.to_string();
        let global = self.specs_global.get(&key);
        let scoped = self.specs_schemes.get(scheme).and_then(|m| m.get(&key));
        [global, scoped].into_iter().flatten().any(field)
    }

    fn collect_diagnostics(&mut self, ids: &[String]) {
        // #117 review: the cap on requestable attrs is PER PAINTED LIST
        // (default or scheme), with the same dedup as `layout_items_for` —
        // a dup does not consume a slot. For an `attr:` the raw form and
        // the Display match, so deduping by raw is enough.
        let mut attrs_seen: Vec<&String> = Vec::new();
        let mut plugins_seen: Vec<&String> = Vec::new();
        for raw in ids {
            match raw.parse::<ColumnId>() {
                Err(_) => {
                    if !self.invalid.contains(raw) {
                        self.invalid.push(raw.clone());
                    }
                }
                // #117-follow-up: `plugin:` ones are ALREADY painted — the
                // only diagnostic left for them is the cap (mirroring attrs).
                Ok(ColumnId::Plugin { .. }) => {
                    if !plugins_seen.contains(&raw) {
                        plugins_seen.push(raw);
                        if plugins_seen.len() > PLUGIN_COLUMNS_MAX_REQUEST
                            && !self.plugins_over_cap.contains(raw)
                        {
                            self.plugins_over_cap.push(raw.clone());
                        }
                    }
                }
                Ok(ColumnId::Attr(a)) => {
                    // #117 encoding-audit M1: an id that parses as a column
                    // but is not legal on the wire is never painted or
                    // requested (`layout_items_for` skips it) — it does not
                    // consume a cap slot, same as it does not consume a
                    // column.
                    if !norte_proto::attrs::is_valid_attr_id(&a) {
                        if !self.attrs_not_wire_safe.contains(raw) {
                            self.attrs_not_wire_safe.push(raw.clone());
                        }
                    } else if !attrs_seen.contains(&raw) {
                        attrs_seen.push(raw);
                        if attrs_seen.len() > norte_proto::ATTRS_MAX_REQUEST
                            && !self.attrs_over_cap.contains(raw)
                        {
                            self.attrs_over_cap.push(raw.clone());
                        }
                    }
                }
                Ok(ColumnId::Builtin(_)) => {}
            }
        }
    }

    /// The order for a pane in `scheme` (#108): the scheme's, or the
    /// global one, or the historical one.
    #[must_use]
    pub fn sort_for(&self, scheme: &str) -> crate::sort::SortSpec {
        self.schemes
            .get(scheme)
            .and_then(|(_, s)| s.clone())
            .unwrap_or_else(|| self.default_sort.clone())
    }

    /// The layout items for a pane in `scheme`, in paint order (#117 and
    /// follow-up: builtins, attrs AND `plugin:` — each family with its own
    /// painted == requested cap). Dedup by id; the name never disappears
    /// nor stops going first. A `[[ui.columns.spec]]`'s `width` (global ←
    /// scheme) REPLACES the item's policy — on any column, attrs included.
    /// A width over `name` is applied but keeps `is_name: true`: the name's
    /// floor rules in [`layout`] (rule 3, [`NAME_MIN`], never dropped)
    /// still win.
    #[must_use]
    pub fn layout_items_for(&self, scheme: &str) -> Vec<(ColumnId, LayoutItem)> {
        let ids = self.configured_ids(scheme);
        let mut out = match ids {
            None => {
                let mut v: Vec<(ColumnId, LayoutItem)> = default_layout_items()
                    .into_iter()
                    .map(|(b, it)| (ColumnId::Builtin(b), it))
                    .collect();
                // The PERMISSIONS column, where there are permissions (spec
                // 2026-09-20). It only goes in the default set: as soon as
                // someone writes their own columns, their list rules, and
                // if they did not name it, they do not want it.
                if SCHEMES_WITH_PERMISSIONS.contains(&scheme) {
                    v.push((
                        ColumnId::Attr(POSIX_MODE_ATTR.to_owned()),
                        attr_layout_item(),
                    ));
                }
                v
            }
            Some(ids) => {
                let mut out: Vec<(ColumnId, LayoutItem)> = Vec::new();
                for id in ids {
                    match id {
                        _ if out.iter().any(|(x, _)| x == id) => {}
                        // #117-follow-up: same treatment as attrs — a
                        // default item (width override by spec for free)
                        // and painted == requested cap (each column is one
                        // RPC).
                        ColumnId::Plugin { .. } => {
                            let plugins = out
                                .iter()
                                .filter(|(x, _)| matches!(x, ColumnId::Plugin { .. }))
                                .count();
                            if plugins < PLUGIN_COLUMNS_MAX_REQUEST {
                                out.push((id.clone(), plugin_layout_item()));
                            }
                        }
                        ColumnId::Builtin(b) => out.push((id.clone(), builtin_layout_item(*b))),
                        // #117 review: at most ATTRS_MAX_REQUEST attrs —
                        // painted == requested (`attr_ids_for`); the rest is
                        // diagnostic (`attrs_over_cap`, the doctor names
                        // it), never a permanently blank column.
                        // Encoding-audit M1: only WIRE-LEGAL ids — an
                        // `attr:Posix.Mode` requested from a daemon would be
                        // -32602 and would bring down the whole listing; it
                        // is skipped here (diagnostic `attrs_not_wire_safe`).
                        ColumnId::Attr(a) => {
                            if norte_proto::attrs::is_valid_attr_id(a) {
                                let attrs = out
                                    .iter()
                                    .filter(|(x, _)| matches!(x, ColumnId::Attr(_)))
                                    .count();
                                if attrs < norte_proto::ATTRS_MAX_REQUEST {
                                    out.push((id.clone(), attr_layout_item()));
                                }
                            }
                        }
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

    /// Folds the specs' `width` (global ← scheme, `Some` wins) onto each
    /// item's policy (#108 7b). `is_name` is not touched: the name's floors
    /// in [`layout`] rule.
    fn apply_width_overrides(&self, scheme: &str, items: &mut [(ColumnId, LayoutItem)]) {
        for (id, item) in items.iter_mut() {
            let key = id.to_string();
            let global = self.specs_global.get(&key).and_then(|s| s.width);
            let scoped = self
                .specs_schemes
                .get(scheme)
                .and_then(|m| m.get(&key))
                .and_then(|s| s.width);
            if let Some(w) = scoped.or(global) {
                item.policy = match w {
                    norte_config::WidthChoice::Auto => WidthPolicy::Auto,
                    norte_config::WidthChoice::Fixed(n) => WidthPolicy::Fixed(n),
                    norte_config::WidthChoice::Flex { min, weight } => {
                        WidthPolicy::Flex { min, weight }
                    }
                };
            }
        }
    }

    /// `scheme`'s CONFIGURED and renderable attr ids (#117): what the pane
    /// requests in `fs.list`. The funnel's dedup; the cap at
    /// [`norte_proto::ATTRS_MAX_REQUEST`] is already applied by
    /// `layout_items_for` (painted == requested) — the `take` here is a
    /// belt (the daemon would reject more).
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

    /// SORTED fingerprint of [`Self::attr_ids_for`] (#117, review task 3):
    /// decides whether a column change requires re-listing. Sorted because a
    /// mere column reorder does not change WHAT values need requesting —
    /// both frontends compare before/after fingerprints with this single
    /// definition.
    #[must_use]
    pub fn attr_fingerprint(&self, scheme: &str) -> Vec<String> {
        let mut ids = self.attr_ids_for(scheme);
        ids.sort_unstable();
        ids
    }

    /// `scheme`'s CONFIGURED and paintable `plugin:` columns
    /// (#117-follow-up), as `(plugin, column)` pairs in paint order — what
    /// the frontend requests via `plugin.column_values` (the wire id is
    /// the bare COLUMN; the plugin validates membership against the
    /// `plugin.list` catalog). Cap already applied by `layout_items_for`
    /// (painted == requested).
    #[must_use]
    pub fn plugin_ids_for(&self, scheme: &str) -> Vec<(String, String)> {
        self.layout_items_for(scheme)
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Plugin { plugin, column } => Some((plugin, column)),
                _ => None,
            })
            .take(PLUGIN_COLUMNS_MAX_REQUEST)
            .collect()
    }

    /// SORTED fingerprint of [`Self::plugin_ids_for`] — mirror of
    /// [`Self::attr_fingerprint`]: decides whether a column change requires
    /// re-requesting plugin values (a reorder does not).
    #[must_use]
    pub fn plugin_fingerprint(&self, scheme: &str) -> Vec<(String, String)> {
        let mut ids = self.plugin_ids_for(scheme);
        ids.sort_unstable();
        ids
    }

    /// A pane's COMBINED attr+plugin fingerprint (#117-follow-up, review
    /// MAJOR-1): the ONE definition of "did what this pane requests
    /// change?" for both frontends — attr ids plus `plugin:` display ids in
    /// sorted form. If it diverged per frontend, one would stop re-listing
    /// on a plugins-only change and the new column would stay permanently
    /// blank.
    #[must_use]
    pub fn pane_fingerprint(&self, scheme: &str) -> Vec<String> {
        let mut ids = self.attr_fingerprint(scheme);
        ids.extend(
            self.plugin_fingerprint(scheme)
                .into_iter()
                .map(|(p, c)| plugin_display_id(&p, &c)),
        );
        ids
    }

    /// `scheme`'s effective CONFIGURED id list in Display form, scheme
    /// override > default > built-in set (#108 7a). Preserves ids with no
    /// renderer and ones that fail to parse: the picker shows them and
    /// re-persists them WHOLE — cleaning up the user's config is not its
    /// job (the doctor reports them).
    #[must_use]
    pub fn raw_ids_for(&self, scheme: &str) -> Vec<String> {
        if let Some(ids) = self.raw_schemes.get(scheme) {
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

    /// Does `scheme` have its own entry in config (columns or sort)?
    /// Decides the picker's TARGET: with an entry, saving writes the
    /// scheme; without it, the default (#108 7a — one rule, said in its
    /// name).
    #[must_use]
    pub fn has_scheme_entry(&self, scheme: &str) -> bool {
        self.schemes.contains_key(scheme)
    }

    /// Applies the picker's result IN MEMORY (#108 7a): the same semantics
    /// as the write-back to disk (`persist_columns`) so the session and the
    /// file do not diverge while the hot-reload arrives. Keeps the raw and
    /// parsed lists in step.
    pub fn apply_picked(
        &mut self,
        target: Option<&str>,
        ids: &[String],
        sort: crate::sort::SortSpec,
    ) {
        // M3 review 7a: the doctor's diagnostics (`invalid`/`unrenderable`)
        // are NOT recomputed here — `collect_diagnostics` only ADDS, never
        // removes stale entries, so calling it here would lie; the
        // hot-reload's re-resolve is what refreshes them honestly.
        // #108 7b: the retained specs maps (`specs_global`/`specs_schemes`)
        // are independent of the column LIST — a spec styles its id
        // "wherever it appears", so choosing columns in the picker does not
        // touch them and there is nothing to keep in step here.
        let parsed = parse_ids(ids);
        if let Some(s) = target {
            self.raw_schemes.insert(s.to_owned(), ids.to_vec());
            self.schemes
                .insert(s.to_owned(), (Some(parsed), Some(sort)));
        } else {
            self.raw_default = Some(ids.to_vec());
            self.default_set = Some(parsed);
            self.default_sort = sort;
        }
    }
}

/// Each builtin's default layout item (#108): the same widths as
/// [`default_layout_items`] — separator INCLUDED in the non-name ones.
#[must_use]
pub fn builtin_layout_item(b: Builtin) -> LayoutItem {
    match b {
        Builtin::Name => LayoutItem {
            policy: WidthPolicy::Flex { min: 10, weight: 1 },
            measured: 0,
            is_name: true,
        },
        Builtin::Size => LayoutItem {
            policy: WidthPolicy::Fixed(11),
            measured: 0,
            is_name: false,
        },
        // 12 = "09-10 14:02" (11, this year's `smart`) + separator. The
        // SAME number as `default_layout_items`: with `[ui.columns]`
        // configured, execution went through here, and here it was still
        // 10 —the date came out "09-10 20:" (2026-09-11). There is a test
        // that ties the two tables together.
        Builtin::Mtime => LayoutItem {
            policy: WidthPolicy::Fixed(12),
            measured: 0,
            is_name: false,
        },
        // Localized "dir"/"file"/"symlink"/"other"; 9 = "symlink"(7)+sep
        // with margin.
        Builtin::Kind => LayoutItem {
            policy: WidthPolicy::Fixed(9),
            measured: 0,
            is_name: false,
        },
    }
}

/// An attr column's default layout item (#117): `Fixed(12)` (separator
/// included) — the fine width is adjusted with the spec's width override,
/// which arrives for free via `apply_width_overrides`.
#[must_use]
pub fn attr_layout_item() -> LayoutItem {
    LayoutItem {
        policy: WidthPolicy::Fixed(12),
        measured: 0,
        is_name: false,
    }
}

/// A `plugin:` column's default layout item (#117-follow-up): the same
/// `Fixed(12)` as attrs — the values are capped at
/// [`COLUMN_VALUE_MAX_CHARS`] at ingest and the fine width is adjusted with
/// the spec's width override.
#[must_use]
pub fn plugin_layout_item() -> LayoutItem {
    LayoutItem {
        policy: WidthPolicy::Fixed(12),
        measured: 0,
        is_name: false,
    }
}

fn parse_ids(ids: &[String]) -> Vec<ColumnId> {
    ids.iter()
        .filter_map(|raw| raw.parse::<ColumnId>().ok())
        .collect()
}

fn map_sort(s: Option<&norte_config::SortChoice>) -> crate::sort::SortSpec {
    use crate::sort::{SortColumn, SortDir, SortSpec};
    let Some(s) = s else {
        return SortSpec::default();
    };
    SortSpec {
        column: SortColumn::from(s.column),
        dir: if s.descending {
            SortDir::Desc
        } else {
            SortDir::Asc
        },
        dirs_first: s.dirs_first,
    }
}

/// Does this listing set the permissions column on its own?
///
/// Three conditions, and all three are necessary:
///
/// 1. nobody wrote their own columns for this scheme — if they did, their
///    list rules, and if they did not name it, they do not want it;
/// 2. the scheme is one of those with POSIX permissions;
/// 3. the provider ANSWERED and says it has `posix.mode`, with the hint
///    that says it is a mode. `None` —it has not answered yet— means no.
///
/// The third requires the hint and not just the id because the id is a
/// name any provider can use for whatever it wants: without checking it,
/// norte would put its translated "Mode" header over a foreign string
/// nobody asked to see. The header is norte's, so the value has to be too.
///
/// It is public because TWO surfaces need the answer and it has to be the
/// same for both: the listing, which paints it, and the column selector,
/// which has to show it turned on — saying it is off while it is painted
/// turns confirming the dialog into erasing it without warning.
#[must_use]
pub fn sets_permission_column(
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> bool {
    !settings.has_user_columns(scheme)
        && SCHEMES_WITH_PERMISSIONS.contains(&scheme)
        && catalog.is_some_and(|c| {
            c.iter()
                .any(|a| a.id == POSIX_MODE_ATTR && a.hint == norte_proto::AttrHint::Mode)
        })
}

/// The columns this listing is going to PAINT: the configured ones, minus
/// the permissions one norte set if the backend cannot answer it.
///
/// A single place where this is decided, because two functions ask it and
/// the answer has to be the same in both (spec 2026-09-20).
fn paintable_items(
    settings: &ColumnsSettings,
    scheme: &str,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(ColumnId, LayoutItem)> {
    let mut set = settings.layout_items_for(scheme);
    // A column the user requested stays even if it comes out empty: it is
    // their choice, and erasing it would be answering them with a no.
    if !settings.has_user_columns(scheme) && !sets_permission_column(settings, scheme, catalog) {
        set.retain(|(id, _)| !matches!(id, ColumnId::Attr(a) if a == POSIX_MODE_ATTR));
    }
    set
}

/// Column widths (#108) for an inner width in CELLS: `(id, width)` of
/// `settings`'s ALIVE columns for `scheme`, in paint order — a column with
/// no room does not appear. Shared TUI/GUI: both frontends paint the SAME
/// set from the SAME [`layout`].
#[must_use]
pub fn column_widths(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
    // Same as in [`fitted_columns`], and for the same reason: two different
    // answers to "what columns are there?" would leave the edge the mouse
    // drags gripping the neighbouring column.
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(ColumnId, u16)> {
    let set = paintable_items(settings, scheme, catalog);
    let items: Vec<_> = set.iter().map(|(_, it)| *it).collect();
    let placed = layout(inner_width, &items);
    set.into_iter()
        .zip(placed)
        .filter_map(|((id, _), w)| w.map(|w| (id, w)))
        .collect()
}

/// A compact column's width, separator included: `1023B`, `1.3M`, `22:19`
/// and `09-16` fit in five cells.
pub const COMPACT_WIDTH: u16 = 6;

/// A column as painted after [`fitted_columns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    /// Which column.
    pub id: ColumnId,
    /// Its width in cells, separator included.
    pub width: u16,
    /// Goes in short format ([`SizeFormat::Short`] / [`TimeFormat::Short`]):
    /// whoever paints applies it to the style with [`ColumnStyle::compacted`].
    pub compact: bool,
}

/// A rung of [`fitted_columns`]'s ladder.
#[derive(Debug, Clone, Copy)]
enum Rung {
    Hide,
    Compact,
}

/// Which column a rung points at. It exists because since spec 2026-09-20
/// the ladder has a rung that is NOT a builtin: the permissions column the
/// listing sets on its own.
#[derive(Debug, Clone, Copy)]
enum Yields {
    Builtin(Builtin),
    /// An attribute, named by its id. Only the one the listing itself adds
    /// by default: one the user requested stays exempt (ADR 0124).
    ByDefault(&'static str),
}

impl Yields {
    fn id(self) -> ColumnId {
        match self {
            Self::Builtin(b) => ColumnId::Builtin(b),
            Self::ByDefault(a) => ColumnId::Attr(a.to_owned()),
        }
    }
}

/// The order in which columns yield room to the name. First what is already
/// said another way —the class is told by the icon, the color and the
/// `/`—, then what can be said shorter, and only at the end what is lost.
///
/// Permissions go FIRST of all, and not for being less useful: it is the
/// column the listing sets with nobody asking for it, so it is the first
/// that has to go when there is no room. ADR 0124 exempts `attr:` columns
/// from the ladder "because someone requested them on purpose"; nobody
/// requested this one, and that is why the exemption does not reach it (see
/// [`ColumnsSettings::has_user_columns`]).
const LADDER: [(Yields, Rung); 6] = [
    (Yields::ByDefault(POSIX_MODE_ATTR), Rung::Hide),
    (Yields::Builtin(Builtin::Kind), Rung::Hide),
    (Yields::Builtin(Builtin::Mtime), Rung::Compact),
    (Yields::Builtin(Builtin::Size), Rung::Compact),
    (Yields::Builtin(Builtin::Mtime), Rung::Hide),
    (Yields::Builtin(Builtin::Size), Rung::Hide),
];

/// Widths that give priority to READING the name.
///
/// [`column_widths`] splits without looking at what is in the directory:
/// the name keeps whatever is left over and, in a narrow pane, that is
/// `Cap….png`. Here the name has a target, `name_wanted` (what the painter
/// measured from the listing, see [`name_width_p80`]), capped at 3/5 of the
/// width so an extremely long name does not eat all the others. Until it
/// arrives, the other columns yield one rung at a time —hide the class,
/// short date, short size, hide the date, hide the size— and it stops as
/// soon as it arrives: a wide pane loses nothing.
///
/// It only yields what the user has not touched: a column with `width` in
/// its spec neither compacts nor hides, one with `format` does not compact,
/// and a name with a fixed width disables the whole ladder. `attr:` and
/// `plugin:` columns are not on the ladder: someone requested them on
/// purpose.
#[must_use]
pub fn fitted_columns(
    settings: &ColumnsSettings,
    scheme: &str,
    inner_width: u16,
    name_wanted: u16,
    // The connected provider's attribute catalog, if already known (spec
    // 2026-09-20). It only decides one thing: whether the listing's own
    // permissions column is painted.
    //
    // `None` —it has not answered yet— means NO. norte set the column, so
    // norte is the one that must not show it empty: a "Mode" header over
    // twelve blank cells is exactly the name width ADR 0124 came to
    // reclaim. Appearing one frame late is cheap; being there unable to say
    // anything is not.
    //
    // And it is not hypothetical: `file` on Windows does not announce
    // `posix.mode`, and that listing never will.
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<Fitted> {
    let name = ColumnId::Builtin(Builtin::Name);
    let ceiling = u16::try_from(u32::from(inner_width) * 3 / 5).unwrap_or(u16::MAX);
    let target = name_wanted.min(ceiling).max(NAME_MIN);
    let free = !settings.width_pinned(scheme, &name);
    let mut set = paintable_items(settings, scheme, catalog);
    let mut alive = vec![true; set.len()];
    let mut short = vec![false; set.len()];
    let mut ladder = LADDER.iter();
    loop {
        let items: Vec<LayoutItem> = set
            .iter()
            .zip(&alive)
            .filter(|(_, v)| **v)
            .map(|((_, it), _)| *it)
            .collect();
        let mut widths = layout(inner_width, &items).into_iter();
        let out: Vec<Fitted> = set
            .iter()
            .enumerate()
            .filter(|(i, _)| alive[*i])
            .filter_map(|(i, (id, _))| {
                widths.next().flatten().map(|width| Fitted {
                    id: id.clone(),
                    width,
                    compact: short[i],
                })
            })
            .collect();
        let has = out.iter().find(|f| f.id == name).map_or(0, |f| f.width);
        if !free || has >= target {
            return out;
        }
        // The next rung that CAN be given; with none left, this is the
        // best that fits.
        loop {
            let Some(&(yields, rung)) = ladder.next() else {
                return out;
            };
            // A rung that points at a default column is not given if this
            // listing paints the columns someone wrote: there, a person
            // requested that column, and ADR 0124 leaves it alone.
            if matches!(yields, Yields::ByDefault(_)) && settings.has_user_columns(scheme) {
                continue;
            }
            let id = yields.id();
            let Some(i) = set.iter().position(|(c, _)| *c == id) else {
                continue;
            };
            if !alive[i] || settings.width_pinned(scheme, &id) {
                continue;
            }
            match rung {
                Rung::Hide => alive[i] = false,
                Rung::Compact => {
                    if short[i] || settings.format_pinned(scheme, &id) {
                        continue;
                    }
                    short[i] = true;
                    set[i].1.policy = WidthPolicy::Fixed(COMPACT_WIDTH);
                }
            }
            break;
        }
    }
}

/// The width that covers 80% of the names, from a width histogram
/// (`counts[w]` = how many names measure `w` cells; the last bucket also
/// counts the wider ones). The 80% and not the max: a single mile-long name
/// must not leave the rest of the listing with no date.
#[must_use]
pub fn name_width_p80(counts: &[u32]) -> u16 {
    let total: u64 = counts.iter().map(|c| u64::from(*c)).sum();
    if total == 0 {
        return 0;
    }
    let goal = (total * 4).div_ceil(5);
    let mut accumulated = 0u64;
    for (w, c) in counts.iter().enumerate() {
        accumulated += u64::from(*c);
        if accumulated >= goal {
            return u16::try_from(w).unwrap_or(u16::MAX);
        }
    }
    u16::try_from(counts.len().saturating_sub(1)).unwrap_or(u16::MAX)
}

/// The sort column that corresponds to a builtin, if it is sortable.
/// `Kind` is not (there is no `SortColumn::Kind`): its header carries no
/// arrow and is not clickable.
#[must_use]
pub fn sort_column(b: Builtin) -> Option<crate::sort::SortColumn> {
    use crate::sort::SortColumn;
    match b {
        Builtin::Name => Some(SortColumn::Name),
        Builtin::Size => Some(SortColumn::Size),
        Builtin::Mtime => Some(SortColumn::Mtime),
        Builtin::Kind => None,
    }
}

/// [`sort_column`] for any id: sortable builtins and `attr:` ones (ADR
/// 0144); `plugin:` ones not.
#[must_use]
pub fn sort_column_id(id: &ColumnId) -> Option<crate::sort::SortColumn> {
    match id {
        ColumnId::Builtin(b) => sort_column(*b),
        // An attribute sorts by its VALUE (ADR 0144): permissions, UID and
        // GID are numbers, and sorting by them groups what paints the same.
        ColumnId::Attr(id) => Some(crate::sort::SortColumn::Attr(id.clone())),
        // A plugin does not: its values do not live in the `Entry` but in
        // the pane's side map, and sorting by something that arrives after
        // the listing would reorder the rows under the cursor while it is
        // being read.
        ColumnId::Plugin { .. } => None,
    }
}

/// A non-name column's cell text (#108 L5, #117 over [`ColumnId`]) with a
/// resolved [`ColumnStyle`] (7b): `None` = absence (a dir with no size, an
/// attr the provider did not send) — painted blank, never a manufactured
/// `0`. `now_ms` is injected by the caller (snapshot stability and
/// purity). `plugin:` ones return `None` HERE on purpose: their values do
/// not live in the `Entry` but in the pane's side-map
/// (`PaneState::plugin_cell`) — the render resolves them through that path.
#[must_use]
pub fn styled_cell(
    entry: &norte_proto::Entry,
    col: &ColumnId,
    now_ms: i64,
    style: &ColumnStyle,
) -> Option<String> {
    styled_cell_in(entry, col, now_ms, style, norte_i18n::active())
}

/// [`styled_cell`] in a GIVEN language.
///
/// Three of its cells translate —the class, a boolean and the relative
/// date— and all three came out in the PROCESS's language when the window
/// was the one painting: half a screen in each language is worse than no
/// translation.
#[must_use]
pub fn styled_cell_in(
    entry: &norte_proto::Entry,
    col: &ColumnId,
    now_ms: i64,
    style: &ColumnStyle,
    lang: norte_i18n::Lang,
) -> Option<String> {
    match col {
        ColumnId::Builtin(b) => match b {
            Builtin::Name => None, // the frontend paints the name
            Builtin::Kind => Some(norte_i18n::t_in(
                lang,
                match entry.kind {
                    norte_proto::EntryKind::Dir => "col-kind-dir",
                    norte_proto::EntryKind::File => "col-kind-file",
                    norte_proto::EntryKind::Symlink => "col-kind-symlink",
                    norte_proto::EntryKind::Other => "col-kind-other",
                },
            )),
            Builtin::Size => entry.size.map(|n| format_size(n, style.size_format)),
            Builtin::Mtime => entry
                .mtime_ms
                .map(|ms| format_mtime_in(ms, style.time_format, now_ms, lang)),
        },
        ColumnId::Attr(id) => entry
            .attrs
            .get(id)
            .and_then(|v| attr_cell(v, style, now_ms, lang)),
        ColumnId::Plugin { .. } => None,
    }
}

/// [`styled_cell`] with the builtin's defaults (iec/relative) — the
/// historical pre-7b signature, identical behaviour (pinned by the
/// existing tests).
#[must_use]
pub fn builtin_cell(entry: &norte_proto::Entry, col: Builtin, now_ms: i64) -> Option<String> {
    styled_cell(
        entry,
        &ColumnId::Builtin(col),
        now_ms,
        &ColumnStyle::default_for(col),
    )
}

/// An attr value's cell (#117): the value's TAG decides (ADR 0039 §1 —
/// never coerced to the declared type); the style's hint refines the
/// numeric ones. Text/Bytes are THIRD-PARTY: masked and capped by
/// [`sanitize_cell`]; Bytes first goes through [`crate::display_name`]'s
/// MARKED lossy conversion (rule 1: the original bytes are not touched).
fn attr_cell(
    v: &norte_proto::AttrValue,
    style: &ColumnStyle,
    now_ms: i64,
    lang: norte_i18n::Lang,
) -> Option<String> {
    use norte_proto::attrs::{AttrHint, AttrValue};
    match v {
        AttrValue::Uint(n) => Some(match style.hint {
            AttrHint::Size => format_size(*n, style.size_format),
            AttrHint::Mode => format_mode(*n, style.mode_format),
            AttrHint::Timestamp => i64::try_from(*n).map_or_else(
                |_| n.to_string(),
                |ms| format_mtime(ms, style.time_format, now_ms),
            ),
            _ => n.to_string(),
        }),
        AttrValue::Int(i) => Some(match style.hint {
            AttrHint::Timestamp => format_mtime(*i, style.time_format, now_ms),
            _ => i.to_string(),
        }),
        AttrValue::TimeMs(ms) => Some(format_mtime(*ms, style.time_format, now_ms)),
        // Present-but-unpaintable (masks to empty) = a visible "?": blank
        // stays RESERVED for ABSENT (#117 review).
        AttrValue::Text(s) => sanitize_cell(Some(s)).or_else(|| Some("?".to_owned())),
        AttrValue::Bytes(b) => {
            let (shown, _hostile) = crate::display_name(b);
            sanitize_cell(Some(&shown)).or_else(|| Some("?".to_owned()))
        }
        AttrValue::Bool(b) => Some(norte_i18n::t_in(
            lang,
            if *b { "col-cell-yes" } else { "col-cell-no" },
        )),
        // A bad cell costs a cell: visible, never blank (blank = ABSENT).
        AttrValue::Unknown => Some("?".to_owned()),
    }
}

/// A POSIX mode by format. A value that does not fit in u32 is not a mode:
/// raw decimal, never a panic nor a silent truncation.
fn format_mode(n: u64, fmt: ModeFormat) -> String {
    match u32::try_from(n) {
        Ok(m) => match fmt {
            ModeFormat::Rwx => format_mode_rwx(m),
            ModeFormat::Octal => format_mode_octal(m),
        },
        Err(_) => n.to_string(),
    }
}

/// Any column's header label (#117), shared TUI/GUI: the spec's custom
/// `header` wins (ALREADY sanitized at resolve time); builtin → Fluent;
/// attr → Fluent by first-party id (`col-attr-posix-mode`), otherwise the
/// catalog's MASKED label, otherwise the sanitized id. `t()` returns the
/// key when it is missing: detected by comparing.
#[must_use]
pub fn header_label(
    id: &ColumnId,
    style: &ColumnStyle,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> String {
    header_label_in(id, style, catalog, norte_i18n::active())
}

/// A column's header in a SPECIFIC language.
///
/// It exists because whoever keeps the language in a field —the window's
/// host, which receives it at start-up— cannot use the global one:
/// [`norte_i18n::t`] reads the process's negotiation, so the attributes
/// sheet's fixed half came out in the requested language and the
/// attributes' headers in the system's, on the same screen.
///
/// [`header_label`] is this with the global language, which is what a
/// terminal wants: there the two always agree.
#[must_use]
pub fn header_label_in(
    id: &ColumnId,
    style: &ColumnStyle,
    catalog: Option<&norte_proto::AttrCatalog>,
    lang: norte_i18n::Lang,
) -> String {
    if let Some(h) = &style.header {
        return h.clone();
    }
    match id {
        ColumnId::Builtin(b) => norte_i18n::t_in(
            lang,
            match b {
                Builtin::Name => "col-header-name",
                Builtin::Size => "col-header-size",
                Builtin::Mtime => "col-header-mtime",
                Builtin::Kind => "col-header-kind",
            },
        ),
        ColumnId::Attr(aid) => {
            // #117 review: the Fluent key is only derived for FIRST-party
            // namespaces — a provider id like `posix-mode` would flatten to
            // the SAME `col-attr-posix-mode` and steal `posix.mode`'s
            // translation.
            const FIRST_PARTY: &[&str] = &["posix.", "win.", "s3.", "archive."];
            if FIRST_PARTY.iter().any(|ns| aid.starts_with(ns)) {
                let key = format!("col-attr-{}", aid.replace(['.', '_'], "-"));
                let loc = norte_i18n::t_in(lang, &key);
                if loc != key {
                    return loc;
                }
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
            sanitize_header(aid)
                .chars()
                .take(HEADER_MAX_CHARS)
                .collect()
        }
        ColumnId::Plugin { plugin, column } => sanitize_header(&format!("{plugin}/{column}"))
            .chars()
            .take(HEADER_MAX_CHARS)
            .collect(),
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;

    /// A plugin column's MANIFEST label is used in the header;
    /// `[ui.columns] header` still wins over it, and with neither of the
    /// two, the id remains, which is what always used to show.
    #[test]
    fn the_manifests_label_names_a_plugin_column() {
        let id: ColumnId = "plugin:acme.git/status".parse().expect("plugin id");
        let mut s = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let label = |s: &ColumnsSettings| {
            header_label_in(
                &id,
                &s.style_for_id("file", &id, None),
                None,
                norte_i18n::Lang::Es,
            )
        };
        assert_eq!(label(&s), "acme.git/status", "with no catalog, the id");

        assert!(
            s.apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Status".to_owned())])
        );
        assert_eq!(label(&s), "Status");
        assert!(
            !s.apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Status".to_owned())]),
            "the same label again is not a change"
        );

        // The user's wins over the manifest's.
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "plugin:acme.git/status".to_owned(),
            norte_config::ColumnSpec {
                header: Some("Git".to_owned()),
                ..Default::default()
            },
        );
        let mut with_spec = ColumnsSettings::resolve(&cfg);
        with_spec
            .apply_plugin_headers([("plugin:acme.git/status".to_owned(), "Status".to_owned())]);
        assert_eq!(label(&with_spec), "Git");
    }

    #[test]
    fn style_for_applies_the_global_spec_and_the_scheme_wins() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                format: Some("si".into()),
                header: Some("Weight".into()),
                width: Some(norte_config::WidthChoice::Fixed(9)),
                ..Default::default()
            },
        );
        let mut sc = norte_config::SchemeColumns::default();
        sc.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                format: Some("exact".into()),
                ..Default::default()
            },
        );
        cfg.schemes.insert("sftp".into(), sc);
        let s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Si
        );
        assert_eq!(
            s.style_for("sftp", Builtin::Size).size_format,
            SizeFormat::Exact
        );
        // The global's header survives on the scheme (last-wins PER FIELD).
        assert_eq!(
            s.style_for("sftp", Builtin::Size).header.as_deref(),
            Some("Weight")
        );
        // The width override reaches the layout.
        let items = s.layout_items_for("file");
        let size = items
            .iter()
            .find(|(id, _)| *id == ColumnId::Builtin(Builtin::Size))
            .expect("size");
        assert_eq!(size.1.policy, WidthPolicy::Fixed(9));
    }

    #[test]
    fn a_format_that_does_not_match_is_diagnostic_not_applied() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "mtime".into(),
            norte_config::ColumnSpec {
                format: Some("iec".into()), // iec on a timestamp: no match
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Mtime).time_format,
            TimeFormat::Relative
        );
        assert!(s.bad_specs.iter().any(|b| b.contains("mtime")));
    }

    #[test]
    fn a_spec_id_that_fails_to_parse_is_diagnostic() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "rota!!".into(),
            norte_config::ColumnSpec {
                header: Some("X".into()),
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        assert!(s.bad_specs.iter().any(|b| b.contains("rota!!")));
    }

    #[test]
    fn a_hostile_spec_header_is_sanitized_and_capped_at_resolve() {
        let mut cfg = norte_config::ColumnsConfig::default();
        cfg.specs.insert(
            "size".into(),
            norte_config::ColumnSpec {
                header: Some(format!("A\u{202E}{}", "x".repeat(60))),
                ..Default::default()
            },
        );
        let s = ColumnsSettings::resolve(&cfg);
        let h = s.style_for("file", Builtin::Size).header.expect("header");
        assert!(!h.chars().any(norte_encoding::is_terminal_hazard));
        assert!(h.chars().count() <= HEADER_MAX_CHARS);
    }

    #[test]
    fn builtin_cell_honours_the_format() {
        let e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("mem:///a.bin").unwrap(),
            kind: norte_proto::EntryKind::File,
            size: Some(2048),
            mtime_ms: Some(0),
        };
        let styled = ColumnStyle {
            size_format: SizeFormat::Exact,
            ..ColumnStyle::default_for(Builtin::Size)
        };
        assert_eq!(
            styled_cell(&e, &ColumnId::Builtin(Builtin::Size), 0, &styled).as_deref(),
            Some("2048")
        );
        // The default wrapper does not change behaviour (Iec).
        assert_eq!(
            builtin_cell(&e, Builtin::Size, 0).as_deref(),
            Some("2.0 KiB")
        );
    }

    #[test]
    fn default_for_alignments() {
        assert_eq!(ColumnStyle::default_for(Builtin::Name).align, Align::Left);
        assert_eq!(ColumnStyle::default_for(Builtin::Size).align, Align::Right);
        assert_eq!(ColumnStyle::default_for(Builtin::Mtime).align, Align::Right);
        assert_eq!(ColumnStyle::default_for(Builtin::Kind).align, Align::Right);
    }

    /// #108 7b: `apply_format` updates the retained spec and `style_for`
    /// sees it instantly (the picker's session↔disk lockstep); an existing
    /// entry keeps its other fields (header).
    #[test]
    fn apply_format_updates_the_style_in_session() {
        let cfg = norte_config::ColumnsConfig {
            specs: [(
                "size".to_owned(),
                norte_config::ColumnSpec {
                    header: Some("Weight".to_owned()),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let mut s = ColumnsSettings::resolve(&cfg);
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Iec
        );
        s.apply_format("size", "si");
        let style = s.style_for("file", Builtin::Size);
        assert_eq!(style.size_format, SizeFormat::Si, "visible instantly");
        assert_eq!(
            style.header.as_deref(),
            Some("Weight"),
            "the spec's other fields survive"
        );
        // With no prior entry: it is created.
        s.apply_format("mtime", "iso");
        assert_eq!(
            s.style_for("file", Builtin::Mtime).time_format,
            TimeFormat::Iso
        );
    }

    /// V2 (spec 2026-09-11): `apply_width` fixes the policy in session, is
    /// capped to the loader's range, and keeps the spec's other fields.
    #[test]
    fn apply_width_fixes_the_policy_in_session_and_caps_it() {
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
        let mut s = ColumnsSettings::resolve(&cfg);
        let policy = |s: &ColumnsSettings| {
            s.layout_items_for("file")
                .into_iter()
                .find(|(id, _)| *id == ColumnId::Builtin(Builtin::Size))
                .map(|(_, item)| item.policy)
        };
        assert_ne!(policy(&s), Some(WidthPolicy::Fixed(12)));
        assert_eq!(s.apply_width("size", 12), 12);
        assert_eq!(policy(&s), Some(WidthPolicy::Fixed(12)));
        assert_eq!(
            s.style_for("file", Builtin::Size).size_format,
            SizeFormat::Si,
            "the spec's format survives"
        );
        assert_eq!(s.apply_width("size", 0), 1, "the loader's floor");
        assert_eq!(s.apply_width("size", 900), 64, "the loader's ceiling");
        assert_eq!(policy(&s), Some(WidthPolicy::Fixed(64)));
    }

    /// m3 review 7b: the frontend's tables' strings are a SUBSET of the
    /// global vocabulary config accepts (`norte-config/src/load.rs`, the
    /// spec's parse: `"exact" | "iec" | "si" | "relative" | "iso" | "octal"
    /// | "rwx"` — hardcoded here because config cannot depend on the
    /// frontend to share the const). A new name in the table with no
    /// config-side counterpart would be a spec impossible to write.
    #[test]
    fn the_format_table_is_a_subset_of_configs_vocabulary() {
        let config_vocab = [
            "exact", "iec", "si", "relative", "iso", "smart", "octal", "rwx",
        ];
        for (s, _) in SIZE_FORMATS {
            assert!(config_vocab.contains(s), "{s} is not in config");
        }
        for (s, _) in TIME_FORMATS {
            assert!(config_vocab.contains(s), "{s} is not in config");
        }
        for (s, _) in MODE_FORMATS {
            assert!(config_vocab.contains(s), "{s} is not in config");
        }
        // And the enum→str direction covers EVERY enum value (a new enum
        // with no row in the table would break the picker's seed).
        for f in [SizeFormat::Exact, SizeFormat::Iec, SizeFormat::Si] {
            let style = ColumnStyle {
                size_format: f,
                ..ColumnStyle::default_for(Builtin::Size)
            };
            assert!(format_name(Builtin::Size, &style).is_some(), "{f:?}");
        }
        for f in [TimeFormat::Relative, TimeFormat::Iso, TimeFormat::Smart] {
            let style = ColumnStyle {
                time_format: f,
                ..ColumnStyle::default_for(Builtin::Mtime)
            };
            assert!(format_name(Builtin::Mtime, &style).is_some(), "{f:?}");
        }
        // #117: Mode's enum→str direction goes through the style's OWN
        // hint (attrs).
        let mode_id = ColumnId::Attr("posix.mode".into());
        for f in [ModeFormat::Rwx, ModeFormat::Octal] {
            let style = ColumnStyle {
                mode_format: f,
                hint: norte_proto::attrs::AttrHint::Mode,
                ..ColumnStyle::default_for_id(&mode_id, None)
            };
            assert!(format_name_id(&mode_id, &style).is_some(), "{f:?}");
        }
    }

    /// #117 review task 4: the THREE format tables are DISJOINT from each
    /// other. `style_for_id`'s fold looks up the word in size→time→mode and
    /// assigns to the first field that matches: a word repeated in two
    /// tables would silently write the wrong field.
    #[test]
    fn the_format_tables_share_no_words() {
        let all: Vec<&str> = SIZE_FORMATS
            .iter()
            .map(|(s, _)| *s)
            .chain(TIME_FORMATS.iter().map(|(s, _)| *s))
            .chain(MODE_FORMATS.iter().map(|(s, _)| *s))
            .collect();
        let unique: std::collections::BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "duplicate word: {all:?}");
    }
}

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
        let mut e = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: norte_proto::VPath::parse("mem:///a.bin").unwrap(),
            kind: norte_proto::EntryKind::File,
            size: Some(1),
            mtime_ms: Some(0),
        };
        for (k, v) in attrs {
            e.attrs.insert((*k).to_owned(), v.clone());
        }
        e
    }

    #[test]
    fn layout_items_for_includes_attrs_and_dedupes() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.mode".into(),
                "attr:mem.mode".into(), // dup: a single column
                "size".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let items = st.layout_items_for("file");
        let ids: Vec<String> = items.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(ids, vec!["name", "attr:mem.mode", "size"]);
        // attr default: Fixed(12), not the name.
        let attr = &items[1].1;
        assert_eq!(attr.policy, WidthPolicy::Fixed(12));
        assert!(!attr.is_name);
    }

    #[test]
    fn attr_ids_for_returns_the_schemes_configured_ones() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.mode".into()]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.attr_ids_for("file"), vec!["mem.mode".to_owned()]);
        // With nothing configured, the only attr requested is the POSIX
        // mode, and only where there are permissions (spec 2026-09-20): the
        // listing itself sets the column, so the listing also pays for its
        // `fs.list` slot.
        let st2 = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        assert_eq!(st2.attr_ids_for("file"), vec![POSIX_MODE_ATTR.to_owned()]);
        assert_eq!(st2.attr_ids_for("sftp"), vec![POSIX_MODE_ATTR.to_owned()]);
        // And where there are none, nothing is requested: a column the
        // backend cannot answer is name width spent on a blank slot.
        assert!(st2.attr_ids_for("s3").is_empty());
        assert!(st2.attr_ids_for("zip").is_empty());
    }

    /// The permissions column norte sets YIELDS room to the name, and the
    /// one the user requested does not (spec 2026-09-20, ADR 0124 amendment).
    ///
    /// This is the whole difference between the two: the ladder exists so a
    /// narrow pane still lets the name be read, and a column nobody
    /// requested cannot be what stops that. One requested by hand can,
    /// because removing it would be disobeying.
    /// A catalog that DOES announce the POSIX mode, like the local
    /// provider's.
    fn catalog_with_mode() -> norte_proto::AttrCatalog {
        use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
        norte_proto::AttrCatalog::new(vec![AttrInfo {
            id: POSIX_MODE_ATTR.into(),
            label: "Mode".into(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        }])
    }

    #[test]
    fn default_permissions_yield_and_requested_ones_do_not() {
        let mode = ColumnId::Attr(POSIX_MODE_ATTR.to_owned());
        let cat = catalog_with_mode();
        // By default: present in a wide pane, and gone in a narrow one.
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        let wide = fitted_columns(&st, "file", 100, 30, Some(&cat));
        assert!(wide.iter().any(|f| f.id == mode), "it fits: shown");
        let narrow = fitted_columns(&st, "file", 40, 30, Some(&cat));
        assert!(
            !narrow.iter().any(|f| f.id == mode),
            "does not fit: the first to yield is the one nobody requested"
        );
        // Requested by hand: it stays, even under pressure.
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "size".into(),
                "mtime".into(),
                format!("attr:{POSIX_MODE_ATTR}"),
            ]),
            ..Default::default()
        };
        let theirs = ColumnsSettings::resolve(&cfg);
        let narrow = fitted_columns(&theirs, "file", 40, 30, Some(&cat));
        assert!(
            narrow.iter().any(|f| f.id == mode),
            "someone requested it on purpose (ADR 0124)"
        );
    }

    /// A backend that does not announce permissions does not spend name
    /// width on a blank "Mode" column — not even while it has not answered.
    ///
    /// It is the honest half of the automatism: setting the column by
    /// scheme is a bet, and `file` on Windows always loses it.
    #[test]
    fn with_no_catalog_or_no_mode_the_column_is_not_set() {
        let mode = ColumnId::Attr(POSIX_MODE_ATTR.to_owned());
        let st = ColumnsSettings::resolve(&norte_config::ColumnsConfig::default());
        // Has not answered yet.
        let f = fitted_columns(&st, "file", 100, 30, None);
        assert!(!f.iter().any(|x| x.id == mode));
        // Answered, and has no POSIX permissions.
        let empty = norte_proto::AttrCatalog::new(vec![]);
        let f = fitted_columns(&st, "file", 100, 30, Some(&empty));
        assert!(!f.iter().any(|x| x.id == mode));
        // But if the user requested it, it stays: empty is THEIR answer.
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), format!("attr:{POSIX_MODE_ATTR}")]),
            ..Default::default()
        };
        let theirs = ColumnsSettings::resolve(&cfg);
        let f = fitted_columns(&theirs, "file", 100, 30, Some(&empty));
        assert!(f.iter().any(|x| x.id == mode));
    }

    /// #117 encoding-audit M1: an `attr:` that parses but is not a
    /// wire-LEGAL id (`is_valid_attr_id` — here a casing typo) is neither
    /// painted nor requested: requested from a daemon it would be -32602
    /// and would bring down the whole `fs.list`. It goes to the diagnostic
    /// and the raw form is preserved for the picker.
    #[test]
    fn a_non_wire_safe_attr_id_is_neither_painted_nor_requested() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:Posix.Mode".into(),
                "attr:mem.mode".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        assert_eq!(st.attr_ids_for("file"), vec!["mem.mode".to_owned()]);
        let painted: Vec<String> = st
            .layout_items_for("file")
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        assert!(
            !painted.contains(&"attr:Posix.Mode".to_owned()),
            "{painted:?}"
        );
        assert_eq!(st.attrs_not_wire_safe, vec!["attr:Posix.Mode".to_owned()]);
        // Not `invalid` (it parses) and the raw form stays for the picker.
        assert!(st.invalid.is_empty());
        assert!(
            st.raw_ids_for("file")
                .contains(&"attr:Posix.Mode".to_owned())
        );
    }

    /// #117 encoding-audit M1, full corpus: a hostile name from the
    /// canonical corpus configured as `attr:<hostile>` never reaches
    /// `attr_ids_for` (the pane does not request it) and is always left
    /// diagnosed.
    ///
    /// **The rule is set by `is_valid_attr_id`, not the fixture list.** The
    /// corpus stopped being hostile-in-every-byte when #129 added folding
    /// TWINS: `strasse.txt` enters for what it means ALONGSIDE
    /// `straße.txt`, and by itself it is an ordinary ASCII name and a
    /// perfectly legal attr id. Accepting it is correct, so the loop
    /// measures against the wire's validator instead of assuming no corpus
    /// name passes — which used to be true by accident and stopped being
    /// so.
    #[test]
    fn a_hostile_attr_id_from_the_corpus_never_reaches_the_wire() {
        let mut rejected = 0;
        for fixture in norte_testkit::corpus::hostile_names() {
            // Only the UTF-8 ones: a column id is a config String.
            let Ok(name) = String::from_utf8(fixture.bytes.clone()) else {
                continue;
            };
            if norte_proto::attrs::is_valid_attr_id(&name) {
                // A benign twin (#129): the funnel accepts it, and should.
                continue;
            }
            rejected += 1;
            let id = format!("attr:{name}");
            let cfg = norte_config::ColumnsConfig {
                default_columns: Some(vec!["name".into(), id.clone()]),
                ..Default::default()
            };
            let st = ColumnsSettings::resolve(&cfg);
            assert!(
                st.attr_ids_for("file").is_empty(),
                "{} would reach the wire",
                fixture.id
            );
            assert!(
                st.attrs_not_wire_safe.contains(&id),
                "{} with no diagnostic",
                fixture.id
            );
        }
        // And the loop has to have measured something: a `continue` that
        // swallowed the whole corpus would leave the test green while
        // proving nothing.
        assert!(
            rejected >= 20,
            "only {rejected} corpus names reached the funnel"
        );
    }

    /// Pin of `attr_fingerprint`'s invariant (#117 review task 3): the
    /// fingerprint is SORTED — reordering columns produces the SAME
    /// fingerprint (no re-list), removing/adding an attr changes it.
    #[test]
    fn attr_fingerprint_has_a_stable_order() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.owner".into(),
                "attr:mem.mode".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let fingerprint = st.attr_fingerprint("file");
        assert_eq!(
            fingerprint,
            vec!["mem.mode".to_owned(), "mem.owner".to_owned()]
        );
        let reordered = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "attr:mem.mode".into(),
                "name".into(),
                "attr:mem.owner".into(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            ColumnsSettings::resolve(&reordered).attr_fingerprint("file"),
            fingerprint,
            "reorder = same fingerprint"
        );
    }

    /// #117-follow-up: `plugin:` ones now have a renderer — they enter the
    /// layout like attrs (default item, width override by spec) and stop
    /// being diagnostic. The `unrenderable` field died with them.
    #[test]
    fn a_plugin_enters_the_layout_as_a_column() {
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:posix.mode".into(),
                "plugin:git/branch".into(),
            ]),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let items = st.layout_items_for("file");
        assert!(
            items.iter().any(|(id, _)| matches!(
                id,
                ColumnId::Plugin { plugin, column } if plugin == "git" && column == "branch"
            )),
            "plugin:git/branch is paintable: {items:?}"
        );
        // Request: which plugin columns to request for the scheme, as
        // (plugin, column) pairs — mirror of `attr_ids_for`.
        assert_eq!(
            st.plugin_ids_for("file"),
            vec![("git".to_owned(), "branch".to_owned())]
        );
        // Order-insensitive fingerprint, mirror of `attr_fingerprint`.
        let reordered = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "plugin:git/branch".into(),
                "name".into(),
                "attr:posix.mode".into(),
            ]),
            ..Default::default()
        };
        assert_eq!(
            ColumnsSettings::resolve(&reordered).plugin_fingerprint("file"),
            st.plugin_fingerprint("file"),
            "reorder = same fingerprint"
        );
    }

    /// #117-follow-up: cap on plugin columns per painted list
    /// ([`PLUGIN_COLUMNS_MAX_REQUEST`]) — painted == requested (each column
    /// is one `plugin.column_values` RPC); the rest is diagnostic, never a
    /// permanently blank column.
    #[test]
    fn plugin_over_cap_is_diagnosed_and_not_painted() {
        let mut ids = vec!["name".to_owned()];
        for i in 0..=PLUGIN_COLUMNS_MAX_REQUEST {
            ids.push(format!("plugin:p/c{i}"));
        }
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(ids),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let painted = st
            .layout_items_for("file")
            .iter()
            .filter(|(id, _)| matches!(id, ColumnId::Plugin { .. }))
            .count();
        assert_eq!(painted, PLUGIN_COLUMNS_MAX_REQUEST);
        assert_eq!(st.plugin_ids_for("file").len(), PLUGIN_COLUMNS_MAX_REQUEST);
        assert_eq!(
            st.plugins_over_cap,
            vec![format!("plugin:p/c{PLUGIN_COLUMNS_MAX_REQUEST}")],
            "the overflow is named, not silenced"
        );
    }

    #[test]
    fn styled_cell_attr_by_the_values_tag_with_a_hint() {
        let cat = catalog();
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.mode".into()]),
            ..Default::default()
        };
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
        // Absent → None (blank), never a manufactured value.
        let empty = entry_with(&[]);
        assert_eq!(styled_cell(&empty, &id, 0, &style), None);
        // Unknown → "?" (a bad cell costs a cell).
        let odd = entry_with(&[("mem.mode", AttrValue::Unknown)]);
        assert_eq!(styled_cell(&odd, &id, 0, &style).as_deref(), Some("?"));
    }

    /// #117 encoding-audit L3: a `Uint` that does not fit in i64 under hint
    /// Timestamp falls back to raw decimal — never a panic nor a time
    /// manufactured by truncation.
    #[test]
    fn an_overflowed_uint_under_timestamp_hint_falls_back_to_decimal() {
        let id = ColumnId::Attr("mem.stamp".into());
        let style = ColumnStyle {
            hint: AttrHint::Timestamp,
            ..ColumnStyle::default_for_id(&id, None)
        };
        let e = entry_with(&[("mem.stamp", AttrValue::Uint(u64::MAX))]);
        assert_eq!(
            styled_cell(&e, &id, 0, &style).as_deref(),
            Some(u64::MAX.to_string().as_str())
        );
    }

    #[test]
    fn hostile_attr_text_and_bytes_are_masked() {
        let id = ColumnId::Attr("mem.note".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let e = entry_with(&[(
            "mem.note",
            AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()),
        )]);
        let cell = styled_cell(&e, &id, 0, &style).expect("cell");
        assert!(
            !cell.chars().any(norte_encoding::is_terminal_hazard),
            "{cell:?}"
        );
        let id2 = ColumnId::Attr("mem.owner".into());
        let e2 = entry_with(&[("mem.owner", AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()))]);
        let cell2 = styled_cell(&e2, &id2, 0, &style).expect("cell");
        assert!(
            !cell2.chars().any(norte_encoding::is_terminal_hazard),
            "{cell2:?}"
        );
        assert!(cell2.contains('\u{FFFD}'), "marked lossy: {cell2:?}");
        // Present-but-unpaintable (masks/truncates to empty) → "?" — blank
        // stays reserved for ABSENT. An all-hostile value does NOT mask to
        // empty (each hazard turns into a visible U+FFFD): the real empty
        // case is the present empty string.
        let empty = entry_with(&[("mem.note", AttrValue::Text(String::new()))]);
        assert_eq!(styled_cell(&empty, &id, 0, &style).as_deref(), Some("?"));
        let empty2 = entry_with(&[("mem.owner", AttrValue::Bytes(Vec::new()))]);
        assert_eq!(styled_cell(&empty2, &id2, 0, &style).as_deref(), Some("?"));
        let hostile = entry_with(&[("mem.note", AttrValue::Text("\u{202e}\u{200b}".into()))]);
        assert_eq!(
            styled_cell(&hostile, &id, 0, &style).as_deref(),
            Some("\u{FFFD}\u{FFFD}"),
            "all-hostile = VISIBLE masked, not empty"
        );
    }

    #[test]
    fn header_label_fluent_catalog_or_masked_id() {
        let cat = catalog();
        // First-party: Fluent key (col-attr-posix-mode exists).
        let id = ColumnId::Attr("posix.mode".into());
        let style = ColumnStyle::default_for_id(&id, None);
        let h = header_label(&id, &style, None);
        assert_ne!(h, "col-attr-posix-mode", "the Fluent key must exist");
        assert!(!h.starts_with("col-attr-"), "{h:?}");
        // Unknown with a catalog: the provider's MASKED label.
        let id2 = ColumnId::Attr("mem.owner".into());
        let h2 = header_label(
            &id2,
            &ColumnStyle::default_for_id(&id2, Some(&cat)),
            Some(&cat),
        );
        assert!(
            !h2.chars().any(norte_encoding::is_terminal_hazard),
            "{h2:?}"
        );
        // Unknown with no catalog: the id (safe charset after sanitize).
        let id3 = ColumnId::Attr("mem.stamp".into());
        let h3 = header_label(&id3, &ColumnStyle::default_for_id(&id3, None), None);
        assert_eq!(h3, "mem.stamp");
        // The spec's custom header ALWAYS wins.
        let mut st = ColumnStyle::default_for_id(&id3, None);
        st.header = Some("Custom".into());
        assert_eq!(header_label(&id3, &st, None), "Custom");
    }

    /// Audit F2 (#117-follow-up): a HOSTILE `plugin:` id from a
    /// project-layer config (RLO/ZWSP parse — `from_str` accepts any
    /// non-empty segment) never reaches the header raw: `header_label`'s
    /// Plugin arm masks it — and neither TUI nor GUI re-mask afterward
    /// (they trust this choke point; a regression here would make the
    /// whole header row in ratatui disappear).
    #[test]
    fn header_label_plugin_masks_a_hostile_id() {
        let id: ColumnId = "plugin:e\u{202E}vil/c\u{200B}ol".parse().expect("parses");
        let h = header_label(&id, &ColumnStyle::default_for_id(&id, None), None);
        assert!(
            !h.chars().any(norte_encoding::is_terminal_hazard),
            "raw hazard in the header: {h:?}"
        );
        assert!(h.contains('\u{FFFD}'), "visible masked: {h:?}");
        assert!(h.contains("vil/c"), "the rest of the id survives: {h:?}");
    }

    #[test]
    fn attr_cell_all_the_tags() {
        let opaque = ColumnStyle::default_for_id(&ColumnId::Attr("x.y".into()), None);
        let e = |v: AttrValue| entry_with(&[("x.y", v)]);
        let id = ColumnId::Attr("x.y".into());
        assert_eq!(
            styled_cell(&e(AttrValue::Uint(42)), &id, 0, &opaque).as_deref(),
            Some("42")
        );
        assert_eq!(
            styled_cell(&e(AttrValue::Int(-5)), &id, 0, &opaque).as_deref(),
            Some("-5")
        );
        assert_eq!(
            styled_cell(&e(AttrValue::Bool(true)), &id, 0, &opaque),
            Some(norte_i18n::t("col-cell-yes"))
        );
        // TimeMs always formats as time, with or without a hint.
        let t = styled_cell(&e(AttrValue::TimeMs(0)), &id, 60_000, &opaque).expect("cell");
        assert!(!t.is_empty());
    }

    /// #117 review: the Mode hint's `octal` format and the decimal fallback
    /// for a word that does not fit in u32 (not a mode — never a panic).
    #[test]
    fn octal_mode_and_overflow_to_decimal() {
        let id = ColumnId::Attr("x.m".into());
        let mut style = ColumnStyle::default_for_id(&id, None);
        style.hint = AttrHint::Mode;
        style.mode_format = ModeFormat::Octal;
        let e = entry_with(&[("x.m", AttrValue::Uint(0o100_644))]);
        assert_eq!(styled_cell(&e, &id, 0, &style).as_deref(), Some("0644"));
        let big = u64::from(u32::MAX) + 1;
        let expected = big.to_string();
        for fmt in [ModeFormat::Rwx, ModeFormat::Octal] {
            style.mode_format = fmt;
            let e2 = entry_with(&[("x.m", AttrValue::Uint(big))]);
            assert_eq!(
                styled_cell(&e2, &id, 0, &style).as_deref(),
                Some(expected.as_str()),
                "{fmt:?}"
            );
        }
    }

    /// #117 review: attrs above [`norte_proto::ATTRS_MAX_REQUEST`] are
    /// neither painted nor requested (painted == requested — no permanently
    /// blank columns) and are left diagnosed.
    #[test]
    fn attrs_over_the_cap_painted_equals_requested() {
        let mut ids: Vec<String> = vec!["name".into()];
        ids.extend((0..17).map(|i| format!("attr:mem.a{i:02}")));
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(ids),
            ..Default::default()
        };
        let st = ColumnsSettings::resolve(&cfg);
        let painted: Vec<String> = st
            .layout_items_for("file")
            .into_iter()
            .filter_map(|(id, _)| match id {
                ColumnId::Attr(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(painted.len(), norte_proto::ATTRS_MAX_REQUEST);
        assert_eq!(painted, st.attr_ids_for("file"), "pintado == pedido");
        assert_eq!(st.attrs_over_cap, vec!["attr:mem.a16".to_owned()]);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn with_kind() -> ColumnsSettings {
        ColumnsSettings::resolve(&norte_config::ColumnsConfig {
            default_columns: Some(
                ["name", "size", "mtime", "kind"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            ..Default::default()
        })
    }

    fn with_spec(id: &str, spec: norte_config::ColumnSpec) -> ColumnsSettings {
        let mut cfg = norte_config::ColumnsConfig {
            default_columns: Some(
                ["name", "size", "mtime", "kind"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            ..Default::default()
        };
        cfg.specs.insert(id.to_owned(), spec);
        ColumnsSettings::resolve(&cfg)
    }

    fn summary(f: &[Fitted]) -> Vec<String> {
        f.iter()
            .map(|c| format!("{}{}", c.id, if c.compact { "~" } else { "" }))
            .collect()
    }

    fn name_width(f: &[Fitted]) -> u16 {
        f.iter()
            .find(|c| c.id == ColumnId::Builtin(Builtin::Name))
            .map_or(0, |c| c.width)
    }

    /// The screenshot that motivated this: a 46-cell pane with class, size
    /// and date left the name with 14. With names of ~20, the class yields
    /// —already said by the icon and the `/`— and nothing else.
    #[test]
    fn the_class_yields_first_and_that_is_enough() {
        let f = fitted_columns(&with_kind(), "file", 46, 20, None);
        assert_eq!(summary(&f), ["name", "size", "mtime"]);
        assert!(name_width(&f) >= 20, "{f:?}");
    }

    /// Long names (screenshots): after the class, the date compacts, and it
    /// stops as soon as the name reaches its capped target.
    #[test]
    fn long_names_compact_the_date() {
        let f = fitted_columns(&with_kind(), "file", 46, 40, None);
        assert_eq!(summary(&f), ["name", "size", "mtime~"]);
        // Target capped at 3/5 of 46 = 27.
        assert!(name_width(&f) >= 27, "{f:?}");
    }

    /// A wide pane loses nothing: the ladder is only climbed when needed.
    #[test]
    fn plenty_of_width_touches_nothing() {
        let f = fitted_columns(&with_kind(), "file", 120, 30, None);
        assert_eq!(summary(&f), ["name", "size", "mtime", "kind"]);
    }

    /// A column with a width the user fixed neither hides nor compacts; the
    /// ladder skips to the next rung.
    #[test]
    fn what_the_user_fixed_does_not_yield() {
        let s = with_spec(
            "kind",
            norte_config::ColumnSpec {
                width: Some(norte_config::WidthChoice::Fixed(9)),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 20, None);
        assert_eq!(summary(&f), ["name", "size", "mtime~", "kind"]);
    }

    /// A chosen format is not swapped for the short one either.
    #[test]
    fn a_chosen_format_does_not_compact() {
        let s = with_spec(
            "mtime",
            norte_config::ColumnSpec {
                format: Some("iso".to_owned()),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 40, None);
        assert_eq!(summary(&f), ["name", "size~", "mtime"]);
    }

    /// With a fixed-width name there is no ladder: it is exactly the usual
    /// split.
    #[test]
    fn a_fixed_name_disables_the_ladder() {
        let s = with_spec(
            "name",
            norte_config::ColumnSpec {
                width: Some(norte_config::WidthChoice::Fixed(12)),
                ..Default::default()
            },
        );
        let f = fitted_columns(&s, "file", 46, 40, None);
        let before: Vec<_> = column_widths(&s, "file", 46, None)
            .into_iter()
            .map(|(id, width)| Fitted {
                id,
                width,
                compact: false,
            })
            .collect();
        assert_eq!(f, before);
    }

    #[test]
    fn p80_ignores_the_long_tail() {
        let mut counts = [0u32; 65];
        counts[8] = 8;
        counts[64] = 2;
        assert_eq!(name_width_p80(&counts), 8);
        assert_eq!(name_width_p80(&[0; 65]), 0);
    }

    #[test]
    fn short_sizes_fit_in_five() {
        for (n, expected) in [
            (0, "0B"),
            (1023, "1023B"),
            (1024, "1.0K"),
            (9 * 1024 + 900, "9.9K"),
            (81_715, "80K"),
            (1_023 * 1024 + 1000, "1.0M"),
            (1_363_149, "1.3M"),
            (u64::MAX, "16E"),
        ] {
            let s = format_size(n, SizeFormat::Short);
            assert_eq!(s, expected, "{n}");
            assert!(s.chars().count() <= 5, "{s}");
        }
    }

    #[test]
    fn short_dates_fit_in_five() {
        let tz = jiff::tz::TimeZone::UTC;
        let now = 1_789_000_000_000; // 2026-09-10 00:26 UTC
        let lang = norte_i18n::Lang::Es;
        let today = format_mtime_tz(now - 60_000, TimeFormat::Short, now, lang, &tz);
        let this_year = format_mtime_tz(now - 40 * 86_400_000, TimeFormat::Short, now, lang, &tz);
        let earlier = format_mtime_tz(now - 400 * 86_400_000, TimeFormat::Short, now, lang, &tz);
        assert_eq!(today, "00:25");
        assert_eq!(this_year, "08-01");
        assert_eq!(earlier, "2025");
    }
}
