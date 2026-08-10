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
pub mod lua;
pub mod mouse;
pub mod nav;
pub mod palette;
pub mod settings;
pub mod tasks;
pub mod theme;
pub mod tty;
pub mod ui;
pub mod viewer;
pub mod watch;
