use super::*;

// ---------------------------------------------------------------------------
// #290 fase A: los gestos de panel que el TUI tenía y la ventana no.
//
// Ninguno inventa modelo. Lo que estos tests comprueban es que la ventana usa
// el COMPARTIDO —`SortSpec::after_click`, `PaneState::toggle_hidden`,
// `History::entries`, el rol `Target` de la ADR 0058— y que lo que no puede
// hacer lo DICE, en vez de quedarse muda.
// ---------------------------------------------------------------------------

/// La columna por la que se ordena, y en qué sentido, leídas de las cabeceras
/// que cruzan el puente: es lo único que el renderer sabe del orden.
pub(super) fn orden_de(b: &norte_ui_host::dto::BrowserSlotView) -> (String, String) {
    let marcadas: Vec<&norte_ui_host::dto::ColumnHeader> =
        b.columns.iter().filter(|c| c.sort.is_some()).collect();
    assert_eq!(
        marcadas.len(),
        1,
        "una sola columna lleva la marca de orden: {:?}",
        b.columns
    );
    (
        marcadas[0].id.clone(),
        marcadas[0].sort.clone().expect("marcada"),
    )
}

/// Una foto de ahora mismo.
pub(super) async fn foto(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    siguiente_foto(sub).await
}

/// Espera a que una FOTO cumpla `cond`, sin relojes de intervalo.
///
/// Hay que PREGUNTAR: ni `total_rows` ni `path_display` viajan en un parche
/// —solo en una foto, y la foto la pide `Resync`—, así que quedarse
/// escuchando no basta. Lo que este ayudante no hace es dormir 25 ms entre
/// intento e intento: se BLOQUEA en el siguiente mensaje del host, o sea
/// que va al ritmo del host y no al del planificador. El plazo total está
/// para que una condición que no se cumple se lea como un fallo con su
/// frase, y no como un test colgado.
pub(super) async fn esperar_foto(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    que: &str,
    cond: impl Fn(&norte_ui_host::ViewSnapshot) -> bool,
) -> norte_ui_host::ViewSnapshot {
    let espera = async {
        loop {
            let f = foto(h, sub).await;
            if cond(&f) {
                return f;
            }
            // CUALQUIER mensaje del host, no la siguiente FOTO: las fotos las
            // pide el renderer, y un drenaje viaja entero en PARCHES. Esperar
            // otra foto era esperar a que el host mandara una por su cuenta,
            // que es justo lo que no hace: el bucle se colgaba en vez de
            // agotar el plazo, y un test colgado no dice qué falló.
            let _ = sub.recv().await.expect("el host sigue vivo");
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), espera)
        .await
        .unwrap_or_else(|_| panic!("plazo agotado esperando a que {que}"))
}

/// `pane.sort-size` ordena por tamaño y repetirlo INVIERTE.
///
/// La misma semántica que un click en la cabecera porque es el MISMO camino:
/// quien decide es `SortSpec::after_click`, no una tabla por superficie.
#[tokio::test]
async fn el_comando_de_orden_es_el_click_en_la_cabecera() {
    let (h, snap) = host_arbol(arbol()).await;
    let (columna_inicial, _) = orden_de(listado(&snap));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let despues = foto(&h, &mut sub).await;
    let (columna, sentido) = orden_de(listado(&despues));
    assert_ne!(columna, columna_inicial, "ordena por OTRA columna");
    assert_eq!(sentido, "asc", "una columna nueva empieza ascendente");

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let otra_vez = foto(&h, &mut sub).await;
    let (misma, sentido) = orden_de(listado(&otra_vez));
    assert_eq!(misma, columna, "sigue siendo la misma columna");
    assert_eq!(sentido, "desc", "la columna activa INVIERTE");
}

/// `pane.sort-menu` no estrena pantalla: abre el selector de COLUMNAS, donde
/// están la columna, la dirección y `dirs_first`. Es la decisión del TUI, y
/// dos pantallas para lo mismo serían otra que mantener y otra que aprender.
#[tokio::test]
async fn el_menu_de_orden_es_el_selector_de_columnas() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.sort-menu").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        despues.columns.is_some(),
        "el menú de orden es el selector de columnas"
    );
}

