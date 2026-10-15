//! The search panel in the TUI: the slot's `kind`, its keys and its runs.
//!
//! The MODEL — the form, the pause, the results — lives in
//! [`norte_frontend::search_panel`], shared with the window. What is here is
//! what only this frontend knows: which of its inputs has the caret, which
//! keys type and which move, and the run in flight — launched when the
//! pause is over, drained in the loop's `select!`, cancelled when another
//! replaces it or the panel goes away. The panel's runs do NOT go on the
//! task board: the panel itself says how its search is going (D3).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::search::{
    ID_CASE, ID_KINDS, ID_RECURSIVE, ID_REGEX, ID_WHOLE_WORD, SearchField,
};
use norte_frontend::results::ResultSet;
use norte_frontend::search_panel::{self, Activation, Due, SearchPanel};
use norte_frontend::search_status::{Outcome, outcome_of};
use norte_i18n::t;
use norte_proto::methods::SearchHits;

use crate::app::{App, KeyOwner, detail_for_bar, error_category};
use crate::console::Console;
use crate::jobs::{InFlight, SearchRun};
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};
use crate::navigate::{Cd, cd, settle_cd};
use crate::viewer_open::open_viewer_at;

/// The slot's `kind`, exactly as the shared registry declares it.
pub const KIND: &str = "search";

/// Rows a page key moves in the results.
const PAGE: usize = 10;

/// Where the panel's keys go in the terminal (the window's inputs own their
/// caret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    /// One of the form's inputs: printable keys are typed into it.
    Field(SearchField),
    /// The results: arrows move, Enter activates.
    Tree,
}

impl Default for SearchFocus {
    fn default() -> Self {
        Self::Field(SearchField::Name)
    }
}

/// The panel's run in flight: the batches of `epoch` arrive on `rx`.
pub struct PanelRun {
    /// The model's epoch for this run: its batches carry it back.
    pub epoch: u64,
    /// The `fs.search` Task, to cancel it.
    pub task: TaskRef,
    /// Its batches; closed when the walk ends.
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
}

/// The clock the pause is measured with: tokio's, so a test that pauses
/// time drives the pause too.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// The instant "changed in the last N days" counts from.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn panel_mut(app: &mut App) -> Option<&mut SearchPanel> {
    let slot = app.search_panel_slot()?;
    app.panes.search_panel_mut(slot)
}

/// Is the keyboard in one of the panel's inputs? Then a plain key is text.
#[must_use]
pub fn typing(app: &App) -> bool {
    app.key_owner() == KeyOwner::Search && matches!(app.search_panel_focus, SearchFocus::Field(_))
}

/// Is the full-screen viewer open over the panel (Enter on a line row)? Its
/// keys are the viewer's while it lasts; the panel keeps the keyboard
/// OWNERSHIP, so closing the viewer lands the keys back on the results.
#[must_use]
pub fn viewer_over(app: &App) -> bool {
    app.viewer.is_some() && app.key_owner() == KeyOwner::Search
}

/// Would this key be TEXT in the panel's input? A plain key with the caret
/// in an input is; a chord with ctrl or alt never is, so it can still run a
/// shared panel command (`alt+F` closes the panel it opened).
#[must_use]
pub fn types_text(app: &App, mods: KeyModifiers) -> bool {
    typing(app) && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// The inputs Tab goes through, in order: the filters only while unfolded.
fn inputs(filters_open: bool) -> &'static [SearchField] {
    if filters_open {
        &SearchField::ORDEN
    } else {
        &SearchField::ORDEN[..2]
    }
}

/// The stop after `focus`: the inputs, then the results, then back to the
/// name.
fn next_stop(focus: SearchFocus, filters_open: bool) -> SearchFocus {
    let list = inputs(filters_open);
    match focus {
        SearchFocus::Tree => SearchFocus::Field(SearchField::Name),
        SearchFocus::Field(f) => match list.iter().position(|x| *x == f) {
            Some(i) if i + 1 < list.len() => SearchFocus::Field(list[i + 1]),
            _ => SearchFocus::Tree,
        },
    }
}

/// The stop before `focus`.
fn prev_stop(focus: SearchFocus, filters_open: bool) -> SearchFocus {
    let list = inputs(filters_open);
    match focus {
        SearchFocus::Tree => SearchFocus::Field(list[list.len() - 1]),
        SearchFocus::Field(f) => match list.iter().position(|x| *x == f) {
            Some(i) if i > 0 => SearchFocus::Field(list[i - 1]),
            _ => SearchFocus::Tree,
        },
    }
}

