//! Settings (F11 / `app.settings`) as seen from the host: what is
//! configured, where it comes from, and changing it.
//!
//! None of this is decided by this module. The settings catalog, each
//! entry's effective value, its localized text and the editing MACHINE
//! — cycling a boolean, validating an integer — are `norte_frontend::settings`:
//! the same catalog and the same editor as the terminal, with the same
//! stable ids. What is contributed here is the PROJECTION onto the bridge's
//! vocabulary, a section that is not configuration but diagnostics — where
//! each thing lives — and the way of asking for a value: the terminal types
//! it inline, and the window opens a field's dialog, which is its way of
//! asking.

use std::path::PathBuf;

use norte_frontend::settings::{
    Focus, PendingWrite, Row, Section, SettingsEditError, SettingsState, build_rows_in,
};
use norte_i18n::Lang;

use crate::bridge::clamp_display;
use crate::dto::{
    PathRowView, SectionIndexView, SettingRowView, SettingsSectionView, SettingsView,
};

/// A configuration layer, named the way the user names it.
///
/// Mirrors `norte_config::Layer` without depending on that crate: the host
/// does not discover files — whoever launched it already resolved the
/// layers — and dragging the directory finder in here would give it a
/// second idea of where the configuration lives (ADR 0066, decision D14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayer {
    /// `/etc/norte` (or `%ProgramData%\norte`).
    System,
    /// `$XDG_CONFIG_HOME/norte`.
    User,
    /// `<config>/profiles/<name>`, the layer the reader CHOOSES by name
    /// (spec 2026-08-26, D1).
    Profile,
    /// `./.norte`, only after trust (ADR 0026).
    Project,
}

impl ConfigLayer {
    /// Its name's Fluent key.
    fn label_id(self) -> &'static str {
        match self {
            Self::System => "settings-path-config-system",
            Self::User => "settings-path-config-user",
            Self::Profile => "settings-path-config-profile",
            Self::Project => "settings-path-config-project",
        }
    }
}

/// A location the window can show, with its existence ALREADY resolved.
///
/// The `missing` field comes from outside on purpose. Knowing whether a
/// directory exists is `std::fs::metadata`, i.e. blocking I/O, and this
/// projection runs on the SOLE WRITER's loop: with a configuration layer on
/// a hung NFS mount, opening settings froze the whole window — no keys, no
/// listings landing, no task progress — until the mount timed out. This is
/// rule 2, and startup already has a `spawn_blocking` where it can be done
/// right.
#[derive(Debug, Clone)]
pub struct HostPath {
    /// The path, in native bytes. Painted with `display_os_name`.
    pub path: PathBuf,
    /// It does not exist. A layer nobody created is STATED, instead of
    /// painting a path that looks like it is there.
    pub missing: bool,
}

/// Where each thing lives, as resolved by whoever launched the host.
///
/// Received already resolved on purpose. The host does not read files or
/// query the environment: if it did, a window could end up saying its
/// configuration lives somewhere other than where it actually read it from
/// — and it would do so while blocking the actor.
#[derive(Debug, Clone, Default)]
pub struct HostPaths {
    /// The configuration layers, in ASCENDING precedence.
    pub config_layers: Vec<(ConfigLayer, HostPath)>,
    /// The state directory (session, history).
    pub state_dir: Option<HostPath>,
    /// Where this window writes its logs.
    pub logs_dir: Option<HostPath>,
    /// The daemon socket it talks to.
    pub socket: Option<HostPath>,
}

/// What Enter (or a double click) does on the cursor's row.
#[derive(Debug)]
pub(crate) enum Activation {
    /// Nothing to activate: a path, or no row at all.
    Nothing,
    /// The row cycled on its own — boolean, enum, theme, preset — and this
    /// is what has to be written. Boxed: a `PendingWrite` carries a
    /// `toml_edit::Value` and is large next to the other variants.
    Write(Box<PendingWrite>),
    /// The row wants a typed value: `name` and `actual` for the dialog
    /// that asks for it.
    RequestText {
        /// The entry's name, already translated.
        name: String,
        /// What it currently says.
        actual: String,
        /// WHAT it was asked ABOUT, by its catalog id.
        ///
        /// An id and not a row number: the dialog stays open while the
        /// search behind it stays alive, and a position stops naming the
        /// same row as soon as the filter changes — confirming would write
        /// the typed value onto ANOTHER setting. A position does not name a
        /// row in a list that moves.
        id: &'static str,
    },
}

