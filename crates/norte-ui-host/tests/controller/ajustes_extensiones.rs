use super::*;

// ---------------------------------------------------------------------------
// Los ajustes: lo que enseñan (tarea 4.5). Escribirlos es
// `ajustes_escritura.rs`.
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga los ajustes.
pub(super) async fn siguiente_ajustes(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SettingsView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Settings { settings } = c {
                    return settings.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con ajustes");
}

/// Un host con unas rutas dichas, para la sección de diagnóstico.
pub(super) async fn host_con_rutas(paths: norte_ui_host::settings::HostPaths) -> UiHost {
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths,
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
    .0
}

/// `F11` abre los ajustes con el registro COMPARTIDO y su valor efectivo, y
/// cada fila dice si cambiarla hace efecto ya o al reiniciar.
#[tokio::test]
async fn los_ajustes_ensenan_el_registro_compartido_con_su_valor() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");

    let general = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .expect("hay sección general");
    assert_eq!(
        general.len(),
        norte_frontend::settings::catalog().len(),
        "ni una entrada del catálogo compartido se queda fuera"
    );
    for r in general {
        assert!(!r.id.is_empty(), "cada fila lleva su id estable");
        assert!(!r.name.is_empty(), "y su nombre traducido: {r:?}");
        assert!(
            !r.name.starts_with("setting-"),
            "ninguna pinta una clave Fluent: {r:?}"
        );
    }
    // Lo que se aplica en caliente lo dice el catálogo compartido, que es lo
    // que el terminal enseña; y lo que la ventana no puede aplicar —idioma,
    // fuentes, movimiento— lo dice `fuera_de_alcance_en_caliente` al escribir.
    let vivo = general
        .iter()
        .find(|r| r.id == "ui.theme")
        .expect("el tema está");
    assert!(
        !vivo.restart_required,
        "el tema se aplica en caliente al escribirlo, y la fila no dice lo contrario"
    );
    let frio = general
        .iter()
        .find(|r| r.id == "ui.lang")
        .expect("el idioma está");
    assert!(
        frio.restart_required,
        "el idioma pide reiniciar la ventana, y la fila lo dice"
    );
}

/// Una ubicación con su existencia resuelta, como la resuelve el arranque.
pub(super) fn sitio(p: std::path::PathBuf) -> norte_ui_host::settings::HostPath {
    norte_ui_host::settings::HostPath {
        missing: !p.exists(),
        path: p,
    }
}

/// La sección de ubicaciones dice dónde vive cada cosa, marca lo que falta y
/// no enseña ni un valor.
#[tokio::test]
async fn las_ubicaciones_se_dicen_y_lo_que_falta_se_marca() {
    let tmp = tempfile::tempdir().expect("tmp");
    let existe = tmp.path().join("config");
    std::fs::create_dir(&existe).expect("mkdir");
    let no_existe = tmp.path().join("no-esta");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        // El `missing` lo trae YA resuelto quien arranca: el host no hace
        // I/O al proyectar, y el test lo dice porque es el contrato.
        config_layers: vec![
            (
                norte_ui_host::settings::ConfigLayer::User,
                sitio(existe.clone()),
            ),
            (
                norte_ui_host::settings::ConfigLayer::Project,
                sitio(no_existe),
            ),
        ],
        state_dir: None,
        logs_dir: None,
        socket: Some(sitio(tmp.path().join("daemon.sock"))),
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");

    let rutas = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("hay sección de rutas");
    assert_eq!(rutas.len(), 3, "dos capas y el socket");
    assert!(!rutas[0].missing, "la capa que existe no se marca");
    assert!(
        rutas[1].missing,
        "la que no existe SÍ: no se pinta como si estuviera"
    );
    for r in rutas {
        assert!(!r.label.is_empty(), "cada una dice QUÉ es: {r:?}");
        assert!(!r.display.is_empty(), "y dónde: {r:?}");
    }
}

