//! The search panel inside `App`: opening it rooted at the focused folder,
//! giving the keyboard to it and taking it back.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Fixed language: the snapshot freezes localized text.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entries(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segment"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn test_app() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir)),
        Pane::new(dir.clone(), entries(&dir)),
    );
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Top);
    app
}

/// The command opens the panel at the focused listing's folder with the
/// keyboard in it; again closes it and gives the keys back.
#[test]
fn layout_search_opens_rooted_at_the_focused_folder_and_closes() {
    let mut app = test_app();
    let dir = app.focused().dir().clone();
    app.toggle_search_panel();
    let slot = app.search_panel_slot().expect("open");
    assert_eq!(app.panes.search_panel(slot).expect("model").root(), &dir);
    assert_eq!(app.key_owner(), KeyOwner::Search);
    app.toggle_search_panel();
    assert!(app.search_panel_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Docked on the left by default (ADR 0173).
#[test]
fn snapshot_search_panel_opens_on_the_left() {
    let mut app = test_app();
    app.render_now_ms = Some(0);
    app.toggle_search_panel();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}
