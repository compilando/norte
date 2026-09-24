//! The TUI's screens: each a full-screen overlay with its own key table.
//!
//! They all used to live in the `ntc` binary's root — a crate DISTINCT from
//! this lib — so neither the integration tests nor a future alternative
//! frontend could reach them without the event loop acting as go-between.
//!
//! One file per screen and a pure-facade `mod.rs`, the same pattern as
//! [`crate::jobs`].

pub mod extensions;
pub mod help;
pub mod pickers;
pub mod profile_save;
pub mod settings;
pub mod side_nav;

pub use extensions::{on_extensions_click, on_extensions_key};
pub use help::{HelpDispatch, on_help_key, run_plugin_command};
pub use pickers::{
    apply_theme, on_columns_key, on_connections_picker_key, on_layout_picker_key,
    on_profile_picker_key, on_theme_picker_key, pane_attr_ids,
};
pub use profile_save::profile_save_as;
pub use settings::{on_settings_key, persist_setting, plugin_config_summaries};
pub use side_nav::{
    drain_places_drives, on_disk_map_key, on_nav_popup_key, on_panel_key, on_places_key,
    on_processes_key, on_timeline_key, on_tree_key, open_drive_popup, refresh_places_drives,
};
