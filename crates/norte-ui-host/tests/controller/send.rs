use super::*;

// ---------------------------------------------------------------------------
// `pane.send-left`/`-right`, pressed through the KRUSADER preset's real
// chords: the location travels the way the arrow points, by screen position.
// The same cases as the TUI's `gestures::send_tests`, so the two cannot drift.
// ---------------------------------------------------------------------------

/// `n` listings side by side (ids in `ids`, left to right) or stacked.
fn listings(ids: &[u32], dir: &str) -> norte_frontend::layout::Node {
    let children: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"slot": {{"id": {id}, "kind": "browser"}}}}"#))
        .collect();
    let sizes = vec![r#"{"weight": 1}"#; ids.len()].join(", ");
    let json = format!(
        r#"{{"split": {{"dir": "{dir}", "children": [{}], "sizes": [{sizes}]}}}}"#,
        children.join(", ")
    );
    serde_json::from_str(&json).expect("tree")
}

async fn krusader(layout: norte_frontend::layout::Node) -> UiHost {
    UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("krusader").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("krusader").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("krusader").expect("preset"),
        layout,
        viewport: (160, 50),
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
    .0
}

/// Slot `slot` enters `/casa/docs`.
async fn enter_docs(h: &UiHost, sub: &mut norte_ui_host::UiSubscription, slot: u32) {
    let snap = lands(h, sub, slot, "/casa").await;
    let b = listing_of(&snap, slot);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs is listed");
    let (key, generation) = (docs.key, b.generation);
    h.dispatch(UiAction::Activate {
        slot_id: slot,
        key,
        generation,
    })
    .await
    .expect("host alive");
    wait_snapshot(h, sub, "the slot enters docs", |f| {
        listing_of(f, slot).path_display.ends_with("/casa/docs")
    })
    .await;
}

async fn focus(h: &UiHost, slot: u32) {
    h.dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host alive");
}

/// Krusader's `Ctrl+<arrow>`.
async fn ctrl(h: &UiHost, arrow: &str) -> ActionAck {
    h.dispatch(key_mod(arrow, true, false))
        .await
        .expect("host alive")
}

/// Slot `slot` reaches the directory ending in `suffix`.
async fn lands(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    slot: u32,
    suffix: &str,
) -> norte_ui_host::ViewSnapshot {
    wait_snapshot(h, sub, suffix, |f| {
        listing_of(f, slot).path_display.ends_with(suffix)
    })
    .await
}

/// With two panes, Ctrl+→ always lands in the RIGHT one and Ctrl+← in the
/// LEFT one, from either focus, and the focus never moves.
#[tokio::test]
async fn ctrl_arrows_follow_the_arrow_from_either_pane() {
    let h = krusader(listings(&[1, 2], "horizontal")).await;
    let mut sub = h.subscribe();

    // Focus right, →: nothing to its right, so it takes the left's place.
    enter_docs(&h, &mut sub, 2).await;
    focus(&h, 2).await;
    ctrl(&h, "ArrowRight").await;
    let f = lands(&h, &mut sub, 2, "/casa").await;
    assert!(listing_of(&f, 1).path_display.ends_with("/casa"));
    assert_eq!(f.focus, Some(2), "focus does not move");

    // Focus left, ←: the left takes the right's place.
    enter_docs(&h, &mut sub, 2).await;
    focus(&h, 1).await;
    ctrl(&h, "ArrowLeft").await;
    let f = lands(&h, &mut sub, 1, "/casa/docs").await;
    assert_eq!(f.focus, Some(1));

    // Focus left, →: the right one goes where the left is.
    h.dispatch(UiAction::Parent { slot_id: 1 })
        .await
        .expect("host alive");
    lands(&h, &mut sub, 1, "/casa").await;
    ctrl(&h, "ArrowRight").await;
    let f = lands(&h, &mut sub, 2, "/casa").await;
    assert_eq!(f.focus, Some(1));

    // Focus right, ←: the left one goes where the right is.
    enter_docs(&h, &mut sub, 2).await;
    focus(&h, 2).await;
    ctrl(&h, "ArrowLeft").await;
    let f = lands(&h, &mut sub, 1, "/casa/docs").await;
    assert_eq!(f.focus, Some(2));
}

/// The screen decides, not the slot id nor where a listing came from: with
/// slot 2 on the LEFT, and after `pane.swap` (Ctrl+U) moved the contents.
#[tokio::test]
async fn the_arrow_follows_the_screen_after_a_swap() {
    let h = krusader(listings(&[2, 1], "horizontal")).await;
    let mut sub = h.subscribe();
    enter_docs(&h, &mut sub, 1).await;
    focus(&h, 2).await;
    // Left (2) on /casa, right (1) on docs; the swap turns that around.
    h.dispatch(key_mod("u", true, false))
        .await
        .expect("host alive");
    lands(&h, &mut sub, 2, "/casa/docs").await;
    ctrl(&h, "ArrowRight").await;
    let f = lands(&h, &mut sub, 1, "/casa/docs").await;
    assert_eq!(f.focus, Some(2));
}

/// Three side by side: from the middle each arrow reaches its immediate
/// neighbour.
#[tokio::test]
async fn with_three_the_middle_one_sends_both_ways() {
    let h = krusader(listings(&[1, 2, 3], "horizontal")).await;
    let mut sub = h.subscribe();
    enter_docs(&h, &mut sub, 2).await;
    focus(&h, 2).await;
    ctrl(&h, "ArrowRight").await;
    let f = lands(&h, &mut sub, 3, "/casa/docs").await;
    assert!(
        listing_of(&f, 1).path_display.ends_with("/casa"),
        "← untouched"
    );
    ctrl(&h, "ArrowLeft").await;
    let f = lands(&h, &mut sub, 1, "/casa/docs").await;
    assert_eq!(f.focus, Some(2));
}

/// Stacked panes (what `layout.flip` leaves) have no side: it SAYS so, and
/// nothing moves.
#[tokio::test]
async fn stacked_panes_say_there_is_nothing_beside() {
    let h = krusader(listings(&[1, 2], "vertical")).await;
    let mut sub = h.subscribe();
    enter_docs(&h, &mut sub, 2).await;
    focus(&h, 1).await;
    let said = norte_i18n::t_in(norte_i18n::Lang::Es, "msg-pane-nothing-beside");
    for arrow in ["ArrowRight", "ArrowLeft"] {
        ctrl(&h, arrow).await;
        // Waited for, not read once: a snapshot already queued by the
        // navigation above would answer first.
        let f = wait_snapshot(&h, &mut sub, "the refusal is said", |f| {
            f.status.message.as_deref() == Some(said.as_str())
        })
        .await;
        assert!(listing_of(&f, 1).path_display.ends_with("/casa"), "{arrow}");
        assert!(
            listing_of(&f, 2).path_display.ends_with("/casa/docs"),
            "{arrow}"
        );
    }
}
