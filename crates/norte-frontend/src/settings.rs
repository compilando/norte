//! Settings registry (S2) + the shared pure editor state machine (S3/S4
//! hoist): a CURATED, Fluent-localized catalog of general settings, shared
//! by the TUI overlay (S3) and the GUI full-view swap (S4) — NOT parsed from
//! the JSON schema at runtime (the schema's descriptions are English
//! rustdoc; the UI must localize).
//!
//! Every entry resolves two Fluent keys ([`fluent_name_id`]/
//! [`fluent_desc_id`]) that MUST exist in both locales — pinned by a
//! coverage test below (`fluent_keys_existen_en_ambos_locales_para_cada_entrada`),
//! the same "coverage over every catalog entry" discipline as the rest of
//! norte's Fluent-backed UI surfaces.
//!
//! [`build_rows`]/[`Row`]/[`SettingsState`]/[`PendingWrite`]/
//! [`SettingsEditError`] landed in the TUI first (S3, `norte-tui/src/{app,
//! settings}.rs`) with ZERO TUI-specific coupling (no ratatui/crossterm
//! types, only [`crate::nav::fold`] and [`FrontendConfig`], both already
//! shared) — S4 hoists them here rather than duplicating the same pure state
//! machine in the GUI (CLAUDE.md rule 7: business logic belongs in the core
//! or a shared frontend crate). The TUI now re-exports these names from its
//! own `app`/`settings` modules for source compatibility.

