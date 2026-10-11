//! The theme REALLY paints (ADR 0020, phase T5): snapshots are just text,
//! so here ratatui's BUFFER is inspected — that a directory comes out with
//! the theme's color and that depth degradation applies.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{ColorDepth, Theme};
use norte_tui::app::{App, Pane};
use norte_tui::theme::TuiTheme;
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

fn app_con_dir(depth: ColorDepth) -> App {
    let dir = vp("file:///casa");
    let entries = vec![Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
    }];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    let theme = Theme::preset("catppuccin-mocha").unwrap().unwrap();
    app.theme = TuiTheme::new(theme, depth);
    app
}

/// The panel bar WITH NAMES (spec 2026-09-10): each button paints its name
/// with the access letter underlined; with `letters` or with no room for
/// all of them, it falls back to letters; and the mouse zones measure the
/// same as what is painted in both cases.
#[test]
fn the_pane_bar_paints_names_with_the_underlined_letter_and_falls_back_to_letters() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.panel_bar = true;
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Top);
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Names);

    // 80 columns, the common terminal: the names must fit there (ADR 0171).
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let row: String = (0..80)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(
        row.contains("Sitios"),
        "with names, the name reads: {row:?}"
    );
    let s = row.find('S').expect("the S of Sitios");
    let s = u16::try_from(row[..s].chars().count()).expect("it fits");
    assert!(
        terminal.backend().buffer()[(s, 1)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
        "the access letter is underlined"
    );
    let zones = ui::panel_zones(&app, ratatui::layout::Rect::new(0, 0, 80, 16));
    let width_zone = zones[0].x1 - zones[0].x0 + 1;
    assert_eq!(
        usize::from(width_zone),
        "Sitios".len() + 2,
        "the zone measures what is painted"
    );

    // No room for all the names: letters, and three-cell zones.
    let mut terminal = Terminal::new(TestBackend::new(30, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let row: String = (0..30)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(!row.contains("Sitios"), "no room, letters: {row:?}");
    let zones = ui::panel_zones(&app, ratatui::layout::Rect::new(0, 0, 30, 16));
    assert_eq!(zones[0].x1 - zones[0].x0 + 1, 3);

    // `letters` requested by hand, with room to spare: letters just the same.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let row: String = (0..80)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(!row.contains("Sitios"), "{row:?}");
}

/// The pane footer (spec 2026-09-10): with `[ui] pane_footer` on, the
/// bottom border states how many directories and files there are and the
/// free space of the volume cached in `App`; off, the border stays clean;
/// and with the incremental search open it shows the search instead.
#[test]
fn the_pane_footer_counts_and_tells_the_free_space() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.chrome.pane_footer = Some(true);
    app.volumes = vec![norte_proto::methods::Volume {
        mount: vp("file:///"),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(200 << 30),
        free_bytes: Some(120 << 30),
        read_only: false,
    }];
    let row_down = |app: &App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        // The panes' bottom border row: the last one minus the status bar.
        (0..100)
            .map(|x| terminal.backend().buffer()[(x, 14)].symbol().to_string())
            .collect()
    };
    let con = row_down(&app);
    assert!(
        con.contains("1 dir ·") && con.contains("0 ficheros"),
        "{con:?}"
    );
    assert!(con.contains("120") && con.contains("libres"), "{con:?}");

    app.chrome.pane_footer = Some(false);
    let sin = row_down(&app);
    assert!(
        !sin.contains("dirs"),
        "off, the border stays clean: {sin:?}"
    );
}

