//! TUI mouse: hit test against a REAL layout (it is painted and resolved
//! against what was painted, not against a geometry invented here), wheel,
//! capture around the external opener, and `[ui] mouse = false`.
//!
//! The gestures themselves (what a sweep marks, when it is a transfer) have
//! their own tests in `norte-frontend::mouse`: this checks what belongs to
//! the TERMINAL — which cell is which row and who holds the capture.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Modal, Pane, TransferKind};
use norte_tui::mouse::{self, After};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Width and height of these tests' terminal. With 12 rows the layout comes
/// out EXACT and by hand: rows 0..=10 for the panes (the tasks panel
/// measures 0 with no tasks) and 11 for the status bar. Inside a pane: 0 top
/// border, 1 column header, 2..=9 the EIGHT listing rows, 10 bottom border.
const W: u16 = 60;
/// Height of these tests' terminal (see [`W`]).
const H: u16 = 12;

/// First listing row of a pane in this layout.
///
/// FOUR since there are two chrome rows pinned by default: row 0 the menu
/// bar (`[ui] menu_bar`), 1 the panel bar (`[ui] panel_bar`, #324), 2 the top
/// border and 3 the column header.
///
/// That changing this constant FIXES every test in this file is the proof
/// that click mapping followed the geometry alone — the row subtraction
/// happens in the frame's layout, not in the painter, and the mouse reads
/// that same layout. The panel bar proved it again: two constants and not
/// one click test touched.
const FILA0: u16 = 4;
/// How many listing rows fit. Two fewer: the two bars have eaten them.
const ROWS: u16 = 6;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// `n` entries `f0..f{n-1}` under `dir`.
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

/// An `App` with `n` entries in each pane, ALREADY painted once: the
/// mouse's geometry comes from the real frame (the same path as the run
/// loop, #124), never from a hand-written `PaneGeometry` — a test that made
/// up the geometry would pass just the same with a broken layout.
fn app_pintada(n: usize) -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, n)),
        Pane::new(dir.clone(), entries(&dir, n)),
    );
    let _ = paint(&mut app);
    app
}

/// Paints a frame, returns the geometry to the model (like the run loop)
/// and hands back THE PAINTED LINES.
///
/// Returning the buffer is not a convenience: without it these tests would
/// only check that `ui::pane_geometry` agrees with itself. If `draw_pane`
/// moved the listing by one row and the geometry stayed at `y + 2`,
/// everything would stay green and the mouse would mark the neighboring
/// file — which is exactly the bug the geometry exists to not have. The
/// tests that resolve an index CONTRAST it against that row's text.
fn paint(app: &mut App) -> Vec<String> {
    paint_at(app, W, H)
}

/// Like [`paint`] over a terminal of a different size: with a side panel
/// open, 60×12 is not enough to place it and the layout leaves it out.
fn paint_at(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    // The SAME order as the run loop: reconcile the window, paint, return
    // the geometry. Without the first step it would paint a window nobody
    // reconciled, i.e. a screen no user sees.
    ui::before_frame(app, ratatui::layout::Rect::new(0, 0, w, h));
    let frame = terminal.draw(|f| ui::draw(f, app)).expect("draw");
    let geometria = ui::pane_geometry(app, frame.area);
    mouse::after_frame(
        app,
        geometria,
        mouse::FrameZones {
            tabs: ui::tab_zones(app, frame.area),
            menus: ui::menu_zones(app, frame.area),
            panels: ui::panel_zones(app, frame.area),
            keys: ui::key_zones(app, frame.area),
            modal: ui::modal_zones(app, frame.area),
            places: ui::places_zones(app, frame.area),
            tree: ui::tree_zones(app, frame.area),
            extensions: ui::extension_zones(app, frame.area),
            help: ui::help_zones(app, frame.area),
            session: ui::session_zone(app, frame.area),
            status_items: ui::status_item_zones(app, frame.area),
            borders: ui::resize_borders(app, frame.area),
            slots: ui::panel_slots(app, frame.area),
        },
    );
    terminal
        .backend()
        .to_string()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

/// Checks that row `row` of the frame paints entry `index` of the LEFT
/// pane: the contrast between what the hit test resolves and what the user
/// has in front of them.
///
/// Compares against the entry's name, not against a literal: `Pane::new`
/// SORTS, so `entries[13]` is not "f13".
fn assert_row(lines: &[String], app: &App, row: u16, index: usize) {
    let entry = &app.panes[0].entries()[index];
    let name = String::from_utf8_lossy(
        entry
            .path
            .file_name()
            .expect("a test entry has a name")
            .as_bytes(),
    )
    .into_owned();
    let pintada = &lines[usize::from(row)];
    assert!(
        pintada.contains(&format!("{name} ")),
        "row {row} should paint `{name}` (index {index}) and paints: {pintada}"
    );
}

/// A mouse event with no modifiers.
fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    ev_con(kind, col, row, KeyModifiers::NONE)
}

/// A mouse event with modifiers.
fn ev_con(kind: MouseEventKind, col: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers,
    }
}

/// The border that OPENS the left pane's second column, on an already
/// painted terminal of `width` columns: `(cell, header row, id, width)`.
///
/// Comes from the SAME layout that paints the header, over the geometry's
/// interior width: a border calculated with different arithmetic would pass
/// with the layout broken.
fn edge_of_the_second_column(app: &App) -> (u16, u16, norte_frontend::columns::ColumnId, u16) {
    let g = &app.mouse.geometry().expect("there is geometry")[0];
    let (x0, interior, header) = (g.x + 1, g.width - 2, g.first_list_row - 1);
    let scheme = app.panes[0].dir().scheme();
    let cols = norte_frontend::columns::column_widths(
        &app.columns,
        scheme,
        interior,
        app.attr_catalog(scheme),
    );
    assert!(
        cols.len() >= 2,
        "there are more columns than the name: {cols:?}"
    );
    (x0 + cols[0].1, header, cols[1].0.clone(), cols[1].1)
}

/// The width the layout gives RIGHT NOW to column `id` of the left pane.
fn width_of(app: &App, id: &norte_frontend::columns::ColumnId) -> u16 {
    let g = &app.mouse.geometry().expect("there is geometry")[0];
    let scheme = app.panes[0].dir().scheme();
    norte_frontend::columns::column_widths(
        &app.columns,
        scheme,
        g.width - 2,
        app.attr_catalog(scheme),
    )
    .into_iter()
    .find(|(c, _)| c == id)
    .expect("the column is still there")
    .1
}

/// Dragging a column's border changes its width, LIVE, and releasing
/// requests saving it: the width persists on its own (the same as the
/// window already did).
#[test]
fn dragging_a_column_edge_changes_its_width_and_asks_to_save_it() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, 3)),
        Pane::new(dir.clone(), entries(&dir, 3)),
    );
    let _ = paint_at(&mut app, 120, H);
    let (edge, header, id, width) = edge_of_the_second_column(&app);

    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, edge, header)),
        After::Nothing
    );
    assert_eq!(
        mouse::handle(&mut app, ev(DRAG, edge - 3, header)),
        After::Nothing
    );
    assert_eq!(
        width_of(&app, &id),
        width + 3,
        "the border follows the pointer"
    );
    assert_eq!(
        mouse::handle(&mut app, ev(UP, edge - 3, header)),
        After::ColumnWidth
    );
    assert_eq!(
        app.mouse.take_column_width(),
        Some((id.to_string(), width + 3))
    );
    assert_eq!(
        app.mouse.take_column_width(),
        None,
        "it is consumed on read"
    );
}

/// A CLICK on the border, with no drag, writes nothing: grabbing the cell
/// right before the separator would change the width by one and save it
/// without the reader having moved anything.
#[test]
fn a_click_on_a_columns_border_saves_nothing() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, 3)),
        Pane::new(dir.clone(), entries(&dir, 3)),
    );
    let _ = paint_at(&mut app, 120, H);
    let (edge, header, id, width) = edge_of_the_second_column(&app);
    mouse::handle(&mut app, ev(DOWN, edge - 1, header));
    assert_eq!(
        mouse::handle(&mut app, ev(UP, edge - 1, header)),
        After::Nothing
    );
    assert_eq!(app.mouse.take_column_width(), None);
    assert_eq!(width_of(&app, &id), width);
}

/// Grabbing the cell BEFORE the separator does not jump the width on the
/// first move: it is measured against where it was grabbed, not against the
/// border.
#[test]
fn grabbing_before_the_separator_does_not_skip_a_cell() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entries(&dir, 3)),
        Pane::new(dir.clone(), entries(&dir, 3)),
    );
    let _ = paint_at(&mut app, 120, H);
    let (edge, header, id, width) = edge_of_the_second_column(&app);
    mouse::handle(&mut app, ev(DOWN, edge - 1, header));
    mouse::handle(&mut app, ev(DRAG, edge - 3, header));
    assert_eq!(
        width_of(&app, &id),
        width + 2,
        "two cells to the left, two more"
    );
}

/// Left button down.
const DOWN: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
/// Left button up.
const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
/// Drag with the left button held.
const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

