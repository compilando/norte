//! The `plan_hash`: the fingerprint of what a human approves when they
//! approve a plan.
//!
//! A [`PlanHasher`] is seeded with the plan's INTENT — both roots, the mode,
//! `on_unknown` and the comparison options — and then swallows the flow's
//! elements ONE by ONE, in order. It keeps none of them: its state is a
//! `Sha256` and a counter, so planning half a million steps costs the same in
//! memory as planning three (hard rule 3: a Task cannot afford to assemble
//! the whole plan just to hash it).
//!
//! # What goes in and what does not
//! The CONCLUSIONS go in: what is going to be done, over which source path,
//! over which destination path, with what size, with what criterion and
//! confidence, with what reversal and what reason — and every blocker. The
//! step's `id` does NOT go in, since it is presentation: a filter that
//! renumbers the panel must not be able to invalidate an approval.
//!
//! # How it is fed (and why concatenating is not enough)
//! Every field goes with its LENGTH in front and every optional with a
//! presence byte, just like the journal's chain (ADR 0023) and the rename
//! batch's `plan_hash`. Without the prefix, `"ab" + "c"` and `"a" + "bc"`
//! produce the same digest and two plans that write to DIFFERENT paths
//! compare equal; without the presence byte, "there is no `dest_rel`" and
//! "there is a `dest_rel` that is the root" are not distinguished either —
//! and a `Skip` can legitimately carry a root `rel`, which is exactly the
//! zero-byte sequence.
//!
//! Every element also carries a class TAG, so a `Skip` and a blocker at the
//! same `rel` do not collide.
//!
//! Enums feed their serde NAME, never their discriminant:
//! `TypeMismatchDir` was inserted BEFORE `Unknown` when code already existed,
//! and the next variant will also be inserted in the middle. A digest over
//! the discriminant would have turned that insertion into an approval that
//! authorizes another plan.
//!
//! # It feeds from the flow the human SEES, not another one
//! Normative: the hasher is given EXACTLY the elements that end up in the
//! plan that is shown and summarized in
//! [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) — i.e., after
//! applying the request's `include`, which filters the transducer's OUTPUT
//! (see [`plan`](crate::plan())). Hashing the unfiltered flow and showing the
//! filtered one mints a token for a plan nobody approved, and it is the only
//! way to wire it wrong that neither the type nor the tests can catch. That
//! is why `include` is not seeded: the elements that remain ALREADY carry its
//! effect, and seeding it too would invite the belief that it does not matter
//! which flow feeds it. The same holds for
//! [`SyncCounts`](norte_proto::methods::SyncCounts).
//!
//! # What this hash CANNOT do alone
//! A plan whose destination is read-only produces EXACTLY one element — its
//! blocker — whatever the tree is, so all of them hash alike. That is correct
//! (those plans have no conclusions to distinguish) and has a consequence
//! whoever executes must know: **`sync.apply` decides by
//! [`SyncPlanDone::executable`](norte_proto::methods::SyncPlanDone::executable),
//! not by a hash matching**. A matching hash says "this is the plan you were
//! shown", never "this plan can be executed".
//!
//! There is a second legitimate equality, and for the same reason: a step's
//! `rel` is measured against the root of the side it SPEAKS about, and the
//! step carries no field saying which one (see [`SyncStep::rel`]). A `Skip`
//! for an unreadable listing on the SOURCE and another for one on the
//! DESTINATION, under the same name, come out byte-for-byte equal and hash
//! alike. The two affected shapes — that `Skip` and an overlap blocker
//! reached from one side or the other — write NOTHING, so no pair of plans
//! that write differently can share a fingerprint; what is lost is a reading
//! distinction, not an effect one.
//!
//! # It is not a persisted format
//! The hash identifies a plan RETAINED in the spool, with
//! [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS)'s TTL, and the
//! same binary produces and consumes it within that window. There are no old
//! journals that get invalidated if this framing changes, unlike ADR 0023 —
//! but changing it does invalidate in-flight plans, so it is changed with a
//! deployment and not lightly.

use std::borrow::Cow;
use std::fmt;

use norte_proto::VPath;
use norte_proto::methods::{
    CompareConfidence, CompareCriteria, CompareCriterion, DescendSide, OnUnknown, PlanHash,
    RelPath, Side, StepReversal, SyncBlocker, SyncBlockerKind, SyncCompareOptions, SyncMode,
    SyncReason, SyncStep, SyncStepKind,
};
use sha2::{Digest, Sha256};

use crate::{PlanItem, SyncOptions};

/// Tag of a step within the digest.
const TAG_STEP: u8 = b'S';
/// Tag of a blocker. Different from [`TAG_STEP`] so a [`SyncStepKind::Skip`]
/// and a blocker over the SAME `rel` cannot produce the same byte sequence.
const TAG_BLOCKER: u8 = b'B';
/// Tag of the closing, in front of the element count.
///
/// Without it the end would be told apart from one more element only because
/// the counter's length prefix's first byte (`0x08`) does not match either of
/// the other two tags — true today and by accident.
const TAG_END: u8 = b'E';
// The three tags have to be distinct or the flow stops being decodable in a
// single way, which is the only thing that prevents a collision.
const _: () = assert!(TAG_END != TAG_STEP && TAG_END != TAG_BLOCKER);
const _: () = assert!(TAG_STEP != TAG_BLOCKER);

