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
const FILAS: u16 = 6;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

/// `n` entries `f0..f{n-1}` under `dir`.
fn entradas(dir: &VPath, n: usize) -> Vec<Entry> {
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
        Pane::new(dir.clone(), entradas(&dir, n)),
        Pane::new(dir.clone(), entradas(&dir, n)),
    );
    let _ = pintar(&mut app);
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
fn pintar(app: &mut App) -> Vec<String> {
    pintar_en(app, W, H)
}

/// Like [`pintar`] over a terminal of a different size: with a side panel
/// open, 60×12 is not enough to place it and the layout leaves it out.
fn pintar_en(app: &mut App, w: u16, h: u16) -> Vec<String> {
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
fn assert_fila(lines: &[String], app: &App, row: u16, index: usize) {
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
/// painted terminal of `ancho` columns: `(cell, header row, id, width)`.
///
/// Comes from the SAME layout that paints the header, over the geometry's
/// interior width: a border calculated with different arithmetic would pass
/// with the layout broken.
fn borde_de_la_segunda_columna(app: &App) -> (u16, u16, norte_frontend::columns::ColumnId, u16) {
    let g = &app.mouse.geometry().expect("there is geometry")[0];
    let (x0, interior, cabecera) = (g.x + 1, g.width - 2, g.first_list_row - 1);
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
    (x0 + cols[0].1, cabecera, cols[1].0.clone(), cols[1].1)
}

/// The width the layout gives RIGHT NOW to column `id` of the left pane.
fn ancho_de(app: &App, id: &norte_frontend::columns::ColumnId) -> u16 {
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
fn arrastrar_el_borde_de_una_columna_cambia_su_ancho_y_pide_guardarlo() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, 3)),
        Pane::new(dir.clone(), entradas(&dir, 3)),
    );
    let _ = pintar_en(&mut app, 120, H);
    let (borde, cabecera, id, ancho) = borde_de_la_segunda_columna(&app);

    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, borde, cabecera)),
        After::Nothing
    );
    assert_eq!(
        mouse::handle(&mut app, ev(ARRASTRE, borde - 3, cabecera)),
        After::Nothing
    );
    assert_eq!(
        ancho_de(&app, &id),
        ancho + 3,
        "the border follows the pointer"
    );
    assert_eq!(
        mouse::handle(&mut app, ev(ARRIBA, borde - 3, cabecera)),
        After::ColumnWidth
    );
    assert_eq!(
        app.mouse.take_column_width(),
        Some((id.to_string(), ancho + 3))
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
fn un_clic_en_el_borde_de_una_columna_no_guarda_nada() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, 3)),
        Pane::new(dir.clone(), entradas(&dir, 3)),
    );
    let _ = pintar_en(&mut app, 120, H);
    let (borde, cabecera, id, ancho) = borde_de_la_segunda_columna(&app);
    mouse::handle(&mut app, ev(ABAJO, borde - 1, cabecera));
    assert_eq!(
        mouse::handle(&mut app, ev(ARRIBA, borde - 1, cabecera)),
        After::Nothing
    );
    assert_eq!(app.mouse.take_column_width(), None);
    assert_eq!(ancho_de(&app, &id), ancho);
}

/// Grabbing the cell BEFORE the separator does not jump the width on the
/// first move: it is measured against where it was grabbed, not against the
/// border.
#[test]
fn agarrar_antes_del_separador_no_salta_una_celda() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, 3)),
        Pane::new(dir.clone(), entradas(&dir, 3)),
    );
    let _ = pintar_en(&mut app, 120, H);
    let (borde, cabecera, id, ancho) = borde_de_la_segunda_columna(&app);
    mouse::handle(&mut app, ev(ABAJO, borde - 1, cabecera));
    mouse::handle(&mut app, ev(ARRASTRE, borde - 3, cabecera));
    assert_eq!(
        ancho_de(&app, &id),
        ancho + 2,
        "two cells to the left, two more"
    );
}

/// Left button down.
const ABAJO: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
/// Left button up.
const ARRIBA: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
/// Drag with the left button held.
const ARRASTRE: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

/// A DETACHED window carries its indicator in the status bar, and clicking it
/// requests the explanation: the run loop opens help on the panels page. The
/// zone comes from the PAINTED frame, so it is checked against the line the
/// reader has in front of them and not against parallel arithmetic.
#[test]
fn pulsar_el_indicador_de_sesion_pide_la_ayuda() {
    let mut app = app_pintada(3);
    app.session.detached = true;
    let lines = pintar(&mut app);
    // The test backend quotes each line: the first cell is byte 1, not 0.
    let barra = lines[usize::from(H - 1)].trim_start_matches('"');
    let badge = app.session_banner().expect("there is an indicator");
    assert!(barra.contains(&badge), "the bar paints it: {barra}");
    let x0 = u16::try_from(barra.find(&badge).expect("it is there")).expect("it fits");
    // The first character's column: on this line everything before it is
    // ASCII, so bytes and cells coincide.
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, x0, H - 1)),
        After::SessionHelp,
        "clicking the indicator requests help"
    );
    // And to its left, no: the rest of the bar is not clickable.
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, x0.saturating_sub(1), H - 1)),
        After::Nothing
    );
    // The page exists in the corpus, in both languages: the constant cannot
    // point at a deleted page without this turning red.
    for (lang, sesion) in [
        (norte_help::Lang::Es, "sesión"),
        (norte_help::Lang::En, "session"),
    ] {
        let pagina = norte_help::topic(lang, mouse::SESSION_HELP_TOPIC)
            .unwrap_or_else(|| panic!("page {} exists in {lang:?}", mouse::SESSION_HELP_TOPIC));
        // And it TALKS about the session: existing was not enough. When
        // `panes` was split, the constant kept pointing at a real page that
        // no longer covered it.
        let habla = pagina.blocks.iter().any(|b| {
            matches!(b, norte_help::Block::Heading { text, .. } if text.to_lowercase().contains(sesion))
        });
        assert!(
            habla,
            "{lang:?}: page {} has no section about the {sesion}",
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
    let _ = pintar(&mut app);
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, x0, H - 1)),
        After::Nothing
    );
}

