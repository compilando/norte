//! The walk: two roots go in, a stream of [`CompareRow`] comes out.
//!
//! It is **depth-first with an explicit stack**, not async recursion: no
//! future boxed per level and no stack blown on a deep tree.
//!
//! # The memory ceiling, said precisely
//!
//! It is O(one directory), not O(one tree) — which is what matters and what
//! [`COMPARE_MAX_DIR_ENTRIES`] bounds— but the constant is NOT 1: inside
//! `visit` the two listings coexist, their two indexes (a `BTreeMap` with a
//! `Vec` per key), the common subdirectories and the rows already produced,
//! which on top of that carry CLONED `Entry`s and survive `visit` until the
//! consumer drains them. Four or five times a listing, not one. The stack,
//! on the other hand, IS negligible: it grows with each level's siblings,
//! and all of them really exist in the tree.
//!
//! # Why directory against directory
//!
//! `fs.list` documents its order as "the provider's, with no guarantee", so
//! there are no two ordered streams to merge. One directory is drained from
//! each side, they are indexed by their pairing key ([`crate::key`]) —which
//! does order them—, the two key lists are merged, the rows are emitted and
//! the common subdirectories are pushed. The memory ceiling comes from
//! there, and so does [`COMPARE_MAX_DIR_ENTRIES`]: a directory above it
//! costs ITS row, never an OOM that takes the other three hours of work with
//! it.
//!
//! # What the listing did not bring is asked, and only when needed
//!
//! `Entry::size` and `Entry::mtime_ms` are `Option` because a listing may
//! not bring them, and the provider people USE does not bring them:
//! `norte-vfs-local::list` leaves them at `None` on purpose (#52 — a 40,000
//! entry directory's `readdir` does not spend 40,000 `stat`s to paint a
//! list). Feeding the cascade with that makes the size rung answer
//! `Same`/`Unknown` for two files of 5 and 12 bytes, i.e. the whole local
//! comparison —the one almost everybody does— tells nothing apart.
//!
//! So the walk **hydrates on demand**: `hydrate` spends a `stat` on the side
//! missing the field, and only when the pair is going to REACH the rung
//! that uses it. Its rustdoc —the function's, private, in this same file—
//! carries the cost, when it is paid and what happens when the `stat`
//! fails. It is not linked from here on purpose: this module doc is public
//! and `hydrate` is not, and `-D warnings` turns that link into a docs-gate
//! error.
//!
//! # An orphan can be descended, and from ONLY one side
//!
//! By default a directory that exists only on one side is ONE row and its
//! subtree is not looked at: whoever copies that orphan will do it with a
//! recursive `fs.copy`, so enumerating it buys nothing and costs the whole
//! traversal.
//!
//! [`CompareOptions::descend_orphans`](crate::CompareOptions::descend_orphans)
//! changes that for ONE named side, and it does it through the SAME stack:
//! an orphan's frame carries one side as `Some` and the other as `None`,
//! and the missing side contributes the EMPTY listing. From there, through
//! the same merge-join as always, comes one orphan row per entry on the
//! side that IS there — with the same [`COMPARE_MAX_DIR_ENTRIES`] ceiling,
//! the same per-directory and per-pair cancellation, and the same
//! `max_depth`. There is no second path to maintain.
//!
//! The reason it is one side and not both is in spec 2
//! (`2026-08-11-directory-sync-design.md`): at a synchronization's
//! destination, an orphan is a WHOLE-tree deletion —a trash bin, a journal
//! entry, one thing to restore—, so descending it would buy forty thousand
//! listings that do not change a single step of the plan.
//!
//! # Errors are rows
//!
//! An unreadable listing, an oversized directory, a pairing collision or a
//! read that breaks halfway through a hash produce their row and the walk
//! CONTINUES. The only thing that ends the
//! stream early is cancellation (hard rule 3), and it says so with a final
//! [`CompareError::Cancelled`] so whoever consumes it does not have to
//! guess whether the tree finished or was cut short.
//!
//! An error or ambiguity row ABOUT A DIRECTORY takes its whole subtree down
//! with it, which stays unexamined and without rows. It is the right
//! decision —what could not be listed cannot be paired— but the row does
//! not say so, so whoever paints it has to say it in its place.
//!
//! # Row order
//!
//! Deterministic: per directory, first the left's ambiguous rows, then the
//! right's, and then the merge-join in KEY order. Common subdirectories are
//! pushed in reverse so the stack pops them out in key order too. Without
//! that determinism it cannot be asserted that comparing in reverse gives
//! the exact mirror, which is how it is checked that the comparison has no
//! favorite side.
//!
//! The only known asymmetry is ORDER (not verdicts) when BOTH sides fail at
//! once —a key that collides on both, two unreadable directories paired—:
//! the left's rows come out first by convention, and there is no symmetric
//! convention possible.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;

use futures::StreamExt;
use futures::stream::{self, FusedStream};
use norte_proto::{Capabilities, Entry, EntryKind, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::cascade::{Decision, HashOutcome, Prefetched, decide};
use crate::hash::{HashFailure, sha256_of};
use crate::key::{PairName, SideIndex, Sides, index_side, key_for};
use crate::{
    COMPARE_MAX_DIR_ENTRIES, CompareConfidence, CompareCriterion, CompareError, CompareOptions,
    CompareReason, CompareRow, CompareVerdict, Side,
};

/// The stream [`compare`] produces.
///
/// It is a `Box` and not an `impl Stream` for a boring and good reason: the
/// type is CONCRETE, so `norte-core` can store it in a Task struct without
/// dragging type parameters along, and the two roots can be passed by
/// reference without their borrow ending up captured in the return type.
/// One allocation per whole comparison.
///
/// And it is [`FusedStream`], not a plain `BoxStream` — same as
/// `norte_sync::plan` solved the same problem: asking it for another item
/// after the end returns `None` instead of panicking, which is what
/// `futures`'s raw `Unfold` does. A `select!` with a second branch (a flush
/// `tick`, a cancellation) is legal over this stream (#175).
pub type CompareStream<'a> =
    Pin<Box<dyn FusedStream<Item = Result<CompareRow, CompareError>> + Send + 'a>>;

/// The capabilities of directory `dir` according to its provider, or `None`
/// if there is no directory (a missing side) or the probe did not know.
async fn capabilities_of(provider: &dyn Provider, dir: Option<&Entry>) -> Option<Capabilities> {
    provider.capabilities_at(&dir?.path).await.ok()
}

/// Compares two trees and emits one row per pair.
///
/// `left`/`right` are the two providers and `left_root`/`right_root` the two
/// roots; they need not be the same provider nor the same scheme. `sides`
/// is how THE PAIR of sides pairs — normally
/// `Sides::from_capabilities(left.capabilities(), right.capabilities())`,
/// but ALWAYS computed by the caller and never by this engine (#153): it
/// used to be recomputed here inside, on the first listing, against
/// `Provider::capabilities()` — which takes no path, so the same provider
/// serving two different MOUNTS (a `LocalProvider` for `/home` and for
/// `/mnt/usb`, two real filesystems) answered the SAME thing for both.
/// Moving the computation to whoever knows the two roots does not close
/// that gap by itself — `Provider::capabilities()` still takes no path —
/// but it is the step that does not require touching the `Provider` trait
/// (a per-path query is #164's shape, and wants its own ADR), and where it
/// is computed now is where it can grow without touching this engine
/// again. `cancel` is the Task's token (hard rule 3): as soon as it fires,
/// the stream drops whatever it had pending, emits a
/// [`CompareError::Cancelled`] and ends.
///
/// Mutates nothing and reads no content unless `opts.criteria.hash` asks
/// for it.
///
/// It is SYMMETRIC under equal options: comparing in reverse gives the same
/// rows with the sides and verdicts swapped, and nothing more. `opts` can
/// NAME a side ([`CompareOptions::descend_orphans`](crate::CompareOptions::descend_orphans)),
/// and then swapping the two trees forces swapping it too: descending the
/// left of `(a, b)` is the mirror of descending the right of `(b, a)`, not
/// of descending the left.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the eighth is `excluded`, and grouping (provider, root) into a new type would be a different API for changing nothing else"
)]
pub fn compare<'a>(
    left: &'a dyn Provider,
    left_root: &VPath,
    right: &'a dyn Provider,
    right_root: &VPath,
    opts: CompareOptions,
    sides: Sides,
    excluded: Vec<VPath>,
    cancel: CancellationToken,
) -> CompareStream<'a> {
    // A root that ALREADY falls under the excluded set is not compared:
    // without this, the protected directory's own listing would come out
    // whole in rows (#209).
    if excluded
        .iter()
        .any(|x| is_descendant(x, left_root) || is_descendant(x, right_root))
    {
        return Box::pin(stream::empty());
    }
    let walk = Walk {
        left,
        right,
        opts,
        sides,
        excluded,
        cancel,
        stack: vec![Frame {
            left: Some(synthetic_dir(left_root)),
            right: Some(synthetic_dir(right_root)),
            depth: 0,
        }],
        pending: VecDeque::new(),
        next_id: 0,
        finished: false,
    };
    // `Unfold` is not fusable by itself and `FusedStream` is not an
    // auto-trait that leaks through `Pin<Box<dyn _>>`: without the
    // `.fuse()` here, a caller that polls it once too many —any `select!`
    // with a flush `tick` does— gets a panic AFTER having compared
    // correctly (#175).
    Box::pin(
        stream::unfold(walk, |mut walk| async move {
            let item = walk.step().await?;
            Some((item, walk))
        })
        .fuse(),
    )
}

/// A root's `Entry`, which nobody listed: the walk needs it to be able to
/// name the directory in an error row, and a root has no parent that
/// described it.
fn synthetic_dir(path: &VPath) -> Entry {
    Entry {
        path: path.clone(),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
        attrs: BTreeMap::new(),
    }
}

/// What came out of looking at ONE paired match.
///
/// They are three different things and not two: a decision that gets
/// published, a broken read that is ITS OWN row and ends nothing, and a
/// cancellation that publishes no row at all and ends the stream.
enum PairOutcome {
    /// The cascade decided, with or without the expensive rung.
    Decided(Decision),
    /// The expensive rung could not read a side. Carries the side that
    /// failed, which is the useful half of the error row.
    ReadFailed(Side),
    /// The token fired while hashing.
    Cancelled,
}

/// The same as [`HashFailure`], already knowing which side it came from.
enum PairFailure {
    Read(Side),
    Cancelled,
}

impl PairFailure {
    fn of(failure: HashFailure, side: Side) -> Self {
        match failure {
            HashFailure::Read => Self::Read(side),
            // Cancellation has no side: it is not about a file, it is about
            // the Task.
            HashFailure::Cancelled => Self::Cancelled,
        }
    }
}

/// Why a pair's [`hydrate`] could not complete.
#[derive(Debug)]
enum HydrationFailure {
    /// The `stat` failed. Carries the side that failed and the rung that
    /// asked for it, which are the two useful halves of the error row.
    Stat {
        side: Side,
        /// `Size` or `Mtime`: which rung was left without its data.
        rung: CompareCriterion,
    },
    /// The token fired before a `stat`.
    Cancelled,
}

/// How hydrating ONE pair turned out.
///
/// The failure case carries BOTH entries just like the good one: if the
/// left answered and the right did not, what the left said is true and the
/// error row must carry it — the panel paints that cell, and clearing it
/// would be throwing away an answer that WAS obtained.
enum Hydrated<'e> {
    /// The fields the cascade is going to look at, already filled in.
    Ready(Cow<'e, Entry>, Cow<'e, Entry>),
    /// A `stat` failed: the two entries as they stood, and whose and which
    /// rung's the failure was.
    Failed {
        left: Cow<'e, Entry>,
        right: Cow<'e, Entry>,
        side: Side,
        rung: CompareCriterion,
    },
    /// The token fired before a `stat`.
    Cancelled,
}

/// One side of the pair while it is asked what the listing did not bring.
///
/// The `Cow` is what makes hydration cost nothing when it is not needed: a
/// provider that already filled in the fields —SFTP, object, archive,
/// `MemProvider`— comes out via [`Cow::Borrowed`] without having cloned or
/// asked anything.
struct Fresh<'e> {
    entry: Cow<'e, Entry>,
    /// Its `stat` has ALREADY been spent. Whatever is still missing after
    /// that is really missing, and asking again is a second trip to hear
    /// the same thing.
    asked: bool,
}

impl<'e> Fresh<'e> {
    const fn of(entry: &'e Entry) -> Self {
        Self {
            entry: Cow::Borrowed(entry),
            asked: false,
        }
    }
}

/// Spends a `stat` on `fresh` so rung `rung` gets its data.
///
/// # When it is paid
///
/// Only when all THREE things happen at once: the pair is of files (an
/// absence is decided by presence, a different type by kind, two
/// directories by kind too, and a link by its target — none of them look at
/// size or date), the rung that needs the field is really going to run, and
/// that side does not already bring it. A provider that fills in its
/// listing receives not one extra call; the second rung reuses the first's
/// `stat` (`asked`), so the ceiling is **one `stat` per side and per file
/// pair**.
///
/// The price of that discipline shows in the panel: over a lazy provider, a
/// pair of files shows its size and an ORPHAN does not, because no rung
/// looks at it (<https://github.com/compilando/norte/issues/157>).
///
/// # What it costs, said plainly
///
/// A `stat` is a round trip to the provider, and **they go in SERIES**: one
/// after another, one side after the other, one pair after the previous
/// one, with queue depth one. A tree of N paired files costs up to 2N
/// chained trips. It is the same shape as the copy engine
/// (`norte-core::ops::hydrate_plan`, which stats its plan's leaves for the
/// same #52), and for a local disk that is scheduling, not latency.
///
/// Where it DOES hurt, which is not where it looks like:
///
/// - `file://` **does not mean local disk**. `LocalProvider` serves whatever
///   the OS has mounted, and over SMB, NFS or sshfs every `lstat` is a
///   network trip. Comparing two mounted shares is the most ordinary thing
///   in the world for a file manager, and there 2N trips in series show.
/// - Remote providers fill in their listing **almost always, not always**:
///   `norte-vfs-sftp` pulls `size` from the `readdir` attributes (always
///   present), but `mtime_ms` only if the server sends `ACMODTIME` —OpenSSH
///   sends it; a minimal server or an appliance may not—, and
///   `norte-vfs-object` pulls `mtime_ms` from `last_modified`, which is also
///   optional. Against one of those, a mirrored tree —equal sizes, i.e.
///   every pair reaching the date rung— pays the 2N trips.
///
/// The way out is not for the engine to guess: it is hydrating one
/// directory's pairs with bounded concurrency (the two listings are already
/// whole in memory once they are paired) or having the provider fill in its
/// listing — the measurement is at
/// <https://github.com/compilando/norte/issues/156>, and until it is taken,
/// this is what it costs.
///
/// # A `stat` that fails is NOT an `Unknown`
///
/// It is a [`CompareVerdict::Error`] row with [`CompareReason::Unreadable`]
/// and the side that failed, same as an unreadable listing or a read broken
/// halfway through a hash. `Unknown` means "the provider cannot answer this
/// question" —a tar's linkless target, a kind that is neither file nor
/// directory—, and that answer travels alongside a `Same` verdict.
/// Downgrading here to `Unknown` would say "equal, don't know" about a pair
/// nobody got to look at, and would cover up an `EACCES` the user can fix.
/// The comparison does not invent answers, and "I could not ask" is not
/// "I asked and it is not known".
///
/// **`NotFound` goes the same way, and it is a decision**: a file that
/// disappears between the `list` and the `stat` is a real race (`/tmp`, a
/// build directory). A provider's listing resolves it the other way around
/// — `norte-vfs-local::list_with` omits the entry that vanished, so as not
/// to kill a live directory's listing—, and here it cannot: the pair is
/// already paired, and staying silent about it would remove from the panel
/// a row the other side does have. The error row says "this could not be
/// compared", which is what happened. The wire's reason does not
/// distinguish the causes (same as `list_all` sends every listing failure
/// to `Unreadable`): the vocabulary has ONE word for "could not read", and
/// refining it means changing the wire.
///
/// # What is copied from the `stat`, and what is not
///
/// ONLY `size` and `mtime_ms`. `path` and `kind` stay the listing's: the
/// path already crossed [`is_direct_child`]'s boundary and the kind already
/// decided its rung, so an entry swapped between the `list` and the `stat`
/// cannot smuggle in another type or another path here.
///
/// The residue, worth knowing: a hydrated `Entry` is a COMPOSITE of two
/// observations at two instants —`path`, `kind` and `attrs` from the
/// listing; `size` and `mtime_ms` from the `stat`—. If someone replaced the
/// file in between, the row describes two objects at once. It is inevitable
/// in any lazy design and looking more times does not fix it, but spec 2 is
/// going to READ these rows to decide what to copy, so it is stated here.
async fn hydrate(
    provider: &dyn Provider,
    fresh: &mut Fresh<'_>,
    side: Side,
    rung: CompareCriterion,
    cancel: &CancellationToken,
) -> Result<(), HydrationFailure> {
    if fresh.asked {
        return Ok(());
    }
    // Hard rule 3: this is I/O, and a 40,000-file directory is 40,000 trips
    // cancellation has no reason to wait for.
    if cancel.is_cancelled() {
        return Err(HydrationFailure::Cancelled);
    }
    fresh.asked = true;
    let statted = provider
        .stat(&fresh.entry.path)
        .await
        .map_err(|_| HydrationFailure::Stat { side, rung })?;
    let entry = fresh.entry.to_mut();
    entry.size = statted.size;
    entry.mtime_ms = statted.mtime_ms;
    Ok(())
}

