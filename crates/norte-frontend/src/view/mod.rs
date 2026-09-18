//! How things are shown: the columns of a listing, names and sizes on
//! screen, the viewer, the disk map and its treemap.
//!
//! This folder groups source files. It is not a public path: each module is
//! re-exported at the crate root (`norte_frontend::viewer`), which stays the
//! only way to name it.

pub mod columns;
pub mod diskmap;
pub mod display;
pub mod format;
pub mod treemap;
pub mod viewer;
