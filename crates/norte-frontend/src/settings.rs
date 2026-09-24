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

/// Under which group of the settings screen an entry is painted.
///
/// It was born with two variants — `General` and `Plugins` — and one of the
/// two did not even appear in [`catalog`]: all 33 entries were `General`, so
/// the screen read as a flat list with a label on top. [`Self::ORDER`]'s
/// order is the screen's, and it is deliberate: what gets touched on day one
/// goes on top, what is diagnostic goes at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Theme, fonts and what things look like.
    Appearance,
    /// What a pane shows and what chrome surrounds it.
    Panes,
    /// What program a file opens with.
    OpenWith,
    /// Keyboard and mouse.
    Input,
    /// What norte does without being asked.
    Behavior,
    /// Built from an approved plugin's manifest (S3/S4) — no entries of this
    /// kind live in [`catalog`] itself.
    Plugins,
    /// The locations (config, state, logs, socket). Also does not come from
    /// the catalog: whoever hosts it projects it. It is a MODEL section so
    /// the index lists it like any other and the terminal gets it without
    /// copying the window's projection.
    Paths,
}

impl Section {
    /// The sections in the order they are painted.
    pub const ORDER: &'static [Section] = &[
        Section::Appearance,
        Section::Panes,
        Section::OpenWith,
        Section::Input,
        Section::Behavior,
        Section::Plugins,
        Section::Paths,
    ];

    /// Its STABLE name, untranslated.
    ///
    /// `@section:` accepts it in any language, and it is what travels over
    /// the bridge to the window: a half-finished translation file cannot
    /// make a section unfindable nor break a jump.
    ///
    /// ```
    /// use norte_frontend::settings::Section;
    /// assert_eq!(Section::OpenWith.stable_key(), "open-with");
    /// ```
    #[must_use]
    pub fn stable_key(self) -> &'static str {
        match self {
            Section::Appearance => "appearance",
            Section::Panes => "panes",
            Section::OpenWith => "open-with",
            Section::Input => "input",
            Section::Behavior => "behavior",
            Section::Plugins => "plugins",
            Section::Paths => "paths",
        }
    }

    /// The Fluent key of its label, derived from [`Self::stable_key`]: two
    /// lists of names is a list that goes out of sync.
    ///
    /// ```
    /// use norte_frontend::settings::Section;
    /// assert_eq!(Section::Appearance.label_key(), "settings-section-appearance");
    /// ```
    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Section::Appearance => "settings-section-appearance",
            Section::Panes => "settings-section-panes",
            Section::OpenWith => "settings-section-open-with",
            Section::Input => "settings-section-input",
            Section::Behavior => "settings-section-behavior",
            Section::Plugins => "settings-section-plugins",
            Section::Paths => "settings-section-paths",
        }
    }

    /// The previous/next section in [`Self::ORDER`], without wrapping.
    #[must_use]
    pub fn step(self, delta: i32) -> Option<Section> {
        let pos = Section::ORDER.iter().position(|s| *s == self)?;
        let target = i32::try_from(pos).ok()?.checked_add(delta)?;
        let target = usize::try_from(target).ok()?;
        Section::ORDER.get(target).copied()
    }
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
    /// A COMMAND LINE: typed as text and saved as an ARRAY of tokens
    /// (`zed %f` → `["zed", "%f"]`).
    ///
    /// Exists because `[ui] editor` is not a string in the file: it is an
    /// argv, and saving it as a string would make the next load reject it.
    /// Splitting is on ASCII spaces, the same convention `$EDITOR` uses to
    /// accept `code -w` — the price is a program whose binary has a space in
    /// it, which has to be written into the file by hand.
    Args,
    /// A NUMBER in `[min, max]` — despite the name, the buffer parses as
    /// `f64` and accepts a fractional part (revision S, M4): `ui.font-size`
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
    /// The editing widget.
    pub kind: SettingKind,
    /// Whether a live edit applies without a restart, from the TUI's point
    /// of view (see the struct doc for the GUI's per-entry split).
    pub applies_live: bool,
}

impl SettingDef {
    /// The section it is painted under.
    ///
    /// Comes from [`section_of`] and not from a field per entry: written 33
    /// times next to each `id`, the assignment cannot be read at a glance
    /// nor audited all at once — which is exactly how the 33 entries ended
    /// up saying `General`.
    ///
    /// ```
    /// use norte_frontend::settings::{catalog, Section};
    /// let tema = catalog().iter().find(|d| d.id == "ui.theme").expect("ui.theme");
    /// assert_eq!(tema.section(), Section::Appearance);
    /// ```
    #[must_use]
    pub fn section(&self) -> Section {
        // The invariant is pinned by `every_catalog_entry_has_a_section`: no
        // catalog id falls here. A new id with no section lands in
        // "Behavior" — visible, not hidden — and the test catches it.
        section_of(self.id).unwrap_or(Section::Behavior)
    }
}

/// The settings screen's filter, already interpreted.
///
/// Free text searches where it always does — id, name and description,
/// folded — and on top of that there are two operators, copied from where
/// the reader already knows them: `@modified` (only what is not factory)
/// and `@section:<x>`.
///
/// `@section:` matches against the section's STABLE key and against its
/// label in **both** languages, not just the active one: a translation file
/// cannot be the difference between finding something and not finding it.
///
/// An `@` that does not open a known operator is normal text. Nobody has to
/// escape anything to search for an `@`, and a filter that eats what it
/// does not understand leaves the reader staring at an empty list with no
/// idea why.
#[derive(Debug, Default)]
struct Query {
    /// Free text, already folded. Empty = does not filter by text.
    text: String,
    /// `@modified` was in the query.
    only_modified: bool,
    /// Sections named with `@section:`. Empty = all.
    sections: Vec<Section>,
    /// `@section:` named something that does not exist. It does not filter
    /// to "all": it filters to NOTHING, which is the honest answer to "show
    /// me the settings of something that is not there". Ignoring the
    /// operator would show the whole list and the reader would read that as
    /// "here is everything you asked for".
    imposible: bool,
}

impl Query {
    /// Interprets the raw query (bytes, as typed).
    fn parse(raw: &[u8]) -> Self {
        let mut q = Query::default();
        // Free text is kept AS-IS while there are no operators, and that is
        // not laziness: `ui.font ` with the trailing space isolates a row
        // `ui.font` does not isolate, because the haystack carries the id
        // followed by the name. Splitting and rejoining with one space eats
        // that precision, and a filter that shows two rows where it used to
        // show one is a silent regression.
        if !raw.contains(&b'@') {
            q.text = crate::nav::fold(raw);
            return q;
        }
        // With operators in the mix: their tokens are pulled out and the
        // rest is folded together, already without the edge-space precision
        // — combining `@modified` with a fragment that depends on a
        // trailing space is not a query anyone writes.
        let mut rest: Vec<&[u8]> = Vec::new();
        // By bytes and splitting on ASCII space: the query is raw user
        // input (paste included) and has no reason to be valid UTF-8.
        // `from_utf8_lossy` to look at a token does not write it anywhere.
        for token in raw.split(|b| *b == b' ').filter(|t| !t.is_empty()) {
            let text = String::from_utf8_lossy(token);
            // Folded, like everything else on this screen: `@Modified` and
            // `@MODIFIED` are the same thing, and two operators with two
            // comparison rules is a trap.
            if crate::nav::fold(token) == "@modified" {
                q.only_modified = true;
            } else if let Some(name) = text.strip_prefix("@section:") {
                match section_by_name(name) {
                    Some(s) => q.sections.push(s),
                    None => q.imposible = true,
                }
            } else {
                rest.push(token);
            }
        }
        q.text = crate::nav::fold(rest.join(&b' ').as_slice());
        q
    }

    /// Does this row pass the filter? `fold` is its already-folded haystack.
    fn matches(&self, row: &Row, fold: &str) -> bool {
        if self.imposible {
            return false;
        }
        if self.only_modified && !row.modified {
            return false;
        }
        if !self.sections.is_empty() && !self.sections.contains(&row.section) {
            return false;
        }
        self.text.is_empty() || fold.contains(&self.text)
    }
}

/// The section whose stable name, or whose label in EITHER of the two
/// languages, STARTS WITH `name` (folding accents and case).
///
/// By prefix and not by equality, for two reasons that are really one: the
/// query is split on spaces, so `@section:open with` only brings `open` —
/// and five of the seven sections have a two-word label, i.e. they would be
/// unreachable under equality — and whoever is typing expects to see the
/// effect as they write, not upon typing the last letter.
///
/// Ambiguity: the first one in [`Section::ORDER`] wins, which is the
/// screen's order. No pair of labels shares a prefix today in either
/// language.
fn section_by_name(name: &str) -> Option<Section> {
    let sought = crate::nav::fold(name.as_bytes());
    if sought.is_empty() {
        return None;
    }
    Section::ORDER.iter().copied().find(|s| {
        if crate::nav::fold(s.stable_key().as_bytes()).starts_with(&sought) {
            return true;
        }
        [norte_i18n::Lang::Es, norte_i18n::Lang::En]
            .into_iter()
            .any(|l| {
                crate::nav::fold(norte_i18n::t_in(l, s.label_key()).as_bytes()).starts_with(&sought)
            })
    })
}

