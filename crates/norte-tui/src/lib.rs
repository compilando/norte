//! norte's TUI frontend (phase 3 M1): an orthodox dual-pane over the embedded
//! core. NO business logic (rule 7): list/copy/move live in `norte-core`;
//! here there is only UI state and rendering.
#![forbid(unsafe_code)]

pub mod alt_menu;
pub mod app;
pub mod config;
pub mod config_reload;
pub mod console;
pub mod diskmap;
pub mod dispatch;
pub mod event_loop;
pub mod fill;
pub mod gestures;
pub mod goto;
pub mod handoff;
pub mod help;
pub mod help_context;
pub mod help_render;
pub mod hints;
pub mod jobs;
pub mod keymap;
pub mod keys;
pub mod kitty_graphics;
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
pub mod panelplugin;
pub mod paste;
pub mod preview;
pub mod probes;
pub mod processes;
pub mod refresh;
pub mod screens;
pub mod session_push;
pub mod settings;
pub mod shortcuts_editor;
pub mod splash;
/// The persistent subshell is POSIX: the pty, `cd`, and the prompt hooks all
/// are (#142, ADR 0084). On Windows `app.toggle-panels` declines, which is
/// the truth — and without this `cfg` the crate would not even compile there,
/// because the path-to-bytes translation is `std::os::unix`.
#[cfg(unix)]
pub mod subshell;
pub mod suspend;
pub mod tasks;
pub mod termpanel;
pub mod theme;
pub mod timeline;
pub mod trail;
pub mod wizard;
/// The tree pane lives in `norte-frontend` since the window also renders it:
/// it is presentation state, and two copies of the same model drift apart
/// (ADR 0066 D14). Re-exported so `crate::tree::` need not be rewritten in
/// twenty places.
pub use norte_frontend::tree;
pub mod tty;
pub mod turn;
pub mod ui;
pub mod viewer;
pub mod viewer_open;