/// The open settings: the shared editor plus the locations.
///
/// The cursor is over the FLAT list — the catalog's entries and then the
/// paths — and is only projected onto the editor's when it is time to
/// activate an entry: the editor knows nothing about paths, and has no
/// reason to.
pub(crate) struct Settings {
    /// The editor shared with the terminal, over the catalog's rows.
    ///
    /// No filter: the terminal filters by typing because its overlay eats
    /// every printable character, and here printable keys never reach the
    /// host. What matters is that cycling, validating and proposing a value
    /// is ONE machine.
    state: SettingsState,
    /// The locations, already sanitized.
    paths: Vec<PathRowView>,
    /// Which row the cursor is on, over the FLAT list.
    cursor: usize,
}

impl Settings {
    /// Opens the view with the configuration the host currently has set.
    ///
    /// The shared model's plugins section is left out: its rows need each
    /// extension's `[config]` schema, which arrives with the next slice.
    /// Showing its "no extension declares settings" row without having
    /// asked would assert something that has not been checked.
    pub(crate) fn open(
        cfg: &norte_frontend::config::FrontendConfig,
        paths: &HostPaths,
        lang: Lang,
    ) -> Self {
        Self {
            state: SettingsState::new(rows_of(cfg, lang)),
            paths: paths_of(paths, lang),
            cursor: 0,
        }
    }

    /// The rows are rebuilt over the RELOADED configuration, with the
    /// cursor where it was.
    ///
    /// This is what happens to the terminal on every hot reload, and for
    /// the same reason: the row that just cycled already shows the new
    /// value (optimistically), and this leaves it saying what the file
    /// actually says.
    pub(crate) fn refresh(&mut self, cfg: &norte_frontend::config::FrontendConfig, lang: Lang) {
        self.state.refresh(rows_of(cfg, lang));
    }

    /// How many eligible rows there are NOW: the ones the filter lets
    /// through, plus the locations, which are not filtered (they are
    /// diagnostics, not settings).
    fn total(&self) -> usize {
        self.state.shown() + self.paths.len()
    }

    /// Flat row `row` as a position among the editor's VISIBLE ones, or
    /// `None` if it falls on the locations (or out of range).
    ///
    /// This is the translation the earlier `debug_assert` said would be
    /// needed the day this window filtered: with a filter set, the third
    /// flat row is not the catalog's third.
    fn visible_row(&self, row: usize) -> Option<usize> {
        (row < self.state.shown()).then_some(row)
    }

    /// Sets the search query.
    ///
    /// The cursor is re-clamped: while filtering, the row it was pointing
    /// at may be gone, and a cursor outside the list is an Enter that
    /// activates something else.
    pub(crate) fn query(&mut self, text: &str) {
        self.state.set_query(text);
        let total = self.total();
        self.cursor = if total == 0 {
            0
        } else {
            self.cursor.min(total - 1)
        };
    }

    /// Takes the cursor to a section's first row, named by its stable key.
    /// One that does not exist, or that the filter emptied, moves nothing.
    pub(crate) fn skip(&mut self, key: &str) {
        let Some(section) = Section::ORDER
            .iter()
            .copied()
            .find(|s| s.stable_key() == key)
        else {
            return;
        };
        if section == Section::Paths {
            // The locations go behind everything and the editor does not
            // carry them.
            if !self.paths.is_empty() {
                self.cursor = self.state.shown();
            }
            return;
        }
        // Decided from the PROJECTION, not by comparing the editor's cursor
        // before and after: that cursor and this window's are two different
        // ones, and only `activate` synchronizes them — so "it did not move"
        // meant nothing, and jumping to an empty section moved the cursor to
        // the list's first row.
        let Some(view) = self
            .state
            .sections()
            .into_iter()
            .find(|v| v.section == section)
        else {
            return;
        };
        let Some(first) = view.first_row else {
            return; // Empty because of the filter: not a place to go to.
        };
        self.state.set_cursor(first);
        self.cursor = first;
    }

    /// Sets a specific value on setting `id` — what a control sends.
    ///
    /// # Errors
    /// Whatever the shared editor rejects, without writing anything.
    pub(crate) fn set(
        &mut self,
        id: &str,
        value: &str,
        themes: &[String],
        presets: &[&str],
    ) -> Result<PendingWrite, SettingsEditError> {
        self.state.set_value(id, value, themes, presets)
    }

    /// Does this id's row still say it is not the factory value?
    ///
    /// Asked AFTER rereading, and it is what distinguishes "reset" from
    /// "another layer sets it" without building layer provenance.
    pub(crate) fn follows_modified(&self, id: &str) -> bool {
        self.state
            .rows()
            .iter()
            .find(|r| r.id() == Some(id))
            .is_some_and(|r| r.modified)
    }

