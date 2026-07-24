//! The strict, canonical schema of `norte.toml` (ADR 0035): every section,
//! `deny_unknown_fields`, compact hostile-safe diagnostics (#73).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Default keymap preset (decision from 2026-07-10).
pub const DEFAULT_PRESET: &str = "orthodox";

/// General configuration from `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NorteToml {
    /// Keymap settings.
    #[serde(default)]
    pub keymap: KeymapSection,
    /// User-interface settings.
    #[serde(default)]
    pub ui: UiSection,
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonSection,
    /// Archive-provider limits (`[archive]`, #95.2).
    #[serde(default)]
    pub archive: ArchiveSection,
    /// Favourite directories shown by `Ctrl+D`.
    ///
    /// Entries accumulate across layers instead of replacing lower-layer
    /// values. The project layer is excluded while [`crate::load::load`]
    /// merges the list. An absent value contributes no favourites from that
    /// layer.
    #[serde(default)]
    pub hotlist: Vec<HotlistEntry>,
    /// AI subsystem settings (`[ai]`, ADR 0031/0035). Honored from
    /// System+User layers only — never Project (fail-closed, same carve-out
    /// as `[archive]`).
    #[serde(default)]
    pub ai: AiSection,
}

/// One `[[hotlist]]` entry as stored in `norte.toml`.
///
/// `path` contains an unvalidated wire value such as `scheme://...`, including
/// valid remote schemes. [`crate::load::load`] validates it as a `VPath`
/// while merging layers. One invalid entry is handled independently and does
/// not prevent the rest of `norte.toml` from loading.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HotlistEntry {
    /// Name displayed in the `Ctrl+D` popup.
    pub name: String,
    /// Path in wire form, not yet validated.
    pub path: String,
}

/// The `[archive]` section of `norte.toml` (#95.2): local anti-bomb limits
/// for browsing zip/tar/tar.gz containers. Absent values keep the compiled
/// defaults. Applied at startup on the embedded engine only — a container
/// that exceeds them fails with `LimitExceeded`, never silently truncates.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArchiveSection {
    /// Maximum indexed entries per container (default 500000).
    #[serde(default)]
    pub max_entries: Option<u64>,
    /// Decompression budget in bytes for indexing a `tar.gz` (default 64 GiB).
    #[serde(default)]
    pub max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56): tope de capas de archivo anidadas.
    pub max_nesting: Option<usize>,
}

/// The `[daemon]` section of `norte.toml` (ADR 0011).
///
/// Transport mode is selected at startup and is not hot reloaded. Changing it
/// requires restarting the frontend.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DaemonSection {
    /// `embedded` for immediate startup (the default), or `daemon`.
    #[serde(default)]
    pub mode: Option<DaemonMode>,
    /// Daemon socket path. When absent, use the operating-system default.
    #[serde(default)]
    pub socket: Option<PathBuf>,
}

/// Core transport. This changes transport only, not behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    /// Core in-process (default).
    Embedded,
    /// Connect to the Unix-domain-socket daemon (Unix only; ADR 0011).
    Daemon,
}

