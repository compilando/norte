use super::*;

// ---------------------------------------------------------------------------
// Los ajustes se ESCRIBEN desde la ventana.
//
// Hasta aquí F11 era una vitrina: enseñaba el registro compartido y decía que
// no escribía. La máquina de edición es la misma que la del terminal
// (`norte_frontend::settings::SettingsState`), y lo que estos tests fijan es
// el cableado de la ventana alrededor de ella: girar con Enter, pedir un
// valor en un diálogo, escribir en la capa que toca y aplicar en caliente.
// ---------------------------------------------------------------------------

/// La posición de una entrada del registro en la lista PLANA de los ajustes.
///
/// Cuenta a través de TODAS las secciones, en su orden: desde que hay siete,
/// el índice dentro de una sección ya no es el de la lista entera.
fn fila_de(a: &norte_ui_host::dto::SettingsView, id: &str) -> u32 {
    let mut plana = 0usize;
    for s in &a.sections {
        match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => {
                for r in rows {
                    if r.id == id {
                        return u32::try_from(plana).expect("cabe");
                    }
                    plana += 1;
                }
            }
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => plana += rows.len(),
        }
    }
    panic!("la entrada {id} está en el registro")
}

/// El valor que la vista de ajustes enseña para una entrada.
fn valor_de(a: &norte_ui_host::dto::SettingsView, id: &str) -> String {
    a.sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => {
                rows.iter().find(|r| r.id == id).map(|r| r.value.clone())
            }
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .unwrap_or_else(|| panic!("la entrada {id} está en la vista"))
}

/// Abre los ajustes y pone el cursor sobre `id`.
async fn ajustes_sobre(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    id: &str,
) -> norte_ui_host::dto::SettingsView {
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(sub).await.expect("abren");
    let fila = fila_de(&a, id);
    h.dispatch(UiAction::SettingsSelectRow { row: fila })
        .await
        .expect("host vivo");
    let a = siguiente_ajustes(sub).await.expect("siguen abiertos");
    assert_eq!(a.cursor, u64::from(fila), "el cursor está sobre {id}");
    a
}

/// Lo que hay en el `norte.toml` de la capa del usuario, si ya existe.
fn toml_de(raiz: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(raiz.join("norte.toml")).ok()
}

/// Con el buscador puesto, el cursor apunta a la fila que SE VE.
///
/// El cursor de esta ventana era plano sobre `filas ++ rutas`, y eso solo
/// valía porque no filtraba: había un `debug_assert` diciendo exactamente
/// eso. Con filtro, la tercera fila de la pantalla no es la tercera del
/// registro, y un Enter activaría otra cosa.
#[tokio::test]
async fn con_filtro_el_cursor_apunta_a_la_fila_que_se_ve() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    let total = a.total;
    assert_eq!(a.shown, total, "sin filtro se ven todos");

    h.dispatch(UiAction::SettingsQuery {
        text: "show-hidden".to_owned(),
    })
    .await
    .expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert!(
        a.shown < total,
        "el filtro tapa filas: {} de {total}",
        a.shown
    );
    assert_eq!(
        fila_de(&a, "ui.show-hidden"),
        0,
        "la fila que queda es la primera de la lista"
    );
    // Y el índice sigue listando las secciones que el filtro vació.
    assert!(
        a.index.iter().any(|s| s.visible == 0),
        "una sección vacía sigue en el índice: {:?}",
        a.index
    );
}