/// A directory pending pairing, with its depth.
///
/// The two sides are `Option` because a frame can be of ONLY ONE side: it is
/// what
/// [`CompareOptions::descend_orphans`](crate::CompareOptions::descend_orphans)
/// pushes when descending into an orphan, where the other side does not
/// exist and its listing is empty. At least one of the two is always
/// `Some` — a frame with no sides names no directory.
struct Frame {
    left: Option<Entry>,
    right: Option<Entry>,
    depth: u32,
}

/// The walk's state between two calls to the stream.
struct Walk<'a> {
    left: &'a dyn Provider,
    right: &'a dyn Provider,
    opts: CompareOptions,
    /// How THE PAIR of sides pairs — decided by [`compare`]'s caller, not by
    /// this struct (#153).
    sides: Sides,
    /// Subtrees this walk does NOT look at: no row, no descent, no `stat`
    /// (#209).
    ///
    /// Exists because the daemon's read gate looks at the two ROOTS and
    /// nothing else: comparing `$HOME` against something else is legitimate
    /// and used to drag the daemon's state directory along with it —
    /// `journal.db`, sync spools, and, with the hash rung on, an equality
    /// oracle over their bytes. It is the half #165 could not close.
    ///
    /// Lives here and not on [`CompareOptions`] because that struct is
    /// `Copy` and this is a list; and not on [`Sides`] for the same reason.
    excluded: Vec<VPath>,
    cancel: CancellationToken,
    /// Explicit stack: depth first with no async recursion.
    stack: Vec<Frame>,
    /// The last paired directory's rows, not yet delivered.
    pending: VecDeque<CompareRow>,
    /// Monotonic counter for [`CompareRow::id`].
    next_id: u64,
    finished: bool,
}

/// `true` if `path` falls under `root` (the root itself counts), byte by
/// byte by segments — the same comparison the daemon's gate does.
fn is_descendant(root: &VPath, path: &VPath) -> bool {
    norte_proto::methods::RelPath::under(root, path).is_some()
}

/// How many hydration `stat`s run AT ONCE (#156). Within the `8..16` the
/// issue asks for: enough to amortize network latency without hammering a
/// spinning disk, and bounded — not "the whole directory at once", which
/// over 400,000 pairs would be the same overhead
/// [`COMPARE_MAX_DIR_ENTRIES`] exists to avoid elsewhere.
const HYDRATE_CONCURRENCY: usize = 12;

/// A row resolved but missing ONLY its `id` — which [`Walk::visit`] assigns
/// in EMISSION order, not in the order its `stat` finished (#156:
/// concurrency is in hydration, not in emission, and the `id` is monotonic
/// with the row, not with when it was computed).
enum PendingRow {
    /// What the cascade decided, or the presence rung (no I/O).
    Decision {
        decision: Decision,
        left: Option<Entry>,
        right: Option<Entry>,
    },
    /// A collision or a read failure: reason and side mandatory, same as
    /// [`flagged`].
    Flagged {
        left: Option<Entry>,
        right: Option<Entry>,
        verdict: CompareVerdict,
        criterion: CompareCriterion,
        reason: CompareReason,
        side: Side,
    },
}

impl PendingRow {
    fn into_row(self, id: u64) -> CompareRow {
        match self {
            Self::Decision {
                decision,
                left,
                right,
            } => decision.into_row(id, left, right),
            Self::Flagged {
                left,
                right,
                verdict,
                criterion,
                reason,
                side,
            } => flagged(id, left, right, verdict, criterion, reason, side),
        }
    }
}

/// One step of [`Walk::merge_join`]'s merge-join, in KEY order.
enum Step {
    /// Already resolved with no I/O: the presence rung, or a collision.
    /// `Box` because `PendingRow` is quite a bit bigger than a `usize` and
    /// this variant is the infrequent one — most of a large directory are
    /// [`Self::Equal`] pairs.
    Ready(Box<PendingRow>),
    /// Pair by key: the index into the `Vec` of pairs [`Walk::merge_join`]
    /// returns alongside the steps, hydrated separately (#156).
    Equal(usize),
}

/// What [`Walk::merge_join`] returns: the steps in key order, the pairs
/// still pending hydration that the [`Step::Equal`]s name (same order as
/// their indices), and the frames to descend into.
type MergeJoinResult<'e> = (Vec<Step>, Vec<(&'e Entry, &'e Entry)>, Vec<Frame>);

