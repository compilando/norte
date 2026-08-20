//! El plan y lo que se puede prometer sobre él.
//!
//! Aquí viven la integridad (¿lo que se enseña es TODO lo que se va a hacer?),
//! el pronóstico de deshacer, y las cuatro fases por las que pasa una
//! sincronización desde que se pide hasta que termina.

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::TaskId;
use norte_proto::methods::{
    DestTrash, SyncCounts, SyncPlanDone, SyncReason, SyncReportResult, SyncStep, SyncStepKind,
};

use super::{StepUndo, step_undo, trash_label};

/// What the undo can give back for the plan AS A WHOLE.
///
/// A function of [`DestTrash`] first and of the counters second, never of the
/// per-step `reversal` column — see the module docs for why that column cannot
/// answer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UndoOutlook {
    /// Everything this plan does can be undone.
    Full,
    /// Some of it can. Not something this core produces — a plan against a
    /// restorable trash has no irreversible steps — but a newer daemon could,
    /// and "some" must not round up to "all".
    Partial,
    /// Nothing comes back. Both of the two ways to get here (an opaque trash,
    /// no trash) end with the same answer for the human, which is why
    /// [`DestTrash`] keeps them apart and this does not.
    Nothing,
    /// This build cannot tell.
    Unclear,
}

impl UndoOutlook {
    /// The outlook for a plan closed with these counts against this
    /// destination.
    ///
    /// This is the COUNTS-only answer. It cannot see the steps, so it cannot
    /// know that one of them carries a reversal this build has no name for —
    /// use [`SyncPlan::outlook`], which refines it with the steps that arrived
    /// and is what a dialog must print.
    ///
    /// ```
    /// use norte_frontend::sync::UndoOutlook;
    /// use norte_proto::methods::{DestTrash, SyncCounts};
    /// let copies = SyncCounts { copy: 5, ..SyncCounts::default() };
    /// assert_eq!(UndoOutlook::of(DestTrash::Restorable, &copies), UndoOutlook::Full);
    /// // The same five copies, and nothing comes back.
    /// assert_eq!(UndoOutlook::of(DestTrash::Absent, &copies), UndoOutlook::Nothing);
    /// ```
    #[must_use]
    pub fn of(dest_trash: DestTrash, counts: &SyncCounts) -> Self {
        match dest_trash {
            DestTrash::Restorable => {
                if counts.irreversible == 0 {
                    Self::Full
                } else {
                    Self::Partial
                }
            }
            // An opaque trash still holds what it buried and the human can dig
            // it out by hand; nothing NORTE does gives it back. No trash at
            // all destroys the overwrites and leaves the copies where they
            // are. Same OUTLOOK, two different situations — which is why the
            // summary prints `trash_label` underneath rather than letting this
            // one word stand for both.
            DestTrash::Opaque | DestTrash::Absent => Self::Nothing,
            _ => Self::Unclear,
        }
    }

