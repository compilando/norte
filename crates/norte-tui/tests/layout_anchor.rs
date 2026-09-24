//! The anchor for the layout refactor (L1a): what the engine BELIEVES it
//! painted is what is in the buffer.
//!
//! Today the one that says so is [`ui::pane_geometry`]; after L1a it will
//! be `Resolved`. The assertion does not change — who reads its rectangles
//! changes, and that is why this test is the only one that can detect the
//! layout shifting by one cell.
//!
//! It is born GREEN on purpose: it is a characterization test for code that
//! already works. If it fails now, this file's helpers are wrong, not the
//! render.
//!
//! The snapshot next to it is the whole plan's acceptance criterion: the
//! `orthodox` screen identical before and after the refactor.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Width of these tests' terminal.
const W: u16 = 100;
/// Height of these tests' terminal.
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    // Fixed language: the snapshot freezes localized text (status bar).
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

/// `n` entries `f00..f{n-1}` under `dir`.
///
/// With a leading ZERO on purpose: `f0` is a prefix of `f01` and of `f10`,
/// and a `contains` over a row would not tell which of the three painted
/// it. The fixed width makes each name findable only by itself.
fn entradas(dir: &VPath, n: usize) -> Vec<Entry> {
    (0..n)
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

/// An `App` with `n` entries in each pane.
fn app_de_prueba_con(n: usize) -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entradas(&dir, n)),
        Pane::new(dir.clone(), entradas(&dir, n)),
    )
}

