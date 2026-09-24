//! The synchronization surface's state, and what the bar says.
//!
//! The state machine a frontend paints: which phase it is, which keys make
//! sense in it, and what sentence sums it up. With nothing of the terminal
//! nor the window: both surfaces share it whole.

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::methods::{
    DestTrash, PlanHash, SyncCounts, SyncMode, SyncPlanDone, SyncReportResult, SyncStep,
    SyncStepsBatch,
};
use norte_proto::{TaskId, TaskState, VPath};

use super::{
    Applied, Applying, Confirmation, PlanIntegrity, Planning, SyncEncodings, SyncPlan, total_steps,
};

/// The dialog's whole state, and the only legal way to move through it.
///
/// **It never goes backwards.** Every transition is a method that only fires
/// from the one state it belongs to, so a notification that arrives late — a
/// `sync.plan_done` from a plan the user already approved, a batch after the
/// close — is dropped rather than rewinding a dialog the human is looking at.
/// Starting over means building a new [`SyncState`], not walking back through
/// this one.
///
/// **And it only ever listens to ONE plan.** One connection can have two plans
/// in flight — which is why `sync.steps` and `sync.plan_done` both carry a
/// `task_id` — so every transition checks it against the task this dialog was
/// opened for and DROPS what belongs to another. Without that check, a user who
/// re-plans with a narrower selection keeps looking at the first plan's steps
/// and approves the first plan's `plan_hash`: the daemon then executes exactly
/// what was approved, which is not what is on screen. The transitions return
/// `false` when they drop something, so a frontend can log it — a batch that
/// arrives after the close is a protocol violation, and silence is how that
/// hides.
///
/// A dialog opened with [`Planning::default`] (no task) accepts whatever
/// arrives: that is the constructed-plan door, for tests and for a caller that
/// correlated the stream itself.
///
/// **There is no failure state, deliberately.** A `sync.plan` or `sync.apply`
/// that fails or is cancelled is a TASK outcome, which a frontend already
/// paints from `task.progress`; this model would only be able to repeat it.
/// The dialog is dropped, not walked back. What is worth knowing after the
/// fact lives on [`Applied`]: a report with no `batch_id` means nothing was
/// journalled, so [`Applied::is_undoable`] — not the plan's outlook — is what
/// a frontend must read once the plan has run.
///
/// ```
/// use norte_frontend::sync::{Planning, SyncState};
/// let s = SyncState::Planning(Planning::default());
/// assert!(!s.can_approve(), "a plan that has not closed cannot be approved");
/// ```
#[derive(Debug, Clone)]
pub enum SyncState {
    /// `sync.plan` is running and steps are arriving.
    Planning(Planning),
    /// The plan closed and is waiting for a human.
    Ready(SyncPlan),
    /// `sync.apply` is running.
    Applying(Applying),
    /// It finished, and the report is in.
    Applied(Applied),
}

impl Default for SyncState {
    fn default() -> Self {
        Self::Planning(Planning::default())
    }
}

impl SyncState {
    /// A closed plan built from the steps that arrived and the notification
    /// that closed it.
    ///
    /// Routes through [`SyncState::on_plan_done`] rather than assembling a
    /// [`SyncPlan`] directly: there is ONE place where the steps are checked
    /// against the counts, and a second constructor is a second chance to
    /// forget it.
    #[must_use]
    pub fn ready(steps: Vec<SyncStep>, done: SyncPlanDone) -> Self {
        let mut planning = Planning::default();
        planning.extend(steps);
        let mut state = Self::Planning(planning);
        state.on_plan_done(done);
        state
    }

    /// A `sync.steps` batch.
    ///
    /// Takes the whole [`SyncStepsBatch`] and not a list of steps, because the
    /// `task_id` is the only thing that says the batch belongs to THIS plan.
    /// Returns `false` when the batch was dropped: it named another task, or
    /// the plan had already closed (which is a protocol violation, since
    /// `sync.plan_done` is last).
    pub fn on_steps(&mut self, batch: SyncStepsBatch) -> bool {
        let Self::Planning(p) = self else {
            return false;
        };
        if !p.owns(batch.task_id) {
            return false;
        }
        p.extend(batch.steps);
        true
    }