/// Con filtro puesto, el diálogo pregunta por el ajuste QUE SE VE.
///
/// El nombre salía de `rows()[fila]` con `fila` contando VISIBLES: filtrando
/// a «Abrir con», Enter sobre la primera fila abría la edición del editor y
/// el diálogo decía «Tema». El lector creía cambiar el tema y reescribía su
/// línea de órdenes.
#[tokio::test]
async fn con_filtro_el_dialogo_pregunta_por_el_ajuste_correcto() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    siguiente_ajustes(&mut sub).await.expect("abren");
    h.dispatch(UiAction::SettingsQuery {
        text: "@section:open-with".to_owned(),
    })
    .await
    .expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    // La primera fila visible es `ui.editor`, una de texto.
    assert_eq!(fila_de(&a, "ui.editor"), 0);
    h.dispatch(UiAction::SettingsSelectRow { row: 0 })
        .await
        .expect("host vivo");
    siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let cuerpo = foto_hasta(&h, &mut sub, "el diálogo dice de qué ajuste habla", |s| {
        s.dialogs
            .first()
            .and_then(|d| d.body.first().map(|t| t.text.clone()))
    })
    .await;
    let nombre_editor = norte_i18n::t_in(norte_i18n::Lang::Es, "setting-ui-editor-name");
    assert_eq!(
        cuerpo, nombre_editor,
        "el diálogo tiene que nombrar el ajuste que se activó"
    );
}

/// `tab` cambia de lado, y con el teclado en el índice las flechas recorren
/// SECCIONES en vez de filas — como la barra lateral de la ayuda.
#[tokio::test]
async fn tab_pasa_el_teclado_al_indice_y_las_flechas_cambian_de_seccion() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("abren");
    assert_eq!(a.focus, "list", "el teclado empieza en la lista");

    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert_eq!(a.focus, "index");

    // Abajo: la sección siguiente, y el cursor a su primera fila.
    h.dispatch(tecla("Down")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.menu-bar")),
        "«Paneles y listado» empieza en su PRIMERA fila, la barra de menú"
    );

    // Y de vuelta: las flechas mueven filas otra vez.
    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert_eq!(a.focus, "list");
    let antes = a.cursor;
    h.dispatch(tecla("Down")).await.expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert_eq!(a.cursor, antes + 1);
}

/// Un click en el índice lleva el cursor a esa sección.
#[tokio::test]
async fn saltar_a_una_seccion_pone_el_cursor_en_su_primera_fila() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host vivo");
    siguiente_ajustes(&mut sub).await.expect("abren");

    h.dispatch(UiAction::SettingsJumpSection {
        section: "open-with".to_owned(),
    })
    .await
    .expect("host vivo");
    let a = siguiente_ajustes(&mut sub).await.expect("siguen abiertos");
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.editor")),
        "«Abrir con» empieza en el editor"
    );
}

/// Restablecer quita la clave, y si OTRA capa la fija se dice: el valor no
/// vuelve al de fábrica, y callarlo mandaría al lector a buscar un bug.
#[tokio::test]
async fn restablecer_con_otra_capa_por_debajo_lo_dice() {
    let sistema = tempfile::tempdir().expect("temp");
    let usuario = tempfile::tempdir().expect("temp");
    // El sistema fija el tema; el usuario lo tapa con otro.
    std::fs::write(
        sistema.path().join("norte.toml"),
        "[ui]\ntheme = \"nord\"\n",
    )
    .expect("escribir sistema");
    std::fs::write(
        usuario.path().join("norte.toml"),
        "[ui]\ntheme = \"tokyonight\"\n",
    )
    .expect("escribir usuario");
    let (h, _snap) = host_con_capas_apiladas(sistema.path(), usuario.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.theme").await;
    let fila = fila_de(&a, "ui.theme");

    h.dispatch(UiAction::SettingsReset { row: fila })
        .await
        .expect("host vivo");

    // La clave del usuario se va...
    let escrito = foto_hasta(&h, &mut sub, "theme quitado del usuario", |_| {
        std::fs::read_to_string(usuario.path().join("norte.toml"))
            .ok()
            .filter(|s| !s.contains("tokyonight"))
    })
    .await;
    assert!(escrito.contains("[ui]"), "la sección se queda: {escrito}");
    // ...y el aviso dice que otra capa lo sigue fijando.
    let dicho = foto_hasta(&h, &mut sub, "lo dice", |s| {
        s.status
            .message
            .clone()
            .filter(|m| m.contains("otra capa") || m.contains("another layer"))
    })
    .await;
    assert!(!dicho.is_empty());
}

/// Enter sobre un booleano lo gira, lo escribe en `norte.toml` y la fila
/// enseña el valor nuevo sin cerrar nada.
#[tokio::test]
async fn girar_un_ajuste_con_enter_lo_escribe_y_refresca_la_fila() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;
    assert_eq!(valor_de(&a, "ui.show-hidden"), "false");

    h.dispatch(tecla("Enter")).await.expect("host vivo");

    // La escritura vuelve por `spawn_blocking`: se mira el FICHERO dando
    // vueltas al actor, no durmiendo un plazo.
    let escrito = foto_hasta(&h, &mut sub, "show_hidden escrito", |_| {
        toml_de(raiz.path()).filter(|s| s.contains("show_hidden = true"))
    })
    .await;
    assert!(escrito.contains("[ui]"), "en su sección: {escrito}");
    let a = foto_hasta(&h, &mut sub, "la fila enseña el valor nuevo", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.show-hidden") == "true")
    })
    .await;
    assert_eq!(
        a.cursor,
        u64::from(fila_de(&a, "ui.show-hidden")),
        "el cursor no se mueve"
    );
}