/// A DETACHED window carries its indicator in the status bar, and clicking it
/// requests the explanation: the run loop opens help on the panels page. The
/// zone comes from the PAINTED frame, so it is checked against the line the
/// reader has in front of them and not against parallel arithmetic.
#[test]
fn pressing_the_session_indicator_opens_help() {
    let mut app = app_pintada(3);
    app.session.detached = true;
    let lines = paint(&mut app);
    // The test backend quotes each line: the first cell is byte 1, not 0.
    let bar = lines[usize::from(H - 1)].trim_start_matches('"');
    let badge = app.session_banner().expect("there is an indicator");
    assert!(bar.contains(&badge), "the bar paints it: {bar}");
    let x0 = u16::try_from(bar.find(&badge).expect("it is there")).expect("it fits");
    // The first character's column: on this line everything before it is
    // ASCII, so bytes and cells coincide.
    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, x0, H - 1)),
        After::SessionHelp,
        "clicking the indicator requests help"
    );
    // And to its left, no: the rest of the bar is not clickable.
    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, x0.saturating_sub(1), H - 1)),
        After::Nothing
    );
    // The page exists in the corpus, in both languages: the constant cannot
    // point at a deleted page without this turning red.
    for (lang, session) in [
        (norte_help::Lang::Es, "sesión"),
        (norte_help::Lang::En, "session"),
    ] {
        let page = norte_help::topic(lang, mouse::SESSION_HELP_TOPIC)
            .unwrap_or_else(|| panic!("page {} exists in {lang:?}", mouse::SESSION_HELP_TOPIC));
        // And it TALKS about the session: existing was not enough. When
        // `panes` was split, the constant kept pointing at a real page that
        // no longer covered it.
        let speaks = page.blocks.iter().any(|b| {
            matches!(b, norte_help::Block::Heading { text, .. } if text.to_lowercase().contains(session))
        });
        assert!(
            speaks,
            "{lang:?}: page {} has no section about the {session}",
            mouse::SESSION_HELP_TOPIC
        );
    }
    // And opening it opens THAT page, as the root: `Esc` closes.
    norte_tui::overlays::open_help_topic(
        &mut app,
        norte_help::Lang::Es,
        &[],
        mouse::SESSION_HELP_TOPIC,
    );
    let help = app.help.as_ref().expect("help opened");
    assert_eq!(help.state.current().as_str(), mouse::SESSION_HELP_TOPIC);

    // The owner has no indicator, and the same cell does nothing.
    app.help = None;
    app.session.detached = false;
    let _ = paint(&mut app);
    assert_eq!(mouse::handle(&mut app, ev(DOWN, x0, H - 1)), After::Nothing);
}

#[test]
fn the_layout_of_these_tests_is_the_one_that_gets_painted() {
    // Anchor for the numbers above: if the pane gains or loses chrome, THIS
    // test fails with a clear message, and not the next six with confusing
    // arithmetic.
    let mut app = app_pintada(5);
    let lines = paint(&mut app);
    let geom = app.mouse.geometry().expect("there is geometry");
    let (left, right) = (geom[0], geom[1]);
    // `y = 2` and two fewer rows of height: the two pinned bars keep rows 0
    // and 1. That the GEOMETRY says so — and not just the painter — is the
    // point: the mouse reads these rectangles, so a click keeps landing
    // where the reader gave it.
    assert_eq!((left.x, left.y, left.width, left.height), (0, 2, 30, 9));
    assert_eq!(
        (right.x, right.y, right.width, right.height),
        (30, 2, 30, 9)
    );
    assert_eq!(left.first_list_row, FILA0, "top border + header");
    assert_eq!(left.list_rows, ROWS, "interior minus the header");
    assert_eq!(left.offset, 0, "cursor on the first: no scroll");
    // And what is really PAINTED on those rows. The sort indicator (`▲`)
    // instead of the column's label: the label is translated and these
    // tests do not fix a language.
    let header = usize::from(FILA0) - 1;
    assert!(
        lines[header].contains('▲'),
        "row {header} = column header: {}",
        lines[header]
    );
    // And row 0 is the menu bar, which is what pushed the header down there.
    // Checked by its SHAPE and not by a label: these tests do not fix a
    // language, and "Archivo" only shows up in one of the two.
    assert!(
        !lines[0].trim().is_empty() && !lines[0].contains('│'),
        "row 0 = menu bar (text, no panel borders): {}",
        lines[0]
    );
    assert_row(&lines, &app, FILA0, 0);
}

#[test]
fn a_click_on_the_first_row_resolves_the_first_entry() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0).expect("inside the left pane");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, Some(0));
}

#[test]
fn a_click_on_the_last_entry_resolves_that_one_and_no_other() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0 + 4).expect("inside the pane");
    assert_eq!(hit.index, Some(4), "fifth painted row = fifth entry");
}

/// The column header is CHROME: it resolves to the pane, never to a row.
/// Without this, sorting by a column with the mouse (which is what the user
/// is going to try there) would also move the cursor to the first entry.
#[test]
fn the_column_header_is_not_any_row() {
    let app = app_pintada(5);
    // Relative to `FILA0` and not a bare number: the header is the row right
    // above the listing's first one, and tying it to the constant makes
    // moving the chrome move this test with it instead of breaking it.
    let hit = mouse::hit_test(&app, 5, FILA0 - 1).expect("still the pane");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, None);
}

/// The top border (where the title with the path goes) and the bottom one
/// (where the quick search input paints) are not rows either.
#[test]
fn the_panes_borders_are_not_rows() {
    let app = app_pintada(5);
    // Row 0 is no longer the pane's border: it is the MENU BAR, and belongs
    // to no panel — same as the status bar below. A click there cannot
    // resolve to an entry or to a pane.
    assert!(
        mouse::hit_test(&app, 5, 0).is_none(),
        "row 0 is the menu bar, not a pane"
    );
    for row in [FILA0 - 2, FILA0 + ROWS] {
        let hit = mouse::hit_test(&app, 5, row).expect("inside the block");
        assert_eq!(hit.index, None, "row {row} is a border");
    }
    for col in [0, 29] {
        let hit = mouse::hit_test(&app, col, FILA0).expect("inside the block");
        assert_eq!(hit.index, None, "column {col} is a side border");
    }
}

/// A click on the pinned menu bar OPENS it.
///
/// It is what makes the bar usable: it used to only handle menu clicks if it
/// was ALREADY open, so with the bar pinned and closed clicking "Archivo"
/// did nothing — a bar that exists so you can find the menu and on which the
/// click is inert.
#[test]
fn a_click_on_the_menu_bar_opens_it() {
    let mut app = app_pintada(5);
    assert!(app.menu.is_none(), "starts closed");
    let _ = mouse::handle(&mut app, ev(DOWN, 2, 0));
    assert!(
        app.menu.is_some(),
        "clicking the first title opens the menu"
    );
}

/// The menu REOPENS where it was.
///
/// Always opening on the first one forces walking the whole bar on every
/// gesture, and whoever uses two entries of the same menu pays for it every
/// time. Closing goes through a single door (`App::close_menu`) precisely so
/// the five places that close it all point at the same thing.
#[test]
fn the_menu_reopens_where_it_left_off() {
    let mut app = app_pintada(5);
    let mut m = norte_frontend::menu::MenuState::new();
    m.open(3);
    app.menu = Some(m);
    app.close_menu();
    assert!(app.menu.is_none(), "closed");

    app.menu = Some(norte_frontend::menu::MenuState::reopen_at(app.menu_last));
    assert_eq!(
        app.menu.as_ref().map(norte_frontend::menu::MenuState::menu),
        Some(3),
        "returns to the one that was open, not the first"
    );
    assert_eq!(
        app.menu.as_ref().map(norte_frontend::menu::MenuState::item),
        Some(0),
        "and the cursor does return to the start: the list is short and reads whole"
    );
}

/// And with BOTH bars off, row 0 goes back to belonging to the panel: there
/// is no bar to click, so the click cannot open anything.
///
/// Both: with only the menu one off, the panel one moves to row 0 and this
/// click landed on a button. The test stayed green — `menu.is_none()` still
/// held — while its stated invariant was already false.
#[test]
fn without_pinned_bars_a_click_above_opens_nothing() {
    let mut app = app_pintada(5);
    app.menu_bar = false;
    app.panel_bar = false;
    let _ = paint(&mut app);
    let _ = mouse::handle(&mut app, ev(DOWN, 2, 0));
    assert!(app.menu.is_none());
    assert!(app.pending_panel_command.is_none());
}

/// Without the menu one but WITH the panel one, row 0 belongs to the panel
/// bar: it moves up and stays clickable.
#[test]
fn without_a_menu_bar_the_panes_bar_moves_to_row_zero() {
    let mut app = app_pintada(5);
    app.menu_bar = false;
    let _ = paint(&mut app);
    let _ = mouse::handle(&mut app, ev(DOWN, 1, 0));
    assert_eq!(
        app.pending_panel_command.as_deref(),
        Some("layout.places"),
        "the bar's first button"
    );
    assert!(
        app.menu.is_none(),
        "and it does not open the menu, which is not there"
    );
}

