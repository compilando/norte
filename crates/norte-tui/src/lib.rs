//! Frontend TUI de norte (fase 3 M1): dual-pane ortodoxo sobre el core
//! embebido. SIN lógica de negocio (regla 7): listar/copiar/mover viven en
//! `norte-core`; aquí solo estado de UI y render.
#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod config_reload;
pub mod console;
pub mod dispatch;
pub mod event_loop;
pub mod fill;
pub mod gestures;
pub mod help;
pub mod help_context;
pub mod help_render;
pub mod hints;
pub mod jobs;
pub mod keymap;
pub mod keys;
pub mod listing;
pub mod logview;
pub mod lua;
pub mod metadata;
pub mod mouse;
pub mod mutations;
pub mod nav;
pub mod navigate;
pub mod overlays;
pub mod palette;
pub mod panel;
pub mod paste;
pub mod preview;
pub mod probes;
pub mod processes;
pub mod refresh;
pub mod screens;
pub mod session_push;
pub mod settings;
pub mod shortcuts_editor;
/// El subshell persistente es POSIX: pty, `cd` y los ganchos de prompt lo son
/// (#142, ADR 0084). En Windows `app.toggle-panels` declina, que es la verdad
/// — y sin este `cfg` el crate ni siquiera compilaba ahí, porque la traducción
/// de rutas a bytes es `std::os::unix`.
#[cfg(unix)]
pub mod subshell;
pub mod suspend;
pub mod tasks;
pub mod theme;
pub mod trail;
/// El panel de árbol vive en `norte-frontend` desde que la ventana también lo
/// pinta: es estado de presentación, y dos copias del mismo modelo se separan
/// (ADR 0066 D14). Se reexporta para no reescribir `crate::tree::` en veinte
/// sitios.
pub use norte_frontend::tree;
pub mod tty;
pub mod turn;
pub mod ui;
pub mod viewer;
pub mod viewer_open;
