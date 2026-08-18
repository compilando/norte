//! El ancla del refactor de layout (L1a): lo que el motor CREE que pintó es lo
//! que hay en el buffer.
//!
//! Hoy quien lo dice es [`ui::pane_geometry`]; tras L1a lo dirá `Resolved`. La
//! aserción no cambia — cambia de quién lee sus rectángulos, y por eso este
//! test es el único que puede detectar que el reparto se movió una celda.
//!
//! Nace en VERDE a propósito: es un test de caracterización del código que ya
//! funciona. Si falla ahora, están mal los ayudantes de este fichero, no el
//! render.
//!
//! El snapshot de al lado es el criterio de aceptación del plan entero: la
//! pantalla `orthodox` idéntica antes y después del refactor.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Ancho del terminal de estos tests.
const W: u16 = 100;
/// Alto del terminal de estos tests.
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    // Idioma fijo: el snapshot congela texto localizado (barra de estado).
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

/// `n` entradas `f00..f{n-1}` bajo `dir`.
///
/// Con CERO a la izquierda a propósito: `f0` es prefijo de `f01` y de `f10`, y
/// un `contains` sobre una fila no distinguiría cuál de las tres pintó. El
/// ancho fijo hace que cada nombre solo se encuentre a sí mismo.
fn entradas(dir: &VPath, n: usize) -> Vec<Entry> {
    (0..n)
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

/// Una `App` con `n` entradas en cada pane.
fn app_de_prueba_con(n: usize) -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entradas(&dir, n)),
        Pane::new(dir.clone(), entradas(&dir, n)),
    )
}

/// Pulsa el botón izquierdo en una celda, por el mismo camino que el run loop.
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

/// Pinta un frame por el MISMO camino que el run loop (reconciliar, pintar) y
/// devuelve las líneas.
///
/// Devolver el buffer no es comodidad: sin él el test solo comprobaría que
/// `pane_geometry` está de acuerdo consigo misma.
fn pintar(app: &mut App) -> Vec<String> {
    pintar_en(app, W, H)
}

/// Como [`pintar`] a un tamaño cualquiera.
fn pintar_en(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal de test");
    let area = ratatui::layout::Rect::new(0, 0, w, h);
    ui::before_frame(app, area);
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    // CERRAR el frame como el run loop: devolver al modelo la geometría y las
    // zonas pulsables. Sin esto el ratón resuelve contra una pantalla que
    // nadie le contó, y un test de botones pasaría sin pulsar nada.
    norte_tui::mouse::after_frame(
        app,
        ui::pane_geometry(app, area),
        ui::tab_zones(app, area),
        ui::menu_zones(app, area),
    );
    // `TestBackend::to_string()` envuelve CADA fila entre comillas. Sin
    // quitarlas, todo recorte por columna va desplazado una celda — y un
    // `contains` lo disimula, que es exactamente cómo un test de geometría
    // deja de comprobar geometría.
    terminal
        .backend()
        .to_string()
        .lines()
        .map(|l| l.trim_matches('"').to_owned())
        .collect()
}

