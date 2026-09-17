//! Task 3 (fase 5 WOW): la decisión de qué modo usa el visor para una
//! imagen, resuelta contra `[ui] images` y lo que contestó la sonda de
//! kitty — ver `viewer_open::modo_efectivo` — y dos invariantes que la
//! ronda de arreglo 1 dejó como regresión: `App.viewer_imagen` no puede
//! quedar colgando cuando el visor se cierra, y no se decide por
//! `Viewer::is_image()` (que un previewer de plugin apaga).

use norte_config::Images;
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::viewer_open::{ImagenColocada, Modo, modo_efectivo};

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
