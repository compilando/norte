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
//! [`SyncPlanDone`] and why every claim this module makes is a function of the
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
//!   [`SyncPlanDone::counts`]**, not trusted. The feed closes on a dropped
//!   batch (task 10), but a model that can count should count: a plan whose
//!   steps do not add up to what the daemon closed with cannot be approved.
//! * **[`SyncStep::rel`] is not always relative to the source root.** A
//!   `DeleteTree` and the `Skip` of a destination listing that would not read
//!   are measured against the DESTINATION, and the step carries no side —
//!   [`anchor_of`] answers [`RelAnchor::Either`] rather than letting a pane
//!   paint them in a column they do not belong to.

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::methods::{
    CompareRow, DestTrash, PlanHash, RelPath, SYNC_MAX_INCLUDE, Side, StepReversal, SyncBlocker,
    SyncBlockerKind, SyncCounts, SyncFailureCause, SyncMode, SyncPlanDone, SyncReason,
    SyncReportResult, SyncStep, SyncStepKind, SyncStepsBatch,
};
use norte_proto::{TaskId, TaskState, VPath};
use unicode_normalization::UnicodeNormalization;

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

/// The reader's word for a [`StepUndo`].
///
/// ```
/// use norte_frontend::sync::{StepUndo, undo_label};
/// use norte_i18n::Lang;
/// assert!(!undo_label(StepUndo::LeftBehind, Lang::En).is_empty());
/// ```
#[must_use]
pub fn undo_label(undo: StepUndo, lang: Lang) -> String {
    let id = match undo {
        StepUndo::Reverts => "reverts",
        StepUndo::LeftBehind => "left-behind",
        StepUndo::Irreversible => "irreversible",
        StepUndo::Nothing => "nothing",
        StepUndo::Unclear => "unclear",
    };
    t_in(lang, &format!("sync-undo-{id}"))
}

/// The glyph for what a step DOES.
///
/// ```
/// use norte_frontend::sync::step_glyph;
/// use norte_proto::methods::SyncStepKind;
/// assert_eq!(step_glyph(SyncStepKind::Copy), '+');
/// assert_eq!(step_glyph(SyncStepKind::DeleteTree), '-');
/// ```
#[must_use]
pub fn step_glyph(kind: SyncStepKind) -> char {
    match kind {
        SyncStepKind::CreateDir => 'D',
        SyncStepKind::Copy => '+',
        SyncStepKind::Overwrite => '#',
        SyncStepKind::DeleteTree => '-',
        SyncStepKind::Skip => '.',
        // A class from a newer daemon: it has a name this build does not know,
        // so it does not borrow another class's mark.
        _ => '?',
    }
}

