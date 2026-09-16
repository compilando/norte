use super::*;

// ---------------------------------------------------------------------------
// La pantalla de arranque (spec 2026-09-15, ADR 0115): qué la pone, qué la
// quita, y qué promete su pie.
// ---------------------------------------------------------------------------

/// Un host con el modo de pantalla que se le pida.
///
/// Los demás constructores de prueba fijan `ajustes_de_prueba()` por dentro,
/// y aquí lo que se prueba ES la clave de configuración: `brief` se quita
/// sola, `home` se queda hasta que alguien la toque, y `off` no pone nada.
async fn host_con_splash(
    backend: Arc<Falso>,
    modo: norte_config::load::SplashMode,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut settings = norte_ui_host::ajustes_por_defecto();
    settings.common.ui_parent_entry = Some(false);
    settings.common.ui_chrome.splash = Some(modo);
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
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

/// `off` no pone nada: la clave se respeta, no se negocia.
#[tokio::test]
async fn apagada_no_pone_pantalla() {
    let (h, _snap) = host_con_splash(arbol(), norte_config::load::SplashMode::Off).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host vivo");
    let foto = crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(foto.splash.is_none(), "con `splash = off` no se pone nada");
}

/// `brief` trae su PLAZO, porque quien lo cumple es el renderer.
///
/// Del lado de la ventana no hay bucle de eventos que despierte al host —eso
/// es del terminal—, así que una pantalla que se promete breve y no dice
/// cuánto le queda se quedaría puesta hasta que alguien tocara una tecla.
#[tokio::test]
async fn la_breve_dice_cuanto_le_queda() {
    let (h, _snap) = host_con_splash(arbol(), norte_config::load::SplashMode::Brief).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host vivo");
    let v = crate::sync::siguiente_foto_tras_resync(&h, &mut sub)
        .await
        .splash
        .expect("se puso");
    let queda = v.close_after_ms.expect("la breve trae plazo");
    assert!(
        queda > 0 && i64::from(queda) <= norte_frontend::splash::BRIEF_MS,
        "el plazo es lo que le QUEDA, no un instante de otro reloj: {queda}"
    );
    // Y va sin secciones: se quita sola, así que una lista de sitios sería
    // una oferta que se retira antes de poder aceptarla.
    assert!(v.sections.is_empty(), "la breve no ofrece sitios");
}

/// Una tecla cualquiera la quita, y no significa nada más.
///
/// Lo que está delante manda: escribir esa tecla en el listado de detrás
/// sería actuar sobre algo que el lector no está viendo.
#[tokio::test]
async fn una_tecla_cualquiera_la_quita_y_no_llega_al_listado() {
    let (h, snap) = host_con_splash(arbol(), norte_config::load::SplashMode::Home).await;
    let antes = listado(&snap).path_display.clone();
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host vivo");
    assert!(
        crate::sync::siguiente_foto_tras_resync(&h, &mut sub)
            .await
            .splash
            .is_some(),
        "se puso"
    );

    h.dispatch(tecla("j")).await.expect("host vivo");
    let foto = crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(foto.splash.is_none(), "la tecla la quitó");
    assert_eq!(
        listado(&foto).cursor,
        listado(&snap).cursor,
        "y la tecla NO movió el cursor del listado de detrás"
    );
    assert_eq!(listado(&foto).path_display, antes, "ni navegó a otro sitio");
}

/// `home` ofrece sitios por número, y el número ABRE.
///
/// La pantalla los pinta numerados y su pie lo promete; sin esto las filas
/// serían una oferta que no se puede aceptar.
#[tokio::test]
async fn en_casa_un_digito_abre_su_fila() {
    let (h, snap) = host_con_splash(arbol(), norte_config::load::SplashMode::Home).await;
    // Una visita primero: la lista sale de a dónde SUELES ir, y un host
    // recién arrancado no ha ido a ninguna parte.
    let b = listado(&snap);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");
    asentar().await;

    // La suscripción va DESPUÉS de navegar: los sobres de la navegación se
    // quedan en la cola, y `siguiente_foto` devolvería una foto anterior a
    // que la pantalla se pusiera —verde o rojo según lo que hubiera dejado
    // el aterrizaje, que es un test que no demuestra nada.
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SplashOpen).await.expect("host vivo");
    let v = crate::sync::siguiente_foto_tras_resync(&h, &mut sub)
        .await
        .splash
        .expect("se puso");
    let fila = v
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|f| f.number == 1)
        .expect("hay una fila numerada");
    assert!(!fila.label.is_empty(), "la fila dice a dónde va");

    h.dispatch(tecla("1")).await.expect("host vivo");
    let foto = crate::sync::siguiente_foto_tras_resync(&h, &mut sub).await;
    assert!(foto.splash.is_none(), "abrir una fila también la quita");
}