    /// `sync.plan_done`. Only ever fires from [`SyncState::Planning`], and only
    /// for the task this dialog is following; a late one — or one belonging to
    /// a plan the user launched afterwards — is dropped, and `false` says so.
    pub fn on_plan_done(&mut self, done: SyncPlanDone) -> bool {
        let Self::Planning(planning) = self else {
            return false;
        };
        if !planning.owns(done.task_id) {
            return false;
        }
        let planning = std::mem::take(planning);
        let integrity = integrity_of(
            &planning.counts,
            &done.counts,
            planning.malformed,
            planning.duplicate_ids,
        );
        let selected = planning.steps.first().map(|s| s.id);
        *self = Self::Ready(SyncPlan {
            done,
            steps: planning.steps,
            integrity,
            unreadable: planning.unreadable,
            selected,
            dropped: planning.dropped,
            viewport_offset: 0,
        });
        true
    }

    /// The human approved and `sync.apply` answered with a task. Only fires
    /// from [`SyncState::Ready`], and only for a plan that
    /// [`SyncPlan::can_approve`] — a frontend cannot talk this model into
    /// showing a blocked plan as running.
    pub fn on_apply_started(&mut self, task_id: TaskId) {
        let Self::Ready(plan) = self else {
            return;
        };
        if !plan.can_approve() {
            return;
        }
        *self = Self::Applying(Applying {
            plan: plan.clone(),
            task_id,
        });
    }

    /// `sync.report` came back. Only fires from [`SyncState::Applying`].
    pub fn on_report(&mut self, report: SyncReportResult) {
        let Self::Applying(applying) = self else {
            return;
        };
        *self = Self::Applied(Applied {
            plan: applying.plan.clone(),
            report,
        });
    }

    /// The closed plan, in whichever state still holds one.
    #[must_use]
    pub fn plan(&self) -> Option<&SyncPlan> {
        match self {
            Self::Planning(_) => None,
            Self::Ready(p) => Some(p),
            Self::Applying(a) => Some(&a.plan),
            Self::Applied(a) => Some(&a.plan),
        }
    }

    /// The closed plan, mutably — for the cursor, and only for the cursor.
    ///
    /// [`SyncPlan::select`] and [`SyncPlan::move_by`] are the whole reason this
    /// exists: a pane moves a cursor, and everything else on [`SyncPlan`] is a
    /// question. It is deliberately not a door back into the state machine —
    /// there is nothing mutable on [`SyncPlan`] that could rewind it.
    pub fn plan_mut(&mut self) -> Option<&mut SyncPlan> {
        match self {
            Self::Planning(_) => None,
            Self::Ready(p) => Some(p),
            Self::Applying(a) => Some(&mut a.plan),
            Self::Applied(a) => Some(&mut a.plan),
        }
    }

    /// Whether the confirm key does anything right now.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        matches!(self, Self::Ready(p) if p.can_approve())
    }
}

/// Do the steps that arrived account for the plan the daemon closed?
///
/// The classes are compared one by one rather than by their total: two errors
/// that cancel out — a lost `delete_tree` and an extra `skip` — would pass a
/// sum, and those are not the same plan.
///
/// A step this build cannot name is reported FIRST, because it is the stronger
/// statement: the totals may match perfectly and the plan still contain
/// something that cannot be painted. Duplicate ids follow the same per-step
/// class as malformed shapes, and for the same reason (#194): each is a
/// defect in ONE step's identity, not in the totals, so it is checked before
/// anything that sums.
fn integrity_of(
    local: &SyncCounts,
    remote: &SyncCounts,
    malformed: u64,
    duplicate_ids: u64,
) -> PlanIntegrity {
    // The daemon's own `unknown_kind` counts too, and it is not redundant: a
    // daemon that reports one while every step we decoded had a name is
    // telling us the plan holds something neither of us can show.
    let unnameable = local.unknown_kind.max(remote.unknown_kind);
    if unnameable > 0 {
        return PlanIntegrity::Unnameable { steps: unnameable };
    }
    if malformed > 0 {
        return PlanIntegrity::Malformed { steps: malformed };
    }
    if duplicate_ids > 0 {
        return PlanIntegrity::DuplicateIds {
            steps: duplicate_ids,
        };
    }
    let classes_agree = local.create_dir == remote.create_dir
        && local.copy == remote.copy
        && local.overwrite == remote.overwrite
        && local.delete_tree == remote.delete_tree
        && local.skip == remote.skip;
    if !classes_agree {
        return PlanIntegrity::Mismatch {
            received: total_steps(local),
            counted: total_steps(remote),
        };
    }
    // Both sides ran `SyncCounts::add` over the same steps, so EVERY field must
    // match, not only the classes. `irreversible` is the one that matters most
    // — the headline is built on it — and `bytes`/`unmeasured_steps` come free
    // in the same comparison.
    if local == remote {
        PlanIntegrity::Complete
    } else {
        PlanIntegrity::Contradictory
    }
}

