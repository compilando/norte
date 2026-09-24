//! The cascade: ONE already-paired match goes in, ONE verdict comes out —
//! and with it, which rung decided it and **what that rung is worth**.
//!
//! It goes from cheap to expensive and **stops at the first rung that
//! decides**, so the criterion is also "how far it had to go": without it,
//! `Different` does not say whether a `u64` or 40 GB of bytes was compared.
//! The table is the spec's
//! (`docs/superpowers/specs/2026-08-11-directory-comparison-design.md`,
//! "The cascade"), and the CONFIDENCES are the part that has to be read
//! slowly:
//!
//! | rung | condition | verdict | confidence |
//! | --- | --- | --- | --- |
//! | Presence | a side is missing | `OnlyLeft`/`OnlyRight` | `Certain` |
//! | Kind | the `EntryKind`s differ | `TypeMismatch` | `Certain` |
//! | Symlink | targets, AS BYTES | `Same`/`Different` | `Certain` |
//! | Size | both known and different | `Different` | `Certain` |
//! | Size | one unknown | `Same` | `Unknown` |
//! | Mtime | \|Δ\| > tolerance | `Different` | `Probable` |
//! | Mtime | \|Δ\| ≤ tolerance | `Same` | `Probable` |
//! | Mtime | unknown on one side | `Same` | `Unknown` |
//! | Hash | streaming sha256 of both | `Same`/`Different` | `Certain` |
//!
//! A different size PROVES different bytes; a different date only SUGGESTS
//! it; and a provider that cannot answer leaves `Unknown`, which is an
//! answer and not a failure. Lowering a row from `Unknown` to `Probable`
//! because "something has to be painted" is exactly the mistake this module
//! exists to not make.
//!
//! # Why [`decide`] is SYNCHRONOUS
//!
//! Because the only two facts that require I/O — a symlink's target
//! (`Provider::read_link`) and the content's sha256 — **come in already
//! computed**, in [`Prefetched`]. Whoever walks the tree (`walk`) knows to
//! ask for them; the cascade only decides. That way the whole table is
//! tested with no provider, no async runtime and no mock answering whatever
//! it is told to.

use norte_proto::{Entry, EntryKind};

use crate::{
    CompareConfidence, CompareCriterion, CompareOptions, CompareRow, CompareVerdict, PairName, Side,
};

/// What the expensive rung (sha256) has ALREADY said about a pair by the
/// time [`decide`] looks at it.
///
/// [`decide`] does not read content: reading it is async and expensive, and
/// deciding is neither. The walk asks first with [`HashOutcome::NotRun`],
/// looks at [`Decision::needs_hash`], and only if that comes back `true`
/// does it read both files and ask again with the result.
///
/// ```
/// use norte_compare::cascade::HashOutcome;
/// assert_eq!(HashOutcome::default(), HashOutcome::NotRun, "nobody has read anything yet");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HashOutcome {
    /// Nobody has hashed anything: either the caller did not ask for the
    /// rung, or it has not gotten to reading yet.
    #[default]
    NotRun,
    /// Both sha256s match.
    Equal,
    /// Both sha256s differ.
    Differ,
}

/// The facts [`decide`] cannot find out on its own, already found out.
///
/// There are exactly two, and both require I/O: a symlink's target and the
/// content's sha256. Targets are compared **as bytes** and never followed
/// (with no following there is no need to detect cycles, and a link whose
/// target changed is a real difference).
///
/// A `None` target means "could not be read / was not read", and the
/// cascade answers `Unknown` instead of inventing that the links are equal.
///
/// ```
/// use norte_compare::cascade::{HashOutcome, Prefetched};
/// let nothing = Prefetched::none();
/// assert_eq!(nothing.hash, HashOutcome::NotRun);
/// assert!(nothing.left_target.is_none());
///
/// let links = Prefetched::links(Some(b"../a"), Some(b"../b"));
/// assert_eq!(links.right_target, Some(&b"../b"[..]));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Prefetched<'a> {
    /// The left symlink's target, in raw bytes. `None` = not read or could
    /// not be read.
    pub left_target: Option<&'a [u8]>,
    /// The right symlink's target, in raw bytes.
    pub right_target: Option<&'a [u8]>,
    /// What the hash rung said, if it ran at all.
    pub hash: HashOutcome,
}