/// Un directorio de configuración con bytes hostiles llega ENMASCARADO y
/// marcado, por el mismo camino que un nombre del listado.
#[tokio::test]
async fn una_ruta_hostil_llega_enmascarada_y_marcada() {
    let tmp = tempfile::tempdir().expect("tmp");
    // Un nombre con un override bidi: legal como fichero, y una mentira en
    // pantalla si se pinta crudo.
    let hostil = tmp.path().join("conf\u{202e}gif");
    std::fs::create_dir(&hostil).expect("mkdir");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        config_layers: vec![(norte_ui_host::settings::ConfigLayer::User, sitio(hostil))],
        state_dir: None,
        logs_dir: None,
        socket: None,
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    let rutas = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("hay sección de rutas");
    assert!(
        !rutas[0].display.contains('\u{202e}'),
        "un override bidi cruzó crudo: {:?}",
        rutas[0].display
    );
    assert!(rutas[0].hostile, "y se MARCA que difiere del nombre real");
}

/// El cursor se mueve y no se sale, y `enter` sin dónde escribir lo dice.
#[tokio::test]
async fn el_cursor_no_se_sale_y_enter_lo_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    assert_eq!(a.cursor, 0);

    h.dispatch(tecla("ArrowUp")).await.expect("host vivo");
    let arriba = siguiente_ajustes(&mut sub).await.expect("sigue abierto");
    assert_eq!(arriba.cursor, 0, "arriba del todo no se sale por arriba");

    h.dispatch(tecla("End")).await.expect("host vivo");
    let final_ = siguiente_ajustes(&mut sub).await.expect("sigue abierto");
    let total: usize = final_
        .sections
        .iter()
        .map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => rows.len(),
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => rows.len(),
        })
        .sum();
    assert_eq!(
        usize::try_from(final_.cursor).expect("cabe"),
        total - 1,
        "y por abajo tampoco"
    );

    // La última fila de un host sin rutas es `keymap.preset`, que gira; y
    // este host no tiene capa de usuario, así que no hay dónde escribirlo.
    // Se dice, en vez de no hacer nada.
    let ack = h.dispatch(tecla("Enter")).await.expect("host vivo");
    assert_eq!(
        ack,
        norte_ui_host::ActionAck::Unavailable {
            reason_key: "host-no-config-dir".to_owned()
        },
        "enter sin dónde escribir lo dice: {ack:?}"
    );

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    foto_hasta(&h, &mut sub, "esc los cierra", |s| {
        s.settings.is_none().then_some(())
    })
    .await;
}

/// Con los ajustes abiertos, una tecla del listado no se cuela.
#[tokio::test]
async fn con_los_ajustes_abiertos_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let _ = siguiente_ajustes(&mut sub).await.expect("abren");

    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "el listado no se movió");
    assert!(foto.settings.is_some(), "y los ajustes siguen abiertos");
}

// ---------------------------------------------------------------------------
// El gestor de extensiones (tarea 4.5).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga el gestor.
pub(super) async fn siguiente_extensiones(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ExtensionsView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Extensions { extensions } = c {
                    return extensions.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con extensiones");
}

/// Espera a que el catálogo haya llegado (deje de estar cargando).
pub(super) async fn extensiones_cargadas(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ExtensionsView {
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(sub).await else {
            continue;
        };
        if !v.loading {
            return v;
        }
    }
    panic!("el catálogo nunca llegó");
}

/// `F12` abre el gestor: primero diciendo que carga, luego con el catálogo
/// saneado y su estado de aprobación.
#[tokio::test]
async fn el_gestor_ensena_lo_instalado_y_su_estado() {
    let mut backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true), {
            let mut p = extension("org.norte.demo", "Demo", false);
            p.approved = false;
            p.enabled = false;
            p.capabilities = vec!["fs-read".to_owned()];
            p
        }],
        &[],
    );
    // Un directorio que no cargó: se enseña, porque una extensión que
    // desaparece en silencio es una que el usuario cree tener.
    std::sync::Arc::get_mut(&mut backend)
        .expect("única referencia")
        .errores_de_carga = vec![(
        "/plugins/roto".to_owned(),
        "el manifiesto no parsea".to_owned(),
    )];
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F12")).await.expect("host vivo");
    let primera = siguiente_extensiones(&mut sub).await.expect("abre");
    assert!(
        primera.loading,
        "se abre DICIENDO que carga: una lista vacía sin ese aviso se lee \
         como «no tienes ninguna»"
    );

    let v = extensiones_cargadas(&mut sub).await;
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].id, "acme.ftp");
    assert!(v.rows[0].approved && v.rows[0].enabled);
    assert!(!v.rows[1].approved, "y la que no está aprobada se ve");
    assert_eq!(
        v.rows[1].capabilities,
        vec!["fs-read".to_owned()],
        "las capabilities van en la FILA: son la decisión que se aprueba"
    );
    assert_eq!(v.errors.len(), 1, "y lo que no cargó se dice");
}

