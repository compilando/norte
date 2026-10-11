//! Search under a directory, and the VIRTUAL pane that shows the hits.
//!
//! Hits arrive in batches and accumulate in a pane that isn't a directory:
//! leaving it (a `cd`) drops the search and cancels the Task, which is why
//! [`super::super::navigate`] has to know about [`SearchRun`].

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::BySlot;
use norte_i18n::{t, ta};
use norte_proto::VPath;
use norte_proto::methods::{FsSearchParams, SearchHits};

use norte_frontend::results::{MAX_HITS, ResultSet, SavedSearch};
use norte_frontend::search_status::{Outcome, outcome_of};

use crate::app::{App, SearchDialog, detail_for_bar, error_category, error_message};
use crate::fill::Fill;
use crate::navigate::{cd, settle_cd};
use crate::probes::{DecorateFetch, Probed};

/// A live search IN PROGRESS (`Alt+F7`, liveSearch T6): the cancelable Task,
/// the channel of hit batches and the virtual pane that shows them. `Fill`
/// mold: it lives in the run loop, gets drained in the `select!` and gets
/// dropped on leaving virtual mode (a `cd`), cancelling the Task (rule 3).
pub struct SearchRun {
    /// `fs.search` Task (cancelable with `TaskRef::cancel`).
    pub task: TaskRef,
    /// Channel of hit batches (embedded: closed by the walker; remote: the
    /// `RemoteBackend`'s pump closes it at the end).
    pub rx: tokio::sync::mpsc::Receiver<SearchHits>,
    /// Pane showing the hits (index into `App::panes`).
    pub pane: usize,
    /// The pane's PREVIOUS directory, to restore it on leaving virtual mode
    /// (Esc after finishing).
    pub prev_dir: VPath,
    /// Accumulated hits (== `panes[pane].entries().len()`, its own counter so
    /// it doesn't depend on the pane's re-sort).
    pub hits: usize,
    /// The run's outcome: `Running` while the walker emits; terminal once
    /// the channel closes (read from `TaskProgress`).
    pub outcome: Outcome,
}

/// Translates a search dialog key (`Alt+F7`, liveSearch T6) into an effect
/// on `App::search_dialog`. Fixed keys like the rest of the overlays (#24);
/// `ctrl+c` keeps its global quit. Returns `Some(params)` ONLY when Enter
/// with some non-empty criterion must LAUNCH the search (the caller closes
/// the dialog and opens the virtual pane); Enter with no criterion warns and
/// continues.
pub fn on_search_dialog_key(
    app: &mut App,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<FsSearchParams> {
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
                let root = app.focused().dir().clone();
                return Some(search_params(app.search_dialog.as_ref()?, root));
            }
            // No criterion at all: no-op with a warning (a search with no
            // criterion makes no sense). The dialog stays open.
            app.message = Some(t("search-empty"));
        }
        _ => {}
    }
    None
}

/// Builds the dialog's [`FsSearchParams`].
///
/// The mapping itself belongs to the SHARED crate
/// ([`norte_frontend::search::params`]): the window asks for the same
/// search, and two mappings would drift apart silently. What's done here is
/// the only thing that belongs to this frontend — reading the CLOCK, because
/// "changed seven days ago" is counted from the instant Enter is pressed,
/// and a mapping that asked for the time on its own couldn't be tested
/// without waiting.
///
/// `max_hits` is fixed to the default cap ([`MAX_HITS`]) — the v1
/// dialog doesn't expose it.
#[must_use]
pub fn search_params(dialog: &SearchDialog, root: VPath) -> FsSearchParams {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    norte_frontend::search::params(dialog, root, now_ms, MAX_HITS)
}

