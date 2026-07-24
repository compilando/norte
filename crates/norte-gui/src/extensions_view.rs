//! GUI extension manager full view (G3c, `F12`): mirrors the TUI's gestor
//! de extensiones — plugin list with approve/enable toggles, PLUS a
//! `[config]` drill-down section built on `norte_frontend::plugin_config`
//! (the SAME pure editor the TUI's extension manager now uses). Same split
//! as `settings_view`: this file owns pure state
//! (`ExtensionsView`/`ConfigPanel`) + keyboard routing (`on_key`, hardcoded
//! GPUI key names — same convention `settings_view::on_key` already
//! established for a full-view screen, no keymap resolver involved);
//! `main.rs` owns the async `Backend` calls (via the session channel) and
//! rendering.

use norte_frontend::plugin_config::{PendingConfigWrite, PluginConfigState};
use norte_frontend::settings::SettingsEditError;
use norte_proto::methods::{PluginInfo, PluginLoadError};

/// Drill-down panel over ONE plugin's `[config]` — mirrors the TUI's
/// `norte_tui::app::PluginConfigPanel`.
#[derive(Debug)]
pub struct ConfigPanel {
    /// Id of the plugin being configured.
    pub plugin_id: String,
    /// Masked plugin name, for the panel header.
    pub plugin_name: String,
    /// The pure cursor+edit widget over this plugin's `[config]` keys.
    pub state: PluginConfigState,
}

/// Full view state: the plugin list + cursor + optional `[config]`
/// drill-down.
#[derive(Debug, Default)]
pub struct ExtensionsView {
    /// Plugins discovered, in the core's order (category, then id).
    pub plugins: Vec<PluginInfo>,
    /// Directories that failed to load (diagnostic).
    pub errors: Vec<PluginLoadError>,
    /// Index of the highlighted plugin.
    pub cursor: usize,
    /// The `[config]` drill-down, `Some` while open.
    pub config: Option<ConfigPanel>,
    /// `true` while the initial `plugin.list` is in flight — the render
    /// shows a loading state instead of "no extensions", distinguishing
    /// "still loading" from "genuinely empty" (same criterion the
    /// viewer's `viewer_loading` flag already established).
    pub loading: bool,
}

impl ExtensionsView {
    /// A freshly-opened view with nothing loaded yet.
    #[must_use]
    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Default::default()
        }
    }

    /// Moves the selection up (clamped at the top).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the selection down (clamped at the end).
    pub fn down(&mut self) {
        let max = self.plugins.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// The plugin under the cursor, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&PluginInfo> {
        self.plugins.get(self.cursor)
    }

    /// Optimistic LOCAL update of the approval bool under the cursor.
    pub fn set_local_approved(&mut self, id: &str, approved: bool) {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.id == id) {
            p.approved = approved;
        }
    }

    /// Analogous to [`Self::set_local_approved`] for the enabled state.
    pub fn set_local_enabled(&mut self, id: &str, enabled: bool) {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.id == id) {
            p.enabled = enabled;
        }
    }
}

/// What the caller (`main.rs`) must do after a key — mirrors
/// `settings_view::SettingsOutcome`'s shape, but the write intents here
/// need an async `Backend` round trip (never a local `norte_config::persist_set`),
/// so the caller sends a `SessionCmd` instead of writing directly.
#[derive(Debug)]
pub enum ExtensionsOutcome {
    /// Nothing to do beyond a repaint.
    None,
    /// Esc on the plugin list (not the config panel, which closes back to
    /// the list first): close the view.
    Close,
    /// `y`: toggle approval of the selected plugin.
    RequestApprove {
        /// Plugin id.
        id: String,
        /// Requested new state.
        approved: bool,
    },
    /// `e`: toggle enabled state of the selected plugin.
    RequestEnable {
        /// Plugin id.
        id: String,
        /// Requested new state.
        enabled: bool,
    },
    /// Enter on the plugin list: fetch `[config]` for `id` — the caller
    /// opens the drill-down ONLY if the result is non-empty (mirrors the
    /// TUI: Enter never approves, it only ever opens a submenu). The
    /// plugin's name (for the drill-down header) is NOT carried here — the
    /// caller looks it up from `ExtensionsView::plugins` when the
    /// `plugin.get_config` response lands, one less field to keep in sync.
    RequestConfig {
        /// Plugin id.
        id: String,
    },
    /// A `bool`/`enum` key cycled immediately, or a `string`/`int` edit was
    /// confirmed: persist via `plugin.set_config`.
    RequestConfigWrite {
        /// Plugin whose config this write targets.
        plugin_id: String,
        /// The write itself (key/value/display).
        write: PendingConfigWrite,
    },
    /// An inline edit's buffer was rejected — unpersisted, buffer intact.
    Invalid(SettingsEditError),
    /// Esc on the `[config]` drill-down (not editing): close the PANEL,
    /// back to the plugin list — never the whole view.
    CloseConfigPanel,
}