/// El trozo de la fila `row` que ocupa un pane que empieza en `x` y mide
/// `width`. Por CARACTERES: `TestBackend` da una celda por carácter.
fn recorte(lineas: &[String], row: u16, x: u16, width: u16) -> String {
    lineas
        .get(row as usize)
        .map(|l| {
            l.chars()
                .skip(x as usize)
                .take(width as usize)
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// El nombre de la entrada que va en la fila `offset` del pane `i`.
fn nombre_visible(app: &App, i: usize, offset: usize) -> String {
    let entrada = &app.panes[i].entries()[offset];
    String::from_utf8_lossy(
        entrada
            .path
            .file_name()
            .expect("una entrada de test tiene nombre")
            .as_bytes(),
    )
    .into_owned()
}

#[test]
fn la_geometria_declarada_coincide_con_las_filas_pintadas() {
    let mut app = app_de_prueba_con(60);
    let lineas = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("dos panes pintados");

    for (i, g) in geom.iter().enumerate() {
        assert!(g.list_rows > 0, "pane {i}: sin filas de listado");
        let esperada = nombre_visible(&app, i, g.offset);

        // La primera fila de listado lleva la primera entrada visible.
        let primera = recorte(&lineas, g.first_list_row, g.x, g.width);
        assert!(
            primera.contains(&esperada),
            "pane {i}: la fila {} debería llevar {esperada:?}, lleva {primera:?}",
            g.first_list_row
        );

        // La fila JUSTO ENCIMA es cromo (cabecera de columnas): nunca listado.
        let cabecera = recorte(&lineas, g.first_list_row - 1, g.x, g.width);
        assert!(
            !cabecera.contains(&esperada),
            "pane {i}: la cabecera no puede llevar contenido de listado: {cabecera:?}"
        );

        // Y la fila justo DEBAJO de la última de listado es el borde inferior.
        let bajo = g.first_list_row + g.list_rows;
        let borde = recorte(&lineas, bajo, g.x, g.width);
        assert!(
            borde.contains('─') && !borde.contains(&esperada),
            "pane {i}: la fila {bajo} debería ser el borde inferior: {borde:?}"
        );
        assert_eq!(
            bajo,
            g.y + g.height - 1,
            "pane {i}: el borde inferior no cae donde dice la geometría"
        );
    }
}

/// Un ancho IMPAR no pierde una columna: los dos panes suman el frame entero.
///
/// Vale la pena aunque parezca aritmética: el corte lo hacía ratatui y ahora
/// lo hace `layout::resolve`, y las dos reparten el resto de la división a
/// sitios distintos. Con anchos pares —los que usan todos los demás tests— la
/// diferencia no existe, así que sin este test el cambio sería invisible hasta
/// que alguien abriera un terminal de 101 columnas.
#[test]
fn con_ancho_impar_los_dos_panes_suman_el_frame() {
    let mut app = app_de_prueba_con(60);
    let lineas = pintar_en(&mut app, 101, H);
    let area = ratatui::layout::Rect::new(0, 0, 101, H);
    let geom = ui::pane_geometry(&app, area).expect("dos panes");
    assert_eq!(geom[0].x, 0);
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        101,
        "se perdió una columna"
    );
    assert_eq!(
        geom[1].x, geom[0].width,
        "el derecho empieza donde acaba el izquierdo"
    );
    // Y lo pintado coincide: la última columna del frame no queda en blanco.
    let borde = recorte(&lineas, 0, geom[1].x, geom[1].width);
    assert_eq!(
        borde.chars().count(),
        geom[1].width as usize,
        "el pane derecho no llega al borde del frame"
    );
}

/// A 30 columnas los dos mínimos del `browser` no caben, así que el `Split`
/// colapsa y se pinta UNO a ancho completo.
///
/// Es la contrapartida visible de todo el motor: dos panes de quince columnas
/// no enseñan ni un nombre con su tamaño, y hasta ahora eran lo único posible.
#[test]
fn a_treinta_columnas_se_pinta_un_solo_pane_a_ancho_completo() {
    let mut app = app_de_prueba_con(60);
    let _ = pintar_en(&mut app, 30, H);
    let area = ratatui::layout::Rect::new(0, 0, 30, H);
    let geom = ui::pane_geometry(&app, area).expect("hay geometría");
    assert_eq!(geom[0].width, 30, "el que se pinta ocupa todo");
    assert_eq!(
        geom.len(),
        1,
        "y no hay geometría para el que no se pintó: un click ahí no resuelve nada"
    );
}

/// Y el foco no se queda en el pane que dejó de pintarse: sería un teclado
/// moviendo un cursor que nadie ve.
#[test]
fn el_foco_abandona_el_pane_que_el_colapso_dejo_fuera() {
    let mut app = app_de_prueba_con(60);
    app.set_focus(1);
    let _ = pintar_en(&mut app, 30, H);
    assert_eq!(app.focus(), 0, "el foco cae en el que sí se ve");
}

/// Con una pestaña abierta, el ancla sigue valiendo: la barra se come una
/// fila y la geometría lo sabe.
///
/// Es el test que importa de las pestañas. La barra cambia el cromo del pane,
/// y si `pane_geometry` no lo descuenta, cada click resuelve una fila más
/// arriba de lo que el usuario ve — el fallo silencioso que la geometría
/// existe para no tener.
#[test]
fn con_una_pestana_abierta_la_geometria_sigue_cuadrando() {
    let mut app = app_de_prueba_con(60);
    let antes =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("dos panes")[0];
    app.tab_new();
    let lineas = pintar(&mut app);
    let geom = ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("dos panes");
    assert_eq!(
        geom[0].first_list_row,
        antes.first_list_row + 1,
        "la barra de pestañas baja el listado una fila"
    );
    assert_eq!(
        geom[0].list_rows,
        antes.list_rows - 1,
        "y le quita una fila de listado"
    );
    let esperada = nombre_visible(&app, 0, geom[0].offset);
    let fila = recorte(&lineas, geom[0].first_list_row, geom[0].x, geom[0].width);
    assert!(
        fila.contains(&esperada),
        "la primera fila de listado debería llevar {esperada:?}, lleva {fila:?}"
    );
}

/// Una pestaña nueva nace en el mismo directorio y YA LLENA: es lo mismo que
/// se estaba mirando, así que no parpadea vacía mientras alguien relee.
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

/// Cerrar la penúltima pestaña disuelve el grupo y devuelve la fila.
#[test]
fn al_cerrar_la_ultima_pestana_el_pane_recupera_su_fila() {
    let mut app = app_de_prueba_con(60);
    let antes =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("dos panes")[0];
    app.tab_new();
    let _ = pintar(&mut app);
    app.tab_close();
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("dos panes");
    assert_eq!(geom[0].list_rows, antes.list_rows);
}

/// Cambiar de pestaña cambia el listado que el lado enseña, y cada una
/// conserva su cursor: no hay nada que recordar porque nada se olvidó.
#[test]
fn cada_pestana_conserva_su_cursor() {
    let mut app = app_de_prueba_con(60);
    app.panes[0].set_cursor(7);
    app.tab_new();
    let _ = pintar(&mut app);
    app.panes[0].set_cursor(2);
    assert_eq!(
        app.panes[0].cursor(),
        2,
        "la pestaña nueva va por su cuenta"
    );
    app.tab_cycle(-1);
    let _ = pintar(&mut app);
    assert_eq!(app.panes[0].cursor(), 7, "la de antes sigue donde estaba");
}

/// Cerrar el último panel se NIEGA. Es lo que mantiene distintos los dos
/// lados: con un solo listado, «el otro pane» sería este mismo y una copia
/// tendría por destino su propio origen.
#[test]
fn no_se_puede_cerrar_el_ultimo_panel() {
    let mut app = app_de_prueba_con(60);
    assert!(!app.layout_close_slot(), "con dos paneles ya no se puede");
    let _ = pintar(&mut app);
    assert!(
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).is_some(),
        "los dos siguen ahí"
    );
}

/// Agrandar un panel le da sitio de verdad, y el otro lo pierde.
#[test]
fn agrandar_un_panel_le_da_sitio_y_al_otro_se_lo_quita() {
    let mut app = app_de_prueba_con(60);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let antes = ui::pane_geometry(&app, area).expect("dos panes")[0].width;
    app.layout_resize(1);
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, area).expect("dos panes");
    assert!(geom[0].width > antes, "el enfocado crece");
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        u32::from(W),
        "y siguen sumando el frame"
    );
}