#[test]
fn el_layout_de_estos_tests_es_el_que_se_pinta() {
    // Anchor for the numbers above: if the pane gains or loses chrome, THIS
    // test fails with a clear message, and not the next six with confusing
    // arithmetic.
    let mut app = app_pintada(5);
    let lines = pintar(&mut app);
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
    assert_eq!(left.list_rows, FILAS, "interior minus the header");
    assert_eq!(left.offset, 0, "cursor on the first: no scroll");
    // And what is really PAINTED on those rows. The sort indicator (`▲`)
    // instead of the column's label: the label is translated and these
    // tests do not fix a language.
    let cabecera = usize::from(FILA0) - 1;
    assert!(
        lines[cabecera].contains('▲'),
        "row {cabecera} = column header: {}",
        lines[cabecera]
    );
    // And row 0 is the menu bar, which is what pushed the header down there.
    // Checked by its SHAPE and not by a label: these tests do not fix a
    // language, and "Archivo" only shows up in one of the two.
    assert!(
        !lines[0].trim().is_empty() && !lines[0].contains('│'),
        "row 0 = menu bar (text, no panel borders): {}",
        lines[0]
    );
    assert_fila(&lines, &app, FILA0, 0);
}

#[test]
fn un_click_en_la_primera_fila_resuelve_la_primera_entrada() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0).expect("inside the left pane");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, Some(0));
}

#[test]
fn un_click_en_la_ultima_entrada_resuelve_esa_y_no_otra() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0 + 4).expect("inside the pane");
    assert_eq!(hit.index, Some(4), "fifth painted row = fifth entry");
}