use crate::config::FrontendConfig;
use norte_i18n::t;

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
    /// Una LÍNEA DE ÓRDENES: se teclea como texto y se guarda como ARRAY de
    /// tokens (`zed %f` → `["zed", "%f"]`).
    ///
    /// Existe porque `[ui] editor` no es una cadena en el fichero: es un argv,
    /// y guardarlo como cadena haría que la siguiente carga lo rechazara. El
    /// troceo es por espacios ASCII, la misma convención con la que `$EDITOR`
    /// admite `code -w` — el precio es un programa cuyo binario lleve un
    /// espacio, que hay que escribir en el fichero a mano.
    Args,
    /// A NUMBER in `[min, max]` — despite the name, the buffer parses as
    /// `f64` and accepts a fractional part (revisión S, M4): `ui.font-size`
    /// is the only entry using this kind, and its underlying config field
    /// (`CommonConfig::ui_font_size`) is `f32`, not an integer — a
    /// hand-edited `font_size = 14.5` was previously un-editable from this
    /// UI (the old strict `i64` parse rejected it outright). `min`/`max`
    /// stay `i64` (every bound in the catalog today is a whole number;
    /// widening them to `f64` for one entry wasn't worth the churn). The
    /// written [`toml_edit::Value`] is an Integer when the parsed number has
    /// no fractional part (keeps `norte.toml` looking the same as before
    /// for the common whole-number case) and a Float otherwise — see
    /// [`SettingsState::edit_commit`].
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
/// it per-frontend, and that split is documented per-entry below where it
/// applies.
///
/// The fonts used to be the example here — "they resolve once at GUI startup,
/// so the GUI marks them restart-required". They did not resolve at all: no
/// frontend read them. They now cross in the window's startup catalogue and
/// re-apply whenever it is rebuilt, which is the same path the theme takes.
/// A terminal still applies none of the four, and says so.
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
        // TUI only: the GUI has no terminal to share the pointer with, so
        // there is nothing there for this to turn off. It is in the CURATED
        // catalog anyway because it is the one key a user needs to find
        // when the terminal stops selecting text (see the `mouse` help
        // topic) — and a setting you only learn about from a config file
        // you did not know existed is not discoverable.
        id: "ui.mouse",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // Solo TUI, como `ui.mouse`. En el catálogo porque apagada por
        // defecto nadie la encontraría, y quien la busca es quien acaba de
        // pulsar Alt en el terminal y no ha pasado nada.
        id: "ui.alt-menu",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La barra de menú fijada. Está en el catálogo por lo mismo que
        // `ui.mouse`: es la clave que alguien va a buscar en cuanto quiera
        // recuperar esa fila, y un ajuste del que solo te enteras leyendo un
        // fichero de config que no sabías que existía no es descubrible.
        id: "ui.menu-bar",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La barra de paneles (#324), y aquí el argumento es el de la propia
        // feature: existe porque un panel que no se ve no lo encuentra nadie.
        // Dejar su interruptor solo en un fichero de config sería cometer el
        // mismo error una capa más arriba.
        id: "ui.panel-bar",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La fila `..`. Mismo criterio que `ui.mouse` y `ui.menu-bar`: no
        // tiene comando ni tecla, así que el fichero era el ÚNICO sitio desde
        // el que se podía apagar o encender.
        id: "ui.parent-entry",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El default de arranque de los ocultos. `pane.toggle-hidden` alterna
        // la SESIÓN y no persiste nada, así que sin esta fila el valor con el
        // que norte abre solo se podía cambiar escribiendo el fichero.
        id: "ui.show-hidden",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El editor de `pane.edit`. Va aquí y no solo en el fichero por lo
        // mismo que el resto: es lo primero que alguien quiere cambiar, y
        // hasta ahora se elegía por variable de entorno, que es el sitio donde
        // menos se busca la configuración de un programa.
        id: "ui.editor",
        section: Section::General,
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // Y si ese editor abre ventana propia. Sin esta fila, poner un editor
        // gráfico deja la terminal en blanco y no hay nada en pantalla que
        // explique por qué.
        id: "ui.editor-detached",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El comparador de `pane.compare-files` (#312), por lo mismo que el
        // editor: sin fila, el que compara dos ficheros solo se elige
        // escribiendo el fichero de configuración.
        id: "ui.diff",
        section: Section::General,
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // Y si ese comparador abre ventana propia (Meld, Kompare).
        id: "ui.diff-detached",
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
    // ─── El cromo (spec 2026-09-10): cada uno existe porque un lector lo
    //     echa de menos en la primera hora, y un interruptor que solo vive en
    //     el fichero es un interruptor que no encuentra nadie.
    SettingDef {
        id: "ui.key-bar",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.panel-bar-style",
        section: Section::General,
        kind: SettingKind::Enum(&["names", "letters"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.pane-footer",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.date-format",
        section: Section::General,
        kind: SettingKind::Enum(&["smart", "relative", "iso"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.notice-seconds",
        section: Section::General,
        kind: SettingKind::Int { min: 0, max: 600 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.history-size",
        section: Section::General,
        kind: SettingKind::Int { min: 5, max: 64 },
        applies_live: true,
    },
    // Spec 2026-09-15, fase 2: la pantalla de arranque, el panel de procesos
    // que se abre solo y la `/` de las carpetas.
    SettingDef {
        id: "ui.splash",
        section: Section::General,
        kind: SettingKind::Enum(&["brief", "off", "home"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.processes-panel",
        section: Section::General,
        kind: SettingKind::Enum(&["auto", "manual"]),
        applies_live: true,
    },
    // Fase 5, tarea 2: cómo el visor de la TUI pinta una imagen. Clave de
    // TERMINAL — la ventana pinta imágenes por su propia webview y no la lee.
    SettingDef {
        id: "ui.images",
        section: Section::General,
        kind: SettingKind::Enum(&["auto", "kitty", "blocks", "off"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dir-indicator",
        section: Section::General,
        kind: SettingKind::Enum(&["auto", "slash", "none"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dialog-buttons",
        section: Section::General,
        kind: SettingKind::Bool,
        applies_live: true,
    },
    // ─── El tema por esquema del escritorio (spec 2026-09-11, V6): solo la
    //     ventana lo lee, pero el fichero es uno y la pantalla de ajustes
    //     es la misma en los dos frontends.
    SettingDef {
        id: "ui.theme-light",
        section: Section::General,
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.theme-dark",
        section: Section::General,
        kind: SettingKind::Text,
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
        // Absent = captured: the row shows `true`, which is what the TUI
        // actually does, rather than an empty cell for a real behavior.
        "ui.mouse" => cfg.common.ui_mouse.unwrap_or(true).to_string(),
        "ui.alt-menu" => cfg.common.ui_alt_menu.unwrap_or(false).to_string(),
        // Ausente = FIJADA, igual que `ui.mouse`: la fila enseña lo que el
        // frontend hace de verdad. Faltaba, y la consecuencia no era cosmética
        // — con la celda vacía, alternar leía «no es true» y escribía `true`
        // siempre, así que la barra no se podía apagar desde aquí.
        "ui.menu-bar" => cfg.common.ui_menu_bar.unwrap_or(true).to_string(),
        "ui.panel-bar" => cfg.common.ui_panel_bar.unwrap_or(true).to_string(),
        "ui.parent-entry" => cfg.common.ui_parent_entry.unwrap_or(true).to_string(),
        "ui.show-hidden" => cfg.common.ui_show_hidden.unwrap_or(false).to_string(),
        "ui.editor" => cfg.common.ui_editor.clone().unwrap_or_default().join(" "),
        "ui.editor-detached" => cfg.common.ui_editor_detached.unwrap_or(false).to_string(),
        // Ausente = `diff -u`, y la fila lo enseña: es lo que norte hace de
        // verdad, no una celda vacía sobre un comportamiento que existe.
        "ui.diff" => cfg
            .common
            .ui_diff
            .clone()
            .unwrap_or_else(|| vec!["diff".to_owned(), "-u".to_owned(), "%F".to_owned()])
            .join(" "),
        "ui.diff-detached" => cfg.common.ui_diff_detached.unwrap_or(false).to_string(),
        "ui.confirm-quit" => cfg.common.ui_confirm_quit.as_str().to_owned(),
        // Ausente = lo que el frontend hace de verdad, como `ui.menu-bar`.
        "ui.key-bar" => cfg.common.ui_chrome.key_bar().to_string(),
        "ui.panel-bar-style" => cfg.common.ui_chrome.panel_bar_style().as_str().to_owned(),
        "ui.pane-footer" => cfg.common.ui_chrome.pane_footer().to_string(),
        "ui.date-format" => cfg.common.ui_chrome.date_format().as_str().to_owned(),
        "ui.notice-seconds" => cfg.common.ui_chrome.notice_seconds().to_string(),
        "ui.history-size" => cfg.common.ui_chrome.history_size().to_string(),
        "ui.splash" => cfg.common.ui_chrome.splash().as_str().to_owned(),
        "ui.processes-panel" => cfg.common.ui_chrome.processes_panel().as_str().to_owned(),
        "ui.images" => cfg.common.ui_chrome.images().as_str().to_owned(),
        "ui.dir-indicator" => cfg.common.ui_chrome.dir_indicator().as_str().to_owned(),
        "ui.dialog-buttons" => cfg.common.ui_chrome.dialog_buttons().to_string(),
        // Vacío = sin variante: la ventana pinta `theme` en los dos esquemas.
        "ui.theme-light" => cfg.common.ui_theme_light.clone().unwrap_or_default(),
        "ui.theme-dark" => cfg.common.ui_theme_dark.clone().unwrap_or_default(),
        "keymap.preset" => cfg.common.preset.clone(),
        // Unreachable for anything in `CATALOG` (pinned by the coverage
        // test below); an id typo'd into `current_value` but not `CATALOG`
        // — or vice versa — would only show up as a fallback, never panic.
        _ => String::new(),
    }
}

/// One approved+enabled plugin's `[config]` SUMMARY (G3c): built by the
/// caller from `plugins_list` + one `plugin.get_config` call per plugin
/// (async — [`build_rows`] stays pure/sync, the caller fetches these
/// FIRST). Drives one [`Row`] per plugin in the Plugins section; drilling
/// into it (caller-side: `Backend::plugin_get_config` again, then a
/// [`crate::plugin_config::PluginConfigState`]) is how the actual keys get
/// edited — this summary only carries enough to LIST the plugin.
#[derive(Debug, Clone)]
pub struct PluginConfigSummary {
    /// Stable plugin id (`org.norte.demo`) — safe to display as-is
    /// (reverse-DNS charset, core-validated) and to pass back to
    /// `Backend::plugin_get_config`/`plugin_set_config`.
    pub plugin_id: String,
    /// Plugin name, ALREADY masked ([`crate::display_name`] — plugin text,
    /// untrusted).
    pub name: String,
    /// How many `[config.<key>]` entries this plugin declares. A plugin
    /// with `0` is NOT expected here — the caller should already have
    /// filtered it out (nothing to show, nothing to drill into).
    pub key_count: usize,
}

/// One row of a settings view (TUI overlay, S3; GUI full-view swap, S4):
/// built, never computed by the editor ([`SettingsState`] only consumes it).
#[derive(Debug, Clone)]
pub struct Row {
    /// Index into [`catalog`]; `None` for a Plugins-section row (a
    /// per-plugin summary, or the informational "nothing configurable"
    /// fallback — see [`build_rows`]) — never editable through THIS state
    /// machine, [`SettingsState::activate`] recognizes it by this, not by
    /// text.
    def_index: Option<usize>,
    /// The plugin id this row summarizes (G3c), or `None` for a General
    /// row or the informational "nothing configurable" fallback. The
    /// caller checks this BEFORE calling [`SettingsState::activate`] — a
    /// `Some` here means Enter should drill into that plugin's own
    /// [`crate::plugin_config::PluginConfigState`], not call `activate`
    /// (which is a no-op for any row with `def_index: None`, plugin
    /// summary included).
    plugin_id: Option<String>,
    /// Localized (Fluent) name to paint.
    pub name: String,
    /// Localized description — footer/detail line of the selected row.
    pub desc: String,
    /// Current value as display text; empty for a row with nothing single
    /// to show (the informational fallback).
    pub value: String,
}

impl Row {
    /// `true` for ANY row in the Plugins section (summary or the
    /// informational fallback): never editable via [`SettingsState::activate`].
    #[must_use]
    pub fn is_plugins_note(&self) -> bool {
        self.def_index.is_none()
    }

    /// The catalog id this row renders (`ui.confirm-quit`, …), or `None` for
    /// a Plugins-section row. A frontend that needs to derive section/key
    /// ([`wire_key`]) or per-entry behavior from a rendered row (e.g. the
    /// GUI's S4 live-vs-restart-required split, since `Row` keeps
    /// `def_index` private) uses this instead of re-deriving the catalog
    /// index itself.
    #[must_use]
    pub fn id(&self) -> Option<&'static str> {
        self.def_index.map(|i| catalog()[i].id)
    }

    /// The plugin id this row summarizes (G3c), or `None` for a General row
    /// or the informational "nothing configurable" fallback. `Some` is the
    /// caller's signal to drill in on Enter (see the field's own doc).
    #[must_use]
    pub fn plugin_id(&self) -> Option<&str> {
        self.plugin_id.as_deref()
    }
}

/// The Plugins section's rows (G3c — replaces the old P2-era informational
/// note now that `plugin.get_config`/`plugin.set_config` put settings on
/// the wire): one row PER `summaries` entry (`name` = the plugin's masked
/// name, `desc` a localized "press Enter" hint, `value` a localized
/// `"N settings"` count) — never directly editable through THIS state
/// machine (`plugin_id().is_some()` is the caller's cue to drill into a
/// [`crate::plugin_config::PluginConfigState`] instead of calling
/// [`SettingsState::activate`]). An EMPTY `summaries` (no approved+enabled
/// plugin declares any `[config]` key) falls back to a single
/// informational row, same shape as before G3c.
fn plugin_summary_rows(summaries: &[PluginConfigSummary], lang: norte_i18n::Lang) -> Vec<Row> {
    if summaries.is_empty() {
        return vec![Row {
            def_index: None,
            plugin_id: None,
            name: norte_i18n::t_in(lang, "settings-plugins-name"),
            desc: norte_i18n::t_in(lang, "settings-plugins-note"),
            value: String::new(),
        }];
    }
    summaries
        .iter()
        .map(|s| Row {
            def_index: None,
            plugin_id: Some(s.plugin_id.clone()),
            name: s.name.clone(),
            desc: norte_i18n::t_in(lang, "settings-plugins-open-hint"),
            value: norte_i18n::ta_in(
                lang,
                "settings-plugins-key-count",
                &[("count", &s.key_count.to_string())],
            ),
        })
        .collect()
}

/// Builds the rows for a settings view: the GENERAL catalog (S2) × the
/// CURRENT value of `cfg` × localized name/description, plus the Plugins
/// section (G3c) built from `plugin_summaries` — the caller fetches those
/// via `plugins_list` + `plugin.get_config` BEFORE
/// calling this (this function stays pure/sync). Called on OPEN
/// (`app.settings`) and on every successful hot-reload with the current
/// `cfg` (TUI) — same criterion as `help_lines`/`palette_rows`: rebuilt
/// wholesale, never mutated row by row.
#[must_use]
pub fn build_rows(cfg: &FrontendConfig, plugin_summaries: &[PluginConfigSummary]) -> Vec<Row> {
    build_rows_in(cfg, plugin_summaries, norte_i18n::active())
}

/// [`build_rows`] en un idioma DADO.
///
/// La pantalla de ajustes traducía los títulos de sección con el idioma del
/// HOST y el nombre y la descripción de cada opción con el del PROCESO, así
/// que salía a medias en dos idiomas.
#[must_use]
pub fn build_rows_in(
    cfg: &FrontendConfig,
    plugin_summaries: &[PluginConfigSummary],
    lang: norte_i18n::Lang,
) -> Vec<Row> {
    let mut rows: Vec<Row> = catalog()
        .iter()
        .enumerate()
        .map(|(i, def)| Row {
            def_index: Some(i),
            plugin_id: None,
            name: norte_i18n::t_in(lang, &fluent_name_id(def.id)),
            desc: norte_i18n::t_in(lang, &fluent_desc_id(def.id)),
            value: current_value(def, cfg),
        })
        .collect();
    rows.extend(plugin_summary_rows(plugin_summaries, lang));
    rows
}

/// A value pending persistence to `norte.toml`, PRODUCED by
/// [`SettingsState::activate`]/[`SettingsState::edit_commit`] — editing is
/// PURE (no [`SettingsState`] method does I/O); the caller (TUI
/// `main::on_settings_key`, GUI `settings_view`) calls
/// `norte_config::persist_set` off the UI thread (rule 2) and announces the
/// result. `section`/`key` already come in WIRE form ([`wire_key`]).
#[derive(Debug, Clone)]
pub struct PendingWrite {
    /// `[section]` of `norte.toml`.
    pub section: &'static str,
    /// Key within that section (`snake_case`, already converted).
    pub key: String,
    /// The TYPED value to write (native bool/int/string — `persist_set`
    /// serializes each in its own TOML shape, never everything as a string).
    pub value: toml_edit::Value,
    /// Localized name of the setting (for the confirmation message).
    pub name: String,
    /// New value as DISPLAY text (for the message + the optimistic row
    /// update [`SettingsState`] performs when it builds this value).
    pub display: String,
}

impl PendingWrite {
    /// A write of a STRING value (spec 2026-09-10, the first-run wizard):
    /// the typed `toml_edit::Value` is built here so a frontend that never
    /// depends on `toml_edit` can still hand a write to its settings path.
    ///
    /// ```
    /// use norte_frontend::settings::PendingWrite;
    /// let w = PendingWrite::text("ui", "theme", "nord", "Theme".to_owned());
    /// assert_eq!((w.section, w.key.as_str(), w.display.as_str()), ("ui", "theme", "nord"));
    /// assert_eq!(w.value.as_str(), Some("nord"));
    /// ```
    #[must_use]
    pub fn text(section: &'static str, key: &str, value: &str, name: String) -> Self {
        Self {
            section,
            key: key.to_owned(),
            value: toml_edit::Value::from(value),
            name,
            display: value.to_owned(),
        }
    }
}

/// Why [`SettingsState::edit_commit`] rejected the buffer — WITHOUT
/// persisting (S3/S4: "invalid = status-bar/inline error, value untouched").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEditError {
    /// The buffer does not parse as an integer ([`SettingKind::Int`]).
    NotAnInt,
    /// Parses, but falls outside `[min, max]`.
    OutOfRange {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
}

/// Settings editor (`app.settings`, S3 TUI overlay / S4 GUI full-view swap):
/// free search ALWAYS active, cursor over the VISIBLE rows (same pattern as
/// the command palette) plus an inline EDIT mode for `Text`/`Int` rows (raw
/// buffer, Enter confirms, Esc cancels). Editing methods are PURE — they
/// return a [`PendingWrite`] or a [`SettingsEditError`], never do I/O — the
/// caller persists and announces. The Plugins section (a single
/// informational row, [`build_rows`]) is never editable: `def_index == None`
/// makes [`Self::activate`] a no-op over it.
#[derive(Debug, Clone)]
pub struct SettingsState {
    /// Rows ([`Row`]) — snapshot frozen on open, replaced WHOLESALE by
    /// [`Self::refresh`] on every successful hot-reload.
    rows: Vec<Row>,
    /// Folded haystack per row (id + name + description, [`crate::nav::fold`]).
    folds: Vec<String>,
    /// Bytes typed as-is into the filter (unsanitized; sanitizing happens
    /// only when painting, [`Self::query_display`]).
    query: Vec<u8>,
    /// REAL indices into `rows` that match (empty query = all).
    visible: Vec<usize>,
    /// Selection position WITHIN `visible`.
    cursor: usize,
    /// Inline edit buffer (`Text`/`Int`): `Some` = editing the row under the
    /// cursor; `None` = normal browsing/filtering. Raw, like a name-input
    /// popup — sanitizing happens on paint.
    edit: Option<String>,
    /// La primera LÍNEA visible de una lista que no cabe, en la unidad de
    /// quien pinta ([`Self::reconcile_viewport`]).
    ///
    /// No existía porque los ajustes cabían en una pantalla — lo decía el
    /// editor de atajos de la terminal, y era verdad cuando se escribió. Con
    /// ~30 ajustes dejó de serlo: bajar con el cursor pasado el borde lo
    /// dejaba fuera de la caja y la lista no se movía.
    viewport_offset: usize,
}

impl SettingsState {
    /// Deja la ventana lista para pintar `rows` líneas con el cursor a la
    /// vista: la arrastra SÓLO si el cursor se salió, por la regla compartida
    /// de [`crate::viewport::sticky_offset`]. Se llama una vez por frame,
    /// antes de pintar.
    ///
    /// Recibe la línea del cursor y el total YA en líneas de pantalla, y no
    /// en filas, porque quien pinta intercala cabeceras de sección entre las
    /// filas: esa cuenta es suya, y hacerla aquí sería una segunda copia de
    /// cómo se pinta. La ventana, que es web, ni lo llama — el navegador ya
    /// desplaza la fila elegida hasta que se ve.
    pub fn reconcile_viewport(&mut self, cursor_line: usize, total_lines: usize, rows: usize) {
        self.viewport_offset =
            crate::viewport::sticky_offset(self.viewport_offset, cursor_line, total_lines, rows);
    }

    /// La primera línea visible — ver [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Opens the editor over `rows` (a [`build_rows`] snapshot): folds each
    /// row's haystack and starts with an empty query (everything visible),
    /// not editing.
    #[must_use]
    pub fn new(rows: Vec<Row>) -> Self {
        let folds = Self::fold_rows(&rows);
        let mut s = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            edit: None,
            viewport_offset: 0,
        };
        s.recompute();
        s
    }

    fn fold_rows(rows: &[Row]) -> Vec<String> {
        let catalog = catalog();
        rows.iter()
            .map(|r| {
                let id: &str = match (r.def_index, r.plugin_id.as_deref()) {
                    (Some(i), _) => catalog[i].id,
                    (None, Some(pid)) => pid,
                    (None, None) => "plugins",
                };
                crate::nav::fold(format!("{id} {} {}", r.name, r.desc).as_bytes())
            })
            .collect()
    }

    /// Replaces the rows with a FRESH snapshot (hot-reload): recomputes the
    /// fold and re-filters with the CURRENT query (kept, unlike
    /// help/palette overlays, which CLOSE — a settings row is just
    /// `(name, description, value)` read from `cfg`, safe to recompute
    /// without invalidating what the user is doing). The edit buffer, if
    /// any, is ALSO kept raw — a reload must not throw away what the user
    /// already typed.
    pub fn refresh(&mut self, rows: Vec<Row>) {
        self.folds = Self::fold_rows(&rows);
        self.rows = rows;
        self.recompute();
    }

    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            let q = crate::nav::fold(&self.query);
            self.folds
                .iter()
                .enumerate()
                .filter(|(_, f)| f.contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Appends a character to the filter query and recomputes. No-op while
    /// editing ([`Self::is_editing`]) — the caller already branches on that,
    /// but the guard here makes it an invariant OF THE TYPE, not just of the
    /// call site.
    pub fn push_char(&mut self, c: char) {
        if self.edit.is_some() {
            return;
        }
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Removes the last complete UTF-8 char from the query. No-op editing.
    pub fn backspace(&mut self) {
        if self.edit.is_some() || self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Moves the selection up (clamped at the top). No-op editing.
    pub fn up(&mut self) {
        if self.edit.is_none() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// Moves the selection down (clamped at the end). No-op editing.
    pub fn down(&mut self) {
        if self.edit.is_none() && self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Sets the selection to `idx` WITHIN [`Self::visible`], clamped to the
    /// last visible row (or 0 with nothing visible) — the mouse hover/click
    /// selection primitive (GUI, S4; a future TUI mouse mode could reuse it
    /// too). No-op while editing, same guard as [`Self::up`]/[`Self::down`].
    pub fn set_cursor(&mut self, idx: usize) {
        if self.edit.is_none() {
            self.cursor = idx.min(self.visible.len().saturating_sub(1));
        }
    }

    /// Moves the selection up `n` positions (page-up). No-op editing.
    pub fn page_up(&mut self, n: usize) {
        if self.edit.is_none() {
            self.cursor = self.cursor.saturating_sub(n);
        }
    }

    /// Moves the selection down `n` positions, clamped at the end
    /// (page-down). No-op editing.
    pub fn page_down(&mut self, n: usize) {
        if self.edit.is_none() {
            self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
        }
    }

    /// REAL indices into [`Self::rows`] visible under the current query.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// All rows — `rows()[visible()[i]]` paints the `i`-th filtered row.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Selection position WITHIN [`Self::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The localized description of the row under the cursor, if any is
    /// visible — the detail/footer line of the overlay/view.
    #[must_use]
    pub fn selected_desc(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].desc.as_str())
    }

    /// Query text ready to paint (lossy, masked — same contract as the
    /// command palette's query display).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// `true` while the inline edit buffer (`Text`/`Int`) is active.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The RAW edit buffer, for painting (sanitizing happens on paint, same
    /// contract as a raw name-input buffer).
    #[must_use]
    pub fn edit_buffer(&self) -> Option<&str> {
        self.edit.as_deref()
    }

    /// Appends a char to the edit buffer. No-op if not editing.
    pub fn edit_push_char(&mut self, c: char) {
        if let Some(buf) = &mut self.edit {
            buf.push(c);
        }
    }

    /// Removes the last char from the edit buffer. No-op if not editing.
    pub fn edit_backspace(&mut self) {
        if let Some(buf) = &mut self.edit {
            buf.pop();
        }
    }

    /// Replaces the edit buffer WHOLESALE. No-op if not editing.
    ///
    /// For a frontend whose text field is native (the window's prompt): the
    /// caret is the widget's, and the host receives the full text on
    /// confirm rather than one character at a time. Same contract as
    /// [`Self::edit_push_char`] otherwise — raw, unsanitized, validated by
    /// [`Self::edit_commit`].
    pub fn edit_set(&mut self, text: &str) {
        if let Some(buf) = &mut self.edit {
            text.clone_into(buf);
        }
    }

    /// Cancels the edit WITHOUT writing — the row's value stays as it was.
    pub fn edit_cancel(&mut self) {
        self.edit = None;
    }

    /// Enter/click over the row under the cursor: `Bool`/`Enum`/`ThemeName`/
    /// `PresetName` CYCLE immediately (return the [`PendingWrite`] right
    /// away — nothing else to confirm); `Text`/`Int` OPEN the edit buffer
    /// (return `None` — [`Self::edit_commit`] produces the [`PendingWrite`]
    /// once the user confirms). The Plugins informational row and "nothing
    /// visible" also return `None`, without opening anything. `theme_names`/
    /// `preset_names` are the LIVE lists (not `&'static`, resolved at
    /// runtime) — the caller computes them.
    pub fn activate(
        &mut self,
        theme_names: &[String],
        preset_names: &[&str],
    ) -> Option<PendingWrite> {
        let &real = self.visible.get(self.cursor)?;
        let idx = self.rows[real].def_index?;
        let def = &catalog()[idx];
        let current = self.rows[real].value.clone();
        match def.kind {
            SettingKind::Bool => {
                let next = current != "true";
                Some(self.commit_row(real, def, next.to_string(), toml_edit::Value::from(next)))
            }
            SettingKind::Enum(values) => {
                let next = cycle(&current, values);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::ThemeName => {
                let refs: Vec<&str> = theme_names.iter().map(String::as_str).collect();
                let next = cycle(&current, &refs);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::PresetName => {
                let next = cycle(&current, preset_names);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::Text | SettingKind::Int { .. } | SettingKind::Args => {
                self.edit = Some(current);
                None
            }
        }
    }

    /// Confirms the inline edit buffer: `Int` parses the buffer as `f64`
    /// (revisión S, M4 — see [`SettingKind::Int`]'s doc for why a "whole
    /// number" kind accepts a fractional part) and validates `[min, max]`
    /// ([`SettingsEditError`] WITHOUT persisting, buffer intact — the user
    /// corrects and retries); `Text` accepts anything. Only reachable with
    /// [`Self::is_editing`] — the caller guarantees it; without an active
    /// edit this returns `SettingsEditError::NotAnInt` as an inert fallback
    /// (unreachable in practice, defense in depth).
    ///
    /// # Errors
    /// [`SettingsEditError::NotAnInt`] if an `Int` row's buffer does not
    /// parse as a number (or, as an inert fallback, if there is no active
    /// edit); [`SettingsEditError::OutOfRange`] if it parses but falls
    /// outside `[min, max]`. Never for a `Text` row.
    pub fn edit_commit(&mut self) -> Result<PendingWrite, SettingsEditError> {
        let (Some(buf), Some(real)) = (self.edit.clone(), self.visible.get(self.cursor).copied())
        else {
            return Err(SettingsEditError::NotAnInt);
        };
        let Some(idx) = self.rows[real].def_index else {
            return Err(SettingsEditError::NotAnInt);
        };
        let def = &catalog()[idx];
        let write = if let SettingKind::Int { min, max } = def.kind {
            let n: f64 = buf
                .trim()
                .parse()
                .map_err(|_| SettingsEditError::NotAnInt)?;
            // `min`/`max` are catalog constants, always tiny (today: 8/32) —
            // the precision loss `as f64` could theoretically incur past
            // 2^53 never applies here.
            #[expect(clippy::cast_precision_loss, reason = "magnitudes lejos de 2^53")]
            let (min_f, max_f) = (min as f64, max as f64);
            if n < min_f || n > max_f {
                return Err(SettingsEditError::OutOfRange { min, max });
            }
            // Whole number → TOML Integer (keeps `norte.toml` looking the
            // same as before this fix for the common case, "14" not
            // "14.0"); fractional → TOML Float ("14.5"). `n.to_string()`
            // already renders a whole `f64` WITHOUT a trailing ".0" (Rust's
            // `Display` for floats picks the shortest round-tripping form),
            // so `display` needs no separate branch.
            #[expect(clippy::cast_possible_truncation, reason = "n ∈ [min, max], both i64")]
            let value = if n.fract() == 0.0 {
                toml_edit::Value::from(n as i64)
            } else {
                toml_edit::Value::from(n)
            };
            self.commit_row(real, def, n.to_string(), value)
        } else if matches!(def.kind, SettingKind::Args) {
            // Una línea de órdenes se GUARDA como array: `zed %f` viaja como
            // `["zed", "%f"]`, que es lo que el fichero declara. Escribirla
            // como cadena haría que la siguiente carga la rechazara.
            //
            // Vacío = un array vacío, que la configuración lee como «ninguno»
            // y devuelve el mando a `$VISUAL`/`$EDITOR`.
            let mut arr = toml_edit::Array::new();
            for tok in buf.split_ascii_whitespace() {
                arr.push(tok);
            }
            let display = buf.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
            self.commit_row(real, def, display, toml_edit::Value::Array(arr))
        } else {
            // By construction, only `Text`/`Int`/`Args` open `self.edit`
            // (`Self::activate`) — this is the `Text` arm.
            self.commit_row(real, def, buf.clone(), toml_edit::Value::from(buf.as_str()))
        };
        self.edit = None;
        Ok(write)
    }

    /// OPTIMISTIC update of row `real` to `display` + builds its
    /// [`PendingWrite`] (`section`/`key` via [`wire_key`]). The hot-reload
    /// that follows ([`Self::refresh`]) corrects it if the write didn't
    /// apply (I/O failure) — this is just immediate feedback, the truth
    /// lives on disk.
    fn commit_row(
        &mut self,
        real: usize,
        def: &SettingDef,
        display: String,
        value: toml_edit::Value,
    ) -> PendingWrite {
        let (section, key) = wire_key(def.id);
        self.rows[real].value.clone_from(&display);
        PendingWrite {
            section,
            key,
            value,
            name: self.rows[real].name.clone(),
            display,
        }
    }
}

/// Next value in `values` after `current` (wrapping); if `current` isn't in
/// `values` (a config with a value the catalog no longer recognizes, or a
/// dynamic list that changed), starts at the FIRST — never panics on an
/// empty list (returns `current` untouched). `pub(crate)`: also reused by
/// [`crate::plugin_config`] (G3c) — same cycle semantics for a plugin's
/// `enum`/`bool` config keys, one source of truth.
pub(crate) fn cycle(current: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return current.to_owned();
    }
    let next = values
        .iter()
        .position(|v| *v == current)
        .map_or(0, |i| (i + 1) % values.len());
    values[next].to_owned()
}

/// Whether `app.quit` should open a confirmation modal, given the
/// configured `[ui] confirm_quit` mode and whether there is pending work to
/// lose (only consulted for `Auto` — `Never`/`Always` are unconditional).
/// "Pending work" means something different per frontend (TUI:
/// `TaskBoard::has_active`; GUI: tasks/marks/inflight, see
/// `confirm_quit_task_count`) — the caller computes THAT; this is only the
/// three-way decision from the mode, and it was byte-identical in both
/// frontends before this hoist (revisión S, M6: TUI's `quit_needs_confirm`
/// and the GUI's `confirm_quit_should_open`).
#[must_use]
pub fn quit_needs_confirm(mode: norte_config::ConfirmQuit, pending: bool) -> bool {
    match mode {
        norte_config::ConfirmQuit::Never => false,
        norte_config::ConfirmQuit::Always => true,
        norte_config::ConfirmQuit::Auto => pending,
    }
}

/// Status-bar/inline message for a [`SettingsEditError`] — by CATEGORY
/// (Fluent), never ad hoc text (#73 pattern). Shared by the TUI overlay
/// (S3) and the GUI view (S4, revisión S M6): both had their own
/// byte-identical copy of this match before this hoist.
#[must_use]
pub fn edit_error_message(e: &SettingsEditError) -> String {
    match e {
        SettingsEditError::NotAnInt => t("msg-settings-invalid-int"),
        SettingsEditError::OutOfRange { min, max } => norte_i18n::ta(
            "msg-settings-invalid-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
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

    /// Una fila de las que se ALTERNAN tiene que enseñar un valor legible, y
    /// no la cadena vacía.
    ///
    /// El test de arriba no bastaba —una celda vacía no es el id, así que
    /// pasaba— y el agujero no era cosmético: `activate` decide el siguiente
    /// valor leyendo el que se PINTA, así que con la celda vacía un `Bool`
    /// leía «no es true» y escribía `true` siempre. `ui.menu-bar` estuvo así:
    /// en el catálogo, sin brazo en `current_value`, y por tanto imposible de
    /// apagar desde esta pantalla.
    #[test]
    fn una_fila_que_se_alterna_nunca_ensena_una_celda_vacia() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        for def in catalog() {
            let value = current_value(def, &cfg);
            match def.kind {
                SettingKind::Bool => assert!(
                    value == "true" || value == "false",
                    "{} pinta {value:?}, que no es un booleano",
                    def.id
                ),
                // Los `Enum` quedan FUERA a sabiendas: `ui.lang` sin valor
                // pinta `auto`, que no es uno de los suyos —es lo que norte
                // hace, negociar con el entorno— y la primera pulsación cae en
                // el primero de la lista igualmente. Lo que aquí se protege es
                // el caso en el que el valor pintado DECIDE el siguiente y una
                // celda vacía lo decide mal.
                //
                // Los de texto libre SÍ pueden estar vacíos: «sin fuente
                // elegida» y «sin editor elegido» son respuestas válidas.
                SettingKind::Enum(_)
                | SettingKind::Text
                | SettingKind::Args
                | SettingKind::Int { .. }
                | SettingKind::ThemeName
                | SettingKind::PresetName => {}
            }
        }
    }

    /// Teclear una línea de órdenes guarda un ARRAY, que es lo que el fichero
    /// declara: una cadena haría que la siguiente carga la rechazara.
    #[test]
    fn una_fila_de_ordenes_se_guarda_como_array() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        let mut st = SettingsState::new(build_rows(&cfg, &[]));
        let fila = st
            .rows()
            .iter()
            .position(|r| r.def_index.map(|i| catalog()[i].id) == Some("ui.editor"))
            .expect("ui.editor está en el catálogo");
        st.set_cursor(fila);
        assert!(
            st.activate(&[], &[]).is_none(),
            "una línea de órdenes se edita, no se alterna"
        );
        for c in "zed %f".chars() {
            st.edit_push_char(c);
        }
        let write = st.edit_commit().expect("texto libre no falla");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "editor");
        assert_eq!(write.value.to_string().trim(), r#"["zed", "%f"]"#);
        assert_eq!(write.display, "zed %f");
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

    // --- `build_rows` (S3/S4 hoist) ---

    /// One row per catalog entry, plus EXACTLY one informational row at the
    /// end when NO plugin declares any `[config]` key (G3c fallback shape).
    #[test]
    fn build_rows_una_fila_por_entrada_mas_la_de_plugins() {
        let rows = build_rows(&cfg_vacia(), &[]);
        assert_eq!(rows.len(), catalog().len() + 1);
        assert!(!rows[0].is_plugins_note());
        assert!(rows.last().unwrap().is_plugins_note());
        assert_eq!(rows.last().unwrap().plugin_id(), None);
    }

    fn cfg_vacia() -> FrontendConfig {
        crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga")
    }

    /// Each General row's value is EXACTLY what `current_value` (S2) would
    /// resolve for the same `def` — never a diverging copy.
    #[test]
    fn build_rows_valores_coinciden_con_current_value() {
        let cfg = cfg_vacia();
        let rows = build_rows(&cfg, &[]);
        for (i, def) in catalog().iter().enumerate() {
            assert_eq!(rows[i].value, current_value(def, &cfg));
        }
    }

    /// The Plugins informational row carries no value (nothing to edit) and
    /// its name/description resolve to REAL text (not the raw Fluent id) in
    /// this suite's active language.
    #[test]
    fn plugins_note_row_sin_valor_y_con_texto_traducido() {
        let rows = build_rows(&cfg_vacia(), &[]);
        let note = rows.last().unwrap();
        assert_eq!(note.value, "");
        assert_ne!(note.name, "settings-plugins-name");
        assert_ne!(note.desc, "settings-plugins-note");
    }

    /// G3c: a NON-EMPTY `plugin_summaries` yields one row PER summary
    /// (never the informational fallback), each with `plugin_id()` set and
    /// a localized `"N settings"` value — the caller's cue to drill in on
    /// Enter, never to call `SettingsState::activate` on it.
    #[test]
    fn build_rows_con_plugins_una_fila_por_resumen() {
        let summaries = vec![
            PluginConfigSummary {
                plugin_id: "org.a".into(),
                name: "Alpha".into(),
                key_count: 3,
            },
            PluginConfigSummary {
                plugin_id: "org.b".into(),
                name: "Beta".into(),
                key_count: 1,
            },
        ];
        let rows = build_rows(&cfg_vacia(), &summaries);
        assert_eq!(rows.len(), catalog().len() + 2);
        let a = &rows[catalog().len()];
        assert_eq!(a.plugin_id(), Some("org.a"));
        assert_eq!(a.name, "Alpha");
        assert!(
            a.is_plugins_note(),
            "no editable vía SettingsState::activate"
        );
        assert!(a.value.contains('3'));
        let b = &rows[catalog().len() + 1];
        assert_eq!(b.plugin_id(), Some("org.b"));
        assert!(b.value.contains('1'));
    }

    /// `PluginConfigSummary::name` is UNTRUSTED plugin text — a hostile
    /// name (bidi override, corpus `rtl_override`) reaches `Row::name`
    /// UNCHANGED by `build_rows` itself: masking is the CALLER's
    /// responsibility (same contract as `palette::plugin_rows`, which
    /// masks BEFORE building the row) — this pins that `build_rows` does
    /// not double-mask nor accidentally corrupt an already-masked name.
    #[test]
    fn build_rows_con_plugins_no_altera_un_nombre_ya_enmascarado() {
        let masked = crate::display_name("\u{202E}evil".as_bytes()).0;
        let summaries = vec![PluginConfigSummary {
            plugin_id: "org.evil".into(),
            name: masked.clone(),
            key_count: 1,
        }];
        let rows = build_rows(&cfg_vacia(), &summaries);
        assert_eq!(rows.last().unwrap().name, masked);
    }

    // --- `SettingsState`/`PendingWrite`/`SettingsEditError` (S3/S4 hoist) ---

    fn rows() -> Vec<Row> {
        build_rows(&cfg_vacia(), &[])
    }

    /// Filtering by a DASHED fragment of the id (`confirm-quit`) — unlikely
    /// in name/description prose — isolates exactly that row.
    fn only(fragment: &str) -> SettingsState {
        let mut s = SettingsState::new(rows());
        for c in fragment.chars() {
            s.push_char(c);
        }
        assert_eq!(
            s.visible().len(),
            1,
            "el fragmento {fragment:?} debería aislar una sola fila"
        );
        s
    }

    #[test]
    fn settings_filtra_por_id_nombre_o_descripcion() {
        let s = only("confirm-quit");
        assert_eq!(
            s.rows()[s.visible()[0]].name,
            t("setting-ui-confirm-quit-name")
        );
    }

    #[test]
    fn settings_query_hostil_se_enmascara() {
        let mut s = SettingsState::new(rows());
        for c in "a\u{202E}b".chars() {
            s.push_char(c);
        }
        let display = s.query_display();
        assert!(!display.chars().any(norte_encoding::is_terminal_hazard));
        assert!(display.contains('\u{FFFD}'));
    }

    #[test]
    fn settings_sin_matches_no_panica_y_activate_es_none() {
        let mut s = SettingsState::new(rows());
        for c in "zzzznuncacasa".chars() {
            s.push_char(c);
        }
        assert!(s.visible().is_empty());
        s.up();
        s.down();
        s.page_up(3);
        s.page_down(3);
        assert_eq!(s.selected_desc(), None);
        assert!(s.activate(&[], &[]).is_none());
    }

    #[test]
    fn activate_en_bool_toggla_y_devuelve_pendingwrite() {
        let mut s = only("reduce-motion");
        assert_eq!(s.rows()[s.visible()[0]].value, "false", "default");
        let write = s.activate(&[], &[]).expect("Bool activa de inmediato");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "reduce_motion");
        assert_eq!(write.value.as_bool(), Some(true));
        assert_eq!(write.display, "true");
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "optimista");
        assert!(!s.is_editing());
    }

    #[test]
    fn activate_en_enum_cicla_con_wrap() {
        let mut s = only("confirm-quit");
        assert_eq!(s.rows()[s.visible()[0]].value, "auto", "default S2");
        let w1 = s.activate(&[], &[]).unwrap();
        assert_eq!(w1.display, "always");
        let w2 = s.activate(&[], &[]).unwrap();
        assert_eq!(w2.display, "never");
        let w3 = s.activate(&[], &[]).unwrap();
        assert_eq!(w3.display, "auto", "wrap al primero");
        assert_eq!(w3.value.as_str(), Some("auto"));
    }

    #[test]
    fn activate_en_theme_name_cicla_sobre_la_lista_viva() {
        // "ui.theme" es prefijo de `ui.theme-light` y `ui.theme-dark` (spec
        // 2026-09-11, V6): el filtro deja TRES filas, y el cursor queda en la
        // primera, que por orden del catálogo es la del tema a secas.
        let mut s = SettingsState::new(rows());
        for c in "ui.theme".chars() {
            s.push_char(c);
        }
        assert_eq!(s.visible().len(), 3, "theme, theme-light y theme-dark");
        assert_eq!(s.rows()[s.visible()[0]].name, t("setting-ui-theme-name"));
        let names = vec!["default".to_owned(), "nord".to_owned()];
        // El valor actual (default de S2) es "default": el próximo es "nord".
        let write = s.activate(&names, &[]).expect("ThemeName activa");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "theme");
        assert_eq!(write.value.as_str(), Some("nord"));
    }

    #[test]
    fn activate_en_preset_name_cicla_sobre_la_lista_viva() {
        let mut s = only("keymap.preset");
        let presets = ["orthodox", "vim", "cua"];
        let write = s.activate(&[], &presets).expect("PresetName activa");
        assert_eq!(write.section, "keymap");
        assert_eq!(write.key, "preset");
        assert_eq!(write.value.as_str(), Some("vim"), "orthodox → vim (wrap)");
    }

    #[test]
    fn activate_en_text_abre_edicion_sin_persistir() {
        // Espacio final: `ui.font` es PREFIJO de `ui.font-size` (el fold
        // pega `"{id} {name} {desc}"`, así que el espacio que sigue al id
        // ancla el fin de token y descarta ese otro id sin ambigüedad).
        let mut s = only("ui.font ");
        assert!(!s.is_editing());
        let write = s.activate(&[], &[]);
        assert!(write.is_none(), "Text no persiste al abrir: solo edita");
        assert!(s.is_editing());
        assert_eq!(s.edit_buffer(), Some(""));
    }

    #[test]
    fn edit_commit_en_text_persiste_lo_tecleado() {
        let mut s = only("mono-font");
        s.activate(&[], &[]);
        for c in "JetBrains Mono".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("Text siempre válido");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "mono_font");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, "JetBrains Mono");
    }

    /// La ventana no teclea carácter a carácter: su campo es nativo y entrega
    /// el texto entero al confirmar. `edit_set` es esa entrada, y fuera de
    /// una edición no hace nada.
    #[test]
    fn edit_set_reemplaza_el_buffer_entero_y_solo_editando() {
        let mut s = only("mono-font");
        s.edit_set("nada");
        assert!(!s.is_editing(), "sin edición abierta no abre una");
        s.activate(&[], &[]);
        s.edit_set("JetBrains Mono");
        assert_eq!(s.edit_buffer(), Some("JetBrains Mono"));
        let write = s.edit_commit().expect("Text siempre válido");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
    }

    #[test]
    fn edit_commit_en_int_valida_rango_sin_persistir_y_conserva_el_buffer() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "999".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("999 fuera de [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(s.is_editing(), "el buffer se conserva tras un rechazo");
        assert_eq!(s.edit_buffer(), Some("999"));
    }

    #[test]
    fn edit_commit_en_int_no_numerico_rechaza() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "abc".chars() {
            s.edit_push_char(c);
        }
        assert_eq!(s.edit_commit().unwrap_err(), SettingsEditError::NotAnInt);
    }

    #[test]
    fn edit_commit_en_int_valido_persiste() {
        let mut s = only("font-size");
        // El buffer arranca con el valor VIGENTE ("" — sin `[ui] font_size`
        // en la config vacía de este test, `current_value` ya lo documenta).
        s.activate(&[], &[]);
        assert_eq!(s.edit_buffer(), Some(""));
        for c in "16".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("16 está en [8,32]");
        assert_eq!(write.value.as_integer(), Some(16));
        assert_eq!(write.display, "16");
    }

    /// Revisión S, M4: `ui.font-size` acepta un valor FRACCIONARIO
    /// (`[ui] font_size` es `f32` en `norte_config`, no un entero — un
    /// `norte.toml` editado a mano con `font_size = 14.5` era imposible de
    /// re-editar desde aquí antes de este fix, el `i64::parse` estricto lo
    /// rechazaba). Round-trip: "14.5" → `Value::Float(14.5)` + `display`
    /// SIN ceros de más.
    #[test]
    fn edit_commit_en_font_size_acepta_fraccion_y_round_tripea() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "14.5".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("14.5 está en [8,32]");
        assert_eq!(write.value.as_float(), Some(14.5));
        assert_eq!(
            write.value.as_integer(),
            None,
            "no debe escribirse como entero"
        );
        assert_eq!(write.display, "14.5");
    }

    /// Un valor fraccionario FUERA de rango (p. ej. `33.5`) sigue
    /// rechazándose — el parse más permisivo (`f64` en vez de `i64`) no
    /// debilita la validación de `[min, max]`.
    #[test]
    fn edit_commit_en_font_size_fraccion_fuera_de_rango_rechaza() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "33.5".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("33.5 fuera de [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
    }

    #[test]
    fn edit_cancel_no_persiste_y_conserva_el_valor_original() {
        let mut s = only("mono-font");
        let original = s.rows()[s.visible()[0]].value.clone();
        s.activate(&[], &[]);
        s.edit_push_char('x');
        s.edit_cancel();
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, original);
    }

    /// La fila informativa de Plugins (última con query vacía) nunca abre
    /// edición ni produce un `PendingWrite`.
    #[test]
    fn activate_en_fila_informativa_de_plugins_es_no_op() {
        let mut s = SettingsState::new(rows());
        let n = catalog().len();
        for _ in 0..n {
            s.down();
        }
        assert!(s.rows()[s.visible()[s.cursor()]].is_plugins_note());
        assert!(s.activate(&[], &[]).is_none());
        assert!(!s.is_editing());
    }

    /// `refresh` (hot-reload) rebuilds the VALUES but keeps the query and
    /// cursor the user typed/moved.
    #[test]
    fn refresh_conserva_query_y_recalcula_valores() {
        let mut s = only("reduce-motion");
        assert_eq!(s.rows()[s.visible()[0]].value, "false");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nreduce_motion = true\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = crate::config::load(&layers).expect("carga");
        s.refresh(build_rows(&cfg, &[]));
        assert_eq!(
            s.visible().len(),
            1,
            "la query 'reduce-motion' se conserva tras el refresh"
        );
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "valor fresco");
    }

    #[test]
    fn set_cursor_clampa_al_ultimo_visible() {
        let mut s = SettingsState::new(rows());
        let last = s.visible().len() - 1;
        s.set_cursor(last + 50);
        assert_eq!(s.cursor(), last, "clampa al último visible");
        s.set_cursor(0);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_es_no_op_mientras_se_edita() {
        let mut s = SettingsState::new(rows());
        // Sin filtrar: TODAS las filas siguen visibles, así que si el guard
        // de edición fallara habría a dónde moverse de verdad. `down()` dos
        // veces aterriza en "ui.font" (índice 2 del catálogo: theme, lang,
        // font), una fila `Text` — activarla abre edición.
        s.down();
        s.down();
        let idx = s.cursor();
        assert_eq!(s.rows()[s.visible()[idx]].name, t("setting-ui-font-name"));
        s.activate(&[], &[]);
        assert!(s.is_editing());
        s.set_cursor(0);
        assert_eq!(
            s.cursor(),
            idx,
            "editando, un click en otra fila no mueve el cursor"
        );
    }

    #[test]
    fn row_id_devuelve_el_id_del_catalogo_o_none_para_la_nota_de_plugins() {
        let rows = rows();
        for (i, def) in catalog().iter().enumerate() {
            assert_eq!(rows[i].id(), Some(def.id));
        }
        assert_eq!(rows.last().unwrap().id(), None);
    }

    #[test]
    fn cycle_envuelve_y_arranca_en_el_primero_si_no_encuentra() {
        let values = ["a", "b", "c"];
        assert_eq!(cycle("a", &values), "b");
        assert_eq!(cycle("c", &values), "a", "wrap");
        assert_eq!(
            cycle("x", &values),
            "a",
            "no encontrado: arranca en el primero"
        );
        assert_eq!(cycle("a", &[]), "a", "lista vacía: no panica, no cambia");
    }

    // --- `quit_needs_confirm`/`edit_error_message` (revisión S, M6 hoist) ---

    #[test]
    fn quit_needs_confirm_los_tres_modos() {
        use norte_config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "Never: jamás"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "Always: siempre"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "Auto: sigue a pending"
        );
        assert!(!quit_needs_confirm(ConfirmQuit::Auto, false));
    }

    #[test]
    fn edit_error_message_por_categoria_nunca_vacio() {
        assert!(!edit_error_message(&SettingsEditError::NotAnInt).is_empty());
        let msg = edit_error_message(&SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(!msg.is_empty());
        assert!(msg.contains('8') && msg.contains("32"), "{msg}");
    }
}