/// The buttons land where the zones say, and only there.
///
/// Three cells per button from column 0: the first is `layout.places` and
/// the sixth `layout.log`. Both EXTREMES are checked and the cell right
/// after the last one, which is where a miscalculated `x1` shows.
#[test]
fn every_bar_button_falls_into_its_place() {
    let mut app = app_pintada(5);
    // This test's columns are the LETTER ones (three cells per button); with
    // names, the zones follow what is painted and `theme_render` checks it.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let _ = paint(&mut app);
    let press = |app: &mut norte_tui::app::App, col: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(DOWN, col, 1));
        app.pending_panel_command.clone()
    };
    assert_eq!(press(&mut app, 1).as_deref(), Some("layout.places"));
    assert_eq!(press(&mut app, 16).as_deref(), Some("layout.log"));
    assert_eq!(
        press(&mut app, 19).as_deref(),
        Some("layout.disk-map"),
        "the disk map (phase 4) landed after the log"
    );
    assert_eq!(
        press(&mut app, 22).as_deref(),
        Some("layout.timeline"),
        "and the timeline (phase 7) after the map, by registration order"
    );
    assert_eq!(
        press(&mut app, 25).as_deref(),
        Some("layout.terminal"),
        "and the terminal (#362) after the timeline, by registration order"
    );
    assert_eq!(
        press(&mut app, 27),
        None,
        "past the last one there is no button"
    );
}

/// With `[ui] panel_bar_position = "left"` the bar is a three-cell-wide
/// COLUMN on the left edge (spec 2026-09-21): one button per row, and the
/// listing shifts up one row and moves over three columns.
///
/// Painted, clicked and layout come from the same geometry; if the mouse
/// still thought the bar was a row, clicking row 1 would open places with
/// the listing underneath.
#[test]
fn in_column_mode_each_button_is_a_row_on_the_left_edge() {
    let mut app = app_pintada(5);
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Left);
    let lines = paint(&mut app);
    let press = |app: &mut norte_tui::app::App, row: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(DOWN, 1, row));
        app.pending_panel_command.clone()
    };
    assert_eq!(press(&mut app, 1).as_deref(), Some("layout.places"));
    assert_eq!(press(&mut app, 6).as_deref(), Some("layout.log"));
    // What is painted says the same: one ICON per row (ADR 0140), on column
    // 1 (character 2: `TestBackend` puts the line in quotes).
    let cell = |lines: &[String], f: usize| {
        lines[f]
            .chars()
            .nth(2)
            .expect("there is column 1")
            .to_string()
    };
    let icon = |k| {
        norte_frontend::panelbar::icon(k, norte_frontend::panelbar::IconSet::Unicode)
            .expect("icon")
            .to_owned()
    };
    assert_eq!(cell(&lines, 1), icon("places"), "{:?}", lines[1]);
    assert_eq!(cell(&lines, 6), icon("log"), "{:?}", lines[6]);
    // And the listing moved with it: its first row is now 3, starting at
    // column 3; the rail's column belongs to no pane.
    assert!(mouse::hit_test(&app, 5, FILA0 - 1).is_some());
    assert!(mouse::hit_test(&app, 1, FILA0 - 1).is_none());

    // With `letters`, the usual letters.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let lines = paint(&mut app);
    assert!(
        cell(&lines, 1).chars().all(char::is_alphabetic),
        "{:?}",
        lines[1]
    );

    // With room to spare, AIR between icons, like VS Code: the first one a
    // row lower and a blank one between two. The mouse measures the same.
    app.chrome.panel_bar_style = None;
    let lines = paint_at(&mut app, 80, 40);
    assert_eq!(cell(&lines, 2), icon("places"), "{:?}", lines[2]);
    assert_eq!(cell(&lines, 3), " ", "air row: {:?}", lines[3]);
    assert_eq!(cell(&lines, 4), icon("viewer"), "{:?}", lines[4]);
    assert_eq!(press(&mut app, 2).as_deref(), Some("layout.places"));
    assert_eq!(press(&mut app, 3), None, "air is not a button");
    assert_eq!(press(&mut app, 4).as_deref(), Some("layout.preview"));
}

/// The layout buttons (ADR 0133) go on the menu bar's right edge and run
/// their order; on a terminal that does not let them fit whole next to the
/// titles, there is none to click.
#[test]
fn layout_buttons_fall_on_the_right_edge() {
    let mut app = app_pintada(5);
    let lines = paint_at(&mut app, 120, H);
    // `[#]` is the last one: its three cells are the row 0's last three.
    assert!(
        lines[0].trim_end_matches('"').ends_with("[#]"),
        "{:?}",
        lines[0]
    );
    let press = |app: &mut norte_tui::app::App, col: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(DOWN, col, 0));
        app.pending_panel_command.clone()
    };
    // All five at 120 columns: `[|] [-] [=] [/] [#]` starting at 101.
    assert_eq!(press(&mut app, 119).as_deref(), Some("layout.pick"));
    assert_eq!(press(&mut app, 101).as_deref(), Some("layout.split-h"));
    assert_eq!(press(&mut app, 114).as_deref(), Some("layout.flip"));
    assert_eq!(press(&mut app, 104), None, "the gap between two buttons");
    // With an overlay in front (the review caught it): they neither paint
    // nor click. Painted and dead was the class of BLOCKER the panel bar
    // already had.
    app.open_theme_picker();
    let lines = paint_at(&mut app, 120, H);
    assert!(!lines[0].contains("[#]"), "{:?}", lines[0]);
    assert_eq!(press(&mut app, 119), None);
    app.theme_picker = None;
    // Narrowing, the first to yield is flip, and the usual four stay there
    // (ADR 0138). The exact width depends on the titles' language, so it is
    // searched for.
    let cede = (60..120)
        .rev()
        .find(|w| !paint_at(&mut app, *w, H)[0].contains("[/]"))
        .expect("it yields at some width");
    let lines = paint_at(&mut app, cede, H);
    assert!(lines[0].contains("[|] [-] [=] [#]"), "{:?}", lines[0]);
    // At sixty columns the titles keep the spot.
    let lines = paint(&mut app);
    assert!(!lines[0].contains("[#]"), "{:?}", lines[0]);
}

/// Grabs `from`'s title and drops it on the bottom half of `over`.
fn release_below(
    app: &mut App,
    from: norte_frontend::layout::Rect,
    over: norte_frontend::layout::Rect,
) {
    let (x, y) = (over.x + over.width / 2, over.y + over.height - 2);
    let _ = mouse::handle(app, ev(DOWN, from.x + 4, from.y));
    let _ = mouse::handle(app, ev(DRAG, x, y));
    let _ = mouse::handle(app, ev(UP, x, y));
}

/// ADR 0138: where stacking two listings would hide one, dropping does
/// nothing and says so: the panel cannot disappear by being moved.
#[test]
fn moving_where_it_does_not_fit_is_refused_and_reported() {
    let mut app = app_pintada(5);
    let _ = paint_at(&mut app, 120, H);
    let a = app.mouse.slot_rect(app.panes.slot_of(0)).expect("placed");
    let b = app.mouse.slot_rect(app.panes.slot_of(1)).expect("placed");
    let before = app.layout.clone();
    app.message = None;
    release_below(&mut app, a, b);
    assert_eq!(app.layout, before, "at {H} rows they do not fit stacked");
    assert!(app.message.is_some(), "and it says so");
}

/// ADR 0138: dragging a listing by its title row and dropping it on the
/// other one's bottom half stacks them; clicking without dragging just
/// focuses.
#[test]
fn dragging_the_title_moves_the_pane() {
    let mut app = app_pintada(5);
    // Room to spare: at `H` rows two stacked listings do not fit.
    let _ = paint_at(&mut app, 120, 50);
    let left = app.panes.slot_of(0);
    let right = app.panes.slot_of(1);
    let rect = |app: &norte_tui::app::App, s| {
        app.mouse
            .slot_rect(s)
            .unwrap_or_else(|| panic!("{s:?} not placed: {:?}", app.layout))
    };
    let (a, b) = (rect(&app, left), rect(&app, right));
    assert_eq!(a.y, b.y, "side by side at the start");
    let before = app.layout.clone();

    // A click on the title moves nothing.
    let _ = mouse::handle(&mut app, ev(DOWN, a.x + 4, a.y));
    let _ = mouse::handle(&mut app, ev(UP, a.x + 4, a.y));
    assert_eq!(app.layout, before, "a click is not a drag");

    // Grab, drag to the other one's bottom half: it highlights.
    let destination_and = b.y + b.height - 2;
    let dest_x = b.x + b.width / 2;
    let _ = mouse::handle(&mut app, ev(DOWN, a.x + 4, a.y));
    let _ = mouse::handle(&mut app, ev(DRAG, dest_x, destination_and));
    assert!(
        app.mouse.move_target().is_some(),
        "the destination highlights"
    );
    let _ = mouse::handle(&mut app, ev(UP, dest_x, destination_and));
    assert!(app.mouse.move_target().is_none());
    let focus = app.focused_slot();
    let _ = paint_at(&mut app, 120, 50);
    let (a, b) = (rect(&app, left), rect(&app, right));
    assert!(a.y > b.y && a.x == b.x, "stacked: {a:?} under {b:?}");
    // Focus stays on its SLOT, even though its position changed.
    assert_eq!(app.focused_slot(), focus);
}