/// How the Task of a sync pane is going, for the status bar.
///
/// Deliberately SHORTER than [`crate::compare::CompareState`]: here "all the
/// rows arrived" is not deduced from a count, `sync.plan_done` SAYS so —
/// without it there is no `plan_hash` and nothing to approve, so an
/// incomplete plan is not a state to paint but a plan that does not exist
/// (`SyncPlanEvent`, ADR 0049).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncRunState {
    /// A live Task: it is planning, or applying.
    #[default]
    Running,
    /// The Task ended well.
    Done,
    /// The user cancelled.
    Cancelled,
    /// The Task failed (the error goes on the bar).
    Failed,
}

impl SyncRunState {
    /// The outcome of the Task running behind the pane —the plan's first,
    /// the apply's afterwards— read from its terminal [`TaskState`].
    ///
    /// Deliberately does NOT touch the localised error each frontend paints
    /// (a truncated bar in the TUI, something different in the GUI): that is
    /// the one half that legitimately differs between the two, and mixing it
    /// in here would tie this pure mapping to a [`Lang`] with no need to.
    /// What WAS one decision repeated by hand
    /// —`Cancelled`/`Failed`/everything else→`Done`— is what lives here, so
    /// a `_ => Done` is not transcribed twice with one arm forgotten from
    /// one of the two copies some day.
    ///
    /// ```
    /// use norte_frontend::sync::SyncRunState;
    /// use norte_proto::TaskState;
    ///
    /// assert_eq!(
    ///     SyncRunState::from_task_state(&TaskState::Cancelled),
    ///     SyncRunState::Cancelled
    /// );
    /// ```
    #[must_use]
    pub fn from_task_state(state: &TaskState) -> Self {
        match state {
            TaskState::Cancelled => Self::Cancelled,
            TaskState::Failed { .. } => Self::Failed,
            _ => Self::Done,
        }
    }
}

