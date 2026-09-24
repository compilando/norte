//! Organizing a directory (WOW program phase 8): a plan that MOVES into
//! subdirectories, and therefore also creates them.
//!
//! It's the sibling of batch rename, and the difference fits in one
//! sentence: there the destination is a name, here it's a relative path.
//! That drags along three things that cannot be borrowed from the other:
//!
//! - **The path has to be validated.** `proposed_rel` is written by a third
//!   party —a model or a plugin— and a `..` in there is a write outside the
//!   directory the human was looking at. Checked by
//!   [`norte_proto::methods::validar_proposed_rel`], which lives in the
//!   protocol so the core and the frontends apply the SAME rule.
//! - **Folders have to be created**, and they have to go in the same batch
//!   as the moves: otherwise undo returns the files and forgets the
//!   directories.
//! - **There is no common directory.** That's why the journal marks the
//!   moves with [`crate::OP_ORGANIZED`] and not with `renamed`: without
//!   that mark, undo would mistake them for a single-directory batch rename
//!   and undo them against the wrong one.

use std::collections::BTreeSet;

use norte_proto::methods::{OrganizeMove, PlanHash};
use norte_proto::{Error, Segment, VPath};

use crate::hashing::hex_lower;

/// An organize plan already validated and bound to its directory.
///
/// This type existing is what prevents applying a plan nobody reviewed:
/// [`Engine::organize`](crate::Engine::organize) only accepts the
/// `plan_hash` that comes out of here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizePlan {
    /// The moves, with their destination already split into segments.
    steps: Vec<OrganizeStep>,
    /// The token that has to be returned to apply it.
    hash: PlanHash,
}

/// A validated move: where it comes from and where it goes, already in
/// segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizeStep {
    /// The name it currently has, inside the plan's directory.
    pub current: Segment,
    /// The destination, relative to the same directory. At least one
    /// segment; the middle ones are folders that may need creating.
    pub rel: Vec<Segment>,
}

impl OrganizeStep {
    /// The absolute destination path, hanging off `dir`.
    #[must_use]
    pub fn dest(&self, dir: &VPath) -> VPath {
        let mut p = dir.clone();
        for s in &self.rel {
            p = p.join(s.clone());
        }
        p
    }

    /// The folders this step needs under `dir`, from shallowest to deepest.
    /// The LAST segment is the file and is not included.
    #[must_use]
    pub fn folders(&self, dir: &VPath) -> Vec<VPath> {
        let mut out = Vec::new();
        let mut p = dir.clone();
        for s in self.rel.iter().take(self.rel.len().saturating_sub(1)) {
            p = p.join(s.clone());
            out.push(p.clone());
        }
        out
    }
}

impl OrganizePlan {
    /// Validates `moves` against `dir` and binds the plan to that directory.
    ///
    /// A single invalid destination brings down the WHOLE plan, and that's
    /// deliberate: a plan is an intention a human approves all at once, and
    /// applying "what could be done" from a proposal that carried a `..`
    /// would mean keeping half of something nobody reviewed. Fail-loud, the
    /// same way the rename plan does with hostile names.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if some `current` is not a valid name or some
    /// `proposed_rel` does not pass
    /// [`norte_proto::methods::validar_proposed_rel`]; also if two moves
    /// collide —the same origin twice, or two equal destinations—, which is
    /// a plan that cannot be fulfilled in full.
    pub fn bind(dir: &VPath, moves: &[OrganizeMove]) -> Result<Self, Error> {
        let mut steps = Vec::with_capacity(moves.len());
        let mut origins: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut destinations: BTreeSet<Vec<Vec<u8>>> = BTreeSet::new();
        for m in moves {
            let current = Segment::new(m.current.as_bytes()).map_err(|_| Error::InvalidPath)?;
            let rel = norte_proto::methods::validar_proposed_rel(&m.proposed_rel)
                .map_err(|_| Error::InvalidPath)?;
            // A repeated origin is a self-contradicting plan; two equal
            // destinations, one that loses a file. Both are rejected
            // BEFORE touching anything: halfway through there's no longer
            // a plan to review.
            if !origins.insert(current.as_bytes().to_vec()) {
                return Err(Error::InvalidPath);
            }
            let key: Vec<Vec<u8>> = rel.iter().map(|s| s.as_bytes().to_vec()).collect();
            if !destinations.insert(key) {
                return Err(Error::InvalidPath);
            }
            steps.push(OrganizeStep { current, rel });
        }
        let hash = hash_of(dir, &steps);
        Ok(Self { steps, hash })
    }