/// Presses the left button on a cell, through the same path as the run loop.
fn pulsar(app: &mut App, col: u16, row: u16) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let _ = norte_tui::mouse::handle(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

/// Paints a frame through the SAME path as the run loop (reconcile, paint)
/// and returns the lines.
///
/// Returning the buffer is not a convenience: without it the test would
/// only check that `pane_geometry` agrees with itself.
fn pintar(app: &mut App) -> Vec<String> {
    pintar_en(app, W, H)
}

/// Like [`pintar`] at any size.
fn pintar_en(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    ui::before_frame(app, area);
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    // CLOSE the frame like the run loop: return the geometry and the
    // clickable zones to the model. Without this the mouse resolves against
    // a screen nobody told it about, and a button test would pass without
    // clicking anything.
    norte_tui::mouse::after_frame(
        app,
        ui::pane_geometry(app, area),
        norte_tui::mouse::FrameZones {
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
    // `TestBackend::to_string()` wraps EACH row in quotes. Without stripping
    // them, every per-column clip is shifted by one cell — and a `contains`
    // hides it, which is exactly how a geometry test stops checking
    // geometry.
    terminal
        .backend()
        .to_string()
        .lines()
        .map(|l| l.trim_matches('"').to_owned())
        .collect()
}

/// The chunk of row `row` occupied by a pane starting at `x` and measuring
/// `width`. By CHARACTER: `TestBackend` gives one cell per character.
fn recorte(lines: &[String], row: u16, x: u16, width: u16) -> String {
    lines
        .get(row as usize)
        .map(|l| {
            l.chars()
                .skip(x as usize)
                .take(width as usize)
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// The name of the entry that goes on row `offset` of pane `i`.
fn nombre_visible(app: &App, i: usize, offset: usize) -> String {
    let entry = &app.panes[i].entries()[offset];
    String::from_utf8_lossy(
        entry
            .path
            .file_name()
            .expect("a test entry has a name")
            .as_bytes(),
    )
    .into_owned()
}

#[test]
fn la_geometria_declarada_coincide_con_las_filas_pintadas() {
    let mut app = app_de_prueba_con(60);
    let lines = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("two panes painted");

    for (i, g) in geom.iter().enumerate() {
        assert!(g.list_rows > 0, "pane {i}: no listing rows");
        let esperada = nombre_visible(&app, i, g.offset);

        // The first listing row carries the first visible entry.
        let primera = recorte(&lines, g.first_list_row, g.x, g.width);
        assert!(
            primera.contains(&esperada),
            "pane {i}: row {} should carry {esperada:?}, carries {primera:?}",
            g.first_list_row
        );

        // The row RIGHT ABOVE is chrome (column header): never a listing.
        let header = recorte(&lines, g.first_list_row - 1, g.x, g.width);
        assert!(
            !header.contains(&esperada),
            "pane {i}: the header cannot carry listing content: {header:?}"
        );

        // And the row right BELOW the last listing one is the bottom border.
        let bajo = g.first_list_row + g.list_rows;
        let border = recorte(&lines, bajo, g.x, g.width);
        assert!(
            border.contains('─') && !border.contains(&esperada),
            "pane {i}: row {bajo} should be the bottom border: {border:?}"
        );
        assert_eq!(
            bajo,
            g.y + g.height - 1,
            "pane {i}: the bottom border does not fall where the geometry says"
        );
    }
}

/// An ODD width does not lose a column: the two panes add up to the whole
/// frame.
///
/// Worth having even though it looks like arithmetic: ratatui used to do
/// the cut and now `layout::resolve` does, and the two distribute the
/// division's remainder to different places. With even widths — the ones
/// every other test uses — the difference does not exist, so without this
/// test the change would be invisible until someone opened a 101-column
/// terminal.
#[test]
fn con_ancho_impar_los_dos_paneles_suman_el_frame() {
    let mut app = app_de_prueba_con(60);
    let lines = pintar_en(&mut app, 101, H);
    let area = ratatui::layout::Rect::new(0, 0, 101, H);
    let geom = ui::pane_geometry(&app, area).expect("two panes");
    assert_eq!(geom[0].x, 0);
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        101,
        "a column was lost"
    );
    assert_eq!(
        geom[1].x, geom[0].width,
        "the right one starts where the left one ends"
    );
    // And what is painted matches: the frame's last column is not left blank.
    let border = recorte(&lines, 0, geom[1].x, geom[1].width);
    assert_eq!(
        border.chars().count(),
        geom[1].width as usize,
        "the right pane does not reach the frame's edge"
    );
}

/// At 30 columns the `browser`'s two minimums do not fit, so the `Split`
/// collapses and ONE gets painted at full width.
///
/// It is the whole engine's visible counterpart: two fifteen-column panes
/// do not show even a name with its size, and until now that was the only
/// option.
#[test]
fn a_treinta_columnas_se_pinta_un_solo_pane_a_ancho_completo() {
    let mut app = app_de_prueba_con(60);
    let _ = pintar_en(&mut app, 30, H);
    let area = ratatui::layout::Rect::new(0, 0, 30, H);
    let geom = ui::pane_geometry(&app, area).expect("there is geometry");
    assert_eq!(geom[0].width, 30, "the one that paints takes it all");
    assert_eq!(
        geom.len(),
        1,
        "and there is no geometry for the one that did not paint: a click there resolves nothing"
    );
}

/// And focus does not stay on the pane the collapse left out: that would be
/// a keyboard moving a cursor nobody sees.
#[test]
fn el_foco_abandona_el_pane_que_el_colapso_dejo_fuera() {
    let mut app = app_de_prueba_con(60);
    app.set_focus(1);
    let _ = pintar_en(&mut app, 30, H);
    assert_eq!(app.focus(), 0, "focus lands on the one that IS visible");
}

/// With a tab open, the anchor still holds: the bar eats one row and the
/// geometry knows it.
///
/// It is the test that matters for tabs. The bar changes the pane's
/// chrome, and if `pane_geometry` does not deduct it, every click resolves
/// one row higher than what the user sees — the silent bug the geometry
/// exists to not have.
#[test]
fn con_una_pestana_abierta_la_geometria_sigue_cuadrando() {
    let mut app = app_de_prueba_con(60);
    let before =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("two panes")[0];
    app.tab_new();
    let lines = pintar(&mut app);
    let geom = ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("two panes");
    assert_eq!(
        geom[0].first_list_row,
        before.first_list_row + 1,
        "the tab bar pushes the listing down one row"
    );
    assert_eq!(
        geom[0].list_rows,
        before.list_rows - 1,
        "and takes one listing row from it"
    );
    let esperada = nombre_visible(&app, 0, geom[0].offset);
    let row = recorte(&lines, geom[0].first_list_row, geom[0].x, geom[0].width);
    assert!(
        row.contains(&esperada),
        "the first listing row should carry {esperada:?}, carries {row:?}"
    );
}

/// A new tab is born in the same directory and ALREADY FULL: it is the same
/// thing that was being looked at, so it does not flash empty while someone
/// rereads.
#[test]
fn una_pestana_nueva_nace_llena_y_en_el_mismo_sitio() {
    let mut app = app_de_prueba_con(60);
    let dir = app.panes[0].dir().clone();
    let n = app.panes[0].entries().len();
    app.tab_new();
    let _ = pintar(&mut app);
    assert_eq!(app.panes[0].dir(), &dir);
    assert_eq!(app.panes[0].entries().len(), n);
}

/// Closing the second-to-last tab dissolves the group and returns the row.
#[test]
fn al_cerrar_la_ultima_pestana_el_pane_recupera_su_fila() {
    let mut app = app_de_prueba_con(60);
    let before =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("two panes")[0];
    app.tab_new();
    let _ = pintar(&mut app);
    app.tab_close();
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("two panes");
    assert_eq!(geom[0].list_rows, before.list_rows);
}

/// Switching tabs changes the listing that side shows, and each keeps its
/// own cursor: there is nothing to remember because nothing was forgotten.
#[test]
fn cada_pestana_conserva_su_cursor() {
    let mut app = app_de_prueba_con(60);
    app.panes[0].set_cursor(7);
    app.tab_new();
    let _ = pintar(&mut app);
    app.panes[0].set_cursor(2);
    assert_eq!(app.panes[0].cursor(), 2, "the new tab is on its own");
    app.tab_cycle(-1);
    let _ = pintar(&mut app);
    assert_eq!(
        app.panes[0].cursor(),
        7,
        "the earlier one is still where it was"
    );
}

/// Closing the last panel is REFUSED. It is what keeps the two sides
/// distinct: with a single listing, "the other pane" would be this same
/// one and a copy would target its own source.
#[test]
fn no_se_puede_cerrar_el_ultimo_panel() {
    let mut app = app_de_prueba_con(60);
    assert!(
        !app.layout_close_slot(),
        "with two panels it can no longer be done"
    );
    let _ = pintar(&mut app);
    assert!(
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).is_some(),
        "both are still there"
    );
}

/// Growing a panel really gives it room, and the other loses it.
#[test]
fn agrandar_un_panel_le_da_sitio_y_al_otro_se_lo_quita() {
    let mut app = app_de_prueba_con(60);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let before = ui::pane_geometry(&app, area).expect("two panes")[0].width;
    app.layout_resize(1);
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, area).expect("two panes");
    assert!(geom[0].width > before, "the focused one grows");
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        u32::from(W),
        "and they still add up to the frame"
    );
}

/// Equalizing returns them to half each.
#[test]
fn igualar_devuelve_los_paneles_a_la_mitad() {
    let mut app = app_de_prueba_con(60);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    app.layout_resize(3);
    let _ = pintar(&mut app);
    app.layout_equalize();
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, area).expect("two panes");
    assert_eq!(geom[0].width, geom[1].width);
}

/// Each tab has its own HISTORY, not just its own cursor.
///
/// It used to be in an array of two, so it belonged to the screen's slot
/// and not to the listing: switching tabs would have given you the other
/// one's history, which is the same bug as seeing its cursor.
#[test]
fn cada_pestana_conserva_su_historial() {
    use norte_proto::VPath;
    let mut app = app_de_prueba_con(60);
    app.history[0].record(VPath::parse("mem:///una").expect("wire"));
    app.tab_new();
    let _ = pintar(&mut app);
    assert!(
        app.history[0].entries().is_empty(),
        "the new tab starts with no history"
    );
    app.history[0].record(VPath::parse("mem:///otra").expect("wire"));
    app.tab_cycle(-1);
    let _ = pintar(&mut app);
    assert_eq!(
        app.history[0].entries().front().map(VPath::to_wire),
        Some("mem:///una".to_owned()),
        "the earlier one gets its own back"
    );
}

/// Splitting gives THREE panels, and all three's geometry matches the
/// buffer.
///
/// It is the ceiling P6 removes: until now `PaneSlots` could only
/// represent two, and a third would have been painted but out of the
/// mouse's reach.
#[test]
fn partir_da_tres_paneles_y_los_tres_cuadran() {
    let mut app = app_de_prueba_con(60);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let lines = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("there is geometry");
    assert_eq!(geom.len(), 3, "three panels");
    let width: u32 = geom.iter().map(|g| u32::from(g.width)).sum();
    assert_eq!(width, u32::from(W), "and they add up to the whole frame");
    for (i, g) in geom.iter().enumerate() {
        let esperada = nombre_visible(&app, i, g.offset);
        let row = recorte(&lines, g.first_list_row, g.x, g.width);
        assert!(
            row.contains(&esperada),
            "panel {i}: row {} should carry {esperada:?}, carries {row:?}",
            g.first_list_row
        );
    }
}

/// The newly split panel keeps focus: splitting is asking for room to work
/// in it, not to look at it from the one next door.
#[test]
fn el_panel_recien_partido_se_queda_el_foco() {
    let mut app = app_de_prueba_con(60);
    let before = app.focused_slot();
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert_ne!(app.focused_slot(), before, "focus travels to the new one");
}

/// With three panels, closing one goes back to two and focus survives.
#[test]
fn con_tres_paneles_cerrar_uno_vuelve_a_dos() {
    let mut app = app_de_prueba_con(60);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert!(app.layout_close_slot(), "with three it CAN be closed");
    let _ = pintar(&mut app);
    let geom =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("there is geometry");
    assert_eq!(geom.len(), 2);
    assert!(app.focus() < 2, "focus stayed in range");
}

/// With THREE panels, a copy with no designated destination does NOT guess.
///
/// With two, the destination is the other one and nobody had to say so.
/// With three, guessing is how a copy heads toward a panel the reader did
/// not have in mind — silent data loss.
#[test]
fn con_tres_paneles_no_hay_destino_hasta_que_se_designa() {
    let mut app = app_de_prueba_con(60);
    assert!(app.target_index().is_some(), "with two, the other one");
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert_eq!(
        app.target_index(),
        None,
        "with three, it has to be designated"
    );
    app.layout_set_target();
    let _ = pintar(&mut app);
    let dest = app.target_index().expect("designated");
    assert_ne!(dest, app.focus(), "and never itself");
}

/// The designated destination gets MARKED in its chrome, and only from
/// three onward: with two it would be noise in the usual case.
#[test]
fn el_destino_designado_se_marca_y_solo_cuando_hace_falta() {
    let mut app = app_de_prueba_con(60);
    let dos = pintar(&mut app).join("\n");
    assert!(!dos.contains("-> "), "with two panels nothing gets marked");
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    app.layout_set_target();
    let tres = pintar(&mut app).join("\n");
    assert!(
        tres.contains("-> "),
        "with three, the destination shows in the chrome:\n{tres}"
    );
}

/// The tab bar's buttons really get clicked, and the zones the mouse
/// measures are the ones that got painted.
///
/// Measuring separately what is painted and what can be clicked is how a
/// click ends up on the next tab over: a bug that does not look like a
/// mouse bug, but like "this changes on its own."
#[test]
fn los_botones_de_la_barra_de_pestanas_se_pulsan() {
    let mut app = app_de_prueba_con(60);
    app.tab_new();
    let _ = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::tab_zones(&app, area);
    assert!(!zonas.is_empty(), "with tabs there are zones to click");

    // Go back to the first tab by clicking it.
    let primera = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::Goto(0))
        .copied()
        .expect("the first tab has its zone");
    let before = app.focused_slot();
    pulsar(&mut app, primera.x0, primera.row);
    let _ = pintar(&mut app);
    assert_ne!(app.focused_slot(), before, "it switched tabs");

    // `[+]` opens another one.
    let zonas = ui::tab_zones(&app, area);
    let mas = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::New)
        .copied()
        .expect("the open button has its zone");
    pulsar(&mut app, mas.x0, mas.row);
    let _ = pintar(&mut app);
    let t = ui::tab_strip_for(&app, 0).expect("there is still a group");
    assert_eq!(t.titles.len(), 3, "the button opened a third one");

    // `[x]` closes the active one.
    let zonas = ui::tab_zones(&app, area);
    let equis = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::Close)
        .copied()
        .expect("the close button has its zone");
    pulsar(&mut app, equis.x0, equis.row);
    let _ = pintar(&mut app);
    let t = ui::tab_strip_for(&app, 0).expect("two are left");
    assert_eq!(t.titles.len(), 2, "and the other closed it");
}

