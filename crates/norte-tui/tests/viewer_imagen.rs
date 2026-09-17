//! Task 3 (fase 5 WOW): la decisión de qué modo usa el visor para una
//! imagen, resuelta contra `[ui] images` y lo que contestó la sonda de
//! kitty — ver `viewer_open::modo_efectivo` — y dos invariantes que la
//! ronda de arreglo 1 dejó como regresión: `App.viewer_imagen` no puede
//! quedar colgando cuando el visor se cierra, y no se decide por
//! `Viewer::is_image()` (que un previewer de plugin apaga).

use norte_config::Images;
use norte_proto::VPath;
use norte_proto::methods::PluginThumbnail;
use norte_tui::app::{App, Modal, Pane};
use norte_tui::kitty_graphics::{escape_borrar, escape_colocar};
use norte_tui::viewer_open::{
    ImagenColocada, Modo, aviso_de_imagen, imagen_desde_miniatura, modo_efectivo,
};
use ratatui::layout::Rect;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

/// Bytes de un PNG que `norte_encoding::detect` clasifica como BINARIO, no
/// sólo los 8 de la firma mágica: con solo la firma, sin ningún byte NUL, la
/// heurística de detección los toma por texto de 8 bytes en una codificación
/// de un byte (`Viewer::recompute` entonces pone `self.image = None`, rama
/// texto) y `is_image()` sale `false` aunque los bytes SÍ empiecen por la
/// firma PNG — regresión real, cazada al escribir estos tests. Mismo patrón
/// que ya usaba `fila_de_estado_con_png_sin_previewer` (firma + `IHDR` +
/// relleno de ceros hasta 40 bytes).
fn png_bytes_binarios() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    bytes
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
        mimetype: "image/png".to_owned(),
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
        "PNG: lo único que `imagen_desde_miniatura` deja pasar"
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

fn thumb(mimetype: &str) -> PluginThumbnail {
    PluginThumbnail {
        plugin_id: "image-thumb".to_owned(),
        plugin_name: "image-thumb".to_owned(),
        mimetype: mimetype.to_owned(),
        bytes: png_bytes_binarios(),
        width: 8,
        height: 4,
    }
}

/// HALLAZGO 1 de la revisión de rama: `escape_colocar` manda `f=100` FIJO
/// —kitty no tiene clave `f=` para JPEG ni WebP, sólo PNG (100) o raster
/// crudo (24/32)— pero `PluginThumbnail.mimetype` admite las tres, y
/// `thumb::reencode` (`norte-plugin-host`) cae de verdad a JPEG cuando el
/// PNG no cabe en 4 MiB, fácil con el `max_edge` de hasta 1920 px que pide
/// este visor. Un JPEG colocado con una cabecera que dice PNG lo rechaza
/// kitty EN SILENCIO (`q=2`), y sin este filtro nadie se entera: ni traza,
/// ni reintento, ni aviso.
#[test]
fn una_miniatura_jpeg_se_descarta() {
    let imagen = imagen_desde_miniatura(&vp("mem:///x.jpg"), thumb("image/jpeg"));
    assert!(
        imagen.is_none(),
        "kitty no sabe f= para JPEG: debe descartarse, no colocarse mal"
    );
}

/// Misma cadena para WebP — el tercer formato que `plugin.thumbnail` puede
/// devolver y que kitty tampoco sabe colocar.
#[test]
fn una_miniatura_webp_se_descarta() {
    let imagen = imagen_desde_miniatura(&vp("mem:///x.webp"), thumb("image/webp"));
    assert!(imagen.is_none(), "kitty no sabe f= para WebP");
}

/// El PNG, el único formato que el protocolo de kitty entiende, SÍ se
/// coloca — y lleva su mimetype consigo, para que la invariante sea
/// comprobable en el resto del camino (no sólo documentada).
#[test]
fn una_miniatura_png_se_coloca() {
    let path = vp("mem:///x.png");
    let imagen = imagen_desde_miniatura(&path, thumb("image/png")).expect("un PNG sí se coloca");
    assert_eq!(imagen.mimetype, "image/png");
    assert_eq!(imagen.path, path);
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
        mimetype: "image/png".to_owned(),
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

fn app_con_imagen_colocada(area_no_vacia: bool) -> (App, Rect) {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 7,
        puesta_en: None,
    });
    let area = if area_no_vacia {
        Rect::new(0, 0, 40, 12)
    } else {
        Rect::new(0, 0, 0, 0)
    };
    (app, area)
}