impl<'a> Prefetched<'a> {
    /// Nothing found out: no targets, no hash. This is what the walk passes
    /// on the first pass of EVERY pair.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            left_target: None,
            right_target: None,
            hash: HashOutcome::NotRun,
        }
    }

    /// Both symlink targets, exactly as `Provider::read_link` gave them.
    #[must_use]
    pub const fn links(left: Option<&'a [u8]>, right: Option<&'a [u8]>) -> Self {
        Self {
            left_target: left,
            right_target: right,
            hash: HashOutcome::NotRun,
        }
    }

    /// The same set of facts, with the hash rung's result set.
    #[must_use]
    pub const fn with_hash(self, hash: HashOutcome) -> Self {
        Self { hash, ..self }
    }
}

/// What the cascade decided about a pair: the verdict, the rung that
/// produced it and what that rung has earned.
///
/// This is not yet a [`CompareRow`]: it is missing the id and the two
/// `Entry`s, which the walk sets ([`Decision::into_row`]).
///
/// ```
/// use norte_compare::cascade::Decision;
/// use norte_compare::{CompareConfidence, CompareCriterion, CompareVerdict};
///
/// // Presence is the cheapest and most certain rung: if a side is not
/// // there, there is nothing else to compare.
/// let d = Decision::only_left();
/// assert_eq!(d.verdict, CompareVerdict::OnlyLeft);
/// assert_eq!(d.criterion, CompareCriterion::Presence);
/// assert_eq!(d.confidence, CompareConfidence::Certain);
/// assert!(!d.needs_hash);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decision {
    /// What one is relative to the other.
    pub verdict: CompareVerdict,
    /// Which rung decided it.
    pub criterion: CompareCriterion,
    /// How much that rung is worth. NEVER raised "to make it look nice".
    pub confidence: CompareConfidence,
    /// Which side is NEWER, and only when the date rung decided it. Nothing
    /// in this spec reads it: spec 2 needs it to propose a direction, and
    /// producing it here costs nothing.
    pub newer: Option<Side>,
    /// The cheap rungs called the pair EQUAL, the caller asked for hash and
    /// the hash has not run yet: this decision **is not final**. Whoever
    /// receives it reads both files and calls [`decide`] again with the
    /// [`HashOutcome`]. Emitting a row with this set to `true` is publishing
    /// a provisional verdict, and in this spec no row is ever corrected
    /// later.
    pub needs_hash: bool,
}

impl Decision {
    /// A final decision from a rung that looks at neither dates nor sides.
    const fn rung(
        verdict: CompareVerdict,
        criterion: CompareCriterion,
        confidence: CompareConfidence,
    ) -> Self {
        Self {
            verdict,
            criterion,
            confidence,
            newer: None,
            needs_hash: false,
        }
    }

    /// The presence rung: exists only on the left.
    #[must_use]
    pub const fn only_left() -> Self {
        Self::rung(
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
        )
    }

    /// The presence rung: exists only on the right.
    #[must_use]
    pub const fn only_right() -> Self {
        Self::rung(
            CompareVerdict::OnlyRight,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
        )
    }

    /// The wire row: this decision, an id and both sides.
    ///
    /// The caller sets the sides because only it knows which one is
    /// missing: a presence decision carries `None` on its missing side.
    ///
    /// ```
    /// use norte_compare::cascade::Decision;
    /// use norte_compare::CompareVerdict;
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # let left = Entry { path: VPath::parse("file:///a").expect("path"),
    /// #     kind: EntryKind::File, size: Some(1), mtime_ms: None, attrs: Default::default() };
    /// let row = Decision::only_left().into_row(7, Some(left), None);
    /// assert_eq!(row.id, 7);
    /// assert_eq!(row.verdict, CompareVerdict::OnlyLeft);
    /// assert!(row.sides_are_consistent());
    /// assert!(row.reason.is_none(), "the cascade does not produce rows with a reason");
    /// ```
    #[must_use]
    pub fn into_row(self, id: u64, left: Option<Entry>, right: Option<Entry>) -> CompareRow {
        // #152's mark is computed HERE and not set by the caller because
        // both entries are here and there is no other path to a row with
        // both sides: a row emitted without going through this constructor
        // could not forget the mark, because it does not exist.
        let paired_under = match (left.as_ref(), right.as_ref()) {
            (Some(l), Some(r)) => crate::key::pair_transform(l.pair_name(), r.pair_name()),
            _ => None,
        };
        let row = CompareRow {
            id,
            left,
            right,
            verdict: self.verdict,
            criterion: self.criterion,
            confidence: self.confidence,
            newer: self.newer,
            // The cascade emits neither `Ambiguous` nor `Error`: the reason
            // and the side belong to the walk (collisions, unreadable
            // listings, broken reads).
            reason: None,
            side: None,
            paired_under,
        };
        // The invariant the wire states and cannot check itself
        // (`CompareRow::paired_under`): a transform is a PAIR's property, so
        // marking a single-sided row would mean nothing. It is true today by
        // construction — the `match` above — and this assert is what keeps
        // it true if someone rewrites that `match`.
        debug_assert!(
            row.paired_under.is_none() || (row.left.is_some() && row.right.is_some()),
            "pairing transform on a row with no two sides"
        );
        debug_assert!(
            row.sides_are_consistent(),
            "verdict {:?} with left={} right={}",
            row.verdict,
            row.left.is_some(),
            row.right.is_some()
        );
        row
    }
}