/// The key bar (spec 2026-09-10): with `[ui] key_bar` on it occupies the
/// LAST row with the number and label of each bound key, the status bar
/// moves up one row, an empty cell is not a zone, and a click on a cell
/// leaves the key synthesized for the loop to dispatch through `on_key`.
/// Off, the last row goes back to being the status one.
#[test]
fn the_key_bar_paints_what_is_bound_and_a_click_is_the_key() {
    use norte_frontend::keybar::KeyCell;
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.chrome.key_bar = Some(true);
    app.key_bars.browse = vec![
        KeyCell {
            key: 1,
            label: String::new(),
            command: None,
        },
        KeyCell {
            key: 2,
            label: "Copiar".to_owned(),
            command: Some("pane.copy".to_owned()),
        },
    ];
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let row = |app: &App, y: u16| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        (0..80)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
            .collect()
    };
    let ultima = row(&app, 15);
    // At 80 columns a cell is 8: `Copiar` fills it, so the space after the
    // number gives way to the blank that separates it from the next cell
    // (review of 2026-10-07). The ZONES do not change — they come from
    // `keybar::layout`, which splits the row the same way.
    assert!(ultima.contains("2Copiar "), "the bound cell: {ultima:?}");
    assert!(
        ultima.starts_with('1'),
        "the empty one only carries the number: {ultima:?}"
    );
    assert!(
        row(&app, 14).contains("/casa"),
        "the status bar moves up one row"
    );

    let zones = ui::key_zones(&app, area);
    assert_eq!(zones.len(), 1, "an empty cell is not a zone: {zones:?}");
    assert_eq!((zones[0].key, zones[0].row), (2, 15));
    norte_tui::mouse::after_frame(
        &mut app,
        None,
        norte_tui::mouse::FrameZones {
            keys: zones.clone(),
            ..Default::default()
        },
    );
    let click = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: zones[0].x0,
        row: 15,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    assert_eq!(
        norte_tui::mouse::handle(&mut app, click),
        norte_tui::mouse::After::SynthKey
    );
    assert_eq!(
        app.pending_key.map(|k| k.code),
        Some(crossterm::event::KeyCode::F(2)),
        "the click leaves the key for `on_key`"
    );

    app.chrome.key_bar = Some(false);
    assert!(
        row(&app, 15).contains("/casa"),
        "off, the last row is the status one"
    );
}

/// A CLOSED panel does not paint like a lit-up block.
///
/// The bar styled `Closed` with `Role::StatusBar`, which is the STATUS
/// BAR's style — on half the themes, a live background and dark text. The
/// panel bar clears with the base background, so closed buttons came out as
/// color blocks over it and open ones as normal text: the visual weight,
/// backward. Looking at the bar answered the opposite of what you ask, and
/// that is why the state looked out of sync.
///
/// What is pinned is the PROPERTY, not a color: a closed button cannot
/// carry a background different from the bar's. With that, any theme that
/// inverts that role breaks it again and it shows here.
#[test]
fn a_closed_panel_is_not_painted_as_a_lit_block() {
    for preset in norte_theme::preset_names() {
        let mut app = app_con_dir(ColorDepth::Truecolor);
        let theme = norte_theme::Theme::preset(preset)
            .expect("valid preset")
            .expect("preset exists");
        app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
        app.panel_bar = true;
        app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Top);

        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        // Row 1 is the panel bar: 0 is the menu. With this test's `App`
        // there is no side panel open, so ALL buttons are closed and the
        // whole row has to read as the bar's background with text on top.
        let background = buf[(0, 1)].bg;
        let different: Vec<String> = (0..80)
            .filter(|x| buf[(*x, 1)].bg != background)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            different.is_empty(),
            "[{preset}] with every panel CLOSED, the bar paints color blocks \
             at {different:?} — the visual weight backward: what is off \
             standing out and what is open as normal text"
        );
    }
}

/// The default panel COLUMN tells open from closed without `bold` or `dim`,
/// which many terminals ignore on a single glyph: an open panel carries a
/// `▎` rule and a closed one paints in another colour. On vscode-dark
/// `Title` and `Regular` are both `#cccccc`, so bold-vs-dim was all there
/// was (2026-10-05).
#[test]
fn the_default_column_tells_an_open_panel_from_a_closed_one() {
    let mut app = app_con_dir(ColorDepth::Truecolor);
    let theme = Theme::preset("vscode-dark").unwrap().unwrap();
    app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
    app.panel_bar = true;
    app.toggle_places();

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    let row_of = |icon: &str| {
        (0..24)
            .find(|y| buf[(1, *y)].symbol() == icon)
            .unwrap_or_else(|| panic!("{icon} is not in the column"))
    };
    let (open, closed) = (row_of("★"), row_of("∿"));
    assert_eq!(buf[(0, open)].symbol(), "▎", "the open panel has its rule");
    assert_eq!(buf[(0, closed)].symbol(), " ", "a closed one has none");
    assert_ne!(
        buf[(1, open)].fg,
        buf[(1, closed)].fg,
        "open and closed icons paint in different colours"
    );
}

/// A big column (`images = "kitty"`) over vscode-dark, places open.
fn app_big_rail() -> App {
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.theme = TuiTheme::new(
        Theme::preset("vscode-dark").unwrap().unwrap(),
        ColorDepth::Truecolor,
    );
    app.panel_bar = true;
    app.chrome.images = Some(norte_config::Images::Kitty);
    app.toggle_places();
    app
}

