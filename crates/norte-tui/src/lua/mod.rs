//! Scripting Lua (M4, spec §7.2 + ADR 0026): comandos de usuario y hook de
//! statusbar. SIN sandbox — es config del usuario, no software de terceros
//! (la capa proyecto pasa por trust TOFU, `trust.rs`). Todo FS vía
//! `Backend` → engine: journal + policy + undo. `io.*`/`os.*` crudos NO
//! dejan rastro (documentado; como un shell).
//!
//! The durable design decisions live in ADR 0026.
mod api;
mod driver;
mod fs;
mod statusbar;
mod trust;

pub use api::{Layer, LuaHost, LuaLoadError, LuaWarning};
pub use driver::{CommandRun, DEFAULT_TIMEOUT, RunOutcome};
pub use fs::PaneCtx;
pub use statusbar::StatusInput;
pub use trust::{TrustDecision, TrustStore};