/// The open sync pane: [`SyncState`]'s pure model plus what a frontend needs
/// to paint it and to talk to the backend.
///
/// The split is the same as [`crate::compare::CompareView`]'s (hard rule 7):
/// the dialog's state —which steps arrived, whether they match what the
/// daemon closed with, what the undo gives back, and what the second
/// question is— lives here, where it is tested without a terminal. What each
/// frontend adds are the two roots the header paints, the run's state, and
/// the confirmation question IN PROGRESS — and those also live here (#161):
/// the TUI and the GUI need the SAME wrapper, not two reimplemented
/// separately.
#[derive(Debug)]
pub struct SyncView {
    /// The dialog's model (Task 12).
    pub state: SyncState,
    /// How the Task currently running is going (the plan's first, the
    /// apply's afterwards).
    pub run: SyncRunState,
    /// The requested mode, which the header paints: a `Mirror` deletes and
    /// an `Update` does not, and the reader has to see it before approving.
    pub mode: SyncMode,
    /// SOURCE root. Almost all steps' `rel` hangs from it.
    pub source_root: VPath,
    /// DESTINATION root. A `DeleteTree`'s and an unreadable `Skip`'s hang
    /// from it ([`crate::sync::anchor_of`]).
    pub dest_root: VPath,
    /// The SOURCE pane's name reinterpretation (#57), frozen at open time.
    pub source_encoding: Option<norte_encoding::NameEncoding>,
    /// The DESTINATION pane's, which may be another one.
    ///
    /// Two and not one, for the same reason the diff pane carries two: the
    /// two panes are two locations and can carry different overrides. Here
    /// it matters even more, because `SyncStep::dest_rel` exists precisely
    /// to show the DESTINATION's spelling (#152) — decoding it with the
    /// SOURCE's codepage would name the file the write is about to land on
    /// with different bytes.
    pub dest_encoding: Option<norte_encoding::NameEncoding>,
    /// The second question, already asked and waiting for a `y`.
    ///
    /// `None` = approve has not been pressed yet, or the plan did not need
    /// it. Lives here and not in the model because it is INTERACTION
    /// state —half-answered— and Task 12's model does not go backwards:
    /// asking belongs to the screen, deciding belongs to it.
    pub confirming: Option<Confirmation>,
    /// Cancellation was already requested (the first `Esc`), same as in the
    /// diff pane and for the same reason: the second `Esc` closes whatever
    /// happens to the Task.
    pub cancel_requested: bool,
    /// A FAILED Task's error category, already localised and sanitised.
    pub error: Option<String>,
    /// `sync.apply` has already GONE OUT and the daemon has not answered
    /// yet.
    ///
    /// Private on purpose: the only way to set it is [`SyncView::submit`]
    /// and the only way to read it, [`SyncView::is_submitted`]. What makes
    /// it necessary is that `Applying` does NOT arrive with the keystroke
    /// but a whole round trip later, when the daemon returns the Task — in
    /// a GUI that reads events between keystrokes that window admits a
    /// second `a`, and also an `Esc` (security review MAJOR-1).
    ///
    /// Lived in `norte-gui` until C2's branch review, and it was wrong
    /// there: `can_approve`, [`hint_id`], and [`status_line`] live in THIS
    /// crate and could not see it, so the footer kept offering `a to
    /// approve` over a plan `approve` already rejected — exactly the broken
    /// screen `hint_id` exists to never paint. STATE TRANSITIONS clear it
    /// ([`SyncView::on_apply_started`], [`SyncView::on_apply_ended`]), never
    /// the request's generation: tying it to the generation left it set
    /// forever when a superseded event got discarded.
    submitted: bool,
}

impl SyncView {
    /// A freshly opened pane over these two roots, with no steps yet.
    #[must_use]
    pub fn new(
        task_id: TaskId,
        mode: SyncMode,
        source_root: VPath,
        dest_root: VPath,
        source_encoding: Option<norte_encoding::NameEncoding>,
        dest_encoding: Option<norte_encoding::NameEncoding>,
    ) -> Self {
        Self {
            // With the `task_id` from the start: it is what makes a batch
            // from ANOTHER plan —the reader re-plans with fewer marks—
            // dropped instead of mixed with this one (Task 12, note 3).
            state: SyncState::Planning(Planning::new(task_id)),
            run: SyncRunState::Running,
            mode,
            source_root,
            dest_root,
            source_encoding,
            dest_encoding,
            confirming: None,
            cancel_requested: false,
            error: None,
            submitted: false,
        }
    }

    /// What trash the DESTINATION has, according to the plan.
    ///
    /// [`DestTrash::Unknown`] while the plan has not closed, which is the
    /// honest answer: with no `sync.plan_done` it is not known, and the
    /// model paints every step as "this version cannot say" instead of
    /// promising it comes back. [`SyncStep::reversal`] is never read raw —
    /// that is half the answer, and the half that lies when the destination
    /// has no trash.
    #[must_use]
    pub fn dest_trash(&self) -> DestTrash {
        self.state
            .plan()
            .map_or(DestTrash::Unknown, SyncPlan::dest_trash)
    }

    /// The two reinterpretations, together and named, to hand to
    /// [`crate::sync::render_step`] as one piece — which is what stops them
    /// from being crossed (#152).
    #[must_use]
    pub fn encodings(&self) -> SyncEncodings {
        SyncEncodings {
            source: self.source_encoding,
            dest: self.dest_encoding,
        }
    }

