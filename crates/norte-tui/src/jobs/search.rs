//! Search under a directory (`Alt+F7`), and the RESULTS TAB that shows the
//! hits.
//!
//! Enter in the dialog opens a new tab next to the focused listing
//! ([`App::open_results_tab`]); the run that feeds it is filed under that
//! tab's SLOT ([`SearchRun`], in a `BySlot`), so it follows the tab through a
//! swap, keeps streaming while the tab is hidden behind another one, and
//! dies with it: closing the tab, or turning it into a folder, cancels the
//! Task on the next harvest ([`crate::refresh::reap_search_runs`]).

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::{BySlot, SlotId};
use norte_i18n::{t, ta};
use norte_proto::methods::SearchHits;

use norte_frontend::results::{MAX_HITS, ResultSet, SavedSearch};
use norte_frontend::search_status::{Outcome, outcome_of};

use crate::app::{App, detail_for_bar, error_category, error_message};
use crate::fill::Fill;
use crate::navigate::{Cd, cd, settle_cd};
use crate::probes::{DecorateFetch, Probed};

/// A results run (`Alt+F7`, or a refresh/restore of a results tab): the
/// cancelable Task and the channel of hit batches. Filed under the slot of
/// the tab it feeds (`InFlight::search`), in the `Fill` mold: drained in the
/// `select!`, and dropped — cancelling the Task, rule 3 — when that slot no
/// longer shows results.
pub struct SearchRun {
    /// `fs.search` Task (cancelable with `TaskRef::cancel`).
    pub task: TaskRef,
    /// Channel of hit batches (embedded: closed by the walker; remote: the
    /// `RemoteBackend`'s pump closes it at the end).
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// `Running` while the channel is open (the event loop polls only
    /// those); terminal once it closed ([`finalize_outcome`]).
    pub outcome: Outcome,
    /// The search panel's run this one came from (Task 6): its batches
    /// also go to the panel while the panel shows that epoch.
    pub panel_epoch: Option<u64>,
}

/// Translates a search dialog key (`Alt+F7`, liveSearch T6) into an effect
/// on `App::search_dialog`. Fixed keys like the rest of the overlays (#24);
/// `ctrl+c` keeps its global quit. Returns `Some(search)` ONLY when Enter
/// with some non-empty criterion must LAUNCH the search (the caller opens
/// the results tab); Enter with no criterion warns and continues.
///
/// The root is the focused listing's folder; from a results tab it is that
/// tab's search root (a results tab's `dir()` is that root too, said
/// through the set so the rule does not lean on it).
pub fn on_search_dialog_key(
    app: &mut App,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<SavedSearch> {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    let dialog = app.search_dialog.as_mut()?;
    // SHIFT passes through (uppercase/symbols arrive as Char+SHIFT); ctrl/alt
    // don't write into the fields.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    match code {
        // Toggles/Tab require `plain` (no ctrl/alt): a Ctrl+F2 doesn't toggle
        // (review MINOR-4), same as the rest of the dialog's capture.
        KeyCode::F(2) if plain => dialog.toggle_regex(),
        KeyCode::F(3) if plain => dialog.toggle_case(),
        KeyCode::F(4) if plain => dialog.toggle_whole_word(),
        KeyCode::F(5) if plain => dialog.toggle_recursive(),
        KeyCode::F(6) if plain => dialog.cycle_kinds(),
        KeyCode::Tab if plain => dialog.toggle_field(),
        KeyCode::Char(c) if plain => dialog.push_char(c),
        KeyCode::Backspace if plain => dialog.backspace(),
        KeyCode::Esc => app.search_dialog = None,
        KeyCode::Enter => {
            // A field that doesn't parse STOPS things BEFORE launching and
            // moves focus to it (0.81.0). Launching while ignoring it would
            // return the whole tree, and that reads just like a result: it's
            // the same trap the version warning avoids against an old
            // daemon.
            if let Some(field) = dialog.field_unreadable() {
                dialog.field = field;
                app.message = Some(t("search-bad-field"));
                return None;
            }
            if dialog.has_criteria() {
                let focused = app.focused();
                let root = focused
                    .results()
                    .map_or_else(|| focused.dir().clone(), |r| r.search().root.clone());
                return Some(SavedSearch::from_form(app.search_dialog.as_ref()?, root));
            }
            // No criterion at all: no-op with a warning (a search with no
            // criterion makes no sense). The dialog stays open.
            app.message = Some(t("search-empty"));
        }
        _ => {}
    }
    None
}