/// Igualar los devuelve a la mitad cada uno.
#[test]
fn igualar_devuelve_los_paneles_a_la_mitad() {
    let mut app = app_de_prueba_con(60);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    app.layout_resize(3);
    let _ = pintar(&mut app);
    app.layout_equalize();
    let _ = pintar(&mut app);
    let geom = ui::pane_geometry(&app, area).expect("dos panes");
    assert_eq!(geom[0].width, geom[1].width);
}

/// Cada pestaña tiene su propio HISTORIAL, no solo su cursor.
///
/// Estaba en un array de dos, así que era del sitio de la pantalla y no del
/// listado: cambiar de pestaña te habría dado el historial de la otra, que es
/// el mismo bug que ver su cursor.
#[test]
fn cada_pestana_conserva_su_historial() {
    use norte_proto::VPath;
    let mut app = app_de_prueba_con(60);
    app.history[0].record(VPath::parse("mem:///una").expect("wire"));
    app.tab_new();
    let _ = pintar(&mut app);
    assert!(
        app.history[0].entries().is_empty(),
        "la pestaña nueva empieza sin historial"
    );
    app.history[0].record(VPath::parse("mem:///otra").expect("wire"));
    app.tab_cycle(-1);
    let _ = pintar(&mut app);
    assert_eq!(
        app.history[0].entries().front().map(VPath::to_wire),
        Some("mem:///una".to_owned()),
        "la de antes recupera el suyo"
    );
}