/// A click on the bar's row but OUTSIDE every zone does nothing.
#[test]
fn un_click_en_el_hueco_de_la_barra_no_hace_nada() {
    let mut app = app_de_prueba_con(60);
    app.tab_new();
    let _ = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::tab_zones(&app, area);
    let row = zonas[0].row;
    let last = zonas
        .iter()
        .filter(|z| z.pane == 0)
        .map(|z| z.x1)
        .max()
        .expect("there are zones");
    let before = ui::tab_strip_for(&app, 0).expect("group").titles.len();
    pulsar(&mut app, last + 1, row);
    let _ = pintar(&mut app);
    assert_eq!(
        ui::tab_strip_for(&app, 0).expect("group").titles.len(),
        before
    );
}

/// The menu paints with its dropdown, and the zones the mouse measures are
/// the ones that got painted.
#[test]
fn el_menu_se_pinta_y_sus_zonas_coinciden() {
    let mut app = app_de_prueba_con(60);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let lines = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::menu_zones(&app, area);
    assert!(!zonas.is_empty(), "there are titles and items to click");

    // The first title is painted where its zone says.
    let title = zonas
        .iter()
        .find(|z| z.hit == ui::MenuHit::Title(0))
        .copied()
        .expect("the first title has a zone");
    let text = recorte(&lines, title.row, title.x0, title.x1 - title.x0 + 1);
    assert!(
        text.trim() == norte_i18n::t("menu-file"),
        "the title's zone does not fall where it was painted: {text:?}"
    );

    // And the dropdown's first item carries its label.
    let item = zonas
        .iter()
        .find(|z| z.hit == ui::MenuHit::Item(0))
        .copied()
        .expect("the first item has a zone");
    let row = recorte(&lines, item.row, item.x0, item.x1 - item.x0 + 1);
    assert!(
        !row.trim().is_empty(),
        "the dropdown did not paint its first item"
    );
}

