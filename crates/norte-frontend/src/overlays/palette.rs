//! Command palette rows shared by TUI and GUI (G3c hoist, mirrors the S3/S4
//! hoist of [`crate::settings`]): the PURE parts of the palette — a plugin
//! command row, its masking, and the visibility filter — don't depend on
//! either frontend's concrete `COMMANDS`/`Effective` wiring (only
//! [`crate::keymap::Effective`], already shared). `build_rows`-proper (one
//! row per BUILT-IN command) stays per-frontend: each frontend's `COMMANDS`
//! list and `help_id` derivation are genuinely different, not incidental
//! duplication.
//!
//! Landed first in `norte-tui/src/palette.rs` (H1 T4); this module is the
//! hoisted twin — TUI now re-exports these names for source compatibility
//! (same pattern [`crate::settings`] already established).

use crate::keymap::Effective;

/// Cosmetic cap on a plugin `description`/`title`'s rendered length
/// (P1 encoding audit F1), in CHARACTERS: a hostile/compromised daemon can
/// send a `description` of unbounded length over the wire (the manifest
/// only bounds it to this many chars on the HONEST path, at parse time) —
/// every consumer of plugin text must clamp it themselves, never trust a
/// caller already did.
pub const PLUGIN_DESCRIPTION_WIRE_CAP: usize = 280;

/// One row of the palette.
///
/// `key` is the internal DISPATCH key, consumed by the caller on Enter —
/// NEVER painted. For a built-in command `key == text` (both trusted:
/// binary constants). For a plugin command row ([`plugin_rows`]) `key`
/// encodes `plugin:{plugin_id}:{command_id}`: `plugin_id` is reverse-DNS,
/// charset-validated by the core (never contains `:`), but `command_id`
/// from the manifest has NO charset validation — it can carry any byte,
/// including `:` or newlines. The FIRST `:` following the `plugin:` +
/// `plugin_id` prefix splits unambiguously (`plugin_id` cannot contain
/// one), and everything remaining — with no further split — is the raw
/// `command_id`. That's why `key` is never painted: `text`/`desc` are the
/// ALREADY-masked view (`crate::display_name`) of the plugin's
/// title/description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Dispatch key — NEVER painted (see the doc above).
    pub key: String,
    /// Text to show (built-in command name, or an already-masked plugin
    /// title).
    pub text: String,
    /// Description to show (Fluent help text, or an already-masked plugin
    /// description).
    pub desc: String,
    /// The real chord, or `"—"` if the command has none bound in this
    /// preset (or, for a plugin command row, always — the palette is its
    /// only entry point).
    pub chord: String,
    /// `text` or `desc` paints DIFFERENTLY from what the manifest declares.
    ///
    /// Always `false` for a built-in row: its text is this project's own
    /// vocabulary. For a plugin row it is the mask flag `display_name`
    /// returned — a masked string travelling without its flag reads as
    /// faithful, and this is the screen where a reader picks which
    /// third-party code to run.
    pub hostile: bool,
}

/// Rows for plugin commands (P1): ONLY approved AND enabled plugins — the
/// same gate `plugin.run_command` (`norte-core`) enforces, so the palette
/// never offers to invoke something the backend would reject. Each
/// [`norte_proto::methods::PluginCommandInfo`] becomes one row, in
/// MANIFEST ORDER (same criterion as the extension manager: never
/// reordered). `text` (title) and `desc` (description, shared by all of a
/// plugin's commands) are THIRD-PARTY text — masked with
/// [`crate::display_name`] BEFORE entering the row (never at paint time,
/// same criterion `first_chord` uses for the chord column here rather than
/// in the keymap engine), and `text` carries the `palette-plugin-prefix`
/// prefix (e.g. `[extension] …`): a hostile plugin could title its command
/// EXACTLY like a built-in (`"app.quit"`) to confuse a human — the prefix,
/// plus the fact a built-in never carries it, breaks the disguise.
#[must_use]
pub fn plugin_rows(plugins: &[norte_proto::methods::PluginInfo]) -> Vec<Row> {
    plugin_rows_in(plugins, norte_i18n::active())
}