/// `enter` sobre una extensión pide su esquema `[config]` y lo enseña con el
/// valor efectivo.
#[tokio::test]
async fn la_ficha_ensena_el_esquema_con_su_valor_efectivo() {
    let mut backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("única referencia")
        .esquemas
        .insert(
            "acme.ftp".to_owned(),
            vec![norte_proto::methods::PluginConfigKeyWire {
                key: "timeout".to_owned(),
                kind: "int".to_owned(),
                default: "10".to_owned(),
                min: Some(1),
                max: Some(300),
                values: Vec::new(),
                description: Some("Segundos antes de rendirse".to_owned()),
                value: "30".to_owned(),
            }],
        );
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let mut ficha = None;
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            ficha = v.detail;
            break;
        }
    }
    let d = ficha.expect("la ficha llega");
    assert_eq!(d.id, "acme.ftp");
    assert_eq!(d.config.len(), 1);
    let k = &d.config[0];
    assert_eq!(k.key, "timeout");
    assert_eq!(k.value, "30", "el valor EFECTIVO, no el del esquema");
    assert_eq!(k.default, "10", "y el del esquema, para ver qué se cambió");
    assert!(!k.domain.is_empty(), "y qué lo acota: {k:?}");
    assert!(
        !k.domain.contains("ext-config-"),
        "sin pintar una clave Fluent: {k:?}"
    );
}

/// Moverse tira la ficha: describe otra extensión.
#[tokio::test]
async fn moverse_tira_la_ficha() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.ftp", "FTP de ACME", false),
            extension("org.norte.demo", "Demo", false),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }
    // Con la ficha abierta, las flechas son SUYAS: recorren sus claves. Este
    // catálogo no declara ninguna, y entonces no se las queda —una ficha sin
    // nada que andar dejaría al lector sin poder moverse sin cerrarla—, así
    // que esta baja el catálogo y tira la ficha.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let v = siguiente_extensiones(&mut sub)
        .await
        .expect("sigue abierto");
    assert_eq!(v.cursor, 1);
    assert!(
        v.detail.is_none(),
        "la ficha de la anterior no puede quedarse describiendo a otra"
    );
}

/// El primer `esc` cierra la FICHA; el segundo, el gestor.
#[tokio::test]
async fn el_primer_esc_cierra_la_ficha_y_el_segundo_el_gestor() {
    let backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let sin_ficha = siguiente_extensiones(&mut sub)
        .await
        .expect("sigue abierto");
    assert!(sin_ficha.detail.is_none(), "el primer esc cierra la ficha");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_extensiones(&mut sub).await.is_none(),
        "el segundo cierra el gestor"
    );
}

/// Dos extensiones para los botones (puente 61): una aprobada y encendida,
/// y una sin aprobar.
fn dos_para_gobernar() -> Arc<Falso> {
    arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true), {
            let mut p = extension("org.norte.demo", "Demo", false);
            p.approved = false;
            p.enabled = false;
            p.capabilities = vec!["fs-read".to_owned()];
            p
        }],
        &[("acme.ftp", "Conecta con un servidor FTP.")],
    )
}