    /// Reset row `row`: the key that has to be removed, or `None`.
    ///
    /// A location is not reset — it is not a setting — and neither is a row
    /// that is already at its factory value.
    pub(crate) fn reset(&mut self, row: usize) -> Option<norte_frontend::settings::PendingReset> {
        let visible = self.visible_row(row)?;
        self.state.set_cursor(visible);
        self.state.reset()
    }

    /// Enter on the cursor's row.
    ///
    /// The theme and preset lists arrive from outside and LIVE, as in the
    /// terminal: the effective theme may have changed hot.
    pub(crate) fn activate(&mut self, themes: &[String], presets: &[&str]) -> Activation {
        // From FLAT row to VISIBLE row: with the search box set, the
        // screen's third row is not the catalog's third.
        let Some(row) = self.visible_row(self.cursor) else {
            return Activation::Nothing;
        };
        self.state.set_cursor(row);
        if let Some(write) = self.state.activate(themes, presets) {
            return Activation::Write(Box::new(write));
        }
        if !self.state.is_editing() {
            return Activation::Nothing;
        }
        // The window does not type inline: it asks with a dialog, and the
        // value comes back WHOLE on confirmation. Until then the editor is
        // not left half-open — `confirm_text` reopens editing on the same
        // row, and a cancelled dialog leaves nothing to close.
        let current = self.state.edit_buffer().unwrap_or_default().to_owned();
        self.state.edit_cancel();
        // By `visible[row]`, not by `row`: `row` is a position among the
        // VISIBLE ones, and with a filter set, indexing `rows()` with it gave
        // the name of a different setting — the dialog said "Theme" and
        // wrote to a different one.
        let resolved = self.state.visible()[row];
        let current_row = &self.state.rows()[resolved];
        let name = current_row.name.clone();
        let Some(id) = current_row.id() else {
            return Activation::Nothing;
        };
        Activation::RequestText {
            name,
            actual: current,
            id,
        }
    }

    /// The value the dialog brought back for setting `id`.
    ///
    /// Re-enters that row's editing, puts in the whole text and confirms:
    /// the validation — an integer's range, a command line's shape — is the
    /// shared editor's, not a copy.
    ///
    /// Looked up BY ID and not by position: the search behind it stays alive
    /// while the dialog is open, and a position stops naming the same row as
    /// soon as the filter changes.
    ///
    /// # Errors
    /// Whatever the editor rejects, without writing anything. A setting that
    /// is no longer visible — the filter changed under the dialog — or that
    /// no longer wants text is rejected like an invalid integer: it is the
    /// editor's inert failure, and there is nothing to write.
    pub(crate) fn confirm_text(
        &mut self,
        id: &str,
        text: &str,
    ) -> Result<PendingWrite, SettingsEditError> {
        let Some(visible) = self
            .state
            .visible()
            .iter()
            .position(|&i| self.state.rows()[i].id() == Some(id))
        else {
            return Err(SettingsEditError::NotAnInt);
        };
        self.state.set_cursor(visible);
        // No lists: a text row does not look at them, and one that did would
        // cycle instead of edit, which is exactly what the guard below
        // rejects. With an empty list `cycle` returns the value that was
        // there, so the `PendingWrite` discarded here was also a no-op.
        if self.state.activate(&[], &[]).is_some() || !self.state.is_editing() {
            self.state.edit_cancel();
            return Err(SettingsEditError::NotAnInt);
        }
        self.state.edit_set(text);
        let result = self.state.edit_commit();
        // A rejection leaves the buffer open in the editor (the terminal
        // keeps it so it can be corrected); here the dialog has already
        // closed, and a hanging edit would make the next Enter fail to
        // cycle.
        self.state.edit_cancel();
        result
    }

    /// Switches sides: index <-> list.
    ///
    /// Focus lives in the shared editor, not here: it is the same decision
    /// — and the same arrow keys — on both screens, and duplicating it is
    /// how they drift apart.
    pub(crate) fn change_side(&mut self) {
        self.state.toggle_focus();
    }

    /// Which side has the keyboard.
    pub(crate) fn focus(&self) -> Focus {
        self.state.focus()
    }