/// HALLAZGO 2 de la revisión de rama: el pintor (`draw_viewer`'s
/// `hay_imagen`) y el run loop (`coloca`, T4) miraban condiciones
/// DISTINTAS para decidir si la miniatura se ve — el pintor sólo el `path`,
/// el run loop además "nada pintado encima" y "el rect no vacío". Con un
/// overlay que NO tapa la pantalla entera (un modal pequeño, el menú,
/// which-key) el pintor blanqueaba el hueco igual que siempre mientras el
/// run loop se negaba a colocar píxeles: ni imagen ni hexview. Las dos
/// preguntas son ahora la MISMA función (`ui::imagen_a_colocar`).
#[test]
fn con_algo_encima_no_se_coloca_aunque_el_path_case() {
    let (mut app, area) = app_con_imagen_colocada(true);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_some(),
        "sin nada encima, la miniatura se coloca"
    );
    app.modal = Some(Modal::ConfirmQuit);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_none(),
        "un modal abierto sobre el visor no debe dejar colocar píxeles"
    );
}

/// El hueco vacío (terminal demasiado bajo para dejar sitio al marco y su
/// interior) es el otro caso en el que no se coloca nada — el test del rect
/// vacío que la revisión de rama pidió por separado de la comprobación de
/// overlay de arriba: antes de esta pasada, `imagen_a_colocar` no existía y
/// nada probaba esta rama de forma aislada del resto de `coloca`.
#[test]
fn con_el_rect_vacio_no_se_coloca() {
    let (app, area) = app_con_imagen_colocada(false);
    assert!(
        norte_tui::ui::imagen_a_colocar(&app, area).is_none(),
        "sin hueco donde caer, no hay nada que colocar: {area:?}"
    );
}

/// HALLAZGO 3 de la revisión de rama: el modo con que se PIDE la miniatura
/// (`viewer_open::open_viewer`, resuelto una vez al abrir) y el modo con que
/// se AVISA y COLOCA (recalculado en cada frame contra la config vigente)
/// podían divergir en caliente — `[ui] images` recarga en vivo
/// (`applies_live`). Estos dos tests fijan el método que
/// `config_reload::reload_config` llama tras reasignar `App::chrome`
/// (`reload_config` en sí pide un `Backend`, tres `Resolver` y una
/// `Layers`, demasiado para un test unitario de esto).
#[test]
fn kitty_a_off_suelta_la_miniatura_colocada_ya_puesta() {
    let (mut app, _) = app_con_imagen_colocada(true);
    app.viewer_modo = Modo::Kitty;
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Nada);
    assert!(soltada, "un Kitty que pasa a off debe soltar la miniatura");
    assert!(
        app.viewer_imagen.is_none(),
        "sin esto los píxeles se quedan pegados en pantalla para siempre, \
         violando lo que la ayuda promete de `off`"
    );
    assert_eq!(
        app.viewer_modo,
        Modo::Nada,
        "el modo pineado se actualiza a la vez que se suelta la miniatura"
    );
}

/// La dirección contraria (`Bloques`/`Nada` → `Kitty`) NO suelta ni
/// actualiza nada, a propósito: hacerlo resucitaría el otro agujero de la
/// misma revisión — el aviso «falta aprobar la extensión de miniaturas»
/// saldría sobre un fichero al que el modo nuevo JAMÁS le pidió una.
#[test]
fn blocks_a_kitty_no_toca_el_modo_pineado() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_modo = Modo::Bloques;
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Kitty);
    assert!(!soltada, "sólo actúa cuando el modo pineado YA era Kitty");
    assert_eq!(
        app.viewer_modo,
        Modo::Bloques,
        "se deja pineado hasta que el lector reabra el fichero"
    );
}

/// Extremo a extremo del mismo hallazgo: `panels::draw_viewer` tiene que
/// leer `App::viewer_modo` (pineado) y NO recalcular contra `app.chrome`
/// EN VIVO. Se simula justo el escenario que la revisión describió — el
/// lector abrió el PNG bajo `Bloques` (nunca se le pidió miniatura) y LUEGO
/// la config cambió a `kitty` en caliente, sin que el lector reabra nada —
/// y se comprueba que el aviso sigue siendo el de `Bloques` («vista
/// previa»), no el de `Kitty` («miniaturas») sobre un fichero al que esa
/// extensión nunca se le pidió.
#[test]
fn el_pintor_usa_el_modo_pineado_no_el_chrome_en_vivo() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        vp("mem:///x.png"),
        png_bytes_binarios(),
        false,
    ));
    // Lo que `open_viewer` fijó cuando el lector abrió el fichero.
    app.viewer_modo = Modo::Bloques;
    // Lo que un hot-reload cambió DESPUÉS, sin tocar `viewer_modo` (el
    // propio arreglo del hallazgo 3: sólo la dirección Kitty→algo-más
    // actualiza el modo pineado).
    app.chrome.images = Some(Images::Kitty);
    // Sin `viewer_imagen`: nunca se pidió una miniatura bajo `Bloques`.

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal de test");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        fila.contains("vista previa"),
        "el aviso debe seguir siendo el de Bloques, el modo con que se \
         abrió: {fila:?}"
    );
    assert!(
        !fila.contains("miniaturas"),
        "el aviso de Kitty mentiría: nunca se le pidió una miniatura a \
         este fichero: {fila:?}"
    );
}

