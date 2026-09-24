//! Batch rename inside ONE directory (§17): a pure planner and a transactional
//! executor. AI rename feeds this, and the rules engine (counters, slices,
//! regex, case, cleanup) will feed it too — it only produces pairs.

pub mod exec;
// `pub(crate)` and not private since #274: the spelling change of `ops` and
// its undo need the SAME temporary prefix as the batch executor. Two
// grammars for machinery names are two things to sweep.
pub(crate) mod naming;
pub mod plan;

use norte_proto::{Error, Segment, methods};

// The planner's whole public surface, re-exported here so the executor task
// (§5) and the daemon dispatch (§7) import it from ONE place and a later split
// of `plan.rs` does not touch either of them.
pub use exec::{BatchReport, DirPlan, StuckStep};
pub use plan::{Collision, CollisionKind, NameCaps, RenamePlan, Step, name_key, plan_batch};

/// One requested rename as the planner takes it: `(from, to)` base names in
/// RAW BYTES (rule 1 — a filename is bytes, and the wire's `Segment` has
/// already vouched that each side is one directory entry).
pub type PairBytes = (Vec<u8>, Vec<u8>);

/// Wire pairs → planner pairs (raw bytes, rule 1).
///
/// The [`Segment`] invariant (one directory entry, no `/`, no NUL, not `.` or
/// `..`) already ran during deserialisation, which is exactly why the wire type
/// is a `Segment` and not a `String`: by the time a name reaches the planner
/// there is nothing left to check and nothing left to decode.
///
/// It lives here rather than in the daemon because BOTH the socket dispatch and
/// `Backend`'s embedded arm need it, and a second copy of a proto↔core mapping
/// is a second place for the two to drift.
///
/// ```
/// use norte_proto::{Segment, methods::RenamePair};
/// let pairs = [RenamePair {
///     from: Segment::new(b"caf\xff.txt".to_vec()).expect("segment"),
///     to: Segment::new(b"cafe.txt".to_vec()).expect("segment"),
/// }];
/// let raw = norte_core::rename::pairs_from_wire(&pairs);
/// assert_eq!(raw, vec![(b"caf\xff.txt".to_vec(), b"cafe.txt".to_vec())]);
/// ```
#[must_use]
pub fn pairs_from_wire(pairs: &[methods::RenamePair]) -> Vec<PairBytes> {
    pairs
        .iter()
        .map(|p| (p.from.as_bytes().to_vec(), p.to.as_bytes().to_vec()))
        .collect()
}

/// Core verdict → wire verdict.
///
/// EXHAUSTIVE on purpose, with no wildcard arm: a verdict added to one side and
/// not the other has to fail to compile. [`methods::RenameCollisionKind`] also
/// carries an `Unknown` variant, which is a DESERIALISATION fallback for a
/// verdict from a newer protocol (ADR 0004) — the core never emits it, and this
/// function is the only place that could.
fn kind_to_proto(kind: CollisionKind) -> methods::RenameCollisionKind {
    use methods::RenameCollisionKind as K;
    match kind {
        CollisionKind::Internal => K::Internal,
        CollisionKind::External => K::External,
        CollisionKind::AbsentSource => K::AbsentSource,
        CollisionKind::AmbiguousSource => K::AmbiguousSource,
    }
}

/// Planner plan → wire plan.
///
/// The hash that goes out is the [`DirPlan`]'s, the one BOUND to the directory
/// the plan was made against — never [`RenamePlan`]'s own, which is the same
/// shape of string and would let a hash a human approved for `~/photos` be
/// replayed against `/etc`.
///
/// # Errors
/// [`Error::Internal`] (`panic: false`) if some name in the plan is not a legal
/// directory entry. It cannot happen — every name here came either from a
/// `Segment` on the way in or from a directory listing — and it is reported
/// rather than dropped anyway: a plan silently missing a step or a verdict is a
/// plan that says something the core did not decide.
pub fn plan_to_proto(plan: &DirPlan) -> Result<methods::FsRenameBatchPlanResult, Error> {
    // `what` says WHERE (step or verdict) and at which index: a "cannot
    // happen" error is exactly the kind that has to be diagnosable from a
    // single log line, and `Error::Internal` looks like any other.
    fn seg(bytes: &[u8], what: &str, index: usize) -> Result<Segment, Error> {
        Segment::new(bytes.to_vec()).map_err(|e| {
            tracing::error!(
                error = %e,
                what,
                index,
                "a name in the plan is not a directory entry"
            );
            Error::Internal { panic: false }
        })
    }
    let inner = plan.plan();
    Ok(methods::FsRenameBatchPlanResult {
        steps: inner
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                Ok(methods::RenameStep {
                    from: seg(&s.from, "step.from", i)?,
                    to: seg(&s.to, "step.to", i)?,
                    temp: s.temp,
                })
            })
            .collect::<Result<_, Error>>()?,
        collisions: inner
            .collisions
            .iter()
            .enumerate()
            .map(|(i, c)| {
                Ok(methods::RenameCollision {
                    pair_index: c.pair_index,
                    name: seg(&c.name, "collision.name", i)?,
                    kind: kind_to_proto(c.kind),
                })
            })
            .collect::<Result<_, Error>>()?,
        executable: plan.executable(),
        plan_hash: plan.hash().clone(),
    })
}