/// `pane.toggle-hidden` aparta los dotfiles del panel y lo ANUNCIA.
///
/// Un aviso de la barra caduca a los `[ui] notice_seconds` segundos (spec
/// 2026-09-10): sale de `status.message` y `notices_unread` cuenta uno más.
/// Se espera la FOTO, sin dormir: el tic de un segundo es del host.
#[tokio::test]
async fn un_aviso_caduca_y_deja_una_insignia() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b".oculto".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.ui_chrome.notice_seconds = Some(1);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
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
    assert_eq!(snap.status.notices_unread, 0);
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        despues.status.message.is_some(),
        "ocultar se dice en la barra"
    );
    foto_hasta(&h, &mut sub, "el aviso caducó a la insignia", |f| {
        (f.status.message.is_none() && f.status.notices_unread == 1).then_some(())
    })
    .await;
}

/// Presentación-solo (#107): el provider no vuelve a listar, así que el
/// backend no ve una petición más.
#[tokio::test]
async fn ocultar_es_presentacion_y_se_dice() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let antes = listado(&snap).rows.len();
    assert!(
        listado(&snap)
            .rows
            .iter()
            .any(|r| r.display_name == ".oculto"),
        "sin `[ui] show_hidden` en la config se enseña todo"
    );
    let listados = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        listado(&despues)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "los ocultos se apartaron"
    );
    assert_eq!(
        backend.listados(),
        listados,
        "y se apartaron SIN volver a pedir el directorio"
    );
    assert!(
        despues.status.message.is_some(),
        "un listado que encoge sin decir por qué se lee como un fallo"
    );

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let otra_vez = foto(&h, &mut sub).await;
    assert_eq!(
        listado(&otra_vez).rows.len(),
        antes,
        "y vuelve a devolverlos, sin re-listar"
    );
}

/// `pane.names-encoding` cambia cómo se PINTA un nombre que no es UTF-8, y no
/// los bytes: la fila sigue marcada como hostil y su clave sigue valiendo.
#[tokio::test]
async fn ciclar_el_encoding_repinta_sin_tocar_los_bytes() {
    let (h, snap) = host_arbol(arbol()).await;
    let hostil = listado(&snap)
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("el árbol trae un nombre que no es UTF-8");
    let (clave, pintado) = (hostil.key, hostil.display_name.clone());
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.names-encoding").await;
    let despues = foto(&h, &mut sub).await;
    let misma = listado(&despues)
        .rows
        .iter()
        .find(|r| r.key == clave)
        .expect("la clave sigue valiendo: los bytes no cambiaron");
    assert_ne!(misma.display_name, pintado, "se pinta de otra forma");
    assert!(
        despues.status.message.is_some(),
        "y se dice con qué se está reinterpretando"
    );
}

