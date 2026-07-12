//! Frontend TUI de norte (fase 3 M1): dual-pane ortodoxo sobre el core
//! embebido. SIN lógica de negocio (regla 7): listar/copiar/mover viven en
//! `norte-core`; aquí solo estado de UI y render.
#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod help;
pub mod keymap;
pub mod tasks;
pub mod ui;
pub mod viewer;
