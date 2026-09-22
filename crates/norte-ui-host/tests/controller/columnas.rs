use super::*;

// ---------------------------------------------------------------------------
// Cabeceras y orden (fase 4, tarea 4.2).
// ---------------------------------------------------------------------------

/// El listado viaja con sus CABECERAS: etiqueta ya traducida, alineación y
/// cuál manda el orden. El renderer las pinta; no las inventa ni las traduce.
#[tokio::test]
async fn el_listado_lleva_sus_cabeceras() {
    let (_h, snap) = host_arbol(arbol()).await;
    let b = listado(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["name", "size", "mtime"],
        "las columnas configuradas, en orden y con el nombre delante"
    );
    for c in &b.columns {
        assert!(!c.label.is_empty(), "cada cabecera trae su etiqueta: {c:?}");
    }
    let nombre = &b.columns[0];
    assert_eq!(
        nombre.sort.as_deref(),
        Some("asc"),
        "y dice cuál ordena y en qué sentido"
    );
    assert!(b.columns[1].sort.is_none(), "las demás, no");
}

/// Ordenar por una columna es del host: misma regla que el TUI —la misma
/// columna invierte, otra columna empieza ascendente— y el cursor se queda
/// en la MISMA entrada, no en la misma fila.
#[tokio::test]
async fn ordenar_por_columna_usa_la_regla_compartida() {
    let (h, snap) = host_arbol(arbol()).await;
    // Los FICHEROS, sin el directorio: `dirs_first` los agrupa aparte y ese
    // grupo va siempre ascendente — invertir el orden no lo toca.
    let ficheros = |s: &norte_ui_host::ViewSnapshot| -> Vec<String> {
        listado(s)
            .rows
            .iter()
            .filter(|r| r.kind != norte_ui_host::dto::RowKind::Dir)
            .map(|r| r.display_name.clone())
            .collect()
    };
    let antes = ficheros(&snap);
    assert!(antes.len() >= 2, "hay ficheros que ordenar: {antes:?}");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let invertido = siguiente_foto(&mut sub).await;
    let despues = ficheros(&invertido);
    let mut al_reves = antes.clone();
    al_reves.reverse();
    assert_eq!(
        despues, al_reves,
        "la misma columna dos veces invierte el sentido"
    );
    assert_eq!(
        listado(&invertido).rows[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "y los directorios siguen primero: invertir no toca su grupo"
    );
    assert_eq!(
        listado(&invertido).columns[0].sort.as_deref(),
        Some("desc"),
        "y la cabecera lo dice"
    );
}

/// Una columna que no ordena —o que no está— no altera el listado, y se
/// responde en vez de callarse.
#[tokio::test]
async fn ordenar_por_una_columna_que_no_ordena_se_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "no-existe".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "se dice que esa columna no ordena: {ack:?}"
    );
    // Un atributo que el esquema no tiene configurado tampoco (ADR 0144):
    // el texto viene del renderer, y no debe acabar en el orden ni en la
    // sesión sin que nadie haya pedido esa columna.
    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "attr:no-existe".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "un attr sin configurar no ordena: {ack:?}"
    );
}

/// ADR 0144: la cabecera de un atributo ordena como las demás —es
/// clicable, y tras pulsarla lleva la flecha— sin que el puente cambie: la
/// columna viaja como el mismo texto `attr:<id>` que ya nombraba la cabecera.
#[tokio::test]
async fn un_atributo_ordena_desde_su_cabecera() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol() as Arc<dyn norte_ui_host::HostBackend>,
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
        columns: columnas_de(&["name", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let antes = siguiente_foto(&mut sub).await;
    let modo = |s: &norte_ui_host::ViewSnapshot| {
        listado(s)
            .columns
            .iter()
            .find(|c| c.id == "attr:posix.mode")
            .cloned()
            .expect("la cabecera del modo")
    };
    assert!(
        modo(&antes).sortable,
        "la cabecera del atributo es clicable"
    );
    assert!(modo(&antes).sort.is_none(), "y aún no manda el orden");

    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "attr:posix.mode".to_owned(),
        })
        .await
        .expect("host vivo");
    assert!(
        !matches!(ack, ActionAck::Unavailable { .. }),
        "un atributo ordena: {ack:?}"
    );
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if modo(&foto).sort.as_deref() == Some("asc") {
            assert!(
                listado(&foto).columns[0].sort.is_none(),
                "el nombre deja de mandar"
            );
            return;
        }
    }
    panic!("la cabecera del atributo tiene que llevar la flecha tras ordenar");
}