/// ADR 0134: two panels on the same edge share a spot as tabs, and their
/// slot's first row is the STRIP with both names. Clicking the hidden one
/// runs its command (which reveals it); the one in front is not a zone.
#[test]
fn panes_on_one_edge_group_with_their_strip() {
    let mut app = app_pintada(5);
    // Both go to the RIGHT.
    app.toggle_preview();
    app.toggle_metadata();
    let (slots, active) = app
        .layout
        .tabs_of(app.metadata_slot().expect("details open"))
        .expect("in a group");
    assert_eq!(slots.len(), 2, "the viewer and the details together");
    assert_eq!(active, 1, "the one that arrives, in front");
    let lines = paint_at(&mut app, 120, 20);
    // The panel bar's names, in the suite's language.
    let lang = norte_i18n::active();
    let visor = norte_frontend::panelbar::label_in(lang, "viewer", "layout.preview");
    let detalles = norte_frontend::panelbar::label_in(lang, "metadata", "layout.metadata");
    // From the body: rows 0 and 1 are the menu and the panel bar, and the
    // strip also says "Visor" and "Detalles".
    let row = lines
        .iter()
        .enumerate()
        .skip(2)
        .find(|(_, l)| l.contains(&visor) && l.contains(&detalles))
        .map_or_else(|| panic!("a row with both tabs: {lines:#?}"), |(i, _)| i);
    // The column of some text on the row: `TestBackend` puts the line in
    // quotes, so character 0 is the quote mark.
    let row_text: String = lines[row].chars().skip(1).collect();
    let col = |text: &str| {
        let byte = row_text.find(text).expect("it is there");
        u16::try_from(row_text[..byte].chars().count()).expect("it fits")
    };
    let row = u16::try_from(row).expect("it fits");
    // The hidden one (Visor) is clickable; the one in front is not.
    app.pending_panel_command = None;
    let _ = mouse::handle(&mut app, ev(DOWN, col(&visor), row));
    assert_eq!(app.pending_panel_command.as_deref(), Some("layout.preview"));
    app.pending_panel_command = None;
    let _ = mouse::handle(&mut app, ev(DOWN, col(&detalles), row));
    assert!(
        app.pending_panel_command.is_none(),
        "the one in front does not close with a click on its tab: {:?}",
        app.pending_panel_command
    );
    // And the mouse measures the content where it paints: below the strip.
    // With two counts, a click on the grouped disk map used to pick the
    // sibling next to it.
    let metadata = app.metadata_slot().expect("details open");
    let slot = app.mouse.slot_rect(metadata).expect("placed");
    assert_eq!(slot.y, row + 1, "the content starts below the strip");
}

/// REGRESSION of a BLOCKER: with an overlay in front, the bar neither
/// paints nor can be clicked.
///
/// The bar paints BEFORE the overlays, so its zones stayed active
/// underneath: with help open, a click on help's title bar — which occupies
/// the same row — landed on a button and opened or closed a panel the
/// reader was not looking at. Painted and clickable have to be the same
/// thing.
#[test]
fn with_an_overlay_in_front_the_bar_is_not_clickable() {
    let mut app = app_pintada(5);
    let _ = paint(&mut app);
    // Any overlay from those that cover the row.
    app.open_theme_picker();
    let _ = paint(&mut app);
    let before = app.layout.clone();
    let _ = mouse::handle(&mut app, ev(DOWN, 1, 1));
    assert!(
        app.pending_panel_command.is_none(),
        "a click on the overlay touched a bar button"
    );
    assert_eq!(before, app.layout, "and the layout changed underneath");
}

/// The status bar belongs to no pane: outside the whole hit test, not "the
/// last row of the pane below".
#[test]
fn the_status_bar_does_not_belong_to_any_pane() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, 5, H - 1).is_none());
}

/// The gap BELOW the last entry of a short listing is not the last entry.
/// It is the case that most surprises if resolved wrong: the natural bug
/// (saturating the index) makes a click in the empty space mark — or move
/// the cursor to — the directory's last file, which is exactly the one
/// nobody was looking at when they clicked there.
#[test]
fn the_slot_below_the_last_entry_is_not_the_last_entry() {
    let mut app = app_pintada(3);
    for row in FILA0 + 3..FILA0 + ROWS {
        let hit = mouse::hit_test(&app, 5, row).expect("still inside the pane");
        assert_eq!(hit.index, None, "row {row}: empty, not an entry");
    }
    // And the real click does not move the cursor either.
    app.panes[0].set_cursor(1);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 6));
    assert_eq!(app.panes[0].cursor(), 1, "the cursor stays where it was");
}

#[test]
fn a_click_outside_both_panes_resolves_nothing() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, W - 1, H - 1).is_none(), "corner");
    assert!(
        mouse::hit_test(&app, 5, H + 5).is_none(),
        "outside the frame"
    );
}

#[test]
fn a_click_focuses_that_pane_and_moves_the_cursor() {
    let mut app = app_pintada(5);
    assert_eq!(app.focus(), 0);
    let after = mouse::handle(&mut app, ev(DOWN, 35, FILA0 + 2));
    assert_eq!(after, After::Nothing);
    assert_eq!(app.focus(), 1, "the click focuses the clicked pane");
    assert_eq!(app.panes[1].cursor(), 2);
    assert_eq!(app.panes[1].marks_len(), 0, "a bare click does not mark");
}

/// The scroll comes from the cursor (see `ui::list_offset`), so a click on
/// a row of a listing ALREADY scrolled has to add the offset. It is where a
/// naive hit test (painted row = index) silently gets it wrong, and it gets
/// it more wrong the further down the user is.
#[test]
fn a_click_on_a_scrolled_listing_adds_in_the_scroll() {
    let mut app = app_pintada(40);
    app.panes[0].set_cursor(20);
    let lines = paint(&mut app);
    let offset = app.mouse.geometry().expect("geometry")[0].offset;
    assert_eq!(
        offset,
        21 - usize::from(ROWS),
        "the cursor goes to the edge"
    );
    let hit = mouse::hit_test(&app, 5, FILA0).expect("inside the pane");
    assert_eq!(hit.index, Some(offset), "the first PAINTED row");
    // Against the buffer: the row that resolves is the one that shows.
    assert_row(&lines, &app, FILA0, offset);
    let hit = mouse::hit_test(&app, 5, FILA0 + ROWS - 1).expect("inside the pane");
    assert_eq!(hit.index, Some(20), "the last painted one is the cursor");
    assert_row(&lines, &app, FILA0 + ROWS - 1, 20);
}

/// The wheel scrolls the listing UNDER THE POINTER and does not touch
/// focus. Looking at one panel while working in the other is the normal
/// gesture with two panels; stealing focus from the active panel just by
/// passing the mouse over it would be a change of the next operation's
/// destination made without clicking anything.
#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer_and_not_the_focused_one() {
    let mut app = app_pintada(40);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollDown, 35, FILA0 + 1));
    assert_eq!(app.focus(), 0, "focus does NOT move with the wheel");
    assert_eq!(app.panes[1].cursor(), 3, "the right pane scrolled");
    assert_eq!(app.panes[0].cursor(), 0, "the focused one, untouched");

    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 35, FILA0 + 1));
    assert_eq!(app.panes[1].cursor(), 0, "and it comes back");
}

/// **The wheel over the VIEWER scrolls it.**
///
/// The viewer is an overlay, and the overlays' cutoff was eating the event:
/// with a file open, scrolling did absolutely nothing. It is the most
/// obvious gesture a viewer has, and the only thing underneath was a
/// listing you cannot see — scrolling THAT would have been worse.
#[test]
fn the_wheel_over_the_viewer_scrolls_it_and_not_the_listing() {
    let mut app = app_pintada(40);
    let text: Vec<u8> = (0..80)
        .flat_map(|i| format!("linea {i}\n").into_bytes())
        .collect();
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///casa/alto.txt"),
        text,
        false,
    ));

    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollDown, 35, FILA0 + 1));
    let bajado = app.viewer.as_ref().expect("viewer open").scroll;
    assert!(bajado > 0, "the viewer scrolled down");
    assert_eq!(
        app.panes[1].cursor(),
        0,
        "and the listing underneath, untouched"
    );

    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 35, FILA0 + 1));
    assert_eq!(
        app.viewer.as_ref().expect("viewer open").scroll,
        0,
        "and it comes back"
    );
}

/// Double click = `nav.enter`. Checks AGREEMENT with the run loop (returns
/// [`After::Enter`], which there dispatches the same keyboard command), not
/// the navigation itself: entering a directory needs a backend and already
/// has its own tests.
#[test]
fn the_double_click_requests_the_same_nav_enter_as_the_keyboard() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(DOWN, 5, FILA0 + 1), t0),
        After::Nothing,
        "the first one is a normal click"
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 1),
            t0 + std::time::Duration::from_millis(120)
        ),
        After::Enter
    );
    assert_eq!(app.panes[0].cursor(), 1);
}