/// El botón de aprobar abre la MISMA pregunta que la tecla, con las
/// capabilities dentro, y señala la fila: un botón no es un atajo para
/// saltarse el consentimiento.
#[tokio::test]
async fn el_boton_de_aprobar_abre_la_misma_pregunta_que_la_tecla() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(UiAction::ExtensionGovern {
        row: 1,
        id: "org.norte.demo".to_owned(),
        change: norte_ui_host::action::ExtensionChange::Approval,
    })
    .await
    .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    let pregunta = d.last().expect("pregunta");
    assert_eq!(pregunta.title_key, "modal-extension-approve-title");
    // Con la pregunta delante, ningún botón del gestor hace nada: es modal
    // para el ratón como para el teclado. Un clic detrás revocaría sin
    // preguntar, o cerraría el gestor bajo la pregunta.
    for accion in [
        UiAction::ExtensionGovern {
            row: 0,
            id: "acme.ftp".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Approval,
        },
        UiAction::ExtensionHelp {
            row: 0,
            id: "acme.ftp".to_owned(),
        },
    ] {
        let ack = h.dispatch(accion).await.expect("host vivo");
        assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    }
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
    assert!(
        pregunta.body.iter().any(|l| l.text == "fs-read"),
        "las capabilities van dentro: {:?}",
        pregunta.body
    );
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "nada viaja antes del sí"
    );
    // Y la fila señalada es la del botón, no la que tenía el cursor.
    let v = siguiente_extensiones(&mut sub)
        .await
        .expect("sigue abierto");
    assert_eq!(v.cursor, 1);

    h.dispatch(UiAction::Dialog {
        id: pregunta.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    assert_eq!(
        backend.gobierno.lock().expect("gobierno").as_slice(),
        ["approval:org.norte.demo:true:digest-de-org.norte.demo"]
    );
}

/// Desinstalar PREGUNTA —por el botón y por la tecla igual— y solo el sí
/// borra; después el catálogo se repide y la fila ya no está.
#[tokio::test]
async fn desinstalar_pregunta_y_solo_el_si_borra() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    // La tecla (`d` es `dialog.remove` en orthodox): pregunta.
    h.dispatch(tecla("d")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    let pregunta = d.last().expect("pregunta");
    assert_eq!(pregunta.title_key, "modal-extension-uninstall-title");
    assert_eq!(
        pregunta.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.ftp")
    );
    let si = pregunta
        .choices
        .iter()
        .find(|c| c.id == "confirm")
        .expect("la respuesta que borra");
    assert!(si.destructive, "y viene marcada como lo que es");
    assert_eq!(si.label_key, "dialog-uninstall", "y dice QUÉ confirma");
    // Cancelar no borra nada.
    h.dispatch(UiAction::Dialog {
        id: pregunta.id,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let _ = siguientes_dialogos(&mut sub).await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());

    // El botón: la misma pregunta, y el sí borra.
    h.dispatch(UiAction::ExtensionGovern {
        row: 0,
        id: "acme.ftp".to_owned(),
        change: norte_ui_host::action::ExtensionChange::Uninstall,
    })
    .await
    .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    let pregunta = d.last().expect("pregunta");
    assert_eq!(pregunta.title_key, "modal-extension-uninstall-title");
    h.dispatch(UiAction::Dialog {
        id: pregunta.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    // El catálogo se REPIDE tras el sí y llega detrás del parche del cursor:
    // se espera al que ya no trae la borrada, no al primero que pase.
    let mut sin_ella = None;
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if !v.loading && v.rows.iter().all(|r| r.id != "acme.ftp") {
            sin_ella = Some(v);
            break;
        }
    }
    let v = sin_ella.expect("la desinstalada deja de listarse");
    assert_eq!(
        backend.gobierno.lock().expect("gobierno").as_slice(),
        ["uninstall:acme.ftp"]
    );
    assert_eq!(
        v.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["org.norte.demo"]
    );
}

/// Encender una sin aprobar por botón se rehúsa y se dice, como con la tecla.
#[tokio::test]
async fn encender_una_sin_aprobar_por_boton_se_rehusa() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    let ack = h
        .dispatch(UiAction::ExtensionGovern {
            row: 1,
            id: "org.norte.demo".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Enabled,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-extension-not-approved"),
        "{ack:?}"
    );
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
    // Una fila que ya no existe, o que ya no es la que el renderer vio —el
    // catálogo se repide de fondo y una borrada por encima corre las de
    // debajo—, no gobierna nada: apagar «la fila 0» habría apagado a la
    // vecina.
    for (row, id) in [(9, "acme.ftp"), (0, "org.norte.demo")] {
        let ack = h
            .dispatch(UiAction::ExtensionGovern {
                row,
                id: id.to_owned(),
                change: norte_ui_host::action::ExtensionChange::Enabled,
            })
            .await
            .expect("host vivo");
        assert!(
            matches!(ack, ActionAck::Stale { .. }),
            "{row} {id}: {ack:?}"
        );
    }
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// El botón de ayuda cierra el gestor y abre la ayuda en la página de ESA
/// extensión, como `F1` sobre la fila en el terminal.
#[tokio::test]
async fn el_boton_de_ayuda_abre_la_pagina_de_esa_extension() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;

    // Sin página no se abre nada, y se dice.
    let ack = h
        .dispatch(UiAction::ExtensionHelp {
            row: 1,
            id: "org.norte.demo".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");

    h.dispatch(UiAction::ExtensionHelp {
        row: 0,
        id: "acme.ftp".to_owned(),
    })
    .await
    .expect("host vivo");
    // El gestor se cierra primero: la ayuda lo sustituye, como en el
    // terminal.
    assert!(
        siguiente_extensiones(&mut sub).await.is_none(),
        "el gestor se cierra"
    );
    let mut pagina = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            pagina = Some(v);
            break;
        }
    }
    let pagina = pagina.expect("la página del plugin se abre y se instala");
    assert!(format!("{:?}", pagina.blocks).contains("Conecta con un servidor FTP"));
}

/// Un nombre, un publicador y una descripción hostiles llegan enmascarados;
/// un id inválido no llega en absoluto.
#[tokio::test]
async fn el_texto_de_una_extension_llega_enmascarado() {
    let mut malo = extension("acme.\u{202e}ftp", "Invisible", false);
    malo.description = Some("desc".to_owned());
    let mut hostil = extension("acme.ftp", "FTP\u{202e}de ACME", false);
    hostil.publisher = "ACME\u{7}".to_owned();
    hostil.description = Some("Sirve\u{202e}ficheros".to_owned());
    hostil.version = "1.0\u{7}".to_owned();
    let backend = arbol_con_plugins(vec![hostil, malo], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let v = extensiones_cargadas(&mut sub).await;

    assert_eq!(v.rows.len(), 1, "el id inválido se DESCARTA en la entrada");
    let texto = format!("{:?}", v.rows[0]);
    assert!(
        !texto.contains('\u{202e}') && !texto.contains('\u{7}'),
        "texto de tercero sin enmascarar: {texto}"
    );
    assert!(
        !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
        "texto de tercero sin enmascarar: {texto}"
    );
}

/// El VALOR de una clave de configuración, su defecto y los valores de un
/// `enum` los escribe el PLUGIN, y llegan enmascarados y marcados.
///
/// El manifiesto solo les acota la LONGITUD —`CONFIG_STRING_MAX_CHARS`,
/// `CONFIG_ENUM_MAX_VALUES`— y no comprueba charset ninguno, así que un
/// `plugin.toml` podía meter un override bidi en un valor de `enum` y verlo
/// llegar crudo a un nodo de texto del DOM. Tres rustdocs decían que esos
/// campos eran «vocabulario de norte, nunca texto libre del plugin».
///
/// Y el `·` que une el dominio se compone AQUÍ: si el valor no se enmascarara,
/// un plugin podría fabricar uno y fingir un dominio que no tiene.
#[tokio::test]
async fn el_valor_de_una_clave_de_plugin_llega_enmascarado_y_marcado() {
    let ext = extension("acme.ftp", "FTP", true);
    let mut f = Falso {
        plugins: vec![ext].into(),
        ..Falso::default()
    };
    f.arbol.clone_from(&arbol().arbol);
    f.esquemas.insert(
        "acme.ftp".to_owned(),
        vec![norte_proto::methods::PluginConfigKeyWire {
            key: "mode".to_owned(),
            kind: "enum".to_owned(),
            default: "safe\u{202e}".to_owned(),
            min: None,
            max: None,
            values: vec!["safe".to_owned(), "fast\u{202e} · read-only".to_owned()],
            description: None,
            value: "fast\u{7}".to_owned(),
        }],
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let mut ficha = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(d) = siguiente_foto(&mut sub)
            .await
            .extensions
            .and_then(|e| e.detail)
        {
            ficha = Some(d);
            break;
        }
    }
    let ficha = ficha.expect("la ficha llega");
    let fila = ficha.config.first().expect("la clave está");
    let texto = format!("{fila:?}");
    assert!(
        !texto.contains('\u{202e}') && !texto.contains('\u{7}'),
        "texto del plugin sin enmascarar: {texto}"
    );
    assert!(
        !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
        "texto del plugin sin enmascarar: {texto}"
    );
    assert!(
        fila.hostile,
        "y se DICE que lo pintado difiere de lo que es: {fila:?}"
    );
}

/// Con el gestor abierto, el listado no se mueve.
#[tokio::test]
async fn con_el_gestor_abierto_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let _ = siguiente_extensiones(&mut sub).await.expect("abre");

    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes);
    assert!(foto.extensions.is_some(), "y el gestor sigue abierto");
}

// ---------------------------------------------------------------------------
// El tema y el selector de volúmenes (tarea 4.5).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización con el tema.
pub(super) async fn siguiente_tema(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ThemeView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Theme { theme } = c {
                    return theme.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con tema");
}

/// Espera la siguiente actualización con el selector.
pub(super) async fn siguiente_selector(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PickerView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Picker { picker } = c {
                    return picker.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con selector");
}

/// Un host con un tema dicho.
pub(super) async fn host_con_tema(theme: norte_ui_host::pickers::HostTheme) -> UiHost {
    UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme,
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca")
    .0
}

/// `F9` enseña el tema rol a rol, y NOMBRA los efectos que esta ventana no
/// sabe pintar: un tema retro que se ve idéntico se lee como roto.
#[tokio::test]
async fn el_tema_se_ve_por_dentro_y_dice_lo_que_no_pinta() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "retro".to_owned(),
        roles: vec![
            ("selection-bg".to_owned(), "#2d4f8a".to_owned()),
            ("error-fg".to_owned(), "#f7768e".to_owned()),
        ],
        effects: vec!["crt".to_owned(), "scanlines".to_owned()],
    })
    .await;
    let mut sub = h.subscribe();
    // `alt+9` desde la spec 2026-09-10: F9 es el menú, como en toda la familia.
    h.dispatch(tecla_alt("9")).await.expect("host vivo");
    let t = siguiente_tema(&mut sub).await.expect("abre");

    assert_eq!(t.name, "retro");
    assert_eq!(t.roles.len(), 2);
    assert_eq!(t.roles[0].color, "#2d4f8a", "el color va como muestra");
    assert_eq!(
        t.unsupported_effects
            .iter()
            .map(|e| e.key.clone())
            .collect::<Vec<_>>(),
        vec!["crt".to_owned(), "scanlines".to_owned()],
        "los efectos se NOMBRAN, no se ignoran"
    );

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(siguiente_tema(&mut sub).await.is_none(), "esc lo cierra");
}

/// Un tema sin efectos no inventa ninguno.
#[tokio::test]
async fn un_tema_sin_efectos_no_dice_nada_de_ellos() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "default".to_owned(),
        roles: vec![("fg".to_owned(), "#d4d8de".to_owned())],
        effects: Vec::new(),
    })
    .await;
    let mut sub = h.subscribe();
    // `alt+9` desde la spec 2026-09-10: F9 es el menú, como en toda la familia.
    h.dispatch(tecla_alt("9")).await.expect("host vivo");
    let t = siguiente_tema(&mut sub).await.expect("abre");
    assert!(t.unsupported_effects.is_empty());
}