    /// The steps there are RIGHT NOW, whether the plan has closed or not.
    ///
    /// While the plan is arriving, [`SyncState::plan`] answers `None` —there
    /// is no plan until `sync.plan_done`, which is what gives it its
    /// `plan_hash`— and yet the steps already received exist and get
    /// painted. Without this, the pane showed an empty gap while the footer
    /// counted "planning… 6 steps", which is the screen contradicting
    /// itself. The undo column of those steps comes out "this version
    /// cannot say", which is the truth until the destination's trash is
    /// known.
    #[must_use]
    pub fn steps(&self) -> &[SyncStep] {
        match &self.state {
            SyncState::Planning(p) => p.steps(),
            _ => self.state.plan().map_or(&[], |p| p.steps()),
        }
    }

    /// Is there still something to approve?
    ///
    /// `false` as soon as the plan is submitted: the key hint cannot keep
    /// offering `a to approve` over a plan that has already been spent
    /// —applying it consumes it, and a second `sync.apply` of the same hash
    /// is `PlanStale`.
    #[must_use]
    pub fn awaiting_approval(&self) -> bool {
        matches!(self.state, SyncState::Ready(_))
    }

    /// Can this pane be approved RIGHT NOW?
    ///
    /// Wraps [`SyncState::can_approve`] and NEVER
    /// [`SyncPlan::can_approve`] — the latter, reachable through
    /// [`SyncState::plan`], still answers yes about a plan already approved,
    /// because its three factors do not change on being spent. This method
    /// is how a caller never gets a chance to take the wrong shortcut (#161,
    /// the trap phase A's CLI did not see: it asked nothing, and a
    /// `Malformed` plan got applied whole from the spool).
    ///
    /// # And the Task's outcome counts
    /// A `Cancelled` or `Failed` run is not approved, even if the plan HAS
    /// closed. The two facts are compatible —`sync.plan_done` arrives before
    /// the channel closes, so an `Esc` (or a daemon crash) in that window
    /// leaves `Ready` + `Cancelled`— and without this clause the screen said
    /// both things at once: the footer painted "cancelled — there is no plan
    /// to approve" ([`status_line`]) while the key hint kept offering to
    /// approve, and the key WORKED (rust review MAJOR-1). It is resolved on
    /// the conservative side: whoever pressed `Esc` asked to stop, and this
    /// screen writes to someone's disk.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        if self.submitted || matches!(self.run, SyncRunState::Cancelled | SyncRunState::Failed) {
            return false;
        }
        self.state.can_approve()
    }

    /// Is there a `sync.apply` in flight with no answer?
    ///
    /// Asked by whoever paints the key hint and whoever interprets an
    /// `Esc`: in this window the daemon is ALREADY writing, so an `Esc` has
    /// to request cancellation and not close the pane. Closing it loses the
    /// report —and with it the count, the failures, and the undo handle—
    /// over a destination that got half rewritten (security review
    /// MAJOR-1).
    #[must_use]
    pub fn is_submitted(&self) -> bool {
        self.submitted
    }

    /// The request resolved WITH NO Task: the daemon rejected it, or a Task
    /// arrived that this pane does not adopt.
    ///
    /// Releases the latch, because otherwise the `a` stays dead forever and
    /// the footer keeps offering it. Also called on the paths where the
    /// event is discarded for a superseded generation: tying the release to
    /// the generation is exactly what left the pane stuck when the second
    /// plan got rejected and no new pane replaced the first one (C2's
    /// branch review, MINOR of both reviews).
    pub fn on_apply_abandoned(&mut self) {
        self.submitted = false;
        // And the cancellation request is forgotten with it. An `Esc` that
        // never cancelled ANYTHING —because the apply was never even
        // born— left `cancel_requested` set forever, and then every later
        // apply had it rejected by `on_apply_started`: the pane turned into
        // a machine that launches writes that are never adopted.
        self.cancel_requested = false;
    }

    /// Sets the latch and returns the hash to submit, or `None` if this
    /// pane cannot be approved.
    ///
    /// One single door for both frontends: whoever wants to apply goes
    /// through here, and what stops a second `sync.apply` is this function,
    /// not the state being `Applying` —it is not, yet. The TUI awaits it
    /// inline and cannot read a key in between, so for it this is a no-op;
    /// the GUI can, and it is the one that needs it.
    pub fn submit(&mut self) -> Option<PlanHash> {
        if !self.can_approve() {
            return None;
        }
        let hash = self.state.plan()?.done().plan_hash.clone();
        self.submitted = true;
        Some(hash)
    }

    /// `sync.apply` was launched and the daemon answered with a Task: joins
    /// the FOUR updates that instant demands — the model advances to
    /// `Applying` ([`SyncState::on_apply_started`]), the run goes back to
    /// `Running`, the second question falls (already answered), and a
    /// cancellation requested by a previous run stops applying to the new
    /// one.
    ///
    /// Before this lived here, `norte-tui` did the four by hand at the spot
    /// that launches the Task; the GUI would have needed exactly the same
    /// four, and a separate reimplementation is exactly the chance to forget
    /// one — the trap this move exists to not repeat (#161, C1's review).
    /// # And it can be REFUSED
    /// Returns `false` without touching anything if cancellation was already
    /// requested. The `Esc` that asked to stop arrived BEFORE the Task, so
    /// adopting it here would resurrect a run the reader considered cut
    /// short and, worse, would erase the cancellation request with the
    /// `cancel_requested = false` below — which exists so an old
    /// cancellation does not stain the new run, not to discard the one that
    /// was just requested.
    ///
    /// The guard was in the GUI's wrapper and not here, so the TUI was left
    /// with the hole: today it does not reach it because it awaits
    /// `sync.apply` inline, i.e. by accident of control flow and not by
    /// design (C2's branch review, rust MAJOR-2). Whoever refuses it has to
    /// cancel the Task it was given back: nobody else knows about it.
    pub fn on_apply_started(&mut self, task_id: TaskId) -> bool {
        if self.cancel_requested {
            return false;
        }
        self.state.on_apply_started(task_id);
        self.run = SyncRunState::Running;
        self.confirming = None;
        self.cancel_requested = false;
        // The daemon answered: the window the latch covers is over, and from
        // here on what stops a second `sync.apply` is the `Applying` state.
        self.submitted = false;
        true
    }

    /// The `sync.apply` Task ended, with whatever `sync.report` answered:
    /// stores the report and sets the outcome. Returns the error category
    /// that has to be said, UNSANITISED — each frontend places it where and
    /// however it paints.
    ///
    /// Shared (#161) because the three rules here are the kind one frontend
    /// fixes and the other keeps:
    ///
    /// 1. **The TASK's error rules over the report's**: it is the one that
    ///    says why it stopped.
    /// 2. **With no report it is not said that it ended well.** `sync.report`
    ///    is the ONLY thing that says how much got written; if it could not
    ///    be requested, the outcome is `Failed` with THAT error's category
    ///    even if the Task said `Completed`. `norte-tui` used to get stuck
    ///    here in `Applying` with a transient bar, and the footer said
    ///    "applying…" forever.
    /// 3. **A NON-terminal state is also a failure.** It is only reached
    ///    with the progress emitters down: the connection died without
    ///    saying what happened, and a synchronization cut halfway through
    ///    is not a success.
    ///
    /// With ONE exception to the last two: a **cancelled** Task is reported
    /// as cancelled even if the report is missing. The reader asked to stop
    /// and that much is already known; turning it into "failed" would take
    /// away the one solid fact it has, and that the report never arrived is
    /// told by the category this returns.
    ///
    /// The second question falls with the request that motivated it: leaving
    /// it set under a footer that already says "failed" is how a later `y`
    /// answers something else.
    ///
    /// The report is stored ALSO when the Task was cancelled: what got
    /// applied up to the cut stays journalled, and a half synchronization is
    /// a real state the reader has to be able to see.
    pub fn on_apply_ended(
        &mut self,
        state: &TaskState,
        report: Result<SyncReportResult, norte_proto::Error>,
        lang: Lang,
    ) -> Option<String> {
        // The language goes as a PARAMETER and is not read from the global:
        // the graphical window has one per instance, and the outcome of a
        // write in another window's language is an outcome that is not
        // read.
        let category = match (state, &report) {
            (TaskState::Failed { error }, _) => Some(crate::error::error_category_in(lang, error)),
            (_, Err(e)) => Some(crate::error::error_category_in(lang, e)),
            _ => None,
        };
        if let Ok(report) = report {
            self.state.on_report(report);
        }
        self.run = if matches!(state, TaskState::Cancelled) {
            // A cancellation is reported as CANCELLED even if the report
            // never arrives: the reader asked to stop and that much is
            // already known, so calling it "failed" would take away the one
            // solid fact it has. That how much got written cannot be said is
            // stated by the banner, with the category this returns.
            SyncRunState::Cancelled
        } else if category.is_some() || !state.is_terminal() {
            SyncRunState::Failed
        } else {
            SyncRunState::from_task_state(state)
        };
        self.confirming = None;
        // It ended: the latch releases whatever happens, even if this
        // arrives without `on_apply_started` ever having run (a Task that
        // fails before being adopted). Otherwise the pane is left unable to
        // approve with the footer still offering to.
        self.submitted = false;
        category
    }
}

