//! Rows of the command palette (`Ctrl+P`/vim `:`, H1 T4, spec-promised): same
//! criterion as `help::build` (F1) — built from the EFFECTIVE keymap and the
//! `help-cmd-*` Fluent catalogue, never from a hand-written list.
//!
//! [`Row`]/[`plugin_rows`]/[`rows_for_context`]/[`first_chord`] live in
//! `norte-frontend` (G3c hoist, same pattern as [`crate::settings`]) — this
//! module re-exports them for source compatibility and adds [`build_rows`],
//! which IS TUI-specific (it depends on [`crate::keymap::COMMANDS`]/
//! [`crate::keymap::help_id`], different from the GUI's).

pub use norte_frontend::palette::{
    PLUGIN_DESCRIPTION_WIRE_CAP, Row, first_chord, plugin_rows, rows_for_context,
};
use norte_i18n::t;

use crate::keymap::{COMMANDS, Effective, help_id};

/// Builds the rows for ALL commands in [`COMMANDS`] (browse + viewer share
/// the same catalogue, ADR 0006): the description comes from `help-cmd-*`
/// (the SAME source as F1 — the i18n suite already requires it to exist,
/// `every_command_has_translated_help`), the chord is the FIRST key by real
/// precedence in the effective `browse`, or, if the command does not live
/// there (it is `viewer.*`), the effective `viewer`'s; with none, `"—"` (a
/// valid command with no key in THIS preset+layers — the palette remains the
/// only way to launch it).
#[must_use]
pub fn build_rows(browse: &Effective, viewer: &Effective) -> Vec<Row> {
    COMMANDS
        .iter()
        .map(|&cmd| {
            let desc = t(&help_id(cmd));
            let chord = first_chord(cmd, browse)
                .or_else(|| first_chord(cmd, viewer))
                .unwrap_or_else(|| "—".to_owned());
            Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc,
                chord,
                // This project's vocabulary: there is nothing to mask.
                hostile: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Screen, presets};

    fn orthodox_effs() -> (Effective, Effective) {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let browse = Effective::build_for(&preset, &[], COMMANDS, Screen::Browse).unwrap();
        let viewer = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
        (browse, viewer)
    }

    #[test]
    fn build_rows_one_row_per_command_with_browse_chord() {
        // This test asserts the ENGLISH corpus strings. Without pinning the
        // language it resolved by environment (`LANG`), so it was green in CI
        // and red on any machine with `LANG=es_*` — the same line the rest of
        // this crate's render tests already carried.
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        assert_eq!(rows.len(), COMMANDS.len(), "one row per command, no more");
        let quit = rows.iter().find(|r| r.key == "app.quit").unwrap();
        assert_eq!(quit.key, quit.text, "built-in: key == text (reliable)");
        assert_eq!(quit.desc, "quit norte");
        // Bound in [global]: q/f10/ctrl+c — the FIRST by precedence.
        assert_ne!(quit.chord, "—");
    }

    #[test]
    fn build_rows_falls_back_to_viewer_if_not_in_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        let close = rows.iter().find(|r| r.key == "viewer.close").unwrap();
        assert_ne!(close.chord, "—", "viewer.close lives in Screen::Viewer");
    }

    /// Encoding audit H1: the same defect as `dialog_hints` but in the
    /// command palette's chord column (`build_rows`/`first_chord`) — a
    /// hostile chord from a user/project layer rebound to a browse command
    /// (`pane.copy`, always present in `COMMANDS`) must not be painted raw.
    #[test]
    fn build_rows_masks_hostile_chords_in_the_chord_column() {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let viewer_empty = Effective::build_for(&preset, &[], COMMANDS, Screen::Viewer).unwrap();
        for hazard in norte_testkit::corpus::hostile_chords() {
            let token_esc = format!("\\u{:04X}", hazard.token as u32);
            let layer_src = format!(
                r#"
                [pane]
                prepend_keymap = [{{ on = ["{token_esc}"], run = "pane.copy" }}]
                "#,
            );
            let layer = crate::keymap::parse_keymap(&layer_src).unwrap();
            let browse = Effective::build_for(&preset, &[layer], COMMANDS, Screen::Browse)
                .unwrap_or_else(|e| panic!("[{}] effective keymap: {e}", hazard.id));
            let rows = build_rows(&browse, &viewer_empty);
            let copy = rows
                .iter()
                .find(|r| r.key == "pane.copy")
                .unwrap_or_else(|| panic!("[{}] pane.copy row", hazard.id));
            assert!(
                !copy.chord.chars().any(norte_encoding::is_terminal_hazard),
                "[{}] raw hazard in the chord column: {:?}",
                hazard.id,
                copy.chord
            );
            assert!(
                copy.chord.contains('\u{FFFD}'),
                "[{}] the hazard must be masked to U+FFFD: {:?}",
                hazard.id,
                copy.chord
            );
        }
    }

    /// MINOR-6 (H1 close): opened from BROWSE (`viewer_open = false`), the
    /// palette hides `viewer.*` — dispatching one without `app.viewer` would
    /// be a silent no-op.
    #[test]
    fn rows_for_context_hides_viewer_from_browse() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        let filtered = rows_for_context(&rows, false);
        assert!(
            filtered.iter().all(|r| !r.key.starts_with("viewer.")),
            "no viewer.* row should survive filtering from browse"
        );
        assert!(
            filtered.iter().any(|r| r.key.starts_with("pane.")),
            "pane.* rows remain present"
        );
        assert!(
            filtered.len() < rows.len(),
            "filtering must remove AT LEAST the viewer.* rows"
        );
    }

    /// Opened FROM the viewer (`viewer_open = true`), the palette keeps
    /// EVERYTHING — including `pane.*` rows.
    #[test]
    fn rows_for_context_keeps_everything_from_the_viewer() {
        let (browse, viewer) = orthodox_effs();
        let rows = build_rows(&browse, &viewer);
        assert_eq!(rows_for_context(&rows, true), rows);
    }
}
