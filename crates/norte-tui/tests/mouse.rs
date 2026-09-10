//! Ratón de la TUI: hit test contra un layout REAL (se pinta y se resuelve
//! contra lo pintado, no contra una geometría inventada aquí), rueda,
//! captura alrededor del opener externo y `[ui] mouse = false`.
//!
//! Los gestos en sí (qué marca un barrido, cuándo es transferencia) tienen
//! sus propios tests en `norte-frontend::mouse`: aquí se comprueba lo que
//! es de la TERMINAL — qué celda es qué fila y quién tiene la captura.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Modal, Pane, TransferKind};
use norte_tui::mouse::{self, After};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Ancho y alto del terminal de estos tests. Con 12 filas el layout sale
/// EXACTO y a mano: filas 0..=10 para los panes (el panel de tasks mide 0
/// sin tasks) y la 11 para la barra de estado. Dentro de un pane: 0 borde
/// superior, 1 cabecera de columnas, 2..=9 las OCHO filas de listado, 10
/// borde inferior.
const W: u16 = 60;
/// Alto del terminal de estos tests (ver [`W`]).
const H: u16 = 12;

/// Primera fila de listado de un pane en este layout.
///
/// CUATRO desde que hay dos filas de cromo fijadas por defecto: fila 0 la barra
/// de menús (`[ui] menu_bar`), 1 la barra de paneles (`[ui] panel_bar`, #324),
/// 2 el borde superior y 3 la cabecera de columnas.
///
/// Que cambiar esta constante ARREGLE todos los tests de este fichero es la
/// demostración de que el mapeo de clics siguió a la geometría solo — la resta
/// de las filas se hace en el reparto del frame, no en el pintor, y el ratón
/// lee ese mismo reparto. Lo volvió a demostrar la barra de paneles: dos
/// constantes y ningún test de clic tocado.
const FILA0: u16 = 4;
/// Cuántas filas de listado caben. Dos menos: las dos barras se las han comido.
const FILAS: u16 = 6;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// `n` entradas `f0..f{n-1}` bajo `dir`.
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

/// Una `App` con `n` entradas en cada pane, YA pintada una vez: la
/// geometría del ratón viene del frame de verdad (el mismo camino del run
/// loop, #124), jamás de un `PaneGeometry` escrito a mano — un test que se
/// inventara la geometría pasaría igual con el layout roto.
fn app_pintada(n: usize) -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, n)),
        Pane::new(dir.clone(), entradas(&dir, n)),
    );
    let _ = pintar(&mut app);
    app
}

/// Pinta un frame, devuelve la geometría al modelo (como el run loop) y
/// entrega LAS LÍNEAS PINTADAS.
///
/// Devolver el buffer no es comodidad: sin él estos tests solo comprobarían
/// que `ui::pane_geometry` está de acuerdo consigo misma. Si `draw_pane`
/// moviera el listado una fila y la geometría se quedara en `y + 2`, todo
/// seguiría verde y el ratón marcaría el fichero de al lado — que es
/// exactamente el fallo que la geometría existe para no tener. Los tests que
/// resuelven un índice lo CONTRASTAN contra el texto de esa fila.
fn pintar(app: &mut App) -> Vec<String> {
    pintar_en(app, W, H)
}

/// Como [`pintar`] sobre un terminal de otro tamaño: con un panel lateral
/// abierto, 60×12 no da para colocarlo y el reparto lo deja fuera.
fn pintar_en(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
    // El MISMO orden que el run loop: reconciliar la ventana, pintar,
    // devolver la geometría. Sin el primer paso se pintaría una ventana que
    // nadie reconcilió, o sea una pantalla que ningún usuario ve.
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
            places: ui::places_zones(app, frame.area),
            tree: ui::tree_zones(app, frame.area),
            extensions: ui::extension_zones(app, frame.area),
            session: ui::session_zone(app, frame.area),
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

/// Comprueba que la fila `row` del frame pinta la entrada `index` del pane
/// IZQUIERDO: el contraste entre lo que resuelve el hit test y lo que el
/// usuario tiene delante.
///
/// Compara contra el nombre de la entrada, no contra un literal: `Pane::new`
/// ORDENA, así que `entries[13]` no es «f13».
fn assert_fila(lines: &[String], app: &App, row: u16, index: usize) {
    let entry = &app.panes[0].entries()[index];
    let name = String::from_utf8_lossy(
        entry
            .path
            .file_name()
            .expect("una entrada de test tiene nombre")
            .as_bytes(),
    )
    .into_owned();
    let pintada = &lines[usize::from(row)];
    assert!(
        pintada.contains(&format!("{name} ")),
        "la fila {row} debería pintar `{name}` (índice {index}) y pinta: {pintada}"
    );
}

/// Un evento de ratón sin modificadores.
fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    ev_con(kind, col, row, KeyModifiers::NONE)
}

/// Un evento de ratón con modificadores.
fn ev_con(kind: MouseEventKind, col: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers,
    }
}

/// Botón izquierdo abajo.
const ABAJO: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
/// Botón izquierdo arriba.
const ARRIBA: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
/// Arrastre con el izquierdo pulsado.
const ARRASTRE: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