/// Un volumen del host con lo que la vista mira.
pub(super) fn volumen(mount: &str, fs: &str, ro: bool) -> norte_proto::methods::Volume {
    norte_proto::methods::Volume {
        mount: norte_proto::VPath::parse(mount).expect("vpath"),
        label: None,
        fs_type: fs.to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(100 * 1024 * 1024 * 1024),
        free_bytes: Some(12 * 1024 * 1024 * 1024),
        read_only: ro,
    }
}

/// El selector de volúmenes se abre PREGUNTANDO, y elegir uno navega el
/// panel a su punto de montaje — que es lectura, y por eso sí se hace.
#[tokio::test]
async fn elegir_un_volumen_navega_el_panel() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // `pane.select-drive` no lo ata el preset orthodox: se corre por la
    // paleta, que es otra puerta al MISMO catálogo.
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
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let primero = siguiente_selector(&mut sub).await.expect("abre");
    assert!(
        !primero.empty.is_empty() || !primero.rows.is_empty(),
        "o pregunta o trae filas, pero nunca se queda mudo"
    );

    let mut con_filas = None;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            con_filas = Some(v);
            break;
        }
    }
    let v = con_filas.expect("la tabla de montaje llega");
    assert_eq!(v.rows.len(), 1);
    assert!(v.rows[0].detail.contains("ext4"), "{:?}", v.rows[0]);
    assert!(
        v.rows[0].detail.contains("12"),
        "y cuánto queda: {:?}",
        v.rows[0]
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.picker.is_none(), "el selector se cierra");
    assert!(
        listado(&foto).path_display.contains("otro"),
        "y el panel navegó al volumen: {}",
        listado(&foto).path_display
    );
}

