//! El sidebar de sitios dentro de la `App` (L3): abrirlo, enfocarlo, cerrarlo
//! y, sobre todo, NO tocar los listados al hacerlo.
//!
//! La regla que estos tests protegen es la 7 del spec: `app.panes[i]` sigue
//! queriendo decir «el i-ésimo LISTADO». Un sidebar no es un lado, y el día
//! que lo fuera, una copia podría tener por destino una lista de discos.

use norte_proto::methods::{Volume, VolumeKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Idioma fijo: el snapshot congela texto localizado.
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

fn app_de_prueba() -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entradas(&dir)),
        Pane::new(dir.clone(), entradas(&dir)),
    )
}

/// Abrir el sidebar no cambia cuántos LISTADOS hay, ni cuál está enfocado, ni
/// dónde está su cursor. Es la regla 7 del spec: el sidebar no es un lado.
#[test]
fn abrir_el_sidebar_no_toca_los_lados() {
    let mut app = app_de_prueba();
    let before = (app.panes.len(), app.focus(), app.focused().dir().clone());
    app.toggle_places();
    assert_eq!(app.panes.len(), before.0, "siguen siendo dos listados");
    assert_eq!(app.focus(), before.1);
    assert_eq!(*app.focused().dir(), before.2);
    assert!(app.places_slot().is_some());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// Y cerrarlo deja el árbol EXACTAMENTE como estaba: sin un `Split` degenerado
/// acumulándose cada vez que alguien abre y cierra el sidebar.
#[test]
fn cerrar_el_sidebar_devuelve_el_arbol_de_antes() {
    let mut app = app_de_prueba();
    let before = app.layout.clone();
    app.toggle_places();
    assert_ne!(app.layout, before, "abrirlo sí cambia el árbol");
    app.toggle_places();
    assert_eq!(app.layout, before);
    assert!(app.places_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Segunda pulsación con el teclado en los listados: ENFOCA, no cierra.
/// Cerrar algo que el lector acaba de mirar de reojo es la respuesta
/// equivocada.
#[test]
fn con_el_sidebar_abierto_y_el_teclado_fuera_la_tecla_lo_enfoca() {
    let mut app = app_de_prueba();
    app.toggle_places();
    app.return_keys_to_panes();
    app.toggle_places();
    assert!(app.places_slot().is_some(), "sigue abierto");
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// El hueco del sidebar existe en el árbol pero NO es un `browser`: iterar los
/// panes sigue dando solo listados, que es de lo que vive medio run loop.
#[test]
fn el_hueco_del_sidebar_no_aparece_como_listado() {
    let mut app = app_de_prueba();
    app.toggle_places();
    let sidebar = app.places_slot().expect("abierto");
    assert!(app.layout.slot_ids().contains(&sidebar));
    assert_eq!(app.panes.iter().count(), 2);
    assert!(app.panes.browser(sidebar).is_none());
    assert!(app.panes.places(sidebar).is_some());
}

/// Un `Split` partido de más no aparece por abrir el sidebar dos veces: la
/// segunda pulsación no acuña otro hueco.
#[test]
fn abrirlo_dos_veces_no_acuna_dos_huecos() {
    let mut app = app_de_prueba();
    app.toggle_places();
    let primero = app.places_slot().expect("abierto");
    app.return_keys_to_panes();
    app.toggle_places();
    assert_eq!(app.places_slot(), Some(primero));
    assert_eq!(
        app.layout
            .slot_ids()
            .iter()
            .filter(|id| app
                .layout
                .kind_of(**id)
                .is_some_and(|k| k.as_str() == "places"))
            .count(),
        1
    );
}

fn volumen(mount: &str, free: u64, total: u64) -> Volume {
    Volume {
        mount: vp(mount),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: VolumeKind::Fixed,
        total_bytes: Some(total),
        free_bytes: Some(free),
        read_only: false,
    }
}

/// Una `App` con el sidebar abierto y poblado, lista para pintar.
fn app_con_sidebar() -> App {
    let mut app = app_de_prueba();
    app.render_now_ms = Some(0);
    app.toggle_places();
    let id = app.places_slot().expect("abierto");
    let sidebar = app.panes.places_mut(id).expect("es un sidebar");
    sidebar.set_drives(&[
        volumen("file:///", 41_000_000_000, 120_000_000_000),
        volumen("file:///boot", 402_000_000, 1_000_000_000),
    ]);
    sidebar.set_favorites(&[
        ("trabajo".to_owned(), Ok(vp("file:///trabajo"))),
        ("roto".to_owned(), Err("hotlist-invalid".to_owned())),
    ]);
    app
}

/// Pulsa el botón izquierdo en una celda, por el mismo camino que el run
/// loop.
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

fn buffer_de(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Las filas del buffer, celda a celda.
///
/// A mano y NO con `TestBackend::to_string()`: ese envuelve cada fila en
/// comillas, así que todo recorte por columna sale desplazado una celda —y un
/// `contains()` lo tapa. Un test de geometría con `contains` no comprueba
/// geometría.
fn filas(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// El sidebar mide 16 celdas EXACTAS y el primer listado empieza justo
/// después. `Fixed` gana al mínimo del kind, así que este número es el ancho
/// de verdad y no una sugerencia.
#[test]
fn el_sidebar_ocupa_dieciseis_celdas_y_el_listado_empieza_en_la_diecisiete() {
    let app = app_con_sidebar();
    let buf = buffer_de(&app, 100, 30);
    let f = filas(&buf);
    // Fila 3: dentro de los dos bloques, ya sin el borde superior. TRES desde
    // que hay dos filas de cromo fijadas: la 0 es la barra de menús, la 1 la
    // de paneles (#324) y la 2 el borde de arriba de los bloques.
    let row = &f[3];
    let celda = |x: usize| row.chars().nth(x).expect("la celda está pintada");
    assert_eq!(celda(0), '│', "borde izquierdo del sidebar");
    assert_eq!(celda(15), '│', "borde derecho del sidebar, en la celda 15");
    assert_eq!(
        celda(16),
        '│',
        "borde izquierdo del primer listado, en la 16"
    );
}

/// La pantalla entera con el sidebar abierto.
#[test]
fn snapshot_sidebar_abierto() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

/// Un favorito roto se PINTA, marcado y atenuado. Esconderlo sería un fallo
/// de configuración que el lector no puede ver; y el motivo entero no cabe en
/// catorce celdas, así que lo dice la barra de estado (ver `places_activate`).
#[test]
fn el_favorito_roto_se_pinta_marcado_y_atenuado() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer().clone();
    let f = filas(&buf);
    let y = f
        .iter()
        .position(|row| row.chars().take(16).collect::<String>().contains("roto"))
        .expect("la fila del favorito roto está pintada");
    let sidebar: String = f[y].chars().take(16).collect();
    assert!(sidebar.contains('!'), "va marcada: {sidebar:?}");
    // Y ATENUADA: el volcado de texto no lleva estilos, así que celda a celda.
    let x = sidebar.find("roto").expect("el nombre está");
    let style = buf[(
        u16::try_from(x).expect("cabe"),
        u16::try_from(y).expect("cabe"),
    )]
        .style();
    assert_eq!(
        style.fg,
        app.theme.role(norte_theme::Role::Info).fg,
        "la fila de un favorito roto se pinta con el frente atenuado"
    );
}

/// Cerrado —el default— la pantalla no lleva sidebar ninguno: el criterio de
/// aceptación de L3 es que el usuario no note nada hasta abrirlo, y los
/// snapshots ortodoxos que ya existen lo comprueban celda a celda.
#[test]
fn cerrado_no_pinta_nada() {
    let mut app = app_de_prueba();
    app.render_now_ms = Some(0);
    let buf = buffer_de(&app, 100, 30);
    let f = filas(&buf);
    // La fila 1 es la barra de paneles, que desde la spec 2026-09-10 nombra
    // el panel («Sitios») justamente para que se sepa que existe: se salta.
    assert!(
        !f.iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .any(|(_, row)| row.contains(&norte_i18n::t_in(norte_i18n::Lang::Es, "places-title"))),
        "sin abrirlo, el título del sidebar no aparece"
    );
}

/// Enter sobre un favorito lleva al LISTADO ENFOCADO a ese sitio, y devuelve
/// el teclado. El sidebar es un MANDO, no un panel con directorio propio.
#[test]
fn enter_en_un_favorito_da_el_destino_y_suelta_el_teclado() {
    let mut app = app_con_sidebar();
    // Cabecera Unidades, dos discos, cabecera Favoritos, trabajo.
    for _ in 0..4 {
        app.places_down();
    }
    let dest = app.places_activate().expect("un favorito da destino");
    assert_eq!(dest, vp("file:///trabajo"));
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Enter sobre una cabecera no hace nada, y el teclado se queda donde está.
#[test]
fn enter_en_una_cabecera_no_hace_nada() {
    let mut app = app_con_sidebar();
    assert!(app.places_activate().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// Enter sobre un favorito ROTO no navega y la barra dice por qué: es la otra
/// mitad de pintarlo marcado, porque en catorce celdas cabe el aviso y no la
/// explicación.
#[test]
fn enter_en_un_favorito_roto_explica_en_la_barra() {
    let mut app = app_con_sidebar();
    for _ in 0..5 {
        app.places_down();
    }
    assert!(app.places_activate().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Places, "no suelta el teclado");
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "hotlist-invalid").as_str()),
        "la barra dice el motivo"
    );
}

/// Enter sobre un disco lleva a su punto de montaje.
#[test]
fn enter_en_un_disco_da_su_montaje() {
    let mut app = app_con_sidebar();
    app.places_down();
    assert_eq!(app.places_activate(), Some(vp("file:///")));
}

/// Plegar una sección esconde sus filas sin cerrar nada.
#[test]
fn plegar_desde_la_app_esconde_las_filas() {
    let mut app = app_con_sidebar();
    let id = app.places_slot().expect("abierto");
    let before = app.panes.places(id).expect("sidebar").rows().len();
    app.places_toggle_fold();
    let after = app.panes.places(id).expect("sidebar").rows().len();
    assert!(after < before);
    assert!(app.places_slot().is_some(), "plegar no cierra el sidebar");
}

/// Una disposición que ya no tiene el panel NO puede dejar el teclado dentro
/// de él.
///
/// `set_layout` no tocaba `key_owner`, así que con el sidebar enfocado y una
/// disposición nueva sin sidebar —cambiar de perfil, aplicar un preset,
/// restaurar una sesión— el teclado se quedaba apuntando a un panel que ya no
/// estaba. Todas las teclas iban a `on_places_key`, `places_slot()` devolvía
/// `None`, y cada brazo era un no-op: el gestor entero dejaba de responder sin
/// nada en pantalla que explicara por qué.
#[test]
fn una_disposicion_sin_el_panel_devuelve_el_teclado() {
    use norte_frontend::layout::{KindId, Node, SlotId};

    // `app_con_sidebar` ya lo abre, y abrirlo YA da el teclado: el ciclo real
    // es abrir-con-teclado → cerrar, no las tres pulsaciones que algún
    // comentario del código describe.
    let mut app = app_con_sidebar();
    assert_eq!(app.key_owner(), KeyOwner::Places, "el teclado está dentro");

    // Una disposición de un solo listado: sin sidebar.
    app.set_layout(Node::slot(SlotId(1), KindId::browser()));

    assert!(app.places_slot().is_none(), "el panel ya no está");
    assert_eq!(
        app.key_owner(),
        KeyOwner::Panes,
        "y el teclado ha vuelto a los listados"
    );
}

/// El cursor arranca sobre una CABECERA, que es lo que hace que `⏎` tenga que
/// contestar ahí.
///
/// `places_activate` devuelve `None` sobre una cabecera, así que Enter era
/// inerte justo en la primera fila del panel: lo abrías, pulsabas la tecla que
/// se prueba primero sobre algo que se abre, y no pasaba nada. Plegar era
/// Espacio y solo Espacio.
#[test]
fn el_cursor_arranca_sobre_una_cabecera() {
    let app = app_con_sidebar();
    assert!(
        app.places_cursor_on_header(),
        "la primera fila es la cabecera de una sección"
    );
}

/// Y bajando hasta una unidad deja de estarlo: ahí `⏎` navega, que es lo que
/// Enter significa sobre una hoja.
#[test]
fn sobre_una_unidad_el_cursor_ya_no_esta_en_una_cabecera() {
    let mut app = app_con_sidebar();
    app.places_down();
    assert!(!app.places_cursor_on_header());
}

/// `layout.places` está atado en los SIETE presets, y en `[global]`.
///
/// Lo primero, porque un comando de núcleo atado en unos y no en otros es el
/// agujero que L1b metió con `pane.tab-next`: podías abrir una pestaña y no
/// volver a ella en cinco de los siete.
///
/// Lo segundo lo destapó pilotar la TUI en tmux con la suite en verde: atado
/// solo en `[pane]`, la tecla no existía para la pantalla `dialog`, que es la
/// que resuelve mientras el teclado está DENTRO del sidebar. O sea que abrías
/// el panel y la tecla de cerrarlo dejaba de funcionar. Por eso se comprueban
/// las DOS pantallas: la que dispara es la que importa.
#[test]
fn layout_places_esta_atado_en_los_siete_presets_y_en_las_dos_pantallas() {
    use norte_frontend::keymap::{CATALOGUE, Effective, Screen, parse_keymap, presets};
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for nombre in presets::NAMES {
        let src = presets::source(nombre).expect("el preset existe");
        let kf = parse_keymap(src).expect("el preset parsea");
        for pantalla in [Screen::Browse, Screen::Dialog] {
            let eff =
                Effective::build_for(&kf, &[], &conocidos, pantalla).expect("el preset fusiona");
            assert!(
                eff.bindings()
                    .iter()
                    .any(|(_, cmd)| *cmd == "layout.places"),
                "{nombre} no ata layout.places en {pantalla:?}"
            );
        }
    }
}

/// El ratón: pulsar una fila la selecciona y trae el teclado; pulsarla otra
/// vez la ACTIVA, que es lo mismo que `Enter` (#226).
///
/// El sidebar se envió con teclado y nada más: sus celdas no son de ningún
/// listado, así que un click ahí caía en «fuera de los panes» y no hacía nada
/// — un panel que se pinta y no se puede tocar.
#[test]
fn pulsar_una_fila_del_sidebar_la_selecciona_y_repulsarla_la_activa() {
    let mut app = app_con_sidebar();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let _ = buffer_de(&app, 100, 30);
    let (geo, tabs, menus, sitios) = (
        norte_tui::ui::pane_geometry(&app, area),
        norte_tui::ui::tab_zones(&app, area),
        norte_tui::ui::menu_zones(&app, area),
        norte_tui::ui::places_zones(&app, area),
    );
    let huecos = norte_tui::ui::panel_slots(&app, area);
    norte_tui::mouse::after_frame(
        &mut app,
        geo,
        norte_tui::mouse::FrameZones {
            tabs,
            menus,
            places: sitios,
            slots: huecos,
            ..Default::default()
        },
    );
    let zonas = norte_tui::ui::places_zones(&app, area);
    assert!(!zonas.is_empty(), "el sidebar tiene filas pulsables");
    // La primera unidad: la fila 0 es la cabecera de la sección.
    let unidad = zonas
        .iter()
        .find(|z| z.index == 1)
        .copied()
        .expect("la primera unidad se ve");

    app.return_keys_to_panes();
    let after = pulsar_en(&mut app, unidad.x0 + 1, unidad.row);
    assert_eq!(after, norte_tui::mouse::After::Nothing, "solo selecciona");
    assert_eq!(app.key_owner(), KeyOwner::Places, "y trae el teclado");
    let cursor = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(norte_frontend::places::PlacesState::cursor);
    assert_eq!(cursor, Some(1));

    // La misma fila otra vez: eso es activar, y activarla la resuelve el run
    // loop por el flujo de `cd` de siempre.
    let after = pulsar_en(&mut app, unidad.x0 + 1, unidad.row);
    assert_eq!(after, norte_tui::mouse::After::PlacesActivate);
    assert!(
        app.places_activate().is_some(),
        "y hay sitio a donde llevar el listado"
    );
}

/// Pulsar una CABECERA pliega su sección de una sola pulsación, y lo dice
/// para que el run loop vuelva a pedir las unidades — el mismo camino que la
/// tecla, y no un cuarto disparador de refresco (#226).
#[test]
fn pulsar_una_cabecera_pliega_su_seccion() {
    let mut app = app_con_sidebar();
    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let _ = buffer_de(&app, 100, 30);
    let (geo, tabs, menus, sitios) = (
        norte_tui::ui::pane_geometry(&app, area),
        norte_tui::ui::tab_zones(&app, area),
        norte_tui::ui::menu_zones(&app, area),
        norte_tui::ui::places_zones(&app, area),
    );
    let huecos = norte_tui::ui::panel_slots(&app, area);
    norte_tui::mouse::after_frame(
        &mut app,
        geo,
        norte_tui::mouse::FrameZones {
            tabs,
            menus,
            places: sitios,
            slots: huecos,
            ..Default::default()
        },
    );
    let zonas = norte_tui::ui::places_zones(&app, area);
    let header = zonas
        .iter()
        .find(|z| z.index == 0)
        .copied()
        .expect("la cabecera se ve");
    let filas_antes = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(|s| s.rows().len())
        .expect("sidebar");

    let after = pulsar_en(&mut app, header.x0 + 1, header.row);
    assert_eq!(after, norte_tui::mouse::After::PlacesFolded);
    let filas_ahora = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .map(|s| s.rows().len())
        .expect("sidebar");
    assert!(
        filas_ahora < filas_antes,
        "plegar esconde sus filas: {filas_antes} → {filas_ahora}"
    );
    assert!(
        !app.places_drives_visible(),
        "y las unidades quedan plegadas"
    );
}

/// Con el teclado DENTRO del sidebar, `layout.grow` cambia el ancho DEL
/// SIDEBAR.
///
/// Antes no lo cambiaba nada: `layout_resize` pasaba siempre
/// `focused_slot()`, que es un listado visible, así que la rama de
/// `Size::Fixed` de `Node::resize` no la alcanzaba ningún camino de
/// producción — el sidebar se quedaba con el ancho con el que abría y el
/// CHANGELOG anunciaba lo contrario (#244 M1). Los tests de `resize` pasaban
/// porque le daban el id del sidebar a mano.
#[test]
fn con_el_teclado_dentro_el_sidebar_cambia_de_ancho() {
    use norte_frontend::layout::Size;

    let mut app = app_de_prueba();
    app.toggle_places();
    assert_eq!(app.key_owner(), KeyOwner::Places, "el teclado está dentro");
    let id = app.places_slot().expect("abierto");
    let width = |app: &App| {
        app.layout
            .sizes_of(id)
            .and_then(|(sizes, pos)| sizes.get(pos).copied())
    };
    let before = width(&app).expect("el sidebar tiene tamaño");
    assert!(matches!(before, Size::Fixed(_)), "y es FIJO: {before:?}");

    app.layout_resize(1);
    assert_ne!(width(&app), Some(before), "creció");

    // Y con el teclado FUERA vuelve a mandar el listado enfocado: el sidebar
    // no se mueve solo.
    app.return_keys_to_panes();
    let now = width(&app);
    app.layout_resize(1);
    assert_eq!(width(&app), now, "el sidebar no se toca desde los listados");
}

/// Y el sidebar DESPACHA su propia tecla: sin esto la tecla llega y se cae en
/// el allowlist, que es la misma pantalla muerta con otro culpable.
#[test]
fn el_sidebar_despacha_su_propia_tecla() {
    assert!(norte_tui::app::ALLOW_PLACES.contains(&"layout.places"));
}

/// Las unidades se PIDEN por bandera, y quien la enciende son los tres
/// caminos por los que la sección aparece.
///
/// `host.volumes` es I/O y `App` no tiene backend, así que cada sitio se lo
/// pedía por su cuenta — y faltaba justo en los que nadie recordó: arrancar
/// con una disposición que trae el sidebar, y cambiar de perfil, que monta
/// una pantalla nueva y con ella un panel vacío.
#[test]
fn los_tres_caminos_dejan_las_unidades_pedidas() {
    use norte_frontend::layout::{Edge, KindId, Node, Size, SlotId};
    let mut app = app_de_prueba();
    assert!(!app.places_wants_drives, "sin sidebar no se pide nada");

    // 1 — abrirlo con su tecla.
    app.toggle_places();
    assert!(app.places_wants_drives);
    app.places_wants_drives = false;

    // 2 — plegar NO las pide (no se ven); desplegar, sí.
    app.places_toggle_fold();
    assert!(!app.places_wants_drives, "plegadas no se piden");
    app.places_toggle_fold();
    assert!(app.places_wants_drives);
    app.places_wants_drives = false;

    // 3 — una disposición que ya lo trae, sin pasar por la tecla.
    let arbol = Node::slot(SlotId(0), KindId::browser()).dock(
        SlotId(0),
        Edge::Left,
        Size::Fixed(16),
        &Node::slot(SlotId(9), KindId::new("places")),
    );
    app.set_layout(arbol);
    assert!(app.places_wants_drives);
}

/// Un favorito válido, como lo deja la config ya cargada.
fn favorito(name: &str, wire: &str) -> norte_tui::config::HotlistItem {
    norte_tui::config::HotlistItem {
        name: name.to_owned(),
        target: Ok(vp(wire)),
    }
}

/// Los nombres de los favoritos que el sidebar pinta ahora mismo.
fn favoritos_del_sidebar(app: &App) -> Vec<String> {
    let id = app.places_slot().expect("el sidebar está abierto");
    app.panes
        .places(id)
        .expect("es un sidebar")
        .rows()
        .iter()
        .filter_map(|r| match r {
            norte_frontend::places::PlaceRow::Favorite { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Un layout que TRAE el sidebar —`full`, `explorer`, una sesión de ayer— lo
/// abre sin pasar por su tecla, y era la tecla la que copiaba los favoritos:
/// el panel salía vacío y nada dentro del programa lo llenaba nunca.
#[test]
fn un_layout_que_trae_el_sidebar_lo_arranca_con_los_favoritos() {
    use norte_frontend::layout::{Edge, KindId, Node, Size, SlotId};
    let mut app = app_de_prueba();
    app.set_hotlist(vec![favorito("descargas", "file:///casa/descargas")]);
    // El sidebar entra por el ÁRBOL, no por `toggle_places`.
    let arbol = Node::slot(SlotId(0), KindId::browser()).dock(
        SlotId(0),
        Edge::Left,
        Size::Fixed(16),
        &Node::slot(SlotId(9), KindId::new("places")),
    );
    app.set_layout(arbol);
    assert_eq!(favoritos_del_sidebar(&app), vec!["descargas".to_owned()]);
}

/// Y un favorito añadido con el sidebar YA abierto sale en él. El popup se
/// reconstruía y el sidebar no, así que las dos superficies del mismo dato
/// decían cosas distintas — la queja era exactamente esa.
#[test]
fn anadir_un_favorito_lo_pinta_tambien_en_el_sidebar() {
    let mut app = app_de_prueba();
    app.toggle_places();
    assert!(
        favoritos_del_sidebar(&app).is_empty(),
        "empieza sin ninguno"
    );

    app.hotlist_apply_saved("descargas", vp("file:///casa/descargas"));
    assert_eq!(favoritos_del_sidebar(&app), vec!["descargas".to_owned()]);

    app.hotlist_apply_removed("descargas");
    assert!(
        favoritos_del_sidebar(&app).is_empty(),
        "y quitarlo lo quita de los dos sitios"
    );
}

/// Un hot-reload del `norte.toml` —o un cambio de perfil, que pasa por el
/// mismo sitio— reemplaza la lista entera, y el sidebar la sigue.
#[test]
fn recargar_la_config_reemplaza_los_favoritos_del_sidebar() {
    let mut app = app_de_prueba();
    app.toggle_places();
    app.set_hotlist(vec![favorito("viejo", "file:///viejo")]);
    assert_eq!(favoritos_del_sidebar(&app), vec!["viejo".to_owned()]);

    app.set_hotlist(vec![favorito("nuevo", "file:///nuevo")]);
    assert_eq!(favoritos_del_sidebar(&app), vec!["nuevo".to_owned()]);
}