/// The wall clock in Unix milliseconds: "changed seven days ago" is counted
/// from the instant the search is launched ([`SavedSearch::params`]).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Launches `search` into a results tab and files its [`SearchRun`] under
/// that tab's slot, which it returns.
///
/// - `into: None` opens a NEW tab next to the focused listing, focused, and
///   closes the dialog. On error it opens nothing, leaves the dialog open
///   and says `search-status-failed` in the bar.
/// - `into: Some(slot)` re-runs that tab (refresh, restore): it starts over
///   empty under a fresh set capped at [`MAX_HITS`]. Any run already filed
///   for the slot is cancelled first. On error the tab's outcome becomes
///   `Failed`, so it never reads as "found nothing"; the hits it had stay,
///   the way a folder whose refresh fails keeps its listing (a restored tab
///   has none yet, so it shows the failure over an empty list).
pub async fn launch_results(
    app: &mut App,
    backend: &Backend,
    runs: &mut BySlot<SearchRun>,
    search: SavedSearch,
    into: Option<SlotId>,
) -> Option<SlotId> {
    if let Some(slot) = into
        && let Some(old) = runs.remove(slot)
    {
        old.task.cancel();
    }
    match backend.search(search.params(now_ms())).await {
        Ok((task, rx)) => {
            let set = ResultSet::new(search, MAX_HITS);
            let slot = match into {
                None => {
                    app.search_dialog = None;
                    app.message = None;
                    app.open_results_tab(set, Vec::new())
                }
                Some(slot) => {
                    let Some(pane) = app.panes.browser_mut(slot) else {
                        // The tab went away while the request was in
                        // flight: nobody to feed.
                        task.cancel();
                        return None;
                    };
                    pane.begin_results(set);
                    slot
                }
            };
            if let Some(old) = runs.insert(
                slot,
                SearchRun {
                    task,
                    rx,
                    outcome: Outcome::Running,
                    panel_epoch: None,
                },
            ) {
                old.task.cancel();
            }
            Some(slot)
        }
        // Invalid criteria or another daemon/engine failure: the detail goes
        // sanitized to the status bar (the error's category — never the raw
        // pattern).
        Err(e) => {
            fail_results(app, into, &e);
            None
        }
    }
}

/// A results run that could not start: `search-status-failed` in the bar
/// and, for a re-run, on the tab itself (its outcome keeps the category).
fn fail_results(app: &mut App, into: Option<SlotId>, e: &norte_proto::Error) {
    let category = error_category(e);
    app.message = Some(ta(
        "search-status-failed",
        &[("error", &detail_for_bar(&category))],
    ));
    if let Some(slot) = into
        && let Some(set) = app
            .panes
            .browser_mut(slot)
            .and_then(crate::app::Pane::results_mut)
    {
        set.set_outcome(Outcome::Failed(category));
    }
}

/// Re-runs the query of the results tab in `slot` (C2: `pane.refresh` on a
/// results tab), through [`launch_results`] with `into: Some(slot)`.
///
/// Only while the slot is still a results tab in the layout: between the
/// request and this call the tab may have been closed or turned into a
/// folder, and re-running then would turn a folder back into results.
///
/// The ROOT is stat'ed first, and a root that cannot be reached fails the
/// tab with `search-status-failed` (Review Focus 5): the core's walk counts
/// an unreadable root as one more skipped entry and ends `Completed` with no
/// hits, which on a tab restored over a folder that is gone would read as
/// "the search found nothing". Any run already filed for the slot is
/// cancelled, as [`launch_results`] would have.
pub async fn rerun_results(
    app: &mut App,
    backend: &Backend,
    runs: &mut BySlot<SearchRun>,
    slot: SlotId,
) {
    if app.layout.kind_of(slot).is_none() {
        return;
    }
    let Some(search) = app
        .panes
        .browser(slot)
        .and_then(crate::app::Pane::results)
        .map(|r| r.search().clone())
    else {
        return;
    };
    if let Err(e) = backend.stat(&search.root).await {
        if let Some(old) = runs.remove(slot) {
            old.task.cancel();
        }
        fail_results(app, Some(slot), &e);
        return;
    }
    let _ = launch_results(app, backend, runs, search, Some(slot)).await;
}

/// Re-runs every results tab a session just restored
/// ([`crate::app::SessionUi::rerun`]), visible or not (C7): a results tab
/// has no other content, so one left cold would read as a search that found
/// nothing. Drained in the turn header, so start-up, a profile switch and a
/// handoff all go through it.
pub async fn rerun_restored(app: &mut App, backend: &Backend, runs: &mut BySlot<SearchRun>) {
    for slot in std::mem::take(&mut app.session.rerun) {
        rerun_results(app, backend, runs, slot).await;
    }
}