/// Un espacio que el sistema no contestó se DICE; jamás se pinta un `0`, que
/// se lee como «lleno» — lo contrario de «no lo sé».
#[tokio::test]
async fn un_volumen_sin_tamano_lo_dice() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let mut v = volumen("mem:///otro", "nfs4", true);
    v.total_bytes = None;
    v.free_bytes = None;
    f.volumenes = vec![v];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
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
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    for _ in 0..20 {
        let Some(view) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if let Some(fila) = view.rows.first() {
            assert!(!fila.detail.contains(" 0 "), "un cero se lee como lleno");
            assert!(
                fila.detail.contains("nfs4"),
                "y sigue diciendo lo que sí sabe: {fila:?}"
            );
            return;
        }
    }
    panic!("la tabla de montaje nunca llegó");
}

/// ADR 0100: la frase de un plugin `hook` llega a la barra de la ventana
/// atribuida al plugin, y como aviso efímero — no como banner: habla de una
/// mutación que ya pasó. El id va DELANTE, puesto por norte.
#[tokio::test]
async fn el_aviso_de_un_hook_llega_a_la_barra_atribuido() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.avisos_plugin.lock().expect("avisos_plugin") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::PluginNotice {
        plugin_id: "org.norte.rename-log".to_owned(),
        kind: "notify".to_owned(),
        text: Some("renamed 3 files".to_owned()),
    })
    .expect("el host escucha");
    let linea = foto_hasta(&h, &mut sub, "el aviso en la barra", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("renamed 3 files"))
    })
    .await;
    assert!(linea.starts_with("⚑ org.norte.rename-log"), "{linea}");
    assert!(
        f_banners_vacios(&h, &mut sub).await,
        "un aviso de hook no enciende ningún banner persistente"
    );

    // Y el aviso viaja también, con la misma línea.
    let mut sub2 = h.subscribe();
    tx.send(norte_proto::methods::PluginNotice {
        plugin_id: "org.norte.rename-log".to_owned(),
        kind: "hooks-disabled".to_owned(),
        text: None,
    })
    .expect("el host escucha");
    let aviso = super::registro::foto_hasta_notice(&mut sub2, "msg-plugin-hooks-disabled").await;
    assert!(aviso.contains("org.norte.rename-log"), "{aviso}");
}

async fn f_banners_vacios(
    h: &norte_ui_host::UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> bool {
    foto_hasta(h, sub, "los banners", |f| Some(f.status.banners.is_empty())).await
}
