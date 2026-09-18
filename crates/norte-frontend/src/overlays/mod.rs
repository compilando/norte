//! What opens on top of the panels and asks for a choice: the palette,
//! which-key, the wizard and the pickers.
//!
//! This folder groups source files. It is not a public path: each module is
//! re-exported at the crate root (`norte_frontend::palette`), which stays the
//! only way to name it.

pub mod columns_picker;
pub mod connections_picker;
pub mod layout_picker;
pub mod palette;
pub mod palette_state;
pub mod profile_picker;
pub mod whichkey;
pub mod wizard;