/// Y la CABECERA se repinta con las filas, sin pedir una foto.
///
/// `pane.names-encoding` transcribe los nombres, y la ruta del propio
/// directorio es un nombre más: si sus bytes no son UTF-8, la cabecera tiene
/// que reinterpretarse igual que las filas. Contestaba con un parche de filas
/// y un parche de estado, y la cabecera solo viaja en la foto entera — así que
/// las filas se retranscribían y el título se quedaba con la lectura vieja,
/// que es exactamente el medio arreglo que #57 y #293 dicen que no puede
/// pasar: el mojibake se queda arriba y el lector no sabe si el comando hizo
/// algo.
///
/// Se comprueba SIN `Resync`, que es lo único que tiene el renderer, y sin
/// pasar por la paleta —abrirla y cerrarla manda fotos que repararían la
/// cabecera por accidente—: la tecla del preset, como un humano.
#[tokio::test]
async fn ciclar_el_encoding_repinta_tambien_la_cabecera() {
    let mut falso = Falso::default();
    // Un directorio cuyo PROPIO nombre no es UTF-8.
    falso.pon("mem:///caf%FF", vec![(b"a.txt".to_vec(), false)]);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        initial_dir: norte_proto::VPath::parse("mem:///caf%FF").expect("wire"),
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
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let antes = listado(&snap).path_display.clone();
    assert!(
        antes.contains('\u{fffd}'),
        "de partida, los bytes no se pueden pintar: {antes}"
    );
    let mut sub = h.subscribe();

    // `alt+e` es `pane.names-encoding` en el preset `orthodox`. A mano y no
    // por `tecla_mod`, que fija `alt: false`.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "e".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    asentar().await;

    let mut cabecera = None;
    tokio::time::pause();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Update::Message(m) = sub.recv().await.expect("host vivo") {
                match m.payload {
                    UiUpdate::Patch(p) => {
                        for c in &p.changes {
                            if let norte_ui_host::dto::ViewChange::BrowserHeader {
                                path_display,
                                ..
                            } = c
                            {
                                cabecera = Some(path_display.clone());
                            }
                        }
                    }
                    UiUpdate::Snapshot(s) => {
                        cabecera = Some(listado(&s).path_display.clone());
                    }
                    UiUpdate::Notice(_) => {}
                }
                if cabecera.is_some() {
                    return;
                }
            }
        }
    })
    .await;
    tokio::time::resume();

    let despues = cabecera.expect(
        "ciclar el encoding no repinta la cabecera: las filas se \
         retranscriben y el título se queda con la lectura vieja",
    );
    assert_ne!(
        despues, antes,
        "la ruta se reinterpreta igual que las filas"
    );
}

/// Una miga pulsada (puente 65) lleva el hueco al ancestro con esa
/// profundidad — el hueco de la miga, no el activo— y la del directorio
/// actual no mueve nada.
#[tokio::test]
async fn una_miga_lleva_al_ancestro_de_su_hueco() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    assert_eq!(
        listado_de(&snap, 2).path_segments,
        vec!["⟨mem⟩", "casa", "docs"],
        "la raíz y un tramo por directorio"
    );
    let mut sub = h.subscribe();
    let generation = listado_de(&snap, 2).generation;

    // La miga del directorio actual: aplicada, y nada se mueve.
    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 2,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let igual = siguiente_foto(&mut sub).await;
    assert!(listado_de(&igual, 2).path_display.ends_with("/casa/docs"));

    // Una miga de OTRA generación —el hueco navegó entre el pintado y el
    // clic— es rancia: no se reinterpreta sobre la ruta nueva.
    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 1,
        generation: generation + 1,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let rancia = siguiente_foto(&mut sub).await;
    assert!(listado_de(&rancia, 2).path_display.ends_with("/casa/docs"));

    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 1,
        generation,
    })
    .await
    .expect("host vivo");
    let f = esperar_foto(&h, &mut sub, "el hueco 2 sube a /casa", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
    })
    .await;
    assert_eq!(listado_de(&f, 2).path_segments, vec!["⟨mem⟩", "casa"]);
    assert!(
        listado_de(&f, 1).path_display.ends_with("/casa"),
        "el hueco activo no se movió"
    );
}

/// Activar una fila del panel que NO tiene el foco lo enfoca y entra: es el
/// doble clic del ratón, y el renderer manda el foco y la activación como
/// dos mensajes. Si el primero no se aplicara, el segundo no puede quedarse
/// mudo — antes `fila_de` lo rehusaba por «otro hueco» y el doble clic en el
/// panel de al lado no hacía nada.
#[tokio::test]
async fn activar_una_fila_del_otro_panel_lo_enfoca_y_entra() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    // El 1 está en `/casa` y tiene el directorio `docs`; el foco se lleva al
    // 2 para que el 1 sea «el otro panel».
    let a = listado_de(&snap, 1);
    let (generation, key) = a
        .rows
        .iter()
        .find(|r| r.display_name.contains("docs"))
        .map(|r| (a.generation, r.key))
        .expect("la fila del directorio");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    let _ = sub.recv().await.expect("el host sigue vivo");

    // Y ahora la activación del OTRO panel, sin `focus_slot` delante.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    let f = esperar_foto(&h, &mut sub, "el hueco 1 entra en /casa/docs", |f| {
        listado_de(f, 1).path_display.ends_with("/casa/docs")
    })
    .await;
    assert!(
        listado_de(&f, 2).path_display.ends_with("/casa/docs"),
        "y el otro panel se queda donde estaba"
    );
}