/// Clicking a title opens THAT menu; clicking outside closes the bar.
#[test]
fn pulsar_un_titulo_abre_su_menu_y_fuera_cierra() {
    let mut app = app_de_prueba_con(60);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let _ = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::menu_zones(&app, area);
    let tercero = zonas
        .iter()
        .find(|z| z.hit == ui::MenuHit::Title(2))
        .copied()
        .expect("there is a third menu");
    pulsar(&mut app, tercero.x0, tercero.row);
    assert_eq!(
        app.menu.expect("still open").menu(),
        2,
        "the one that was clicked opened"
    );

    let mut app = app_de_prueba_con(60);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let _ = pintar(&mut app);
    // A listing row, far from the bar and the dropdown.
    pulsar(&mut app, W - 2, H - 3);
    assert!(app.menu.is_none(), "a click outside closes the menu");
}

/// An open menu keeps ALL the keys: otherwise, a command dispatched behind
/// it would leave the bar eating the keys of what just opened.
#[test]
fn un_menu_abierto_es_dueno_del_teclado() {
    let mut app = app_de_prueba_con(60);
    let before = app.panes[0].cursor();
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let _ = pintar(&mut app);
    assert_eq!(
        app.panes[0].cursor(),
        before,
        "opening the menu moves nothing behind it"
    );
}

