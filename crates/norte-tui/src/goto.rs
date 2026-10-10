//! "Go to anywhere" in the TUI (phase 6 of the WOW program): where its rows
//! come from.
//!
//! The model — sections, order, filtering, cursor —, how each row class is
//! built, and what confirming one means belong to [`norte_frontend::goto`],
//! shared with the window (#357). What lives here is what only this terminal
//! knows: which lists it holds in memory, and what encoding their paths are
//! painted with.
//!
//! Rows are taken as a SNAPSHOT on open, except for the typed path (which is
//! the query) and the semantic index (which arrives once the core answers). A
//! list that changes under the cursor while the reader is looking at it is
//! how an Enter ends up somewhere else.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_frontend::goto::{
    BROUGHT_BY_LIST, FixedSource, Goto, GotoRow, GotoSource, PREFIX_COMMANDS, PathSource,
    SECTION_COMMANDS, SECTION_CONNECTIONS, SECTION_FAVORITES, SECTION_HELP, SECTION_HISTORY,
    SECTION_INDEX, SECTION_PLUGINS, SECTION_POPULAR, command_rows, plugin_command_rows,
    row_connection, row_path,
};
use norte_i18n::t;

use crate::app::App;

pub use norte_frontend::goto::{Action, MINIMUM_FOR_THE_INDEX};

/// The SYNCHRONOUS sources: the lists the TUI already has in memory.
///
/// `connections` and `plugins` arrive separately because reading them
/// touches disk or the daemon, and that is the caller's job, which is async;
/// passing them empty is correct when they could not be read — one fewer
/// section, not a screen that fails to open.
#[must_use]
pub fn sources(
    app: &App,
    connections: &[(String, String)],
    plugins: &[norte_proto::methods::PluginInfo],
) -> Vec<Box<dyn GotoSource + Send>> {
    let focus = app.focus();
    let enc = app.focused().name_encoding();
    let current = app.focused().dir().clone();
    let mut out: Vec<Box<dyn GotoSource + Send>> = Vec::new();

    // The typed path. Its detail says what is about to happen, because the
    // row IS the query, and without that it would look like it did not
    // understand what you typed.
    out.push(Box::new(PathSource::new(t("goto-path-desc"))));

    // History of the focused pane: no filter (the model filters) and no
    // current directory, which nobody wants to go to. It is the ONLY section
    // that carries the pane's reinterpretation, because it is the only one
    // whose paths belong to that pane.
    let history: Vec<GotoRow> =
        norte_frontend::history::history_rows(&app.history[focus], &current, "", enc)
            .into_iter()
            .filter(|r| r.mark != norte_frontend::history::HistoryMark::Current)
            .take(BROUGHT_BY_LIST)
            .map(|r| row_path(SECTION_HISTORY.id, None, &r.path, enc))
            .collect();
    out.push(Box::new(FixedSource::new(SECTION_HISTORY, history)));

    let popular: Vec<GotoRow> = norte_frontend::history::popular_rows(&app.popular, &current, "")
        .into_iter()
        .take(BROUGHT_BY_LIST)
        .map(|r| row_path(SECTION_POPULAR.id, None, &r.path, None))
        .collect();
    out.push(Box::new(FixedSource::new(SECTION_POPULAR, popular)));

    // A favorite whose destination fails to parse is NOT offered: the sites
    // list already says so via its error, and here a row that cannot lead
    // anywhere is only a way to waste an Enter.
    let favorites: Vec<GotoRow> = app
        .hotlist
        .iter()
        .filter_map(|it| {
            it.target
                .as_ref()
                .ok()
                .map(|p| row_path(SECTION_FAVORITES.id, Some(&it.name), p, None))
        })
        .collect();
    out.push(Box::new(FixedSource::new(SECTION_FAVORITES, favorites)));

    let connections: Vec<GotoRow> = connections
        .iter()
        .map(|(name, url)| row_connection(name, url))
        .collect();
    out.push(Box::new(FixedSource::new(SECTION_CONNECTIONS, connections)));

    // The commands of this context: `rows_for_context` already resolves
    // what can run with the viewer open.
    let commands = command_rows(
        crate::palette::rows_for_context(&app.palette_rows, app.viewer.is_some()),
        Some(&app.help_facts()),
        norte_i18n::active(),
    );
    out.push(Box::new(FixedSource::new(SECTION_COMMANDS, commands)));
    // The plugins' commands, renamers and organizers: built apart, because
    // their id has no catalogue namespace to file them under and the daemon,
    // not this frontend, decides whether they run.
    out.push(Box::new(FixedSource::new(
        SECTION_PLUGINS,
        plugin_command_rows(crate::palette::plugin_rows(plugins)),
    )));

    out.push(Box::new(FixedSource::new(
        SECTION_HELP,
        norte_frontend::goto::help_rows(norte_i18n::active()),
    )));

    out
}