/// [`plugin_rows`] in a GIVEN language.
///
/// The prefix is what breaks the disguise of a plugin that titles itself like
/// a built-in command, so it must not come out in a language different from
/// the rest of the palette: that is where it would read as part of the
/// title.
#[must_use]
pub fn plugin_rows_in(
    plugins: &[norte_proto::methods::PluginInfo],
    lang: norte_i18n::Lang,
) -> Vec<Row> {
    plugins
        .iter()
        .filter(|p| p.approved && p.enabled)
        .flat_map(|p| {
            // Defensive cap (P1 encoding audit F1), SAME as any other
            // ingest point for plugin text: `plugin_rows` is public and
            // exercised directly in tests/snapshots with raw data, so it
            // stays self-contained (safe by construction) rather than
            // trusting the caller already clamped.
            let (desc, desc_masked) = p
                .description
                .as_deref()
                .map(|d| {
                    let clamped: String = d.chars().take(PLUGIN_DESCRIPTION_WIRE_CAP).collect();
                    crate::display_name(clamped.as_bytes())
                })
                .unwrap_or_default();
            let plugin_id = p.id.clone();
            p.commands.iter().map(move |c| {
                let (title, masked) = crate::display_name(c.title.as_bytes());
                // A RENAMER (C3, ADR 0095) is another class of row: its key
                // carries a different prefix, because it dispatches to
                // `plugin.rename_plan` and not `plugin.run_command`, and its
                // label says what it does.
                let (prefix, label) = match c.kind {
                    norte_proto::methods::PluginCommandKind::Command => {
                        ("plugin", norte_i18n::t_in(lang, "palette-plugin-prefix"))
                    }
                    norte_proto::methods::PluginCommandKind::Renamer => {
                        ("renamer", norte_i18n::t_in(lang, "palette-renamer-prefix"))
                    }
                    // Phase 8: yet another class, for the same reason — it
                    // dispatches to `plugin.organize_plan`, and its label
                    // says this creates folders and not just renames.
                    norte_proto::methods::PluginCommandKind::Organizer => (
                        "organizer",
                        norte_i18n::t_in(lang, "palette-organizer-prefix"),
                    ),
                };
                Row {
                    key: format!("{prefix}:{plugin_id}:{}", c.id),
                    text: format!("[{label}] {title}"),
                    desc: desc.clone(),
                    chord: "—".to_owned(),
                    hostile: masked || desc_masked,
                }
            })
        })
        .collect()
}

/// Splits a plugin command row's `key` back into `(plugin_id, command_id)`.
///
/// Lives next to [`plugin_rows`], which COMPOSES that key: a format with two
/// homes is a format with two answers about where the `command_id` starts,
/// and the `command_id` is the half with no charset validation.
///
/// The split is at the FIRST `:` after the `plugin:` prefix — `plugin_id` is
/// reverse-DNS and cannot contain one — and everything after it is the raw
/// `command_id`, with no further splitting: it may carry `:`, newlines, or
/// anything else the manifest let through.
///
/// ```
/// use norte_frontend::palette::parse_plugin_key;
///
/// assert_eq!(
///     parse_plugin_key("plugin:org.norte.demo:greet"),
///     Some(("org.norte.demo", "greet"))
/// );
/// // A built-in command is not one of these.
/// assert_eq!(parse_plugin_key("app.quit"), None);
/// // Everything after the first colon is the command id, verbatim.
/// assert_eq!(
///     parse_plugin_key("plugin:org.norte.demo:a:b"),
///     Some(("org.norte.demo", "a:b"))
/// );
/// ```
#[must_use]
pub fn parse_plugin_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, command) = cmd.strip_prefix("plugin:")?.split_once(':')?;
    (!id.is_empty()).then_some((id, command))
}

/// The `(plugin id, renamer id)` of a `renamer:{id}:{renamer}` palette key
/// (C3, ADR 0095), or `None` for anything else — a plugin COMMAND key
/// included: the two dispatch to different methods.
#[must_use]
pub fn parse_renamer_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, renamer) = cmd.strip_prefix("renamer:")?.split_once(':')?;
    (!id.is_empty() && !renamer.is_empty()).then_some((id, renamer))
}