// ---------------------------------------------------------------------------
// Decoraciones y columnas de plugin (tarea 4.2).
// ---------------------------------------------------------------------------

/// Un backend con `n` entradas, una insignia en la primera y una columna de
/// plugin con valor para todas.
pub(super) fn arbol_grande_con_plugins(n: usize) -> Arc<Falso> {
    let nombres: Vec<(Vec<u8>, bool)> = (0..n)
        .map(|i| (format!("f{i:05}.txt").into_bytes(), false))
        .collect();
    let mut f = Falso::default();
    f.arbol.insert("mem:///casa".to_owned(), nombres);
    f.decoraciones
        .insert("mem:///casa/f00000.txt".to_owned(), "M".to_owned());
    *f.plugins.lock().expect("plugins") = vec![{
        let mut p = extension("acme.git", "Git", true);
        // DECLARADA en el catálogo: `validated_plugin_requests` no pide una
        // columna que su plugin no dice tener, para no atribuirla a quien no
        // es.
        p.columns = vec![norte_proto::methods::PluginColumnInfo {
            id: "status".to_owned(),
            header: "Estado".to_owned(),
        }];
        p
    }];
    for i in 0..n {
        f.valores_de_columna.insert(
            ("status".to_owned(), format!("mem:///casa/f{i:05}.txt")),
            "limpio".to_owned(),
        );
    }
    Arc::new(f)
}

/// Solo se le pregunta a los plugins por lo que se VE.
///
/// Cada llamada levanta una instancia de wasm por plugin: #224 midió 167 ms
/// por página de 20 sobre 2000 entradas. Preguntar por el directorio entero
/// multiplica ese precio por el tamaño del directorio, y para nada — el
/// renderer solo puede pintar su ventana. Es donde esto se separa del TUI,
/// que decora todo lo cargado porque su pane no declara ventana.
/// Lo que un plugin contesta LLEGA a la celda de su fila, y su cabecera se
/// llama como el manifiesto dice (`[[contributions.columns]] header`) y no
/// como su id. Hasta aquí solo se comprobaba que la columna se PIDIERA.
#[tokio::test]
async fn la_columna_de_un_plugin_llega_a_la_fila_y_se_llama_como_su_manifiesto() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 20,
    })
    .await
    .expect("host vivo");

    let foto = esperar_foto(&h, &mut sub, "la celda del plugin llegue", |f| {
        listado(f)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.as_deref() == Some("limpio")))
    })
    .await;
    let b = listado(&foto);
    let columna = b
        .columns
        .iter()
        .find(|c| c.id == "plugin:acme.git/status")
        .expect("la columna configurada se pinta");
    assert_eq!(
        columna.label, "Estado",
        "el rótulo del manifiesto, no el id crudo"
    );
    let fila = &b.rows[0];
    let celda = fila
        .cells
        .iter()
        .find(|c| c.column == "plugin:acme.git/status")
        .expect("la fila lleva la celda de esa columna");
    assert_eq!(celda.text.as_deref(), Some("limpio"));
}

