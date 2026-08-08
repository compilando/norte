//! Batch rename inside ONE directory (§17): a pure planner and a transactional
//! executor. AI rename feeds this, and the rules engine (counters, slices,
//! regex, case, cleanup) will feed it too — it only produces pairs.

pub mod exec;
mod naming;
pub mod plan;

// The planner's whole public surface, re-exported here so the executor task
// (§5) and the daemon dispatch (§7) import it from ONE place and a later split
// of `plan.rs` does not touch either of them.
pub use exec::{BatchReport, DirPlan, StuckStep};
pub use plan::{Collision, CollisionKind, NameCaps, RenamePlan, Step, name_key, plan_batch};