/// Which KEY hint applies right now, as a Fluent id.
///
/// Three, and the difference between the last two is the only key on this
/// screen that writes to someone's disk:
///
/// * `sync-hint-confirm` with the second question set — the keyboard has
///   shrunk to `y` and "anything else", and saying "↑↓ move" there would
///   offer something that no longer works;
/// * `sync-hint`, which NAMES the approve key, only when approving does
///   something;
/// * `sync-hint-done` for everything else.
///
/// # Why it is shared
/// The second arm asks about [`SyncView::can_approve`] and not only
/// [`SyncView::awaiting_approval`], and that is the fix: a plan that closed
/// but that the daemon marked non-executable —or whose Task got
/// cancelled— is in `Ready` and CANNOT be approved, and the key hint kept
/// offering `a to approve` over a footer that already said "this plan
/// cannot be approved" ([`status_line`]). It is the same disagreement rust
/// review MAJOR-1 fixed between the footer and the key, one layer up; it
/// lives here so there is ONE answer for both frontends and not one fixed
/// and one not —which is exactly what C1 shipped (#161).
///
/// Applying SPENDS the plan, so in `Applying`/`Applied` the `a` disappears:
/// a second `sync.apply` of the same hash answers `PlanStale`.
///
/// ```
/// use norte_frontend::sync::{SyncView, hint_id};
/// use norte_proto::{TaskId, VPath};
/// use norte_proto::methods::SyncMode;
/// let v = SyncView::new(
///     TaskId::new(1),
///     SyncMode::Update,
///     VPath::parse("file:///a").expect("vpath"),
///     VPath::parse("file:///b").expect("vpath"),
///     None,
///     None,
/// );
/// // Still planning: there is nothing to approve, so it is not offered.
/// assert_eq!(hint_id(&v), "sync-hint-done");
/// ```
#[must_use]
pub fn hint_id(view: &SyncView) -> &'static str {
    // While the daemon is WRITING, `Esc` requests cancellation and does not
    // close: saying "Esc closes" there would be offering to leave a write in
    // progress.
    if view.is_submitted() || matches!(view.state, SyncState::Applying(_)) {
        return "sync-hint-applying";
    }
    if view.confirming.is_some() {
        "sync-hint-confirm"
    } else if view.awaiting_approval() && view.can_approve() {
        "sync-hint"
    } else {
        "sync-hint-done"
    }
}

