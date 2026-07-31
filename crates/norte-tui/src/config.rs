//! Layered configuration — relocated to `norte-config` + `norte-frontend`
//! (ADR 0035; the ADR 0007 "until another frontend needs it" clause fired).
//! This module re-exports the old names so call sites and the schema golden
//! keep compiling, and keeps the TUI-only async wrapper.

pub use norte_config::{
    ConfigError, ConfirmQuit, DEFAULT_PRESET, DaemonMode, HotlistItem, Layer, Layers, NorteToml,
    PersistSort, Watch, WatchMode, persist_column_format, persist_columns, persist_hotlist_add,
    persist_hotlist_remove, persist_set, persist_ui_theme, persist_ui_theme_to, standard_layers,
    user_config_dir, watch, watch_polling,
};
pub use norte_frontend::config::{FrontendConfig as LoadedConfig, load};

/// Load in async context (hot reload): runs in `spawn_blocking` — the
/// runtime never blocks on the FS (rule 2).
///
/// # Errors
/// Those of [`norte_frontend::config::load`].
pub async fn load_async(layers: Layers) -> Result<LoadedConfig, ConfigError> {
    match tokio::task::spawn_blocking(move || norte_frontend::config::load(&layers)).await {
        Ok(res) => res,
        // A panic inside load() is OUR bug: never bury it as a ConfigError
        // with a fake path (rule 6) — let it blow up visibly.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
