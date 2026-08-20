//! El estado semántico de un frontend gráfico, sin saber quién lo pinta.
//!
//! Un renderer —una webview de Tauri, un shell Flutter, un test headless— no
//! guarda estado que signifique algo: manda [`UiAction`] y recibe
//! [`UiUpdate`] ordenados y versionados. Lo que significa algo vive aquí, en
//! Rust, sobre `norte-frontend` (las reglas de presentación que ya comparten
//! los frontends) y `norte-client` (la conversación con el daemon).
//!
//! Por qué existe esta frontera, y qué se decidió en ella: ADR 0066.
//!
//! # Lo que este crate NO hace
//!
//! - No conoce a ningún toolkit. Ni Tauri, ni web, ni GPUI, ni ratatui. Su
//!   test de frontera lo comprueba contra el grafo real de cargo.
//! - No reimplementa reglas de presentación: si el TUI y él ordenan un
//!   listado distinto, es un bug de este crate, no una decisión suya.
//! - No deja que un path crudo cruce al renderer. Lo que cruza es texto
//!   saneado y claves opacas.

pub mod action;
pub mod backend;
pub mod bridge;
pub mod commands;
pub mod controller;
pub mod dto;
pub mod keys;

pub use action::UiAction;
pub use backend::HostBackend;
pub use bridge::{
    ActionAck, BRIDGE_VERSION, BridgeEnvelope, InstanceId, ModalId, RequestToken, RowKey,
    StaleAction,
};
pub use controller::{ShutdownReport, UiHost, UiHostOptions, UiSubscription, Update};
pub use dto::{UiNotice, UiUpdate, ViewPatch, ViewSnapshot};
pub use keys::KeyInput;

/// Las columnas con las que arranca un host sin configuración: nombre,
/// tamaño y fecha.
///
/// Existe para que un test o un primer arranque no tengan que construir la
/// lista a mano; un host de verdad la lee de la configuración del usuario y
/// se la pasa en [`UiHostOptions`].
#[must_use]
pub fn columnas_por_defecto() -> Vec<norte_frontend::columns::ColumnId> {
    use norte_frontend::columns::{Builtin, ColumnId};
    vec![
        ColumnId::Builtin(Builtin::Name),
        ColumnId::Builtin(Builtin::Size),
        ColumnId::Builtin(Builtin::Mtime),
    ]
}
