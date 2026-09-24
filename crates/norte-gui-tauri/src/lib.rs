//! The reference renderer: a Tauri webview over `norte-ui-host`.
//!
//! This crate is an ADAPTER, not a layer (ADR 0066, decision D2). What it
//! knows how to do is four things:
//!
//! 1. start up (read configuration, resolve the socket and the directory),
//! 2. mount [`norte_ui_host::UiHost`] over the daemon's SDK,
//! 3. pump its updates to the window IN ORDER,
//! 4. let the webview send [`norte_ui_host::UiAction`] and nothing else.
//!
//! Everything that decides something lives below, in Rust: the screen layout,
//! the listing order, which command a key is bound to, whether an operation
//! is available. The webview paints.
//!
//! # What the webview CANNOT do
//!
//! There is no filesystem, no shell, no HTTP, no `rpc(method, params)` to ask
//! the daemon for whatever comes to mind: the command list in [`commands`] is
//! the entire surface, and its capabilities file grants nothing beyond
//! listening for events (decision D11).
#![forbid(unsafe_code)]

pub mod catalog;
pub mod commands;
pub mod links;
pub mod nativo;
pub mod sink;
pub mod startup;

pub use commands::Bridge;
pub use sink::{EVENT_LAGGED, EVENT_UPDATE, UpdateSink, pump};
pub use startup::{Cli, StartupError};
