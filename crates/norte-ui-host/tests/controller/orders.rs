use super::*;

// ---------------------------------------------------------------------------
// The `:` box in the window (ADR 0185): the same steps as the terminal.
// ---------------------------------------------------------------------------

fn ctrl_g() -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "g".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    })
}

async fn typed(h: &UiHost, text: &str) {
    for c in text.chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
}

fn marked(b: &norte_ui_host::dto::BrowserSlotView) -> Vec<&str> {
    b.rows
        .iter()
        .filter(|r| r.marked)
        .map(|r| r.display_name.as_str())
        .collect()
}

async fn resync(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host alive");
    next_snapshot(sub).await
}

/// Typing `:sel` marks the rows while the reader types, and the box says how
/// many, in the order line the renderer paints.
#[tokio::test]
async fn typing_sel_lights_the_rows_before_enter() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *.txt").await;
    let snap = resync(&h, &mut sub).await;
    assert_eq!(marked(listing(&snap)), ["notas.txt"]);
    let v = snap.goto.expect("open");
    assert_eq!(v.mode, norte_ui_host::dto::GotoModeView::Order);
    let line = v.order.expect("an order line");
    assert!(line.error.is_empty());
    assert!(line.preview.contains('1'), "{}", line.preview);
    assert!(line.tokens.iter().any(|t| t.class == "verb"));
}

/// Esc gives back the marks the pane had before the box — here, none.
#[tokio::test]
async fn esc_restores_the_marks_from_before_the_box() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *").await;
    h.dispatch(press("Escape")).await.expect("host alive");
    let snap = resync(&h, &mut sub).await;
    assert!(snap.goto.is_none());
    assert!(
        marked(listing(&snap)).is_empty(),
        "no marks before, none after"
    );
}

/// A line that does not parse keeps the box open and says why.
#[tokio::test]
async fn an_invalid_order_keeps_the_box_open() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel size>12Q ").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let snap = resync(&h, &mut sub).await;
    let line = snap.goto.expect("still open").order.expect("order line");
    assert!(!line.error.is_empty());
}

/// Enter on a copy goes through F5's door: the confirmation opens, with the
/// matched rows as its operands.
#[tokio::test]
async fn enter_opens_the_f5_confirmation_for_the_matched_rows() {
    let (h, _snap) = super::transfers::two_panes_with_separate_destination(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *.txt | cp >other").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let snap = resync(&h, &mut sub).await;
    assert!(snap.goto.is_none());
    assert!(!snap.dialogs.is_empty(), "the copy asks first");
}

/// `sel` matching nothing runs nothing: the cursor row is not copied.
#[tokio::test]
async fn zero_matches_copies_nothing() {
    let (h, _snap) = super::transfers::two_panes_with_separate_destination(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *.pdf | cp >other").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let snap = resync(&h, &mut sub).await;
    assert!(snap.dialogs.is_empty(), "no dialog over the cursor row");
}

/// A lazy listing (#52) carries no sizes: Enter measures the rows `size`
/// needs before it selects, and only then marks.
#[tokio::test]
async fn enter_measures_a_lazy_listing_before_selecting_by_size() {
    let mut f = Fake {
        lazy: true,
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel size=1 type:file").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let snap = wait_snapshot(&h, &mut sub, "both measured and marked", |s| {
        marked(listing(s)).len() == 2
    })
    .await;
    assert_eq!(marked(listing(&snap)), ["a.txt", "b.txt"]);
}

async fn host_read_only(backend: Arc<Fake>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::SoloRead,
        log_ring: None,
    })
    .await
    .expect("starts")
}

/// A read-only window refuses an order that writes, whatever door it would
/// take: `mkdir` straight to its task, or a catalogue command whose key the
/// read-only keymap would never have offered.
#[tokio::test]
async fn a_read_only_window_refuses_orders_that_write() {
    let fake = fake_tree();
    let (h, _snap) = host_read_only(Arc::clone(&fake)).await;
    let mut sub = h.subscribe();
    for line in [":mkdir x", ":pane.combine-files", ":sel *.txt | trash"] {
        h.dispatch(ctrl_g()).await.expect("host alive");
        typed(&h, line).await;
        h.dispatch(press("Enter")).await.expect("host alive");
        let snap = resync(&h, &mut sub).await;
        assert!(snap.dialogs.is_empty(), "{line}: no dialog in read-only");
    }
    assert!(
        fake.created.lock().expect("lock").is_empty(),
        "nothing created"
    );
}

/// Stats that land after the reader opened the box again are dropped: the
/// order does not select nor open a dialog over what they are doing now.
#[tokio::test]
async fn measurements_that_land_late_run_nothing() {
    let mut f = Fake {
        lazy: true,
        delay_ms: 300,
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let (h, _snap) = host_tree(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel size=1 | trash").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    h.dispatch(ctrl_g()).await.expect("host alive");
    let snap = wait_snapshot(&h, &mut sub, "the stale order is reported", |s| {
        s.status
            .message
            .as_deref()
            .is_some_and(|m| m.contains("no se ejecutó"))
    })
    .await;
    assert!(snap.dialogs.is_empty(), "no dialog over the reopened box");
    assert!(marked(listing(&snap)).is_empty());
}

/// An order that matched nothing gives the pane's marks back.
#[tokio::test]
async fn an_empty_order_gives_the_marks_back() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *.txt").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel *.zzz | trash").await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let snap = resync(&h, &mut sub).await;
    assert_eq!(
        marked(listing(&snap)),
        ["notas.txt"],
        "the marks from before"
    );
}

/// The window and the terminal mark the same rows for the same line: both
/// go through `PaneState::select_query`, which is the oracle here.
#[tokio::test]
async fn same_order_same_marks_as_the_shared_model() {
    let (h, snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let names: Vec<String> = listing(&snap)
        .rows
        .iter()
        .map(|r| r.display_name.clone())
        .collect();
    h.dispatch(ctrl_g()).await.expect("host alive");
    typed(&h, ":sel !*.txt type:file").await;
    let snap = resync(&h, &mut sub).await;
    let window: Vec<&str> = marked(listing(&snap));
    assert!(names.iter().any(|n| n.starts_with("caf")));
    assert_eq!(window.len(), 1, "only the non-UTF-8 file: {window:?}");
    assert!(window[0].starts_with("caf"));
}