/// The column header is CHROME: it resolves to the pane, never to a row.
/// Without this, sorting by a column with the mouse (which is what the user
/// is going to try there) would also move the cursor to the first entry.
#[test]
fn la_cabecera_de_columnas_no_es_ninguna_fila() {
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
fn los_bordes_del_pane_no_son_filas() {
    let app = app_pintada(5);
    // Row 0 is no longer the pane's border: it is the MENU BAR, and belongs
    // to no panel — same as the status bar below. A click there cannot
    // resolve to an entry or to a pane.
    assert!(
        mouse::hit_test(&app, 5, 0).is_none(),
        "row 0 is the menu bar, not a pane"
    );
    for row in [FILA0 - 2, FILA0 + FILAS] {
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
fn un_click_en_la_barra_de_menu_la_abre() {
    let mut app = app_pintada(5);
    assert!(app.menu.is_none(), "starts closed");
    let _ = mouse::handle(&mut app, ev(ABAJO, 2, 0));
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
fn el_menu_se_reabre_por_donde_iba() {
    let mut app = app_pintada(5);
    let mut m = norte_frontend::menu::MenuState::new();
    m.open(3);
    app.menu = Some(m);
    app.close_menu();
    assert!(app.menu.is_none(), "closed");

    app.menu = Some(norte_frontend::menu::MenuState::reopen_at(app.menu_ultimo));
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
fn sin_barras_fijadas_un_click_arriba_no_abre_nada() {
    let mut app = app_pintada(5);
    app.menu_bar = false;
    app.panel_bar = false;
    let _ = pintar(&mut app);
    let _ = mouse::handle(&mut app, ev(ABAJO, 2, 0));
    assert!(app.menu.is_none());
    assert!(app.pending_panel_command.is_none());
}

/// Without the menu one but WITH the panel one, row 0 belongs to the panel
/// bar: it moves up and stays clickable.
#[test]
fn sin_barra_de_menus_la_de_paneles_se_muda_a_la_fila_cero() {
    let mut app = app_pintada(5);
    app.menu_bar = false;
    let _ = pintar(&mut app);
    let _ = mouse::handle(&mut app, ev(ABAJO, 1, 0));
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
fn cada_boton_de_la_barra_cae_en_su_sitio() {
    let mut app = app_pintada(5);
    // This test's columns are the LETTER ones (three cells per button); with
    // names, the zones follow what is painted and `theme_render` checks it.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let _ = pintar(&mut app);
    let pulsa = |app: &mut norte_tui::app::App, col: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(ABAJO, col, 1));
        app.pending_panel_command.clone()
    };
    assert_eq!(pulsa(&mut app, 1).as_deref(), Some("layout.places"));
    assert_eq!(pulsa(&mut app, 16).as_deref(), Some("layout.log"));
    assert_eq!(
        pulsa(&mut app, 19).as_deref(),
        Some("layout.disk-map"),
        "the disk map (phase 4) landed after the log"
    );
    assert_eq!(
        pulsa(&mut app, 22).as_deref(),
        Some("layout.timeline"),
        "and the timeline (phase 7) after the map, by registration order"
    );
    assert_eq!(
        pulsa(&mut app, 25).as_deref(),
        Some("layout.terminal"),
        "and the terminal (#362) after the timeline, by registration order"
    );
    assert_eq!(
        pulsa(&mut app, 27),
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
fn en_columna_cada_boton_es_una_fila_del_borde_izquierdo() {
    let mut app = app_pintada(5);
    app.chrome.panel_bar_position = Some(norte_config::PanelBarPosition::Left);
    let lineas = pintar(&mut app);
    let pulsa = |app: &mut norte_tui::app::App, fila: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(ABAJO, 1, fila));
        app.pending_panel_command.clone()
    };
    assert_eq!(pulsa(&mut app, 1).as_deref(), Some("layout.places"));
    assert_eq!(pulsa(&mut app, 6).as_deref(), Some("layout.log"));
    // What is painted says the same: one ICON per row (ADR 0140), on column
    // 1 (character 2: `TestBackend` puts the line in quotes).
    let celda = |lineas: &[String], f: usize| {
        lineas[f]
            .chars()
            .nth(2)
            .expect("there is column 1")
            .to_string()
    };
    let icono = |k| {
        norte_frontend::panelbar::icon(k, norte_frontend::panelbar::IconSet::Unicode)
            .expect("icon")
            .to_owned()
    };
    assert_eq!(celda(&lineas, 1), icono("places"), "{:?}", lineas[1]);
    assert_eq!(celda(&lineas, 6), icono("log"), "{:?}", lineas[6]);
    // And the listing moved with it: its first row is now 3, starting at
    // column 3; the rail's column belongs to no pane.
    assert!(mouse::hit_test(&app, 5, FILA0 - 1).is_some());
    assert!(mouse::hit_test(&app, 1, FILA0 - 1).is_none());

    // With `letters`, the usual letters.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let lineas = pintar(&mut app);
    assert!(
        celda(&lineas, 1).chars().all(char::is_alphabetic),
        "{:?}",
        lineas[1]
    );

    // With room to spare, AIR between icons, like VS Code: the first one a
    // row lower and a blank one between two. The mouse measures the same.
    app.chrome.panel_bar_style = None;
    let lineas = pintar_en(&mut app, 80, 40);
    assert_eq!(celda(&lineas, 2), icono("places"), "{:?}", lineas[2]);
    assert_eq!(celda(&lineas, 3), " ", "air row: {:?}", lineas[3]);
    assert_eq!(celda(&lineas, 4), icono("viewer"), "{:?}", lineas[4]);
    assert_eq!(pulsa(&mut app, 2).as_deref(), Some("layout.places"));
    assert_eq!(pulsa(&mut app, 3), None, "air is not a button");
    assert_eq!(pulsa(&mut app, 4).as_deref(), Some("layout.preview"));
}

/// The layout buttons (ADR 0133) go on the menu bar's right edge and run
/// their order; on a terminal that does not let them fit whole next to the
/// titles, there is none to click.
#[test]
fn los_botones_de_disposicion_caen_en_el_borde_derecho() {
    let mut app = app_pintada(5);
    let lineas = pintar_en(&mut app, 120, H);
    // `[#]` is the last one: its three cells are the row 0's last three.
    assert!(
        lineas[0].trim_end_matches('"').ends_with("[#]"),
        "{:?}",
        lineas[0]
    );
    let pulsa = |app: &mut norte_tui::app::App, col: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(ABAJO, col, 0));
        app.pending_panel_command.clone()
    };
    // All five at 120 columns: `[|] [-] [=] [/] [#]` starting at 101.
    assert_eq!(pulsa(&mut app, 119).as_deref(), Some("layout.pick"));
    assert_eq!(pulsa(&mut app, 101).as_deref(), Some("layout.split-h"));
    assert_eq!(pulsa(&mut app, 114).as_deref(), Some("layout.flip"));
    assert_eq!(pulsa(&mut app, 104), None, "the gap between two buttons");
    // With an overlay in front (the review caught it): they neither paint
    // nor click. Painted and dead was the class of BLOCKER the panel bar
    // already had.
    app.open_theme_picker();
    let lineas = pintar_en(&mut app, 120, H);
    assert!(!lineas[0].contains("[#]"), "{:?}", lineas[0]);
    assert_eq!(pulsa(&mut app, 119), None);
    app.theme_picker = None;
    // Narrowing, the first to yield is flip, and the usual four stay there
    // (ADR 0138). The exact width depends on the titles' language, so it is
    // searched for.
    let cede = (60..120)
        .rev()
        .find(|w| !pintar_en(&mut app, *w, H)[0].contains("[/]"))
        .expect("it yields at some width");
    let lineas = pintar_en(&mut app, cede, H);
    assert!(lineas[0].contains("[|] [-] [=] [#]"), "{:?}", lineas[0]);
    // At sixty columns the titles keep the spot.
    let lineas = pintar(&mut app);
    assert!(!lineas[0].contains("[#]"), "{:?}", lineas[0]);
}

/// Grabs `desde`'s title and drops it on the bottom half of `sobre`.
fn soltar_debajo(
    app: &mut App,
    desde: norte_frontend::layout::Rect,
    sobre: norte_frontend::layout::Rect,
) {
    let (x, y) = (sobre.x + sobre.width / 2, sobre.y + sobre.height - 2);
    let _ = mouse::handle(app, ev(ABAJO, desde.x + 4, desde.y));
    let _ = mouse::handle(app, ev(ARRASTRE, x, y));
    let _ = mouse::handle(app, ev(ARRIBA, x, y));
}

/// ADR 0138: where stacking two listings would hide one, dropping does
/// nothing and says so: the panel cannot disappear by being moved.
#[test]
fn mover_donde_no_cabe_se_rehusa_y_se_dice() {
    let mut app = app_pintada(5);
    let _ = pintar_en(&mut app, 120, H);
    let a = app.mouse.slot_rect(app.panes.slot_of(0)).expect("placed");
    let b = app.mouse.slot_rect(app.panes.slot_of(1)).expect("placed");
    let antes = app.layout.clone();
    app.message = None;
    soltar_debajo(&mut app, a, b);
    assert_eq!(app.layout, antes, "at {H} rows they do not fit stacked");
    assert!(app.message.is_some(), "and it says so");
}

/// ADR 0138: dragging a listing by its title row and dropping it on the
/// other one's bottom half stacks them; clicking without dragging just
/// focuses.
#[test]
fn arrastrar_el_titulo_mueve_el_panel() {
    let mut app = app_pintada(5);
    // Room to spare: at `H` rows two stacked listings do not fit.
    let _ = pintar_en(&mut app, 120, 50);
    let izq = app.panes.slot_of(0);
    let der = app.panes.slot_of(1);
    let rect = |app: &norte_tui::app::App, s| {
        app.mouse
            .slot_rect(s)
            .unwrap_or_else(|| panic!("{s:?} not placed: {:?}", app.layout))
    };
    let (a, b) = (rect(&app, izq), rect(&app, der));
    assert_eq!(a.y, b.y, "side by side at the start");
    let antes = app.layout.clone();

    // A click on the title moves nothing.
    let _ = mouse::handle(&mut app, ev(ABAJO, a.x + 4, a.y));
    let _ = mouse::handle(&mut app, ev(ARRIBA, a.x + 4, a.y));
    assert_eq!(app.layout, antes, "a click is not a drag");

    // Grab, drag to the other one's bottom half: it highlights.
    let destino_y = b.y + b.height - 2;
    let destino_x = b.x + b.width / 2;
    let _ = mouse::handle(&mut app, ev(ABAJO, a.x + 4, a.y));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, destino_x, destino_y));
    assert!(
        app.mouse.move_target().is_some(),
        "the destination highlights"
    );
    let _ = mouse::handle(&mut app, ev(ARRIBA, destino_x, destino_y));
    assert!(app.mouse.move_target().is_none());
    let foco = app.focused_slot();
    let _ = pintar_en(&mut app, 120, 50);
    let (a, b) = (rect(&app, izq), rect(&app, der));
    assert!(a.y > b.y && a.x == b.x, "stacked: {a:?} under {b:?}");
    // Focus stays on its SLOT, even though its position changed.
    assert_eq!(app.focused_slot(), foco);
}

