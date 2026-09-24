//! The directory tree (#136) as a PANEL: the mouse and the menu bar.
//!
//! What these tests protect is that a panel with keyboard support stays
//! part of the application: it can be clicked with the mouse, and the
//! chrome's keys — the menu bar — do not die from being inside it.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
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

/// `App` with the tree open over `file:///home` and two branches already
/// read, one of them with a child.
fn app_with_tree() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir)),
        Pane::new(dir.clone(), entries(&dir)),
    );
    app.toggle_tree();
    let t = app.tree_mut().expect("tree open");
    // Hung off the listing by hand: these tests are about CLICKS, not about
    // where it anchors (which since 2026-09-21 is higher up, at home or at
    // the root).
    t.anchor(dir.clone());
    t.insert_children(
        dir.clone(),
        vec![vp("file:///casa/a"), vp("file:///casa/b")],
    );
    t.insert_children(vp("file:///casa/a"), vec![vp("file:///casa/a/x")]);
    app
}

fn pulsar_en(app: &mut App, col: u16, row: u16) -> norte_tui::mouse::After {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    norte_tui::mouse::handle(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    )
}

/// Paints a frame and returns that frame's geometry to the model, which is
/// what the mouse resolves against.
fn after_paint(app: &mut App, area: ratatui::layout::Rect) -> Vec<norte_tui::ui::TreeZone> {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    let (geo, tabs, menus, sitios, branches) = (
        norte_tui::ui::pane_geometry(app, area),
        norte_tui::ui::tab_zones(app, area),
        norte_tui::ui::menu_zones(app, area),
        norte_tui::ui::places_zones(app, area),
        norte_tui::ui::tree_zones(app, area),
    );
    let huecos = norte_tui::ui::panel_slots(app, area);
    norte_tui::mouse::after_frame(
        app,
        geo,
        norte_tui::mouse::FrameZones {
            tabs,
            menus,
            places: sitios,
            tree: branches.clone(),
            session: None,
            slots: huecos,
            ..Default::default()
        },
    );
    branches
}

/// The mouse over the tree: clicking a row selects it and brings the
/// keyboard; clicking it again ACTIVATES it, which is the same as `Enter`.
///
/// The tree shipped with keyboard support and nothing more: its cells
/// belong to no listing, so a click there landed on "outside the panes" and
/// did nothing — a panel that paints and cannot be touched.
#[test]
fn pressing_a_tree_row_selects_it_and_pressing_again_activates_it() {
    let mut app = app_with_tree();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let zones = after_paint(&mut app, area);
    assert!(!zones.is_empty(), "the tree has clickable rows");
    let branch = zones
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("the first branch is visible");

    app.return_keys_to_panes();
    let after = pulsar_en(&mut app, branch.x1, branch.row);
    assert_eq!(
        after,
        norte_tui::mouse::After::Nothing,
        "the first click only selects"
    );
    assert_eq!(app.key_owner(), KeyOwner::Tree, "and brings the keyboard");
    assert_eq!(app.tree().map(norte_frontend::tree::Tree::cursor), Some(1));

    let after = pulsar_en(&mut app, branch.x1, branch.row);
    assert_eq!(after, norte_tui::mouse::After::TreeActivate);
    assert_eq!(
        app.tree_activate(),
        Some(vp("file:///casa/a")),
        "and there is a branch to take the listing to"
    );
}

/// Clicking the MARK (`▸`/`▾`) folds or unfolds that branch with a single
/// click: it is what the already-painted arrow says, and without it a
/// reader who only uses the mouse cannot close what they opened — `Enter`
/// unfolds and navigates, never folds.
#[test]
fn pressing_a_branchs_mark_folds_and_unfolds_it() {
    let mut app = app_with_tree();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let zones = after_paint(&mut app, area);
    let branch = zones
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("the first branch is visible");
    let rows = |app: &App| app.tree().map_or(0, |t| t.rows().len());
    let before = rows(&app);

    let after = pulsar_en(&mut app, branch.mark_x, branch.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing, "navigates nowhere");
    assert_eq!(rows(&app), before + 1, "unfolding shows its child");

    let _ = after_paint(&mut app, area);
    let after = pulsar_en(&mut app, branch.mark_x, branch.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing);
    assert_eq!(rows(&app), before, "and the same mark folds it back");
}

/// The menu bar is APPLICATION chrome, not the listings': with the
/// keyboard inside a side panel its key still has to open it.
///
/// `F10` (and `q`, and whatever binds `app.quit`) also QUITS with the
/// keyboard inside a side panel, and honors `[ui] confirm_quit` the same as
/// from a listing.
///
/// It was dead in all four — tree, places, processes and log —: `app.quit`
/// was not in their allowlists, so the panel ate it. The reader pressed
/// `F10`, nothing happened, closed the terminal window believing they had
/// quit, and `ntc` stayed alive holding the session lock: every following
/// `ntc` started up detached and "saved nothing." Only `Ctrl+C` actually
/// quit.
#[test]
fn exiting_works_with_the_keyboard_inside_a_side_panel() {
    for list in [
        norte_tui::app::ALLOW_PLACES,
        norte_tui::app::ALLOW_PROCESSES,
        norte_tui::app::ALLOW_LOG,
    ] {
        assert!(
            list.contains(&"app.quit"),
            "a side panel cannot eat the quit key"
        );
    }

    let mut app = app_with_tree();
    assert_eq!(app.key_owner(), KeyOwner::Tree);
    assert!(
        app.panel_chrome_command("app.quit"),
        "quitting is handled by the chrome, not the panel"
    );
    assert!(
        app.quit,
        "and it quits: with no tasks and `confirm_quit = auto` it does not ask"
    );

    // With `[ui] confirm_quit = always` it asks, same as from a listing.
    let mut app = app_with_tree();
    app.confirm_quit = norte_config::ConfirmQuit::Always;
    assert!(app.panel_chrome_command("app.quit"));
    assert!(!app.quit, "it does not quit on the first try");
    assert!(
        matches!(app.modal, Some(norte_tui::app::Modal::ConfirmQuit)),
        "it opens the confirmation: {:?}",
        app.modal.is_some()
    );
}

/// It was dead in all three — tree, places and processes —: `app.menu` was
/// not in their allowlists, so the panel ate it and the screen stayed the
/// same. It is the same lesson `layout.places` already brought here.
#[test]
fn the_menu_opens_with_the_keyboard_inside_a_side_panel() {
    for list in [
        norte_tui::app::ALLOW_PLACES,
        norte_tui::app::ALLOW_PROCESSES,
    ] {
        assert!(
            list.contains(&"app.menu"),
            "a side panel cannot eat the menu key"
        );
    }

    let mut app = app_with_tree();
    assert_eq!(
        app.key_owner(),
        KeyOwner::Tree,
        "the keyboard is in the tree"
    );
    assert!(
        app.panel_chrome_command("app.menu"),
        "the menu key is handled by the chrome, not the panel"
    );
    assert!(app.menu.is_some(), "and the menu opens");
    assert!(app.panel_chrome_command("app.menu"));
    assert!(app.menu.is_none(), "the same key closes it");
    assert!(
        !app.panel_chrome_command("dialog.up"),
        "what is not chrome is still handled by the panel"
    );
}
