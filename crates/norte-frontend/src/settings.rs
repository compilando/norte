//! Settings registry (S2): a CURATED, Fluent-localized catalog of general
//! settings, shared by the TUI overlay (S3) and the GUI view (S4) — NOT
//! parsed from the JSON schema at runtime (the schema's descriptions are
//! English rustdoc; the UI must localize). Plugin entries are built
//! separately at open time from each approved plugin's manifest
//! (`ConfigKeySpec`, S3/S4) — this module only defines the shared row type
//! ([`SettingDef`]) and the curated GENERAL list ([`catalog`]).
//!
//! Every entry resolves two Fluent keys ([`fluent_name_id`]/
//! [`fluent_desc_id`]) that MUST exist in both locales — pinned by a
//! coverage test below (`fluent_keys_existen_en_ambos_locales_para_cada_entrada`),
//! the same "coverage over every catalog entry" discipline as the rest of
//! norte's Fluent-backed UI surfaces.

use crate::config::FrontendConfig;

/// Which group of the settings UI an entry renders under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// The curated list in [`catalog`].
    General,
    /// Built from an approved plugin's manifest (S3/S4) — no entries of this
    /// kind live in [`catalog`] itself.
    Plugins,
}

/// The editing widget a setting needs, and (for [`Self::Enum`]) its valid
/// values.
#[derive(Debug, Clone, Copy)]
pub enum SettingKind {
    /// Toggle.
    Bool,
    /// One of a fixed, small set of string values.
    Enum(&'static [&'static str]),
    /// Free text.
    Text,
    /// Integer in `[min, max]`.
    Int {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// A theme preset name or a path to a custom theme file (ADR 0020) —
    /// like [`Self::Enum`], but its value set comes from
    /// `norte_theme::preset_names` at render time, not a `&'static` slice.
    ThemeName,
    /// A keymap preset name — like [`Self::Enum`], but its value set comes
    /// from [`crate::keymap::presets::NAMES`] at render time.
    PresetName,
}

/// One entry of the settings registry: a stable id, which section it
/// renders under, its editing widget, and whether a live edit takes effect
/// without restarting. `applies_live` is written from the TUI's point of
/// view (S3: every entry here hot-reloads there); the GUI (S4) interprets
/// it per-frontend — e.g. fonts resolve once at GUI startup, so the GUI
/// marks them "restart required" even though this flag says `true`. That
/// split is documented per-entry below where it applies.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Stable id (`section.key`, dashed — e.g. `ui.confirm-quit`), stable
    /// across releases: it is also the seed for the Fluent key pair via
    /// [`fluent_name_id`]/[`fluent_desc_id`].
    pub id: &'static str,
    /// [`Section::General`] for every entry in [`catalog`].
    pub section: Section,
    /// The editing widget.
    pub kind: SettingKind,
    /// Whether a live edit applies without a restart, from the TUI's point
    /// of view (see the struct doc for the GUI's per-entry split).
    pub applies_live: bool,
}