/// ADR 0134: two panels on the same edge share a spot as tabs, and their
/// slot's first row is the STRIP with both names. Clicking the hidden one
/// runs its command (which reveals it); the one in front is not a zone.
#[test]
fn los_paneles_de_un_borde_se_agrupan_con_su_tira() {
    let mut app = app_pintada(5);
    // Both go to the RIGHT.
    app.toggle_preview();
    app.toggle_metadata();
    let (huecos, activo) = app
        .layout
        .tabs_of(app.metadata_slot().expect("details open"))
        .expect("in a group");
    assert_eq!(huecos.len(), 2, "the viewer and the details together");
    assert_eq!(activo, 1, "the one that arrives, in front");
    let lineas = pintar_en(&mut app, 120, 20);
    // The panel bar's names, in the suite's language.
    let lang = norte_i18n::active();
    let visor = norte_frontend::panelbar::label_in(lang, "viewer", "layout.preview");
    let detalles = norte_frontend::panelbar::label_in(lang, "metadata", "layout.metadata");
    // From the body: rows 0 and 1 are the menu and the panel bar, and the
    // strip also says "Visor" and "Detalles".
    let fila = lineas
        .iter()
        .enumerate()
        .skip(2)
        .find(|(_, l)| l.contains(&visor) && l.contains(&detalles))
        .map_or_else(|| panic!("a row with both tabs: {lineas:#?}"), |(i, _)| i);
    // The column of some text on the row: `TestBackend` puts the line in
    // quotes, so character 0 is the quote mark.
    let texto_de_fila: String = lineas[fila].chars().skip(1).collect();
    let col = |texto: &str| {
        let byte = texto_de_fila.find(texto).expect("it is there");
        u16::try_from(texto_de_fila[..byte].chars().count()).expect("it fits")
    };
    let fila = u16::try_from(fila).expect("it fits");
    // The hidden one (Visor) is clickable; the one in front is not.
    app.pending_panel_command = None;
    let _ = mouse::handle(&mut app, ev(ABAJO, col(&visor), fila));
    assert_eq!(app.pending_panel_command.as_deref(), Some("layout.preview"));
    app.pending_panel_command = None;
    let _ = mouse::handle(&mut app, ev(ABAJO, col(&detalles), fila));
    assert!(
        app.pending_panel_command.is_none(),
        "the one in front does not close with a click on its tab: {:?}",
        app.pending_panel_command
    );
    // And the mouse measures the content where it paints: below the strip.
    // With two counts, a click on the grouped disk map used to pick the
    // sibling next to it.
    let metadata = app.metadata_slot().expect("details open");
    let hueco = app.mouse.slot_rect(metadata).expect("placed");
    assert_eq!(hueco.y, fila + 1, "the content starts below the strip");
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
fn con_un_overlay_delante_la_barra_no_se_pulsa() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    // Any overlay from those that cover the row.
    app.open_theme_picker();
    let _ = pintar(&mut app);
    let antes = app.layout.clone();
    let _ = mouse::handle(&mut app, ev(ABAJO, 1, 1));
    assert!(
        app.pending_panel_command.is_none(),
        "a click on the overlay touched a bar button"
    );
    assert_eq!(antes, app.layout, "and the layout changed underneath");
}

/// The status bar belongs to no pane: outside the whole hit test, not "the
/// last row of the pane below".
#[test]
fn la_barra_de_estado_no_pertenece_a_ningun_pane() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, 5, H - 1).is_none());
}

/// The gap BELOW the last entry of a short listing is not the last entry.
/// It is the case that most surprises if resolved wrong: the natural bug
/// (saturating the index) makes a click in the empty space mark — or move
/// the cursor to — the directory's last file, which is exactly the one
/// nobody was looking at when they clicked there.
#[test]
fn el_hueco_bajo_la_ultima_entrada_no_es_la_ultima_entrada() {
    let mut app = app_pintada(3);
    for row in FILA0 + 3..FILA0 + FILAS {
        let hit = mouse::hit_test(&app, 5, row).expect("still inside the pane");
        assert_eq!(hit.index, None, "row {row}: empty, not an entry");
    }
    // And the real click does not move the cursor either.
    app.panes[0].set_cursor(1);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 6));
    assert_eq!(app.panes[0].cursor(), 1, "the cursor stays where it was");
}

#[test]
fn un_click_fuera_de_los_dos_panes_no_resuelve_nada() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, W - 1, H - 1).is_none(), "corner");
    assert!(
        mouse::hit_test(&app, 5, H + 5).is_none(),
        "outside the frame"
    );
}

