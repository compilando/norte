//! Task 3 (fase 5 WOW): la decisión de qué modo usa el visor para una
//! imagen, resuelta contra `[ui] images` y lo que contestó la sonda de
//! kitty — ver `viewer_open::modo_efectivo` — y dos invariantes que la
//! ronda de arreglo 1 dejó como regresión: `App.viewer_imagen` no puede
//! quedar colgando cuando el visor se cierra, y no se decide por
//! `Viewer::is_image()` (que un previewer de plugin apaga).

use norte_config::Images;
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::kitty_graphics::{escape_borrar, escape_colocar};
use norte_tui::viewer_open::{ImagenColocada, Modo, modo_efectivo};
use ratatui::layout::Rect;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

fn app_en(dir: &VPath) -> App {
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    )
}

#[test]
fn auto_usa_kitty_solo_si_el_terminal_sabe() {
    assert_eq!(modo_efectivo(Images::Auto, true), Modo::Kitty);
    assert_eq!(modo_efectivo(Images::Auto, false), Modo::Bloques);
}

#[test]
fn kitty_forzado_manda_aunque_la_sonda_dijera_que_no() {
    // La sonda puede equivocarse —un multiplexor con passthrough, un
    // terminal que no contesta pero sabe— y forzar es para eso. Si de
    // verdad no sabe, lo que se ve es basura en pantalla, y por eso no es
    // el valor por defecto.
    assert_eq!(modo_efectivo(Images::Kitty, false), Modo::Kitty);
}

#[test]
fn blocks_no_usa_kitty_aunque_el_terminal_sepa() {
    assert_eq!(modo_efectivo(Images::Blocks, true), Modo::Bloques);
}

#[test]
fn off_no_pinta_nada_y_deja_el_visor_como_estaba() {
    assert_eq!(modo_efectivo(Images::Off, true), Modo::Nada);
}

/// HALLAZGO 1 de la ronda de arreglo 1: `Command::ViewerClose` ponía
/// `app.viewer = None` sin tocar `app.viewer_imagen`, así que tras ver una
/// imagen con miniatura y cerrar el visor, la miniatura de la imagen
/// anterior seguía viva — colgando hasta que T4 la use para colocar/borrar
/// por id. `App::close_viewer` los limpia a la vez; este test fija esa
/// invariante directamente sobre el método, sin pasar por `dispatch` (que
/// pide un `Backend` que este test no necesita).
#[test]
fn cerrar_el_visor_limpia_tambien_su_miniatura() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path: vp("mem:///x.png"),
        bytes: vec![0u8; 4],
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });

    app.close_viewer();

    assert!(app.viewer.is_none(), "el visor se cierra");
    assert!(
        app.viewer_imagen.is_none(),
        "y su miniatura se va CON él, no se queda colgando"
    );
}

/// HALLAZGO 2 de la ronda de arreglo 1: decidir si pedir la miniatura por
/// `Viewer::is_image()` dejaba la fase entera muerta en cuanto un previewer
/// de plugin de imagen (`image-ansi`, en este mismo repo) estuviera
/// aprobado, porque ese getter es `false` en cuanto el preview de plugin
/// sustituye la vista cruda. `viewer_open::viewer_for_width` decide por los
/// BYTES (`image_format`) antes de que la cadena de plugin tenga
/// oportunidad de esconder el formato. Este test deja constancia del
/// cruce que lo justifica: los dos pueden discrepar sobre el MISMO
/// fichero.
///
/// No cubre el camino entero (`viewer_for_width` + `Backend::plugin_thumbnail`
/// con un previewer de imagen REAL aprobado): eso pide la misma
/// infraestructura de `norte-core/tests/plugins_preview_image_e2e.rs`
/// (compilar `plugins/image-ansi` a `wasm32-wasip2`, instalar, aprobar), que
/// hoy no existe en `norte-tui/tests` — deuda anotada en el informe, no
/// construida en esta ronda.
#[test]
fn un_previewer_de_plugin_no_esconde_que_los_bytes_son_imagen() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\n";
    let con_previewer = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
        vp("mem:///x.png"),
        "un-previewer".to_owned(),
        &[],
        false,
    );
    assert!(
        !con_previewer.is_image(),
        "is_image() ve el previewer, no el fichero"
    );
    assert!(
        norte_frontend::viewer::image_format(png).is_some(),
        "pero los bytes del mismo fichero siguen diciendo que ES una imagen"
    );
}