/// `pane.refresh` vuelve a pedir TODOS los listados que se ven, no solo el
/// enfocado: lo que cambia un directorio por debajo es un cambio en el DISCO,
/// y un cambio en el disco no respeta el foco.
#[tokio::test]
async fn refrescar_relista_los_dos_paneles() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let antes = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.refresh").await;
    esperar_foto(&h, &mut sub, "los dos paneles se relisten", |_| {
        backend.listados() >= antes + 2
    })
    .await;
}

/// `pane.mirror` manda la ubicación del panel ACTIVO al panel destino, y el
/// destino sale del rol compartido — nunca de «el de al lado».
#[tokio::test]
async fn el_espejo_manda_la_ubicacion_al_destino() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    // El escenario deja el 2 en `/casa/docs` y el foco en el 1, que sigue en
    // `/casa`: espejar tiene que llevarse el 2 de vuelta.
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let f = esperar_foto(&h, &mut sub, "el destino siga al activo", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
            && listado_de(f, 1).path_display.ends_with("/casa")
    })
    .await;
    assert_eq!(f.focus, Some(1), "el espejo no mueve el foco");
}

/// `pane.mirror-target` manda la CARPETA BAJO EL CURSOR, no la ubicación:
/// `Ctrl+←`/`Ctrl+→` de Krusader, y la misma respuesta que da la TUI porque
/// quien la decide es `PaneState::target_dir` (ADR 0077).
#[tokio::test]
async fn el_espejo_del_objetivo_manda_la_carpeta_del_cursor() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    // Los dos en `/casa` para empezar: así lo que se mide después es el
    // OBJETIVO del cursor y no el arrastre del escenario.
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    esperar_foto(&h, &mut sub, "los dos en /casa", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
    })
    .await;

    // El cursor a la fila 0, que aquí es el directorio `docs`: estos ajustes
    // traen la fila `..` APAGADA (`ui_parent_entry = false`), y el escenario
    // deja el cursor donde lo dejó su propia navegación.
    ejecutar_por_paleta(&h, &mut sub, "cursor.top").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror-target").await;
    let f = esperar_foto(&h, &mut sub, "el destino entre en la carpeta", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    assert!(
        listado_de(&f, 1).path_display.ends_with("/casa"),
        "el panel del foco no se mueve: {}",
        listado_de(&f, 1).path_display
    );
    assert_eq!(f.focus, Some(1), "ni el foco");
}

/// `pane.pull` es el mismo gesto al revés: la ubicación sale del destino y
/// viaja el panel con el foco.
#[tokio::test]
async fn traer_mueve_el_panel_del_foco() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pull").await;
    let f = esperar_foto(
        &h,
        &mut sub,
        "el panel del foco se traiga la otra ubicación",
        |f| listado_de(f, 1).path_display.ends_with("/casa/docs"),
    )
    .await;
    assert!(
        listado_de(&f, 2).path_display.ends_with("/casa/docs"),
        "el otro se queda donde estaba"
    );
}

/// `pane.swap` cambia los dos listados de sitio SIN tocar disco: nadie
/// vuelve a pedir un directorio, y el foco se queda donde estaba.
#[tokio::test]
async fn intercambiar_no_pide_nada_al_backend() {
    let backend = arbol();
    let (h, snap) = crate::dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    assert!(listado_de(&snap, 1).path_display.ends_with("/casa"));
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let listados = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.swap").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        listado_de(&despues, 1).path_display.ends_with("/casa/docs"),
        "el panel del foco enseña lo otro: {}",
        listado_de(&despues, 1).path_display
    );
    assert!(listado_de(&despues, 2).path_display.ends_with("/casa"));
    assert_eq!(despues.focus, Some(1), "el foco no se mueve con el gesto");
    assert_eq!(
        backend.listados(),
        listados,
        "los dos listados ya existían: intercambiarlos no toca disco"
    );
}

