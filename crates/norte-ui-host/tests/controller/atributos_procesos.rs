use super::*;

// ---------------------------------------------------------------------------
// La hoja de atributos y el panel de procesos (huecos de la fase 4).
// ---------------------------------------------------------------------------

/// Un host con la disposición `full`, que trae hoja de atributos y panel de
/// procesos además de los dos listados.
pub(super) async fn host_full(backend: Arc<Falso>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
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

/// El PRIMER listado de una foto, sea cual sea su posición.
///
/// `listado` mira el hueco 0, que en `simple` es el listado; en `full` el
/// hueco 0 es la barra lateral de sitios.
pub(super) fn primer_listado(
    snap: &norte_ui_host::ViewSnapshot,
) -> &norte_ui_host::dto::BrowserSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) => Some(b.as_ref()),
            _ => None,
        })
        .expect("la disposición tiene algún listado")
}

/// La hoja de atributos de una foto, si está colocada.
pub(super) fn hoja(
    snap: &norte_ui_host::ViewSnapshot,
) -> Option<&norte_ui_host::dto::MetadataSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Metadata(m) => Some(m.as_ref()),
        _ => None,
    })
}

/// La hoja de atributos enseña la entrada bajo el cursor del listado al que
/// SIGUE, y se mueve con él.
#[tokio::test]
async fn la_hoja_de_atributos_sigue_al_cursor() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("la disposición `full` coloca la hoja");
    assert!(
        primera.note.is_empty() && !primera.fields.is_empty(),
        "con un listado con entradas, la hoja enseña la primera: {primera:?}"
    );
    let nombre_de = |m: &norte_ui_host::dto::MetadataSlotView| {
        m.fields
            .first()
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let antes = nombre_de(primera);
    assert!(!antes.is_empty(), "el primer campo es el nombre");

    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let despues = nombre_de(hoja(&foto).expect("sigue colocada"));
    assert_ne!(
        antes, despues,
        "la hoja siguió al cursor sin que nadie pidiera nada"
    );
}

/// Con el FOCO en la propia hoja sigue enseñando la entrada del listado
/// activo: seguir al rol activo cuando el activo es ella misma era seguir a
/// nadie, y la hoja se vaciaba al pulsarla (mismo fallo que el visor
/// acoplado, #291).
#[tokio::test]
async fn la_hoja_de_atributos_enfocada_no_se_vacia() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("la disposición `full` coloca la hoja");
    assert!(!primera.fields.is_empty());
    let slot = primera.slot_id;
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let enfocada = foto_hasta(&h, &mut sub, "la hoja con el foco", |s| {
        (s.focus == Some(slot)).then(|| s.clone())
    })
    .await;
    let h2 = hoja(&enfocada).expect("sigue colocada");
    assert!(
        h2.note.is_empty() && h2.fields == primera.fields,
        "la hoja enfocada sigue enseñando la entrada del listado: {h2:?}"
    );
}

