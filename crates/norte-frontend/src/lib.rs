//! Lógica de presentación PURA compartida por los frontends de norte (TUI y
//! GUI) — sin dependencias de UI. Aquí vive el saneado de nombres hostiles
//! ([`display_name`]/[`path_display`], spec §6: display siempre lossy y
//! MARCADO) y el orden del listado ([`sort_entries`], spec §6.1). Ni un
//! frontend directo (TUI sobre ratatui) ni uno de GPU (GUI sobre GPUI) deben
//! reimplementar este criterio: los bytes de un nombre no cambian de
//! naturaleza por el backend de render.
//!
//! El crate NO conoce ningún framework de render: opera sobre bytes crudos y
//! [`norte_proto`] y devuelve `String` + un flag `hostil`; el BADGE que marca
//! un nombre alterado lo aplica cada frontend en su capa de render.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod display;
pub mod keymap;
pub mod nav;
mod pane;
mod sort;
pub mod viewer;

pub use display::{display_name, path_display};
pub use pane::PaneState;
pub use sort::sort_entries;
