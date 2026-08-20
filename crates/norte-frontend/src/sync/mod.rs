//! Presentation of a directory SYNCHRONISATION — the approval dialog's model,
//! pure and testable without a terminal (hard rule 7).
//!
//! Spec 1 gave the diff pane a read-only answer; this is the pane that turns it
//! into writes, so the one thing this module exists to get right is **not
//! promising what the undo cannot deliver**. Everything else here — glyphs, a
//! cursor, a summary — follows [`crate::compare`]'s patterns.
//!
//! # Why a step's `reversal` is not the answer
//!
//! [`SyncStep::reversal`] says how a step would come back *if the destination
//! could take it back*, and that is a different question from whether it will.
//! A [`SyncStepKind::Copy`] onto a destination with no trash carries
//! [`StepReversal::Delete`] — deliberately, because the same step IS reversible
//! where a trash exists — and yet undoing a `created` entry also routes through
//! the trash (#65), so with no trash the undo SKIPS it and the copy stays. Two
//! plans of nothing but copies, byte for byte identical on the wire, one of
//! which reverts entirely and one of which reverts nothing.
//!
//! What separates them is [`DestTrash`], which is why it travels on
//! [`norte_proto::methods::SyncPlanDone`] and why every claim this module makes is a function of the
//! PAIR `(step, dest_trash)`:
//!
//! | destination | what this model says |
//! | --- | --- |
//! | `file://` on Linux/BSD, `sftp://`/object with the logical trash | [`UndoOutlook::Full`] — overwrites, deletions and copies can all be undone |
//! | `file://` on macOS/Windows ([`DestTrash::Opaque`]) | [`UndoOutlook::Nothing`]; every acting step is `Irreversible` and says so — and [`trash_label`] adds that what was replaced is still in the system trash, by hand |
//! | no trash at all ([`DestTrash::Absent`]) | [`UndoOutlook::Nothing`]: overwrites and deletions are gone, and the copies are [`StepUndo::LeftBehind`] — the undo will not remove them |
//!
//! [`UndoOutlook::Full`] is a statement about the PLAN, not a guarantee per
//! entry: an entry the destination's trash cannot name, and a path that
//! changed between the apply and the undo, are both blocked and NAMED in the
//! undo's report instead of being touched. The strings say "you can undo
//! this", never "this will come back whatever happens".
//!
//! # Two more things this model refuses to assume
//!
//! * **The steps that arrived are cross-checked against
//!   [`norte_proto::methods::SyncPlanDone::counts`]**, not trusted. The feed closes on a dropped
//!   batch (task 10), but a model that can count should count: a plan whose
//!   steps do not add up to what the daemon closed with cannot be approved.
//! * **[`SyncStep::rel`] is not always relative to the source root.** A
//!   `DeleteTree` and the `Skip` of a destination listing that would not read
//!   are measured against the DESTINATION, and the step carries no side —
//!   [`anchor_of`] answers [`RelAnchor::Either`] rather than letting a pane
//!   paint them in a column they do not belong to.

mod labels;
mod plan;
mod rel;
mod render;
mod state;

// Un `sync.rs` de 4.567 líneas se partió por lo que cada trozo RESPONDE, no
// por tamaño: etiquetas, anclaje, celdas, plan y estado. Se re-exporta todo
// desde aquí para que ningún call-site tenga que saber en qué trozo cayó lo
// que usa.
pub use labels::*;
pub use plan::*;
pub use rel::*;
pub use render::*;
pub use state::*;

use norte_proto::VPath;
use norte_proto::methods::{
    CompareRow, DestTrash, RelPath, SYNC_MAX_INCLUDE, StepReversal, SyncStep, SyncStepKind,
};

/// Why a selection of diff-pane rows cannot become a `SyncPlanParams::include`.
///
/// Both variants are refusals and neither is a truncation: a plan built from a
/// list the caller silently shortened is a plan the reader did not approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeError {
    /// More rows than [`SYNC_MAX_INCLUDE`], which the daemon refuses outright.
    TooMany {
        /// How many were marked.
        marked: usize,
        /// The cap.
        max: usize,
    },
    /// A marked row hangs from NEITHER root.
    ///
    /// Unreachable from a comparison of the two roots being synchronised, and
    /// refused rather than dropped precisely because of that: silently
    /// narrowing the list turns "these three rows" into an empty selection,
    /// which the pane then paints as "the two trees already agree". A lie on
    /// the screen that authorises writes is worse than a refusal.
    Unrooted,
    /// A marked row IS one of the roots.
    ///
    /// The root in an `include` list means "everything" ([`SYNC_MAX_INCLUDE`]'s
    /// filter treats it as the whole tree), so one such entry turns a narrow
    /// selection into a whole-tree plan — under `Mirror`, into "delete
    /// everything the source does not have" for a reader who marked one row.
    /// `RelPath::under` documents that its caller owns this decision; this is
    /// the caller, and it refuses.
    RootSelected,
}

/// The `include` list for `SyncPlanParams`, from the rows a reader marked.
///
/// `Ok(None)` means NO selection — the plan covers both trees, which on the
/// wire is the ABSENCE of the field. It is never `Ok(Some(vec![]))`: an empty
/// list is a selection of zero paths and produces a plan of zero steps, and the
/// two must not be confused.
///
/// # Which root each row is measured against
/// The SOURCE first, because the `rel` of every step that WRITES is measured
/// against it. Only a row with nothing on the source side falls back to the
/// destination — that is the orphan that only exists there, whose step is a
/// [`SyncStepKind::DeleteTree`], and whose `rel` the core measures against the
/// destination root. This is the request-side twin of [`anchor_of`], which
/// answers the same question for a step that came back; they are next to each
/// other so the two answers cannot drift.
///
/// # Errors
/// [`IncludeError`] — see its variants. Every one of them refuses rather than
/// narrowing.
///
/// ```
/// use norte_frontend::sync::include_from_rows;
/// use norte_proto::VPath;
/// let src = VPath::parse("file:///a").expect("src");
/// let dst = VPath::parse("file:///b").expect("dst");
/// // Nothing marked: the whole tree, and the field is absent.
/// assert_eq!(include_from_rows(&src, &dst, &[]), Ok(None));
/// ```
pub fn include_from_rows(
    source: &VPath,
    dest: &VPath,
    marked: &[&CompareRow],
) -> Result<Option<Vec<RelPath>>, IncludeError> {
    if marked.is_empty() {
        return Ok(None);
    }
    if marked.len() > SYNC_MAX_INCLUDE {
        return Err(IncludeError::TooMany {
            marked: marked.len(),
            max: SYNC_MAX_INCLUDE,
        });
    }
    let mut out = Vec::with_capacity(marked.len());
    for row in marked {
        let rel = [source, dest]
            .into_iter()
            .find_map(|root| {
                [row.left.as_ref(), row.right.as_ref()]
                    .into_iter()
                    .flatten()
                    .find_map(|entry| RelPath::under(root, &entry.path))
            })
            .ok_or(IncludeError::Unrooted)?;
        if rel.is_root() {
            return Err(IncludeError::RootSelected);
        }
        out.push(rel);
    }
    // Ordenada y sin repetidos: dos filas pueden nombrar la misma ruta (las dos
    // caras de una pareja), y un `include` estable es lo que hace que dos
    // selecciones idénticas produzcan un `plan_hash`.
    out.sort();
    out.dedup();
    Ok(Some(out))
}

/// The two roots of a synchronisation, in `(source, dest)` order, each with
/// the name reinterpretation (#57) of ITS OWN side.
///
/// Four named fields and not two pairs: `source` and `dest` are the same type
/// and so are the two encodings, so every transposition compiles — and
/// transposing THESE inverts which tree gets overwritten, which is the half of
/// a plan a human is being asked to approve. Same argument the GUI's
/// `Started`/`SyncEncodings` make one layer up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRoots {
    /// Where the entries are READ from.
    pub source: VPath,
    /// Where they are WRITTEN — the tree a `Mirror` deletes from.
    pub dest: VPath,
    /// The source pane's reinterpretation, frozen with the root.
    pub source_encoding: Option<norte_encoding::NameEncoding>,
    /// The destination pane's, which may be another one: two panes are two
    /// locations and can carry different overrides.
    pub dest_encoding: Option<norte_encoding::NameEncoding>,
}

/// The two panes a frontend has, told apart by FOCUS — the fallback
/// [`sync_roots`] uses when no diff pane is open.
///
/// The fields are named after the focus and not after a screen position on
/// purpose: "the pane on the left" is not what decides, and a frontend whose
/// focused pane is the right-hand one must not have to invert anything here.
#[derive(Debug, Clone, Copy)]
pub struct Panes<'a> {
    /// The pane with the focus. It is the SOURCE.
    pub focused_root: &'a VPath,
    /// Its reinterpretation (#57).
    pub focused_encoding: Option<norte_encoding::NameEncoding>,
    /// The other pane. It is the DESTINATION.
    pub other_root: &'a VPath,
    /// Its reinterpretation, which may differ.
    pub other_encoding: Option<norte_encoding::NameEncoding>,
}

/// Which two roots a synchronisation runs between, and in which direction.
///
/// **One rule, one place, both frontends** (#161). With a diff pane open its
/// ACTIVE side — the one `Tab` moves — is the source, and NOTHING is inferred
/// from the focus or from the order of the panes: the reader has a pane in
/// front of them whose active side is marked, and the plan has to agree with
/// what they are looking at. With no diff pane, the focused pane is the source
/// and the other is the destination, the same split
/// `request_compare`/`start_compare` use for left and right.
///
/// This lived in `norte-tui` until the GUI grew the branch that genuinely
/// decides. Two copies of it would be two answers to "which tree gets
/// overwritten", and the frontend that drifted would be overwriting the wrong
/// one — the cheapest possible bug to write and the most expensive to find,
/// since both copies produce a perfectly plausible plan.
///
/// The encodings travel WITH the roots and are never re-read from the panes
/// afterwards: a reader who pressed `Alt+E` to read a CP1251 share cannot get
/// `????.txt` back when they synchronise it (#57).
///
/// ```
/// use norte_frontend::sync::{Panes, sync_roots};
/// use norte_proto::VPath;
/// let izq = VPath::parse("file:///izq").expect("vpath");
/// let der = VPath::parse("file:///der").expect("vpath");
/// // Sin panel de diferencias: el pane con FOCO es el origen.
/// let r = sync_roots(
///     None,
///     &Panes {
///         focused_root: &der,
///         focused_encoding: None,
///         other_root: &izq,
///         other_encoding: None,
///     },
/// );
/// assert_eq!(r.source, der);
/// assert_eq!(r.dest, izq);
/// ```
#[must_use]
pub fn sync_roots(compare: Option<&crate::compare::CompareView>, panes: &Panes<'_>) -> SyncRoots {
    let Some(view) = compare else {
        return SyncRoots {
            source: panes.focused_root.clone(),
            dest: panes.other_root.clone(),
            source_encoding: panes.focused_encoding,
            dest_encoding: panes.other_encoding,
        };
    };
    // `Side::Right` y no un `_` que se lo trague todo: un lado que ESTA build
    // no sepa nombrar cae en el brazo de la izquierda, que es el default del
    // propio pane (`active_side` nace en `Left`), y no invierte el sentido de
    // una sincronización por una palabra nueva del wire.
    match view.pane.active_side() {
        norte_proto::methods::Side::Right => SyncRoots {
            source: view.right_root.clone(),
            dest: view.left_root.clone(),
            source_encoding: view.right_encoding,
            dest_encoding: view.left_encoding,
        },
        _ => SyncRoots {
            source: view.left_root.clone(),
            dest: view.right_root.clone(),
            source_encoding: view.left_encoding,
            dest_encoding: view.right_encoding,
        },
    }
}

