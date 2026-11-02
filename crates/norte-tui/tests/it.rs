//! Most of norte-tui's integration tests, in ONE binary.
//!
//! Each file used to be its own test binary: 62 of them, each linking the
//! whole TUI and the core under it. The files stay where they were
//! (`autotests = false` in `Cargo.toml`); nextest still runs each test in its
//! own process.
//!
//! Kept as their own binaries, in `Cargo.toml`: the four insta suites
//! (`layout_anchor`, `places`, `searchpanel`, `snapshots_ui`), whose
//! snapshot files are named after the module path and would all change name
//! in here; and `schema`, which needs the `schema` feature.
//!
//! A new test file is a new line here: without it, it is never compiled.

#[path = "app.rs"]
mod app;
#[path = "apply_cd.rs"]
mod apply_cd;
#[path = "assistant.rs"]
mod assistant;
#[path = "caps_cache.rs"]
mod caps_cache;
#[path = "chrome_icons.rs"]
mod chrome_icons;
#[path = "columns_picker.rs"]
mod columns_picker;
#[path = "command_bar.rs"]
mod command_bar;
#[path = "config.rs"]
mod config;
#[path = "config_fuzz.rs"]
mod config_fuzz;
#[path = "connection_degraded.rs"]
mod connection_degraded;
#[path = "context_menu.rs"]
mod context_menu;
#[path = "disk_map_notice.rs"]
mod disk_map_notice;
#[path = "extensions.rs"]
mod extensions;
#[path = "fork.rs"]
mod fork;
#[path = "help_freeze.rs"]
mod help_freeze;
#[path = "help_gate.rs"]
mod help_gate;
#[path = "help_overlay.rs"]
mod help_overlay;
#[path = "keymap.rs"]
mod keymap;
#[path = "keymap_prop.rs"]
mod keymap_prop;
#[path = "landing_settles.rs"]
mod landing_settles;
#[path = "layout_picker.rs"]
mod layout_picker;
#[path = "lua_driver.rs"]
mod lua_driver;
#[path = "lua_e2e.rs"]
mod lua_e2e;
#[path = "lua_fs.rs"]
mod lua_fs;
#[path = "menu_hint.rs"]
mod menu_hint;
#[path = "metadata.rs"]
mod metadata;
#[path = "mirror_fill.rs"]
mod mirror_fill;
#[path = "modal.rs"]
mod modal;
#[path = "mouse.rs"]
mod mouse;
#[path = "orders.rs"]
mod orders;
#[path = "panel_plugin.rs"]
mod panel_plugin;
#[path = "path_edit.rs"]
mod path_edit;
#[path = "pick.rs"]
mod pick;
#[path = "preview.rs"]
mod preview;
#[path = "processes.rs"]
mod processes;
#[path = "profile_picker.rs"]
mod profile_picker;
#[path = "record_step.rs"]
mod record_step;
#[path = "refresh_decorates.rs"]
mod refresh_decorates;
#[path = "refresh_ritual.rs"]
mod refresh_ritual;
#[path = "render.rs"]
mod render;
#[path = "results_tab.rs"]
mod results_tab;
#[path = "search_e2e.rs"]
mod search_e2e;
#[path = "session.rs"]
mod session;
#[path = "splash_render.rs"]
mod splash_render;
#[path = "status_marks.rs"]
mod status_marks;
#[path = "suspend.rs"]
mod suspend;
#[path = "swap.rs"]
mod swap;
#[path = "tasks.rs"]
mod tasks;
#[path = "theme_persist.rs"]
mod theme_persist;
#[path = "theme_picker.rs"]
mod theme_picker;
#[path = "theme_render.rs"]
mod theme_render;
#[path = "tree.rs"]
mod tree;
#[path = "viewer.rs"]
mod viewer;
#[path = "viewer_image.rs"]
mod viewer_image;
#[path = "which_key.rs"]
mod which_key;

/// A file in `tests/` that is neither here nor its own `[[test]]` is never
/// compiled, and its tests pass by not existing.
#[test]
fn every_test_file_is_compiled() {
    const OWN_BINARY: &[&str] = &[
        "layout_anchor.rs",
        "places.rs",
        "searchpanel.rs",
        "snapshots_ui.rs",
        "schema.rs",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let me = include_str!("it.rs");
    for entry in std::fs::read_dir(&dir).expect("tests/") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let is_rs = path.extension().is_some_and(|e| e == "rs");
        if !is_rs || name == "it.rs" || OWN_BINARY.contains(&name.as_str()) {
            continue;
        }
        assert!(
            me.contains(&format!("#[path = \"{name}\"]")),
            "tests/{name} is not in tests/it.rs"
        );
    }
}
