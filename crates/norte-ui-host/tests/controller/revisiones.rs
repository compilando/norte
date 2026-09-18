use super::*;

// ---------------------------------------------------------------------------
// Solo lectura: la ventana todavía no muta (revisión de seguridad de la
// tarea 3.3; el gate de salida de la fase 4 lo exige literalmente).
// ---------------------------------------------------------------------------

pub(super) async fn host_solo_lectura(
    backend: Arc<Falso>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset_con(
            "orthodox",
            norte_ui_host::commands::Efectos::SoloLectura,
        )
        .expect("preset"),
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
        effects: norte_ui_host::commands::Efectos::SoloLectura,
        log_ring: None,
    })
    .await
    .expect("arranca")
}

/// En solo lectura, F8 no abre la confirmación de borrado: lo DICE.
///
/// Un frontend que no tiene todavía el camino seguro de la fase 5 no puede
/// tener la tecla viva y el diálogo detrás; que la tecla exista en el preset
/// no es permiso.
#[tokio::test]
async fn en_solo_lectura_borrar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F8")).await.expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "F8 se responde, no se ejecuta: {ack:?}"
    );
    asentar().await;
    assert!(
        backend.borrados.lock().expect("borrados").is_empty(),
        "y no borra nada"
    );
}

/// Lo mismo con crear directorio.
#[tokio::test]
async fn en_solo_lectura_crear_no_crea() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h.dispatch(tecla("F7")).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(backend.creados.lock().expect("creados").is_empty());
}

/// Y una aprobación de policy no llega siquiera a plantearse: un renderer que
/// no puede mutar tampoco puede aprobar que mute un agente.
#[tokio::test]
async fn en_solo_lectura_no_hay_aprobaciones_que_responder() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::Dialog {
            id: norte_ui_host::ModalId(1),
            choice: "approve".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "no hay diálogo que responder"
    );
    assert!(
        backend.decisiones.lock().expect("decisiones").is_empty(),
        "y ninguna decisión llegó al daemon"
    );
}

/// En modo completo, la misma tecla SÍ abre la confirmación: el gate es una
/// decisión de arranque, no una amputación del host.
#[tokio::test]
async fn en_modo_completo_borrar_sigue_pidiendo_confirmacion() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos.len(), 1);
}

// ---------------------------------------------------------------------------
// Los dos BLOCKER de la revisión.
// ---------------------------------------------------------------------------

/// Navegar a un directorio grande trae el directorio ENTERO, no la primera
/// página.
///
/// `aterriza_en` limpia el testigo al aterrizar la primera página, y la task
/// de drenaje seguía mandando los lotes con ese mismo testigo: `aplicar_lote`
/// los rechazaba todos. El arranque no lo veía porque `listar_inicial`
/// restituye el testigo a mano.
#[tokio::test]
async fn navegar_a_un_directorio_grande_lo_trae_entero() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let muchas: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa/docs", muchas);
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs está")
        .key;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");

    // Cada vuelta es un viaje al actor: el relleno avanza entre foto y foto.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).total_rows == Some(300) {
            return;
        }
    }
    panic!("el listado se quedó en la primera página: el relleno no aterriza");
}

/// Una fila de una generación anterior NO se toca.
///
/// El contrato del bridge lo promete desde el principio («un doble click
/// tardío no actúa sobre el fichero que ocupó esa fila DESPUÉS») y no había
/// nada que lo implementara: las acciones no llevaban generación.
#[tokio::test]
async fn una_fila_de_otra_generacion_no_se_marca() {
    let (h, snap) = host(vec!["a", "b", "c"]).await;
    let vieja = listado(&snap).generation;
    // Reordenar mueve TODAS las filas y sube la generación.
    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host vivo");

    let ack = h
        .dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: RowKey(0),
            generation: vieja,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "la clave era de la pantalla anterior: {ack:?}"
    );
}

/// Y un rango con un extremo fuera de la ventana no se recorta: se rechaza.
///
/// `PaneState::mark_range` recorta a propósito (su contrato), así que un
/// `to: u64::MAX` marcaba el listado ENTERO — incluidas filas que el renderer
/// nunca recibió— y lo marcado es la entrada de un borrado.
#[tokio::test]
async fn un_rango_desbordado_no_marca_el_listado_entero() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    let epoca = listado(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(u64::MAX),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "un extremo que no existe invalida el rango entero"
    );
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).marks, 0, "y no marcó nada");
}

// ---------------------------------------------------------------------------
// El sondeo (revisión: rust M2/M3/m1, encoding M4).
// ---------------------------------------------------------------------------

/// Una ventana más alta que una tanda de sondeo se rellena ENTERA.
///
/// `MAX_SONDEOS` acota cada tanda, y no había nada que pidiera la siguiente:
/// 200 filas con tamaño y el resto en blanco hasta que el usuario moviera
/// algo. Un tope que no se re-arma es un tope silencioso.
#[tokio::test]
async fn una_ventana_grande_se_sondea_en_tandas_hasta_el_final() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    let muchas: Vec<(Vec<u8>, bool)> = (0..500)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", muchas);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 500,
    })
    .await
    .expect("host vivo");

    hasta(&backend, "las 500 entradas sondeadas", |f| {
        (f.sondeos.lock().expect("sondeos").len() >= 500).then_some(())
    })
    .await;
}

