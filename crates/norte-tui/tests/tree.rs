//! El árbol de directorios (#136) como PANEL: el ratón y la barra de menús.
//!
//! Lo que estos tests protegen es que un panel con teclado siga siendo parte
//! de la aplicación: se puede pulsar con el ratón, y las teclas del cromo
//! —la barra de menús— no se mueren por estar dentro de él.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entradas(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segmento"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

/// `App` con el árbol abierto sobre `file:///casa` y dos ramas ya leídas,
/// una de ellas con un hijo.
fn app_con_arbol() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir)),
        Pane::new(dir.clone(), entradas(&dir)),
    );
    app.toggle_tree();
    let t = app.tree_mut().expect("árbol abierto");
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

/// Pinta un frame y devuelve al modelo la geometría de ese frame, que es
/// contra la que el ratón resuelve.
fn tras_pintar(app: &mut App, area: ratatui::layout::Rect) -> Vec<norte_tui::ui::TreeZone> {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    let (geo, tabs, menus, sitios, ramas) = (
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
            tree: ramas.clone(),
            session: None,
            slots: huecos,
            ..Default::default()
        },
    );
    ramas
}

/// El ratón sobre el árbol: pulsar una fila la selecciona y trae el teclado;
/// pulsarla otra vez la ACTIVA, que es lo mismo que `Enter`.
///
/// El árbol se envió con teclado y nada más: sus celdas no son de ningún
/// listado, así que un click ahí caía en «fuera de los panes» y no hacía nada
/// — un panel que se pinta y no se puede tocar.
#[test]
fn pulsar_una_fila_del_arbol_la_selecciona_y_repulsarla_la_activa() {
    let mut app = app_con_arbol();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let zonas = tras_pintar(&mut app, area);
    assert!(!zonas.is_empty(), "el árbol tiene filas pulsables");
    let rama = zonas
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("la primera rama se ve");

    app.return_keys_to_panes();
    let after = pulsar_en(&mut app, rama.x1, rama.row);
    assert_eq!(
        after,
        norte_tui::mouse::After::Nothing,
        "la primera pulsación solo selecciona"
    );
    assert_eq!(app.key_owner(), KeyOwner::Tree, "y trae el teclado");
    assert_eq!(app.tree().map(norte_frontend::tree::Tree::cursor), Some(1));

    let after = pulsar_en(&mut app, rama.x1, rama.row);
    assert_eq!(after, norte_tui::mouse::After::TreeActivate);
    assert_eq!(
        app.tree_activate(),
        Some(vp("file:///casa/a")),
        "y hay rama a la que llevar el listado"
    );
}

/// Pulsar la MARCA (`▸`/`▾`) pliega o despliega esa rama de una sola
/// pulsación: es lo que dice la flecha que ya se pinta, y sin ella un lector
/// que solo usa el ratón no puede cerrar lo que abrió — `Enter` despliega y
/// navega, nunca pliega.
#[test]
fn pulsar_la_marca_de_una_rama_la_pliega_y_la_despliega() {
    let mut app = app_con_arbol();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let zonas = tras_pintar(&mut app, area);
    let rama = zonas
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("la primera rama se ve");
    let filas = |app: &App| app.tree().map_or(0, |t| t.rows().len());
    let antes = filas(&app);

    let after = pulsar_en(&mut app, rama.mark_x, rama.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing, "no navega a nada");
    assert_eq!(filas(&app), antes + 1, "desplegar enseña su hijo");

    let _ = tras_pintar(&mut app, area);
    let after = pulsar_en(&mut app, rama.mark_x, rama.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing);
    assert_eq!(filas(&app), antes, "y la misma marca la vuelve a plegar");
}

/// La barra de menús es cromo de la APLICACIÓN, no de los listados: con el
/// teclado dentro de un panel lateral su tecla tiene que seguir abriéndola.
///
/// Estaba muerta en los tres —árbol, sitios y procesos—: `app.menu` no
/// figuraba en sus allowlists, así que el panel se la comía y la pantalla se
/// quedaba igual. Es la misma lección que ya trajo aquí `layout.places`.
#[test]
fn el_menu_se_abre_con_el_teclado_dentro_de_un_panel_lateral() {
    for lista in [
        norte_tui::app::ALLOW_PLACES,
        norte_tui::app::ALLOW_PROCESSES,
    ] {
        assert!(
            lista.contains(&"app.menu"),
            "un panel lateral no puede comerse la tecla del menú"
        );
    }

    let mut app = app_con_arbol();
    assert_eq!(
        app.key_owner(),
        KeyOwner::Tree,
        "el teclado está en el árbol"
    );
    assert!(
        app.panel_chrome_command("app.menu"),
        "la tecla del menú la atiende el cromo, no el panel"
    );
    assert!(app.menu.is_some(), "y el menú se abre");
    assert!(app.panel_chrome_command("app.menu"));
    assert!(app.menu.is_none(), "la misma tecla lo cierra");
    assert!(
        !app.panel_chrome_command("dialog.up"),
        "lo que no es cromo lo sigue atendiendo el panel"
    );
}
