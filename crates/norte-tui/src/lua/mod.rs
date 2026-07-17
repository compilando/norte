//! Scripting Lua (M4, spec §7.2 + ADR 0026): comandos de usuario y hook de
//! statusbar. SIN sandbox — es config del usuario, no software de terceros
//! (la capa proyecto pasa por trust TOFU, `trust.rs`). Todo FS vía
//! `Backend` → engine: journal + policy + undo. `io.*`/`os.*` crudos NO
//! dejan rastro (documentado; como un shell).
//!
//! Los submódulos se activan por tasks del plan
//! (`docs/superpowers/plans/2026-07-17-m4-lua-scripting.md`):
mod api; // task 2
// mod driver;     // task 5
// mod fs;         // task 4
// mod statusbar;  // task 7
// mod trust;      // task 6

pub use api::{Layer, LuaHost, LuaLoadError, LuaWarning};