/// Kitty's pixels sit ABOVE the text: an icon left placed under a menu,
/// help, the palette or which-key would cover them. Nothing is placed
/// while anything is painted over the body.
#[test]
fn no_icons_under_an_overlay() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 40);
    let mut app = app_big_rail();
    assert!(
        !ui::rail_icons_to_place(&app, area, CELL).is_empty(),
        "nothing over it"
    );
    app.menu = Some(norte_frontend::menu::MenuState::new());
    assert!(
        ui::rail_icons_to_place(&app, area, CELL).is_empty(),
        "menu open"
    );
    app.menu = None;
    app.help = Some(norte_tui::app::HelpView::new(
        norte_i18n::Lang::En,
        Vec::new(),
    ));
    assert!(
        ui::rail_icons_to_place(&app, area, CELL).is_empty(),
        "help open"
    );
}

/// Each icon sits on the 2×2 cells `draw_rail` left blank — column 1, two
/// rows — in the state's colour, and only kinds with an SVG get one.
#[test]
fn icons_sit_on_the_reserved_cells() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 40);
    let app = app_big_rail();
    let icons = ui::rail_icons_to_place(&app, area, CELL);
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    for i in &icons {
        assert_eq!((i.rect.x, i.rect.width, i.rect.height), (1, 2, 2), "{i:?}");
        for (x, y) in [(1, 0), (2, 0), (1, 1), (2, 1)] {
            let cell = &buf[(i.rect.x + x - 1, i.rect.y + y)];
            assert_eq!(cell.symbol(), " ", "{} at {:?}", i.kind, (x, y));
        }
        assert!(norte_frontend::panelbar::icon_svg(&i.kind).is_some());
    }
    assert!(
        icons.iter().any(|i| i.kind == "terminal"),
        "the terminal has its picture since 2026-10-08"
    );
    let places = icons.iter().find(|i| i.kind == "places").expect("places");
    assert_eq!(places.rgb, [0xcc, 0xcc, 0xcc], "open: Title");
    assert_eq!(
        places.canvas,
        norte_tui::rail_icons::canvas_for(CELL),
        "the slot's proportions"
    );
}

/// 9×19 px cells, a common monospace size.
const CELL: (u16, u16) = (9, 19);

/// Which-key, the go-to pop-up or the splash hide the pixels (they sit
/// above the text) but do not hide the column: its slots then show the
/// one-cell glyph instead of a blank square.
#[test]
fn a_big_column_with_its_pixels_hidden_shows_glyphs() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 40);
    let mut app = app_big_rail();
    app.which_key = Some(norte_frontend::whichkey::WhichKeyRows::default());
    assert!(ui::rail_icons_to_place(&app, area, CELL).is_empty());
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    let star = norte_frontend::panelbar::icon("places", norte_frontend::panelbar::IconSet::Unicode)
        .expect("icon");
    assert!(
        (0..40).any(|y| buf[(1, y)].symbol() == star),
        "the places glyph is painted in its slot"
    );
}

/// On every preset a closed icon reads dimmer than an open one: its colour
/// sits closer to the background. Most presets define no `muted`, and the
/// glyph column's `dim` does not reach pixels — the colour has to.
#[test]
fn closed_icons_are_dimmer_than_open_ones_on_every_preset() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 40);
    let dist = |a: [u8; 3], b: [u8; 3]| -> u32 {
        a.iter().zip(b).map(|(x, y)| u32::from(x.abs_diff(y))).sum()
    };
    for name in norte_theme::preset_names() {
        let mut app = app_big_rail();
        let theme = Theme::preset(name).unwrap().unwrap();
        let bg = theme
            .style(norte_theme::Role::Background)
            .bg
            .map_or([0, 0, 0], |c| [c.r, c.g, c.b]);
        app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
        let icons = ui::rail_icons_to_place(&app, area, CELL);
        let open = icons.iter().find(|i| i.kind == "places").expect("places");
        let closed = icons.iter().find(|i| i.kind == "log").expect("log");
        assert!(
            dist(closed.rgb, bg) < dist(open.rgb, bg),
            "{name}: closed {:?} vs open {:?} over {bg:?}",
            closed.rgb,
            open.rgb
        );
    }
}

