//! Las pantallas del TUI: cada overlay a pantalla completa con su propia
//! tabla de teclas.
//!
//! Todas vivían en el root del binario `ntc` —un crate DISTINTO de esta lib—,
//! así que ni los tests de integración ni un futuro frontend alternativo
//! podían alcanzarlas sin que el bucle de eventos hiciera de intermediario.
//!
//! Un fichero por pantalla y `mod.rs` de pura fachada, el mismo patrón que
//! [`crate::jobs`].

mod side_nav;

pub use side_nav::{
    on_nav_popup_key, on_places_key, on_processes_key, on_tree_key, open_drive_popup,
    refresh_places_drives, refresh_places_favorites,
};