    /// Moves the cursor `delta` rows, without going out of bounds.
    ///
    /// With focus on the INDEX it does not move rows: it changes section,
    /// one per keypress, and the flat cursor follows the new section's first
    /// row. A page on the index is one section, not ten: the index has seven
    /// rows and paging through it means nothing.
    pub(crate) fn mover(&mut self, delta: i64) {
        if self.state.focus() == Focus::Index {
            if delta != 0 {
                let step = if delta > 0 { 1 } else { -1 };
                self.state.step_section(step);
                self.cursor = self.state.cursor();
            }
            return;
        }
        let total = self.total();
        if total == 0 {
            return;
        }
        let target = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        self.cursor = usize::try_from(target.max(0)).unwrap_or(0).min(total - 1);
    }

    /// Puts the cursor on a specific row (a click). Out of range does
    /// nothing: whoever is painting can be one frame behind.
    pub(crate) fn point_at(&mut self, row: usize) {
        if row < self.total() {
            self.cursor = row;
        }
    }

    /// The projection: one section for each one that has rows, in screen
    /// order, plus the index and the search box's two counts.
    ///
    /// A section the FILTER emptied stays in the index, dimmed; one this
    /// surface does not have does not appear. The locations go at the end
    /// and the filter does not touch them: they are diagnostics, not
    /// settings.
    pub(crate) fn vista(&self, lang: Lang, themes: &[String], presets: &[&str]) -> SettingsView {
        let sections_list = self.state.sections();
        let mut sections = Vec::new();
        for v in &sections_list {
            if v.section == Section::Paths || v.total == 0 {
                continue;
            }
            let rows: Vec<_> = self
                .state
                .visible()
                .iter()
                .map(|&i| &self.state.rows()[i])
                .filter(|r| r.section == v.section)
                .map(|r| project_row(r, themes, presets))
                .collect();
            if rows.is_empty() {
                continue;
            }
            sections.push(SettingsSectionView::Settings {
                key: v.section.stable_key().to_owned(),
                title: clamp_display(norte_i18n::t_in(lang, v.section.label_key())),
                rows,
            });
        }
        if !self.paths.is_empty() {
            sections.push(SettingsSectionView::Paths {
                title: clamp_display(norte_i18n::t_in(lang, "settings-section-paths")),
                rows: self.paths.clone(),
            });
        }
        let mut index: Vec<SectionIndexView> = sections_list
            .iter()
            .filter(|v| v.section != Section::Paths && v.total > 0)
            .map(|v| SectionIndexView {
                key: v.section.stable_key().to_owned(),
                title: clamp_display(norte_i18n::t_in(lang, v.section.label_key())),
                visible: v.visible as u64,
            })
            .collect();
        if !self.paths.is_empty() {
            index.push(SectionIndexView {
                key: Section::Paths.stable_key().to_owned(),
                title: clamp_display(norte_i18n::t_in(lang, "settings-section-paths")),
                visible: self.paths.len() as u64,
            });
        }
        SettingsView {
            sections,
            index,
            focus: match self.focus() {
                Focus::Index => "index",
                Focus::List => "list",
            }
            .to_owned(),
            cursor: self.cursor as u64,
            query: clamp_display(self.state.query_display()),
            shown: self.state.shown() as u64,
            total: self.state.total() as u64,
        }
    }
}

/// The catalog's rows, in the HOST's language.
///
/// Section titles already came with it, and each option's name and
/// description with the process's: half a screen in each language is worse
/// than no translation at all.
fn rows_of(cfg: &norte_frontend::config::FrontendConfig, lang: Lang) -> Vec<Row> {
    build_rows_in(cfg, &[], lang)
        .into_iter()
        .filter(|r| !r.is_plugins_note())
        .collect()
}

/// What the window CANNOT apply without restarting, by catalog id.
///
/// The shared catalog says what applies hot from the terminal's point of
/// view, which reloads everything. The window rereads the WHOLE
/// configuration when a setting is written (`apply_config`), and almost
/// everything is read at the moment it is used — the bars when projecting
/// each frame, the editor and the comparer when launching them, the search
/// mode when searching, whether it asks on exit when exiting — so it
/// changes instantly. What does not: what the host resolves once at startup
/// — the language, the fonts, reduced motion, which is what
/// `out_of_scope_hot` names — and what gets fixed when creating
/// each slot — the hidden files and the `..` row — which already-open slots
/// do not reread. Marking EVERYTHING else as "requires restart" was lying
/// sixteen times on one screen.
fn needs_restart(id: &str) -> bool {
    matches!(
        id,
        "ui.lang"
            | "ui.font"
            | "ui.mono-font"
            | "ui.font-size"
            | "ui.reduce-motion"
            | "ui.show-hidden"
            | "ui.parent-entry"
    )
}