/// The disk map with a landed report paints its children: the report of
/// 2026-10-06 showed an empty frame with no status over a directory of one
/// subdirectory and nine files (3 MiB).
#[test]
fn a_landed_disk_map_paints_its_children() {
    use norte_proto::methods::{DirUsageChild, FsDirUsageReportResult};
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.theme = TuiTheme::new(
        Theme::preset("vscode-dark").unwrap().unwrap(),
        ColorDepth::Truecolor,
    );
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let child = |name: &str, kind, bytes| DirUsageChild {
        name: Segment::new(name.as_bytes().to_vec()).unwrap(),
        kind,
        bytes,
        entries: 1,
        partial: false,
    };
    let mut children = vec![child("estacion", EntryKind::Dir, 4096)];
    for (i, b) in [
        30_000, 361_000, 11_000, 173, 89_000, 1_400_000, 1_100_000, 38_000, 26_000,
    ]
    .into_iter()
    .enumerate()
    {
        children.push(child(&format!("f{i}.pdf"), EntryKind::File, b));
    }
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.land(
        FsDirUsageReportResult {
            children,
            listed: true,
            ..FsDirUsageReportResult::default()
        },
        true,
    );
    let mut terminal = Terminal::new(TestBackend::new(116, 37)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen: Vec<String> = (0..37)
        .map(|y| {
            (0..116)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_owned())
                .collect()
        })
        .collect();
    assert!(
        screen.iter().any(|l| l.contains("f5.pdf")),
        "the biggest file is labelled:\n{}",
        screen.join("\n")
    );
    // And the rectangles are FILLED: the label's cell carries the class
    // colour as its background, not as text on nothing. (It asserted
    // REVERSED until #423: the fill is now a real background colour, with
    // a foreground chosen to read on it.)
    let buf = terminal.backend().buffer();
    let (y, line) = screen
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains("f5.pdf"))
        .expect("labelled");
    let x = line
        .find("f5.pdf")
        .map(|b| line[..b].chars().count())
        .expect("x");
    let cell = &buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())];
    // Not Rgb-ness (an unpainted panel cell is Rgb too): the fill is one of
    // the document class's tones, and not the panel's own colour.
    let fills: Vec<Color> = (0..3)
        .filter_map(|shade| {
            app.theme
                .class_fill(norte_frontend::treemap::ChildClass::Document, shade)
        })
        .map(|(bg, _)| bg)
        .collect();
    assert!(fills.contains(&cell.bg), "a class tone {fills:?}: {cell:?}");
    for role in [
        norte_theme::Role::Background,
        norte_theme::Role::PaneBackground,
    ] {
        assert_ne!(Some(cell.bg), app.theme.role(role).bg, "{role:?}: {cell:?}");
    }
}

/// A home is all folders (landing shots, 2026-10-08): every rectangle the
/// same blue and nothing told them apart. Now touching folders take
/// different tones of it (ADR 0175).
#[test]
fn a_map_of_folders_paints_more_than_one_tone() {
    use norte_proto::methods::{DirUsageChild, FsDirUsageReportResult};
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.theme = TuiTheme::new(
        Theme::preset("vscode-dark").unwrap().unwrap(),
        ColorDepth::Truecolor,
    );
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let children: Vec<DirUsageChild> = [900u64, 700, 500, 400, 300, 200]
        .iter()
        .enumerate()
        .map(|(i, b)| DirUsageChild {
            name: Segment::new(format!("d{i}").into_bytes()).unwrap(),
            kind: EntryKind::Dir,
            bytes: *b,
            entries: 1,
            partial: false,
        })
        .collect();
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.land(
        FsDirUsageReportResult {
            children,
            listed: true,
            ..FsDirUsageReportResult::default()
        },
        true,
    );
    let mut terminal = Terminal::new(TestBackend::new(116, 37)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let buf = terminal.backend().buffer();
    // The fill is the cell's BACKGROUND (#423), read where each label starts.
    let fills: std::collections::BTreeSet<String> = (0..6)
        .filter_map(|i| label_cell(buf, &format!("d{i}")))
        .map(|c| format!("{:?}", c.bg))
        .collect();
    assert!(fills.len() >= 2, "one tone for every folder: {fills:?}");
}

/// The first cell of the label `name` inside a map, if it is painted. Not
/// the title row, nor the footer, nor a path that merely ends in the name
/// (`/tmp/d0`): only a match that starts a word counts.
fn label_cell<'a>(
    buf: &'a ratatui::buffer::Buffer,
    name: &str,
) -> Option<&'a ratatui::buffer::Cell> {
    for y in 1..buf.area.height.saturating_sub(2) {
        let text: Vec<&str> = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        let joined: String = text.concat();
        for (b, _) in joined.match_indices(name) {
            let x = joined[..b].chars().count();
            let before = x.checked_sub(1).map(|i| text[i]);
            if before.is_some_and(|c| c == "/" || c.chars().all(char::is_alphanumeric)) {
                continue;
            }
            return Some(&buf[(u16::try_from(x).unwrap(), y)]);
        }
    }
    None
}

