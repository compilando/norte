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
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Names);

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
        con.contains("1 dirs") && con.contains("0 ficheros"),
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
    // With a space between the number and the label (spec 2026-09-15): in a
    // cell with room for it, `2 Copiar` reads at a glance and `2Copiar`
    // needs the eye to split it. The ZONES do not change — they come from
    // `keybar::layout`, which splits the row the same way.
    assert!(ultima.contains("2 Copiar"), "the bound cell: {ultima:?}");
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