/// The `plan_hash` accumulator, in STREAMING fashion.
///
/// Built with the plan's intent, fed each element in the order the flow
/// produced it, and closed with [`PlanHasher::finish`].
///
/// ```
/// use norte_proto::VPath;
/// use norte_proto::methods::SyncCompareOptions;
/// use norte_sync::{OnUnknown, PlanHasher, Side, SyncMode, SyncOptions};
///
/// let opts = SyncOptions {
///     source_root: VPath::parse("file:///origen").expect("path"),
///     dest_root: VPath::parse("file:///destino").expect("path"),
///     mode: SyncMode::Update,
///     on_unknown: OnUnknown::Copy,
///     source_side: Side::Left,
///     dest_has_trash: true,
///     dest_trash_restorable: true,
///     dest_writable: true,
/// };
/// let compare = SyncCompareOptions::default();
/// // An EMPTY plan has a hash: it is the "nothing to do" plan.
/// let empty = PlanHasher::new(&opts, &compare).finish();
/// assert_eq!(empty.as_str().len(), norte_proto::methods::PLAN_HASH_LEN);
///
/// // And the same intent with ANOTHER mode does not share it.
/// let other = SyncOptions { mode: SyncMode::Mirror, ..opts };
/// assert_ne!(PlanHasher::new(&other, &compare).finish(), empty);
/// ```
#[derive(Debug, Clone)]
pub struct PlanHasher {
    /// The digest, already seeded with the intent.
    digest: Sha256,
    /// How many elements went in. Fed on CLOSING: a streaming hasher does not
    /// know the total when it starts, which is exactly what makes it O(1) in
    /// memory.
    items: u64,
}

impl PlanHasher {
    /// Seeds the digest with the plan's intent: what decides WHICH plan was
    /// requested, before a single element arrives.
    ///
    /// Everything goes in, including what is also reflected in the steps (the
    /// destination's trash shows in every reversal, the source side in every
    /// `rel`): over-seeding cannot create a collision, and under-seeding
    /// leaves two different requests sharing a fingerprint when the tree
    /// stays silent — two empty trees produce zero steps under ANY option.
    ///
    /// The comparison options go SEPARATELY because they do not live in
    /// [`SyncOptions`]: the transducer does not need them (it does not
    /// compare, it transduces), and whoever plans — `sync.plan` — has both in
    /// front of them. A plan made with `hash` on is not the same as one made
    /// looking only at size, even if the steps come out identical: something
    /// else was approved.
    #[must_use]
    pub fn new(opts: &SyncOptions, compare: &SyncCompareOptions) -> Self {
        // DESTRUCTURED on purpose: a new field on any of the three types
        // breaks compilation here instead of silently staying out of the
        // digest, which is the kind of omission nobody sees until two
        // different plans share a hash.
        let SyncOptions {
            source_root,
            dest_root,
            mode,
            on_unknown,
            source_side,
            dest_has_trash,
            dest_trash_restorable,
            dest_writable,
        } = opts;
        let SyncCompareOptions {
            criteria,
            max_depth,
            mtime_tolerance_ms,
            follow_symlinks,
            descend_orphans,
        } = compare;
        let CompareCriteria { size, mtime, hash } = criteria;

        let mut digest = Sha256::new();
        // Domain separation: this digest is neither the rename batch's nor
        // the journal chain's, and must not be confusable with either.
        feed(&mut digest, b"norte-sync-plan-v1");
        // The roots by PIECES — scheme, authority, segments in bytes — and
        // not by their wire form: percent-decoding and re-encoding is
        // lossless, but making the digest depend on the codec staying
        // canonical is a dependency this crate does not need to take on
        // (hard rule 1: bytes are compared).
        feed_root(&mut digest, source_root);
        feed_root(&mut digest, dest_root);
        feed_name(&mut digest, &mode_name(*mode));
        feed_name(&mut digest, &on_unknown_name(*on_unknown));
        feed_name(&mut digest, &side_name(*source_side));
        feed_flag(&mut digest, *dest_has_trash);
        feed_flag(&mut digest, *dest_trash_restorable);
        feed_flag(&mut digest, *dest_writable);
        feed_flag(&mut digest, *size);
        feed_flag(&mut digest, *mtime);
        feed_flag(&mut digest, *hash);
        feed_opt_u64(&mut digest, max_depth.map(u64::from));
        feed_u64(&mut digest, u64::from(*mtime_tolerance_ms));
        feed_flag(&mut digest, *follow_symlinks);
        feed_opt_name(&mut digest, descend_orphans.map(descend_side_name).as_ref());
        Self { digest, items: 0 }
    }

    /// Swallows one element of the flow, whatever it is. This is what
    /// whoever accumulates the plan consumes: the flow produces [`PlanItem`],
    /// not two sequences.
    pub fn item(&mut self, item: &PlanItem) {
        match item {
            // The destination witness stays OUT of the digest, on purpose: it
            // is where the conclusion came FROM, not the conclusion. Two
            // plans with the same steps over a destination whose date moved
            // without any verdict changing are the same plan and deserve the
            // same approval; what revalidates the witness is the executor,
            // step by step, not a whole-plan fingerprint.
            PlanItem::Step { step, dest: _ } => self.step(step),
            PlanItem::Blocker(blocker) => self.blocker(blocker),
        }
    }