/// One child per class, drawn under `theme`.
fn class_map(theme: Theme, depth: ColorDepth) -> (TuiTheme, ratatui::buffer::Buffer) {
    use norte_proto::methods::{DirUsageChild, FsDirUsageReportResult};
    let mut app = app_con_dir(depth);
    app.theme = TuiTheme::new(theme, depth);
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let children: Vec<DirUsageChild> = [
        ("film.mp4", EntryKind::File, 900_000u64),
        ("shot.png", EntryKind::File, 800_000),
        ("book.pdf", EntryKind::File, 700_000),
        ("disk.iso", EntryKind::File, 600_000),
        ("pack.zip", EntryKind::File, 500_000),
        ("main.rs", EntryKind::File, 400_000),
        ("folder", EntryKind::Dir, 300_000),
    ]
    .into_iter()
    .map(|(name, kind, bytes)| DirUsageChild {
        name: Segment::new(name.as_bytes().to_vec()).unwrap(),
        kind,
        bytes,
        entries: 1,
        partial: false,
    })
    .collect();
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.land(
        FsDirUsageReportResult {
            children,
            listed: true,
            ..FsDirUsageReportResult::default()
        },
        true,
    );
    let mut terminal = Terminal::new(TestBackend::new(116, 37)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    (app.theme.clone(), terminal.backend().buffer().clone())
}

/// The disk map painted its rectangles in the text colour, and left audio,
/// video and code as holes in the panel (#423): the roles it borrowed carry
/// the theme's background as their FOREGROUND (`selection`, `match`), so
/// reversing them filled with the panel; `regular` was the text; `badge` and
/// `muted` have no colour in most themes. Every class now gets a fill that
/// is neither, on every preset, and the fills tell the classes apart.
#[test]
fn every_class_of_the_disk_map_has_its_own_fill_on_every_preset() {
    use norte_frontend::treemap::{ChildClass, Ground};
    let classes = [
        ("film.mp4", ChildClass::Media),
        ("shot.png", ChildClass::Image),
        ("book.pdf", ChildClass::Document),
        ("disk.iso", ChildClass::Other),
        ("pack.zip", ChildClass::Archive),
        ("main.rs", ChildClass::Code),
        ("folder", ChildClass::Directory),
    ];
    for (name, depth) in norte_theme::preset_names().into_iter().flat_map(|n| {
        [
            ColorDepth::Truecolor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ]
        .map(|d| (n, d))
    }) {
        let theme = Theme::preset(name).unwrap().unwrap();
        let (tt, buf) = class_map(theme.clone(), depth);
        let name = &format!("{name} at {depth:?}");
        let mut panel = vec![Color::Reset];
        for role in [
            norte_theme::Role::Background,
            norte_theme::Role::PaneBackground,
        ] {
            panel.extend(tt.role(role).bg);
        }
        let text = tt.role(norte_theme::Role::Regular).fg;
        let mut seen: Vec<(&str, Color)> = Vec::new();
        for (file, class) in classes {
            let resolved = class.colour_candidates().iter().find_map(|(role, ground)| {
                let s = theme.style(*role);
                match ground {
                    Ground::Fg => s.fg,
                    Ground::Bg => s.bg,
                }
            });
            let Some(resolved) = resolved else {
                continue;
            };
            let cell = label_cell(&buf, file).unwrap_or_else(|| panic!("{name}: {file} labelled"));
            assert!(
                !panel.contains(&cell.bg),
                "{name}: {file} is a hole in the panel: {:?}",
                cell.bg
            );
            // Some themes DO give a class the text colour (vscode's `title`
            // is its `regular`): that is the theme's choice and the window
            // paints it too; only a map that fell into it by accident fails.
            // In fewer colours a class and the text may quantise to one
            // index: that is the palette's limit, not the map's fall.
            let theme_says_text = theme.style(norte_theme::Role::Regular).fg == Some(resolved)
                || depth != ColorDepth::Truecolor;
            assert!(
                theme_says_text || Some(cell.bg) != text,
                "{name}: {file} is painted in the text colour: {:?}",
                cell.bg
            );
            assert!(
                !cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                "{name}: {file} is filled, not reversed"
            );
            assert_ne!(cell.fg, cell.bg, "{name}: {file}'s label reads");
            seen.push((file, cell.bg));
        }
        // The classes stand apart, as far as the theme lets them: several
        // presets give `info`, `border-focus`, `selection` and `title` one
        // and the same blue, so folders, code and media can share a fill
        // there (the window has the same collisions: it reads the same
        // table). What a theme cannot give, the map cannot invent; what it
        // can, it must show: at least three different fills.
        let distinct: std::collections::BTreeSet<String> =
            seen.iter().map(|(_, c)| format!("{c:?}")).collect();
        // In 16 colours a pale theme's classes can all land on the same two
        // greys: two is what a palette that small can promise.
        let least = if depth == ColorDepth::Truecolor { 3 } else { 2 };
        assert!(
            distinct.len() >= least,
            "{name}: the classes are told apart: {seen:?}"
        );
    }
}

/// A theme with no colour at all keeps what the map always did: the role's
/// own style, reversed.
#[test]
fn a_monochrome_theme_still_reverses_the_map() {
    let (_, buf) = class_map(
        Theme::from_toml("name = \"mono\"").unwrap(),
        ColorDepth::Truecolor,
    );
    let cell = label_cell(&buf, "film.mp4").expect("labelled");
    assert!(
        cell.modifier.contains(ratatui::style::Modifier::REVERSED),
        "{cell:?}"
    );
}

/// A long measurement says what it has counted in the title: "measuring"
/// alone for minutes read as stuck (2026-10-08).
#[test]
fn a_running_measurement_counts_in_the_title() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.measuring(norte_proto::TaskId::new(5));
    map.progress(12_345, 3 << 30);
    let mut terminal = Terminal::new(TestBackend::new(116, 37)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen: String = (0..37)
        .map(|y| {
            (0..116)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_owned())
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(screen.contains("12345 items"), "{screen}");
}

/// An empty finished map says so, and the title names WHICH directory it
/// measured: a blank frame with only "Disk map" could not tell an empty
/// directory from the other pane's, from one still loading.
#[test]
fn an_empty_disk_map_says_so_and_names_its_directory() {
    use norte_proto::methods::FsDirUsageReportResult;
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.land(
        FsDirUsageReportResult {
            listed: true,
            ..FsDirUsageReportResult::default()
        },
        true,
    );
    assert!(map.nothing_to_draw(), "empty and done");
    let note = norte_i18n::t("disk-map-empty");
    let mut terminal = Terminal::new(TestBackend::new(116, 37)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen: String = (0..37)
        .map(|y| {
            (0..116)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_owned())
                .collect::<String>()
                + "\n"
        })
        .collect();
    let first_words: String = note
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(screen.contains(&first_words), "the note:\n{screen}");
    let title_row = screen
        .lines()
        .find(|l| l.contains(&norte_i18n::t("disk-map-title")))
        .expect("the map's title");
    assert!(
        title_row.contains("casa"),
        "names the directory: {title_row}"
    );
}

/// The View menu marks the panels that are open: it listed them with
/// their keys and said nothing of which were open (review of 2026-10-07).
#[test]
fn the_view_menu_marks_open_panels() {
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.toggle_log();
    let view = norte_frontend::menu::MENUS
        .iter()
        .position(|m| m.title == "menu-view")
        .expect("a View menu");
    let mut menu = norte_frontend::menu::MenuState::new();
    menu.open(view);
    app.menu = Some(menu);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen: String = (0..40)
        .map(|y| {
            (0..120)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_owned())
                .collect::<String>()
                + "\n"
        })
        .collect();
    let log = norte_i18n::t("menu-item-layout-log");
    let tree = norte_i18n::t("menu-item-pane-tree");
    assert!(
        screen.contains(&format!("✓ {log}")),
        "open, marked:\n{screen}"
    );
    assert!(!screen.contains(&format!("✓ {tree}")), "closed, not marked");
}

/// A layout that brings the disk map — yesterday's session, a profile —
/// does not go through the toggle that creates its state: the slot stayed
/// blank and never measured (2026-10-06). Like the tree and the timeline,
/// it is seeded, and it asks to be measured.
#[test]
fn a_restored_disk_map_is_seeded_and_measures() {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.set_layout(Node::Split {
        dir: Dir::Vertical,
        sizes: vec![Size::Weight(1), Size::Fixed(12)],
        children: vec![
            Node::slot(SlotId(70), KindId::browser()),
            Node::slot(SlotId(71), KindId::new("disk-map")),
        ],
    });
    assert!(app.panes.disk_map(SlotId(71)).is_some(), "state seeded");
    assert!(app.disk_map_wants_measure(), "and it asks to be measured");
}

/// A listing refreshed over the directory the map measured — a copy, a
/// mkdir, Ctrl+R — leaves the map stale: it measures again. Before, only
/// the external watcher said so, and norte's own changes (or a remote
/// directory, which has no watcher) left the old map up.
#[test]
fn a_refresh_of_the_measured_directory_measures_again() {
    let mut app = App::new(
        Pane::new(vp("file:///casa"), Vec::new()),
        Pane::new(vp("file:///otro"), Vec::new()),
    );
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    app.panes.disk_map_mut(slot).expect("map").aim(dir);
    assert!(!app.disk_map_wants_measure(), "aimed: nothing to do");
    app.listings_refreshed([false, true]);
    assert!(
        !app.disk_map_wants_measure(),
        "the other pane's dir: not ours"
    );
    app.listings_refreshed([true, false]);
    assert!(app.disk_map_wants_measure(), "ours changed: measure again");
}

/// A refresh while a measurement RUNS leaves it alone: restarting it on
/// every task of a copy batch meant a `$HOME` measurement that never
/// finished. The window does the same.
#[test]
fn a_refresh_does_not_restart_a_running_measurement() {
    let mut app = App::new(
        Pane::new(vp("file:///casa"), Vec::new()),
        Pane::new(vp("file:///otro"), Vec::new()),
    );
    app.open_disk_map();
    let slot = app.disk_map_slot().expect("open");
    let dir = app.focused().dir().clone();
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.aim(dir);
    map.measuring(norte_proto::TaskId::new(7));
    app.listings_refreshed([true, false]);
    assert!(!app.disk_map_wants_measure(), "still measuring: left alone");
    // But not forgotten: when that measurement lands, it measures again —
    // its numbers are from before the change.
    let map = app.panes.disk_map_mut(slot).expect("map");
    map.land(
        norte_proto::methods::FsDirUsageReportResult::default(),
        true,
    );
    app.disk_map_landed();
    assert!(
        app.disk_map_wants_measure(),
        "the change during it is measured"
    );
}

/// `true` if ANY buffer cell has that foreground color.
fn hay_fg(app: &App, want: Color) -> bool {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .any(|cell| cell.fg == want)
}

#[test]
fn truecolor_paints_the_dir_with_the_themes_blue() {
    // catppuccin-mocha: dir = #89b4fa. In truecolor it comes out as is.
    let app = app_con_dir(ColorDepth::Truecolor);
    assert!(
        hay_fg(&app, Color::Rgb(0x89, 0xb4, 0xfa)),
        "the directory did not come out with the theme's blue in truecolor"
    );
}

/// #103 review BLOCKER: the mark gutter (`entry_item`, `ui.rs`) painted
/// `theme.role(Role::Mark)` UNCONDITIONALLY — only the GLYPH (`*`/` `) was
/// conditional, not the style. Every shipped preset defines `mark` as ONLY
/// a `bg` (`crates/norte-theme/presets/*.toml`), so that color stripe
/// showed up in column 1 of EVERY row, marked or not. This test goes
/// through the real `ui::draw` (a text snapshot CANNOT see a color) and
/// pins both halves: with no marks, the mark `bg` shows up in NO cell; with
/// one marked row, it shows up in EXACTLY one cell, and that cell is the
/// gutter's `*` glyph.
#[test]
fn the_mark_gutter_only_paints_the_marked_row() {
    let dir = vp("file:///casa");
    let entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"b".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
    ];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    let theme = Theme::preset("default").unwrap().unwrap();
    app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
    // `default.toml`: `mark = { bg = "#3d3315" }`.
    let mark_bg = Color::Rgb(0x3d, 0x33, 0x15);

    let mark_bg_cells = |app: &App| -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.bg == mark_bg)
            .map(|cell| cell.symbol().to_string())
            .collect()
    };

    assert!(
        mark_bg_cells(&app).is_empty(),
        "with no marks, the mark bg must not show up in any cell: {:?}",
        mark_bg_cells(&app)
    );

    // Marks row 0 ("a") and moves the cursor to row 1 ("b") so the cursor's
    // highlight (`Role::Selection`, another color) does not overlap the
    // marked row when painted.
    app.focused_mut().toggle_mark();
    app.focused_mut().move_down(1);

    let cells = mark_bg_cells(&app);
    assert_eq!(
        cells.len(),
        1,
        "the mark bg must show up in EXACTLY the marked row's gutter cell: {cells:?}"
    );
    assert_eq!(
        cells[0], "*",
        "the cell with the mark bg must be the gutter's glyph"
    );
}

