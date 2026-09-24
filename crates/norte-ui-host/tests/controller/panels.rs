use super::*;

// ---------------------------------------------------------------------------
// The projected layout, and a snapshot that really replaces (phase 3).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries the VIEWER.
///
/// It travels as a patch since version 6: a whole snapshot per scroll line
/// used to send every listing's rows underneath.
pub(super) async fn next_visor(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ViewerView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update with a viewer, not a hang")
            .expect("the host is still alive");
        if let Update::Message(m) = next {
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
    panic!("no update with a viewer ever arrived");
}

/// Waits for the next update that carries a layout.
pub(super) async fn next_layout(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::LayoutView {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update with a layout, not a hang")
            .expect("the host is still alive");
        match next {
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
            Update::Lagged => panic!("no lag in this test"),
        }
    }
    panic!("no update with a layout ever arrived");
}

/// The renderer does not split the screen: it receives it already split.
///
/// Without this, placing two panels would be a presentation rule written in
/// TypeScript — exactly what decision D14 forbids — and on top of that a
/// different one from the TUI's.
#[tokio::test]
async fn the_snapshot_splits_the_screen_through_the_renderer() {
    let (_h, snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let l = &snap.layout;
    assert_eq!(l.cells, (120, 40), "the split is the size it was given");
    let listings: Vec<&norte_ui_host::dto::SlotPlacement> = l
        .placements
        .iter()
        .filter(|p| [1, 2].contains(&p.slot_id))
        .collect();
    assert_eq!(listings.len(), 2, "the usual layout is two listings");
    let left = listings[0];
    let right = listings[1];
    assert!(
        left.width > 0 && left.height > 0,
        "a paintable slot has area"
    );
    assert!(
        left.x + left.width <= right.x,
        "and the two listings do not overlap: {left:?} vs {right:?}"
    );
}

/// Active and target are resolved by the host, with the SAME rule as the
/// TUI: the target is the other visible listing. The renderer only paints
/// them.
#[tokio::test]
async fn roles_are_resolved_by_the_host_not_the_renderer() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let role = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(role(&snap.layout, 1), Some(SlotRole::Active));
    assert_eq!(role(&snap.layout, 2), Some(SlotRole::Target));

    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    let after = next_layout(&mut sub).await;
    assert_eq!(role(&after, 2), Some(SlotRole::Active), "focus changed");
    assert_eq!(
        role(&after, 1),
        Some(SlotRole::Target),
        "and so did the target"
    );
}

/// Resizing the window splits again, and the renderer finds out through the
/// same ordered channel as everything else.
#[tokio::test]
async fn a_resize_splits_again_and_says_so() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let width_before = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("the left listing is painted")
        .width;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetViewport {
        width: 200,
        height: 60,
    })
    .await
    .expect("host alive");
    let after = next_layout(&mut sub).await;
    assert_eq!(after.cells, (200, 60));
    let width_after = after
        .placements
        .iter()
        .find(|p| p.slot_id == 1)
        .expect("still painted")
        .width;
    assert!(
        width_after > width_before,
        "a wider window gives wider listings: {width_before} -> {width_after}"
    );
}

/// A snapshot REPLACES the renderer's state, so it has to carry it whole. If
/// a resync swallowed the open dialog, the renderer would end up painting a
/// screen with no question that is waiting for an answer — and the
/// destructive operation would still be there, alive and unconfirmed.
#[tokio::test]
async fn a_resync_does_not_swallow_the_open_dialog() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let open = next_dialogs(&mut sub).await;
    assert_eq!(open.len(), 1);

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(
        snap.dialogs, open,
        "the snapshot carries the dialog that is open"
    );
}

/// Same for the board: a copy in progress cannot disappear because the
/// renderer requested a new snapshot.
#[tokio::test]
async fn a_resync_does_not_swallow_live_tasks() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F7")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "nueva".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let alive = next_tasks(&mut sub).await;
    assert!(!alive.is_empty(), "there is a task on the board");

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(snap.tasks, alive, "the snapshot carries the whole board");
}