/// Una ventana SUELTA lleva su indicador en la barra de estado, y pulsarlo
/// pide la explicación: el run loop abre la ayuda en la página de los
/// paneles. La zona sale del frame PINTADO, así que se contrasta contra la
/// línea que el lector tiene delante y no contra una aritmética paralela.
#[test]
fn pulsar_el_indicador_de_sesion_pide_la_ayuda() {
    let mut app = app_pintada(3);
    app.session.detached = true;
    let lines = pintar(&mut app);
    // El backend de prueba entrecomilla cada línea: la primera celda es el
    // byte 1, no el 0.
    let barra = lines[usize::from(H - 1)].trim_start_matches('"');
    let badge = app.session_banner().expect("hay indicador");
    assert!(barra.contains(&badge), "la barra lo pinta: {barra}");
    let x0 = u16::try_from(barra.find(&badge).expect("está")).expect("cabe");
    // La columna del primer carácter: en esta línea todo lo anterior es
    // ASCII, así que bytes y celdas coinciden.
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, x0, H - 1)),
        After::SessionHelp,
        "pulsar el indicador pide la ayuda"
    );
    // Y a su izquierda no: el resto de la barra no es pulsable.
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, x0.saturating_sub(1), H - 1)),
        After::Nothing
    );
    // La página existe en el corpus, en los dos idiomas: la constante no
    // puede apuntar a una página borrada sin que esto se ponga rojo.
    for lang in [norte_help::Lang::Es, norte_help::Lang::En] {
        assert!(
            norte_help::topic(lang, mouse::SESSION_HELP_TOPIC).is_some(),
            "la página {} existe en {lang:?}",
            mouse::SESSION_HELP_TOPIC
        );
    }
    // Y abrirla la abre en ESA página, como raíz: `Esc` cierra.
    norte_tui::overlays::open_help_topic(
        &mut app,
        norte_help::Lang::Es,
        &[],
        mouse::SESSION_HELP_TOPIC,
    );
    let help = app.help.as_ref().expect("la ayuda se abrió");
    assert_eq!(help.state.current().as_str(), mouse::SESSION_HELP_TOPIC);

    // La dueña no tiene indicador, y la misma celda no hace nada.
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
    // Ancla de los números de arriba: si el pane gana o pierde cromo, cae
    // ESTE test con un mensaje claro, y no los seis siguientes con
    // aritmética confusa.
    let mut app = app_pintada(5);
    let lines = pintar(&mut app);
    let geom = app.mouse.geometry().expect("hay geometría");
    let (left, right) = (geom[0], geom[1]);
    // `y = 2` y dos filas menos de alto: las dos barras fijadas se quedan las
    // filas 0 y 1. Que la GEOMETRÍA lo diga —y no solo el pintor— es el punto:
    // el ratón lee estos rectángulos, así que un clic sigue cayendo donde el
    // lector lo dio.
    assert_eq!((left.x, left.y, left.width, left.height), (0, 2, 30, 9));
    assert_eq!(
        (right.x, right.y, right.width, right.height),
        (30, 2, 30, 9)
    );
    assert_eq!(left.first_list_row, FILA0, "borde superior + cabecera");
    assert_eq!(left.list_rows, FILAS, "interior menos la cabecera");
    assert_eq!(left.offset, 0, "cursor en la primera: sin scroll");
    // Y lo que de verdad hay PINTADO en esas filas. El indicador de orden
    // (`▲`) en vez del rótulo de la columna: el rótulo está traducido y
    // estos tests no fijan idioma.
    let cabecera = usize::from(FILA0) - 1;
    assert!(
        lines[cabecera].contains('▲'),
        "fila {cabecera} = cabecera de columnas: {}",
        lines[cabecera]
    );
    // Y la fila 0 es la barra de menú, que es lo que empujó a la cabecera
    // hasta ahí. Se comprueba por su FORMA y no por un rótulo: estos tests no
    // fijan idioma, y «Archivo» solo aparece en uno de los dos.
    assert!(
        !lines[0].trim().is_empty() && !lines[0].contains('│'),
        "fila 0 = barra de menú (texto, sin bordes de panel): {}",
        lines[0]
    );
    assert_fila(&lines, &app, FILA0, 0);
}

#[test]
fn un_click_en_la_primera_fila_resuelve_la_primera_entrada() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0).expect("dentro del pane izquierdo");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, Some(0));
}

#[test]
fn un_click_en_la_ultima_entrada_resuelve_esa_y_no_otra() {
    let app = app_pintada(5);
    let hit = mouse::hit_test(&app, 5, FILA0 + 4).expect("dentro del pane");
    assert_eq!(hit.index, Some(4), "quinta fila pintada = quinta entrada");
}

/// La cabecera de columnas es CROMO: resuelve al pane, jamás a una fila.
/// Sin esto, ordenar por una columna con el ratón (que es lo que el usuario
/// va a intentar ahí) movería además el cursor a la primera entrada.
#[test]
fn la_cabecera_de_columnas_no_es_ninguna_fila() {
    let app = app_pintada(5);
    // Relativa a `FILA0` y no un número suelto: la cabecera es la fila justo
    // encima de la primera del listado, y atarla a la constante hace que
    // mover el cromo mueva este test con él en vez de romperlo.
    let hit = mouse::hit_test(&app, 5, FILA0 - 1).expect("sigue siendo el pane");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, None);
}

/// El borde superior (donde va el título con la ruta) y el inferior (donde
/// se pinta el input del quick search) tampoco son filas.
#[test]
fn los_bordes_del_pane_no_son_filas() {
    let app = app_pintada(5);
    // La fila 0 ya no es el borde del pane: es la BARRA DE MENÚ, y no
    // pertenece a ningún panel — igual que la barra de estado de abajo. Un
    // clic ahí no puede resolver a una entrada ni a un pane.
    assert!(
        mouse::hit_test(&app, 5, 0).is_none(),
        "la fila 0 es la barra de menú, no un pane"
    );
    for row in [FILA0 - 2, FILA0 + FILAS] {
        let hit = mouse::hit_test(&app, 5, row).expect("dentro del bloque");
        assert_eq!(hit.index, None, "fila {row} es borde");
    }
    for col in [0, 29] {
        let hit = mouse::hit_test(&app, col, FILA0).expect("dentro del bloque");
        assert_eq!(hit.index, None, "columna {col} es borde lateral");
    }
}

/// Un clic en la barra de menú fijada la ABRE.
///
/// Es lo que hace usable la barra: antes solo se atendían clics del menú si YA
/// estaba abierto, así que con la barra fijada y cerrada pulsar «Archivo» no
/// hacía nada — una barra que existe para que encuentres el menú y en la que
/// el clic es inerte.
#[test]
fn un_click_en_la_barra_de_menu_la_abre() {
    let mut app = app_pintada(5);
    assert!(app.menu.is_none(), "arranca cerrado");
    let _ = mouse::handle(&mut app, ev(ABAJO, 2, 0));
    assert!(
        app.menu.is_some(),
        "el clic en el primer título abre el menú"
    );
}

/// El menú se REABRE por donde iba.
///
/// Abrirlo siempre por el primero obliga a recorrer la barra entera en cada
/// gesto, y quien usa dos entradas del mismo menú lo paga cada vez. El cierre
/// pasa por una sola puerta (`App::close_menu`) justamente para que los cinco
/// sitios que cierran apunten lo mismo.
#[test]
fn el_menu_se_reabre_por_donde_iba() {
    let mut app = app_pintada(5);
    let mut m = norte_frontend::menu::MenuState::new();
    m.open(3);
    app.menu = Some(m);
    app.close_menu();
    assert!(app.menu.is_none(), "cerrado");

    app.menu = Some(norte_frontend::menu::MenuState::reopen_at(app.menu_ultimo));
    assert_eq!(
        app.menu.as_ref().map(norte_frontend::menu::MenuState::menu),
        Some(3),
        "vuelve al que estaba abierto, no al primero"
    );
    assert_eq!(
        app.menu.as_ref().map(norte_frontend::menu::MenuState::item),
        Some(0),
        "y el cursor sí vuelve al principio: la lista es corta y se lee entera"
    );
}

/// Y con las DOS barras apagadas, la fila 0 vuelve a ser del panel: no hay
/// barra que pulsar, así que el clic no puede abrir nada.
///
/// Las dos: con solo la de menús apagada, la de paneles se muda a la fila 0 y
/// este clic caía en un botón. El test seguía verde —`menu.is_none()` se
/// cumplía igual— mientras su invariante declarado era ya falso.
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