/// Una entrada de TEXTO no se gira: pide el valor en el diálogo de un campo,
/// prellenado con el actual, y confirmar lo escribe.
#[tokio::test]
async fn un_ajuste_de_texto_pide_el_valor_en_un_dialogo_y_confirmar_lo_escribe() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.font").await;

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d.len(), 1, "un diálogo, el del valor");
    assert_eq!(d[0].title_key, "modal-setting-edit");
    assert_eq!(
        d[0].input.as_deref(),
        Some(""),
        "prellenado con el valor actual, que está vacío"
    );

    h.dispatch(UiAction::DialogInput {
        id: d[0].id,
        text: "Fira Code".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d[0].id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "un valor válido se acepta: {ack:?}"
    );

    let escrito = foto_hasta(&h, &mut sub, "font escrito", |_| {
        toml_de(raiz.path()).filter(|s| s.contains("font = \"Fira Code\""))
    })
    .await;
    assert!(escrito.contains("[ui]"), "en su sección: {escrito}");
    foto_hasta(&h, &mut sub, "la fila enseña el valor nuevo", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.font") == "Fira Code")
    })
    .await;
}

/// Un entero fuera de rango se RECHAZA con el motivo, y no toca el fichero.
#[tokio::test]
async fn un_entero_fuera_de_rango_no_se_escribe_y_se_dice() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.font-size").await;

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-setting-edit");
    h.dispatch(UiAction::DialogInput {
        id: d[0].id,
        text: "99".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d[0].id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-settings-invalid-range".to_owned()
        },
        "se rechaza como tal, no como aplicado"
    );
    asentar().await;
    assert!(
        toml_de(raiz.path()).is_none_or(|s| !s.contains("font_size")),
        "nada se escribió: {:?}",
        toml_de(raiz.path())
    );
}

/// Sin capa de usuario no hay dónde escribir, y se dice en vez de callar.
#[tokio::test]
async fn sin_capa_de_usuario_no_se_escribe_y_se_dice() {
    let (h, _snap) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    assert_eq!(siguiente_aviso(&mut sub).await, "host-no-config-dir");
}

/// Girar el tema desde los ajustes lo escribe Y lo aplica: la ventana recarga
/// su configuración por el mismo camino que un cambio de perfil, así que el
/// tema nuevo llega a quien hospeda sin reiniciar.
#[tokio::test]
async fn girar_el_tema_desde_los_ajustes_lo_aplica_en_caliente() {
    use norte_ui_host::dto::NativeEffect;
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let mut nativos = h.native_effects();
    let a = ajustes_sobre(&h, &mut sub, "ui.theme").await;
    let antes = valor_de(&a, "ui.theme");

    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let efecto = tokio::time::timeout(std::time::Duration::from_secs(5), nativos.recv())
        .await
        .expect("sale el aviso de tema")
        .expect("canal vivo");
    let NativeEffect::ThemeChanged { name } = efecto else {
        panic!("el efecto es el del tema: {efecto:?}")
    };
    assert_ne!(name, antes, "el siguiente de la lista, no el mismo");
    let escrito = foto_hasta(&h, &mut sub, "theme escrito", |_| {
        toml_de(raiz.path()).filter(|s| s.contains(&format!("theme = \"{name}\"")))
    })
    .await;
    assert!(escrito.contains("[ui]"), "en su sección: {escrito}");
}

