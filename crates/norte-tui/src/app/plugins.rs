//! Los plugins vistos desde la UI: cómo se recorta su descripción para la lista
//! y el gestor de extensiones.

/// El tope y el enmascarado de las etiquetas cortas de un plugin (`name`,
/// `publisher`, título de comando) viven en `norte-frontend` desde la tarea
/// 4.4: la ayuda del host gráfico entra por la misma puerta y masquear texto
/// de tercero no puede tener dos definiciones. Se re-exportan con su nombre
/// de siempre para que ningún call site de este crate se mueva.
pub use norte_frontend::help_badge::{
    PLUGIN_DESCRIPTION_WIRE_CAP, PLUGIN_NAME_WIRE_CAP, plugin_description, plugin_label,
};

/// Clampa ([`PLUGIN_DESCRIPTION_WIRE_CAP`]) y enmascara ([`norte_frontend::display_name`])
/// la `description` de CADA plugin de `plugins`, IN PLACE — en el único
/// punto donde un `PluginListResult` recién llegado del `Backend` entra al
/// estado del TUI (`main::dispatch`, brazos `app.extensions`/
/// `app.palette`). El trabajo se hace UNA vez por plugin aquí, no por fila
/// ni por frame: ambos consumidores ([`ExtensionManager`],
/// [`crate::palette::plugin_rows`]) comparten el resultado ya seguro para
/// pintar — `ExtensionManager` la repinta cada frame
/// (`ui::plugin_description_line`), y antes de este fix recalculaba el
/// enmascarado del String crudo (sin tope) en CADA uno.
pub fn clamp_plugin_descriptions(plugins: &mut [norte_proto::methods::PluginInfo]) {
    for p in plugins {
        if let Some(raw) = &p.description {
            p.description = Some(plugin_description(raw));
        }
    }
}

/// Overlay del catálogo de extensiones (M4-P3): la lista de plugins descubierta
/// por el core (YA ordenada por categoría e id) más los directorios que
/// fallaron al cargar, con un cursor de selección. Regla 7: el TUI no decide
/// nada — aprobar/activar viaja al core por el `Backend`; aquí solo se navega y
/// se refleja el estado. El `name`/`publisher` de cada plugin son texto LIBRE
/// de un tercero: se enmascaran con [`norte_frontend::display_name`] al pintar (superficie de
/// decisión de seguridad).
#[derive(Debug, Clone)]
pub struct ExtensionManager {
    /// Plugins descubiertos, en el orden del core (categoría, luego id).
    pub plugins: Vec<norte_proto::methods::PluginInfo>,
    /// Directorios que no cargaron (diagnóstico), se pintan al final.
    pub errors: Vec<norte_proto::methods::PluginLoadError>,
    /// Índice del plugin resaltado.
    pub cursor: usize,
    /// Drill-down editor over the SELECTED plugin's `[config]` (G3c):
    /// `Some` while open — `dialog.confirm` on the plugin list opens it
    /// (fetches `plugin.get_config`), `dialog.cancel` inside it closes
    /// back to the plugin list (never the whole overlay).
    pub config: Option<PluginConfigPanel>,
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
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope al último plugin).
    pub fn down(&mut self) {
        let max = self.plugins.len().saturating_sub(1);
        self.cursor = (self.cursor + 1).min(max);
    }

    /// El plugin bajo el cursor, si lo hay.
    #[must_use]
    pub fn selected(&self) -> Option<&norte_proto::methods::PluginInfo> {
        self.plugins.get(self.cursor)
    }

    /// Togglea el bool LOCAL de aprobación del plugin bajo el cursor, para
    /// feedback inmediato tras un `plugins_set_approval` OK en el Backend (la
    /// verdad vive en el core; esto solo evita un relistado para repintar).
    pub fn set_local_approved(&mut self, approved: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.approved = approved;
        }
    }

    /// Análogo a [`Self::set_local_approved`] para el estado de activación.
    pub fn set_local_enabled(&mut self, enabled: bool) {
        if let Some(p) = self.plugins.get_mut(self.cursor) {
            p.enabled = enabled;
        }
    }
}