/// Sin la de menús pero CON la de paneles, la fila 0 es de la barra de
/// paneles: se muda arriba y sigue siendo pulsable.
#[test]
fn sin_barra_de_menus_la_de_paneles_se_muda_a_la_fila_cero() {
    let mut app = app_pintada(5);
    app.menu_bar = false;
    let _ = pintar(&mut app);
    let _ = mouse::handle(&mut app, ev(ABAJO, 1, 0));
    assert_eq!(
        app.pending_panel_command.as_deref(),
        Some("layout.places"),
        "el primer botón de la barra"
    );
    assert!(app.menu.is_none(), "y no abre el menú, que no está");
}

/// Los botones caen donde dicen las zonas, y solo ahí.
///
/// Tres celdas por botón desde la columna 0: el primero es `layout.places` y
/// el sexto `layout.log`. Se comprueban los dos EXTREMOS y la celda siguiente
/// al último, que es donde un `x1` mal calculado se nota.
#[test]
fn cada_boton_de_la_barra_cae_en_su_sitio() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    let pulsa = |app: &mut norte_tui::app::App, col: u16| {
        app.pending_panel_command = None;
        let _ = mouse::handle(app, ev(ABAJO, col, 1));
        app.pending_panel_command.clone()
    };
    assert_eq!(pulsa(&mut app, 1).as_deref(), Some("layout.places"));
    assert_eq!(
        pulsa(&mut app, 16).as_deref(),
        Some("layout.log"),
        "el último"
    );
    assert_eq!(pulsa(&mut app, 18), None, "pasado el último no hay botón");
}

/// REGRESIÓN de un BLOCKER: con un overlay delante, la barra ni se pinta ni se
/// puede pulsar.
///
/// La barra se pinta ANTES que los overlays, así que sus zonas seguían activas
/// por debajo: con la ayuda abierta, un clic en la barra de título de la ayuda
/// —que ocupa la misma fila— caía en un botón y abría o cerraba un panel que
/// el lector no estaba viendo. Pintada y pulsable tienen que ser lo mismo.
#[test]
fn con_un_overlay_delante_la_barra_no_se_pulsa() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    // Un overlay cualquiera de los que tapan la fila.
    app.open_theme_picker();
    let _ = pintar(&mut app);
    let antes = app.layout.clone();
    let _ = mouse::handle(&mut app, ev(ABAJO, 1, 1));
    assert!(
        app.pending_panel_command.is_none(),
        "un clic sobre el overlay tocó un botón de la barra"
    );
    assert_eq!(antes, app.layout, "y la disposición cambió por debajo");
}

/// La barra de estado no pertenece a ningún pane: fuera del hit test
/// entero, no «la última fila del pane de abajo».
#[test]
fn la_barra_de_estado_no_pertenece_a_ningun_pane() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, 5, H - 1).is_none());
}

/// El hueco BAJO la última entrada de un listado corto no es la última
/// entrada. Es el caso que más sorprende si se resuelve mal: el fallo
/// natural (saturar el índice) hace que un click en el vacío marque —o
/// mueva el cursor a— el último fichero del directorio, que es justo el que
/// nadie estaba mirando cuando pulsó ahí.
#[test]
fn el_hueco_bajo_la_ultima_entrada_no_es_la_ultima_entrada() {
    let mut app = app_pintada(3);
    for row in FILA0 + 3..FILA0 + FILAS {
        let hit = mouse::hit_test(&app, 5, row).expect("sigue dentro del pane");
        assert_eq!(hit.index, None, "fila {row}: vacío, no entrada");
    }
    // Y el click de verdad tampoco mueve el cursor.
    app.panes[0].set_cursor(1);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 6));
    assert_eq!(app.panes[0].cursor(), 1, "el cursor se queda donde estaba");
}

#[test]
fn un_click_fuera_de_los_dos_panes_no_resuelve_nada() {
    let app = app_pintada(5);
    assert!(mouse::hit_test(&app, W - 1, H - 1).is_none(), "esquina");
    assert!(mouse::hit_test(&app, 5, H + 5).is_none(), "fuera del frame");
}

#[test]
fn un_click_enfoca_ese_pane_y_mueve_el_cursor() {
    let mut app = app_pintada(5);
    assert_eq!(app.focus(), 0);
    let after = mouse::handle(&mut app, ev(ABAJO, 35, FILA0 + 2));
    assert_eq!(after, After::Nothing);
    assert_eq!(app.focus(), 1, "el click enfoca el pane pulsado");
    assert_eq!(app.panes[1].cursor(), 2);
    assert_eq!(app.panes[1].marks_len(), 0, "un click a secas no marca");
}

/// El scroll sale del cursor (ver `ui::list_offset`), así que un click sobre
/// una fila de un listado YA desplazado tiene que sumar el offset. Es donde
/// un hit test ingenuo (fila pintada = índice) se equivoca en silencio, y se
/// equivoca más cuanto más abajo esté el usuario.
#[test]
fn un_click_sobre_un_listado_desplazado_suma_el_scroll() {
    let mut app = app_pintada(40);
    app.panes[0].set_cursor(20);
    let lines = pintar(&mut app);
    let offset = app.mouse.geometry().expect("geometría")[0].offset;
    assert_eq!(offset, 21 - usize::from(FILAS), "el cursor va al borde");
    let hit = mouse::hit_test(&app, 5, FILA0).expect("dentro del pane");
    assert_eq!(hit.index, Some(offset), "la primera fila PINTADA");
    // Contra el buffer: la fila que se resuelve es la que se ve.
    assert_fila(&lines, &app, FILA0, offset);
    let hit = mouse::hit_test(&app, 5, FILA0 + FILAS - 1).expect("dentro del pane");
    assert_eq!(hit.index, Some(20), "la última pintada es el cursor");
    assert_fila(&lines, &app, FILA0 + FILAS - 1, 20);
}

/// La rueda desplaza el listado BAJO EL PUNTERO y no toca el foco. Mirar un
/// panel mientras se trabaja en el otro es el gesto normal con dos paneles;
/// robarle el foco al panel activo por pasar el ratón por encima sería un
/// cambio de destino de la siguiente operación hecho sin pulsar nada.
#[test]
fn la_rueda_desplaza_el_pane_bajo_el_puntero_y_no_el_del_foco() {
    let mut app = app_pintada(40);
    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollDown, 35, FILA0 + 1));
    assert_eq!(app.focus(), 0, "el foco NO se mueve con la rueda");
    assert_eq!(
        app.panes[1].cursor(),
        3,
        "se desplazó el pane de la derecha"
    );
    assert_eq!(app.panes[0].cursor(), 0, "el del foco, intacto");

    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 35, FILA0 + 1));
    assert_eq!(app.panes[1].cursor(), 0, "y vuelve");
}