#[test]
fn degradation_avoids_rgb_on_poor_terminal() {
    // At 16 colors there can be NO Rgb at all: everything goes indexed.
    let app = app_con_dir(ColorDepth::Ansi16);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let no_rgb = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .all(|cell| !matches!(cell.fg, Color::Rgb(..)) && !matches!(cell.bg, Color::Rgb(..)));
    assert!(no_rgb, "a 16-color terminal must not receive Rgb");
}

/// No VISIBLE glyph can be left with the TERMINAL's foreground when the
/// theme sets its own background: over a light theme with a dark terminal
/// (or the other way around) that paints text the color of the background —
/// invisible. That is how the Size/Date/Type cells and the column header
/// disappeared, painted with a bare `Modifier::DIM`: `dim` + the terminal's
/// `fg`.
///
/// Checked across EVERY shipped preset and with the text overlays open
/// (help, palette, settings, modal), which also painted with `Line::raw`.
#[test]
fn no_text_inherits_the_terminals_foreground_with_a_background_theme() {
    for name in norte_theme::preset_names() {
        let theme = Theme::preset(name)
            .expect("preset parses")
            .expect("preset exists");
        let dir = vp("file:///casa");
        let entries = vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"notas.txt".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(4096),
                mtime_ms: Some(1),
            },
        ];
        let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
        app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
        app.render_now_ms = Some(2);
        app.help = Some(norte_tui::app::HelpView::new(
            norte_i18n::Lang::En,
            vec![ratatui::text::Line::raw("  f1             ayuda")],
        ));
        // H3b: the body is laid out before painting (the run loop does
        // this); without this the overlay would only paint its sidebar and
        // the orphan-glyph sweep would not see the open theme's body.
        app.refresh_help(50, 16);

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let orphaned: Vec<String> = buffer
            .content
            .iter()
            .filter(|c| c.symbol() != " " && c.fg == Color::Reset)
            .map(|c| c.symbol().to_owned())
            .collect();
        assert!(
            orphaned.is_empty(),
            "{name}: {} glyphs with the terminal's foreground over the theme's background: {:?}",
            orphaned.len(),
            &orphaned[..orphaned.len().min(20)]
        );
    }
}