impl Walk<'_> {
    /// One step of the stream: delivers the next row, pairing directories
    /// as long as it has none at hand.
    async fn step(&mut self) -> Option<Result<CompareRow, CompareError>> {
        loop {
            // Cancellation BEFORE delivering anything (hard rule 3): no row
            // comes out after the cut, not even one already computed.
            if self.cancel.is_cancelled() {
                if self.finished {
                    return None;
                }
                self.finished = true;
                self.pending.clear();
                self.stack.clear();
                return Some(Err(CompareError::Cancelled));
            }
            if let Some(row) = self.pending.pop_front() {
                return Some(Ok(row));
            }
            if self.finished {
                return None;
            }
            let Some(frame) = self.stack.pop() else {
                self.finished = true;
                return None;
            };
            self.visit(frame).await;
        }
    }

    /// Pairs ONE pair of directories: fills [`Walk::pending`] with its rows
    /// and pushes the common subdirectories.
    ///
    /// Also handles the ONE-side-only frame —descending into an orphan—:
    /// the missing side contributes the empty listing and everything else
    /// is the same path, orphan rows included.
    /// [`list_side`] minus what is EXCLUDED (#209): an entry under a
    /// protected subtree falls out here, before pairing, so it produces no
    /// row, no descent, no hydration `stat`.
    ///
    /// It is filtered on the listing and not on the descent because a
    /// protected subtree must not even be NAMED: a row that said
    /// "only on the left: journal.db" already tells what the gate wanted
    /// kept quiet.
    async fn list_visible(
        &self,
        provider: &dyn Provider,
        dir: Option<&Entry>,
    ) -> Result<Vec<Entry>, ListFailure> {
        let mut entries = list_side(provider, dir, &self.cancel).await?;
        if !self.excluded.is_empty() {
            entries.retain(|e| !self.excluded.iter().any(|x| is_descendant(x, &e.path)));
        }
        Ok(entries)
    }

    /// How THIS pair of directories pairs.
    ///
    /// Comes from each side's [`Provider::capabilities_at`] (ADR 0054) and
    /// falls back to what the caller decided —the ROOT's rules— when a side
    /// is not there (descending into an orphan) or when the probe fails.
    /// Falling back to the root's rules and not to "fold nothing" matters:
    /// the latter turns a directory that could not answer into one that
    /// reports no case collisions, which is the more expensive of the two
    /// lies.
    async fn sides_for(&self, frame: &Frame) -> Sides {
        let (left, right) = futures::join!(
            capabilities_of(self.left, frame.left.as_ref()),
            capabilities_of(self.right, frame.right.as_ref()),
        );
        match (left, right) {
            (Some(left), Some(right)) => Sides::from_capabilities(left, right),
            _ => self.sides,
        }
    }

    async fn visit(&mut self, frame: Frame) {
        debug_assert!(
            frame.left.is_some() || frame.right.is_some(),
            "a frame with no side at all names no directory"
        );
        // BOTH sides are always listed, even if the first already failed:
        // two broken directories are two facts, and turning back at the
        // first would leave the second undiscovered forever.
        //
        // A MISSING side —descending into an orphan— is not listed: its
        // listing is the empty one, and from there comes one orphan row per
        // entry on the side that IS there, through the same path as
        // everything else.
        let listed = (
            self.list_visible(self.left, frame.left.as_ref()).await,
            self.list_visible(self.right, frame.right.as_ref()).await,
        );
        if matches!(listed.0, Err(ListFailure::Cancelled))
            || matches!(listed.1, Err(ListFailure::Cancelled))
        {
            return;
        }
        if let Err(ListFailure::Reason(reason)) = listed.0 {
            self.push_error(frame.left.clone(), None, reason, Side::Left);
        }
        if let Err(ListFailure::Reason(reason)) = listed.1 {
            self.push_error(None, frame.right.clone(), reason, Side::Right);
        }
        // A listing that failed leaves what it contained unknown, so the
        // other part cannot be paired either: saying `OnlyRight` about its
        // entries would be asserting an absence nobody has checked.
        let (Ok(lefts), Ok(rights)) = listed else {
            return;
        };

        // The pairing rules are THIS directory's, not the root's (#215).
        // Under one root there are mounts: an exFAT stick hung off
        // `/data/backup`, an ext4 `+F` subtree, a bind. Comparing its
        // entries with the root's rules is #153 one level down — the same
        // silent loss of collisions, under another name.
        //
        // It does not cost what it looks like: `Provider::capabilities_at`
        // answers the declaration with NO I/O for any backend whose
        // locations are equal (the trait's default), and
        // `norte-vfs-local`, which does really probe, caches by directory
        // identity. A remote does not pay a trip per directory, which
        // would be #156 all over again.
        let sides = self.sides_for(&frame).await;
        let left_index = index_side(&lefts, sides);
        let right_index = index_side(&rights, sides);
        let left_collided = collided_keys(&left_index, sides);
        let right_collided = collided_keys(&right_index, sides);

        // The collisions: ONE row per involved entry, never merged and
        // never deduplicated (`CompareVerdict::Ambiguous`'s normative
        // contract).
        for (entry, reason) in left_index.collisions() {
            self.push_ambiguous(Some(entry.clone()), None, reason, Side::Left);
        }
        for (entry, reason) in right_index.collisions() {
            self.push_ambiguous(None, Some(entry.clone()), reason, Side::Right);
        }

        // STEP 1 — the merge-join, synchronous: decides the row ORDER and
        // which pairs are left pending hydration (#156).
        let depth = frame.depth.saturating_add(1);
        let Some((steps, pairs, descend)) = self.merge_join(
            &left_index,
            &right_index,
            &left_collided,
            &right_collided,
            frame.depth,
            depth,
        ) else {
            return;
        };

        // STEP 2 — hydrates ALL of this directory's pairs at once, bounded
        // (#156): before, each `stat` waited for the previous one, up to
        // 2N chained trips over a network mount. `Walk::pair_outcome` takes
        // `&self` — not `&mut self` — precisely so many copies can run at
        // once; assigning the `id` and publishing the row is step 3's job,
        // sequential, so the COUNTER stays monotonic with emission order
        // and not with when each `stat` finished.
        let mut resolved: Vec<Option<PendingRow>> = Vec::with_capacity(pairs.len());
        resolved.resize_with(pairs.len(), || None);
        if !pairs.is_empty() {
            let this: &Self = self;
            // A `Vec` of futures built up front, not `Iterator::map` with
            // an async closure: `map`'s closure needs ONE type that works
            // for any invocation (`FnMut`), and there rustc does not infer
            // `pairs`'s borrowed lifetime — "implementation of `FnOnce` is
            // not general enough". An ordinary loop, instead, instantiates
            // each future with ITS OWN concrete lifetime without asking the
            // closure for anything generic.
            let futures: Vec<_> = pairs
                .iter()
                .copied()
                .enumerate()
                .map(|(i, (l, r))| async move { (i, this.pair_outcome(l, r).await) })
                .collect();
            let mut hydrating = stream::iter(futures).buffer_unordered(HYDRATE_CONCURRENCY);
            while let Some((i, outcome)) = hydrating.next().await {
                resolved[i] = outcome;
            }
        }
        // Cancelled halfway through hydration: `pair_outcome` only returns
        // `None` for that reason (hard rule 3, checked by `stat` inside
        // `hydrate` — see its rustdoc). Same as before: nothing from this
        // directory is published, the stream's step takes care of ending.
        if self.cancel.is_cancelled() {
            return;
        }

        // STEP 3 — emits, in the order decided by step 1, with the `id`
        // assigned HERE and not before.
        for step in steps {
            let row = match step {
                Step::Ready(row) => *row,
                Step::Equal(i) => resolved[i].take().expect(
                    "every index in `pairs` received its result from the stream above, and \
                     `pair_outcome` only returns `None` on cancellation — already checked",
                ),
            };
            let id = self.next_id();
            self.pending.push_back(row.into_row(id));
        }

        // In reverse: the stack is LIFO, so pushing in reverse key order is
        // what makes them come out in key order.
        for pending in descend.into_iter().rev() {
            self.stack.push(pending);
        }
    }

    /// A directory's merge-join: decides the rows' final ORDER (extracted
    /// from [`Walk::visit`] — #156, so that function fits the gate's line
    /// limit). Synchronous, not a single `await`: none of this needs I/O,
    /// not even for an `Equal` pair, whose `stat` —if one is needed— is
    /// [`Walk::pair_outcome`]'s job afterward.
    ///
    /// Returns `None` if the token fired halfway through the walk —case in
    /// which `Walk::visit` publishes nothing from this directory, same as
    /// before #156—; otherwise, the steps in key order, the pairs left
    /// pending hydration (in the same order the `Step::Equal`s name them)
    /// and the frames to descend into (still in key order, not reversed —
    /// reversing them for the LIFO stack is the caller's job).
    #[must_use = "None means cancelled: the caller has to drop the whole directory"]
    fn merge_join<'e>(
        &self,
        left_index: &SideIndex<'e, Entry>,
        right_index: &SideIndex<'e, Entry>,
        left_collided: &BTreeMap<Vec<u8>, CompareReason>,
        right_collided: &BTreeMap<Vec<u8>, CompareReason>,
        parent_depth: u32,
        child_depth: u32,
    ) -> Option<MergeJoinResult<'e>> {
        let mut steps: Vec<Step> = Vec::new();
        let mut pairs: Vec<(&'e Entry, &'e Entry)> = Vec::new();
        let mut descend: Vec<Frame> = Vec::new();
        let mut lefts_iter = left_index.unique().peekable();
        let mut rights_iter = right_index.unique().peekable();
        loop {
            // Per PAIR, and not just per directory: two listings at the cap
            // hold 400,000 pairs, and this step does not have a single
            // `await`, so without this cancellation would wait for the
            // whole directory to finish (hard rule 3).
            //
            // No test can see this from outside —`step` already drops the
            // whole `pending` on cancellation, so the OUTPUT is the same
            // with or without this check—: what changes is how long it
            // takes to arrive, and 400,000 pairs of thrown-away work. It is
            // not an invariant without a test, it is a latency invariant.
            if self.cancel.is_cancelled() {
                return None;
            }
            let order = match (lefts_iter.peek(), rights_iter.peek()) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some((lk, _)), Some((rk, _))) => lk.cmp(rk),
            };
            match order {
                Ordering::Less => {
                    // `expect`: `peek` just returned `Some` on this very
                    // iterator and nobody touched it in between, so `next`
                    // cannot be `None` (hard rule 6).
                    let (key, entry) = lefts_iter.next().expect("peek said there was one");
                    let collided = right_collided.get(key.as_bytes()).copied();
                    let (row, can_descend) =
                        Self::side_outcome(Some(entry.clone()), None, collided, Side::Right);
                    steps.push(Step::Ready(Box::new(row)));
                    if can_descend {
                        descend.extend(self.orphan_frame(entry, Side::Left, parent_depth));
                    }
                }
                Ordering::Greater => {
                    let (key, entry) = rights_iter.next().expect("peek said there was one");
                    let collided = left_collided.get(key.as_bytes()).copied();
                    let (row, can_descend) =
                        Self::side_outcome(None, Some(entry.clone()), collided, Side::Left);
                    steps.push(Step::Ready(Box::new(row)));
                    if can_descend {
                        descend.extend(self.orphan_frame(entry, Side::Right, parent_depth));
                    }
                }
                Ordering::Equal => {
                    let (_, left_entry) = lefts_iter.next().expect("peek said there was one");
                    let (_, right_entry) = rights_iter.next().expect("peek said there was one");
                    // Descending into a directory pair is decided by the
                    // KIND, which is already known — never `stat`, so there
                    // is no need to wait for the hydration afterward to
                    // know it (two directories are NEVER hydrated: see
                    // `hydrate_rungs`).
                    if left_entry.kind == EntryKind::Dir
                        && right_entry.kind == EntryKind::Dir
                        && self.descends_below(parent_depth)
                    {
                        descend.push(Frame {
                            left: Some(left_entry.clone()),
                            right: Some(right_entry.clone()),
                            depth: child_depth,
                        });
                    }
                    steps.push(Step::Equal(pairs.len()));
                    pairs.push((left_entry, right_entry));
                }
            }
        }
        Some((steps, pairs, descend))
    }

    /// An entry that appears on only one side. If the key it would have
    /// matched on the OTHER side is collided, the row is NOT
    /// `OnlyLeft`/`OnlyRight`: it is `Ambiguous`.
    ///
    /// The reason is what a synchronization plan would do with each one.
    /// `OnlyRight` tells it "copy it to the other side", and copying into a
    /// directory that can no longer tell those two names apart creates a
    /// THIRD colliding file. `Ambiguous` makes that plan refuse to act,
    /// which is the only safe answer while nobody has undone the collision.
    ///
    /// Publishes nothing — unlike the version before #156, which pushed
    /// straight to `self.pending` — because `Walk::visit` decides the `id`
    /// in step 3, after hydrating. Returns the resolved row and whether it
    /// can be descended into.
    #[must_use = "the value says whether this row can be descended into"]
    fn side_outcome(
        left: Option<Entry>,
        right: Option<Entry>,
        collided_with: Option<CompareReason>,
        collision_side: Side,
    ) -> (PendingRow, bool) {
        if let Some(reason) = collided_with {
            let row = PendingRow::Flagged {
                left,
                right,
                verdict: CompareVerdict::Ambiguous,
                criterion: CompareCriterion::Presence,
                reason,
                side: collision_side,
            };
            return (row, false);
        }
        let decision = if left.is_some() {
            Decision::only_left()
        } else {
            Decision::only_right()
        };
        (
            PendingRow::Decision {
                decision,
                left,
                right,
            },
            true,
        )
    }

    /// The pair ALREADY HYDRATED (or its failure), with no `id`: that is
    /// decided by `Walk::visit` in step 3, sequentially (#156). `&self`,
    /// not `&mut self`, so many copies can run AT ONCE via
    /// `buffer_unordered` — neither `next_id` nor `pending` are touched
    /// here.
    ///
    /// `None` means cancelled: the pair produces no row, and `visit` drops
    /// the whole directory, same as before #156.
    async fn pair_outcome(&self, left: &Entry, right: &Entry) -> Option<PendingRow> {
        // What the listing did not bring and the cascade is going to need,
        // asked BEFORE deciding. The rows carry the hydrated entries: a row
        // that says "different by size" about two empty sizes cannot be
        // read.
        let (left, right) = match self.hydrated_pair(left, right).await {
            Hydrated::Ready(left, right) => (left, right),
            Hydrated::Cancelled => return None,
            Hydrated::Failed {
                left,
                right,
                side,
                rung,
            } => {
                return Some(PendingRow::Flagged {
                    // What was learned travels: if the left answered and
                    // the right did not, its size is true and the panel's
                    // cell shows it.
                    left: Some(left.into_owned()),
                    right: Some(right.into_owned()),
                    verdict: CompareVerdict::Error,
                    // The rung that was left without its data, same as a
                    // broken read says `Hash`: that rung ran and died.
                    criterion: rung,
                    reason: CompareReason::Unreadable,
                    side,
                });
            }
        };
        let (left, right) = (left.as_ref(), right.as_ref());
        match self.verdict_for_pair(left, right).await {
            PairOutcome::Cancelled => None,
            PairOutcome::Decided(decision) => Some(PendingRow::Decision {
                decision,
                left: Some(left.clone()),
                right: Some(right.clone()),
            }),
            // A broken read costs ITS OWN row and the walk continues, same
            // as an unreadable listing. The row carries both sides: the
            // pair DID pair, what failed was verifying it.
            PairOutcome::ReadFailed(side) => Some(PendingRow::Flagged {
                left: Some(left.clone()),
                right: Some(right.clone()),
                verdict: CompareVerdict::Error,
                // `Hash` and not `Presence`: the rung RAN and died.
                // `Presence`'s convention is for rows where none ran.
                criterion: CompareCriterion::Hash,
                reason: CompareReason::ReadFailed,
                side,
            }),
        }
    }

    /// The pair with the fields the cascade is going to look at already
    /// filled in.
    ///
    /// Follows the SAME order as `cascade::size_and_mtime`, and for the
    /// same reason the hash rung only reaches what the cheap ones called
    /// equal: what is already decided is not paid for. Two files with
    /// different sizes do not spend the date rung's `stat`, and a rung
    /// that is off spends nothing.
    ///
    /// Returning [`Cow`] is the cheap path: with no missing field, not a
    /// single `Entry` is cloned.
    async fn hydrated_pair<'e>(&self, left: &'e Entry, right: &'e Entry) -> Hydrated<'e> {
        let mut l = Fresh::of(left);
        let mut r = Fresh::of(right);
        match self.hydrate_rungs(&mut l, &mut r).await {
            Ok(()) => Hydrated::Ready(l.entry, r.entry),
            Err(HydrationFailure::Cancelled) => Hydrated::Cancelled,
            // Both entries travel equally: the one that DID answer carries
            // its data, and the error row shows it.
            Err(HydrationFailure::Stat { side, rung }) => Hydrated::Failed {
                left: l.entry,
                right: r.entry,
                side,
                rung,
            },
        }
    }

    /// The two rungs, in order, over the two sides. Separated from
    /// [`Walk::hydrated_pair`] only so `?` can be used without losing what
    /// was already hydrated when something fails.
    async fn hydrate_rungs(
        &self,
        l: &mut Fresh<'_>,
        r: &mut Fresh<'_>,
    ) -> Result<(), HydrationFailure> {
        // Only FILE pairs. An orphan is decided by presence and does not
        // even reach here; a different kind is decided by kind; two
        // directories too (C3: a directory's date moves with any child, so
        // they are compared neither by date nor by size); and a link, by
        // its target. Statting any of them is a trip to the provider for
        // nothing in return.
        if l.entry.kind == EntryKind::File && r.entry.kind == EntryKind::File {
            if self.opts.criteria.size {
                if l.entry.size.is_none() {
                    hydrate(
                        self.left,
                        l,
                        Side::Left,
                        CompareCriterion::Size,
                        &self.cancel,
                    )
                    .await?;
                }
                if r.entry.size.is_none() {
                    hydrate(
                        self.right,
                        r,
                        Side::Right,
                        CompareCriterion::Size,
                        &self.cancel,
                    )
                    .await?;
                }
                // The size rung decides —different, or one still unknown
                // after asking— and the cascade does not go down to the
                // date one: hydrating it would be paying for a rung that
                // is not going to run.
                match (l.entry.size, r.entry.size) {
                    (Some(a), Some(b)) if a == b => {}
                    _ => return Ok(()),
                }
            }
            if self.opts.criteria.mtime {
                if l.entry.mtime_ms.is_none() {
                    hydrate(
                        self.left,
                        l,
                        Side::Left,
                        CompareCriterion::Mtime,
                        &self.cancel,
                    )
                    .await?;
                }
                if r.entry.mtime_ms.is_none() {
                    hydrate(
                        self.right,
                        r,
                        Side::Right,
                        CompareCriterion::Mtime,
                        &self.cancel,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// ONE paired pair's decision, with what requires I/O already found
    /// out.
    async fn verdict_for_pair(&self, left: &Entry, right: &Entry) -> PairOutcome {
        // `read_link` ONLY when both sides are links: if one is not, the
        // kind rung already decided and reading the other's target is a
        // call to the provider for nothing in return.
        let (left_target, right_target) =
            if left.kind == EntryKind::Symlink && right.kind == EntryKind::Symlink {
                (
                    self.left.read_link(&left.path).await.ok(),
                    self.right.read_link(&right.path).await.ok(),
                )
            } else {
                (None, None)
            };
        let facts = Prefetched::links(left_target.as_deref(), right_target.as_deref());
        let decision = decide(left, right, &self.opts, &facts);

        // `needs_hash == true` means exactly this: the cheap rungs called
        // the pair EQUAL and the caller asked for hash, so the decision is
        // NOT final. Publishing it here would be a provisional verdict,
        // and in this spec no row is corrected afterward.
        if !decision.needs_hash {
            return PairOutcome::Decided(decision);
        }
        let outcome = match self.hash_pair(left, right).await {
            Ok(outcome) => outcome,
            Err(PairFailure::Cancelled) => return PairOutcome::Cancelled,
            Err(PairFailure::Read(side)) => return PairOutcome::ReadFailed(side),
        };
        let decided = decide(left, right, &self.opts, &facts.with_hash(outcome));
        debug_assert!(
            !decided.needs_hash,
            "the hash rung answered and the cascade asked for it again"
        );
        PairOutcome::Decided(decided)
    }

    /// The expensive rung over ONE pair: the two sha256s, and what they
    /// say.
    ///
    /// The sides go in order and not in parallel. Reading both at once
    /// doubles bandwidth and live memory to advance at most half the time,
    /// and above all makes a failure of the first arrive with the second
    /// file already half read. If the left cannot be read, the right is
    /// not opened: the row is already an error and reading it whole would
    /// not change a single letter of it.
    async fn hash_pair(&self, left: &Entry, right: &Entry) -> Result<HashOutcome, PairFailure> {
        let left_digest = sha256_of(self.left, &left.path, &self.cancel)
            .await
            .map_err(|failure| PairFailure::of(failure, Side::Left))?;
        let right_digest = sha256_of(self.right, &right.path, &self.cancel)
            .await
            .map_err(|failure| PairFailure::of(failure, Side::Right))?;
        Ok(if left_digest == right_digest {
            HashOutcome::Equal
        } else {
            HashOutcome::Differ
        })
    }

    /// The frame that ENUMERATES an orphan, if it has to be enumerated.
    ///
    /// `side` is the side the entry is on, and the frame that comes out
    /// carries the opposite one as `None`: there is nothing to list there,
    /// and that empty list is exactly what makes the merge-join emit one
    /// orphan row per child.
    fn orphan_frame(&self, entry: &Entry, side: Side, parent_depth: u32) -> Option<Frame> {
        if !self.descends_into_orphan(entry, side, parent_depth) {
            return None;
        }
        let entry = Some(entry.clone());
        let depth = parent_depth.saturating_add(1);
        match side {
            Side::Left => Some(Frame {
                left: entry,
                right: None,
                depth,
            }),
            Side::Right => Some(Frame {
                left: None,
                right: entry,
                depth,
            }),
            // Never reached —`descends_into_orphan` already answered no—,
            // and even so it is not attributed to a side: "no side" is not
            // the right, and a refactor that softened that guard must not
            // find a hand-written descent here on the wrong side.
            Side::Unknown => None,
        }
    }

    /// Does orphan `entry`, which is only on `side`, need to be descended
    /// into?
    ///
    /// Three conditions, all three necessary: it is a directory, the
    /// caller asked to descend ON THAT side, and `max_depth` allows it —
    /// what is bounded is the number of listings, whether it comes from a
    /// pair or from an orphan.
    ///
    /// `Some(Side::Unknown)` is no side at all and therefore descends
    /// nothing: see [`CompareOptions::descend_orphans`](crate::CompareOptions::descend_orphans).
    fn descends_into_orphan(&self, entry: &Entry, side: Side, depth: u32) -> bool {
        entry.kind == EntryKind::Dir
            && self.opts.descend_orphans == Some(side)
            && self.descends_below(depth)
    }

    /// Can one more level be descended from `depth`?
    fn descends_below(&self, depth: u32) -> bool {
        self.opts
            .max_depth
            .is_none_or(|max| depth.saturating_add(1) <= max)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn push_ambiguous(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Ambiguous,
            CompareCriterion::Presence,
            reason,
            side,
        ));
    }

    fn push_error(
        &mut self,
        left: Option<Entry>,
        right: Option<Entry>,
        reason: CompareReason,
        side: Side,
    ) {
        let id = self.next_id();
        self.pending.push_back(flagged(
            id,
            left,
            right,
            CompareVerdict::Error,
            CompareCriterion::Presence,
            reason,
            side,
        ));
    }
}

/// The two rows the cascade does not produce: `Ambiguous` and `Error`. Both
/// carry reason and side mandatory.
///
/// The caller sets `criterion` because there are two cases and not one: a
/// row for a read broken halfway through a hash says `Hash` —that rung RAN
/// and died—, and the others say `Presence`, which is the wire's convention
/// for "no criterion reports here". Confidence is `Unknown` in both: nothing
/// was left compared.
fn flagged(
    id: u64,
    left: Option<Entry>,
    right: Option<Entry>,
    verdict: CompareVerdict,
    criterion: CompareCriterion,
    reason: CompareReason,
    side: Side,
) -> CompareRow {
    // An `Ambiguous` is of ONE side and an `Error` may have no entry to
    // show, but an `Error` CAN carry both —a directory that paired and
    // would not be listed—, and then its two names paired like any other
    // row's. The field means the same in all of them or it means nothing,
    // so it is answered with the same rule as in `Decision::into_row`
    // (#152).
    let paired_under = match (left.as_ref(), right.as_ref()) {
        (Some(l), Some(r)) => crate::key::pair_transform(l.pair_name(), r.pair_name()),
        _ => None,
    };
    let row = CompareRow {
        id,
        left,
        right,
        verdict,
        criterion,
        confidence: CompareConfidence::Unknown,
        newer: None,
        reason: Some(reason),
        side: Some(side),
        paired_under,
    };
    debug_assert!(row.reason_is_consistent(), "row {verdict:?} with no reason");
    debug_assert!(
        row.sides_are_consistent(),
        "row {verdict:?} with broken sides"
    );
    row
}

/// Why a directory could not be paired.
enum ListFailure {
    /// The reason that travels in the row.
    Reason(CompareReason),
    /// Cancelled halfway through draining: there is no row, there is the
    /// end of the stream.
    Cancelled,
}

/// One side of a [`Frame`]'s listing: the directory's when that side is
/// there, and the EMPTY one when it is not.
///
/// A missing side is not "an empty directory" from the provider —that would
/// be an assertion about the filesystem— but "there is nothing to pair on
/// this side". [`Walk::visit`]'s merge-join turns that empty list into one
/// orphan row per entry on the side that IS there, which is exactly what
/// has to be emitted when descending into an orphan, and through the same
/// path: the same [`COMPARE_MAX_DIR_ENTRIES`] ceiling and the same
/// cancellation.
async fn list_side(
    provider: &dyn Provider,
    dir: Option<&Entry>,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, ListFailure> {
    match dir {
        Some(entry) => list_all(provider, &entry.path, cancel).await,
        None => Ok(Vec::new()),
    }
}

/// Drains a WHOLE directory's listing, with a ceiling.
///
/// The ceiling is checked BEFORE putting the entry in, so a directory with
/// exactly [`COMPARE_MAX_DIR_ENTRIES`] entries pairs and one with one more
/// is rejected without having materialized the extra one: the limit is also
/// the memory ceiling, not just the answer's.
///
/// The token is checked inside the loop: a directory with hundreds of
/// thousands of entries cannot make cancellation wait until it finishes
/// draining.
async fn list_all(
    provider: &dyn Provider,
    dir: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, ListFailure> {
    let mut stream = provider
        .list(dir)
        .await
        .map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            return Err(ListFailure::Cancelled);
        }
        // An error halfway through listing leaves the directory HALF done,
        // and a half listing paired would produce `OnlyLeft` for entries
        // that WERE there: it counts as unreadable, whole.
        let entry = item.map_err(|_| ListFailure::Reason(CompareReason::Unreadable))?;
        // HARD BOUNDARY (defense in depth, security T4 — the same
        // criterion as `norte-core::search::run_walk`): it is NOT trusted
        // that `list` returns only DIRECT children of `dir`. A provider
        // with a bug —or a third-party plugin's— that lists a path from
        // outside would make the comparison pair it, name it in a row and,
        // with the hash rung, READ its content; and the daemon's gate only
        // checks the two ROOTS, so a smuggled-in path would skip the whole
        // scope.
        //
        // The whole listing counts as unreadable, the entry is not
        // skipped: the same reason as the error halfway through listing
        // above — a listing missing an entry produces `OnlyLeft` for the
        // opposite side, i.e. a WRONG answer instead of one that declares
        // itself.
        //
        // No trace: this crate does not depend on `tracing` (it is a pure
        // function of two providers) and is not going to for one warning.
        // The signal is the error row, which does reach the user.
        if !is_direct_child(dir, &entry.path) {
            return Err(ListFailure::Reason(CompareReason::Unreadable));
        }
        if out.len() >= COMPARE_MAX_DIR_ENTRIES {
            return Err(ListFailure::Reason(CompareReason::DirTooLarge));
        }
        out.push(entry);
    }
    Ok(out)
}

/// `true` if `path` is a DIRECT child of `dir`: same scheme and same
/// authority, and its segments are `dir`'s plus exactly one.
///
/// Byte-exact (hard rule 1): compares raw segments, never the wire form
/// —which would confuse `a` with `ab`— nor a string.
///
/// It is stricter than "falls under `dir`" on purpose: what a `list` can
/// legitimately return is its children, and a grandchild in the list is
/// already a provider not answering the question it was asked.
fn is_direct_child(dir: &VPath, path: &VPath) -> bool {
    if dir.scheme() != path.scheme() || dir.authority() != path.authority() {
        return false;
    }
    let mut d = dir.segments();
    let mut p = path.segments();
    loop {
        match (d.next(), p.next()) {
            // `dir` ran out: exactly one segment is left to consume.
            (None, Some(_)) => return p.next().is_none(),
            (Some(ds), Some(ps)) if ds == ps => {}
            _ => return false,
        }
    }
}

/// One side's COLLIDED keys, with their collision's reason.
///
/// Only walks entries that already collide (almost always none), not the
/// whole listing. The reason kept is the group's first entry's in listing
/// order: a group can have different causes per pair, and the opposite
/// side's row needs ONE.
fn collided_keys(index: &SideIndex<'_, Entry>, sides: Sides) -> BTreeMap<Vec<u8>, CompareReason> {
    let mut out = BTreeMap::new();
    for (entry, reason) in index.collisions() {
        out.entry(key_for(entry.pair_name(), sides).as_bytes().to_vec())
            .or_insert(reason);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::{MemProvider, TarSmith};
    use norte_vfs_archive::{ArchiveProvider, Format};
    use norte_vfs_local::LocalProvider;

    use super::*;
    use crate::PairTransform;

    // ---------- tree utilities ----------

    /// Splits `"sub/deep/c.txt"` into raw segments. Tests speak `&str` for
    /// convenience; what travels to the provider is BYTES (hard rule 1).
    fn segments(path: &str) -> Vec<Segment> {
        path.split('/')
            .map(|s| Segment::new(s.as_bytes().to_vec()).expect("valid segment"))
            .collect()
    }

    /// Creates `path` with `content`, materializing its intermediate
    /// directories.
    async fn seed(mem: &MemProvider, path: &str, content: &[u8]) {
        let segs = segments(path);
        let (name, dirs) = segs.split_last().expect("non-empty path");
        let mut at = MemProvider::root();
        for dir in dirs {
            at = at.join(dir.clone());
            // Already exists: several seeded paths share the tree.
            let _ = mem.mkdir(&at).await;
        }
        let file = at.join(name.clone());
        let mut sink = mem.write(&file).await.expect("write");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    /// A tree with those files; each one's content is its own path, so two
    /// trees with the same list come out identical byte for byte.
    async fn tree(paths: &[&str]) -> MemProvider {
        let mem = MemProvider::new();
        for path in paths {
            seed(&mem, path, path.as_bytes()).await;
        }
        mem
    }

    /// Two identical trees. `MemProvider`'s mtimes are a LOGICAL clock (one
    /// unit per mutation), so seeding the same list in the same order gives
    /// the same dates: nothing in this test depends on the wall clock.
    async fn twin_trees(paths: &[&str]) -> (MemProvider, MemProvider) {
        (tree(paths).await, tree(paths).await)
    }

    /// A `wide/` with `n` entries on the LEFT and empty on the right. Wide
    /// on a single side on purpose: the ceiling is checked per side, and
    /// seeding double only doubles how long the test takes.
    async fn twin_trees_with_wide_dir(n: usize) -> (MemProvider, MemProvider) {
        let left = MemProvider::new();
        let right = MemProvider::new();
        let wide = MemProvider::root().join(Segment::new(b"wide".to_vec()).expect("seg"));
        left.mkdir(&wide).await.expect("mkdir");
        right.mkdir(&wide).await.expect("mkdir");
        for i in 0..n {
            let name = Segment::new(format!("e{i:07}").into_bytes()).expect("seg");
            left.mkdir(&wide.join(name)).await.expect("mkdir");
        }
        (left, right)
    }

    /// Trees with one row of each cheap category.
    async fn trees_that_differ() -> (MemProvider, MemProvider) {
        let left = tree(&["equal.txt", "left-only.txt", "sub/inside.txt"]).await;
        let right = tree(&["equal.txt", "right-only.txt", "sub/inside.txt"]).await;
        seed(&left, "size.txt", b"aaaa").await;
        seed(&right, "size.txt", b"aaaaaaaaaaaa").await;
        (left, right)
    }

    /// `path`'s `VPath` inside a [`MemProvider`].
    fn at(path: &str) -> VPath {
        let mut out = MemProvider::root();
        for seg in segments(path) {
            out = out.join(seg);
        }
        out
    }

    /// `dir`'s `list` fails with I/O; the rest of the tree lists normally.
    fn deny_list(mem: &MemProvider, dir: &str) {
        mem.faults().fail_list_at(&at(dir));
    }

    /// Two trees with ONE file with the same name and different contents.
    ///
    /// Seeding both with the same sequence of mutations gives them the SAME
    /// date (`MemProvider`'s mtime is a logical clock), so with contents of
    /// the same size the cheap rungs cannot tell them apart: it is exactly
    /// the pair the hash rung exists to catch.
    async fn pair_with_content(
        name: &str,
        left: &[u8],
        right: &[u8],
    ) -> (MemProvider, MemProvider) {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, name, left).await;
        seed(&r, name, right).await;
        (l, r)
    }

    // ---------- row utilities ----------

    fn compare_with<'a>(
        left: &'a MemProvider,
        right: &'a MemProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        let sides = Sides::from_capabilities(left.capabilities(), right.capabilities());
        compare(
            left,
            &MemProvider::root(),
            right,
            &MemProvider::root(),
            opts,
            sides,
            Vec::new(),
            CancellationToken::new(),
        )
    }

    fn compare_default<'a>(left: &'a MemProvider, right: &'a MemProvider) -> CompareStream<'a> {
        compare_with(left, right, CompareOptions::cheap())
    }

    /// A provider whose locations are NOT all equal: the root is
    /// case-sensitive and a subdirectory is not, which is what happens when
    /// a mount hangs off there (an exFAT stick at `/data/backup`, an ext4
    /// in `+F`).
    struct MountAware {
        inner: MemProvider,
        /// The name of the directory that is NOT case-sensitive.
        folds: &'static [u8],
    }

    #[async_trait::async_trait]
    impl Provider for MountAware {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, norte_proto::Error> {
            let mut caps = self.inner.capabilities();
            let inside = p.segments().any(|seg| seg == self.folds);
            caps.flags
                .set(norte_proto::CapabilityFlags::CASE_SENSITIVE, !inside);
            Ok(caps)
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            self.inner.list(p).await
        }
        async fn read(
            &self,
            p: &VPath,
            r: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, r).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(a, b).await
        }
    }

    /// #215: pairing rules are the DIRECTORY's, not the root's.
    ///
    /// Under a case-sensitive root there can be a mount that is not, and
    /// comparing its entries with the root's rules silently loses its
    /// collisions — which is #153 one level down.
    #[tokio::test]
    async fn the_rules_come_from_the_directory_and_not_the_root() {
        let left = MountAware {
            inner: tree(&["backup/A.txt", "backup/a.txt"]).await,
            folds: b"backup",
        };
        let right = MountAware {
            inner: tree(&["backup/A.txt"]).await,
            folds: b"backup",
        };
        // The ROOT is case-sensitive on both sides: with the root's rules,
        // `A.txt` and `a.txt` are two different names and nothing
        // collides.
        let root_sides = Sides::from_capabilities(
            left.capabilities_at(&MemProvider::root())
                .await
                .expect("root"),
            right
                .capabilities_at(&MemProvider::root())
                .await
                .expect("root"),
        );
        assert!(
            !root_sides.folds_case(),
            "this mount's root is case-sensitive"
        );

        let rows = collect(compare(
            &left,
            &MemProvider::root(),
            &right,
            &MemProvider::root(),
            CompareOptions::cheap(),
            root_sides,
            Vec::new(),
            CancellationToken::new(),
        ))
        .await;

        assert!(
            rows.iter().any(|f| f.verdict == CompareVerdict::Ambiguous),
            "the folding subdirectory has to report the collision: {:?}",
            rows.iter()
                .map(|f| (
                    f.verdict,
                    f.left
                        .as_ref()
                        .or(f.right.as_ref())
                        .map(|e| e.path.display_lossy())
                ))
                .collect::<Vec<_>>()
        );
    }

    async fn collect(stream: CompareStream<'_>) -> Vec<CompareRow> {
        stream
            .map(|item| item.expect("none of these comparisons cancel"))
            .collect()
            .await
    }

    /// Is either of the row's two sides named this? By BYTES: `VPath` has no
    /// `as_bytes` because a name is not text (hard rule 1).
    fn named(row: &CompareRow, name: &[u8]) -> bool {
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .any(|entry| entry.path.file_name().map(Segment::as_bytes) == Some(name))
    }

    fn flip(side: Side) -> Side {
        match side {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            // `Side` is NOT `#[non_exhaustive]` (unlike the comparison's
            // four vocabularies): a side this binary does not know has no
            // mirror, and saying it does would be inventing it.
            Side::Unknown => Side::Unknown,
        }
    }

    /// The same rows seen from the other side: entries, verdict, newer side
    /// and reason side, all swapped. Nothing more.
    fn mirror(rows: &[CompareRow]) -> Vec<CompareRow> {
        rows.iter()
            .map(|row| CompareRow {
                left: row.right.clone(),
                right: row.left.clone(),
                verdict: match row.verdict {
                    CompareVerdict::OnlyLeft => CompareVerdict::OnlyRight,
                    CompareVerdict::OnlyRight => CompareVerdict::OnlyLeft,
                    other => other,
                },
                newer: row.newer.map(flip),
                side: row.side.map(flip),
                ..row.clone()
            })
            .collect()
    }

    // ---------- the plan's tests ----------

    /// The base case, and the one a user runs after every copy: two identical
    /// trees produce nothing but `Same`, at every depth.
    /// #209: an EXCLUDED subtree comes out in no row, not from one side nor
    /// the other, and is not descended into.
    ///
    /// It is the half #165 could not close: the daemon's read gate looks at
    /// the two ROOTS, so comparing `$HOME` against something else is
    /// legitimate and used to drag the daemon's state directory along with
    /// it. A row that said "only on the left: journal.db" already tells
    /// what the gate wanted kept quiet, so the exclusion is applied to the
    /// LISTING and not to the descent.
    #[tokio::test]
    async fn an_excluded_subtree_comes_out_in_no_row() {
        let left = tree(&["docs/a.txt", "state/journal.db", "state/spools/s.jsonl"]).await;
        let right = tree(&["docs/a.txt"]).await;
        let excluded = MemProvider::root().join(Segment::new(b"state".to_vec()).expect("seg"));

        let rows = collect(compare(
            &left,
            &MemProvider::root(),
            &right,
            &MemProvider::root(),
            CompareOptions::cheap(),
            Sides::from_capabilities(left.capabilities(), right.capabilities()),
            vec![excluded.clone()],
            CancellationToken::new(),
        ))
        .await;

        let names: Vec<String> = rows
            .iter()
            .filter_map(|r| r.left.as_ref().or(r.right.as_ref()))
            .map(|e| e.path.display_lossy())
            .collect();
        assert!(
            names.iter().any(|n| n.contains("a.txt")),
            "what is outside is compared as usual: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("state")),
            "neither the protected directory nor anything inside it: {names:?}"
        );

        // And an excluded ROOT produces nothing at all: without this, the
        // protected directory's own listing would come out whole.
        let root = excluded.clone();
        let empty = collect(compare(
            &left,
            &root,
            &right,
            &MemProvider::root(),
            CompareOptions::cheap(),
            Sides::from_capabilities(left.capabilities(), right.capabilities()),
            vec![excluded],
            CancellationToken::new(),
        ))
        .await;
        assert!(empty.is_empty(), "{empty:?}");
    }

    #[tokio::test]
    async fn identical_trees_are_all_same() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter().all(|row| row.verdict == CompareVerdict::Same),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    /// A directory that exists on one side only is ONE row, not its whole
    /// subtree: the plan will copy it with a recursive `fs.copy`, so
    /// enumerating it buys nothing and costs the walk everything.
    #[tokio::test]
    async fn an_orphan_directory_is_one_row_and_is_not_enumerated() {
        let l = tree(&["only/1.txt", "only/2.txt", "only/deep/3.txt"]).await;
        let r = tree(&[]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
        assert_eq!(
            rows[0].left.as_ref().expect("the side that IS there").kind,
            EntryKind::Dir
        );
    }

    /// One orphan per side: `a/` (with `a/1.txt` and `a/deep/2.txt`) only on
    /// the left, `b/` (with `b/3.txt`) only on the right. One per side on
    /// purpose: without the right's, it could not be asserted that
    /// descending the left does not touch the other.
    async fn orphan_trees() -> (MemProvider, MemProvider) {
        (
            tree(&["a/1.txt", "a/deep/2.txt"]).await,
            tree(&["b/3.txt"]).await,
        )
    }

    /// The row's entry's path's raw SEGMENTS, in bytes (hard rule 1).
    ///
    /// The whole path and not the basename: what a descent can break is
    /// precisely UNDER WHICH root a name comes out, and plain `2.txt` holds
    /// just the same if the row came from listing the wrong directory.
    fn segments_of(row: &CompareRow) -> Vec<Vec<u8>> {
        let entry = [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .next()
            .expect("every row from these comparisons carries a side");
        entry.path.segments().map(<[u8]>::to_vec).collect()
    }

    /// The paths of the rows with that verdict, in the order they came out.
    fn paths_of(rows: &[CompareRow], verdict: CompareVerdict) -> Vec<Vec<Vec<u8>>> {
        rows.iter()
            .filter(|row| row.verdict == verdict)
            .map(segments_of)
            .collect()
    }

    /// `[["a", "deep", "2.txt"]]` written short.
    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|s| s.to_vec()).collect()
    }

    fn descending(side: Side) -> CompareOptions {
        CompareOptions {
            descend_orphans: Some(side),
            ..CompareOptions::cheap()
        }
    }

    /// The default is still spec 1's, with orphans on BOTH sides: one and
    /// one, and nothing that is inside them.
    #[tokio::test]
    async fn an_orphan_directory_is_one_row_by_default() {
        let (l, r) = orphan_trees().await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyLeft),
            vec![path(&[b"a"])]
        );
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyRight),
            vec![path(&[b"b"])]
        );
    }

    /// `descend_orphans` enumerates the orphan on the side it is named for
    /// —down to the bottom— and leaves the other side's exactly as it was.
    #[tokio::test]
    async fn descend_orphans_left_enumerates_the_left_orphan_and_not_the_right_one() {
        let (l, r) = orphan_trees().await;
        let rows = collect(compare_with(&l, &r, descending(Side::Left))).await;

        // WHOLE paths and in order: the container first and each child
        // under it, which a loose basename cannot assert.
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyLeft),
            vec![
                path(&[b"a"]),
                path(&[b"a", b"1.txt"]),
                path(&[b"a", b"deep"]),
                path(&[b"a", b"deep", b"2.txt"]),
            ]
        );
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyRight),
            vec![path(&[b"b"])],
            "the other side is not touched"
        );
    }

    /// The named side, and the opposite one is still one row and nothing
    /// more.
    #[tokio::test]
    async fn descend_orphans_right_is_the_mirror_image() {
        let (l, r) = orphan_trees().await;
        let rows = collect(compare_with(&l, &r, descending(Side::Right))).await;
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyLeft),
            vec![path(&[b"a"])]
        );
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyRight),
            vec![path(&[b"b"]), path(&[b"b", b"3.txt"])]
        );
    }

    /// And the real mirror: descending the left of `(l, r)` gives exactly
    /// the same rows as descending the right of `(r, l)`, swapped side.
    /// It is the same criterion as spec 1's symmetry, applied to the only
    /// piece of the walk that dispatches a side by hand.
    #[tokio::test]
    async fn descending_one_side_is_the_mirror_of_descending_the_other() {
        let (l, r) = orphan_trees().await;
        let forward = collect(compare_with(&l, &r, descending(Side::Left))).await;
        let backward = collect(compare_with(&r, &l, descending(Side::Right))).await;
        assert!(!forward.is_empty());
        assert_eq!(mirror(&forward), backward);
    }

    /// `Side::Unknown` is no side: it is what a wire `"lft"` produces
    /// (`Side` degrades with `serde(other)`), and here it descends NOTHING
    /// — the same set of rows as the default. Whoever handles
    /// `fs.compare` rejects it beforehand precisely because from inside it
    /// is indistinguishable from not having asked for it.
    #[tokio::test]
    async fn an_unknown_side_descends_nothing() {
        let (l, r) = orphan_trees().await;
        let rows = collect(compare_with(&l, &r, descending(Side::Unknown))).await;
        assert_eq!(rows, collect(compare_default(&l, &r)).await);
    }

    /// `max_depth` bounds descending into an orphan the same as into a
    /// pair: what is bounded is the number of listings.
    #[tokio::test]
    async fn descending_an_orphan_still_respects_max_depth() {
        let (l, r) = orphan_trees().await;
        let opts = CompareOptions {
            max_depth: Some(1),
            ..descending(Side::Left)
        };
        let rows = collect(compare_with(&l, &r, opts)).await;
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyLeft),
            vec![
                path(&[b"a"]),
                path(&[b"a", b"1.txt"]),
                path(&[b"a", b"deep"]),
            ],
            "`a/deep` comes out as a row, but is not opened"
        );
    }

    /// Hard rule 3 INSIDE the descent: the stream ends with `Cancelled` and
    /// delivers not one more row, not even the ones it already had
    /// computed.
    ///
    /// The cut happens with the descent already UNDERWAY —it drains until
    /// seeing a row from inside the orphan— and not before: cancelling on
    /// the first row would test spec 1's guard, and the test would pass
    /// just the same without `descend_orphans`.
    #[tokio::test]
    async fn descending_an_orphan_honours_cancellation() {
        // `solo/` only on the left, with 2,000 children: 2,000 rows the
        // descent has to be producing while it gets cut.
        let left = MemProvider::new();
        let solo = MemProvider::root().join(Segment::new(b"solo".to_vec()).expect("seg"));
        left.mkdir(&solo).await.expect("mkdir");
        for i in 0..2_000 {
            let name = Segment::new(format!("e{i:07}").into_bytes()).expect("seg");
            left.mkdir(&solo.join(name)).await.expect("mkdir");
        }
        let right = MemProvider::new();

        let cancel = CancellationToken::new();
        let sides = Sides::from_capabilities(left.capabilities(), right.capabilities());
        let mut stream = compare(
            &left,
            &MemProvider::root(),
            &right,
            &MemProvider::root(),
            descending(Side::Left),
            sides,
            Vec::new(),
            cancel.clone(),
        );
        let mut seen = 0_usize;
        loop {
            let row = stream
                .next()
                .await
                .expect("the stream does not end before the descent")
                .expect("not cancelled yet");
            seen += 1;
            // A row from INSIDE the orphan: the single-side frame has
            // already been visited, which is what this test has to catch
            // by cutting.
            if segments_of(&row).len() == 2 {
                break;
            }
        }
        assert!(seen < 2_000, "the whole tree was needed to get started");
        cancel.cancel();
        let rest: Vec<Result<CompareRow, CompareError>> = stream.collect().await;
        assert_eq!(rest, vec![Err(CompareError::Cancelled)]);
    }

    /// **#152 end to end**: `K.txt` (U+212A KELVIN SIGN) on the left and
    /// `K.txt` (ASCII) on the right are TWO files —they coexist on ext4, no
    /// case folding involved— and the key pairs them, because NFC is not
    /// injective. The row that comes out is an ordinary `Same`/`Different`,
    /// and the only thing that tells it apart from a real pair is
    /// `paired_under`.
    ///
    /// Without that mark, a synchronization plan reads the row as "update
    /// the right one with the left one" and writes over a file that has
    /// nothing to do with it.
    #[tokio::test]
    async fn the_nfc_singleton_marks_the_pair_it_joins() {
        let corpus = norte_testkit::corpus::hostile_names();
        let bytes = |id: &str| {
            corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
                .bytes
                .clone()
        };
        let left = MemProvider::new();
        let right = MemProvider::new();
        for (mem, id, content) in [
            (&left, "singleton_kelvin_sign", &b"kelvin"[..]),
            (&right, "ascii_capital_k", &b"the-real-k"[..]),
        ] {
            let file = MemProvider::root().join(Segment::new(bytes(id)).expect("seg"));
            let mut sink = mem.write(&file).await.expect("write");
            sink.write(Bytes::copy_from_slice(content))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let rows = collect(compare_default(&left, &right)).await;
        assert_eq!(rows.len(), 1, "they pair: ONE row, not two orphans");
        let row = &rows[0];
        assert_eq!(
            row.verdict,
            CompareVerdict::Different,
            "by size, which is what the cascade looks at"
        );
        assert_eq!(
            row.paired_under,
            Some(PairTransform::NormalizationSingleton),
            "and the row SAYS its two halves are not the same name"
        );
        // Each side's bytes are still its own (hard rule 1): the key
        // pairs, the path names.
        assert_eq!(
            row.left
                .as_ref()
                .expect("left")
                .path
                .file_name()
                .expect("name")
                .as_bytes(),
            bytes("singleton_kelvin_sign").as_slice()
        );
        assert_eq!(
            row.right
                .as_ref()
                .expect("right")
                .path
                .file_name()
                .expect("name")
                .as_bytes(),
            bytes("ascii_capital_k").as_slice()
        );
    }

    /// The other half of the contract: the NFC/NFD pair the key exists to
    /// join still pairs, is marked as what it is and NOT as the dangerous
    /// one. A consumer that rejected every `paired_under` would break the
    /// macOS↔Linux case, so the two answers have to be different.
    #[tokio::test]
    async fn the_nfc_nfd_pair_is_marked_but_not_as_a_singleton() {
        let corpus = norte_testkit::corpus::hostile_names();
        let bytes = |id: &str| {
            corpus
                .iter()
                .find(|n| n.id == id)
                .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
                .bytes
                .clone()
        };
        let left = MemProvider::new();
        let right = MemProvider::new();
        for (mem, id) in [(&left, "nfc_e_acute"), (&right, "nfd_e_acute")] {
            let file = MemProvider::root().join(Segment::new(bytes(id)).expect("seg"));
            let mut sink = mem.write(&file).await.expect("write");
            sink.write(Bytes::from_static(b"same"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let rows = collect(compare_default(&left, &right)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, CompareVerdict::Same);
        assert_eq!(rows[0].paired_under, Some(PairTransform::Normalization));
        assert!(
            rows[0].paired_under.expect("marked").names_one_text(),
            "it is the SAME text, and the wire has to be able to say so"
        );
    }

    /// The WHOLE hostile corpus inside a descended orphan: each name comes
    /// out with its bytes intact and under its directory.
    ///
    /// This proves that the pairing key —which folds NFC, and that is why
    /// it makes a corpus pair collide— does NOT touch a single byte of the
    /// path: the key pairs, the path names (hard rule 1). The directory
    /// also carries a hostile name, because descending means LISTING it,
    /// and listing it by its key would be listing something else.
    #[tokio::test]
    async fn descending_an_orphan_keeps_the_hostile_bytes_of_every_name() {
        let corpus = norte_testkit::corpus::hostile_names();
        let dir = corpus
            .iter()
            .find(|n| n.id == "shift_jis_tesuto")
            .expect("the corpus carries shift_jis_tesuto")
            .bytes
            .clone();
        let left = MemProvider::new();
        let dir_path = MemProvider::root().join(Segment::new(dir.clone()).expect("seg"));
        left.mkdir(&dir_path).await.expect("mkdir");
        for name in &corpus {
            let file = dir_path.join(Segment::new(name.bytes.clone()).expect("seg"));
            let mut sink = left.write(&file).await.expect("write");
            sink.write(Bytes::from_static(b"x")).await.expect("chunk");
            sink.commit().await.expect("commit");
        }
        let right = MemProvider::new();

        let rows = collect(compare_with(&left, &right, descending(Side::Left))).await;
        assert_eq!(
            segments_of(&rows[0]),
            vec![dir.clone()],
            "the container comes out by its bytes, not by its key"
        );
        // Each corpus name, ONCE, under its directory and byte for byte.
        // The verdict is not fixed here: the corpus's NFC/NFD pair
        // collapses into one key and comes out `Ambiguous`, which is
        // another decision and has its own tests. What is fixed is that no
        // name is lost or rewritten.
        let mut inside: Vec<Vec<u8>> = rows[1..]
            .iter()
            .map(|row| {
                let segs = segments_of(row);
                assert_eq!(segs.len(), 2, "a row outside the orphan: {segs:?}");
                assert_eq!(segs[0], dir, "child hung off another directory");
                segs[1].clone()
            })
            .collect();
        inside.sort_unstable();
        let mut expected: Vec<Vec<u8>> = corpus.iter().map(|n| n.bytes.clone()).collect();
        expected.sort_unstable();
        assert_eq!(inside, expected);
    }

    /// An orphan whose key COLLIDES with the other side's comes out
    /// `Ambiguous`, and then it is not descended into: nobody is going to
    /// copy that directory while the collision stands, so enumerating it
    /// is listing for nothing.
    #[tokio::test]
    async fn an_ambiguous_orphan_directory_is_not_descended() {
        // Both spellings go on the side that IS case-sensitive (the other
        // would reject them on creation), and the orphan on the one that
        // is not: its key `foo` is collided across the way, so `Foo` is
        // not a clean orphan.
        let left = tree(&["foo", "FOO"]).await;
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "Foo/inside.txt", b"inside").await;

        let rows = collect(compare_with(&left, &right, descending(Side::Right))).await;
        assert!(
            rows.iter()
                .all(|row| row.verdict == CompareVerdict::Ambiguous),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| named(row, b"inside.txt")),
            "descended into an ambiguous directory: {rows:#?}"
        );
    }

    /// And INSIDE the orphan it keeps folding with both sides'
    /// capabilities, even though the other side has nothing there: two
    /// names the destination would not be able to tell apart come out
    /// `Ambiguous` and their subtree is not opened.
    ///
    /// It is the right thing for what the option exists for —they are
    /// exactly the two files that could not be written together at the
    /// destination— and surprising enough to need a test: the other side
    /// decides about a directory it is not in.
    #[tokio::test]
    async fn a_fold_collision_inside_an_orphan_is_ambiguous_and_stops_there() {
        let left = tree(&["solo/README/x.txt", "solo/readme/y.txt"]).await;
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);

        let rows = collect(compare_with(&left, &right, descending(Side::Left))).await;
        assert_eq!(
            paths_of(&rows, CompareVerdict::OnlyLeft),
            vec![path(&[b"solo"])],
            "only the orphan above is clean"
        );
        let ambiguous = paths_of(&rows, CompareVerdict::Ambiguous);
        assert_eq!(
            ambiguous,
            vec![path(&[b"solo", b"README"]), path(&[b"solo", b"readme"])],
            "one row per involved entry, with its bytes"
        );
        assert!(
            !rows
                .iter()
                .any(|row| named(row, b"x.txt") || named(row, b"y.txt")),
            "a collision's subtree is not opened: {rows:#?}"
        );
    }

    /// An unreadable orphan costs ITS OWN row and the descent continues
    /// with the next one, same as an unreadable paired directory.
    #[tokio::test]
    async fn an_unreadable_orphan_is_a_row_and_the_descent_continues() {
        let l = tree(&["solo/denied/x.txt", "solo/after/y.txt"]).await;
        let r = tree(&[]).await;
        deny_list(&l, "solo/denied");
        let rows = collect(compare_with(&l, &r, descending(Side::Left))).await;
        let bad = rows
            .iter()
            .find(|row| row.verdict == CompareVerdict::Error)
            .expect("error row");
        assert_eq!(bad.reason, Some(CompareReason::Unreadable));
        assert_eq!(bad.side, Some(Side::Left));
        assert!(
            rows.iter().any(|row| named(row, b"y.txt")),
            "the descent stopped at the error: {rows:#?}"
        );
    }

    /// Swapping the sides mirrors the verdicts and nothing else. A comparison
    /// that is not symmetric is a comparison that has a favourite.
    #[tokio::test]
    async fn comparing_the_other_way_round_mirrors_the_verdicts() {
        let (l, r) = trees_that_differ().await;
        let forward = collect(compare_default(&l, &r)).await;
        let backward = collect(compare_default(&r, &l)).await;
        assert!(!forward.is_empty());
        assert_eq!(mirror(&forward), backward);
    }

    /// One unreadable subdirectory must cost ITSELF, not the other 40 000
    /// leaves. This is the difference between a three-hour comparison that
    /// answers and one that dies at the first EACCES.
    #[tokio::test]
    async fn an_unreadable_directory_is_a_row_and_the_walk_continues() {
        let (l, r) = twin_trees(&["ok.txt", "denied/x.txt", "after/y.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let bad = rows
            .iter()
            .find(|row| row.verdict == CompareVerdict::Error)
            .expect("error row");
        assert_eq!(bad.reason, Some(CompareReason::Unreadable));
        assert_eq!(bad.side, Some(Side::Left));
        assert!(
            rows.iter().any(|row| named(row, b"y.txt")),
            "the walk stopped at the error"
        );
    }

    /// A provider that lists a path from OUTSIDE the directory does not get
    /// the comparison to pair it, to name it in a row, or —with the hash
    /// rung— to read it: the whole listing counts as unreadable.
    ///
    /// No provider in the tree can do this today (they all build the child
    /// with `dir.join(Segment)`, and `Segment` rejects `/`, `.` and `..`),
    /// and that is exactly why the test is needed: `fs.compare`'s gate
    /// boundary is the two ROOTS, so a path smuggled in by a plugin
    /// provider would skip the whole scope. It is defense in depth, and
    /// without a test it is only an intention.
    #[tokio::test]
    async fn an_entry_outside_the_directory_invalidates_the_listing() {
        let honest = tree(&["inside.txt"]).await;
        let liar = LiarProvider {
            inner: tree(&["inside.txt"]).await,
            at: MemProvider::root(),
            escape: Entry {
                // GRANDCHILD of the root, not a child: `mem:///secret`
                // would be a legitimate entry of `mem:///` and would prove
                // nothing.
                path: at("outside/secret"),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: Some(0),
                attrs: BTreeMap::new(),
            },
        };
        let sides = Sides::from_capabilities(liar.capabilities(), honest.capabilities());
        let rows = collect(compare(
            &liar,
            &MemProvider::root(),
            &honest,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        ))
        .await;
        assert!(
            rows.iter().all(|row| row.verdict == CompareVerdict::Error
                && row.reason == Some(CompareReason::Unreadable)),
            "a listing that strays outside its directory is not paired: {rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| named(row, b"secret")),
            "the smuggled-in path cannot reach a row: {rows:#?}"
        );
    }

    /// The rule, isolated: direct child yes, grandchild no, the directory
    /// itself no, another scheme or authority no.
    #[test]
    fn is_direct_child_is_exact() {
        let vp = |wire: &str| VPath::parse(wire).expect("valid wire");
        let dir = vp("mem:///a/b");
        assert!(is_direct_child(&dir, &vp("mem:///a/b/c")));
        assert!(!is_direct_child(&dir, &vp("mem:///a/b/c/d")), "grandchild");
        assert!(!is_direct_child(&dir, &vp("mem:///a/b")), "itself");
        assert!(!is_direct_child(&dir, &vp("mem:///a")), "its parent");
        assert!(!is_direct_child(&dir, &vp("mem:///a/bb/c")), "sibling");
        assert!(
            !is_direct_child(&dir, &vp("file:///a/b/c")),
            "another scheme"
        );
        assert!(
            !is_direct_child(&vp("sftp://one/x"), &vp("sftp://other/x/y")),
            "another authority"
        );
    }

    /// A `MemProvider` with ONE poisoned listing: for `at` it returns
    /// `escape` (a path that is not its child) and for everything else it
    /// delegates.
    struct LiarProvider {
        inner: MemProvider,
        at: VPath,
        escape: Entry,
    }

    #[async_trait::async_trait]
    impl Provider for LiarProvider {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            if p == &self.at {
                let escape = self.escape.clone();
                return Ok(stream::once(async move { Ok(escape) }).boxed());
            }
            self.inner.list(p).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    /// A directory over the declared cap costs that directory, not an OOM.
    #[tokio::test]
    async fn a_directory_over_the_cap_is_a_row_not_an_oom() {
        let (l, r) = twin_trees_with_wide_dir(COMPARE_MAX_DIR_ENTRIES + 1).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            rows.iter()
                .any(|row| row.reason == Some(CompareReason::DirTooLarge)),
            "{rows:#?}"
        );
    }

    /// Hard rule 3. Cancelling stops the stream — no row after the cut, no work
    /// after the cut, and nothing to clean up because nothing is written.
    #[tokio::test]
    async fn cancelling_stops_the_stream_cleanly() {
        let (l, r) = twin_trees_with_wide_dir(5_000).await;
        let cancel = CancellationToken::new();
        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let mut stream = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            cancel.clone(),
        );
        let first = stream.next().await.expect("at least one row");
        assert!(first.is_ok());
        cancel.cancel();
        let rest = stream.count().await;
        assert!(
            rest < 5_000,
            "the walk kept going after cancellation: {rest} more rows"
        );
    }

    /// A token that already came fired pairs NOTHING: not one listing, not
    /// one row. Cancellation is checked before working, not after.
    #[tokio::test]
    async fn a_token_already_cancelled_pairs_nothing() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let items: Vec<Result<CompareRow, CompareError>> = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            cancel,
        )
        .collect()
        .await;
        assert_eq!(items, vec![Err(CompareError::Cancelled)]);
    }

    /// Draining ONE directory checks the token entry by entry.
    ///
    /// It is the half of hard rule 3 the stream tests cannot see: a
    /// directory with hundreds of thousands of entries drains INSIDE a
    /// single stream step, so without this check cancellation would wait
    /// for it to finish. It is tested directly on `list_all` because doing
    /// it through the stream would require cancelling halfway through an
    /// `await`, which is a race.
    #[tokio::test]
    async fn draining_a_directory_checks_the_token() {
        let mem = tree(&["a.txt", "b.txt"]).await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = list_all(&mem, &MemProvider::root(), &cancel).await;
        assert!(
            matches!(outcome, Err(ListFailure::Cancelled)),
            "draining continued with the token fired"
        );
        // And without cancelling, the same listing does drain whole.
        let ok = list_all(&mem, &MemProvider::root(), &CancellationToken::new()).await;
        assert!(matches!(ok, Ok(entries) if entries.len() == 2));
    }

    /// `max_depth` bounds the descent and says so by not emitting deeper rows.
    #[tokio::test]
    async fn max_depth_bounds_the_descent() {
        let (l, r) = twin_trees(&["a.txt", "one/b.txt", "one/two/c.txt"]).await;
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(1))).await;
        assert!(rows.iter().any(|row| named(row, b"b.txt")), "{rows:#?}");
        assert!(!rows.iter().any(|row| named(row, b"c.txt")), "{rows:#?}");
    }

    /// `max_depth(0)` pairs ONLY the root: its direct children come out as
    /// rows and no directory is opened.
    ///
    /// It is `max_depth`'s contract's edge case, which the rustdoc could be
    /// read two ways and now says one.
    #[tokio::test]
    async fn max_depth_zero_pairs_only_the_root() {
        let (l, r) = twin_trees(&["a.txt", "one/b.txt"]).await;
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(0))).await;
        assert!(rows.iter().any(|row| named(row, b"a.txt")), "{rows:#?}");
        assert!(rows.iter().any(|row| named(row, b"one")), "{rows:#?}");
        assert!(!rows.iter().any(|row| named(row, b"b.txt")), "{rows:#?}");
    }

    /// `follow_symlinks` is accepted and does NOTHING.
    ///
    /// An option that is accepted and ignored is worse than one that does
    /// not exist: the caller believes it asked for something. While it
    /// stays on the struct —and on the wire—, this fixes that it does not
    /// change a single row, so whoever implements it one day sees this test
    /// fall.
    #[tokio::test]
    async fn follow_symlinks_is_accepted_and_does_nothing() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt"]).await;
        l.symlink(&at("link"), b"../outside", norte_vfs::SymlinkKind::File)
            .await
            .expect("symlink");

        let without = collect(compare_default(&l, &r)).await;
        let with = collect(compare_with(
            &l,
            &r,
            CompareOptions {
                follow_symlinks: true,
                ..CompareOptions::cheap()
            },
        ))
        .await;
        assert_eq!(without, with);
    }

    // ---------- what the plan does not fix ----------

    /// The walk READS the links' targets and passes them to the cascade.
    ///
    /// The cascade already tests that two different targets are
    /// `Different`; what is missing to test here is that SOMEBODY calls
    /// `read_link`. Without this test, a walk that read not a single target
    /// would still pass the archive's test —which expects `Unknown`
    /// precisely because the targets CANNOT be read—: the absence of the
    /// call and the absence of the answer look the same from outside.
    #[tokio::test]
    async fn the_walk_reads_the_links_targets() {
        let link = || MemProvider::root().join(Segment::new(b"l".to_vec()).expect("seg"));
        let seed_link = async |target: &'static [u8]| {
            let mem = MemProvider::new();
            mem.symlink(&link(), target, norte_vfs::SymlinkKind::File)
                .await
                .expect("symlink");
            mem
        };

        let l = seed_link(b"../a").await;
        let different = seed_link(b"../b").await;
        let rows = collect(compare_default(&l, &different)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );

        let same = seed_link(b"../a").await;
        let rows = collect(compare_default(&l, &same)).await;
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Same,
                CompareCriterion::LinkTarget,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
    }

    /// A link against a file does not spend a `read_link`: the kind rung
    /// already decided, and asking for the other's target is a call to the
    /// provider for nothing in return.
    #[tokio::test]
    async fn a_link_against_a_file_is_a_type_mismatch() {
        let l = MemProvider::new();
        l.symlink(
            &MemProvider::root().join(Segment::new(b"x".to_vec()).expect("seg")),
            b"../a",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");
        let r = tree(&["x"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::TypeMismatch);
        assert_eq!(rows[0].criterion, CompareCriterion::Kind);
    }

    /// The walk continues AFTER the error, not only around it: `zz` sorts
    /// after `denied`, so its row can only exist if the walk continued past
    /// the error row.
    #[tokio::test]
    async fn the_walk_continues_after_the_unreadable_directory() {
        let (l, r) = twin_trees(&["denied/x.txt", "zz/z.txt"]).await;
        deny_list(&l, "denied");
        let rows = collect(compare_default(&l, &r)).await;
        let error_at = rows
            .iter()
            .position(|row| row.verdict == CompareVerdict::Error)
            .expect("the error row");
        let z_at = rows
            .iter()
            .position(|row| named(row, b"z.txt"))
            .expect("the leaf that comes after");
        assert!(error_at < z_at, "{rows:#?}");
    }

    /// The id is monotonic and does not repeat: the panel's selection
    /// anchors to it.
    #[tokio::test]
    async fn ids_are_monotonic_and_unique() {
        let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "{ids:?}");
    }

    /// Two entries on the same side that collapse come out as TWO
    /// `Ambiguous` rows, each in ITS side's field and with `side` naming
    /// where the collision is. They are never merged and never
    /// deduplicated.
    #[tokio::test]
    async fn a_same_side_collision_comes_out_as_one_row_per_entry() {
        let left = tree(&["README", "readme"]).await;
        // A side that is not case-sensitive: pairing against it is folding.
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let ambiguous: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Ambiguous)
            .collect();
        assert_eq!(
            ambiguous.len(),
            3,
            "two collided + their counterpart: {rows:#?}"
        );
        assert!(
            ambiguous
                .iter()
                .all(|row| row.reason == Some(CompareReason::CaseFold)
                    && row.side == Some(Side::Left)),
            "{ambiguous:#?}"
        );
        // The left's two carry THEIR entry; the counterpart, its own.
        let lefts = ambiguous.iter().filter(|row| row.left.is_some()).count();
        let rights = ambiguous.iter().filter(|row| row.right.is_some()).count();
        assert_eq!((lefts, rights), (2, 1), "{ambiguous:#?}");
        assert!(
            ambiguous
                .iter()
                .all(|row| row.left.is_none() || row.right.is_none()),
            "a collision is of ONE side: {ambiguous:#?}"
        );
    }

    /// A collision's lone counterpart is NOT `OnlyRight`.
    ///
    /// Telling a synchronization plan `OnlyRight` is telling it "copy it to
    /// the other side", and copying into a directory that can no longer
    /// tell those two names apart creates a THIRD collided file.
    /// `Ambiguous` makes that plan refuse to act, which is the only safe
    /// answer.
    #[tokio::test]
    async fn a_collisions_counterpart_is_not_offered_for_copying() {
        let left = tree(&["README", "readme"]).await;
        let right = MemProvider::with_flags(norte_vfs::CapabilityFlags::CASE_PRESERVING);
        seed(&right, "README", b"README").await;

        let rows = collect(compare_default(&left, &right)).await;
        let counterpart = rows
            .iter()
            .find(|row| row.right.is_some() && row.left.is_none())
            .expect("the counterpart's row");
        assert_eq!(counterpart.verdict, CompareVerdict::Ambiguous);
        assert_eq!(counterpart.reason, Some(CompareReason::CaseFold));
        assert_eq!(
            counterpart.side,
            Some(Side::Left),
            "the colliding side is the left, not the row's own"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
    }

    /// Two unreadable directories paired are TWO rows, one per side.
    ///
    /// Turning back at the first failure is the comfortable thing and
    /// leaves the second undiscovered: the user would fix the left's
    /// permissions and the next comparison would show the same broken
    /// directory again, now on the other side.
    #[tokio::test]
    async fn two_unreadable_sides_are_two_rows() {
        let (l, r) = twin_trees(&["dir/a.txt"]).await;
        deny_list(&l, "dir");
        deny_list(&r, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        let errors: Vec<&CompareRow> = rows
            .iter()
            .filter(|row| row.verdict == CompareVerdict::Error)
            .collect();
        assert_eq!(errors.len(), 2, "{rows:#?}");
        assert_eq!(errors[0].side, Some(Side::Left));
        assert_eq!(errors[1].side, Some(Side::Right));
        // Each row carries the directory of the side it names, and only
        // that one.
        assert!(errors[0].left.is_some() && errors[0].right.is_none());
        assert!(errors[1].right.is_some() && errors[1].left.is_none());
    }

    /// An unreadable directory does NOT turn what is across from it into
    /// `OnlyRight`: nobody has checked that absence.
    #[tokio::test]
    async fn a_broken_listing_does_not_invent_absences_on_the_other_side() {
        let (l, r) = twin_trees(&["dir/a.txt", "dir/b.txt"]).await;
        deny_list(&l, "dir");
        let rows = collect(compare_default(&l, &r)).await;
        assert!(
            !rows
                .iter()
                .any(|row| row.verdict == CompareVerdict::OnlyRight),
            "{rows:#?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.verdict == CompareVerdict::Error)
                .count(),
            1,
            "{rows:#?}"
        );
    }

    // ---------- the hash rung ----------

    /// The point of the rung: same size, same mtime, different bytes. Every
    /// cheap criterion says `Same`; only the hash tells the truth. This is the
    /// case a user turns hashing on FOR.
    #[tokio::test]
    async fn same_size_same_mtime_different_bytes_is_caught_only_by_hash() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"bbbb").await;

        let cheap = collect(compare_default(&l, &r)).await;
        assert_eq!(cheap.len(), 1, "{cheap:#?}");
        assert_eq!(cheap[0].verdict, CompareVerdict::Same);
        assert_eq!(cheap[0].confidence, CompareConfidence::Probable);

        let hashed = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(hashed.len(), 1, "{hashed:#?}");
        assert_eq!(hashed[0].verdict, CompareVerdict::Different);
        assert_eq!(hashed[0].criterion, CompareCriterion::Hash);
        assert_eq!(hashed[0].confidence, CompareConfidence::Certain);
    }

    /// With hash off, comparison reads no content at all. A user who did not
    /// ask to hash a terabyte over SFTP must not be made to.
    #[tokio::test]
    async fn without_the_hash_rung_no_content_is_read() {
        let (l, r) = twin_trees(&["a.txt", "b.txt"]).await;
        let rows = collect(compare_default(&l, &r)).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(l.faults().read_calls(), 0);
        assert_eq!(r.faults().read_calls(), 0);
    }

    /// The hash only reaches the pairs the cheap rungs called equal. Hashing a
    /// pair already known to differ is pure waste.
    #[tokio::test]
    async fn the_hash_only_runs_on_pairs_the_cheap_rungs_called_equal() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        // Same sequence of mutations on both sides: same dates.
        seed(&l, "equal.bin", b"aaaa").await;
        seed(&r, "equal.bin", b"aaaa").await;
        seed(&l, "size.bin", b"aa").await;
        seed(&r, "size.bin", b"aaaaaaaaaa").await;

        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            l.faults().read_calls(),
            1,
            "only the equal-looking pair should be read: {rows:#?}"
        );
        assert_eq!(r.faults().read_calls(), 1, "{rows:#?}");
    }

    /// A read that fails mid-hash costs its row, not the walk — and says which
    /// side failed.
    ///
    /// The criterion is `Hash` and not `Presence`: the rung RAN and died.
    /// `Presence`'s convention is for rows where none ran.
    #[tokio::test]
    async fn a_read_that_fails_mid_hash_is_an_error_row() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        l.faults().fail_read_at(&at("x.bin"), 2);
        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error);
        assert_eq!(rows[0].reason, Some(CompareReason::ReadFailed));
        assert_eq!(rows[0].side, Some(Side::Left));
        assert_eq!(rows[0].criterion, CompareCriterion::Hash);
        assert!(
            rows[0].left.is_some() && rows[0].right.is_some(),
            "the pair DID pair: the row carries both sides"
        );
        assert!(rows[0].reason_is_consistent());
    }

    /// A side that cannot be read does not make the other one get read: the
    /// row is already an error, and reading the second file whole would not
    /// change a single letter of it. Over 40 GB that is half an hour given
    /// away for free.
    #[tokio::test]
    async fn a_broken_read_does_not_drag_the_other_side_along() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        l.faults().fail_read_at(&at("x.bin"), 2);
        collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(l.faults().read_calls(), 1);
        assert_eq!(r.faults().read_calls(), 0);
    }

    /// The walk CONTINUES after a broken read, same as it continues after
    /// an unreadable listing: a three-hour comparison does not die on leaf
    /// 40,000.
    #[tokio::test]
    async fn the_walk_continues_after_a_broken_read() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.bin", b"aaaa").await;
        seed(&r, "a.bin", b"aaaa").await;
        seed(&l, "zz.bin", b"zzzz").await;
        seed(&r, "zz.bin", b"zzzz").await;
        l.faults().fail_read_at(&at("a.bin"), 2);

        let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error, "{rows:#?}");
        let zz = rows
            .iter()
            .find(|row| named(row, b"zz.bin"))
            .expect("the leaf after the broken read");
        assert_eq!(zz.verdict, CompareVerdict::Same);
        assert_eq!(zz.criterion, CompareCriterion::Hash);
        assert_eq!(zz.confidence, CompareConfidence::Certain);
    }

    /// With the expensive rung on, the comparison is still SYMMETRIC: the
    /// side that fails to read swaps places, and nothing more.
    #[tokio::test]
    async fn the_hash_rung_is_also_symmetric() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"bbbb").await;
        let opts = CompareOptions::cheap().with_hash();
        let forward = collect(compare_with(&l, &r, opts)).await;
        let back = collect(compare_with(&r, &l, opts)).await;
        assert_eq!(mirror(&forward), back);

        l.faults().fail_read_at(&at("x.bin"), 2);
        let forward = collect(compare_with(&l, &r, opts)).await;
        let back = collect(compare_with(&r, &l, opts)).await;
        assert_eq!(forward[0].side, Some(Side::Left));
        assert_eq!(back[0].side, Some(Side::Right));
        assert_eq!(mirror(&forward), back);
    }

    /// Cancelling while the expensive rung reads publishes no provisional
    /// row: the stream ends in [`CompareError::Cancelled`] and that is it.
    #[tokio::test]
    async fn cancelling_during_the_hash_does_not_publish_the_pair() {
        let (l, r) = pair_with_content("x.bin", b"aaaa", b"aaaa").await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let items: Vec<Result<CompareRow, CompareError>> = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap().with_hash(),
            sides,
            Vec::new(),
            cancel,
        )
        .collect()
        .await;
        assert_eq!(items, vec![Err(CompareError::Cancelled)]);
        assert_eq!(l.faults().read_calls(), 0, "the file was not even opened");
    }

    // ---------- the provider people USE ----------
    //
    // `norte-vfs-local::list` fills in neither `size` nor `mtime_ms` (#52:
    // lazy stat, `None` = "don't know", `Entry`'s contract). The whole suite
    // above runs on `MemProvider`, which DOES fill them in, so none of its
    // tests can see the one thing that happens to a user: comparing two
    // local directories. These three go against real temporary
    // directories.

    /// Seeds a temporary directory and serves it through `norte-vfs-local`.
    ///
    /// `seed` receives the NATIVE path: tests that fix dates write there.
    /// The `TempDir` travels inside the provider (`with_guard`), so it lives
    /// exactly as long as it does.
    fn local_tree(seed: impl FnOnce(&std::path::Path)) -> LocalProvider {
        let dir = tempfile::tempdir().expect("tempdir");
        seed(dir.path());
        let base = dir.path().to_path_buf();
        LocalProvider::rooted(base).with_guard(Box::new(dir))
    }

    /// Sets a file's modification date, in seconds since epoch.
    ///
    /// The wall clock is no good: two files written back to back can fall
    /// within the 2s tolerance, or not, depending on how loaded the machine
    /// is. A test that sometimes passes proves nothing.
    fn set_mtime(path: &std::path::Path, secs: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open to set the date");
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .expect("set the date");
    }

    async fn compare_local<'a>(
        left: &'a LocalProvider,
        right: &'a LocalProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        // `LocalProvider::capabilities()` is exact only after an async
        // operation (the probe runs there): a root `stat` forces it before
        // reading it, same as `norte-core` does now that #153 moved
        // `Sides`'s computation out of this engine.
        let _ = left.stat(&LocalProvider::root()).await;
        let _ = right.stat(&LocalProvider::root()).await;
        let sides = Sides::from_capabilities(left.capabilities(), right.capabilities());
        compare(
            left,
            &LocalProvider::root(),
            right,
            &LocalProvider::root(),
            opts,
            sides,
            Vec::new(),
            CancellationToken::new(),
        )
    }

    /// Two local files of 5 and 12 bytes are DIFFERENT, and with certainty.
    ///
    /// This is the test that was missing: without hydrating, `list` does
    /// not bring the size, the size rung stops at `Same`/`Unknown` and the
    /// local comparison —the one almost everybody does— cannot tell apart
    /// two files that have nothing in common.
    #[tokio::test]
    async fn two_local_files_of_different_size_are_different() {
        let l = local_tree(|d| {
            std::fs::write(d.join("a.txt"), b"hello!").expect("seed");
        });
        let r = local_tree(|d| {
            std::fs::write(d.join("a.txt"), b"hello world!!").expect("seed");
        });

        let rows = collect(compare_local(&l, &r, CompareOptions::cheap()).await).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
        // And the row CARRIES the sizes that decided it: a panel that
        // paints "different by size" over two empty sizes cannot be read.
        assert_eq!(rows[0].left.as_ref().expect("left side").size, Some(6));
        assert_eq!(rows[0].right.as_ref().expect("right side").size, Some(13));
    }

    /// The same one rung down: same size, dates one minute apart. Without
    /// hydrating, the date rung does not even get to run.
    #[tokio::test]
    async fn two_local_files_of_the_same_size_are_decided_by_date() {
        let l = local_tree(|d| {
            let p = d.join("a.txt");
            std::fs::write(&p, b"aaaa").expect("seed");
            set_mtime(&p, 1_700_000_000);
        });
        let r = local_tree(|d| {
            let p = d.join("a.txt");
            std::fs::write(&p, b"bbbb").expect("seed");
            set_mtime(&p, 1_700_000_060);
        });

        let rows = collect(compare_local(&l, &r, CompareOptions::cheap()).await).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion, rows[0].confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Mtime,
                CompareConfidence::Probable
            ),
            "{rows:#?}"
        );
        assert_eq!(rows[0].newer, Some(Side::Right));
        assert_eq!(
            rows[0].left.as_ref().expect("left side").mtime_ms,
            Some(1_700_000_000_000)
        );
    }

    // ---------- who a `stat` is spent on, and who it is not ----------

    /// A `MemProvider` that LISTS like the real local provider: no `size`
    /// and no `mtime_ms` (#52). Counts `stat`s and can fail them.
    ///
    /// The local one is no good for this: its calls cannot be counted nor
    /// can one specific `stat` be broken. What the local one does prove
    /// —that the comparison everybody does works— is the two tests above;
    /// this proves WHO is asked.
    struct LazyProvider {
        inner: MemProvider,
        stats: std::sync::atomic::AtomicUsize,
        /// Which fields it blanks from the listing. `false` = that field
        /// travels as `MemProvider` set it.
        blank_size: bool,
        stat_fails: Option<norte_proto::Error>,
    }

    impl LazyProvider {
        /// Lazy like the local one: no `size` and no `mtime_ms`.
        fn new(inner: MemProvider) -> Self {
            Self {
                inner,
                stats: std::sync::atomic::AtomicUsize::new(0),
                blank_size: true,
                stat_fails: None,
            }
        }

        /// Lazy ONLY in the date: the shape of an SFTP server that does not
        /// send `ACMODTIME` in the `readdir` attributes (`russh_sftp`'s
        /// `size` always comes; `mtime` is optional).
        fn only_mtime_missing(inner: MemProvider) -> Self {
            Self {
                blank_size: false,
                ..Self::new(inner)
            }
        }

        fn failing(inner: MemProvider, error: norte_proto::Error) -> Self {
            Self {
                stat_fails: Some(error),
                ..Self::new(inner)
            }
        }

        fn stats(&self) -> usize {
            self.stats.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl Provider for LazyProvider {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            self.stats
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(error) = self.stat_fails.clone() {
                return Err(error);
            }
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            let blank_size = self.blank_size;
            Ok(self
                .inner
                .list(p)
                .await?
                .map(move |item| {
                    item.map(|entry| Entry {
                        size: if blank_size { None } else { entry.size },
                        mtime_ms: None,
                        ..entry
                    })
                })
                .boxed())
        }
        async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, norte_proto::Error> {
            self.inner.read_link(p).await
        }
        async fn symlink(
            &self,
            link: &VPath,
            target: &[u8],
            kind: norte_vfs::SymlinkKind,
        ) -> Result<(), norte_proto::Error> {
            self.inner.symlink(link, target, kind).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    fn compare_lazy<'a>(
        left: &'a LazyProvider,
        right: &'a LazyProvider,
        opts: CompareOptions,
    ) -> CompareStream<'a> {
        let sides = Sides::from_capabilities(left.capabilities(), right.capabilities());
        compare(
            left,
            &MemProvider::root(),
            right,
            &MemProvider::root(),
            opts,
            sides,
            Vec::new(),
            CancellationToken::new(),
        )
    }

    /// Hydration is ONLY for pairs that are going to use the data.
    ///
    /// An orphan is decided by presence, a different type is decided by
    /// kind and two directories too (C3): none of them spends a trip to
    /// the provider. Over SFTP, statting what is already decided is the
    /// difference between a comparison and a wait.
    #[tokio::test]
    async fn what_presence_or_kind_decide_spends_no_stat() {
        // An orphan: presence decides and nobody asks anything.
        let l = LazyProvider::new(tree(&["solo.txt"]).await);
        let r = LazyProvider::new(tree(&[]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
        assert_eq!((l.stats(), r.stats()), (0, 0), "an orphan is not statted");

        // Different types: decided by kind.
        let l = LazyProvider::new(tree(&["x/inside.txt"]).await);
        let r = LazyProvider::new(tree(&["x"]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::TypeMismatch);
        assert_eq!(
            (l.stats(), r.stats()),
            (0, 0),
            "a different type is not statted"
        );

        // Two directories: decided by kind; their CHILDREN are hydrated,
        // and with a single `stat` per side and per pair.
        let l = LazyProvider::new(tree(&["d/a.txt"]).await);
        let r = LazyProvider::new(tree(&["d/a.txt"]).await);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            (l.stats(), r.stats()),
            (1, 1),
            "only the file: the directory was decided by kind"
        );
    }

    /// A provider that ALREADY fills in its listing receives not one extra
    /// call.
    ///
    /// `MemProvider` brings `size` and `mtime_ms` on every entry, so the
    /// whole comparison spends not a single `stat`. Without this, hydration
    /// could fire every time and nobody would notice: the verdict would be
    /// the same and the bill, double.
    #[tokio::test]
    async fn a_listing_that_already_brings_the_fields_is_not_asked_again() {
        struct Counted(MemProvider, std::sync::atomic::AtomicUsize);
        #[async_trait::async_trait]
        impl Provider for Counted {
            fn scheme(&self) -> &str {
                self.0.scheme()
            }
            fn capabilities(&self) -> norte_proto::Capabilities {
                self.0.capabilities()
            }
            async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
                self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.0.stat(p).await
            }
            async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
                self.0.list(p).await
            }
            async fn read(
                &self,
                p: &VPath,
                range: Option<norte_proto::ByteRange>,
            ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
                self.0.read(p, range).await
            }
            async fn write(
                &self,
                p: &VPath,
            ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
                self.0.write(p).await
            }
            async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
                self.0.mkdir(p).await
            }
            async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
                self.0.remove(p).await
            }
            async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
                self.0.rename(from, to).await
            }
        }

        let count = || std::sync::atomic::AtomicUsize::new(0);
        let l = Counted(tree(&["a.txt", "sub/b.txt"]).await, count());
        let r = Counted(tree(&["a.txt", "sub/b.txt"]).await, count());
        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let rows = collect(compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        ))
        .await;
        assert!(!rows.is_empty());
        assert_eq!(
            (
                l.1.load(std::sync::atomic::Ordering::Relaxed),
                r.1.load(std::sync::atomic::Ordering::Relaxed)
            ),
            (0, 0),
            "the listing already brought the fields: {rows:#?}"
        );
    }

    /// A `stat` that fails is an ERROR row, not an `Unknown`.
    ///
    /// `Unknown` is "the provider cannot answer this question", and travels
    /// with a `Same` verdict. A broken `stat` is not that: it is an
    /// `EACCES` the user can fix, or a file that disappeared between the
    /// `list` and the `stat`. Answering "equal, don't know" about a pair
    /// nobody got to look at is exactly what the confidence vocabulary
    /// exists to avoid doing.
    #[tokio::test]
    async fn a_stat_that_fails_is_an_error_row() {
        let l = LazyProvider::failing(
            tree(&["a.txt", "zz.txt"]).await,
            norte_proto::Error::Io { retryable: false },
        );
        let r = LazyProvider::new(tree(&["a.txt", "zz.txt"]).await);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 2, "{rows:#?}");
        for row in &rows {
            assert_eq!(row.verdict, CompareVerdict::Error, "{rows:#?}");
            assert_eq!(row.reason, Some(CompareReason::Unreadable));
            assert_eq!(row.side, Some(Side::Left));
            // The rung left without its data, same as a broken read says
            // `Hash`: that rung ran and died.
            assert_eq!(row.criterion, CompareCriterion::Size);
            assert!(
                row.left.is_some() && row.right.is_some(),
                "the pair DID pair: what failed was describing it"
            );
            assert!(row.reason_is_consistent() && row.sides_are_consistent());
        }
        // The walk CONTINUES after the error —both rows are there— and the
        // side that did not fail does not pay the trip: the row is already
        // an error and statting the right would not change a single letter
        // of it.
        assert_eq!(l.stats(), 2, "one `stat` per pair, no more");
        assert_eq!(r.stats(), 0, "a broken side does not drag the other along");
    }

    /// A `LazyProvider` whose `stat` cooperatively yields BEFORE answering
    /// — more times the SMALLER the name's index — so the pairs finish
    /// their hydration in the REVERSE order they were submitted in. With no
    /// wall clock (no `tokio::time::sleep`, which the repo avoids as an
    /// ordering mechanism: "under load anyone can lose their race"):
    /// `yield_now` reorders `buffer_unordered`'s POLLING deterministically,
    /// not by timer chance.
    ///
    /// Exists so `hydrating_many_pairs_at_once_does_not_cross_their_rows`
    /// really exercises the out-of-order path — without this, `LazyProvider`
    /// answers every `stat` on the first `poll`, so `buffer_unordered`
    /// would resolve them in the same order they were submitted and a
    /// regression that wrote `resolved` by ARRIVAL order instead of by
    /// index would go unnoticed.
    struct ReorderedProvider {
        inner: LazyProvider,
        /// How many pairs there are in total: index `i` yields
        /// `total - 1 - i` times, so pair `total - 1` (the last submitted) yields
        /// nothing and `0` yields more than any other.
        total: usize,
    }

    #[async_trait::async_trait]
    impl Provider for ReorderedProvider {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            let name = p.file_name().map(Segment::as_bytes).unwrap_or_default();
            let name = std::str::from_utf8(name).unwrap_or_default();
            if let Some(i) = name.get(1..3).and_then(|s| s.parse::<usize>().ok()) {
                for _ in 0..self.total.saturating_sub(1).saturating_sub(i) {
                    tokio::task::yield_now().await;
                }
            }
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            self.inner.list(p).await
        }
        async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, norte_proto::Error> {
            self.inner.read_link(p).await
        }
        async fn symlink(
            &self,
            link: &VPath,
            target: &[u8],
            kind: norte_vfs::SymlinkKind,
        ) -> Result<(), norte_proto::Error> {
            self.inner.symlink(link, target, kind).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    /// #156: hydrating many pairs AT ONCE (`buffer_unordered`, which
    /// resolves out of order — here forced to finish in the REVERSE order
    /// of submission via [`ReorderedProvider`]) cannot mix one pair's
    /// result with another's index, NOR reorder the emitted rows:
    /// `Walk::visit` assigns the `id` in step 1's KEY order, never in the
    /// order its `stat` finished. With 16 pairs against
    /// `HYDRATE_CONCURRENCY = 12`, at least four have to wait in line
    /// behind the first twelve.
    #[tokio::test]
    async fn hydrating_many_pairs_at_once_does_not_cross_their_rows() {
        const N: usize = 16;
        let names: Vec<String> = (0..N).map(|i| format!("p{i:02}.txt")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let l = MemProvider::new();
        let r = MemProvider::new();
        for (i, name) in paths.iter().enumerate() {
            // Unique content per PAIR and per SIDE (neither the left nor
            // the right size repeats between any two pairs), so a row with
            // ANOTHER pair's data would show even if that other pair were
            // also `Same`.
            let left_content = vec![b'x'; 100 + i];
            let right_content = if i.is_multiple_of(2) {
                left_content.clone() // even: `Same`
            } else {
                vec![b'x'; 100 + i + 1] // odd: `Different`, one byte more
            };
            seed(&l, name, &left_content).await;
            seed(&r, name, &right_content).await;
        }
        let l = ReorderedProvider {
            inner: LazyProvider::new(l),
            total: N,
        };
        let r = LazyProvider::new(r);

        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let rows = collect(compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        ))
        .await;
        assert_eq!(rows.len(), N, "{rows:#?}");
        for (k, row) in rows.iter().enumerate() {
            let left = row.left.as_ref().expect("paired: left side");
            let right = row.right.as_ref().expect("paired: right side");
            // The EMISSION order is the KEY order — `p00.txt` first,
            // `p15.txt` last — whatever the order their `stat`s finished
            // in (#156, id assigned in step 3).
            assert_eq!(
                left.path.file_name().map(Segment::as_bytes),
                Some(paths[k].as_bytes()),
                "row {k}: emission order is not key order: {rows:#?}"
            );
            assert_eq!(
                left.path.file_name().map(Segment::as_bytes),
                right.path.file_name().map(Segment::as_bytes),
                "the row has to pair the SAME name on both sides: {row:#?}"
            );
            let expected_left_size = 100 + k as u64;
            assert_eq!(
                left.size,
                Some(expected_left_size),
                "{}'s left size is not ANOTHER pair's: {row:#?}",
                paths[k],
            );
            if k.is_multiple_of(2) {
                assert_eq!(row.verdict, CompareVerdict::Same, "{}: {row:#?}", paths[k]);
                assert_eq!(
                    right.size,
                    Some(expected_left_size),
                    "{}: {row:#?}",
                    paths[k]
                );
            } else {
                assert_eq!(
                    row.verdict,
                    CompareVerdict::Different,
                    "{}: {row:#?}",
                    paths[k]
                );
                assert_eq!(
                    right.size,
                    Some(expected_left_size + 1),
                    "{}'s right size is another pair's: {row:#?}",
                    paths[k],
                );
            }
        }
    }

    /// A `LazyProvider` whose `stat` fires `cancel` itself, after a fixed
    /// number of calls — to cancel WHILE other hydrations are still in
    /// flight under `buffer_unordered`, with no blind sleeps nor race with
    /// the clock.
    struct CancelAfterN {
        inner: LazyProvider,
        remaining: std::sync::atomic::AtomicI64,
        cancel: CancellationToken,
    }

    #[async_trait::async_trait]
    impl Provider for CancelAfterN {
        fn scheme(&self) -> &str {
            self.inner.scheme()
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            self.inner.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, norte_proto::Error> {
            if self
                .remaining
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
                == 1
            {
                self.cancel.cancel();
            }
            self.inner.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, norte_proto::Error> {
            self.inner.list(p).await
        }
        async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, norte_proto::Error> {
            self.inner.read_link(p).await
        }
        async fn symlink(
            &self,
            link: &VPath,
            target: &[u8],
            kind: norte_vfs::SymlinkKind,
        ) -> Result<(), norte_proto::Error> {
            self.inner.symlink(link, target, kind).await
        }
        async fn read(
            &self,
            p: &VPath,
            range: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, norte_proto::Error> {
            self.inner.read(p, range).await
        }
        async fn write(
            &self,
            p: &VPath,
        ) -> Result<Box<dyn norte_vfs::ByteSink>, norte_proto::Error> {
            self.inner.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.remove(p).await
        }
        async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), norte_proto::Error> {
            self.inner.rename(from, to).await
        }
    }

    /// #156: cancelling WHILE concurrent hydration is IN FLIGHT does not
    /// panic (step 3's `resolved[i].take().expect(..)` in `Walk::visit`
    /// depends on the cancellation check after step 2 being exhaustive)
    /// and publishes not one row of the directory — same as if the token
    /// had already come fired, which is what
    /// `a_token_already_cancelled_pairs_nothing` fixes. This test is the
    /// half that one does NOT cover: a cancel that arrives mid a real
    /// `stat` under `buffer_unordered`, with other hydrations still
    /// pending (20 pairs against `HYDRATE_CONCURRENCY = 12`).
    #[tokio::test]
    async fn cancelling_mid_concurrent_hydration_does_not_panic() {
        const N: usize = 20;
        let names: Vec<String> = (0..N).map(|i| format!("q{i:02}.txt")).collect();
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();
        let l = tree(&paths).await;
        let r = tree(&paths).await;

        let cancel = CancellationToken::new();
        let l = CancelAfterN {
            inner: LazyProvider::new(l),
            // Fires halfway through hydration: enough for other pairs from
            // the first `HYDRATE_CONCURRENCY` batch to still be in flight.
            remaining: std::sync::atomic::AtomicI64::new(3),
            cancel: cancel.clone(),
        };
        let r = LazyProvider::new(r);
        let sides = Sides::from_capabilities(l.capabilities(), r.capabilities());
        let mut stream = compare(
            &l,
            &MemProvider::root(),
            &r,
            &MemProvider::root(),
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            cancel,
        );

        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }
        // Getting here with no panic IS half of this test. The other half:
        // the only frame (the root, flat) is dropped whole — not one row
        // half hydrated, not one published twice — and the stream ends in
        // the same `Cancelled` as if the token had come already fired.
        assert_eq!(items, vec![Err(CompareError::Cancelled)], "{items:#?}");
    }

    /// A file that DISAPPEARS between the `list` and the `stat` comes out
    /// as an error row, and it is a decision.
    ///
    /// It is a real race —`/tmp`, a build directory— and a provider's
    /// listing resolves it the other way around:
    /// `norte-vfs-local::list_with` omits the entry that vanished so as not
    /// to kill a live directory's listing. Here it cannot be omitted: the
    /// pair is already paired, and staying silent about it would remove
    /// from the panel a row the other side does have. This test fixes that
    /// decision so changing it costs a discussion.
    #[tokio::test]
    async fn a_file_that_disappears_between_the_list_and_the_stat_is_an_error() {
        let l = LazyProvider::failing(tree(&["a.txt"]).await, norte_proto::Error::NotFound);
        let r = LazyProvider::new(tree(&["a.txt"]).await);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].verdict, CompareVerdict::Error, "{rows:#?}");
        assert_eq!(rows[0].reason, Some(CompareReason::Unreadable));
        assert_eq!(rows[0].side, Some(Side::Left));
    }

    /// The error row carries what the side that DID answer said.
    ///
    /// The right fails, the left does not: its size was found out and is
    /// true, and the panel paints that cell. Clearing it would be throwing
    /// away an answer that was obtained.
    #[tokio::test]
    async fn a_broken_stats_row_keeps_the_side_that_answered() {
        let l = LazyProvider::new(tree(&["a.txt"]).await);
        let r = LazyProvider::failing(
            tree(&["a.txt"]).await,
            norte_proto::Error::Io { retryable: false },
        );

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].side, Some(Side::Right));
        assert_eq!(
            rows[0].left.as_ref().expect("the side that answered").size,
            Some(b"a.txt".len() as u64),
            "what was learned travels in the row"
        );
        assert!(
            rows[0]
                .right
                .as_ref()
                .expect("the broken side")
                .size
                .is_none(),
            "and nothing is invented for the one that did not answer"
        );
    }

    /// The cut between rungs: with the size already known and different,
    /// the date is not asked.
    ///
    /// Only visible with a provider that fills in `size` and not
    /// `mtime_ms` —the shape of an SFTP server that does not send
    /// `ACMODTIME`—: with a fully lazy one, the size rung's `stat` already
    /// brings the date and the cut saves nothing measurable.
    #[tokio::test]
    async fn a_size_that_already_decides_does_not_pay_the_dates_stat() {
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.txt", b"aa").await;
        seed(&r, "a.txt", b"aaaaaaaaaa").await;
        let l = LazyProvider::only_mtime_missing(l);
        let r = LazyProvider::only_mtime_missing(r);

        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].verdict, rows[0].criterion),
            (CompareVerdict::Different, CompareCriterion::Size),
            "{rows:#?}"
        );
        assert_eq!(
            (l.stats(), r.stats()),
            (0, 0),
            "the size was in the listing and already decided: the date is not asked"
        );

        // And when the size does NOT decide, the date IS asked: one `stat`
        // per side, not two.
        let l = MemProvider::new();
        let r = MemProvider::new();
        seed(&l, "a.txt", b"aa").await;
        seed(&r, "a.txt", b"bb").await;
        let l = LazyProvider::only_mtime_missing(l);
        let r = LazyProvider::only_mtime_missing(r);
        let rows = collect(compare_lazy(&l, &r, CompareOptions::cheap())).await;
        assert_eq!(rows[0].criterion, CompareCriterion::Mtime, "{rows:#?}");
        assert_eq!((l.stats(), r.stats()), (1, 1));
    }

    /// Hydration checks the token BEFORE each `stat` (hard rule 3).
    ///
    /// Tested directly on [`hydrate`], same as draining a directory is
    /// tested on `list_all`: through the stream it would need cancelling
    /// halfway through an `await`, which is a race.
    #[tokio::test]
    async fn hydration_checks_the_token_before_asking() {
        let mem = LazyProvider::new(tree(&["a.txt"]).await);
        let entry = Entry {
            path: at("a.txt"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: BTreeMap::new(),
        };

        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut fresh = Fresh::of(&entry);
        let outcome = hydrate(
            &mem,
            &mut fresh,
            Side::Left,
            CompareCriterion::Size,
            &cancel,
        )
        .await;
        assert!(matches!(outcome, Err(HydrationFailure::Cancelled)));
        assert_eq!(mem.stats(), 0, "not even asked");

        // And without cancelling, the same side hydrates only once: the
        // second rung reuses the first's `stat`.
        let mut fresh = Fresh::of(&entry);
        let live = CancellationToken::new();
        for rung in [CompareCriterion::Size, CompareCriterion::Mtime] {
            hydrate(&mem, &mut fresh, Side::Left, rung, &live)
                .await
                .expect("hydrate");
        }
        assert_eq!(mem.stats(), 1, "one `stat` per side and per pair");
        assert_eq!(fresh.entry.size, Some(b"a.txt".len() as u64));
        assert!(fresh.entry.mtime_ms.is_some());
    }

    // ---------- the provider that really cannot answer ----------

    /// A REAL tar, indexed by the archive provider.
    ///
    /// The container lives on a `MemProvider` because what is being tested
    /// is the archive, not the filesystem that stores it.
    async fn tar_provider(bytes: &[u8]) -> (ArchiveProvider, VPath) {
        let mem = Arc::new(MemProvider::new());
        let container = MemProvider::root().join(Segment::new(b"f.tar".to_vec()).expect("seg"));
        let mut sink = mem.write(&container).await.expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        let root = VPath::archive_compose("tar", &container, &[]).expect("compose");
        let provider = ArchiveProvider::new(mem as Arc<dyn Provider>, Format::Tar, "tar+mem");
        (provider, root)
    }

    /// `Unknown` demanded from a provider that REALLY cannot answer, not
    /// from a mock told what to say.
    ///
    /// The tar carries a link whose header brings no target —any careless
    /// producer writes them, and the archive provider already has its path
    /// for that: `read_link` answers `Corrupt`—. Without both targets there
    /// is no comparison possible, and the honest answer is
    /// `Same`/`LinkTarget`/`Unknown`: inventing a difference would be as
    /// false as inventing an equality, and calling it an error would be
    /// saying the comparison failed when what happens is that it is not
    /// known.
    ///
    /// The OTHER side lists LAZILY (like `norte-vfs-local`, #52) on
    /// purpose. With a side that fills in the fields, this test also
    /// passed with the C7b bug still there: an `Unknown` produced because
    /// nobody hydrated the size looks the same as one produced by a
    /// targetless link. With a lazy side it no longer does: the file pair
    /// has to come out `Certain`, so the link's `Unknown` can only come
    /// from what really is not known.
    #[tokio::test]
    async fn a_real_archive_produces_unknown_rather_than_a_guess() {
        let bytes = TarSmith::new()
            .file(b"a.txt", b"hello")
            .symlink(b"link", b"")
            .build();
        let (zip, zip_root) = tar_provider(&bytes).await;

        let mem = MemProvider::new();
        seed(&mem, "a.txt", b"hello world").await;
        mem.symlink(
            &MemProvider::root().join(Segment::new(b"link".to_vec()).expect("seg")),
            b"../x",
            norte_vfs::SymlinkKind::File,
        )
        .await
        .expect("symlink");
        let local = LazyProvider::new(mem);

        let sides = Sides::from_capabilities(local.capabilities(), zip.capabilities());
        let stream = compare(
            &local,
            &MemProvider::root(),
            &zip,
            &zip_root,
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        );
        let rows = collect(stream).await;

        let row = rows
            .iter()
            .find(|row| named(row, b"link"))
            .expect("the paired row");
        assert_eq!(row.confidence, CompareConfidence::Unknown, "{rows:#?}");
        assert_eq!(row.criterion, CompareCriterion::LinkTarget);
        assert_ne!(
            row.verdict,
            CompareVerdict::Error,
            "unknown is an answer, not a failure"
        );
        assert!(
            row.left.as_ref().expect("this side's link").size.is_none(),
            "the link's `stat` was not spent: its target decides it"
        );

        // And what the archive CAN answer is answered WITH CERTAINTY: 11
        // bytes against 5, with the left's size hydrated by hand.
        let file = rows
            .iter()
            .find(|row| named(row, b"a.txt"))
            .expect("the normal pair");
        assert_eq!(
            (file.verdict, file.criterion, file.confidence),
            (
                CompareVerdict::Different,
                CompareCriterion::Size,
                CompareConfidence::Certain
            ),
            "{rows:#?}"
        );
        assert_eq!(file.left.as_ref().expect("this side's file").size, Some(11));
        assert_eq!(local.stats(), 1, "only the file pair is statted");
    }

    #[tokio::test]
    async fn polling_past_the_end_gives_none_instead_of_panicking() {
        // `futures`'s raw `Unfold` PANICS if polled after `None`, and any
        // loop with `select!` and a flush tick does that (#175).
        // `compare()`'s `.fuse()` is what prevents it — same as in
        // `norte_sync::plan`, which solved the same problem first.
        let left = MemProvider::new();
        let right = MemProvider::new();
        let mut stream = compare_default(&left, &right);
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none(), "and again, no panic");
        assert!(stream.is_terminated());
    }
}