    /// Swallows ONE step.
    ///
    /// `id` does NOT go in: it is presentation, not conclusion. The panel
    /// anchors to it and a filter can renumber it, and an approval that got
    /// invalidated by sorting a list would not be protecting anything.
    pub fn step(&mut self, step: &SyncStep) {
        // Destructured for the same reason as in `new`: a new field on
        // `SyncStep` has to break compilation, not fall out of the digest.
        let SyncStep {
            id: _,
            kind,
            rel,
            dest_rel,
            size,
            criterion,
            confidence,
            reversal,
            reason,
        } = step;
        self.digest.update([TAG_STEP]);
        feed_name(&mut self.digest, &step_kind_name(*kind));
        feed_rel(&mut self.digest, rel);
        // `dest_rel` is the only thing that distinguishes "I overwrite the
        // file that is there" from "I create a second one alongside" (issue
        // #152), and the hash is the token writing is authorized with: it
        // has to go in, with its presence byte, because a root `rel` is also
        // zero bytes.
        feed_opt_rel(&mut self.digest, dest_rel.as_ref());
        feed_opt_u64(&mut self.digest, *size);
        feed_name(&mut self.digest, &criterion_name(*criterion));
        feed_name(&mut self.digest, &confidence_name(*confidence));
        feed_opt_name(&mut self.digest, reversal.map(reversal_name).as_ref());
        feed_opt_name(&mut self.digest, reason.map(reason_name).as_ref());
        self.items = self.items.saturating_add(1);
    }

    /// Swallows ONE blocker.
    ///
    /// EVERY blocker goes in, and that is what distinguishes this accumulator
    /// from the list that travels in
    /// [`SyncPlanDone::blockers`](norte_proto::methods::SyncPlanDone::blockers):
    /// that one is trimmed to
    /// [`SYNC_MAX_BLOCKERS_REPORTED`](norte_proto::methods::SYNC_MAX_BLOCKERS_REPORTED)
    /// and the number of blockers is NOT bounded —
    /// [`SyncBlockerKind::TypeMismatchDir`]'s grows with the tree. Hashing the
    /// trimmed list would make two plans that differ only from blocker 257
    /// onward share a fingerprint.
    ///
    /// Approving a blocked plan and approving a clean one are different acts
    /// even when the steps match, so a blocker changes the hash.
    pub fn blocker(&mut self, blocker: &SyncBlocker) {
        let SyncBlocker { rel, kind, side } = blocker;
        self.digest.update([TAG_BLOCKER]);
        feed_name(&mut self.digest, &blocker_kind_name(*kind));
        feed_rel(&mut self.digest, rel);
        feed_opt_name(&mut self.digest, side.map(side_name).as_ref());
        self.items = self.items.saturating_add(1);
    }

    /// Closes the plan and returns its [`PlanHash`]: sha256 in LOWERCASE hex.
    ///
    /// The element count is fed here, at the end, preceded by its closing
    /// tag, because a streaming hasher does not know it beforehand (the
    /// rename batch's puts it in front because it receives a `slice`). It is
    /// not needed for the digest to be injective — every element is tagged
    /// and length-prefixed — but it also ties down the plan's SIZE, which is
    /// the first thing whoever approves reads.
    ///
    /// **Called when the flow has ended in `None`, never over a cut one.** A
    /// plan cancelled midway produces a digest indistinguishable from that of
    /// a shorter plan that did finish, and that digest must not exist:
    /// whoever plans emits
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone) only when the flow
    /// ran out without error, and without that notification there is no hash
    /// anyone can approve.
    #[must_use]
    pub fn finish(self) -> PlanHash {
        let mut digest = self.digest;
        digest.update([TAG_END]);
        feed_u64(&mut digest, self.items);
        let bytes: [u8; 32] = digest.finalize().into();
        // The hex is produced by the TYPE (`PlanHash::from_digest`) and not
        // an encoder of this crate: a second copy is a second chance to
        // write uppercase, which is the detail that makes two writes of the
        // same hash compare unequal. It also removes the `expect` (hard rule
        // 6).
        PlanHash::from_digest(&bytes)
    }
}

/// Feeds a field with its LENGTH in front: `"ab" + "c"` and `"a" + "bc"`
/// cannot produce the same digest.
///
/// # #174: no longer a copy — it is [`norte_proto::hashing::feed`]
/// This crate had its own, byte-for-byte identical to `norte_core::hashing`'s,
/// because sharing upward was not possible (`norte-core` → `norte-sync`, not
/// the other way around) and the natural shared place would have relicensed
/// AGPL code. ADR 0051 chose a home: `norte-proto`, which this crate already
/// uses and which sees everything that speaks the protocol. `norte-core`'s
/// copy stays where it is — it is the journal's tamper-evident chain
/// (ADR 0023) and the audit anchor (ADR 0025), and its framing cannot change
/// a single byte without invalidating every `journal.db` already written —
/// but it can no longer drift silently: a test of its own compares it against
/// this one.
use norte_proto::hashing::feed;

/// A piece of text, by its bytes.
fn feed_str(digest: &mut Sha256, text: &str) {
    feed(digest, text.as_bytes());
}

/// An enum token's serde name.
fn feed_name(digest: &mut Sha256, name: &str) {
    feed_str(digest, name);
}