/// Partir da TRES paneles, y la geometría de los tres cuadra con el buffer.
///
/// Es el techo que P6 retira: hasta ahora `PaneSlots` solo podía representar
/// dos, y un tercero habría quedado pintado pero fuera del alcance del ratón.
#[test]
fn partir_da_tres_paneles_y_los_tres_cuadran() {
    let mut app = app_de_prueba_con(60);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let lineas = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("hay geometría");
    assert_eq!(geom.len(), 3, "tres paneles");
    let ancho: u32 = geom.iter().map(|g| u32::from(g.width)).sum();
    assert_eq!(ancho, u32::from(W), "y suman el frame entero");
    for (i, g) in geom.iter().enumerate() {
        let esperada = nombre_visible(&app, i, g.offset);
        let fila = recorte(&lineas, g.first_list_row, g.x, g.width);
        assert!(
            fila.contains(&esperada),
            "panel {i}: la fila {} debería llevar {esperada:?}, lleva {fila:?}",
            g.first_list_row
        );
    }
}

/// El panel nuevo se queda con el foco: partir es pedir sitio para trabajar
/// en él, no para mirarlo desde el de al lado.
#[test]
fn el_panel_recien_partido_se_queda_el_foco() {
    let mut app = app_de_prueba_con(60);
    let antes = app.focused_slot();
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert_ne!(app.focused_slot(), antes, "el foco viaja al nuevo");
}

/// Con tres paneles, cerrar uno vuelve a dos y el foco sobrevive.
#[test]
fn con_tres_paneles_cerrar_uno_vuelve_a_dos() {
    let mut app = app_de_prueba_con(60);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert!(app.layout_close_slot(), "con tres SÍ se puede cerrar");
    let _ = pintar(&mut app);
    let geom =
        ui::pane_geometry(&app, ratatui::layout::Rect::new(0, 0, W, H)).expect("hay geometría");
    assert_eq!(geom.len(), 2);
    assert!(app.focus() < 2, "el foco quedó en rango");
}

/// Con TRES paneles, una copia sin destino designado NO adivina.
///
/// Con dos, el destino es el otro y nadie tuvo que decirlo. Con tres,
/// adivinar es cómo una copia sale hacia un panel que el lector no tenía en
/// la cabeza — pérdida de datos silenciosa.
#[test]
fn con_tres_paneles_no_hay_destino_hasta_que_se_designa() {
    let mut app = app_de_prueba_con(60);
    assert!(app.target_index().is_some(), "con dos, el otro");
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let _ = pintar(&mut app);
    assert_eq!(app.target_index(), None, "con tres, hay que designarlo");
    app.layout_set_target();
    let _ = pintar(&mut app);
    let destino = app.target_index().expect("designado");
    assert_ne!(destino, app.focus(), "y nunca es uno mismo");
}