/// Como [`host_full`], pero con la fila `..` ENCENDIDA — que es lo que trae
/// la configuración de fábrica y lo que ve cualquiera que abra la ventana.
///
/// El resto de esta suite la apaga a propósito (razona sobre índices de
/// listado). Los tests de los paneles que SIGUEN al cursor no pueden
/// permitírselo: el cursor nace justo sobre esa fila, así que apagarla es
/// probar el único estado en el que nadie arranca.
pub(super) async fn host_full_con_fila_de_subir(
    backend: Arc<Falso>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut cfg = norte_ui_host::ajustes_por_defecto();
    cfg.common.ui_parent_entry = Some(true);
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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

/// Con el cursor sobre `..` la hoja DESCRIBE esa fila, no se vacía.
///
/// El fallo que cierra: la ventana recién abierta enseñaba «nada bajo el
/// cursor» en cada arranque y después de cada `cd`, porque el cursor nace
/// sobre `..` y la hoja preguntaba por el OPERANDO —que sobre esa fila es
/// `None` a propósito— en vez de por lo señalado.
#[tokio::test]
async fn la_hoja_describe_la_fila_de_subir_en_vez_de_vaciarse() {
    let (_h, snap) = host_full_con_fila_de_subir(arbol()).await;
    let hoja = hoja(&snap).expect("la disposición `full` coloca la hoja");
    assert!(
        hoja.note.is_empty(),
        "sobre `..` hay algo que describir: {hoja:?}"
    );
    let filas: Vec<(&str, &str)> = hoja
        .fields
        .iter()
        .map(|f| (f.label.as_str(), f.value.as_str()))
        .collect();
    assert_eq!(
        filas,
        [
            ("Nombre", ".."),
            ("Clase", "carpeta"),
            ("Destino", "⟨mem⟩/")
        ],
        "`..` se llama `..` y dice a dónde lleva, no el nombre del padre"
    );
}

/// Subir por la fila `..` deja el cursor sobre el directorio del que se sale.
///
/// Igual que `UiAction::Parent`, que es la otra puerta a la MISMA
/// navegación. Sin esto el cursor aterrizaba en la primera fila del padre
/// según por cuál de las dos se subiera, y subir-y-bajar dejaba de ser
/// reversible por una de ellas.
#[tokio::test]
async fn subir_por_la_fila_de_subir_deja_el_cursor_donde_estabas() {
    let (h, snap) = host_full_con_fila_de_subir(arbol()).await;
    let mut sub = h.subscribe();
    let listado = primer_listado(&snap);
    let slot = listado.slot_id;

    // Bajar a `docs` (la fila 1: detrás de `..`).
    h.dispatch(UiAction::Activate {
        slot_id: slot,
        key: norte_ui_host::RowKey(1),
        generation: listado.generation,
    })
    .await
    .expect("host vivo");
    let dentro = foto_hasta(&h, &mut sub, "el listado de `docs`", |s| {
        let b = primer_listado(s);
        b.path_display.ends_with("docs").then(|| b.clone())
    })
    .await;

    // Y volver a subir POR LA FILA `..`, que es la primera.
    h.dispatch(UiAction::Activate {
        slot_id: slot,
        key: norte_ui_host::RowKey(0),
        generation: dentro.generation,
    })
    .await
    .expect("host vivo");
    let fuera = foto_hasta(&h, &mut sub, "de vuelta en `casa`", |s| {
        let b = primer_listado(s);
        b.path_display.ends_with("casa").then(|| b.clone())
    })
    .await;
    let bajo_el_cursor = fuera
        .cursor
        .and_then(|k| fuera.rows.get(usize::try_from(k.0).unwrap_or(0)))
        .map(|r| r.display_name.clone());
    assert_eq!(
        bajo_el_cursor.as_deref(),
        Some("docs"),
        "el cursor vuelve al directorio del que se salió, no a la fila 0: {:?}",
        fuera
            .rows
            .iter()
            .map(|r| &r.display_name)
            .collect::<Vec<_>>()
    );
}

/// La hoja DICE a qué listado sigue, y cambia cuando cambia el foco.
///
/// «Detalles» a secas no dice de qué son los detalles: con dos listados
/// abiertos, la única forma de saber cuál está describiendo era mover el
/// cursor y ver si la hoja se movía. Ahora lleva la ruta del panel al que
/// sigue, que es la pregunta que faltaba contestar.
#[tokio::test]
async fn la_hoja_dice_a_que_listado_sigue() {
    let (h, snap) = host_full_con_fila_de_subir(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("colocada");
    assert_eq!(
        primera.follows_display,
        norte_frontend::path_display(&dir()).0,
        "la ruta del listado al que sigue: {primera:?}"
    );
    assert!(!primera.follows_hostile);

    // Con el foco en el OTRO listado, la hoja lo dice: sigue al activo.
    let otro = snap
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(b.slot_id),
            _ => None,
        })
        .nth(1)
        .expect("`full` tiene dos listados");
    h.dispatch(UiAction::Activate {
        slot_id: otro,
        key: norte_ui_host::RowKey(1),
        generation: 0,
    })
    .await
    .ok();
    h.dispatch(UiAction::FocusSlot { slot_id: otro })
        .await
        .expect("host vivo");
    let foto = foto_hasta(&h, &mut sub, "el foco en el otro listado", |s| {
        (s.focus == Some(otro)).then(|| s.clone())
    })
    .await;
    let h2 = hoja(&foto).expect("sigue colocada");
    assert!(
        !h2.follows_display.is_empty(),
        "y sigue diciendo a quién sigue: {h2:?}"
    );
}