/// La ventana de PINTADO no viaja con el intercambio.
///
/// `primera_visible`/`visibles` los pone el renderer por slot con
/// `set_visible_range`, y su `scrollTop` es suyo: un swap no lo mueve ni
/// dispara un evento de scroll. Si la ventana viajara con el hueco, cada
/// panel pintaría filas de una banda que el lector no tiene delante y los DOS
/// se verían VACÍOS. Con listados cortos y los dos arriba del todo no se nota
/// nada, que es por lo que hace falta este test y no el de al lado.
#[tokio::test]
async fn el_intercambio_no_mueve_la_ventana_de_pintado() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..300).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 200,
        count: 30,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 30,
    })
    .await
    .expect("host vivo");
    let antes = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&antes, 1).first_visible, 200);
    assert_eq!(listado_de(&antes, 2).first_visible, 0);

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 1).first_visible,
        200,
        "el slot que estaba por la fila 200 sigue pintando por la 200: el \
         scroll del renderer no se ha movido"
    );
    assert_eq!(
        listado_de(&despues, 2).first_visible,
        0,
        "y el que estaba arriba sigue arriba"
    );
}

/// Un intercambio durante una NAVEGACIÓN la repide, apuntando a donde iba.
///
/// La respuesta en vuelo viaja etiquetada con su slot: tras el cambio llega
/// al hueco equivocado y se descarta por testigo. Sin repedirla, el panel se
/// queda `Loading` para siempre.
#[tokio::test]
async fn el_intercambio_repide_la_navegacion_en_vuelo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"informe.pdf".to_vec(), false)]);
    // Lo bastante lento para que el swap caiga DENTRO de la navegación.
    f.retraso_ms = 400;
    let (h, snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    let b1 = listado_de(&snap, 1);
    let docs = b1
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b1.generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");

    let f = esperar_foto(
        &h,
        &mut sub,
        "la navegación interrumpida llegue a su destino",
        |f| {
            listado_de(f, 2).path_display.ends_with("/casa/docs")
                && listado_de(f, 1).path_display.ends_with("/casa")
        },
    )
    .await;
    let (izq, der) = (listado_de(&f, 1), listado_de(&f, 2));
    assert!(
        !matches!(izq.state, norte_ui_host::dto::SlotState::Loading { .. })
            && !matches!(der.state, norte_ui_host::dto::SlotState::Loading { .. }),
        "ningún panel se queda cargando: {:?} {:?}",
        izq.state,
        der.state
    );
}

/// Un intercambio durante el DRENAJE también lo repide.
///
/// `en_vuelo` muere con la primera página y `drenando` sigue vivo: en un
/// directorio de más de cien entradas —o sea casi cualquiera— hay una ventana
/// en la que solo vive el drenaje. Mirar solo `en_vuelo` dejaba el listado
/// congelado en cien entradas, en `Ready` y sin decir nada, y marcar todo
/// actuaba sobre ese trozo.
#[tokio::test]
async fn el_intercambio_repide_el_drenaje() {
    let puerta = Arc::new(backend_falso::Puerta::default());
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..250).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    f.puerta_drenaje = Some(Arc::clone(&puerta));
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // La primera página ya está en pantalla y el resto sigue detenido en la
    // puerta: ESTE es el estado que el bug necesitaba.
    esperar_foto(&h, &mut sub, "aterrice la primera página", |f| {
        listado_de(f, 1).total_rows == Some(100)
    })
    .await;

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host vivo");
    puerta.abrir();

    esperar_foto(&h, &mut sub, "el drenaje se repida y llegue entero", |f| {
        listado_de(f, 1).total_rows == Some(250) && listado_de(f, 2).total_rows == Some(250)
    })
    .await;
}

/// `pane.toggle-hidden` PODA las marcas de lo que aparta, y lo dice.
///
/// El contrato de `PaneState::pruned_marks` es que una selección que alimenta
/// una op en masa jamás encoge en silencio: callarlo copiaría menos ficheros
/// de los que el lector marcó, creyendo él que van todos.
#[tokio::test]
async fn apartar_los_ocultos_dice_las_marcas_que_se_lleva() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".env".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let oculta = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == ".env")
        .expect("los ocultos se ven de fábrica");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: oculta.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 1).marks,
        0,
        "la marca se fue con la fila"
    );
    let dicho = despues.status.message.clone().unwrap_or_default();
    assert!(
        dicho.contains('1') && dicho.contains("caída") || dicho.contains("caídas"),
        "y se DICE cuántas se cayeron: {dicho:?}"
    );
}