#[tokio::test]
async fn a_los_plugins_solo_se_les_pregunta_por_la_ventana() {
    const TOTAL: usize = 2000;
    let backend = arbol_grande_con_plugins(TOTAL);
    let (h, _snap) = UiHost::start(UiHostOptions {
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
        columns: columnas_de(&["name", "size", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 20,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }

    let lotes = backend.decorados.lock().expect("mutex").clone();
    assert!(!lotes.is_empty(), "se pregunta a los plugins");
    for lote in &lotes {
        assert!(
            lote.len() <= 20,
            "un lote de {} rutas sobre {TOTAL} entradas: se está pidiendo más \
             que la ventana",
            lote.len()
        );
    }
    let pedidas: usize = lotes.iter().map(Vec::len).sum();
    assert!(
        pedidas <= 40,
        "en total se pidieron {pedidas} de {TOTAL}: la ventana es 20"
    );

    // Y la columna de plugin viaja por el mismo lote, no por otro barrido.
    let cols = backend.columnas_pedidas.lock().expect("mutex").clone();
    assert!(!cols.is_empty(), "la columna configurada se pide");
    for (plugin, columna, paths) in &cols {
        assert_eq!(plugin, "acme.git");
        assert_eq!(columna, "status");
        assert!(
            paths.len() <= 20,
            "la columna se pide para {} rutas, no para la ventana",
            paths.len()
        );
    }
}

/// ADR 0105: el icono llega a la fila en su propio campo, la insignia en el
/// suyo —los dos huecos coexisten—, y el lote que se pide decorar lleva la
/// CLASE de cada ruta, sin la que un decorador de iconos no sabe qué es
/// carpeta.
#[tokio::test]
async fn el_icono_de_un_plugin_llega_a_la_fila_y_la_clase_viaja() {
    let mut f = Falso::default();
    f.arbol.insert(
        "mem:///casa".to_owned(),
        vec![(b"src".to_vec(), true), (b"a.rs".to_vec(), false)],
    );
    f.iconos
        .insert("mem:///casa/src".to_owned(), "📁".to_owned());
    f.iconos
        .insert("mem:///casa/a.rs".to_owned(), "🦀".to_owned());
    f.decoraciones
        .insert("mem:///casa/a.rs".to_owned(), "M".to_owned());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host vivo");

    let mut filas = None;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado(&foto);
        if b.rows.iter().any(|r| !r.icon.is_empty()) {
            filas = Some(b.rows.clone());
            break;
        }
    }
    let filas = filas.expect("el icono llega a la fila");
    let src = filas.iter().find(|r| r.display_name == "src").expect("src");
    assert_eq!(src.icon, "📁");
    assert!(src.badge.is_empty());
    let a = filas
        .iter()
        .find(|r| r.display_name == "a.rs")
        .expect("a.rs");
    assert_eq!(a.icon, "🦀", "el icono, en su hueco");
    assert_eq!(a.badge, "M", "y la insignia, en el suyo: no se tapan");
    assert_eq!(a.badge_role, "warning");
    // Y la clase viajó con el lote, posicional: `src` es una carpeta.
    let clases = backend.clases_decoradas.lock().expect("clases");
    let lotes = backend.decorados.lock().expect("decorados");
    let (paths, kinds) = (&lotes[0], &clases[0]);
    assert_eq!(paths.len(), kinds.len(), "una clase por ruta");
    let de_src = paths
        .iter()
        .position(|p| p.to_wire() == "mem:///casa/src")
        .expect("src en el lote");
    assert_eq!(kinds[de_src], norte_proto::EntryKind::Dir);
}

/// Apagar un decorador desde el gestor QUITA sus insignias de las filas ya
/// pintadas: los listados abiertos olvidan lo que los plugins dijeron y lo
/// vuelven a pedir. Antes se quedaban hasta el siguiente `cd`, y el lector
/// concluía que apagar no apaga.
#[tokio::test]
async fn apagar_un_decorador_desde_el_gestor_quita_sus_insignias() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host vivo");
    let mut con_insignia = false;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).rows.iter().any(|r| !r.badge.is_empty()) {
            con_insignia = true;
            break;
        }
    }
    assert!(con_insignia, "la insignia llega primero");
    let tandas_antes = backend.decorados.lock().expect("decorados").len();

    // F12, y `e` sobre la única extensión: se apaga.
    h.dispatch(tecla("F12")).await.expect("host vivo");
    let v = extensiones_cargadas(&mut sub).await;
    assert_eq!(v.rows[0].id, "acme.git");
    h.dispatch(tecla("e")).await.expect("host vivo");

    let mut sin_insignia = false;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado(&foto);
        if b.rows.iter().all(|r| r.badge.is_empty())
            && b.rows
                .iter()
                .all(|r| r.cells.iter().all(|c| c.text.is_none()))
        {
            sin_insignia = true;
            break;
        }
    }
    assert!(
        sin_insignia,
        "las filas se quedan sin la insignia del plugin apagado"
    );
    assert!(
        backend.decorados.lock().expect("decorados").len() > tandas_antes,
        "y se volvió a pedir la decoración, no se adivinó"
    );
}

