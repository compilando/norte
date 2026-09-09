use super::*;

// ---------------------------------------------------------------------------
// La disposición proyectada, y un snapshot que de verdad reemplaza (fase 3).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga el VISOR.
///
/// Viaja como parche desde la versión 6: una foto entera por cada línea de
/// scroll mandaba las filas de todos los listados de debajo.
pub(super) async fn siguiente_visor(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ViewerView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización con visor, no un cuelgue")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente {
            match &m.payload {
                UiUpdate::Patch(p) => {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Viewer { viewer } = c {
                            return viewer.clone();
                        }
                    }
                }
                UiUpdate::Snapshot(s) => return s.viewer.clone(),
                UiUpdate::Notice(_) => {}
            }
        }
    }
    panic!("no llegó ninguna actualización con visor");
}

/// Espera la siguiente actualización que traiga disposición.
pub(super) async fn siguiente_disposicion(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::LayoutView {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización con disposición, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Layout(l) = c {
                            return l.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload {
                    return s.layout.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("no llegó ninguna actualización con disposición");
}

/// El renderer no reparte la pantalla: la recibe repartida.
///
/// Sin esto, colocar dos paneles sería una regla de presentación escrita en
/// TypeScript — exactamente lo que la decisión D14 prohíbe—, y encima una
/// distinta de la del TUI.
#[tokio::test]
async fn el_snapshot_reparte_la_pantalla_por_el_renderer() {
    let (_h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let l = &snap.layout;
    assert_eq!(l.cells, (120, 40), "el reparto es del tamaño que se le dio");
    let listados: Vec<&norte_ui_host::dto::SlotPlacement> = l
        .placements
        .iter()
        .filter(|p| [1, 2].contains(&p.slot_id))
        .collect();
    assert_eq!(
        listados.len(),
        2,
        "la disposición de siempre son dos listados"
    );
    let izq = listados[0];
    let der = listados[1];
    assert!(
        izq.width > 0 && izq.height > 0,
        "un hueco pintable tiene área"
    );
    assert!(
        izq.x + izq.width <= der.x,
        "y los dos listados no se solapan: {izq:?} vs {der:?}"
    );
}

/// Activo y destino los resuelve el host, con la MISMA regla que el TUI: el
/// destino es el otro listado visible. El renderer solo los pinta.
#[tokio::test]
async fn los_roles_los_resuelve_el_host_no_el_renderer() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(rol(&snap.layout, 1), Some(SlotRole::Active));
    assert_eq!(rol(&snap.layout, 2), Some(SlotRole::Target));

    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host vivo");
    let despues = siguiente_disposicion(&mut sub).await;
    assert_eq!(rol(&despues, 2), Some(SlotRole::Active), "el foco cambió");
    assert_eq!(
        rol(&despues, 1),
        Some(SlotRole::Target),
        "y el destino también"
    );
}

/// Cambiar el tamaño de la ventana reparte otra vez, y el renderer se entera
/// por el mismo canal ordenado que todo lo demás.
#[tokio::test]
async fn un_resize_reparte_otra_vez_y_lo_dice() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (120, 40)).await;
    let ancho_antes = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("el listado izquierdo se pinta")
        .width;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetViewport {
        width: 200,
        height: 60,
    })
    .await
    .expect("host vivo");
    let despues = siguiente_disposicion(&mut sub).await;
    assert_eq!(despues.cells, (200, 60));
    let ancho_despues = despues
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("sigue pintándose")
        .width;
    assert!(
        ancho_despues > ancho_antes,
        "una ventana más ancha da listados más anchos: {ancho_antes} -> {ancho_despues}"
    );
}

/// Un snapshot REEMPLAZA el estado del renderer, así que tiene que llevarlo
/// entero. Si un resync se comiera el diálogo abierto, el renderer se quedaría
/// pintando una pantalla sin la pregunta que está esperando respuesta — y la
/// operación destructiva seguiría ahí, viva y sin confirmar.
#[tokio::test]
async fn un_resync_no_se_come_el_dialogo_abierto() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let abierto = siguientes_dialogos(&mut sub).await;
    assert_eq!(abierto.len(), 1);

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        foto.dialogs, abierto,
        "el snapshot lleva el diálogo que hay abierto"
    );
}

