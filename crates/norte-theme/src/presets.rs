//! Presets embebidos (ADR 0020 D3): temas cuidados que viajan en el binario.
//! `[ui].theme` acepta uno de estos NOMBRES o una ruta a un `.toml` propio (la
//! lectura del fichero la hace el frontend; este crate solo parsea).

use crate::theme::{Theme, ThemeError};

/// Nombre del preset por defecto (el que se aplica sin `[ui].theme`).
pub const DEFAULT_PRESET: &str = "default";

/// `(nombre, fuente TOML)` de cada preset embebido.
const PRESETS: &[(&str, &str)] = &[
    ("default", include_str!("../presets/default.toml")),
    (
        "catppuccin-mocha",
        include_str!("../presets/catppuccin-mocha.toml"),
    ),
    ("gruvbox-dark", include_str!("../presets/gruvbox-dark.toml")),
    ("nord", include_str!("../presets/nord.toml")),
    // Claros.
    (
        "gruvbox-light",
        include_str!("../presets/gruvbox-light.toml"),
    ),
    (
        "catppuccin-latte",
        include_str!("../presets/catppuccin-latte.toml"),
    ),
    // Retro (G1): [effects] interpretados por la GUI (ADR 0036).
    ("retro-crt", include_str!("../presets/retro-crt.toml")),
    (
        "retro-crt-amber",
        include_str!("../presets/retro-crt-amber.toml"),
    ),
];

/// Nombres de todos los presets embebidos (para autocompletar / validar).
#[must_use]
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(n, _)| *n).collect()
}

/// La fuente TOML cruda de un preset, si existe con ese nombre.
#[must_use]
pub fn preset_source(name: &str) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| *src)
}

impl Theme {
    /// Carga un preset embebido por nombre.
    ///
    /// # Errors
    /// [`ThemeError::Toml`] jamás en la práctica (los presets se testean); es
    /// un `Result` por coherencia con [`Theme::from_toml`].
    ///
    /// Devuelve `Ok(None)` si el nombre no es un preset conocido (el caller lo
    /// trata como ruta de fichero).
    pub fn preset(name: &str) -> Result<Option<Theme>, ThemeError> {
        match preset_source(name) {
            Some(src) => Theme::from_toml(src).map(Some),
            None => Ok(None),
        }
    }

    /// El tema por defecto (`default`), garantizado presente.
    ///
    /// # Panics
    /// Nunca: el preset `default` está embebido y se testea que parsea.
    #[must_use]
    pub fn preset_default() -> Theme {
        Theme::from_toml(preset_source(DEFAULT_PRESET).expect("preset default embebido"))
            .expect("preset default parsea")
    }
}