/// The `[ui]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// Language (`es` or `en`). When absent, negotiate from the environment.
    #[serde(default)]
    pub lang: Option<String>,
    /// Theme preset name (`default`, `catppuccin-mocha`, `gruvbox-dark`,
    /// `nord`, `gruvbox-light`, or `catppuccin-latte`) or a path to a custom
    /// TOML theme (ADR 0020). When absent, use `default`.
    #[serde(default)]
    pub theme: Option<String>,
    /// Quick-search mode for `/`: `"filter"` narrows the listing (the default),
    /// while `"jump"` moves the cursor without changing the listing.
    ///
    /// [`crate::load::load`] rejects other values so its diagnostic can
    /// include the source configuration path. Invalid values never silently
    /// fall back.
    #[serde(default)]
    pub quick_search: Option<String>,
    /// UI font family for GUI chrome (GP: `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`).
    /// When absent, use the platform default (bundled fallback).
    #[serde(default)]
    pub font: Option<String>,
    /// Monospace font family for listings/viewer (GP spec, same design doc).
    /// When absent, use the bundled monospace font.
    #[serde(default)]
    pub mono_font: Option<String>,
    /// Base UI font size in pixels (GP spec, same design doc). Valid range is
    /// `[8.0, 32.0]`; [`crate::load::load`] rejects values outside that range
    /// so its diagnostic can include the source configuration path — it is
    /// NOT clamped in silence.
    #[serde(default)]
    pub font_size: Option<f32>,
    /// Accessibility motion override (spec §17 a11y; GUI phase G2). `true`
    /// forces every animated GUI effect off — CRT flicker, cursor blink, and
    /// any future `with_animation` use — regardless of what a theme's
    /// `[effects]` section declares. `None`/absent leaves motion as the
    /// theme requests it; GPUI exposes no platform-level "prefers reduced
    /// motion" hint at this revision, so the effective default when absent
    /// is `false` (motion allowed), not an OS query. `Option` for the same
    /// absent-vs-explicit reason as `[ai] enabled`.
    #[serde(default)]
    pub reduce_motion: Option<bool>,
    /// Whether `app.quit` confirms before closing (S2): `"auto"` (default)
    /// confirms only with pending work (active tasks in the TUI's board,
    /// tasks/marks in the GUI — the behavior before this setting existed,
    /// preserved as the default rather than a fixed point in time);
    /// `"always"` always confirms, even with nothing pending; `"never"`
    /// closes immediately.
    ///
    /// [`crate::load::load`] rejects other values (same pattern as
    /// `quick_search`) so its diagnostic can include the source path.
    #[serde(default)]
    pub confirm_quit: Option<String>,
}

/// The `[keymap]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Base preset (`orthodox`, `vim`, or `cua`). Inherit when absent.
    #[serde(default)]
    pub preset: Option<String>,
}

/// The `[ai]` section of `norte.toml` (ADR 0031). All off by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiSection {
    /// AI enabled. `None`/`false` = the gate rejects every operation.
    /// `Option` (not a plain `bool`) so layer merge distinguishes "absent"
    /// (inherit the lower layer's value) from "explicitly false".
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Local-only mode: reject remote providers (spec §9). `Option` for the
    /// same absent-vs-false reason as `enabled` above.
    #[serde(default)]
    pub local_only: Option<bool>,
    /// Prefixes whose content/names never leave the process. Wire strings;
    /// validated to `VPath` during merge ([`crate::load::load`]). Merge is
    /// a UNION across layers, not last-wins: a deny never disappears by
    /// adding a layer (ADR 0035).
    #[serde(default)]
    pub denied_prefixes: Vec<String>,
    /// Provider name used for AI rename. By-name, later-layer-wins merge
    /// (same as other `Option` scalars in this schema).
    #[serde(default)]
    pub rename_provider: Option<String>,
    /// Declared providers (`[ai.providers.<name>]`). By-name, later-layer-wins
    /// merge: a provider redeclared in a higher layer replaces the lower
    /// layer's entry for that name, other names are untouched.
    #[serde(default)]
    pub providers: BTreeMap<String, AiProviderEntry>,
}

/// One `[ai.providers.<name>]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AiProviderEntry {
    /// `anthropic` | `ollama` | `openai-compat`. An invalid value is not
    /// rejected at parse time — same pattern as `[ui] quick_search` — the AI
    /// gate rejects it at use, where the diagnostic can name the provider.
    pub kind: String,
    /// Model id as the provider expects it.
    pub model: String,
    /// Base URL (required for `openai-compat`).
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Error de carga de config. Siempre con el ARCHIVO en el diagnóstico.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No se pudo leer un archivo que existe.
    #[error("no se pudo leer {}: {source}", path.display())]
    Io {
        /// El archivo.
        path: PathBuf,
        /// La causa.
        source: std::io::Error,
    },
    /// TOML inválido o con claves desconocidas.
    #[error("{}: {message}", path.display())]
    Toml {
        /// El archivo.
        path: PathBuf,
        /// Diagnóstico del parser (incluye campo y posición).
        message: String,
    },
}