/// **La rueda sobre el VISOR lo desplaza.**
///
/// El visor es un overlay, y el corte de los overlays se comía el evento: con
/// un fichero abierto, rodar no hacía absolutamente nada. Es el gesto más
/// obvio que tiene un visor, y lo único que había debajo era un listado que no
/// se ve — desplazar ESE habría sido peor.
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
    let bajado = app.viewer.as_ref().expect("visor abierto").scroll;
    assert!(bajado > 0, "el visor bajó");
    assert_eq!(app.panes[1].cursor(), 0, "y el listado de debajo, intacto");

    let _ = mouse::handle(&mut app, ev(MouseEventKind::ScrollUp, 35, FILA0 + 1));
    assert_eq!(
        app.viewer.as_ref().expect("visor abierto").scroll,
        0,
        "y vuelve"
    );
}

/// Doble click = `nav.enter`. Se comprueba el ACUERDO con el run loop
/// (devuelve [`After::Enter`], que allí despacha el mismo comando del
/// teclado), no la navegación en sí: entrar en un directorio necesita
/// backend y ya tiene sus tests.
#[test]
fn el_doble_click_pide_el_mismo_nav_enter_del_teclado() {
    let mut app = app_pintada(5);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 1), t0),
        After::Nothing,
        "el primero es un click normal"
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

/// **Un doble click sobre un FICHERO deja algo que lanzar.**
///
/// `nav.enter` sobre un fichero local no navega: resuelve el programa del
/// escritorio y lo deja armado en `pending_open` para que lo lance el dueño de
/// la terminal. El brazo del ratón corría el comando y no remataba esa parte,
/// así que un doble click en un `.jpg` no hacía absolutamente nada ni decía
/// por qué.
///
/// Lo que se comprueba aquí es el CONTRATO del que depende ese remate: que
/// `nav.enter` sobre un fichero arma el lanzamiento. El cable en sí
/// —`despachar_clic` lanzando lo armado— no tiene test porque `on_mouse`
/// necesita una terminal de verdad; el arreglo es que los tres brazos del
/// ratón salgan por la MISMA función, que es lo que impide olvidarlo otra vez.
#[test]
fn nav_enter_sobre_un_fichero_deja_un_opener_armado() {
    use norte_tui::gestures::{EnterAction, enter_action, resolve_opener};

    let mut app = app_pintada(5);
    // El listado son ficheros locales; el cursor arranca en el primero.
    app.set_focus(0);
    app.panes[0].set_cursor(0);
    assert!(
        matches!(enter_action(&app), EnterAction::OpenExternal),
        "sobre un fichero local, entrar es ABRIR: {:?}",
        enter_action(&app)
    );

    assert!(app.pending_open.is_none());
    resolve_opener(&mut app);
    assert!(
        app.pending_open.is_some(),
        "y resolverlo deja el programa armado para el dueño de la terminal"
    );
}

/// Dos clicks LENTOS sobre la misma fila son dos clicks. Y dos rápidos
/// sobre filas distintas, también: si no, bajar por el listado a golpe de
/// click entraría en un directorio cada dos filas.
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
        "tres segundos después no es un doble click"
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
        "otra fila tampoco"
    );
}

/// Un ctrl+click NO es la primera mitad de un doble click. Marcar una fila
/// y volver a pulsarla enseguida es exactamente lo que se hace para
/// arrastrarla: si contara, el gesto entraría en el directorio en vez de
/// arrancar el arrastre.
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
    assert_eq!(app.panes[0].marks_len(), 0, "el mismo gesto desmarca");
}

#[test]
fn un_arrastre_marca_lo_que_barre() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    assert_eq!(app.panes[0].marks_len(), 0, "la pulsación todavía no marca");
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 4));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + 4));
    assert_eq!(app.panes[0].marks_len(), 4, "filas 1..=4");
}

/// Soltar una selección en el otro panel abre EXACTAMENTE el modal que
/// abriría la tecla de copiar sobre esa misma selección.
///
/// Es la aserción que sostiene todo lo demás: un drop es una mutación, y si
/// no entra por la misma puerta que F5 se queda sin la confirmación, sin el
/// diálogo de colisión, sin la entrada de journal, sin el undo o sin la
/// puerta de policy — y no de golpe, sino el día que una de las dos rutas
/// cambie. Se comparan los MODALES, no una descripción de ellos: son lo que
/// se somete.
#[test]
fn un_drop_abre_el_mismo_modal_que_la_tecla_de_copiar() {
    let mut app = app_pintada(10);
    // Dos marcas a mano (con dos, la puerta abre el confirm de lista).
    for row in [1, 2] {
        let _ = mouse::handle(
            &mut app,
            ev_con(ABAJO, 5, FILA0 + row, KeyModifiers::CONTROL),
        );
    }
    assert_eq!(app.panes[0].marks_len(), 2);

    // Lo que somete el TECLADO con esta misma selección.
    app.open_transfer(TransferKind::Copy, 0, 1, None);
    let por_teclado = app.modal.take().expect("F5 abre modal");

    // Y ahora el ratón: pulsa SOBRE una fila marcada y suelta en el otro.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 35, FILA0 + 1));

    assert_eq!(
        app.modal.as_ref(),
        Some(&por_teclado),
        "el drop somete lo mismo que la tecla, o hay dos rutas de mutación"
    );
    assert_eq!(app.panes[0].marks_len(), 2, "no barrió: era transferencia");
    assert_eq!(app.panes[1].marks_len(), 0, "el destino, intacto");
}

/// El flag copiar/mover se lee AL SOLTAR: el MISMO arrastre acaba en un
/// modal de copia o en uno de movimiento según se tenga Mayús pulsado
/// cuando sube el botón. Es lo que deja cambiar de idea a mitad de gesto sin
/// mover (mutación destructiva en el origen) lo que se creía copiar.
#[test]
fn mayus_al_soltar_decide_copiar_o_mover() {
    let arrastra = |mods: KeyModifiers| {
        let mut app = app_pintada(10);
        let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
        let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, KeyModifiers::CONTROL));
        // La pulsación va SIN Mayús en los dos casos: solo cambia el release.
        let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 2));
        let _ = mouse::handle(&mut app, ev_con(ARRIBA, 35, FILA0 + 2, mods));
        match app.modal {
            Some(Modal::ConfirmTransfer { kind, .. }) => kind,
            otro => panic!("se esperaba un confirm de transferencia: {otro:?}"),
        }
    };
    assert_eq!(arrastra(KeyModifiers::NONE), TransferKind::Copy);
    assert_eq!(arrastra(KeyModifiers::SHIFT), TransferKind::Move);
}