/// A row's control, with its values already RESOLVED.
///
/// The live lists are resolved here and not in the renderer: installed
/// themes and presets change hot, and a dropdown carrying the baked-in list
/// would show the one from two reloads ago.
fn project_control(
    r: &Row,
    themes: &[String],
    presets: &[&str],
) -> (String, Vec<String>, Option<i64>, Option<i64>) {
    use norte_frontend::settings::{Control, control_of};
    let Some(control) = r.id().and_then(control_of) else {
        // A row that does not come from the catalog — a plugin's summary —
        // is not edited from here: you enter it instead.
        return ("none".to_owned(), Vec::new(), None, None);
    };
    match control {
        Control::Toggle => ("toggle".to_owned(), Vec::new(), None, None),
        Control::Choice(v) => (
            "choice".to_owned(),
            v.iter().map(|s| (*s).to_owned()).collect(),
            None,
            None,
        ),
        Control::ThemeChoice => ("choice".to_owned(), themes.to_vec(), None, None),
        Control::PresetChoice => (
            "choice".to_owned(),
            presets.iter().map(|s| (*s).to_owned()).collect(),
            None,
            None,
        ),
        Control::Number { min, max } => ("number".to_owned(), Vec::new(), Some(min), Some(max)),
        Control::Text => ("text".to_owned(), Vec::new(), None, None),
        Control::Args => ("args".to_owned(), Vec::new(), None, None),
    }
}

/// A catalog row, projected.
fn project_row(r: &Row, themes: &[String], presets: &[&str]) -> SettingRowView {
    let (control, choices, min, max) = project_control(r, themes, presets);
    let (value_text, hostile) = norte_frontend::display_name(r.value.as_bytes());
    SettingRowView {
        // The id is an IDENTITY from the shared catalog, not prose: it
        // travels whole, unclamped, and the renderer never paints it.
        id: r.id().unwrap_or_default().to_owned(),
        name: clamp_display(r.name.clone()),
        desc: clamp_display(r.desc.clone()),
        // The VALUE comes from `norte.toml` as-is — `ui.font`, `ui.theme`,
        // `keymap.preset` are strings the user writes, and the PROJECT
        // layer is "I opened this repository", not "I vouch for this
        // string" (ADR 0026). It was the only place in this window where
        // outside text reached the DOM without going through the mask.
        value: clamp_display(value_text),
        hostile,
        // By id and not by `SettingDef::applies_live`: that field is written
        // from the terminal's point of view, which reloads everything hot,
        // and this window only reloads what a profile change knows how to
        // apply. Saying an entry applies itself when it does not is the
        // kind of lie that sends the user hunting for a bug that does not
        // exist.
        restart_required: r.id().is_none_or(needs_restart),
        // The factory value, masked like any other: it comes from the
        // catalog, but is painted in the same column as one from the file.
        default: r
            .id()
            .and_then(|id| {
                norte_frontend::settings::catalog()
                    .iter()
                    .find(|d| d.id == id)
            })
            .map(|d| clamp_display(norte_frontend::settings::default_value(d)))
            .unwrap_or_default(),
        control,
        choices,
        min,
        max,
        modified: r.modified,
    }
}

/// The locations, sanitized for painting.
///
/// ZERO I/O: each location's existence comes from [`HostPath`], already
/// resolved by startup. This is what makes "the host does not read files"
/// true, which this module used to say three times while calling
/// `exists()`.
fn paths_of(paths: &HostPaths, lang: Lang) -> Vec<PathRowView> {
    let mut out = Vec::new();
    for (layer, dir) in &paths.config_layers {
        out.push(path_row_of(norte_i18n::t_in(lang, layer.label_id()), dir));
    }
    for (key, dir) in [
        ("settings-path-state", paths.state_dir.as_ref()),
        ("settings-path-logs", paths.logs_dir.as_ref()),
        ("settings-path-socket", paths.socket.as_ref()),
    ] {
        if let Some(d) = dir {
            out.push(path_row_of(norte_i18n::t_in(lang, key), d));
        }
    }
    out
}

/// A location: the already-masked text, whether it differs from the real
/// one, and whether it is there.
///
/// A path is BYTES and not a string (rule 1), so it is painted the same way
/// as a listing's file name — `display_name` over the native bytes — and
/// NEVER via `to_string_lossy`, which swallows the difference between a
/// strange name and a hostile one without saying so.
fn path_row_of(label: String, dir: &HostPath) -> PathRowView {
    let (paintable, hostile) = norte_frontend::display::display_os_name(dir.path.as_os_str());
    PathRowView {
        label: clamp_display(label),
        display: clamp_display(paintable),
        hostile,
        missing: dir.missing,
    }
}
