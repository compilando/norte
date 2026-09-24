use super::*;

// ---------------------------------------------------------------------------
// The journal's timeline in the window (#359, phase 7).
// ---------------------------------------------------------------------------

/// The timeline's slot, next to the listing.
const SLOT_LINEA: u32 = 8;

/// A HUMAN journal row, reversible.
fn row(seq: i64, path: &str) -> norte_proto::methods::JournalRow {
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

/// A double with three human mutations, newest to oldest.
fn con_historial() -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.journal = Some(vec![
        row(3, "/casa/c.txt"),
        row(2, "/casa/b.txt"),
        row(1, "/casa/a.txt"),
    ]);
    Arc::new(f)
}

/// A host with the listing and, next to it, the timeline.
async fn host_con_linea(backend: Arc<Falso>) -> UiHost {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let tree = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(40)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT_LINEA), KindId::new("timeline")),
        ],
    };
    norte_frontend::layout::validate(&tree).expect("the tree is valid");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: tree,
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
    .expect("starts");
    h
}

/// The slot's timeline, if the snapshot carries it.
fn linea_de(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::TimelineSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Timeline(t) if t.slot_id == SLOT_LINEA => Some(&**t),
        _ => None,
    })
}

/// The first snapshot that satisfies `cond`, requesting snapshots until it
/// arrives.
///
/// "The next snapshot" does not work: a page that lands sends its own, so
/// the queue can hold snapshots from BEFORE the last action.
async fn snapshot_where(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    expected: &str,
    cond: impl Fn(&norte_ui_host::ViewSnapshot) -> bool,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(sub).await;
        if cond(&snap) {
            return snap;
        }
        asentar().await;
    }
    panic!("a snapshot with {expected} never arrived");
}

/// A snapshot in which the timeline already has rows.
async fn snapshot_with_rows(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    snapshot_where(h, sub, "rows in the timeline", |f| {
        linea_de(f).is_some_and(|l| !l.rows.is_empty())
    })
    .await
}

/// When its slot appears, the timeline requests its first page and paints
/// it: one row per mutation, newest to oldest, with the cursor at the top.
#[tokio::test]
async fn the_timeline_requests_its_first_page_and_paints_it() {
    let h = host_con_linea(con_historial()).await;
    let mut sub = h.subscribe();
    let snap = snapshot_with_rows(&h, &mut sub).await;
    let l = linea_de(&snap).expect("there is a timeline");
    assert_eq!(
        l.rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["/casa/c.txt", "/casa/b.txt", "/casa/a.txt"]
    );
    assert_eq!(l.cursor, Some(0), "the cursor starts on the newest");
    assert_eq!(
        l.footer, "no hay nada tuyo que deshacer por encima de esa fila",
        "with the cursor at the top, an Enter would undo nothing — and it says so"
    );
}

/// Down, `Enter`: it asks with the COUNT; confirming sends the cut at the
/// pointed-to row, which stays.
#[tokio::test]
async fn enter_asks_with_the_count_and_confirming_sends_the_cut() {
    let backend = con_historial();
    let h = host_con_linea(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = snapshot_with_rows(&h, &mut sub).await;

    h.dispatch(UiAction::FocusSlot {
        slot_id: SLOT_LINEA,
    })
    .await
    .expect("host alive");
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    // The arrow moves through the timeline, not the listing.
    let snap = snapshot_where(&h, &mut sub, "the timeline's cursor on row 2", |f| {
        linea_de(f).is_some_and(|l| l.cursor == Some(1))
    })
    .await;
    assert_eq!(
        linea_de(&snap).expect("there is a timeline").footer,
        "1 entradas se deshacen"
    );

    h.dispatch(tecla("Enter")).await.expect("host alive");
    let snap = snapshot_where(&h, &mut sub, "the question", |f| !f.dialogs.is_empty()).await;
    let d = snap.dialogs.last().expect("asks before undoing");
    assert_eq!(d.title_key, "timeline-undo-title");
    assert!(
        d.body.iter().any(|l| l.text == "1 entradas se deshacen"),
        "the body says how many: {:?}",
        d.body
    );
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let cuts = backend
        .hasta("the cut that was sent", |f| {
            let c = f.deshechos_hasta.lock().expect("cortes").clone();
            (!c.is_empty()).then_some(c)
        })
        .await;
    assert_eq!(
        cuts,
        vec![(2, Some(3))],
        "the cut is the pointed-to row, which stays; the ceiling is the \
         newest thing the count counted"
    );
}

/// A daemon with no journal SAYS so: an empty panel with no explanation
/// reads as "you have done nothing".
#[tokio::test]
async fn a_daemon_with_no_journal_says_so() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let h = host_con_linea(Arc::new(f)).await;
    let mut sub = h.subscribe();
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        if let Some(l) = linea_de(&snap)
            && l.empty == "este daemon no guarda historial"
        {
            assert!(l.rows.is_empty());
            return;
        }
        asentar().await;
    }
    panic!("the timeline never said there is no history");
}