/// Un listado y una hoja de atributos, SIN visor acoplado.
///
/// La disposición `full` tiene los dos, y eso escondía el fallo: la hoja no
/// tenía forma de actualizarse sola y viajaba de gorra en la foto entera que
/// el VISOR provocaba al cambiar de nota. Sin visor en la disposición no hay
/// quien la arrastre, y la hoja se quedaba congelada.
pub(super) async fn host_hoja_sin_visor(
    backend: Arc<Falso>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_frontend::layout::{Bindings, Dir, Follow, KindId, Node, RoleId, SlotId};
    let mut cfg = norte_ui_host::ajustes_por_defecto();
    cfg.common.ui_parent_entry = Some(true);
    let arbol = Node::split(
        Dir::Horizontal,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot_bound(
                SlotId(8),
                KindId::new("metadata"),
                Bindings {
                    follows: Some(Follow::Role(RoleId::Active)),
                },
            ),
        ],
    );
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
        viewport: (200, 60),
        settings: cfg,
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

/// Los kinds cuya vista sale del CURSOR del listado al que siguen.
///
/// Lista a mano y a propósito, como `paridad.rs::NO_APLICA`: quien añada un
/// hueco que siga al cursor la edita, y el test de abajo le exige una sonda.
/// Derivarla del registro de kinds no vale — «seguir» es un vínculo del
/// hueco, no una propiedad del kind, así que el registro no lo sabe.
pub(super) const SIGUEN_AL_CURSOR: &[&str] = &["viewer", "metadata"];

/// Cada uno de ellos tiene camino propio hasta el renderer (ADR 0097, D3).
///
/// La ventana habla por PARCHES: uno de filas escribe `generation`,
/// `first_visible`, `rows` y `cursor`, y nada más. Un panel que se deriva del
/// cursor y no tiene sonda propia solo se refresca cuando OTRO panel provoca
/// una foto entera — y la disposición de fábrica (`orthodox`) no coloca
/// ninguno de los dos, así que ese «otro» no existe para la mayoría.
///
/// Así se quedó congelada la hoja de atributos: viajaba de gorra en la foto
/// del visor. Este test pone cada kind SOLO con un listado, mueve el cursor,
/// y exige una foto SIN pedir `Resync` — que es lo único que tiene el
/// renderer de verdad.
#[tokio::test]
async fn todo_hueco_que_sigue_al_cursor_tiene_sonda_propia() {
    use norte_frontend::layout::{Bindings, Dir, Follow, KindId, Node, RoleId, SlotId};
    for kind in SIGUEN_AL_CURSOR {
        let arbol_layout = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot_bound(
                    SlotId(8),
                    KindId::new(*kind),
                    Bindings {
                        follows: Some(Follow::Role(RoleId::Active)),
                    },
                ),
            ],
        );
        let h = UiHost::start(UiHostOptions {
            backend: arbol(),
            initial_dir: dir(),
            initial_dir_pedido: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox")
                .expect("preset"),
            layout: arbol_layout,
            viewport: (200, 60),
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
        let (h, snap) = h;
        let mut sub = h.subscribe();
        let listado = primer_listado(&snap);

        h.dispatch(UiAction::SelectRow {
            slot_id: listado.slot_id,
            key: norte_ui_host::RowKey(1),
            generation: listado.generation,
        })
        .await
        .expect("host vivo");
        asentar().await;

        tokio::time::pause();
        let llegada =
            tokio::time::timeout(std::time::Duration::from_secs(5), siguiente_foto(&mut sub)).await;
        tokio::time::resume();
        assert!(
            llegada.is_ok(),
            "el hueco `{kind}` sigue al cursor y no manda nada al moverlo: \
             se queda congelado en cualquier disposición que no traiga otro \
             panel que provoque una foto"
        );
    }
}

