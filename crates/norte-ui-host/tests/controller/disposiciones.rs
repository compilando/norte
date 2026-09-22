use super::*;

// ---------------------------------------------------------------------------
// Disposiciones: redimensionar, igualar y elegir.
// ---------------------------------------------------------------------------

/// Corre un comando por la PALETA, que es otra puerta al mismo catálogo.
///
/// Ninguno de los comandos de disposición lo ata un preset de fábrica, así
/// que este es el camino por el que llegan hoy.
pub(super) async fn por_la_paleta(h: &UiHost, sub: &mut norte_ui_host::UiSubscription, cmd: &str) {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(sub).await.expect("la paleta abre");
    for c in cmd.chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
}

/// **Con la navegación sincronizada puesta, los dos huecos andan juntos.**
///
/// Y el eco NO entra en el rastro del hueco espejado: viaja como
/// `Trail::Seed`, que es lo que impide que su «atrás» cuente un paso que el
/// lector no dio ahí — y, de paso, lo que corta la recursión.
#[tokio::test]
async fn con_la_navegacion_sincronizada_los_dos_huecos_andan_juntos() {
    // `orthodox` y no la disposición de partida: hacen falta DOS listados,
    // porque sin hueco destino no hay a quién espejar.
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();

    // Encender NO mueve nada: alinea la siguiente navegación, no la actual.
    por_la_paleta(&h, &mut sub, "sync-nav").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let antes: Vec<String> = foto
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(b.path_display.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(antes.len(), 2, "la disposición de partida son dos listados");
    assert_eq!(antes[0], antes[1], "y arrancan en el mismo sitio");

    // Ahora una navegación del hueco activo: entra en `docs`.
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let despues = foto_hasta(&h, &mut sub, "los dos listados se movieron", |foto| {
        let rutas: Vec<String> = foto
            .slots
            .iter()
            .filter_map(|s| match s {
                SlotView::Browser(b) => Some(b.path_display.clone()),
                _ => None,
            })
            .collect();
        (rutas.len() == 2 && rutas[0] != antes[0] && rutas[1] != antes[1]).then_some(rutas)
    })
    .await;
    assert_eq!(
        despues[0], despues[1],
        "el destino repitió la navegación del activo: {despues:?}"
    );
}

/// El ancho de un hueco en la foto.
pub(super) fn ancho_de(snap: &norte_ui_host::ViewSnapshot, slot: u32) -> u16 {
    snap.layout
        .placements
        .iter()
        .find(|p| p.slot_id == slot)
        .map_or(0, |p| p.width)
}

/// Crecer ensancha el hueco con el FOCO, y encoger lo devuelve.
#[tokio::test]
async fn crecer_y_encoger_mueven_el_hueco_enfocado() {
    // Una disposición con DOS listados: redimensionar reparte entre
    // hermanos, y con un hueco solo no hay a quién quitarle.
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let activo = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("hay un hueco activo");
    let antes = ancho_de(&snap, activo);

    por_la_paleta(&h, &mut sub, "layout.grow").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let crecido = siguiente_foto(&mut sub).await;
    assert!(
        ancho_de(&crecido, activo) > antes,
        "creció: {} → {}",
        antes,
        ancho_de(&crecido, activo)
    );

    por_la_paleta(&h, &mut sub, "layout.shrink").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let vuelto = siguiente_foto(&mut sub).await;
    assert_eq!(ancho_de(&vuelto, activo), antes, "y encoger lo devuelve");
}

/// Igualar deja a los hermanos con el mismo peso.
#[tokio::test]
async fn igualar_reparte_a_partes_iguales() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let activo = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("hay un hueco activo");

    // Se desequilibra y se vuelve a igualar.
    for _ in 0..3 {
        por_la_paleta(&h, &mut sub, "layout.grow").await;
    }
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let torcido = ancho_de(&siguiente_foto(&mut sub).await, activo);

    por_la_paleta(&h, &mut sub, "layout.equalize").await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let igualado = ancho_de(&siguiente_foto(&mut sub).await, activo);
    assert!(igualado < torcido, "igualar deshace el desequilibrio");
}

