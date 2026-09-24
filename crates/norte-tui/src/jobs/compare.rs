//! Compare two directories, and the panel that shows the rows as they arrive.

use crossterm::event::KeyCode;
use norte_core::backend::{Backend, TaskRef};
use norte_frontend::layout::BySlot;
use norte_i18n::ta;

use super::search::SearchRun;
use crate::app::{App, CompareState, detail_for_bar, error_category, error_message};
use crate::fill::Fill;
use crate::navigate::{apply_cd, cd};
use crate::probes::{DecorateFetch, Probed};

/// A directory comparison IN PROGRESS (`Shift+F2`,
/// 2026-08-11-directory-comparison.md): the cancelable Task and the channel
/// of row batches. Same mold as [`SearchRun`] — it lives in the run loop,
/// gets drained in the `select!` and gets dropped when the panel closes,
/// cancelling the Task (rule 3).
pub struct CompareRun {
    /// `fs.compare` Task (cancelable with `TaskRef::cancel`).
    pub task: TaskRef,
    /// Channel of row batches (embedded: closed by the walk; remote: the
    /// `RemoteBackend`'s pump closes it at the end).
    pub rx: tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
    /// Rows RECEIVED. Its own counter and not `pane.len()`, for the same
    /// reason as in search: not depending on what the model does with them.
    pub rows: usize,
    /// The run's state; terminal once the channel closes.
    pub state: CompareState,
}

