//! PURE presentation logic shared by norte's frontends (TUI and GUI) — with no
//! UI dependencies. This is where the sanitizing of hostile names lives
//! ([`display_name`]/[`path_display`], spec §6: display is always lossy and
//! MARKED) and the listing's order ([`sort_entries`], spec §6.1). Neither a
//! direct frontend (the TUI over ratatui) nor a GPU one (the GUI over GPUI)
//! should reimplement this criterion: a name's bytes do not change nature
//! because of the render backend.
//!
//! The crate knows NO render framework: it operates on raw bytes and
//! [`norte_proto`] and returns a `String` plus a `hostile` flag; the BADGE
//! that marks an altered name is applied by each frontend in its render
//! layer.
//!
//! # "Pure" stopped being accurate, and it is worth saying so
//!
//! Since item 7 of the post-alpha roadmap, [`watch`] lives here, watching
//! directories: it brings in `notify`, a `tokio` dependency and a background
//! task. It is not presentation. It is here because it is FRONTEND
//! INFRASTRUCTURE independent of the toolkit — the same as the rest of the
//! crate, with a different kind of content — and because the alternative was
//! a crate whose entire content is one file with two consumers.
//!
//! The rule that still holds, and the one that matters, is hard rule 7: there
//! is no BUSINESS logic here. Watching a directory decides nothing about the
//! files; it says something changed, and the frontend is the one that
//! decides what to do, the same path as its manual refresh.
//!
//! # Where everything is
//!
//! The source is grouped into folders by what it DOES: `ops/` (operations on
//! files), `navigation/` (going places), `chrome/` (what frames the panes),
//! `overlays/` (what opens on top and asks for a choice) and `view/` (how it
//! is shown). The folders are PRIVATE: every module is re-exported at the
//! root (`norte_frontend::chmod`), and that stays the only public path. A new
//! module goes in its group's folder and is re-exported here; one that fits
//! none stays at the root.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod chrome;
mod navigation;
mod ops;
mod overlays;
mod view;

pub use chrome::{banners, footer, frame, keybar, layoutbar, menu, panelbar, splash, statusbar};
pub use navigation::{goto, history, places, tree, watch};
pub use ops::{checksums, chmod, compare, diffpair, organize, rename_pattern};
pub use overlays::{
    columns_picker, connections_picker, layout_picker, palette, palette_state, profile_picker,
    whichkey, wizard,
};
pub use view::{columns, diskmap, display, format, treemap, viewer};

pub mod ansi;
pub mod availability;
pub mod broken_plugin;
pub mod busy;
pub mod cli;
pub mod config;
pub mod confine;
mod decoration;
pub mod error;
pub mod handoff;
pub mod help;
pub mod help_badge;
pub mod help_chords;
pub mod keymap;
pub mod keysheet;
pub mod layout;
pub mod logpanel;
pub mod metadata;
mod modal;
pub mod mouse;
pub mod nav;
pub mod notes;
pub mod openers;
mod pane;
pub mod plugin_config;
pub mod processes;
pub mod search;
pub mod search_status;
pub mod secret;
pub mod session;
pub mod settings;
pub mod shell;
pub mod shortcuts;
mod sort;
pub mod space;
pub mod subshell;
pub mod sync;
pub mod task_strip;
pub mod tasks;
pub mod theme;
pub mod timeline;
pub mod version;
pub mod viewport;

pub use decoration::{
    BADGE_MAX_CHARS, Decoration, merge_decorations, sanitize_decoration, sanitize_icon,
};
pub use display::{
    cells, display_name, display_name_with, display_os_name, ellipsis_at_bytes, middle_ellipsis,
    path_display, path_display_with, skip_cells,
};
pub use format::{human_bytes, human_bytes_short};
pub use modal::{
    AI_RENAME_PAIR_LIMIT, BatchPlan, DetailPart, MAX_AI_PLAN_ENTRIES, MODAL_ITEM_LIMIT,
    RENAME_COLLISION_LIMIT, ReportLine, SEMANTIC_HIT_LIMIT, SEMANTIC_K, approval_ready,
    batch_report_is_clean, batch_report_lines, collision_kind_key, item_lines, item_lines_with,
    overflow_hostile, overflow_hostile_redacted, redacted_hostile, rename_pairs, rename_pairs_in,
    undo_report_is_clean, undo_report_lines, validate_ai_plan, validate_ai_plan_in,
    validate_semantic_hits,
};
pub use pane::{DEFAULT_PAGE, MarksSummary, PaneState, PatternError};
pub use sort::{SortColumn, SortDir, SortSpec, sort_entries, sort_entries_with};