/// Lo mismo para el tablero: una copia en marcha no puede desaparecer porque
/// el renderer haya pedido una foto nueva.
#[tokio::test]
async fn un_resync_no_se_come_las_tasks_vivas() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let dialogos = siguientes_dialogos(&mut sub).await;
    let id = dialogos[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "nueva".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let vivas = siguientes_tasks(&mut sub).await;
    assert!(!vivas.is_empty(), "hay una task en el tablero");

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.tasks, vivas, "el snapshot lleva el tablero entero");
}

/// Espera a que el falso reciba un lote de `dir_size` (el brazo lo lanza en
/// una tarea aparte, así que no está listo al volver del dispatch).
pub(super) async fn siguiente_recuento(falso: &Falso) -> Vec<VPath> {
    for _ in 0..200 {
        if let Some(lote) = falso.recuentos.lock().expect("recuentos").first() {
            return lote.clone();
        }
        tokio::task::yield_now().await;
    }
    panic!("nadie pidió contar");
}

/// El mensaje de la barra tras un desenlace, reintentando: el progreso viaja
/// por su propio canal y el parche puede tardar un tick en salir.
pub(super) async fn siguiente_mensaje_de_estado(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> String {
    // Cada vuelta es un viaje de ida y vuelta al actor: el bucle avanza al
    // ritmo del host, no al del reloj.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(sub).await;
        if let Some(m) = foto.status.message.clone() {
            return m;
        }
    }
    panic!("la barra no dijo nada");
}

/// `pane.dir-size` cuenta lo MARCADO, y en UNA sola Task (#139, #290).
///
/// Una Task por marca obligaría a quien pregunta a sumar los bytes y los
/// ilegibles por su cuenta, y esos dos no se suman igual.
#[tokio::test]
async fn contar_el_tamano_manda_las_marcas_en_un_solo_lote() {
    let backend = Arc::new(arbol_como_falso());
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MarkRange {
        slot_id: 1,
        from: RowKey(1),
        to: RowKey(2),
        generation: epoca,
    })
    .await
    .expect("host vivo");
    let _ = sub.recv().await.expect("host vivo");

    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;

    let lote = siguiente_recuento(&backend).await;
    assert_eq!(lote.len(), 2, "las dos marcas, en un solo lote: {lote:?}");
    assert_eq!(
        backend.recuentos.lock().expect("recuentos").len(),
        1,
        "y una sola Task"
    );
}

/// **El total de un recuento se DICE.** `fs.dir_size` no publica nada: su
/// resultado es su progreso terminal, así que sin esto la ventana lanzaría la
/// cuenta, la vería terminar y no diría jamás cuánto ocupaba.
#[tokio::test]
async fn el_total_de_un_recuento_llega_a_la_barra() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;
    let _ = siguiente_recuento(&backend).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
    });

    let mensaje = siguiente_mensaje_de_estado(&h, &mut sub).await;
    assert!(
        mensaje.contains('3'),
        "el total dice cuántas entradas: {mensaje}"
    );
}

/// Y lo que NO se pudo leer cambia la frase: un recuento sirve para decidir si
/// algo CABE, así que un total redondo sin haber podido contarlo entero es una
/// respuesta equivocada, no una incompleta.
#[tokio::test]
async fn un_recuento_con_ilegibles_no_da_el_total_a_secas() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;
    let _ = siguiente_recuento(&backend).await;

    let tx = backend
        .progreso
        .lock()
        .expect("progreso")
        .clone()
        .expect("hay task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
        p.unreadable = Some(2);
    });

    let mensaje = siguiente_mensaje_de_estado(&h, &mut sub).await;
    assert!(
        mensaje.contains('2'),
        "tiene que decir cuántas no pudo leer: {mensaje}"
    );
    assert_ne!(
        mensaje,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "msg-dir-size",
            &[("size", "2,0 KB"), ("count", "3")]
        ),
        "y no puede ser la frase del total redondo"
    );
}

/// Sin marcas se cuenta lo que hay bajo el CURSOR: es la misma fuente de
/// «sobre qué opera esto» que usa una transferencia, y no un segundo respaldo
/// que se pueda separar del primero.
#[tokio::test]
async fn contar_el_tamano_sin_marcas_usa_el_cursor() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.dir-size").await;

    let lote = siguiente_recuento(&backend).await;
    assert_eq!(lote.len(), 1, "solo el del cursor: {lote:?}");
}