/// Pinchar una fila mueve la hoja, aunque no haya visor que arrastre la foto.
///
/// Lo que veía Oscar: en una disposición con árbol, dos listados y detalles
/// —sin visor—, hacer clic en cualquier fila de cualquier panel dejaba la
/// hoja en `..` para siempre. `SelectRow` contesta con un parche de FILAS, y
/// la hoja solo viaja en la foto entera.
#[tokio::test]
async fn pinchar_una_fila_mueve_la_hoja_sin_visor_en_la_disposicion() {
    let (h, snap) = host_hoja_sin_visor(arbol()).await;
    let mut sub = h.subscribe();
    let primera = hoja(&snap).expect("la disposición coloca la hoja");
    assert_eq!(
        primera.fields.first().map(|f| f.value.as_str()),
        Some(".."),
        "de partida, la fila de subir"
    );
    let listado = primer_listado(&snap);
    let generation = listado.generation;
    // La fila 2 del listado: `..`, `docs`, y la siguiente.
    let objetivo = listado
        .rows
        .get(2)
        .expect("hay tercera fila")
        .display_name
        .clone();

    h.dispatch(UiAction::SelectRow {
        slot_id: listado.slot_id,
        key: norte_ui_host::RowKey(2),
        generation,
    })
    .await
    .expect("host vivo");
    asentar().await;

    // SIN `Resync`, y ahí está la gracia: `foto_hasta` pide una foto en cada
    // vuelta, así que un test escrito con él se pone verde aunque el clic no
    // mande nada — la foto que examina la provocó el propio test. Lo que se
    // comprueba aquí es lo que el host manda POR SU CUENTA al pinchar, que es
    // lo único que tiene el renderer.
    tokio::time::pause();
    let llegada =
        tokio::time::timeout(std::time::Duration::from_secs(5), siguiente_foto(&mut sub)).await;
    tokio::time::resume();
    let foto = llegada.expect("pinchar no produjo ninguna foto: la hoja se queda congelada");

    let hoja = hoja(&foto).expect("sigue colocada");
    assert_eq!(
        hoja.fields.first().map(|f| f.value.as_str()),
        Some(objetivo.as_str()),
        "la hoja describe la fila pinchada: {:?}",
        hoja.fields
    );
}

/// Con un filtro eligiendo otra fila, la hoja describe ESA fila y no `..`.
///
/// El cursor REAL no se mueve en modo Filter, así que preguntar
/// `is_parent_row(cursor())` por un lado y `cursor_entry()` por otro dejaba
/// la hoja diciendo «`..`, carpeta» mientras el listado resaltaba un fichero
/// — y el nombre hostil que el lector estaba mirando no se marcaba, que es
/// justo para lo que se consulta esta hoja.
#[tokio::test]
async fn con_un_filtro_la_hoja_describe_la_fila_elegida_y_no_la_de_subir() {
    let (h, _snap) = host_full_con_fila_de_subir(arbol()).await;
    let mut sub = h.subscribe();
    // `caf\xC3(` es la entrada no-UTF-8 del árbol de pruebas: se filtra por
    // una letra que la fila `..` no tiene.
    ejecutar_por_paleta(&h, &mut sub, "pane.quick-search").await;
    for c in "caf".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    // Se espera a la entrada HOSTIL, no a «la primera que no sea `..`»: con
    // la query a medias («c») el filtro pasa por `docs`, que también es una
    // fila de verdad y contestaría a esa pregunta sin probar nada.
    let foto = foto_hasta(&h, &mut sub, "la hoja sobre lo filtrado", |s| {
        hoja(s)
            .filter(|m| m.fields.first().is_some_and(|f| f.hostile))
            .cloned()
    })
    .await;
    let nombre = foto.fields.first().expect("hay nombre");
    assert!(
        nombre.hostile,
        "describe la entrada resaltada, y la marca: {nombre:?}"
    );
    assert!(
        !foto.fields.iter().any(|f| f.label == "Destino"),
        "y no la fila de subir: {:?}",
        foto.fields
    );
}

/// Y el visor acoplado dice «directorio», no «nada seleccionado».
///
/// La misma avería en el otro panel que sigue al cursor, y por la misma
/// razón: preguntaba por el operando.
#[tokio::test]
async fn el_visor_acoplado_sobre_la_fila_de_subir_dice_directorio() {
    let (h, _snap) = host_full_con_fila_de_subir(arbol()).await;
    let mut sub = h.subscribe();
    // Hasta que el listado aterriza no hay cursor, y ESA nota es otra: se
    // espera a la del directorio, como el resto de los tests del visor.
    let vista = foto_hasta(&h, &mut sub, "el hueco de preview sobre `..`", |s| {
        s.slots
            .iter()
            .find_map(|v| match v {
                SlotView::Preview(p) => Some(p.as_ref().clone()),
                _ => None,
            })
            .filter(|p| p.viewer.is_none() && p.note == "directorio")
    })
    .await;
    assert_eq!(
        vista.note, "directorio",
        "`..` lleva a una carpeta: eso es lo que hay bajo el cursor"
    );
}

/// Un nombre hostil llega a la hoja enmascarado y MARCADO, igual que a una
/// fila del listado.
#[tokio::test]
async fn un_nombre_hostil_en_la_hoja_va_marcado() {
    let (h, snap) = host_full(arbol()).await;
    let mut sub = h.subscribe();
    // El árbol de pruebas tiene una entrada cuyo nombre no es UTF-8.
    let mut vista = hoja(&snap).expect("colocada").clone();
    for _ in 0..6 {
        if vista.fields.first().is_some_and(|f| f.hostile) {
            break;
        }
        h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        vista = hoja(&foto).expect("colocada").clone();
    }
    let nombre = vista.fields.first().expect("hay nombre");
    assert!(nombre.hostile, "la entrada no-UTF-8 se marca: {nombre:?}");
    assert!(
        !nombre.value.contains('\u{fffd}') || nombre.hostile,
        "y su texto ya viene saneado"
    );
}

