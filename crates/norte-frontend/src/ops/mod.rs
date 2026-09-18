//! Operations on the reader's files that need frontend-side state: what to
//! ask for, how to preview it, how to report it. The operation itself runs in
//! the core.
//!
//! This folder groups source files. It is not a public path: each module is
//! re-exported at the crate root (`norte_frontend::chmod`), which stays the
//! only way to name it.

pub mod checksums;
pub mod chmod;
pub mod compare;
pub mod diffpair;
pub mod organize;
pub mod rename_pattern;