/// Un barrido con el ratón marca el rango entero de UNA vez, con la regla
/// compartida: qué entra en el rango no lo decide quien pinta.
#[tokio::test]
async fn un_rango_se_marca_de_una_vez() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    assert_eq!(listado(&snap).marks, 0);
    let epoca = listado(&snap).generation;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(3),
            to: RowKey(1),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }));
    let _ = sub.recv().await.expect("el host sigue vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(
        listado(&foto).marks,
        3,
        "los extremos entran, y el orden en que se dan da igual"
    );
}

/// Un extremo de una generación anterior no marca A MEDIAS: marcar hasta un
/// sitio que ya no es el que el usuario señaló es peor que no marcar nada.
#[tokio::test]
async fn un_rango_con_un_extremo_viejo_no_marca_nada() {
    let (h, snap) = host(vec!["a", "b"]).await;
    let epoca = listado(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(99),
            generation: epoca,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// Un listado PEREZOSO —el del provider local (#52): sin tamaño ni fecha—
/// no deja las columnas en blanco: el host sonda lo que se ve.
///
/// El backend de tabla daba tamaño en el propio listado, así que esta
/// diferencia solo se vio cuando el spike de Tauri pintó un directorio real
/// y enseñó dos columnas vacías.
#[tokio::test]
async fn un_listado_perezoso_se_sondea_y_las_celdas_se_llenan() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    f.pon(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    assert!(
        listado(&snap).rows[0]
            .cells
            .iter()
            .all(|c| c.text.is_none()),
        "el listado llega sin tamaño, como el de verdad: {:?}",
        listado(&snap).rows[0].cells
    );

    let mut sub = h.subscribe();
    // El sondeo sale solo, en cuanto el listado aterriza. Se pide foto tras
    // foto: lo que importa es que la pantalla acabe con las celdas llenas, no
    // por qué mensaje llegó. Cada vuelta es un viaje al actor, no una espera.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lleno = listado(&foto)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if lleno {
            assert!(
                !backend.sondeos.lock().expect("sondeos").is_empty(),
                "y se llenaron sondeando, no inventando"
            );
            return;
        }
    }
    panic!("las celdas siguen en blanco: el sondeo no llegó");
}

/// Lo ya sondeado no se vuelve a sondear: un `stat` por repintado sería un
/// bucle contra el daemon, y uno que falla lo sería para siempre.
#[tokio::test]
async fn lo_sondeado_no_se_vuelve_a_pedir() {
    let mut f = Falso {
        lazy: true,
        ..Falso::default()
    };
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    for _ in 0..5 {
        h.dispatch(UiAction::SetVisibleRange {
            slot_id: 1,
            first: 0,
            count: 40,
        })
        .await
        .expect("host vivo");
    }
    // Se espera al PRIMER sondeo y se deja correr lo demás: si los cinco
    // repintados sondearan, los otros cuatro ya estarían encolados.
    hasta(&backend, "el primer sondeo", |f| {
        (!f.sondeos.lock().expect("sondeos").is_empty()).then_some(())
    })
    .await;
    asentar().await;
    let sondeos = backend.sondeos.lock().expect("sondeos").clone();
    assert_eq!(
        sondeos.len(),
        1,
        "una entrada se sondea UNA vez, no una por repintado: {sondeos:?}"
    );
}

// ---------------------------------------------------------------------------
// Dos paneles son DOS paneles (fase 4, tarea 4.1).
// ---------------------------------------------------------------------------

/// La rueda sobre el panel que NO tiene el foco mueve ESE panel, y no le
/// roba el foco al otro.
///
/// Declarar qué filas se ven no es actuar sobre el listado: es decir dónde
/// está mirando el usuario. Tratarlo como una acción del panel activo dejaba
/// el segundo panel de una disposición de dos sin poder desplazarse.
#[tokio::test]
async fn el_panel_inactivo_se_desplaza_sin_robar_el_foco() {
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::SetVisibleRange {
            slot_id: 2,
            first: 1,
            count: 10,
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "el panel de al lado también se desplaza: {ack:?}"
    );

    let mut visto = None;
    for _ in 0..10 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Rows {
                    slot_id,
                    first_visible,
                    ..
                } = c
                {
                    visto = Some((*slot_id, *first_visible));
                }
            }
        }
        if visto.is_some() {
            break;
        }
    }
    assert_eq!(
        visto,
        Some((2, 1)),
        "las filas que viajan son las del hueco que se desplazó"
    );

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(foto.focus, Some(1), "y el foco no se movió");
}

