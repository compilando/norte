//! `norte-compare`: the engine that answers "are these two trees the same?"
//! — and, when it answers yes, says **how much that yes is worth** (ADR
//! 0048, spec
//! `docs/superpowers/specs/2026-08-11-directory-comparison-design.md`).
//!
//! It is a pure function of two [`Provider`](norte_vfs::Provider)s: it knows
//! nothing of the daemon, the scheduler, the policy engine, or the journal.
//! A pair of roots goes in, a stream of [`CompareRow`] comes out. Who
//! accumulates those rows, who groups them into batches and who decides
//! whether the caller had permission to ask for them is `norte-core`'s
//! business.
//!
//! Mutates nothing: the comparison does not write a single byte, so it does
//! not enter the journal (hard rule 4 does not apply, and saying so here
//! saves the question).
//!
//! The pieces, bottom up:
//!
//! - [`key`] — the pairing: which name on one side is measured against
//!   which name on the other, and which two names on the same side collapse
//!   into one. Each name's original bytes survive intact (hard rule 1): the
//!   key exists ONLY to pair.
//! - [`cascade`] — the decision: a paired match goes in and a verdict comes
//!   out, the rung that decided it and what that rung is worth. Pure and
//!   synchronous: whatever needs I/O (a symlink's target, sha256) goes in
//!   already found out.
//! - `hash` (private) — the expensive rung: a file's streaming sha256. Not
//!   published because the engine does not offer "hash this", it offers
//!   [`CompareOptions::with_hash`].
//! - [`walk`] — the traversal: two roots go in and the stream of rows comes
//!   out. Depth first with an explicit stack, directory against directory,
//!   with errors turned into rows and cancellation as the only premature
//!   end.
//!
//! The rows' vocabulary — verdict, criterion and confidence — lives in
//! `norte-proto` and is re-exported here so whoever uses the engine does not
//! have to depend on the wire by hand.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cascade;
mod hash;
pub mod key;
pub mod walk;

pub use cascade::{Decision, HashOutcome, Prefetched, decide};
pub use key::{PairKey, PairName, SideIndex, Sides, index_side, key_for, pair_transform};
pub use walk::{CompareStream, compare};

pub use norte_proto::methods::{
    COMPARE_MAX_DIR_ENTRIES, COMPARE_ROWS_MAX_BATCH, CompareConfidence, CompareCriteria,
    CompareCriterion, CompareReason, CompareRow, CompareVerdict, PairTransform, Side,
};

/// The only thing that can end a comparison early.
///
/// Real failures — an unreadable subdirectory, an oversized directory, a
/// read broken halfway through a hash — are NOT here: they are
/// [`CompareVerdict::Error`] rows, and the walk continues. A three-hour
/// comparison must not die on leaf 40,000's `EACCES`.
///
/// ```
/// use norte_compare::CompareError;
/// assert_eq!(CompareError::Cancelled.to_string(), "comparison cancelled");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum CompareError {
    /// The Task's token fired (hard rule 3). The stream emits it ONCE and
    /// ends; it serves to distinguish "the tree finished" from "it was cut
    /// short" with no second channel to say so.
    ///
    /// There is nothing to clean up: the comparison does not write a single
    /// byte.
    #[error("comparison cancelled")]
    Cancelled,
}

