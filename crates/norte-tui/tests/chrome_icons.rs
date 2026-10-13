//! The chrome's icons in pixels (kitty/sixel) where the terminal can show
//! them, the same shared drawings the window paints; the ASCII glyphs
//! where it cannot.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const W: u16 = 100;
const H: u16 = 20;

fn app(images: norte_config::Images) -> App {
    let dir = VPath::parse("file:///casa").unwrap();
    let es: Vec<Entry> = (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(format!("f{i}").into_bytes()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect();
    let mut app = App::new(Pane::new(dir.clone(), es.clone()), Pane::new(dir, es));
    app.menu_bar = true;
    app.chrome.images = Some(images);
    app
}

fn first_row(app: &mut App) -> String {
    let mut t = Terminal::new(TestBackend::new(W, H)).unwrap();
    ui::before_frame(app, ratatui::layout::Rect::new(0, 0, W, H));
    t.draw(|f| ui::draw(f, app)).unwrap();
    t.backend()
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// With pixels, each layout button gets its shared icon over the three
/// cells of its button (the click zone), and the ASCII glyph is not
/// painted under it.
#[test]
fn the_layout_buttons_draw_their_icons_with_pixels() {
    let mut app = app(norte_config::Images::Kitty);
    let row = first_row(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let icons = ui::chrome_icons_to_place(&app, area, (10, 20));
    let kinds: Vec<&str> = icons.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "layout:split-h",
            "layout:split-v",
            "layout:equalize",
            "layout:flip",
            "layout:pick"
        ]
    );
    for i in &icons {
        assert_eq!((i.rect.y, i.rect.width, i.rect.height), (0, 3, 1), "{i:?}");
    }
    assert!(!row.contains("[|]"), "{row}");
}

/// Without pixels nothing is placed and the glyphs stay.
#[test]
fn without_pixels_the_glyphs_stay() {
    let mut app = app(norte_config::Images::Off);
    let row = first_row(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    assert!(ui::chrome_icons_to_place(&app, area, (10, 20)).is_empty());
    assert!(row.contains("[|]"), "{row}");
}

/// An open menu covers the buttons: no icon is placed, so a sixel one
/// already up comes down.
#[test]
fn an_open_menu_places_no_icon() {
    let mut app = app(norte_config::Images::Kitty);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    assert!(ui::chrome_icons_to_place(&app, area, (10, 20)).is_empty());
}
