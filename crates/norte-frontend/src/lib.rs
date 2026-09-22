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
//!
//! # «Pura» dejó de ser exacto, y conviene decirlo
//!
//! Desde el ítem 7 del roadmap post-alpha vive aquí [`watch`], que vigila
//! directorios: trae `notify`, una dependencia de `tokio` y una task de fondo.
//! No es presentación. Está aquí porque es INFRAESTRUCTURA DE FRONTEND
//! independiente del toolkit —lo mismo que el resto del crate, con otra clase
//! de contenido— y porque la alternativa era un crate cuyo contenido entero es
//! un fichero con dos consumidores.
//!
//! La regla que sigue valiendo, y que es la que importa, es la de la regla dura
//! 7: aquí no hay lógica de NEGOCIO. Vigilar un directorio no decide nada sobre
//! los ficheros; dice que algo cambió y quien decide qué hacer es el frontend,
//! por el mismo camino que su refresco manual.
//!
//! # Dónde está cada cosa
//!
//! El código fuente se agrupa en carpetas por lo que HACE: `ops/`
//! (operaciones sobre ficheros), `navigation/` (ir a sitios), `chrome/` (lo
//! que enmarca los paneles), `overlays/` (lo que se abre encima y pide
//! elegir) y `view/` (cómo se enseña). Las carpetas son PRIVADAS: cada módulo
//! se re-exporta en la raíz (`norte_frontend::chmod`), y ésa sigue siendo la
//! única ruta pública. Un módulo nuevo va en la carpeta de su grupo y se
//! re-exporta aquí; uno que no encaja en ninguna se queda en la raíz.
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