    /// The outlook for an APPLIED batch, read from its report alone (#208,
    /// 0.42.0's `dest_trash` on `SyncReportResult`).
    ///
    /// It exists for the readers that never saw a `sync.plan_done`: `norte
    /// sync --json`, `print_sync_report`, an agent reading a report through
    /// MCP, a second frontend attached to a session it did not start. Before
    /// the field existed they had to say nothing at all, because a report of
    /// five copies against a destination with no trash and one against a
    /// restorable trash are byte for byte the same report — and one comes back
    /// whole and the other not at all.
    ///
    /// `batch_id` is read FIRST and it overrules the trash: with no batch
    /// there is no journal unit to undo, so nothing comes back whatever the
    /// destination could have offered. That is not the same statement as "the
    /// destination has no trash", and the two are distinguished on purpose —
    /// this one means the apply died before it could open a unit.
    ///
    /// The counts a plan has are not here (a report counts steps done, not
    /// steps irreversible), so this answers `Full` where
    /// [`Self::of`] could have said `Partial`. A caller holding the plan
    /// should keep using [`SyncPlan::outlook`], which sees the steps.
    ///
    /// ```
    /// use norte_frontend::sync::UndoOutlook;
    /// use norte_proto::methods::{DestTrash, SyncReportResult};
    ///
    /// let base = SyncReportResult {
    ///     done: 5,
    ///     failed: 0,
    ///     skipped: 0,
    ///     bytes: 100,
    ///     failures: Vec::new(),
    ///     batch_id: Some(7),
    ///     dest_trash: DestTrash::Restorable,
    /// };
    /// assert_eq!(UndoOutlook::of_report(&base), UndoOutlook::Full);
    ///
    /// // Sin unidad de journal no hay nada que deshacer, diga lo que diga la
    /// // papelera del destino.
    /// let sin_lote = SyncReportResult { batch_id: None, ..base.clone() };
    /// assert_eq!(UndoOutlook::of_report(&sin_lote), UndoOutlook::Nothing);
    ///
    /// let sin_papelera = SyncReportResult { dest_trash: DestTrash::Absent, ..base };
    /// assert_eq!(UndoOutlook::of_report(&sin_papelera), UndoOutlook::Nothing);
    /// ```
    #[must_use]
    pub fn of_report(report: &norte_proto::methods::SyncReportResult) -> Self {
        if report.batch_id.is_none() {
            return Self::Nothing;
        }
        Self::of(report.dest_trash, &SyncCounts::default())
    }

    /// The stable id a Fluent message and a config name it by.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Nothing => "nothing",
            Self::Unclear => "unclear",
        }
    }
}

/// Whether the steps that arrived account for the plan the daemon closed.
///
/// Cross-checking is not paranoia about the transport — task 10 made a dropped
/// batch close the feed without a `sync.plan_done`, so a truncated plan should
/// not be approvable at all. It is that a model which can count should count:
/// the alternative is a dialog whose summary line and whose list disagree, and
/// a human approving the smaller of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanIntegrity {
    /// Every step arrived, every one of them has a name this build knows, and
    /// every counter the daemon closed with is the one these steps add up to.
    Complete,
    /// The steps received do not add up to [`SyncPlanDone::counts`]'s per-class
    /// counters.
    Mismatch {
        /// How many steps arrived.
        received: u64,
        /// How many the daemon says the plan has.
        counted: u64,
    },
    /// The classes add up and something else does not: `irreversible`,
    /// `bytes` or `unmeasured_steps`.
    ///
    /// It matters because those are the numbers the dialog LEADS with. A plan
    /// closed with `irreversible: 0` over steps that each say they cannot be
    /// undone would otherwise be headlined "everything here can be undone".
    Contradictory,
    /// Some steps are of a class this build cannot name
    /// ([`SyncStepKind::Unknown`]): the daemon is newer, and approving a plan
    /// whose contents cannot be painted is approving blind.
    Unnameable {
        /// How many such steps arrived.
        steps: u64,
    },
    /// Some steps contradict themselves
    /// ([`SyncStep::shape_is_consistent`] is `false`): a writing class with no
    /// reversal, a `Skip` claiming one, a `dest_rel` that repeats `rel`.
    ///
    /// The wire tolerates one so a bad token cannot kill a batch of 256; an
    /// approval dialog does not, because it cannot say what such a step will
    /// do.
    Malformed {
        /// How many such steps arrived.
        steps: u64,
    },
    /// Two or more steps arrived sharing the same [`SyncStep::id`] (#194).
    ///
    /// `integrity_of` cross-checks every OTHER counter against the daemon
    /// precisely because the daemon is not trusted to be self-consistent —
    /// id uniqueness is one more such counter, not a special case. It matters
    /// on this frontend specifically because a step's element id is built
    /// from `step.id` (`sync-step-{id}`, `sync-step-{id}-{rel|dest}`), and
    /// GPUI's own accessibility guide says two nodes under the same ancestors
    /// with the same id collapse to ONE AccessKit global id — in a release
    /// build the second is silently dropped. `SyncPlan::select` and
    /// `selected_step` compound it: both take the FIRST match, so a repeated
    /// id also gives a cursor that can never reach the second row. Refusing
    /// the plan is cheaper than painting it half-reachable.
    DuplicateIds {
        /// How many steps arrived carrying an id an EARLIER step already
        /// used, in wire order.
        steps: u64,
    },
}