/// Keyboard routing (GPUI key names, hardcoded — same convention
/// `settings_view::on_key` uses for a full-view screen, no keymap resolver
/// involved). The caller (`NorteGui::on_extensions_key`) has already gated
/// ctrl/alt/platform modifiers out before calling this.
#[must_use]
pub fn on_key(view: &mut ExtensionsView, key: &str, key_char: Option<&str>) -> ExtensionsOutcome {
    if let Some(panel) = &mut view.config {
        let outcome = on_config_key(panel, key, key_char);
        // `on_config_key` only has `&mut ConfigPanel` — closing the panel
        // (back to the plugin list) needs `&mut ExtensionsView`, so THIS
        // level applies it and swallows the signal (nothing left for the
        // caller to do beyond a repaint, same as `ExtensionsOutcome::None`).
        if matches!(outcome, ExtensionsOutcome::CloseConfigPanel) {
            view.config = None;
            return ExtensionsOutcome::None;
        }
        return outcome;
    }
    match key {
        "escape" => ExtensionsOutcome::Close,
        "up" => {
            view.up();
            ExtensionsOutcome::None
        }
        "down" => {
            view.down();
            ExtensionsOutcome::None
        }
        "y" => match view.selected() {
            Some(p) => ExtensionsOutcome::RequestApprove {
                id: p.id.clone(),
                approved: !p.approved,
            },
            None => ExtensionsOutcome::None,
        },
        "e" => match view.selected() {
            Some(p) => ExtensionsOutcome::RequestEnable {
                id: p.id.clone(),
                enabled: !p.enabled,
            },
            None => ExtensionsOutcome::None,
        },
        // Enter NEVER approves (P1 pin): it only ever opens the `[config]`
        // drill-down submenu, if the plugin declares any key.
        "enter" => match view.selected() {
            Some(p) => ExtensionsOutcome::RequestConfig { id: p.id.clone() },
            None => ExtensionsOutcome::None,
        },
        _ => ExtensionsOutcome::None,
    }
}

/// Keys while the `[config]` drill-down is open — raw capture while
/// `panel.state.is_editing()` (same idiom as `settings_view::on_key`'s
/// editing branch), navigation/activate otherwise.
fn on_config_key(panel: &mut ConfigPanel, key: &str, key_char: Option<&str>) -> ExtensionsOutcome {
    if panel.state.is_editing() {
        return match key {
            "backspace" => {
                panel.state.edit_backspace();
                ExtensionsOutcome::None
            }
            "escape" => {
                panel.state.edit_cancel();
                ExtensionsOutcome::None
            }
            "enter" => match panel.state.edit_commit() {
                Ok(write) => ExtensionsOutcome::RequestConfigWrite {
                    plugin_id: panel.plugin_id.clone(),
                    write,
                },
                Err(e) => ExtensionsOutcome::Invalid(e),
            },
            _ => {
                if let Some(c) = typed_char(key, key_char) {
                    panel.state.edit_push_char(c);
                }
                ExtensionsOutcome::None
            }
        };
    }
    match key {
        "escape" => ExtensionsOutcome::CloseConfigPanel,
        "up" => {
            panel.state.up();
            ExtensionsOutcome::None
        }
        "down" => {
            panel.state.down();
            ExtensionsOutcome::None
        }
        "enter" => match panel.state.activate() {
            Some(write) => ExtensionsOutcome::RequestConfigWrite {
                plugin_id: panel.plugin_id.clone(),
                write,
            },
            None => ExtensionsOutcome::None,
        },
        _ => ExtensionsOutcome::None,
    }
}

/// The single char `key` types — local copy of `settings_view::typed_char`
/// (same rationale as that file's own doc).
fn typed_char(key: &str, key_char: Option<&str>) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    single_char(key_char).or_else(|| single_char(Some(key)))
}