#[test]
fn un_click_enfoca_ese_pane_y_mueve_el_cursor() {
    let mut app = app_pintada(5);
    assert_eq!(app.focus(), 0);
    let after = mouse::handle(&mut app, ev(ABAJO, 35, FILA0 + 2));
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
fn un_click_sobre_un_listado_desplazado_suma_el_scroll() {
    let mut app = app_pintada(40);
    app.panes[0].set_cursor(20);
    let lines = pintar(&mut app);
    let offset = app.mouse.geometry().expect("geometry")[0].offset;
    assert_eq!(
        offset,
        21 - usize::from(FILAS),
        "the cursor goes to the edge"
    );
    let hit = mouse::hit_test(&app, 5, FILA0).expect("inside the pane");
    assert_eq!(hit.index, Some(offset), "the first PAINTED row");
    // Against the buffer: the row that resolves is the one that shows.
    assert_fila(&lines, &app, FILA0, offset);
    let hit = mouse::hit_test(&app, 5, FILA0 + FILAS - 1).expect("inside the pane");
    assert_eq!(hit.index, Some(20), "the last painted one is the cursor");
    assert_fila(&lines, &app, FILA0 + FILAS - 1, 20);
}

/// The wheel scrolls the listing UNDER THE POINTER and does not touch
/// focus. Looking at one panel while working in the other is the normal
/// gesture with two panels; stealing focus from the active panel just by
/// passing the mouse over it would be a change of the next operation's
/// destination made without clicking anything.
#[test]
fn la_rueda_desplaza_el_pane_bajo_el_puntero_y_no_el_del_foco() {
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
fn la_rueda_sobre_el_visor_lo_desplaza_y_no_el_listado() {
    let mut app = app_pintada(40);
    let texto: Vec<u8> = (0..80)
        .flat_map(|i| format!("linea {i}\n").into_bytes())
        .collect();
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("file:///casa/alto.txt"),
        texto,
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
fn el_doble_click_pide_el_mismo_nav_enter_del_teclado() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 1), t0),
        After::Nothing,
        "the first one is a normal click"
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 1),
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
/// `nav.enter` on a file arms the launch. The wire itself — `despachar_clic`
/// launching what is armed — has no test because `on_mouse` needs a real
/// terminal; the fix is that all three mouse arms go through the SAME
/// function, which is what stops it from being forgotten again.
#[test]
fn nav_enter_sobre_un_fichero_deja_un_opener_armado() {
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
fn dos_clicks_lejanos_en_tiempo_o_en_fila_no_son_un_doble() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 1), t0);
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 1),
            t0 + std::time::Duration::from_secs(3)
        ),
        After::Nothing,
        "three seconds later is not a double click"
    );

    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 1), t0);
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 2),
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
fn un_click_con_modificador_no_es_la_primera_mitad_de_un_doble() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    let _ = mouse::handle_at(
        &mut app,
        ev_con(ABAJO, 5, FILA0 + 1, KeyModifiers::CONTROL),
        t0,
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 1),
            t0 + std::time::Duration::from_millis(50)
        ),
        After::Nothing
    );
}

#[test]
fn ctrl_click_marca_y_desmarca_la_fila_pulsada() {
    let mut app = app_pintada(5);
    let ctrl = KeyModifiers::CONTROL;
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, ctrl));
    assert_eq!(app.panes[0].marks_len(), 1);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, ctrl));
    assert_eq!(app.panes[0].marks_len(), 0, "the same gesture unmarks");
}

#[test]
fn un_arrastre_marca_lo_que_barre() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    assert_eq!(app.panes[0].marks_len(), 0, "the press does not mark yet");
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 4));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + 4));
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
fn un_drop_abre_el_mismo_modal_que_la_tecla_de_copiar() {
    let mut app = app_pintada(10);
    // Two marks by hand (with two, the gate opens the list confirm).
    for row in [1, 2] {
        let _ = mouse::handle(
            &mut app,
            ev_con(ABAJO, 5, FILA0 + row, KeyModifiers::CONTROL),
        );
    }
    assert_eq!(app.panes[0].marks_len(), 2);

    // What the KEYBOARD produces with this same selection.
    app.open_transfer(TransferKind::Copy, 0, 1, None);
    let por_teclado = app.modal.take().expect("F5 opens a modal");

    // And now the mouse: press ON a marked row and drop on the other one.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 35, FILA0 + 1));

    assert_eq!(
        app.modal.as_ref(),
        Some(&por_teclado),
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
fn mayus_al_soltar_decide_copiar_o_mover() {
    let arrastra = |mods: KeyModifiers| {
        let mut app = app_pintada(10);
        let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
        let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, KeyModifiers::CONTROL));
        // The press goes with NO Shift in both cases: only the release changes.
        let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev_con(ARRIBA, 35, FILA0 + 2, mods));
        match app.modal {
            Some(Modal::ConfirmTransfer { kind, .. }) => kind,
            otro => panic!("expected a transfer confirm: {otro:?}"),
        }
    };
    assert_eq!(arrastra(KeyModifiers::NONE), TransferKind::Copy);
    assert_eq!(arrastra(KeyModifiers::SHIFT), TransferKind::Move);
}