/// **A double click on a FILE leaves something to launch.**
///
/// `nav.enter` on a local file does not navigate: it resolves the desktop
/// program and leaves it armed in `pending_open` for the terminal's owner to
/// launch. The mouse arm ran the command and did not finish that part, so a
/// double click on a `.jpg` did absolutely nothing and did not say why.
///
/// What is checked here is the CONTRACT that finish depends on: that
/// `nav.enter` on a file arms the launch. The wire itself — `despachar_click`
/// launching what is armed — has no test because `on_mouse` needs a real
/// terminal; the fix is that all three mouse arms go through the SAME
/// function, which is what stops it from being forgotten again.
#[test]
fn nav_enter_on_a_file_leaves_an_opener_armed() {
    use norte_tui::gestures::{EnterAction, enter_action, resolve_opener};

    let mut app = app_pintada(5);
    // The listing is local files; the cursor starts on the first one.
    app.set_focus(0);
    app.panes[0].set_cursor(0);
    assert!(
        matches!(enter_action(&app), EnterAction::OpenExternal),
        "on a local file, entering is OPENING: {:?}",
        enter_action(&app)
    );

    assert!(app.pending_open.is_none());
    resolve_opener(&mut app);
    assert!(
        app.pending_open.is_some(),
        "and resolving it leaves the program armed for the terminal's owner"
    );
}

/// Two SLOW clicks on the same row are two clicks. And two fast ones on
/// different rows, too: otherwise, scrolling down the listing click by
/// click would enter a directory every other row.
#[test]
fn two_clicks_far_apart_in_time_or_row_are_not_a_double() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(&mut app, ev(DOWN, 5, FILA0 + 1), t0);
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 1),
            t0 + std::time::Duration::from_secs(3)
        ),
        After::Nothing,
        "three seconds later is not a double click"
    );

    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(&mut app, ev(DOWN, 5, FILA0 + 1), t0);
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 2),
            t0 + std::time::Duration::from_millis(50)
        ),
        After::Nothing,
        "neither is another row"
    );
}

/// A ctrl+click is NOT the first half of a double click. Marking a row and
/// clicking it again right away is exactly what you do to drag it: if it
/// counted, the gesture would enter the directory instead of starting the
/// drag.
#[test]
fn a_click_with_a_modifier_is_not_the_first_half_of_a_double() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(
        &mut app,
        ev_con(DOWN, 5, FILA0 + 1, KeyModifiers::CONTROL),
        t0,
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 1),
            t0 + std::time::Duration::from_millis(50)
        ),
        After::Nothing
    );
}

#[test]
fn ctrl_click_marks_and_unmarks_the_clicked_row() {
    let mut app = app_pintada(5);
    let ctrl = KeyModifiers::CONTROL;
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 3, ctrl));
    assert_eq!(app.panes[0].marks_len(), 1);
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 3, ctrl));
    assert_eq!(app.panes[0].marks_len(), 0, "the same gesture unmarks");
}

#[test]
fn a_drag_marks_what_it_sweeps() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    assert_eq!(app.panes[0].marks_len(), 0, "the press does not mark yet");
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 4));
    let _ = mouse::handle(&mut app, ev(UP, 5, FILA0 + 4));
    assert_eq!(app.panes[0].marks_len(), 4, "rows 1..=4");
}

/// Dropping a selection on the other pane opens EXACTLY the modal the copy
/// key would open on that same selection.
///
/// It is the assertion that everything else rests on: a drop is a mutation,
/// and if it does not go through the same door as F5 it ends up without the
/// confirmation, without the collision dialog, without the journal entry,
/// without undo, or without the policy gate — and not all at once, but on
/// the day one of the two paths changes. The MODALS are compared, not a
/// description of them: they are what gets put to the test.
#[test]
fn a_drop_opens_the_same_modal_as_the_copy_key() {
    let mut app = app_pintada(10);
    // Two marks by hand (with two, the gate opens the list confirm).
    for row in [1, 2] {
        let _ = mouse::handle(
            &mut app,
            ev_con(DOWN, 5, FILA0 + row, KeyModifiers::CONTROL),
        );
    }
    assert_eq!(app.panes[0].marks_len(), 2);

    // What the KEYBOARD produces with this same selection.
    app.open_transfer(TransferKind::Copy, 0, 1, None);
    let by_keyboard = app.modal.take().expect("F5 opens a modal");

    // And now the mouse: press ON a marked row and drop on the other one.
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(UP, 35, FILA0 + 1));

    assert_eq!(
        app.modal.as_ref(),
        Some(&by_keyboard),
        "the drop produces the same as the key, or there are two mutation paths"
    );
    assert_eq!(
        app.panes[0].marks_len(),
        2,
        "it did not sweep: it was a transfer"
    );
    assert_eq!(app.panes[1].marks_len(), 0, "the destination, untouched");
}

/// The copy/move flag is read ON RELEASE: the SAME drag ends in a copy
/// modal or a move one depending on whether Shift is held when the button
/// comes up. It is what lets you change your mind mid-gesture without
/// moving (a destructive mutation at the source) what you meant to copy.
#[test]
fn shift_on_drop_decides_copy_or_move() {
    let drags = |mods: KeyModifiers| {
        let mut app = app_pintada(10);
        let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 2, KeyModifiers::CONTROL));
        let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 3, KeyModifiers::CONTROL));
        // The press goes with NO Shift in both cases: only the release changes.
        let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev_con(UP, 35, FILA0 + 2, mods));
        match app.modal {
            Some(Modal::ConfirmTransfer { kind, .. }) => kind,
            other => panic!("expected a transfer confirm: {other:?}"),
        }
    };
    assert_eq!(drags(KeyModifiers::NONE), TransferKind::Copy);
    assert_eq!(drags(KeyModifiers::SHIFT), TransferKind::Move);
}

/// A drag born on an UNMARKED row that crosses to the other pane is
/// promoted to a transfer of THAT row — the most common drag in any file
/// manager — and returns the marks it swept along the way. What travels is
/// the pressed row, not the eleven marks the pane might have.
#[test]
fn a_promoted_drag_carries_its_row_and_returns_what_it_swept() {
    let mut app = app_pintada(10);
    // A previous mark, unrelated to the gesture.
    let _ = mouse::handle(
        &mut app,
        ev_con(DOWN, 5, FILA0 + ROWS - 1, KeyModifiers::CONTROL),
    );
    assert_eq!(app.panes[0].marks_len(), 1);

    // Press on an UNMARKED row, sweeps along the way, and crosses to the
    // other pane.
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 3));
    assert_eq!(app.panes[0].marks_len(), 4, "swept 1..=3 along the way");
    let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 1));
    assert_eq!(
        app.panes[0].marks_len(),
        1,
        "crossing returns what was swept: only the previous mark is left"
    );
    let _ = mouse::handle(&mut app, ev(UP, 35, FILA0 + 1));

    let expected = app.panes[0].entries()[1].path.clone();
    let Some(Modal::TransferName {
        from, from_marks, ..
    }) = &app.modal
    else {
        panic!("a single item: editable name, like F5 with one entry");
    };
    assert_eq!(from, &expected, "the pressed row, not the unrelated mark");
    assert!(!from_marks, "the send cannot consume an unrelated mark");
    assert_eq!(app.panes[0].marks_len(), 1, "and it stays intact");
}

/// Dropping on the SOURCE pane produces nothing: it is an explicit no-op,
/// not a directory copying onto itself, and it is what whoever changed
/// their mind mid-drag and went back home asks for.
#[test]
fn dropping_on_the_source_pane_submits_nothing() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 2)); // wanders…
    let _ = mouse::handle(&mut app, ev(UP, 5, FILA0 + 6)); // …and comes back
    assert!(app.modal.is_none(), "neither modal nor transfer");
    assert_eq!(app.panes[0].marks_len(), 1, "the mark is still there");
}

/// A CANCELLED drag leaves the selection exactly as it was. It is half the
/// contract that makes it acceptable for the gesture to mean two things
/// depending on where it ends: aborting it has to return the earlier state.
///
/// It is cancelled by dropping on the CHROME (the status bar), which is
/// where the gesture of someone who changed their mind ends: dropping
/// outside every row does not guess a destination.
#[test]
fn a_canceled_drag_restores_the_marks() {
    let mut app = app_pintada(10);
    for row in [5, 6] {
        let _ = mouse::handle(
            &mut app,
            ev_con(DOWN, 5, FILA0 + row, KeyModifiers::CONTROL),
        );
    }
    let marked = |app: &App| -> Vec<bool> {
        app.panes[0]
            .entries()
            .iter()
            .map(|e| app.panes[0].is_marked(e))
            .collect()
    };
    let before = marked(&app);

    // Press on an unmarked row, sweeps, crosses (promotes) and drops on the
    // status bar, which belongs to no pane.
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(UP, 5, H - 1));

    assert!(app.modal.is_none(), "cancelling produces nothing");
    assert_eq!(marked(&app), before, "the marks, exactly as they were");
}