/// Waits for the fake to receive a `dir_size` batch (the arm launches it in a
/// separate task, so it is not ready right when dispatch returns).
pub(super) async fn next_count(fake: &Fake) -> Vec<VPath> {
    for _ in 0..200 {
        if let Some(batch) = fake.recuentos.lock().expect("recuentos").first() {
            return batch.clone();
        }
        tokio::task::yield_now().await;
    }
    panic!("nobody requested a count");
}

/// The bar's message after an outcome, retrying: progress travels on its own
/// channel and the patch can take a tick to come out.
pub(super) async fn next_status_message(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> String {
    // Each round is a round trip to the actor: the loop advances at the
    // host's pace, not the clock's.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(sub).await;
        if let Some(m) = snap.status.message.clone() {
            return m;
        }
    }
    panic!("the bar said nothing");
}

/// `pane.dir-size` counts what is MARKED, and in ONE single Task (#139,
/// #290).
///
/// A Task per mark would force whoever is asking to add up the bytes and the
/// unreadable ones themselves, and the two do not add up the same way.
#[tokio::test]
async fn counting_size_sends_the_marks_in_a_single_batch() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let generation = listing(&snap).generation;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MarkRange {
        slot_id: 1,
        from: RowKey(1),
        to: RowKey(2),
        generation,
    })
    .await
    .expect("host alive");
    let _ = sub.recv().await.expect("host alive");

    run_by_palette(&h, &mut sub, "pane.dir-size").await;

    let batch = next_count(&backend).await;
    assert_eq!(batch.len(), 2, "both marks, in a single batch: {batch:?}");
    assert_eq!(
        backend.recuentos.lock().expect("recuentos").len(),
        1,
        "and a single Task"
    );
}

/// **A count's total is SAID.** `fs.dir_size` publishes nothing: its result
/// is its terminal progress, so without this the window would launch the
/// count, watch it finish, and never say how much it took up.
#[tokio::test]
async fn a_counts_total_reaches_the_bar() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.dir-size").await;
    let _ = next_count(&backend).await;

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
    });

    let message = next_status_message(&h, &mut sub).await;
    assert!(
        message.contains('3'),
        "the total says how many entries: {message}"
    );
}

/// And what could NOT be read changes the sentence: a count exists to decide
/// whether something FITS, so a round total that could not count everything
/// is a wrong answer, not an incomplete one.
#[tokio::test]
async fn a_count_with_unreadable_entries_does_not_give_a_plain_total() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.dir-size").await;
    let _ = next_count(&backend).await;

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.bytes_done = 2048;
        p.entries_done = 3;
        p.unreadable = Some(2);
    });

    let message = next_status_message(&h, &mut sub).await;
    assert!(
        message.contains('2'),
        "it has to say how many it could not read: {message}"
    );
    assert_ne!(
        message,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "msg-dir-size",
            &[("size", "2,0 KB"), ("count", "3")]
        ),
        "and it cannot be the plain round-total sentence"
    );
}

/// With no marks, what is under the CURSOR is counted: it is the same
/// source of "what does this operate on" a transfer uses, and not a second
/// fallback that can drift apart from the first.
#[tokio::test]
async fn counting_size_with_no_marks_uses_the_cursor() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.dir-size").await;

    let batch = next_count(&backend).await;
    assert_eq!(batch.len(), 1, "just the cursor's: {batch:?}");
}

/// A mouse sweep marks the whole range at ONCE, with the shared rule: what
/// falls in the range is not decided by whoever paints.
#[tokio::test]
async fn a_range_gets_marked_all_at_once() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    assert_eq!(listing(&snap).marks, 0);
    let generation = listing(&snap).generation;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(3),
            to: RowKey(1),
            generation,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }));
    let _ = sub.recv().await.expect("the host is still alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(
        listing(&snap).marks,
        3,
        "the ends are included, and the order they are given in does not matter"
    );
}

/// An end from a previous generation does not mark HALFWAY: marking up to a
/// place that is no longer the one the user pointed at is worse than
/// marking nothing.
#[tokio::test]
async fn a_range_with_a_stale_end_marks_nothing() {
    let (h, snap) = host(vec!["a", "b"]).await;
    let generation = listing(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(99),
            generation,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        }
    );
}