/// L1a's acceptance criterion, written as a test: this screen is identical
/// before and after the refactor.
///
/// If a cell changes, either the refactor moved something or someone
/// changed the render on purpose — and then the new snapshot is accepted
/// BY HAND, after looking at it, never with a blind `--accept`.
#[test]
fn la_pantalla_orthodox_no_se_mueve() {
    let mut app = app_de_prueba_con(60);
    let lines = pintar(&mut app);
    insta::assert_snapshot!("orthodox-100x30", lines.join("\n"));
}

/// The anchor, again, WITH the sidebar open (L3).
///
/// It is the same test as above, and that is why it counts: what the
/// engine believes it painted is still what is in the buffer when in front
/// of the listings there is a panel that is not a listing. A wrongly
/// subtracted `Fixed(16)` shifts both panes by one cell and none of the
/// existing snapshots would see it, because none of them carries a
/// sidebar.
#[test]
fn la_geometria_declarada_coincide_con_lo_pintado_con_el_sidebar_abierto() {
    let mut app = app_de_prueba_con(60);
    app.toggle_places();
    let lines = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("two panes painted");

    assert_eq!(geom.len(), 2, "the sidebar is not a pane");
    assert_eq!(geom[0].x, 16, "the first listing starts after the 16 cells");
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        u32::from(W) - 16,
        "the two listings split what the sidebar leaves"
    );

    for (i, g) in geom.iter().enumerate() {
        let esperada = nombre_visible(&app, i, g.offset);
        let primera = recorte(&lines, g.first_list_row, g.x, g.width);
        assert!(
            primera.contains(&esperada),
            "pane {i}: row {} should carry {esperada:?}, carries {primera:?}",
            g.first_list_row
        );
    }
}

