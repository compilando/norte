//! The plan, in cells.
//!
//! Takes a step or a failure and returns the columns already sanitized and
//! bounded —with their glyphs and their name reinterpretation— so each
//! surface only has to place them. What it does NOT do is decide: that was
//! already decided.

use norte_proto::methods::{DestTrash, SyncReason, SyncStep};
use unicode_normalization::UnicodeNormalization;

use super::{
    RelAnchor, RelDisplay, StepUndo, anchor_for, anchor_of, rel_display, step_glyph, step_undo,
    undo_glyph,
};

/// The name reinterpretations (#57) of a synchronization's two sides.
///
/// A struct with two NAMED fields and not a `(Option<_>, Option<_>)` tuple:
/// the two values are of the same type, so transposing them compiles — and
/// transposing them IS #152, a `dest_rel` decoded with the SOURCE's
/// codepage, i.e. naming different bytes than the file the write lands on.
/// Here the compiler does not help; the name does.
///
/// The default —neither of the two— is the right one for a frontend with no
/// per-location overrides, such as the CLI: names are read as they come.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncEncodings {
    /// The SOURCE side's.
    pub source: Option<norte_encoding::NameEncoding>,
    /// The DESTINATION side's, which can be a different one: the two panes
    /// are two locations and can carry different overrides.
    pub dest: Option<norte_encoding::NameEncoding>,
}