/// Applies a batch of hits (or the channel closing) to `slot`'s results
/// tab, visible or not. If the slot no longer shows results (closed, or
/// turned into a folder) before the harvest saw it, the run is dropped and
/// its Task cancelled: a late batch never lands in a folder listing. `None`
/// = end of stream: the terminal state is read and kept on the run and on
/// the tab.
pub fn drain_search(
    app: &mut App,
    runs: &mut BySlot<SearchRun>,
    slot: SlotId,
    hits: Option<SearchHits>,
) {
    let Some(s) = runs.get_mut(slot) else {
        return;
    };
    if let Some(batch) = hits {
        // In the tree AND results: a closed tab's listing may linger in the
        // store as an orphan, and feeding it would keep a dead run going.
        let in_tree = app.layout.kind_of(slot).is_some();
        let Some(pane) = app
            .panes
            .browser_mut(slot)
            .filter(|p| in_tree && p.is_results())
        else {
            s.task.cancel();
            runs.remove(slot);
            return;
        };
        let panel_epoch = s.panel_epoch;
        if let Some(results) = pane.results_mut() {
            // #81: the match's context (line + preview, sanitized AT THE
            // SOURCE by the core) is saved by path in the result set — the
            // status bar paints it for the hit under the cursor.
            // Wire contract: aligned 1:1. A server bug sending fewer
            // matches would truncate the zip SILENTLY — noisy in dev.
            debug_assert!(
                batch
                    .matches
                    .as_ref()
                    .is_none_or(|m| m.len() == batch.entries.len()),
                "matches out of alignment"
            );
            results.note_hits(&batch);
        }
        if let Some(e) = panel_epoch
            && let Some(p) = watching_panel(app, e)
        {
            p.apply_hits(e, &batch);
        }
        if let Some(pane) = app.panes.browser_mut(slot) {
            pane.extend_listing(batch.entries);
        }
        return;
    }
    // Channel closed: the walker finished. Non-blocking terminal state.
    let outcome = finalize_outcome(s);
    s.outcome = outcome.clone();
    if let Some(e) = s.panel_epoch
        && let Some(p) = watching_panel(app, e)
    {
        // The panel words a failure as its own drain does: masked for the bar.
        let said = match &outcome {
            Outcome::Failed(category) => Outcome::Failed(detail_for_bar(category)),
            other => other.clone(),
        };
        p.finished(e, said);
    }
    // The concrete detail goes to the status bar ONCE (error_message); the
    // result set keeps the CATEGORY (inside `Outcome::Failed`) to paint
    // `search-status-failed` persistently after the message clears (review
    // MINOR-2).
    if matches!(outcome, Outcome::Failed(_)) {
        let mut rx = s.task.progress();
        if let norte_proto::TaskState::Failed { error } = rx.borrow_and_update().state.clone() {
            app.message = Some(error_message(&error));
        }
    }
    if let Some(results) = app
        .panes
        .browser_mut(slot)
        .and_then(crate::app::Pane::results_mut)
    {
        results.set_outcome(outcome);
    }
}

/// The search panel, while it still watches the run of `epoch` that it
/// handed to a results tab (C9): it still shows that query and has not
/// been stopped ("search here"). Once its epoch moved on, the model would
/// ignore the batch anyway.
fn watching_panel(
    app: &mut App,
    epoch: u64,
) -> Option<&mut norte_frontend::search_panel::SearchPanel> {
    let slot = app.search_panel_slot()?;
    app.panes
        .search_panel_mut(slot)
        .filter(|p| p.epoch() == epoch && p.is_running())
}

/// Reads a [`SearchRun`]'s terminal state from `TaskProgress` (non-blocking)
/// and maps it to an [`Outcome`] (the cap, `Truncated` in the old terms, is
/// the result set's `at_cap`, read when the status is painted). A channel
/// closed without a terminal state published yet (a race) is treated as
/// `Done` (the walker no longer emits).
pub fn finalize_outcome(s: &SearchRun) -> Outcome {
    let mut rx = s.task.progress();
    outcome_of(&rx.borrow_and_update().state, error_category).unwrap_or(Outcome::Done)
}

/// Enter over a hit in a results tab (C10): opens the hit's FOLDER in a
/// NEW tab next to it, with the cursor on the hit; the results tab stays,
/// and so does its run.
///
/// The cursor lands through the pane's pending focus, resolved when the
/// listing arrives — not an index set afterwards, which would miss a hit on
/// a page still filling. A folder that cannot be listed closes the empty
/// tab again (the error is already in the bar), and so does Esc during a
/// slow listing: an empty tab at the folder would read as an empty folder.
pub async fn open_hit_folder(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    runs: &mut BySlot<SearchRun>,
) {
    if !app.focused().is_results() {
        return;
    }
    let Some(hit) = app.focused().selected().map(|e| e.path.clone()) else {
        return;
    };
    let Some(parent) = hit.parent() else {
        return;
    };
    let _slot = app.tab_new_at(parent.clone());
    app.focused_mut().set_pending_focus(hit);
    let outcome = cd(app, backend, events, parent).await;
    let failed = matches!(outcome, Cd::Failed(_) | Cd::Cancelled);
    settle_cd(
        app,
        backend,
        fill,
        decorate_fetch,
        last_probed,
        runs,
        outcome,
    );
    if failed {
        app.focused_mut().clear_pending_focus();
        app.tab_close();
    }
}
