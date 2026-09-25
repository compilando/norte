//! Plan a sync and apply it.
//!
//! Two Tasks and not one: first the PLAN, which reads, and then the
//! APPLICATION, which writes. Between the two there is an explicit approval
//! — `a` and then `y` — because syncing deletes and overwrites.

use crossterm::event::KeyCode;
use norte_core::backend::{Backend, TaskRef};
use norte_i18n::{t, ta};

use super::compare::COMPARE_PAGE_STEP;
use crate::app::{App, detail_for_bar, error_category};

/// What just happened to a [`SyncRun`]'s live Task.
///
/// A single `select!` arm covers both phases of the dialog, so a type is
/// needed to say which of them spoke.
pub enum SyncTick {
    /// A plan event, or `None` = end of stream.
    Plan(Option<norte_core::sync::SyncPlanEvent>),
    /// The application Task changed state.
    Applied {
        /// Someone is still publishing its progress.
        ///
        /// `false` = ALL senders dropped. It's an end, not a tick: treating
        /// it as a tick with a last non-terminal state re-arms the arm on a
        /// future that's already ready and spins. Decided the same way as
        /// `TaskRef::join`, which synthesizes a failure for the same case
        /// instead of inheriting another crate's invariant.
        alive: bool,
    },
}

/// A sync IN PROGRESS (`Ctrl+Y`, 2026-08-11-directory-sync.md).
///
/// Covers the flow's TWO Tasks, one after the other, because they're a
/// single dialog for the reader: `sync.plan` (with its event channel) and,
/// if approved, `sync.apply` (no channel — what it did is requested with
/// `sync.report`).
pub struct SyncRun {
    /// The live Task, cancelable (rule 3).
    pub task: TaskRef,
    /// Plan event channel. `None` while the APPLICATION runs, which has no
    /// stream.
    pub rx: Option<tokio::sync::mpsc::Receiver<norte_core::sync::SyncPlanEvent>>,
    /// Task progress, to know when the application ended and request its
    /// report. Also checked when the plan's channel closes, same as in the
    /// comparison.
    pub progress: tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    /// The application is already running (`sync.apply`), not the plan.
    pub applying: bool,
}

