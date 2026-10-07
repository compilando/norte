//! The window's panels, as a reader drives them (usability review
//! 2026-10-07, plan `2026-10-07-panels-usability.md`, line W).

use super::*;
use norte_frontend::layout::{Dir, KindId, Node, SlotId};

/// A host over `listing(1) | side(9)`, side by side, the side panel fixed.
async fn host_beside(kind: &str) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_over(Node::Split {
        dir: Dir::Horizontal,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(9), KindId::new(kind)),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(40),
        ],
    })
    .await
}

/// A host over `listing(1)` on top, `side(9)` docked at the bottom.
async fn host_docked(kind: &str) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_over(Node::Split {
        dir: Dir::Vertical,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(9), KindId::new(kind)),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(12),
        ],
    })
    .await
}

async fn host_over(layout: Node) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "en".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
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
}

/// Runs `action` and answers the screen as it is AFTER it.
async fn after(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    action: UiAction,
) -> (ActionAck, norte_ui_host::ViewSnapshot) {
    let ack = h.dispatch(action).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    // The resync's snapshot is the LAST one, and it is already queued: the
    // ack is answered after the update is broadcast.
    let mut snap = next_snapshot(sub).await;
    while let Ok(Some(u)) =
        tokio::time::timeout(std::time::Duration::from_millis(5), sub.recv()).await
    {
        if let Update::Message(m) = u
            && let UiUpdate::Snapshot(s) = m.payload
        {
            snap = *s;
        }
    }
    (ack, snap)
}

/// The slot with the keyboard.
fn focused(snap: &norte_ui_host::ViewSnapshot) -> Option<u32> {
    snap.layout
        .placements
        .iter()
        .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
        .map(|p| p.slot_id)
}

/// The kind of every placed slot, by id.
fn kinds(snap: &norte_ui_host::ViewSnapshot) -> Vec<(u32, String)> {
    snap.slots
        .iter()
        .filter_map(|s| {
            let v = serde_json::to_value(s).ok()?;
            let id = u32::try_from(v.get("slot_id")?.as_u64()?).ok()?;
            let kind = v.get("kind")?.as_str()?.to_owned();
            Some((id, kind))
        })
        .collect()
}

/// W1: `alt+o` LEAVES the terminal panel.
///
/// Inside it every key belongs to the shell, and `alt+o` was written to it as
/// `ESC o` — the ring's key did nothing there, so the only exits were the
/// panel's own toggle or the mouse. The ring, the close key and the panel
/// toggles go their normal way; the rest is still the shell's.
#[tokio::test]
async fn the_ring_key_leaves_the_terminal_panel() {
    let (h, _snap) = host_beside("terminal").await;
    let mut sub = h.subscribe();
    let (_, snap) = after(&h, &mut sub, UiAction::FocusSlot { slot_id: 9 }).await;
    assert_eq!(focused(&snap), Some(9), "inside the terminal");

    let (_, snap) = after(&h, &mut sub, key_alt("o")).await;
    assert_eq!(
        focused(&snap),
        Some(1),
        "alt+o took the keys back to the listing"
    );
}

/// W1: `Escape` in a side panel gives the keys back to the listing.
///
/// Nothing in the browse keymap binds it, so it was swallowed: the reader
/// pressed the universal "get me out" and stayed in the places bar.
#[tokio::test]
async fn escape_in_a_side_panel_returns_to_the_listing() {
    for kind in ["places", "processes", "log"] {
        let (h, _snap) = host_beside(kind).await;
        let mut sub = h.subscribe();
        let (_, snap) = after(&h, &mut sub, UiAction::FocusSlot { slot_id: 9 }).await;
        assert_eq!(focused(&snap), Some(9), "{kind}: focus inside");
        let (_, snap) = after(&h, &mut sub, press("Escape")).await;
        assert_eq!(
            focused(&snap),
            Some(1),
            "{kind}: Escape went back to the listing"
        );
    }
}

/// W1: Escape on the LISTING changes nothing — there is nowhere to return.
#[tokio::test]
async fn escape_on_the_listing_stays() {
    let (h, _snap) = host_beside("places").await;
    let mut sub = h.subscribe();
    let (_, snap) = after(&h, &mut sub, press("Escape")).await;
    assert_eq!(focused(&snap), Some(1));
}

/// W2: a layout button pressed while a DOCK has the keys splits the
/// listing, not the dock.
///
/// It used to split the focused slot, so a click on "split side by side"
/// with the processes dock focused put a new listing INSIDE the dock, under
/// its tab strip and without a path bar.
#[tokio::test]
async fn a_split_button_splits_the_listing_not_the_dock() {
    let (h, _snap) = host_docked("processes").await;
    let mut sub = h.subscribe();
    let _ = after(&h, &mut sub, UiAction::FocusSlot { slot_id: 9 }).await;
    let (ack, snap) = after(
        &h,
        &mut sub,
        UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        },
    )
    .await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let dock = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == 9)
        .expect("the dock is still placed");
    let listings: Vec<_> = snap
        .layout
        .placements
        .iter()
        .filter(|p| {
            kinds(&snap)
                .iter()
                .any(|(id, k)| *id == p.slot_id && k == "browser")
        })
        .collect();
    assert_eq!(listings.len(), 2, "one more listing: {:?}", kinds(&snap));
    for l in &listings {
        assert!(
            l.y + l.height <= dock.y,
            "every listing sits ABOVE the dock, none inside it: {l:?} vs {dock:?}"
        );
    }
    assert_eq!(
        dock.width, snap.layout.cells.0,
        "the dock keeps the full width"
    );
}