/// El relleno de un listado pinta en SU panel.
///
/// Los lotes que drenan por detrás se anunciaban siempre como filas del panel
/// activo, así que en una disposición de dos el segundo se quedaba con lo que
/// cupo en la primera página hasta que algo lo tocara.
#[tokio::test]
async fn el_relleno_de_un_panel_no_se_anuncia_en_el_otro() {
    let mut f = Falso::default();
    // Más entradas que la primera página (100), para que haya relleno.
    let muchas: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.pon("mem:///casa", muchas);
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 20,
    })
    .await
    .expect("host vivo");

    let mut slots = std::collections::BTreeSet::new();
    for _ in 0..40 {
        let Ok(Some(siguiente)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), sub.recv()).await
        else {
            break;
        };
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Rows { slot_id, .. } = c {
                    slots.insert(*slot_id);
                }
            }
        }
    }
    assert!(
        slots.contains(&2),
        "el segundo panel también recibe sus filas: {slots:?}"
    );
}

/// El tabulador cambia de panel, con el MISMO recorrido que el TUI: solo lo
/// que se ve y solo lo que se puede enfocar.
///
/// Sin esto, en una ventana de dos paneles el teclado no podía cambiar de
/// panel: había que usar el ratón, que es exactamente la clase de diferencia
/// entre frontends que la capa compartida existe para no tener.
#[tokio::test]
async fn el_tabulador_cambia_de_panel() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();

    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let l = siguiente_disposicion(&mut sub).await;
    let rol = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(rol(&l, 2), Some(SlotRole::Active), "el foco pasó al otro");

    // Y vuelve: el recorrido CICLA, no se queda en el último.
    h.dispatch(tecla("Tab")).await.expect("host vivo");
    let vuelta = siguiente_disposicion(&mut sub).await;
    assert_eq!(rol(&vuelta, 1), Some(SlotRole::Active));
}

/// El foco jamás aterriza en un hueco que no se enfoca (la barra de estado,
/// la franja de tareas): el recorrido es el de la capa compartida.
#[tokio::test]
async fn el_tabulador_no_enfoca_la_barra_de_estado() {
    let (h, _snap) = host_con_layout(arbol(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    for _ in 0..6 {
        h.dispatch(tecla("Tab")).await.expect("host vivo");
        let l = siguiente_disposicion(&mut sub).await;
        let activo = l
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        assert!(
            matches!(activo, Some(1 | 2)),
            "el foco solo pasa por los listados, no por {activo:?}"
        );
    }
}

/// Designar destino nunca se señala a uno mismo: un destino igual al panel
/// con el foco sería pedirle a una copia que se copie encima.
///
/// De paso, esto prueba el camino de una CAPA de usuario: el binding no está
/// en ningún preset (#228), así que la tecla la ata el keymap efectivo que
/// recibe el host — el mismo que construye un arranque de verdad.
#[tokio::test]
async fn el_destino_nunca_es_el_panel_enfocado() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};
    use norte_ui_host::dto::SlotRole;

    let preset = parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset de fábrica"),
    )
    .expect("preset parsea");
    let capa = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [{ on = ["ctrl+t"], run = "layout.set-target" }]
"#,
    )
    .expect("capa parsea");
    let keymap = Effective::build_for(
        &preset,
        &[capa],
        norte_ui_host::commands::IMPLEMENTADOS,
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
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
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

    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "t".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "la capa del usuario ata la tecla: {ack:?}"
    );
    let l = siguiente_disposicion(&mut sub).await;
    let rol_de = |r: SlotRole| {
        l.placements
            .iter()
            .find(|p| p.role == Some(r))
            .map(|p| p.slot_id)
    };
    assert_eq!(rol_de(SlotRole::Active), Some(1));
    assert_eq!(
        rol_de(SlotRole::Target),
        Some(2),
        "el destino es SIEMPRE otro hueco"
    );
}