/// Which half of the settings screen has the keyboard.
///
/// The same vocabulary as [`crate::help::Focus`], and for the same reason:
/// two lists at once ask which one is in charge, and both screens that do
/// this have to say it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The settings list: up/down walk rows, Enter edits.
    #[default]
    List,
    /// The index: up/down change SECTION, and the list follows.
    Index,
}

/// A section as the index paints it: its already-translated label, how many
/// visible rows it has with the filter on, and where it starts.
///
/// Projected, not stored: the state is the filter, and an index stored
/// alongside it would be a second copy that goes stale the moment someone
/// types a letter.
#[derive(Debug, Clone)]
pub struct SectionView {
    /// Which section it is.
    pub section: Section,
    /// Its label, translated into the active language.
    pub title: String,
    /// How many of its rows are visible with the filter on. Zero with
    /// [`Self::total`] greater than zero = dimmed in the index, never
    /// absent.
    pub visible: usize,
    /// How many rows it has in total, whatever the filter is.
    ///
    /// Distinguishes "the filter hid it" from "this surface does not have
    /// it": the terminal does not project locations, and an index that
    /// announces a section that will never have anything promises something
    /// it will not deliver.
    pub total: usize,
    /// Position of its first visible row within [`SettingsState::visible`]
    /// — the cursor's unit. `None` if the filter left it empty.
    pub first_row: Option<usize>,
}

/// The FACTORY configuration: the one that comes out of zero layers.
///
/// It is against this that whether a row is "modified" is decided, and it
/// is computed with [`current_value`], the same function that paints the
/// value — a hand-written table of defaults goes out of sync with the
/// schema the moment someone changes one.
///
/// Cached because all 33 entries are compared against the same one and
/// [`crate::config::load`] with zero layers does not touch disk (it walks
/// an empty list). If it ever failed, `None` degrades to "nothing is
/// modified": one dot missing is an inert failure, and one extra dot flags
/// as touched something nobody touched.
fn factory_config() -> Option<&'static FrontendConfig> {
    static FACTORY: std::sync::OnceLock<Option<FrontendConfig>> = std::sync::OnceLock::new();
    FACTORY
        .get_or_init(|| crate::config::load(&norte_config::Layers { dirs: vec![] }).ok())
        .as_ref()
}

/// What class of control a setting asks for, and with what values.
///
/// It is what a GRAPHICAL frontend needs to paint a toggle instead of the
/// word `true`: the terminal gets by with [`SettingsState::activate`]'s
/// cycle, but a window has real controls and cannot guess what class each
/// row is by looking at its text.
///
/// The live lists — themes and presets — do NOT come from here: the caller
/// resolves them, as in [`SettingsState::activate`], because they change
/// live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// A toggle.
    Toggle,
    /// A closed list, with its values.
    Choice(&'static [&'static str]),
    /// A list whose values the caller resolves: themes.
    ThemeChoice,
    /// The same with keyboard presets.
    PresetChoice,
    /// A number between two bounds, both included.
    Number {
        /// Lower bound.
        min: i64,
        /// Upper bound.
        max: i64,
    },
    /// Free text.
    Text,
    /// A command line: typed as text and saved split into pieces.
    Args,
}

/// The control setting `id` asks for, or `None` if it is not in the catalog.
///
/// ```
/// use norte_frontend::settings::{control_of, Control};
/// assert_eq!(control_of("ui.mouse"), Some(Control::Toggle));
/// assert!(matches!(control_of("ui.font-size"), Some(Control::Number { .. })));
/// assert_eq!(control_of("ni.idea"), None);
/// ```
#[must_use]
pub fn control_of(id: &str) -> Option<Control> {
    let def = catalog().iter().find(|d| d.id == id)?;
    Some(match def.kind {
        SettingKind::Bool => Control::Toggle,
        SettingKind::Enum(v) => Control::Choice(v),
        SettingKind::ThemeName => Control::ThemeChoice,
        SettingKind::PresetName => Control::PresetChoice,
        SettingKind::Int { min, max } => Control::Number { min, max },
        SettingKind::Text => Control::Text,
        SettingKind::Args => Control::Args,
    })
}

/// The FACTORY value of an entry, as display text.
///
/// It is [`current_value`] over the zero-layer configuration: the same
/// function that paints the value, not a second table of defaults that goes
/// out of sync. Empty if the factory configuration could not be loaded,
/// which is the same thing an empty-valued row shows.
///
/// The window paints it as an empty field's placeholder: "empty" is not a
/// gap, it is this value, and saying which one is informative — saying it
/// with a sentence takes the data's spot without giving it.
///
/// ```
/// use norte_frontend::settings::{catalog, default_value};
/// let tema = catalog().iter().find(|d| d.id == "ui.theme").expect("ui.theme");
/// assert_eq!(default_value(tema), "default");
/// ```
#[must_use]
pub fn default_value(def: &SettingDef) -> String {
    factory_config()
        .map(|f| current_value(def, f))
        .unwrap_or_default()
}

/// The catalog's assignment into sections, in ONE place.
///
/// `None` for an id that is not in the catalog. A catalog id that returns
/// `None` is a bug the coverage test catches: the alternative — a `_ =>`
/// giving it some section or other — files it wrong, silently.
#[must_use]
pub fn section_of(id: &str) -> Option<Section> {
    let s = match id {
        "ui.theme" | "ui.theme-light" | "ui.theme-dark" | "ui.font" | "ui.mono-font"
        | "ui.font-size" | "ui.reduce-motion" | "ui.row-stripes" | "ui.images" | "ui.titlebar" => {
            Section::Appearance
        }
        "ui.show-hidden"
        | "ui.parent-entry"
        | "ui.dir-indicator"
        | "ui.pane-footer"
        | "ui.date-format"
        | "ui.panel-bar"
        | "ui.panel-bar-style"
        | "ui.panel-bar-position"
        | "ui.status-items"
        | "ui.menu-bar"
        | "ui.key-bar"
        | "ui.splash"
        | "ui.processes-panel" => Section::Panes,
        "ui.editor" | "ui.editor-detached" | "ui.diff" | "ui.diff-detached" => Section::OpenWith,
        "keymap.preset" | "ui.mouse" | "ui.alt-menu" | "ui.quick-search" => Section::Input,
        "ui.confirm-quit" | "ui.dialog-buttons" | "ui.notice-seconds" | "ui.history-size"
        | "ui.lang" => Section::Behavior,
        _ => return None,
    };
    Some(s)
}

