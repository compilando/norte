//! Lua scripting (M4, spec §7.2 + ADR 0026): user commands and the
//! statusbar hook. NO sandbox — it is user config, not third-party
//! software (the project layer goes through TOFU trust, `trust.rs`). All FS
//! access goes via `Backend` → engine: journal + policy + undo. Raw
//! `io.*`/`os.*` leave NO trace (documented; like a shell).
//!
//! The durable design decisions live in ADR 0026.
mod api;
mod driver;
mod fs;
mod host;
mod statusbar;
mod trust;

pub use api::{Layer, LuaHost, LuaLoadError, LuaWarning};
pub use driver::{CommandRun, DEFAULT_TIMEOUT, RunOutcome};
pub use fs::PaneCtx;
pub use host::{load_lua, refresh_lua_status, resolve_lua_trust, run_lua_command, start_lua_run};
pub use statusbar::StatusInput;
pub use trust::{TrustDecision, TrustStore};
