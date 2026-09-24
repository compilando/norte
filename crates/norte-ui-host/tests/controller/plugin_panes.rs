use super::*;

// ---------------------------------------------------------------------------
// The panel a PLUGIN paints (phase 3): what it is asked, when it is asked,
// and what is done with what it answers.
//
// What is NOT tested here, said so it does not read as an oversight: an
// expired witness and a slot that changes plugin mid-request cannot be
// triggered from outside — the controller's state is `pub(super)` — so that
// logic lives tested in the terminal (`panelplugin::adoptar` and its tests),
// where it is the same decision written once.
// ---------------------------------------------------------------------------

const PLUGIN: &str = "acme.git";
const KIND: &str = "plugin:acme.git:status";
const SLOT: u32 = 9;

/// A consented plugin that contributes the `status` panel.
fn extension_con_panel() -> norte_proto::methods::PluginInfo {
    let mut e = extension(PLUGIN, "Git de ACME", false);
    "panel".clone_into(&mut e.category);
    e.panels = vec![norte_proto::methods::PluginPanelInfo {
        kind: "status".to_owned(),
        title: "Git".to_owned(),
        min_cols: None,
        min_rows: None,
    }];
    e
}

/// Any frame from the guest, with a clickable zone.
fn frame(text: &str, command: &str) -> norte_proto::methods::PanelFrame {
    norte_proto::methods::PanelFrame {
        plugin_id: PLUGIN.to_owned(),
        lines: vec![vec![norte_proto::methods::SpanWire {
            text: text.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }]],
        hits: vec![norte_proto::methods::PanelHit {
            row: 0,
            col: 0,
            width: 8,
            command: command.to_owned(),
            arg: None,
        }],
        state: Some(b"opaco".to_vec()),
    }
}

/// A host with a listing and, next to it, the plugin panel's slot.
async fn host_con_panel(backend: Arc<Fake>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let tree = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(30)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT), KindId::new(KIND)),
        ],
    };
    norte_frontend::layout::validate(&tree).expect("the tree is valid");
    UiHost::start(UiHostOptions {
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
    .expect("starts")
}

/// The fake backend with the catalogue and the frame set.
fn backend_con(frame: Option<norte_proto::methods::PanelFrame>) -> Arc<Fake> {
    let base = tree_with_plugins(vec![extension_con_panel()], &[]);
    let mut f = Fake {
        plugins: vec![extension_con_panel()].into(),
        marco_de_panel: frame,
        ..Fake::default()
    };
    f.tree.clone_from(&base.tree);
    Arc::new(f)
}

/// The slot's panel, if the snapshot carries it with a frame.
fn panel_de(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::PanelSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Panel(p) if p.slot_id == SLOT => Some(&**p),
        _ => None,
    })
}

/// What the guest describes ends up in the snapshot, and with the panel's
/// title.
///
/// The slot is placed before the catalogue arrives, so the first snapshot
/// carries it with no frame: what this test pins down is that the SECOND
/// one, after the panel is declared and its frame has landed, brings it
/// painted.
#[tokio::test]
async fn the_guests_frame_reaches_the_snapshot() {
    let backend = backend_con(Some(frame("rama main", "layout.focus-next")));
    let (h, snap) = host_con_panel(Arc::clone(&backend)).await;
    assert!(
        panel_de(&snap).is_some_and(|p| p.lines.is_empty()),
        "the first snapshot carries the slot with no frame yet"
    );
    let mut sub = h.subscribe();
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let view = next_snapshot(&mut sub).await;
    let panel = panel_de(&view).expect("the slot is still a panel");
    assert_eq!(
        panel.title, "status",
        "the title is the kind, with no prefix"
    );
    let text: String = panel
        .lines
        .iter()
        .flat_map(|l| l.iter().map(|s| s.text.clone()))
        .collect();
    assert_eq!(text, "rama main");
    assert_eq!(panel.hits.len(), 1, "and its zone, with no command");
}

/// It is requested ONCE per signature: not once per actor message.
///
/// It is the failure the terminal had to fix — one RPC per painted frame —
/// and here the equivalent would be one per keystroke.
#[tokio::test]
async fn the_same_thing_is_not_requested_twice() {
    let backend = backend_con(Some(frame("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    for _ in 0..5 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        asentar().await;
    }
    let requested = backend.panels_pedidos.lock().expect("mutex").len();
    assert_eq!(requested, 1, "five messages, one request: {requested}");
}

/// And what the guest is told is what the reader is looking at: the
/// directory, the slot WITHOUT its frame, and the row under the cursor.
#[tokio::test]
async fn the_guest_is_told_where_the_reader_is() {
    let backend = backend_con(Some(frame("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    asentar().await;

    let requested = backend.panels_pedidos.lock().expect("mutex");
    let p = requested.first().expect("the frame was requested");
    assert_eq!(p.plugin_id, PLUGIN);
    assert_eq!(p.kind, "status", "the plugin's kind, with no prefix");
    assert_eq!(p.dir, dir(), "the directory the listing shows");
    assert_eq!(p.cols, 28, "thirty cells minus the two borders");
    assert!(p.cursor_name.is_some(), "and the row under the cursor");
}

/// With no plugin to paint it, the slot stays with no frame and is NOT
/// re-requested.
///
/// It happens with nothing hostile involved: a saved layout that names a
/// panel whose plugin got disabled. Repeating it per message would be one
/// RPC per keystroke.
#[tokio::test]
async fn a_panel_with_no_frame_is_not_re_requested() {
    let backend = backend_con(None);
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    for _ in 0..4 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        asentar().await;
    }
    let requested = backend.panels_pedidos.lock().expect("mutex").len();
    assert_eq!(requested, 1, "it was tried once and recorded: {requested}");
}

/// A frame signed by ANOTHER plugin does not get painted.
#[tokio::test]
async fn a_frame_from_another_plugin_does_not_paint() {
    let mut foreign = frame("soy otro", "layout.focus-next");
    foreign.plugin_id = "evil.thing".to_owned();
    let backend = backend_con(Some(foreign));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let view = next_snapshot(&mut sub).await;
    assert!(
        panel_de(&view).is_some_and(|p| p.lines.is_empty()),
        "the slot is still without a frame"
    );
}

/// Clicking a zone whose command is OUTSIDE its scope runs nothing.
///
/// The plugin chooses the label and the command, and nothing ties them
/// together: a zone that says "Update" can name something that copies
/// files. Consent was for painting.
#[tokio::test]
async fn a_zone_outside_its_scope_runs_nothing() {
    let backend = backend_con(Some(frame("Actualizar", "pane.unpack")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    asentar().await;

    let ack = h
        .dispatch(UiAction::PanelClick {
            slot_id: SLOT,
            row: 0,
            col: 1,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "not an error for the reader, it simply does nothing: {ack:?}"
    );
    assert!(
        backend.ejecutados.lock().expect("ejecutados").is_empty(),
        "and it certainly does not run"
    );
}

/// A cell with no zone also does nothing, and it is not an error.
#[tokio::test]
async fn a_cell_with_no_zone_does_nothing() {
    let backend = backend_con(Some(frame("rama main", "layout.focus-next")));
    let (h, _snap) = host_con_panel(Arc::clone(&backend)).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    asentar().await;

    let ack = h
        .dispatch(UiAction::PanelClick {
            slot_id: SLOT,
            row: 9,
            col: 40,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
}