/// La insignia y el valor de columna llegan a la fila, marcados como lo que
/// son: texto de un TERCERO.
#[tokio::test]
async fn la_insignia_de_un_plugin_llega_a_la_fila() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    // Lo que hace el renderer nada más montarse. El arranque NO adorna ni
    // sondea: espera a que se declare la ventana, igual que con los tamaños
    // —pedir por una ventana inventada es pedir de más—.
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host vivo");

    let mut adornada = None;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let b = listado(&foto);
        if let Some(f) = b.rows.iter().find(|r| !r.badge.is_empty()) {
            adornada = Some(f.clone());
            break;
        }
    }
    let fila = adornada.expect("la insignia llega a la fila");
    assert_eq!(fila.display_name, "f00000.txt");
    assert_eq!(fila.badge, "M");
    assert_eq!(
        fila.badge_role, "warning",
        "el rol viene del vocabulario CERRADO del tema, no de una cadena \
         libre que el plugin elija"
    );

    // Y la celda de la columna del plugin.
    let celda = fila
        .cells
        .iter()
        .find(|c| c.column == "plugin:acme.git/status")
        .expect("la columna configurada tiene su celda");
    assert_eq!(celda.text.as_deref(), Some("limpio"));
}

/// Lo que el provider se SALTÓ al listar se dice, y traducido.
///
/// Es la clase de fallo que no se puede descubrir mirando: lo que falta no
/// está, así que no hay ninguna fila donde el lector pueda tropezarse con
/// ello. Un listado incompleto que se calla miente por omisión. La cuenta la
/// da el provider —`FsListResult::skipped`— y `HostBackend::list` la TIRABA.
#[tokio::test]
async fn lo_que_el_provider_se_salto_se_dice() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = Some(3);
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;

    let b = listado(&snap);
    assert_eq!(b.rows.len(), 1, "se pinta lo que sí vino");
    assert!(
        b.skipped_note.contains('3'),
        "y se dice cuántas faltan: {:?}",
        b.skipped_note
    );
    assert!(
        !b.skipped_note.starts_with("listing-"),
        "traducido, no la clave: {:?}",
        b.skipped_note
    );
}

/// Un provider que no lleva la cuenta NO dice que no se saltó ninguna.
///
/// `None` y `Some(0)` no son lo mismo, y afirmar «no falta nada» cuando
/// nadie lo ha comprobado es peor que callarse.
#[tokio::test]
async fn un_provider_sin_cuenta_no_afirma_nada() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = None;
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;
    assert!(listado(&snap).skipped_note.is_empty());
}

/// Y la cuenta es de ESTE listado: no se arrastra al siguiente directorio.
#[tokio::test]
async fn la_cuenta_de_omitidas_no_sobrevive_a_un_cd() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.omitidas = Some(2);
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(!listado(&snap).skipped_note.is_empty(), "el primero sí");

    // El segundo directorio también las salta —el doble contesta lo mismo—,
    // pero lo que importa es que la cuenta se VUELVA a poner y no se herede:
    // `set_listing` la limpia, así que sin `set_skipped` después quedaría
    // vacía. Se comprueba que sigue diciéndose.
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("el directorio está")
        .key;
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host vivo");
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if listado(&foto).path_display.contains("docs") {
            assert!(
                !listado(&foto).skipped_note.is_empty(),
                "la cuenta se vuelve a poner tras el `cd`"
            );
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }
    panic!("nunca llegó el listado de docs");
}

// ---------------------------------------------------------------------------
// El selector de columnas (tarea 4.2).
// ---------------------------------------------------------------------------

/// La primera vista del selector con el cursor donde `aguja` diga.
pub(super) async fn selector_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    por_la_paleta(h, sub, "pane.columns").await;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("el selector de columnas no abre");
}

/// El selector enseña lo configurado, dice su ALCANCE y avisa de que lo
/// elegido no se guarda.
///
/// Lo último importa: esta fase no escribe configuración, y un selector que
/// se calla deja al usuario creyendo que acaba de configurar norte.
#[tokio::test]
async fn el_selector_de_columnas_dice_su_alcance_y_que_no_guarda() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    assert!(!c.rows.is_empty(), "hay columnas que enseñar");
    assert!(
        c.rows[0].fixed,
        "la primera es el NOMBRE, y no se apaga ni se mueve: {:?}",
        c.rows[0]
    );
    assert!(
        !c.title.starts_with("columns-picker"),
        "el título viene traducido: {:?}",
        c.title
    );
    assert!(
        !c.note.is_empty() && !c.note.starts_with("columns-picker"),
        "y dice que no guarda, traducido: {:?}",
        c.note
    );
    for r in &c.rows {
        assert!(!r.label.is_empty(), "cada fila dice cómo se llama: {r:?}");
    }
}

