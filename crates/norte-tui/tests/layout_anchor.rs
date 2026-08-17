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
    ui::before_frame(app, ratatui::layout::Rect::new(0, 0, w, h));
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal
        .backend()
        .to_string()
        .lines()
        .map(ToOwned::to_owned)
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
