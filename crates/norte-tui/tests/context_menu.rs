//! The TUI's context menu: the shared model (`norte_frontend::context_menu`)
//! opened by a right click or the Menu key, painted over the panes, run
//! through the same path as a key.

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_frontend::context_menu::Action;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::context_menu;
use norte_tui::mouse::{self, After};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const W: u16 = 80;
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

fn entries(dir: &VPath, n: usize) -> Vec<Entry> {
    (0..n)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(format!("f{i}").into_bytes()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn paint(app: &mut App) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(W, H)).expect("test terminal");
    ui::before_frame(app, ratatui::layout::Rect::new(0, 0, W, H));
    let area = terminal.draw(|f| ui::draw(f, app)).expect("draw").area;
    let geometry = ui::pane_geometry(app, area);
    mouse::after_frame(
        app,
        geometry,
        mouse::FrameZones {
            tabs: ui::tab_zones(app, area),
            menus: ui::menu_zones(app, area),
            panels: ui::panel_zones(app, area),
            keys: ui::key_zones(app, area),
            modal: ui::modal_zones(app, area),
            places: ui::places_zones(app, area),
            tree: ui::tree_zones(app, area),
            extensions: ui::extension_zones(app, area),
            help: ui::help_zones(app, area),
            session: ui::session_zone(app, area),
            status_items: ui::status_item_zones(app, area),
            borders: ui::resize_borders(app, area),
            slots: ui::panel_slots(app, area),
        },
    );
    terminal
        .backend()
        .to_string()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn app_on(wire: &str) -> App {
    let dir = vp(wire);
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, 5)),
        Pane::new(dir.clone(), entries(&dir, 5)),
    );
    let _ = paint(&mut app);
    app
}

fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// The first painted cell of a real entry in the left pane.
fn a_row(app: &App) -> (u16, u16, usize) {
    for col in 0..W {
        for row in 0..H {
            if let Some(h) = mouse::hit_test(app, col, row)
                && h.pane == 0
                && let Some(i) = h.index
                && !app.panes[0].is_parent_row(i)
            {
                return (col, row, i);
            }
        }
    }
    panic!("no row painted");
}

fn right_click(app: &mut App, col: u16, row: u16) {
    let _ = mouse::handle(app, ev(MouseEventKind::Down(MouseButton::Right), col, row));
    let _ = paint(app);
}

fn labels(app: &App) -> Vec<String> {
    app.context_menu
        .as_ref()
        .expect("menu open")
        .rows
        .iter()
        .map(|r| r.label.clone())
        .collect()
}

#[test]
fn a_right_click_on_a_row_opens_its_menu_on_that_row() {
    let mut app = app_on("file:///casa");
    let (col, row, i) = a_row(&app);
    right_click(&mut app, col, row);
    let l = labels(&app);
    assert_eq!(l[0], norte_i18n::t("ctx-open"));
    assert!(l.contains(&norte_i18n::t("menu-item-pane-copy")), "{l:?}");
    assert_eq!(app.panes[0].cursor(), i, "the cursor goes to the row");
    let header = &app.context_menu.as_ref().unwrap().header;
    let name = app.panes[0].entries()[i]
        .path
        .file_name()
        .unwrap()
        .as_bytes();
    assert!(
        header.contains(&*String::from_utf8_lossy(name)),
        "the header names the operand: {header}"
    );
}

#[test]
fn a_click_on_an_entry_runs_it_through_the_keys_path() {
    let mut app = app_on("file:///casa");
    let (col, row, _) = a_row(&app);
    right_click(&mut app, col, row);
    let (c, r) = context_menu::cell_of(&app, "pane.copy").expect("copy is painted");
    let after = mouse::handle(&mut app, ev(MouseEventKind::Down(MouseButton::Left), c, r));
    assert!(matches!(after, After::PanelBar), "{after:?}");
    assert_eq!(app.pending_panel_command.as_deref(), Some("pane.copy"));
    assert!(app.context_menu.is_none(), "it closes before running");
}

