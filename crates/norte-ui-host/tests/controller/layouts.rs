use super::*;

// ---------------------------------------------------------------------------
// Layouts: resizing, equalizing and picking.
// ---------------------------------------------------------------------------

/// Runs a command through the PALETTE, which is another door to the same
/// catalogue.
///
/// None of the layout commands is bound by a factory preset, so this is the
/// path they arrive by today.
pub(super) async fn by_palette(h: &UiHost, sub: &mut norte_ui_host::UiSubscription, cmd: &str) {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let _ = next_palette(sub).await.expect("the palette opens");
    for c in cmd.chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
}

/// **With synchronized navigation on, both slots move together.**
///
/// And the echo does NOT enter the mirrored slot's trail: it travels as
/// `Trail::Seed`, which is what keeps its "back" from counting a step the
/// reader did not take there — and, along the way, what cuts the recursion.
#[tokio::test]
async fn with_synchronized_navigation_both_slots_move_together() {
    // `orthodox` and not the starting layout: TWO listings are needed,
    // because with no target slot there is nobody to mirror.
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();

    // Turning it on does NOT move anything: it lines up the next navigation,
    // not the current one.
    by_palette(&h, &mut sub, "sync-nav").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let before: Vec<String> = snap
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(b.path_display.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(before.len(), 2, "the starting layout is two listings");
    assert_eq!(before[0], before[1], "and they start in the same place");

    // Now a navigation of the active slot: it enters `docs`.
    h.dispatch(press("Enter")).await.expect("host alive");
    let after = snapshot_until(&h, &mut sub, "both listings moved", |snap| {
        let paths: Vec<String> = snap
            .slots
            .iter()
            .filter_map(|s| match s {
                SlotView::Browser(b) => Some(b.path_display.clone()),
                _ => None,
            })
            .collect();
        (paths.len() == 2 && paths[0] != before[0] && paths[1] != before[1]).then_some(paths)
    })
    .await;
    assert_eq!(
        after[0], after[1],
        "the target repeated the active slot's navigation: {after:?}"
    );
}

/// A slot's width in the snapshot.
pub(super) fn width_of(snap: &norte_ui_host::ViewSnapshot, slot: u32) -> u16 {
    snap.layout
        .placements
        .iter()
        .find(|p| p.slot_id == slot)
        .map_or(0, |p| p.width)
}

/// Growing widens the FOCUSED slot, and shrinking returns it.
#[tokio::test]
async fn growing_and_shrinking_move_the_focused_slot() {
    // A layout with TWO listings: resizing splits between siblings, and
    // with only one slot there is nobody to take from.
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let active = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("there is an active slot");
    let before = width_of(&snap, active);

    by_palette(&h, &mut sub, "layout.grow").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let grown = next_snapshot(&mut sub).await;
    assert!(
        width_of(&grown, active) > before,
        "it grew: {} → {}",
        before,
        width_of(&grown, active)
    );

    by_palette(&h, &mut sub, "layout.shrink").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let back = next_snapshot(&mut sub).await;
    assert_eq!(width_of(&back, active), before, "and shrinking returns it");
}

/// Equalizing leaves siblings with the same weight.
#[tokio::test]
async fn equalizing_splits_evenly() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let active = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
        .expect("there is an active slot");

    // It gets unbalanced and then equalized again.
    for _ in 0..3 {
        by_palette(&h, &mut sub, "layout.grow").await;
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let skewed = width_of(&next_snapshot(&mut sub).await, active);

    by_palette(&h, &mut sub, "layout.equalize").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let equalized = width_of(&next_snapshot(&mut sub).await, active);
    assert!(equalized < skewed, "equalizing undoes the imbalance");
}

/// The picker offers the five factory ones with their preview, and choosing
/// one CHANGES the screen.
#[tokio::test]
async fn picking_a_layout_changes_the_screen() {
    let (h, snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let slots_before = snap.slots.len();

    by_palette(&h, &mut sub, "layout.pick").await;
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        if let Some(l) = snap.layouts.clone() {
            v = Some(l);
            break;
        }
    }
    let picker = v.expect("the picker opens");
    assert_eq!(
        picker.rows.len(),
        norte_frontend::layout::presets::NAMES.len(),
        "the five factory ones, and none of the user's on this host"
    );
    assert!(picker.rows.iter().all(|r| r.factory));
    assert!(
        !picker.preview.is_empty(),
        "and the chosen one shows its SHAPE, painted by the same engine that splits"
    );
    let widths: std::collections::BTreeSet<usize> =
        picker.preview.iter().map(|l| l.chars().count()).collect();
    assert_eq!(widths.len(), 1, "the thumbnail is a rectangle: {widths:?}");

    // The `full` layout has more slots than `simple`.
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "full")
        .expect("`full` is there");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("fits"),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = next_snapshot(&mut sub).await;
    assert!(after.layouts.is_none(), "the picker closes on choosing");
    assert!(
        after.slots.len() > slots_before,
        "and the screen is different: {} → {}",
        slots_before,
        after.slots.len()
    );
    assert!(
        after.slots.iter().any(|s| matches!(s, SlotView::Places(_))),
        "with the side bar `full` places"
    );
}