/// The reader's word for what a step does.
///
/// ```
/// use norte_frontend::sync::step_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncStepKind;
/// assert!(!step_label(SyncStepKind::Overwrite, Lang::En).is_empty());
/// ```
#[must_use]
pub fn step_label(kind: SyncStepKind, lang: Lang) -> String {
    let id = match kind {
        SyncStepKind::CreateDir => "create-dir",
        SyncStepKind::Copy => "copy",
        SyncStepKind::Overwrite => "overwrite",
        SyncStepKind::DeleteTree => "delete-tree",
        SyncStepKind::Skip => "skip",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-step-{id}"))
}

/// The reader's word for why a step is a `Skip` or is irreversible.
///
/// [`SyncReason::NoTrashOnTarget`] covers BOTH "there is no trash" and "there
/// is one that cannot say where it put things", so its message must not claim
/// the first — the wire token deliberately does not distinguish them.
///
/// ```
/// use norte_frontend::sync::reason_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncReason;
/// assert!(!reason_label(SyncReason::NoTrashOnTarget, Lang::En).is_empty());
/// ```
#[must_use]
pub fn reason_label(reason: SyncReason, lang: Lang) -> String {
    let id = match reason {
        SyncReason::AmbiguousSource => "ambiguous-source",
        SyncReason::UnknownConfidence => "unknown-confidence",
        SyncReason::Unreadable => "unreadable",
        SyncReason::NoTrashOnTarget => "no-trash-on-target",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-reason-{id}"))
}

/// The reader's word for why a plan cannot run.
///
/// ```
/// use norte_frontend::sync::blocker_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncBlockerKind;
/// assert!(!blocker_label(SyncBlockerKind::DestReadOnly, Lang::En).is_empty());
/// ```
#[must_use]
pub fn blocker_label(kind: SyncBlockerKind, lang: Lang) -> String {
    let id = match kind {
        SyncBlockerKind::AmbiguousDest => "ambiguous-dest",
        SyncBlockerKind::OverlapDetected => "overlap-detected",
        SyncBlockerKind::DestReadOnly => "dest-read-only",
        SyncBlockerKind::DirTooLarge => "dir-too-large",
        SyncBlockerKind::TypeMismatchDir => "type-mismatch-dir",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-blocker-{id}"))
}

/// El calificador de un [`RelAnchor`], o `None` cuando la ruta cuelga del
/// ORIGEN y no hace falta decir nada.
///
/// Vive aquí y no en cada painter por lo mismo que [`failure_cause_label`]:
/// la revisión de rama de C2 lo encontró transcrito a mano en tres sitios
/// —`norte-tui/src/ui.rs` y dos veces en `norte-gui/src/sync_view.rs`— y a la
/// CLI se le había olvidado, que es la forma en que esta clase de duplicado
/// se nota tarde (rust MAJOR-3, encoding MAJOR-1).
///
/// `None` para [`RelAnchor::Source`] a propósito: un calificador vacío
/// pintado igualmente mete un espacio en banda, y esta pantalla ya tiene un
/// problema con los separadores que un nombre puede llevar dentro.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, anchor_label};
/// use norte_i18n::Lang;
/// assert!(anchor_label(RelAnchor::Source, Lang::En).is_none());
/// assert_ne!(
///     anchor_label(RelAnchor::Dest, Lang::En),
///     anchor_label(RelAnchor::Either, Lang::En),
/// );
/// ```
#[must_use]
pub fn anchor_label(anchor: RelAnchor, lang: Lang) -> Option<String> {
    match anchor {
        RelAnchor::Source => None,
        RelAnchor::Dest => Some(t_in(lang, "sync-anchor-dest")),
        RelAnchor::Either => Some(t_in(lang, "sync-anchor-either")),
    }
}

/// The reader's parenthetical for [`StepCells::dest_rel_twin`] /
/// [`FailureCells::dest_rel_twin`] (#192), shaped exactly like
/// [`anchor_label`] so a painter drops it in the same way: `None` when there
/// is nothing to say, `Some` otherwise.
///
/// A hostile badge would be a LIE about the name — `café.txt` (NFC) and
/// `café.txt` (NFD) are both valid UTF-8 and neither is hostile — so this is
/// a separate sentence, never a badge folded into [`RelDisplay::hostile`].
///
/// ```
/// use norte_frontend::sync::dest_twin_label;
/// use norte_i18n::Lang;
/// assert!(dest_twin_label(false, Lang::En).is_none());
/// assert!(dest_twin_label(true, Lang::En).is_some());
/// ```
#[must_use]
pub fn dest_twin_label(twin: bool, lang: Lang) -> Option<String> {
    twin.then(|| t_in(lang, "sync-dest-twin"))
}

/// El nombre de un [`SyncMode`], que la cabecera pinta.
///
/// El `_` NO cae a «update»: un modo que este build no sabe nombrar tiene que
/// decirlo, porque la diferencia entre los dos que sí conoce es si BORRA.
/// Estaba escrito dos veces en esta rama, una por frontend (rust MAJOR-3).
///
/// ```
/// use norte_frontend::sync::mode_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncMode;
/// assert_ne!(
///     mode_label(SyncMode::Update, Lang::En),
///     mode_label(SyncMode::Mirror, Lang::En),
/// );
/// ```
#[must_use]
pub fn mode_label(mode: SyncMode, lang: Lang) -> String {
    match mode {
        SyncMode::Update => t_in(lang, "sync-mode-update"),
        SyncMode::Mirror => t_in(lang, "sync-mode-mirror"),
        _ => t_in(lang, "sync-mode-unknown"),
    }
}

/// The reader's word for why ONE step of an applied plan did not happen
/// ([`norte_proto::methods::SyncFailure::cause`]).
///
/// Shared, and not a `match` per frontend, for the reason the rest of this
/// module is shared: the CLI wrote this table first (phase A) and the GUI
/// needed the same five sentences (phase C2). Two copies of a table whose
/// `_` arm exists precisely to survive a NEWER daemon is two chances for one
/// of them to name a cause the other calls "unrecognised".
///
/// The `_` arm covers both `Unknown` (the decoder's `#[serde(other)]`, which
/// the core never emits) and the enum's `#[non_exhaustive]`: a wire that
/// grows a sixth cause lands there instead of failing to compile.
///
/// ```
/// use norte_frontend::sync::failure_cause_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::SyncFailureCause;
/// assert_ne!(
///     failure_cause_label(SyncFailureCause::Denied, Lang::En),
///     failure_cause_label(SyncFailureCause::Io, Lang::En),
/// );
/// ```
#[must_use]
pub fn failure_cause_label(cause: SyncFailureCause, lang: Lang) -> String {
    let id = match cause {
        SyncFailureCause::Conflict => "conflict",
        SyncFailureCause::Denied => "denied",
        SyncFailureCause::IllegalName => "illegal-name",
        SyncFailureCause::Io => "io",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-cause-{id}"))
}

/// What the destination's trash means for the human, in words.
///
/// [`UndoOutlook`] answers "does the undo give it back", and for both bad
/// trashes that answer is no — but they are not the same situation and a
/// dialog must not print one sentence for both: with a [`DestTrash::Opaque`]
/// trash (macOS, Windows) what was replaced is sitting in the system trash and
/// can be fished out by hand, and with [`DestTrash::Absent`] it is gone. That
/// distinction is the reason the wire carries three values instead of a
/// boolean, and this is where it reaches the reader.
///
/// ```
/// use norte_frontend::sync::trash_label;
/// use norte_i18n::Lang;
/// use norte_proto::methods::DestTrash;
/// assert_ne!(
///     trash_label(DestTrash::Opaque, Lang::En),
///     trash_label(DestTrash::Absent, Lang::En),
///     "una papelera del sistema y ninguna papelera no son la misma frase"
/// );
/// ```
#[must_use]
pub fn trash_label(dest_trash: DestTrash, lang: Lang) -> String {
    let id = match dest_trash {
        DestTrash::Restorable => "restorable",
        DestTrash::Opaque => "opaque",
        DestTrash::Absent => "absent",
        _ => "unknown",
    };
    t_in(lang, &format!("sync-trash-{id}"))
}

/// Which root a step's [`SyncStep::rel`] hangs from.
///
/// A pane has two columns and the wire has no side field, so this is what
/// stops a `DeleteTree` from being painted under the source. See
/// [`anchor_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelAnchor {
    /// The source root (and, through `dest_rel`, the destination too).
    Source,
    /// The destination root only: there is no source path to name.
    Dest,
    /// Could be either, and the step does not say. Paint it in neither
    /// column, or in both — but do not claim one.
    Either,
}

/// Which root `step`'s `rel` is measured against.
///
/// Normative on the wire ([`SyncStep::rel`]): "almost always the source". The
/// two exceptions are a [`SyncStepKind::DeleteTree`], which only ever speaks
/// about the destination, and the [`SyncReason::Unreadable`] `Skip` — which is
/// emitted for an unreadable listing on EITHER side and carries nothing to
/// tell them apart. Answering `Source` for it would paint a destination path
/// under the source column, which is why the third variant exists.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, anchor_of};
/// use norte_proto::methods::SyncStepKind;
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncStep};
/// let del = SyncStep {
///     id: 1,
///     kind: SyncStepKind::DeleteTree,
///     rel: RelPath::parse_wire("old").expect("rel"),
///     dest_rel: None,
///     size: None,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::RestoreTrash),
///     reason: None,
/// };
/// assert_eq!(anchor_of(&del), RelAnchor::Dest);
/// ```
#[must_use]
pub fn anchor_of(step: &SyncStep) -> RelAnchor {
    match step.kind {
        SyncStepKind::DeleteTree => RelAnchor::Dest,
        // The step names a destination path explicitly, so whatever the class
        // is, `rel` is the source's half of the pair.
        _ if step.dest_rel.is_some() => RelAnchor::Source,
        SyncStepKind::CreateDir | SyncStepKind::Copy | SyncStepKind::Overwrite => RelAnchor::Source,
        // A collision or a confidence the caller asked to skip is a fact about
        // the SOURCE. Everything else a `Skip` can say — an unreadable listing
        // (emitted for either side, with nothing to tell them apart), a reason
        // this build cannot name, no reason at all — could be the
        // destination's.
        SyncStepKind::Skip => match step.reason {
            Some(SyncReason::AmbiguousSource | SyncReason::UnknownConfidence) => RelAnchor::Source,
            _ => RelAnchor::Either,
        },
        // A class a newer daemon named. `DeleteTree` proves a destination-only
        // class is a shape this family HAS, so painting an unknown one under
        // the source column would put a path in a tree it may not be in.
        _ => RelAnchor::Either,
    }
}

/// Which root a [`SyncBlocker`]'s `rel` hangs from — the third member of the
/// family [`anchor_of`] and [`render_failure`] already form (#189).
///
/// [`SyncBlocker::side`] is normative when it is present: the wire's
/// convention is [`Side::Left`] for the source and [`Side::Right`] for the
/// destination, always, independent of which pane launched the plan. Three of
/// the four named kinds name a DESTINATION path by definition even without a
/// `side` — [`SyncBlockerKind::AmbiguousDest`], [`SyncBlockerKind::DestReadOnly`]
/// and [`SyncBlockerKind::DirTooLarge`] are never about the source.
/// [`SyncBlockerKind::OverlapDetected`] is about both roots at once, so it
/// answers [`RelAnchor::Either`] rather than pick one it cannot justify.
/// [`SyncBlockerKind::TypeMismatchDir`] carries `side` on the wire ALWAYS
/// (normative, see its rustdoc), so its fall-through here is defensive, not
/// reachable against a conforming daemon.
///
/// Without this, the obvious code for a pane that lists blockers is
/// `rel_display(&b.rel, view.source_encoding)`, which reproduces #152 against
/// three paths that are never the source's spelling.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, blocker_anchor};
/// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
/// let dest_ro = SyncBlocker {
///     rel: RelPath::parse_wire("").expect("rel"),
///     kind: SyncBlockerKind::DestReadOnly,
///     side: None,
/// };
/// assert_eq!(blocker_anchor(&dest_ro), RelAnchor::Dest);
///
/// // `side` wins when present, even against a kind that would otherwise
/// // derive the opposite anchor.
/// let overlap_from_the_left = SyncBlocker {
///     rel: RelPath::parse_wire("shared").expect("rel"),
///     kind: SyncBlockerKind::OverlapDetected,
///     side: Some(Side::Left),
/// };
/// assert_eq!(blocker_anchor(&overlap_from_the_left), RelAnchor::Source);
/// ```
#[must_use]
pub fn blocker_anchor(blocker: &SyncBlocker) -> RelAnchor {
    match blocker.side {
        Some(Side::Right) => RelAnchor::Dest,
        Some(Side::Left) => RelAnchor::Source,
        // `Side::Unknown` is the decoder's `#[serde(other)]` fallback, which
        // the core never emits — treated the same as absent, since neither
        // root is provably named.
        Some(Side::Unknown) | None => match blocker.kind {
            SyncBlockerKind::AmbiguousDest
            | SyncBlockerKind::DestReadOnly
            | SyncBlockerKind::DirTooLarge => RelAnchor::Dest,
            _ => RelAnchor::Either,
        },
    }
}

/// A relative path ready to paint: masked text, the original bytes, and the
/// flag that says the two differ (rule 1, spec §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelDisplay {
    /// Display form: lossy, and safe to paint. NEVER empty for the root
    /// —which is what a whole-tree blocker such as
    /// [`SyncBlockerKind::DestReadOnly`] carries— when built through
    /// [`rel_display_or_root`]: a pane says "the whole tree" there rather
    /// than nothing. [`rel_display`] itself has no [`Lang`] to say it in and
    /// stays empty; it is the wrapper's job (#193).
    pub text: String,
    /// The path's ORIGINAL bytes, `/`-joined. A theme matches an extension
    /// against these, never against [`RelDisplay::text`].
    pub raw: Vec<u8>,
    /// Some segment was altered to be painted: badge it.
    pub hostile: bool,
}

/// One relative path, masked exactly the way a listing masks a name.
///
/// ```
/// use norte_frontend::sync::rel_display;
/// use norte_proto::methods::RelPath;
/// let d = rel_display(&RelPath::parse_wire("sub/a.txt").expect("rel"), None);
/// assert_eq!(d.text, "sub/a.txt");
/// assert!(!d.hostile);
/// ```
#[must_use]
pub fn rel_display(rel: &RelPath, reinterpret: Option<norte_encoding::NameEncoding>) -> RelDisplay {
    let mut text = String::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut hostile = false;
    for (i, seg) in rel.segments().iter().enumerate() {
        if i > 0 {
            text.push('/');
            raw.push(b'/');
        }
        let bytes = seg.as_bytes();
        let (masked, h) = crate::display_name_with(bytes, reinterpret);
        hostile |= h;
        text.push_str(&masked);
        raw.extend_from_slice(bytes);
    }
    RelDisplay { text, raw, hostile }
}

/// The same as [`rel_display`], except the root reads as the localized
/// "whole tree" sentence instead of an empty string — the contract
/// [`RelDisplay::text`] documents and no painter honoured (#193).
///
/// Lives here, once, and not per painter: a root [`RelDisplay`] is not a
/// [`SyncBlocker`] concept, it is a [`RelPath::is_root`] one, so whichever
/// surface eventually paints a whole-tree blocker gets the same sentence
/// without writing it again. An EMPTY string painted in a pane reads as
/// "there is no row here", the opposite of what the wire is saying — a
/// [`SyncBlockerKind::DestReadOnly`] names the destination root precisely
/// because there IS something to say about every path under it.
///
/// [`RelDisplay::raw`] and [`RelDisplay::hostile`] are untouched: the root has
/// no bytes to badge, so `raw` stays empty and `hostile` stays `false` — the
/// sentence is this module's words, not a reading of the name.
///
/// ```
/// use norte_frontend::sync::rel_display_or_root;
/// use norte_i18n::Lang;
/// use norte_proto::methods::RelPath;
/// let whole = rel_display_or_root(&RelPath::parse_wire("").expect("rel"), None, Lang::En);
/// assert!(!whole.text.is_empty());
/// assert!(whole.raw.is_empty(), "no hay bytes que decir");
/// assert!(!whole.hostile, "la raíz no es un nombre hostil");
///
/// // Cualquier otra ruta se comporta exactamente como `rel_display`.
/// let named = rel_display_or_root(&RelPath::parse_wire("a.txt").expect("rel"), None, Lang::En);
/// assert_eq!(named.text, "a.txt");
/// ```
#[must_use]
pub fn rel_display_or_root(
    rel: &RelPath,
    reinterpret: Option<norte_encoding::NameEncoding>,
    lang: Lang,
) -> RelDisplay {
    let display = rel_display(rel, reinterpret);
    if rel.is_root() {
        RelDisplay {
            text: t_in(lang, "sync-rel-root"),
            ..display
        }
    } else {
        display
    }
}

/// Las reinterpretaciones de nombres (#57) de los dos lados de una
/// sincronización.
///
/// Una struct con dos campos NOMBRADOS y no una tupla `(Option<_>,
/// Option<_>)`: los dos valores son del mismo tipo, así que trasponerlos
/// compila — y trasponerlos ES el #152, un `dest_rel` decodificado con el
/// codepage del ORIGEN, o sea nombrando otros bytes que el fichero sobre el
/// que cae la escritura. Aquí el compilador no ayuda; el nombre sí.
///
/// El default —ninguna de las dos— es lo correcto para un frontend que no
/// tiene overrides por ubicación, como el CLI: los nombres se leen como
/// vienen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncEncodings {
    /// La del lado ORIGEN.
    pub source: Option<norte_encoding::NameEncoding>,
    /// La del lado DESTINO, que puede ser otra: los dos panes son dos
    /// ubicaciones y pueden llevar overrides distintos.
    pub dest: Option<norte_encoding::NameEncoding>,
}