/// Launches `fs.compare` over the two panes and opens the diff panel.
///
/// The params already arrive resolved and validated by
/// [`App::request_compare`](crate::app::App::request_compare) — this side
/// only owns the channel and the Task. A previous panel gets replaced and
/// its Task cancelled (rule 3): two comparisons at once would be two streams
/// feeding a single panel.
pub async fn launch_compare(
    app: &mut App,
    backend: &Backend,
    compare_run: &mut Option<CompareRun>,
    params: norte_proto::methods::FsCompareParams,
) {
    let (left_root, right_root) = (params.left.clone(), params.right.clone());
    match backend.compare(params).await {
        Ok((task, rx)) => {
            app.message = None;
            // The two re-interpretations (#57) come from the two panes the
            // roots came from, in that same order.
            let left = app.focus();
            let (left_encoding, right_encoding) = (
                app.panes[left].name_encoding(),
                app.panes[left ^ 1].name_encoding(),
            );
            app.compare = Some(crate::app::CompareView::new(
                left_root,
                right_root,
                left,
                left_encoding,
                right_encoding,
            ));
            // #157: the cache of hydrated sizes and its dedup belong to THIS
            // comparison — a new one starts with nothing requested, just
            // like `last_probed` gets cleared with every new listing. And it
            // advances the generation (#198): a probe from the previous
            // comparison still in flight would land in these freshly
            // cleared tables without the mark.
            app.begin_compare_generation();
            if let Some(old) = compare_run.replace(CompareRun {
                task,
                rx,
                rows: 0,
                state: CompareState::Running,
            }) {
                old.task.cancel();
            }
        }
        // The panel does NOT open: an empty panel that says "failed" is
        // worse than the status-bar phrase, because it also has to be
        // closed. The category goes sanitized, never the provider's raw
        // error.
        Err(e) => {
            app.message = Some(ta(
                "compare-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Applies a batch of rows (or the channel closing) to the diff panel.
///
/// `None` = end of stream. And that's where the difference with search is,
/// which is what C6 discovered and the plan doesn't say: **the channel
/// closing does NOT mean all rows have arrived**. The row pump and the
/// terminal snapshot's are independent tasks — but the COUNT that decides
/// `Done` versus [`CompareState::Incomplete`] no longer lives here (#158),
/// and since the branch review (MAJOR-1) the whole MAPPING doesn't live here
/// either: the four arms are
/// [`norte_frontend::compare::CompareView::finish_from_task`], which the GUI
/// also calls. What's left on this side is the TRANSIENT status-bar notice,
/// which the GUI doesn't have.
pub fn drain_compare(
    app: &mut App,
    compare_run: &mut Option<CompareRun>,
    batch: Option<norte_proto::methods::CompareRowsBatch>,
) {
    let Some(c) = compare_run else {
        return;
    };
    if let Some(b) = batch {
        // The panel closed under the drainer: the run is dropped, cancelling
        // the Task, the same as search does on leaving virtual mode.
        let Some(view) = app.compare.as_mut() else {
            c.task.cancel();
            *compare_run = None;
            return;
        };
        c.rows += b.rows.len();
        view.pane.extend(b.rows);
    } else {
        let mut rx = c.task.progress();
        let snapshot = rx.borrow_and_update().clone();
        let expected = snapshot.entries_done;
        if let Some(view) = app.compare.as_mut() {
            // The whole mapping — the four arms — belongs to the model. What
            // remains here is the TRANSIENT status-bar notice, which is the
            // only thing this surface has that the GUI doesn't.
            let notice = view
                .finish_from_task(
                    &snapshot.state,
                    expected,
                    c.rows as u64,
                    norte_i18n::active(),
                )
                .map(error_message);
            c.state = view.state;
            if let Some(m) = notice {
                app.message = Some(m);
            }
        } else {
            // The panel already closed: there's nothing to paint, and the
            // only use of `c.state` is a `== Running` check (further down
            // here in `on_compare_key`, and in the run loop's `select!`) —
            // any terminal variant serves, so which one isn't decided again
            // (it was a FOURTH copy of the `Completed` vs `Incomplete`
            // count, and the only one nobody could see get it wrong).
            c.state = CompareState::Done;
        }
    }
}

/// How many rows a page moves in the diff panel.
///
/// A constant and not the frame's height: the panel's layout is decided by
/// `ui::draw` and doesn't yet return its geometry to the run loop the way
/// `pane_geometry` does for the panes. Ten rows is what a `PageDown` covers
/// in a medium-height pane, so the error is one of TRAVEL and not of
/// correctness — the cursor never leaves the list, because `move_by` clamps.
pub const COMPARE_PAGE_STEP: isize = 10;

/// What a key MEANS in the diff panel.
///
/// Separated from dispatch so it can be asserted without a backend or a
/// terminal: what can get it wrong here is the DECISION — and one of them,
/// exiting, is the difference between an overlay and a trap — not the `cd`
/// that comes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareKey {
    /// Nothing to do with this key.
    Ignore,
    /// Quit norte (`Ctrl+C`, as in all the other overlays).
    Quit,
    /// Request the Task's cancellation and stay: rows already received are
    /// kept.
    CancelTask,
    /// Close the panel, cancelling the Task if it's still alive.
    Close,
    /// Swap the active side.
    SwapSide,
    /// Move the cursor.
    Move(isize),
    /// To the start of what's visible.
    First,
    /// To the end of what's visible.
    Last,
    /// Toggle the filter for category `n` of `CATEGORIES`.
    Filter(usize),
    /// Go to where the row lives.
    Open,
    /// Mark or unmark the row under the cursor: what gets marked seeds the
    /// plan's `include`.
    Mark,
    /// Plan a sync in this mode.
    Sync(norte_proto::methods::SyncMode),
}

/// Translates a diff-panel key.
///
/// * `Ctrl+C` quits norte. This panel keeps the WHOLE keyboard and `ISIG` is
///   off in raw mode, so without this it would be the only norte screen
///   there's no way out of (review BLOCKER-1); the other nine overlays
///   handle it the same way.
/// * The first `Esc` over a live comparison cancels it and keeps its rows;
///   **any later `Esc` closes**, without looking at the Task's state.
///   Conditioning the close on a terminal state left the reader locked in
///   when the row channel never got to close — a downed daemon, a provider
///   hung on a dead NFS mount.
/// * Any other modifier doesn't paint anything: without that filter,
///   `Alt+1` would toggle a filter and `Alt+Tab` would swap sides.
#[must_use]
pub fn compare_key(
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
    running: bool,
    cancel_requested: bool,
) -> CompareKey {
    use crossterm::event::KeyModifiers as M;
    if mods.contains(M::CONTROL) {
        return if code == KeyCode::Char('c') {
            CompareKey::Quit
        } else {
            CompareKey::Ignore
        };
    }
    if mods.contains(M::ALT) {
        return CompareKey::Ignore;
    }
    match code {
        KeyCode::Esc => {
            if running && !cancel_requested {
                CompareKey::CancelTask
            } else {
                CompareKey::Close
            }
        }
        KeyCode::Tab => CompareKey::SwapSide,
        KeyCode::Up => CompareKey::Move(-1),
        KeyCode::Down => CompareKey::Move(1),
        KeyCode::PageUp => CompareKey::Move(-COMPARE_PAGE_STEP),
        KeyCode::PageDown => CompareKey::Move(COMPARE_PAGE_STEP),
        KeyCode::Home => CompareKey::First,
        KeyCode::End => CompareKey::Last,
        // The pattern's range is exactly `CATEGORIES`'s, so the index can't
        // go out of bounds.
        KeyCode::Char(c @ '1'..='5') => CompareKey::Filter(c as usize - '1' as usize),
        KeyCode::Enter => CompareKey::Open,
        KeyCode::Insert => CompareKey::Mark,
        // BARE LETTERS, and that's the decision (#159): under tmux no
        // function key with a modifier arrives, so a `Shift+F5` here would
        // be a documented and dead shortcut — this repo already shipped
        // one. Inside this panel the keyboard is entirely its own, so
        // there's nothing to collide with.
        KeyCode::Char('s') => CompareKey::Sync(norte_proto::methods::SyncMode::Update),
        KeyCode::Char('m') => CompareKey::Sync(norte_proto::methods::SyncMode::Mirror),
        _ => CompareKey::Ignore,
    }
}

/// Dispatches a diff-panel key (`Shift+F2`). Fixed, like the search dialog's:
/// there's no `dialog.*` vocabulary for "swap sides" or "hide the equal
/// ones". The meaning is decided by [`compare_key`]; this just executes it.
#[expect(clippy::too_many_arguments, reason = "diff-panel wiring, not API")]
pub async fn on_compare_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
) {
    use norte_frontend::compare::CATEGORIES;

    let Some(cancel_requested) = app.compare.as_ref().map(|v| v.cancel_requested) else {
        return;
    };
    let running = compare_run
        .as_ref()
        .is_some_and(|c| c.state == CompareState::Running);
    match compare_key(mods, code, running, cancel_requested) {
        CompareKey::Ignore => {}
        CompareKey::Quit => {
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.quit = true;
        }
        CompareKey::CancelTask => {
            if let Some(c) = compare_run.as_ref() {
                c.task.cancel();
            }
            if let Some(view) = app.compare.as_mut() {
                view.cancel_requested = true;
            }
        }
        CompareKey::Close => {
            // Dropping the `CompareRun` cancels nothing (`TaskRef` has no
            // `Drop`): remotely, the daemon would keep walking both whole
            // trees for a panel that no longer exists (review MAJOR-4). It
            // is ALWAYS cancelled on exit.
            if let Some(c) = compare_run.take() {
                c.task.cancel();
            }
            app.close_compare();
        }
        CompareKey::Open => {
            on_compare_enter(
                app,
                backend,
                events,
                fill,
                decorate_fetch,
                last_probed,
                search_run,
                compare_run,
            )
            .await;
        }
        // Outside the arm that borrows `view`: resolving the params needs
        // the WHOLE `App` (both roots, the backend's journal and the status
        // bar where it says no). It only RESOLVES; launching belongs to the
        // run loop, same as when opening this very panel.
        CompareKey::Sync(sync_mode) => {
            app.request_sync(sync_mode);
        }
        other => {
            let Some(view) = app.compare.as_mut() else {
                return;
            };
            match other {
                CompareKey::SwapSide => view.pane.swap_active_side(),
                CompareKey::Move(delta) => view.pane.move_by(delta),
                CompareKey::First => view.pane.select_first(),
                CompareKey::Last => view.pane.select_last(),
                CompareKey::Filter(i) => {
                    if let Some(cat) = CATEGORIES.get(i) {
                        view.pane.toggle_filter(*cat);
                    }
                }
                CompareKey::Mark => {
                    if let Some(id) = view.pane.selected_id() {
                        view.pane.toggle_mark(id);
                    }
                }
                _ => {}
            }
        }
    }
}

/// `Enter` on a diff-panel row: navigates to the ACTIVE side's REAL
/// directory and closes the panel.
///
/// An orphan the walk emitted as ONE row without enumerating its subtree
/// gets expanded this way, which is why the row carries the whole `Entry`
/// and not just a name. With nothing on the active side, it does NOT fall
/// back to the other one: it says so.
#[expect(clippy::too_many_arguments, reason = "diff-panel wiring, not API")]
pub async fn on_compare_enter(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    fill: &mut BySlot<Fill>,
    decorate_fetch: &mut BySlot<DecorateFetch>,
    last_probed: &mut Probed,
    search_run: &mut Option<SearchRun>,
    compare_run: &mut Option<CompareRun>,
) {
    let Some(view) = app.compare.as_ref() else {
        return;
    };
    if view.pane.target_entry().is_none() {
        let side = view.pane.active_side();
        // The active language, the same one `t`/`ta` resolve two lines from
        // here: passing a different one would give a half-translated
        // sentence.
        let side_word = norte_frontend::compare::side_label(side, norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    }
    // The MODEL decides which directory to go to (rule 7): the path itself
    // if the row is a directory, its parent if it's a file — the same rule
    // the GUI will need.
    let Some(dest) = view.pane.navigation_target() else {
        // There's an entry but nowhere to go: a file hanging off its
        // scheme's root has no parent. It's SAID, same as the case above —
        // an `Enter` that does nothing and doesn't explain why reads as the
        // key being broken. The GUI fixed it and it was missing here
        // (branch review, MAJOR-4).
        let side_word =
            norte_frontend::compare::side_label(view.pane.active_side(), norte_i18n::active());
        app.message = Some(ta("compare-no-target", &[("side", &side_word)]));
        return;
    };
    // And the cursor lands on the entry it left, byte-exact (the listing
    // consumes it on landing; if it no longer exists, it falls back to the
    // default). The GUI did this and this branch didn't, while its comment
    // claimed parity (branch review, MINOR-8).
    let focus = view.pane.target_path().cloned();
    // To the ACTIVE side's pane, and focus with it: always sending it to the
    // focused pane cost the reader the other directory just to come look at
    // this one.
    let dest_pane = app.compare_active_pane().unwrap_or_else(|| app.focus());
    if let Some(c) = compare_run.take() {
        c.task.cancel();
    }
    app.close_compare();
    app.set_focus(dest_pane);
    if let Some(p) = focus {
        app.panes[dest_pane].set_pending_focus(p);
    }
    let outcome = cd(app, backend, events, dest).await;
    apply_cd(
        &app.panes,
        fill,
        decorate_fetch,
        last_probed,
        search_run,
        outcome,
    );
}

/// The diff pane's run-loop wiring (`Shift+F2`,
/// 2026-08-11-directory-comparison.md). The MODEL is tested in
/// `norte-frontend`, without a terminal; what is pinned here is the part only
/// this crate can get wrong.
#[cfg(test)]
mod compare_tests {
    use super::{CompareRun, drain_compare, launch_compare};
    use crate::app::{App, CompareState, Pane};
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareRow, CompareRowsBatch, CompareVerdict,
    };

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("test wire")
    }

    fn app_at(left: &str, right: &str) -> App {
        App::new(
            Pane::new(vp(left), Vec::new()),
            Pane::new(vp(right), Vec::new()),
        )
    }

    /// A synthetic run whose task reports `state` and `entries_done`, plus the
    /// sender that stays alive so the channel is only closed on purpose.
    fn run_with(
        state: norte_proto::TaskState,
        entries_done: u64,
        received: usize,
    ) -> (
        CompareRun,
        tokio::sync::mpsc::Sender<CompareRowsBatch>,
        tokio::sync::watch::Sender<norte_proto::TaskProgress>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<CompareRowsBatch>(4);
        let id = norte_proto::TaskId::new(1);
        let (progress, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        (
            CompareRun {
                task: TaskRef::synthetic_for_tests(id, prx),
                rx,
                rows: received,
                state: CompareState::Running,
            },
            tx,
            progress,
        )
    }

    fn row(id: u64) -> CompareRow {
        CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(norte_proto::methods::CompareReason::Unreadable),
            side: None,
            paired_under: None,
        }
    }

    /// The panel that LAUNCHES is the left one, even if it's the screen's
    /// right pane. The spec says so and it isn't cosmetic: a reader who
    /// presses the key from the right expects their directory to be "theirs",
    /// and every `<`/`>` mark in the panel hangs off that choice.
    #[test]
    fn the_focused_pane_is_the_left_side() {
        let mut app = app_at("file:///a", "file:///b");
        app.switch_focus();
        app.request_compare();
        let params = app.pending_compare.expect("leaves pending params");
        assert_eq!(params.left, vp("file:///b"));
        assert_eq!(params.right, vp("file:///a"));
    }

    /// C6, finding 4: the daemon answers `InvalidPath` for two equal roots,
    /// and it's right — but the phrase doesn't depend on a round trip over
    /// the network, and no Task ever comes to exist.
    #[test]
    fn both_panes_in_the_same_place_are_rejected_here() {
        let mut app = app_at("file:///a", "file:///a");
        app.request_compare();
        assert!(app.pending_compare.is_none(), "the Task can't be requested");
        assert!(app.message.is_some(), "and the reader has to find out");
    }

    /// A list of hits isn't a directory: there's no root to send. Same
    /// refusal that mirror and pull already give over a virtual pane.
    #[test]
    fn a_virtual_pane_cannot_be_compared() {
        let mut app = app_at("file:///a", "file:///b");
        app.panes[1].begin_search(vp("file:///b"));
        app.request_compare();
        assert!(app.pending_compare.is_none());
        assert_eq!(app.message, Some(norte_i18n::t("msg-pane-not-a-location")));
    }

    /// C6, finding 3: there's no symlink toggle in the UI because the engine
    /// accepts the field and ignores it, so `Backend::compare` answers
    /// `Unsupported` for `true` before any Task exists. Requesting `true`
    /// from here would be requesting a promise nobody keeps.
    #[test]
    fn following_symlinks_is_never_requested() {
        let mut app = app_at("file:///a", "file:///b");
        app.request_compare();
        assert!(!app.pending_compare.expect("params").follow_symlinks);
    }

    /// The local copy of the wire's default can't drift from the wire.
    #[test]
    fn the_default_tolerance_follows_the_wire() {
        let from_wire: norte_proto::methods::FsCompareParams =
            serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#)
                .expect("minimal params");
        let mut app = app_at("file:///a", "file:///b");
        app.request_compare();
        assert_eq!(
            app.pending_compare.expect("params").mtime_tolerance_ms,
            from_wire.mtime_tolerance_ms
        );
    }

    /// **C6, finding 2 — the one the plan doesn't say.** The row channel
    /// closing does NOT mean all of them arrived: the two pumps are
    /// independent tasks. With the Task `Completed` counting MORE rows than
    /// received, the panel says `Incomplete` — saying "done" would be lying
    /// about how complete the answer is, which in a comparison is the whole
    /// answer.
    #[tokio::test]
    async fn a_lost_batch_is_reported_instead_of_passed_as_done() {
        let mut app = app_at("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        // The task counted 9 rows; 7 arrived.
        let (run, _tx, _p) = run_with(norte_proto::TaskState::Completed, 9, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("the panel is still open");
        assert_eq!(view.state, CompareState::Incomplete);
        assert_eq!(view.rows_expected, 9);
    }

    /// And the opposite case, which is the normal one: all counted rows
    /// arrived, so `Done` — without accusing anyone of losing anything.
    #[tokio::test]
    async fn when_all_arrive_it_is_done_not_incomplete() {
        let mut app = app_at("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_with(norte_proto::TaskState::Completed, 7, 7);
        let mut run = Some(run);
        drain_compare(&mut app, &mut run, None);
        assert_eq!(
            app.compare.expect("the panel").state,
            CompareState::Done,
            "a benign race can't read as a loss"
        );
    }

    /// Cancelling keeps what arrived: the comparison writes nothing, so rows
    /// already seen stay true.
    #[tokio::test]
    async fn cancelling_keeps_the_rows_that_arrived() {
        let mut app = app_at("file:///a", "file:///b");
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///a"),
            vp("file:///b"),
            0,
            None,
            None,
        ));
        let (run, _tx, _p) = run_with(norte_proto::TaskState::Cancelled, 2, 2);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![row(1), row(2)],
            }),
        );
        drain_compare(&mut app, &mut run, None);
        let view = app.compare.expect("the panel");
        assert_eq!(view.state, CompareState::Cancelled);
        assert_eq!(view.pane.len(), 2);
    }

    /// A batch that arrives with the panel ALREADY closed drops the run and
    /// cancels the Task (rule 3): without this, the drainer would stay alive
    /// feeding a panel that doesn't exist.
    #[tokio::test]
    async fn a_batch_with_the_panel_closed_harvests_the_run() {
        let mut app = app_at("file:///a", "file:///b");
        let (run, _tx, _p) = run_with(norte_proto::TaskState::Running, 0, 0);
        let mut run = Some(run);
        drain_compare(
            &mut app,
            &mut run,
            Some(CompareRowsBatch {
                task_id: norte_proto::TaskId::new(1),
                rows: vec![row(1)],
            }),
        );
        assert!(run.is_none(), "the run has to be dropped");
    }

    /// **What the tmux harness caught and no model assertion could see.**
    /// `Enter` on a row navigates to the pane on the ACTIVE side, not the
    /// FOCUSED pane: looking at the right side, Enter used to send the left
    /// pane — the one with focus — to see the right's directory, and the
    /// reader was left with both panes in the same place and their left one
    /// lost.
    ///
    /// It's pinned on `compare_active_pane` and not on the `cd`, which needs
    /// a backend and an `EventStream`: what can go wrong is the pane's
    /// CHOICE, and that's what this pins down.
    #[test]
    fn enter_goes_to_the_active_side_pane_not_the_focused_one() {
        let mut app = app_at("file:///a", "file:///b");
        // Launched from the RIGHT pane: the panel's left is panes[1].
        app.switch_focus();
        app.compare = Some(crate::app::CompareView::new(
            vp("file:///b"),
            vp("file:///a"),
            app.focus(),
            None,
            None,
        ));
        assert_eq!(app.compare_active_pane(), Some(1), "left side = panes[1]");
        app.compare
            .as_mut()
            .expect("the panel")
            .pane
            .swap_active_side();
        assert_eq!(
            app.compare_active_pane(),
            Some(0),
            "the panel's right side is the OTHER pane, whichever has focus"
        );
        // And with the panel closed there's no pane to choose.
        app.close_compare();
        assert_eq!(app.compare_active_pane(), None);
    }

    /// **Review BLOCKER-1.** The panel keeps the WHOLE keyboard and `ISIG` is
    /// off in raw mode, so if no key closes it unconditionally, the panel is
    /// a trap: the state only moves to terminal when the row channel closes,
    /// and there are ways for it to never close — a daemon that goes down
    /// publishes the failure on the watch without touching the row channel,
    /// a provider hung on a dead NFS mount doesn't check its token until the
    /// syscall returns. Both exits, asserted:
    #[test]
    fn the_diff_panel_can_always_be_exited() {
        use crossterm::event::KeyModifiers as M;

        // Ctrl+C quits norte, with the Task alive or dead, same as the other
        // nine overlays.
        for alive in [true, false] {
            assert_eq!(
                super::compare_key(
                    M::CONTROL,
                    crossterm::event::KeyCode::Char('c'),
                    alive,
                    false
                ),
                super::CompareKey::Quit,
                "alive={alive}"
            );
        }
        // First Esc with the Task alive: cancels and KEEPS the rows.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, false),
            super::CompareKey::CancelTask
        );
        // The second closes it EVEN IF the Task still says it's running —
        // which is exactly the case where the channel never closes.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, true, true),
            super::CompareKey::Close
        );
        // And with the Task already terminal, the first Esc closes.
        assert_eq!(
            super::compare_key(M::NONE, crossterm::event::KeyCode::Esc, false, false),
            super::CompareKey::Close
        );
    }

    /// A modifier the panel doesn't use must not SLIP THROUGH as the bare
    /// key: without that filter, `Alt+1` would hide a category and
    /// `Alt+Tab` would swap sides (review MINOR).
    #[test]
    fn a_key_with_alt_does_nothing_in_the_panel() {
        use crossterm::event::{KeyCode as C, KeyModifiers as M};
        for code in [C::Tab, C::Char('1'), C::Enter, C::Down] {
            assert_eq!(
                super::compare_key(M::ALT, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
            // And with no modifier the SAME key does mean something: the
            // filter can't have eaten the normal case.
            assert_ne!(
                super::compare_key(M::NONE, code, false, false),
                super::CompareKey::Ignore,
                "{code:?}"
            );
        }
        // A Ctrl that isn't Ctrl+C doesn't either: it swallows the key, it
        // doesn't quit.
        assert_eq!(
            super::compare_key(M::CONTROL, C::Down, false, false),
            super::CompareKey::Ignore
        );
    }

    /// A failure requesting the Task does NOT open the panel: an empty panel
    /// that says "failed" is worse than the status-bar phrase, because it
    /// also has to be closed. And the detail goes by category, never raw.
    #[tokio::test]
    async fn a_failed_launch_does_not_open_the_panel() {
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let mut app = app_at("file:///a", "file:///b");
        let mut run: Option<CompareRun> = None;
        // `follow_symlinks: true` is `Unsupported` before any Task exists
        // (C6, finding 3): the cheapest failure to trigger here.
        launch_compare(
            &mut app,
            &backend,
            &mut run,
            norte_proto::methods::FsCompareParams {
                left: vp("file:///a"),
                right: vp("file:///b"),
                criteria: norte_proto::methods::CompareCriteria::default(),
                max_depth: None,
                mtime_tolerance_ms: 2000,
                follow_symlinks: true,
                descend_orphans: None,
            },
        )
        .await;
        assert!(app.compare.is_none(), "the panel can't open empty");
        assert!(run.is_none());
        assert!(app.message.is_some());
    }
}