impl PlanIntegrity {
    /// `true` only for [`PlanIntegrity::Complete`].
    #[must_use]
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }
}

/// How many steps a [`SyncCounts`] describes, across every class.
pub(super) fn total_steps(counts: &SyncCounts) -> u64 {
    counts
        .create_dir
        .saturating_add(counts.copy)
        .saturating_add(counts.overwrite)
        .saturating_add(counts.delete_tree)
        .saturating_add(counts.skip)
        .saturating_add(counts.unknown_kind)
}

/// How many step BODIES a frontend keeps while a plan streams in (#196).
///
/// The wire caps a batch
/// ([`SYNC_STEPS_MAX_BATCH`](norte_proto::methods::SYNC_STEPS_MAX_BATCH)) and
/// the `include` of a
/// report, but NOT the number of steps a plan streams: a `Mirror` over a
/// hostile —or merely enormous— remote tree is as many steps as it has
/// entries, and each one carries two relative paths. Held whole, that is
/// unbounded client memory, and in the GUI it is also a render tree.
///
/// What the cap does NOT touch is any number the human decides on: the
/// counters, the integrity verdict and the confirmation are all incremental
/// over EVERY step that arrives, capped or not. What is lost is the tail of
/// the LIST, which is a display concern — nobody reads step 200 000 — and the
/// plan itself never left the daemon's spool anyway: `sync.apply` sends a
/// hash, not steps.
pub const PLAN_STEPS_RETAINED_MAX: usize = 20_000;

/// A plan being received: the steps so far, and what they add up to.
///
/// The counters are summed with [`SyncCounts::add`] — the same function the
/// daemon used — so the comparison in [`SyncState::on_plan_done`] is between
/// two numbers produced by one rule, not by two.
#[derive(Debug, Clone, Default)]
pub struct Planning {
    /// The task the plan is running under, once it is known.
    pub(super) task_id: Option<TaskId>,
    pub(super) steps: Vec<SyncStep>,
    pub(super) counts: SyncCounts,
    pub(super) unreadable: u64,
    pub(super) malformed: u64,
    /// The highest id seen so far. The wire says a step's id is MONOTONIC
    /// within one plan, so the maximum is all it takes to catch a repeat as it
    /// arrives (#194) — and it takes eight bytes instead of a set that grows
    /// with the plan, which is the other half of #196.
    pub(super) max_id: Option<u64>,
    /// How many steps arrived with an id that did not advance past
    /// [`Self::max_id`]: a repeat, or an order the wire forbids.
    pub(super) duplicate_ids: u64,
    /// Steps counted but NOT retained, because
    /// [`PLAN_STEPS_RETAINED_MAX`] was already reached.
    pub(super) dropped: u64,
}

impl Planning {
    /// A plan about to start under `task_id`.
    #[must_use]
    pub fn new(task_id: TaskId) -> Self {
        Self {
            task_id: Some(task_id),
            ..Self::default()
        }
    }

    /// The task, once `sync.plan` has answered with one.
    #[must_use]
    pub fn task_id(&self) -> Option<TaskId> {
        self.task_id
    }

    /// Does a notification for `task_id` belong to this plan?
    ///
    /// A [`Planning`] with no task accepts everything — that is the
    /// constructed-plan door ([`SyncState::ready`] and tests), where the
    /// caller already did the correlating. One WITH a task accepts only its
    /// own, which is what stops a second plan's steps from being appended to
    /// the first plan's list.
    #[must_use]
    pub fn owns(&self, task_id: TaskId) -> bool {
        self.task_id.is_none_or(|mine| mine == task_id)
    }