/// A user layout that fails to parse is OFFERED, with no preview and saying
/// why, and choosing it does not change the screen.
#[tokio::test]
async fn a_broken_layout_shows_and_does_not_apply() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
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
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("mia"),
            tree: Err("no parsea".to_owned()),
        }],
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    by_palette(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(l) = next_snapshot(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("the picker opens");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "mia")
        .expect("the user's one is OFFERED even though it fails to parse");
    assert!(picker.rows[i].broken, "and it is said to be broken");
    assert!(!picker.rows[i].factory);

    let ack = h
        .dispatch(UiAction::LayoutActivateRow {
            row: u32::try_from(i).expect("fits"),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "choosing it does NOT change the screen over a broken file: {ack:?}"
    );
}

/// A layout whose SPLIT places no listing at all cannot leave the host with
/// no slots.
///
/// `validate` guarantees the tree HAS a `browser`, not that the split PLACES
/// it: a `Tabs` whose active one is another kind sends the listing to
/// `hidden`, and an all-weighted split that does not fit does the same to
/// all but child 0. Seeding the slots from `placements` instead of from the
/// tree emptied the map, and the next keystroke died in `slot()`'s
/// `expect` — inside the actor's task, with no log and no visible crash,
/// leaving the window dead answering `Down` forever. It is #242's shape on
/// this surface.
#[tokio::test]
async fn a_layout_that_hides_the_listing_leaves_the_slot_alive() {
    use norte_frontend::layout::{KindId, Node, SlotId};
    let hidden = Node::Tabs {
        children: vec![
            Node::slot(SlotId(9), KindId::new("metadata")),
            Node::slot(SlotId(1), KindId::browser()),
        ],
        active: 0,
    };
    norte_frontend::layout::validate(&hidden)
        .expect("the tree is VALID: it has a browser, even if the split does not place it");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
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
        user_layouts: vec![norte_frontend::layout_picker::UserLayout {
            name: std::ffi::OsString::from("escondida"),
            tree: Ok(hidden),
        }],
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        profile: None,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    by_palette(&h, &mut sub, "layout.pick").await;
    let mut picker = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(l) = next_snapshot(&mut sub).await.layouts.clone() {
            picker = Some(l);
            break;
        }
    }
    let picker = picker.expect("the picker opens");
    let i = picker
        .rows
        .iter()
        .position(|r| r.name == "escondida")
        .expect("the user's one is there");
    h.dispatch(UiAction::LayoutActivateRow {
        row: u32::try_from(i).expect("fits"),
    })
    .await
    .expect("host alive");

    // The keystroke that used to kill it: any one that touches the active
    // slot.
    h.dispatch(press("Down"))
        .await
        .expect("the host stays ALIVE after picking a layout that hides the listing");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = next_snapshot(&mut sub).await;
    assert!(
        after
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "and the screen is the one that was requested: the active tab is the sheet"
    );
}