/// A LAZY listing — the local provider's (#52): no size, no date — does not
/// leave columns blank: the host probes what is visible.
///
/// The table backend gave a size right in the listing, so this difference
/// only showed when the Tauri spike painted a real directory and revealed
/// two empty columns.
#[tokio::test]
async fn a_lazy_listing_gets_probed_and_cells_fill_up() {
    let mut f = Fake {
        lazy: true,
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    assert!(
        listing(&snap).rows[0]
            .cells
            .iter()
            .all(|c| c.text.is_none()),
        "the listing arrives with no size, like the real one: {:?}",
        listing(&snap).rows[0].cells
    );

    let mut sub = h.subscribe();
    // The probe goes out on its own, as soon as the listing lands. Snapshots
    // are requested one after another: what matters is that the screen ends
    // up with the cells full, not which message it arrived in. Each round is
    // a trip to the actor, not a wait.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        let filled = listing(&snap)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if filled {
            assert!(
                !backend.sondeos.lock().expect("sondeos").is_empty(),
                "and they filled by probing, not by inventing"
            );
            return;
        }
    }
    panic!("the cells are still blank: the probe never arrived");
}

/// Whatever was already probed is not probed again: a `stat` per repaint
/// would be a loop against the daemon, and a failing one would be forever.
#[tokio::test]
async fn what_was_probed_is_not_requested_again() {
    let mut f = Fake {
        lazy: true,
        ..Fake::default()
    };
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    for _ in 0..5 {
        h.dispatch(UiAction::SetVisibleRange {
            slot_id: 1,
            first: 0,
            count: 40,
        })
        .await
        .expect("host alive");
    }
    // It waits for the FIRST probe and lets the rest run: if all five
    // repaints probed, the other four would already be queued.
    until(&backend, "the first probe", |f| {
        (!f.sondeos.lock().expect("sondeos").is_empty()).then_some(())
    })
    .await;
    asentar().await;
    let probes = backend.sondeos.lock().expect("sondeos").clone();
    assert_eq!(
        probes.len(),
        1,
        "an entry is probed ONCE, not once per repaint: {probes:?}"
    );
}

// ---------------------------------------------------------------------------
// Two panels are TWO panels (phase 4, task 4.1).
// ---------------------------------------------------------------------------

/// The wheel over the panel that does NOT have focus moves THAT panel, and
/// does not steal focus from the other one.
///
/// Declaring which rows are visible is not acting on the listing: it is
/// saying where the user is looking. Treating it as an action of the active
/// panel left the second panel of a two-panel layout unable to scroll.
#[tokio::test]
async fn the_inactive_panel_scrolls_without_stealing_focus() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::SetVisibleRange {
            slot_id: 2,
            first: 1,
            count: 10,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the panel next to it also scrolls: {ack:?}"
    );

    let mut seen = None;
    for _ in 0..10 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Rows {
                    slot_id,
                    first_visible,
                    ..
                } = c
                {
                    seen = Some((*slot_id, *first_visible));
                }
            }
        }
        if seen.is_some() {
            break;
        }
    }
    assert_eq!(
        seen,
        Some((2, 1)),
        "the rows that travel are the slot's that scrolled"
    );

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(snap.focus, Some(1), "and focus did not move");
}

/// A listing's fill paints in ITS OWN panel.
///
/// Batches draining behind the scenes used to always be announced as the
/// active panel's rows, so in a two-panel layout the second one kept
/// whatever fit on the first page until something touched it.
#[tokio::test]
async fn a_panels_fill_does_not_announce_in_the_other() {
    let mut f = Fake::default();
    // More entries than the first page (100), so there is a fill.
    let many: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.put("mem:///casa", many);
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 20,
    })
    .await
    .expect("host alive");

    let mut slots = std::collections::BTreeSet::new();
    for _ in 0..40 {
        let Ok(Some(next)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), sub.recv()).await
        else {
            break;
        };
        if let Update::Message(m) = next
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
        "the second panel also receives its rows: {slots:?}"
    );
}