/// Con el foco en el panel de PROCESOS, bajar baja por él y no por el
/// listado de al lado.
#[tokio::test]
async fn el_panel_de_procesos_toma_sus_teclas() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_full(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let cursor_antes = primer_listado(&snap).cursor;

    // Se lanzan dos borrados para que el tablero tenga filas. El id del
    // diálogo se LEE, y se espera al NUEVO: fijarlo a mano dejaba el segundo
    // sin contestar, y un diálogo abierto se queda el teclado —que es justo
    // lo que debe hacer—, así que el tabulador ya no llegaba a ningún lado.
    // `Enter` no vale para confirmarlo: en un borrado `confirm` es
    // destructivo y el teclado elige la primera respuesta que no lo es.
    let mut contestado = norte_ui_host::ModalId(0);
    for _ in 0..2 {
        h.dispatch(tecla("F8")).await.expect("host vivo");
        let id = loop {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            if let Some(d) = siguiente_foto(&mut sub).await.dialogs.last()
                && d.id != contestado
            {
                break d.id;
            }
        };
        contestado = id;
        h.dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    }

    // Se rota el foco hasta el panel de procesos. Por el recorrido de la
    // PANTALLA (`alt+o`): `Tab` cicla listados y no para en los laterales
    // (ADR 0102).
    let mut en_procesos = false;
    for _ in 0..8 {
        h.dispatch(tecla_alt("o")).await.expect("host vivo");
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let activo = foto
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        if let Some(id) = activo
            && foto
                .slots
                .iter()
                .any(|s| matches!(s, SlotView::Processes { slot_id, .. } if *slot_id == id))
        {
            en_procesos = true;
            break;
        }
    }
    assert!(en_procesos, "el anillo llega al panel de procesos");

    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        primer_listado(&foto).cursor,
        cursor_antes,
        "bajar con el foco en procesos NO mueve el listado: el rol lo pintaba \
         enfocado y las teclas se iban al panel de al lado"
    );
}

/// La barra lateral de sitios de una foto, si está colocada.
pub(super) fn sitios(
    snap: &norte_ui_host::ViewSnapshot,
) -> Option<&norte_ui_host::dto::PlacesSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Places(p) => Some(p.as_ref()),
        _ => None,
    })
}

/// La barra lateral llega con sus DOS cabeceras desde el primer frame, y los
/// volúmenes se le añaden cuando el host contesta.
#[tokio::test]
async fn la_barra_de_sitios_no_da_un_brinco_cuando_llegan_los_discos() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let primera = sitios(&snap).expect("la disposición `full` coloca la barra");
    let cabeceras = primera
        .rows
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Header { .. }))
        .count();
    assert_eq!(
        cabeceras, 2,
        "las dos cabeceras están desde el principio, aunque no haya nada debajo"
    );

    // Los volúmenes llegan después: la barra no espera a ellos para pintarse.
    let mut con_discos = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let v = sitios(&foto).expect("sigue colocada").clone();
        if v.rows
            .iter()
            .any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            con_discos = Some(v);
            break;
        }
    }
    let v = con_discos.expect("los volúmenes llegan a la barra");
    let disco = v
        .rows
        .iter()
        .find_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Drive { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .expect("hay un disco");
    assert!(!disco.is_empty(), "y dice cuánto espacio tiene");
}

