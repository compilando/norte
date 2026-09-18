//! Getting somewhere: go-anywhere, the history, places, the directory tree
//! and watching what is on screen.
//!
//! This folder groups source files. It is not a public path: each module is
//! re-exported at the crate root (`norte_frontend::goto`), which stays the
//! only way to name it.

pub mod goto;
pub mod history;
pub mod places;
pub mod tree;
pub mod watch;
