//! Embedded presets (ADR 0020 D3): curated themes that travel in the binary.
//! `[ui].theme` accepts one of these NAMES or a path to a custom `.toml` (the
//! frontend reads the file; this crate only parses).

use crate::theme::{Theme, ThemeError};

/// Name of the default preset (the one applied without `[ui].theme`).
pub const DEFAULT_PRESET: &str = "default";

/// `(name, TOML source)` of each embedded preset.
const PRESETS: &[(&str, &str)] = &[
    ("default", include_str!("../presets/default.toml")),
    (
        "catppuccin-mocha",
        include_str!("../presets/catppuccin-mocha.toml"),
    ),
    ("gruvbox-dark", include_str!("../presets/gruvbox-dark.toml")),
    ("nord", include_str!("../presets/nord.toml")),
    // Light ones.
    (
        "gruvbox-light",
        include_str!("../presets/gruvbox-light.toml"),
    ),
    (
        "catppuccin-latte",
        include_str!("../presets/catppuccin-latte.toml"),
    ),
    // VSCode (spec 2026-09-11): transcriptions of Dark/Light Modern, with the
    // editor color registry's default values for the ids that NO file in the
    // `include` chain defines. Each file states in its header where each role
    // comes from and where it diverges.
    ("vscode-dark", include_str!("../presets/vscode-dark.toml")),
    ("vscode-light", include_str!("../presets/vscode-light.toml")),
    // Retro (G1): [effects] interpreted by the GUI (ADR 0036).
    ("retro-crt", include_str!("../presets/retro-crt.toml")),
    (
        "retro-crt-amber",
        include_str!("../presets/retro-crt-amber.toml"),
    ),
];

/// Names of all embedded presets (for autocompletion / validation).
#[must_use]
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(n, _)| *n).collect()
}

/// The raw TOML source of a preset, if one exists with that name.
#[must_use]
pub fn preset_source(name: &str) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| *src)
}

impl Theme {
    /// Loads an embedded preset by name.
    ///
    /// # Errors
    /// [`ThemeError::Toml`] never in practice (the presets are tested); it is
    /// a `Result` for consistency with [`Theme::from_toml`].
    ///
    /// Returns `Ok(None)` if the name is not a known preset (the caller treats
    /// it as a file path).
    pub fn preset(name: &str) -> Result<Option<Theme>, ThemeError> {
        match preset_source(name) {
            Some(src) => Theme::from_toml(src).map(Some),
            None => Ok(None),
        }
    }

    /// The default theme (`default`), guaranteed present.
    ///
    /// # Panics
    /// Never: the `default` preset is embedded and tested to parse.
    #[must_use]
    pub fn preset_default() -> Theme {
        Theme::from_toml(preset_source(DEFAULT_PRESET).expect("default preset embedded"))
            .expect("default preset parses")
    }
}