/// A boolean, as a byte with its length.
fn feed_flag(digest: &mut Sha256, flag: bool) {
    feed(digest, &[u8::from(flag)]);
}

/// An integer, little-endian.
fn feed_u64(digest: &mut Sha256, value: u64) {
    feed(digest, &value.to_le_bytes());
}

/// An OPTIONAL integer, with a presence byte.
fn feed_opt_u64(digest: &mut Sha256, value: Option<u64>) {
    norte_proto::hashing::feed_opt(digest, value.map(u64::to_le_bytes).as_ref().map(|b| &b[..]));
}

/// An OPTIONAL token name, with a presence byte.
fn feed_opt_name(digest: &mut Sha256, name: Option<&Cow<'_, str>>) {
    norte_proto::hashing::feed_opt(digest, name.map(|n| n.as_bytes()));
}

/// A root: scheme, authority (with its presence byte — `file://` has none and
/// `file://x/` does) and then its segments, same as a relative path.
///
/// The authority goes BYTE FOR BYTE, without folding: for `mem://` and for an
/// object storage connection id it is an opaque token, and folding it would
/// merge two different connections. It is the same comparison `rel_under`
/// makes, and it has to be: two roots the transducer considers different
/// cannot hash alike.
fn feed_root(digest: &mut Sha256, root: &VPath) {
    feed_str(digest, root.scheme());
    norte_proto::hashing::feed_opt(digest, root.authority().map(str::as_bytes));
    let segments: Vec<&[u8]> = root.segments().collect();
    feed_u64(digest, segments.len() as u64);
    for segment in segments {
        feed(digest, segment);
    }
}

/// A relative path: how many segments, then each one by its BYTES (hard rule
/// 1 — never the percent-encoded form, never a folded string).
///
/// The segment count in front and each one's length is what keeps `a/bc` and
/// `ab/c` from colliding.
fn feed_rel(digest: &mut Sha256, rel: &RelPath) {
    feed_u64(digest, rel.segments().len() as u64);
    for segment in rel.segments() {
        feed(digest, segment.as_bytes());
    }
}

/// An OPTIONAL relative path, with a presence byte: the root is zero
/// segments, i.e. zero bytes, so without it "absent" and "present and empty"
/// would be the same digest — and a `Skip` can legitimately carry a root
/// `rel`.
fn feed_opt_rel(digest: &mut Sha256, rel: Option<&RelPath>) {
    match rel {
        None => digest.update([0u8]),
        Some(rel) => {
            digest.update([1u8]);
            feed_rel(digest, rel);
        }
    }
}

/// The name of a token this binary does NOT know: its `Debug` name, with a
/// prefix no serde name can have (they are all `snake_case`).
///
/// Only tokens from a future `norte-proto` this crate has not learned reach
/// it, and it is still a NAME: two different new variants do not collide with
/// each other nor with any known one.
///
/// This is the point where the guarantee of destructuring STRUCTS falls
/// short: an enum from another crate is `#[non_exhaustive]`, so the wildcard
/// is mandatory and a new variant passes through here instead of breaking
/// compilation. It is safe — still injective — but whoever adds a variant to
/// `norte-proto` should also add its arm here, so the digest speaks its wire
/// name and not its Rust one.
fn unknown_name<T: fmt::Debug>(token: &T) -> Cow<'static, str> {
    Cow::Owned(format!("?{token:?}"))
}