/// Per-preset TEXT contrast (second half of the theme review): no text
/// glyph can fall below 3:1 over its own background — the WCAG AA
/// threshold for UI components — which is exactly what the column bug
/// broke (terminal foreground over theme background: ~1:1 contrast,
/// invisible).
///
/// The bar is NOT body text's 4.5 on purpose: presets' bars and accents
/// (e.g. `gruvbox-light`'s status bar, cream on amber, 3.33:1) are the
/// theme's palette decisions, not accidents, and raising them would change
/// its look. Box-drawing glyphs are left out — an unfocused border is
/// decoration deliberately dimmed (`dim`) — ; what is painted with
/// `Modifier::DIM` (column cells, header) is measured by its UN-dimmed
/// color, which is all the buffer knows: the terminal applies the dimming,
/// and over a light background it darkens it (more contrast, not less).
#[test]
fn each_presets_text_reaches_the_contrast_floor() {
    fn luminance(c: (u8, u8, u8)) -> f64 {
        let canal = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * canal(c.0) + 0.7152 * canal(c.1) + 0.0722 * canal(c.2)
    }
    fn contraste(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (l1, l2) = (luminance(a), luminance(b));
        let (height, below) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (height + 0.05) / (below + 0.05)
    }
    fn rgb(c: Color) -> Option<(u8, u8, u8)> {
        match c {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }

    let dir = vp("file:///casa");
    for name in norte_theme::preset_names() {
        let entries = vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"notas.txt".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(4096),
                mtime_ms: Some(1),
            },
        ];
        let mut app = App::new(
            Pane::new(dir.clone(), entries),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.theme = TuiTheme::new(
            Theme::preset(name).expect("parses").expect("exists"),
            ColorDepth::Truecolor,
        );
        app.render_now_ms = Some(2);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        for cell in &terminal.backend().buffer().content {
            let glyph = cell.symbol();
            let decoration = glyph == " "
                || glyph
                    .chars()
                    .all(|c| matches!(c, '\u{2500}'..='\u{257f}' | '\u{2580}'..='\u{259f}'));
            if decoration {
                continue;
            }
            let (Some(fg), Some(bg)) = (rgb(cell.fg), rgb(cell.bg)) else {
                continue;
            };
            let r = contraste(fg, bg);
            assert!(
                r >= 3.0,
                "{name}: glyph {glyph:?} paints at {r:.2}:1 over its background (the floor is 3.0)"
            );
        }
    }
}