/// A drag born on an UNMARKED row that crosses to the other pane is
/// promoted to a transfer of THAT row — the most common drag in any file
/// manager — and returns the marks it swept along the way. What travels is
/// the pressed row, not the eleven marks the pane might have.
#[test]
fn un_arrastre_promovido_lleva_su_fila_y_devuelve_lo_que_barrio() {
    let mut app = app_pintada(10);
    // A previous mark, unrelated to the gesture.
    let _ = mouse::handle(
        &mut app,
        ev_con(ABAJO, 5, FILA0 + FILAS - 1, KeyModifiers::CONTROL),
    );
    assert_eq!(app.panes[0].marks_len(), 1);

    // Press on an UNMARKED row, sweeps along the way, and crosses to the
    // other pane.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 3));
    assert_eq!(app.panes[0].marks_len(), 4, "swept 1..=3 along the way");
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    assert_eq!(
        app.panes[0].marks_len(),
        1,
        "crossing returns what was swept: only the previous mark is left"
    );
    let _ = mouse::handle(&mut app, ev(ARRIBA, 35, FILA0 + 1));

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
fn soltar_en_el_panel_de_origen_no_somete_nada() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 2)); // wanders…
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + 6)); // …and comes back
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
fn un_arrastre_cancelado_restituye_las_marcas() {
    let mut app = app_pintada(10);
    for row in [5, 6] {
        let _ = mouse::handle(
            &mut app,
            ev_con(ABAJO, 5, FILA0 + row, KeyModifiers::CONTROL),
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
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, H - 1));

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
fn la_barra_anuncia_lo_que_haria_soltar_ahora() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, KeyModifiers::CONTROL));

    // Sweep at home: nothing to announce.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 8));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 9));
    assert_eq!(mouse::drop_hint(&app), None, "marking promises nothing");

    // Transfer still over its own pane: neither.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 5));
    assert_eq!(mouse::drop_hint(&app), None, "at home, dropping is a no-op");

    // Over the other pane: it says how many and that it COPIES…
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    let copia = mouse::drop_hint(&app).expect("there is a pending drop");
    assert!(copia.contains('2'), "the two marks: {copia}");
    // The destination with the SAME sanitizing as the pane's header (rule 1).
    let (dest, _) = norte_frontend::path_display_with(app.panes[1].dir(), None);
    assert_eq!(
        copia,
        norte_i18n::ta("drag-copy", &[("n", "2"), ("to", &dest)]),
    );
    // …and the bar PAINTS it (over any pending message).
    app.message = Some("un mensaje cualquiera".to_owned());
    let cabeza = copia
        .split_once("  ")
        .map_or(copia.as_str(), |(head, _)| head)
        .to_owned();
    let lineas = pintar(&mut app);
    let barra = lineas.last().expect("status bar");
    // The bar YIELDS on the right: its ELEMENTS — position, marks, encoding
    // (ADR 0132) — keep their part and the notice cuts wherever it must.
    // Requiring the WHOLE head tied this test to the language without saying
    // so: the Spanish sentence measures about forty cells and the English
    // one about thirty, so the same sixty-wide screen passed in English and
    // failed in Spanish. What is asserted here is not how much fits, but WHO
    // RULES: the notice starts the bar and the pending message does not show.
    let principio: String = cabeza.chars().take(15).collect();
    assert!(
        barra.contains(&principio),
        "the notice rules the bar while the drag lasts.\n\
         expected the bar to start with: {principio:?}\n\
         and the painted bar is:         {barra:?}"
    );
    assert!(
        !barra.contains("un mensaje cualquiera"),
        "and the pending message does not sneak in underneath: {barra:?}"
    );

    // With Shift, MOVE — and the drop does what was promised.
    let mut con_mayus = ev(ARRASTRE, 35, FILA0 + 2);
    con_mayus.modifiers = KeyModifiers::SHIFT;
    let _ = mouse::handle(&mut app, con_mayus);
    let mover = mouse::drop_hint(&app).expect("there is still a drop");
    assert_eq!(
        mover,
        norte_i18n::ta("drag-move", &[("n", "2"), ("to", &dest)]),
    );
    let _ = mouse::handle(&mut app, ev_con(ARRIBA, 35, FILA0 + 2, KeyModifiers::SHIFT));
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
fn marcar_bajo_un_filtro_no_alcanza_lo_que_el_filtro_esconde() {
    let dir = vp("file:///casa");
    // Alternating names: the `sí` filter leaves the EVEN indices visible, so
    // there is always a hidden one between two painted rows.
    let entradas: Vec<Entry> = ["a-si", "b-no", "c-si", "d-no", "e-si", "f-no"]
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
        Pane::new(dir.clone(), entradas),
        Pane::new(dir.clone(), Vec::new()),
    );
    app.panes[0].quick_start(norte_tui::nav::Mode::Filter);
    for c in "si".chars() {
        app.panes[0].quick_char(c);
    }
    // The REAL cursor stays far from the painted anchor on purpose.
    app.panes[0].set_cursor(5);
    let _ = pintar(&mut app);
    assert_eq!(
        app.panes[0].quick_visible(),
        Some(&[0, 2, 4][..]),
        "three rows painted out of six entries"
    );

    // shift+click on the THIRD painted row: range from the painted anchor
    // (the first) to it.
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::SHIFT));
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
fn un_click_limpio_cierra_el_quick_search_sobre_la_fila_pulsada() {
    let mut app = app_pintada(20);
    app.panes[0].quick_start(norte_tui::nav::Mode::Filter);
    app.panes[0].quick_char('f');
    let _ = pintar(&mut app);
    let expected = app.mouse.geometry().expect("geometry")[0].offset + 2;

    let hit = mouse::hit_test(&app, 5, FILA0 + 2).expect("inside the pane");
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
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
fn con_un_overlay_abierto_el_raton_no_toca_los_panes() {
    let mut app = app_pintada(5);
    app.modal = Some(norte_tui::app::Modal::ConfirmQuit);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 3));
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
fn un_release_que_se_comio_otro_pump_no_deja_el_gesto_armado() {
    let mut app = app_pintada(10);
    let dir = app.panes[0].dir().clone();
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 2));
    let marks = app.panes[0].marks_len();
    assert!(marks > 0, "the sweep was underway");

    // …the release falls inside a pump that only looks at keys: it never
    // arrives. What does happen is that pump refreshes the listing.
    app.panes[0].refresh_listing(entradas(&dir, 10));
    let _ = pintar(&mut app);

    let after = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + FILAS - 1));
    assert_eq!(
        app.panes[0].marks_len(),
        after,
        "the motion does not continue a sweep that no longer exists"
    );
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + FILAS - 1));
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
fn un_click_antes_y_otro_despues_de_un_cd_no_son_un_doble_click() {
    let mut app = app_pintada(10);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 2), t0),
        After::Nothing
    );

    // cd: the pane switches to another listing (the real path of `nav.enter`).
    let other = vp("file:///casa/subdir");
    app.panes[0].set_listing(other.clone(), entradas(&other, 10));
    let _ = pintar(&mut app);

    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 2),
            t0 + std::time::Duration::from_millis(80)
        ),
        After::Nothing,
        "same cell and 80 ms, but no longer the same row"
    );
}

/// A modal opened mid-drag also carries away the gesture: by the time the
/// user answers, the drag is history.
#[test]
fn un_modal_abierto_a_mitad_de_un_arrastre_se_lleva_el_gesto() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 2));
    let marks = app.panes[0].marks_len();

    app.modal = Some(norte_tui::app::Modal::ConfirmQuit);
    let _ = pintar(&mut app);
    app.modal = None;
    let _ = pintar(&mut app);

    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + FILAS - 1));
    assert_eq!(app.panes[0].marks_len(), marks, "dead gesture");
}

/// And what must NOT expire: a normal frame, with nothing moving, leaves
/// the gesture alive. Without this, expiry would be "always cancel", which
/// passes the two tests above and breaks every drag.
#[test]
fn un_frame_normal_no_caduca_el_gesto() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = pintar(&mut app);
    let _ = pintar(&mut app);
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 4));
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
fn un_intercambio_de_panes_a_mitad_de_un_arrastre_se_lleva_el_gesto() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 2));
    assert!(app.panes[0].marks_len() > 0, "the sweep was underway");
    assert_eq!(
        app.panes[0].listing_epoch(),
        app.panes[1].listing_epoch(),
        "the epochs TIE: that is exactly what blinds the epoch check"
    );

    app.swap_panes();
    let _ = pintar(&mut app);

    // The sweep's marks traveled with their pane to slot 1; pane 0 is now
    // the other listing, and the armed gesture still names `pane: 0`.
    let before = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + FILAS - 1));
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
fn la_captura_se_suelta_y_se_restaura_alrededor_del_suspend() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    cap.set(true, &mut out).expect("activate");
    assert!(cap.active());

    out.clear();
    let habia = mouse::release_for_suspend(&mut cap, &mut out).expect("release");
    assert!(habia, "it was set");
    assert!(
        !cap.active(),
        "with the terminal handed over, the capture is not ours"
    );
    assert!(
        !out.is_empty(),
        "the terminal was told, not just the struct"
    );

    mouse::restore_after_suspend(&mut cap, habia, &mut out).expect("restore");
    assert!(cap.active(), "on return, as it was");
}