    /// Appends steps, counting them exactly the way the daemon counted them.
    pub fn extend(&mut self, steps: impl IntoIterator<Item = SyncStep>) {
        for step in steps {
            self.counts.add(&step);
            if step.kind == SyncStepKind::Skip && step.reason == Some(SyncReason::Unreadable) {
                self.unreadable = self.unreadable.saturating_add(1);
            }
            if !step.shape_is_consistent() {
                self.malformed = self.malformed.saturating_add(1);
            }
            // #194: the daemon is not trusted to hand out unique ids any more
            // than it is trusted to hand out consistent shapes above — a
            // repeat is counted the same incremental way, across batches. The
            // maximum is enough because the id is monotonic BY CONTRACT: an id
            // that repeats an earlier one cannot be above the maximum, so
            // every repeat is caught, and a non-monotonic id that repeats
            // nothing is a broken contract counted under the same heading.
            if self.max_id.is_some_and(|max| step.id <= max) {
                self.duplicate_ids = self.duplicate_ids.saturating_add(1);
            } else {
                self.max_id = Some(step.id);
            }
            // #196: past the cap the step is COUNTED and dropped. Everything
            // the human decides on was already folded in above.
            if self.steps.len() < PLAN_STEPS_RETAINED_MAX {
                self.steps.push(step);
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }

    /// The steps received so far.
    #[must_use]
    pub fn steps(&self) -> &[SyncStep] {
        &self.steps
    }

    /// How many steps arrived — retained or dropped by
    /// [`PLAN_STEPS_RETAINED_MAX`]. It is what the human is told is arriving,
    /// so it counts what arrived, not what is still in the `Vec`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps
            .len()
            .saturating_add(usize::try_from(self.dropped).unwrap_or(usize::MAX))
    }

    /// How many steps were counted but not retained.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether nothing has arrived yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.dropped == 0
    }
}

/// A plan that closed and is waiting for a human.
#[derive(Debug, Clone)]
pub struct SyncPlan {
    pub(super) done: SyncPlanDone,
    pub(super) steps: Vec<SyncStep>,
    pub(super) integrity: PlanIntegrity,
    pub(super) unreadable: u64,
    pub(super) selected: Option<u64>,
    /// Steps counted but not retained ([`PLAN_STEPS_RETAINED_MAX`], #196).
    pub(super) dropped: u64,
    /// La primera fila VISIBLE de la lista de pasos, PEGAJOSA (#210): se
    /// arrastra solo cuando el cursor se sale — ver
    /// [`crate::viewport::sticky_offset`]. Un plan tiene cientos de miles de
    /// pasos, así que es la lista donde más se nota que el cursor viva clavado
    /// en la última fila.
    pub(super) viewport_offset: usize,
}

impl SyncPlan {
    /// The closing notification, verbatim.
    #[must_use]
    pub fn done(&self) -> &SyncPlanDone {
        &self.done
    }

    /// What the plan adds up to, according to the daemon.
    #[must_use]
    pub fn counts(&self) -> &SyncCounts {
        &self.done.counts
    }

