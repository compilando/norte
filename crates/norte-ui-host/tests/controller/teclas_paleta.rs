use super::*;

// ---------------------------------------------------------------------------
// Which-key: qué continúa un prefijo a medias (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Un prefijo a medias enseña QUÉ puede seguir, con la etiqueta de cada
/// tecla en el idioma del usuario y diciendo cuáles no se pueden hacer aquí.
///
/// Lo construye `norte_frontend::whichkey`, el mismo modelo que pinta el TUI:
/// el renderer no sabe resolver un prefijo, solo pintar lo que continúa.
#[tokio::test]
async fn un_prefijo_a_medias_ensena_lo_que_sigue() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};

    let preset = parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset de fábrica"),
    )
    .expect("preset parsea");
    // Una secuencia de dos teclas, que es lo que which-key existe para
    // enseñar. Ningún preset de fábrica las usa en `pane`.
    let capa = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [
    { on = ["ctrl+x", "g"], run = "cursor.top" },
    { on = ["ctrl+x", "b"], run = "cursor.bottom" },
]
"#,
    )
    .expect("capa parsea");
    let keymap = Effective::build_for(
        &preset,
        &[capa],
        &norte_ui_host::commands::todos(),
        Screen::Browse,
    )
    .expect("efectivo");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap,
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("arranca");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "x".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let panel = siguiente_whichkey(&mut sub).await.expect("hay panel");
    assert!(
        !panel.title.is_empty(),
        "el panel dice qué prefijo describe"
    );
    let teclas: Vec<&str> = panel.rows.iter().map(|r| r.chord.as_str()).collect();
    assert!(
        teclas.contains(&"g") && teclas.contains(&"b"),
        "enseña las dos continuaciones: {teclas:?}"
    );
    for fila in &panel.rows {
        assert!(!fila.label.is_empty(), "cada tecla dice qué hace: {fila:?}");
    }

    // Y al completar la secuencia, el panel se va: describía teclas que ya no
    // están vivas.
    h.dispatch(tecla("g")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.whichkey.is_none(), "la secuencia se cerró");
}

/// Espera la siguiente actualización que traiga el panel de continuaciones.
pub(super) async fn siguiente_whichkey(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::WhichKeyView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::WhichKey { whichkey } = c {
                    return whichkey.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con panel");
}

// ---------------------------------------------------------------------------
// La paleta de comandos (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga la paleta.
pub(super) async fn siguiente_paleta(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PaletteView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Palette { palette } = c {
                    return palette.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con paleta");
}

pub(super) fn tecla_de(k: &str) -> UiAction {
    tecla(k)
}

/// `ctrl+p` abre la paleta con TODO lo que el host implementa, cada fila con
/// su descripción y su atajo real.
#[tokio::test]
async fn la_paleta_ofrece_lo_que_el_host_implementa() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    assert!(p.query.is_empty(), "arranca sin filtro");
    assert_eq!(
        usize::try_from(p.total).unwrap_or(usize::MAX),
        norte_ui_host::commands::todos().len(),
        "ofrece todo lo implementado, ni más ni menos"
    );
    assert_eq!(p.rows.len() as u64, p.total, "sin filtro se ven todas");
    let entrar = p
        .rows
        .iter()
        .find(|r| r.text == "nav.enter")
        .expect("nav.enter está");
    assert!(!entrar.desc.is_empty(), "cada fila dice qué hace");
    assert_ne!(
        entrar.chord, "—",
        "y el atajo sale del preset, no de una lista a mano"
    );
}

/// Teclear ACOTA, y lo que se corre es lo seleccionado — por el mismo camino
/// que una tecla.
#[tokio::test]
async fn teclear_en_la_paleta_acota_y_enter_ejecuta() {
    let (h, snap) = host_arbol(arbol()).await;
    let cursor_antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await;

    for c in ["c", "u", "r", "s", "o", "r"] {
        h.dispatch(tecla_de(c)).await.expect("host vivo");
    }
    // Una foto, no el siguiente parche: hay seis en la cola y el primero
    // describe la paleta tras la PRIMERA letra.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let filtrada = siguiente_foto(&mut sub)
        .await
        .palette
        .expect("sigue abierta");
    assert!(
        !filtrada.rows.is_empty()
            && filtrada.rows.len() < usize::try_from(filtrada.total).unwrap_or(usize::MAX),
        "teclear acota: {} de {}",
        filtrada.rows.len(),
        filtrada.total
    );
    assert!(
        filtrada.rows.iter().all(|r| {
            // El filtro casa sobre lo PINTADO —nombre y descripción—, que es
            // lo que el modelo compartido pliega: una fila cuya descripción
            // habla del cursor casa igual, y eso es lo correcto.
            let heno = format!("{} {}", r.text, r.desc).to_lowercase();
            heno.contains("cursor")
        }),
        "y lo que queda casa con lo tecleado: {:?}",
        filtrada.rows
    );

    // Bajar y ejecutar: la paleta se cierra y el comando corre.
    h.dispatch(tecla_de("ArrowDown")).await.expect("host vivo");
    h.dispatch(tecla_de("Enter")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.palette.is_none(), "la paleta se cierra al ejecutar");
    assert_ne!(
        listado(&foto).cursor,
        cursor_antes,
        "y el comando de cursor se ejecutó"
    );
}

/// `esc` la cierra sin ejecutar nada.
#[tokio::test]
async fn escape_cierra_la_paleta_sin_ejecutar() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await;
    h.dispatch(tecla_de("Escape")).await.expect("host vivo");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "la paleta se cierra"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "y no ejecutó nada");
}

/// Ejecutar desde la paleta CIERRA la paleta en el flujo de parches, no solo
/// en la siguiente foto.
///
/// Un renderer que aplica parches —que es lo que hace el de referencia, y
/// para lo que existe la secuencia— no puede enterarse de que la paleta se
/// cerró solo si pide un `Resync`. Antes de este test, `enter` mandaba el
/// parche del COMANDO y ninguno de la paleta: la lista se quedaba pintada
/// encima del listado hasta que algo, por otro motivo, provocaba una foto.
#[tokio::test]
async fn ejecutar_en_la_paleta_manda_su_cierre_en_un_parche() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("la paleta abre");

    h.dispatch(tecla_de("Enter")).await.expect("host vivo");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "el cierre viaja como parche, sin esperar a una foto"
    );
}