/// Task 4: el APC que coloca la imagen lleva el id, el tamaño en CELDAS
/// (`c`/`r`, no píxeles) y los bytes en base64 — nunca crudos, porque un APC
/// se cierra con `\x1b\\` y un PNG contiene esa pareja de bytes con toda
/// normalidad.
#[test]
fn colocar_lleva_el_id_el_tamano_y_base64() {
    let esc = escape_colocar(7, b"PNGFALSO", Rect::new(1, 2, 40, 20));
    assert!(esc.starts_with("\x1b_G"), "empieza por APC: {esc}");
    assert!(esc.contains("i=7"), "lleva el id: {esc}");
    assert!(
        esc.contains("f=100"),
        "PNG, que es lo que da el kind thumbnail"
    );
    assert!(
        esc.contains("c=40") && esc.contains("r=20"),
        "el hueco: {esc}"
    );
    // Ronda de arreglo 2 (MENOR nuevo): sin `C=1` colocar mueve el cursor y
    // puede scrollear la pantalla (CRÍTICO 1); sin `q=2` el terminal
    // contesta y su respuesta entra al lector de eventos como pulsaciones
    // sueltas (CRÍTICO 3). Ninguno de los dos tenía un aserto que lo
    // impidiera desaparecer en silencio.
    assert!(esc.contains("C=1"), "no mueve el cursor al colocar: {esc}");
    assert!(
        esc.contains("q=2"),
        "calla la respuesta del terminal: {esc}"
    );
    assert!(esc.ends_with("\x1b\\"), "cierra el APC: {esc}");
    // Los bytes van en base64 y NO en crudo: un APC se termina con
    // `\x1b\\`, y un PNG contiene esa pareja de bytes con toda normalidad.
    assert!(esc.contains("UE5HRkFMU08"), "base64 del contenido: {esc}");
}

/// Una miniatura de verdad no cabe en un solo APC, así que hay que
/// trocearla: todos los trozos menos el último llevan `m=1` y el último
/// `m=0`. Sin este test, los de arriba pasan con un `escape_colocar` que no
/// sabe trocear — 8 bytes nunca llegan al tope.
#[test]
fn un_contenido_grande_se_trocea() {
    let grande = vec![0u8; 12 * 1024];
    let esc = escape_colocar(7, &grande, Rect::new(1, 2, 40, 20));
    let trozos: Vec<&str> = esc.split("\x1b_G").skip(1).collect();
    assert!(
        trozos.len() > 1,
        "una imagen grande va en varios trozos: {}",
        trozos.len()
    );
    let (ultimo, previos) = trozos.split_last().expect("hay al menos uno");
    for t in previos {
        assert!(t.contains("m=1"), "un trozo que no es el último sigue: {t}");
    }
    assert!(ultimo.contains("m=0"), "el último cierra: {ultimo}");
}

/// `d=I` (mayúscula) borra la colocación Y libera los bytes que el terminal
/// guarda para el id — no sólo `d=i` (minúscula), que deja el PNG vivo en la
/// memoria del terminal para siempre porque `mint_image_id` nunca recicla un
/// id (ronda de arreglo 1, IMPORTANTE 6: este aserto estaba mal en el
/// encargo original, no el código que lo seguía). `i=<id>` sigue acotando el
/// borrado a ESTA imagen: sin él se borrarían las de todo el terminal,
/// incluidas las de otro programa en otra pestaña.
#[test]
fn borrar_nombra_solo_ese_id_y_libera_los_datos() {
    let esc = escape_borrar(7);
    assert!(
        esc.contains("a=d") && esc.contains("d=I") && esc.contains("i=7"),
        "{esc}"
    );
}

/// MENOR 8 (ronda de arreglo 1): el rect que el run loop manda al terminal
/// (`ui::rect_del_visor`) tiene que ser el MISMO que `draw_viewer` deja en
/// blanco cuando hay una imagen colocada — no dos cuentas del mismo hueco
/// que puedan divergir en silencio (memoria `funcion-compartida-no-basta`).
///
/// Sin este test, `rect_del_visor` podía devolver el marco CON bordes (el
/// bug del CRÍTICO 2) y nada lo habría cazado: los tests de arriba sólo
/// miran la FORMA del escape, nunca dónde cae de verdad. Este renderiza de
/// verdad y comprueba las CELDAS.
#[test]
fn el_rect_del_visor_es_el_hueco_que_draw_viewer_deja_en_blanco() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });
    let area = Rect::new(0, 0, 40, 12);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal de test");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let hueco = norte_tui::ui::rect_del_visor(&app, area);
    assert!(
        hueco.width > 0 && hueco.height > 0,
        "el hueco no puede ser vacío en un terminal de {area:?}: {hueco:?}"
    );
    let buf = terminal.backend().buffer();
    for y in hueco.top()..hueco.bottom() {
        for x in hueco.left()..hueco.right() {
            assert_eq!(
                buf[(x, y)].symbol(),
                " ",
                "la celda ({x},{y}) del hueco debería estar en blanco con la \
                 imagen colocada"
            );
        }
    }
    // Y el marco, justo por ENCIMA del hueco, NO está en blanco: si lo
    // estuviera, el test de arriba pasaría con cualquier rect más grande que
    // el real — exactamente el bug del CRÍTICO 2, que se pasaba de los
    // bordes y tapaba el marco con `z=0`.
    let borde_y = hueco.top() - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, borde_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        fila.contains(['┌', '─', '┐']),
        "encima del hueco sigue el marco del visor, no más blanco: {fila:?}"
    );
}
