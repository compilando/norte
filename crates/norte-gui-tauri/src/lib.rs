//! El renderer de referencia: una webview de Tauri sobre `norte-ui-host`.
//!
//! Este crate es un ADAPTADOR, no una capa (ADR 0066, decisión D2). Lo que
//! sabe hacer es cuatro cosas:
//!
//! 1. arrancar (leer configuración, resolver el socket y el directorio),
//! 2. montar el [`norte_ui_host::UiHost`] sobre el SDK del daemon,
//! 3. bombear sus actualizaciones a la ventana EN ORDEN,
//! 4. dejar que la webview mande [`norte_ui_host::UiAction`] y nada más.
//!
//! Todo lo que decide algo vive debajo, en Rust: el reparto de la pantalla, el
//! orden del listado, qué comando lleva ligada una tecla, si una operación
//! está disponible. La webview pinta.
//!
//! # Lo que la webview NO puede hacer
//!
//! No hay filesystem, ni shell, ni HTTP, ni un `rpc(method, params)` por el
//! que pedirle al daemon lo que se le ocurra: la lista de comandos de
//! [`commands`] es la superficie entera, y su fichero de capacidades no
//! concede más que escuchar eventos (decisión D11).
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