/// The bar's notice comes from `Drag::pending`, the SAME source the release
/// reads, so it cannot promise one thing and have the drop do another: the
/// pending text is checked against the modal that dropping right there
/// opens.
///
/// And nothing is announced while the gesture is at home (dropping there is
/// a no-op: promising a copy that will not happen is worse than promising
/// nothing) nor when the gesture is a sweep.
#[test]
fn the_bar_announces_what_dropping_now_would_do() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 3, KeyModifiers::CONTROL));

    // Sweep at home: nothing to announce.
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 8));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 9));
    assert_eq!(mouse::drop_hint(&app), None, "marking promises nothing");

    // Transfer still over its own pane: neither.
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 5));
    assert_eq!(mouse::drop_hint(&app), None, "at home, dropping is a no-op");

    // Over the other pane: it says how many and that it COPIES…
    let _ = mouse::handle(&mut app, ev(DRAG, 35, FILA0 + 1));
    let copy = mouse::drop_hint(&app).expect("there is a pending drop");
    assert!(copy.contains('2'), "the two marks: {copy}");
    // The destination with the SAME sanitizing as the pane's header (rule 1).
    let (dest, _) = norte_frontend::path_display_with(app.panes[1].dir(), None);
    assert_eq!(
        copy,
        norte_i18n::ta("drag-copy", &[("n", "2"), ("to", &dest)]),
    );
    // …and the bar PAINTS it (over any pending message).
    app.message = Some("un mensaje cualquiera".to_owned());
    let cabeza = copy
        .split_once("  ")
        .map_or(copy.as_str(), |(head, _)| head)
        .to_owned();
    let lines = paint(&mut app);
    let bar = lines.last().expect("status bar");
    // The bar YIELDS on the right: its ELEMENTS — position, marks, encoding
    // (ADR 0132) — keep their part and the notice cuts wherever it must.
    // Requiring the WHOLE head tied this test to the language without saying
    // so: the Spanish sentence measures about forty cells and the English
    // one about thirty, so the same sixty-wide screen passed in English and
    // failed in Spanish. What is asserted here is not how much fits, but WHO
    // RULES: the notice starts the bar and the pending message does not show.
    let start: String = cabeza.chars().take(15).collect();
    assert!(
        bar.contains(&start),
        "the notice rules the bar while the drag lasts.\n\
         expected the bar to start with: {start:?}\n\
         and the painted bar is:         {bar:?}"
    );
    assert!(
        !bar.contains("un mensaje cualquiera"),
        "and the pending message does not sneak in underneath: {bar:?}"
    );

    // With Shift, MOVE — and the drop does what was promised.
    let mut with_shift = ev(DRAG, 35, FILA0 + 2);
    with_shift.modifiers = KeyModifiers::SHIFT;
    let _ = mouse::handle(&mut app, with_shift);
    let mover = mouse::drop_hint(&app).expect("there is still a drop");
    assert_eq!(
        mover,
        norte_i18n::ta("drag-move", &[("n", "2"), ("to", &dest)]),
    );
    let _ = mouse::handle(&mut app, ev_con(UP, 35, FILA0 + 2, KeyModifiers::SHIFT));
    assert!(
        matches!(
            app.modal,
            Some(Modal::ConfirmTransfer {
                kind: TransferKind::Move,
                ref items,
                ..
            }) if items.len() == 2
        ),
        "the drop does exactly what the notice promised: {:?}",
        app.modal
    );
    assert_eq!(
        mouse::drop_hint(&app),
        None,
        "and on release it stops announcing"
    );
}

/// Marking with the mouse under a quick search in FILTER mode does not
/// reach what the filter hides.
///
/// It is the rule `mark_range`/`set_mark`/`apply_sweep` document and the one
/// that stops the next copy or delete from widening over files the user was
/// not looking at. It breaks as soon as someone closes the filter BEFORE
/// applying the gesture's effects: then a shift+click also marks every
/// hidden index in between, silently, and the bug only shows when the
/// operation is confirmed.
///
/// The range's anchor is also the HIGHLIGHTED row (the filter's selection),
/// not the real cursor, which under a filter can be anywhere.
#[test]
fn marking_under_a_filter_does_not_reach_what_the_filter_hides() {
    let dir = vp("file:///casa");
    // Alternating names: the `sí` filter leaves the EVEN indices visible, so
    // there is always a hidden one between two painted rows.
    let entries: Vec<Entry> = ["a-si", "b-no", "c-si", "d-no", "e-si", "f-no"]
        .iter()
        .map(|n| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new((*n).as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect();
    let mut app = App::new(
        Pane::new(dir.clone(), entries),
        Pane::new(dir.clone(), Vec::new()),
    );
    app.panes[0].quick_start(norte_tui::nav::Mode::Filter);
    for c in "si".chars() {
        app.panes[0].quick_char(c);
    }
    // The REAL cursor stays far from the painted anchor on purpose.
    app.panes[0].set_cursor(5);
    let _ = paint(&mut app);
    assert_eq!(
        app.panes[0].quick_visible(),
        Some(&[0, 2, 4][..]),
        "three rows painted out of six entries"
    );

    // shift+click on the THIRD painted row: range from the painted anchor
    // (the first) to it.
    let _ = mouse::handle(&mut app, ev_con(DOWN, 5, FILA0 + 2, KeyModifiers::SHIFT));
    assert!(
        app.panes[0].quick_visible().is_some(),
        "a marking gesture does NOT close the filter"
    );
    assert_eq!(
        app.panes[0].marks_len(),
        3,
        "the three visible ones, not the five of the absolute range"
    );
    let marked: Vec<bool> = app.panes[0]
        .entries()
        .iter()
        .map(|e| app.panes[0].is_marked(e))
        .collect();
    assert_eq!(marked, [true, false, true, false, true, false]);
}

/// A CLEAN click does close the filter, and that is why it can: it marks
/// nothing. The cursor lands on the clicked entry (ABSOLUTE index), so the
/// next operation acts on the row that was clicked and not on the one the
/// filter had selected.
#[test]
fn a_clean_click_closes_quick_search_on_the_clicked_row() {
    let mut app = app_pintada(20);
    app.panes[0].quick_start(norte_tui::nav::Mode::Filter);
    app.panes[0].quick_char('f');
    let _ = paint(&mut app);
    let expected = app.mouse.geometry().expect("geometry")[0].offset + 2;

    let hit = mouse::hit_test(&app, 5, FILA0 + 2).expect("inside the pane");
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 2));
    assert!(app.panes[0].quick_visible().is_none(), "filter closed");
    assert_eq!(app.panes[0].cursor(), hit.index.expect("row"));
    assert_eq!(
        app.panes[0].cursor(),
        expected,
        "the index is the ABSOLUTE one"
    );
    assert_eq!(app.panes[0].marks_len(), 0, "and it marked nothing");
}

/// With an overlay open the mouse touches nothing: the panes stay painted
/// UNDERNEATH, so the geometry would resolve a row perfectly — and would
/// move the cursor of a listing the user is not looking at while a modal
/// asks them something else. The keyboard is already routed this way.
#[test]
fn with_an_overlay_open_the_mouse_does_not_touch_the_panes() {
    let mut app = app_pintada(5);
    app.modal = Some(norte_tui::app::Modal::ConfirmQuit);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 3));
    assert_eq!(app.panes[0].cursor(), 0);
    assert_eq!(app.panes[0].marks_len(), 0);
}

/// A release EATEN by another pump cannot leave the gesture armed.
///
/// The internal `select!`s (the cd's, `on_tick`, `refresh_panes`, the
/// viewer's) filter `Event::Key` and drop the rest, so a button released
/// while they run never arrives. Without expiry, the next motion — minutes
/// later, in another directory — would continue that sweep and mark rows
/// the user cannot even see; and nothing bounds that in time.
///
/// It expires where it stops being true: the listing moved, so the
/// gesture's indices no longer name what got painted.
#[test]
fn a_release_that_swallowed_another_pump_does_not_leave_the_gesture_armed() {
    let mut app = app_pintada(10);
    let dir = app.panes[0].dir().clone();
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 2));
    let marks = app.panes[0].marks_len();
    assert!(marks > 0, "the sweep was underway");

    // …the release falls inside a pump that only looks at keys: it never
    // arrives. What does happen is that pump refreshes the listing.
    app.panes[0].refresh_listing(entries(&dir, 10));
    let _ = paint(&mut app);

    let after = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + ROWS - 1));
    assert_eq!(
        app.panes[0].marks_len(),
        after,
        "the motion does not continue a sweep that no longer exists"
    );
    let _ = mouse::handle(&mut app, ev(UP, 5, FILA0 + ROWS - 1));
    assert_eq!(app.panes[0].marks_len(), after, "nor the late release");
}

/// A click BEFORE a cd and another one after are not a double click.
///
/// Both land on the same cell — the pane's row 2 — and may fall within the
/// same 400 ms window, but in between the pane changed directory: the
/// second row 2 is a different file. Pairing them enters a directory nobody
/// chose, and is one of the hardest things to explain ("I clicked twice and
/// it went into a folder I never touched").
#[test]
fn a_click_before_and_another_after_a_cd_are_not_a_double_click() {
    let mut app = app_pintada(10);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(DOWN, 5, FILA0 + 2), t0),
        After::Nothing
    );

    // cd: the pane switches to another listing (the real path of `nav.enter`).
    let other = vp("file:///casa/subdir");
    app.panes[0].set_listing(other.clone(), entries(&other, 10));
    let _ = paint(&mut app);

    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 2),
            t0 + std::time::Duration::from_millis(80)
        ),
        After::Nothing,
        "same cell and 80 ms, but no longer the same row"
    );
}

/// A modal opened mid-drag also carries away the gesture: by the time the
/// user answers, the drag is history.
#[test]
fn a_modal_opened_mid_drag_carries_the_gesture_along() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 2));
    let marks = app.panes[0].marks_len();

    app.modal = Some(norte_tui::app::Modal::ConfirmQuit);
    let _ = paint(&mut app);
    app.modal = None;
    let _ = paint(&mut app);

    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + ROWS - 1));
    assert_eq!(app.panes[0].marks_len(), marks, "dead gesture");
}