impl SyncEncodings {
    /// Which of the two a path anchored at `anchor` is read with.
    ///
    /// This is the half of #152 that was not written down anywhere: the
    /// destination's spelling was read with the destination's (every
    /// frontend already did that by hand), but a
    /// [`norte_proto::methods::SyncStepKind::DeleteTree`]'s `rel` —which
    /// hangs from the DESTINATION, see [`crate::sync::anchor_of`]— was read
    /// with the SOURCE's. With two panes carrying different overrides, that
    /// names the subtree about to be deleted with the codepage of the tree
    /// that is NOT being touched, on the very screen where deleting it gets
    /// approved.
    ///
    /// [`RelAnchor::Either`] is read with the source's, since that is where a
    /// `rel` hangs "almost always" (normative on the wire): it is not known,
    /// and choosing the other one would not be any truer — what a pane must
    /// not do with an `Either` is assert the COLUMN, and that is
    /// [`StepCells::anchor`]'s job.
    ///
    /// Two things make that branch less dangerous than it looks, and both
    /// get lost if they are not written down:
    ///
    /// * the choice only CHANGES anything for bytes that are not valid UTF-8
    ///   (`display_name_with` does not reinterpret valid UTF-8), and in that
    ///   case the result is ALWAYS `hostile = true` — meaning an `Either`
    ///   read with the other side's codepage arrives marked "this text is
    ///   not the bytes" on both surfaces;
    /// * **for a STEP**, the only destructive path to `Either` is a
    ///   [`norte_proto::methods::SyncStepKind::Unknown`]
    ///   ([`crate::sync::anchor_of`]), and a single step like that leaves
    ///   the plan in [`crate::sync::PlanIntegrity::Unnameable`], which
    ///   cannot be approved. What is left under `Either` is a `Skip`, which
    ///   writes nothing.
    ///
    /// **That second point does NOT hold for a report FAILURE**
    /// ([`render_failure`]), and saying so matters: a `DeleteTree` that
    /// fails on permissions against a read-only destination is the most
    /// common row of a `Mirror`, its `rel` hangs from the DESTINATION, and
    /// the report does not carry the class that would say so. There, this
    /// branch CAN name a subtree of the destination with the codepage of the
    /// tree that is not being touched. What bounds it is that the result
    /// arrives `hostile = true` and that the anchor gets PAINTED.
    ///
    /// ```
    /// use norte_frontend::sync::{RelAnchor, SyncEncodings};
    /// use norte_encoding::NameEncoding;
    /// let enc = SyncEncodings {
    ///     source: Some(NameEncoding::Cp437),
    ///     dest: None,
    /// };
    /// // A `DeleteTree` speaks about the DESTINATION, even though it paints
    /// // in the first column.
    /// assert_eq!(enc.for_anchor(RelAnchor::Dest), None);
    /// assert_eq!(enc.for_anchor(RelAnchor::Source), Some(NameEncoding::Cp437));
    /// // And what is not on record is read as the source, which is where a
    /// // `rel` hangs "almost always".
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
    /// repeating itself (#192). See [`crate::sync::dest_twin_label`].
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
    // Always with the DESTINATION's, whatever the anchor is: `dest_rel`
    // exists precisely to show that side's spelling, which is the one the
    // write lands on.
    //
    // And it is folded HERE when the BYTES match, not in every painter and
    // not by the painted text: `RelDisplay::text` is lossy, so
    // `caf\xe9.txt` and `caf\x82.txt` —two different files— are the same
    // `caf\u{FFFD}.txt`, and a painter that compares texts hides exactly the
    // field that exists to say which name the write lands on (#152). The
    // wire already compares by bytes (`SyncStep::shape_is_consistent`); this
    // is the same rule, once, for the three frontends.
    let dest_rel = step
        .dest_rel
        .as_ref()
        .filter(|d| **d != step.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192: the BYTES already tell the two paths apart (otherwise `dest_rel`
    // would be `None`), but they can still RENDER the same — an NFC/NFD pair
    // is valid UTF-8 on both halves, so neither one arrives `hostile`, and
    // not even `text == text` catches it: precomposed "é" and "e" + a
    // combining accent are DIFFERENT Strings that a font composes to the
    // same glyph. By NFC and not by bytes NOR by plain String equality —the
    // comparison is the only part of this that normalizes; `RelDisplay::text`
    // itself is still the same byte-exact masked form as always.
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

/// One row of the report (`sync.report`), already resolved: both paths read
/// with the reinterpretation that fits each one, and the destination's
/// FOLDED when the bytes match.
///
/// Twin of [`StepCells`], and kept separate from it because a
/// [`norte_proto::methods::SyncFailure`] is not a step: it carries no class,
/// so there are no glyphs to paint and no undo to judge. What it does share
/// is what can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureCells {
    /// The failure's path, masked and badged.
    pub rel: RelDisplay,
    /// The DESTINATION's spelling, if the report sends it and it DIFFERS in
    /// bytes.
    pub dest_rel: Option<RelDisplay>,
    /// Twin of [`StepCells::dest_rel_twin`], and for the same reason (#192):
    /// `dest_rel` is `Some` but renders THE SAME as `rel` — an NFC/NFD pair,
    /// for instance, is valid UTF-8 on both halves and neither arrives
    /// `hostile`.
    pub dest_rel_twin: bool,
    /// Which root [`FailureCells::rel`] hangs from:
    /// [`RelAnchor::Source`] when the report sends `dest_rel`, and
    /// [`RelAnchor::Either`] when it does not — see [`render_failure`]. **A
    /// painter has to paint it**: on a panel where an unqualified path means
    /// "from the source", staying silent about an `Either` is asserting the
    /// source.
    pub anchor: RelAnchor,
}

/// Resolves ONE row of the report, with the same two rules as
/// [`render_step`] and for the same two reasons.
///
/// * **The folding is by BYTES**, not by the painted text: `RelDisplay::text`
///   is lossy, so two different files each carrying one invalid byte paint
///   the same — and comparing texts makes the field that says which name the
///   write landed on disappear exactly when the names are adversarial.
/// * **The destination's spelling is read with the destination's**, whatever
///   the anchor is (#152): it exists precisely to name the file over there.
///
/// # A failure's anchor is almost never on record, and then it is `Either`
/// [`SyncStep::rel`] is "almost always" the source's and
/// [`crate::sync::anchor_of`] uses the step's CLASS to know when it is not
/// —a `DeleteTree` speaks about the destination—. A
/// [`norte_proto::methods::SyncFailure`] carries no class: the report is
/// read with no plan in front of it. ONE piece of evidence is left, and it
/// is the same one [`crate::sync::anchor_of`] uses: if the report sends
/// `dest_rel`, then `rel` is the SOURCE half of the pair (same rule, same
/// field, see [`norte_proto::methods::SyncFailure::dest_rel`]). With no
/// `dest_rel` it is not known, and saying "source" would be exactly what
/// [`RelAnchor::Either`] exists to avoid doing — **a `DeleteTree` that fails
/// on permissions is the MOST common hostile row of a `Mirror`**, and its
/// `rel` hangs from the destination.
///
/// What a painter must NOT do with an `Either` is stay silent: on a panel
/// where an unqualified path means "from the source" (which is how
/// [`StepCells::anchor`] writes it), silence is the assertion. The anchor
/// travels in [`FailureCells::anchor`] so it gets painted, and this phase's
/// encoding audit (MAJOR-2) is exactly that.
///
/// An `Either`'s DECODING is still the source's
/// ([`SyncEncodings::for_anchor`]) because there is nothing better to
/// choose; with two different #57 overrides that can name a subtree of the
/// destination with the codepage of the tree that was not touched, and it
/// arrives marked hostile but not as "from the other side".
///
/// **Since 0.42.0 there is something to close this with, and this function
/// does not use it yet** (#195 put it on the wire, #208 consumes it):
/// [`norte_proto::methods::SyncFailure::kind`] carries the class the core had
/// in hand and was throwing away, so a `DeleteTree` that failed can already
/// be anchored at the DESTINATION with the same rule
/// [`crate::sync::anchor_of`] applies to a step, instead of falling into
/// `Either`. Changing what this module returns changes what two frontends
/// paint, so it does not travel in the wire's bump.
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
/// assert!(cells.dest_rel.is_none(), "the same spelling is not repeated");
/// // With `dest_rel` on the wire, `rel` is the SOURCE half of the pair —
/// // even though the two spellings match and there is nothing extra to
/// // paint.
/// assert_eq!(cells.anchor, RelAnchor::Source);
///
/// // With no `dest_rel` there is no evidence, and that is NOT "from the
/// // source": a failed `DeleteTree`'s `rel` hangs from the destination.
/// let alone = SyncFailure {
///     rel: RelPath::parse_wire("old").expect("rel"),
///     dest_rel: None,
///     cause: SyncFailureCause::Io,
///     kind: SyncStepKind::DeleteTree,
/// };
/// // …and since 0.42.0 the wire says so (`kind`), so this function READS it
/// // (#208): a `DeleteTree` speaks about the destination, with or without
/// // `dest_rel`.
/// assert_eq!(
///     render_failure(&alone, SyncEncodings::default()).anchor,
///     RelAnchor::Dest
/// );
/// ```
#[must_use]
pub fn render_failure(
    failure: &norte_proto::methods::SyncFailure,
    enc: SyncEncodings,
) -> FailureCells {
    // #208: the SAME rule as a step (`anchor_of`), now that 0.42.0 puts the
    // class on the wire. The most common hostile row of a mirror —a delete
    // rejected on permissions, with no `dest_rel`, its `rel` measured
    // against the destination— stops being `Either`, which is what used to
    // make `SyncEncodings::for_anchor` decode it with the reinterpretation of
    // the TREE THAT WAS NOT TOUCHED (an encoding-auditor finding).
    let anchor = anchor_for(failure.kind, failure.dest_rel.is_some(), None);
    let rel = rel_display(&failure.rel, enc.for_anchor(anchor));
    let dest_rel = failure
        .dest_rel
        .as_ref()
        .filter(|d| **d != failure.rel)
        .map(|r| rel_display(r, enc.dest));
    // #192, the same rule as `render_step`: by NFC, not by plain `String`
    // equality — precomposed "é" and "e" + a combining accent are different
    // Strings that render to the same glyph.
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