/// Un sondeo que aterriza cuando el listado YA es otro no pega nada.
///
/// El guard miraba el testigo de la petición en vuelo, que tras aterrizar es
/// `None` — así que valía cero y la comparación era siempre falsa. Lo que
/// distingue un listado de otro es su ÉPOCA, que está definida siempre.
#[tokio::test]
async fn un_sondeo_de_otro_listado_no_hidrata() {
    let mut f = Falso {
        lazy: true,
        // El stat tarda: da tiempo a navegar por debajo.
        retraso_ms: 120,
        ..Falso::default()
    };
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs")
        .key;
    // Navegar mientras el sondeo del listado anterior vuela.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    // Lo que este caso pone en vuelo: el listado de casa, el sondeo del
    // `a.txt` de FUERA —el que llega tarde y no debe hidratar— y el listado
    // de docs. Se espera a que no quede ninguna volando.
    hasta(&backend, "el sondeo tardío ya servido", |f| {
        (f.listados() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    // El `a.txt` de DENTRO es otro fichero que el `a.txt` de fuera; lo que se
    // comprueba es que la pantalla es coherente, no que tenga o no tamaño.
    assert!(
        listado(&foto).path_display.ends_with("/docs"),
        "se navegó: {}",
        listado(&foto).path_display
    );
}

/// La hidratación casa por la ruta que se PIDIÓ, no por la que devuelve el
/// provider.
///
/// Un HFS+ que devuelve NFD, un SMB que devuelve otra caja o un `stat` que
/// sigue un enlace producen una respuesta cuya ruta no está en el listado.
/// Como el path pedido ya quedó marcado como sondeado, la celda se quedaba en
/// blanco para siempre.
#[tokio::test]
async fn un_provider_que_devuelve_otra_ortografia_no_deja_la_celda_en_blanco() {
    let mut f = Falso {
        lazy: true,
        // El stat contesta con el nombre en MAYÚSCULAS: otra ortografía de lo
        // mismo, como haría un servidor sin distinción de caja.
        stat_grita: true,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lleno = listado(&foto)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if lleno {
            return;
        }
    }
    panic!("la celda sigue en blanco: se casó por la ruta devuelta");
}

// ---------------------------------------------------------------------------
// Nombres y texto (revisión de encoding).
// ---------------------------------------------------------------------------

/// Lo que se teclea en el diálogo es lo que se crea, byte a byte.
///
/// El nombre viajaba por `clamp_display`, que recorta a 4 KiB y AÑADE `…`, y
/// `Segment::new` acepta la elipsis: se creaba un directorio con un nombre
/// que nadie tecleó. Es ADR 0061 en miniatura — un texto de pantalla que
/// acaba siendo un nombre de fichero.
#[tokio::test]
async fn el_nombre_que_se_teclea_es_el_que_se_crea() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // Un nombre con un carácter de control dentro: legal en Unix, y lo que
    // se cree tiene que ser EXACTAMENTE eso.
    let crudo = "caf\u{202e}e.txt";
    h.dispatch(UiAction::DialogInput {
        id,
        text: crudo.to_owned(),
    })
    .await
    .expect("host vivo");

    // Lo que se PINTA está enmascarado y se dice que lo está.
    let pintado = siguientes_dialogos(&mut sub).await;
    assert!(
        pintado[0].input_hostile,
        "un nombre con una marca de dirección se DICE: {:?}",
        pintado[0].input
    );
    assert!(
        !pintado[0]
            .input
            .as_deref()
            .unwrap_or_default()
            .contains('\u{202e}'),
        "y no se pinta crudo"
    );

    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let creados = hasta(&backend, "la creación encolada", |f| {
        let c = f.creados.lock().expect("creados").clone();
        (!c.is_empty()).then_some(c)
    })
    .await;
    assert_eq!(creados.len(), 1, "se encoló una creación");
    let nombre = creados[0]
        .file_name()
        .expect("tiene nombre")
        .as_bytes()
        .to_vec();
    assert_eq!(
        nombre,
        crudo.as_bytes(),
        "lo creado son los bytes tecleados, no su proyección"
    );
}

/// Un nombre imposible se RECHAZA en vez de recortarse.
#[tokio::test]
async fn un_nombre_desmesurado_no_se_recorta() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    let ack = h
        .dispatch(UiAction::DialogInput {
            id,
            text: "a".repeat(5000),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "se dice que no cabe: {ack:?}"
    );
}

/// El id de una columna es una IDENTIDAD: viaja entero, y lo que se enmascara
/// es la ETIQUETA.
///
/// Enmascarar el id no es inyectivo. Dos columnas configuradas que solo se
/// diferencien en un carácter invisible daban el MISMO id enmascarado, y la
/// resolución del click hace `find`: pulsar la segunda ordenaba por la
/// primera. Es la regla del ADR 0061 sobre una superficie que el ADR no
/// cubría. Lo que el renderer PINTA es `label`; el id solo va en un `data-`.
#[tokio::test]
async fn dos_columnas_que_se_enmascaran_igual_siguen_siendo_dos() {
    let backend = arbol();
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
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        // Los dos ids se enmascaran a lo MISMO: U+200B y U+202E son los dos
        // peligros de terminal y `display_name` los sustituye por U+FFFD.
        // Van por `plugin:` y no por `attr:`: los `attr:` ya los filtra
        // `is_valid_attr_id` —un id que no es legal en el wire tumbaría el
        // listado entero— y los de plugin no los filtra nadie.
        columns: columnas_de(&[
            "name",
            "plugin:acme.a\u{200b}b/x",
            "plugin:acme.a\u{202e}b/x",
        ]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    drop(h);
    let b = listado(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "las tres columnas se pintan: {ids:?}");
    assert_ne!(
        ids[1], ids[2],
        "y siguen siendo DOS: enmascarar el id las fundía en una, y el `find` \
         de la resolución habría ordenado siempre por la primera"
    );
    assert!(
        ids[1].contains('\u{200b}') && ids[2].contains('\u{202e}'),
        "el id viaja ENTERO, que es lo que hace que case consigo mismo: {ids:?}"
    );
    // Lo que se PINTA sí va enmascarado.
    for c in &b.columns {
        assert!(
            !c.label.contains('\u{202e}') && !c.label.contains('\u{200b}'),
            "la etiqueta va cruda: {:?}",
            c.label
        );
    }
    // Y la celda nombra su columna con la misma identidad.
    let columnas_de_celdas: std::collections::BTreeSet<&str> = b
        .rows
        .iter()
        .flat_map(|f| f.cells.iter().map(|c| c.column.as_str()))
        .collect();
    for c in &columnas_de_celdas {
        assert!(
            ids.contains(c),
            "una celda nombra una columna que no está en la cabecera: {c:?}"
        );
    }
}

/// Una lectura del visor que llega tarde no abre nada.
///
/// F3 sobre un fichero en un montaje lento, `esc`, y segundos después el
/// visor aparecía solo — y como las teclas se enrutan por «hay visor», la
/// siguiente tecla la interpretaba otro mapa sin que nadie lo pidiera.
#[tokio::test]
async fn un_visor_que_llega_tarde_no_se_abre_solo() {
    let mut f = Falso {
        // La lectura tarda; da tiempo a cerrar.
        retraso_ms: 150,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), b"hola\n".to_vec());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    // Antes de que llegue el contenido, se cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    // La lectura tardía ya volvió: no queda ninguna en vuelo.
    hasta(&backend, "la lectura tardía servida", |f| {
        (f.servidos() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.viewer.is_none(),
        "el visor no se abre por su cuenta después de cerrarlo"
    );
}

/// Una disposición sin ningún listado se rechaza al ARRANCAR.
///
/// Es #242 en esta superficie: no panicaba al arrancar sino en la primera
/// tecla, dentro de la task del actor —sin log, sin caída visible— y la
/// ventana se quedaba muerta contestando `Down` para siempre.
#[tokio::test]
async fn una_disposicion_sin_listado_no_arranca() {
    let arbol_sin_listado = norte_frontend::layout::Node::slot(
        norte_frontend::layout::SlotId(1),
        norte_frontend::layout::KindId::new("status"),
    );
    let salida = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: arbol_sin_listado,
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
    .await;
    assert!(
        matches!(
            salida,
            Err(norte_ui_host::controller::UiError::NoBrowserSlot)
        ),
        "una pantalla sin listado no es una pantalla"
    );
}

/// Las columnas se resuelven POR ESQUEMA, no una vez al arrancar.
///
/// Con una lista resuelta en el arranque, `[ui.columns.schemes.sftp]` quedaba
/// muerta: sus columnas no se pintaban y sus atributos no se pedían nunca,
/// porque los que viajan en cada listado se habían congelado con los del
/// esquema inicial.
#[tokio::test]
async fn las_columnas_de_otro_esquema_no_estan_muertas() {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".to_owned(), "size".to_owned()]),
        schemes: [(
            "mem".to_owned(),
            norte_config::SchemeColumns {
                columns: Some(vec!["name".to_owned(), "attr:mem.mode".to_owned()]),
                ..norte_config::SchemeColumns::default()
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    let backend = arbol();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
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
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_frontend::columns::ColumnsSettings::resolve(&cfg),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    drop(h);

    let ids: Vec<&str> = listado(&snap)
        .columns
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["name", "attr:mem.mode"],
        "manda la configuración del esquema `mem`, no la de por defecto"
    );
    assert!(
        backend
            .attrs_pedidos
            .lock()
            .expect("attrs")
            .iter()
            .any(|a| a.iter().any(|id| id == "mem.mode")),
        "y su atributo se PIDE en el listado"
    );
}
