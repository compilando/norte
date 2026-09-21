//! What frames the panels: the menu bar, the key bar, the footer, the panel
//! bars, the banners, the splash and the shared frame.
//!
//! This folder groups source files. It is not a public path: each module is
//! re-exported at the crate root (`norte_frontend::menu`), which stays the
//! only way to name it.

pub mod banners;
pub mod footer;
pub mod frame;
pub mod keybar;
pub mod menu;
pub mod panelbar;
pub mod splash;
pub mod statusbar;