impl SyncEncodings {
    /// Con cuál de las dos se lee una ruta anclada en `anchor`.
    ///
    /// Es la mitad del #152 que no estaba escrita en ninguna parte: la
    /// ortografía del destino se leía con la del destino (eso ya lo hacía cada
    /// frontend a mano), pero el `rel` de un [`SyncStepKind::DeleteTree`]
    /// —que cuelga del DESTINO, ver [`anchor_of`]— se leía con la del ORIGEN.
    /// Con dos panes con overrides distintos, eso nombra el subárbol que se va
    /// a borrar con el codepage del árbol que NO se toca, en la pantalla donde
    /// se aprueba borrarlo.
    ///
    /// [`RelAnchor::Either`] se lee con la del origen, que es de donde cuelga
    /// «casi siempre» un `rel` (normativo en el wire): no se sabe, y elegir la
    /// otra no sería más cierto — lo que un pane no debe hacer con un `Either`
    /// es afirmar la COLUMNA, y eso lo dice [`StepCells::anchor`].
    ///
    /// Dos cosas hacen ese brazo menos peligroso de lo que parece, y las dos
    /// se pierden si no se escriben:
    ///
    /// * la elección solo CAMBIA algo para bytes que no son UTF-8 válido
    ///   (`display_name_with` no reinterpreta el UTF-8 válido), y en ese caso
    ///   el resultado es SIEMPRE `hostile = true` — o sea que un `Either`
    ///   leído con el codepage del otro lado llega marcado como «este texto no
    ///   son los bytes» a las dos superficies;
    /// * **para un PASO**, el único camino destructivo hasta `Either` es un
    ///   [`SyncStepKind::Unknown`] ([`anchor_of`]), y un solo paso así deja el
    ///   plan en [`PlanIntegrity::Unnameable`], que no se puede aprobar. Lo
    ///   que queda bajo `Either` es un `Skip`, que no escribe nada.
    ///
    /// **Ese segundo punto NO vale para un FALLO del informe**
    /// ([`render_failure`]), y decirlo importa: un `DeleteTree` que falla por
    /// permisos contra un destino de solo lectura es la fila más corriente de
    /// un `Mirror`, su `rel` cuelga del DESTINO, y el informe no trae la clase
    /// que lo diría. Ahí este brazo sí puede nombrar un subárbol del destino
    /// con el codepage del árbol que no se toca. Lo que lo acota es que el
    /// resultado llega `hostile = true` y que el ancla se PINTA.
    ///
    /// ```
    /// use norte_frontend::sync::{RelAnchor, SyncEncodings};
    /// use norte_encoding::NameEncoding;
    /// let enc = SyncEncodings {
    ///     source: Some(NameEncoding::Cp437),
    ///     dest: None,
    /// };
    /// // Un `DeleteTree` habla del DESTINO, aunque se pinte en la primera
    /// // columna.
    /// assert_eq!(enc.for_anchor(RelAnchor::Dest), None);
    /// assert_eq!(enc.for_anchor(RelAnchor::Source), Some(NameEncoding::Cp437));
    /// // Y lo que no consta se lee como el origen, que es de donde cuelga
    /// // «casi siempre» un `rel`.
    /// assert_eq!(enc.for_anchor(RelAnchor::Either), Some(NameEncoding::Cp437));
    /// ```
    #[must_use]
    pub fn for_anchor(self, anchor: RelAnchor) -> Option<norte_encoding::NameEncoding> {
        match anchor {
            RelAnchor::Dest => self.dest,
            RelAnchor::Source | RelAnchor::Either => self.source,
        }
    }
}

