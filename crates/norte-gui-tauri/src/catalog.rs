//! What the renderer needs ONCE: the strings and the colors, already resolved.
//!
//! Both things travel resolved IN RUST, and for the same reason. Strings,
//! because translating means choosing plural, order and form, and doing that
//! twice means having two catalogues that diverge. Colors, because the theme
//! is a project document with its own roles and fallbacks, and a renderer
//! that made them up would paint a different norte.

use std::collections::BTreeMap;

use norte_i18n::Lang;
use norte_theme::Theme;
use norte_ui_host::{BRIDGE_VERSION, InstanceId};
use serde::{Deserialize, Serialize};

/// The renderer's startup bundle.
///
/// **It is the wire's fifth message and the only one that did not live in
/// `norte-ui-host`**, so neither the versioned bridge nor its golden corpus
/// covered it (#259). It stays here — what it carries is translated strings
/// and colors, which is the painter's business, not the host's — but it no
/// longer travels without a net: `Deserialize` and a case in
/// `tests/catalogo_wire.rs` pin its shape.
///
/// Its `bridge_version` is INFORMATIONAL. Compatibility is decided by the
/// renderer over the envelope it is about to interpret (`session.ts`), which
/// already carries its own: trusting a side message for that would mean
/// believing a number that does not travel with the data.
// `PartialEq` without `Eq`: `font_size` is an `f32` because the configuration
// deliberately accepts a fractional part (a hand-written `14.5` can be edited
// from the settings screen), and a float is not `Eq`. Here it is only compared
// in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostCatalog {
    /// The contract version this host speaks, for diagnostics.
    ///
    /// It is not what decides whether the renderer continues: the envelope
    /// says that.
    pub bridge_version: u32,
    /// The live instance. A message from another one is not interpreted.
    pub instance_id: String,
    /// The negotiated language.
    pub locale: String,
    /// Fluent key → already-translated text.
    pub strings: BTreeMap<String, String>,
    /// CSS variable name (without `--`) → `#rrggbb` color.
    pub theme: BTreeMap<String, String>,
    /// The renderer has to MEASURE ITSELF instead of waiting on a human.
    ///
    /// Switched on by `NORTE_GUI_MEASURE=1`, and only used by task 3.6: a
    /// scripted pass of keys and scroll that records latencies and sends them
    /// through the `metrics` command (which only exists with the feature of
    /// the same name, i.e. not in the published binary).
    pub measure: bool,
    /// How long to wait before SHOWING that it is waiting, in ms.
    ///
    /// It travels instead of being written into the CSS because it is a
    /// decision shared with the terminal: `norte_frontend::busy::THRESHOLD`,
    /// with its reasoning — below that, the operation finishes before the eye
    /// registers it and all that is left is a flicker. A number repeated in a
    /// stylesheet is the third place to change it and the first place to
    /// forget.
    pub busy_threshold_ms: u64,
    /// What this window paints that is not color: fonts and motion.
    pub appearance: Appearance,
    /// There is no user `norte.toml` yet (spec 2026-09-10): the renderer opens
    /// the first-run wizard when it paints the first frame. Startup decides
    /// this, since it is the one that looks at disk; `NORTE_NO_WIZARD` turns
    /// it off, as in the terminal. With `default`: an earlier catalogue does
    /// not carry it, and not carrying it means "this is not the first run".
    #[serde(default)]
    pub first_run: bool,
    /// This window starts without a splash screen (ADR 0115): `--no-splash`
    /// or `NORTE_NO_SPLASH`. Startup decides this, since it is the one that
    /// sees the command line and the environment; the renderer only silences
    /// the startup notice. With `default`: an earlier catalogue does not
    /// carry it, and not carrying it means "yes, show it if the configuration
    /// wants it".
    #[serde(default)]
    pub no_splash: bool,
    /// `[ui] theme_light` / `theme_dark`, already resolved to variables (spec
    /// 2026-09-11, V6): the renderer applies the one that matches
    /// `prefers-color-scheme`, and `theme` when there is no variant for that
    /// side. `None` = `theme` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_light: Option<BTreeMap<String, String>>,
    /// The dark variant; see [`Self::theme_light`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_dark: Option<BTreeMap<String, String>>,
}

/// `[ui] font`, `mono_font`, `font_size` and `reduce_motion`, for the renderer.
///
/// The four were loaded, validated, offered on the settings screen — with
/// `applies_live: true` — and read by NOBODY: they do not apply in the
/// terminal (a terminal does not choose its font) and in the window they
/// never made it across. `reduce_motion` is also an accessibility commitment
/// from spec §17.
///
/// They travel in the CATALOGUE and not in the frame because they are not
/// screen state: they are startup and reload state, like the theme, and they
/// apply live on a profile change by the same path.
///
/// No field carries `skip_serializing_if`: absent and `null` have to mean the
/// same thing here — "the configuration does not say" — and the only way to
/// guarantee that is for the field to always travel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    /// Family for interface text. `None` = the system's.
    #[serde(default)]
    pub font: Option<String>,
    /// Monospace family, for what aligns in columns. `None` = the one the
    /// stylesheet ships.
    #[serde(default)]
    pub mono_font: Option<String>,
    /// Base size in px, already validated to `[8, 32]` by the configuration.
    ///
    /// It is not just bigger text: this window's grid is laid out in CELLS,
    /// so row height and column width come from here. A size that only
    /// changed the glyph would leave it overflowing its row.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// Whoever asks for less motion sees no animations. `None` = whatever the
    /// system says (`prefers-reduced-motion`) rules, which is the correct
    /// default: the configuration can only ADD the request, never contradict
    /// someone who already made it on their desktop.
    #[serde(default)]
    pub reduce_motion: Option<bool>,
    /// `[ui] titlebar = "custom"` (ADR 0136): the window started without the
    /// desktop's bar, and the menu bar acts as the title bar — it is
    /// draggable and carries minimize, maximize and close. STARTUP-ONLY: the
    /// decoration is removed when the window is created, so changing it asks
    /// for a restart.
    #[serde(default)]
    pub custom_titlebar: bool,
}

