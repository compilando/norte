//! Rows of the settings overlay (`app.settings`, S3): a thin re-export.
//!
//! [`Row`]/[`build_rows`] (together with the `app::Settings` editor that
//! consumes them) used to live here, but had NO coupling to the TUI (neither
//! ratatui/crossterm nor I/O) — only `norte_frontend::settings::catalog`/
//! `current_value` + Fluent. S4 (GUI settings view) hoisted them to
//! `norte_frontend::settings` so both frontends share the SAME row
//! construction instead of duplicating it (CLAUDE.md rule 7); this module
//! stays as a source-compatibility alias (`norte_tui::
//! settings::{Row, build_rows}` still resolves the same for the rest of the
//! crate and the integration tests).
pub use norte_frontend::settings::{Row, build_rows};