fn single_char(s: Option<&str>) -> Option<char> {
    let s = s?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str, approved: bool, enabled: bool) -> PluginInfo {
        PluginInfo {
            id: id.into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "previewer".into(),
            capabilities: Vec::new(),
            approved,
            enabled,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
        }
    }

    fn view() -> ExtensionsView {
        ExtensionsView {
            plugins: vec![plugin("org.a", true, true), plugin("org.b", false, false)],
            errors: Vec::new(),
            cursor: 0,
            config: None,
            loading: false,
        }
    }

    #[test]
    fn escape_cierra_la_vista() {
        let mut v = view();
        assert!(matches!(
            on_key(&mut v, "escape", None),
            ExtensionsOutcome::Close
        ));
    }

    #[test]
    fn y_pide_toggle_de_aprobacion() {
        let mut v = view();
        match on_key(&mut v, "y", None) {
            ExtensionsOutcome::RequestApprove { id, approved } => {
                assert_eq!(id, "org.a");
                assert!(!approved, "org.a está aprobado: pide revocar");
            }
            other => panic!("esperaba RequestApprove, vino {other:?}"),
        }
    }

    #[test]
    fn e_pide_toggle_de_activacion() {
        let mut v = view();
        match on_key(&mut v, "e", None) {
            ExtensionsOutcome::RequestEnable { id, enabled } => {
                assert_eq!(id, "org.a");
                assert!(!enabled);
            }
            other => panic!("esperaba RequestEnable, vino {other:?}"),
        }
    }

    #[test]
    fn enter_pide_config_nunca_aprueba() {
        let mut v = view();
        match on_key(&mut v, "enter", None) {
            ExtensionsOutcome::RequestConfig { id, .. } => assert_eq!(id, "org.a"),
            other => panic!("esperaba RequestConfig, vino {other:?}"),
        }
        // El estado de aprobación NO cambió.
        assert!(v.plugins[0].approved);
    }

    #[test]
    fn escape_en_panel_de_config_lo_cierra_sin_cerrar_la_vista() {
        let mut v = view();
        v.config = Some(ConfigPanel {
            plugin_id: "org.a".into(),
            plugin_name: "Demo".into(),
            state: PluginConfigState::new(Vec::new()),
        });
        assert!(matches!(
            on_key(&mut v, "escape", None),
            ExtensionsOutcome::None
        ));
        assert!(v.config.is_none(), "el panel se cierra");
    }

    #[test]
    fn up_down_clampan() {
        let mut v = view();
        v.up();
        assert_eq!(v.cursor, 0);
        v.down();
        v.down();
        assert_eq!(v.cursor, 1);
    }

    fn wire(key: &str, kind: &str, value: &str) -> norte_proto::methods::PluginConfigKeyWire {
        norte_proto::methods::PluginConfigKeyWire {
            key: key.into(),
            kind: kind.into(),
            default: value.into(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: value.into(),
        }
    }

    #[test]
    fn config_panel_bool_cicla_de_inmediato() {
        let mut v = view();
        v.config = Some(ConfigPanel {
            plugin_id: "org.a".into(),
            plugin_name: "Demo".into(),
            state: PluginConfigState::new(norte_frontend::plugin_config::sanitize_config_keys(&[
                wire("verbose", "bool", "false"),
            ])),
        });
        match on_key(&mut v, "enter", None) {
            ExtensionsOutcome::RequestConfigWrite { plugin_id, write } => {
                assert_eq!(plugin_id, "org.a");
                assert_eq!(write.value, "true");
            }
            other => panic!("esperaba RequestConfigWrite, vino {other:?}"),
        }
    }

    #[test]
    fn config_panel_string_edita_raw_y_confirma() {
        let mut v = view();
        v.config = Some(ConfigPanel {
            plugin_id: "org.a".into(),
            plugin_name: "Demo".into(),
            state: PluginConfigState::new(norte_frontend::plugin_config::sanitize_config_keys(&[
                wire("greeting", "string", "hola"),
            ])),
        });
        assert!(matches!(
            on_key(&mut v, "enter", None),
            ExtensionsOutcome::None
        ));
        assert!(v.config.as_ref().unwrap().state.is_editing());
        for _ in 0..4 {
            let _ = on_key(&mut v, "backspace", None);
        }
        for c in "hey".chars() {
            let s = c.to_string();
            let _ = on_key(&mut v, &s, Some(&s));
        }
        match on_key(&mut v, "enter", None) {
            ExtensionsOutcome::RequestConfigWrite { write, .. } => assert_eq!(write.value, "hey"),
            other => panic!("esperaba RequestConfigWrite, vino {other:?}"),
        }
    }
}