/// Con un perfil puesto DESDE EL SELECTOR, el ajuste se escribe en el
/// perfil, no en la capa del usuario.
///
/// Lo encontró la revisión: las capas del arranque solo llevan el perfil si
/// se arrancó con `--profile`, y `dir_de_escritura` miraba solo ahí. Escrito
/// en la capa del usuario, el perfil lo tapaba en la relectura y la barra
/// decía «guardado» sobre un valor sin efecto.
#[tokio::test]
async fn con_un_perfil_puesto_el_ajuste_se_escribe_en_el_perfil() {
    let raiz = tempfile::tempdir().expect("temp");
    let fotos = raiz.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&fotos).expect("mkdir");
    std::fs::write(
        fotos.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\nshow_hidden = false\n",
    )
    .expect("escribir");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "profile.pick").await;
    foto_hasta(&h, &mut sub, "el selector de perfiles con su lista", |s| {
        s.profiles
            .as_ref()
            .filter(|p| !p.rows.is_empty())
            .map(|_| ())
    })
    .await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    foto_hasta(&h, &mut sub, "el perfil puesto", |s| {
        s.profiles.is_none().then_some(())
    })
    .await;

    ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");

    let escrito = foto_hasta(&h, &mut sub, "show_hidden escrito en el perfil", |_| {
        std::fs::read_to_string(fotos.join("norte.toml"))
            .ok()
            .filter(|s| s.contains("show_hidden = true"))
    })
    .await;
    assert!(
        escrito.contains("title = \"Fotos\""),
        "el resto del perfil sigue: {escrito}"
    );
    assert!(
        toml_de(raiz.path()).is_none_or(|s| !s.contains("show_hidden")),
        "y la capa del usuario no se toca: {:?}",
        toml_de(raiz.path())
    );
    // Y la fila lo dice tras releer: el perfil ya no lo tapa.
    foto_hasta(&h, &mut sub, "la fila enseña el valor del perfil", |s| {
        s.settings
            .clone()
            .filter(|a| valor_de(a, "ui.show-hidden") == "true")
    })
    .await;
}

/// Con el prompt del valor abierto, un doble clic no escribe ni apila otro
/// prompt: el ratón respeta el diálogo igual que el teclado.
#[tokio::test]
async fn con_el_prompt_abierto_el_doble_clic_no_hace_nada() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.font").await;
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await;
    assert_eq!(d.len(), 1);

    let ack = h
        .dispatch(UiAction::SettingsActivate {
            row: fila_de(&a, "ui.show-hidden"),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "con un diálogo delante el clic es obsoleto: {ack:?}"
    );
    asentar().await;
    assert!(
        toml_de(raiz.path()).is_none(),
        "nada se escribió: {:?}",
        toml_de(raiz.path())
    );
    let foto = foto_hasta(&h, &mut sub, "una foto", |s| Some(s.dialogs.len())).await;
    assert_eq!(foto, 1, "sigue UN diálogo, el del valor");
}

/// El doble clic sobre una fila hace lo que Enter: girarla.
#[tokio::test]
async fn un_doble_clic_activa_la_fila_como_enter() {
    let raiz = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_con_capas(raiz.path()).await;
    let mut sub = h.subscribe();
    let a = ajustes_sobre(&h, &mut sub, "ui.show-hidden").await;

    h.dispatch(UiAction::SettingsActivate {
        row: fila_de(&a, "ui.show-hidden"),
    })
    .await
    .expect("host vivo");

    foto_hasta(&h, &mut sub, "show_hidden escrito por el ratón", |_| {
        toml_de(raiz.path()).filter(|s| s.contains("show_hidden = true"))
    })
    .await;
}