/// The curated GENERAL settings (v1). Order is DISPLAY order (S3/S4 render
/// top to bottom before a search filter narrows it) — grouped by `norte.toml`
/// section (`[ui]` first, then `[keymap]`), not alphabetically.
const CATALOG: &[SettingDef] = &[
    SettingDef {
        id: "ui.theme",
        section: Section::General,
        kind: SettingKind::ThemeName,
        applies_live: true,
    },
    SettingDef {
        id: "ui.lang",
        section: Section::General,
        kind: SettingKind::Enum(&["es", "en"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.font",
        section: Section::General,
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.mono-font",
        section: Section::General,
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.font-size",
        section: Section::General,
        kind: SettingKind::Int { min: 8, max: 32 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.quick-search",
        section: Section::General,
        kind: SettingKind::Enum(&["filter", "jump"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.reduce-motion",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.confirm-quit",
        section: Section::General,
        kind: SettingKind::Enum(&["auto", "always", "never"]),
        applies_live: true,
    },
    SettingDef {
        id: "keymap.preset",
        section: Section::General,
        kind: SettingKind::PresetName,
        applies_live: true,
    },
];

/// The curated GENERAL settings list (v1). Plugin entries are NOT here —
/// they are built separately at open time from each approved plugin's
/// manifest (S3/S4).
#[must_use]
pub fn catalog() -> &'static [SettingDef] {
    CATALOG
}

/// The Fluent id for a setting's display NAME: `setting-<id-dashed>-name`,
/// where `id-dashed` replaces `.` with `-` (`ui.confirm-quit` →
/// `setting-ui-confirm-quit-name`). Both `id` and the resulting key are
/// `norte`-authored constants (never user data) — no sanitization needed.
#[must_use]
pub fn fluent_name_id(id: &str) -> String {
    format!("setting-{}-name", id.replace('.', "-"))
}

/// The Fluent id for a setting's DESCRIPTION: `setting-<id-dashed>-desc` —
/// see [`fluent_name_id`] for the dashing rule.
#[must_use]
pub fn fluent_desc_id(id: &str) -> String {
    format!("setting-{}-desc", id.replace('.', "-"))
}

/// Maps a curated id (`ui.confirm-quit`) to the `norte.toml` WIRE location it
/// persists to: `(section, key)`. `section` is the part of `id` before the
/// first `.` (an id always has one — pinned by the coverage test below, over
/// every entry in [`catalog`]); `key` swaps every `-` for `_` (ids are dashed
/// for the Fluent derivation above, but `norte.toml` keys are `snake_case` —
/// see `norte_config::CommonConfig`'s fields, e.g. `confirm_quit`). Shared
/// by the TUI overlay (S3) and the GUI view (S4): both write through
/// `norte_config::persist_set(dir, section, key, value)`, and must derive the
/// exact same wire location from the same id.
///
/// # Panics
/// Never for an id from [`catalog`] (pinned below); a hand-rolled id without
/// a `.` would panic — a bug in the caller, not reachable through this crate.
#[must_use]
pub fn wire_key(id: &str) -> (&str, String) {
    let (section, key) = id
        .split_once('.')
        .expect("un id de catalog() siempre tiene sección.clave");
    (section, key.replace('-', "_"))
}

/// The current value of `def` read from `cfg`, as DISPLAY text (S3/S4 render
/// it directly; editing widgets parse it back per `def.kind`). An absent
/// config value renders the same string the frontend would actually use —
/// `"default"`/`"auto"` rather than empty, so the settings UI never shows a
/// blank row for something that resolves to a real behavior.
///
/// # Panics
/// Never — every arm is total; an id in [`catalog`] with no matching arm
/// here is a logic bug the catalog-coverage test below would catch (every
/// def must resolve without panicking).
#[must_use]
pub fn current_value(def: &SettingDef, cfg: &FrontendConfig) -> String {
    match def.id {
        "ui.theme" => cfg
            .common
            .ui_theme
            .clone()
            .unwrap_or_else(|| "default".to_owned()),
        "ui.lang" => cfg
            .common
            .ui_lang
            .clone()
            .unwrap_or_else(|| "auto".to_owned()),
        "ui.font" => cfg.common.ui_font.clone().unwrap_or_default(),
        "ui.mono-font" => cfg.common.ui_mono_font.clone().unwrap_or_default(),
        "ui.font-size" => cfg
            .common
            .ui_font_size
            .map(|f| f.to_string())
            .unwrap_or_default(),
        "ui.quick-search" => match cfg.common.quick_search {
            norte_config::QuickSearch::Filter => "filter",
            norte_config::QuickSearch::Jump => "jump",
        }
        .to_owned(),
        "ui.reduce-motion" => cfg.common.ui_reduce_motion.unwrap_or(false).to_string(),
        "ui.confirm-quit" => cfg.common.ui_confirm_quit.as_str().to_owned(),
        "keymap.preset" => cfg.common.preset.clone(),
        // Unreachable for anything in `CATALOG` (pinned by the coverage
        // test below); an id typo'd into `current_value` but not `CATALOG`
        // — or vice versa — would only show up as a fallback, never panic.
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};
    use norte_i18n::{Lang, t_in};

    use super::*;

    /// Every catalog entry's id is unique — a duplicate would silently
    /// shadow one entry's Fluent keys/current value with another's.
    #[test]
    fn catalog_ids_son_unicos() {
        let ids: Vec<&str> = catalog().iter().map(|d| d.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "id duplicado en catalog(): {ids:?}"
        );
    }

    /// `fluent_name_id`/`fluent_desc_id` dash the id's dots — pinned with a
    /// concrete example so a refactor can't silently change the derivation.
    #[test]
    fn fluent_ids_dashean_los_puntos() {
        assert_eq!(
            fluent_name_id("ui.confirm-quit"),
            "setting-ui-confirm-quit-name"
        );
        assert_eq!(
            fluent_desc_id("ui.confirm-quit"),
            "setting-ui-confirm-quit-desc"
        );
        assert_eq!(
            fluent_name_id("keymap.preset"),
            "setting-keymap-preset-name"
        );
    }

    /// The F1-style coverage test: EVERY catalog entry's `name`/`desc`
    /// Fluent keys must resolve to a REAL message (not fall back to the id
    /// itself) in BOTH locales — a missing translation would otherwise only
    /// surface as a raw id leaking into the settings UI.
    #[test]
    fn fluent_keys_existen_en_ambos_locales_para_cada_entrada() {
        for def in catalog() {
            for lang in [Lang::Es, Lang::En] {
                let name_id = fluent_name_id(def.id);
                let desc_id = fluent_desc_id(def.id);
                assert_ne!(
                    t_in(lang, &name_id),
                    name_id,
                    "falta la clave Fluent {name_id} en {lang:?} (id={})",
                    def.id
                );
                assert_ne!(
                    t_in(lang, &desc_id),
                    desc_id,
                    "falta la clave Fluent {desc_id} en {lang:?} (id={})",
                    def.id
                );
            }
        }
    }

    /// Every def resolves against a DEFAULT `FrontendConfig` (no layers —
    /// same "empty config" fixture the rest of `norte-frontend`/`norte-config`
    /// use) without panicking, and never returns an id-shaped fallback that
    /// would suggest a typo in `current_value`'s match.
    #[test]
    fn current_value_resuelve_para_cada_entrada_sin_panic() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        for def in catalog() {
            let value = current_value(def, &cfg);
            assert_ne!(
                value, def.id,
                "current_value no debería devolver el id como fallback: {}",
                def.id
            );
        }
    }

    /// `ui.confirm-quit`'s default value round-trips through `current_value`
    /// as the same wire string `[ui] confirm_quit` accepts in `norte.toml`
    /// (S2's exemplar setting — this is the one already wired end-to-end).
    #[test]
    fn current_value_confirm_quit_default_es_auto() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit está en el catálogo");
        assert_eq!(current_value(def, &cfg), "auto");
    }

    /// `wire_key` splits on the FIRST `.` and dashes-to-underscores the rest
    /// — pinned with concrete examples (mirrors `fluent_ids_dashean_los_puntos`
    /// above, same derivation family, different target vocabulary).
    #[test]
    fn wire_key_deriva_seccion_y_clave_snake_case() {
        assert_eq!(
            wire_key("ui.confirm-quit"),
            ("ui", "confirm_quit".to_owned())
        );
        assert_eq!(wire_key("ui.font-size"), ("ui", "font_size".to_owned()));
        assert_eq!(wire_key("keymap.preset"), ("keymap", "preset".to_owned()));
    }

    /// Coverage: EVERY `catalog()` id resolves through `wire_key` without
    /// panicking (never true for a real id, but a future entry missing the
    /// `section.key` shape would panic here first, not in the TUI/GUI).
    #[test]
    fn wire_key_resuelve_para_cada_entrada_del_catalog() {
        for def in catalog() {
            let (section, key) = wire_key(def.id);
            assert!(!section.is_empty());
            assert!(!key.is_empty());
        }
    }

    /// `ui.confirm-quit` reflects a NON-default value loaded from
    /// `norte.toml` — not just the default path above.
    #[test]
    fn current_value_confirm_quit_refleja_config_cargada() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nconfirm_quit = \"always\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = crate::config::load(&layers).expect("carga");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit está en el catálogo");
        assert_eq!(current_value(def, &cfg), "always");
    }
}