/// Un espejo REDUNDANTE no toca el otro panel.
///
/// Los dos ya enseñan lo mismo, así que un cd sería re-listar: `set_listing`
/// le borra las marcas —una navegación, a diferencia de un refresco, no las
/// restaura— y le desliza el listado bajo el cursor, a cambio de nada. El TUI
/// lo rehúsa por lo mismo (`gestures::mirror_plan`).
#[tokio::test]
async fn un_espejo_redundante_no_borra_las_marcas_del_otro() {
    let backend = arbol();
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let b2 = listado_de(&snap, 2);
    // `fila_de` RECHAZA un slot que no sea el activo, así que el foco va
    // primero al 2 y vuelve al 1. Lo que se prueba aquí es el espejo.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 2,
        key: b2.rows[0].key,
        generation: b2.generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    let mut sub = h.subscribe();
    let antes = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&antes, 2).marks, 1);
    let listados = backend.listados();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let despues = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&despues, 2).marks,
        1,
        "los dos ya estaban en el mismo sitio: la marca sigue puesta"
    );
    assert_eq!(
        backend.listados(),
        listados,
        "y no se ha vuelto a pedir nada"
    );
}

/// Un gesto de panel sin otro panel se DICE, y con la frase que distingue
/// «no hay otro» de «hay varios, designa uno».
#[tokio::test]
async fn un_gesto_sin_otro_panel_se_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    for cmd in ["pane.mirror", "pane.pull", "pane.swap"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        match ack {
            ActionAck::Unavailable { reason_key } => {
                assert_eq!(reason_key, "host-no-other-slot", "{cmd}");
            }
            otro => panic!("{cmd} con un solo panel: {otro:?}"),
        }
    }
}

/// `pane.history` enseña el rastro del panel, y elegir una fila navega.
///
/// Las filas son las de `History::entries` —el MRU compartido—: qué recuerda
/// un panel y en qué orden no puede depender de quién lo pinta.
#[tokio::test]
async fn el_historial_es_el_rastro_compartido() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está");
    let (key, generation) = (docs.key, listado(&snap).generation);
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    let mut despues = siguiente_foto(&mut sub).await;
    while !listado(&despues).path_display.ends_with("/casa/docs") {
        despues = siguiente_foto(&mut sub).await;
    }

    ejecutar_por_paleta(&h, &mut sub, "pane.history").await;
    let abierto = foto(&h, &mut sub).await;
    let picker = abierto.picker.expect("el historial está abierto");
    assert!(
        picker.rows.iter().any(|r| r.label.ends_with("/casa")),
        "el sitio del que se salió está en el rastro: {:?}",
        picker.rows
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    esperar_foto(&h, &mut sub, "elegir en el historial navegue", |f| {
        f.picker.is_none() && listado(f).path_display.ends_with("/casa")
    })
    .await;
}

/// `pane.hotlist` enseña los favoritos de la configuración, y uno cuya ruta
/// no parsea SE QUEDA con su aviso: la hotlist es data del usuario, y un
/// favorito que desaparece en silencio es un fallo que nadie puede ver.
#[tokio::test]
async fn un_favorito_invalido_se_queda_y_se_dice() {
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: Ok(dir()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
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
        settings: ajustes,
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

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    let abierto = foto(&h, &mut sub).await;
    let picker = abierto.picker.expect("los favoritos están abiertos");
    assert_eq!(picker.rows.len(), 2, "el inválido NO se cae de la lista");
    assert_eq!(picker.rows[1].label, "roto");
    assert!(
        !picker.rows[1].detail.is_empty(),
        "y su detalle dice que la ruta no vale"
    );

    // El cursor sobre el inválido: elegirlo no puede navegar a ninguna parte,
    // y quedarse mudo no se distingue de una tecla rota.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let ack = h.dispatch(tecla("Enter")).await.expect("host vivo");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "hotlist-invalid"),
        otro => panic!("un favorito inválido: {otro:?}"),
    }
}