/// What the undo would actually do with ONE step, once the destination's trash
/// is taken into account.
///
/// The type exists because [`SyncStep::reversal`] alone cannot answer it — see
/// the module docs. Every variant is a different sentence to a human, and none
/// of them is "probably".
///
/// ```
/// use norte_frontend::sync::{StepUndo, step_undo};
/// use norte_proto::methods::DestTrash;
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncStep, SyncStepKind};
/// let copy = SyncStep {
///     id: 1,
///     kind: SyncStepKind::Copy,
///     rel: RelPath::parse_wire("a.txt").expect("rel"),
///     dest_rel: None,
///     size: Some(10),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// // The SAME step, and two different truths.
/// assert_eq!(step_undo(&copy, DestTrash::Restorable), StepUndo::Reverts);
/// assert_eq!(step_undo(&copy, DestTrash::Absent), StepUndo::LeftBehind);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepUndo {
    /// The undo puts this back. Only ever true against a
    /// [`DestTrash::Restorable`] destination.
    Reverts,
    /// The step happens and the undo LEAVES IT THERE: the destination has no
    /// trash, so removing what the sync created could destroy work the human
    /// did afterwards (#65), and the undo counts it in
    /// `skipped_created_no_trash` instead. Nothing is lost — and nothing is
    /// taken back either.
    LeftBehind,
    /// The step cannot be undone, and the plan said so before it ran
    /// ([`StepReversal::Irreversible`], hard rule 4).
    Irreversible,
    /// There is nothing to undo: the step does not touch anything
    /// ([`SyncStepKind::Skip`]).
    Nothing,
    /// This build cannot tell — a step class or a reversal a newer daemon
    /// named, or a [`DestTrash`] it does not know. Never a promise: an unknown
    /// is not a "yes".
    Unclear,
}

/// What the undo would do with `step`, given the destination's trash.
///
/// The order of the arms is the whole safety argument, so it is written out
/// rather than left to a `match` reading:
///
/// 1. A step that DECLARES itself irreversible is irreversible, whatever its
///    class is and whatever trash the destination has. A class this build
///    cannot name does not make the declaration less final.
/// 2. A [`SyncStepKind::Skip`] undoes to nothing, because it does nothing.
/// 3. A class this build cannot name is [`StepUndo::Unclear`] — it could be
///    anything, so it is not painted as coming back.
/// 4. Only then does the reversal decide, and it decides TOGETHER with
///    `dest_trash`. `Delete` against [`DestTrash::Absent`] is the trap this
///    whole module exists for.
///
/// `Delete` against [`DestTrash::Opaque`] is unreachable from this core (an
/// opaque trash makes every acting step irreversible) and answers
/// [`StepUndo::Unclear`] rather than guessing which of the two neighbouring
/// meanings a future daemon intended.
///
/// ```
/// use norte_frontend::sync::{StepUndo, step_undo};
/// use norte_proto::methods::{DestTrash, SyncStepKind};
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncReason, SyncStep};
/// let skip = SyncStep {
///     id: 1,
///     kind: SyncStepKind::Skip,
///     rel: RelPath::parse_wire("a.txt").expect("rel"),
///     dest_rel: None,
///     size: None,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: None,
///     reason: Some(SyncReason::Unreadable),
/// };
/// assert_eq!(step_undo(&skip, DestTrash::Restorable), StepUndo::Nothing);
/// ```
#[must_use]
pub fn step_undo(step: &SyncStep, dest_trash: DestTrash) -> StepUndo {
    if step.reversal == Some(StepReversal::Irreversible) {
        return StepUndo::Irreversible;
    }
    match step.kind {
        SyncStepKind::Skip => return StepUndo::Nothing,
        SyncStepKind::CreateDir
        | SyncStepKind::Copy
        | SyncStepKind::Overwrite
        | SyncStepKind::DeleteTree => {}
        // A class this build cannot name. It is not `Skip`, so it probably
        // writes; it is not declared irreversible, so it probably comes back.
        // "Probably" is not something to paint.
        _ => return StepUndo::Unclear,
    }
    match (step.reversal, dest_trash) {
        // Both named reversals mean the same thing against a trash that can
        // give things back — one deletes what was created, the other digs out
        // what was buried, and the journal walks `seq` backwards so the order
        // sorts itself out.
        (Some(StepReversal::Delete | StepReversal::RestoreTrash), DestTrash::Restorable) => {
            StepUndo::Reverts
        }
        // THE trap: the wire says `delete` and the undo will not run it. The
        // rule it mirrors lives in `norte_core::undo` (the `delete` reversal
        // is gated on the provider declaring a trash, #65); if that rule ever
        // changes, this arm is the second place to change, and nothing will
        // fail to compile to say so.
        (Some(StepReversal::Delete), DestTrash::Absent) => StepUndo::LeftBehind,
        // Everything else: a reversal this build does not know, a trash this
        // build does not know, or a pair the core cannot emit.
        _ => StepUndo::Unclear,
    }
}