/// Sin visor abierto no hay nada que soltar — un cambio de config con el
/// navegador (no el visor) al frente no debe tocar `viewer_imagen`, que ya
/// es `None`.
#[test]
fn sin_visor_no_hay_nada_que_soltar() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    assert_eq!(app.viewer_modo, Modo::Nada);
    let soltada = app.soltar_miniatura_si_deja_de_ser_kitty(Modo::Nada);
    assert!(!soltada, "sin viewer abierto no hay miniatura que soltar");
}

/// Task 5: el agujero de usabilidad que encontró el piloto — sin previewer
/// aprobado, un PNG en `Modo::Bloques` cae a hexview igual que un fichero
/// que nadie sabe interpretar, y nada en pantalla distinguía los dos casos.
#[test]
fn en_bloques_sin_previewer_el_visor_lo_dice() {
    // Un hexview silencioso es indistinguible de «norte no sabe hacerlo».
    let aviso = aviso_de_imagen(Modo::Bloques, false);
    assert!(aviso.is_some(), "hay que decir que falta aprobar el plugin");
}

#[test]
fn con_previewer_no_se_avisa_de_nada() {
    assert!(aviso_de_imagen(Modo::Bloques, true).is_none());
}

#[test]
fn en_off_no_se_avisa_porque_lo_pidio_el_lector() {
    assert!(aviso_de_imagen(Modo::Nada, false).is_none());
}

/// Task 5b (hallazgo de revisión de la 6): el mismo agujero de Task 5, pero
/// en `Modo::Kitty`. El docstring de `aviso_de_imagen` decía que en Kitty
/// "el terminal ya pinta píxeles por su cuenta" y por tanto no había nada
/// que avisar — falso: los píxeles los da un plugin `thumbnail`
/// (`plugins/image-thumb`), igual de aprobable y ausente por defecto que el
/// previewer de `Modo::Bloques`. Sin uno aprobado, `Modo::Kitty` caía en
/// hexview tan silenciosamente como el agujero que Task 5 tapó en la otra
/// rama.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice() {
    let aviso = aviso_de_imagen(Modo::Kitty, false);
    assert!(
        aviso.is_some(),
        "hay que decir que falta aprobar el plugin de miniaturas"
    );
}

#[test]
fn con_miniatura_colocada_no_se_avisa_de_nada_en_kitty() {
    assert!(aviso_de_imagen(Modo::Kitty, true).is_none());
}

/// Las dos ramas piden aprobar EXTENSIONES distintas (`previewer` contra
/// `thumbnail`): un aviso que reutilizara el texto de `Modo::Bloques` en
/// `Modo::Kitty` mandaría al lector a aprobar la equivocada, que es peor que
/// no avisar (el encargo lo llama explícitamente).
#[test]
fn el_aviso_de_bloques_y_el_de_kitty_son_textos_distintos() {
    let bloques = aviso_de_imagen(Modo::Bloques, false).expect("bloques avisa");
    let kitty = aviso_de_imagen(Modo::Kitty, false).expect("kitty avisa");
    assert_ne!(
        bloques, kitty,
        "cada modo pide aprobar una extensión distinta"
    );
}

/// Contraparte de `no_hace_falta_avisar_de_imagen` para `Modo::Kitty`: usa
/// el plugin `thumbnail`, no el `previewer`, así que la condición de «ya se
/// ve» es distinta — una miniatura COLOCADA para ESTE fichero, no un
/// previewer que sustituyó la vista cruda.
#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_no_es_imagen() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.txt"), b"hola mundo".to_vec(), false);
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, None),
        "no es una imagen: nada que avisar"
    );
}