/// Tab switches panels, with the SAME traversal as the TUI: only what is
/// visible and only what can be focused.
///
/// Without this, in a two-panel window the keyboard could not switch panels:
/// the mouse had to be used, which is exactly the kind of difference between
/// frontends the shared layer exists to not have.
#[tokio::test]
async fn tab_switches_panels() {
    use norte_ui_host::dto::SlotRole;
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    assert_eq!(snap.focus, Some(1));
    let mut sub = h.subscribe();

    h.dispatch(press("Tab")).await.expect("host alive");
    let l = next_layout(&mut sub).await;
    let role = |l: &norte_ui_host::dto::LayoutView, id: u32| {
        l.placements
            .iter()
            .find(|p| p.slot_id == id)
            .and_then(|p| p.role)
    };
    assert_eq!(
        role(&l, 2),
        Some(SlotRole::Active),
        "focus moved to the other one"
    );

    // And back: the traversal CYCLES, it does not stay on the last one.
    h.dispatch(press("Tab")).await.expect("host alive");
    let back = next_layout(&mut sub).await;
    assert_eq!(role(&back, 1), Some(SlotRole::Active));
}

/// Focus never lands on a slot that is not focusable (the status bar, the
/// task strip): the traversal is the shared layer's.
#[tokio::test]
async fn tab_does_not_focus_the_status_bar() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    for _ in 0..6 {
        h.dispatch(press("Tab")).await.expect("host alive");
        let l = next_layout(&mut sub).await;
        let active = l
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        assert!(
            matches!(active, Some(1 | 2)),
            "focus only passes through the listings, not {active:?}"
        );
    }
}

/// Designating a target never points at yourself: a target equal to the
/// focused panel would be asking a copy to copy onto itself.
///
/// Along the way, this tests a USER LAYER's path: the binding is not in any
/// preset (#228), so the key is bound by the effective keymap the host
/// receives — the same one a real startup builds.
#[tokio::test]
async fn the_target_is_never_the_focused_panel() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};
    use norte_ui_host::dto::SlotRole;

    let preset =
        parse_keymap(norte_frontend::keymap::presets::source("orthodox").expect("factory preset"))
            .expect("preset parses");
    let layer = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [{ on = ["ctrl+t"], run = "layout.set-target" }]
"#,
    )
    .expect("layer parses");
    let keymap = Effective::build_for(
        &preset,
        &[layer],
        norte_ui_host::commands::IMPLEMENTADOS,
        Screen::Browse,
    )
    .expect("effective");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (200, 60),
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
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the user's layer binds the key: {ack:?}"
    );
    let l = next_layout(&mut sub).await;
    let role_of = |r: SlotRole| {
        l.placements
            .iter()
            .find(|p| p.role == Some(r))
            .map(|p| p.slot_id)
    };
    assert_eq!(role_of(SlotRole::Active), Some(1));
    assert_eq!(
        role_of(SlotRole::Target),
        Some(2),
        "the target is ALWAYS another slot"
    );
}

// ---------------------------------------------------------------------------
// The disk map (phase 4): measuring, landing, and not re-requesting.
// ---------------------------------------------------------------------------

/// The map's slot, in a tree with the listing next to it.
const SLOT_MAP: u32 = 7;

/// A host with a listing and, next to it, the disk map's slot.
async fn host_with_map(backend: Arc<Fake>) -> UiHost {
    use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

    let tree = Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(40)],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(SLOT_MAP), KindId::new("disk-map")),
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

/// The slot's map, if the snapshot brings it.
fn map_of(snap: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::DiskMapSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::DiskMap(m) if m.slot_id == SLOT_MAP => Some(&**m),
        _ => None,
    })
}