/// The status sentence of a run, in `lang`. The counts pick their own id
/// (`-one` or plural) HERE: Fluent cannot select a plural on string args, so
/// the Spanish read "1 ficheros en 1 carpetas".
#[must_use]
pub fn run_text(lang: norte_i18n::Lang, key: &str, files: usize, folders: usize) -> String {
    let count = |one: &str, many: &str, n: usize| {
        if n == 1 {
            norte_i18n::t_in(lang, one)
        } else {
            norte_i18n::ta_in(lang, many, &[("n", &n.to_string())])
        }
    };
    norte_i18n::ta_in(
        lang,
        key,
        &[
            ("n", &files.to_string()),
            (
                "files",
                &count("search-panel-files-one", "search-panel-files", files),
            ),
            (
                "folders",
                &count("search-panel-folders-one", "search-panel-folders", folders),
            ),
        ],
    )
}

/// A key while the panel has the keyboard.
///
/// With an input focused the keys are FIXED, like the `Alt+F7` dialog's
/// (`jobs/search.rs`): a printable key is text, so it cannot go through a
/// keymap where `j` or `/` mean something. In the results they resolve
/// through the `dialog` keymap, like the timeline's.
pub async fn on_key(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    work: &mut InFlight,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    // F9 is a FIXED key like F2–F8, from an input and from the results.
    if mods.is_empty() && code == KeyCode::F(9) {
        open_listing(app, work);
        app.needs_frame = true;
        return;
    }
    let mut activation = None;
    match app.search_panel_focus {
        SearchFocus::Field(field) => on_input_key(app, work, field, mods, code),
        SearchFocus::Tree => activation = on_tree_key(app, resolver, mods, code),
    }
    app.needs_frame = true;
    carry_out(app, backend, events, work, activation).await;
}

/// Activates the cursor's row as Enter in the results does: the mouse's
/// press on the row that was already selected.
pub async fn activate(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    work: &mut InFlight,
) {
    let activation = panel_mut(app).and_then(SearchPanel::activate);
    app.needs_frame = true;
    carry_out(app, backend, events, work, activation).await;
}

/// Does what activating a row asked for.
async fn carry_out(
    app: &mut App,
    backend: &Backend,
    events: &mut Console<'_>,
    work: &mut InFlight,
    activation: Option<Activation>,
) {
    match activation {
        // A folder folded or unfolded: only a repaint.
        None | Some(Activation::Toggled) => {}
        // The focused listing lands on the entry, the way a compare row
        // does, and the keys go back to the panes.
        Some(Activation::GoTo { dir, entry }) => {
            app.return_keys_to_panes();
            app.focused_mut().set_pending_focus(entry);
            let outcome = cd(app, backend, events, dir).await;
            if matches!(outcome, Cd::Failed(_)) {
                app.focused_mut().clear_pending_focus();
            }
            settle_cd(
                app,
                backend,
                &mut work.fill,
                &mut work.decorate,
                &mut work.probed,
                &mut work.search,
                outcome,
            );
        }
        // The viewer opens over the panel; closing it lands back here.
        Some(Activation::View { path, target }) => {
            open_viewer_at(app, backend, events, path, target).await;
        }
    }
}

/// A key with the caret in `field`.
fn on_input_key(
    app: &mut App,
    work: &mut InFlight,
    field: SearchField,
    mods: KeyModifiers,
    code: KeyCode,
) {
    // SHIFT passes through (uppercase arrives as Char+SHIFT); ctrl/alt do
    // not write, same as the dialog.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    let root = app.focused().dir().clone();
    let Some(p) = panel_mut(app) else {
        return;
    };
    let open = p.filters_open();
    let mut focus = SearchFocus::Field(field);
    let toggle = |p: &mut SearchPanel, id: &str| {
        p.toggle(id, now());
    };
    match code {
        KeyCode::Char(c) if plain => p.push_char(field, c, now()),
        KeyCode::Backspace if plain => p.backspace(field, now()),
        KeyCode::F(2) if plain => toggle(p, ID_REGEX),
        KeyCode::F(3) if plain => toggle(p, ID_CASE),
        KeyCode::F(4) if plain => toggle(p, ID_WHOLE_WORD),
        KeyCode::F(5) if plain => toggle(p, ID_RECURSIVE),
        KeyCode::F(6) if plain => toggle(p, ID_KINDS),
        // Search here: the panel moves to the listing's folder, and what
        // runs under the old one stops.
        KeyCode::F(7) if plain => {
            p.stop();
            p.set_root(root, now());
            cancel(&mut work.search_panel);
        }
        KeyCode::F(8) if plain => {
            p.toggle_filters();
            // Folding the filters with the caret in one of them leaves it
            // in the last input still painted.
            if !p.filters_open() && !inputs(false).contains(&field) {
                focus = SearchFocus::Field(SearchField::Content);
            }
        }
        KeyCode::Tab | KeyCode::Down => focus = next_stop(focus, open),
        KeyCode::BackTab => focus = prev_stop(focus, open),
        KeyCode::Up if field != SearchField::Name => focus = prev_stop(focus, open),
        KeyCode::Enter => p.force(now()),
        KeyCode::Esc => app.return_keys_to_panes(),
        _ => {}
    }
    app.search_panel_focus = focus;
}