/// Acción del usuario sobre el overlay de extensiones (el frontend traduce las
/// teclas; el efecto —llamar al `Backend`— vive en `main`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtAction {
    /// Resalta el anterior.
    Up,
    /// Resalta el siguiente.
    Down,
    /// Togglea la aprobación del plugin resaltado.
    ToggleApprove,
    /// Togglea la activación del plugin resaltado.
    ToggleEnable,
    /// Cierra el overlay.
    Close,
}

/// Popup de selección de tema: lista de presets con preview EN VIVO (mover el
/// cursor aplica el tema al vuelo; Esc revierte al que había, Enter lo fija).
#[derive(Debug, Clone)]
pub struct ThemePicker {
    /// Nombres de preset a elegir.
    pub names: Vec<String>,
    /// Índice resaltado.
    pub cursor: usize,
    /// Tema que había ANTES de abrir, para revertir al cancelar.
    pub original: crate::theme::TuiTheme,
}

impl ThemePicker {
    /// Sube el cursor (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja el cursor (tope al último).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.names.len() {
            self.cursor += 1;
        }
    }

    /// El nombre resaltado.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.names.get(self.cursor).map(String::as_str)
    }
}

/// Acción del usuario sobre el popup de tema (el frontend traduce las teclas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// Resalta el anterior (con preview).
    Up,
    /// Resalta el siguiente (con preview).
    Down,
    /// Fija el tema resaltado y cierra.
    Confirm,
    /// Revierte al tema previo y cierra.
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

    /// P1 encoding audit F1 (MEDIUM): `PluginInfo.description` no tiene tope
    /// en el wire (el manifiesto solo lo limita al PARSEAR, en el camino
    /// honesto) — un daemon hostil/comprometido podría mandar cualquier
    /// longitud. `clamp_plugin_descriptions` es el único punto donde
    /// `plugins_list` entra al estado del TUI (`main::dispatch`); debe
    /// recortarla ahí, de una vez, para ambos consumidores.
    /// El recorte se MARCA con `…`, así que son el tope MÁS uno.
    ///
    /// Cambió al izar la función a `norte-frontend` (tarea 4.5), y a
    /// propósito: cortar en seco presenta una descripción truncada como si
    /// estuviera completa, que es la misma clase de mentira que
    /// `plugin_label` —su vecina, con el mismo tipo de texto— lleva
    /// evitando desde H3e. El tope defensivo sigue siendo el mismo número.
    #[test]
    fn clampa_al_tope_del_wire() {
        let mut plugins = vec![plugin(Some(&"a".repeat(50_000)))];
        clamp_plugin_descriptions(&mut plugins);
        let recortada = plugins[0].description.as_deref().unwrap();
        assert_eq!(
            recortada.chars().count(),
            crate::app::PLUGIN_DESCRIPTION_WIRE_CAP + 1
        );
        assert!(recortada.ends_with('…'), "el recorte se ve");
    }

    #[test]
    fn none_se_queda_none() {
        let mut plugins = vec![plugin(None)];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(plugins[0].description, None);
    }

    #[test]
    fn corta_bajo_el_tope_no_se_toca() {
        let mut plugins = vec![plugin(Some("una description corta"))];
        clamp_plugin_descriptions(&mut plugins);
        assert_eq!(
            plugins[0].description.as_deref(),
            Some("una description corta")
        );
    }

    /// El override RTL nunca sobrevive crudo al clamp — se enmascara aquí,
    /// no en cada frame del gestor de extensiones.
    #[test]
    fn enmascara_override_rtl() {
        let mut plugins = vec![plugin(Some("abc\u{202E}gpj.exe"))];
        clamp_plugin_descriptions(&mut plugins);
        let d = plugins[0].description.as_deref().unwrap();
        assert!(!d.contains('\u{202E}'));
        assert!(d.contains('\u{FFFD}'));
    }
}
