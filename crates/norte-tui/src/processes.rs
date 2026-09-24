//! The processes pane (phase A): the cursor, and nothing else.
//!
//! The state lives in `norte-frontend`, not here: clamping the cursor on READ
//! is the answer to "which row would be cancelled?", and both surfaces answer
//! that question (ADR 0077). What stays in this crate is the `KIND`, which is
//! indeed terminal-specific — it is what its layout writes.

/// The kind that occupies a processes slot.
pub const KIND: &str = "processes";

pub use norte_frontend::processes::Processes;