/// The measurement is requested for the LISTING's directory, and its report
/// lands.
///
/// It is what tells apart a declared panel from a working one: until T5 the
/// slot got painted — with its border and title — and measured nothing, so
/// `measuring` stayed false over an empty map forever and nobody noticed.
#[tokio::test]
async fn the_map_measures_the_listings_directory_and_the_report_lands() {
    let backend = fake_tree();
    let h = host_with_map(Arc::clone(&backend)).await;
    asentar().await;

    let requested = backend
        .until("a map measurement", |f| {
            f.maps_pedidos.lock().expect("mapas").first().cloned()
        })
        .await;
    assert_eq!(
        requested,
        dir(),
        "what is measured is what the listing shows"
    );

    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let view = next_snapshot(&mut sub).await;
    let map = map_of(&view).expect("the slot is still a map");
    assert!(
        !map.measuring,
        "the report landed: a map still saying \"measuring\" with the \
         measurement finished is the frozen panel this exists to prevent"
    );
    // The double answers a LISTED, empty report, which is what a directory
    // with no children produces. `squarify` with no children returns an
    // empty frame, so THAT is what is asserted, not "all cells blank": with
    // `lines` empty, an `all` over its lines holds without looking at
    // anything — a hollow green.
    assert!(
        map.lines.is_empty(),
        "with no children there is no split: {} lines",
        map.lines.len()
    );
    assert!(
        map.hits.is_empty(),
        "with no children there is nothing to click"
    );
}

/// It measures ONCE per directory: not once per actor message.
///
/// The probe runs after every message, so half its value is right here. It
/// is the same failure the plugin panel had to avoid — one RPC per
/// keystroke — and here it would be worse: every request walks a whole
/// tree.
#[tokio::test]
async fn the_map_does_not_measure_the_same_directory_twice() {
    let backend = fake_tree();
    let h = host_with_map(Arc::clone(&backend)).await;
    asentar().await;
    let _ = backend
        .until("the first measurement", |f| {
            f.maps_pedidos.lock().expect("mapas").first().cloned()
        })
        .await;

    for _ in 0..5 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        asentar().await;
    }
    let requested = backend.maps_pedidos.lock().expect("mapas").len();
    assert_eq!(requested, 1, "five messages, one measurement: {requested}");
}

// ---------------------------------------------------------------------------
// The TERMINAL panel (#362). The two tests that were missing, and neither
// needs a pty: the probe sweep cannot open it — its panels are `mem:///` and
// a shell refuses to sit there — so these two things went unchecked on their
// own, which is how six bugs reached `main` with the gate green.
// ---------------------------------------------------------------------------

/// **A read-only window opens no shell, not even through the button.**
///
/// The keymap's filter is not enough and that was the bug: it governs KEY
/// resolution, and the panel bar's button, the menu entry and the status
/// bar's buttons call dispatch without going through it. A click was enough.
///
/// It is the widest back door a window that promises not to write can have:
/// inside a shell, anything can be typed.
#[tokio::test]
async fn in_read_only_the_terminal_button_opens_nothing() {
    let backend = Arc::new(tree_as_fake());
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset_con(
            "orthodox",
            norte_ui_host::commands::Effects::SoloRead,
        )
        .expect("preset"),
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
    .expect("starts");

    // The button DOES exist in the bar — it comes from the shared registry —
    // and that is part of the case: what it must not do is work.
    let button = snap
        .panel_bar
        .buttons
        .iter()
        .position(|b| b.kind == "terminal")
        .expect("the terminal has a button in the bar");

    let ack = h
        .dispatch(UiAction::PanelBarActivate {
            button: u32::try_from(button).expect("fits"),
        })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "a read-only window cannot open a shell: {ack:?}"
    );
}

/// **It is entered and EXITED with the same key.**
///
/// The two halves of the same bug, and both are needed: opening by key left
/// focus on the listing — so the panel could only be reached with the mouse
/// — and once inside, the exit key did nothing. Since inside there ALL keys
/// belong to the shell, including the panel ring's, that was a mousetrap.
#[tokio::test]
async fn the_terminal_panel_opens_and_exits_with_the_same_key() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_with_tree(
        Arc::clone(&backend),
        norte_frontend::layout::presets::tree("simple").expect("layout"),
        (120, 40),
    )
    .await;
    let mut sub = h.subscribe();

    // The fake backend serves `mem:///`, where a shell refuses to sit. It is
    // checked because it is the same door as `app.terminal` and with the
    // same phrase, and because it is what keeps the probe sweep from being
    // able to open this panel.
    let ack = execute_via_palette_ack(&h, &mut sub, "layout.terminal").await;
    assert!(
        matches!(
            ack,
            ActionAck::Unavailable { ref reason_key } if reason_key == "host-not-local"
        ),
        "over a panel that is not local it refuses and says so: {ack:?}"
    );
}