/// Encender una columna `attr:` RE-LISTA el hueco.
///
/// Los valores de un atributo solo llegan si se piden en `fs.list`, así que
/// una columna nueva sobre el listado viejo se quedaría en blanco — y en
/// blanco significa «este fichero no tiene ese atributo», que es otra cosa.
/// La huella que decide si hace falta es la COMPARTIDA (`pane_fingerprint`),
/// la misma que usa el TUI.
#[tokio::test]
async fn encender_una_columna_attr_vuelve_a_listar() {
    let backend = arbol();
    let (h, _snap) = UiHost::start(UiHostOptions {
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
        // SIN la columna de modo: encenderla es lo que cambia la huella.
        columns: columnas_de(&["name", "size", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    // Se baja hasta la fila del atributo y se APAGA: quitarla también cambia
    // la huella, y es el caso que no pide un viaje de más al daemon... pero
    // sí un re-listado, porque `attrs_de` deja de pedirla.
    let fila = c
        .rows
        .iter()
        .position(|r| r.id == "attr:posix.mode")
        .expect("la columna de modo está");
    for _ in 0..fila {
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    let antes = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla(" ")).await.expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    // La foto puede venir atrasada, así que se drena hasta ver el efecto.
    let mut cerrado = false;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.columns.is_none() {
            cerrado = true;
            break;
        }
    }
    assert!(cerrado, "el selector se cierra al aplicar");

    for _ in 0..20 {
        if backend.listados.load(Ordering::SeqCst) > antes {
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }
    panic!(
        "cambiar el conjunto de columnas `attr:` tiene que RE-LISTAR: los \
         valores de un atributo solo llegan pidiéndolos en `fs.list`, y sin \
         volver a pedirlo la columna se queda en blanco — que significa otra \
         cosa"
    );
}

/// Y cambiar solo el ORDEN no re-lista: no cambia qué se pide al provider.
#[tokio::test]
async fn cambiar_el_orden_de_las_columnas_no_vuelve_a_listar() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = selector_columnas(&h, &mut sub).await;

    let antes = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(tecla("s")).await.expect("host vivo");
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }
    assert_eq!(
        backend.listados.load(Ordering::SeqCst),
        antes,
        "ordenar es cosa del pane: no hay nada nuevo que pedirle al provider"
    );
}

/// El PIE del selector anuncia teclas, y esas teclas hacen lo que dice.
///
/// El pie es una cadena del catálogo y las teclas son un `match` del host:
/// dos sitios, ninguna atadura. La primera versión de esto escuchaba `J`/`K`
/// y `→` mientras el pie prometía `Shift+↑/↓` y `F` — una mentira que solo
/// se descubre probando, y que ningún test verde decía.
#[tokio::test]
async fn las_teclas_del_selector_de_columnas_son_las_que_anuncia_su_pie() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let antes = selector_columnas(&h, &mut sub).await;

    // El pie viene del HOST y sale del KEYMAP (#287): no es una cadena que
    // nombre teclas y pueda quedarse rancia cuando alguien las reata.
    let pie = antes.hint.clone();
    assert!(!pie.is_empty(), "el pie llega pintado: {pie:?}");
    assert!(
        pie.contains("activa") && pie.contains("aplica"),
        "y dice qué hace cada acorde: {pie:?}"
    );
    // El cursor abre sobre el NOMBRE, que es fijo: espacio ahí no hace nada,
    // y eso es el contrato —la primera columna ES el nombre por contrato del
    // render— no un fallo.
    assert_eq!(antes.cursor, 0);
    assert!(antes.rows[0].fixed);
    h.dispatch(tecla(" ")).await.expect("host vivo");
    let quieta = siguiente_columnas(&h, &mut sub).await;
    assert!(
        quieta.rows[0].enabled,
        "el nombre no se puede apagar: {:?}",
        quieta.rows[0]
    );

    // Una fila que SÍ se puede tocar: espacio la apaga y la enciende.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let sobre_otra = siguiente_columnas(&h, &mut sub).await;
    let fila = usize::try_from(sobre_otra.cursor).expect("cabe");
    assert!(fila > 0 && !sobre_otra.rows[fila].fixed);
    let encendida = sobre_otra.rows[fila].enabled;
    h.dispatch(tecla(" ")).await.expect("host vivo");
    let despues = siguiente_columnas(&h, &mut sub).await;
    assert_ne!(
        despues.rows[fila].enabled, encendida,
        "espacio activa y desactiva"
    );

    // SHIFT+FLECHA mueve la FILA, no el cursor.
    assert!(pie.contains("Shift+"), "{pie:?}");
    let orden_antes: Vec<String> = despues.rows.iter().map(|r| r.id.clone()).collect();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "ArrowDown".to_owned(),
        ctrl: false,
        alt: false,
        shift: true,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let movido = siguiente_columnas(&h, &mut sub).await;
    let orden_despues: Vec<String> = movido.rows.iter().map(|r| r.id.clone()).collect();
    assert_ne!(
        orden_antes, orden_despues,
        "shift+↓ mueve la fila: {orden_antes:?} → {orden_despues:?}"
    );

    // F cicla el formato de la fila del cursor, si lo admite. Se recorre
    // como lo haría una persona —bajando y mirando dónde está— en vez de
    // apuntar a un índice calculado sobre una lista que el paso anterior
    // acaba de reordenar. Con tope: un bucle sobre una condición que puede
    // no llegar es un test que se CUELGA en vez de fallar, y uno colgado no
    // dice nada.
    assert!(
        pie.to_lowercase().contains('f'),
        "y el acorde de formato: {pie:?}"
    );
    let mut ciclado = false;
    for _ in 0..movido.rows.len() + 2 {
        let v = siguiente_columnas(&h, &mut sub).await;
        let aqui = usize::try_from(v.cursor).expect("cabe");
        let Some(fila) = v.rows.get(aqui) else { break };
        if !fila.format.is_empty() && !fila.format_locked {
            let antes = fila.format.clone();
            h.dispatch(tecla("f")).await.expect("host vivo");
            let luego = siguiente_columnas(&h, &mut sub).await;
            assert_ne!(
                luego.rows[aqui].format, antes,
                "F cicla el formato de la fila del cursor"
            );
            ciclado = true;
            break;
        }
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    }
    assert!(ciclado, "alguna columna admite formato y se pudo ciclar");
}

/// La vista del selector tras la última tecla.
pub(super) async fn siguiente_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("el selector sigue abierto");
}