#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_ya_hay_una_colocada() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let path = vp("mem:///x.png");
    let v = norte_tui::viewer::Viewer::new(path.clone(), png_bytes_binarios(), false);
    let imagen = ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    };
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, Some(&imagen)),
        "ya hay píxeles puestos: nada que avisar"
    );
}

/// La miniatura colocada es de OTRO fichero (el lector ya se movió, o T4
/// todavía no la ha reemplazado): sigue haciendo falta avisar del que se ve
/// AHORA.
#[test]
fn no_hace_falta_avisar_de_miniatura_compara_el_path() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), png_bytes_binarios(), false);
    let de_otro_fichero = ImagenColocada {
        path: vp("mem:///otro.png"),
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    };
    assert!(
        !no_hace_falta_avisar_de_miniatura(&v, Some(&de_otro_fichero)),
        "la miniatura colocada es de OTRO fichero: sigue faltando la de éste"
    );
}

/// Medios bloques pintados (un previewer de plugin sustituyó la vista cruda,
/// como en `Modo::Bloques`) también apagan `is_image()`: si ESO ya se ve, no
/// hay nada que avisar del plugin de miniaturas tampoco, aunque no haya
/// `ImagenColocada`.
#[test]
fn no_hace_falta_avisar_de_miniatura_cuando_un_previewer_ya_sustituyo_la_vista() {
    use norte_tui::viewer_open::no_hace_falta_avisar_de_miniatura;
    let v = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
        vp("mem:///x.png"),
        "un-previewer".to_owned(),
        &[],
        false,
    );
    assert!(
        no_hace_falta_avisar_de_miniatura(&v, None),
        "un previewer ya pintó algo: nada que avisar del plugin de miniaturas"
    );
}

/// Renderiza la app con un PNG en hexview (sin previewer) y devuelve la
/// última fila del terminal (la barra de estado del visor a pantalla
/// completa, `status_area`), a 80 columnas — el ancho de referencia de la
/// tarea (`snapshot_viewer_texto_y_hex` usa el mismo).
///
/// Firma mágica real de PNG (`is_image()` la reconoce) + relleno hasta 40
/// bytes: a 16 bytes por fila de hexview (`HEX_COLS`) da 3 filas, así que
/// `scroll_down(1)` deja una posición que NO es el trivial «1/1».
fn fila_de_estado_con_png_sin_previewer() -> String {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    // Hallazgo 3: `panels::draw_viewer` lee `App::viewer_modo` (fijado al
    // abrir), no un recálculo en vivo — `Auto` sin sonda de terminal (no
    // hay tty en un test) es `Modo::Bloques`
    // (`auto_usa_kitty_solo_si_el_terminal_sabe`), que es justo lo que
    // `open_viewer` habría fijado aquí.
    app.viewer_modo = Modo::Bloques;
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    let mut v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), bytes, false);
    v.scroll_down(1);
    app.viewer = Some(v);

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal de test");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

/// Ronda de arreglo 1, IMPORTANTE 2: sin previewer aprobado es el estado por
/// DEFECTO de cualquier instalación (nada que aprobar todavía), así que la
/// rama del aviso es el caso COMÚN, no el raro. Antes de ese arreglo,
/// `format!(" {aviso}")` sustituía la barra de estado entera y se comía
/// `n/total` — un PNG grande en hexview perdía el conteo de posición justo
/// mientras se hacía scroll por él.
///
/// Ronda de arreglo 2: el `format!(" {aviso}  {pos}")` de la ronda 1 era
/// correcto en el código pero NO en pantalla — el texto ES original (82
/// caracteres) ya desbordaba las 80 columnas él solo, así que `pos` seguía
/// invisible. `es.ftl`/`en.ftl` se acortaron para que quepan los dos con
/// `pos` al lado; este test fija el locale a ES (`norte_i18n::force`, sólo
/// gana la PRIMERA llamada del proceso — nextest da un proceso por test,
/// así que no choca con `el_aviso_no_se_come_la_posicion_de_scroll_en_ingles`
/// de abajo, que fija EN en OTRO proceso) para probar el caso que estaba
/// roto de verdad, no el que el entorno de esta máquina (`LANG=en_US.UTF-8`)
/// hacía pasar por casualidad.
#[test]
fn el_aviso_no_se_come_la_posicion_de_scroll() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let fila = fila_de_estado_con_png_sin_previewer();
    assert!(
        fila.contains("2/3"),
        "el aviso no debe comerse la posición, en ES: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "y el aviso sigue presente a la vez, en ES: {fila:?}"
    );
}

