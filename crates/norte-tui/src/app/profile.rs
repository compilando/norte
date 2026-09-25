//! Profiles as seen from `App` (ADR 0079): where a setting writes when a
//! profile is active, and what part of a profile can't be applied hot in
//! THIS terminal.
//!
//! What is NOT here is anything the two frontends share. Reading the
//! directory is [`norte_frontend::config::read_profiles`] and cycling
//! through the list is
//! [`norte_frontend::profile_picker::next_profile`]: both used to live here,
//! and the window needed them just the same — a second copy of "what's the
//! next profile" is two rules waiting to diverge (ADR 0077).

impl crate::app::App {
    /// Where a setting the reader changes from the UI goes.
    ///
    /// The ACTIVE profile's directory if there is one, and the user's if
    /// not.
    ///
    /// It isn't a style preference: a profile sits ABOVE the user layer
    /// (ADR 0079, D1), so writing there a setting the profile also fixes
    /// leaves it covered up — the theme gets saved, the status bar says
    /// "config reloaded", and the screen doesn't change color. It's exactly
    /// the shape of the bug D10 fixed for shortcuts, and that the rest of
    /// the settings didn't have fixed.
    ///
    /// Changing a setting INSIDE a workspace means changing it in that
    /// workspace, whether the profile fixes it or not. Whoever wants to
    /// touch their own layer leaves the profile first.
    ///
    /// `None` when there's nowhere to write, the same as `user_config_dir()`
    /// used to answer before: the caller already knows how to say that.
    #[must_use]
    pub fn config_write_dir(&self) -> Option<std::path::PathBuf> {
        match &self.active_profile {
            Some(name) => norte_config::profile_dir_from(&|k| std::env::var_os(k), name),
            None => norte_config::user_config_dir(),
        }
    }
}