/// W2: the keyboard's split from a dock does the same.
#[tokio::test]
async fn the_split_key_from_a_dock_splits_the_listing() {
    let (h, _snap) = host_docked("log").await;
    let mut sub = h.subscribe();
    let _ = after(&h, &mut sub, UiAction::FocusSlot { slot_id: 9 }).await;
    // Side by side: splitting the dock that way is what narrows it.
    let _ = h.dispatch(key_alt("h")).await.expect("host alive");
    let (_, snap) = after(&h, &mut sub, UiAction::Resync).await;
    let dock = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == 9)
        .expect("the dock is still placed");
    assert_eq!(dock.width, snap.layout.cells.0, "the dock was not split");
    let browsers = kinds(&snap).iter().filter(|(_, k)| k == "browser").count();
    assert_eq!(browsers, 2, "{:?}", kinds(&snap));
}

/// W5: the border above a bottom dock drags, from a listing that ALSO has a
/// neighbor to its right.
///
/// The host looked for the right neighbor first, so grabbing the horizontal
/// border under the left listing resized the two listings instead — or,
/// clamped, nothing at all. The grip now says which border it is.
#[tokio::test]
async fn the_border_above_a_bottom_dock_drags() {
    let (h, snap) = host_over(Node::Split {
        dir: Dir::Vertical,
        children: vec![
            Node::Split {
                dir: Dir::Horizontal,
                children: vec![
                    Node::slot(SlotId(1), KindId::browser()),
                    Node::slot(SlotId(2), KindId::browser()),
                ],
                sizes: vec![
                    norte_frontend::layout::Size::Weight(1),
                    norte_frontend::layout::Size::Weight(1),
                ],
            },
            Node::slot(SlotId(9), KindId::new("processes")),
        ],
        sizes: vec![
            norte_frontend::layout::Size::Weight(1),
            norte_frontend::layout::Size::Fixed(12),
        ],
    })
    .await;
    let mut sub = h.subscribe();
    let place = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .expect("placed")
            .clone()
    };
    let dock = place(&snap, 9);
    let left = place(&snap, 1);
    let (_, after_drag) = after(
        &h,
        &mut sub,
        UiAction::ResizeSlot {
            slot_id: 1,
            cells: dock.y - 8,
            axis: Some(norte_ui_host::BorderAxis::Row),
        },
    )
    .await;
    let dock_now = place(&after_drag, 9);
    assert!(
        dock_now.y < dock.y && dock_now.height > dock.height,
        "the dock grew upward: {dock:?} -> {dock_now:?}"
    );
    assert_eq!(
        place(&after_drag, 1).width,
        left.width,
        "the listings' vertical border did not move"
    );
}

/// W3: the disk map moved into the bottom dock's tab group stays alive.
///
/// In the window it went blank — no rectangles, no "measuring", no empty
/// note — after a tab-drag into the dock.
#[tokio::test]
async fn the_disk_map_moved_into_a_dock_still_says_something() {
    let backend = fake_tree();
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: backend.clone(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "en".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(7), KindId::new("disk-map")),
                    ],
                    sizes: vec![
                        norte_frontend::layout::Size::Weight(1),
                        norte_frontend::layout::Size::Fixed(40),
                    ],
                },
                Node::slot(SlotId(9), KindId::new("processes")),
            ],
            sizes: vec![
                norte_frontend::layout::Size::Weight(1),
                norte_frontend::layout::Size::Fixed(12),
            ],
        },
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
    .expect("starts");
    settle().await;
    let _ = backend
        .until("the first measurement", |f| {
            f.maps_requests.lock().expect("maps").first().cloned()
        })
        .await;
    let mut sub = h.subscribe();
    let _ = after(
        &h,
        &mut sub,
        UiAction::MoveSlot {
            slot_id: 7,
            target: 9,
            zone: norte_frontend::layout::DropZone::Center,
        },
    )
    .await;
    settle().await;
    let (_, snap) = after(&h, &mut sub, UiAction::Resync).await;
    let map = snap
        .slots
        .iter()
        .find_map(|s| match s {
            SlotView::DiskMap(m) => Some(m.clone()),
            _ => None,
        })
        .expect("the map is still a slot");
    assert!(
        snap.layout
            .placements
            .iter()
            .any(|p| p.slot_id == map.slot_id),
        "the moved map is the tab in front: {:?}",
        snap.layout.placements
    );
    assert!(
        map.measuring || !map.lines.is_empty() || !map.empty.is_empty(),
        "a map in the dock says something: {map:?}"
    );
}

/// W4: closing a SIDE panel does not say "splits again".
///
/// That hint is for a closed listing — the reader is left with half a
/// screen and needs the split key. Closing the log is not that, and the
/// split key does not bring the log back.
#[tokio::test]
async fn closing_a_side_panel_does_not_offer_the_split() {
    let (h, _snap) = host_docked("log").await;
    let mut sub = h.subscribe();
    let _ = after(&h, &mut sub, UiAction::FocusSlot { slot_id: 9 }).await;
    let (_, snap) = after(&h, &mut sub, key_alt("x")).await;
    assert!(
        !snap.layout.placements.iter().any(|p| p.slot_id == 9),
        "the log closed"
    );
    let message = snap.status.message.clone().unwrap_or_default();
    assert!(
        !message.contains("alt+h") && !message.contains("split"),
        "no split hint for a side panel: {message:?}"
    );
}