/// Un arrastre que nace en una fila SIN marcar y cruza al otro panel se
/// promueve a transferencia de ESA fila —el arrastre más común de cualquier
/// file manager— y devuelve las marcas que barrió de camino. Lo que viaja es
/// la fila del press, no las once marcas que el pane pudiera tener.
#[test]
fn un_arrastre_promovido_lleva_su_fila_y_devuelve_lo_que_barrio() {
    let mut app = app_pintada(10);
    // Una marca previa, ajena al gesto.
    let _ = mouse::handle(
        &mut app,
        ev_con(ABAJO, 5, FILA0 + FILAS - 1, KeyModifiers::CONTROL),
    );
    assert_eq!(app.panes[0].marks_len(), 1);

    // Press en una fila SIN marcar, barrido de camino, y cruce al otro panel.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 3));
    assert_eq!(app.panes[0].marks_len(), 4, "barrió 1..=3 de camino");
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    assert_eq!(
        app.panes[0].marks_len(),
        1,
        "al cruzar devuelve lo barrido: solo queda la marca previa"
    );
    let _ = mouse::handle(&mut app, ev(ARRIBA, 35, FILA0 + 1));

    let expected = app.panes[0].entries()[1].path.clone();
    let Some(Modal::TransferName {
        from, from_marks, ..
    }) = &app.modal
    else {
        panic!("un solo ítem: nombre editable, como F5 con una entrada");
    };
    assert_eq!(from, &expected, "la fila del press, no la marca ajena");
    assert!(!from_marks, "el envío no puede consumir una marca ajena");
    assert_eq!(app.panes[0].marks_len(), 1, "y sigue intacta");
}

/// Soltar sobre el PANE DE ORIGEN no somete nada: es un no-op explícito, no
/// una copia de un directorio sobre sí mismo, y lo pide quien se arrepintió
/// a medio arrastre y volvió a casa.
#[test]
fn soltar_en_el_panel_de_origen_no_somete_nada() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 2)); // pasea…
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + 6)); // …y vuelve
    assert!(app.modal.is_none(), "ni modal ni transferencia");
    assert_eq!(app.panes[0].marks_len(), 1, "la marca sigue puesta");
}

/// Un arrastre CANCELADO deja la selección exactamente como estaba. Es la
/// mitad del contrato que hace aceptable que el gesto signifique dos cosas
/// según dónde acabe: abortarlo tiene que devolver el estado de antes.
///
/// Se cancela soltando sobre el CROMO (la barra de estado), que es donde
/// acaba el gesto de quien se arrepiente: soltar fuera de toda fila no
/// adivina un destino.
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

    // Press en una fila sin marcar, barre, cruza (promueve) y suelta en la
    // barra de estado, que no pertenece a ningún pane.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 3));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, H - 1));

    assert!(app.modal.is_none(), "cancelar no somete nada");
    assert_eq!(marked(&app), before, "las marcas, exactamente las de antes");
}

/// El aviso de la barra sale de `Drag::pending`, la MISMA fuente que lee el
/// release, así que no puede prometer una cosa y el drop hacer otra: se
/// contrasta el texto pendiente con el modal que abre soltar ahí mismo.
///
/// Y no se anuncia nada mientras el gesto está en casa (soltar ahí es un
/// no-op: prometer una copia que no va a ocurrir es peor que no prometer
/// nada) ni cuando el gesto es un barrido.
#[test]
fn la_barra_anuncia_lo_que_haria_soltar_ahora() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 3, KeyModifiers::CONTROL));

    // Barrido en casa: nada que anunciar.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 8));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 9));
    assert_eq!(mouse::drop_hint(&app), None, "marcando no se promete nada");

    // Transferencia todavía sobre su propio panel: tampoco.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 5));
    assert_eq!(mouse::drop_hint(&app), None, "en casa soltar es un no-op");

    // Sobre el otro panel: dice cuántas y que COPIA…
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 1));
    let copia = mouse::drop_hint(&app).expect("hay drop pendiente");
    assert!(copia.contains('2'), "las dos marcas: {copia}");
    // El destino con el MISMO saneado que la cabecera del pane (regla 1).
    let (dest, _) = norte_frontend::path_display_with(app.panes[1].dir(), None);
    assert_eq!(
        copia,
        norte_i18n::ta("drag-copy", &[("n", "2"), ("to", &dest)]),
    );
    // …y la barra lo PINTA (por encima de cualquier mensaje pendiente).
    app.message = Some("un mensaje cualquiera".to_owned());
    assert!(
        pintar(&mut app).last().expect("barra de estado").contains(
            copia
                .split_once("  ")
                .map_or(copia.as_str(), |(head, _)| head)
        ),
        "el aviso manda sobre la barra mientras dura el arrastre"
    );

    // Con Mayús, MOVER — y el drop hace lo prometido.
    let mut con_mayus = ev(ARRASTRE, 35, FILA0 + 2);
    con_mayus.modifiers = KeyModifiers::SHIFT;
    let _ = mouse::handle(&mut app, con_mayus);
    let mover = mouse::drop_hint(&app).expect("sigue habiendo drop");
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
        "el drop hace exactamente lo que el aviso prometía: {:?}",
        app.modal
    );
    assert_eq!(mouse::drop_hint(&app), None, "y al soltar deja de anunciar");
}

/// Marcar con el ratón bajo un quick search en modo FILTRO no alcanza lo
/// que el filtro esconde.
///
/// Es la regla que `mark_range`/`set_mark`/`apply_sweep` documentan y la que
/// impide que la siguiente copia o borrado se ensanche sobre ficheros que el
/// usuario no estaba viendo. Cae en cuanto alguien cierre el filtro ANTES de
/// aplicar los efectos del gesto: entonces un shift+click marca también
/// todos los índices intermedios ocultos, en silencio, y el fallo solo se
/// nota al confirmar la operación.
///
/// El ancla del rango es además la fila RESALTADA (la selección del filtro),
/// no el cursor real, que bajo un filtro puede estar en cualquier parte.
#[test]
fn marcar_bajo_un_filtro_no_alcanza_lo_que_el_filtro_esconde() {
    let dir = vp("file:///casa");
    // Nombres alternos: el filtro `sí` deja visibles los índices PARES, así
    // que entre dos filas pintadas siempre hay una escondida.
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
    // El cursor REAL se queda lejos del ancla pintada a propósito.
    app.panes[0].set_cursor(5);
    let _ = pintar(&mut app);
    assert_eq!(
        app.panes[0].quick_visible(),
        Some(&[0, 2, 4][..]),
        "tres filas pintadas de seis entradas"
    );

    // shift+click sobre la TERCERA fila pintada: rango desde el ancla
    // pintada (la primera) hasta ella.
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::SHIFT));
    assert!(
        app.panes[0].quick_visible().is_some(),
        "un gesto de marcado NO cierra el filtro"
    );
    assert_eq!(
        app.panes[0].marks_len(),
        3,
        "las tres visibles, no las cinco del rango absoluto"
    );
    let marked: Vec<bool> = app.panes[0]
        .entries()
        .iter()
        .map(|e| app.panes[0].is_marked(e))
        .collect();
    assert_eq!(marked, [true, false, true, false, true, false]);
}

