//! The command palette.
//!
//! The MODEL (filtering, cursor, what is selected) lives in `norte-frontend`
//! since phase 4 of the multi-frontend plan: these are presentation rules,
//! and two frontends with two copies are two palettes that behave
//! differently without anyone noticing (ADR 0066, decision D14). This module
//! re-exports it so the call sites don't need to change; its tests moved
//! with the model, which is where they describe something.

pub use norte_frontend::palette_state::Palette;
