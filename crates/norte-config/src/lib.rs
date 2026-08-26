//! Layered configuration (ADR 0007, ADR 0035): the single config-dir
//! resolver, the strict `norte.toml` schema, scalar merge across layers,
//! persistence helpers, and (feature `watch`) live reload plumbing.
//!
//! This crate deliberately reads configuration with `std::fs`; using
//! providers would be circular because configuration selects how a frontend
//! starts. It depends on `norte-proto` only (for `VPath`) — never on the
//! core or a frontend.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dirs;
pub mod load;
/// Montaje de `tracing` para los binarios: fichero rotatorio con permisos
/// cerrados, y el cap de seguridad de `suppaftp` (#43, regla 10).
///
/// Tras una feature porque arrastra `tracing-subscriber` y
/// `tracing-appender`, y solo los BINARIOS los necesitan: una biblioteca que
/// depende de este crate por leer `norte.toml` no tiene por qué compilarlos.
#[cfg(feature = "logging")]
pub mod logging;
pub mod profiles;
pub mod schema;
#[cfg(feature = "watch")]
pub mod watch;

pub use dirs::{
    Layer, Layers, config_dir, standard_layers, standard_layers_from, standard_layers_no_project,
    user_config_dir, user_config_dir_from,
};
pub use load::{
    AiSettings, AlignChoice, ColumnSpec, ColumnsConfig, CommonConfig, ConfirmQuit, HotlistItem,
    KeymapList, KeymapWrite, PersistSort, QuickSearch, SchemeColumns, SortChoice, SortColumnKey,
    WidthChoice, load, persist_column_format, persist_columns, persist_hotlist_add,
    persist_hotlist_remove, persist_keymap_bind, persist_keymap_unbind, persist_set,
    persist_ui_theme, persist_ui_theme_to,
};
pub use profiles::{
    list_profiles, profile_dir_from, profiles_dir_from, standard_layers_no_project_with_profile,
    standard_layers_with_profile,
};
pub use schema::{
    AiProviderEntry, AiSection, ArchiveSection, ConfigError, DEFAULT_PRESET, DaemonMode,
    DaemonSection, HotlistEntry, KeymapSection, NorteToml, UiSection,
};
#[cfg(feature = "watch")]
pub use watch::{Watch, WatchMode, watch, watch_polling};