/// Un click LIMPIO sí cierra el filtro, y por eso puede: no marca nada. El
/// cursor aterriza en la entrada pulsada (índice ABSOLUTO), así que la
/// siguiente operación actúa sobre la fila que se pulsó y no sobre la que el
/// filtro tenía seleccionada.
#[test]
fn un_click_limpio_cierra_el_quick_search_sobre_la_fila_pulsada() {
    let mut app = app_pintada(20);
    app.panes[0].quick_start(norte_tui::nav::Mode::Filter);
    app.panes[0].quick_char('f');
    let _ = pintar(&mut app);
    let expected = app.mouse.geometry().expect("geometría")[0].offset + 2;

    let hit = mouse::hit_test(&app, 5, FILA0 + 2).expect("dentro del pane");
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    assert!(app.panes[0].quick_visible().is_none(), "filtro cerrado");
    assert_eq!(app.panes[0].cursor(), hit.index.expect("fila"));
    assert_eq!(app.panes[0].cursor(), expected, "el índice es el ABSOLUTO");
    assert_eq!(app.panes[0].marks_len(), 0, "y no marcó nada");
}

/// Con un overlay abierto el ratón no toca nada: los panes siguen pintados
/// DEBAJO, así que la geometría resolvería una fila perfectamente — y
/// movería el cursor de un listado que el usuario no está mirando mientras
/// un modal le pregunta otra cosa. El teclado ya se enruta así.
#[test]
fn con_un_overlay_abierto_el_raton_no_toca_los_panes() {
    let mut app = app_pintada(5);
    app.modal = Some(norte_tui::app::Modal::ConfirmQuit);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 3));
    assert_eq!(app.panes[0].cursor(), 0);
    assert_eq!(app.panes[0].marks_len(), 0);
}

/// Un release que se COMIÓ otro pump no puede dejar el gesto armado.
///
/// Los `select!` internos (el del cd, `on_tick`, `refresh_panes`, el del
/// viewer) filtran `Event::Key` y tiran el resto, así que un botón soltado
/// mientras corren no llega nunca. Sin caducidad, la siguiente motion
/// —minutos después, en otro directorio— continuaría aquel barrido y
/// marcaría filas que el usuario ni ve; y nada acota eso en el tiempo.
///
/// Caduca donde deja de ser verdad: el listado se movió, así que los
/// índices del gesto ya no nombran lo que se pintó.
#[test]
fn un_release_que_se_comio_otro_pump_no_deja_el_gesto_armado() {
    let mut app = app_pintada(10);
    let dir = app.panes[0].dir().clone();
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 2));
    let marks = app.panes[0].marks_len();
    assert!(marks > 0, "el barrido iba en marcha");

    // …el release cae dentro de un pump que solo mira teclas: jamás llega.
    // Lo que sí pasa es que ese pump refresca el listado.
    app.panes[0].refresh_listing(entradas(&dir, 10));
    let _ = pintar(&mut app);

    let after = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + FILAS - 1));
    assert_eq!(
        app.panes[0].marks_len(),
        after,
        "la motion no continúa un barrido que ya no existe"
    );
    let _ = mouse::handle(&mut app, ev(ARRIBA, 5, FILA0 + FILAS - 1));
    assert_eq!(app.panes[0].marks_len(), after, "ni el release tardío");
}

/// Un click de ANTES de un cd y otro de después no son un doble click.
///
/// Los dos caen sobre la misma celda —la fila 2 del pane— y pueden caer
/// dentro de la misma ventana de 400 ms, pero entre medias el pane cambió de
/// directorio: la segunda fila 2 es otro fichero. Emparejarlos entra en un
/// directorio que nadie eligió, y es de las cosas más difíciles de explicar
/// («hice click dos veces y se metió en una carpeta que no toqué»).
#[test]
fn un_click_antes_y_otro_despues_de_un_cd_no_son_un_doble_click() {
    let mut app = app_pintada(10);
    let t0 = std::time::Instant::now();
    assert_eq!(
        mouse::handle_at(&mut app, ev(ABAJO, 5, FILA0 + 2), t0),
        After::Nothing
    );

    // cd: el pane pasa a otro listado (el camino real de `nav.enter`).
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
        "misma celda y 80 ms, pero ya no es la misma fila"
    );
}

/// Un modal abierto a mitad de un arrastre también se lleva el gesto: para
/// cuando el usuario responda, el arrastre es historia.
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
    assert_eq!(app.panes[0].marks_len(), marks, "gesto muerto");
}

/// Y lo que NO debe caducar: un frame normal, sin nada que se mueva, deja el
/// gesto vivo. Sin esto la caducidad sería «cancelar siempre», que pasa los
/// dos tests de arriba y rompe todos los arrastres.
#[test]
fn un_frame_normal_no_caduca_el_gesto() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = pintar(&mut app);
    let _ = pintar(&mut app);
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 4));
    assert_eq!(app.panes[0].marks_len(), 4, "el barrido sigue vivo");
}

/// Un `pane.swap` a mitad de un arrastre también se lleva el gesto.
///
/// `listing_epoch` VIAJA con el pane, así que el intercambio se limita a
/// cruzar los dos valores: cuando EMPATAN —los dos panes habiendo listado el
/// mismo número de veces, lo normal recién arrancado— la vigencia por épocas
/// no ve nada moverse y el gesto sobrevive. Pero su `Spot { pane, index }`
/// nombra ahora el contenido del OTRO lado: el gesto quedó reatribuido a
/// espaldas del lector.
#[test]
fn un_intercambio_de_panes_a_mitad_de_un_arrastre_se_lleva_el_gesto() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + 2));
    assert!(app.panes[0].marks_len() > 0, "el barrido iba en marcha");
    assert_eq!(
        app.panes[0].listing_epoch(),
        app.panes[1].listing_epoch(),
        "las épocas EMPATAN: es justo lo que deja ciego al chequeo por épocas"
    );

    app.swap_panes();
    let _ = pintar(&mut app);

    // Las marcas del barrido viajaron con su pane al lado 1; el pane 0 es
    // ahora el otro listado, y el gesto armado sigue nombrando `pane: 0`.
    let before = app.panes[0].marks_len();
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 5, FILA0 + FILAS - 1));
    assert_eq!(
        app.panes[0].marks_len(),
        before,
        "el arrastre no puede continuar sobre el contenido del otro lado"
    );
}

/// La captura se SUELTA antes de ceder la terminal a un programa externo y
/// se restituye al volver. Sin esto el programa lanzado (un editor, un
/// paginador) hereda una terminal en modo ratón que no pidió y recibe cada
/// movimiento del puntero como si fueran teclas.
#[test]
fn la_captura_se_suelta_y_se_restaura_alrededor_del_suspend() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    cap.set(true, &mut out).expect("activar");
    assert!(cap.active());

    out.clear();
    let habia = mouse::release_for_suspend(&mut cap, &mut out).expect("soltar");
    assert!(habia, "estaba puesta");
    assert!(
        !cap.active(),
        "cedida la terminal, la captura no es nuestra"
    );
    assert!(!out.is_empty(), "se le dijo al terminal, no solo al struct");

    mouse::restore_after_suspend(&mut cap, habia, &mut out).expect("restaurar");
    assert!(cap.active(), "al volver, como estaba");
}