/// Mismo caso que [`el_aviso_no_se_come_la_posicion_de_scroll`], en EN —
/// proceso aparte bajo nextest, mismo motivo para fijar el locale.
#[test]
fn el_aviso_no_se_come_la_posicion_de_scroll_en_ingles() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let fila = fila_de_estado_con_png_sin_previewer();
    assert!(
        fila.contains("2/3"),
        "el aviso no debe comerse la posición, en EN: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "y el aviso sigue presente a la vez, en EN: {fila:?}"
    );
}

/// Task 5b: el mismo render que `fila_de_estado_con_png_sin_previewer`, pero
/// forzando `images = "kitty"` (`Images::Kitty` manda igual sin sonda, ver
/// `kitty_forzado_manda_aunque_la_sonda_dijera_que_no`) y SIN colocar
/// `app.viewer_imagen` — el caso real: `Modo::Kitty` pidió la miniatura por
/// el plugin `thumbnail` y no había ninguno aprobado, así que
/// `viewer_for_width` devolvió `None` y nada se colocó.
fn fila_de_estado_con_png_en_kitty_sin_miniatura() -> String {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.chrome.images = Some(Images::Kitty);
    // Hallazgo 3: `panels::draw_viewer` lee `App::viewer_modo` (fijado al
    // abrir), no `app.chrome.images()` en vivo — este test bypassa
    // `open_viewer`, así que tiene que fijarlo él mismo, tal como lo haría
    // `open_viewer` para un visor abierto bajo este `chrome`.
    app.viewer_modo = Modo::Kitty;
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    let mut v = norte_tui::viewer::Viewer::new(vp("mem:///x.png"), bytes, false);
    v.scroll_down(1);
    app.viewer = Some(v);
    // A propósito NO se pone `app.viewer_imagen`: es justo el estado que
    // deja `viewer_for_width` sin plugin `thumbnail` aprobado.

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal de test");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

/// El defecto que reportó la revisión de T6: en `Modo::Kitty` sin plugin de
/// miniaturas, el visor se quedaba en hexview SIN ningún aviso — exactamente
/// el agujero que Task 5 tapó en `Modo::Bloques`, reabierto en la otra rama.
/// Mismo presupuesto de 80 columnas: el aviso Y `pos` visibles a la vez.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice_y_no_se_come_la_posicion() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let fila = fila_de_estado_con_png_en_kitty_sin_miniatura();
    assert!(
        fila.contains("2/3"),
        "el aviso no debe comerse la posición, en ES: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "y el aviso de miniatura sigue presente a la vez, en ES: {fila:?}"
    );
}

/// Mismo caso, en EN — proceso aparte bajo nextest.
#[test]
fn en_kitty_sin_miniatura_el_visor_lo_dice_y_no_se_come_la_posicion_en_ingles() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let fila = fila_de_estado_con_png_en_kitty_sin_miniatura();
    assert!(
        fila.contains("2/3"),
        "el aviso no debe comerse la posición, en EN: {fila:?}"
    );
    assert!(
        fila.contains("F12"),
        "y el aviso de miniatura sigue presente a la vez, en EN: {fila:?}"
    );
}

/// Con la miniatura YA colocada (píxeles puestos), el aviso de Kitty NO debe
/// salir — «si la imagen se está viendo... no hay nada que avisar» (encargo).
/// El hueco de contenido va en blanco (T4), pero la barra de estado sigue
/// siendo la normal (encoding/EOL/…), sin el texto de F12.
#[test]
fn en_kitty_con_miniatura_colocada_no_sale_el_aviso() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    app.chrome.images = Some(Images::Kitty);
    // Ídem: fijar el modo pineado a mano, ver el comentario en
    // `fila_de_estado_con_png_en_kitty_sin_miniatura`.
    app.viewer_modo = Modo::Kitty;
    let path = vp("mem:///x.png");
    app.viewer = Some(norte_tui::viewer::Viewer::new(
        path.clone(),
        png_bytes_binarios(),
        false,
    ));
    app.viewer_imagen = Some(ImagenColocada {
        path,
        bytes: vec![0u8; 4],
        mimetype: "image/png".to_owned(),
        width: 8,
        height: 4,
        id: 1,
        puesta_en: None,
    });

    let area = Rect::new(0, 0, 80, 16);
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
            .expect("terminal de test");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let status_y = area.height - 1;
    let fila: String = (0..area.width)
        .map(|x| buf[(x, status_y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        !fila.contains("F12"),
        "con píxeles ya colocados no hay nada que avisar: {fila:?}"
    );
}