/// [`SyncStepKind`]'s serde name.
fn step_kind_name(kind: SyncStepKind) -> Cow<'static, str> {
    match kind {
        SyncStepKind::CreateDir => Cow::Borrowed("create_dir"),
        SyncStepKind::Copy => Cow::Borrowed("copy"),
        SyncStepKind::Overwrite => Cow::Borrowed("overwrite"),
        SyncStepKind::DeleteTree => Cow::Borrowed("delete_tree"),
        SyncStepKind::Skip => Cow::Borrowed("skip"),
        SyncStepKind::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// [`StepReversal`]'s serde name.
fn reversal_name(reversal: StepReversal) -> Cow<'static, str> {
    match reversal {
        StepReversal::Delete => Cow::Borrowed("delete"),
        StepReversal::RestoreTrash => Cow::Borrowed("restore_trash"),
        StepReversal::Irreversible => Cow::Borrowed("irreversible"),
        StepReversal::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// [`SyncReason`]'s serde name.
fn reason_name(reason: SyncReason) -> Cow<'static, str> {
    match reason {
        SyncReason::AmbiguousSource => Cow::Borrowed("ambiguous_source"),
        SyncReason::UnknownConfidence => Cow::Borrowed("unknown_confidence"),
        SyncReason::Unreadable => Cow::Borrowed("unreadable"),
        SyncReason::NoTrashOnTarget => Cow::Borrowed("no_trash_on_target"),
        SyncReason::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// [`CompareCriterion`]'s serde name.
fn criterion_name(criterion: CompareCriterion) -> Cow<'static, str> {
    match criterion {
        CompareCriterion::Presence => Cow::Borrowed("presence"),
        CompareCriterion::Kind => Cow::Borrowed("kind"),
        CompareCriterion::LinkTarget => Cow::Borrowed("link_target"),
        CompareCriterion::Size => Cow::Borrowed("size"),
        CompareCriterion::Mtime => Cow::Borrowed("mtime"),
        CompareCriterion::Hash => Cow::Borrowed("hash"),
        CompareCriterion::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// [`CompareConfidence`]'s serde name. Careful: `unknown` is a REAL value of
/// the vocabulary and `unrecognised` is the decode fallback — two different
/// facts, and that is why two different names.
fn confidence_name(confidence: CompareConfidence) -> Cow<'static, str> {
    match confidence {
        CompareConfidence::Certain => Cow::Borrowed("certain"),
        CompareConfidence::Probable => Cow::Borrowed("probable"),
        CompareConfidence::Unknown => Cow::Borrowed("unknown"),
        CompareConfidence::Unrecognised => Cow::Borrowed("unrecognised"),
        other => unknown_name(&other),
    }
}

/// [`SyncBlockerKind`]'s serde name.
fn blocker_kind_name(kind: SyncBlockerKind) -> Cow<'static, str> {
    match kind {
        SyncBlockerKind::AmbiguousDest => Cow::Borrowed("ambiguous_dest"),
        SyncBlockerKind::OverlapDetected => Cow::Borrowed("overlap_detected"),
        SyncBlockerKind::DestReadOnly => Cow::Borrowed("dest_read_only"),
        SyncBlockerKind::DirTooLarge => Cow::Borrowed("dir_too_large"),
        SyncBlockerKind::TypeMismatchDir => Cow::Borrowed("type_mismatch_dir"),
        SyncBlockerKind::Unknown => Cow::Borrowed("unknown"),
        other => unknown_name(&other),
    }
}

/// [`Side`]'s serde name.
fn side_name(side: Side) -> Cow<'static, str> {
    match side {
        Side::Left => Cow::Borrowed("left"),
        Side::Right => Cow::Borrowed("right"),
        Side::Unknown => Cow::Borrowed("unknown"),
    }
}

/// [`SyncMode`]'s serde name.
fn mode_name(mode: SyncMode) -> Cow<'static, str> {
    match mode {
        SyncMode::Update => Cow::Borrowed("update"),
        SyncMode::Mirror => Cow::Borrowed("mirror"),
        other => unknown_name(&other),
    }
}

/// [`OnUnknown`]'s serde name.
fn on_unknown_name(on_unknown: OnUnknown) -> Cow<'static, str> {
    match on_unknown {
        OnUnknown::Copy => Cow::Borrowed("copy"),
        OnUnknown::Skip => Cow::Borrowed("skip"),
        other => unknown_name(&other),
    }
}