/// Con el foco en la barra lateral, bajar baja por ELLA, y entrar navega el
/// LISTADO — que es lo que hace que tenerla abierta no cambie a dónde van las
/// operaciones.
#[tokio::test]
async fn la_barra_de_sitios_navega_el_listado_y_no_se_lo_queda() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, _snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // Se espera a que los discos estén, y se busca su fila.
    let mut fila_del_disco = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let v = sitios(&foto).expect("colocada");
        if let Some(i) = v
            .rows
            .iter()
            .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            fila_del_disco = Some((i, v.generation));
            break;
        }
    }
    let (i, generacion) = fila_del_disco.expect("los discos llegan");

    // Un click en el disco: elige Y activa, porque una barra lateral existe
    // para ir a sitios.
    h.dispatch(UiAction::PlaceActivateRow {
        row: u32::try_from(i).expect("cabe"),
        generation: generacion,
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let mut llego = false;
    for _ in 0..20 {
        let foto = siguiente_foto(&mut sub).await;
        if primer_listado(&foto).path_display.contains("otro") {
            llego = true;
            break;
        }
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }
    assert!(llego, "el LISTADO navegó al volumen, no la barra");
}

/// Un click en la barra lateral no puede navegar a un sitio que no se pulsó.
///
/// Los volúmenes llegan de una tarea de fondo y se insertan EN MEDIO —las
/// unidades van antes que los favoritos—, así que entre que el usuario suelta
/// el botón sobre un favorito y el host atiende la acción, esa fila es otra.
/// Sin generación el host la aceptaba, y `set_cursor` recorta en vez de
/// rechazar, así que el peor caso era navegar al ÚLTIMO sitio de la lista con
/// un acuse `Applied`. Es la carrera que el ADR 0068 existe para cerrar.
#[tokio::test]
async fn un_click_en_la_barra_no_navega_a_otro_sitio_si_la_lista_cambio() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.hotlist = vec![norte_config::HotlistItem {
        name: "proyectos".to_owned(),
        target: norte_proto::VPath::parse("mem:///proyectos").map_err(|_| "err".to_owned()),
    }];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.pon("mem:///proyectos", vec![(b"p.txt".to_vec(), false)]);
    f.pon("mem:///boot", vec![(b"vmlinuz".to_vec(), false)]);
    f.pon("mem:///datos", vec![(b"d.txt".to_vec(), false)]);
    // DOS discos: desplazan el favorito lo justo para que su índice caiga
    // sobre un disco y no sobre una cabecera. Con uno el click habría plegado
    // una sección, que también está mal pero se nota menos.
    f.volumenes = vec![
        volumen("mem:///boot", "ext4", false),
        volumen("mem:///datos", "ext4", false),
    ];
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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

    // La foto que el usuario TIENE DELANTE, antes de que lleguen los discos.
    let antes = sitios(&snap).expect("colocada").clone();
    let fila_pulsada = antes
        .rows
        .iter()
        .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))
        .expect("el favorito está desde el principio");

    // Los discos aterrizan y la lista es OTRA.
    let mut despues = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let v = sitios(&siguiente_foto(&mut sub).await)
            .expect("colocada")
            .clone();
        if v.generation != antes.generation {
            despues = Some(v);
            break;
        }
    }
    let despues = despues.expect("los volúmenes llegan y suben la generación");
    assert!(
        matches!(
            despues.rows.get(fila_pulsada),
            Some(norte_ui_host::dto::PlaceRowView::Drive { .. })
        ),
        "la fila que se pulsó es ahora un DISCO, que es lo que hace peligroso \
         el índice desnudo: {:?}",
        despues.rows.get(fila_pulsada)
    );

    // El click en vuelo, con la generación de la pantalla que se vio.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(fila_pulsada).expect("cabe"),
            generation: antes.generation,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "se rechaza en vez de navegar a otro sitio: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    for _ in 0..8 {
        let foto = siguiente_foto(&mut sub).await;
        assert!(
            !primer_listado(&foto).path_display.contains("boot"),
            "y el panel NO se fue al disco que nadie pulsó"
        );
        h.dispatch(UiAction::Resync).await.expect("host vivo");
    }

    // Con la generación buena, el mismo click sí va.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(fila_pulsada).expect("cabe"),
            generation: despues.generation,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "y la generación buena sí vale: {ack:?}"
    );
}

/// Un favorito cuya ruta no parsea se PINTA con su motivo: uno que
/// desaparece en silencio es un fallo de configuración que nadie puede ver.
#[tokio::test]
async fn un_favorito_roto_se_ve_y_dice_por_que() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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
    let _ = &h;

    let v = sitios(&snap).expect("colocada");
    let favoritos: Vec<(String, String)> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Favorite { name, broken, .. } => {
                Some((name.clone(), broken.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        favoritos.len(),
        2,
        "los dos favoritos se ven: {favoritos:?}"
    );
    let roto = favoritos
        .iter()
        .find(|(n, _)| n == "roto")
        .expect("el roto está");
    assert!(!roto.1.is_empty(), "y dice por qué está roto");
    assert!(
        !roto.1.starts_with("err-"),
        "traducido, no la clave: {roto:?}"
    );
}