/// Decides ONE pair: goes down the cascade and stops at the first rung that
/// answers.
///
/// `facts` carries whatever needs I/O, already done (see [`Prefetched`]);
/// `opts` says which rungs run and with what tolerance. The function is
/// pure: same inputs, same decision, always.
///
/// It is **symmetric**: swapping the two sides swaps the verdict
/// (`OnlyLeft`↔`OnlyRight`, which the walk decides) and [`Decision::newer`]'s
/// side, and changes nothing else. A comparison that was not would have a
/// favorite.
///
/// ```
/// use norte_compare::cascade::{decide, Prefetched};
/// use norte_compare::{CompareConfidence, CompareCriterion, CompareOptions, CompareVerdict};
/// # use norte_proto::{Entry, EntryKind, VPath};
/// # fn f(size: Option<u64>, mtime: Option<i64>) -> Entry {
/// #     Entry { path: VPath::parse("file:///x").expect("path"), kind: EntryKind::File,
/// #             size, mtime_ms: mtime, attrs: Default::default() }
/// # }
/// let opts = CompareOptions::cheap();
///
/// // Different sizes: PROOF of different bytes.
/// let d = decide(&f(Some(10), Some(0)), &f(Some(20), Some(0)), &opts, &Prefetched::none());
/// assert_eq!(d.criterion, CompareCriterion::Size);
/// assert_eq!(d.confidence, CompareConfidence::Certain);
///
/// // Same date: only a SUGGESTION that they are equal.
/// let d = decide(&f(Some(10), Some(0)), &f(Some(10), Some(500)), &opts, &Prefetched::none());
/// assert_eq!((d.verdict, d.confidence), (CompareVerdict::Same, CompareConfidence::Probable));
///
/// // A provider that does not know the size gets no invented confidence.
/// let d = decide(&f(None, Some(0)), &f(Some(10), Some(0)), &opts, &Prefetched::none());
/// assert_eq!((d.verdict, d.confidence), (CompareVerdict::Same, CompareConfidence::Unknown));
/// ```
#[must_use]
pub fn decide(
    left: &Entry,
    right: &Entry,
    opts: &CompareOptions,
    facts: &Prefetched<'_>,
) -> Decision {
    let cheap = cheap_rungs(left, right, opts, facts);

    // The expensive rung only reaches pairs the cheap ones called equal —
    // "verify what looks the same" — and only files: a directory has no
    // content to hash and a symlink already decided by its target.
    if !opts.criteria.hash || cheap.verdict != CompareVerdict::Same || left.kind != EntryKind::File
    {
        return cheap;
    }
    match facts.hash {
        HashOutcome::NotRun => Decision {
            needs_hash: true,
            ..cheap
        },
        HashOutcome::Equal => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
        ),
        HashOutcome::Differ => Decision::rung(
            CompareVerdict::Different,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
        ),
    }
}