impl Appearance {
    /// The four `[ui]` scalars, exactly as the configuration left them.
    #[must_use]
    pub fn de(cfg: &norte_config::CommonConfig) -> Self {
        Self {
            font: cfg.ui_font.clone(),
            mono_font: cfg.ui_mono_font.clone(),
            font_size: cfg.ui_font_size,
            reduce_motion: cfg.ui_reduce_motion,
            custom_titlebar: cfg.ui_chrome.titlebar() == norte_config::Titlebar::Custom,
        }
    }
}

/// Builds the bundle for this instance, this language and this theme.
#[must_use]
pub fn catalog(instance: &InstanceId, lang: Lang, theme: &Theme) -> HostCatalog {
    let mut strings = BTreeMap::new();
    for id in norte_i18n::message_ids(lang) {
        let text = norte_i18n::t_in(lang, &id);
        strings.insert(id, text);
    }
    HostCatalog {
        bridge_version: BRIDGE_VERSION,
        instance_id: instance.as_str().to_owned(),
        locale: match lang {
            Lang::Es => "es".to_owned(),
            Lang::En => "en".to_owned(),
        },
        strings,
        theme: variables(theme),
        measure: std::env::var_os("NORTE_GUI_MEASURE").is_some_and(|v| v == "1"),
        busy_threshold_ms: u64::try_from(norte_frontend::busy::THRESHOLD.as_millis())
            .unwrap_or(250),
        appearance: Appearance::default(),
        first_run: false,
        no_splash: false,
        theme_light: None,
        theme_dark: None,
    }
}

impl HostCatalog {
    /// The same catalogue with the appearance the configuration says.
    ///
    /// Separate from [`catalog`] and not one more parameter because the
    /// places that build a catalogue without a configuration are almost all
    /// of them — the tests — and a fourth argument that half the callers fill
    /// with a `Default` is an argument that gets forgotten where it matters.
    #[must_use]
    pub fn con_apariencia(mut self, cfg: &norte_config::CommonConfig) -> Self {
        self.appearance = Appearance::de(cfg);
        self
    }
}

/// The theme's roles, as CSS variables.
///
/// The mapping lives in the HOST (`pickers::theme_roles`) ever since its
/// theme picker started choosing: the host then has to resolve by name a
/// theme nobody handed it, and two lists — one to paint and one to show —
/// would end up saying different things about the same theme. Here it is
/// only given the shape the webview expects.
#[must_use]
pub fn variables(theme: &Theme) -> BTreeMap<String, String> {
    let mut v: BTreeMap<String, String> = norte_ui_host::pickers::theme_roles(theme)
        .into_iter()
        .collect();
    // `[effects] backdrop` (spec 2026-09-11, V6): the only effect this window
    // interprets today. It travels as the variable `#dialogs` reads; any
    // other value — or its absence — is the usual veil.
    if theme.effect_str("backdrop") == Some("blur") {
        v.insert("dialog-backdrop".to_owned(), "blur(6px)".to_owned());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue carries TRANSLATED text, not the key: the renderer has
    /// no catalogue of its own to consult.
    #[test]
    fn strings_come_already_resolved() {
        let c = catalog(&InstanceId::new("i"), Lang::Es, &Theme::preset_default());
        assert_eq!(c.bridge_version, BRIDGE_VERSION);
        assert!(!c.strings.is_empty(), "there is a catalogue");
        for (key, text) in c.strings.iter().take(20) {
            assert!(!text.is_empty(), "{key} has no text");
        }
    }

    /// Two languages, two catalogues: the one sent is the negotiated one.
    #[test]
    fn the_language_rules() {
        let es = catalog(&InstanceId::new("i"), Lang::Es, &Theme::preset_default());
        let en = catalog(&InstanceId::new("i"), Lang::En, &Theme::preset_default());
        assert_eq!(es.locale, "es");
        assert_eq!(en.locale, "en");
        assert_ne!(es.strings, en.strings, "not the same catalogue");
    }

    /// The colors come from the theme, in the shape CSS understands.
    #[test]
    fn colors_come_from_the_theme() {
        let t = Theme::preset("catppuccin-mocha")
            .expect("parses")
            .expect("factory preset");
        let v = variables(&t);
        for (name, value) in &v {
            assert!(
                value.starts_with('#') && value.len() == 7,
                "{name} = {value} is not #rrggbb"
            );
        }
        assert!(v.contains_key("fg"), "at least the normal text is there");
    }

    /// `[effects] backdrop = "blur"` crosses as the veil's variable; a theme
    /// without effects does not carry it, and the renderer falls back to the
    /// usual veil.
    #[test]
    fn theme_blur_crosses_as_a_variable() {
        let with_blur = Theme::preset_default();
        assert_eq!(
            variables(&with_blur)
                .get("dialog-backdrop")
                .map(String::as_str),
            Some("blur(6px)"),
            "the factory preset asks for it"
        );
        let without_blur = Theme::preset("nord").expect("parses").expect("preset");
        assert!(
            !variables(&without_blur).contains_key("dialog-backdrop"),
            "no `[effects]`, no variable"
        );
    }
}
