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
pub mod logline;
/// El anillo en memoria que un frontend pinta (#323 dejó ver por qué hacía
/// falta). Misma feature que [`logging`]: es otra capa del mismo subscriber.
/// El TIPO de línea, en cambio, vive en [`logline`] y sin feature — lo necesita
/// quien pinta, que no compila subscriber ninguno.
#[cfg(feature = "logging")]
pub mod logring;
pub mod profiles;
pub mod schema;
#[cfg(feature = "watch")]
pub mod watch;

pub use dirs::{
    Layer, Layers, config_dir, standard_layers, standard_layers_from, standard_layers_no_project,
    user_config_dir, user_config_dir_from,
};
pub use load::{
    AiSettings, AlignChoice, ColumnSpec, ColumnsConfig, CommonConfig, ConfirmQuit, DateFormat,
    HotlistItem, KeymapList, KeymapWrite, PanelBarStyle, PersistSort, QuickSearch, SchemeColumns,
    SortChoice, SortColumnKey, UiChrome, WidthChoice, load, persist_column_format,
    persist_column_width, persist_columns, persist_hotlist_add, persist_hotlist_remove,
    persist_keymap_bind, persist_keymap_unbind, persist_set, persist_ui_theme, persist_ui_theme_to,
};
pub use profiles::{
    Loaded, PROFILE_LAYOUT_NAME, ProfileError, ProfileLoad, ProfileSnapshot, ProfileSource,
    list_profiles, load_with, load_with_profile, profile_dir_from, profiles_dir_from, save_profile,
    standard_layers_no_project_with_profile, standard_layers_with_profile, valid_profile_name,
};
pub use schema::{
    AiProviderEntry, AiSection, ArchiveSection, ConfigError, DEFAULT_PRESET, DaemonMode,
    DaemonSection, HotlistEntry, KeymapSection, NorteToml, UiSection,
};
#[cfg(feature = "watch")]
pub use watch::{Watch, WatchMode, watch, watch_polling};
