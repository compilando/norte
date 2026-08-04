//! Ratón de la TUI: hit test contra un layout REAL (se pinta y se resuelve
//! contra lo pintado, no contra una geometría inventada aquí), rueda,
//! captura alrededor del opener externo y `[ui] mouse = false`.
//!
//! Los gestos en sí (qué marca un barrido, cuándo es transferencia) tienen
//! sus propios tests en `norte-frontend::mouse`: aquí se comprueba lo que
//! es de la TERMINAL — qué celda es qué fila y quién tiene la captura.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
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
const FILA0: u16 = 2;
/// Cuántas filas de listado caben.
const FILAS: u16 = 8;

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
    let mut terminal = Terminal::new(TestBackend::new(W, H)).expect("terminal de test");
    let frame = terminal.draw(|f| ui::draw(f, app)).expect("draw");
    let geometria = ui::pane_geometry(app, frame.area);
    app.mouse.set_geometry(geometria);
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
fn assert_fila(lineas: &[String], app: &App, row: u16, index: usize) {
    let entrada = &app.panes[0].entries()[index];
    let nombre = String::from_utf8_lossy(
        entrada
            .path
            .file_name()
            .expect("una entrada de test tiene nombre")
            .as_bytes(),
    )
    .into_owned();
    let pintada = &lineas[usize::from(row)];
    assert!(
        pintada.contains(&format!("{nombre} ")),
        "la fila {row} debería pintar `{nombre}` (índice {index}) y pinta: {pintada}"
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

#[test]
fn el_layout_de_estos_tests_es_el_que_se_pinta() {
    // Ancla de los números de arriba: si el pane gana o pierde cromo, cae
    // ESTE test con un mensaje claro, y no los seis siguientes con
    // aritmética confusa.
    let mut app = app_pintada(5);
    let lineas = pintar(&mut app);
    let [izq, der] = *app.mouse.geometry().expect("hay geometría");
    assert_eq!((izq.x, izq.y, izq.width, izq.height), (0, 0, 30, 11));
    assert_eq!((der.x, der.y, der.width, der.height), (30, 0, 30, 11));
    assert_eq!(izq.first_list_row, FILA0, "borde superior + cabecera");
    assert_eq!(izq.list_rows, FILAS, "interior menos la cabecera");
    assert_eq!(izq.offset, 0, "cursor en la primera: sin scroll");
    // Y lo que de verdad hay PINTADO en esas filas. El indicador de orden
    // (`▲`) en vez del rótulo de la columna: el rótulo está traducido y
    // estos tests no fijan idioma.
    assert!(
        lineas[1].contains('▲'),
        "fila 1 = cabecera de columnas: {}",
        lineas[1]
    );
    assert_fila(&lineas, &app, FILA0, 0);
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
    let hit = mouse::hit_test(&app, 5, 1).expect("sigue siendo el pane");
    assert_eq!(hit.pane, 0);
    assert_eq!(hit.index, None);
}

/// El borde superior (donde va el título con la ruta) y el inferior (donde
/// se pinta el input del quick search) tampoco son filas.
#[test]
fn los_bordes_del_pane_no_son_filas() {
    let app = app_pintada(5);
    for row in [0, FILA0 + FILAS] {
        let hit = mouse::hit_test(&app, 5, row).expect("dentro del bloque");
        assert_eq!(hit.index, None, "fila {row} es borde");
    }
    for col in [0, 29] {
        let hit = mouse::hit_test(&app, col, FILA0).expect("dentro del bloque");
        assert_eq!(hit.index, None, "columna {col} es borde lateral");
    }
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
    let lineas = pintar(&mut app);
    let offset = app.mouse.geometry().expect("geometría")[0].offset;
    assert_eq!(offset, 21 - usize::from(FILAS), "el cursor va al borde");
    let hit = mouse::hit_test(&app, 5, FILA0).expect("dentro del pane");
    assert_eq!(hit.index, Some(offset), "la primera fila PINTADA");
    // Contra el buffer: la fila que se resuelve es la que se ve.
    assert_fila(&lineas, &app, FILA0, offset);
    let hit = mouse::hit_test(&app, 5, FILA0 + FILAS - 1).expect("dentro del pane");
    assert_eq!(hit.index, Some(20), "la última pintada es el cursor");
    assert_fila(&lineas, &app, FILA0 + FILAS - 1, 20);
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

/// Arrastrar una selección al otro panel NO copia todavía en la TUI (el
/// drop es la tarea 5 del plan, y solo llega a la GUI), pero LO DICE. Un
/// gesto que no hace nada y además no explica nada se lee como que el ratón
/// está roto, y el usuario lo repite. Además: no marca nada por el camino
/// —el gesto era una transferencia, no un barrido— y no toca el destino.
#[test]
fn arrastrar_marcas_al_otro_panel_avisa_en_vez_de_no_hacer_nada() {
    let mut app = app_pintada(10);
    let _ = mouse::handle(&mut app, ev_con(ABAJO, 5, FILA0 + 2, KeyModifiers::CONTROL));
    assert_eq!(app.panes[0].marks_len(), 1, "una marca a mano");
    app.message = None;

    // Pulsa SOBRE la fila marcada y suelta en el otro panel.
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRASTRE, 35, FILA0 + 2));
    let _ = mouse::handle(&mut app, ev(ARRIBA, 35, FILA0 + 2));

    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("msg-mouse-transfer-unavailable").as_str()),
        "el gesto se explica en la barra"
    );
    assert_eq!(app.panes[0].marks_len(), 1, "no barrió: era transferencia");
    assert_eq!(app.panes[1].marks_len(), 0, "el destino, intacto");
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
    let marcadas: Vec<bool> = app.panes[0]
        .entries()
        .iter()
        .map(|e| app.panes[0].is_marked(e))
        .collect();
    assert_eq!(marcadas, [true, false, true, false, true, false]);
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
    let esperado = app.mouse.geometry().expect("geometría")[0].offset + 2;

    let hit = mouse::hit_test(&app, 5, FILA0 + 2).expect("dentro del pane");
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 2));
    assert!(app.panes[0].quick_visible().is_none(), "filtro cerrado");
    assert_eq!(app.panes[0].cursor(), hit.index.expect("fila"));
    assert_eq!(app.panes[0].cursor(), esperado, "el índice es el ABSOLUTO");
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
    app.mouse.set_geometry(None);
    let _ = mouse::handle(&mut app, ev(ABAJO, 5, FILA0 + 3));
    assert_eq!(
        app.panes[0].cursor(),
        0,
        "sin geometría no se resuelve nada"
    );
    assert_eq!(app.focus(), 0);
}