/// `pane.select-drive-left` nombra un LADO de la pantalla, no el foco: con el
/// foco en el panel derecho, el selector sigue siendo el del izquierdo.
#[tokio::test]
async fn los_volumenes_por_lado_no_siguen_al_foco() {
    // Los dos sentidos, porque una implementación que leyera el foco pasaría
    // cualquiera de los dos por separado: lo que hay que fijar es que el
    // panel que se MUEVE es el del lado nombrado y el otro no se toca.
    for (comando, montado, quieto) in [
        ("pane.select-drive-left", 1_u32, 2_u32),
        ("pane.select-drive-right", 2, 1),
    ] {
        let mut f = Falso::default();
        f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
        f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
        f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
        let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
        // El foco se pone en el panel CONTRARIO al que el comando nombra.
        h.dispatch(UiAction::FocusSlot { slot_id: quieto })
            .await
            .expect("host vivo");
        let mut sub = h.subscribe();

        ejecutar_por_paleta(&h, &mut sub, comando).await;
        let con_filas = foto_hasta(&h, &mut sub, "la tabla de montaje", |f| {
            f.picker
                .as_ref()
                .is_some_and(|p| !p.rows.is_empty())
                .then(|| f.picker.clone())
        })
        .await;
        assert!(
            con_filas.is_some(),
            "{comando}: la tabla de montaje llega al selector"
        );

        h.dispatch(tecla("Enter")).await.expect("host vivo");
        let montada = foto_hasta(
            &h,
            &mut sub,
            &format!("{comando}: el volumen montado en el hueco del lado {montado}"),
            |f| {
                (f.picker.is_none() && listado_de(f, montado).path_display.contains("otro"))
                    .then(|| f.clone())
            },
        )
        .await;
        assert!(
            listado_de(&montada, quieto).path_display.ends_with("/casa"),
            "{comando}: el panel del foco NO se ha movido: {}",
            listado_de(&montada, quieto).path_display
        );
    }
}

/// Las propiedades de esta ventana son el hueco `metadata`, que ya enseña
/// nombre, clase, tamaño y fecha de lo señalado. La ventana lo hace de otra
/// forma, igual que ordena pulsando la cabecera.
#[tokio::test]
async fn las_propiedades_abren_la_hoja_de_atributos() {
    let (h, snap) = host_arbol(arbol()).await;
    assert!(
        !snap
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "de fábrica `simple` no trae hoja de atributos"
    );
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.properties").await;
    let despues = foto(&h, &mut sub).await;
    assert!(
        despues
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "las propiedades abren la hoja"
    );
}

/// El orden y la ocultación VUELVEN de la sesión.
///
/// Se escribían y no los leía nadie: la ventana se acordaba de dónde estabas
/// y olvidaba cómo lo estabas mirando, así que ordenar por tamaño o apartar
/// los dotfiles duraba hasta cerrar.
#[tokio::test]
async fn la_sesion_devuelve_el_orden_y_los_ocultos() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: dir(),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec {
                column: norte_frontend::SortColumn::Size,
                dir: norte_frontend::SortDir::Desc,
                dirs_first: true,
            },
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    *falso.sesion.lock().expect("sesión") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 7,
            body: serde_json::to_value(&body).expect("json"),
        },
        true,
    );

    let (_h, snap) = host_arbol(Arc::new(falso)).await;
    let b = listado(&snap);
    assert!(
        b.rows.iter().all(|r| r.display_name != ".oculto"),
        "la sesión decía que estaban apartados"
    );
    let (_, sentido) = orden_de(b);
    assert_eq!(sentido, "desc", "y que se ordenaba al revés");
}

/// `[ui] show_hidden = false` siembra el estado inicial de los paneles, igual
/// que en el TUI (#107). Sin esto, la clave estaba muerta en esta ventana.
#[tokio::test]
async fn la_config_siembra_la_ocultacion() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".oculto".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let mut ajustes = ajustes_de_prueba();
    ajustes.common.ui_show_hidden = Some(false);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes,
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
    assert!(
        listado(&snap)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "la configuración decía que no se enseñan"
    );
}
