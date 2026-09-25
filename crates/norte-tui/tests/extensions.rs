//! The extensions manager overlay (M4-P3): navigation, local feedback
//! toggle and render. The logic lives in `ExtensionManager` (inside
//! `App`), so it is tested without the event loop. The render is checked
//! with a `TestBackend`, including the masking of a hostile `name` (the
//! name is free-form text from a third party — a security decision
//! surface, spec §6).

use norte_proto::methods::{PluginInfo, PluginLoadError};
use norte_tui::app::{App, ExtensionManager, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn plugin(id: &str, name: &str, category: &str, caps: &[&str], approved: bool) -> PluginInfo {
    PluginInfo {
        id: id.into(),
        name: name.into(),
        publisher: "acme".into(),
        version: "1.0.0".into(),
        category: category.into(),
        capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
        approved,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: Vec::new(),
        has_help: false,
        manifest_digest: None,
    }
}

fn mgr() -> ExtensionManager {
    ExtensionManager {
        // ALREADY sorted by category and id (as the core delivers them).
        plugins: vec![
            plugin("org.a.idx", "Alpha Indexer", "indexer", &["fs-read"], true),
            plugin("org.b.prev", "Beta Preview", "previewer", &[], false),
            plugin(
                "org.c.prev",
                "Gamma Preview",
                "previewer",
                &["fs-read"],
                true,
            ),
        ],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    }
}

fn app_with(mgr: ExtensionManager) -> App {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let d = norte_proto::VPath::parse("file:///x").expect("wire");
    let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
    app.extensions = Some(mgr);
    app
}

#[test]
fn navigation_clamps_and_selects() {
    let mut m = mgr();
    assert_eq!(m.selected().map(|p| p.id.as_str()), Some("org.a.idx"));
    m.up(); // upper cap
    assert_eq!(m.cursor, 0);
    m.down();
    m.down();
    assert_eq!(m.selected().map(|p| p.id.as_str()), Some("org.c.prev"));
    m.down(); // lower cap (3 plugins)
    assert_eq!(m.cursor, 2);
}

#[test]
fn local_toggle_mutates_the_bool_under_the_cursor() {
    let mut m = mgr();
    m.down(); // Beta Preview, unapproved
    assert!(!m.selected().unwrap().approved);
    m.set_local_approved(true);
    assert!(m.selected().unwrap().approved);
    // Did not touch the others.
    assert!(m.plugins[0].approved);
    m.set_local_enabled(false);
    assert!(!m.selected().unwrap().enabled);
}

#[test]
fn selected_on_empty_is_none() {
    let m = ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    };
    assert!(m.selected().is_none());
}

#[test]
fn render_shows_name_badge_and_notice() {
    let app = app_with(mgr());
    let mut t = Terminal::new(TestBackend::new(80, 24)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = t.backend().to_string();
    // A plugin's name.
    assert!(text.contains("Alpha Indexer"), "missing the name: {text}");
    // A capability badge.
    assert!(
        text.contains("fs-read"),
        "missing the capability badge: {text}"
    );
    // The Spanish locale is forced above (`app_with`): the not-approved
    // notice (Beta Preview) comes from the `es` Fluent catalogue, which
    // stays Spanish by design (see TRANSLATION_WIRE_SURFACE.md).
    assert!(text.contains("sin aprobar"), "missing the notice: {text}");
    // Category group header.
    assert!(
        text.contains("previewer"),
        "missing the group header: {text}"
    );
}

#[test]
fn render_of_an_empty_overlay_shows_ext_empty() {
    let app = app_with(ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    // Spanish locale forced above: the `ext-empty` catalogue string stays
    // Spanish by design.
    assert!(t.backend().to_string().contains("no hay extensiones"));
}

#[test]
fn render_masks_a_hostile_name() {
    // A name with a control char (BEL): a security decision surface
    // (approving). The render must NOT paint the raw byte — it masks it as
    // �.
    let app = app_with(ExtensionManager {
        plugins: vec![plugin(
            "org.evil.x",
            "bad\u{0007}one",
            "previewer",
            &[],
            false,
        )],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = t.backend().to_string();
    assert!(
        !text.contains('\u{0007}'),
        "the control byte was painted raw: {text:?}"
    );
    assert!(
        text.contains('\u{FFFD}'),
        "the masking was not marked: {text:?}"
    );
}

/// P1 encoding audit F1 (MEDIUM): a hostile/compromised daemon can send a
/// `description` of ANY length over the wire (the manifest only caps it at
/// 280 chars while PARSING, a check on the honest path that a faithful
/// daemon respects but a hostile one need not). This test builds
/// `ExtensionManager` DIRECTLY (as any caller that does not go through
/// `main::dispatch`'s ingest, `clamp_plugin_descriptions`, would), with an
/// uncapped description, and only checks that the render does not panic
/// nor hang — the effect of capping in `ui::plugin_description_line` is not
/// visible in the rendered frame (the popup already caps what is VISIBLE
/// by width, with or without the 280 cap: `plugin_description_line`'s own
/// unit test in `ui.rs` tests the cap directly, without going through the
/// layout).
#[test]
fn render_does_not_panic_with_an_unbounded_wire_description() {
    let mut p = plugin("org.norte.demo", "Demo", "previewer", &[], true);
    p.description = Some("a".repeat(50_000));
    let app = app_with(ExtensionManager {
        plugins: vec![p],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
}

#[test]
fn render_shows_load_errors() {
    let app = app_with(ExtensionManager {
        plugins: Vec::new(),
        errors: vec![PluginLoadError {
            dir: "/plugins/broken".into(),
            reason: "invalid manifest".into(),
            dir_bytes: None,
        }],
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(70, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = t.backend().to_string();
    assert!(text.contains("broken"), "missing the error's dir: {text}");
    assert!(text.contains("invalid"), "missing the reason: {text}");
}