/// A key in the results, through the `dialog` keymap. ←/→ and Tab are
/// matched first: the `dialog` screen gives the arrows no command, and Tab
/// must leave the results whatever the preset calls it.
///
/// Returns what Enter asked for, when it asked for something to do.
fn on_tree_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<Activation> {
    if mods.is_empty() {
        match code {
            KeyCode::Left | KeyCode::Right => {
                if let Some(p) = panel_mut(app) {
                    p.fold(code == KeyCode::Right);
                }
                return None;
            }
            KeyCode::Tab => {
                app.search_panel_focus = SearchFocus::Field(SearchField::Name);
                return None;
            }
            _ => {}
        }
    }
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    match cmd.as_str() {
        "dialog.cancel" => app.return_keys_to_panes(),
        "dialog.pane" => app.search_panel_focus = SearchFocus::Field(SearchField::Name),
        _ => {
            let p = panel_mut(app)?;
            match cmd.as_str() {
                "dialog.up" => p.up(1),
                "dialog.down" => p.down(1),
                "dialog.page-up" => p.up(PAGE),
                "dialog.page-down" => p.down(PAGE),
                "dialog.top" => p.home(),
                "dialog.bottom" => p.end(),
                // What it asks for needs the listing or the viewer: the
                // caller does it.
                "dialog.confirm" => return p.activate(),
                _ => {}
            }
        }
    }
    None
}

/// Open as listing (F9): the panel's files become a results tab next to
/// the focused listing, and the keys go back to the panes.
///
/// C9: a LIVE run is HANDED to the tab — it leaves `work.search_panel` and
/// is filed under the tab's slot with `panel_epoch`, so the panel's next
/// query launches a new run without cancelling it, and the tab keeps
/// receiving to the end. Meanwhile the tab's drain forwards each batch to
/// the panel, which takes it while it still shows that epoch
/// ([`crate::jobs::drain_search`]). The tab opened from the panel keeps the
/// panel's cap ([`search_panel::MAX_FILES`]) until its first refresh.
///
/// With nothing shown — an empty tree, or a new query launched whose first
/// answer has not replaced the old one — it says so and opens nothing.
pub fn open_listing(app: &mut App, work: &mut InFlight) {
    let Some(l) = panel_mut(app).and_then(|p| p.listing()) else {
        app.message = Some(t("search-panel-nothing-to-list"));
        return;
    };
    let mut set = ResultSet::new(l.search, search_panel::MAX_FILES);
    // The hits the panel already received count toward the cap, as if the
    // tab had received them: a run stopped at the cap still reads so.
    set.note_hits(&SearchHits {
        task_id: norte_proto::TaskId::new(0),
        entries: l.entries.clone(),
        matches: None,
        all_matches: None,
        more: None,
    });
    set.seed_matches(l.matches);
    let adopt = l
        .live
        .filter(|e| work.search_panel.as_ref().is_some_and(|r| r.epoch == *e));
    match (adopt, l.live) {
        (Some(_), _) => {}
        (None, None) => set.set_outcome(l.outcome),
        // Live in the panel but its run is not the panel's to hand over
        // (a second F9: the first tab has it). This tab is a snapshot that
        // receives nothing more, so it reads as stopped, not as running
        // forever; a refresh runs it again.
        (None, Some(_)) => set.set_outcome(Outcome::Cancelled),
    }
    let slot = app.open_results_tab(set, l.entries);
    if let Some(e) = adopt
        && let Some(PanelRun { task, rx, .. }) = work.search_panel.take()
        && let Some(old) = work.search.insert(
            slot,
            SearchRun {
                task,
                rx,
                outcome: Outcome::Running,
                panel_epoch: Some(e),
            },
        )
    {
        old.task.cancel();
    }
    app.return_keys_to_panes();
}