/// The `(plugin id, organizer id)` of an `organizer:{id}:{org}` key (phase 8),
/// or `None` for anything else — a renamer included: the two propose a
/// reviewable plan, but through different methods and with destinations of a
/// different shape.
///
/// ```
/// use norte_frontend::palette::{parse_organizer_key, parse_renamer_key};
///
/// assert_eq!(
///     parse_organizer_key("organizer:org.norte.demo:by-extension"),
///     Some(("org.norte.demo", "by-extension"))
/// );
/// // A renamer is NOT one of these, in either direction.
/// assert_eq!(parse_organizer_key("renamer:org.norte.demo:cleanup"), None);
/// assert_eq!(parse_renamer_key("organizer:org.norte.demo:by-extension"), None);
/// ```
#[must_use]
pub fn parse_organizer_key(cmd: &str) -> Option<(&str, &str)> {
    let (id, organizer) = cmd.strip_prefix("organizer:")?.split_once(':')?;
    (!id.is_empty() && !organizer.is_empty()).then_some((id, organizer))
}

/// The FIRST chord (in `eff.bindings()`'s precedence order) that resolves
/// to `cmd`, if any, ready to PAINT.
///
/// Goes through [`crate::keymap::paint_chord`], the single presentation home
/// for a chord: it masks (render-side duty, encoding audit H1 — `eff` can
/// come from a hostile `./.norte/keymap.toml`, an untrusted PROJECT layer,
/// and `Chord`'s `Display` writes it raw ON PURPOSE for logs) and only then
/// spells it the way the documentation does (`F5`, not `f5`).
#[must_use]
pub fn first_chord(cmd: &str, eff: &Effective) -> Option<String> {
    eff.bindings()
        .into_iter()
        .find(|(_, c)| *c == cmd)
        .map(|(chord, _)| crate::keymap::paint_chord(&chord))
}