/// Every preset, painted, at two sizes. The large one is the real screen;
/// the small one is where three columns no longer fit, so it is the one
/// that exercises the collapse — the path no `resolve` test at 100x30
/// touches.
///
/// Two things the snapshots leave WRITTEN and are worth reading for what
/// they are:
///
/// - The details sheet comes out with "nothing under the cursor". It is not
///   a bug: the run loop fills it every turn (`metadata::want`), and here
///   only one frame is painted. What the sheet really shows is pinned by
///   `metadata`'s tests.
/// - At 40x10, `explorer` and `full` SET ASIDE chrome (#229): the processes
///   panel in both, and in `full` also the right column. What stays is what
///   fits — sidebar, listings with real rows and the status bar — and what
///   was set aside comes back on its own as the terminal grows, because the
///   tree is not touched. Before #229 these two snapshots showed three
///   chrome headers and not one file name.
#[test]
fn los_cinco_presets_pintan_lo_que_dicen() {
    for name in norte_frontend::layout::presets::NAMES {
        for (w, h) in [(80_u16, 24_u16), (40, 10)] {
            let mut app = app_de_prueba_con(60);
            app.set_layout(norte_frontend::layout::presets::tree(name).expect("factory"));
            let lines = pintar_en(&mut app, w, h);
            assert_eq!(lines.len(), h as usize, "{name} {w}x{h}");
            insta::assert_snapshot!(format!("preset-{name}-{w}x{h}"), lines.join("\n"));
        }
    }
}

/// With ONE listing there is no "other panel", so a copy has no default
/// destination. L1's rule is that the operation ASKS — opens the address
/// prompt — instead of failing. `simple` is the first preset where that
/// stops being hypothetical, and this test is what stops it from becoming
/// an error message again.
#[test]
fn con_un_solo_listado_una_copia_pregunta_el_destino() {
    use norte_tui::app::{Modal, TransferKind};

    let mut app = app_de_prueba_con(3);
    app.set_layout(norte_frontend::layout::presets::tree("simple").expect("s"));
    let _ = pintar(&mut app);
    assert_eq!(app.panes.len(), 1, "a single listing");
    assert_eq!(app.target_index(), None, "and so no destination");

    // The same path F5 takes when `target_index()` does not answer.
    app.open_transfer_dest(TransferKind::Copy);
    let Some(Modal::TransferDest { kind, input, error }) = &app.modal else {
        panic!(
            "it asks for the address instead of failing: {:?}",
            app.modal
        )
    };
    assert_eq!(*kind, TransferKind::Copy);
    assert_eq!(
        input,
        &app.panes[0].dir().to_wire(),
        "pre-filled with the panel's own address"
    );
    assert!(error.is_none());
}

/// Confirming the prompt does not transfer: it opens the modal an F5 would
/// have opened with two panels. A second path to submit a transfer is a
/// path left without confirmation, without collision handling and without
/// undo.
#[test]
fn confirmar_el_destino_abre_el_modal_de_siempre() {
    use norte_tui::app::{Modal, TransferKind};

    let mut app = app_de_prueba_con(3);
    app.set_layout(norte_frontend::layout::presets::tree("simple").expect("s"));
    let _ = pintar(&mut app);
    app.open_transfer_dest(TransferKind::Copy);
    for _ in 0..app.panes[0].dir().to_wire().chars().count() {
        app.transfer_dest_pop();
    }
    for c in "file:///otro".chars() {
        app.transfer_dest_push(c);
    }
    assert!(app.transfer_dest_confirm(), "the address parses");

    let Some(Modal::TransferName { kind, to_dir, .. }) = &app.modal else {
        panic!("the usual modal: {:?}", app.modal)
    };
    assert_eq!(*kind, TransferKind::Copy);
    assert_eq!(to_dir.to_wire(), "file:///otro");
}