/// Launches `sync.plan` and opens the sync panel.
///
/// The params already arrive resolved and validated by
/// [`App::request_sync`](crate::app::App::request_sync). A previous panel
/// gets replaced and its Task cancelled (rule 3): two plans at once would be
/// two streams feeding a dialog whose `plan_hash` is what gets approved.
pub async fn launch_sync_plan(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    params: norte_proto::methods::SyncPlanParams,
) {
    let (source, dest, mode) = (params.source.clone(), params.dest.clone(), params.mode);
    match backend.sync_plan(params).await {
        Ok((task, rx)) => {
            app.message = None;
            let progress = task.progress();
            let (source_encoding, dest_encoding) = app.pending_sync_encoding;
            app.sync = Some(crate::app::SyncView::new(
                task.id(),
                mode,
                source,
                dest,
                source_encoding,
                dest_encoding,
            ));
            if let Some(old) = sync_run.replace(SyncRun {
                task,
                rx: Some(rx),
                progress,
                applying: false,
            }) {
                old.task.cancel();
            }
        }
        // The panel does NOT open, for the same reason as the diff one: an
        // empty panel that says "failed" is worse than the status-bar
        // phrase. The category goes sanitized — and `OverlappingRoots`
        // arrives here with its relation, which is exactly the error this
        // path really produces.
        Err(e) => {
            app.message = Some(ta(
                "sync-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Launches `sync.apply` over the APPROVED plan.
///
/// The hash is the only thing that travels: there's no way to request
/// running something different from what the panel showed (ADR 0049). The
/// plan's Task already finished, so this `SyncRun` REPLACES it without
/// cancelling anything.
pub async fn launch_sync_apply(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    plan_hash: &norte_proto::methods::PlanHash,
) {
    match backend.sync_apply(plan_hash).await {
        Ok(task) => {
            let progress = task.progress();
            // And it CAN be refused: `on_apply_started` refuses a Task that
            // arrives after cancellation was already requested. The TUI
            // can't read a key between `sync_apply` and this line — it waits
            // for it inline — so it's unreachable today; the guard lives in
            // `norte-frontend` because it was in the GUI's wrapper and this
            // branch left it depending on control flow (branch review, rust
            // MAJOR-2). Whoever refuses it cancels it: nobody else knows
            // about it.
            let adopted = app
                .sync
                .as_mut()
                .is_some_and(|view| view.on_apply_started(task.id()));
            if !adopted {
                task.cancel();
                return;
            }
            // And ONTO THE BOARD (#173): the panel keeps the task — `Esc` is
            // still where an approved plan gets stopped from — and the board
            // keeps a `TaskObserver`, which paints and cancels without
            // owning. It wasn't there before because the board kept the
            // whole `TaskRef`, so the program's most destructive operation
            // was the only invisible one: with the panel closed, a `Mirror`
            // kept rewriting a subtree with no row, no progress and no way
            // to stop it.
            app.board.push_observed(task.observer(), None);
            // The plan's Task is ALWAYS cancelled when replaced: `Ready` is
            // reached on RECEIVING `sync.plan_done`, and its stream may not
            // have closed yet. `TaskRef` has no `Drop`, so dropping it
            // outright leaves the daemon walking two trees for a plan
            // already approved. Cancelling a finished Task does nothing.
            if let Some(previous) = sync_run.replace(SyncRun {
                task,
                rx: None,
                progress,
                applying: true,
            }) {
                previous.task.cancel();
            }
        }
        Err(e) => {
            if let Some(view) = app.sync.as_mut() {
                view.run = crate::app::SyncRunState::Failed;
                view.error = Some(detail_for_bar(&error_category(&e)));
            }
            app.message = Some(ta(
                "sync-status-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}

/// Applies a plan event (or the channel closing) to the panel.
///
/// `None` = end of stream. Unlike the comparison, there's NO count to square
/// against `entries_done` here: `sync.plan_done` is the signal, and its
/// absence is the protection — without it there's no `plan_hash` and nothing
/// to approve. What does happen is reading the terminal state, to tell a
/// cancelled plan from a failed one.
pub fn drain_sync_plan(
    app: &mut App,
    sync_run: &mut Option<SyncRun>,
    event: Option<norte_core::sync::SyncPlanEvent>,
) {
    let Some(run) = sync_run else {
        return;
    };
    let Some(view) = app.sync.as_mut() else {
        // The panel closed under the stream: cancel instead of continuing to
        // receive steps nobody is going to look at (same reason as in the
        // comparison — remotely, the daemon would keep walking the trees).
        run.task.cancel();
        *sync_run = None;
        return;
    };
    match event {
        Some(norte_core::sync::SyncPlanEvent::Steps(batch)) => {
            if !view.state.on_steps(batch) {
                // A batch from another plan, or one that arrives AFTER the
                // close — which is a protocol violation. The model drops it;
                // having it logged is what keeps it from hiding.
                tracing::warn!("sync.steps batch discarded: not from this plan");
            }
        }
        Some(norte_core::sync::SyncPlanEvent::Done(done)) => {
            if !view.state.on_plan_done(done) {
                tracing::warn!("sync.plan_done discarded: not from this plan");
            }
        }
        None => {
            let snapshot = run.progress.borrow_and_update().clone();
            // The `TaskState` → `SyncRunState` mapping belongs to
            // `crate::app::SyncRunState::from_task_state` (#161): localizing
            // the error that follows is the only half that truly differs
            // between frontends, and that's why it stays here.
            view.run = crate::app::SyncRunState::from_task_state(&snapshot.state);
            if let norte_proto::TaskState::Failed { error } = snapshot.state {
                let category = detail_for_bar(&error_category(&error));
                view.error = Some(category.clone());
                app.message = Some(ta("sync-status-failed", &[("error", &category)]));
            }
            // The channel ended: the `SyncRun` has nothing left to drain,
            // but it's kept so `Esc` can still cancel if the Task wasn't
            // terminal yet.
            run.rx = None;
        }
    }
}

/// Harvests the `sync.apply` Task once it reaches a terminal state and
/// requests its report.
///
/// The report is ALWAYS requested once the Task ends, cancellation included:
/// what was applied up to the cut stays, journalled, and half a sync is a
/// real state the reader has to be able to see.
pub async fn harvest_sync_apply(
    app: &mut App,
    backend: &Backend,
    sync_run: &mut Option<SyncRun>,
    alive: bool,
) {
    let Some(run) = sync_run.as_mut() else {
        return;
    };
    let snapshot = run.progress.borrow_and_update().clone();
    // With no senders left, nothing more is going to arrive, so a
    // non-terminal state here is all that's ever going to be known: it's
    // harvested anyway. Returning without harvesting would re-arm the arm on
    // a `changed()` that returns `Err` instantly.
    if alive && !snapshot.state.is_terminal() {
        return;
    }
    let task_id = run.task.id();
    *sync_run = None;
    let report = backend.sync_report(task_id).await;
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    // The THREE rules for this instant — the Task's error wins over the
    // report's, a report that doesn't arrive is a failure, and so is a
    // non-terminal state — belong to
    // `norte_frontend::sync::SyncView::on_apply_ended`, SHARED with the GUI
    // (#161). They used to be here, written by hand, with the second one
    // NOT applied: a `sync.report` that failed left the model in `Applying`
    // and the footer saying "applying..." forever, with the only
    // explanation in a transient status-bar message. What stays on this
    // side is the only half that truly differs between frontends: how the
    // category gets sanitized and where it gets painted.
    let category = view
        .on_apply_ended(&snapshot.state, report, norte_i18n::active())
        .map(|c| detail_for_bar(&c));
    view.error.clone_from(&category);
    if let Some(c) = category {
        app.message = Some(ta("sync-status-failed", &[("error", &c)]));
    }
}

/// What a key MEANS in the sync panel.
///
/// Separated from dispatch for the same reason as [`super::CompareKey`]:
/// what can get it wrong here is the DECISION, and one of them — approving —
/// writes to someone's disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKey {
    /// Nothing to do with this key.
    Ignore,
    /// Quit norte (`Ctrl+C`, as in all the other overlays).
    Quit,
    /// Request the Task's cancellation and stay.
    CancelTask,
    /// Close the panel, cancelling the Task if it's still alive.
    Close,
    /// Move the cursor over the steps.
    Move(isize),
    /// Approve: the FIRST answer. May open the second question.
    Approve,
    /// Answer "yes" to the second question.
    ConfirmYes,
    /// Any other key with the second question on screen: cancels it.
    ///
    /// It exists as its own variant and not as `Ignore` because a
    /// half-answered question has to be resolved: leaving it up while the
    /// cursor moves underneath is how a later `y` approves something else.
    ConfirmNo,
}

/// Translates a sync-panel key.
///
/// * `Ctrl+C` quits norte, like the other ten overlays.
/// * The first `Esc` over a live Task cancels it; any later `Esc` closes,
///   without looking at the Task's state — the same emergency exit as the
///   diff panel, for the same reason.
/// * With the second question on screen the keyboard shrinks to `y` and
///   "no": `Ctrl+C` and `Esc` still count (quitting and closing aren't
///   answers to the question), and EVERYTHING else cancels it instead of
///   being ignored.
/// * `Enter` does NOT approve. Syncing deletes and overwrites, so a key
///   nobody presses out of habit is required — the same criterion as the
///   TOFU dialogs and approving an agent op.
#[must_use]
pub fn sync_key(
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
    running: bool,
    cancel_requested: bool,
    confirming: bool,
) -> SyncKey {
    use crossterm::event::KeyModifiers as M;
    // The TWO exceptions, and only them: quitting norte and closing the
    // panel aren't answers to the question, so they count with the question
    // up.
    if mods.contains(M::CONTROL) && code == KeyCode::Char('c') {
        return SyncKey::Quit;
    }
    if code == KeyCode::Esc {
        return if running && !cancel_requested {
            SyncKey::CancelTask
        } else {
            SyncKey::Close
        };
    }
    // And the question is resolved BEFORE the modifier filters. With them in
    // front, a habitual `Ctrl+r` or `Alt+e` used to fall into `Ignore` and
    // left "2 trees are about to be deleted... Continue?" armed on screen,
    // waiting for a `y` that no longer knew what it was answering.
    if confirming {
        return if mods.is_empty() && code == KeyCode::Char('y') {
            SyncKey::ConfirmYes
        } else {
            SyncKey::ConfirmNo
        };
    }
    if mods.intersects(M::CONTROL | M::ALT) {
        return SyncKey::Ignore;
    }
    match code {
        KeyCode::Up => SyncKey::Move(-1),
        KeyCode::Down => SyncKey::Move(1),
        KeyCode::PageUp => SyncKey::Move(-COMPARE_PAGE_STEP),
        KeyCode::PageDown => SyncKey::Move(COMPARE_PAGE_STEP),
        KeyCode::Char('a') => SyncKey::Approve,
        _ => SyncKey::Ignore,
    }
}

/// Dispatches a sync-panel key. Fixed, like the diff panel's and for the
/// same reason: there's no `dialog.*` vocabulary for "approve this plan" and
/// it isn't a keymap screen of its own.
pub fn on_sync_key(
    app: &mut App,
    sync_run: &mut Option<SyncRun>,
    mods: crossterm::event::KeyModifiers,
    code: KeyCode,
) {
    let Some(view) = app.sync.as_ref() else {
        return;
    };
    let running = view.run == crate::app::SyncRunState::Running;
    let action = sync_key(
        mods,
        code,
        running,
        view.cancel_requested,
        view.confirming.is_some(),
    );
    match action {
        SyncKey::Ignore => {}
        SyncKey::Quit => {
            if let Some(s) = sync_run.take() {
                s.task.cancel();
            }
            app.quit = true;
        }
        SyncKey::CancelTask => {
            if let Some(s) = sync_run.as_ref() {
                s.task.cancel();
            }
            if let Some(view) = app.sync.as_mut() {
                view.cancel_requested = true;
                // The question falls with the Task that motivated it:
                // leaving it up is how a later `y` approves something else.
                view.confirming = None;
            }
        }
        SyncKey::Close => {
            // ALWAYS cancelled on exit, same as the diff panel: remotely,
            // the daemon would keep planning — or APPLYING — for a panel
            // that no longer exists.
            if let Some(s) = sync_run.take() {
                s.task.cancel();
            }
            app.close_sync();
        }
        SyncKey::Move(delta) => {
            if let Some(plan) = app.sync.as_mut().and_then(|v| v.state.plan_mut()) {
                plan.move_by(delta);
            }
        }
        SyncKey::Approve => approve_sync(app),
        SyncKey::ConfirmYes => {
            if let Some(view) = app.sync.as_mut() {
                view.confirming = None;
            }
            submit_sync(app);
        }
        SyncKey::ConfirmNo => {
            if let Some(view) = app.sync.as_mut() {
                view.confirming = None;
            }
        }
    }
}

/// `a` over a closed plan: either opens the second question, or sends it
/// right away.
///
/// The second question is decided by the MODEL
/// ([`norte_frontend::sync::SyncPlan::confirmation`]), which returns it only
/// when the plan deletes trees or when undo doesn't cover it whole. Asking
/// twice for an `Update` that undoes entirely teaches skipping both.
pub fn approve_sync(app: &mut App) {
    let lang = norte_i18n::active();
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    // `SyncView::can_approve` — which wraps `SyncState::can_approve` and
    // NEVER `SyncPlan::can_approve` — because the latter keeps answering yes
    // about a plan that's already been approved: `SyncState::plan()` returns
    // the same plan in `Applying` and in `Applied`, and none of its three
    // factors change when it's spent. With just the plan's check, an extra
    // `a` during a long application launched a second `sync.apply` that the
    // spool answers `PlanStale`, and the error arm painted "the plan failed"
    // over a sync that was still WRITING; the next `Esc` half-cancelled it
    // thinking it was closing a failure.
    if !view.can_approve() {
        app.message = Some(t("msg-sync-cannot-approve"));
        return;
    }
    let Some(plan) = view.state.plan() else {
        return;
    };
    match plan.confirmation(lang) {
        Some(c) => view.confirming = Some(c),
        None => submit_sync(app),
    }
}

/// Leaves the approved `plan_hash` ready for the run loop to apply.
///
/// Asks `can_approve` again: between the first answer and the second nothing
/// has arrived that could change it — the model doesn't go backward — but
/// the hash leaves here headed for a write, and there's no second gate after
/// this one.
pub fn submit_sync(app: &mut App) {
    // Through `SyncView::submit`, the ONE gate: it checks `can_approve` and
    // throws the in-flight apply's bolt in the same gesture. Splitting them
    // is what left the window the GUI DID reach (C2 branch review).
    let Some(view) = app.sync.as_mut() else {
        return;
    };
    let Some(hash) = view.submit() else {
        app.message = Some(t("msg-sync-cannot-approve"));
        return;
    };
    app.pending_sync_apply = Some(Box::new(hash));
}

/// The SYNC keys and params (2026-08-11-directory-sync.md).
///
/// No backend and no terminal: what can get it wrong here is the DECISION —
/// what gets synced, in which direction, and how many times it asks before
/// writing — and all of that is asserted over an `App` and a pure function.
#[cfg(test)]
mod sync_tests {
    use super::{
        SyncKey, SyncRun, SyncTick, approve_sync, drain_sync_plan, harvest_sync_apply, on_sync_key,
        sync_key,
    };
    use crate::app::{App, Pane};
    use crossterm::event::{KeyCode, KeyModifiers as M};
    use norte_core::backend::TaskRef;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, CompareRow, CompareVerdict, PlanHash, Side,
        SyncCounts, SyncMode, SyncPlanDone,
    };

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("test wire")
    }

    fn entry(wire: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind: norte_proto::EntryKind::File,
            size: Some(10),
            mtime_ms: None,
        }
    }

    /// A paired row, with an entry on both sides.
    fn paired(id: u64, name: &str) -> CompareRow {
        CompareRow {
            id,
            left: Some(entry(&format!("file:///home/{name}"))),
            right: Some(entry(&format!("file:///other/{name}"))),
            verdict: CompareVerdict::Different,
            criterion: CompareCriterion::Size,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    /// An `App` with a daemon (otherwise everything gets refused before
    /// looking at anything) and a diff panel with `n` paired rows.
    fn app_with_rows(n: u64) -> App {
        let mut app = App::new(
            Pane::new(vp("file:///home"), Vec::new()),
            Pane::new(vp("file:///other"), Vec::new()),
        );
        app.backend_journalled = true;
        let mut view =
            crate::app::CompareView::new(vp("file:///home"), vp("file:///other"), 0, None, None);
        view.pane
            .extend((1..=n).map(|i| paired(i, &format!("f{i}.txt"))));
        app.compare = Some(view);
        app
    }

    fn mark(app: &mut App, id: u64) {
        app.compare.as_mut().expect("panel").pane.toggle_mark(id);
    }

    /// What's marked in the diff panel is what gets synced, and it travels
    /// as `include` — paths RELATIVE to the root, which is what the core's
    /// filter compares.
    #[test]
    fn the_panel_seeds_include_with_whats_marked() {
        let mut app = app_with_rows(3);
        mark(&mut app, 1);
        mark(&mut app, 2);
        let params = app.request_sync(SyncMode::Update).expect("params");
        let include = params.include.as_ref().expect("include");
        assert_eq!(include.len(), 2);
        let wire: Vec<String> = include
            .iter()
            .map(norte_proto::methods::RelPath::to_wire)
            .collect();
        assert_eq!(wire, vec!["f1.txt".to_owned(), "f2.txt".to_owned()]);
    }

    /// With no marks the plan covers the WHOLE tree, and that's the ABSENCE
    /// of the field: an empty list would mean a zero-step plan.
    #[test]
    fn with_no_marks_the_plan_covers_the_whole_tree() {
        let mut app = app_with_rows(3);
        let params = app.request_sync(SyncMode::Update).expect("params");
        assert!(params.include.is_none());
    }

    /// The direction is decided by the ACTIVE SIDE, and isn't inferred from
    /// anything: `Tab` changes it and both roots swap entirely. It's half of
    /// what the reader approves.
    #[test]
    fn the_active_side_decides_the_direction_and_nothing_is_inferred() {
        let mut app = app_with_rows(1);
        let a = app.request_sync(SyncMode::Update).expect("params").clone();
        app.compare.as_mut().expect("panel").pane.swap_active_side();
        let b = app.request_sync(SyncMode::Update).expect("params").clone();
        assert_eq!(a.source, b.dest);
        assert_eq!(a.dest, b.source);
        assert_eq!(
            app.compare.as_ref().expect("panel").pane.active_side(),
            Side::Right
        );
    }

    /// The mode travels as is: `m` plans a mirror, which also DELETES.
    #[test]
    fn the_mode_travels_unchanged() {
        let mut app = app_with_rows(1);
        assert_eq!(
            app.request_sync(SyncMode::Mirror).expect("params").mode,
            SyncMode::Mirror
        );
    }

    /// The embedded TUI doesn't sync, and it SAYS so: `sync.apply` requires a
    /// journal and refuses without one (hard rule 4), so planning against it
    /// would show a plan nobody can approve. The phrase is actionable — it
    /// says to start with `--daemon` — not an "unsupported".
    #[test]
    fn with_no_journal_it_does_not_plan_and_says_how_to_fix_it() {
        let mut app = app_with_rows(1);
        app.backend_journalled = false;
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert!(app.pending_sync.is_none(), "nothing gets launched");
        let msg = app.message.clone().expect("a message");
        assert_eq!(msg, norte_i18n::t("msg-sync-needs-daemon"));
        assert_ne!(msg, norte_i18n::t("err-unsupported"));
    }

    /// Both roots in the same place get refused here, without a round trip
    /// to the daemon — same as when comparing.
    #[test]
    fn both_roots_in_the_same_place_are_refused_here() {
        let mut app = App::new(
            Pane::new(vp("file:///home"), Vec::new()),
            Pane::new(vp("file:///home"), Vec::new()),
        );
        app.backend_journalled = true;
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert!(app.message.is_some());
    }

    /// `Enter` does NOT approve. Syncing deletes and overwrites, so a key
    /// nobody presses out of habit is required — the same criterion as the
    /// TOFU dialogs and approving an agent op.
    #[test]
    fn enter_does_not_approve_a_sync() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Enter, false, false, false),
            SyncKey::Ignore
        );
        assert_eq!(
            sync_key(M::NONE, KeyCode::Char('a'), false, false, false),
            SyncKey::Approve
        );
    }

    /// The default key is NOT a function key with a modifier (#159: under
    /// tmux none arrives). Inside the diff panel they're bare letters.
    #[test]
    fn the_panels_keys_are_not_function_keys_with_a_modifier() {
        use crate::jobs::{CompareKey, compare_key};
        for (code, mode) in [
            (KeyCode::Char('s'), SyncMode::Update),
            (KeyCode::Char('m'), SyncMode::Mirror),
        ] {
            assert_eq!(
                compare_key(M::NONE, code, false, false),
                CompareKey::Sync(mode)
            );
        }
        // And with a modifier they're NOTHING: `Alt+s` can't sync by
        // accident from a panel whose keyboard is entirely its own.
        assert_eq!(
            compare_key(M::ALT, KeyCode::Char('s'), false, false),
            CompareKey::Ignore
        );
    }

    /// With the second question on screen the keyboard shrinks: `y` answers
    /// yes, `Esc` and `Ctrl+C` still count because they aren't answers to
    /// the question, and EVERYTHING else cancels it. Leaving it up while the
    /// cursor moves underneath is how a later `y` approves something else.
    #[test]
    fn the_second_question_shrinks_the_keyboard() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Char('y'), false, false, true),
            SyncKey::ConfirmYes
        );
        for code in [KeyCode::Down, KeyCode::Char('a'), KeyCode::Enter] {
            assert_eq!(
                sync_key(M::NONE, code, false, false, true),
                SyncKey::ConfirmNo,
                "{code:?} left the question half-answered"
            );
        }
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, false, false, true),
            SyncKey::Close
        );
        assert_eq!(
            sync_key(M::CONTROL, KeyCode::Char('c'), false, false, true),
            SyncKey::Quit
        );
    }

    /// The diff panel's emergency exit, here too: the first `Esc` over a
    /// live Task cancels it and ANY later `Esc` closes, without looking at
    /// the Task's state. Without this, a downed daemon leaves the reader
    /// locked in the screen from which writes get approved.
    #[test]
    fn the_second_esc_closes_no_matter_what() {
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, true, false, false),
            SyncKey::CancelTask
        );
        assert_eq!(
            sync_key(M::NONE, KeyCode::Esc, true, true, false),
            SyncKey::Close
        );
    }

    /// Every Fluent key this panel paints exists in BOTH locales. A missing
    /// one reaches the screen as its own id, and this panel is where what's
    /// about to be deleted gets read.
    #[test]
    fn every_panel_string_exists_in_both_locales() {
        for key in [
            "sync-title",
            "sync-mode-update",
            "sync-mode-mirror",
            "sync-planning",
            "sync-empty",
            "sync-status-cancelled",
            "sync-status-failed",
            "sync-status-ready",
            "sync-status-not-approvable",
            "sync-status-applying",
            "sync-status-applied-undoable",
            "sync-status-applied-not-undoable",
            // The five this panel paints SINCE `sync_status_line` delegates
            // to the shared `status_line` and `mode_label` to the shared
            // one: the list stopped covering what the screen says (C2
            // branch review, rust MINOR-5). The four "cut" ones are exactly
            // the ones this branch added so a cut run doesn't read as a
            // clean one.
            "sync-status-applied-cut-undoable",
            "sync-status-applied-cut-not-undoable",
            "sync-status-applied-failed-undoable",
            "sync-status-applied-failed-not-undoable",
            "sync-mode-unknown",
            "sync-hint",
            "sync-hint-done",
            "sync-hint-confirm",
            "sync-anchor-dest",
            "sync-anchor-either",
            "compare-marked",
            "msg-sync-needs-daemon",
            "msg-sync-too-many-marks",
            "msg-sync-cannot-approve",
            "reason-needs-daemon",
        ] {
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                assert_ne!(
                    norte_i18n::t_in(lang, key),
                    key,
                    "missing {key} in {lang:?}"
                );
            }
        }
    }

    /// A synthetic `SyncRun`: the Task is backed by the test's own `watch`,
    /// so the run loop's logic can be asserted without an engine or a daemon
    /// (same mold as `compare_tests::run_with`).
    fn run_sync(
        state: norte_proto::TaskState,
        applying: bool,
    ) -> (
        SyncRun,
        tokio::sync::mpsc::Sender<norte_core::sync::SyncPlanEvent>,
        tokio::sync::watch::Sender<norte_proto::TaskProgress>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let id = norte_proto::TaskId::new(1);
        let (progress_tx, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Sync,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        (
            SyncRun {
                task: TaskRef::synthetic_for_tests(id, prx.clone()),
                rx: (!applying).then_some(rx),
                progress: prx,
                applying,
            },
            tx,
            progress_tx,
        )
    }

    fn view_at(app: &mut App, mode: SyncMode) {
        app.sync = Some(crate::app::SyncView::new(
            norte_proto::TaskId::new(1),
            mode,
            vp("file:///home"),
            vp("file:///other"),
            None,
            None,
        ));
    }

    /// A one-step, approvable plan close.
    fn plan_done() -> SyncPlanDone {
        SyncPlanDone {
            task_id: norte_proto::TaskId::new(1),
            plan_hash: PlanHash::from_digest(&[3u8; 32]),
            counts: SyncCounts {
                copy: 1,
                bytes: 10,
                ..SyncCounts::default()
            },
            blockers: Vec::new(),
            blockers_total: 0,
            executable: true,
            dest_trash: norte_proto::methods::DestTrash::Restorable,
        }
    }

    fn step() -> norte_proto::methods::SyncStep {
        norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Copy,
            rel: norte_proto::methods::RelPath::parse_wire("a.txt").expect("rel"),
            dest_rel: None,
            size: Some(10),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    /// A plan READY in the panel.
    fn app_with_ready_plan() -> App {
        let mut app = app_with_rows(1);
        view_at(&mut app, SyncMode::Update);
        let view = app.sync.as_mut().expect("panel");
        view.state = norte_frontend::sync::SyncState::ready(vec![step()], plan_done());
        view.run = crate::app::SyncRunState::Done;
        app
    }

    /// **The BLOCKER regression.** An extra `a` over a plan already approved
    /// can't send it again.
    ///
    /// `SyncPlan::can_approve` keeps answering yes forever — its three
    /// factors don't change when the plan is spent — and `SyncState::plan()`
    /// returns the same plan in `Applying` and in `Applied`. With just that
    /// check, an `a` during a long application launched a second
    /// `sync.apply` that the spool answers `PlanStale`, the error arm
    /// painted "the plan failed" over a sync that was still WRITING, and
    /// since that leaves `run != Running` the next `Esc` half-cancelled it
    /// thinking it was closing a failure.
    #[test]
    fn approving_twice_does_not_send_the_plan_twice() {
        let mut app = app_with_ready_plan();
        approve_sync(&mut app);
        assert!(
            app.pending_sync_apply.is_some(),
            "the first one does send it"
        );
        // The run loop takes it and the Task starts.
        app.pending_sync_apply = None;
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));

        approve_sync(&mut app);
        assert!(
            app.pending_sync_apply.is_none(),
            "a second `a` sent the same hash again"
        );
        assert_eq!(app.message, Some(norte_i18n::t("msg-sync-cannot-approve")));
    }

    /// And not afterward either, with the report already on screen:
    /// overwriting the line that says what can be undone with "the plan
    /// failed" destroys the only thing left written about it.
    #[test]
    fn approving_an_already_applied_plan_does_nothing() {
        let mut app = app_with_ready_plan();
        {
            let view = app.sync.as_mut().expect("panel");
            view.state.on_apply_started(norte_proto::TaskId::new(9));
            view.state
                .on_report(norte_proto::methods::SyncReportResult {
                    done: 1,
                    failed: 0,
                    skipped: 0,
                    bytes: 10,
                    failures: Vec::new(),
                    batch_id: Some(1),
                    dest_trash: norte_proto::methods::DestTrash::Restorable,
                });
        }
        approve_sync(&mut app);
        assert!(app.pending_sync_apply.is_none());
    }

    /// The half-answered question is resolved by ANY key, including ones
    /// with a modifier.
    ///
    /// With the modifier filter in front, a habitual `Ctrl+r` or `Alt+e`
    /// used to fall into `Ignore` and left "2 trees are about to be
    /// deleted..." armed on screen, waiting for a `y` that no longer knew
    /// what it was answering.
    #[test]
    fn a_modifier_also_cancels_the_second_question() {
        for (mods, code) in [
            (M::CONTROL, KeyCode::Char('r')),
            (M::ALT, KeyCode::Char('e')),
            (M::SHIFT, KeyCode::Char('Y')),
        ] {
            assert_eq!(
                sync_key(mods, code, false, false, true),
                SyncKey::ConfirmNo,
                "{mods:?}+{code:?} left the question armed"
            );
        }
        // And `y` with a modifier is NOT a yes: the yes is the bare key.
        assert_eq!(
            sync_key(M::CONTROL, KeyCode::Char('y'), false, false, true),
            SyncKey::ConfirmNo
        );
    }

    /// Cancelling the Task also drops the question: leaving it up while the
    /// sync that motivated it stops is how a later `y` approves something
    /// else.
    #[test]
    fn cancelling_drops_the_second_question() {
        let mut app = app_with_ready_plan();
        {
            let view = app.sync.as_mut().expect("panel");
            view.run = crate::app::SyncRunState::Running;
            view.confirming = Some(norte_frontend::sync::Confirmation {
                id: "sync-confirm-delete",
                text: "??".to_owned(),
            });
        }
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, true);
        let mut sync_run = Some(run);
        on_sync_key(&mut app, &mut sync_run, M::NONE, KeyCode::Esc);
        let view = app.sync.as_ref().expect("the panel is still open");
        assert!(view.cancel_requested);
        assert!(view.confirming.is_none(), "the question survived the Esc");
    }

    /// The second `Esc` closes and CANCELS: remotely, the daemon would keep
    /// applying for a panel that no longer exists.
    #[test]
    fn the_second_esc_closes_and_cancels_the_task() {
        let mut app = app_with_ready_plan();
        app.sync.as_mut().expect("panel").cancel_requested = true;
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, true);
        let canceller = run.task.canceller();
        let mut sync_run = Some(run);
        on_sync_key(&mut app, &mut sync_run, M::NONE, KeyCode::Esc);
        assert!(app.sync.is_none(), "the panel closes");
        assert!(sync_run.is_none(), "and the run gets dropped");
        drop(canceller);
    }

    /// A batch with the panel already closed cancels the Task instead of
    /// continuing to receive steps nobody is going to look at.
    #[test]
    fn a_batch_with_the_panel_closed_harvests_the_run() {
        let mut app = app_with_rows(1);
        app.sync = None;
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        drain_sync_plan(&mut app, &mut sync_run, None);
        assert!(
            sync_run.is_none(),
            "the run gets dropped with the panel closed"
        );
    }

    /// The plan's stream ending leaves `rx` at `None`: a closed channel
    /// would return `None` in a loop and the `select!` arm would spin.
    #[test]
    fn the_streams_end_disarms_the_plans_arm() {
        let mut app = app_with_rows(1);
        view_at(&mut app, SyncMode::Update);
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        prog.send_modify(|p| p.state = norte_proto::TaskState::Completed);
        drain_sync_plan(&mut app, &mut sync_run, None);
        let run = sync_run.as_ref().expect("the run is kept for the Esc");
        assert!(run.rx.is_none(), "the arm disarms once the channel closes");
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Done
        );
    }

    /// A CANCELLED plan says cancelled, not "done": without `sync.plan_done`
    /// there's no `plan_hash`, so there's nothing to approve.
    #[test]
    fn a_cancelled_plan_says_cancelled() {
        let mut app = app_with_rows(1);
        view_at(&mut app, SyncMode::Update);
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, false);
        let mut sync_run = Some(run);
        prog.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);
        drain_sync_plan(&mut app, &mut sync_run, None);
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Cancelled
        );
        assert!(!app.sync.as_ref().expect("panel").state.can_approve());
    }

    /// With the progress senders dropped and a NON-terminal state, the
    /// application is harvested anyway and counted as a failure.
    ///
    /// Returning without harvesting would re-arm the arm on a `changed()`
    /// that returns `Err` instantly — a spin — and calling it "done" would
    /// be saying a half-finished sync ended well.
    #[tokio::test]
    async fn a_dropped_sender_harvests_the_application_as_a_failure() {
        let mut app = app_with_ready_plan();
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let (run, _tx, prog) = run_sync(norte_proto::TaskState::Running, true);
        let mut sync_run = Some(run);
        drop(prog);
        harvest_sync_apply(&mut app, &backend, &mut sync_run, false).await;
        assert!(sync_run.is_none(), "the run gets dropped");
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Failed
        );
    }

    /// And a Task that FINISHED requests its report no matter what,
    /// cancellation included: what was applied up to the cut stays,
    /// journalled, and half a sync is a real state the reader has to be
    /// able to see.
    #[tokio::test]
    async fn a_cancelled_application_asks_for_its_report() {
        let mut app = app_with_ready_plan();
        app.sync
            .as_mut()
            .expect("panel")
            .state
            .on_apply_started(norte_proto::TaskId::new(9));
        let engine = norte_core::Engine::new();
        let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
        let (run, _tx, _prog) = run_sync(norte_proto::TaskState::Cancelled, true);
        let mut sync_run = Some(run);
        harvest_sync_apply(&mut app, &backend, &mut sync_run, true).await;
        assert!(sync_run.is_none());
        // The embedded engine doesn't have that report, so the status bar
        // says so — what's asserted is that it WAS requested and that the
        // terminal state got painted.
        assert_eq!(
            app.sync.as_ref().expect("panel").run,
            crate::app::SyncRunState::Cancelled
        );
        assert!(app.message.is_some());
    }

    /// `SyncTick` exists because `select!` won't let `sync_run` be borrowed
    /// twice; its two variants being SUCCESSIVE phases — plan and
    /// application are never both at once — is what makes the single arm
    /// correct.
    #[test]
    fn the_tick_distinguishes_the_two_phases() {
        let (plan, _tx, _prog) = run_sync(norte_proto::TaskState::Running, false);
        assert!(plan.rx.is_some() && !plan.applying);
        let (applying, _tx2, _prog2) = run_sync(norte_proto::TaskState::Running, true);
        assert!(applying.rx.is_none() && applying.applying);
        assert_ne!(
            std::mem::discriminant(&SyncTick::Plan(None)),
            std::mem::discriminant(&SyncTick::Applied { alive: true })
        );
    }

    /// A mark that is one of the two ROOTS gets refused instead of sent: a
    /// root in an `include` means "everything", so a single one would turn
    /// a narrow selection into a whole-tree plan — under `Mirror`, into
    /// "delete from the destination everything the source doesn't have".
    #[test]
    fn a_mark_that_is_the_root_is_refused() {
        let mut app = app_with_rows(1);
        {
            let view = app.compare.as_mut().expect("panel");
            view.pane.extend(vec![CompareRow {
                id: 99,
                left: Some(entry("file:///home")),
                right: None,
                verdict: CompareVerdict::OnlyLeft,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
                paired_under: None,
            }]);
            view.pane.toggle_mark(99);
        }
        assert!(app.request_sync(SyncMode::Mirror).is_none());
        assert_eq!(
            app.message,
            Some(norte_i18n::t("msg-sync-mark-is-the-root"))
        );
    }

    /// And one that hangs off neither of the two roots also gets refused,
    /// instead of crashing: an `include` shrunk to an empty list is a
    /// zero-step plan, which the panel paints as "both trees already
    /// match" — a lie on a screen that authorizes writes.
    #[test]
    fn a_mark_outside_both_roots_is_refused() {
        let mut app = app_with_rows(1);
        {
            let view = app.compare.as_mut().expect("panel");
            view.pane.extend(vec![CompareRow {
                id: 98,
                left: Some(entry("file:///elsewhere/x.txt")),
                right: None,
                verdict: CompareVerdict::OnlyLeft,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                newer: None,
                reason: None,
                side: None,
                paired_under: None,
            }]);
            view.pane.toggle_mark(98);
        }
        assert!(app.request_sync(SyncMode::Update).is_none());
        assert_eq!(
            app.message,
            Some(norte_i18n::t("msg-sync-mark-outside-roots"))
        );
    }
}