/// Y si NO estaba puesta (`[ui] mouse = false`), volver del programa
/// externo no se la enciende: el suspend restituye el estado, no un default.
#[test]
fn el_suspend_no_enciende_una_captura_que_estaba_apagada() {
    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    let habia = mouse::release_for_suspend(&mut cap, &mut out).expect("soltar");
    assert!(!habia);
    mouse::restore_after_suspend(&mut cap, habia, &mut out).expect("restaurar");
    assert!(!cap.active());
    assert!(out.is_empty(), "ni una secuencia por un no-op");
}

/// `[ui] mouse = false`: ni captura ni manejo.
///
/// Las dos mitades, porque son dos mecanismos. La captura: `set(false)` no
/// escribe NADA en la terminal y deja el estado apagado, así que el
/// emulador nunca reporta eventos de ratón y el brazo `Event::Mouse` del
/// run loop no llega a correr. Y el manejo: sin geometría —lo que también
/// ocurre antes del primer frame y con el visor abierto— ningún evento que
/// se colara resolvería fila alguna.
#[test]
fn con_mouse_false_no_hay_captura_ni_manejo() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("norte.toml"), "[ui]\nmouse = false\n").expect("escribir");
    let cfg = norte_config::load(&norte_config::Layers {
        dirs: vec![(dir.path().to_path_buf(), norte_config::Layer::User)],
    })
    .expect("carga");
    assert_eq!(cfg.ui_mouse, Some(false));

    let mut cap = mouse::Capture::new();
    let mut out: Vec<u8> = Vec::new();
    cap.set(cfg.ui_mouse.unwrap_or(true), &mut out)
        .expect("aplicar");
    assert!(!cap.active(), "sin captura");
    assert!(out.is_empty(), "nada escrito al terminal");

    let mut app = app_pintada(5);
    mouse::after_frame(&mut app, None, mouse::FrameZones::default());
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 3));
    assert_eq!(
        app.panes[0].cursor(),
        0,
        "sin geometría no se resuelve nada"
    );
    assert_eq!(app.focus(), 0);
}

/// **Review MAJOR-1.** El panel de diferencias (`Shift+F2`) SUSTITUYE a los
/// dos panes en pantalla. Sin declararlo overlay, la geometría de los panes
/// seguía siendo válida y el ratón resolvía filas de un listado que el lector
/// no puede ver: la rueda movía su cursor, un click marcaba entradas, y un
/// DOBLE click pedía un `nav.enter` de verdad — un `cd` en un pane invisible,
/// con el panel todavía abierto sobre unas raíces que ya no describen a nadie.
///
/// `keyboard_owner` ya lo declara dueño del teclado; esto es la otra mitad de
/// la misma pieza, y el rustdoc de `overlay_open` es donde está escrita la
/// regla: «el ratón hace lo mismo, de una pieza».
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
    // Un click cualquiera, y el doble click que sería un `cd`.
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
        "un doble click NO puede pedir un nav.enter en un pane que no se ve"
    );
    assert_eq!(app.panes[0].cursor(), cursor_antes, "ni mover su cursor");
    assert_eq!(
        [app.panes[0].dir().clone(), app.panes[1].dir().clone()],
        dir_antes
    );
    assert!(app.compare.is_some(), "y el panel sigue donde estaba");
}

/// La ventana pegajosa, END-TO-END: `End` y luego subir hasta arriba tiene que
/// dejar el listado enseñando el principio, no clavado donde estaba.
#[test]
fn subir_desde_el_final_acaba_arrastrando_la_ventana() {
    let mut app = app_pintada(60);
    app.panes[0].move_to_end();
    let _ = pintar(&mut app);
    let abajo = app.mouse.geometry().expect("geometría")[0].offset;
    assert!(abajo > 0, "el final desplaza la ventana: {abajo}");

    // Sube UNA fila: la ventana no se mueve (el cursor va dentro).
    app.panes[0].move_up(1);
    let _ = pintar(&mut app);
    assert_eq!(
        app.mouse.geometry().expect("geometría")[0].offset,
        abajo,
        "subir dentro de la ventana no la mueve"
    );

    // Y hasta arriba del todo: la ventana acaba en 0.
    for _ in 0..60 {
        app.panes[0].move_up(1);
    }
    let _ = pintar(&mut app);
    assert_eq!(app.panes[0].cursor(), 0);
    assert_eq!(
        app.mouse.geometry().expect("geometría")[0].offset,
        0,
        "el cursor arriba del todo tiene que verse"
    );
}

/// El borde entre los dos panes se ARRASTRA, y lo que uno gana lo pierde el
/// otro.
///
/// El arrastre escribe en el ÁRBOL, que es lo que la sesión guarda: por eso un
/// borde movido sigue donde se dejó al volver a abrir, sin nada más.
#[test]
fn arrastrar_el_borde_mueve_la_frontera_entre_los_panes() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    let antes = app.mouse.geometry().expect("geometría").to_vec();
    let (izq_antes, der_antes) = (antes[0].width, antes[1].width);
    let borde = antes[0].x + antes[0].width;

    // Agarrar el borde y llevarlo seis celdas a la izquierda. Seis y no
    // veinte: un `browser` declara veinte columnas de mínimo, y por debajo el
    // reparto COLAPSA su split — el pane no encoge, desaparece. Lo que este
    // test mide es el arrastre, no el colapso.
    let destino = borde - 6;
    let _ = mouse::handle(&mut app, ev(ABAJO, borde, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, destino, FILA0));
    let _ = mouse::handle(&mut app, ev(ARRIBA, destino, FILA0));
    let _ = pintar(&mut app);

    let ahora = app.mouse.geometry().expect("geometría").to_vec();
    assert!(
        ahora[0].width < izq_antes,
        "el de la izquierda encoge: {izq_antes} -> {}",
        ahora[0].width
    );
    assert!(
        ahora[1].width > der_antes,
        "y lo que pierde lo gana el otro: {der_antes} -> {}",
        ahora[1].width
    );
    assert_eq!(
        ahora[0].width + ahora[1].width,
        izq_antes + der_antes,
        "la pareja ocupa lo mismo: arrastrar un borde no toca al resto"
    );
}

