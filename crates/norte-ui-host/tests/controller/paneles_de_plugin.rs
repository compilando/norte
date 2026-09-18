use super::*;

// ---------------------------------------------------------------------------
// El panel que pinta un PLUGIN (fase 3): qué se le pide, cuándo se le pide, y
// qué se hace con lo que conteste.
//
// Lo que NO se prueba aquí, dicho para que no se lea como un olvido: un
// testigo caducado y un hueco que cambia de plugin a media petición no se
// pueden provocar desde fuera —el estado del controlador es `pub(super)`—, así
// que esa lógica vive probada en el terminal (`panelplugin::adoptar` y sus
// tests), donde es la misma decisión escrita una vez.
// ---------------------------------------------------------------------------

const PLUGIN: &str = "acme.git";
const KIND: &str = "plugin:acme.git:status";
const SLOT: u32 = 9;

/// Un plugin consentido que aporta el panel `status`.
fn extension_con_panel() -> norte_proto::methods::PluginInfo {
    let mut e = extension(PLUGIN, "Git de ACME", false);
    "panel".clone_into(&mut e.category);
    e.panels = vec![norte_proto::methods::PluginPanelInfo {
        kind: "status".to_owned(),
        title: "Git".to_owned(),
        min_cols: None,
        min_rows: None,
    }];
    e
}

/// Un marco cualquiera del guest, con una zona pulsable.
fn marco(texto: &str, comando: &str) -> norte_proto::methods::PanelFrame {
    norte_proto::methods::PanelFrame {
        plugin_id: PLUGIN.to_owned(),
        lines: vec![vec![norte_proto::methods::SpanWire {
            text: texto.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }]],
        hits: vec![norte_proto::methods::PanelHit {
            row: 0,
            col: 0,
            width: 8,
            command: comando.to_owned(),
            arg: None,
        }],
        state: Some(b"opaco".to_vec()),
    }
}

/// Un host con un listado y, al lado, el hueco del panel del plugin.
async fn host_con_panel(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let arbol = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(30)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT), KindId::new(KIND)),
        ],
    };
    norte_frontend::layout::validate(&arbol).expect("el árbol es válido");
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: arbol,
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

/// El backend falso con el catálogo y el marco puestos.
fn backend_con(marco: Option<norte_proto::methods::PanelFrame>) -> Arc<Falso> {
    let base = arbol_con_plugins(vec![extension_con_panel()], &[]);
    let mut f = Falso {
        plugins: vec![extension_con_panel()].into(),
        marco_de_panel: marco,
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// El panel del hueco, si la foto lo trae con marco.
fn panel_de(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::PanelSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Panel(p) if p.slot_id == SLOT => Some(&**p),
        _ => None,
    })
}

/// Lo que el guest describe acaba en la foto, y con el título del panel.
///
/// El hueco se coloca antes de que el catálogo llegue, así que la primera foto
/// lo lleva sin marco: lo que este test fija es que la SEGUNDA, después de que
/// el panel esté declarado y su marco haya aterrizado, lo trae pintado.
#[tokio::test]
async fn el_marco_del_guest_llega_a_la_foto() {
    let backend = backend_con(Some(marco("rama main", "layout.focus-next")));
    let (h, snap) = host_con_panel(Arc::clone(&backend)).await;
    assert!(
        panel_de(&snap).is_some_and(|p| p.lines.is_empty()),
        "la primera foto lleva el hueco sin marco todavía"
    );
    let mut sub = h.subscribe();
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let vista = siguiente_foto(&mut sub).await;
    let panel = panel_de(&vista).expect("el hueco sigue siendo un panel");
    assert_eq!(panel.title, "status", "el título es el kind, sin prefijo");
    let texto: String = panel
        .lines
        .iter()
        .flat_map(|l| l.iter().map(|s| s.text.clone()))
        .collect();
    assert_eq!(texto, "rama main");
    assert_eq!(panel.hits.len(), 1, "y su zona, sin comando");
}

/// Se pide UNA vez por firma: no una por mensaje del actor.
///
/// Es el fallo que el terminal tuvo que arreglar —una RPC por frame pintado—,
/// y aquí el equivalente sería una por tecla.
#[tokio::test]
async fn no_se_pide_dos_veces_lo_mismo() {
    let backend = backend_con(Some(marco("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    for _ in 0..5 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        asentar().await;
    }
    let pedidos = backend.paneles_pedidos.lock().expect("mutex").len();
    assert_eq!(pedidos, 1, "cinco mensajes, una petición: {pedidos}");
}

/// Y lo que se le cuenta al guest es lo que está mirando el lector: el
/// directorio, el hueco SIN su marco, y la fila bajo el cursor.
#[tokio::test]
async fn al_guest_se_le_cuenta_donde_esta_el_lector() {
    let backend = backend_con(Some(marco("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    asentar().await;

    let pedidos = backend.paneles_pedidos.lock().expect("mutex");
    let p = pedidos.first().expect("se pidió el marco");
    assert_eq!(p.plugin_id, PLUGIN);
    assert_eq!(p.kind, "status", "el kind del plugin, sin el prefijo");
    assert_eq!(p.dir, dir(), "el directorio que el listado enseña");
    assert_eq!(p.cols, 28, "treinta celdas menos los dos bordes");
    assert!(p.cursor_name.is_some(), "y la fila bajo el cursor");
}

/// Sin plugin que lo pinte, el hueco se queda sin marco y NO se repide.
///
/// Pasa sin nada hostil: una disposición guardada que nombra un panel cuyo
/// plugin se desactivó. Repetirlo por mensaje sería una RPC por tecla.
#[tokio::test]
async fn un_panel_sin_marco_no_se_repide() {
    let backend = backend_con(None);
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    for _ in 0..4 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        asentar().await;
    }
    let pedidos = backend.paneles_pedidos.lock().expect("mutex").len();
    assert_eq!(pedidos, 1, "se intentó una vez y se anotó: {pedidos}");
}

/// Un marco firmado por OTRO plugin no se pinta.
#[tokio::test]
async fn un_marco_de_otro_plugin_no_se_pinta() {
    let mut ajeno = marco("soy otro", "layout.focus-next");
    ajeno.plugin_id = "evil.thing".to_owned();
    let backend = backend_con(Some(ajeno));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let vista = siguiente_foto(&mut sub).await;
    assert!(
        panel_de(&vista).is_some_and(|p| p.lines.is_empty()),
        "el hueco sigue sin marco"
    );
}

/// Pulsar una zona cuyo comando está FUERA de su alcance no ejecuta nada.
///
/// El plugin elige la etiqueta y el comando, y nada los ata: una zona que pone
/// «Actualizar» puede nombrar algo que copia ficheros. El consentimiento fue
/// para pintar.
#[tokio::test]
async fn una_zona_fuera_de_su_alcance_no_ejecuta_nada() {
    let backend = backend_con(Some(marco("Actualizar", "pane.unpack")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    asentar().await;

    let ack = h
        .dispatch(UiAction::PanelClick {
            slot_id: SLOT,
            row: 0,
            col: 1,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "no es un error del lector, simplemente no hace nada: {ack:?}"
    );
    assert!(
        backend.ejecutados.lock().expect("ejecutados").is_empty(),
        "y desde luego no se ejecuta"
    );
}

/// Una celda sin zona tampoco hace nada, y no es un error.
#[tokio::test]
async fn una_celda_sin_zona_no_hace_nada() {
    let backend = backend_con(Some(marco("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    asentar().await;

    let ack = h
        .dispatch(UiAction::PanelClick {
            slot_id: SLOT,
            row: 9,
            col: 40,
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
}