/// And what must NOT expire: a normal frame, with nothing moving, leaves
/// the gesture alive. Without this, expiry would be "always cancel", which
/// passes the two tests above and breaks every drag.
#[test]
fn a_normal_frame_does_not_expire_the_gesture() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = paint(&mut app);
    let _ = paint(&mut app);
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 4));
    assert_eq!(app.panes[0].marks_len(), 4, "the sweep is still alive");
}

/// A `pane.swap` mid-drag also carries away the gesture.
///
/// `listing_epoch` TRAVELS with the pane, so the swap just crosses the two
/// values: when they TIE — both panes having listed the same number of
/// times, the norm right after startup — the epoch-based check sees nothing
/// move and the gesture survives. But its `Spot { pane, index }` now names
/// the content of the OTHER side: the gesture was reattributed behind the
/// reader's back.
#[test]
fn swapping_panes_mid_drag_carries_the_gesture_along() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + 2));
    assert!(app.panes[0].marks_len() > 0, "the sweep was underway");
    assert_eq!(
        app.panes[0].listing_epoch(),
        app.panes[1].listing_epoch(),
        "the epochs TIE: that is exactly what blinds the epoch check"
    );

    app.swap_panes();
    let _ = paint(&mut app);

    // The sweep's marks traveled with their pane to slot 1; pane 0 is now
    // the other listing, and the armed gesture still names `pane: 0`.
    let before = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(DRAG, 5, FILA0 + ROWS - 1));
    assert_eq!(
        app.panes[0].marks_len(),
        before,
        "the drag cannot continue over the other side's content"
    );
}

/// The capture is RELEASED before handing the terminal to an external
/// program and restored on return. Without this, the launched program (an
/// editor, a pager) inherits a mouse-mode terminal it never asked for and
/// receives every pointer movement as if it were keys.
#[test]
fn the_capture_is_released_and_restored_around_suspend() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    cap.set(true, &mut out).expect("activate");
    assert!(cap.active());

    out.clear();
    let had = mouse::release_for_suspend(&mut cap, &mut out).expect("release");
    assert!(had, "it was set");
    assert!(
        !cap.active(),
        "with the terminal handed over, the capture is not ours"
    );
    assert!(
        !out.is_empty(),
        "the terminal was told, not just the struct"
    );

    mouse::restore_after_suspend(&mut cap, had, &mut out).expect("restore");
    assert!(cap.active(), "on return, as it was");
}

/// And if it was NOT set (`[ui] mouse = false`), returning from the
/// external program does not turn it on: suspend restores the state, not a
/// default.
#[test]
fn suspend_does_not_turn_on_a_capture_that_was_off() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    let had = mouse::release_for_suspend(&mut cap, &mut out).expect("release");
    assert!(!had);
    mouse::restore_after_suspend(&mut cap, had, &mut out).expect("restore");
    assert!(!cap.active());
    assert!(out.is_empty(), "not one sequence for a no-op");
}

/// `[ui] mouse = false`: neither capture nor handling.
///
/// Both halves, because they are two mechanisms. The capture: `set(false)`
/// writes NOTHING to the terminal and leaves the state off, so the emulator
/// never reports mouse events and the run loop's `Event::Mouse` arm never
/// gets to run. And the handling: with no geometry — which also happens
/// before the first frame and with the viewer open — no event that slipped
/// through would resolve any row.
#[test]
fn with_mouse_false_no_capture_or_handling() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("norte.toml"), "[ui]\nmouse = false\n").expect("write");
    let cfg = norte_config::load(&norte_config::Layers {
        dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
    })
    .expect("load");
    assert_eq!(cfg.ui_mouse, Some(false));

    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    cap.set(cfg.ui_mouse.unwrap_or(true), &mut out)
        .expect("apply");
    assert!(!cap.active(), "no capture");
    assert!(out.is_empty(), "nothing written to the terminal");

    let mut app = app_pintada(5);
    mouse::after_frame(&mut app, None, mouse::FrameZones::default());
    let _ = mouse::handle(&mut app, ev(DOWN, 5, FILA0 + 3));
    assert_eq!(
        app.panes[0].cursor(),
        0,
        "with no geometry nothing resolves"
    );
    assert_eq!(app.focus(), 0);
}

/// **Review MAJOR-1.** The differences pane (`Shift+F2`) REPLACES both
/// panes on screen. Without declaring it an overlay, the panes' geometry
/// stayed valid and the mouse resolved rows of a listing the reader cannot
/// see: the wheel moved its cursor, a click marked entries, and a DOUBLE
/// click requested a real `nav.enter` — a `cd` in an invisible pane, with
/// the panel still open over roots that no longer describe anyone.
///
/// `keyboard_owner` already declares it the keyboard's owner; this is the
/// other half of the same piece, and `overlay_open`'s rustdoc is where the
/// rule is written: "the mouse does the same, as one piece."
#[test]
fn the_diff_pane_eats_the_mouse_like_any_overlay() {
    let mut app = app_pintada(5);
    let dir_before = [app.panes[0].dir().clone(), app.panes[1].dir().clone()];
    let cursor_before = app.panes[0].cursor();

    app.compare = Some(norte_tui::app::CompareView::new(
        vp("file:///izq"),
        vp("file:///der"),
        0,
        None,
        None,
    ));

    let t0 = std::time::Instant::now();
    // Any click, and the double click that would be a `cd`.
    assert_eq!(
        mouse::handle_at(&mut app, ev(DOWN, 5, FILA0 + 1), t0),
        After::Nothing
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(DOWN, 5, FILA0 + 1),
            t0 + std::time::Duration::from_millis(120)
        ),
        After::Nothing,
        "a double click CANNOT request a nav.enter on a pane you cannot see"
    );
    assert_eq!(app.panes[0].cursor(), cursor_before, "nor move its cursor");
    assert_eq!(
        [app.panes[0].dir().clone(), app.panes[1].dir().clone()],
        dir_before
    );
    assert!(app.compare.is_some(), "and the panel is still where it was");
}

/// The sticky window, END-TO-END: `End` and then scrolling up to the top has
/// to leave the listing showing the start, not stuck where it was.
#[test]
fn dragging_up_from_the_end_ends_up_dragging_the_window() {
    let mut app = app_pintada(60);
    app.panes[0].move_to_end();
    let _ = paint(&mut app);
    let down = app.mouse.geometry().expect("geometry")[0].offset;
    assert!(down > 0, "the end scrolls the window: {down}");

    // Scroll up ONE row: the window does not move (the cursor goes inside).
    app.panes[0].move_up(1);
    let _ = paint(&mut app);
    assert_eq!(
        app.mouse.geometry().expect("geometry")[0].offset,
        down,
        "scrolling up inside the window does not move it"
    );

    // And all the way to the top: the window ends at 0.
    for _ in 0..60 {
        app.panes[0].move_up(1);
    }
    let _ = paint(&mut app);
    assert_eq!(app.panes[0].cursor(), 0);
    assert_eq!(
        app.mouse.geometry().expect("geometry")[0].offset,
        0,
        "the cursor at the very top has to be visible"
    );
}

/// The border between the two panes is DRAGGABLE, and what one gains the
/// other loses.
///
/// The drag writes to the TREE, which is what the session saves: that is
/// why a moved border stays where it was left when reopened, with nothing
/// more.
#[test]
fn dragging_the_edge_moves_the_boundary_between_the_panes() {
    let mut app = app_pintada(5);
    let _ = paint(&mut app);
    let before = app.mouse.geometry().expect("geometry").to_vec();
    let (left_before, right_before) = (before[0].width, before[1].width);
    let edge = before[0].x + before[0].width;

    // Grab the border and move it six cells to the left. Six and not
    // twenty: a `browser` declares a minimum of twenty columns, and below
    // that the layout COLLAPSES its split — the pane does not shrink, it
    // disappears. What this test measures is the drag, not the collapse.
    let dest = edge - 6;
    let _ = mouse::handle(&mut app, ev(DOWN, edge, FILA0));
    let _ = mouse::handle(&mut app, ev(DRAG, dest, FILA0));
    let _ = mouse::handle(&mut app, ev(UP, dest, FILA0));
    let _ = paint(&mut app);

    let now = app.mouse.geometry().expect("geometry").to_vec();
    assert!(
        now[0].width < left_before,
        "the left one shrinks: {left_before} -> {}",
        now[0].width
    );
    assert!(
        now[1].width > right_before,
        "and what it loses the other gains: {right_before} -> {}",
        now[1].width
    );
    assert_eq!(
        now[0].width + now[1].width,
        left_before + right_before,
        "the pair occupies the same total: dragging a border does not touch the rest"
    );
}