/// Y agarrar el borde no señala ni marca nada: agarrar no es elegir.
#[test]
fn agarrar_el_borde_no_selecciona_una_fila() {
    let mut app = app_pintada(5);
    let _ = pintar(&mut app);
    let cursor = app.panes[0].cursor();
    let geom = app.mouse.geometry().expect("geometría").to_vec();
    let borde = geom[0].x + geom[0].width;
    let _ = mouse::handle(&mut app, ev(ABAJO, borde, FILA0 + 1));
    let _ = mouse::handle(&mut app, ev(ARRIBA, borde, FILA0 + 1));
    assert_eq!(app.panes[0].cursor(), cursor, "el cursor no se movió");
    assert_eq!(app.panes[0].marks_len(), 0, "y no se marcó nada");
}

/// Pulsar un listado le da también el TECLADO, no solo el foco.
///
/// Con el sidebar delante, un click en el listado movía su cursor y dejaba
/// las flechas en el sidebar: el borde de foco decía una cosa y el teclado
/// iba a otra. Señalar un panel con el ratón es decir «ahora trabajo aquí»,
/// y eso incluye las teclas.
#[test]
fn un_click_en_un_listado_trae_el_teclado_desde_el_sidebar() {
    let mut app = app_pintada(5);
    app.toggle_places();
    let _ = pintar_en(&mut app, 100, 30);
    assert_eq!(app.key_owner(), KeyOwner::Places, "el sidebar lo tomó");

    let g = app.mouse.geometry().expect("geometría")[0];
    let after = mouse::handle(&mut app, ev(ABAJO, g.x + 2, g.first_list_row));
    assert_eq!(after, After::Nothing);
    assert_eq!(app.key_owner(), KeyOwner::Panes, "y el click lo trae");
    assert_eq!(app.focus(), 0);
}

/// Y al revés: pulsar un panel LATERAL le da a él el teclado, aunque ahí no
/// haya ninguna fila que resolver.
///
/// El mismo gesto para los dos lados, y por el mismo camino: quien decide de
/// quién es el teclado es el hueco que hay bajo el puntero.
#[test]
fn un_click_en_un_panel_lateral_le_da_el_teclado() {
    let mut app = app_pintada(5);
    app.toggle_processes();
    app.return_keys_to_panes();
    let _ = pintar_en(&mut app, 100, 30);

    let area = ratatui::layout::Rect::new(0, 0, 100, 30);
    let id = app.processes_slot().expect("abierto");
    let r = ui::resolved_for(&app, area)
        .placements
        .iter()
        .find(|(s, _)| *s == id)
        .map(|(_, r)| *r)
        .expect("el panel se colocó");
    let _ = mouse::handle(&mut app, ev(ABAJO, r.x + 1, r.y + 1));
    assert_eq!(app.key_owner(), KeyOwner::Processes);
}

/// Un plugin de prueba del gestor, aprobado y encendido.
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
        has_help: false,
        manifest_digest: None,
    }
}

/// Una `App` con el gestor de extensiones abierto sobre dos plugins, pintada
/// a `w`×`h`. Devuelve las líneas para contrastar las zonas con el texto.
fn app_con_gestor(w: u16, h: u16) -> (App, Vec<String>) {
    let mut app = app_pintada(3);
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![
            plugin("org.norte.uno", "Uno", "columns"),
            plugin("org.norte.dos", "Dos", "previewer"),
        ],
        errors: Vec::new(),
        cursor: 0,
        config: None,
    });
    let lineas = pintar_en(&mut app, w, h);
    (app, lineas)
}

/// La fila y la columna de la primera aparición de `texto` en lo pintado.
///
/// `TestBackend::to_string()` envuelve cada fila entre comillas: la primera
/// celda de la pantalla es el segundo carácter de la línea.
fn donde(lineas: &[String], texto: &str) -> (u16, u16) {
    for (y, l) in lineas.iter().enumerate() {
        let l = l.strip_prefix('"').unwrap_or(l);
        if let Some(byte) = l.find(texto) {
            let col = l[..byte].chars().count();
            return (
                u16::try_from(y).expect("fila"),
                u16::try_from(col).expect("columna"),
            );
        }
    }
    panic!("{texto:?} no está pintado:\n{}", lineas.join("\n"));
}

/// El gestor de extensiones nació mudo al ratón: `overlay_open` devolvía
/// `Nothing` para todo. Un clic en una fila la elige, y en la fila YA
/// elegida abre sus ajustes — lo que su pie promete («pulsa Intro, o la
/// fila»). Contrastado contra el TEXTO pintado: la fila que se pulsa es la
/// que enseña «Dos».
#[test]
fn clic_en_una_fila_del_gestor_la_elige_y_repetirlo_abre_sus_ajustes() {
    let (mut app, lineas) = app_con_gestor(100, 24);
    let (row, col) = donde(&lineas, "Dos v1.0.0");
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, col, row)),
        After::Nothing,
        "elegir no habla con el backend"
    );
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, col, row)),
        After::Extension("dialog.confirm"),
        "la fila ya elegida abre sus ajustes por el MISMO comando que Intro"
    );
}

/// Los botones de la ficha disparan EL MISMO comando que su tecla, y salen
/// de lo pintado: el ratón encuentra «[Apagar]» donde el frame lo puso.
#[test]
fn los_botones_de_la_ficha_disparan_el_comando_de_su_tecla() {
    let (mut app, lineas) = app_con_gestor(100, 24);
    // Los tests de este fichero pintan en inglés (el locale por defecto sin
    // `[ui] lang`), así que las etiquetas son las de `en.ftl`.
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
    // Y entre dos botones no hay nada: el espacio no es un botón.
    let (row, col) = donde(&lineas, "[Disable] [Revoke]");
    let hueco = col + u16::try_from("[Disable]".len()).unwrap();
    assert_eq!(
        mouse::handle(&mut app, ev(ABAJO, hueco, row)),
        After::Nothing
    );
}

/// La rueda mueve el cursor de la lista, y elegir OTRA fila con los
/// ajustes de la anterior abiertos los cierra: la ficha no puede enseñar un
/// plugin y los ajustes de otro.
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
    assert!(mgr.config.is_none(), "los ajustes eran de «Uno»");
}

/// En un terminal estrecho no hay ficha ni botones, pero las filas de la
/// lista de siempre siguen siendo pulsables — con la descripción debajo,
/// que NO es una fila.
#[test]
fn en_estrecho_las_filas_se_pulsan_y_la_descripcion_no() {
    let (mut app, lineas) = app_con_gestor(W, H);
    let (row, col) = donde(&lineas, "Dos v1.0.0");
    let _ = mouse::handle(&mut app, ev(ABAJO, col, row));
    assert_eq!(app.extensions.as_ref().unwrap().cursor, 1);
    // La cabecera de categoría de encima no es un plugin.
    let (row, col) = donde(&lineas, "previewer");
    let _ = mouse::handle(&mut app, ev(ABAJO, col, row));
    assert_eq!(
        app.extensions.as_ref().unwrap().cursor,
        1,
        "una cabecera no elige nada"
    );
}
