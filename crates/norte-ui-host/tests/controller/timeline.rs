use super::*;

// ---------------------------------------------------------------------------
// The journal's timeline in the window (#359, phase 7).
// ---------------------------------------------------------------------------

/// The timeline's slot, next to the listing.
const SLOT_LINE: u32 = 8;

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
fn with_history() -> Arc<Fake> {
    let mut f = Fake::default();
    f.put(
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
async fn host_with_timeline(backend: Arc<Fake>) -> UiHost {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let tree = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(40)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT_LINE), KindId::new("timeline")),
        ],
    };
    norte_frontend::layout::validate(&tree).expect("the tree is valid");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: tree,
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");
    h
}

/// The slot's timeline, if the snapshot carries it.
fn line_of(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::TimelineSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Timeline(t) if t.slot_id == SLOT_LINE => Some(&**t),
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
        let snap = next_snapshot(sub).await;
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
        line_of(f).is_some_and(|l| !l.rows.is_empty())
    })
    .await
}

/// When its slot appears, the timeline requests its first page and paints
/// it: one row per mutation, newest to oldest, with the cursor at the top.
#[tokio::test]
async fn the_timeline_requests_its_first_page_and_paints_it() {
    let h = host_with_timeline(with_history()).await;
    let mut sub = h.subscribe();
    let snap = snapshot_with_rows(&h, &mut sub).await;
    let l = line_of(&snap).expect("there is a timeline");
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
    let backend = with_history();
    let h = host_with_timeline(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = snapshot_with_rows(&h, &mut sub).await;

    h.dispatch(UiAction::FocusSlot { slot_id: SLOT_LINE })
        .await
        .expect("host alive");
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    // The arrow moves through the timeline, not the listing.
    let snap = snapshot_where(&h, &mut sub, "the timeline's cursor on row 2", |f| {
        line_of(f).is_some_and(|l| l.cursor == Some(1))
    })
    .await;
    assert_eq!(
        line_of(&snap).expect("there is a timeline").footer,
        "1 entradas se deshacen"
    );

    h.dispatch(press("Enter")).await.expect("host alive");
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
        .until("the cut that was sent", |f| {
            let c = f.deshechos_until.lock().expect("cortes").clone();
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
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let h = host_with_timeline(Arc::new(f)).await;
    let mut sub = h.subscribe();
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        if let Some(l) = line_of(&snap)
            && l.empty == "este daemon no guarda historial"
        {
            assert!(l.rows.is_empty());
            return;
        }
        asentar().await;
    }
    panic!("the timeline never said there is no history");
}