/// The curated GENERAL settings (v1). Order is DISPLAY order (S3/S4 render
/// top to bottom before a search filter narrows it) — grouped by `norte.toml`
/// section (`[ui]` first, then `[keymap]`), not alphabetically.
const CATALOG: &[SettingDef] = &[
    SettingDef {
        id: "ui.theme",
        kind: SettingKind::ThemeName,
        applies_live: true,
    },
    SettingDef {
        id: "ui.lang",
        kind: SettingKind::Enum(&["es", "en"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.font",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.mono-font",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.font-size",
        kind: SettingKind::Int { min: 8, max: 32 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.quick-search",
        kind: SettingKind::Enum(&["filter", "jump"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.reduce-motion",
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
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // TUI only, like `ui.mouse`. In the catalog because turned off by
        // default nobody would find it, and whoever looks for it just
        // pressed Alt in the terminal and nothing happened.
        id: "ui.alt-menu",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // The pinned menu bar. In the catalog for the same reason as
        // `ui.mouse`: it is the key someone is going to search for as soon
        // as they want that row back, and a setting you only learn about by
        // reading a config file you did not know existed is not
        // discoverable.
        id: "ui.menu-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // The panel bar (#324), and here the argument is the feature's own:
        // it exists because a panel nobody can see is a panel nobody finds.
        // Leaving its toggle only in a config file would be making the same
        // mistake one layer up.
        id: "ui.panel-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // The `..` row. Same criterion as `ui.mouse` and `ui.menu-bar`: it
        // has no command and no key, so the file was the ONLY place it
        // could be turned on or off from.
        id: "ui.parent-entry",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // The startup default for hidden entries. `pane.toggle-hidden`
        // toggles the SESSION and persists nothing, so without this row the
        // value norte opens with could only be changed by writing the file.
        id: "ui.show-hidden",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // `pane.edit`'s editor. Goes here and not only in the file for the
        // same reason as the rest: it is the first thing anyone wants to
        // change, and until now it was chosen via an environment variable,
        // which is the last place anyone looks for a program's
        // configuration.
        id: "ui.editor",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // And whether that editor opens its own window. Without this row,
        // setting a graphical editor leaves the terminal blank with nothing
        // on screen to explain why.
        id: "ui.editor-detached",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // `pane.compare-files`'s comparer (#312), for the same reason as the
        // editor: without a row, whatever compares two files can only be
        // chosen by writing the config file.
        id: "ui.diff",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // And whether that comparer opens its own window (Meld, Kompare).
        id: "ui.diff-detached",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.confirm-quit",
        kind: SettingKind::Enum(&["auto", "always", "never"]),
        applies_live: true,
    },
    // ─── The chrome (spec 2026-09-10): each one exists because a reader
    //     misses it in the first hour, and a toggle that only lives in the
    //     file is a toggle nobody finds.
    SettingDef {
        id: "ui.key-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.panel-bar-style",
        kind: SettingKind::Enum(&["names", "letters", "icons", "nerd"]),
        applies_live: true,
    },
    SettingDef {
        // Top in the terminal and left in the window (`auto`), or the same
        // in both (spec 2026-09-21).
        id: "ui.panel-bar-position",
        kind: SettingKind::Enum(&["auto", "top", "left"]),
        applies_live: true,
    },
    SettingDef {
        // The window's title bar (ADR 0136). At STARTUP: the decoration is
        // removed when the window is created.
        id: "ui.titlebar",
        kind: SettingKind::Enum(&["native", "custom"]),
        applies_live: false,
    },
    SettingDef {
        // The status bar's right half (ADR 0132): ids separated by spaces,
        // in the order they are painted.
        id: "ui.status-items",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        id: "ui.pane-footer",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.row-stripes",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.date-format",
        kind: SettingKind::Enum(&["smart", "relative", "iso"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.notice-seconds",
        kind: SettingKind::Int { min: 0, max: 600 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.history-size",
        kind: SettingKind::Int { min: 5, max: 64 },
        applies_live: true,
    },
    // Spec 2026-09-15, phase 2: the splash screen, the processes panel that
    // opens on its own, and the `/` on directories.
    SettingDef {
        id: "ui.splash",
        kind: SettingKind::Enum(&["brief", "off", "home"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.processes-panel",
        kind: SettingKind::Enum(&["auto", "manual"]),
        applies_live: true,
    },
    // Phase 5, task 2: how the TUI's viewer paints an image. A TERMINAL
    // key — the window paints images through its own webview and does not
    // read it.
    SettingDef {
        id: "ui.images",
        kind: SettingKind::Enum(&["auto", "kitty", "blocks", "off"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dir-indicator",
        kind: SettingKind::Enum(&["auto", "slash", "none"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dialog-buttons",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    // ─── The theme per desktop scheme (spec 2026-09-11, V6): only the
    //     window reads it, but the file is one and the settings screen is
    //     the same in both frontends.
    SettingDef {
        id: "ui.theme-light",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.theme-dark",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "keymap.preset",
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
        .expect("a catalog() id always has section.key");
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
        // Absent = PINNED, same as `ui.mouse`: the row shows what the
        // frontend really does. It was missing, and the consequence was not
        // cosmetic — with the empty cell, toggling read "is not true" and
        // always wrote `true`, so the bar could not be turned off from here.
        "ui.menu-bar" => cfg.common.ui_menu_bar.unwrap_or(true).to_string(),
        "ui.panel-bar" => cfg.common.ui_panel_bar.unwrap_or(true).to_string(),
        "ui.parent-entry" => cfg.common.ui_parent_entry.unwrap_or(true).to_string(),
        "ui.show-hidden" => cfg.common.ui_show_hidden.unwrap_or(false).to_string(),
        "ui.editor" => cfg.common.ui_editor.clone().unwrap_or_default().join(" "),
        "ui.editor-detached" => cfg.common.ui_editor_detached.unwrap_or(false).to_string(),
        // Absent = `diff -u`, and the row shows it: it is what norte
        // really does, not an empty cell over a behavior that exists.
        "ui.diff" => cfg
            .common
            .ui_diff
            .clone()
            .unwrap_or_else(|| vec!["diff".to_owned(), "-u".to_owned(), "%F".to_owned()])
            .join(" "),
        "ui.diff-detached" => cfg.common.ui_diff_detached.unwrap_or(false).to_string(),
        "ui.confirm-quit" => cfg.common.ui_confirm_quit.as_str().to_owned(),
        // Absent = what the frontend really does, like `ui.menu-bar`.
        "ui.key-bar" => cfg.common.ui_chrome.key_bar().to_string(),
        "ui.panel-bar-style" => cfg.common.ui_chrome.panel_bar_style().as_str().to_owned(),
        "ui.panel-bar-position" => cfg
            .common
            .ui_chrome
            .panel_bar_position()
            .as_str()
            .to_owned(),
        "ui.titlebar" => cfg.common.ui_chrome.titlebar().as_str().to_owned(),
        "ui.status-items" => cfg.common.ui_chrome.status_items().to_ids().join(" "),
        "ui.pane-footer" => cfg.common.ui_chrome.pane_footer().to_string(),
        "ui.row-stripes" => cfg.common.ui_chrome.row_stripes().to_string(),
        "ui.date-format" => cfg.common.ui_chrome.date_format().as_str().to_owned(),
        "ui.notice-seconds" => cfg.common.ui_chrome.notice_seconds().to_string(),
        "ui.history-size" => cfg.common.ui_chrome.history_size().to_string(),
        "ui.splash" => cfg.common.ui_chrome.splash().as_str().to_owned(),
        "ui.processes-panel" => cfg.common.ui_chrome.processes_panel().as_str().to_owned(),
        "ui.images" => cfg.common.ui_chrome.images().as_str().to_owned(),
        "ui.dir-indicator" => cfg.common.ui_chrome.dir_indicator().as_str().to_owned(),
        "ui.dialog-buttons" => cfg.common.ui_chrome.dialog_buttons().to_string(),
        // Empty = no variant: the window paints `theme` in both schemes.
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
    /// The section it is painted under: the catalog's for a curated entry,
    /// [`Section::Plugins`] for a plugin summary.
    ///
    /// It goes in the row and is not re-derived from the id in each
    /// frontend: two counts of "where does this go" are two screens that
    /// go out of order separately.
    pub section: Section,
    /// The effective value is NOT the factory one.
    ///
    /// "Is not the factory one", not "you touched it": a key only the
    /// system layer sets turns the dot on without the reader having done
    /// anything, and one hand-written with the value it already had does
    /// not turn it on. The screen's label says the first thing, which is
    /// what this measures.
    ///
    /// Always false for a row that does not come from the catalog: there is
    /// no factory value to compare it against.
    pub modified: bool,
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
            section: Section::Plugins,
            modified: false,
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
            section: Section::Plugins,
            modified: false,
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

/// [`build_rows`] in a GIVEN language.
///
/// The settings screen used to translate section titles with the HOST's
/// language and each option's name and description with the PROCESS's, so
/// it came out half in one language and half in another.
#[must_use]
pub fn build_rows_in(
    cfg: &FrontendConfig,
    plugin_summaries: &[PluginConfigSummary],
    lang: norte_i18n::Lang,
) -> Vec<Row> {
    let factory = factory_config();
    let mut rows: Vec<Row> = catalog()
        .iter()
        .enumerate()
        .map(|(i, def)| {
            let value = current_value(def, cfg);
            Row {
                def_index: Some(i),
                plugin_id: None,
                name: norte_i18n::t_in(lang, &fluent_name_id(def.id)),
                desc: norte_i18n::t_in(lang, &fluent_desc_id(def.id)),
                modified: factory.is_some_and(|f| value != current_value(def, f)),
                value,
                section: def.section(),
            }
        })
        .collect();
    // In SCREEN order: by section first, and the catalog's order within
    // each one. The catalog is grouped by `norte.toml` section (`[ui]` then
    // `[keymap]`), which is not the same assignment: `ui.lang` is Behavior
    // and `ui.quick-search` is Input, and they are two rows apart. Without
    // sorting here, a list with headers would paint the same section seven
    // times.
    //
    // `sort_by_key` is STABLE, which is what keeps the catalog's order
    // within each section without writing a second criterion.
    rows.sort_by_key(|r| {
        Section::ORDER
            .iter()
            .position(|s| *s == r.section)
            .unwrap_or(usize::MAX)
    });
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

/// A key that has to be REMOVED from the write layer, produced by
/// [`SettingsState::reset`].
///
/// Like [`PendingWrite`], it is pure: whoever receives it calls
/// `norte_config::persist_unset` off the paint thread (rule 2) and
/// announces the result. It carries no value because there is none to
/// write — resetting is ceasing to say anything, not saying the default:
/// writing the factory value into the file would freeze it against a future
/// change of the default, which is exactly the opposite of what the reader
/// asked for.
#[derive(Debug, Clone)]
pub struct PendingReset {
    /// `[section]` of `norte.toml`.
    pub section: &'static str,
    /// The key within that section (`snake_case`, already converted).
    pub key: String,
    /// The catalog id (`ui.theme`), to find the row again after rereading.
    ///
    /// It travels because the trip back — from `(section, key)` to the id —
    /// is NOT a bijection: a future id with `_` would come back with `-` and
    /// would not match any row, and whoever looks it up would answer "back
    /// to the factory value" for everything, which is the wrong, silent
    /// answer.
    pub id: &'static str,
    /// The setting's translated name, for the notice.
    pub name: String,
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
    /// The value does not fit what the setting admits; `key` is the Fluent
    /// id of the message that says what does (ADR 0132: an unknown or
    /// repeated `ui.status-items` id is refused HERE, before it reaches
    /// `norte.toml` and breaks the next load).
    Invalid {
        /// The Fluent id of the message.
        key: &'static str,
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
    /// The first visible LINE of a list that does not fit, in whoever paints
    /// it's own unit ([`Self::reconcile_viewport`]).
    ///
    /// It did not exist because settings fit on one screen — the terminal's
    /// shortcut editor said so, and it was true when written. With ~30
    /// settings it stopped being true: going down with the cursor past the
    /// edge left it outside the box and the list did not move.
    viewport_offset: usize,
    /// Which half has the keyboard.
    ///
    /// There is no separate index cursor: with focus on it, moving CHANGES
    /// section and the list follows, just like the help sidebar opens the
    /// topic as you walk through it. A second cursor that had to be
    /// synchronized with the first is the kind of state that goes out of
    /// sync.
    focus: Focus,
}

impl SettingsState {
    /// Leaves the window ready to paint `rows` lines with the cursor in
    /// view: it drags it ONLY if the cursor went outside, by
    /// [`crate::viewport::sticky_offset`]'s shared rule. Called once per
    /// frame, before painting.
    ///
    /// Receives the cursor's line and the total ALREADY in screen lines, not
    /// rows, because whoever paints interleaves section headers between the
    /// rows: that count is theirs, and doing it here would be a second copy
    /// of how it is painted. The window, being web, does not even call it —
    /// the browser already scrolls the chosen row into view.
    ///
    /// `anchor_line` is the first line that has to be seen WITH the cursor:
    /// its section's header when the cursor is on the section's first row,
    /// and `cursor_line` in any other case. It exists because anchoring only
    /// to the cursor hides the header forever: the first row lives on line
    /// 1 — 0 is "General" — so scrolling all the way up left the offset at 1
    /// and the header never came back. The cursor rules the BOTTOM edge (a
    /// header cannot push it out of the box) and the anchor only pulls
    /// UPWARD.
    pub fn reconcile_viewport(
        &mut self,
        cursor_line: usize,
        anchor_line: usize,
        total_lines: usize,
        rows: usize,
    ) {
        let off =
            crate::viewport::sticky_offset(self.viewport_offset, cursor_line, total_lines, rows);
        self.viewport_offset = off.min(anchor_line.min(cursor_line));
    }

    /// The first visible line — see [`Self::reconcile_viewport`].
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
            focus: Focus::List,
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
        let filter = Query::parse(&self.query);
        self.visible = self
            .folds
            .iter()
            .enumerate()
            .filter(|(i, f)| filter.matches(&self.rows[*i], f))
            .map(|(i, _)| i)
            .collect();
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

    /// Sets the WHOLE query at once and recomputes.
    ///
    /// The window needs it: its search box is a browser `<input>` and what
    /// crosses the bridge is the full text, not the keystroke. The terminal
    /// stays with [`Self::push_char`] because its overlay does receive
    /// keystrokes.
    ///
    /// The text arrives as bytes from a text box: it is neither validated
    /// nor trimmed here — folding and filtering is all that is done with
    /// it, and painting it is up to whoever paints ([`Self::query_display`]
    /// masks it).
    pub fn set_query(&mut self, text: &str) {
        if self.edit.is_some() {
            return;
        }
        self.query = text.as_bytes().to_vec();
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
        if self.edit.is_some() {
            return;
        }
        // With focus on the index, SECTIONS are walked, not rows, and the
        // list follows: it is what the help sidebar does, opening the topic
        // as it passes over it.
        if self.focus == Focus::Index {
            self.step_section(-1);
            return;
        }
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the selection down (clamped at the end). No-op editing.
    pub fn down(&mut self) {
        if self.edit.is_some() {
            return;
        }
        if self.focus == Focus::Index {
            self.step_section(1);
            return;
        }
        if self.cursor + 1 < self.visible.len() {
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

    /// The section index the screen paints: ALL of them, in
    /// [`Section::ORDER`]'s order, with how many visible rows each has and
    /// where it starts.
    ///
    /// A section the filter leaves at zero **stays in the list**, dimmed: an
    /// index that changes length while you type is an index that cannot be
    /// used as a map.
    ///
    /// `first_row` is a position within [`Self::visible`] — the same unit as
    /// [`Self::cursor`] — and NOT an index within [`Self::rows`]. Mixing the
    /// two units is a cursor that points at another row.
    #[must_use]
    pub fn sections(&self) -> Vec<SectionView> {
        Section::ORDER
            .iter()
            .map(|s| {
                let mut visible = 0;
                let mut first_row = None;
                for (pos, &real) in self.visible.iter().enumerate() {
                    if self.rows[real].section == *s {
                        visible += 1;
                        if first_row.is_none() {
                            first_row = Some(pos);
                        }
                    }
                }
                SectionView {
                    section: *s,
                    title: t(s.label_key()),
                    visible,
                    total: self.rows.iter().filter(|r| r.section == *s).count(),
                    first_row,
                }
            })
            .collect()
    }

    /// Which half has the keyboard.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Switches sides. No-op while editing: an open edit freezes everything
    /// else, like the rest of this machine.
    ///
    /// The index cannot lead somewhere that does not exist, so switching to
    /// it with an empty list makes no sense either: with no visible rows
    /// there is no section to go to, and focus stays where it is.
    pub fn toggle_focus(&mut self) {
        if self.edit.is_some() {
            return;
        }
        self.focus = match self.focus {
            Focus::Index => Focus::List,
            Focus::List if self.visible.is_empty() => Focus::List,
            Focus::List => Focus::Index,
        };
    }

    /// Moves the cursor to the previous section (negative `delta`) or the
    /// next, SKIPPING the ones the filter left empty. Returns which one it
    /// went to, or `None` if there was none with rows on that side.
    ///
    /// Lives here and not in each frontend because both screens have to
    /// move the same way: with the walk written twice, the eighth section —
    /// or a reordering — separates them silently and only one has a test.
    /// Skipping the empty ones is what makes the key work with a filter on:
    /// stopping on one would force pressing twice with nothing happening.
    pub fn step_section(&mut self, delta: i32) -> Option<Section> {
        let &real = self.visible.get(self.cursor)?;
        let mut current = self.rows[real].section;
        let index = self.sections();
        while let Some(next) = current.step(delta) {
            if index.iter().any(|v| v.section == next && v.visible > 0) {
                self.jump_to(next);
                return Some(next);
            }
            current = next;
        }
        None
    }

    /// Moves the cursor to the first visible row of `section`.
    ///
    /// A section with no visible rows moves nothing: a jump that lands on
    /// another section's row is worse than a jump that does not happen.
    pub fn jump_to(&mut self, section: Section) {
        let target = self
            .visible
            .iter()
            .position(|&real| self.rows[real].section == section);
        if let Some(pos) = target {
            self.set_cursor(pos);
        }
    }

    /// Selection position WITHIN [`Self::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How many rows there are in total, whatever the filter is.
    #[must_use]
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// How many are visible with the filter on.
    ///
    /// Goes with [`Self::total`] on screen ("7 of 33") because without the
    /// second figure "there is nothing" and "I hid it with a letter" read
    /// the same.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.visible.len()
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

    /// Sets a SPECIFIC value on row `id` — what a window control needs (a
    /// toggle, a dropdown, a numeric field).
    ///
    /// Exists because [`Self::activate`] **cycles**: with a dropdown of ten
    /// themes, choosing the seventh would be seven round trips and six
    /// writes to `norte.toml`. Here the control says which value to go to
    /// and it is written once.
    ///
    /// The validation is the SAME as the keyboard's: an integer goes through
    /// the catalog's range, a command line is split the same way, and a
    /// value not in an enum's list is rejected. None of this lives in the
    /// renderer — a frontend validating on its own would be a second rule
    /// splitting off from the first.
    ///
    /// `theme_names`/`preset_names` arrive LIVE, as in [`Self::activate`].
    ///
    /// # Errors
    /// [`SettingsEditError`] under the same criterion as
    /// [`Self::edit_commit`]; a row that does not exist, that is not from
    /// the catalog, or a value outside its enum's list are rejected like an
    /// invalid integer — the inert failure the editor already uses for
    /// "this cannot be written".
    pub fn set_value(
        &mut self,
        id: &str,
        value: &str,
        theme_names: &[String],
        preset_names: &[&str],
    ) -> Result<PendingWrite, SettingsEditError> {
        if self.edit.is_some() {
            return Err(SettingsEditError::NotAnInt);
        }
        let Some(pos) = self
            .visible
            .iter()
            .position(|&i| self.rows[i].id() == Some(id))
        else {
            return Err(SettingsEditError::NotAnInt);
        };
        let real = self.visible[pos];
        let Some(idx) = self.rows[real].def_index else {
            return Err(SettingsEditError::NotAnInt);
        };
        let def = &catalog()[idx];
        match def.kind {
            SettingKind::Bool => {
                let b = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(SettingsEditError::NotAnInt),
                };
                Ok(self.commit_row(real, def, b.to_string(), toml_edit::Value::from(b)))
            }
            SettingKind::Enum(values) => {
                if !values.contains(&value) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(value);
                Ok(self.commit_row(real, def, value.to_owned(), v))
            }
            SettingKind::ThemeName => {
                if !theme_names.iter().any(|t| t == value) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(value);
                Ok(self.commit_row(real, def, value.to_owned(), v))
            }
            SettingKind::PresetName => {
                if !preset_names.contains(&value) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(value);
                Ok(self.commit_row(real, def, value.to_owned(), v))
            }
            // The ones the line editor already knows how to validate: the
            // whole text is passed to it through its own path, instead of
            // copying a command line's splitting or an integer's range.
            SettingKind::Int { .. } | SettingKind::Text | SettingKind::Args => {
                let before = self.cursor;
                self.cursor = pos;
                self.edit = Some(value.to_owned());
                let out = self.edit_commit();
                self.edit = None;
                if out.is_err() {
                    self.cursor = before;
                }
                out
            }
        }
    }

    /// Resets the cursor's row: the key that has to be REMOVED from the
    /// write layer, or `None` if there is nothing to remove.
    ///
    /// `None` when the row is already at its factory value (removing a
    /// key that is not there is a no-op that does not deserve a notice),
    /// when it is not from the catalog (a plugin summary has no factory
    /// value), or while editing — same as the rest of this machine, editing
    /// freezes everything else.
    ///
    /// **Removing the key from YOUR layer does not always return the
    /// factory value**: if the system, the profile or the project set the
    /// same one, the value changes and is still not the default. This
    /// function does not know that; the caller rebuilds the rows afterward
    /// — it already does so after every write — and looks at the dot: if
    /// the row is still `modified`, it says so with
    /// `settings-still-set-elsewhere`, and if not, with
    /// `settings-reset-done`. The lit dot is true with no provenance
    /// machinery.
    pub fn reset(&mut self) -> Option<PendingReset> {
        if self.edit.is_some() {
            return None;
        }
        let &real = self.visible.get(self.cursor)?;
        let row = &self.rows[real];
        if !row.modified {
            return None;
        }
        let id = row.id()?;
        let (section, key) = wire_key(id);
        Some(PendingReset {
            section,
            key,
            id,
            name: row.name.clone(),
        })
    }

    /// Confirms the inline edit buffer: `Int` parses the buffer as `f64`
    /// (revision S, M4 — see [`SettingKind::Int`]'s doc for why a "whole
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
            #[expect(clippy::cast_precision_loss, reason = "magnitudes far from 2^53")]
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
            // A command line is SAVED as an array: `zed %f` travels as
            // `["zed", "%f"]`, which is what the file declares. Writing it
            // as a string would make the next load reject it.
            //
            // Empty = an empty array, which the configuration reads as
            // "none" and hands control back to `$VISUAL`/`$EDITOR`.
            // A list with closed vocabulary is validated HERE: written
            // wrong, the next load would reject the whole file.
            if def.id == "ui.status-items" {
                let ids: Vec<&str> = buf.split_ascii_whitespace().collect();
                if norte_config::StatusItems::parse(&ids).is_err() {
                    return Err(SettingsEditError::Invalid {
                        key: "msg-settings-invalid-status-items",
                    });
                }
            }
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
/// frontends before this hoist (revision S, M6: TUI's `quit_needs_confirm`
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
/// (S3) and the GUI view (S4, revision S M6): both had their own
/// byte-identical copy of this match before this hoist.
#[must_use]
pub fn edit_error_message(e: &SettingsEditError) -> String {
    match e {
        SettingsEditError::NotAnInt => t("msg-settings-invalid-int"),
        SettingsEditError::OutOfRange { min, max } => norte_i18n::ta(
            "msg-settings-invalid-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
        SettingsEditError::Invalid { key } => t(key),
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
    fn catalog_ids_are_unique() {
        let ids: Vec<&str> = catalog().iter().map(|d| d.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "duplicate id in catalog(): {ids:?}"
        );
    }

    /// `fluent_name_id`/`fluent_desc_id` dash the id's dots — pinned with a
    /// concrete example so a refactor can't silently change the derivation.
    #[test]
    fn fluent_ids_dash_the_dots() {
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
    fn fluent_keys_exist_in_both_locales_for_every_entry() {
        for def in catalog() {
            for lang in [Lang::Es, Lang::En] {
                let name_id = fluent_name_id(def.id);
                let desc_id = fluent_desc_id(def.id);
                assert_ne!(
                    t_in(lang, &name_id),
                    name_id,
                    "missing Fluent key {name_id} in {lang:?} (id={})",
                    def.id
                );
                assert_ne!(
                    t_in(lang, &desc_id),
                    desc_id,
                    "missing Fluent key {desc_id} in {lang:?} (id={})",
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
    fn current_value_resolves_for_every_entry_without_panicking() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("empty config loads");
        for def in catalog() {
            let value = current_value(def, &cfg);
            assert_ne!(
                value, def.id,
                "current_value should not return the id as a fallback: {}",
                def.id
            );
        }
    }

    /// A row that TOGGLES has to show a readable value, not the empty
    /// string.
    ///
    /// The test above was not enough — an empty cell is not the id, so it
    /// passed — and the hole was not cosmetic: `activate` decides the next
    /// value by reading the one PAINTED, so with the empty cell a `Bool`
    /// read "is not true" and always wrote `true`. `ui.menu-bar` was like
    /// this: in the catalog, with no arm in `current_value`, and therefore
    /// impossible to turn off from this screen.
    #[test]
    fn a_toggled_row_never_shows_an_empty_cell() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("empty config loads");
        for def in catalog() {
            let value = current_value(def, &cfg);
            match def.kind {
                SettingKind::Bool => assert!(
                    value == "true" || value == "false",
                    "{} paints {value:?}, which is not a boolean",
                    def.id
                ),
                // `Enum`s are knowingly left OUT: `ui.lang` with no value
                // paints `auto`, which is not one of its own — it is what
                // norte does, negotiate with the environment — and the
                // first press falls on the first of the list all the same.
                // What is protected here is the case where the painted
                // value DECIDES the next one and an empty cell decides it
                // wrong.
                //
                // Free-text ones CAN be empty: "no font chosen" and "no
                // editor chosen" are valid answers.
                SettingKind::Enum(_)
                | SettingKind::Text
                | SettingKind::Args
                | SettingKind::Int { .. }
                | SettingKind::ThemeName
                | SettingKind::PresetName => {}
            }
        }
    }

    /// Typing a command line saves an ARRAY, which is what the file
    /// declares: a string would make the next load reject it.
    #[test]
    fn a_command_line_row_is_saved_as_an_array() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("empty config loads");
        let mut st = SettingsState::new(build_rows(&cfg, &[]));
        let row = st
            .rows()
            .iter()
            .position(|r| r.def_index.map(|i| catalog()[i].id) == Some("ui.editor"))
            .expect("ui.editor is in the catalog");
        st.set_cursor(row);
        assert!(
            st.activate(&[], &[]).is_none(),
            "a command line is edited, not toggled"
        );
        for c in "zed %f".chars() {
            st.edit_push_char(c);
        }
        let write = st.edit_commit().expect("free text does not fail");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "editor");
        assert_eq!(write.value.to_string().trim(), r#"["zed", "%f"]"#);
        assert_eq!(write.display, "zed %f");
    }

    /// `ui.confirm-quit`'s default value round-trips through `current_value`
    /// as the same wire string `[ui] confirm_quit` accepts in `norte.toml`
    /// (S2's exemplar setting — this is the one already wired end-to-end).
    #[test]
    fn current_value_confirm_quit_default_is_auto() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("empty config loads");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit is in the catalog");
        assert_eq!(current_value(def, &cfg), "auto");
    }

    /// `wire_key` splits on the FIRST `.` and dashes-to-underscores the rest
    /// — pinned with concrete examples (mirrors `fluent_ids_dash_the_dots`
    /// above, same derivation family, different target vocabulary).
    #[test]
    fn wire_key_derives_section_and_snake_case_key() {
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
    fn wire_key_resolves_for_every_catalog_entry() {
        for def in catalog() {
            let (section, key) = wire_key(def.id);
            assert!(!section.is_empty());
            assert!(!key.is_empty());
        }
    }

    /// `ui.confirm-quit` reflects a NON-default value loaded from
    /// `norte.toml` — not just the default path above.
    #[test]
    fn current_value_confirm_quit_reflects_loaded_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nconfirm_quit = \"always\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = crate::config::load(&layers).expect("loads");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit is in the catalog");
        assert_eq!(current_value(def, &cfg), "always");
    }

    // --- `build_rows` (S3/S4 hoist) ---

    /// One row per catalog entry, plus EXACTLY one informational row at the
    /// end when NO plugin declares any `[config]` key (G3c fallback shape).
    #[test]
    fn build_rows_one_row_per_entry_plus_the_plugins_one() {
        let rows = build_rows(&empty_cfg(), &[]);
        assert_eq!(rows.len(), catalog().len() + 1);
        assert!(!rows[0].is_plugins_note());
        assert!(rows.last().unwrap().is_plugins_note());
        assert_eq!(rows.last().unwrap().plugin_id(), None);
    }

    fn empty_cfg() -> FrontendConfig {
        crate::config::load(&Layers { dirs: vec![] }).expect("empty config loads")
    }

    /// Each General row's value is EXACTLY what `current_value` (S2) would
    /// resolve for the same `def` — never a diverging copy.
    #[test]
    fn build_rows_values_match_current_value() {
        let cfg = empty_cfg();
        let rows = build_rows(&cfg, &[]);
        // By ID, not by position: rows come out in SCREEN order (section
        // first) and the catalog is grouped by `norte.toml` section, which
        // is a different order.
        for def in catalog() {
            let row = rows
                .iter()
                .find(|r| r.id() == Some(def.id))
                .unwrap_or_else(|| panic!("\"{}\" is not in the rows", def.id));
            assert_eq!(row.value, current_value(def, &cfg));
        }
        // The whole catalog, plus the Plugins informational row.
        assert_eq!(rows.len(), catalog().len() + 1);
    }

    /// The Plugins informational row carries no value (nothing to edit) and
    /// its name/description resolve to REAL text (not the raw Fluent id) in
    /// this suite's active language.
    #[test]
    fn plugins_note_row_has_no_value_and_translated_text() {
        let rows = build_rows(&empty_cfg(), &[]);
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
    fn build_rows_with_plugins_one_row_per_summary() {
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
        let rows = build_rows(&empty_cfg(), &summaries);
        assert_eq!(rows.len(), catalog().len() + 2);
        let a = &rows[catalog().len()];
        assert_eq!(a.plugin_id(), Some("org.a"));
        assert_eq!(a.name, "Alpha");
        assert!(
            a.is_plugins_note(),
            "not editable via SettingsState::activate"
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
    fn build_rows_with_plugins_does_not_alter_an_already_masked_name() {
        let masked = crate::display_name("\u{202E}evil".as_bytes()).0;
        let summaries = vec![PluginConfigSummary {
            plugin_id: "org.evil".into(),
            name: masked.clone(),
            key_count: 1,
        }];
        let rows = build_rows(&empty_cfg(), &summaries);
        assert_eq!(rows.last().unwrap().name, masked);
    }

    // --- `SettingsState`/`PendingWrite`/`SettingsEditError` (S3/S4 hoist) ---

    fn rows() -> Vec<Row> {
        build_rows(&empty_cfg(), &[])
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
            "fragment {fragment:?} should isolate a single row"
        );
        s
    }

    #[test]
    fn settings_filters_by_id_name_or_description() {
        let s = only("confirm-quit");
        assert_eq!(
            s.rows()[s.visible()[0]].name,
            t("setting-ui-confirm-quit-name")
        );
    }

    #[test]
    fn settings_hostile_query_is_masked() {
        let mut s = SettingsState::new(rows());
        for c in "a\u{202E}b".chars() {
            s.push_char(c);
        }
        let display = s.query_display();
        assert!(!display.chars().any(norte_encoding::is_terminal_hazard));
        assert!(display.contains('\u{FFFD}'));
    }

    #[test]
    fn settings_with_no_matches_does_not_panic_and_activate_is_none() {
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
    fn activate_on_bool_toggles_and_returns_pendingwrite() {
        let mut s = only("reduce-motion");
        assert_eq!(s.rows()[s.visible()[0]].value, "false", "default");
        let write = s.activate(&[], &[]).expect("Bool activates immediately");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "reduce_motion");
        assert_eq!(write.value.as_bool(), Some(true));
        assert_eq!(write.display, "true");
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "optimistic");
        assert!(!s.is_editing());
    }

    #[test]
    fn activate_on_enum_cycles_with_wrap() {
        let mut s = only("confirm-quit");
        assert_eq!(s.rows()[s.visible()[0]].value, "auto", "default S2");
        let w1 = s.activate(&[], &[]).unwrap();
        assert_eq!(w1.display, "always");
        let w2 = s.activate(&[], &[]).unwrap();
        assert_eq!(w2.display, "never");
        let w3 = s.activate(&[], &[]).unwrap();
        assert_eq!(w3.display, "auto", "wraps to the first");
        assert_eq!(w3.value.as_str(), Some("auto"));
    }

    #[test]
    fn activate_on_theme_name_cycles_over_the_live_list() {
        // "ui.theme" is a prefix of `ui.theme-light` and `ui.theme-dark`
        // (spec 2026-09-11, V6): the filter leaves THREE rows, and the
        // cursor lands on the first, which by catalog order is the plain
        // theme's.
        let mut s = SettingsState::new(rows());
        for c in "ui.theme".chars() {
            s.push_char(c);
        }
        assert_eq!(s.visible().len(), 3, "theme, theme-light and theme-dark");
        assert_eq!(s.rows()[s.visible()[0]].name, t("setting-ui-theme-name"));
        let names = vec!["default".to_owned(), "nord".to_owned()];
        // The current value (S2's default) is "default": the next is "nord".
        let write = s.activate(&names, &[]).expect("ThemeName activates");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "theme");
        assert_eq!(write.value.as_str(), Some("nord"));
    }

    #[test]
    fn activate_on_preset_name_cycles_over_the_live_list() {
        let mut s = only("keymap.preset");
        let presets = ["orthodox", "vim", "cua"];
        let write = s.activate(&[], &presets).expect("PresetName activates");
        assert_eq!(write.section, "keymap");
        assert_eq!(write.key, "preset");
        assert_eq!(write.value.as_str(), Some("vim"), "orthodox → vim (wrap)");
    }

    #[test]
    fn activate_on_text_opens_editing_without_persisting() {
        // Trailing space: `ui.font` is a PREFIX of `ui.font-size` (the fold
        // pastes `"{id} {name} {desc}"`, so the space following the id
        // anchors the end of the token and discards that other id
        // unambiguously).
        let mut s = only("ui.font ");
        assert!(!s.is_editing());
        let write = s.activate(&[], &[]);
        assert!(write.is_none(), "Text does not persist on open: only edits");
        assert!(s.is_editing());
        assert_eq!(s.edit_buffer(), Some(""));
    }

    #[test]
    fn edit_commit_on_text_persists_what_was_typed() {
        let mut s = only("mono-font");
        s.activate(&[], &[]);
        for c in "JetBrains Mono".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("Text is always valid");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "mono_font");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, "JetBrains Mono");
    }

    /// The window does not type character by character: its field is native
    /// and hands over the whole text on confirm. `edit_set` is that input,
    /// and outside an edit it does nothing.
    #[test]
    fn edit_set_replaces_the_whole_buffer_and_only_while_editing() {
        let mut s = only("mono-font");
        s.edit_set("nothing");
        assert!(!s.is_editing(), "with no edit open it does not open one");
        s.activate(&[], &[]);
        s.edit_set("JetBrains Mono");
        assert_eq!(s.edit_buffer(), Some("JetBrains Mono"));
        let write = s.edit_commit().expect("Text is always valid");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
    }

    #[test]
    fn edit_commit_on_int_validates_range_without_persisting_and_keeps_the_buffer() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "999".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("999 is out of [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(s.is_editing(), "the buffer is kept after a rejection");
        assert_eq!(s.edit_buffer(), Some("999"));
    }

    #[test]
    fn edit_commit_on_int_non_numeric_rejects() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "abc".chars() {
            s.edit_push_char(c);
        }
        assert_eq!(s.edit_commit().unwrap_err(), SettingsEditError::NotAnInt);
    }

    #[test]
    fn edit_commit_on_int_valid_persists() {
        let mut s = only("font-size");
        // The buffer starts with the CURRENT value ("" — with no
        // `[ui] font_size` in this test's empty config, `current_value`
        // already documents this).
        s.activate(&[], &[]);
        assert_eq!(s.edit_buffer(), Some(""));
        for c in "16".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("16 is in [8,32]");
        assert_eq!(write.value.as_integer(), Some(16));
        assert_eq!(write.display, "16");
    }

    /// Revision S, M4: `ui.font-size` accepts a FRACTIONAL value (`[ui]
    /// font_size` is `f32` in `norte_config`, not an integer — a hand-edited
    /// `norte.toml` with `font_size = 14.5` was impossible to re-edit from
    /// here before this fix, the strict `i64::parse` rejected it outright).
    /// Round trip: "14.5" → `Value::Float(14.5)` + `display` with NO extra
    /// zeros.
    #[test]
    fn edit_commit_on_font_size_accepts_a_fraction_and_round_trips() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "14.5".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("14.5 is in [8,32]");
        assert_eq!(write.value.as_float(), Some(14.5));
        assert_eq!(
            write.value.as_integer(),
            None,
            "must not be written as an integer"
        );
        assert_eq!(write.display, "14.5");
    }

    /// A fractional value OUT of range (e.g. `33.5`) is still rejected — the
    /// more permissive parse (`f64` instead of `i64`) does not weaken the
    /// `[min, max]` validation.
    #[test]
    fn edit_commit_on_font_size_fraction_out_of_range_rejects() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "33.5".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("33.5 is out of [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
    }

    #[test]
    fn edit_cancel_does_not_persist_and_keeps_the_original_value() {
        let mut s = only("mono-font");
        let original = s.rows()[s.visible()[0]].value.clone();
        s.activate(&[], &[]);
        s.edit_push_char('x');
        s.edit_cancel();
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, original);
    }

    /// The Plugins informational row (last one with an empty query) never
    /// opens editing nor produces a `PendingWrite`.
    #[test]
    fn activate_on_the_plugins_informational_row_is_a_no_op() {
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
    fn refresh_keeps_the_query_and_recomputes_values() {
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
        let cfg = crate::config::load(&layers).expect("loads");
        s.refresh(build_rows(&cfg, &[]));
        assert_eq!(
            s.visible().len(),
            1,
            "the 'reduce-motion' query is kept after the refresh"
        );
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "fresh value");
    }

    /// The anchor pulls UPWARD and the cursor rules below.
    ///
    /// With the window scrolled down, going back to the first row (line 1,
    /// because 0 is the "General" header) left the offset at 1: the header
    /// never came back. The anchor is that header's line.
    #[test]
    fn the_sections_header_comes_in_with_its_first_row() {
        let mut s = SettingsState::new(rows());
        // Ten lines of box over forty; the window has already scrolled down.
        s.reconcile_viewport(39, 39, 40, 10);
        assert_eq!(s.viewport_offset(), 30);
        // Going back to the first row: its header is line 0.
        s.reconcile_viewport(1, 0, 40, 10);
        assert_eq!(s.viewport_offset(), 0, "the header comes back with its row");
        // A row that does NOT open a section pulls nothing: anchor = cursor.
        s.reconcile_viewport(25, 25, 40, 10);
        assert_eq!(s.viewport_offset(), 16);
        // And a header cannot push the cursor out at the bottom.
        s.reconcile_viewport(39, 38, 40, 10);
        assert!(s.viewport_offset() <= 38 && s.viewport_offset() + 10 > 39);
    }

    #[test]
    fn resetting_a_touched_row_asks_to_remove_its_key() {
        let mut cfg = empty_cfg();
        cfg.common.ui_theme = Some("nord".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.theme"))
            .expect("ui.theme visible");
        s.set_cursor(pos);
        let r = s.reset().expect("there is something to remove");
        assert_eq!((r.section, r.key.as_str()), ("ui", "theme"));
    }

    #[test]
    fn resetting_what_is_already_factory_asks_nothing() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        s.set_cursor(0);
        assert!(s.reset().is_none());
    }

    #[test]
    fn a_plugins_row_does_not_reset() {
        let summary = PluginConfigSummary {
            plugin_id: "org.a".into(),
            name: "A".into(),
            key_count: 2,
        };
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[summary]));
        let last = s.visible().len() - 1;
        s.set_cursor(last);
        assert!(s.reset().is_none());
    }

    /// While editing, resetting does nothing: same as the rest of this
    /// machine, an open edit freezes everything else.
    #[test]
    fn while_editing_nothing_resets() {
        let mut cfg = empty_cfg();
        cfg.common.ui_font = Some("Inter".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(pos);
        s.activate(&[], &[]);
        assert!(s.is_editing());
        assert!(s.reset().is_none());
    }

    #[test]
    fn the_modified_operator_leaves_only_whats_touched() {
        let mut cfg = empty_cfg();
        cfg.common.ui_theme = Some("nord".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        for c in "@modified".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 1);
        assert_eq!(s.rows()[s.visible()[0]].id(), Some("ui.theme"));
    }

    /// In BOTH languages, and by the stable key: a translation file cannot
    /// be the difference between finding something and not finding it.
    #[test]
    fn the_section_operator_accepts_the_translated_and_the_stable_name() {
        for q in ["@section:appearance", "@section:apariencia"] {
            let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
            for c in q.chars() {
                s.push_char(c);
            }
            assert!(s.shown() > 0, "\"{q}\" found nothing");
            assert!(
                s.visible()
                    .iter()
                    .all(|&i| s.rows()[i].section == Section::Appearance)
            );
        }
    }

    /// EVERY section, in BOTH languages, by its whole label and by a prefix.
    /// Five of the seven have a two-word label, and the query is split on
    /// spaces: under exact equality they were unfindable, and the test that
    /// only tried "appearance" — the only one-word one in both languages —
    /// did not see it.
    #[test]
    fn every_section_is_found_by_its_label_in_both_languages() {
        for s in Section::ORDER {
            let mut queries = vec![s.stable_key().to_owned()];
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let label = norte_i18n::t_in(lang, s.label_key());
                // The first word: what survives the splitting.
                let first = label.split(' ').next().unwrap_or(&label).to_owned();
                queries.push(first);
            }
            for q in queries {
                assert_eq!(
                    section_by_name(&q),
                    Some(*s),
                    "\"{q}\" had to lead to {s:?}"
                );
            }
        }
    }

    /// And the other operator compares the same way: folded. Two operators
    /// with two case rules is a trap.
    #[test]
    fn the_modified_operator_is_case_insensitive() {
        let mut cfg = empty_cfg();
        cfg.common.ui_theme = Some("nord".to_owned());
        for q in ["@modified", "@Modified", "@MODIFIED"] {
            let mut s = SettingsState::new(build_rows(&cfg, &[]));
            for c in q.chars() {
                s.push_char(c);
            }
            assert_eq!(s.shown(), 1, "\"{q}\"");
        }
    }

    #[test]
    fn the_operators_combine_with_the_text() {
        let mut cfg = empty_cfg();
        cfg.common.ui_theme = Some("nord".to_owned());
        cfg.common.ui_show_hidden = Some(true);
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        for c in "@modified".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 2, "two touched");
        for c in " theme".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 1, "and with the text, one");
    }

    /// An `@` that does not open a known operator is TEXT. Nobody has to
    /// escape anything to search for an `@`.
    #[test]
    fn a_lone_at_sign_is_normal_text() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        for c in "@nada".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
        assert_eq!(
            s.total(),
            s.rows().len(),
            "the total is untouched by the filter"
        );
    }

    /// A section that does not exist filters to NOTHING. Ignoring the
    /// operator would show the whole list, and the reader would read that
    /// as "this is everything you asked for".
    #[test]
    fn a_nonexistent_section_does_not_show_the_whole_list() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        for c in "@section:loquesea".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
    }

    #[test]
    fn the_index_lists_every_section_even_if_the_filter_empties_one() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        // By ID, not by label: these tests run in the default locale, and a
        // filter written in Spanish matches nothing in English.
        for c in "theme".chars() {
            s.push_char(c);
        }
        let idx = s.sections();
        assert_eq!(
            idx.len(),
            Section::ORDER.len(),
            "the index does not shrink when filtering"
        );
        let appearance = idx
            .iter()
            .find(|v| v.section == Section::Appearance)
            .expect("appearance");
        assert!(appearance.visible > 0);
        let open_with = idx
            .iter()
            .find(|v| v.section == Section::OpenWith)
            .expect("open with");
        assert_eq!(open_with.visible, 0);
        assert_eq!(open_with.first_row, None);
    }

    #[test]
    fn jumping_to_a_section_puts_the_cursor_on_its_first_visible_row() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        s.jump_to(Section::Input);
        let row = &s.rows()[s.visible()[s.cursor()]];
        assert_eq!(row.section, Section::Input);
        // And it is the FIRST of the section, not just any one.
        assert!(s.cursor() == 0 || s.rows()[s.visible()[s.cursor() - 1]].section != Section::Input);
    }

    /// Setting a specific value validates with the SAME rules as the
    /// keyboard: it is what makes a window control not a second rule.
    #[test]
    fn set_value_validates_like_the_editor() {
        let themes = vec!["default".to_owned(), "nord".to_owned()];
        let presets = ["orthodox", "vim"];
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));

        // A boolean, no cycling.
        let w = s
            .set_value("ui.mouse", "false", &themes, &presets)
            .expect("bool");
        assert_eq!(
            (w.section, w.key.as_str(), w.display.as_str()),
            ("ui", "mouse", "false")
        );
        assert_eq!(w.value.as_bool(), Some(false));

        // A theme from the LIVE list, and one that is not there.
        assert!(s.set_value("ui.theme", "nord", &themes, &presets).is_ok());
        assert!(
            s.set_value("ui.theme", "inventado", &themes, &presets)
                .is_err()
        );

        // An out-of-range integer is rejected WITH its bounds, like the
        // editor.
        let e = s
            .set_value("ui.font-size", "999", &themes, &presets)
            .expect_err("out of range");
        assert!(matches!(e, SettingsEditError::OutOfRange { .. }));

        // And a command line is split the same way: array, not string.
        let w = s
            .set_value("ui.editor", "zed %f", &themes, &presets)
            .expect("args");
        assert!(w.value.as_array().is_some(), "saved split into pieces");
    }

    /// A value not from a closed list does not get in, wherever it comes
    /// from: the renderer does not validate, and a bridge can bring
    /// anything.
    #[test]
    fn set_value_rejects_what_is_not_in_the_list() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        assert!(s.set_value("ui.confirm-quit", "quizas", &[], &[]).is_err());
        assert!(s.set_value("ui.mouse", "SI", &[], &[]).is_err());
        // The status bar's items (ADR 0132): an id that does not exist, or a
        // repeated one, would break the next load of the file.
        assert_eq!(
            s.set_value("ui.status-items", "tasks git", &[], &[])
                .expect_err("unknown id"),
            SettingsEditError::Invalid {
                key: "msg-settings-invalid-status-items"
            }
        );
        assert!(
            s.set_value("ui.status-items", "tasks tasks", &[], &[])
                .is_err()
        );
        let w = s
            .set_value("ui.status-items", "notices  position", &[], &[])
            .expect("valid");
        assert_eq!(w.display, "notices position");
        assert!(s.set_value("no.existe", "1", &[], &[]).is_err());
    }

    #[test]
    fn every_entrys_control_comes_from_the_catalog() {
        assert_eq!(control_of("ui.mouse"), Some(Control::Toggle));
        assert_eq!(control_of("ui.theme"), Some(Control::ThemeChoice));
        assert_eq!(control_of("keymap.preset"), Some(Control::PresetChoice));
        assert!(matches!(control_of("ui.editor"), Some(Control::Args)));
        assert!(matches!(
            control_of("ui.confirm-quit"),
            Some(Control::Choice(_))
        ));
        // And no catalog entry is left without a control.
        for d in catalog() {
            assert!(control_of(d.id).is_some(), "\"{}\" has no control", d.id);
        }
    }

    /// With focus on the index, the arrows walk SECTIONS and the list
    /// follows — like the help sidebar, which opens the topic as it passes
    /// over it. With no second cursor to synchronize.
    #[test]
    fn with_focus_on_the_index_the_arrows_change_section() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        let section = |s: &SettingsState| s.rows()[s.visible()[s.cursor()]].section;
        assert_eq!(s.focus(), Focus::List);
        s.down();
        assert_eq!(
            section(&s),
            Section::Appearance,
            "in the list, goes down one row"
        );

        s.toggle_focus();
        assert_eq!(s.focus(), Focus::Index);
        s.down();
        assert_eq!(
            section(&s),
            Section::Panes,
            "in the index, goes down one section"
        );
        s.up();
        assert_eq!(section(&s), Section::Appearance);

        // And going back to the other side returns the arrows to the rows.
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
        let before = s.cursor();
        s.down();
        assert_eq!(s.cursor(), before + 1);
    }

    /// With no visible rows there is no section to go to: focus does not
    /// cross.
    #[test]
    fn with_an_empty_list_focus_does_not_move_to_the_index() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        for c in "@section:loquesea".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
    }

    /// And while editing it does not cross either: an open edit freezes
    /// everything else.
    #[test]
    fn while_editing_focus_does_not_change() {
        let mut cfg = empty_cfg();
        cfg.common.ui_font = Some("Inter".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(pos);
        s.activate(&[], &[]);
        assert!(s.is_editing());
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
    }

    /// Section stepping, which is the SAME in both screens.
    #[test]
    fn section_stepping_goes_and_comes_back() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        let section = |s: &SettingsState| s.rows()[s.visible()[s.cursor()]].section;
        assert_eq!(section(&s), Section::Appearance);
        assert_eq!(s.step_section(1), Some(Section::Panes));
        assert_eq!(section(&s), Section::Panes);
        assert_eq!(s.step_section(-1), Some(Section::Appearance));
    }

    /// At the edge there is nowhere to go and the cursor stays: pretending
    /// to wrap around is a cursor that teleports.
    #[test]
    fn section_stepping_stops_at_the_edge() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        assert_eq!(s.step_section(-1), None);
        assert_eq!(s.cursor(), 0);
    }

    /// A section the filter emptied is WALKED THROUGH, and if none is left
    /// with rows, nothing moves.
    #[test]
    fn section_stepping_skips_the_empty_ones() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        // "theme" only leaves rows in Appearance — not even the Plugins
        // note, whose haystack does not carry that word.
        for c in "theme".chars() {
            s.push_char(c);
        }
        let before = s.cursor();
        assert_eq!(s.step_section(1), None);
        assert_eq!(s.cursor(), before);
    }

    #[test]
    fn jumping_to_an_empty_section_moves_nothing() {
        let mut s = SettingsState::new(build_rows(&empty_cfg(), &[]));
        for c in "theme".chars() {
            s.push_char(c);
        }
        let before = s.cursor();
        s.jump_to(Section::OpenWith);
        assert_eq!(
            s.cursor(),
            before,
            "a section with no visible rows does not move the cursor"
        );
    }

    /// The "you touched this" dot is computed against the FACTORY value,
    /// with the same function that paints the value: a hand-written table
    /// of defaults goes out of sync with the schema the moment someone
    /// changes one.
    #[test]
    fn over_the_default_config_nothing_is_modified() {
        for r in build_rows(&empty_cfg(), &[]) {
            assert!(!r.modified, "\"{}\" should not come out modified", r.name);
        }
    }

    #[test]
    fn changing_one_field_lights_up_that_rows_dot_and_no_other() {
        let mut cfg = empty_cfg();
        cfg.common.ui_theme = Some("nord".to_owned());
        let rows = build_rows(&cfg, &[]);
        let touched: Vec<_> = rows
            .iter()
            .filter(|r| r.modified)
            .map(super::Row::id)
            .collect();
        assert_eq!(touched, vec![Some("ui.theme")]);
    }

    /// A row that does not come from the catalog is never modified: there
    /// is no factory value to compare it against.
    #[test]
    fn a_plugins_row_is_never_modified() {
        let summary = PluginConfigSummary {
            plugin_id: "org.a".into(),
            name: "A".into(),
            key_count: 2,
        };
        let rows = build_rows(&empty_cfg(), &[summary]);
        let row = rows.last().expect("there is a plugin row");
        assert_eq!(row.section, Section::Plugins);
        assert!(!row.modified);
    }

    /// No entry is left without a place. A new id with no section falls
    /// into "Behavior" via `SettingDef::section`'s `unwrap_or`, and this
    /// test is the only thing separating that patch from a silent
    /// misfiling.
    #[test]
    fn every_catalog_entry_has_a_section() {
        for d in catalog() {
            assert!(
                section_of(d.id).is_some(),
                "\"{}\" is not assigned to any section",
                d.id
            );
        }
    }

    /// And no catalog section is left empty: a section the index lists and
    /// that never has anything is a broken promise.
    #[test]
    fn every_catalog_section_has_at_least_one_entry() {
        for s in Section::ORDER {
            if matches!(s, Section::Plugins | Section::Paths) {
                continue; // Do not come from the catalog.
            }
            assert!(
                catalog().iter().any(|d| d.section() == *s),
                "section {s:?} has no entry"
            );
        }
    }

    /// Every section is said in both languages. Half a translated screen is
    /// worse than none.
    #[test]
    fn every_section_has_its_label_in_both_locales() {
        for s in Section::ORDER {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let txt = norte_i18n::t_in(lang, s.label_key());
                assert!(
                    !txt.is_empty() && !txt.contains(s.label_key()),
                    "{s:?} untranslated in {lang:?}: {txt}"
                );
            }
        }
    }

    /// Stepping forward and backward through the index does not wrap
    /// around: at the edges there is nowhere to go, and pretending there is
    /// is a cursor that teleports.
    #[test]
    fn section_stepping_stops_at_the_edges() {
        assert_eq!(Section::Appearance.step(-1), None);
        assert_eq!(Section::Appearance.step(1), Some(Section::Panes));
        assert_eq!(Section::Paths.step(1), None);
        assert_eq!(Section::Paths.step(-1), Some(Section::Plugins));
    }

    #[test]
    fn set_cursor_clamps_to_the_last_visible() {
        let mut s = SettingsState::new(rows());
        let last = s.visible().len() - 1;
        s.set_cursor(last + 50);
        assert_eq!(s.cursor(), last, "clamps to the last visible");
        s.set_cursor(0);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_is_a_no_op_while_editing() {
        let mut s = SettingsState::new(rows());
        // Unfiltered: ALL rows stay visible, so if the editing guard failed
        // there would be somewhere real to move to. `ui.font` is looked up
        // by its ID — a `Text` row, which opens editing on activation — not
        // by its position: rows come out in screen order, which is not the
        // catalog's.
        let idx = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(idx);
        assert_eq!(s.rows()[s.visible()[idx]].name, t("setting-ui-font-name"));
        s.activate(&[], &[]);
        assert!(s.is_editing());
        s.set_cursor(0);
        assert_eq!(
            s.cursor(),
            idx,
            "while editing, a click on another row does not move the cursor"
        );
    }

    #[test]
    fn row_id_returns_the_catalog_id_or_none_for_the_plugins_note() {
        let rows = rows();
        // Each catalog id comes out ONCE; the order is the screen's
        // (section first), not the catalog's.
        for def in catalog() {
            assert_eq!(
                rows.iter().filter(|r| r.id() == Some(def.id)).count(),
                1,
                "\"{}\" has to come out exactly once",
                def.id
            );
        }
        assert_eq!(rows.last().unwrap().id(), None);
    }

    #[test]
    fn cycle_wraps_and_starts_at_the_first_if_not_found() {
        let values = ["a", "b", "c"];
        assert_eq!(cycle("a", &values), "b");
        assert_eq!(cycle("c", &values), "a", "wrap");
        assert_eq!(cycle("x", &values), "a", "not found: starts at the first");
        assert_eq!(
            cycle("a", &[]),
            "a",
            "empty list: does not panic, does not change"
        );
    }

    // --- `quit_needs_confirm`/`edit_error_message` (revision S, M6 hoist) ---

    #[test]
    fn quit_needs_confirm_all_three_modes() {
        use norte_config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "Never: never"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "Always: always"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "Auto: follows pending"
        );
        assert!(!quit_needs_confirm(ConfirmQuit::Auto, false));
    }

    #[test]
    fn edit_error_message_by_category_is_never_empty() {
        assert!(!edit_error_message(&SettingsEditError::NotAnInt).is_empty());
        let msg = edit_error_message(&SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(!msg.is_empty());
        assert!(msg.contains('8') && msg.contains("32"), "{msg}");
    }
}