/// El selector ofrece las cinco de fábrica con su vista previa, y elegir una
/// CAMBIA la pantalla.
#[tokio::test]
async fn elegir_una_disposicion_cambia_la_pantalla() {
    let (h, snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let huecos_antes = snap.slots.len();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if let Some(l) = foto.layouts.clone() {
            v = Some(l);
            break;
        }
    }
    let picker = v.expect("el selector abre");
    assert_eq!(
        picker.rows.len(),
        norte_frontend::layout::presets::NAMES.len(),
        "las cinco de fábrica, y ninguna del usuario en este host"
    );
    assert!(picker.rows.iter().all(|r| r.factory));
    assert!(
        !picker.preview.is_empty(),
        "y la elegida enseña su FORMA, pintada por el mismo motor que reparte"
    );
    let anchos: std::collections::BTreeSet<usize> =
        picker.preview.iter().map(|l| l.chars().count()).collect();
    assert_eq!(anchos.len(), 1, "la miniatura es un rectángulo: {anchos:?}");

    // La disposición `full` tiene más huecos que `simple`.
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "full")
        .expect("`full` está");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert!(despues.layouts.is_none(), "el selector se cierra al elegir");
    assert!(
        despues.slots.len() > huecos_antes,
        "y la pantalla es otra: {} → {}",
        huecos_antes,
        despues.slots.len()
    );
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Places(_))),
        "con la barra lateral que `full` coloca"
    );
}

/// Una disposición del usuario que no parsea se OFRECE, sin vista previa y
/// diciendo por qué, y elegirla no cambia la pantalla.
#[tokio::test]
async fn una_disposicion_rota_se_ve_y_no_se_aplica() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("mia"),
            tree: Err("no parsea".to_owned()),
        }],
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(l) = siguiente_foto(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("el selector abre");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "mia")
        .expect("la del usuario se OFRECE aunque no parsee");
    assert!(picker.rows[i].broken, "y se dice que está rota");
    assert!(!picker.rows[i].factory);

    let ack = h
        .dispatch(UiAction::LayoutActivateRow {
            row: u32::try_from(i).expect("cabe"),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "elegirla NO cambia la pantalla por un fichero roto: {ack:?}"
    );
}

/// Una disposición cuyo REPARTO no coloca ningún listado no puede dejar al
/// host sin huecos.
///
/// `validate` garantiza que el árbol TENGA un `browser`, no que el reparto lo
/// COLOQUE: un `Tabs` cuyo activo es otro kind manda el listado a `hidden`, y
/// un split todo-ponderado que no quepa hace lo mismo con todos menos el hijo
/// 0. Sembrar los huecos desde `placements` en vez de desde el árbol vaciaba
/// el mapa, y la siguiente tecla moría en el `expect` de `hueco()` —dentro de
/// la task del actor, sin log y sin caída visible, dejando la ventana muerta
/// contestando `Down` para siempre. Es la forma de #242 en esta superficie.
#[tokio::test]
async fn una_disposicion_que_esconde_el_listado_deja_el_hueco_vivo() {
    use norte_frontend::layout::{KindId, Node, SlotId};
    let escondida = Node::Tabs {
        children: vec![
            Node::slot(SlotId(9), KindId::new("metadata")),
            Node::slot(SlotId(1), KindId::browser()),
        ],
        active: 0,
    };
    norte_frontend::layout::validate(&escondida)
        .expect("el árbol es VÁLIDO: tiene un browser, aunque el reparto no lo coloque");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("escondida"),
            tree: Ok(escondida),
        }],
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(l) = siguiente_foto(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("el selector abre");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "escondida")
        .expect("la del usuario está");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");

    // La tecla que mataba: cualquiera que toque el hueco activo.
    h.dispatch(tecla("Down"))
        .await
        .expect("el host sigue VIVO tras elegir una disposición que esconde el listado");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "y la pantalla es la que se pidió: la pestaña activa es la ficha"
    );
}
