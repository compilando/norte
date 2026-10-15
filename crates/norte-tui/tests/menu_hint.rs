//! The key that opens the menu, at the LEFT of the menu bar: read first,
//! it says how to enter the titles that follow. On the right it sat next
//! to the layout buttons and read as their label.

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::keymap::{COMMANDS, Effective, Screen, presets};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

fn eff(screen: Screen) -> Effective {
    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("orthodox preset");
    Effective::build_for(&preset, &[], COMMANDS, screen).expect("effective")
}

fn app() -> App {
    let dir = VPath::parse("file:///casa").unwrap();
    let mut app = App::new(Pane::new(dir.clone(), vec![]), Pane::new(dir, vec![]));
    app.menu_bar = true;
    app.palette_rows = norte_tui::palette::build_rows(&eff(Screen::Browse), &eff(Screen::Viewer));
    app
}

fn chord(app: &App) -> String {
    app.palette_rows
        .iter()
        .find(|r| r.key == "app.menu")
        .map(|r| r.chord.clone())
        .expect("app.menu has a key in orthodox")
}

fn first_row(app: &mut App, w: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
    ui::before_frame(app, Rect::new(0, 0, w, 20));
    t.draw(|f| ui::draw(f, app)).unwrap();
    t.backend()
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

fn first_title_x(app: &App, w: u16) -> u16 {
    ui::menu_zones(app, Rect::new(0, 0, w, 20))
        .iter()
        .find(|z| z.hit == ui::MenuHit::Title(0))
        .map(|z| z.x0)
        .expect("first title")
}

/// With room, the bar starts with the key and the titles follow it; the
/// click zones move with the titles.
#[test]
fn the_key_opens_the_bar_on_the_left() {
    let mut app = app();
    let key = chord(&app);
    let row = first_row(&mut app, 140);
    assert!(row.trim_start().starts_with(&key), "{row:?}");
    let x = first_title_x(&app, 140);
    let title = norte_i18n::t(norte_frontend::menu::MENUS[0].title);
    let painted: String = row.chars().skip(usize::from(x)).collect();
    assert!(painted.trim_start().starts_with(&title), "{painted:?}");
}

/// Opening a menu does not move the titles: the key stays where it was.
#[test]
fn opening_a_menu_moves_no_title() {
    let mut app = app();
    let closed = first_title_x(&app, 140);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    assert_eq!(first_title_x(&app, 140), closed);
}

/// Narrow, a title is worth more than the key: the key goes first.
#[test]
fn narrow_the_key_yields_to_the_titles() {
    let mut app = app();
    let key = chord(&app);
    let row = first_row(&mut app, 50);
    assert!(!row.contains(&key), "{row:?}");
    assert_eq!(first_title_x(&app, 50), 0);
}