/// The three glyphs a step paints: what it does, how sure the comparison was,
/// and whether it comes back.
///
/// Three and not two, and the third is the one that needed a wire field: see
/// the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepGlyphs {
    /// What the step does ([`step_glyph`]).
    pub kind: char,
    /// What the comparison's verdict is worth
    /// ([`crate::compare::confidence_glyph`] — the same marks as the diff
    /// pane, because it is the same question).
    pub confidence: char,
    /// Whether the undo gives it back ([`undo_glyph`]).
    pub undo: char,
}

/// Everything a painter needs for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCells {
    /// The step's id, for a cursor to anchor to.
    pub id: u64,
    /// The three marks.
    pub glyphs: StepGlyphs,
    /// Which root [`StepCells::rel`] hangs from.
    pub anchor: RelAnchor,
    /// The path, masked.
    pub rel: RelDisplay,
    /// The DESTINATION's own spelling, when its bytes differ from `rel`'s
    /// (#152). `Some` means the two sides spell one entry two ways and the
    /// pane must show both: the write lands on THIS one.
    pub dest_rel: Option<RelDisplay>,
    /// `dest_rel` is `Some` AND its NFC form matches `rel`'s NFC form, even
    /// though the bytes AND the `String`s differ — an NFC/NFD pair
    /// (precomposed `café.txt` vs `café.txt` spelled with a combining
    /// acute) is the canonical case: not `String`-equal (`'é'` is one
    /// `char`, `'e' + '\u{301}'` is two), valid UTF-8 on both sides so
    /// neither half is `hostile`, and rendered to the SAME glyph by any font
    /// that composes combining marks. Nothing else says the pane is not just
    /// repeating itself (#192). See [`dest_twin_label`].
    pub dest_rel_twin: bool,
    /// Bytes the step moves, when the provider said.
    pub size: Option<u64>,
    /// What the undo would do with it.
    pub undo: StepUndo,
    /// Why it is a `Skip` or why it cannot be undone.
    pub reason: Option<SyncReason>,
}