    /// The validated steps.
    #[must_use]
    pub fn steps(&self) -> &[OrganizeStep] {
        &self.steps
    }

    /// The token that has to be returned to apply it.
    #[must_use]
    pub fn hash(&self) -> &PlanHash {
        &self.hash
    }

    /// Every folder the plan needs under `dir`, without repeats and from
    /// shallowest to deepest.
    ///
    /// Order matters: creating `a/b` before `a` fails on any provider that
    /// does not create parents on its own, and a provider cannot be asked
    /// to do that —the rule is that the core knows what it created, so it
    /// can undo it—.
    #[must_use]
    pub fn folders(&self, dir: &VPath) -> Vec<VPath> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<VPath> = Vec::new();
        for step in &self.steps {
            for c in step.folders(dir) {
                if seen.insert(c.to_wire()) {
                    out.push(c);
                }
            }
        }
        // By depth: the parent before the child. `segments().count()` is
        // the number of segments, i.e. exactly the depth.
        out.sort_by_key(|p| p.segments().count());
        out
    }
}

/// The token for a freshly proposed plan, ready to travel WITH it.
///
/// It exists because whoever proposes and whoever applies are separated by
/// the wire: a plan with no token cannot be approved, and computing it on
/// the client would put the digest algorithm in two places —which is
/// exactly how they'd stop matching one day and `fs.organize` would answer
/// `PlanStale` for a plan nobody touched—.
///
/// # Errors
/// Whatever [`OrganizePlan::bind`] rejects: a plan with an invalid or
/// self-contradicting destination has no token, because it will not be
/// possible to apply it.
pub fn plan_hash(dir: &VPath, moves: &[OrganizeMove]) -> Result<PlanHash, Error> {
    OrganizePlan::bind(dir, moves).map(|p| p.hash)
}

/// A plan's hash, bound to its directory.
///
/// Same criterion as rename's `DirPlan::bind` —its own domain so this digest
/// cannot collide with another one in the core over the same bytes— and
/// with the steps inside, so changing a destination invalidates the token
/// the human approved.
fn hash_of(dir: &VPath, steps: &[OrganizeStep]) -> PlanHash {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"norte-organize-plan-dir-v1");
    feed(&mut h, dir.to_wire().as_bytes());
    for step in steps {
        feed(&mut h, step.current.as_bytes());
        // How many segments, and then each one: without the count, `a/b`
        // and `ab` could feed the same bytes.
        feed(&mut h, &(step.rel.len() as u64).to_le_bytes());
        for s in &step.rel {
            feed(&mut h, s.as_bytes());
        }
    }
    // INVARIANT: the lowercase hex of a sha256 is 64 digits, which is the
    // entire contract of `PlanHash`.
    PlanHash::parse(&hex_lower(&h.finalize())).expect("a sha256 in hex is a PlanHash")
}