    /// Deja la ventana de la lista de pasos lista para pintar `rows` filas
    /// (#210): se arrastra solo cuando el cursor se sale.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        let cursor = self
            .selected
            .and_then(|id| self.steps.iter().position(|s| s.id == id))
            .unwrap_or(0);
        self.viewport_offset =
            crate::viewport::sticky_offset(self.viewport_offset, cursor, self.steps.len(), rows);
    }

    /// La primera fila visible de la lista de pasos — ver
    /// [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// How many steps were counted but NOT retained
    /// ([`PLAN_STEPS_RETAINED_MAX`], #196), so a pane can say that the list it
    /// is painting stops before the plan does. The counters, the integrity
    /// verdict and the confirmation cover every step; only the list is cut.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Which trash the destination has — the fact every claim here is a
    /// function of.
    #[must_use]
    pub fn dest_trash(&self) -> DestTrash {
        self.done.dest_trash
    }

    /// Every step that arrived, in wire order (which is execution order).
    #[must_use]
    pub fn steps(&self) -> &[SyncStep] {
        &self.steps
    }

    /// Whether the steps account for the counts.
    #[must_use]
    pub fn integrity(&self) -> PlanIntegrity {
        self.integrity
    }

    /// How many entries the walk could not read and the plan therefore does
    /// NOT cover.
    #[must_use]
    pub fn unreadable(&self) -> u64 {
        self.unreadable
    }

    /// What the undo would give back if this plan ran — the answer a dialog
    /// prints.
    ///
    /// [`UndoOutlook::of`] reads the counters; this also reads the STEPS, and
    /// downgrades to [`UndoOutlook::Unclear`] as soon as one of them is a step
    /// this build cannot judge (a reversal or a class a newer daemon named).
    /// Otherwise a plan of five copies, one of them carrying an unrecognised
    /// reversal, would be headlined "everything here can be undone" over a row
    /// glyphed "this version cannot say" — a promise made about an admitted
    /// unknown, which is the one thing this module refuses to do.
    #[must_use]
    pub fn outlook(&self) -> UndoOutlook {
        let base = UndoOutlook::of(self.done.dest_trash, &self.done.counts);
        if matches!(base, UndoOutlook::Full | UndoOutlook::Partial)
            && self
                .steps
                .iter()
                .any(|s| step_undo(s, self.done.dest_trash) == StepUndo::Unclear)
        {
            return UndoOutlook::Unclear;
        }
        base
    }

    /// Whether the plan may be approved.
    ///
    /// Three conditions, and the first is the wire's:
    ///
    /// 1. [`SyncPlanDone::executable`], read verbatim and never deduced from
    ///    `blockers` — a future blocker with no name to show must still stop
    ///    the plan (the same rule `fs.rename_batch` follows).
    /// 2. The steps received account for the counts
    ///    ([`PlanIntegrity::Complete`]).
    /// 3. There is something to do. An empty plan is not an error and not a
    ///    button either.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        self.done.executable && self.integrity.is_complete() && self.acting() > 0
    }

    /// How many steps actually write something.
    #[must_use]
    pub fn acting(&self) -> u64 {
        let c = &self.done.counts;
        c.create_dir
            .saturating_add(c.copy)
            .saturating_add(c.overwrite)
            .saturating_add(c.delete_tree)
    }

    /// The step the cursor is on.
    #[must_use]
    pub fn selected_id(&self) -> Option<u64> {
        self.selected
    }

    /// The selected step.
    #[must_use]
    pub fn selected_step(&self) -> Option<&SyncStep> {
        let id = self.selected?;
        self.steps.iter().find(|s| s.id == id)
    }

    /// Selects a step by id. A no-op if no such step arrived — the
    /// alternative is a cursor naming a row that is not there. Ids, never
    /// indices: [`SyncStep::id`] is stable and a filter must not renumber it.
    pub fn select(&mut self, id: u64) {
        if self.steps.iter().any(|s| s.id == id) {
            self.selected = Some(id);
        }
    }

    /// Moves the cursor `delta` steps, clamped at both ends.
    pub fn move_by(&mut self, delta: isize) {
        if self.steps.is_empty() {
            return;
        }
        let from = self
            .selected
            .and_then(|id| self.steps.iter().position(|s| s.id == id))
            .map_or(0isize, |i| isize::try_from(i).unwrap_or(isize::MAX));
        let last = isize::try_from(self.steps.len() - 1).unwrap_or(isize::MAX);
        let to = from.saturating_add(delta).clamp(0, last);
        let index = usize::try_from(to).unwrap_or(0);
        self.selected = self.steps.get(index).map(|s| s.id);
    }

    /// The lines of the approval summary, in reading order.
    ///
    /// What leads, and why:
    ///
    /// 1. **What comes back**, because it is the one thing a human cannot
    ///    recover from getting wrong, and because the per-step column cannot
    ///    say it. When the answer is not [`UndoOutlook::Full`] a second line
    ///    says WHICH destination this is ([`trash_label`]): "in the system
    ///    trash, by hand" and "gone" are the same outlook and very different
    ///    news.
    /// 2. **How many steps are irreversible**, on a line of its own.
    /// 3. What the plan does, and how many bytes — `bytes` NEVER alone: a
    ///    total that hides `unmeasured_steps` files of unknown size is a
    ///    confident lie, and on `file://` it is the normal case.
    /// 4. **How many entries could not be read**, because a plan can be
    ///    complete and executable while a whole subtree was never seen (an
    ///    unreadable listing is a `Skip`, not a blocker).
    /// 5. Why it cannot run, if it cannot — including the fact that blockers
    ///    are never filtered by `include`, so a selection of three files can
    ///    come back blocked by something forty thousand rows away.
    ///
    /// A line is a whole sentence: a caller may wrap them but must not
    /// concatenate them.
    #[must_use]
    pub fn summary_lines(&self, lang: Lang) -> Vec<String> {
        let c = &self.done.counts;
        let mut lines = Vec::new();
        if self.acting() > 0 {
            let outlook = self.outlook();
            lines.push(t_in(lang, &format!("sync-outlook-{}", outlook.id())));
            if outlook != UndoOutlook::Full {
                lines.push(trash_label(self.done.dest_trash, lang));
            }
        }
        if c.irreversible > 0 {
            lines.push(ta_in(
                lang,
                "sync-summary-irreversible",
                &[("n", &c.irreversible.to_string())],
            ));
        }
        lines.push(ta_in(
            lang,
            "sync-summary-actions",
            &[
                ("copy", &c.copy.to_string()),
                ("overwrite", &c.overwrite.to_string()),
                ("createdir", &c.create_dir.to_string()),
                ("deletetree", &c.delete_tree.to_string()),
                ("skip", &c.skip.to_string()),
            ],
        ));
        // Only the two classes that WRITE content have bytes to talk about. A
        // pure-`Mirror` deletion plan saying "0 B to write" reads as "this
        // does nothing".
        if c.copy > 0 || c.overwrite > 0 {
            lines.push(match c.exact_bytes() {
                Some(bytes) => ta_in(
                    lang,
                    "sync-summary-bytes",
                    &[("bytes", &crate::human_bytes(bytes))],
                ),
                // A lower bound plus the size of the ignorance. Never a
                // confident zero: an orphan row is not hydrated (#157) and
                // `norte-vfs-local` lists with no size at all.
                None => ta_in(
                    lang,
                    "sync-summary-bytes-partial",
                    &[
                        ("bytes", &crate::human_bytes(c.bytes)),
                        ("n", &c.unmeasured_steps.to_string()),
                    ],
                ),
            });
        }
        if self.unreadable > 0 {
            lines.push(ta_in(
                lang,
                "sync-summary-unreadable",
                &[("n", &self.unreadable.to_string())],
            ));
        }
        match self.integrity {
            PlanIntegrity::Complete => {}
            PlanIntegrity::Mismatch { received, counted } => lines.push(ta_in(
                lang,
                "sync-summary-mismatch",
                &[
                    ("received", &received.to_string()),
                    ("n", &counted.to_string()),
                ],
            )),
            PlanIntegrity::Unnameable { steps } => lines.push(ta_in(
                lang,
                "sync-summary-unnameable",
                &[("n", &steps.to_string())],
            )),
            PlanIntegrity::Malformed { steps } => lines.push(ta_in(
                lang,
                "sync-summary-malformed",
                &[("n", &steps.to_string())],
            )),
            PlanIntegrity::DuplicateIds { steps } => lines.push(ta_in(
                lang,
                "sync-summary-duplicate-ids",
                &[("n", &steps.to_string())],
            )),
            PlanIntegrity::Contradictory => lines.push(t_in(lang, "sync-summary-contradictory")),
        }
        // #196: the list stops before the plan does. Said out loud, because a
        // list that ends without saying so reads as the whole plan — and the
        // steps it hides are as approvable as the ones it shows.
        if self.dropped > 0 {
            lines.push(ta_in(
                lang,
                "sync-summary-list-truncated",
                &[
                    ("shown", &self.steps.len().to_string()),
                    ("hidden", &self.dropped.to_string()),
                ],
            ));
        }
        if !self.done.executable {
            lines.push(ta_in(
                lang,
                "sync-summary-blocked",
                &[("n", &self.done.blockers_total.to_string())],
            ));
        }
        lines
    }

    /// The SECOND question, when this plan deserves one — [`None`] when it
    /// does not, so a routine update is one keystroke.
    ///
    /// Two things earn it, and both are "you may not be able to take this
    /// back":
    ///
    /// * the plan DELETES trees from the destination (`Mirror`, which the
    ///   counters name: `Update` never emits one), or
    /// * the undo will not give all of it back.
    ///
    /// **The question says exactly what the summary said**, which is why there
    /// is a branch per [`UndoOutlook`] and not a `Full`/not-`Full` split: a
    /// dialog whose headline reads "some of this can be undone" over a
    /// confirmation that reads "none of it can" teaches the reader to skip
    /// both. [`UndoOutlook::Partial`] names how many steps are irreversible;
    /// [`UndoOutlook::Unclear`] says it cannot tell rather than asserting the
    /// worst as a fact.
    ///
    /// A plan that cannot be approved has no second question: there is no
    /// first one either.
    #[must_use]
    pub fn confirmation(&self, lang: Lang) -> Option<Confirmation> {
        if !self.can_approve() {
            return None;
        }
        let trees = self.done.counts.delete_tree;
        let irreversible = self.done.counts.irreversible.to_string();
        let acting = self.acting().to_string();
        let (id, args): (&'static str, Vec<(&str, String)>) = match (trees, self.outlook()) {
            (0, UndoOutlook::Full) => return None,
            (0, UndoOutlook::Partial) => ("sync-confirm-partial", vec![("n", irreversible)]),
            (0, UndoOutlook::Nothing) => ("sync-confirm-no-way-back", vec![("n", acting)]),
            (0, UndoOutlook::Unclear) => ("sync-confirm-unclear", vec![("n", acting)]),
            (n, UndoOutlook::Full) => ("sync-confirm-delete", vec![("n", n.to_string())]),
            (n, UndoOutlook::Partial) => (
                "sync-confirm-delete-partial",
                vec![("n", n.to_string()), ("steps", irreversible)],
            ),
            (n, UndoOutlook::Nothing) => ("sync-confirm-delete-final", vec![("n", n.to_string())]),
            (n, UndoOutlook::Unclear) => {
                ("sync-confirm-delete-unclear", vec![("n", n.to_string())])
            }
        };
        let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        Some(Confirmation {
            id,
            text: ta_in(lang, id, &args),
        })
    }
}