/// The listing changed under the open menu: its entry would act on
/// whatever now sits there, so it runs nothing and closes.
#[test]
fn a_target_that_changed_runs_nothing() {
    let mut app = app_on("file:///casa");
    let (col, row, _) = a_row(&app);
    right_click(&mut app, col, row);
    let (c, r) = context_menu::cell_of(&app, "pane.copy").expect("copy is painted");
    let other = vp("file:///otra");
    app.panes[0] = Pane::new(other.clone(), entries(&other, 5));
    let after = mouse::handle(&mut app, ev(MouseEventKind::Down(MouseButton::Left), c, r));
    assert!(!matches!(after, After::PanelBar), "{after:?}");
    assert!(app.pending_panel_command.is_none());
    assert!(app.context_menu.is_none());
}

/// A dimmed entry says why and runs nothing; the menu stays.
#[test]
fn a_disabled_entry_runs_nothing_and_stays_open() {
    let mut app = app_on("mem:///casa");
    let (col, row, _) = a_row(&app);
    right_click(&mut app, col, row);
    let menu = app.context_menu.as_ref().unwrap();
    let term = menu
        .rows
        .iter()
        .find(|r| r.action == Action::Command("app.terminal"))
        .expect("terminal here is listed");
    assert!(!term.enabled);
    assert_eq!(term.reason, norte_i18n::t("reason-not-on-this-disk"));
    let (c, r) = context_menu::cell_of(&app, "app.terminal").expect("painted");
    let after = mouse::handle(&mut app, ev(MouseEventKind::Down(MouseButton::Left), c, r));
    assert!(!matches!(after, After::PanelBar));
    assert!(app.context_menu.is_some());
}

/// ADR 0178: a click outside closes it.
#[test]
fn a_click_outside_closes_it() {
    let mut app = app_on("file:///casa");
    let (col, row, _) = a_row(&app);
    right_click(&mut app, col, row);
    let _ = mouse::handle(
        &mut app,
        ev(MouseEventKind::Down(MouseButton::Left), W - 1, H - 1),
    );
    assert!(app.context_menu.is_none());
}

/// The empty area below the rows gets the folder's menu and touches no
/// marks.
#[test]
fn the_empty_area_gets_the_folders_menu() {
    let mut app = app_on("file:///casa");
    let (col, _, _) = a_row(&app);
    let row = (0..H)
        .rev()
        .find(|&r| mouse::hit_test(&app, col, r).is_some_and(|h| h.pane == 0 && h.index.is_none()))
        .expect("an empty cell below the rows");
    right_click(&mut app, col, row);
    let l = labels(&app);
    assert!(l.contains(&norte_i18n::t("menu-item-pane-mkdir")), "{l:?}");
}

/// Keys: arrows walk, Enter picks, Esc closes.
#[test]
fn keys_walk_pick_and_close() {
    let mut app = app_on("file:///casa");
    let (col, row, _) = a_row(&app);
    right_click(&mut app, col, row);
    assert_eq!(context_menu::on_key(&mut app, KeyCode::Down), None);
    assert_eq!(app.context_menu.as_ref().unwrap().cursor, 1);
    let picked = context_menu::on_key(&mut app, KeyCode::Enter);
    assert_eq!(picked, Some(Action::Command("pane.view")));
    assert!(app.context_menu.is_none());
    right_click(&mut app, col, row);
    assert_eq!(context_menu::on_key(&mut app, KeyCode::Esc), None);
    assert!(app.context_menu.is_none());
}

/// The Menu key opens it on the cursor's row.
#[test]
fn the_menu_key_opens_it_on_the_cursor() {
    let mut app = app_on("file:///casa");
    let (_, _, i) = a_row(&app);
    app.panes[0].set_cursor(i);
    context_menu::open_on_cursor(&mut app);
    assert_eq!(labels(&app)[0], norte_i18n::t("ctx-open"));
    let lines = paint(&mut app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&norte_i18n::t("menu-item-pane-copy"))),
        "painted:\n{}",
        lines.join("\n")
    );
}
