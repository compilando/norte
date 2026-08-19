//! Frontend TUI de norte (fase 3 M1): dual-pane ortodoxo sobre el core
//! embebido. SIN lógica de negocio (regla 7): listar/copiar/mover viven en
//! `norte-core`; aquí solo estado de UI y render.
#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod help;
pub mod help_context;
pub mod help_render;
pub mod hints;
pub mod keymap;
pub mod listing;
pub mod lua;
pub mod metadata;
pub mod mouse;
pub mod nav;
pub mod overlays;
pub mod palette;
pub mod panel;
pub mod paste;
pub mod preview;
pub mod processes;
pub mod session_push;
pub mod settings;
pub mod tasks;
pub mod theme;
pub mod tree;
pub mod tty;
pub mod ui;
pub mod viewer;
pub mod viewer_open;
