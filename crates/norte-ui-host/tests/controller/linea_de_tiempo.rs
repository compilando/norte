use super::*;

// ---------------------------------------------------------------------------
// La línea de tiempo del journal en la ventana (#359, fase 7).
// ---------------------------------------------------------------------------

/// El hueco de la línea de tiempo, al lado del listado.
const SLOT_LINEA: u32 = 8;

/// Una fila del journal del HUMANO, reversible.
fn fila(seq: i64, path: &str) -> norte_proto::methods::JournalRow {
    norte_proto::methods::JournalRow {
        seq,
        ts_ms: 1_700_000_000_000 + seq * 1000,
        actor_kind: "user".to_owned(),
        actor_id: None,
        op: "renamed".to_owned(),
        path: path.to_owned(),
        path_to: None,
        hostile: false,
        reversible: true,
        undoes_seq: None,
        undone: false,
        batch_id: None,
    }
}

/// Un doble con tres mutaciones del humano, de la más nueva a la más vieja.
fn con_historial() -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.journal = Some(vec![
        fila(3, "/casa/c.txt"),
        fila(2, "/casa/b.txt"),
        fila(1, "/casa/a.txt"),
    ]);
    Arc::new(f)
}

/// Un host con el listado y, al lado, la línea de tiempo.
async fn host_con_linea(backend: Arc<Falso>) -> UiHost {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let arbol = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(40)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT_LINEA), KindId::new("timeline")),
        ],
    };
    norte_frontend::layout::validate(&arbol).expect("el árbol es válido");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: arbol,
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
    h
}

/// La línea del hueco, si la foto la trae.
fn linea_de(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::TimelineSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Timeline(t) if t.slot_id == SLOT_LINEA => Some(&**t),
        _ => None,
    })
}

/// La primera foto que cumple `cond`, pidiendo fotos hasta que llegue.
///
/// No vale «la siguiente foto»: una página que aterriza manda la suya, así
/// que en la cola puede haber fotos de ANTES de la última acción.
async fn foto_que(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    que_esperaba: &str,
    cond: impl Fn(&norte_ui_host::ViewSnapshot) -> bool,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(sub).await;
        if cond(&foto) {
            return foto;
        }
        asentar().await;
    }
    panic!("nunca llegó una foto con {que_esperaba}");
}

/// Una foto en la que la línea ya tiene filas.
async fn foto_con_filas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    foto_que(h, sub, "filas en la línea", |f| {
        linea_de(f).is_some_and(|l| !l.rows.is_empty())
    })
    .await
}

/// Al aparecer su hueco, la línea pide su primera página y la pinta: una
/// fila por mutación, de la más nueva a la más vieja, con el cursor arriba.
#[tokio::test]
async fn la_linea_pide_su_primera_pagina_y_la_pinta() {
    let h = host_con_linea(con_historial()).await;
    let mut sub = h.subscribe();
    let foto = foto_con_filas(&h, &mut sub).await;
    let l = linea_de(&foto).expect("hay línea");
    assert_eq!(
        l.rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["/casa/c.txt", "/casa/b.txt", "/casa/a.txt"]
    );
    assert_eq!(l.cursor, Some(0), "el cursor empieza en lo más nuevo");
    assert_eq!(
        l.footer, "no hay nada tuyo que deshacer por encima de esa fila",
        "con el cursor arriba, un Enter no se llevaría nada — y se dice"
    );
}

/// Bajar, `Enter`: pregunta con el RECUENTO; confirmar manda el corte de la
/// fila señalada, que se queda.
#[tokio::test]
async fn enter_pregunta_con_el_recuento_y_confirmar_manda_el_corte() {
    let backend = con_historial();
    let h = host_con_linea(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = foto_con_filas(&h, &mut sub).await;

    h.dispatch(UiAction::FocusSlot {
        slot_id: SLOT_LINEA,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    // La flecha anda por la línea, no por el listado.
    let foto = foto_que(&h, &mut sub, "el cursor de la línea en la 2ª fila", |f| {
        linea_de(f).is_some_and(|l| l.cursor == Some(1))
    })
    .await;
    assert_eq!(
        linea_de(&foto).expect("hay línea").footer,
        "1 entradas se deshacen"
    );

    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let foto = foto_que(&h, &mut sub, "la pregunta", |f| !f.dialogs.is_empty()).await;
    let d = foto.dialogs.last().expect("pregunta antes de deshacer");
    assert_eq!(d.title_key, "timeline-undo-title");
    assert!(
        d.body.iter().any(|l| l.text == "1 entradas se deshacen"),
        "el cuerpo dice cuánto: {:?}",
        d.body
    );
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let cortes = backend
        .hasta("el corte que se mandó", |f| {
            let c = f.deshechos_hasta.lock().expect("cortes").clone();
            (!c.is_empty()).then_some(c)
        })
        .await;
    assert_eq!(
        cortes,
        vec![(2, Some(3))],
        "el corte es la fila señalada, que se queda; el techo, lo más nuevo \
         que el recuento contó"
    );
}

/// Un daemon sin journal lo DICE: un panel vacío sin explicación se lee como
/// «no has hecho nada».
#[tokio::test]
async fn un_daemon_sin_journal_lo_dice() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let h = host_con_linea(Arc::new(f)).await;
    let mut sub = h.subscribe();
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        if let Some(l) = linea_de(&foto)
            && l.empty == "este daemon no guarda historial"
        {
            assert!(l.rows.is_empty());
            return;
        }
        asentar().await;
    }
    panic!("la línea nunca dijo que no hay historial");
}
