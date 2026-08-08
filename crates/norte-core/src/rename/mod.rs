//! Batch rename inside ONE directory (§17): a pure planner and a transactional
//! executor. AI rename feeds this, and the rules engine (counters, slices,
//! regex, case, cleanup) will feed it too — it only produces pairs.

mod naming;
pub mod plan;
