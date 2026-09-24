//! Plugins as seen from the UI: how their description gets trimmed for the
//! list and the extension manager.

/// A plugin's short labels' cap and masking (`name`, `publisher`, a
/// command's title) live in `norte-frontend` since task 4.4: the graphical
/// host's help goes through the same door, and masking third-party text
/// can't have two definitions. Re-exported under their usual names so no
/// call site in this crate has to move.
pub use norte_frontend::help_badge::{
    PLUGIN_DESCRIPTION_WIRE_CAP, PLUGIN_NAME_WIRE_CAP, plugin_description, plugin_label,
};

/// Clamps ([`PLUGIN_DESCRIPTION_WIRE_CAP`]) and masks
/// ([`norte_frontend::display_name`]) EVERY plugin's `description` in
/// `plugins`, IN PLACE — at the ONE point where a `PluginListResult` freshly
/// arrived from the `Backend` enters the TUI's state (`main::dispatch`, the
/// `app.extensions`/`app.palette` arms). The work is done ONCE per plugin
/// here, not per row nor per frame: both consumers ([`ExtensionManager`],
/// [`crate::palette::plugin_rows`]) share the result already safe to paint —
/// `ExtensionManager` repaints it every frame
/// (`ui::plugin_description_line`), and before this fix it recomputed the
/// masking of the raw, uncapped String on EVERY one.
pub fn clamp_plugin_descriptions(plugins: &mut [norte_proto::methods::PluginInfo]) {
    for p in plugins {
        if let Some(raw) = &p.description {
            p.description = Some(plugin_description(raw));
        }
    }
}

/// Extensions catalogue overlay (M4-P3): the list of plugins the core
/// discovered (ALREADY sorted by category and id) plus the directories that
/// failed to load, with a selection cursor. Rule 7: the TUI decides
/// nothing — approving/enabling travels to the core through the `Backend`;
/// here it only navigates and reflects state. Each plugin's `name`/
/// `publisher` are a third party's FREE text: masked with
/// [`norte_frontend::display_name`] when painted (a security decision
/// surface).
#[derive(Debug, Clone)]
pub struct ExtensionManager {
    /// Discovered plugins, in the core's order (category, then id).
    pub plugins: Vec<norte_proto::methods::PluginInfo>,
    /// Directories that didn't load (diagnostic), painted at the end.
    pub errors: Vec<norte_proto::methods::PluginLoadError>,
    /// Index of the highlighted plugin.
    pub cursor: usize,
    /// Drill-down editor over the SELECTED plugin's `[config]` (G3c):
    /// `Some` while open — `dialog.confirm` on the plugin list opens it
    /// (fetches `plugin.get_config`), `dialog.cancel` inside it closes
    /// back to the plugin list (never the whole overlay).
    pub config: Option<PluginConfigPanel>,
    /// Where the keys go inside the manager: the list, or one of the
    /// card's buttons ([`ExtFoco`]). `dialog.pane` — `tab` — moves it.
    pub foco: ExtFoco,
}

/// Where the extension manager's focus is: the plugin list, or button `n`
/// of the card, counting from 0 in the order they're painted.
///
/// Exists because the manager was born with a single keyboard stop — the
/// list — and the card, which the parity work with the window (ADR 0104)
/// put next to it, arrived with buttons only the mouse could press as such.
/// Each button keeps its own key; this is the path for whoever walks the
/// screen with `tab` instead of remembering five letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtFoco {
    /// The keys move the list's cursor (the usual thing).
    #[default]
    Lista,
    /// The keys go to button `n` of the card; `dialog.confirm` fires it.
    Boton(usize),
}

/// `tab`'s ring's next stop: the list, then each button, and back to the
/// list.
///
/// `botones` is how many buttons the LAST frame painted, not how many the
/// card would have if it fit: with a narrow box there's no card, and then
/// the ring has a single stop and `tab` does nothing. Moving focus to
/// something not on screen is a keyboard moving what nobody sees, which is
/// exactly the bug this ring exists to fix.
#[must_use]
pub fn siguiente_foco(foco: ExtFoco, botones: usize) -> ExtFoco {
    if botones == 0 {
        return ExtFoco::Lista;
    }
    match foco {
        ExtFoco::Lista => ExtFoco::Boton(0),
        ExtFoco::Boton(i) if i + 1 < botones => ExtFoco::Boton(i + 1),
        ExtFoco::Boton(_) => ExtFoco::Lista,
    }
}