/// Where the synchronisation dialog IS: planning, waiting for a human, running
/// or finished — one sentence, for whatever a frontend uses as a footer.
///
/// Shared by both frontends (#161) for the same reason
/// [`crate::compare::status_line`] is, and this one carries more weight: its
/// `Ready` arm is where "this plan can be approved" reaches a human as words,
/// and a second copy of that decision is a second answer to the only question
/// on this screen that writes to a disk. It lived in `norte-tui`'s renderer
/// until the GUI needed the same footer.
///
/// Three things it deliberately does NOT re-derive:
///
/// * approvability is [`SyncPlan::can_approve`] over the plan the state still
///   holds — and the state is what chose this arm, so a plan that has already
///   been spent is `Applying`/`Applied` here and never `Ready`;
/// * whether the applied plan can be undone is [`Applied::is_undoable`], which
///   reads the report's `batch_id`, and NEVER the plan's outlook: a report with
///   no batch means nothing was journalled, whatever the plan promised before
///   it ran;
/// * the localised error of a failed task is [`SyncView::error`], which each
///   frontend fills the way it sanitises text.
///
/// The frontend adds its own padding; this returns the sentence alone.
///
/// ```
/// use norte_frontend::sync::{SyncView, status_line};
/// use norte_i18n::Lang;
/// use norte_proto::{TaskId, VPath};
/// use norte_proto::methods::SyncMode;
/// let v = SyncView::new(
///     TaskId::new(1),
///     SyncMode::Update,
///     VPath::parse("file:///a").expect("vpath"),
///     VPath::parse("file:///b").expect("vpath"),
///     None,
///     None,
/// );
/// // Freshly opened: planning, with zero steps.
/// assert!(!status_line(&v, Lang::En).is_empty());
/// ```
#[must_use]
pub fn status_line(view: &SyncView, lang: Lang) -> String {
    let plan = view.state.plan();
    let n = plan.map_or_else(
        || match &view.state {
            SyncState::Planning(p) => p.len(),
            _ => 0,
        },
        |p| {
            p.steps()
                .len()
                .saturating_add(usize::try_from(p.dropped()).unwrap_or(usize::MAX))
        },
    );
    let n = n.to_string();
    match (&view.state, view.run) {
        // **The report rules, and it goes FIRST** — but WITHOUT losing how it
        // ended.
        //
        // An apply cut short halfway through HAS a report (what got applied
        // up to the cut stays, journalled) and it is exactly the state where
        // the reader most needs to know how much got written. With this arm
        // behind `Cancelled`'s, the screen said "cancelled — N steps had
        // arrived, and there is no plan to approve" —a sentence about the
        // PLAN, already approved— over the APPLY's failure list.
        //
        // And the outcome chooses the SENTENCE instead of getting lost:
        // "cancelled after applying N" and "failed after applying N" say
        // both halves. Putting the `Failed` arm in front hid the count of a
        // `Mirror` that deleted forty trees and then died, which is the
        // place they can least afford to be hidden; putting it behind with
        // no sentences of its own erased the word "cancelled", and the color
        // would have been the only signal — on the branch whose previous
        // commit is titled "readable without color" (#161, phase C2 task 4;
        // rust review MAJOR-2 and security review MAJOR-4).
        (SyncState::Applied(a), run) => {
            let done = a.report().done.to_string();
            let failed = a.report().failed.to_string();
            let undo = if a.is_undoable() {
                "undoable"
            } else {
                "not-undoable"
            };
            let id = match run {
                SyncRunState::Cancelled => format!("sync-status-applied-cut-{undo}"),
                SyncRunState::Failed => format!("sync-status-applied-failed-{undo}"),
                SyncRunState::Running | SyncRunState::Done => {
                    format!("sync-status-applied-{undo}")
                }
            };
            ta_in(
                lang,
                &id,
                &[
                    ("done", &done),
                    ("failed", &failed),
                    ("error", view.error.as_deref().unwrap_or_default()),
                ],
            )
        }
        // Sin informe, el fallo manda: un error tiene que llegar entero, y no
        // hay recuento que lo pueda sustituir.
        (_, SyncRunState::Failed) => ta_in(
            lang,
            "sync-status-failed",
            &[("error", view.error.as_deref().unwrap_or_default())],
        ),
        (_, SyncRunState::Cancelled) => ta_in(lang, "sync-status-cancelled", &[("n", &n)]),
        (SyncState::Planning(_), _) => ta_in(lang, "sync-planning", &[("n", &n)]),
        // `view.can_approve()` and not `p.can_approve()`: ONE single
        // function answers that question, and it is the same one the key
        // hint consults. With the plan's alone, this arm and that one could
        // disagree as soon as the Task's outcome came into play.
        (SyncState::Ready(_), _) => {
            let id = if view.can_approve() {
                "sync-status-ready"
            } else {
                "sync-status-not-approvable"
            };
            ta_in(lang, id, &[("n", &n)])
        }
        (SyncState::Applying(_), _) => t_in(lang, "sync-status-applying"),
    }
}