/// And if it was NOT set (`[ui] mouse = false`), returning from the
/// external program does not turn it on: suspend restores the state, not a
/// default.
#[test]
fn el_suspend_no_enciende_una_captura_que_estaba_apagada() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    let habia = mouse::release_for_suspend(&mut cap, &mut out).expect("release");
    assert!(!habia);
    mouse::restore_after_suspend(&mut cap, habia, &mut out).expect("restore");
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
fn con_mouse_false_no_hay_captura_ni_manejo() {
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
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 3));
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
fn el_panel_de_diferencias_se_come_el_raton_como_cualquier_overlay() {
    let mut app = app_pintada(5);
    let dir_antes = [app.panes[0].dir().clone(), app.panes[1].dir().clone()];
    let cursor_antes = app.panes[0].cursor();

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
        mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 1), t0),
        After::Nothing
    );
    assert_eq!(
        mouse::handle_at(
            &mut app,
            ev(ABAJO, 5, FILA0 + 1),
            t0 + std::time::Duration::from_millis(120)
        ),
        After::Nothing,
        "a double click CANNOT request a nav.enter on a pane you cannot see"
    );
    assert_eq!(app.panes[0].cursor(), cursor_antes, "nor move its cursor");
    assert_eq!(
        [app.panes[0].dir().clone(), app.panes[1].dir().clone()],
        dir_antes
    );
    assert!(app.compare.is_some(), "and the panel is still where it was");
}

/// The sticky window, END-TO-END: `End` and then scrolling up to the top has
/// to leave the listing showing the start, not stuck where it was.
#[test]
fn subir_desde_el_final_acaba_arrastrando_la_ventana() {
    let mut app = app_pintada(60);
    app.panes[0].move_to_end();
    let _ = pintar(&mut app);
    let abajo = app.mouse.geometry().expect("geometry")[0].offset;
    assert!(abajo > 0, "the end scrolls the window: {abajo}");

    // Scroll up ONE row: the window does not move (the cursor goes inside).
    app.panes[0].move_up(1);
    let _ = pintar(&mut app);
    assert_eq!(
        app.mouse.geometry().expect("geometry")[0].offset,
        abajo,
        "scrolling up inside the window does not move it"
    );

    // And all the way to the top: the window ends at 0.
    for _ in 0..60 {
        app.panes[0].move_up(1);
    }
    let _ = pintar(&mut app);
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
fn arrastrar_el_borde_mueve_la_frontera_entre_los_panes() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    let antes = app.mouse.geometry().expect("geometry").to_vec();
    let (izq_antes, der_antes) = (antes[0].width, antes[1].width);
    let borde = antes[0].x + antes[0].width;

    // Grab the border and move it six cells to the left. Six and not
    // twenty: a `browser` declares a minimum of twenty columns, and below
    // that the layout COLLAPSES its split — the pane does not shrink, it
    // disappears. What this test measures is the drag, not the collapse.
    let destino = borde - 6;
    let _ = mouse::handle(&mut app, ev(ABAJO, borde, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, destino, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRIBA, destino, FILA0));
    let _ = pintar(&mut app);

    let ahora = app.mouse.geometry().expect("geometry").to_vec();
    assert!(
        ahora[0].width < izq_antes,
        "the left one shrinks: {izq_antes} -> {}",
        ahora[0].width
    );
    assert!(
        ahora[1].width > der_antes,
        "and what it loses the other gains: {der_antes} -> {}",
        ahora[1].width
    );
    assert_eq!(
        ahora[0].width + ahora[1].width,
        izq_antes + der_antes,
        "the pair occupies the same total: dragging a border does not touch the rest"
    );
}

/// And grabbing the border neither points at nor marks anything: grabbing
/// is not choosing.
#[test]
fn agarrar_el_borde_no_selecciona_una_fila() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    let cursor = app.panes[0].cursor();
    let geom = app.mouse.geometry().expect("geometry").to_vec();
    let borde = geom[0].x + geom[0].width;
    let _ = mouse::handle(&mut app, ev(ABAJO, borde, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRIBA, borde, FILA0 + 1));
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
fn un_click_en_un_listado_trae_el_teclado_desde_el_sidebar() {
    let mut app = app_pintada(5);
    app.toggle_places();
    let _ = pintar_en(&mut app, 100, 30);
    assert_eq!(app.key_owner(), KeyOwner::Places, "the sidebar took it");

    let g = app.mouse.geometry().expect("geometry")[0];
    let after = mouse::handle(&mut app, ev(ABAJO, g.x + 2, g.first_list_row));
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
fn un_click_en_un_panel_lateral_le_da_el_teclado() {
    let mut app = app_pintada(5);
    app.toggle_processes();
    app.return_keys_to_panes();
    let _ = pintar_en(&mut app, 100, 30);

    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let id = app.processes_slot().expect("open");
    let r = ui::resolved_for(&app, area)
        .placements
        .iter()
        .find(|(s, _)| *s == id)
        .map(|(_, r)| *r)
        .expect("the panel was placed");
    let _ = mouse::handle(&mut app, ev(ABAJO, r.x + 1, r.y + 1));
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
fn app_con_gestor(w: u16, h: u16) -> (App, Vec<String>) {
    let mut app = app_pintada(3);
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![
            plugin("org.norte.uno", "Uno", "columns"),
            plugin("org.norte.dos", "Dos", "previewer"),
        ],
        errors: Vec::new(),
        cursor: 0,
        foco: norte_tui::app::ExtFoco::Lista,
        config: None,
    });
    let lineas = pintar_en(&mut app, w, h);
    (app, lineas)
}