/// The PREVIOUS stop of the same ring (`←`): the exact inverse of
/// [`siguiente_foco`], so from the list it jumps to the last button.
#[must_use]
pub fn anterior_foco(foco: ExtFoco, botones: usize) -> ExtFoco {
    if botones == 0 {
        return ExtFoco::Lista;
    }
    match foco {
        ExtFoco::Lista => ExtFoco::Boton(botones - 1),
        ExtFoco::Boton(0) => ExtFoco::Lista,
        ExtFoco::Boton(i) => ExtFoco::Boton(i.min(botones) - 1),
    }
}

/// The extension manager's config drill-down (G3c): which plugin, its
/// masked name (for the header — `Row`'s `name`/`desc` inside `state` are
/// ALREADY masked by `norte_frontend::plugin_config::sanitize_config_keys`,
/// this is just the plugin's own display name), and the pure editor state.
#[derive(Debug, Clone)]
pub struct PluginConfigPanel {
    /// Id of the plugin being configured — needed to call
    /// `Backend::plugin_set_config(id, key, value)` on commit.
    pub plugin_id: String,
    /// Masked plugin name, for the panel header.
    pub plugin_name: String,
    /// The pure cursor+edit widget over this plugin's `[config]` keys.
    pub state: norte_frontend::plugin_config::PluginConfigState,
}

impl ExtensionManager {
    /// Moves the cursor up (clamped at the top).
    ///
    /// And hands focus back to the list: the buttons belong to the CHOSEN
    /// plugin, so one focused while the cursor moves to another plugin
    /// would be a button no longer about what's being looked at.
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.foco = ExtFoco::Lista;
    }

    /// Moves the cursor down (clamped at the last row: the ones that didn't
    /// load come after the plugins). Hands focus back to the list, for the
    /// same reason as [`Self::up`].
    pub fn down(&mut self) {
        let max = (self.plugins.len() + self.errors.len()).saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
        self.foco = ExtFoco::Lista;
    }

    /// The plugin under the cursor, if there is one.
    #[must_use]
    pub fn selected(&self) -> Option<&norte_proto::methods::PluginInfo> {
        self.plugins.get(self.cursor)
    }

    /// The extension that did NOT load under the cursor, if the cursor is on
    /// one.
    ///
    /// They come after the plugins: row `plugins.len() + j` is `errors[j]`.
    /// A cursor that stopped at the last plugin left a broken extension with
    /// no way to ask for it to be removed.
    #[must_use]
    pub fn selected_broken(&self) -> Option<&norte_proto::methods::PluginLoadError> {
        self.cursor
            .checked_sub(self.plugins.len())
            .and_then(|j| self.errors.get(j))
    }

    /// Toggles the LOCAL approval bool of the plugin under the cursor, for
    /// immediate feedback after a `plugins_set_approval` OK from the Backend
    /// (the truth lives in the core; this just avoids a re-list to
    /// repaint).
    pub fn set_local_approved(&mut self, approved: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.approved = approved;
        }
    }

    /// Same as [`Self::set_local_approved`] for the enabled state.
    pub fn set_local_enabled(&mut self, enabled: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.enabled = enabled;
        }
    }
}

/// A user action on the extensions overlay (the frontend translates the
/// keys; the effect — calling the `Backend` — lives in `main`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtAction {
    /// Highlights the previous one.
    Up,
    /// Highlights the next one.
    Down,
    /// Toggles the highlighted plugin's approval.
    ToggleApprove,
    /// Toggles the highlighted plugin's enabled state.
    ToggleEnable,
    /// Closes the overlay.
    Close,
}

/// Theme picker popup: a list of presets with a LIVE preview (moving the
/// cursor applies the theme on the fly; Esc reverts to what was there,
/// Enter fixes it).
#[derive(Debug, Clone)]
pub struct ThemePicker {
    /// Preset names to choose from.
    pub names: Vec<String>,
    /// Highlighted index.
    pub cursor: usize,
    /// The theme that was there BEFORE opening, to revert on cancelling.
    pub original: crate::theme::TuiTheme,
}

impl ThemePicker {
    /// Moves the cursor up (clamped at the top).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor down (clamped at the last one).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.names.len() {
            self.cursor += 1;
        }
    }

    /// The highlighted name.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.names.get(self.cursor).map(String::as_str)
    }
}