/// Which cascade rungs run, and under what tolerance.
///
/// This is the wire's [`FsCompareParams`](norte_proto::methods::FsCompareParams)
/// minus the two roots: the engine receives those separately, alongside
/// their providers.
///
/// ```
/// use norte_compare::CompareOptions;
/// let o = CompareOptions::cheap();
/// assert_eq!(o.mtime_tolerance_ms, 2000, "the FAT rule");
/// assert!(!o.criteria.hash, "the rung that READS content is always explicit");
/// assert!(!o.follow_symlinks);
/// assert!(o.descend_orphans.is_none(), "an orphan is ONE row unless asked otherwise");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompareOptions {
    /// Which rungs run. The expensive one (`hash`) is opt-in.
    pub criteria: CompareCriteria,
    /// Maximum DESCENT depth, counting the root as 0. `None` = no limit.
    ///
    /// This is the depth of the directory being paired, not of the rows:
    /// with `Some(0)` only the root is paired, which emits its direct
    /// children's rows and does not descend into any of them.
    pub max_depth: Option<u32>,
    /// The mtime rung's tolerance, in milliseconds. Default 2000 (the FAT
    /// rule, the widest real granularity a filesystem this tree touches can
    /// have).
    ///
    /// `u32` and NOT `i64`: a negative tolerance makes `|Δ| > tolerance`
    /// true for EVERY pair, i.e. a whole comparison answering "different"
    /// over a typo. The type prevents it, here and on the wire
    /// (`protocol-guardian`'s MAJOR finding, C1 review).
    pub mtime_tolerance_ms: u32,
    /// Follow symlinks. Default `false`, and the spec leaves it out: targets
    /// are compared AS BYTES, so there is no need to detect cycles.
    ///
    /// **Accepted and IGNORED**: setting it to `true` does not change a
    /// single row, and nothing in the engine reads it. It is here because
    /// the field exists on the wire; whoever handles `fs.compare` must
    /// refuse `true` with `INVALID_PARAMS` instead of silently accepting a
    /// request it is not going to honor.
    pub follow_symlinks: bool,
    /// Descend into directories that exist ONLY on this side. `None` — the
    /// default, and the only thing spec 1 knew how to do — emits ONE row for
    /// the orphan and does not walk it.
    ///
    /// As a comparison option it stands on its own ("show me everything that
    /// is only on the left, not just the tip"), but whoever asked for it is
    /// the sync plan: whoever approves it needs to know HOW MANY files are
    /// inside the SOURCE's orphan, and the executor needs one step per file
    /// to journal it and to isolate a failure to a single file.
    ///
    /// Files, not bytes: an orphan row is not hydrated — no rung looks at
    /// it — so over a lazy provider (`file://` among them) its `size` comes
    /// back empty, and summing a plan's bytes requires `stat`ing them
    /// separately (<https://github.com/compilando/norte/issues/157>).
    ///
    /// The container's row STILL comes out, and it comes out before the ones
    /// inside it. It carries no "this one comes descended" mark because none
    /// is needed: the descent is a REQUEST option, so whoever asked for it
    /// already knows the directory's children follow behind it, and whoever
    /// did not ask for it receives the usual row.
    ///
    /// **ONE side, not both**, and the type enforces it. On a sync's
    /// destination, an orphan is a whole-tree deletion: a move to the trash,
    /// one journal entry and one thing to restore. Splitting it into forty
    /// thousand steps makes the undo worse and costs forty thousand listings
    /// to not change a single step of the plan.
    ///
    /// What the descent does NOT change: `max_depth` still bounds it (what
    /// gets bounded is the number of listings, whether it comes from a pair
    /// or an orphan), the [`COMPARE_MAX_DIR_ENTRIES`] ceiling is still per
    /// directory, an unreadable listing is still its own row, and an
    /// AMBIGUOUS orphan is not descended — same as an unreadable directory
    /// takes its subtree with it.
    ///
    /// And one that is surprising: inside an orphan, folding still happens
    /// with BOTH sides' capabilities ([`Sides::from_capabilities`]), even
    /// though the other side has nothing there. Two names the other side
    /// could not tell apart come out `Ambiguous` inside the orphan too, and
    /// that is the right thing for what the option exists for: they are
    /// exactly the two files that could not be written together at the
    /// destination.
    ///
    /// [`Side::Unknown`] is not a side, so it descends nothing. This is what
    /// a `"lft"` on the wire produces (`Side` degrades via `serde(other)`),
    /// and that is why whoever handles `fs.compare` refuses it with
    /// `INVALID_PARAMS` instead of silently serving a different set of rows
    /// than what was asked for.
    pub descend_orphans: Option<Side>,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 2000,
            follow_symlinks: false,
            descend_orphans: None,
        }
    }
}

impl CompareOptions {
    /// The comparison that does NOT read content: size and date, no hash.
    ///
    /// This is the default, with the expensive rung turned off explicitly so
    /// it shows at the call site.
    #[must_use]
    pub fn cheap() -> Self {
        Self {
            criteria: CompareCriteria {
                hash: false,
                ..CompareCriteria::default()
            },
            ..Self::default()
        }
    }

    /// Bounds the descent: the root is 0, so `max_depth(1)` pairs the root
    /// and its direct children, and goes no further.
    ///
    /// Shares its name with the field on purpose (they are different
    /// namespaces): whoever builds options writes `.max_depth(1)` and
    /// whoever reads them writes `opts.max_depth`.
    ///
    /// ```
    /// use norte_compare::CompareOptions;
    /// assert_eq!(CompareOptions::cheap().max_depth(1).max_depth, Some(1));
    /// assert_eq!(CompareOptions::cheap().max_depth, None, "no cap by default");
    /// ```
    #[must_use]
    pub fn max_depth(self, depth: u32) -> Self {
        Self {
            max_depth: Some(depth),
            ..self
        }
    }

    /// Turns on the expensive rung: streaming sha256 of the pairs the cheap
    /// rungs called EQUAL.
    ///
    /// This is the only thing in this struct that READS content, and that is
    /// why it is explicit and not a default: nobody hashes a terabyte over
    /// SFTP without having asked for it.
    ///
    /// ```
    /// use norte_compare::CompareOptions;
    /// assert!(CompareOptions::cheap().with_hash().criteria.hash);
    /// assert!(!CompareOptions::cheap().criteria.hash, "still opt-in");
    /// ```
    #[must_use]
    pub fn with_hash(self) -> Self {
        Self {
            criteria: CompareCriteria {
                hash: true,
                ..self.criteria
            },
            ..self
        }
    }
}
