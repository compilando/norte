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
/// `tracing` setup for the binaries: a rotating file with closed permissions,
/// and `suppaftp`'s security cap (#43, rule 10).
///
/// Behind a feature because it pulls in `tracing-subscriber` and
/// `tracing-appender`, and only the BINARIES need them: a library that
/// depends on this crate to read `norte.toml` has no reason to compile them.
#[cfg(feature = "logging")]
pub mod logging;
pub mod logline;
/// The in-memory ring a frontend paints (#323 showed why it was needed).
/// Same feature as [`logging`]: it is another layer of the same subscriber.
/// The line TYPE, on the other hand, lives in [`logline`] with no feature —
/// the painter needs it, and it compiles no subscriber at all.
#[cfg(feature = "logging")]
pub mod logring;
pub mod profiles;
pub mod schema;
pub mod sections;
#[cfg(feature = "watch")]
pub mod watch;

pub use dirs::{
    Layer, Layers, config_dir, standard_layers, standard_layers_from, standard_layers_no_project,
    user_config_dir, user_config_dir_from,
};
pub use load::{
    AiSettings, AlignChoice, ColumnSpec, ColumnsConfig, CommonConfig, ConfigWrite, ConfirmQuit,
    DateFormat, HotlistItem, Images, KeymapList, KeymapWrite, PanelBarPosition, PanelBarStyle,
    PersistSort, QuickSearch, SchemeColumns, SortChoice, SortColumnKey, StatusItem, StatusItems,
    Titlebar, UiChrome, WidthChoice, load, persist_column_format, persist_column_width,
    persist_columns, persist_hotlist_add, persist_hotlist_remove, persist_keymap_bind,
    persist_keymap_unbind, persist_set, persist_ui_theme, persist_ui_theme_to, persist_unset,
};
pub use profiles::{
    Loaded, PROFILE_LAYOUT_NAME, ProfileError, ProfileLoad, ProfileSnapshot, ProfileSource,
    list_profiles, load_with, load_with_profile, profile_dir_from, profiles_dir_from, save_profile,
    standard_layers_no_project_with_profile, standard_layers_with_profile, valid_profile_name,
};
pub use schema::{
    AiProviderEntry, AiSection, ArchiveSection, ConfigError, DEFAULT_PRESET, DaemonMode,
    DaemonSection, HotlistEntry, KeymapSection, LogFormat, NorteToml, UiSection,
};
pub use sections::{ArchiveSettings, DaemonSettings, LogSettings};
#[cfg(feature = "watch")]
pub use watch::{Watch, WatchMode, watch, watch_polling};