/// A stuck step → its wire form. Shared by the batch report and by
/// [`methods::PolicyUndoReportResult::batch_stuck`], because undoing a batch
/// gets stuck in exactly the same way as running one.
#[must_use]
pub fn stuck_to_proto(s: &StuckStep) -> methods::RenameStuckStep {
    methods::RenameStuckStep {
        from: s.from.clone(),
        to: s.to.clone(),
        pair_index: s.pair_index,
        error: s.error.clone(),
        journalled: s.journalled,
        still_applied: s.still_applied,
    }
}

/// Executor report → wire report ([`methods::FS_RENAME_BATCH_REPORT`]).
///
/// Everything here is either a counter or a place to look. Nothing is dropped
/// on the way out: the whole point of the report is that a batch which could
/// not clean up after itself says so out loud instead of answering a bare
/// error.
#[must_use]
pub fn report_to_proto(r: &BatchReport) -> methods::FsRenameBatchReportResult {
    methods::FsRenameBatchReportResult {
        applied: r.applied,
        rolled_back: r.rolled_back,
        failed_pair: r.failed_pair,
        stuck: r.stuck.as_ref().map(stuck_to_proto),
        uncertain: r.uncertain.as_ref().map(stuck_to_proto),
        compensations_lost: r.compensations_lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLOSED vocabulary of rename verdicts, pinned the way
    /// `deny_reason_rule_ids_are_the_closed_wire_vocabulary` pins the policy
    /// one: every core verdict has a wire twin, and the wire's `Unknown` — the
    /// N+1 deserialisation fallback — is NOT among the things the core can
    /// produce. Adding a verdict to either side without the other breaks
    /// `kind_to_proto`'s exhaustive match at COMPILE time; this test is what
    /// catches the other direction, a wire variant nobody mapped.
    #[test]
    fn collision_kinds_are_the_closed_wire_vocabulary() {
        use methods::RenameCollisionKind as K;
        let mapped: Vec<K> = [
            CollisionKind::Internal,
            CollisionKind::External,
            CollisionKind::AbsentSource,
            CollisionKind::AmbiguousSource,
        ]
        .into_iter()
        .map(kind_to_proto)
        .collect();
        assert_eq!(
            mapped,
            vec![
                K::Internal,
                K::External,
                K::AbsentSource,
                K::AmbiguousSource
            ],
        );
        // Every wire verdict, except the fallback, comes from a core
        // verdict. A `K::Unknown` produced by the core would be the core
        // inventing a class it doesn't even understand itself.
        for wire in [
            K::Internal,
            K::External,
            K::AbsentSource,
            K::AmbiguousSource,
        ] {
            assert!(mapped.contains(&wire), "{wire:?} has no core verdict");
        }
        assert!(
            !mapped.contains(&K::Unknown),
            "the core never emits Unknown"
        );
    }

    /// The wire hash is the DIRECTORY-bound one: the same pairs against two
    /// directories that plan identically must not answer the same token.
    #[test]
    fn the_wire_hash_is_bound_to_the_directory() {
        let pairs = vec![(b"a".to_vec(), b"x".to_vec())];
        let listing = vec![b"a".to_vec()];
        let caps = NameCaps {
            fold: norte_encoding::FoldMode::None,
        };
        let plan = plan_batch(&pairs, &listing, caps);
        let here = DirPlan::bind(
            &norte_proto::VPath::parse("mem:///here").expect("path"),
            plan,
        );
        let there = DirPlan::bind(
            &norte_proto::VPath::parse("mem:///there").expect("path"),
            plan_batch(&pairs, &listing, caps),
        );
        let a = plan_to_proto(&here).expect("wire plan");
        let b = plan_to_proto(&there).expect("wire plan");
        assert_eq!(a.steps, b.steps, "the steps ARE the same");
        assert_ne!(
            a.plan_hash, b.plan_hash,
            "the token is bound to the directory"
        );
    }
}