/// The rungs that do not read content: kind, link target, size and date.
fn cheap_rungs(
    left: &Entry,
    right: &Entry,
    opts: &CompareOptions,
    facts: &Prefetched<'_>,
) -> Decision {
    if left.kind != right.kind {
        return Decision::rung(
            CompareVerdict::TypeMismatch,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
        );
    }

    match left.kind {
        EntryKind::File => size_and_mtime(left, right, opts),
        // Two directories with the same name are THE SAME directory: their
        // differences are their children's rows, which the walk emits
        // separately. Comparing them by size or date would paint
        // "different" on every directory containing a changed file — a
        // directory's date moves with any child — and drown the panel in
        // noise that cannot be acted on.
        EntryKind::Dir => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
        ),
        EntryKind::Symlink => link_target(facts),
        // A socket, a fifo, a device — or a kind from an N+1 protocol this
        // binary does not know. They are the same type and that is as far
        // as knowledge goes: their "size" is not content and their date
        // says nothing.
        EntryKind::Other => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Kind,
            CompareConfidence::Unknown,
        ),
    }
}

/// The symlink rung: the targets, as bytes, never followed.
fn link_target(facts: &Prefetched<'_>) -> Decision {
    match (facts.left_target, facts.right_target) {
        (Some(l), Some(r)) => Decision::rung(
            if l == r {
                CompareVerdict::Same
            } else {
                CompareVerdict::Different
            },
            CompareCriterion::LinkTarget,
            CompareConfidence::Certain,
        ),
        // Without both targets there is no comparison possible, and saying
        // `Different` would invent a difference just as saying `Same` would
        // invent an equality.
        _ => Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::LinkTarget,
            CompareConfidence::Unknown,
        ),
    }
}