/// One step, ready to paint.
///
/// `dest_trash` is not optional and not defaulted: without it the undo column
/// cannot be computed, and a renderer that reads [`SyncStep::reversal`] on its
/// own is exactly the bug this module exists to prevent.
///
/// Each path is masked with the reinterpretation of the side it hangs from
/// ([`SyncEncodings::for_anchor`]), which is a decision no caller has to make
/// again: `dest_rel` is ALWAYS the destination's spelling (#152), and a
/// `DeleteTree`'s `rel` is a destination path too even though it sits in the
/// first column.
///
/// ```
/// use norte_frontend::sync::{StepUndo, SyncEncodings, render_step};
/// use norte_proto::methods::{DestTrash, SyncStepKind};
/// # use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, StepReversal,
/// #     SyncStep};
/// let step = SyncStep {
///     id: 3,
///     kind: SyncStepKind::Copy,
///     rel: RelPath::parse_wire("a.txt").expect("rel"),
///     dest_rel: None,
///     size: Some(10),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// let cells = render_step(&step, DestTrash::Absent, SyncEncodings::default());
/// assert_eq!(cells.undo, StepUndo::LeftBehind);
/// ```
#[must_use]
pub fn render_step(step: &SyncStep, dest_trash: DestTrash, enc: SyncEncodings) -> StepCells {
    let undo = step_undo(step, dest_trash);
    let anchor = anchor_of(step);
    let rel = rel_display(&step.rel, enc.for_anchor(anchor));
    // Siempre con la del DESTINO, sea cual sea el ancla: `dest_rel` existe
    // precisamente para enseñar la ortografía de allí, que es sobre la que
    // cae la escritura.
    //
    // Y se pliega AQUÍ cuando los BYTES coinciden, no en cada pintor y no
    // por el texto pintado: `RelDisplay::text` es lossy, así que
    // `caf\xe9.txt` y `caf\x82.txt` —dos ficheros distintos— son el mismo
    // `caf\u{FFFD}.txt`, y un pintor que compare textos esconde justo el
    // campo que existe para decir sobre qué nombre cae la escritura
    // (#152). El wire ya compara por bytes
    // (`SyncStep::shape_is_consistent`); esto es la misma regla, una sola
    // vez, para los tres frontends.
    let dest_rel = step
        .dest_rel
        .as_ref()
        .filter(|d| **d != step.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192: los BYTES ya distinguen las dos rutas (si no, `dest_rel` sería
    // `None`), pero pueden RENDERIZAR igual de todas formas — un par NFC/NFD
    // es UTF-8 válido en las dos mitades, así que ninguna llega `hostile`, y
    // ni siquiera `text == text` lo detecta: "é" precompuesta y "e" + acento
    // combinante son Strings DISTINTOS que una fuente compone al mismo
    // glifo. Por NFC y no por bytes NI por igualdad de String a secas —la
    // comparación es la única parte de esto que normaliza; `RelDisplay::text`
    // en sí sigue siendo el enmascarado byte-exacto de siempre.
    let dest_rel_twin = dest_rel
        .as_ref()
        .is_some_and(|d| d.text.nfc().eq(rel.text.nfc()));
    StepCells {
        id: step.id,
        glyphs: StepGlyphs {
            kind: step_glyph(step.kind),
            confidence: crate::compare::confidence_glyph(step.confidence),
            undo: undo_glyph(undo),
        },
        anchor,
        rel,
        dest_rel,
        dest_rel_twin,
        size: step.size,
        undo,
        reason: step.reason,
    }
}

/// Una fila del informe (`sync.report`), ya resuelta: las dos rutas leídas con
/// la reinterpretación que le toca a cada una, y la del destino PLEGADA cuando
/// los bytes coinciden.
///
/// Gemela de [`StepCells`], y separada de ella porque un
/// [`norte_proto::methods::SyncFailure`] no es un paso: no lleva clase, así que
/// no hay glifos que pintar ni undo que juzgar. Lo que sí comparte es lo que se
/// puede equivocar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureCells {
    /// La ruta del fallo, enmascarada y badgeada.
    pub rel: RelDisplay,
    /// La ortografía del DESTINO, si el informe la manda y DIFIERE en bytes.
    pub dest_rel: Option<RelDisplay>,
    /// Gemelo de [`StepCells::dest_rel_twin`], y por el mismo motivo (#192):
    /// `dest_rel` es `Some` pero pinta IGUAL que `rel` — un par NFC/NFD, por
    /// ejemplo, es UTF-8 válido en las dos mitades y ninguna llega `hostile`.
    pub dest_rel_twin: bool,
    /// De qué raíz cuelga [`FailureCells::rel`]: [`RelAnchor::Source`] cuando
    /// el informe manda `dest_rel`, y [`RelAnchor::Either`] cuando no — ver
    /// [`render_failure`]. **Un pintor tiene que pintarlo**: en un panel donde
    /// una ruta sin calificar significa «del origen», callar un `Either` es
    /// afirmar el origen.
    pub anchor: RelAnchor,
}

/// Resuelve UNA fila del informe, con las mismas dos reglas que
/// [`render_step`] y por los mismos dos motivos.
///
/// * **El plegado es por BYTES**, no por el texto pintado: `RelDisplay::text`
///   es lossy, así que dos ficheros distintos con un byte inválido cada uno se
///   pintan igual — y comparando textos, el campo que dice sobre qué nombre
///   cayó la escritura desaparece justo cuando los nombres son adversarios.
/// * **La ortografía del destino se lee con la del destino**, sea cual sea el
///   ancla (#152): existe precisamente para nombrar el fichero de allí.
///
/// # El ancla de un fallo casi nunca consta, y entonces es `Either`
/// [`SyncStep::rel`] es «casi siempre» del origen y [`anchor_of`] usa la CLASE
/// del paso para saber cuándo no lo es —un `DeleteTree` habla del destino—.
/// Un [`norte_proto::methods::SyncFailure`] no lleva clase: el informe se lee
/// sin el plan delante. Queda UNA prueba, y es la misma que usa
/// [`anchor_of`]: si el informe manda `dest_rel`, entonces `rel` es la mitad
/// del ORIGEN de la pareja (misma regla y mismo campo, ver
/// [`norte_proto::methods::SyncFailure::dest_rel`]). Sin `dest_rel` no se
/// sabe, y decir «origen» sería justo lo que [`RelAnchor::Either`] existe
/// para no hacer — **un `DeleteTree` que falla por permisos es la fila hostil
/// MÁS común de un `Mirror`**, y su `rel` cuelga del destino.
///
/// Lo que un pintor NO puede hacer con un `Either` es callarse: en un panel
/// donde una ruta sin calificar significa «del origen» (así lo escribe
/// [`StepCells::anchor`]), el silencio es la afirmación. El ancla viaja en
/// [`FailureCells::anchor`] para que se pinte, y la auditoría de encoding de
/// esta fase (MAJOR-2) es exactamente eso.
///
/// La DECODIFICACIÓN de un `Either` sigue siendo la del origen
/// ([`SyncEncodings::for_anchor`]) porque no hay nada mejor que elegir; con
/// dos overrides #57 distintos eso puede nombrar un subárbol del destino con
/// el codepage del árbol que no se tocó, y llega marcado como hostil pero no
/// como «del otro lado».
///
/// **Desde 0.42.0 hay con qué cerrarlo, y esta función todavía no lo usa**
/// (#195 lo puso en el wire, #208 lo consume):
/// [`norte_proto::methods::SyncFailure::kind`] lleva la clase que el core tenía
/// en la mano y tiraba, así que un `DeleteTree` que falló ya se puede anclar en
/// el DESTINO con la misma regla que [`anchor_of`] aplica a un paso, en vez de
/// caer en `Either`. Cambiar lo que este módulo devuelve cambia lo que dos
/// frontends pintan, así que no viaja en el bump del wire.
///
/// ```
/// use norte_frontend::sync::{RelAnchor, SyncEncodings, render_failure};
/// use norte_proto::methods::{RelPath, SyncFailure, SyncFailureCause, SyncStepKind};
/// let f = SyncFailure {
///     rel: RelPath::parse_wire("sub/a.txt").expect("rel"),
///     dest_rel: Some(RelPath::parse_wire("sub/a.txt").expect("rel")),
///     cause: SyncFailureCause::Denied,
///     kind: SyncStepKind::Copy,
/// };
/// let cells = render_failure(&f, SyncEncodings::default());
/// assert_eq!(cells.rel.text, "sub/a.txt");
/// assert!(cells.dest_rel.is_none(), "la misma ortografía no se repite");
/// // Con `dest_rel` en el wire, `rel` es la mitad del ORIGEN de la pareja —
/// // aunque las dos ortografías coincidan y no haya nada que pintar aparte.
/// assert_eq!(cells.anchor, RelAnchor::Source);
///
/// // Sin `dest_rel` no hay prueba, y eso NO es «del origen»: el `rel` de un
/// // `DeleteTree` que falló cuelga del destino.
/// let solo = SyncFailure {
///     rel: RelPath::parse_wire("viejo").expect("rel"),
///     dest_rel: None,
///     cause: SyncFailureCause::Io,
///     kind: SyncStepKind::DeleteTree,
/// };
/// // …y desde 0.42.0 el wire SÍ lo dice (`kind`), pero esta función todavía
/// // no lo lee: cambiar lo que pinta es la ola siguiente, no el bump.
/// assert_eq!(
///     render_failure(&solo, SyncEncodings::default()).anchor,
///     RelAnchor::Either
/// );
/// ```
#[must_use]
pub fn render_failure(
    failure: &norte_proto::methods::SyncFailure,
    enc: SyncEncodings,
) -> FailureCells {
    // La única prueba que queda en el wire, y es la MISMA que usa `anchor_of`
    // para un paso: con `dest_rel` presente, `rel` es la mitad del origen.
    let anchor = if failure.dest_rel.is_some() {
        RelAnchor::Source
    } else {
        RelAnchor::Either
    };
    let rel = rel_display(&failure.rel, enc.for_anchor(anchor));
    let dest_rel = failure
        .dest_rel
        .as_ref()
        .filter(|d| **d != failure.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192, la misma regla que `render_step`: por NFC, no por igualdad de
    // `String` a secas — "é" precompuesta y "e" + acento combinante son
    // Strings distintos que rinden al mismo glifo.
    let dest_rel_twin = dest_rel
        .as_ref()
        .is_some_and(|d| d.text.nfc().eq(rel.text.nfc()));
    FailureCells {
        rel,
        dest_rel,
        dest_rel_twin,
        anchor,
    }
}

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
fn total_steps(counts: &SyncCounts) -> u64 {
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
    task_id: Option<TaskId>,
    steps: Vec<SyncStep>,
    counts: SyncCounts,
    unreadable: u64,
    malformed: u64,
    /// The highest id seen so far. The wire says a step's id is MONOTONIC
    /// within one plan, so the maximum is all it takes to catch a repeat as it
    /// arrives (#194) — and it takes eight bytes instead of a set that grows
    /// with the plan, which is the other half of #196.
    max_id: Option<u64>,
    /// How many steps arrived with an id that did not advance past
    /// [`Self::max_id`]: a repeat, or an order the wire forbids.
    duplicate_ids: u64,
    /// Steps counted but NOT retained, because
    /// [`PLAN_STEPS_RETAINED_MAX`] was already reached.
    dropped: u64,
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
    done: SyncPlanDone,
    steps: Vec<SyncStep>,
    integrity: PlanIntegrity,
    unreadable: u64,
    selected: Option<u64>,
    /// Steps counted but not retained ([`PLAN_STEPS_RETAINED_MAX`], #196).
    dropped: u64,
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
    plan: SyncPlan,
    task_id: TaskId,
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
    plan: SyncPlan,
    report: SyncReportResult,
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

/// Cómo va la Task de un panel de sincronización, para la barra de estado.
///
/// Deliberadamente MÁS CORTO que [`crate::compare::CompareState`]: aquí el
/// «llegaron todas las filas» no se deduce de un conteo, lo DICE el
/// `sync.plan_done` — sin él no hay `plan_hash` y no hay nada que aprobar, así
/// que un plan incompleto no es un estado que pintar sino un plan que no
/// existe (`SyncPlanEvent`, ADR 0049).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncRunState {
    /// Una Task viva: se está planificando, o se está aplicando.
    #[default]
    Running,
    /// La Task terminó bien.
    Done,
    /// El usuario canceló.
    Cancelled,
    /// La Task falló (el error va por la barra).
    Failed,
}

impl SyncRunState {
    /// El desenlace de la Task que corre detrás del panel —la del plan
    /// primero, la de la aplicación después— leído de su [`TaskState`]
    /// terminal.
    ///
    /// Deliberadamente NO toca el error localizado que cada frontend pinta
    /// (una barra truncada en la TUI, algo distinto en la GUI): eso es la
    /// única mitad que legítimamente difiere entre las dos, y mezclarla aquí
    /// ataría este mapeo puro a un [`Lang`] sin necesidad. Lo que SÍ era una
    /// sola decisión repetida a mano —`Cancelled`/`Failed`/lo demás→`Done`—
    /// es lo que vive aquí, para que un `_ => Done` no se transcriba dos
    /// veces y un día se le olvide un brazo a una de las dos copias.
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

/// El panel de sincronización abierto: el modelo puro de [`SyncState`] más lo
/// que un frontend necesita para pintarlo y para hablar con el backend.
///
/// El reparto es el mismo que el de [`crate::compare::CompareView`] (regla
/// dura 7): el estado del diálogo —qué pasos llegaron, si cuadran con lo que
/// el daemon cerró, qué devuelve el undo y cuál es la segunda pregunta— vive
/// aquí, donde se prueba sin terminal. Lo que cada frontend añade son las dos
/// raíces que la cabecera pinta, el estado del run y la pregunta de
/// confirmación EN CURSO — y esas también viven aquí (#161): la TUI y la GUI
/// necesitan el MISMO envoltorio, no dos reimplementados por separado.
#[derive(Debug)]
pub struct SyncView {
    /// El modelo del diálogo (Task 12).
    pub state: SyncState,
    /// Cómo va la Task que está corriendo ahora mismo (la del plan primero, la
    /// de la aplicación después).
    pub run: SyncRunState,
    /// Modo pedido, que la cabecera pinta: un `Mirror` borra y un `Update` no,
    /// y el lector tiene que verlo antes de aprobar.
    pub mode: SyncMode,
    /// Raíz ORIGEN. De ella cuelgan las `rel` de casi todos los pasos.
    pub source_root: VPath,
    /// Raíz DESTINO. De ella cuelgan las de un `DeleteTree` y las de un `Skip`
    /// ilegible ([`anchor_of`]).
    pub dest_root: VPath,
    /// Reinterpretación de nombres (#57) del pane ORIGEN, congelada al abrir.
    pub source_encoding: Option<norte_encoding::NameEncoding>,
    /// La del pane DESTINO, que puede ser otra.
    ///
    /// Dos y no una, por lo mismo que el panel de diferencias lleva dos: los
    /// dos panes son dos ubicaciones y pueden llevar overrides distintos. Aquí
    /// además importa más, porque `SyncStep::dest_rel` existe precisamente
    /// para enseñar la ortografía del DESTINO (#152) — decodificarla con el
    /// codepage del ORIGEN nombraría con otros bytes el fichero sobre el que
    /// va a caer la escritura.
    pub dest_encoding: Option<norte_encoding::NameEncoding>,
    /// La segunda pregunta, ya formulada y esperando un `y`.
    ///
    /// `None` = todavía no se ha pulsado aprobar, o el plan no la necesitaba.
    /// Vive aquí y no en el modelo porque es estado de INTERACCIÓN —a medio
    /// contestar— y el modelo de Task 12 no retrocede: preguntar es de la
    /// pantalla, decidir es suyo.
    pub confirming: Option<Confirmation>,
    /// Ya se pidió cancelar (el primer `Esc`), igual que en el panel de
    /// diferencias y por el mismo motivo: el segundo `Esc` cierra pase lo que
    /// pase con la Task.
    pub cancel_requested: bool,
    /// Categoría del error de una Task que FALLÓ, ya localizada y saneada.
    pub error: Option<String>,
    /// El `sync.apply` ya SALIÓ y el daemon todavía no ha contestado.
    ///
    /// Privado a propósito: la única forma de echarlo es [`SyncView::submit`]
    /// y la única de leerlo, [`SyncView::is_submitted`]. Lo que lo hace
    /// necesario es que `Applying` NO llega con la tecla sino una vuelta
    /// entera después, cuando el daemon devuelve la Task — en una GUI que lee
    /// eventos entre teclas esa ventana admite un segundo `a`, y también un
    /// `Esc` (revisión de seguridad MAJOR-1).
    ///
    /// Vivió en `norte-gui` hasta la revisión de rama de C2, y ahí estaba mal:
    /// `can_approve`, [`hint_id`] y [`status_line`] viven en ESTE crate y no
    /// podían verlo, así que el pie seguía ofreciendo `a aprobar` sobre un
    /// plan que `approve` ya rechazaba — justo la pantalla rota que `hint_id`
    /// existe para no pintar. Lo limpian las TRANSICIONES DE ESTADO
    /// ([`SyncView::on_apply_started`], [`SyncView::on_apply_ended`]), nunca
    /// la generación de la petición: atarlo a la generación lo dejaba echado
    /// para siempre cuando un evento superado se descartaba.
    submitted: bool,
}

impl SyncView {
    /// Un panel recién abierto sobre estas dos raíces, sin pasos todavía.
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
            // Con el `task_id` desde el principio: es lo que hace que un lote
            // de OTRO plan —el lector replanifica con menos marcas— se caiga
            // en vez de mezclarse con éste (Task 12, nota 3).
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

    /// Qué papelera tiene el DESTINO, según el plan.
    ///
    /// [`DestTrash::Unknown`] mientras el plan no ha cerrado, que es la
    /// respuesta honesta: sin `sync.plan_done` no se sabe, y el modelo pinta
    /// cada paso como «esta versión no puede decirlo» en vez de prometer que
    /// vuelve. Nunca se lee [`SyncStep::reversal`] a pelo — esa es la mitad de
    /// la respuesta y la que miente cuando el destino no tiene papelera.
    #[must_use]
    pub fn dest_trash(&self) -> DestTrash {
        self.state
            .plan()
            .map_or(DestTrash::Unknown, SyncPlan::dest_trash)
    }

    /// Las dos reinterpretaciones, juntas y nombradas, para pasárselas a
    /// [`render_step`] de una pieza — que es lo que evita cruzarlas (#152).
    #[must_use]
    pub fn encodings(&self) -> SyncEncodings {
        SyncEncodings {
            source: self.source_encoding,
            dest: self.dest_encoding,
        }
    }

    /// Los pasos que hay AHORA MISMO, esté cerrado el plan o no.
    ///
    /// Mientras el plan llega, [`SyncState::plan`] contesta `None` —no hay
    /// plan hasta el `sync.plan_done`, que es lo que le da su `plan_hash`— y
    /// aun así los pasos ya recibidos existen y se pintan. Sin esto el panel
    /// enseñaba un hueco vacío mientras el pie contaba «planificando… 6
    /// pasos», que es la pantalla diciéndose la contraria a sí misma. La
    /// columna del undo de esos pasos sale «esta versión no puede decirlo»,
    /// que es la verdad hasta que se sepa la papelera del destino.
    #[must_use]
    pub fn steps(&self) -> &[SyncStep] {
        match &self.state {
            SyncState::Planning(p) => p.steps(),
            _ => self.state.plan().map_or(&[], |p| p.steps()),
        }
    }

    /// ¿Sigue habiendo algo que aprobar?
    ///
    /// `false` en cuanto el plan se manda: la línea de teclas no puede seguir
    /// ofreciendo `a aprobar` sobre un plan que ya se gastó —aplicarlo lo
    /// consume, y un segundo `sync.apply` del mismo hash es `PlanStale`—.
    #[must_use]
    pub fn awaiting_approval(&self) -> bool {
        matches!(self.state, SyncState::Ready(_))
    }

    /// ¿Se puede aprobar este panel AHORA MISMO?
    ///
    /// Envuelve [`SyncState::can_approve`] y NUNCA
    /// [`SyncPlan::can_approve`] — el segundo, alcanzable por
    /// [`SyncState::plan`], sigue contestando que sí sobre un plan que ya se
    /// aprobó, porque sus tres factores no cambian al gastarse. Este método
    /// es la forma de que un llamante no tenga ocasión de coger el atajo
    /// equivocado (#161, la trampa que la fase A del CLI no vio: no preguntó
    /// nada, y un plan `Malformed` se aplicó entero desde el spool).
    ///
    /// # Y el desenlace de la Task cuenta
    /// Un run `Cancelled` o `Failed` no se aprueba, aunque el plan HAYA
    /// cerrado. Los dos hechos son compatibles —`sync.plan_done` llega antes
    /// de que el canal se cierre, así que un `Esc` (o una caída del daemon)
    /// en esa ventana deja `Ready` + `Cancelled`—, y sin esta cláusula la
    /// pantalla decía las dos cosas a la vez: el pie pintaba «cancelado — no
    /// hay plan que aprobar» ([`status_line`]) mientras la línea de teclas
    /// seguía ofreciendo aprobar, y la tecla FUNCIONABA (revisión rust
    /// MAJOR-1). Se resuelve del lado conservador: quien pulsó `Esc` pidió
    /// parar, y esta pantalla escribe en el disco de alguien.
    #[must_use]
    pub fn can_approve(&self) -> bool {
        if self.submitted || matches!(self.run, SyncRunState::Cancelled | SyncRunState::Failed) {
            return false;
        }
        self.state.can_approve()
    }

    /// ¿Hay un `sync.apply` en vuelo sin contestar?
    ///
    /// Lo pregunta quien pinta la línea de teclas y quien interpreta un `Esc`:
    /// en esta ventana el daemon YA está escribiendo, así que un `Esc` tiene
    /// que pedir cancelación y no cerrar el panel. Cerrarlo pierde el informe
    /// —y con él el recuento, los fallos y el asa del undo— sobre un destino
    /// que se reescribió a medias (revisión de seguridad MAJOR-1).
    #[must_use]
    pub fn is_submitted(&self) -> bool {
        self.submitted
    }

    /// La petición se resolvió SIN Task: el daemon la rechazó, o llegó una
    /// Task que este panel no adopta.
    ///
    /// Suelta el pestillo, porque si no la `a` queda muerta para siempre y el
    /// pie sigue ofreciéndola. Se llama también en los caminos donde el evento
    /// se descarta por generación superada: atar la suelta a la generación es
    /// justo lo que dejaba el panel encallado cuando el segundo plan se
    /// rechazaba y ningún panel nuevo sustituía al primero (revisión de rama
    /// de C2, MINOR de las dos revisiones).
    pub fn on_apply_abandoned(&mut self) {
        self.submitted = false;
    }

    /// Echa el pestillo y devuelve el hash que se manda, o `None` si este
    /// panel no se puede aprobar.
    ///
    /// Una sola puerta para los dos frontends: quien quiera aplicar pasa por
    /// aquí, y lo que impide el segundo `sync.apply` es esta función, no que
    /// el estado sea `Applying` —no lo es todavía—. La TUI lo espera en línea
    /// y no puede leer una tecla en medio, así que para ella es un no-op; la
    /// GUI sí puede, y es la que lo necesita.
    pub fn submit(&mut self) -> Option<PlanHash> {
        if !self.can_approve() {
            return None;
        }
        let hash = self.state.plan()?.done().plan_hash.clone();
        self.submitted = true;
        Some(hash)
    }

    /// Se lanzó `sync.apply` y el daemon contestó con una Task: junta las
    /// CUATRO actualizaciones que ese instante exige — el modelo avanza a
    /// `Applying` ([`SyncState::on_apply_started`]), el run vuelve a
    /// `Running`, la segunda pregunta se cae (ya se contestó) y la
    /// cancelación pedida por un run anterior deja de aplicar al nuevo.
    ///
    /// Antes de que esto viviera aquí, `norte-tui` hacía las cuatro a mano en
    /// el sitio que lanza la Task; la GUI habría necesitado exactamente las
    /// mismas cuatro, y una reimplementación por su cuenta es justo la
    /// oportunidad de olvidar una — la trampa que este movimiento existe para
    /// no repetir (#161, revisión de C1).
    /// # Y puede NEGARSE
    /// Devuelve `false` sin tocar nada si ya se pidió cancelar. El `Esc` que
    /// pidió parar llegó ANTES que la Task, así que adoptarla aquí resucitaría
    /// un run que el lector dio por cortado y, peor, borraría la petición de
    /// cancelación con el `cancel_requested = false` de abajo — que existe
    /// para que una cancelación vieja no manche el run nuevo, no para
    /// descartar la que acaba de pedirse.
    ///
    /// El guard estaba en el envoltorio de la GUI y no aquí, así que la TUI se
    /// quedaba con el agujero: hoy no lo alcanza porque espera el `sync.apply`
    /// en línea, o sea por casualidad del flujo de control y no por diseño
    /// (revisión de rama de C2, rust MAJOR-2). Quien lo niegue tiene que
    /// cancelar la Task que le devolvieron: nadie más la conoce.
    pub fn on_apply_started(&mut self, task_id: TaskId) -> bool {
        if self.cancel_requested {
            return false;
        }
        self.state.on_apply_started(task_id);
        self.run = SyncRunState::Running;
        self.confirming = None;
        self.cancel_requested = false;
        // El daemon contestó: la ventana que el pestillo cubre se acabó, y a
        // partir de aquí quien impide el segundo `sync.apply` es el estado
        // `Applying`.
        self.submitted = false;
        true
    }

    /// Terminó la Task de `sync.apply`, con lo que `sync.report` contestó:
    /// mete el informe y fija el desenlace. Devuelve la categoría del error que
    /// hay que decir, SIN sanear — cada frontend la mete donde y como pinta.
    ///
    /// Compartida (#161) porque las tres reglas de aquí son de las que un
    /// frontend arregla y el otro se queda:
    ///
    /// 1. **El error de la TASK manda sobre el del informe**: es el que dice
    ///    por qué se paró.
    /// 2. **Sin informe no se dice que terminó bien.** `sync.report` es lo
    ///    ÚNICO que dice cuánto se llegó a escribir; si no se pudo pedir, el
    ///    desenlace es `Failed` con la categoría de ESE error aunque la Task
    ///    dijera `Completed`. `norte-tui` se quedaba aquí en `Applying` con una
    ///    barra transitoria, y el pie decía «aplicando…» para siempre.
    /// 3. **Un estado NO terminal también es fallo.** Solo se llega a él con
    ///    los emisores del progreso caídos: la conexión murió sin decir qué
    ///    pasó, y una sincronización a medias no es un éxito.
    ///
    /// Con UNA excepción a las dos últimas: una Task **cancelada** se dice
    /// cancelada aunque el informe falte. El lector pidió parar y eso ya lo
    /// sabe; convertirlo en «falló» le quita el único dato firme que tiene, y
    /// que el informe no llegara lo cuenta la categoría que esto devuelve.
    ///
    /// La segunda pregunta se cae con la petición que la motivó: dejarla puesta
    /// bajo un pie que ya dice «falló» es cómo un `y` posterior contesta a otra
    /// cosa.
    ///
    /// El informe se mete TAMBIÉN cuando la Task se canceló: lo aplicado hasta
    /// el corte se queda journalizado, y media sincronización es un estado real
    /// que el lector tiene que poder ver.
    pub fn on_apply_ended(
        &mut self,
        state: &TaskState,
        report: Result<SyncReportResult, norte_proto::Error>,
    ) -> Option<String> {
        let categoria = match (state, &report) {
            (TaskState::Failed { error }, _) => Some(crate::error::error_category(error)),
            (_, Err(e)) => Some(crate::error::error_category(e)),
            _ => None,
        };
        if let Ok(informe) = report {
            self.state.on_report(informe);
        }
        self.run = if matches!(state, TaskState::Cancelled) {
            // Una cancelación se dice CANCELADA aunque el informe no llegue:
            // el lector pidió parar y eso ya lo sabe, así que llamarlo «falló»
            // le quita el único dato firme que tiene. Que no se pueda decir
            // cuánto se escribió lo dice el banner, con la categoría que esto
            // devuelve.
            SyncRunState::Cancelled
        } else if categoria.is_some() || !state.is_terminal() {
            SyncRunState::Failed
        } else {
            SyncRunState::from_task_state(state)
        };
        self.confirming = None;
        // Terminó: el pestillo se suelta pase lo que pase, incluso si esto
        // llega sin que `on_apply_started` haya pasado nunca (una Task que
        // falla antes de adoptarse). Si no, el panel se queda sin poder
        // aprobar y con el pie ofreciéndolo.
        self.submitted = false;
        categoria
    }
}

/// Qué línea de TECLAS toca ahora mismo, como id de Fluent.
///
/// Tres, y la diferencia entre las dos últimas es la única tecla de esta
/// pantalla que escribe en el disco de alguien:
///
/// * `sync-hint-confirm` con la segunda pregunta puesta — el teclado se ha
///   reducido a `y` y «cualquier otra», y decir «↑↓ mover» ahí es ofrecer algo
///   que ya no funciona;
/// * `sync-hint`, que NOMBRA la tecla de aprobar, solo cuando aprobar hace
///   algo;
/// * `sync-hint-done` en todo lo demás.
///
/// # Por qué es compartida
/// El segundo brazo pregunta por [`SyncView::can_approve`] y no solo por
/// [`SyncView::awaiting_approval`], y ésa es la corrección: un plan que cerró
/// pero que el daemon marcó no ejecutable —o cuya Task se canceló— está en
/// `Ready` y NO se puede aprobar, y la línea de teclas seguía ofreciendo `a
/// aprobar` encima de un pie que ya decía «este plan no se puede aprobar»
/// ([`status_line`]). Es el mismo desacuerdo que la revisión rust MAJOR-1
/// arregló entre el pie y la tecla, una capa más arriba; vive aquí para que
/// haya UNA respuesta para los dos frontends y no una arreglada y otra no
/// —que es exactamente lo que C1 shipeó (#161)—.
///
/// Aplicar GASTA el plan, así que en `Applying`/`Applied` la `a` desaparece:
/// un segundo `sync.apply` del mismo hash contesta `PlanStale`.
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
/// // Todavía planificando: no hay nada que aprobar, así que no se ofrece.
/// assert_eq!(hint_id(&v), "sync-hint-done");
/// ```
#[must_use]
pub fn hint_id(view: &SyncView) -> &'static str {
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
/// // Recién abierto: planificando, con cero pasos.
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
        // **El informe manda, y va PRIMERO** — pero SIN perder cómo acabó.
        //
        // Una aplicación cortada a medias TIENE informe (lo aplicado hasta el
        // corte se queda, journalizado) y es justo el estado en el que el
        // lector más necesita saber cuánto llegó a escribirse. Con este brazo
        // detrás del de `Cancelled`, la pantalla decía «cancelado — habían
        // llegado N pasos, y no hay plan que aprobar» —una frase sobre el PLAN,
        // que ya se aprobó— encima de la lista de fallos de la APLICACIÓN.
        //
        // Y el desenlace elige la FRASE en vez de perderse: «cancelado tras
        // aplicar N» y «falló tras aplicar N» dicen las dos mitades. Poner el
        // brazo de `Failed` delante escondía las cuentas de un `Mirror` que
        // borró cuarenta árboles y luego murió, que es el sitio donde menos se
        // pueden esconder; ponerlo detrás sin frases propias borraba la palabra
        // «cancelado», y el color habría sido la única señal — en la rama cuyo
        // commit anterior se titula «legible sin color» (#161, fase C2 tarea 4;
        // revisiones rust MAJOR-2 y de seguridad MAJOR-4).
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
        // `view.can_approve()` y no `p.can_approve()`: UNA sola función
        // contesta esa pregunta, y es la misma que la línea de teclas
        // consulta. Con la del plan a secas, este brazo y aquella podían
        // discrepar en cuanto el desenlace de la Task entraba en juego.
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

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{CompareConfidence, CompareCriterion};

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