/// Filters a full [`Row`] snapshot for an OPEN palette in a given context
/// (MINOR-6, H1 close). `Ctrl+P`/vim `:` live in `[global]`, merged into
/// BOTH effectives (`Screen::Browse` and `Screen::Viewer`) — the palette
/// can open from the viewer too. A `viewer.*` dispatched WITHOUT a viewer
/// open is a silent no-op, so those rows hide when there is none open.
/// Opened FROM the viewer keeps ALL rows — `pane.*` still reaches the
/// focused pane the same way; a symmetric filter (hiding `pane.*` from the
/// viewer) is left for when the palette becomes screen-aware in both
/// directions.
#[must_use]
pub fn rows_for_context(rows: &[Row], viewer_open: bool) -> Vec<Row> {
    if viewer_open {
        rows.to_vec()
    } else {
        rows.iter()
            .filter(|row| !row.key.starts_with("viewer."))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_info(
        id: &str,
        approved: bool,
        enabled: bool,
        description: Option<&str>,
        commands: Vec<(&str, &str)>,
    ) -> norte_proto::methods::PluginInfo {
        norte_proto::methods::PluginInfo {
            id: id.into(),
            name: "N".into(),
            publisher: "p".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: description.map(str::to_owned),
            commands: commands
                .into_iter()
                .map(|(cid, title)| norte_proto::methods::PluginCommandInfo {
                    id: cid.into(),
                    title: title.into(),
                    kind: norte_proto::methods::PluginCommandKind::Command,
                })
                .collect(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    #[test]
    fn plugin_rows_only_approved_and_enabled() {
        let plugins = vec![
            plugin_info("org.a", true, true, None, vec![("greet", "Greet")]),
            plugin_info("org.b", false, true, None, vec![("x", "X")]),
            plugin_info("org.c", true, false, None, vec![("y", "Y")]),
        ];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 1, "only org.a is approved AND enabled");
        assert_eq!(rows[0].key, "plugin:org.a:greet");
    }

    #[test]
    fn plugin_rows_one_row_per_command_in_manifest_order() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            Some("Greets from the palette."),
            vec![("greet", "Greet"), ("wave", "Wave")],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "plugin:org.norte.demo:greet");
        assert!(rows[0].text.contains("Greet"));
        assert_eq!(rows[0].desc, "Greets from the palette.");
        assert_eq!(rows[1].key, "plugin:org.norte.demo:wave");
        assert_eq!(rows[1].chord, "—");
    }

    #[test]
    fn plugin_rows_with_no_description_is_empty_desc() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            None,
            vec![("greet", "Greet")],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows[0].desc, "");
    }

    /// P1 encoding audit F1 (MEDIUM): a hostile/compromised daemon can send
    /// a `description` of ANY length over the wire — the manifest only
    /// bounds it to 280 chars at parse time on the honest path.
    /// `plugin_rows` must clamp the same way, without relying on the
    /// caller having already done it.
    #[test]
    fn plugin_rows_clamps_description_to_the_wire_cap() {
        let long = "a".repeat(10_000);
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            Some(long.as_str()),
            vec![("greet", "Greet")],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(
            rows[0].desc.chars().count(),
            PLUGIN_DESCRIPTION_WIRE_CAP,
            "an unbounded description reached the row raw"
        );
    }

    /// P1, encoding: a hostile plugin title (bidi override, corpus
    /// `rtl_override`) is NEVER painted raw — `text` goes through
    /// `display_name` BEFORE entering the row (same criterion as the
    /// extension manager and `build_rows`' chord column).
    #[test]
    fn plugin_rows_masks_a_hostile_title() {
        let hostile = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("corpus fixture");
        let title = String::from_utf8_lossy(&hostile.bytes).into_owned();
        let plugins = vec![plugin_info(
            "org.evil.x",
            true,
            true,
            None,
            vec![("run", title.as_str())],
        )];
        let rows = plugin_rows(&plugins);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].text.chars().any(norte_encoding::is_terminal_hazard),
            "the RTL override was painted raw: {:?}",
            rows[0].text
        );
        assert!(
            rows[0].text.contains('\u{FFFD}'),
            "the hazard must be masked to U+FFFD: {:?}",
            rows[0].text
        );
        assert_eq!(rows[0].key, "plugin:org.evil.x:run");
    }

    /// The `[extension]` prefix (P1) visually marks a plugin row — a
    /// plugin cannot disguise itself as a built-in by copying its exact
    /// name, because no built-in carries this prefix.
    #[test]
    fn plugin_rows_carry_the_extension_prefix() {
        let plugins = vec![plugin_info(
            "org.norte.demo",
            true,
            true,
            None,
            vec![("greet", "Greet")],
        )];
        let rows = plugin_rows(&plugins);
        assert!(
            rows[0].text.starts_with('['),
            "plugin row with no prefix: {:?}",
            rows[0].text
        );
    }

    /// P1 encoding audit M1: a hostile plugin that titles its command
    /// STARTING with the genuine prefix (trying to fabricate
    /// `"[extension] app.quit"`) never manages to hide or replace the real
    /// prefix — `text` ALWAYS starts with the genuine prefix (`format!`
    /// prepends it, never interprets or lets it be overwritten), and the
    /// attempted title stays DOUBLED and visible afterwards, never removed
    /// or merged with the original.
    #[test]
    fn doubled_prefix_from_a_hostile_title_is_never_removed() {
        let prefix = norte_i18n::t("palette-plugin-prefix");
        let payload = format!("{prefix}] app.quit");
        let plugins = vec![plugin_info(
            "org.evil.x",
            true,
            true,
            None,
            vec![("run", payload.as_str())],
        )];
        let rows = plugin_rows(&plugins);
        let genuine_prefix = format!("[{prefix}] ");
        assert!(
            rows[0].text.starts_with(&genuine_prefix),
            "the genuine prefix must still be the row's start: {:?}",
            rows[0].text
        );
        assert_eq!(
            rows[0].text,
            format!("{genuine_prefix}{payload}"),
            "the attempt to double the prefix must stay whole, not collapsed: {:?}",
            rows[0].text
        );
        assert_ne!(rows[0].text, "app.quit");
    }

    #[test]
    fn rows_for_context_hides_viewer_rows_when_there_is_no_viewer() {
        let rows = vec![
            Row {
                key: "viewer.close".into(),
                text: "viewer.close".into(),
                desc: String::new(),
                chord: "—".into(),
                hostile: false,
            },
            Row {
                key: "pane.copy".into(),
                text: "pane.copy".into(),
                desc: String::new(),
                chord: "—".into(),
                hostile: false,
            },
        ];
        let filtered = rows_for_context(&rows, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].key, "pane.copy");
        assert_eq!(rows_for_context(&rows, true), rows);
    }
}