/// A file's two metadata rungs, in order.
fn size_and_mtime(left: &Entry, right: &Entry, opts: &CompareOptions) -> Decision {
    if opts.criteria.size {
        match (left.size, right.size) {
            (Some(l), Some(r)) if l != r => {
                return Decision::rung(
                    CompareVerdict::Different,
                    CompareCriterion::Size,
                    CompareConfidence::Certain,
                );
            }
            // Equal sizes decide nothing: two 4 KiB files can have
            // different bytes. The cascade continues.
            (Some(_), Some(_)) => {}
            // A side with no size: the rung cannot answer, and the next one
            // does not fix it either. `Same`/`Unknown` is the honest
            // answer — following through to the date would give
            // `Probable`, i.e. MORE confidence than there actually is.
            _ => {
                return Decision::rung(
                    CompareVerdict::Same,
                    CompareCriterion::Size,
                    CompareConfidence::Unknown,
                );
            }
        }
    }

    if opts.criteria.mtime {
        let (Some(l), Some(r)) = (left.mtime_ms, right.mtime_ms) else {
            return Decision::rung(
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Unknown,
            );
        };
        // `(l - r).abs()` OVERFLOWS: pre-1970 dates are negative and real,
        // and a pair (i64::MIN, i64::MAX) blows up in debug and gives
        // garbage in release. `saturating_sub` + `unsigned_abs` cannot
        // overflow, and the tolerance is `u32` so there is no negative
        // tolerance that could turn the whole comparison into "different".
        if l.saturating_sub(r).unsigned_abs() > u64::from(opts.mtime_tolerance_ms) {
            return Decision {
                newer: Some(if l > r { Side::Left } else { Side::Right }),
                ..Decision::rung(
                    CompareVerdict::Different,
                    CompareCriterion::Mtime,
                    CompareConfidence::Probable,
                )
            };
        }
        return Decision::rung(
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Probable,
        );
    }

    // Neither size nor date: no rung decided. `Presence` is these rows'
    // criterion by wire convention (see `CompareCriterion::Presence`), and
    // the confidence is `Unknown` because literally nothing was compared.
    Decision::rung(
        CompareVerdict::Same,
        CompareCriterion::Presence,
        CompareConfidence::Unknown,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norte_proto::VPath;

    use super::*;
    use crate::CompareCriteria;

    /// `size` and `mtime` are generic because the SAME table carries
    /// `file(10, 0)`, `file(10, None)` and `file(None, 0)`.
    fn entry(
        kind: EntryKind,
        size: impl Into<Option<u64>>,
        mtime: impl Into<Option<i64>>,
    ) -> Entry {
        Entry {
            path: VPath::parse("file:///x").expect("path"),
            kind,
            size: size.into(),
            mtime_ms: mtime.into(),
            attrs: BTreeMap::default(),
        }
    }

    fn file(size: impl Into<Option<u64>>, mtime: impl Into<Option<i64>>) -> Entry {
        entry(EntryKind::File, size, mtime)
    }

    fn dir() -> Entry {
        entry(EntryKind::Dir, None, 1_700_000_000_000)
    }

    fn link() -> Entry {
        entry(EntryKind::Symlink, 4, 0)
    }

    /// Nothing found out: no link targets, no hash.
    fn no_hash() -> Prefetched<'static> {
        Prefetched::none()
    }

    fn targets<'a>(left: &'a [u8], right: &'a [u8]) -> Prefetched<'a> {
        Prefetched::links(Some(left), Some(right))
    }

    fn with_hash() -> CompareOptions {
        CompareOptions {
            criteria: CompareCriteria {
                hash: true,
                ..CompareCriteria::default()
            },
            ..CompareOptions::cheap()
        }
    }

    /// The cascade's whole contract, one row per rung. The confidences are the
    /// point: a different size PROVES different bytes, a different mtime only
    /// suggests it, and a provider that cannot say leaves `Unknown` rather than
    /// having something invented for it.
    #[test]
    fn the_cascade_decides_and_says_how_sure_it_is() {
        use CompareConfidence as Conf;
        use CompareCriterion as C;
        use CompareVerdict::{Different, Same, TypeMismatch};
        let opts = CompareOptions::cheap(); // tolerance 2000 ms, no hash

        let cases = [
            // (left, right, verdict, criterion, confidence)
            (file(10, 0), file(20, 0), Different, C::Size, Conf::Certain),
            (file(10, 0), dir(), TypeMismatch, C::Kind, Conf::Certain),
            (file(10, 0), file(10, 1_000), Same, C::Mtime, Conf::Probable),
            (
                file(10, 0),
                file(10, 5_000),
                Different,
                C::Mtime,
                Conf::Probable,
            ),
            (file(10, 0), file(10, None), Same, C::Mtime, Conf::Unknown),
            (file(None, 0), file(10, 0), Same, C::Size, Conf::Unknown),
        ];
        for (l, r, verdict, criterion, confidence) in cases {
            let row = decide(&l, &r, &opts, &no_hash());
            assert_eq!(
                (row.verdict, row.criterion, row.confidence),
                (verdict, criterion, confidence),
                "{l:?} vs {r:?}"
            );
        }
    }

    /// Spec 2 proposes a direction from this field, so it is produced here even
    /// though nothing in this spec reads it.
    #[test]
    fn a_row_that_differs_by_mtime_records_the_newer_side() {
        let row = decide(
            &file(10, 0),
            &file(10, 9_000),
            &CompareOptions::cheap(),
            &no_hash(),
        );
        assert_eq!(row.newer, Some(Side::Right));
    }

    /// Symlinks are compared, not followed: no cycle detection needed, and a
    /// link whose target changed is a real difference.
    ///
    /// The target does NOT live in `Entry`: `Provider::read_link` reads it,
    /// which is async, so it comes in via [`Prefetched`] and `decide` stays
    /// pure.
    #[test]
    fn symlink_targets_are_compared_as_bytes() {
        let opts = CompareOptions::cheap();
        let same = decide(&link(), &link(), &opts, &targets(b"../a", b"../a"));
        assert_eq!(
            (same.verdict, same.criterion),
            (CompareVerdict::Same, CompareCriterion::LinkTarget)
        );
        let diff = decide(&link(), &link(), &opts, &targets(b"../a", b"../b"));
        assert_eq!(diff.verdict, CompareVerdict::Different);
        assert_eq!(diff.confidence, CompareConfidence::Certain);
    }

    // ---- what the spec's table does not fix, and a review could bend ----

    /// A target that could not be read does not turn into "they are equal":
    /// without both targets the confidence is `Unknown`, which is an answer.
    #[test]
    fn a_link_with_no_target_read_does_not_invent_an_equality() {
        let opts = CompareOptions::cheap();
        for facts in [
            no_hash(),
            Prefetched::links(Some(b"../a"), None),
            Prefetched::links(None, Some(b"../a")),
        ] {
            let d = decide(&link(), &link(), &opts, &facts);
            assert_eq!(
                (d.verdict, d.criterion, d.confidence),
                (
                    CompareVerdict::Same,
                    CompareCriterion::LinkTarget,
                    CompareConfidence::Unknown
                ),
                "{facts:?}"
            );
        }
    }

    /// Two directories with the same name are the same directory: their
    /// differences are their children's rows. Comparing them by date would
    /// paint "different" on every folder that contains a changed file.
    #[test]
    fn two_directories_are_not_compared_by_date_or_size() {
        let left = entry(EntryKind::Dir, 4096, 0);
        let right = entry(EntryKind::Dir, 8192, 999_999_999);
        let d = decide(&left, &right, &CompareOptions::cheap(), &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Kind,
                CompareConfidence::Certain
            )
        );
        assert!(d.newer.is_none(), "a directory has no newer side");
    }

    /// A kind that is neither file, directory nor link — a fifo, a device,
    /// or an N+1 daemon's kind — gets no invented confidence.
    #[test]
    fn an_unknown_kind_is_not_compared_by_metadata() {
        let left = entry(EntryKind::Other, 1, 0);
        let right = entry(EntryKind::Other, 2, 500_000);
        let d = decide(&left, &right, &CompareOptions::cheap(), &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Kind,
                CompareConfidence::Unknown
            )
        );
    }

    /// Pre-1970 dates are negative and REAL: `(l - r).abs()` overflows with
    /// them. The saturated subtraction does not, and keeps answering what it
    /// should.
    #[test]
    fn pre_1970_dates_do_not_overflow_the_subtraction() {
        let opts = CompareOptions::cheap();
        let d = decide(&file(10, i64::MIN), &file(10, i64::MAX), &opts, &no_hash());
        assert_eq!(d.verdict, CompareVerdict::Different);
        assert_eq!(d.newer, Some(Side::Right));

        let d = decide(&file(10, i64::MIN), &file(10, i64::MIN), &opts, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            )
        );

        // And a pre-1970 date within tolerance is still the same.
        let d = decide(
            &file(10, -1_000_000_000_000),
            &file(10, -1_000_000_001_000),
            &opts,
            &no_hash(),
        );
        assert_eq!(d.verdict, CompareVerdict::Same);
    }

    /// The tolerance INCLUDES its boundary: the spec says `|Δ| > tolerance`
    /// for `Different`, and a FAT with 2s granularity cannot distinguish
    /// exactly 2000ms.
    #[test]
    fn the_tolerance_includes_its_boundary() {
        let opts = CompareOptions::cheap();
        assert_eq!(
            decide(&file(10, 0), &file(10, 2_000), &opts, &no_hash()).verdict,
            CompareVerdict::Same
        );
        assert_eq!(
            decide(&file(10, 0), &file(10, 2_001), &opts, &no_hash()).verdict,
            CompareVerdict::Different
        );
    }

    /// The cascade is SYMMETRIC: swapping the sides changes `newer`'s side
    /// and nothing else. The walk's mirror test relies on this.
    #[test]
    fn swapping_the_sides_only_changes_the_newer_side() {
        let opts = CompareOptions::cheap();
        let pairs = [
            (file(10, 0), file(20, 0)),
            (file(10, 0), file(10, 9_000)),
            (file(10, 0), file(10, None)),
            (file(None, 0), file(10, 0)),
            (file(10, 0), dir()),
            (link(), link()),
        ];
        for (l, r) in pairs {
            let forward = decide(&l, &r, &opts, &targets(b"../a", b"../b"));
            let back = decide(&r, &l, &opts, &targets(b"../b", b"../a"));
            assert_eq!(
                (forward.verdict, forward.criterion, forward.confidence),
                (back.verdict, back.criterion, back.confidence),
                "{l:?} vs {r:?}"
            );
            let mirrored = match back.newer {
                Some(Side::Left) => Some(Side::Right),
                Some(Side::Right) => Some(Side::Left),
                other => other,
            };
            assert_eq!(forward.newer, mirrored, "{l:?} vs {r:?}");
        }
    }

    /// The expensive rung does NOT decide here: `decide` is synchronous and
    /// reads no content. When the cheap ones call the pair equal and the
    /// caller asked for hash, the decision comes out marked as NOT final,
    /// and the walk comes back with the result.
    #[test]
    fn the_hash_rung_is_requested_and_then_answered() {
        let opts = with_hash();

        let pending = decide(&file(10, 0), &file(10, 0), &opts, &no_hash());
        assert!(pending.needs_hash, "the cheap ones said `Same`");
        assert_eq!(pending.criterion, CompareCriterion::Mtime);

        let equal = decide(
            &file(10, 0),
            &file(10, 0),
            &opts,
            &no_hash().with_hash(HashOutcome::Equal),
        );
        assert_eq!(
            (equal.verdict, equal.criterion, equal.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Hash,
                CompareConfidence::Certain
            )
        );
        assert!(!equal.needs_hash);

        let different = decide(
            &file(10, 0),
            &file(10, 0),
            &opts,
            &no_hash().with_hash(HashOutcome::Differ),
        );
        assert_eq!(
            (different.verdict, different.criterion, different.confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Hash,
                CompareConfidence::Certain
            )
        );
    }

    /// The expensive rung ONLY reaches what the cheap ones called equal, and
    /// only files. Hashing a pair already known to differ is reading two
    /// whole files to learn nothing.
    #[test]
    fn the_hash_does_not_reach_what_is_already_decided() {
        let opts = with_hash();
        for (l, r) in [
            (file(10, 0), file(20, 0)),     // different by size
            (file(10, 0), file(10, 9_000)), // different by date
            (file(10, 0), dir()),           // different by kind
            (dir(), dir()),                 // a directory has no content
            (link(), link()),               // the target already decided
        ] {
            let d = decide(&l, &r, &opts, &targets(b"../a", b"../a"));
            assert!(!d.needs_hash, "{l:?} vs {r:?}");
        }
    }

    /// An unknown size does not prevent hashing either: it is exactly the
    /// pair the expensive rung turns from `Unknown` into `Certain`.
    #[test]
    fn a_pair_with_no_size_can_still_be_verified() {
        let d = decide(&file(None, 0), &file(10, 0), &with_hash(), &no_hash());
        assert!(d.needs_hash);
        assert_eq!(d.confidence, CompareConfidence::Unknown);
    }

    /// Turning off a rung SKIPS it, it does not turn it into `Different`.
    #[test]
    fn turning_off_a_rung_skips_it() {
        let no_size = CompareOptions {
            criteria: CompareCriteria {
                size: false,
                ..CompareCriteria::default()
            },
            ..CompareOptions::cheap()
        };
        let d = decide(&file(10, 0), &file(999, 500), &no_size, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            ),
            "without the size rung the date decides"
        );

        // With NO rung at all there is no criterion that decided: `Presence`
        // by wire convention, and `Unknown` because nothing was compared.
        let no_rungs = CompareOptions {
            criteria: CompareCriteria {
                size: false,
                mtime: false,
                hash: false,
            },
            ..CompareOptions::cheap()
        };
        let d = decide(&file(10, 0), &file(999, 500), &no_rungs, &no_hash());
        assert_eq!(
            (d.verdict, d.criterion, d.confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::Presence,
                CompareConfidence::Unknown
            )
        );
    }

    /// The presence rung: the cheapest and the only one that compares
    /// nothing.
    #[test]
    fn presence_decides_with_certainty_and_produces_a_consistent_row() {
        let left = Decision::only_left();
        assert_eq!(
            (left.verdict, left.criterion, left.confidence),
            (
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain
            )
        );
        let row = left.into_row(3, Some(file(10, 0)), None);
        assert!(row.sides_are_consistent());
        assert!(row.reason_is_consistent());
        assert_eq!(row.id, 3);

        let right = Decision::only_right();
        assert_eq!(right.verdict, CompareVerdict::OnlyRight);
        assert!(
            right
                .into_row(4, None, Some(file(10, 0)))
                .sides_are_consistent()
        );
    }

    /// A cascade decision turns into a row with NOTHING lost: the newer side
    /// travels, and neither the reason nor the side is invented.
    #[test]
    fn the_decision_travels_whole_into_the_row() {
        let d = decide(
            &file(10, 0),
            &file(10, 9_000),
            &CompareOptions::cheap(),
            &no_hash(),
        );
        let row = d.into_row(9, Some(file(10, 0)), Some(file(10, 9_000)));
        assert_eq!(row.verdict, CompareVerdict::Different);
        assert_eq!(row.criterion, CompareCriterion::Mtime);
        assert_eq!(row.confidence, CompareConfidence::Probable);
        assert_eq!(row.newer, Some(Side::Right));
        assert_eq!(row.reason, None);
        assert_eq!(row.side, None);
        assert!(row.sides_are_consistent() && row.reason_is_consistent());
    }
}