/// The glyph for a [`StepUndo`]. ASCII, for the same reason
/// [`crate::compare::verdict_glyph`] is (§17: never colour alone).
///
/// The marks are unique WITHIN this column and are not unique across the
/// three: `'!'` is `Certain` in the confidence column and `Irreversible` here,
/// `'?'` is an unknown in all of them. A pane must therefore label its columns
/// (or space them); the alternative — a single alphabet across three
/// questions — costs legibility on the column a reader consults most.
///
/// ```
/// use norte_frontend::sync::{StepUndo, undo_glyph};
/// assert_eq!(undo_glyph(StepUndo::Reverts), '<');
/// assert_ne!(undo_glyph(StepUndo::LeftBehind), undo_glyph(StepUndo::Reverts));
/// ```
#[must_use]
pub fn undo_glyph(undo: StepUndo) -> char {
    match undo {
        StepUndo::Reverts => '<',
        StepUndo::LeftBehind => '*',
        StepUndo::Irreversible => '!',
        StepUndo::Nothing => '.',
        StepUndo::Unclear => '?',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Los tests de este módulo cubren TODO `sync`, no solo lo que quedó en
    // `mod.rs`: nombran tipos que ahora viven en los trozos hermanos y que
    // llegan por el `pub use` de arriba, más los del wire que solo usan
    // ellos.
    use norte_i18n::{Lang, t_in, ta_in};
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, PlanHash, Side, SyncBlocker, SyncBlockerKind,
        SyncCounts, SyncFailureCause, SyncMode, SyncPlanDone, SyncReason, SyncReportResult,
        SyncStepsBatch,
    };
    use norte_proto::{TaskId, TaskState};
    use unicode_normalization::UnicodeNormalization;

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    fn hash() -> PlanHash {
        PlanHash::parse(&"a".repeat(64)).expect("hex")
    }

    fn task() -> TaskId {
        TaskId::new(7)
    }

    fn origen() -> VPath {
        VPath::parse("file:///origen").expect("vpath")
    }

    fn destino() -> VPath {
        VPath::parse("file:///destino").expect("vpath")
    }

    /// A step of `kind`, with the reversal the transducer would give it
    /// against a destination whose trash is `dest`.
    fn step(id: u64, kind: SyncStepKind, dest: DestTrash) -> SyncStep {
        let (reversal, reason) = match (kind, dest) {
            (SyncStepKind::Skip, _) => (None, Some(SyncReason::AmbiguousSource)),
            (_, DestTrash::Restorable) => (
                Some(match kind {
                    SyncStepKind::Overwrite | SyncStepKind::DeleteTree => {
                        StepReversal::RestoreTrash
                    }
                    _ => StepReversal::Delete,
                }),
                None,
            ),
            // No trash: what destroys is irreversible, what creates still says
            // `delete` — task 11's explicit decision, and the trap this module
            // exists for.
            (SyncStepKind::Overwrite | SyncStepKind::DeleteTree, DestTrash::Absent) => (
                Some(StepReversal::Irreversible),
                Some(SyncReason::NoTrashOnTarget),
            ),
            (_, DestTrash::Absent) => (Some(StepReversal::Delete), None),
            // An opaque trash: EVERY acting step, copies included.
            (_, _) => (
                Some(StepReversal::Irreversible),
                Some(SyncReason::NoTrashOnTarget),
            ),
        };
        SyncStep {
            id,
            kind,
            rel: rel("sub/a.txt"),
            dest_rel: None,
            size: Some(10),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal,
            reason,
        }
    }

    fn batch(task_id: TaskId, steps: Vec<SyncStep>) -> SyncStepsBatch {
        SyncStepsBatch { task_id, steps }
    }

    fn counts_of(steps: &[SyncStep]) -> SyncCounts {
        let mut c = SyncCounts::default();
        for s in steps {
            c.add(s);
        }
        c
    }

    fn done_for(steps: &[SyncStep], dest_trash: DestTrash) -> SyncPlanDone {
        SyncPlanDone {
            task_id: task(),
            plan_hash: hash(),
            counts: counts_of(steps),
            blockers: vec![],
            blockers_total: 0,
            executable: true,
            dest_trash,
        }
    }

    /// A ready plan whose steps and counts agree by construction.
    fn ready(steps: Vec<SyncStep>, dest_trash: DestTrash) -> SyncPlan {
        let done = done_for(&steps, dest_trash);
        match SyncState::ready(steps, done) {
            SyncState::Ready(p) => p,
            other => panic!("una notificación de cierre deja el diálogo listo: {other:?}"),
        }
    }

    fn update_plan() -> SyncPlan {
        ready(
            vec![
                step(1, SyncStepKind::Copy, DestTrash::Restorable),
                step(2, SyncStepKind::Overwrite, DestTrash::Restorable),
            ],
            DestTrash::Restorable,
        )
    }

    /// `executable` is the verdict, and the frontend obeys it.
    #[test]
    fn a_plan_with_blockers_cannot_be_approved() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            blockers: vec![SyncBlocker {
                rel: rel("build"),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Right),
            }],
            blockers_total: 1,
            executable: false,
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        assert!(!state.can_approve());
    }

    /// …and deduces nothing from the list, so a future blocker with no name to
    /// show still stops the plan.
    #[test]
    fn approval_is_decided_by_executable_and_never_by_the_blocker_list() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            blockers: vec![],
            blockers_total: 4,
            executable: false,
            ..done_for(&steps, DestTrash::Restorable)
        };
        assert!(!SyncState::ready(steps, done).can_approve());
    }

    /// THE test of this task. A copy onto a destination with no trash carries
    /// `delete` on the wire — and the undo skips it, so the file stays. A
    /// dialog that read the `reversal` column would promise it comes back.
    #[test]
    fn a_copy_says_delete_and_still_does_not_come_back_without_a_trash() {
        let copy = step(1, SyncStepKind::Copy, DestTrash::Absent);
        assert_eq!(
            copy.reversal,
            Some(StepReversal::Delete),
            "el wire dice `delete`, que es lo que hace la trampa"
        );
        assert_eq!(step_undo(&copy, DestTrash::Absent), StepUndo::LeftBehind);
        let plan = ready(vec![copy], DestTrash::Absent);
        assert_eq!(plan.outlook(), UndoOutlook::Nothing);
    }

    /// The one claim this module must never make: a step painted as coming
    /// back inside a plan that gives nothing back. Exhaustive over every step
    /// class and every destination.
    #[test]
    fn no_step_is_painted_as_coming_back_when_the_plan_gives_nothing_back() {
        for dest in [
            DestTrash::Restorable,
            DestTrash::Opaque,
            DestTrash::Absent,
            DestTrash::Unknown,
        ] {
            let steps: Vec<SyncStep> = [
                SyncStepKind::CreateDir,
                SyncStepKind::Copy,
                SyncStepKind::Overwrite,
                SyncStepKind::DeleteTree,
                SyncStepKind::Skip,
            ]
            .iter()
            .enumerate()
            .map(|(i, k)| step(u64::try_from(i).expect("cabe") + 1, *k, dest))
            .collect();
            let plan = ready(steps.clone(), dest);
            let reverts = steps
                .iter()
                .any(|s| step_undo(s, dest) == StepUndo::Reverts);
            match plan.outlook() {
                UndoOutlook::Full | UndoOutlook::Partial => {}
                UndoOutlook::Nothing | UndoOutlook::Unclear => assert!(
                    !reverts,
                    "{dest:?}: un paso se pinta como recuperable en un plan que no devuelve nada"
                ),
            }
        }
    }

    /// An opaque trash (macOS, Windows) buries the file where the human can
    /// still find it — and norte's undo cannot. Nothing reverts, and every
    /// step says so before it runs.
    #[test]
    fn an_opaque_trash_promises_nothing_although_the_file_still_exists() {
        let plan = ready(
            vec![
                step(1, SyncStepKind::Copy, DestTrash::Opaque),
                step(2, SyncStepKind::Overwrite, DestTrash::Opaque),
            ],
            DestTrash::Opaque,
        );
        assert_eq!(plan.outlook(), UndoOutlook::Nothing);
        for s in plan.steps() {
            assert_eq!(step_undo(s, DestTrash::Opaque), StepUndo::Irreversible);
            assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
        }
    }

    /// A restorable trash is the only destination this model lets a plan claim
    /// anything on.
    #[test]
    fn only_a_restorable_trash_gives_the_whole_batch_back() {
        assert_eq!(update_plan().outlook(), UndoOutlook::Full);
        for s in update_plan().steps() {
            assert_eq!(step_undo(s, DestTrash::Restorable), StepUndo::Reverts);
        }
    }

    /// The count of irreversible steps is the one number a human must not have
    /// to derive, so it gets a line to itself.
    #[test]
    fn the_summary_leads_with_the_irreversible_count_on_its_own_line() {
        let plan = ready(
            vec![
                step(1, SyncStepKind::Copy, DestTrash::Absent),
                step(2, SyncStepKind::Overwrite, DestTrash::Absent),
                step(3, SyncStepKind::DeleteTree, DestTrash::Absent),
            ],
            DestTrash::Absent,
        );
        let lines = plan.summary_lines(Lang::En);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("irreversible") && l.contains('2')),
            "{lines:?}"
        );
    }

    /// `bytes` is a lower bound and never travels alone: on `file://` a
    /// listing gives no sizes at all, so a confident total is the normal way
    /// to lie here.
    #[test]
    fn unmeasured_files_are_shown_and_never_folded_into_the_byte_total() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            counts: SyncCounts {
                copy: 1,
                bytes: 1_200_000_000,
                unmeasured_steps: 340,
                ..SyncCounts::default()
            },
            ..done_for(&steps, DestTrash::Restorable)
        };
        // The counts are the daemon's here (they describe a much bigger plan
        // than the one step that arrived), so the dialog also refuses it —
        // which is the next test. This one is about the sentence.
        let state = SyncState::ready(steps, done);
        let lines = state.plan().expect("plan").summary_lines(Lang::En);
        assert!(
            lines.iter().any(|l| l.contains("340")),
            "un total de bytes que esconde 340 ficheros sin medir es mentira: {lines:?}"
        );
    }

    /// The steps received are checked against the counts the plan closed with,
    /// rather than trusted to have all arrived.
    #[test]
    fn steps_that_do_not_add_up_to_the_counts_cannot_be_approved() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            counts: SyncCounts {
                copy: 40,
                ..SyncCounts::default()
            },
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        let plan = state.plan().expect("plan");
        assert_eq!(
            plan.integrity(),
            PlanIntegrity::Mismatch {
                received: 1,
                counted: 40
            }
        );
        assert!(!plan.can_approve(), "no se aprueba un plan a medias");
        assert!(
            plan.summary_lines(Lang::En)
                .iter()
                .any(|l| l.contains("40"))
        );
    }

    /// Two mistakes that cancel out are still two mistakes: the classes are
    /// compared one by one, not by their sum.
    #[test]
    fn a_lost_deletion_hidden_by_an_extra_skip_is_still_caught() {
        let steps = vec![
            step(1, SyncStepKind::Skip, DestTrash::Restorable),
            step(2, SyncStepKind::Skip, DestTrash::Restorable),
        ];
        let done = SyncPlanDone {
            counts: SyncCounts {
                skip: 1,
                delete_tree: 1,
                ..SyncCounts::default()
            },
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        assert!(!state.plan().expect("plan").integrity().is_complete());
    }

    /// A plan carrying a step class this build cannot name is not approvable:
    /// the list cannot show what it does, so a human cannot judge it.
    #[test]
    fn a_step_this_build_cannot_name_stops_the_approval() {
        let unknown = SyncStep {
            kind: SyncStepKind::Unknown,
            ..step(1, SyncStepKind::Copy, DestTrash::Restorable)
        };
        let plan = ready(vec![unknown], DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::Unnameable { steps: 1 });
        assert!(!plan.can_approve());
    }

    /// …and so does a plan whose steps this build DID name, when the daemon
    /// says one of them is of a class it could not name itself. Both counters
    /// are read, because either of them saying "there is something here you
    /// cannot see" is enough.
    #[test]
    fn a_daemon_that_counts_an_unnameable_step_stops_the_approval_too() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            counts: SyncCounts {
                unknown_kind: 1,
                ..counts_of(&steps)
            },
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        let plan = state.plan().expect("plan");
        assert_eq!(plan.integrity(), PlanIntegrity::Unnameable { steps: 1 });
        assert!(!plan.can_approve());
    }

    /// `Mirror` deletes, and deleting asks twice.
    #[test]
    fn mirror_asks_a_second_time_and_names_how_many_trees() {
        let steps: Vec<SyncStep> = (1..=4)
            .map(|i| step(i, SyncStepKind::DeleteTree, DestTrash::Restorable))
            .collect();
        let plan = ready(steps, DestTrash::Restorable);
        let c = plan.confirmation(Lang::En).expect("una segunda pregunta");
        assert!(c.text.contains('4'), "{c:?}");
    }

    /// An ordinary update against a destination that can take it back is one
    /// keystroke.
    #[test]
    fn update_asks_only_once() {
        assert!(update_plan().confirmation(Lang::En).is_none());
    }

    /// …but the same update against a destination that gives nothing back is
    /// not ordinary, and says so.
    #[test]
    fn an_update_that_cannot_be_undone_asks_twice() {
        let plan = ready(
            vec![step(1, SyncStepKind::Copy, DestTrash::Absent)],
            DestTrash::Absent,
        );
        let c = plan.confirmation(Lang::En).expect("una segunda pregunta");
        assert_eq!(c.id, "sync-confirm-no-way-back");
    }

    /// A plan nobody can approve asks nothing.
    #[test]
    fn a_blocked_plan_has_no_second_question() {
        let steps = vec![step(1, SyncStepKind::DeleteTree, DestTrash::Restorable)];
        let done = SyncPlanDone {
            executable: false,
            blockers_total: 1,
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        assert!(state.plan().expect("plan").confirmation(Lang::En).is_none());
    }

    /// §17: textual cues, never colour alone — two confidences must not
    /// collapse for a colour-blind reader, and neither must two classes.
    #[test]
    fn a_step_renders_its_class_and_its_confidence_as_distinct_glyphs() {
        let certain = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let probable = SyncStep {
            confidence: CompareConfidence::Probable,
            ..certain.clone()
        };
        let a = render_step(&certain, DestTrash::Restorable, SyncEncodings::default());
        let b = render_step(&probable, DestTrash::Restorable, SyncEncodings::default());
        assert_ne!(a.glyphs, b.glyphs);

        let mut seen: Vec<char> = [
            SyncStepKind::CreateDir,
            SyncStepKind::Copy,
            SyncStepKind::Overwrite,
            SyncStepKind::DeleteTree,
            SyncStepKind::Skip,
            SyncStepKind::Unknown,
        ]
        .iter()
        .map(|k| step_glyph(*k))
        .collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "dos clases comparten glifo: {seen:?}");
    }

    /// The undo column is a column of its own, so its marks must not collapse
    /// either.
    #[test]
    fn every_undo_answer_has_its_own_glyph() {
        let mut seen: Vec<char> = [
            StepUndo::Reverts,
            StepUndo::LeftBehind,
            StepUndo::Irreversible,
            StepUndo::Nothing,
            StepUndo::Unclear,
        ]
        .iter()
        .map(|u| undo_glyph(*u))
        .collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "{seen:?}");
    }

    /// A class from a newer daemon paints without panicking and promises
    /// nothing.
    #[test]
    fn an_unknown_step_kind_renders_without_panicking() {
        let unknown = SyncStep {
            kind: SyncStepKind::Unknown,
            reversal: Some(StepReversal::Unknown),
            reason: None,
            ..step(1, SyncStepKind::Copy, DestTrash::Restorable)
        };
        let cells = render_step(&unknown, DestTrash::Restorable, SyncEncodings::default());
        assert_eq!(cells.undo, StepUndo::Unclear);
        assert_eq!(cells.glyphs.kind, '?');
    }

    /// An unknown class that DECLARES itself irreversible is irreversible: not
    /// knowing what a step does makes it less countable, not less final (the
    /// same rule `SyncCounts::add` follows).
    #[test]
    fn an_unknown_class_that_says_irreversible_is_believed() {
        let unknown = SyncStep {
            kind: SyncStepKind::Unknown,
            reversal: Some(StepReversal::Irreversible),
            reason: Some(SyncReason::NoTrashOnTarget),
            ..step(1, SyncStepKind::Copy, DestTrash::Restorable)
        };
        assert_eq!(
            step_undo(&unknown, DestTrash::Restorable),
            StepUndo::Irreversible
        );
    }

    /// A `DeleteTree`'s `rel` hangs from the DESTINATION root, and an
    /// unreadable `Skip` from either — a pane that anchored both to the source
    /// column would paint them in the wrong place.
    #[test]
    fn the_two_steps_that_are_not_source_relative_say_so() {
        let del = step(1, SyncStepKind::DeleteTree, DestTrash::Restorable);
        assert_eq!(anchor_of(&del), RelAnchor::Dest);

        let unreadable = SyncStep {
            reason: Some(SyncReason::Unreadable),
            ..step(2, SyncStepKind::Skip, DestTrash::Restorable)
        };
        assert_eq!(anchor_of(&unreadable), RelAnchor::Either);

        let copy = step(3, SyncStepKind::Copy, DestTrash::Restorable);
        assert_eq!(anchor_of(&copy), RelAnchor::Source);
    }

    /// A blocker's anchor: `side` wins when present, and the three
    /// destination-named kinds still answer `Dest` without one. `Either`
    /// covers what neither the wire nor the kind can tell apart — a solaced
    /// overlap with no `side`, and a decoder-unknown kind (#189).
    #[test]
    fn a_blocker_s_anchor_prefers_side_then_the_kind() {
        let blocker = |kind: SyncBlockerKind, side: Option<Side>| SyncBlocker {
            rel: rel("sub"),
            kind,
            side,
        };
        // The three kinds that name the destination by definition, with no
        // `side` on the wire.
        for kind in [
            SyncBlockerKind::AmbiguousDest,
            SyncBlockerKind::DestReadOnly,
            SyncBlockerKind::DirTooLarge,
        ] {
            assert_eq!(
                blocker_anchor(&blocker(kind, None)),
                RelAnchor::Dest,
                "{kind:?}"
            );
        }
        // An overlap names neither root alone.
        assert_eq!(
            blocker_anchor(&blocker(SyncBlockerKind::OverlapDetected, None)),
            RelAnchor::Either
        );
        // `TypeMismatchDir` always carries `side` on a conforming daemon, and
        // the wire wins over the kind's usual "destination" pull the moment
        // it says the SOURCE had the directory.
        assert_eq!(
            blocker_anchor(&blocker(SyncBlockerKind::TypeMismatchDir, Some(Side::Left))),
            RelAnchor::Source
        );
        assert_eq!(
            blocker_anchor(&blocker(
                SyncBlockerKind::TypeMismatchDir,
                Some(Side::Right)
            )),
            RelAnchor::Dest
        );
        // A newer daemon's kind, with no side either: nothing to derive from.
        assert_eq!(
            blocker_anchor(&blocker(SyncBlockerKind::Unknown, None)),
            RelAnchor::Either
        );
    }

    /// A subtree the walk could not read is a `Skip`, not a blocker, so a plan
    /// can be complete and executable while a whole branch was never seen.
    /// The summary leads with that count the way it leads with the
    /// irreversible one.
    #[test]
    fn the_summary_names_the_entries_that_could_not_be_read() {
        let unreadable = SyncStep {
            reason: Some(SyncReason::Unreadable),
            ..step(2, SyncStepKind::Skip, DestTrash::Restorable)
        };
        let plan = ready(
            vec![
                step(1, SyncStepKind::Copy, DestTrash::Restorable),
                unreadable,
            ],
            DestTrash::Restorable,
        );
        assert_eq!(plan.unreadable(), 1);
        let lines = plan.summary_lines(Lang::En);
        assert!(
            lines.iter().any(|l| l.contains("could not be read")),
            "{lines:?}"
        );
    }

    /// A name is bytes and the dialog paints it, so it goes through the same
    /// lossy-and-MARKED path a listing does (rule 1, spec §6) — and the
    /// original bytes travel beside the masked text.
    #[test]
    fn a_hostile_rel_is_masked_and_flagged_and_keeps_its_bytes() {
        let raw = b"a\nb\xff.txt";
        let hostile = RelPath::new(vec![
            norte_proto::Segment::new(b"sub".to_vec()).expect("seg"),
            norte_proto::Segment::new(raw.to_vec()).expect("seg"),
        ]);
        let d = rel_display(&hostile, None);
        assert!(d.hostile, "un salto de línea en un nombre se marca");
        assert!(!d.text.contains('\n'), "el byte crudo no llega a pintarse");
        assert_eq!(d.raw, b"sub/a\nb\xff.txt", "los bytes viajan intactos");
    }

    /// La raíz (#193): `rel_display` sola la pinta vacía, y eso es justo lo
    /// que un panel de sincronización NO puede decir de un bloqueo de todo el
    /// árbol —un destino de solo lectura no tiene «ningún nombre», tiene
    /// TODOS—. `rel_display_or_root` es el contrato que documenta
    /// `RelDisplay::text`.
    #[test]
    fn la_raiz_dice_todo_el_arbol_y_no_nada() {
        let root = RelPath::parse_wire("").expect("rel");
        assert!(root.is_root());

        let bare = rel_display(&root, None);
        assert!(
            bare.text.is_empty(),
            "el contrato es de la envoltura, no de esta función"
        );

        let whole = rel_display_or_root(&root, None, Lang::En);
        assert!(!whole.text.is_empty());
        assert_ne!(whole.text, bare.text);
        assert!(whole.raw.is_empty(), "la raíz no tiene bytes que decir");
        assert!(!whole.hostile, "la frase no es una lectura del nombre");

        // Una ruta normal se comporta exactamente como `rel_display`.
        let named = rel_display_or_root(&rel("a.txt"), None, Lang::En);
        assert_eq!(named, rel_display(&rel("a.txt"), None));
    }

    /// A pair the two sides spell differently shows BOTH names: the write
    /// lands on the destination's spelling, not on the source's (#152).
    #[test]
    fn a_step_that_writes_under_another_spelling_shows_both() {
        let s = SyncStep {
            dest_rel: Some(rel("sub/A.TXT")),
            ..step(1, SyncStepKind::Overwrite, DestTrash::Restorable)
        };
        let cells = render_step(&s, DestTrash::Restorable, SyncEncodings::default());
        assert_eq!(cells.rel.text, "sub/a.txt");
        assert_eq!(
            cells.dest_rel.expect("la otra ortografía").text,
            "sub/A.TXT"
        );
    }

    /// El sentido de una sincronización lo decide el lado ACTIVO del panel de
    /// diferencias cuando lo hay, y `Tab` intercambia las DOS raíces enteras
    /// con sus dos reinterpretaciones. Los panes no se miran siquiera: el
    /// lector tiene delante un panel con un lado marcado, y el plan tiene que
    /// hablar de lo que está mirando.
    #[test]
    fn el_lado_activo_del_panel_decide_el_sentido_y_los_panes_no_se_miran() {
        let a = VPath::parse("file:///a").expect("vpath");
        let b = VPath::parse("file:///b").expect("vpath");
        // Un par DISTINTO en los panes: si saliera cualquiera de estos dos,
        // es que el panel no decidió.
        let p0 = VPath::parse("file:///pane0").expect("vpath");
        let p1 = VPath::parse("file:///pane1").expect("vpath");
        let panes = Panes {
            focused_root: &p0,
            focused_encoding: None,
            other_root: &p1,
            other_encoding: None,
        };
        let enc_izq = Some(norte_encoding::NameEncoding::Cp437);
        let mut v = crate::compare::CompareView::new(a.clone(), b.clone(), 0, enc_izq, None);

        let r = sync_roots(Some(&v), &panes);
        assert_eq!(r.source, a, "el lado activo nace a la izquierda");
        assert_eq!(r.dest, b);
        assert_eq!(
            r.source_encoding, enc_izq,
            "y su reinterpretación viaja con él"
        );

        v.pane.swap_active_side();
        let r = sync_roots(Some(&v), &panes);
        assert_eq!(r.source, b, "Tab invierte el SENTIDO");
        assert_eq!(r.dest, a);
        assert_eq!(
            r.dest_encoding, enc_izq,
            "y la reinterpretación se va con SU raíz, no se queda en su lado"
        );

        // Sin panel, y solo entonces, mandan los panes.
        let r = sync_roots(None, &panes);
        assert_eq!(r.source, p0);
        assert_eq!(r.dest, p1);
    }

    /// **La regresión que la auditoría de encoding destapó**: la ortografía
    /// del destino se plegaba comparando el TEXTO pintado, que es lossy. La
    /// pareja `lossy_collapse_ff`/`lossy_collapse_fe` de la corpus existe
    /// justo para esto —bytes distintos, mismo pliegue a `U+FFFD`—, y con la
    /// comparación por texto el campo que dice sobre qué nombre cae la
    /// escritura DESAPARECÍA de la pantalla, sin flecha y sin marca, en cuanto
    /// los dos nombres llevaban un byte inválido cada uno (#152).
    #[test]
    fn dos_ortografias_que_colapsan_al_pintarse_siguen_siendo_dos() {
        let fixtures = norte_testkit::corpus::hostile_names();
        let uno = fixtures
            .iter()
            .find(|f| f.id == "lossy_collapse_ff")
            .expect("corpus");
        let otro = fixtures
            .iter()
            .find(|f| f.id == "lossy_collapse_fe")
            .expect("corpus");
        let rel_de = |bytes: &[u8]| {
            RelPath::new(vec![
                norte_proto::Segment::new(bytes.to_vec()).expect("seg"),
            ])
        };
        let paso = SyncStep {
            rel: rel_de(&uno.bytes),
            dest_rel: Some(rel_de(&otro.bytes)),
            ..step(1, SyncStepKind::Overwrite, DestTrash::Restorable)
        };
        let cells = render_step(&paso, DestTrash::Restorable, SyncEncodings::default());
        let dest = cells
            .dest_rel
            .expect("dos ficheros distintos son dos ortografías");
        assert_eq!(
            dest.text, cells.rel.text,
            "y colapsan al pintarse, que es justo lo que hacía el pliegue por texto"
        );
        assert_ne!(dest.raw, cells.rel.raw, "pero los BYTES no colapsan");
        // #192: el pliegue visual también deja marcado el gemelo, esté o no
        // badgeado ya como hostil por otro motivo.
        assert!(cells.dest_rel_twin, "las dos mitades pintan igual");

        // Y byte-idénticas SÍ se pliegan: enseñar la misma ruta dos veces con
        // una flecha en medio sugiere un renombrado que no hay.
        let mismo = SyncStep {
            dest_rel: Some(rel_de(&uno.bytes)),
            ..paso
        };
        let cells_mismo = render_step(&mismo, DestTrash::Restorable, SyncEncodings::default());
        assert!(cells_mismo.dest_rel.is_none());
        assert!(
            !cells_mismo.dest_rel_twin,
            "sin `dest_rel` no hay pareja que marcar"
        );
    }

    /// #192, el caso que motivó el marcador: `café.txt` NFC y `café.txt` NFD
    /// son BYTE-distintos, los dos UTF-8 válido, y ninguno es hostil — así
    /// que sin `dest_rel_twin` el lector ve la misma cadena dos veces sin
    /// nada que explique la flecha. `nfc_e_acute`/`nfd_e_acute` son la pareja
    /// exacta que la corpus ya trae para esto.
    #[test]
    fn un_par_nfc_nfd_se_marca_como_la_misma_ortografia_en_pantalla() {
        let fixtures = norte_testkit::corpus::hostile_names();
        let nfc = fixtures
            .iter()
            .find(|f| f.id == "nfc_e_acute")
            .expect("corpus");
        let nfd = fixtures
            .iter()
            .find(|f| f.id == "nfd_e_acute")
            .expect("corpus");
        assert_ne!(nfc.bytes, nfd.bytes, "el fixture es byte-distinto");
        let rel_de = |bytes: &[u8]| {
            RelPath::new(vec![
                norte_proto::Segment::new(bytes.to_vec()).expect("seg"),
            ])
        };
        let paso = SyncStep {
            rel: rel_de(&nfc.bytes),
            dest_rel: Some(rel_de(&nfd.bytes)),
            ..step(1, SyncStepKind::Overwrite, DestTrash::Restorable)
        };
        let cells = render_step(&paso, DestTrash::Restorable, SyncEncodings::default());
        let dest = cells.dest_rel.expect("bytes distintos, dos ortografías");
        // NO son el mismo `String` —"é" precompuesta contra "e" + acento
        // combinante— y esa es justo la trampa: una fuente los compone al
        // MISMO glifo, así que una igualdad de `text` a secas no cazaría
        // este par aunque en pantalla sea indistinguible.
        assert_ne!(dest.text, cells.rel.text, "distintos como String");
        assert_eq!(
            dest.text.nfc().collect::<String>(),
            cells.rel.text.nfc().collect::<String>(),
            "pero la MISMA forma NFC, que es lo que pinta el glifo"
        );
        assert!(
            !cells.rel.hostile,
            "NFC es UTF-8 válido, no hay nada que enmascarar"
        );
        assert!(!dest.hostile, "NFD también es UTF-8 válido");
        assert!(
            cells.dest_rel_twin,
            "el marcador es lo único que distingue esta fila de una repetida"
        );

        // Y `render_failure` sigue exactamente la misma regla.
        let fallo = norte_proto::methods::SyncFailure {
            rel: rel_de(&nfc.bytes),
            dest_rel: Some(rel_de(&nfd.bytes)),
            cause: SyncFailureCause::IllegalName,
            kind: SyncStepKind::Copy,
        };
        let fcells = render_failure(&fallo, SyncEncodings::default());
        assert!(fcells.dest_rel_twin);
    }

    /// Un plan CERRADO cuya Task acabó cancelada (o fallando) no se aprueba, y
    /// el pie y la línea de teclas no pueden discrepar sobre eso: los dos
    /// hechos son compatibles —`sync.plan_done` llega antes de que el canal se
    /// cierre— y la pantalla llegó a decir «cancelado, no hay plan que
    /// aprobar» mientras la tecla de aprobar seguía funcionando (revisión rust
    /// MAJOR-1).
    #[test]
    fn el_pie_y_la_aprobacion_no_pueden_discrepar() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = done_for(&steps, DestTrash::Restorable);
        let mut v = SyncView::new(task(), SyncMode::Update, origen(), destino(), None, None);
        assert!(v.state.on_steps(batch(task(), steps)));
        assert!(v.state.on_plan_done(done));
        let listo = status_line(&v, Lang::Es);
        assert!(v.can_approve(), "cerrado, íntegro y con la Task viva");

        for desenlace in [SyncRunState::Cancelled, SyncRunState::Failed] {
            v.run = desenlace;
            assert!(
                !v.can_approve(),
                "{desenlace:?}: el lector pidió parar (o el daemon se cayó)"
            );
            assert_ne!(
                status_line(&v, Lang::Es),
                listo,
                "{desenlace:?}: y el pie no puede seguir diciendo que se apruebe"
            );
        }
    }

    /// La línea de TECLAS no puede ofrecer `a aprobar` sobre un plan que no se
    /// puede aprobar: es el mismo desacuerdo que
    /// [`el_pie_y_la_aprobacion_no_pueden_discrepar`] una capa más arriba, y
    /// la razón de que [`hint_id`] sea compartida en vez de estar escrita en
    /// cada frontend (la TUI la tenía sin la mitad del `can_approve`).
    #[test]
    fn la_linea_de_teclas_no_ofrece_aprobar_lo_que_no_se_aprueba() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let mut done = done_for(&steps, DestTrash::Restorable);
        let mut v = SyncView::new(task(), SyncMode::Update, origen(), destino(), None, None);
        assert!(v.state.on_steps(batch(task(), steps.clone())));
        assert!(v.state.on_plan_done(done.clone()));
        assert_eq!(hint_id(&v), "sync-hint", "cerrado y sano: se nombra la `a`");

        // La segunda pregunta se queda el teclado entero.
        v.confirming = Some(Confirmation {
            id: "sync-confirm-delete",
            text: "¿seguro?".to_owned(),
        });
        assert_eq!(hint_id(&v), "sync-hint-confirm");
        v.confirming = None;

        // Un plan BLOQUEADO está en `Ready` y no se aprueba: la `a` no se
        // nombra, y el pie ya dice por qué.
        done.executable = false;
        let mut bloqueado =
            SyncView::new(task(), SyncMode::Update, origen(), destino(), None, None);
        assert!(bloqueado.state.on_steps(batch(task(), steps)));
        assert!(bloqueado.state.on_plan_done(done));
        assert!(bloqueado.awaiting_approval(), "cerró: está en `Ready`");
        assert_eq!(hint_id(&bloqueado), "sync-hint-done");

        // Y una Task cancelada tras cerrar el plan, igual.
        v.run = SyncRunState::Cancelled;
        assert_eq!(hint_id(&v), "sync-hint-done");

        // Gastado: aplicar lo consume.
        v.run = SyncRunState::Running;
        v.on_apply_started(TaskId::new(9));
        assert_eq!(hint_id(&v), "sync-hint-done");
    }

    /// Las tres reglas del final de una aplicación, COMPARTIDAS (#161): el
    /// error de la Task manda sobre el del informe, un informe que no llega es
    /// un fallo aunque la Task dijera `Completed`, y un estado no terminal
    /// también. `norte-tui` las tenía escritas a mano con la segunda SIN
    /// aplicar: un `sync.report` que fallaba dejaba el diálogo en `Applying` y
    /// el pie diciendo «aplicando…» para siempre.
    #[test]
    fn el_final_de_una_aplicacion_obedece_una_sola_regla() {
        let armar = || {
            let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
            let done = done_for(&steps, DestTrash::Restorable);
            let mut v = SyncView::new(task(), SyncMode::Update, origen(), destino(), None, None);
            assert!(v.state.on_steps(batch(task(), steps)));
            assert!(v.state.on_plan_done(done));
            v.on_apply_started(TaskId::new(9));
            v
        };
        let informe = || SyncReportResult {
            done: 3,
            failed: 1,
            skipped: 0,
            bytes: 30,
            failures: vec![],
            batch_id: Some(7),
            dest_trash: DestTrash::Restorable,
        };

        // Terminó bien y con informe: `Done`, sin nada que decir.
        let mut v = armar();
        assert!(
            v.on_apply_ended(&TaskState::Completed, Ok(informe()))
                .is_none()
        );
        assert_eq!(v.run, SyncRunState::Done);
        assert!(matches!(v.state, SyncState::Applied(_)));

        // Sin informe NO se dice que terminó bien, aunque la Task dijera que
        // sí: sin él no se sabe cuánto se escribió.
        let mut v = armar();
        let c = v
            .on_apply_ended(&TaskState::Completed, Err(norte_proto::Error::NotFound))
            .expect("un informe que no llega es un fallo que decir");
        assert_eq!(v.run, SyncRunState::Failed);
        assert_eq!(
            c,
            crate::error::error_category(&norte_proto::Error::NotFound)
        );

        // El error de la TASK manda sobre el del informe.
        let mut v = armar();
        let c = v
            .on_apply_ended(
                &TaskState::Failed {
                    error: norte_proto::Error::PermissionDenied,
                },
                Ok(informe()),
            )
            .expect("un fallo trae su categoría");
        assert_eq!(
            c,
            crate::error::error_category(&norte_proto::Error::PermissionDenied)
        );

        // Un estado NO terminal también es fallo: solo se llega a él con los
        // emisores del progreso caídos.
        let mut v = armar();
        assert!(
            v.on_apply_ended(&TaskState::Running, Ok(informe()))
                .is_none()
        );
        assert_eq!(v.run, SyncRunState::Failed);

        // Pero una CANCELACIÓN se dice cancelada aunque el informe falte: el
        // lector pidió parar y eso ya lo sabe.
        let mut v = armar();
        assert!(
            v.on_apply_ended(&TaskState::Cancelled, Err(norte_proto::Error::NotFound))
                .is_some(),
            "y aun así se dice que no se pudo pedir el informe"
        );
        assert_eq!(v.run, SyncRunState::Cancelled);

        // Y la segunda pregunta se cae en todos los casos.
        let mut v = armar();
        v.confirming = Some(Confirmation {
            id: "sync-confirm-delete",
            text: "¿seguro?".to_owned(),
        });
        assert!(
            v.on_apply_ended(&TaskState::Cancelled, Ok(informe()))
                .is_none()
        );
        assert!(v.confirming.is_none());
        assert_eq!(v.run, SyncRunState::Cancelled);
    }

    /// Una aplicación CORTADA a medias tiene informe, y el pie cuenta lo que
    /// se escribió — no «cancelado, y no hay plan que aprobar», que es una
    /// frase sobre el plan (ya aprobado) y que la pantalla llegó a pintar
    /// encima de la lista de fallos de la aplicación (#161 fase C2 tarea 4).
    ///
    /// El fallo, en cambio, sigue mandando: un error tiene que llegar entero.
    #[test]
    fn una_aplicacion_cortada_cuenta_lo_que_escribio() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = done_for(&steps, DestTrash::Restorable);
        let mut v = SyncView::new(task(), SyncMode::Update, origen(), destino(), None, None);
        assert!(v.state.on_steps(batch(task(), steps)));
        assert!(v.state.on_plan_done(done));
        v.on_apply_started(TaskId::new(9));
        v.state.on_report(SyncReportResult {
            done: 3,
            failed: 1,
            skipped: 0,
            bytes: 30,
            failures: vec![],
            batch_id: Some(7),
            dest_trash: DestTrash::Restorable,
        });

        v.run = SyncRunState::Done;
        let entera = status_line(&v, Lang::Es);

        v.run = SyncRunState::Cancelled;
        let cortada = status_line(&v, Lang::Es);
        assert!(cortada.contains('3') && cortada.contains('1'), "{cortada}");
        assert_ne!(
            cortada,
            ta_in(Lang::Es, "sync-status-cancelled", &[("n", "1")]),
            "el informe manda sobre el «cancelado» del plan"
        );
        assert_ne!(
            cortada, entera,
            "y no se lee igual que una que terminó sola: el color no puede ser la única señal"
        );

        v.run = SyncRunState::Failed;
        v.error = Some("boom".to_owned());
        let fallida = status_line(&v, Lang::Es);
        assert!(fallida.contains("boom"), "un fallo sigue llegando entero");
        assert!(
            fallida.contains('3'),
            "y ya no esconde cuánto llegó a escribirse: {fallida}"
        );
    }

    /// #152, la mitad que faltaba: cada ruta se lee con la reinterpretación
    /// del lado del que CUELGA. El `rel` de un `DeleteTree` es una ruta del
    /// DESTINO ([`anchor_of`]) aunque se pinte en la primera columna, así que
    /// leerla con el codepage del ORIGEN nombra el subárbol que se va a
    /// borrar con los bytes de otro árbol — en la pantalla donde se aprueba
    /// borrarlo.
    #[test]
    fn cada_ruta_se_lee_con_la_reinterpretacion_del_lado_del_que_cuelga() {
        // Del ciclo de reinterpretación, no de `encoding_rs` a pelo: ese
        // crate se consume por la API de `norte-encoding` y no directo.
        let origen = norte_encoding::NameEncoding::Cp437;
        let destino = norte_encoding::name_reinterpret_cycle()
            .iter()
            .copied()
            .find(|e| e.label() != origen.label())
            .expect("el ciclo trae más de una");
        let enc = SyncEncodings {
            source: Some(origen),
            dest: Some(destino),
        };
        // Los bytes salen del corpus canónico (`cp866_papka`, cuyo `why`
        // nombra el #57) y no de un literal escrito aquí: una regresión de
        // codificación se pinea con la corpus, que es donde el repo las junta.
        let bytes = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == "cp866_papka")
            .expect("la corpus trae cp866_papka")
            .bytes;
        let rel_hostil = RelPath::new(vec![norte_proto::Segment::new(bytes.clone()).expect("seg")]);
        let cp437 = norte_encoding::decode_name(&bytes, origen);
        let ibm866 = norte_encoding::decode_name(&bytes, destino);
        assert_ne!(cp437, ibm866, "el fixture distingue las dos lecturas");

        let borrado = SyncStep {
            rel: rel_hostil.clone(),
            ..step(1, SyncStepKind::DeleteTree, DestTrash::Restorable)
        };
        let cells = render_step(&borrado, DestTrash::Restorable, enc);
        assert_eq!(cells.anchor, RelAnchor::Dest);
        assert_eq!(
            cells.rel.text, ibm866,
            "un DeleteTree habla del DESTINO: con la del destino"
        );

        // Y una copia cuelga del origen, con `dest_rel` del destino. Los
        // mismos bytes más un sufijo ASCII: byte-DISTINTOS (si no, el modelo
        // los pliega, que es lo correcto — ver
        // `dos_ortografias_que_colapsan_al_pintarse_siguen_siendo_dos`) y aun
        // así distinguibles por el codepage con que se leen.
        let mut otros = bytes.clone();
        otros.push(b'2');
        let copia = SyncStep {
            rel: rel_hostil,
            dest_rel: Some(RelPath::new(vec![
                norte_proto::Segment::new(otros).expect("seg"),
            ])),
            ..step(2, SyncStepKind::Copy, DestTrash::Restorable)
        };
        let cells = render_step(&copia, DestTrash::Restorable, enc);
        assert_eq!(cells.rel.text, cp437, "el rel de una copia es del origen");
        assert_eq!(
            cells.dest_rel.expect("hay ortografía de destino").text,
            format!("{ibm866}2"),
            "y la ortografía sobre la que cae la escritura, del destino"
        );
    }

    /// The dialog walks forwards only. A late `sync.plan_done` — a plan the
    /// user already approved, answered twice — must not rewind a dialog that
    /// is already applying.
    #[test]
    fn states_go_planning_ready_applying_applied_and_never_backwards() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = done_for(&steps, DestTrash::Restorable);
        let mut s = SyncState::Planning(Planning::new(task()));
        assert!(s.on_steps(batch(task(), steps.clone())));
        assert!(matches!(s, SyncState::Planning(_)));
        assert!(s.on_plan_done(done.clone()));
        assert!(matches!(s, SyncState::Ready(_)));
        s.on_apply_started(TaskId::new(9));
        assert!(matches!(s, SyncState::Applying(_)));

        assert!(!s.on_plan_done(done), "un aviso rancio se descarta");
        assert!(
            matches!(s, SyncState::Applying(_)),
            "un aviso rancio no rebobina el diálogo"
        );
        assert!(!s.on_steps(batch(task(), steps)));
        assert!(matches!(s, SyncState::Applying(_)));

        s.on_report(SyncReportResult {
            done: 1,
            failed: 0,
            skipped: 0,
            bytes: 10,
            failures: vec![],
            batch_id: Some(3),
            dest_trash: DestTrash::Restorable,
        });
        match &s {
            SyncState::Applied(a) => assert_eq!(a.report().done, 1),
            other => panic!("el informe cierra el diálogo: {other:?}"),
        }
        assert!(
            !s.can_approve(),
            "un plan ya aplicado no se vuelve a aprobar"
        );
    }

    /// Un panel con el plan ya cerrado, sano y con un paso que escribe: el
    /// estado de partida de todo lo que se aprueba.
    fn vista_lista() -> SyncView {
        let mut v = SyncView::new(
            task(),
            norte_proto::methods::SyncMode::Update,
            origen(),
            destino(),
            None,
            None,
        );
        v.state = SyncState::Ready(ready(
            vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)],
            DestTrash::Restorable,
        ));
        v
    }

    /// #161: el envoltorio del run vivía en `norte-tui`, así que la GUI
    /// habría tenido que reimplementarlo. C1 aprendió que mover MEDIA
    /// decisión es peor que no moverla: el comentario decía «una sola regla»
    /// y había tres copias. Aquí se mueve entera.
    #[test]
    fn el_envoltorio_del_run_vive_con_el_modelo() {
        let v = SyncView::new(
            task(),
            norte_proto::methods::SyncMode::Update,
            origen(),
            destino(),
            None,
            None,
        );
        assert!(v.confirming.is_none(), "nace sin pregunta pendiente");
        assert!(
            !v.can_approve(),
            "un plan que todavía no cerró NO se puede aprobar"
        );
    }

    /// La trampa que la TUI documenta y que el CLI de la fase A no vio:
    /// `SyncState::can_approve` sabe que un plan YA aprobado no se vuelve a
    /// aprobar; `SyncPlan::can_approve`, que sigue accesible por
    /// `SyncState::plan()`, contesta que sí.
    #[test]
    fn un_plan_ya_aprobado_no_se_aprueba_dos_veces() {
        let mut v = SyncView::new(
            task(),
            norte_proto::methods::SyncMode::Update,
            origen(),
            destino(),
            None,
            None,
        );
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        v.state = SyncState::Ready(ready(steps, DestTrash::Restorable));
        assert!(v.can_approve(), "cerrado y sano: se puede");

        assert!(
            v.on_apply_started(TaskId::new(9)),
            "sin cancelación: adopta"
        );
        assert!(
            !v.can_approve(),
            "ya aplicándose: la respuesta es NO, aunque el plan de dentro diga que sí"
        );
    }

    /// Revisión de rama de C2 (rust MAJOR-1 + seguridad MAJOR-1): la ventana
    /// entre la tecla y la respuesta del daemon. `Applying` NO llega con la
    /// tecla, así que sin pestillo el panel se queda diciendo «aprobable»
    /// mientras hay un `sync.apply` volando — y el pie ofrece una tecla que
    /// `approve` ya rechaza. El pestillo vive AQUÍ, con `can_approve`,
    /// `hint_id` y `status_line`, que es lo que la versión de `norte-gui` no
    /// podía conseguir.
    #[test]
    fn el_pestillo_del_apply_en_vuelo_lo_ven_las_tres_funciones() {
        let mut v = vista_lista();
        assert!(v.can_approve(), "cerrado y sano");
        assert_eq!(hint_id(&v), "sync-hint", "ofrece aprobar");

        let hash = v.submit().expect("aprobable: da el hash");
        assert!(v.is_submitted(), "el apply está en vuelo");
        assert!(
            !v.can_approve(),
            "y en esa ventana NO se puede aprobar otra vez"
        );
        assert_ne!(
            hint_id(&v),
            "sync-hint",
            "el pie no puede seguir ofreciendo una tecla que approve rechaza"
        );
        assert!(v.submit().is_none(), "el segundo submit no da hash");

        // Y lo suelta la TRANSICIÓN, no la generación de la petición.
        assert!(v.on_apply_started(TaskId::new(9)));
        assert!(!v.is_submitted(), "adoptada la Task, el pestillo se suelta");
        let _ = hash;
    }

    /// El pestillo se suelta también cuando la Task muere ANTES de adoptarse:
    /// si no, el panel queda sin poder aprobar para siempre y con el pie
    /// ofreciéndolo (revisión de rama, MINOR de las dos revisiones).
    #[test]
    fn una_task_que_muere_sin_adoptarse_suelta_el_pestillo() {
        let mut v = vista_lista();
        v.submit().expect("aprobable");
        assert!(v.is_submitted());
        v.on_apply_ended(
            &TaskState::Failed {
                error: norte_proto::Error::PermissionDenied,
            },
            Err(norte_proto::Error::PermissionDenied),
        );
        assert!(!v.is_submitted(), "terminó: el pestillo se suelta");
    }

    /// Revisión de rama de C2, rust MAJOR-2: el guard estaba en el envoltorio
    /// de la GUI, así que la TUI se quedaba con el agujero. Un `Esc` que llega
    /// antes que la Task pidió PARAR; adoptarla resucita el run y, de paso,
    /// borra la petición de cancelación.
    #[test]
    fn una_task_que_llega_tras_el_esc_no_se_adopta() {
        let mut v = vista_lista();
        v.submit().expect("aprobable");
        v.cancel_requested = true;
        assert!(
            !v.on_apply_started(TaskId::new(9)),
            "ya se pidió cancelar: no se adopta"
        );
        assert!(
            v.cancel_requested,
            "y la petición de cancelación SIGUE puesta"
        );
    }

    /// A dialog listens to ONE plan. Re-planning with a narrower selection
    /// starts a second `sync.plan` on the same connection, and its batches
    /// must not be appended to the first plan's list: the human would be
    /// looking at plan A's steps while approving plan A's hash, with plan B's
    /// rows mixed in.
    #[test]
    fn a_second_plans_notifications_do_not_land_in_this_dialog() {
        let mine = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let theirs = vec![step(2, SyncStepKind::DeleteTree, DestTrash::Restorable)];
        let mut s = SyncState::Planning(Planning::new(task()));
        assert!(s.on_steps(batch(task(), mine.clone())));
        assert!(
            !s.on_steps(batch(TaskId::new(99), theirs)),
            "un lote de OTRO plan se descarta y lo dice"
        );

        let done_ajeno = SyncPlanDone {
            task_id: TaskId::new(99),
            ..done_for(&mine, DestTrash::Restorable)
        };
        assert!(!s.on_plan_done(done_ajeno));
        assert!(matches!(s, SyncState::Planning(_)), "y no cierra el mío");

        assert!(s.on_plan_done(done_for(&mine, DestTrash::Restorable)));
        let plan = s.plan().expect("plan");
        assert_eq!(plan.steps().len(), 1, "solo los pasos de mi plan");
        assert!(plan.can_approve());
    }

    /// The dialog's headline is built on `counts.irreversible`, and that
    /// number is checked against the steps like every other. A plan closed
    /// with `irreversible: 0` over steps that each say they cannot be undone
    /// must not be headlined "you can undo all of this".
    #[test]
    fn a_plan_whose_own_totals_contradict_its_steps_cannot_be_approved() {
        let steps = vec![step(1, SyncStepKind::Overwrite, DestTrash::Absent)];
        assert_eq!(counts_of(&steps).irreversible, 1);
        let done = SyncPlanDone {
            counts: SyncCounts {
                irreversible: 0,
                ..counts_of(&steps)
            },
            // …and a trash that would make it look fully reversible.
            ..done_for(&steps, DestTrash::Restorable)
        };
        let state = SyncState::ready(steps, done);
        let plan = state.plan().expect("plan");
        assert_eq!(plan.integrity(), PlanIntegrity::Contradictory);
        assert!(!plan.can_approve());
    }

    /// A step that contradicts itself — here a `Copy` with no reversal at all,
    /// which the wire tolerates so one bad token cannot kill a batch of 256 —
    /// is not something an approval dialog can describe, so it refuses.
    #[test]
    fn a_step_that_contradicts_itself_stops_the_approval() {
        let broken = SyncStep {
            reversal: None,
            ..step(1, SyncStepKind::Copy, DestTrash::Restorable)
        };
        assert!(!broken.shape_is_consistent());
        let plan = ready(vec![broken], DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::Malformed { steps: 1 });
        assert!(!plan.can_approve());
    }

    /// #196: a plan bigger than the retention cap is COUNTED whole and
    /// RETAINED in part. Everything the human decides on — the counters, the
    /// integrity verdict, the confirmation — comes from the counting half, so
    /// the plan still closes `Complete`; what stops is the list.
    #[test]
    fn un_plan_por_encima_del_tope_se_cuenta_entero_y_se_retiene_en_parte() {
        let de_mas = 5usize;
        let steps: Vec<SyncStep> = (0..PLAN_STEPS_RETAINED_MAX + de_mas)
            .map(|i| step(i as u64, SyncStepKind::Copy, DestTrash::Restorable))
            .collect();
        let plan = ready(steps, DestTrash::Restorable);
        assert_eq!(
            plan.steps().len(),
            PLAN_STEPS_RETAINED_MAX,
            "no se retiene más de lo pactado"
        );
        assert_eq!(plan.dropped(), de_mas as u64);
        assert_eq!(
            plan.counts().copy,
            (PLAN_STEPS_RETAINED_MAX + de_mas) as u64,
            "los contadores los ve TODOS"
        );
        assert_eq!(
            plan.integrity(),
            PlanIntegrity::Complete,
            "el recorte no es un desacuerdo con el daemon"
        );
        assert!(plan.can_approve());
        // Y se dice: una lista que acaba sin avisar se lee como el plan entero.
        let resumen = plan.summary_lines(Lang::En).join("\n");
        assert!(
            resumen.contains(&de_mas.to_string()),
            "el resumen nombra lo que no lista: {resumen}"
        );
    }

    /// El otro medio de #196: el rastro de ids es de tamaño CONSTANTE (el
    /// máximo visto), no un conjunto que crece con el plan — y sigue cazando
    /// la repetición que motivó #194, incluso lejos del original.
    #[test]
    fn un_id_repetido_lejos_del_original_sigue_cazandose() {
        let mut steps: Vec<SyncStep> = (0..50)
            .map(|i| step(i, SyncStepKind::Copy, DestTrash::Restorable))
            .collect();
        steps.push(step(0, SyncStepKind::Copy, DestTrash::Restorable));
        let plan = ready(steps, DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::DuplicateIds { steps: 1 });
        assert!(!plan.can_approve());
    }

    /// Y un id que no repite nada pero TAMPOCO avanza —el wire dice que el id
    /// es monótono dentro de un plan— cae bajo el mismo veredicto: es la misma
    /// promesa rota, y el panel ancla su cursor a ese id.
    #[test]
    fn un_id_que_no_avanza_cuenta_igual_aunque_no_repita() {
        let steps = vec![
            step(0, SyncStepKind::Copy, DestTrash::Restorable),
            step(9, SyncStepKind::Copy, DestTrash::Restorable),
            step(4, SyncStepKind::Copy, DestTrash::Restorable),
        ];
        let plan = ready(steps, DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::DuplicateIds { steps: 1 });
    }

    /// #194: two steps sharing an id refuse the plan, the same way a
    /// self-contradicting step does. Each is individually well-formed — the
    /// defect is only that the SECOND repeats the first's id — so nothing
    /// short of a uniqueness check catches it: the totals agree (both count
    /// as two `Copy`s), and `shape_is_consistent` never looks at another
    /// step.
    #[test]
    fn two_steps_sharing_an_id_stop_the_approval() {
        let first = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let repeat = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let plan = ready(vec![first, repeat], DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::DuplicateIds { steps: 1 });
        assert!(!plan.can_approve());
    }

    /// The SAME plan, arriving in two `sync.steps` batches instead of one
    /// call to [`ready`]: the check has to survive the split, because that is
    /// how the daemon actually delivers a plan.
    #[test]
    fn a_repeated_id_across_two_batches_still_refuses() {
        let first = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let repeat = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let done = done_for(&[first.clone(), repeat.clone()], DestTrash::Restorable);
        let mut state = SyncState::Planning(Planning::new(task()));
        assert!(state.on_steps(batch(task(), vec![first])));
        assert!(state.on_steps(batch(task(), vec![repeat])));
        assert!(state.on_plan_done(done));
        let plan = state.plan().expect("closed");
        assert_eq!(plan.integrity(), PlanIntegrity::DuplicateIds { steps: 1 });
    }

    /// Two DIFFERENT ids next to each other never trip the check — the
    /// common case has to stay `Complete`, or every ordinary plan would
    /// refuse.
    #[test]
    fn distinct_ids_do_not_trip_the_duplicate_check() {
        let a = step(1, SyncStepKind::Copy, DestTrash::Restorable);
        let b = step(2, SyncStepKind::Copy, DestTrash::Restorable);
        let plan = ready(vec![a, b], DestTrash::Restorable);
        assert_eq!(plan.integrity(), PlanIntegrity::Complete);
    }

    /// A reversal a newer daemon named is an admitted unknown, and an
    /// admitted unknown is never a promise: one such step downgrades the whole
    /// headline, which the counters alone could not do.
    #[test]
    fn one_step_this_build_cannot_judge_takes_the_headline_down_with_it() {
        let strange = SyncStep {
            reversal: Some(StepReversal::Unknown),
            ..step(2, SyncStepKind::Copy, DestTrash::Restorable)
        };
        assert!(strange.shape_is_consistent(), "el wire lo acepta");
        let plan = ready(
            vec![step(1, SyncStepKind::Copy, DestTrash::Restorable), strange],
            DestTrash::Restorable,
        );
        assert_eq!(
            UndoOutlook::of(DestTrash::Restorable, plan.counts()),
            UndoOutlook::Full,
            "los contadores solos dirían que todo vuelve"
        );
        assert_eq!(
            plan.outlook(),
            UndoOutlook::Unclear,
            "los PASOS dicen que no"
        );
        let c = plan.confirmation(Lang::En).expect("segunda pregunta");
        assert_eq!(c.id, "sync-confirm-unclear");
    }

    /// The two bad trashes give the same outlook and are not the same news:
    /// one leaves the file in the system trash, the other leaves nothing. The
    /// summary must not print one sentence for both.
    #[test]
    fn the_summary_says_which_of_the_two_bad_trashes_this_is() {
        let opaque = ready(
            vec![step(1, SyncStepKind::Overwrite, DestTrash::Opaque)],
            DestTrash::Opaque,
        )
        .summary_lines(Lang::En);
        let absent = ready(
            vec![step(1, SyncStepKind::Overwrite, DestTrash::Absent)],
            DestTrash::Absent,
        )
        .summary_lines(Lang::En);
        assert_ne!(opaque, absent, "dos destinos distintos, dos avisos");
        assert!(
            opaque.iter().any(|l| l.contains("system trash")),
            "lo enterrado se puede rescatar a mano, y hay que decirlo: {opaque:?}"
        );
        assert!(absent.iter().any(|l| l.contains("no trash")), "{absent:?}");
    }

    /// The confirmation says what the summary said. A plan that is partly
    /// reversible must not be confirmed as if none of it were.
    #[test]
    fn the_second_question_never_contradicts_the_summary() {
        // A restorable trash with an irreversible step: only a newer daemon
        // produces it, and `Partial` is what it means.
        let odd = SyncStep {
            reversal: Some(StepReversal::Irreversible),
            reason: Some(SyncReason::NoTrashOnTarget),
            ..step(2, SyncStepKind::Overwrite, DestTrash::Restorable)
        };
        let plan = ready(
            vec![step(1, SyncStepKind::Copy, DestTrash::Restorable), odd],
            DestTrash::Restorable,
        );
        assert_eq!(plan.outlook(), UndoOutlook::Partial);
        let c = plan.confirmation(Lang::En).expect("segunda pregunta");
        assert_eq!(c.id, "sync-confirm-partial");
        assert!(
            !c.text.contains("none of them"),
            "el titular dice «parte sí», así que la pregunta no puede decir «nada»: {c:?}"
        );
    }

    /// A `Mirror` that only deletes has no bytes to write, and saying "0 B to
    /// write" reads as "this does nothing".
    #[test]
    fn a_deletion_only_plan_does_not_claim_zero_bytes() {
        let plan = ready(
            vec![step(1, SyncStepKind::DeleteTree, DestTrash::Restorable)],
            DestTrash::Restorable,
        );
        let lines = plan.summary_lines(Lang::En);
        assert!(!lines.iter().any(|l| l.contains("to write")), "{lines:?}");
    }

    /// After the fact the REPORT decides, not the plan: an apply that died
    /// before opening a journal batch left nothing to undo, whatever the
    /// dialog promised beforehand.
    #[test]
    fn a_plan_that_promised_an_undo_but_never_journalled_says_so_afterwards() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let mut s = SyncState::ready(steps, {
            let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
            done_for(&steps, DestTrash::Restorable)
        });
        assert_eq!(s.plan().expect("plan").outlook(), UndoOutlook::Full);
        s.on_apply_started(TaskId::new(9));
        s.on_report(SyncReportResult {
            done: 0,
            failed: 1,
            skipped: 0,
            bytes: 0,
            failures: vec![],
            batch_id: None,
            dest_trash: DestTrash::Restorable,
        });
        match &s {
            SyncState::Applied(a) => assert!(
                !a.is_undoable(),
                "sin lote de journal no hay nada que deshacer"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// A plan that cannot be approved cannot be started either — the guard
    /// lives in the model, not in whichever frontend remembers to ask.
    #[test]
    fn a_blocked_plan_cannot_be_talked_into_applying() {
        let steps = vec![step(1, SyncStepKind::Copy, DestTrash::Restorable)];
        let done = SyncPlanDone {
            executable: false,
            blockers_total: 1,
            ..done_for(&steps, DestTrash::Restorable)
        };
        let mut s = SyncState::ready(steps, done);
        s.on_apply_started(TaskId::new(9));
        assert!(matches!(s, SyncState::Ready(_)));
    }

    /// An empty plan is not an error and not a button either.
    #[test]
    fn an_empty_plan_is_not_approvable() {
        let plan = ready(vec![], DestTrash::Restorable);
        assert!(!plan.can_approve(), "no hay nada que aprobar");
        assert!(plan.confirmation(Lang::En).is_none());
    }

    /// The cursor is anchored to `SyncStep::id`, never to an index.
    #[test]
    fn the_cursor_walks_the_steps_and_clamps() {
        let mut plan = ready(
            vec![
                step(4, SyncStepKind::Copy, DestTrash::Restorable),
                step(9, SyncStepKind::Copy, DestTrash::Restorable),
            ],
            DestTrash::Restorable,
        );
        assert_eq!(plan.selected_id(), Some(4), "se selecciona el primero");
        plan.move_by(1);
        assert_eq!(plan.selected_id(), Some(9));
        plan.move_by(1);
        assert_eq!(plan.selected_id(), Some(9), "y para al final");
        plan.select(4);
        assert_eq!(plan.selected_step().expect("paso").id, 4);
        plan.select(1000);
        assert_eq!(
            plan.selected_id(),
            Some(4),
            "un id que no llegó no mueve nada"
        );
    }

    /// Every word this model paints is a Fluent id in BOTH locales. A missing
    /// message renders as the id itself, which is what a reader would see.
    #[test]
    fn every_label_is_translated_in_both_locales() {
        for lang in [Lang::En, Lang::Es] {
            for k in [
                SyncStepKind::CreateDir,
                SyncStepKind::Copy,
                SyncStepKind::Overwrite,
                SyncStepKind::DeleteTree,
                SyncStepKind::Skip,
                SyncStepKind::Unknown,
            ] {
                let s = step_label(k, lang);
                assert!(!s.starts_with("sync-"), "{lang:?} {k:?}: {s}");
            }
            for u in [
                StepUndo::Reverts,
                StepUndo::LeftBehind,
                StepUndo::Irreversible,
                StepUndo::Nothing,
                StepUndo::Unclear,
            ] {
                let s = undo_label(u, lang);
                assert!(!s.starts_with("sync-"), "{lang:?} {u:?}: {s}");
            }
            for r in [
                SyncReason::AmbiguousSource,
                SyncReason::UnknownConfidence,
                SyncReason::Unreadable,
                SyncReason::NoTrashOnTarget,
                SyncReason::Unknown,
            ] {
                let s = reason_label(r, lang);
                assert!(!s.starts_with("sync-"), "{lang:?} {r:?}: {s}");
            }
            for b in [
                SyncBlockerKind::AmbiguousDest,
                SyncBlockerKind::OverlapDetected,
                SyncBlockerKind::DestReadOnly,
                SyncBlockerKind::DirTooLarge,
                SyncBlockerKind::TypeMismatchDir,
                SyncBlockerKind::Unknown,
            ] {
                let s = blocker_label(b, lang);
                assert!(!s.starts_with("sync-"), "{lang:?} {b:?}: {s}");
            }
            for o in [
                UndoOutlook::Full,
                UndoOutlook::Partial,
                UndoOutlook::Nothing,
                UndoOutlook::Unclear,
            ] {
                let s = t_in(lang, &format!("sync-outlook-{}", o.id()));
                assert!(!s.starts_with("sync-"), "{lang:?} {o:?}: {s}");
            }
            for t in [
                DestTrash::Restorable,
                DestTrash::Opaque,
                DestTrash::Absent,
                DestTrash::Unknown,
            ] {
                let s = trash_label(t, lang);
                assert!(!s.starts_with("sync-"), "{lang:?} {t:?}: {s}");
            }
        }
    }

    /// Every message id this model can ASK for, including the branches a
    /// scenario test would have to be contrived to reach. An id with no
    /// message renders as the id itself, and the branch that emits it is the
    /// one a human meets on the worst day.
    #[test]
    fn every_message_this_model_can_ask_for_exists_in_both_locales() {
        let ids: &[(&str, &[(&str, &str)])] = &[
            ("sync-summary-irreversible", &[("n", "2")]),
            (
                "sync-summary-actions",
                &[
                    ("copy", "1"),
                    ("overwrite", "2"),
                    ("createdir", "3"),
                    ("deletetree", "4"),
                    ("skip", "5"),
                ],
            ),
            ("sync-summary-bytes", &[("bytes", "1.5 KiB")]),
            (
                "sync-summary-bytes-partial",
                &[("bytes", "1.5 KiB"), ("n", "340")],
            ),
            ("sync-summary-unreadable", &[("n", "3")]),
            ("sync-summary-mismatch", &[("received", "1"), ("n", "40")]),
            ("sync-summary-unnameable", &[("n", "1")]),
            ("sync-summary-malformed", &[("n", "1")]),
            ("sync-summary-contradictory", &[]),
            // #194. La prueba de que esta lista es load-bearing está en el
            // historial de su propia rama: `1dacd58` embarcó
            // `PlanIntegrity::DuplicateIds` y su `ta_in` SIN cadena en ningún
            // `.ftl`, `ta_in` devuelve el id cuando falta el mensaje, y la
            // suite no se puso roja. Las cadenas llegaron en `5ed8899`.
            ("sync-summary-duplicate-ids", &[("n", "1")]),
            ("sync-summary-blocked", &[("n", "300")]),
            ("sync-confirm-delete", &[("n", "4")]),
            ("sync-confirm-delete-final", &[("n", "4")]),
            ("sync-confirm-delete-partial", &[("n", "4"), ("steps", "2")]),
            ("sync-confirm-delete-unclear", &[("n", "4")]),
            ("sync-confirm-no-way-back", &[("n", "9")]),
            ("sync-confirm-partial", &[("n", "2")]),
            ("sync-confirm-unclear", &[("n", "9")]),
        ];
        for lang in [Lang::En, Lang::Es] {
            for (id, args) in ids {
                let text = ta_in(lang, id, args);
                assert!(!text.starts_with("sync-"), "{lang:?} {id}: sin mensaje");
                assert!(!text.contains('{'), "{lang:?} {id}: argumento sin resolver");
            }
        }
    }

    /// Every LINE the dialog can print, in both locales, with none of them
    /// coming out as its own id. The summary is the surface this task exists
    /// for; an untranslated line there is a sentence a human cannot read.
    #[test]
    fn every_summary_line_is_translated_in_both_locales() {
        let steps = vec![
            step(1, SyncStepKind::Copy, DestTrash::Absent),
            step(2, SyncStepKind::Overwrite, DestTrash::Absent),
            step(3, SyncStepKind::DeleteTree, DestTrash::Absent),
            SyncStep {
                reason: Some(SyncReason::Unreadable),
                ..step(4, SyncStepKind::Skip, DestTrash::Absent)
            },
        ];
        // Counts that do not match, so the integrity line prints too.
        let done = SyncPlanDone {
            counts: SyncCounts {
                copy: 9,
                ..counts_of(&steps)
            },
            blockers_total: 3,
            executable: false,
            ..done_for(&steps, DestTrash::Absent)
        };
        for lang in [Lang::En, Lang::Es] {
            let state = SyncState::ready(steps.clone(), done.clone());
            let plan = state.plan().expect("plan");
            let mut lines = plan.summary_lines(lang);
            lines.extend(plan.confirmation(lang).map(|c| c.text));
            assert!(lines.len() >= 6, "{lines:?}");
            for line in &lines {
                assert!(!line.starts_with("sync-"), "{lang:?}: {line}");
                assert!(!line.contains('{'), "argumento sin resolver: {line}");
            }
        }
        // …and the second question of a plan that both deletes and cannot be
        // undone, which the block above cannot reach (it is not approvable).
        for lang in [Lang::En, Lang::Es] {
            let plan = ready(
                vec![step(1, SyncStepKind::DeleteTree, DestTrash::Absent)],
                DestTrash::Absent,
            );
            let c = plan.confirmation(lang).expect("segunda pregunta");
            assert_eq!(c.id, "sync-confirm-delete-final");
            assert!(!c.text.starts_with("sync-") && !c.text.contains('{'));
        }
    }
}
