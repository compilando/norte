//! La sesión de UI vista desde el frontend (L2): qué se captura, qué se
//! aplica, y qué NO viaja.
//!
//! La regla que estos tests fijan es que capturar y aplicar son la misma
//! pantalla dicha dos veces. Lo demás son las tres cosas que se decidieron a
//! propósito: las marcas no viajan, el estado de un hueco que el layout no
//! tiene se conserva, y un cuerpo ilegible no deja pantalla en blanco.

use norte_frontend::layout::{KindId, Node, SlotId};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn app_basica() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

/// Capturar y volver a aplicar deja la misma pantalla: mismo árbol, mismas
/// rutas, mismo cursor.
#[test]
fn capturar_y_aplicar_es_la_identidad() {
    let mut app = app_basica();
    app.set_layout(norte_frontend::layout::presets::tree("krusader").expect("preset"));
    let before = app.session_body();
    let mut other = app_basica();
    other.apply_session(&before);
    assert_eq!(other.session_body(), before);
}

/// El historial viaja: `nav.back` sigue funcionando tras un reinicio.
#[test]
fn el_rastro_de_vuelta_sobrevive() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    app.history.for_slot_mut(slot).record(vp("file:///antes"));
    let body = app.session_body();
    assert_eq!(body.slots[&slot.0].back, vec![vp("file:///antes")]);

    let mut other = app_basica();
    other.apply_session(&body);
    assert_eq!(
        other.history.for_slot(slot).expect("historial").trail(),
        [vp("file:///antes")]
    );
}

/// Las marcas NO viajan: son el estado de una operación, no de una sesión.
#[test]
fn las_marcas_no_viajan() {
    let mut app = app_basica();
    app.focused_mut().mark_all();
    let v = serde_json::to_string(&app.session_body().to_value()).expect("json");
    assert!(!v.contains("mark"), "{v}");
}

/// Una sesión que menciona un hueco que este layout no tiene no rompe nada: se
/// guarda su estado y se pinta lo que el layout dice.
#[test]
fn un_hueco_que_el_layout_no_tiene_no_rompe_la_aplicacion() {
    let mut app = app_basica();
    let mut body = app.session_body();
    let uno = body.slots.values().next().expect("uno").clone();
    body.slots.insert(99, uno);
    app.apply_session(&body);
    assert!(app.session_body().slots.contains_key(&99), "se conserva");
}

/// Un cuerpo corrupto no deja pantalla en blanco: se ignora y queda el layout
/// de la config, con un aviso. Y se SIGUE escribiendo: lo que había ilegible
/// ya está perdido, y no volver a guardar nunca sería peor.
#[test]
fn un_cuerpo_corrupto_deja_la_pantalla_de_la_config() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // Malformado de verdad: un hueco sin `path`, que es el único campo que no
    // tiene default.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "slots": { "1": { "cursor": 3 } } }),
    );
    assert_eq!(app.layout, before);
    assert!(app.message.is_some(), "y lo dice");
    assert!(!app.session.detached, "y esta ventana sigue escribiendo");
}

/// La versión del SOBRE deja la ventana suelta igual que la de dentro (#247).
///
/// El cuerpo llevaba una copia sin documentar de la versión, y era la única
/// que se leía: un cliente ajeno que hiciera lo que dice el contrato —`version`
/// en el sobre, cuerpo v2— llegaba a un lector que la veía ausente, la tomaba
/// por 0, se comía los campos que no entendía y los reescribía perdidos.
#[test]
fn la_version_del_sobre_tambien_deja_la_ventana_suelta() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // Cuerpo SIN copia dentro, que es lo que escribe un cliente que sigue el
    // contrato documentado.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION + 1,
        &serde_json::json!({ "layouts": {}, "slots": {} }),
    );
    assert_eq!(app.layout, before, "no se aplica lo que no se sabe leer");
    assert!(app.session.detached, "y sobre todo no se pisa");
    assert!(app.message.is_some(), "y lo dice");
}

/// Un cuerpo de una versión MÁS NUEVA deja esta ventana SUELTA: no se lee y,
/// sobre todo, no se vuelve a escribir encima. Sin esto el aviso salía y un
/// segundo después el volcado publicaba la pantalla de la configuración sobre
/// la sesión del binario nuevo.
#[test]
fn un_cuerpo_del_futuro_deja_la_ventana_suelta() {
    let mut app = app_basica();
    let before = app.layout.clone();
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "version": 999 }),
    );
    assert_eq!(app.layout, before);
    assert!(app.session.detached, "no se pisa lo que no se sabe leer");
    assert!(app.message.is_some(), "y lo dice");
}

/// Las dos versiones de esquema —la del core, que decide si un fichero se
/// puede pisar, y la del frontend, que decide si un cuerpo se puede leer— van
/// del brazo.
///
/// Viven en crates distintos porque el core no puede depender del frontend, y
/// nada las ata en tiempo de compilación: subir SOLO la del frontend hace que
/// el core rehúse su propio fichero en cada arranque, para siempre y detrás de
/// un `warn!`. Éste es el único crate que ve las dos.
#[test]
fn las_dos_versiones_de_esquema_van_del_brazo() {
    assert_eq!(
        norte_core::ui_session::disk::SCHEMA_VERSION,
        norte_frontend::session::SCHEMA_VERSION,
        "si subes una, sube la otra"
    );
}

/// El cursor se coloca cuando llega el listado, no antes: sobre un pane vacío
/// la fila 12 es la fila 0, y aplicarlo ahí lo perdería.
#[test]
fn el_cursor_espera_a_su_listado() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut body = app.session_body();
    body.slots.get_mut(&slot.0).expect("hueco").cursor = 2;
    app.apply_session(&body);
    assert_eq!(app.panes[0].cursor(), 0, "sin listado no hay dónde ponerlo");

    let entradas: Vec<norte_proto::Entry> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| norte_proto::Entry {
            path: vp(&format!("file:///izq/{n}")),
            kind: norte_proto::EntryKind::File,
            size: Some(0),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect();
    app.panes[0].begin_listing(vp("file:///izq"), entradas, false, None);
    app.restore_cursor(slot);
    assert_eq!(app.panes[0].cursor(), 2);
}

/// Un hueco cuyo layout SÍ existe recupera su directorio y su orden.
#[test]
fn el_directorio_y_el_orden_vuelven() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut spec = app.panes[0].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[0].set_sort(spec);
    app.panes[0].set_show_hidden(false);
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    assert!(ask.contains(&slot), "hay que listarlo");
    assert_eq!(other.panes[0].dir(), &vp("file:///izq"));
    assert_eq!(other.panes[0].sort(), spec);
    assert!(!other.panes[0].show_hidden());
}

/// Un layout que este binario no sabe pintar viaja igual: la sesión guarda el
/// árbol, no lo interpreta.
#[test]
fn un_kind_desconocido_viaja_en_el_layout() {
    let mut app = app_basica();
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::new("kind-de-otro-binario")),
        ],
    ));
    let body = app.session_body();
    let vuelta = norte_frontend::session::SessionBody::from_value(
        norte_frontend::session::SCHEMA_VERSION,
        &body.to_value(),
    )
    .expect("parsea");
    assert_eq!(vuelta.layouts["default"], app.layout);
}