/// Puente 64: arrastrar el borde de una cabecera fija el ancho de ESA
/// columna en sesión —la cabecera lo declara en celdas, acotado a lo que
/// la configuración acepta— y una columna que el hueco no pinta se rehúsa
/// sin tocar nada.
#[tokio::test]
async fn redimensionar_una_columna_fija_su_ancho_en_la_cabecera() {
    let (h, snap) = host_arbol(arbol()).await;
    let ancho_de = |s: &norte_ui_host::ViewSnapshot, id: &str| {
        listado(s)
            .columns
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.width)
            .expect("la columna se pinta")
    };
    // De fábrica `size` ya es fija (la tabla compartida de anchos), así que
    // lo que se comprueba es que el arrastre la CAMBIA, no que la estrene.
    assert_ne!(
        ancho_de(&snap, "size"),
        Some(12),
        "el ancho de partida no es el pedido"
    );
    let mut sub = h.subscribe();

    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "size".to_owned(),
        cells: 12,
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let despues = siguiente_foto(&mut sub).await;
    assert_eq!(
        ancho_de(&despues, "size"),
        Some(12),
        "la cabecera lo declara"
    );
    assert_eq!(
        listado(&despues)
            .columns
            .iter()
            .find(|c| c.id == "size")
            .map(|c| c.align.as_str()),
        Some("right"),
        "y la alineación configurada viaja con ella"
    );

    // Fuera del rango del loader: se acota, nunca se escribe algo que el
    // siguiente `load` rechazaría entero.
    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "size".to_owned(),
        cells: 900,
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let acotado = siguiente_foto(&mut sub).await;
    assert_eq!(ancho_de(&acotado, "size"), Some(64), "techo del loader");

    // Una columna que este hueco no pinta: obsoleta, y la cabecera no cambia.
    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "attr:nadie".to_owned(),
        cells: 5,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let igual = siguiente_foto(&mut sub).await;
    assert_eq!(ancho_de(&igual, "size"), Some(64));
    assert!(
        listado(&igual).columns.iter().all(|c| c.id != "attr:nadie"),
        "no nace una columna por pedir su ancho"
    );
}
