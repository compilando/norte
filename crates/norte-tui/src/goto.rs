//! "Go to anywhere" in the TUI (phase 6 of the WOW program): where its rows
//! come from.
//!
//! The model — sections, order, filtering, cursor —, how each row class is
//! built, and what confirming one means belong to [`norte_frontend::goto`],
//! shared with the window (#357). What lives here is what only this terminal
//! knows: which lists it holds in memory, and what encoding their paths are
//! painted with.
//!
//! Rows are taken as a SNAPSHOT on open, same as the palette does with its
//! own, except for the typed path (which is the query) and the semantic index
//! (which arrives once the core answers). A list that changes under the
//! cursor while the reader is looking at it is how an Enter ends up somewhere
//! else.

use norte_frontend::goto::{
    BROUGHT_BY_LIST, FixedSource, Goto, GotoRow, GotoSource, PathSource, SECTION_COMMANDS,
    SECTION_CONNECTIONS, SECTION_FAVORITES, SECTION_HISTORY, SECTION_INDEX, SECTION_POPULAR,
    command_rows, row_connection, row_path,
};
use norte_i18n::t;

use crate::app::App;

pub use norte_frontend::goto::{Action, MINIMUM_FOR_THE_INDEX};

/// The SYNCHRONOUS sources: the lists the TUI already has in memory.
///
/// `connections` arrives separately because reading them touches disk, and
/// that is the caller's job, which is async; passing them empty is correct
/// when they could not be read — one fewer section, not a screen that fails
/// to open.
#[must_use]
pub fn sources(app: &App, connections: &[(String, String)]) -> Vec<Box<dyn GotoSource + Send>> {
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

    // The commands, the SAME ones the palette offers in this context: the
    // palette already resolves what can run with the viewer open.
    let commands = command_rows(crate::palette::rows_for_context(
        &app.palette_rows,
        app.viewer.is_some(),
    ));
    out.push(Box::new(
        FixedSource::new(SECTION_COMMANDS, commands).only_with_query(),
    ));

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

/// What to do with the row the reader just confirmed — the decision belongs
/// to the shared model ([`norte_frontend::goto::action`]).
#[must_use]
pub fn action(_app: &App, key: &str) -> Action {
    norte_frontend::goto::action(key)
}

/// Opens the screen with the given sources.
pub fn open(app: &mut App, connections: &[(String, String)]) {
    let sources = sources(app, connections);
    app.goto = Some(Goto::new(sources));
}