/// Launches the search: `backend.search` → Err leaves the dialog open and
/// warns via the status bar (`search-status-failed`); Ok closes the dialog,
/// saves the previous dir, starts the virtual pane and registers the
/// [`SearchRun`] (cancelling a previous one — the virtual pane is one).
pub async fn launch_search(
    app: &mut App,
    backend: &Backend,
    fill: &mut BySlot<Fill>,
    search_run: &mut Option<SearchRun>,
    params: FsSearchParams,
) {
    let pane = app.focus();
    let root = params.root.clone();
    match backend.search(params).await {
        Ok((task, rx)) => {
            // `prev_dir` and `root` are the SAME directory: the root comes
            // from `app.focused().dir()` on the dialog's Enter. `back_target`
            // leans on that equality — a virtual pane's `dir()` is what it
            // leaves in the forward branch, and it's only honest because
            // it's the place the reader was. If the root can ever be typed
            // in, the trail needs `prev_dir`, not `dir()`.
            let prev_dir = app.panes[pane].dir().clone();
            // The query the dialog still holds travels with the results (a
            // missing dialog, in tests, saves an empty one).
            let saved = app.search_dialog.as_ref().map_or_else(
                || SavedSearch::from_form(&SearchDialog::new(), root.clone()),
                |d| SavedSearch::from_form(d, root.clone()),
            );
            app.search_dialog = None;
            app.message = None;
            app.panes[pane].begin_results(ResultSet::new(saved, MAX_HITS));
            // The pane becomes virtual: a paginated fill in flight for THIS
            // pane (dir still loading) would feed the real listing as hits
            // (review MAJOR T6) — it's dropped right away (belt-and-braces;
            // `apply_fill_msg` is the belt in case a batch arrives first).
            fill.remove(app.panes.slot_of(pane));
            // A previous run (rare: the dialog closes on launch) gets
            // cancelled.
            if let Some(old) = search_run.replace(SearchRun {
                task,
                rx,
                pane,
                prev_dir,
                hits: 0,
                outcome: Outcome::Running,
            }) {
                old.task.cancel();
            }
        }
        // Invalid criteria or another daemon/engine failure: the dialog
        // STAYS open (the user fixes it) and the detail goes sanitized to
        // the status bar (the error's category — never the raw pattern).
        Err(e) => {
            app.message = Some(ta(
                "search-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Applies a batch of hits (or the channel closing) to the virtual pane. A
/// batch arrives as long as the pane stays in virtual mode; if a `cd` turned
/// it off, the run is dropped (its drainer would feed a real listing),
/// cancelling the Task. `None` = end of stream: the terminal state is read
/// and reflected on the pane.
pub fn drain_search(app: &mut App, search_run: &mut Option<SearchRun>, hits: Option<SearchHits>) {
    let Some(s) = search_run else {
        return;
    };
    if let Some(batch) = hits {
        if let Some(results) = app.panes[s.pane].results_mut() {
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
            let n = batch.entries.len();
            app.panes[s.pane].extend_listing(batch.entries);
            s.hits += n;
        } else {
            s.task.cancel();
            *search_run = None;
        }
    } else {
        // Channel closed: the walker finished. Non-blocking terminal state.
        let outcome = finalize_outcome(s);
        s.outcome = outcome.clone();
        // The concrete detail goes to the status bar ONCE (error_message);
        // the result set keeps the CATEGORY (inside `Outcome::Failed`) to
        // paint `search-status-failed` persistently after the message
        // clears (review MINOR-2).
        if matches!(outcome, Outcome::Failed(_)) {
            let mut rx = s.task.progress();
            if let norte_proto::TaskState::Failed { error } = rx.borrow_and_update().state.clone() {
                app.message = Some(error_message(&error));
            }
        }
        if let Some(results) = app.panes[s.pane].results_mut() {
            results.set_outcome(outcome);
        }
    }
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

/// Esc in the search virtual pane (liveSearch T6): with the Task alive, asks
/// for cancellation (hits already received stay; the state will move to
/// `Cancelled` when the channel closes); once finished, leaves virtual mode
/// restoring the previous dir with a normal `cd`.
pub async fn on_search_escape(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    let Some(s) = search_run.as_ref() else {
        return;
    };
    if s.outcome == Outcome::Running {
        s.task.cancel();
        return;
    }
    let prev = s.prev_dir.clone();
    *search_run = None;
    let outcome = cd(app, backend, events, prev).await;
    settle_cd(
        app,
        backend,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// Enter over a hit in the virtual pane (liveSearch T6): cd to the hit's
/// PARENT and leaves the cursor on it (by path, if it's already on the first
/// page). Cancels the Task if still alive and leaves virtual mode.
pub async fn on_search_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
) {
    let Some(hit) = app.focused().selected().map(|e| e.path.clone()) else {
        return;
    };
    let Some(parent) = hit.parent() else {
        return;
    };
    if let Some(s) = search_run.as_ref()
        && s.outcome == Outcome::Running
    {
        s.task.cancel();
    }
    *search_run = None;
    let pane = app.focus();
    let outcome = cd(app, backend, events, parent).await;
    settle_cd(
        app,
        backend,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
    // Re-anchors the cursor on the hit by path (the cd resets it to 0); if it
    // landed on a page not yet drained, the cursor stays at the top (v1).
    if let Some(i) = app.panes[pane].entries().iter().position(|e| e.path == hit) {
        app.panes[pane].set_cursor(i);
    }
}