/// Feeds `goto` what the index answered.
pub fn set_index(app: &mut App, hits: &[norte_proto::methods::SemanticHit]) {
    let rows = norte_frontend::goto::index_rows(hits);
    if let Some(goto) = &mut app.goto {
        // `ya_filtered`: the index matched by MEANING, and running the
        // query's subsequence over it again would throw away exactly what
        // makes it useful.
        goto.replace_section(SECTION_INDEX, rows, true);
    }
}

/// Opens the box on `query` (`""` places, `>` commands). Already open, it
/// only switches the mode in place: the rows it took on open are still the
/// snapshot, and the reader keeps the box they were looking at.
pub fn open(
    app: &mut App,
    connections: &[(String, String)],
    plugins: &[norte_proto::methods::PluginInfo],
    query: &str,
) {
    if let Some(g) = &mut app.goto {
        g.set_query(query);
        return;
    }
    let mut goto = Goto::new(sources(app, connections, plugins)).with_recent(&app.palette_recent);
    goto.set_query(query);
    app.goto = Some(goto);
}

/// What a non-typing key does in the open box: `Some(query)` if it is the
/// chord of `app.palette` or `app.goto` (switch the mode in place).
/// A plain character never gets here — it types — so vim's `:` writes a
/// colon inside the box.
#[must_use]
pub fn switch_for(
    eff: &crate::keymap::Effective,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<&'static str> {
    let chord = crate::keymap::chord_from_crossterm(mods, code)?;
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if plain && matches!(code, KeyCode::Char(_)) {
        return None;
    }
    if eff.single_chord_runs(chord, "app.palette") {
        Some(PREFIX_COMMANDS)
    } else if eff.single_chord_runs(chord, "app.goto") {
        Some("")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::open;
    use crate::app::{App, Pane};
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_frontend::goto::Mode;
    use norte_proto::VPath;

    fn app() -> App {
        let d = VPath::parse("file:///x").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// Krusader's Alt+P is `app.palette`, and the palette is the box with `>`.
    #[test]
    fn krusaders_alt_p_opens_the_box_with_the_commands_prefix() {
        let (_, preset) = crate::keymap::presets()
            .into_iter()
            .find(|(n, _)| *n == "krusader")
            .expect("krusader preset");
        let eff = crate::keymap::Effective::build_for(
            &preset,
            &[],
            crate::keymap::COMMANDS,
            crate::keymap::Screen::Browse,
        )
        .expect("effective");
        let alt_p = crate::keymap::chord_from_crossterm(KeyModifiers::ALT, KeyCode::Char('p'))
            .expect("a chord");
        assert!(eff.single_chord_runs(alt_p, "app.palette"));
        let mut a = app();
        open(&mut a, &[], &[], norte_frontend::goto::PREFIX_COMMANDS);
        let g = a.goto.as_ref().expect("open");
        assert_eq!((g.query(), g.mode()), (">", Mode::Commands));
    }

    /// Ctrl+G opens it empty, in places; opening again while open switches
    /// the mode in place instead of rebuilding.
    #[test]
    fn opening_while_open_switches_the_mode_in_place() {
        let mut a = app();
        open(&mut a, &[], &[], "");
        assert_eq!(
            a.goto.as_ref().map(norte_frontend::goto::Goto::mode),
            Some(Mode::Places)
        );
        a.goto.as_mut().expect("open").push_char('x');
        open(&mut a, &[], &[], ">");
        assert_eq!(
            a.goto.as_ref().map(|g| g.query().to_owned()),
            Some(">".to_owned())
        );
    }

    #[test]
    fn vims_colon_types_and_ctrl_p_switches() {
        let (_, preset) = crate::keymap::presets()
            .into_iter()
            .find(|(n, _)| *n == "vim")
            .expect("vim preset");
        let eff = crate::keymap::Effective::build_for(
            &preset,
            &[],
            crate::keymap::COMMANDS,
            crate::keymap::Screen::Browse,
        )
        .expect("effective");
        assert_eq!(
            super::switch_for(&eff, KeyModifiers::NONE, KeyCode::Char(':')),
            None
        );
        assert_eq!(
            super::switch_for(&eff, KeyModifiers::SHIFT, KeyCode::Char(':')),
            None
        );
        assert_eq!(
            super::switch_for(&eff, KeyModifiers::CONTROL, KeyCode::Char('p')),
            Some(">")
        );
        assert_eq!(
            super::switch_for(&eff, KeyModifiers::CONTROL, KeyCode::Char('g')),
            Some("")
        );
    }
}
