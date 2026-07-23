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

pub use dirs::{Layer, Layers, config_dir, standard_layers, user_config_dir};