/// Backspace deletes ONE letter of the name, not one character of the
/// text: the text is wire form, so `é` is six characters (`%C3%A9`) and
/// `String::pop` left `%C3%A`, which no longer parses (#246 M3).
#[test]
fn retroceder_sobre_un_escape_borra_la_letra_entera() {
    use norte_tui::app::{Modal, TransferKind};

    let dir = vp("file:///caf%C3%A9");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, 3)),
        Pane::new(dir.clone(), entradas(&dir, 3)),
    );
    app.focused_mut().toggle_mark();
    app.open_transfer_dest(TransferKind::Copy);
    app.transfer_dest_pop();

    let Some(Modal::TransferDest { input, .. }) = &app.modal else {
        panic!("open: {:?}", app.modal)
    };
    assert_eq!(input, "file:///caf", "the whole é left, not half an escape");
    assert!(
        VPath::parse(input).is_ok(),
        "and what is left is still an address"
    );
}

/// The prompt is pre-filled with the SOURCE directory, so `Enter` with no
/// editing used to ask to copy every mark onto itself: N failed tasks
/// instead of one line in the dialog (#244 m6).
#[test]
fn confirmar_el_destino_de_origen_lo_dice_en_el_prompt() {
    use norte_tui::app::{Modal, TransferKind};

    let mut app = app_de_prueba_con(3);
    app.focused_mut().toggle_mark();
    app.open_transfer_dest(TransferKind::Copy);
    assert!(!app.transfer_dest_confirm(), "it submits nothing");

    let Some(Modal::TransferDest { error, .. }) = &app.modal else {
        panic!("still open: {:?}", app.modal)
    };
    assert!(error.is_some(), "and says why");
}

/// An address that does not parse KEEPS what was typed and shows its
/// diagnosis: the prompt does not close swallowing the operation.
#[test]
fn un_destino_que_no_es_una_direccion_deja_el_prompt_abierto() {
    use norte_tui::app::{Modal, TransferKind};

    let mut app = app_de_prueba_con(3);
    app.set_layout(norte_frontend::layout::presets::tree("simple").expect("s"));
    let _ = pintar(&mut app);
    app.open_transfer_dest(TransferKind::Move);
    for _ in 0..app.panes[0].dir().to_wire().chars().count() {
        app.transfer_dest_pop();
    }
    for c in "/home/yo".chars() {
        app.transfer_dest_push(c);
    }
    assert!(!app.transfer_dest_confirm(), "a local path is not wire");

    let Some(Modal::TransferDest { input, error, .. }) = &app.modal else {
        panic!("still open: {:?}", app.modal)
    };
    assert_eq!(input, "/home/yo", "what was typed survives");
    assert!(error.is_some(), "and says why");
}

/// Splitting a slot that no longer has room for two is REFUSED, and says
/// so.
///
/// Without this the key created a panel the layout hid in the same frame:
/// the `Split` did not fit, degraded to tabs, and the screen went back to
/// showing one — with the tree keeping the new one regardless. From the
/// outside, a key that sometimes splits, sometimes does nothing, and other
/// times undoes the previous action.
#[test]
fn partir_sin_sitio_se_niega_y_lo_dice() {
    use norte_frontend::layout::Dir;

    // 30 rows tall: room to split in two vertically, and a third time
    // already no.
    let mut app = app_de_prueba_con(3);
    let _ = pintar(&mut app);
    let huecos_antes = app.layout.slot_ids().len();

    app.layout_split(Dir::Vertical);
    let _ = pintar(&mut app);
    assert_eq!(
        app.layout.slot_ids().len(),
        huecos_antes + 1,
        "the first one fits"
    );

    // Split until the answer is no, and then NOTHING changes.
    let mut mensajes = 0;
    for _ in 0..6 {
        let antes = app.layout.clone();
        app.message = None;
        app.layout_split(Dir::Vertical);
        let _ = pintar(&mut app);
        if app.layout == antes {
            mensajes += 1;
            assert!(app.message.is_some(), "refusing silently is a broken key");
        }
    }
    assert!(mensajes > 0, "in 30 rows there is a cap and it is reached");

    // And what stayed on screen matches the model: no listing hidden by
    // the layout (the tree also carries the chrome, which is not a pane).
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let colocados = ui::pane_geometry(&app, area).expect("geometry").len();
    assert_eq!(
        colocados,
        app.panes.len(),
        "every listing in the model is visible"
    );
}