/// Diagnóstico COMPACTO de un error de `toml`: posición + mensaje semántico.
/// El `Display` multilínea del crate cita ENTERA la línea del fichero —
/// contenido potencialmente hostil/kilométrico que además desplazaría lo
/// accionable («unknown field …», que va al final) fuera del tope de la
/// barra (#73).
///
/// Wired into [`crate::load::load`]'s error path.
pub(crate) fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// Lee un archivo si existe; `None` si no está (una capa ausente no es
/// error), `Err` si existe pero no se puede leer.
#[doc(hidden)]
pub fn read_optional(path: &std::path::Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The latent bug this task fixes: the strict struct must accept `[ai]`
    /// (ADR 0031 documents it in norte.toml; the old TUI struct rejected it).
    #[test]
    fn norte_toml_estricto_acepta_seccion_ai() {
        let doc = r#"
[ui]
theme = "nord"

[ai]
enabled = true
local_only = true
denied_prefixes = ["file:///secret"]
rename_provider = "local"

[ai.providers.local]
kind = "ollama"
model = "llama3"
"#;
        let parsed: NorteToml = toml::from_str(doc).expect("[ai] es sección canónica");
        assert_eq!(parsed.ai.enabled, Some(true));
        assert_eq!(parsed.ai.local_only, Some(true));
        assert_eq!(parsed.ai.denied_prefixes, vec!["file:///secret".to_owned()]);
        assert_eq!(parsed.ai.rename_provider, Some("local".to_owned()));
        assert_eq!(parsed.ai.providers.len(), 1);
        let provider = parsed.ai.providers.get("local").expect("declarado");
        assert_eq!(provider.kind, "ollama");
        assert_eq!(provider.model, "llama3");
    }

    /// `deny_unknown_fields` pinned on the NEW `[ai]` struct too: a typo in
    /// its top-level fields is a hard error, not silently ignored.
    #[test]
    fn ai_section_campo_desconocido_es_error() {
        assert!(toml::from_str::<NorteToml>("[ai]\nenabld = true\n").is_err());
    }

    /// `deny_unknown_fields` pinned on `[ai.providers.<name>]` too.
    #[test]
    fn ai_provider_entry_campo_desconocido_es_error() {
        let doc = r#"
[ai.providers.x]
kind = "ollama"
model = "m"
modle = "typo"
"#;
        assert!(toml::from_str::<NorteToml>(doc).is_err());
    }

    /// Strictness is uniform: a typo anywhere is a hard error.
    #[test]
    fn campo_desconocido_sigue_siendo_error() {
        assert!(toml::from_str::<NorteToml>("[ui]\ntheem = \"nord\"\n").is_err());
    }
}

#[cfg(test)]
mod toml_diag_tests {
    use super::*;

    /// #73: el diagnóstico compacto conserva posición + mensaje semántico y
    /// NO cita la línea del fichero — un TOML hostil puede meter valores
    /// kilométricos/bidi que desplazarían lo accionable fuera del tope de la
    /// barra (hallazgo MEDIA-1 del encoding-auditor).
    #[test]
    fn toml_diag_compacto_sin_citar_el_contenido() {
        let hostil = format!("v = \"{}\u{202E}\"\nbad", "x".repeat(300));
        let e = toml::from_str::<NorteToml>(&hostil).expect_err("no parsea");
        let d = toml_diag(&hostil, &e);
        assert!(!d.contains("xxx"), "no cita el contenido: {d}");
        assert!(!d.contains('\u{202E}'), "sin bidi: {d}");
        assert!(d.len() < 200, "compacto ({} bytes): {d}", d.len());
        assert!(d.contains("line "), "la posición sobrevive: {d}");
    }

    /// El span puede faltar (errores semánticos sin posición): mensaje solo.
    #[test]
    fn toml_diag_sin_span_no_panica() {
        let e = toml::from_str::<NorteToml>("keymap = 3").expect_err("no valida");
        let _ = toml_diag("keymap = 3", &e);
    }
}
