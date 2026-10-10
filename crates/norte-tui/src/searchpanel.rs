//! The search panel in the TUI: the slot's `kind`.
//!
//! The MODEL — the form, the pause, the results — lives in
//! [`norte_frontend::search_panel`], shared with the window. What is here is
//! what only this frontend knows.

/// The slot's `kind`, exactly as the shared registry declares it.
pub const KIND: &str = "search";
