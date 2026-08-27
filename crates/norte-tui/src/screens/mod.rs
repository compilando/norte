//! Las pantallas del TUI: cada overlay a pantalla completa con su propia
//! tabla de teclas.
//!
//! Todas vivían en el root del binario `ntc` —un crate DISTINTO de esta lib—,
//! así que ni los tests de integración ni un futuro frontend alternativo
//! podían alcanzarlas sin que el bucle de eventos hiciera de intermediario.
//!
//! Un fichero por pantalla y `mod.rs` de pura fachada, el mismo patrón que
//! [`crate::jobs`].

pub mod extensions;
pub mod help;
pub mod pickers;
pub mod settings;
pub mod side_nav;

pub use extensions::on_extensions_key;
pub use help::{HelpDispatch, on_help_key, run_plugin_command};
pub use pickers::{
    apply_theme, on_columns_key, on_connections_picker_key, on_layout_picker_key,
    on_profile_picker_key, on_theme_picker_key, pane_attr_ids,
};
pub use settings::{on_settings_key, persist_setting, plugin_config_summaries};
pub use side_nav::{
    drain_places_drives, on_nav_popup_key, on_places_key, on_processes_key, on_tree_key,
    open_drive_popup, refresh_places_drives,
};
