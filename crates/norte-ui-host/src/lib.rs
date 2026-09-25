//! The semantic state of a graphical frontend, without knowing who paints it.
//!
//! A renderer — a Tauri webview, a Flutter shell, a headless test — does not
//! hold state that means anything: it sends [`UiAction`] and receives
//! [`UiUpdate`] in order and versioned. What means something lives here, in
//! Rust, on top of `norte-frontend` (the presentation rules the frontends
//! already share) and `norte-client` (the conversation with the daemon).
//!
//! Why this boundary exists, and what was decided at it: ADR 0066.
//!
//! # What this crate does NOT do
//!
//! - It does not know about any toolkit. Not Tauri, not web, not GPUI, not
//!   ratatui. Its boundary test checks it against cargo's real graph.
//! - It does not reimplement presentation rules: if the TUI and it order a
//!   listing differently, that is a bug in this crate, not a decision of its
//!   own.
//! - It does not let a raw path cross to the renderer. What crosses is
//!   sanitized text and opaque keys.

// This crate is a public CONTRACT with its own golden corpus (ADR 0066),
// which is the profile the `missing_docs` convention targets even though the
// rule names proto/VFS/SDK. And watch out: `just t` does not run doctests and
// `just c` does not check intra-doc links, so what is here is only verified
// in `docs`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod action;
mod agents;
pub mod backend;
pub mod bridge;
pub mod commands;
pub mod controller;
pub mod dto;
/// The extension manager (`app.extensions`), read-only.
mod extensions;
/// Help (F1): the shared corpus projected onto the bridge's vocabulary.
mod help;
pub mod keys;
/// The theme and the volume picker, read-only.
///
/// There is no connection picker yet (#264), and announcing it here would
/// have promised it in the documentation index.
pub mod pickers;
pub mod settings;

pub use action::{TabVerb, UiAction};
pub use backend::HostBackend;
pub use bridge::{
    ActionAck, BRIDGE_VERSION, BridgeEnvelope, InstanceId, MAX_TASKS_RETAINED, MAX_TRANSFER_BATCH,
    ModalId, RequestToken, RowKey, StaleAction,
};
pub use controller::{ShutdownReport, UiHost, UiHostOptions, UiSubscription, Update};
pub use dto::{UiNotice, UiUpdate, ViewPatch, ViewSnapshot};
pub use keys::KeyInput;

/// A host's configuration with NO files: the factory values.
///
/// It exists so a test or a first launch does not have to build one; a real
/// host receives the one its startup loaded from the user's layers.
///
/// # Panics
/// Never: loading ZERO layers cannot fail (there is no file to mis-parse).
#[must_use]
pub fn default_settings() -> norte_frontend::config::FrontendConfig {
    norte_frontend::config::load(&norte_config::Layers { dirs: Vec::new() })
        .expect("loading zero layers cannot fail")
}

/// A host's column configuration with no configuration: the factory ones
/// (name, size and date), the same for every scheme.
///
/// It exists so a test or a first launch does not have to build it by hand;
/// a real host resolves it from the user's configuration with
/// `ColumnsSettings::resolve` and passes it in [`UiHostOptions`].
#[must_use]
pub fn default_columns() -> norte_frontend::columns::ColumnsSettings {
    norte_frontend::columns::ColumnsSettings::default()
}
