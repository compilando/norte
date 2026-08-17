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
        geom[1].list_rows, 0,
        "el que no se pinta no tiene ni una fila que clicar"
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