/// What CANNOT be applied without restarting, from this profile, in THIS
/// terminal.
///
/// Measured, not assumed (D8). What DOES apply is applied by
/// [`crate::config_reload::reload_config`], which is step 2 of the change:
/// theme (ADR 0020), the whole keymap, columns with their re-order,
/// favorites, openers, the quick search mode and the quit confirmation; the
/// mouse gets re-applied by the loop right behind it, and the layout and
/// hidden files arrive via steps 4 and 5. The list below is the rest.
///
/// **`[ui] lang` is the only thing left**, and not by oversight:
/// `norte_i18n::force` runs ONCE per process, and the watcher's hot-reload
/// already carries that same limitation in its signature from before there
/// were profiles. A change that stayed quiet about this would be a change
/// that lies.
///
/// Fonts and `reduce_motion` don't appear here because in a terminal they
/// don't apply at all: they belong to the window, and saying "couldn't be
/// applied" about something this frontend never applies would be noise.
#[must_use]
pub fn not_hot_reloadable(
    before: &norte_config::CommonConfig,
    after: &norte_config::CommonConfig,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if before.ui_lang != after.ui_lang {
        out.push("ui.lang");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A setting changed with an active PROFILE gets written TO THE PROFILE.
    ///
    /// Writing it to the user layer leaves it covered by the profile, which
    /// sits above it (ADR 0079, D1): the theme gets saved, the status bar
    /// says "config reloaded" and the screen doesn't change color. It's the
    /// same shape D10 fixed for shortcuts, and that the settings didn't
    /// have.
    #[test]
    fn with_an_active_profile_settings_are_written_to_the_profile() {
        let mut app = super::super::App::new(
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///x").expect("wire"),
                Vec::new(),
            ),
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///y").expect("wire"),
                Vec::new(),
            ),
        );
        app.active_profile = Some(OsString::from("work"));
        let dir = app.config_write_dir().expect("there is a directory");
        assert!(
            dir.ends_with("profiles/work"),
            "the setting goes to the profile, not to the user layer: {}",
            dir.display()
        );
    }

    /// And with no profile, wherever it always goes.
    #[test]
    fn with_no_profile_settings_go_to_the_user_layer() {
        let app = super::super::App::new(
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///x").expect("wire"),
                Vec::new(),
            ),
            crate::app::Pane::new(
                norte_proto::VPath::parse("file:///y").expect("wire"),
                Vec::new(),
            ),
        );
        assert_eq!(app.config_write_dir(), norte_config::user_config_dir());
    }

    /// Changing the language gets ANNOUNCED; changing the theme doesn't,
    /// because the theme DOES apply hot.
    #[test]
    fn only_the_language_gets_announced() {
        let base = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("empty");
        let mut other = base.clone();
        other.ui_theme = Some("nord".to_owned());
        assert!(
            not_hot_reloadable(&base, &other).is_empty(),
            "the theme is hot-reloadable (ADR 0020)"
        );

        let mut with_lang = base.clone();
        with_lang.ui_lang = Some("es".to_owned());
        assert_eq!(not_hot_reloadable(&base, &with_lang), vec!["ui.lang"]);
    }

    /// Every `CommonConfig` field is CLASSIFIED: either it applies hot, or
    /// it gets announced, or it doesn't belong to this frontend.
    ///
    /// The destructuring has no `..` on purpose. A new field makes this test
    /// FAIL TO COMPILE, which is stronger than a failing assert: it forces a
    /// decision about which group it falls into exactly when someone is
    /// adding it, and it's the only way for the line the profile switch
    /// tells the reader to stay true a year from now.
    #[test]
    fn every_common_config_field_is_classified() {
        let c = norte_config::load(&norte_config::Layers { dirs: Vec::new() }).expect("empty");
        let norte_config::CommonConfig {
            // — Apply hot: `reload_config` (step 2 of the change).
            preset: _,
            ui_theme: _,
            ui_theme_light: _,
            ui_theme_dark: _,
            quick_search: _,
            ui_confirm_quit: _,
            ui_columns: _,
            hotlist: _,
            // — The event loop re-applies it right behind the reload.
            ui_mouse: _,
            // — Same: the loop requests or drops kitty's keyboard protocol
            //   right behind the reload.
            ui_alt_menu: _,
            // — Applies hot: each frame's layout reads the current config,
            //   so the bar appears or disappears on the next paint with
            //   nothing else needed.
            ui_menu_bar: _,
            // — Same as the menu one, and for the same reason (#324).
            ui_panel_bar: _,
            // — Applies hot: `reload_config` copies the chrome to `App` and
            //   every frame reads it (spec 2026-09-10).
            ui_chrome: _,
            // — Applies hot: `reload_config` copies them to `App`, the
            //   status bar reads them every frame and the next listing
            //   requests its columns (ADR 0137).
            ui_status_plugins: _,
            // — Applies hot: the reload passes it to both panes and the row
            //   appears or disappears on the next paint.
            ui_parent_entry: _,
            // — Apply hot: `reload_config` copies the editor back to `App`,
            //   the same as it does with openers.
            ui_editor: _,
            ui_editor_detached: _,
            // — Same as the editor: `reload_config` copies them back, and
            //   the next `pane.compare-files` already uses the new
            //   comparator.
            ui_diff: _,
            ui_diff_detached: _,
            // — Arrive via steps 4 and 5 (layout and slot seeding).
            ui_layout: _,
            ui_show_hidden: _,
            // — Does NOT apply on a reload, and that's its semantics: it
            //   says where a slot opens the FIRST time, so it gets seeded on
            //   entering the profile (`seed_profile_start`, step 3) and only
            //   for the slots the session knows nothing about. Re-applying
            //   it on every reload would send you back to square one every
            //   time the file is touched.
            profile_start: _,
            // — GETS ANNOUNCED: `norte_i18n::force` runs once per process.
            ui_lang: _,
            // — Belongs to the WINDOW: a terminal never applies them, so
            //   saying "couldn't be applied" would be noise.
            ui_font: _,
            ui_mono_font: _,
            ui_font_size: _,
            ui_reduce_motion: _,
            // — A profile CANNOT fix them (ADR 0079, D2), so a profile
            //   switch doesn't move them by construction. By WHOLE SECTION:
            //   the carve-out is the section's, so a new key inside them
            //   inherits the answer without passing through here.
            daemon: _,
            archive: _,
            log: _,
            ai: _,
            // — Load diagnostics, not settings.
            sources: _,
            project_warnings: _,
            profile_warnings: _,
            profile_title: _,
        } = c;
    }
}
