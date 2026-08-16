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

pub mod ansi;
pub mod availability;
pub mod cli;
pub mod columns;
pub mod columns_picker;
pub mod compare;
pub mod config;
pub mod confine;
mod decoration;
mod display;
pub mod error;
mod format;
pub mod help;
pub mod help_badge;
pub mod keymap;
pub mod keysheet;
mod modal;
pub mod mouse;
pub mod nav;
pub mod openers;
pub mod palette;
mod pane;
pub mod plugin_config;
pub mod settings;
pub mod shell;
pub mod shortcuts;
mod sort;
pub mod space;
pub mod sync;
pub mod theme;
pub mod viewer;
pub mod viewport;
pub mod watch;
pub mod whichkey;

pub use decoration::{BADGE_MAX_CHARS, Decoration, merge_decorations, sanitize_decoration};
pub use display::{
    cells, display_name, display_name_with, middle_ellipsis, path_display, path_display_with,
};
pub use format::human_bytes;
pub use modal::{
    AI_RENAME_PAIR_LIMIT, BatchPlan, MAX_AI_PLAN_ENTRIES, MODAL_ITEM_LIMIT, RENAME_COLLISION_LIMIT,
    SEMANTIC_HIT_LIMIT, SEMANTIC_K, collision_kind_key, item_lines, item_lines_with, rename_pairs,
    validate_ai_plan, validate_semantic_hits,
};
pub use pane::{DEFAULT_PAGE, PaneState, PatternError};
pub use sort::{SortColumn, SortDir, SortSpec, sort_entries, sort_entries_with};