/// And grabbing the border neither points at nor marks anything: grabbing
/// is not choosing.
#[test]
fn grabbing_the_edge_does_not_select_a_row() {
    let mut app = app_pintada(5);
    let _ = paint(&mut app);
    let cursor = app.panes[0].cursor();
    let geom = app.mouse.geometry().expect("geometry").to_vec();
    let edge = geom[0].x + geom[0].width;
    let _ = mouse::handle(&mut app, ev(DOWN, edge, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(UP, edge, FILA0 + 1));
    assert_eq!(app.panes[0].cursor(), cursor, "the cursor did not move");
    assert_eq!(app.panes[0].marks_len(), 0, "and nothing got marked");
}

/// Clicking a listing also gives it the KEYBOARD, not just focus.
///
/// With the sidebar in front, a click on the listing moved its cursor and
/// left the arrows on the sidebar: the focus border said one thing and the
/// keyboard went to another. Pointing at a panel with the mouse means "I
/// work here now," and that includes the keys.
#[test]
fn a_click_on_a_listing_brings_the_keyboard_from_the_sidebar() {
    let mut app = app_pintada(5);
    app.toggle_places();
    let _ = paint_at(&mut app, 100, 30);
    assert_eq!(app.key_owner(), KeyOwner::Places, "the sidebar took it");

    let g = app.mouse.geometry().expect("geometry")[0];
    let after = mouse::handle(&mut app, ev(DOWN, g.x + 2, g.first_list_row));
    assert_eq!(after, After::Nothing);
    assert_eq!(
        app.key_owner(),
        KeyOwner::Panes,
        "and the click brings it back"
    );
    assert_eq!(app.focus(), 0);
}

/// And the other way around: clicking a SIDE panel gives it the keyboard,
/// even if there is no row there to resolve.
///
/// The same gesture for both sides, and through the same path: what decides
/// whose the keyboard is, is the slot under the pointer.
#[test]
fn a_click_on_a_side_pane_gives_it_the_keyboard() {
    let mut app = app_pintada(5);
    app.toggle_processes();
    app.return_keys_to_panes();
    let _ = paint_at(&mut app, 100, 30);

    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let id = app.processes_slot().expect("open");
    let r = ui::resolved_for(&app, area)
        .placements
        .iter()
        .find(|(s, _)| *s == id)
        .map(|(_, r)| *r)
        .expect("the panel was placed");
    let _ = mouse::handle(&mut app, ev(DOWN, r.x + 1, r.y + 1));
    assert_eq!(app.key_owner(), KeyOwner::Processes);
}

/// A test plugin for the manager, approved and enabled.
fn plugin(id: &str, name: &str, category: &str) -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: id.into(),
        name: name.into(),
        publisher: "norte".into(),
        version: "1.0.0".into(),
        category: category.into(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: Vec::new(),
        has_help: false,
        manifest_digest: None,
    }
}

/// An `App` with the extension manager open over two plugins, painted at
/// `w`×`h`. Returns the lines to check the zones against the text.
fn app_with_manager(w: u16, h: u16) -> (App, Vec<String>) {
    let mut app = app_pintada(3);
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![
            plugin("org.norte.uno", "Uno", "columns"),
            plugin("org.norte.dos", "Dos", "previewer"),
        ],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let lines = paint_at(&mut app, w, h);
    (app, lines)
}

/// The row and column of the first occurrence of `text` in what is
/// painted.
///
/// `TestBackend::to_string()` wraps each row in quotes: the screen's first
/// cell is the line's second character.
fn where_(lines: &[String], text: &str) -> (u16, u16) {
    for (y, l) in lines.iter().enumerate() {
        let l = l.strip_prefix('"').unwrap_or(l);
        if let Some(byte) = l.find(text) {
            let col = l[..byte].chars().count();
            return (
                u16::try_from(y).expect("row"),
                u16::try_from(col).expect("column"),
            );
        }
    }
    panic!("{text:?} is not painted:\n{}", lines.join("\n"));
}

/// The extension manager was born mute to the mouse: `overlay_open`
/// returned `Nothing` for everything. A click on a row selects it, and on
/// the ALREADY selected row opens its settings — what its footer promises
/// ("press Enter, or the row"). Checked against the PAINTED TEXT: the row
/// that gets clicked is the one showing "Dos".
#[test]
fn clicking_a_manager_row_selects_it_and_repeating_it_opens_its_settings() {
    let (mut app, lines) = app_with_manager(100, 24);
    let (row, col) = where_(&lines, "Dos v1.0.0");
    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, col, row)),
        After::Nothing,
        "selecting does not talk to the backend"
    );
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, col, row)),
        After::Extension("dialog.confirm"),
        "the already-selected row opens its settings via the SAME command as Enter"
    );
}

/// The card's buttons fire THE SAME command as their key, and come from
/// what is painted: the mouse finds "[Disable]" where the frame put it.
#[test]
fn the_cards_buttons_fire_their_keys_command() {
    // The labels below are `en.ftl`'s, so the language is FIXED before
    // painting: without this the test reads the `LANG` of whoever runs it
    // and on a Spanish machine looks for "[Disable]" where "[TurnOff]" was
    // painted. Same pattern as `render.rs`; nextest gives one process per
    // test.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (mut app, lines) = app_with_manager(100, 24);
    for (label, cmd) in [
        ("[Disable]", "dialog.toggle-enabled"),
        ("[Revoke]", "dialog.approve"),
        ("[Settings]", "dialog.confirm"),
        ("[Uninstall]", "dialog.remove"),
    ] {
        let (row, col) = where_(&lines, label);
        assert_eq!(
            mouse::handle(&mut app, ev(DOWN, col + 1, row)),
            After::Extension(cmd),
            "{label}"
        );
    }
    // And between two buttons there is nothing: the gap is not a button.
    let (row, col) = where_(&lines, "[Disable] [Revoke]");
    let slot = col + u16::try_from("[Disable]".len()).unwrap();
    assert_eq!(mouse::handle(&mut app, ev(DOWN, slot, row)), After::Nothing);
}

/// The wheel moves the list's cursor, and selecting ANOTHER row with the
/// previous one's settings open closes them: the card cannot show one
/// plugin and another one's settings.
#[test]
fn the_wheel_moves_the_cursor_and_changing_row_closes_foreign_settings() {
    let (mut app, lines) = app_with_manager(100, 24);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollDown, 50, 10));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 50, 10));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 0);
    app.extensions.as_mut().unwrap().config = Some(norte_tui::app::PluginConfigPanel {
        plugin_id: "org.norte.uno".into(),
        plugin_name: "Uno".into(),
        state: norte_frontend::plugin_config::PluginConfigState::new(Vec::new()),
    });
    let (row, col) = where_(&lines, "Dos v1.0.0");
    let _ = mouse::handle(&mut app, ev(DOWN, col, row));
    let mgr = app.extensions.as_ref().unwrap();
    assert_eq!(mgr.cursor, 1);
    assert!(mgr.config.is_none(), "the settings were \"Uno\"'s");
}

/// On a narrow terminal there is no card or buttons, but the usual list
/// rows are still clickable — with the description below, which is NOT a
/// row.
#[test]
fn in_narrow_mode_the_rows_are_clickable_and_the_description_is_not() {
    let (mut app, lines) = app_with_manager(W, H);
    let (row, col) = where_(&lines, "Dos v1.0.0");
    let _ = mouse::handle(&mut app, ev(DOWN, col, row));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    // The category header above is not a plugin.
    let (row, col) = where_(&lines, "previewer");
    let _ = mouse::handle(&mut app, ev(DOWN, col, row));
    assert_eq!(
        app.extensions.as_ref().unwrap().cursor,
        1,
        "a header selects nothing"
    );
}

/// With help open the mouse EXISTS: the wheel scrolls down through the text
/// and a click on the index selects that page. Until now help was one more
/// overlay for `overlay_open`'s lock, and both gestures were dropped
/// entirely.
#[test]
fn the_wheel_scrolls_down_through_help_and_a_click_chooses_a_page() {
    let (w, h) = (100u16, 30u16);
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    let mut app = app_pintada(3);
    norte_tui::overlays::open_help_topic(&mut app, norte_help::Lang::Es, &[], "copying");
    let refresh = |app: &mut App| {
        let (width, alto) = ui::help_body_size(area, norte_help::Lang::Es);
        app.refresh_help(width, alto);
        let _ = paint_at(app, w, h);
    };
    refresh(&mut app);
    let z = ui::help_zones(&app, area).expect("help is open");

    // The wheel over the BODY scrolls it.
    let before = app.help.as_ref().expect("open").state.body_scroll();
    let _ = mouse::handle(
        &mut app,
        ev(MouseEventKind::ScrollDown, z.body.x + 2, z.body.y + 2),
    );
    refresh(&mut app);
    let after = app.help.as_ref().expect("open").state.body_scroll();
    assert!(
        after > before,
        "the wheel scrolled the text down: {before} → {after}"
    );

    // A click on a VISIBLE index page that is not the open one selects it.
    let z = ui::help_zones(&app, area).expect("help is open");
    let cursor = app.help.as_ref().expect("open").state.cursor();
    let &(row, model) = z
        .rows
        .iter()
        .find(|(_, m)| *m != cursor)
        .expect("there is another page in view");
    let _ = mouse::handle(&mut app, ev(DOWN, z.sidebar.x + 3, row));
    assert_eq!(
        app.help.as_ref().expect("open").state.cursor(),
        model,
        "the click selected THAT row's page"
    );

    // And none of this reaches the panes below.
    assert_eq!(
        mouse::handle(&mut app, ev(DOWN, 0, 0)),
        After::Nothing,
        "with help in front, a click outside it does nothing"
    );
}
