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

// Este crate es un CONTRATO público con su propio corpus golden (ADR 0066),
// que es el perfil al que apunta la convención de `missing_docs` aunque la
// regla nombre proto/VFS/SDK. Y ojo: `just t` no corre doctests y `just c` no
// comprueba enlaces intra-doc, así que lo de aquí solo se verifica en `docs`.
#![warn(missing_docs)]

pub mod action;
pub mod backend;
pub mod bridge;
pub mod commands;
pub mod controller;
pub mod dto;
/// La ayuda (F1): el corpus compartido proyectado al vocabulario del bridge.
/// El gestor de extensiones (`app.extensions`), en solo lectura.
mod extensions;
mod help;
pub mod keys;
/// El tema y los selectores de conexión y volumen, en solo lectura.
pub mod pickers;
pub mod settings;

pub use action::UiAction;
pub use backend::HostBackend;
pub use bridge::{
    ActionAck, BRIDGE_VERSION, BridgeEnvelope, InstanceId, ModalId, RequestToken, RowKey,
    StaleAction,
};
pub use controller::{ShutdownReport, UiHost, UiHostOptions, UiSubscription, Update};
pub use dto::{UiNotice, UiUpdate, ViewPatch, ViewSnapshot};
pub use keys::KeyInput;

/// La configuración de un host SIN ficheros: los valores de fábrica.
///
/// Existe para que un test o un primer arranque no tengan que fabricarla; un
/// host de verdad recibe la que su arranque cargó de las capas del usuario.
///
/// # Panics
/// Nunca: cargar CERO capas no puede fallar (no hay fichero que parsear mal).
#[must_use]
pub fn ajustes_por_defecto() -> norte_frontend::config::FrontendConfig {
    norte_frontend::config::load(&norte_config::Layers { dirs: Vec::new() })
        .expect("cargar cero capas no puede fallar")
}

/// La configuración de columnas de un host sin configuración: las de fábrica
/// (nombre, tamaño y fecha), iguales para todos los esquemas.
///
/// Existe para que un test o un primer arranque no tengan que construirla a
/// mano; un host de verdad la resuelve de la configuración del usuario con
/// `ColumnsSettings::resolve` y se la pasa en [`UiHostOptions`].
#[must_use]
pub fn columnas_por_defecto() -> norte_frontend::columns::ColumnsSettings {
    norte_frontend::columns::ColumnsSettings::default()
}
