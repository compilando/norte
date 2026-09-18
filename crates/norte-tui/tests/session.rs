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

/// Este proceso mira UN perfil y el documento es de todos: lo de los demás
/// viaja de vuelta intacto.
///
/// Escribir solo el activo borraría del cuerpo el sitio donde los otros
/// perfiles dejaron sus paneles, y el lector lo descubriría al volver a uno y
/// encontrárselo en blanco (ADR 0079, D5).
#[test]
fn las_disposiciones_de_otros_perfiles_vuelven_intactas() {
    let ajena = norte_frontend::layout::presets::tree("krusader").expect("preset");
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts.insert("photos".to_owned(), ajena.clone());

    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session(&body);
    let vuelta = app.session_body();

    assert_eq!(
        vuelta.layouts.get("photos"),
        Some(&ajena),
        "la de «photos» no se toca"
    );
    assert!(
        vuelta.layouts.contains_key("work"),
        "y la de este proceso va bajo SU nombre, no bajo `default`: {:?}",
        vuelta.layouts.keys().collect::<Vec<_>>()
    );
    assert_eq!(vuelta.active, "work");
}

/// El perfil PEGAJOSO llega con la sesión y pide el cambio.
///
/// No puede aplicarse antes: vive en la sesión, la sesión la tiene el daemon, y
/// al daemon se llega con la configuración que ya está cargada. Así que se pide
/// y lo hace el bucle por el mismo camino que cualquier otro cambio.
#[test]
fn el_perfil_pegajoso_pide_el_cambio() {
    let body = norte_frontend::session::SessionBody {
        active: "work".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(
        app.pending_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// Y `--profile` lo GANA: el lector nombró uno para esta vez, así que el que
/// venía de la sesión no lo pisa.
#[test]
fn un_perfil_explicito_gana_al_pegajoso() {
    let body = norte_frontend::session::SessionBody {
        active: "photos".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(app.pending_profile, None, "no se pide ningún cambio");
    assert_eq!(
        app.active_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// `ntc <DIR>` gana a la sesión guardada en el panel activo, y SOLO ahí: el
/// resto de la pantalla vuelve como estaba. Antes la sesión pisaba el
/// argumento y `ntc ~/proyecto` abría donde se cerró la última vez.
#[test]
fn un_directorio_explicito_gana_a_la_sesion_en_el_panel_activo() {
    let mut app = app_basica();
    let foco = app.focus();
    let activo = app.panes.slot_of(foco);
    let mut spec = app.panes[foco].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[foco].set_sort(spec);
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    other.pin_start_dir(vp("file:///pedido"));
    let foco = other.focus();
    assert_eq!(other.panes[foco].dir(), &vp("file:///pedido"));
    assert_eq!(
        other.panes[foco].sort(),
        spec,
        "el orden guardado se conserva: solo cambia el directorio"
    );
    assert_eq!(
        other.panes[1 - foco].dir(),
        &vp("file:///der"),
        "el otro panel es el de la sesión"
    );
    assert!(ask.contains(&activo), "el hueco sigue pidiendo su listado");
}

/// Sin perfil, la clave sigue siendo `default`: quien nunca elija uno lee y
/// escribe exactamente donde ya escribía.
#[test]
fn sin_perfil_la_clave_sigue_siendo_default() {
    let app = app_basica();
    let body = app.session_body();
    assert!(body.layouts.contains_key("default"));
    assert_eq!(body.active, "");
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

/// Restaurar una sesión NO apaga la fila `..`.
///
/// El fallo que cierra: `apply_session` reemplazaba el pane entero y volvía a
/// poner solo el orden y los ocultos, así que `[ui] parent_entry = true` se
/// convertía en `false` en silencio a partir de la primera sesión guardada.
/// El lector lo veía como «el TUI no tiene fila de subir y la ventana sí»,
/// que es la misma configuración diciendo dos cosas.
#[test]
fn restaurar_una_sesion_no_apaga_la_fila_de_subir() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    assert!(
        app.panes.browser(slot).expect("listado").is_parent_row(0),
        "de partida la lleva"
    );

    let body = app.session_body();
    let mut otra = app_basica();
    otra.set_parent_row(true);
    otra.apply_session(&body);
    assert!(
        otra.panes.browser(slot).expect("listado").is_parent_row(0),
        "y después de aplicar la sesión, también"
    );
}

/// Y un listado que nace FUERA —el que trae el arranque para cada hueco de la
/// sesión— la lleva por la misma puerta.
///
/// Esa era la segunda mitad del mismo agujero: `restore_slots` sustituía el
/// pane por uno recién listado y le devolvía el orden y los ocultos, nunca la
/// fila. Una puerta sola para adoptar un listado ajeno, y las dos quedan
/// tapadas.
#[test]
fn un_listado_adoptado_llega_con_la_fila_de_subir() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(app.panes.browser(slot).expect("listado").is_parent_row(0));

    app.set_parent_row(false);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(
        !app.panes.browser(slot).expect("listado").is_parent_row(0),
        "y apagada tampoco se cuela"
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

/// Un hueco HUÉRFANO —que el almacén todavía tiene y el layout ya no— se
/// conserva tal cual, no se readopta.
///
/// La pregunta «¿tiene este layout el hueco?» se le hacía al almacén de
/// panes, y los huérfanos siguen ahí: contestaba que sí, así que el hueco
/// entraba por la puerta de adopción —perdiendo el estado que la sesión
/// guardaba de él— en vez de volver a escribirse intacto. Volver a la
/// disposición de ayer tiene que devolver el panel donde estaba.
#[test]
fn un_hueco_huerfano_del_almacen_se_conserva_en_vez_de_readoptarse() {
    let mut app = app_basica();
    // Un layout de un solo hueco: el segundo queda huérfano en el almacén.
    let solo = norte_frontend::layout::presets::tree("simple").expect("preset");
    app.set_layout(solo);
    let fuera = SlotId(2);
    assert!(
        app.panes.browser(fuera).is_some(),
        "el almacén conserva el huérfano"
    );
    assert!(
        !app.layout.slot_ids().contains(&fuera),
        "y el layout ya no lo coloca"
    );

    let mut body = app.session_body();
    let mut estado = body.slots.values().next().expect("uno").clone();
    estado.path = vp("file:///de-ayer");
    body.slots.insert(fuera.0, estado);
    app.apply_session(&body);

    assert_eq!(
        app.session_body().slots.get(&fuera.0).map(|s| &s.path),
        Some(&vp("file:///de-ayer")),
        "vuelve a escribirse tal cual, sin pasar por el pane"
    );
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

/// `[profile.start]` abre el hueco del que la sesión no sabe nada.
///
/// Es lo que hace útil un perfil recién creado, o uno que llega de otra
/// máquina. La clave la escribían los dos frontends y no la leía NINGUNO, con
/// dos ficheros prometiendo que sí.
#[test]
fn profile_start_siembra_un_hueco_sin_sesion() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    // Por `apply_session_value`, que es el camino de verdad: es donde el
    // terminal apunta de qué huecos SABE lo guardado, y ese apunte es el veto.
    // Llamando al sembrador con un cuerpo a mano se probaba el arnés.
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    let sembrados = app.seed_profile_start(&start);

    assert_eq!(sembrados, vec![SlotId(1)]);
    assert_eq!(
        app.panes.browser(SlotId(1)).expect("hay listado").dir(),
        &vp("file:///fotos")
    );
}

/// Y solo la PRIMERA vez. Sin sesión guardada —una instalación nueva— la
/// sesión no sabe nunca nada de ningún hueco, así que sin llevar la cuenta de
/// lo sembrado, entrar y salir del perfil sacaba al lector de donde estaba en
/// cada vuelta: la decisión 2 de la ADR 0098 puesta del revés.
#[test]
fn un_hueco_ya_sembrado_no_se_vuelve_a_sembrar() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);
    assert_eq!(app.seed_profile_start(&start), vec![SlotId(1)]);

    // El lector se va a otro sitio y vuelve a entrar al perfil.
    app.adoptar_pane(
        SlotId(1),
        Pane::new(vp("file:///trabajo"), Vec::new()),
        None,
        None,
    );

    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes.browser(SlotId(1)).expect("hay listado").dir(),
        &vp("file:///trabajo"),
        "sembrar es de la primera vez"
    );
}

/// Y la SESIÓN gana: `[profile.start]` dice dónde abre un hueco la primera
/// vez, no cada vez.
///
/// Un perfil es un espacio de trabajo, no un marcador que te devuelve al
/// principio: si cada entrada te sacara de donde estabas, sería inservible
/// justo para quien lo usa a diario.
#[test]
fn la_sesion_gana_a_profile_start() {
    let mut app = app_basica();
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: vp("file:///donde/lo/dejaste"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    assert!(
        app.seed_profile_start(&start).is_empty(),
        "no se siembra un hueco que la sesión ya coloca"
    );
    assert_eq!(
        app.panes.browser(SlotId(1)).expect("hay listado").dir(),
        &vp("file:///donde/lo/dejaste")
    );
}

/// Un id que el perfil nombra y esta disposición no coloca no tiene dónde
/// abrir: se cae en silencio en vez de acuñar un hueco que nadie pidió.
#[test]
fn profile_start_no_inventa_un_hueco_que_el_layout_no_tiene() {
    let mut app = app_basica();
    let start = std::collections::BTreeMap::from([(4242, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert!(app.panes.browser(SlotId(4242)).is_none());
}

/// Y tampoco pisa un hueco HUÉRFANO: uno que el almacén guarda y esta
/// disposición no coloca.
///
/// Al almacén no se le puede preguntar «¿existe?»: guarda los huérfanos para
/// cuando se vuelva a su disposición, y `insert` los REVIVE. Sembrando ahí, el
/// lector vuelve a aquella disposición y se encuentra el panel en el
/// directorio de arranque del perfil en vez de donde lo dejó — que es la
/// promesa que el almacén tiene escrita. Se le pregunta al LAYOUT, que es la
/// misma corrección que `apply_session` documenta treinta líneas más arriba.
#[test]
fn profile_start_no_revive_un_hueco_huerfano() {
    let mut app = app_basica();
    // El hueco 3 existe en el almacén y NO en la disposición.
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(3), KindId::browser()),
        ],
    ));
    app.adoptar_pane(
        SlotId(3),
        Pane::new(vp("file:///lo/de/ayer"), Vec::new()),
        None,
        None,
    );
    app.set_layout(Node::slot(SlotId(1), KindId::browser()));

    let start = std::collections::BTreeMap::from([(3, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes.browser(SlotId(3)).map(Pane::dir),
        Some(&vp("file:///lo/de/ayer")),
        "el huérfano sigue donde estaba, para cuando se vuelva a su disposición"
    );
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