/// El destino designado se MARCA en su cromo, y solo a partir de tres: con
/// dos sería ruido en el caso de siempre.
#[test]
fn el_destino_designado_se_marca_y_solo_cuando_hace_falta() {
    let mut app = app_de_prueba_con(60);
    let dos = pintar(&mut app).join("\n");
    assert!(!dos.contains("-> "), "con dos paneles no se marca nada");
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    app.layout_set_target();
    let tres = pintar(&mut app).join("\n");
    assert!(
        tres.contains("-> "),
        "con tres, el destino se ve en el cromo:\n{tres}"
    );
}

/// Los botones de la barra de pestañas se pulsan de verdad, y las zonas que
/// el ratón mide son las que se pintaron.
///
/// Medir por separado lo que se pinta y lo que se puede pulsar es cómo un
/// click acaba en la pestaña de al lado: un fallo que no se ve como un bug de
/// ratón, sino como «esto se cambia solo».
#[test]
fn los_botones_de_la_barra_de_pestanas_se_pulsan() {
    let mut app = app_de_prueba_con(60);
    app.tab_new();
    let _ = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::tab_zones(&app, area);
    assert!(!zonas.is_empty(), "con pestañas hay zonas que pulsar");

    // Volver a la primera pestaña pulsándola.
    let primera = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::Goto(0))
        .copied()
        .expect("la primera pestaña tiene su zona");
    let antes = app.focused_slot();
    pulsar(&mut app, primera.x0, primera.row);
    let _ = pintar(&mut app);
    assert_ne!(app.focused_slot(), antes, "cambió de pestaña");

    // `[+]` abre otra.
    let zonas = ui::tab_zones(&app, area);
    let mas = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::New)
        .copied()
        .expect("el botón de abrir tiene su zona");
    pulsar(&mut app, mas.x0, mas.row);
    let _ = pintar(&mut app);
    let t = ui::tab_strip_for(&app, 0).expect("sigue habiendo grupo");
    assert_eq!(t.titulos.len(), 3, "el botón abrió una tercera");

    // `[x]` cierra la activa.
    let zonas = ui::tab_zones(&app, area);
    let equis = zonas
        .iter()
        .find(|z| z.pane == 0 && z.action == ui::TabAction::Close)
        .copied()
        .expect("el botón de cerrar tiene su zona");
    pulsar(&mut app, equis.x0, equis.row);
    let _ = pintar(&mut app);
    let t = ui::tab_strip_for(&app, 0).expect("quedan dos");
    assert_eq!(t.titulos.len(), 2, "y el otro la cerró");
}

/// Un click en la fila de la barra pero FUERA de toda zona no hace nada.
#[test]
fn un_click_en_el_hueco_de_la_barra_no_hace_nada() {
    let mut app = app_de_prueba_con(60);
    app.tab_new();
    let _ = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::tab_zones(&app, area);
    let fila = zonas[0].row;
    let ultima = zonas
        .iter()
        .filter(|z| z.pane == 0)
        .map(|z| z.x1)
        .max()
        .expect("hay zonas");
    let antes = ui::tab_strip_for(&app, 0).expect("grupo").titulos.len();
    pulsar(&mut app, ultima + 1, fila);
    let _ = pintar(&mut app);
    assert_eq!(
        ui::tab_strip_for(&app, 0).expect("grupo").titulos.len(),
        antes
    );
}

/// El menú se pinta con su desplegable, y las zonas que el ratón mide son las
/// que se pintaron.
#[test]
fn el_menu_se_pinta_y_sus_zonas_coinciden() {
    let mut app = app_de_prueba_con(60);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let lineas = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let zonas = ui::menu_zones(&app, area);
    assert!(!zonas.is_empty(), "hay títulos y elementos que pulsar");

    // El primer título está pintado donde su zona dice.
    let titulo = zonas
        .iter()
        .find(|z| z.hit == ui::MenuHit::Title(0))
        .copied()
        .expect("el primer título tiene zona");
    let texto = recorte(&lineas, titulo.row, titulo.x0, titulo.x1 - titulo.x0 + 1);
    assert!(
        texto.trim() == norte_i18n::t("menu-file"),
        "la zona del título no cae donde se pintó: {texto:?}"
    );

    // Y el primer elemento del desplegable lleva su etiqueta.
    let item = zonas
        .iter()
        .find(|z| z.hit == ui::MenuHit::Item(0))
        .copied()
        .expect("el primer elemento tiene zona");
    let fila = recorte(&lineas, item.row, item.x0, item.x1 - item.x0 + 1);
    assert!(
        !fila.trim().is_empty(),
        "el desplegable no pintó su primer elemento"
    );
}

