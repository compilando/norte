//! The theme picker popup: navigation with a LIVE preview, Enter fixes it,
//! Esc reverts (ADR 0020). The logic lives in `App`, so it is tested
//! without the event loop.

use norte_proto::VPath;
use norte_tui::app::{App, Pane, PickerAction};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(w: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(w).expect("wire")
}

fn app() -> App {
    let d = vp("file:///x");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

#[test]
fn opening_lists_the_presets_and_previews() {
    let mut app = app();
    app.open_theme_picker();
    let picker = app.theme_picker.as_ref().expect("popup open");
    // All the embedded presets are there.
    assert!(picker.names.iter().any(|n| n == "catppuccin-mocha"));
    assert!(picker.names.iter().any(|n| n == "nord"));
    // The popup is painted (the title and the names show up in the buffer).
    let mut t = Terminal::new(TestBackend::new(60, 16)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = t.backend().to_string();
    assert!(
        text.contains("nord"),
        "the popup does not list the themes: {text}"
    );
}

#[test]
fn navigating_previews_and_enter_fixes_it() {
    let mut app = app();
    app.open_theme_picker();
    // Move down to catppuccin-mocha and confirm.
    // (The order of names comes from preset_names; we navigate until we find it.)
    let idx = app
        .theme_picker
        .as_ref()
        .unwrap()
        .names
        .iter()
        .position(|n| n == "catppuccin-mocha")
        .unwrap();
    for _ in 0..idx {
        app.theme_picker_input(PickerAction::Down);
    }
    // LIVE preview: navigating already changed the current theme (the exact
    // color, which depends on terminal depth, is covered by
    // theme_render.rs).
    assert_eq!(
        app.theme.name(),
        Some("catppuccin-mocha"),
        "the preview did not apply"
    );
    app.theme_picker_input(PickerAction::Confirm);
    // Closed and FIXED: the theme is still catppuccin after confirming.
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.name(), Some("catppuccin-mocha"));
}

#[test]
fn cancelling_reverts_to_the_previous_theme() {
    let mut app = app();
    let before = app.theme.name().map(String::from);
    app.open_theme_picker();
    app.theme_picker_input(PickerAction::Down);
    app.theme_picker_input(PickerAction::Down);
    // Cancel: goes back to the theme from before opening.
    app.theme_picker_input(PickerAction::Cancel);
    assert!(app.theme_picker.is_none());
    assert_eq!(app.theme.name().map(String::from), before);
}