/// [`DescendSide`]'s serde name.
fn descend_side_name(side: DescendSide) -> Cow<'static, str> {
    match side {
        DescendSide::Left => Cow::Borrowed("left"),
        DescendSide::Right => Cow::Borrowed("right"),
        other => unknown_name(&other),
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::VPath;
    use norte_proto::methods::{CompareCriteria, PLAN_HASH_LEN};

    use super::*;

    fn vpath(wire: &str) -> VPath {
        VPath::parse(wire).expect("path")
    }

    fn opts_update() -> SyncOptions {
        SyncOptions {
            source_root: vpath("file:///origen"),
            dest_root: vpath("file:///destino"),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_trash_restorable: true,
            dest_writable: true,
        }
    }

    fn opts_mirror() -> SyncOptions {
        SyncOptions {
            mode: SyncMode::Mirror,
            ..opts_update()
        }
    }

    fn compare_opts() -> SyncCompareOptions {
        SyncCompareOptions::default()
    }

    fn hasher(opts: &SyncOptions) -> PlanHasher {
        PlanHasher::new(opts, &compare_opts())
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    /// A `rel` from its segments' BYTES: NFC and NFD are both valid UTF-8, so
    /// the wire form does not tell them apart by eye.
    fn rel_of(segments: &[&[u8]]) -> RelPath {
        RelPath::new(
            segments
                .iter()
                .map(|b| norte_proto::Segment::new(b.to_vec()).expect("segment"))
                .collect(),
        )
    }

    fn copy_step(rel_wire: &str, size: u64) -> SyncStep {
        SyncStep {
            id: 1,
            kind: SyncStepKind::Copy,
            rel: rel(rel_wire),
            dest_rel: None,
            size: Some(size),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Delete),
            reason: None,
        }
    }

    fn step_with_id(id: u64) -> SyncStep {
        SyncStep {
            id,
            ..copy_step("a.txt", 10)
        }
    }

    fn skip_step(rel_wire: &str) -> SyncStep {
        SyncStep {
            kind: SyncStepKind::Skip,
            rel: rel(rel_wire),
            size: None,
            reversal: None,
            reason: Some(SyncReason::Unreadable),
            ..copy_step(rel_wire, 0)
        }
    }

    fn blocker(kind: SyncBlockerKind, rel_wire: &str) -> SyncBlocker {
        SyncBlocker {
            rel: rel(rel_wire),
            kind,
            side: Some(Side::Right),
        }
    }

    #[test]
    fn the_hash_covers_the_conclusions_and_not_the_ids() {
        // `id` is presentation: two plans that do the same thing hash alike
        // even if a filter renumbered the panel.
        let mut a = hasher(&opts_update());
        let mut b = hasher(&opts_update());
        a.step(&step_with_id(1));
        b.step(&step_with_id(99));
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn changing_a_step_changes_the_hash() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a.txt", 10));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a.txt", 11));
        assert_ne!(a.finish(), b.finish(), "size is a conclusion");
    }

    #[test]
    fn changing_the_mode_changes_the_hash_with_identical_steps() {
        let mut a = hasher(&opts_update());
        let mut b = hasher(&opts_mirror());
        a.step(&copy_step("a.txt", 10));
        b.step(&copy_step("a.txt", 10));
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn order_is_part_of_the_plan() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        a.step(&copy_step("b", 2));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("b", 2));
        b.step(&copy_step("a", 1));
        assert_ne!(
            a.finish(),
            b.finish(),
            "CreateDir before Copy is a conclusion too"
        );
    }

    #[test]
    fn a_blocker_is_in_the_hash() {
        // Approving a blocked plan and approving a clean one are different
        // acts even when the steps match.
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a", 1));
        b.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "README"));
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn the_hash_is_lowercase_hex_of_the_documented_length() {
        let h = hasher(&opts_update()).finish();
        assert_eq!(h.as_str().len(), PLAN_HASH_LEN);
        assert!(
            h.as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    /// The class tag: a `Skip` and a blocker at the SAME path are not the
    /// same plan, even though almost everything else they carry is the same.
    #[test]
    fn a_skip_and_a_blocker_at_the_same_rel_do_not_collide() {
        let mut a = hasher(&opts_update());
        a.step(&skip_step("sub/x"));
        let mut b = hasher(&opts_update());
        b.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "sub/x"));
        assert_ne!(a.finish(), b.finish());
    }

    /// Without a length prefix, `a/bc` and `ab/c` are the same bytes glued
    /// together — and they are two DIFFERENT files of someone's tree.
    #[test]
    fn two_paths_that_concatenate_alike_do_not_collide() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a/bc", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("ab/c", 1));
        assert_ne!(a.finish(), b.finish());
    }

    /// The same between two glued FIELDS: a step's `rel` + `dest_rel` cannot
    /// be read as another split of the same bytes.
    #[test]
    fn a_rel_and_a_dest_rel_cannot_be_read_as_one_another() {
        let mut a = hasher(&opts_update());
        let mut step = copy_step("ab", 1);
        step.dest_rel = Some(rel("c"));
        a.step(&step);
        let mut b = hasher(&opts_update());
        let mut other = copy_step("a", 1);
        other.dest_rel = Some(rel("bc"));
        b.step(&other);
        assert_ne!(a.finish(), b.finish());
    }

    /// `dest_rel`'s presence byte: a `Skip` can carry a ROOT `rel`, which
    /// encodes to zero bytes, so "absent" and "present and empty" have to be
    /// kept apart even though a well-formed step does not produce the
    /// ambiguity.
    #[test]
    fn an_absent_dest_rel_and_an_empty_one_are_not_the_same_plan() {
        let mut a = hasher(&opts_update());
        a.step(&skip_step("sub/x"));
        let mut b = hasher(&opts_update());
        let mut step = skip_step("sub/x");
        step.dest_rel = Some(RelPath::default());
        b.step(&step);
        assert_ne!(a.finish(), b.finish());
    }

    /// `dest_rel` decides which file is written to (#152), so two plans that
    /// only differ in it CANNOT be approved with the same token.
    #[test]
    fn the_destination_spelling_is_part_of_the_hash() {
        let mut a = hasher(&opts_update());
        let mut nfc = copy_step("x", 1);
        // `café` in NFC…
        nfc.dest_rel = Some(rel_of(&["caf\u{e9}".as_bytes()]));
        a.step(&nfc);
        let mut b = hasher(&opts_update());
        let mut nfd = copy_step("x", 1);
        // …and in NFD: the same characters, different BYTES, a different
        // file on ext4.
        nfd.dest_rel = Some(rel_of(&["cafe\u{301}".as_bytes()]));
        b.step(&nfd);
        assert_ne!(a.finish(), b.finish());
    }

    /// A name that is not UTF-8 goes in by its bytes and distinguishes.
    #[test]
    fn a_non_utf8_name_is_hashed_by_its_bytes() {
        let mut a = hasher(&opts_update());
        let mut one = copy_step("x", 1);
        one.rel = rel_of(&[b"informe\xff\xfe.dat"]);
        a.step(&one);
        let mut b = hasher(&opts_update());
        let mut other = copy_step("x", 1);
        other.rel = rel_of(&[b"informe\xfe\xff.dat"]);
        b.step(&other);
        assert_ne!(a.finish(), b.finish());
    }

    /// Every field of the step is a conclusion and none is left out.
    #[test]
    fn every_field_of_a_step_moves_the_hash() {
        let base = copy_step("a.txt", 10);
        let baseline = {
            let mut h = hasher(&opts_update());
            h.step(&base);
            h.finish()
        };
        let variants = [
            SyncStep {
                kind: SyncStepKind::Overwrite,
                reversal: Some(StepReversal::RestoreTrash),
                ..base.clone()
            },
            SyncStep {
                rel: rel("b.txt"),
                ..base.clone()
            },
            SyncStep {
                dest_rel: Some(rel("A.TXT")),
                ..base.clone()
            },
            SyncStep {
                size: None,
                ..base.clone()
            },
            SyncStep {
                criterion: CompareCriterion::Mtime,
                ..base.clone()
            },
            SyncStep {
                confidence: CompareConfidence::Unknown,
                ..base.clone()
            },
            SyncStep {
                reversal: Some(StepReversal::Irreversible),
                reason: Some(SyncReason::NoTrashOnTarget),
                ..base.clone()
            },
            SyncStep {
                kind: SyncStepKind::Skip,
                reversal: None,
                reason: Some(SyncReason::AmbiguousSource),
                ..base.clone()
            },
        ];
        for variant in variants {
            let mut h = hasher(&opts_update());
            h.step(&variant);
            assert_ne!(
                h.finish(),
                baseline,
                "did not enter the digest: {variant:?}"
            );
        }
    }

    /// And every field of the blocker.
    #[test]
    fn every_field_of_a_blocker_moves_the_hash() {
        let base = blocker(SyncBlockerKind::AmbiguousDest, "sub/x");
        let baseline = {
            let mut h = hasher(&opts_update());
            h.blocker(&base);
            h.finish()
        };
        let variants = [
            SyncBlocker {
                kind: SyncBlockerKind::DirTooLarge,
                ..base.clone()
            },
            SyncBlocker {
                rel: rel("sub/y"),
                ..base.clone()
            },
            SyncBlocker {
                side: None,
                ..base.clone()
            },
            SyncBlocker {
                side: Some(Side::Left),
                ..base.clone()
            },
        ];
        for variant in variants {
            let mut h = hasher(&opts_update());
            h.blocker(&variant);
            assert_ne!(
                h.finish(),
                baseline,
                "did not enter the digest: {variant:?}"
            );
        }
    }

    /// The INTENT is seeded whole: two different requests do not share a
    /// fingerprint even when the tree produces not a single step.
    #[test]
    fn every_part_of_the_intention_seeds_the_hash() {
        let base = opts_update();
        let baseline = hasher(&base).finish();
        let variants = [
            SyncOptions {
                source_root: vpath("file:///otro"),
                ..base.clone()
            },
            SyncOptions {
                dest_root: vpath("file:///otro"),
                ..base.clone()
            },
            SyncOptions {
                mode: SyncMode::Mirror,
                ..base.clone()
            },
            SyncOptions {
                on_unknown: OnUnknown::Skip,
                ..base.clone()
            },
            SyncOptions {
                source_side: Side::Right,
                ..base.clone()
            },
            SyncOptions {
                dest_has_trash: false,
                ..base.clone()
            },
            SyncOptions {
                dest_trash_restorable: false,
                ..base.clone()
            },
            SyncOptions {
                dest_writable: false,
                ..base.clone()
            },
        ];
        for variant in variants {
            assert_ne!(
                hasher(&variant).finish(),
                baseline,
                "did not seed the digest: {variant:?}"
            );
        }
    }

    /// And the comparison options underneath: a plan made by reading 40 GB of
    /// content is not the same as one made by looking at sizes, even if the
    /// steps come out identical.
    #[test]
    fn the_compare_options_seed_the_hash() {
        let opts = opts_update();
        let baseline = PlanHasher::new(&opts, &SyncCompareOptions::default()).finish();
        let variants = [
            SyncCompareOptions {
                criteria: CompareCriteria {
                    hash: true,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                criteria: CompareCriteria {
                    size: false,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                criteria: CompareCriteria {
                    mtime: false,
                    ..CompareCriteria::default()
                },
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                max_depth: Some(1),
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                mtime_tolerance_ms: 0,
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                follow_symlinks: true,
                ..SyncCompareOptions::default()
            },
            SyncCompareOptions {
                descend_orphans: Some(DescendSide::Left),
                ..SyncCompareOptions::default()
            },
        ];
        for variant in variants {
            assert_ne!(
                PlanHasher::new(&opts, &variant).finish(),
                baseline,
                "did not seed the digest: {variant:?}"
            );
        }
        // And the two sides of `descend_orphans` are not the same plan.
        assert_ne!(
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    descend_orphans: Some(DescendSide::Left),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    descend_orphans: Some(DescendSide::Right),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
        );
    }

    /// An absent `max_depth` is not `max_depth: 0` (which means "only the
    /// root").
    #[test]
    fn an_absent_max_depth_is_not_a_zero_one() {
        let opts = opts_update();
        assert_ne!(
            PlanHasher::new(&opts, &SyncCompareOptions::default()).finish(),
            PlanHasher::new(
                &opts,
                &SyncCompareOptions {
                    max_depth: Some(0),
                    ..SyncCompareOptions::default()
                }
            )
            .finish(),
        );
    }

    /// `item` is what whoever accumulates the flow consumes, and it has to
    /// give exactly the same as calling by hand.
    /// The destination witness does NOT go into the digest, and it has to be
    /// pinned: if someone ever feeds it, the writer would hash one thing and
    /// `Spool::open` — which rebuilds the digest from the STEPS and never
    /// sees the witness — another, and every plan with an overwrite would
    /// fail its own verification and come out as `PlanStale`. Silently, and
    /// only in production.
    #[test]
    fn the_destination_witness_is_not_part_of_the_digest() {
        use norte_proto::EntryKind;

        use crate::DestWitness;

        let step = copy_step("a", 1);
        let mut without = hasher(&opts_update());
        without.item(&PlanItem::Step {
            step: step.clone(),
            dest: None,
        });
        let mut with = hasher(&opts_update());
        with.item(&PlanItem::Step {
            step: step.clone(),
            dest: Some(DestWitness {
                kind: EntryKind::File,
                size: Some(99),
                mtime_ms: Some(7),
                entries: None,
            }),
        });
        let mut other = hasher(&opts_update());
        other.item(&PlanItem::Step {
            step,
            dest: Some(DestWitness {
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
                entries: None,
            }),
        });
        let (without, with, other) = (without.finish(), with.finish(), other.finish());
        assert_eq!(without, with, "setting a witness does not change the plan");
        assert_eq!(with, other, "nor does swapping it for another");
    }

    #[test]
    fn feeding_items_and_feeding_halves_agree() {
        let step = copy_step("a", 1);
        let block = blocker(SyncBlockerKind::OverlapDetected, "sub");
        let mut a = hasher(&opts_update());
        a.item(&PlanItem::Step {
            step: step.clone(),
            dest: None,
        });
        a.item(&PlanItem::Blocker(block.clone()));
        let mut b = hasher(&opts_update());
        b.step(&step);
        b.blocker(&block);
        assert_eq!(a.finish(), b.finish());
    }

    /// A plan with more elements cannot hash like one with fewer, not even
    /// when the first is a prefix of the second.
    #[test]
    fn a_prefix_of_a_plan_is_not_that_plan() {
        let mut a = hasher(&opts_update());
        a.step(&copy_step("a", 1));
        let mut b = hasher(&opts_update());
        b.step(&copy_step("a", 1));
        b.step(&copy_step("b", 1));
        assert_ne!(a.finish(), b.finish());
    }

    /// **The consequence Task 8 and Task 10 have to know.** A read-only
    /// destination produces ONE element whatever the tree is, so all those
    /// plans hash alike: the hash says "this is the plan you were shown", and
    /// whoever executes decides by `executable`.
    #[test]
    fn every_read_only_plan_hashes_alike_so_apply_must_gate_on_executable() {
        let opts = SyncOptions {
            dest_writable: false,
            ..opts_update()
        };
        let read_only = SyncBlocker {
            rel: RelPath::default(),
            kind: SyncBlockerKind::DestReadOnly,
            side: Some(Side::Right),
        };
        let mut a = hasher(&opts);
        a.blocker(&read_only);
        let mut b = hasher(&opts);
        b.blocker(&read_only);
        assert_eq!(
            a.finish(),
            b.finish(),
            "two different trees, the same single element"
        );
    }

    /// The length prefix, checked on the mechanism itself and not only on its
    /// users: without it, two splits of the same bytes collide.
    #[test]
    fn the_length_prefix_is_what_separates_two_adjacent_fields() {
        let mut ab_c = Sha256::new();
        feed(&mut ab_c, b"ab");
        feed(&mut ab_c, b"c");
        let mut a_bc = Sha256::new();
        feed(&mut a_bc, b"a");
        feed(&mut a_bc, b"bc");
        let ab_c: [u8; 32] = ab_c.finalize().into();
        let a_bc: [u8; 32] = a_bc.finalize().into();
        assert_ne!(
            PlanHash::from_digest(&ab_c),
            PlanHash::from_digest(&a_bc),
            "without a length prefix, these two are the same digest"
        );
    }

    /// **FROZEN VECTOR of the framing.** The other twenty-odd tests are
    /// RELATIVE (`assert_ne!` between two digests), so they would still pass
    /// if the length prefix changed from `u64` to `u32`, if the presence byte
    /// swapped 0 and 1, or if two fields changed order — and any of those
    /// three silently invalidates every in-flight plan.
    ///
    /// Unlike `norte_core::hashing`'s vector, this one CAN be updated: there
    /// is nothing on disk depending on it (the spool lives `SYNC_PLAN_TTL_MS`
    /// and the same binary writes and reads it). What cannot be done is
    /// updating it so a diff you cannot explain turns green. If you added a
    /// field to the digest on purpose, change the constant and say so in the
    /// commit; if not, you broke the framing.
    #[test]
    fn the_framing_is_frozen() {
        let mut h = hasher(&opts_update());
        h.step(&copy_step("a.txt", 10));
        h.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "sub/x"));
        assert_eq!(
            h.finish().as_str(),
            // Changed when `dest_trash_restorable` was seeded (task 11b of
            // the sync plan): a NEW field in the intent, on purpose.
            "78f235a59de1d47760539a7b4f79bb35c3cda5e88e6ff230cc89afda9f52e289",
        );
    }

    /// A token this binary does not know is still a NAME, and two different
    /// unknowns do not collapse into one.
    #[test]
    fn an_unknown_token_hashes_as_a_name_and_not_as_a_hole() {
        assert_eq!(step_kind_name(SyncStepKind::Copy), "copy");
        assert_eq!(unknown_name(&SyncStepKind::CreateDir), "?CreateDir");
        assert_ne!(
            unknown_name(&SyncStepKind::CreateDir),
            unknown_name(&SyncStepKind::Copy)
        );
        // And it is never confused with a serde name, which is always
        // snake_case.
        assert!(unknown_name(&SyncStepKind::Copy).starts_with('?'));
    }
}