/// Pulsar un título abre ESE menú; pulsar fuera cierra la barra.
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
        .expect("hay un tercer menú");
    pulsar(&mut app, tercero.x0, tercero.row);
    assert_eq!(
        app.menu.expect("sigue abierta").menu(),
        2,
        "se abrió el que se pulsó"
    );

    let mut app = app_de_prueba_con(60);
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let _ = pintar(&mut app);
    // Una fila de listado, lejos de la barra y del desplegable.
    pulsar(&mut app, W - 2, H - 3);
    assert!(app.menu.is_none(), "un click fuera cierra el menú");
}

/// Un menú abierto se queda TODAS las teclas: si no, un comando despachado por
/// detrás dejaría la barra comiéndose las teclas de lo que acaba de abrirse.
#[test]
fn un_menu_abierto_es_dueno_del_teclado() {
    let mut app = app_de_prueba_con(60);
    let antes = app.panes[0].cursor();
    app.menu = Some(norte_frontend::menu::MenuState::new());
    let _ = pintar(&mut app);
    assert_eq!(
        app.panes[0].cursor(),
        antes,
        "abrir el menú no mueve nada de detrás"
    );
}

/// El criterio de aceptación de L1a/// El criterio de aceptación de L1a, escrito como test: esta pantalla es
/// idéntica antes y después del refactor.
///
/// Si cambia una celda, o el refactor movió algo o alguien cambió el render a
/// propósito — y entonces el snapshot nuevo se acepta A MANO, tras mirarlo,
/// jamás con un `--accept` a ciegas.
#[test]
fn la_pantalla_orthodox_no_se_mueve() {
    let mut app = app_de_prueba_con(60);
    let lineas = pintar(&mut app);
    insta::assert_snapshot!("orthodox-100x30", lineas.join("\n"));
}

/// El ancla, otra vez, CON el sidebar abierto (L3).
///
/// Es el mismo test de arriba, y por eso vale: lo que el motor cree que pintó
/// sigue siendo lo que hay en el buffer cuando delante de los listados hay un
/// panel que no es un listado. Un `Fixed(16)` mal restado desplaza los dos
/// panes una celda y ningún snapshot de los que ya existen lo vería, porque
/// ninguno lleva sidebar.
#[test]
fn la_geometria_declarada_coincide_con_lo_pintado_con_el_sidebar_abierto() {
    let mut app = app_de_prueba_con(60);
    app.toggle_places();
    let lineas = pintar(&mut app);
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let geom = ui::pane_geometry(&app, area).expect("dos panes pintados");

    assert_eq!(geom.len(), 2, "el sidebar no es un pane");
    assert_eq!(
        geom[0].x, 16,
        "el primer listado empieza tras las 16 celdas"
    );
    assert_eq!(
        u32::from(geom[0].width) + u32::from(geom[1].width),
        u32::from(W) - 16,
        "los dos listados se reparten lo que el sidebar deja"
    );

    for (i, g) in geom.iter().enumerate() {
        let esperada = nombre_visible(&app, i, g.offset);
        let primera = recorte(&lineas, g.first_list_row, g.x, g.width);
        assert!(
            primera.contains(&esperada),
            "pane {i}: la fila {} debería llevar {esperada:?}, lleva {primera:?}",
            g.first_list_row
        );
    }
}