/// A user action on the theme popup (the frontend translates the keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// Highlights the previous one (with preview).
    Up,
    /// Highlights the next one (with preview).
    Down,
    /// Fixes the highlighted theme and closes.
    Confirm,
    /// Reverts to the previous theme and closes.
    Cancel,
}

#[cfg(test)]
mod clamp_plugin_descriptions_tests {
    use crate::app::clamp_plugin_descriptions;
    use norte_proto::methods::PluginInfo;

    fn plugin(description: Option<&str>) -> PluginInfo {
        PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: description.map(str::to_owned),
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }
    }

    /// P1 encoding audit F1 (MEDIUM): `PluginInfo.description` has no cap on
    /// the wire (the manifest only bounds it while PARSING, on the honest
    /// path) — a hostile/compromised daemon could send any length.
    /// `clamp_plugin_descriptions` is the one point where `plugins_list`
    /// enters the TUI's state (`main::dispatch`); it has to trim it there,
    /// once, for both consumers.
    /// The trim gets MARKED with `…`, so it's the cap PLUS one.
    ///
    /// Changed when the function got hoisted to `norte-frontend` (task 4.5),
    /// and on purpose: cutting it off flat presents a truncated description
    /// as if it were complete, which is the same kind of lie `plugin_label`
    /// — its neighbor, with the same kind of text — has been avoiding since
    /// H3e. The defensive cap is still the same number.
    #[test]
    fn clampa_al_tope_del_wire() {
        let mut plugins = vec![plugin(Some(&"a".repeat(50_000)))];
        clamp_plugin_descriptions(&mut plugins);
        let trimmed = plugins[0].description.as_deref().unwrap();
        assert_eq!(
            trimmed.chars().count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP + 1
        );
        assert!(trimmed.ends_with('…'), "the trim is visible");
    }

    #[test]
    fn none_se_queda_none() {
        let mut plugins = vec![plugin(None)];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(plugins[0].description, None);
    }

    #[test]
    fn corta_bajo_el_tope_no_se_toca() {
        let mut plugins = vec![plugin(Some("a short description"))];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(
            plugins[0].description.as_deref(),
            Some("a short description")
        );
    }

    /// The RTL override never survives the clamp raw — it gets masked here,
    /// not on every frame of the extension manager.
    #[test]
    fn enmascara_override_rtl() {
        let mut plugins = vec![plugin(Some("abc\u{202E}gpj.exe"))];
        clamp_plugin_descriptions(&mut plugins);
        let d = plugins[0].description.as_deref().unwrap();
        assert!(!d.contains('\u{202E}'));
        assert!(d.contains('\u{FFFD}'));
    }
}

/// The manager's `tab` ring: the list, each button, the list.
#[cfg(test)]
mod siguiente_foco_tests {
    use super::{ExtFoco, siguiente_foco};

    /// With four buttons, `tab` walks them in order and returns to the
    /// list: five presses close the ring, not one stop too many.
    #[test]
    fn el_anillo_recorre_los_botones_y_vuelve() {
        let mut f = ExtFoco::Lista;
        let walk: Vec<ExtFoco> = (0..5)
            .map(|_| {
                f = siguiente_foco(f, 4);
                f
            })
            .collect();
        assert_eq!(
            walk,
            vec![
                ExtFoco::Boton(0),
                ExtFoco::Boton(1),
                ExtFoco::Boton(2),
                ExtFoco::Boton(3),
                ExtFoco::Lista,
            ]
        );
    }

    /// With no card painted there are no buttons, and then `tab` moves
    /// nothing: the narrow-box case, where focusing a button would be
    /// focusing something not on screen.
    #[test]
    fn sin_botones_pintados_el_foco_se_queda_en_la_lista() {
        assert_eq!(siguiente_foco(ExtFoco::Lista, 0), ExtFoco::Lista);
        assert_eq!(siguiente_foco(ExtFoco::Boton(2), 0), ExtFoco::Lista);
    }

    /// A focus left pointing past the buttons that are now painted — the
    /// card shrank, or the chosen plugin has no help and one fewer button —
    /// goes back to the list instead of staying out of range.
    #[test]
    fn un_foco_rebasado_vuelve_a_la_lista() {
        assert_eq!(siguiente_foco(ExtFoco::Boton(9), 4), ExtFoco::Lista);
    }
}
