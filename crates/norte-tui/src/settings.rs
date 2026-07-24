//! Filas del overlay de ajustes (`app.settings`, S3): re-export fino.
//!
//! [`Row`]/[`build_rows`] (junto con el editor `app::Settings` que las
//! consume) vivían aquí, pero no tenían NINGÚN acoplo a la TUI (ni
//! ratatui/crossterm ni I/O) — solo `norte_frontend::settings::catalog`/
//! `current_value` + Fluent. S4 (GUI settings view) los hoisteó a
//! `norte_frontend::settings` para que ambos frontends compartan la MISMA
//! construcción de filas en vez de duplicarla (CLAUDE.md regla 7); este
//! módulo queda como alias de compatibilidad de fuente (`norte_tui::
//! settings::{Row, build_rows}` sigue resolviendo igual para el resto del
//! crate y los tests de integración).
pub use norte_frontend::settings::{Row, build_rows};