/// Feeds a chunk with its length in front, so two different chunks cannot
/// produce the same byte string.
fn feed(h: &mut impl sha2::Digest, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::{OrganizeMove, OrganizePlan};
    use norte_proto::VPath;

    fn dir() -> VPath {
        VPath::parse("mem:///downloads").expect("wire")
    }

    fn mov(current: &str, rel: &str) -> OrganizeMove {
        OrganizeMove {
            current: current.to_owned(),
            proposed_rel: rel.to_owned(),
        }
    }

    /// **A destination that escapes the directory brings down the WHOLE plan.**
    ///
    /// This is the whole phase's security property: `proposed_rel` is
    /// written by a model or a plugin, and a `..` in there is a write
    /// outside what the human was looking at. And it brings down the whole
    /// plan, not just that step: applying "what could be done" from a
    /// proposal carrying that would mean keeping half of something nobody
    /// reviewed.
    #[test]
    fn a_destination_that_escapes_brings_down_the_whole_plan() {
        for bad in [
            "../outside.txt",
            "a/../../outside.txt",
            "/etc/passwd",
            "",
            "a//b",
            "a/",
            "./x",
        ] {
            let r = OrganizePlan::bind(&dir(), &[mov("good.txt", "ok/good.txt"), mov("x", bad)]);
            assert!(r.is_err(), "«{bad}» should be rejected");
        }
    }

    /// Two moves that collide —same origin, or same destination— are a plan
    /// that cannot be fulfilled in full, and it's rejected before touching
    /// anything.
    #[test]
    fn a_self_contradicting_plan_is_rejected() {
        let same_origin = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a"), mov("a.txt", "y/a")]);
        assert!(same_origin.is_err());
        let same_destination =
            OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a"), mov("b.txt", "x/a")]);
        assert!(same_destination.is_err());
    }

    /// The folders come out without repeats and with the PARENT BEFORE THE
    /// CHILD: creating `a/b` before `a` fails on any provider that doesn't
    /// invent parents.
    #[test]
    fn folders_go_from_shallowest_to_deepest() {
        let plan = OrganizePlan::bind(
            &dir(),
            &[
                mov("one.pdf", "invoices/2026/march/one.pdf"),
                mov("two.pdf", "invoices/2026/april/two.pdf"),
                mov("three.txt", "notes/three.txt"),
            ],
        )
        .expect("valid plan");
        let folders: Vec<String> = plan
            .folders(&dir())
            .iter()
            .map(|p| p.to_wire().replace("mem:///downloads/", ""))
            .collect();
        assert_eq!(
            folders,
            vec![
                "invoices",
                "notes",
                "invoices/2026",
                "invoices/2026/march",
                "invoices/2026/april"
            ],
            "no `invoices` repeated, and each parent before its child"
        );
    }

    /// A destination with NO subdirectory is a plain rename, and that's
    /// fine: the same screen serves both organizing and renaming along the
    /// way.
    #[test]
    fn a_destination_with_no_folder_is_a_rename() {
        let plan = OrganizePlan::bind(&dir(), &[mov("a.txt", "b.txt")]).expect("valid plan");
        assert!(plan.folders(&dir()).is_empty());
        assert_eq!(
            plan.steps()[0].dest(&dir()).to_wire(),
            "mem:///downloads/b.txt"
        );
    }

    /// The hash BINDS the plan to its directory and to its steps: changing
    /// either one invalidates the token the human approved.
    #[test]
    fn the_hash_binds_the_plan_to_the_directory_and_the_steps() {
        let a = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a.txt")]).expect("plan");
        let same = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a.txt")]).expect("plan");
        assert_eq!(a.hash(), same.hash(), "the same plan, the same token");

        let other_dir = OrganizePlan::bind(
            &VPath::parse("mem:///other").expect("wire"),
            &[mov("a.txt", "x/a.txt")],
        )
        .expect("plan");
        assert_ne!(
            a.hash(),
            other_dir.hash(),
            "another directory, another token"
        );

        let other_destination =
            OrganizePlan::bind(&dir(), &[mov("a.txt", "y/a.txt")]).expect("plan");
        assert_ne!(a.hash(), other_destination.hash());
    }

    /// And the segments go with their length in front, so `a/b` and `ab`
    /// cannot feed the same bytes.
    #[test]
    fn two_different_plans_do_not_share_a_hash_by_concatenation() {
        let split = OrganizePlan::bind(&dir(), &[mov("f", "a/b")]).expect("plan");
        let joined = OrganizePlan::bind(&dir(), &[mov("f", "ab")]).expect("plan");
        assert_ne!(split.hash(), joined.hash());
    }
}