/// The second question a dangerous plan asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// The Fluent id the text came from — for a test, a log, or a frontend
    /// that wants its own wording.
    pub id: &'static str,
    /// The question, localised.
    pub text: String,
}

/// A plan that was approved and is executing.
#[derive(Debug, Clone)]
pub struct Applying {
    pub(super) plan: SyncPlan,
    pub(super) task_id: TaskId,
}

impl Applying {
    /// The plan being applied — the one the human saw.
    #[must_use]
    pub fn plan(&self) -> &SyncPlan {
        &self.plan
    }

    /// The `sync.apply` task.
    #[must_use]
    pub fn task_id(&self) -> TaskId {
        self.task_id
    }
}

/// A plan that finished, and what it did.
#[derive(Debug, Clone)]
pub struct Applied {
    pub(super) plan: SyncPlan,
    pub(super) report: SyncReportResult,
}

impl Applied {
    /// The plan that ran.
    ///
    /// Its [`SyncPlan::summary_lines`] describe what was ABOUT to happen and
    /// are stale here — in particular its outlook, which
    /// [`Applied::is_undoable`] supersedes.
    #[must_use]
    pub fn plan(&self) -> &SyncPlan {
        &self.plan
    }

    /// What `sync.report` answered.
    #[must_use]
    pub fn report(&self) -> &SyncReportResult {
        &self.report
    }

    /// Is there anything to undo at all?
    ///
    /// `false` when the apply died before opening a journal batch
    /// ([`SyncReportResult::batch_id`] absent), whatever the plan promised
    /// beforehand. A plan can close [`UndoOutlook::Full`] and still leave
    /// nothing undoable, so this is what a frontend prints after the fact —
    /// and it is a NECESSARY condition, not a sufficient one: the undo still
    /// blocks per entry on a path that drifted since.
    #[must_use]
    pub fn is_undoable(&self) -> bool {
        self.report.batch_id.is_some() && self.plan.outlook() != UndoOutlook::Nothing
    }
}