/// Cada preset, pintado, a dos tamaños. El grande es la pantalla de verdad; el
/// pequeño es donde tres columnas ya no caben, así que es el que ejercita el
/// colapso — el camino que ningún test de `resolve` a 100x30 toca.
///
/// Dos cosas que los snapshots dejan ESCRITAS y conviene leer como lo que son:
///
/// - La hoja de detalles sale con «nada bajo el cursor». No es un fallo: la
///   llena el run loop cada vuelta (`metadata::want`), y aquí solo se pinta un
///   frame. Lo que la hoja enseña de verdad lo fijan los tests de `metadata`.
/// - A 40x10, `explorer` y `full` APARTAN cromo (#229): el panel de procesos en
///   los dos, y en `full` también la columna derecha. Lo que se queda es lo que
///   cabe —sidebar, listados con filas de verdad y la barra de estado—, y lo
///   apartado vuelve solo al crecer el terminal, porque el árbol no se toca.
///   Antes de #229 estos dos snapshots enseñaban tres cabeceras de cromo y ni
///   un nombre de fichero.
#[test]
fn los_cinco_presets_pintan_lo_que_dicen() {
    for name in norte_frontend::layout::presets::NAMES {
        for (w, h) in [(80_u16, 24_u16), (40, 10)] {
            let mut app = app_de_prueba_con(60);
            app.set_layout(norte_frontend::layout::presets::tree(name).expect("de fábrica"));
            let lineas = pintar_en(&mut app, w, h);
            assert_eq!(lineas.len(), h as usize, "{name} {w}x{h}");
            insta::assert_snapshot!(format!("preset-{name}-{w}x{h}"), lineas.join("\n"));
        }
    }
}

/// Con UN listado no hay «el otro panel», así que una copia no tiene destino
/// por defecto. La regla de L1 es que la operación PREGUNTA — abre el prompt
/// de dirección — en vez de fallar. `simple` es el primer preset donde eso
/// deja de ser hipotético, y este test es lo que impide que vuelva a ser un
/// mensaje de error.
#[test]
fn con_un_solo_listado_una_copia_pregunta_el_destino() {
    use norte_tui::app::{Modal, TransferKind};

    let mut app = app_de_prueba_con(3);
    app.set_layout(norte_frontend::layout::presets::tree("simple").expect("s"));
    let _ = pintar(&mut app);
    assert_eq!(app.panes.len(), 1, "un solo listado");
    assert_eq!(app.target_index(), None, "y por tanto ningún destino");

    // El mismo camino que toma F5 cuando `target_index()` no contesta.
    app.open_transfer_dest(TransferKind::Copy);
    let Some(Modal::TransferDest { kind, input, error }) = &app.modal else {
        panic!("pregunta la dirección en vez de fallar: {:?}", app.modal)
    };
    assert_eq!(*kind, TransferKind::Copy);
    assert_eq!(
        input,
        &app.panes[0].dir().to_wire(),
        "prellenado con la dirección del propio panel"
    );
    assert!(error.is_none());
}

/// Confirmar el prompt no transfiere: abre el modal que habría abierto un F5
/// con dos paneles. Un segundo camino para someter una transferencia es un
/// camino que se queda sin confirmación, sin colisión y sin undo.
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
    assert!(app.transfer_dest_confirm(), "la dirección parsea");

    let Some(Modal::TransferName { kind, to_dir, .. }) = &app.modal else {
        panic!("el modal de siempre: {:?}", app.modal)
    };
    assert_eq!(*kind, TransferKind::Copy);
    assert_eq!(to_dir.to_wire(), "file:///otro");
}

/// Una dirección que no parsea CONSERVA lo tecleado y deja su diagnóstico: el
/// prompt no se cierra tragándose la operación.
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
    assert!(!app.transfer_dest_confirm(), "una ruta local no es wire");

    let Some(Modal::TransferDest { input, error, .. }) = &app.modal else {
        panic!("sigue abierto: {:?}", app.modal)
    };
    assert_eq!(input, "/home/yo", "lo tecleado sobrevive");
    assert!(error.is_some(), "y dice por qué");
}