/// The row and column of the first occurrence of `texto` in what is
/// painted.
///
/// `TestBackend::to_string()` wraps each row in quotes: the screen's first
/// cell is the line's second character.
fn donde(lineas: &[String], texto: &str) -> (u16, u16) {
    for (y, l) in lineas.iter().enumerate() {
        let l = l.strip_prefix('"').unwrap_or(l);
        if let Some(byte) = l.find(texto) {
            let col = l[..byte].chars().count();
            return (
                u16::try_from(y).expect("row"),
                u16::try_from(col).expect("column"),
            );
        }
    }
    panic!("{texto:?} is not painted:\n{}", lineas.join("\n"));
}

/// The extension manager was born mute to the mouse: `overlay_open`
/// returned `Nothing` for everything. A click on a row selects it, and on
/// the ALREADY selected row opens its settings — what its footer promises
/// ("press Enter, or the row"). Checked against the PAINTED TEXT: the row
/// that gets clicked is the one showing "Dos".
#[test]
fn clic_en_una_fila_del_gestor_la_elige_y_repetirlo_abre_sus_ajustes() {
    let (mut app, lineas) = app_con_gestor(100, 24);
    let (row, col) = donde(&lineas, "Dos v1.0.0");
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, col, row)),
        After::Nothing,
        "selecting does not talk to the backend"
    );
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, col, row)),
        After::Extension("dialog.confirm"),
        "the already-selected row opens its settings via the SAME command as Enter"
    );
}

/// The card's buttons fire THE SAME command as their key, and come from
/// what is painted: the mouse finds "[Disable]" where the frame put it.
#[test]
fn los_botones_de_la_ficha_disparan_el_comando_de_su_tecla() {
    // The labels below are `en.ftl`'s, so the language is FIXED before
    // painting: without this the test reads the `LANG` of whoever runs it
    // and on a Spanish machine looks for "[Disable]" where "[Apagar]" was
    // painted. Same pattern as `render.rs`; nextest gives one process per
    // test.
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (mut app, lineas) = app_con_gestor(100, 24);
    for (etiqueta, cmd) in [
        ("[Disable]", "dialog.toggle-enabled"),
        ("[Revoke]", "dialog.approve"),
        ("[Settings]", "dialog.confirm"),
        ("[Uninstall]", "dialog.remove"),
    ] {
        let (row, col) = donde(&lineas, etiqueta);
        assert_eq!(
            mouse::handle(&mut app, ev(ABAJO, col + 1, row)),
            After::Extension(cmd),
            "{etiqueta}"
        );
    }
    // And between two buttons there is nothing: the gap is not a button.
    let (row, col) = donde(&lineas, "[Disable] [Revoke]");
    let hueco = col + u16::try_from("[Disable]".len()).unwrap();
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, hueco, row)),
        After::Nothing
    );
}

/// The wheel moves the list's cursor, and selecting ANOTHER row with the
/// previous one's settings open closes them: the card cannot show one
/// plugin and another one's settings.
#[test]
fn la_rueda_mueve_el_cursor_y_cambiar_de_fila_cierra_los_ajustes_ajenos() {
    let (mut app, lineas) = app_con_gestor(100, 24);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollDown, 50, 10));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 50, 10));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 0);
    app.extensions.as_mut().unwrap().config = Some(norte_tui::app::PluginConfigPanel {
        plugin_id: "org.norte.uno".into(),
        plugin_name: "Uno".into(),
        state: norte_frontend::plugin_config::PluginConfigState::new(Vec::new()),
    });
    let (row, col) = donde(&lineas, "Dos v1.0.0");
    let _ = mouse::handle(&mut app, ev(ABAJO, col, row));
    let mgr = app.extensions.as_ref().unwrap();
    assert_eq!(mgr.cursor, 1);
    assert!(mgr.config.is_none(), "the settings were \"Uno\"'s");
}

/// On a narrow terminal there is no card or buttons, but the usual list
/// rows are still clickable — with the description below, which is NOT a
/// row.
#[test]
fn en_estrecho_las_filas_se_pulsan_y_la_descripcion_no() {
    let (mut app, lineas) = app_con_gestor(W, H);
    let (row, col) = donde(&lineas, "Dos v1.0.0");
    let _ = mouse::handle(&mut app, ev(ABAJO, col, row));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    // The category header above is not a plugin.
    let (row, col) = donde(&lineas, "previewer");
    let _ = mouse::handle(&mut app, ev(ABAJO, col, row));
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
fn la_rueda_baja_por_la_ayuda_y_un_clic_elige_pagina() {
    let (w, h) = (100u16, 30u16);
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    let mut app = app_pintada(3);
    norte_tui::overlays::open_help_topic(&mut app, norte_help::Lang::Es, &[], "copying");
    let refrescar = |app: &mut App| {
        let (ancho, alto) = ui::help_body_size(area, norte_help::Lang::Es);
        app.refresh_help(ancho, alto);
        let _ = pintar_en(app, w, h);
    };
    refrescar(&mut app);
    let z = ui::help_zones(&app, area).expect("help is open");

    // The wheel over the BODY scrolls it.
    let antes = app.help.as_ref().expect("open").state.body_scroll();
    let _ = mouse::handle(
        &mut app,
        ev(MouseEventKind::ScrollDown, z.body.x + 2, z.body.y + 2),
    );
    refrescar(&mut app);
    let despues = app.help.as_ref().expect("open").state.body_scroll();
    assert!(
        despues > antes,
        "the wheel scrolled the text down: {antes} → {despues}"
    );

    // A click on a VISIBLE index page that is not the open one selects it.
    let z = ui::help_zones(&app, area).expect("help is open");
    let cursor = app.help.as_ref().expect("open").state.cursor();
    let &(fila, modelo) = z
        .rows
        .iter()
        .find(|(_, m)| *m != cursor)
        .expect("there is another page in view");
    let _ = mouse::handle(&mut app, ev(ABAJO, z.sidebar.x + 3, fila));
    assert_eq!(
        app.help.as_ref().expect("open").state.cursor(),
        modelo,
        "the click selected THAT row's page"
    );

    // And none of this reaches the panes below.
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, 0, 0)),
        After::Nothing,
        "with help in front, a click outside it does nothing"
    );
}