/// Once per loop turn: when the pause is over, cancels what runs and
/// launches the new search; a run whose panel is gone is cancelled.
///
/// A run the panel handed to a results tab (C9) is not the panel's to
/// cancel. When that tab's run ends without telling the panel — the tab
/// closed, turned into a folder, refreshed or Esc'd — the panel stops
/// saying it is searching: nobody feeds it any more.
pub async fn tick(app: &mut App, backend: &Backend, work: &mut InFlight) {
    let Some(p) = panel_mut(app) else {
        // Closed by any path — its command, closing the slot, a layout —
        // the run it left goes with it.
        cancel(&mut work.search_panel);
        return;
    };
    if p.is_running() {
        let e = p.epoch();
        let fed = work.search_panel.as_ref().is_some_and(|r| r.epoch == e)
            || work
                .search
                .iter()
                .any(|(_, s)| s.panel_epoch == Some(e) && s.outcome == Outcome::Running);
        if !fed {
            p.stop();
            app.needs_frame = true;
        }
    }
    let Some(p) = panel_mut(app) else {
        return;
    };
    let Some(due) = p.due(now(), now_ms()) else {
        return;
    };
    // A quiet tick skips the frame otherwise, and the panel just changed.
    app.needs_frame = true;
    match due {
        Due::Launch { epoch, params } => {
            // Cancel-then-launch: the old walk stops before the new one
            // starts, so two never race over the same tree.
            cancel(&mut work.search_panel);
            match backend.search(*params).await {
                Ok((task, rx)) => work.search_panel = Some(PanelRun { epoch, task, rx }),
                Err(e) => {
                    if let Some(p) = panel_mut(app) {
                        p.finished(epoch, Outcome::Failed(detail_for_bar(&error_category(&e))));
                    }
                }
            }
        }
        Due::Clear => cancel(&mut work.search_panel),
        // Only dimmed: what runs keeps landing on what is shown.
        Due::Unreadable(_) => {}
    }
}

/// A batch of the panel's run, or its end (`None`): the end reads how the
/// task finished and drops the run.
pub fn drain(app: &mut App, run: &mut Option<PanelRun>, hits: Option<SearchHits>) {
    let Some(r) = run.as_ref() else {
        return;
    };
    let epoch = r.epoch;
    // A channel that closed before the terminal state was published (a
    // race) reads as `Done`: the walker sends no more.
    let outcome = hits.is_none().then(|| {
        let state = r.task.progress().borrow().state.clone();
        outcome_of(&state, |e| detail_for_bar(&error_category(e))).unwrap_or(Outcome::Done)
    });
    let Some(p) = panel_mut(app) else {
        cancel(run);
        return;
    };
    match (hits, outcome) {
        (Some(batch), _) => {
            p.apply_hits(epoch, &batch);
        }
        (None, outcome) => {
            p.finished(epoch, outcome.unwrap_or(Outcome::Done));
            *run = None;
        }
    }
    app.needs_frame = true;
}

/// Stops the run in flight, if any: the panel closing, searching somewhere
/// else, or a new query replacing it.
pub fn cancel(run: &mut Option<PanelRun>) {
    if let Some(r) = run.take() {
        r.task.cancel();
    }
}

/// Closes the panel and cancels its run.
pub fn close(app: &mut App, work: &mut InFlight) {
    app.close_search_panel();
    cancel(&mut work.search_panel);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tab's ring: the filters join it only while unfolded.
    #[test]
    fn the_ring_of_stops() {
        let f = SearchFocus::Field;
        assert_eq!(next_stop(f(SearchField::Content), false), SearchFocus::Tree);
        assert_eq!(
            next_stop(f(SearchField::Content), true),
            f(SearchField::Exclude)
        );
        assert_eq!(next_stop(f(SearchField::Encoding), true), SearchFocus::Tree);
        assert_eq!(next_stop(SearchFocus::Tree, true), f(SearchField::Name));
        assert_eq!(prev_stop(SearchFocus::Tree, false), f(SearchField::Content));
        assert_eq!(prev_stop(SearchFocus::Tree, true), f(SearchField::Encoding));
        assert_eq!(prev_stop(f(SearchField::Name), false), SearchFocus::Tree);
    }
}
