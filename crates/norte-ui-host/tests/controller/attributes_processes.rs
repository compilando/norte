use super::*;

// ---------------------------------------------------------------------------
// The attributes sheet and the processes panel (phase 4 gaps).
// ---------------------------------------------------------------------------

/// A host with the `full` layout, which brings an attributes sheet and a
/// processes panel besides the two listings.
pub(super) async fn host_full(backend: Arc<Fake>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
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
    .expect("starts")
}

/// The FIRST listing in a snapshot, whatever its position.
///
/// `listing` looks at slot 0, which in `simple` is the listing; in `full`
/// slot 0 is the places side bar.
pub(super) fn primer_listing(
    snap: &norte_ui_host::ViewSnapshot,
) -> &norte_ui_host::dto::BrowserSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Browser(b) => Some(b.as_ref()),
            _ => None,
        })
        .expect("the layout has some listing")
}

/// A snapshot's attributes sheet, if it is placed.
pub(super) fn sheet(
    snap: &norte_ui_host::ViewSnapshot,
) -> Option<&norte_ui_host::dto::MetadataSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Metadata(m) => Some(m.as_ref()),
        _ => None,
    })
}

/// The attributes sheet shows the entry under the cursor of the listing it
/// FOLLOWS, and moves with it.
#[tokio::test]
async fn the_attributes_sheet_follows_the_cursor() {
    let (h, snap) = host_full(fake_tree()).await;
    let mut sub = h.subscribe();
    let first = sheet(&snap).expect("the `full` layout places the sheet");
    assert!(
        first.note.is_empty() && !first.fields.is_empty(),
        "with a listing that has entries, the sheet shows the first one: {first:?}"
    );
    let name_of = |m: &norte_ui_host::dto::MetadataSlotView| {
        m.fields
            .first()
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let before = name_of(first);
    assert!(!before.is_empty(), "the first field is the name");

    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let after = name_of(sheet(&snap).expect("still placed"));
    assert_ne!(
        before, after,
        "the sheet followed the cursor without anyone asking for anything"
    );
}

/// With FOCUS on the sheet itself it still shows the active listing's entry:
/// following the active role when the active one is itself was following
/// nobody, and the sheet went empty when it was pressed (same bug as the
/// docked viewer, #291).
#[tokio::test]
async fn a_focused_attributes_sheet_does_not_go_empty() {
    let (h, snap) = host_full(fake_tree()).await;
    let mut sub = h.subscribe();
    let first = sheet(&snap).expect("the `full` layout places the sheet");
    assert!(!first.fields.is_empty());
    let slot = first.slot_id;
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let focused = snapshot_until(&h, &mut sub, "the sheet with focus", |s| {
        (s.focus == Some(slot)).then(|| s.clone())
    })
    .await;
    let h2 = sheet(&focused).expect("still placed");
    assert!(
        h2.note.is_empty() && h2.fields == first.fields,
        "the focused sheet still shows the listing's entry: {h2:?}"
    );
}

/// Like [`host_full`], but with the `..` row ON — which is what the factory
/// configuration brings and what anyone opening the window sees.
///
/// The rest of this suite turns it off on purpose (it reasons about listing
/// indices). The tests for panels that FOLLOW the cursor cannot afford that:
/// the cursor is born right on that row, so turning it off tests the one
/// state nobody starts in.
pub(super) async fn host_full_with_parent_row(
    backend: Arc<Fake>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    let mut cfg = norte_ui_host::default_settings();
    cfg.common.ui_parent_entry = Some(true);
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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

/// With the cursor on `..` the sheet DESCRIBES that row, it does not go
/// empty.
///
/// The bug this closes: a freshly opened window showed "nothing under the
/// cursor" on every startup and after every `cd`, because the cursor is born
/// on `..` and the sheet asked for the OPERAND — which on that row is `None`
/// on purpose — instead of what is pointed to.
#[tokio::test]
async fn the_sheet_describes_the_parent_row_instead_of_going_empty() {
    let (_h, snap) = host_full_with_parent_row(fake_tree()).await;
    let sheet = sheet(&snap).expect("the `full` layout places the sheet");
    assert!(
        sheet.note.is_empty(),
        "on `..` there is something to describe: {sheet:?}"
    );
    let rows: Vec<(&str, &str)> = sheet
        .fields
        .iter()
        .map(|f| (f.label.as_str(), f.value.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("Nombre", ".."),
            ("Clase", "carpeta"),
            ("Destino", "⟨mem⟩/")
        ],
        "`..` is named `..` and says where it leads, not the parent's name"
    );
}

/// Going up through the `..` row leaves the cursor on the directory being
/// left.
///
/// Same as `UiAction::Parent`, which is the other door to the SAME
/// navigation. Without this the cursor used to land on the parent's first
/// row depending on which of the two was used to go up, and up-and-down
/// stopped being reversible through one of them.
#[tokio::test]
async fn going_up_through_the_parent_row_leaves_the_cursor_where_you_were() {
    let (h, snap) = host_full_with_parent_row(fake_tree()).await;
    let mut sub = h.subscribe();
    let listing = primer_listing(&snap);
    let slot = listing.slot_id;

    // Go down into `docs` (row 1: right after `..`).
    h.dispatch(UiAction::Activate {
        slot_id: slot,
        key: norte_ui_host::RowKey(1),
        generation: listing.generation,
    })
    .await
    .expect("host alive");
    let inside = snapshot_until(&h, &mut sub, "the `docs` listing", |s| {
        let b = primer_listing(s);
        b.path_display.ends_with("docs").then(|| b.clone())
    })
    .await;

    // And go back up THROUGH the `..` row, which is the first one.
    h.dispatch(UiAction::Activate {
        slot_id: slot,
        key: norte_ui_host::RowKey(0),
        generation: inside.generation,
    })
    .await
    .expect("host alive");
    let outside = snapshot_until(&h, &mut sub, "back in `casa`", |s| {
        let b = primer_listing(s);
        b.path_display.ends_with("casa").then(|| b.clone())
    })
    .await;
    let under_the_cursor = outside
        .cursor
        .and_then(|k| outside.rows.get(usize::try_from(k.0).unwrap_or(0)))
        .map(|r| r.display_name.clone());
    assert_eq!(
        under_the_cursor.as_deref(),
        Some("docs"),
        "the cursor goes back to the directory it left, not to row 0: {:?}",
        outside
            .rows
            .iter()
            .map(|r| &r.display_name)
            .collect::<Vec<_>>()
    );
}

/// The sheet SAYS which listing it follows, and changes when focus changes.
///
/// "Details" on its own does not say what the details are of: with two
/// listings open, the only way to know which one it was describing was to
/// move the cursor and see if the sheet moved. Now it carries the path of
/// the panel it follows, which is the question that was missing an answer.
#[tokio::test]
async fn the_sheet_says_which_listing_it_follows() {
    let (h, snap) = host_full_with_parent_row(fake_tree()).await;
    let mut sub = h.subscribe();
    let first = sheet(&snap).expect("placed");
    assert_eq!(
        first.follows_display,
        norte_frontend::path_display(&dir()).0,
        "the path of the listing it follows: {first:?}"
    );
    assert!(!first.follows_hostile);

    // With focus on the OTHER listing, the sheet says so: it follows the
    // active one.
    let other = snap
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(b.slot_id),
            _ => None,
        })
        .nth(1)
        .expect("`full` has two listings");
    h.dispatch(UiAction::Activate {
        slot_id: other,
        key: norte_ui_host::RowKey(1),
        generation: 0,
    })
    .await
    .ok();
    h.dispatch(UiAction::FocusSlot { slot_id: other })
        .await
        .expect("host alive");
    let snap = snapshot_until(&h, &mut sub, "focus on the other listing", |s| {
        (s.focus == Some(other)).then(|| s.clone())
    })
    .await;
    let h2 = sheet(&snap).expect("still placed");
    assert!(
        !h2.follows_display.is_empty(),
        "and it still says whom it follows: {h2:?}"
    );
}

/// A listing and an attributes sheet, WITH NO docked viewer.
///
/// The `full` layout has both, and that hid the bug: the sheet had no way to
/// update on its own and travelled for free in the whole snapshot the VIEWER
/// triggered when the note changed. With no viewer in the layout there is
/// nobody to drag it along, and the sheet stayed frozen.
pub(super) async fn leaf_host_without_viewer(
    backend: Arc<Fake>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_frontend::layout::{Bindings, Dir, Follow, KindId, Node, RoleId, SlotId};
    let mut cfg = norte_ui_host::default_settings();
    cfg.common.ui_parent_entry = Some(true);
    let tree = Node::split(
        Dir::Horizontal,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot_bound(
                SlotId(8),
                KindId::new("metadata"),
                Bindings {
                    follows: Some(Follow::Role(RoleId::Active)),
                },
            ),
        ],
    );
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
        viewport: (200, 60),
        settings: cfg,
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

/// The kinds whose view comes from the CURSOR of the listing they follow.
///
/// A hand-written list, on purpose, like `paridad.rs::NO_APLICA`: whoever
/// adds a slot that follows the cursor edits it, and the test below demands
/// it have a probe. Deriving it from the kind registry does not work —
/// "following" is a bond of the slot, not a property of the kind, so the
/// registry does not know it.
pub(super) const FOLLOW_THE_CURSOR: &[&str] = &["viewer", "metadata"];

/// Each one of them has its own path to the renderer (ADR 0097, D3).
///
/// The window speaks in PATCHES: a row one writes `generation`,
/// `first_visible`, `rows` and `cursor`, and nothing else. A panel that
/// derives from the cursor and has no probe of its own only refreshes when
/// ANOTHER panel triggers a whole snapshot — and the factory layout
/// (`orthodox`) places neither of them, so that "other" does not exist for
/// most.
///
/// This is how the attributes sheet ended up frozen: it travelled for free
/// in the viewer's snapshot. This test puts each kind ALONE with a listing,
/// moves the cursor, and demands a snapshot with NO `Resync` requested —
/// which is the only thing a real renderer has.
#[tokio::test]
async fn every_slot_that_follows_the_cursor_has_its_own_probe() {
    use norte_frontend::layout::{Bindings, Dir, Follow, KindId, Node, RoleId, SlotId};
    for kind in FOLLOW_THE_CURSOR {
        let layout_tree = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot_bound(
                    SlotId(8),
                    KindId::new(*kind),
                    Bindings {
                        follows: Some(Follow::Role(RoleId::Active)),
                    },
                ),
            ],
        );
        let h = UiHost::start(UiHostOptions {
            backend: fake_tree(),
            initial_dir: dir(),
            initial_dir_requested: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
            layout: layout_tree,
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
        let (h, snap) = h;
        let mut sub = h.subscribe();
        let listing = primer_listing(&snap);

        h.dispatch(UiAction::SelectRow {
            slot_id: listing.slot_id,
            key: norte_ui_host::RowKey(1),
            generation: listing.generation,
        })
        .await
        .expect("host alive");
        asentar().await;

        tokio::time::pause();
        let arrival =
            tokio::time::timeout(std::time::Duration::from_secs(5), next_snapshot(&mut sub)).await;
        tokio::time::resume();
        assert!(
            arrival.is_ok(),
            "the `{kind}` slot follows the cursor and sends nothing when it \
             moves: it stays frozen in any layout that brings no other panel \
             to trigger a snapshot"
        );
    }
}

/// Clicking a row moves the sheet, even with no viewer to drag the snapshot
/// along.
///
/// What Oscar saw: in a layout with a tree, two listings and details — with
/// no viewer — clicking any row of any panel left the sheet on `..` forever.
/// `SelectRow` answers with a ROWS patch, and the sheet only travels in a
/// whole snapshot.
#[tokio::test]
async fn clicking_a_row_moves_the_sheet_with_no_viewer_in_the_layout() {
    let (h, snap) = leaf_host_without_viewer(fake_tree()).await;
    let mut sub = h.subscribe();
    let first = sheet(&snap).expect("the layout places the sheet");
    assert_eq!(
        first.fields.first().map(|f| f.value.as_str()),
        Some(".."),
        "to start with, the parent row"
    );
    let listing = primer_listing(&snap);
    let generation = listing.generation;
    // Row 2 of the listing: `..`, `docs`, and the next one.
    let target = listing
        .rows
        .get(2)
        .expect("there is a third row")
        .display_name
        .clone();

    h.dispatch(UiAction::SelectRow {
        slot_id: listing.slot_id,
        key: norte_ui_host::RowKey(2),
        generation,
    })
    .await
    .expect("host alive");
    asentar().await;

    // With NO `Resync`, and that is the whole point: `snapshot_until` requests a
    // snapshot on every round, so a test written with it would pass green
    // even if the click sent nothing — the snapshot it examines would be the
    // one the test itself triggered. What is checked here is what the host
    // sends ON ITS OWN on a click, which is the only thing the renderer has.
    tokio::time::pause();
    let arrival =
        tokio::time::timeout(std::time::Duration::from_secs(5), next_snapshot(&mut sub)).await;
    tokio::time::resume();
    let snap = arrival.expect("clicking produced no snapshot: the sheet stays frozen");

    let sheet = sheet(&snap).expect("still placed");
    assert_eq!(
        sheet.fields.first().map(|f| f.value.as_str()),
        Some(target.as_str()),
        "the sheet describes the clicked row: {:?}",
        sheet.fields
    );
}

/// With a filter choosing another row, the sheet describes THAT row and not
/// `..`.
///
/// The REAL cursor does not move in Filter mode, so asking
/// `is_parent_row(cursor())` on one side and `cursor_entry()` on the other
/// left the sheet saying "`..`, folder" while the listing highlighted a
/// file — and the hostile name the reader was looking at went unmarked,
/// which is exactly what this sheet is consulted for.
#[tokio::test]
async fn with_a_filter_the_sheet_describes_the_chosen_row_not_the_parent() {
    let (h, _snap) = host_full_with_parent_row(fake_tree()).await;
    let mut sub = h.subscribe();
    // `caf\xC3(` is the test tree's non-UTF-8 entry: it is filtered by a
    // letter the `..` row does not have.
    run_by_palette(&h, &mut sub, "pane.quick-search").await;
    for c in "caf".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    // It waits for the HOSTILE entry, not "the first one that is not `..`":
    // with the query half-typed ("c") the filter passes through `docs`,
    // which is also a real row and would answer that question without
    // proving anything.
    let snap = snapshot_until(&h, &mut sub, "the sheet over what was filtered", |s| {
        sheet(s)
            .filter(|m| m.fields.first().is_some_and(|f| f.hostile))
            .cloned()
    })
    .await;
    let name = snap.fields.first().expect("there is a name");
    assert!(
        name.hostile,
        "it describes the highlighted entry, and marks it: {name:?}"
    );
    assert!(
        !snap.fields.iter().any(|f| f.label == "Destino"),
        "and not the parent row: {:?}",
        snap.fields
    );
}

/// And the docked viewer says "directory", not "nothing selected".
///
/// The same bug in the other panel that follows the cursor, and for the same
/// reason: it was asking for the operand.
#[tokio::test]
async fn the_docked_viewer_over_the_parent_row_says_directory() {
    let (h, _snap) = host_full_with_parent_row(fake_tree()).await;
    let mut sub = h.subscribe();
    // Until the listing lands there is no cursor, and THAT note is another
    // one: it waits for the directory's, like the rest of the viewer's
    // tests.
    let view = snapshot_until(&h, &mut sub, "the preview slot over `..`", |s| {
        s.slots
            .iter()
            .find_map(|v| match v {
                SlotView::Preview(p) => Some(p.as_ref().clone()),
                _ => None,
            })
            .filter(|p| p.viewer.is_none() && p.note == "directorio")
    })
    .await;
    assert_eq!(
        view.note, "directorio",
        "`..` leads to a folder: that is what is under the cursor"
    );
}

/// A hostile name reaches the sheet masked and MARKED, just like a listing
/// row.
#[tokio::test]
async fn a_hostile_name_in_the_sheet_is_marked() {
    let (h, snap) = host_full(fake_tree()).await;
    let mut sub = h.subscribe();
    // The test tree has an entry whose name is not UTF-8.
    let mut view = sheet(&snap).expect("placed").clone();
    for _ in 0..6 {
        if view.fields.first().is_some_and(|f| f.hostile) {
            break;
        }
        h.dispatch(press("ArrowDown")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        view = sheet(&snap).expect("placed").clone();
    }
    let name = view.fields.first().expect("there is a name");
    assert!(name.hostile, "the non-UTF-8 entry is marked: {name:?}");
    assert!(
        !name.value.contains('\u{fffd}') || name.hostile,
        "and its text already comes sanitized"
    );
}

/// With focus on the PROCESSES panel, down moves through it and not through
/// the listing next to it.
#[tokio::test]
async fn the_processes_panel_takes_its_own_keys() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_full(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let cursor_before = primer_listing(&snap).cursor;

    // Two deletes are launched so the board has rows. The dialog's id is
    // READ, and the NEW one is waited for: fixing it by hand left the second
    // one unanswered, and an open dialog keeps the keyboard — which is
    // exactly what it should do — so tab never reached anywhere. `Enter`
    // does not work to confirm it: in a delete, `confirm` is destructive and
    // the keyboard chooses the first answer that is not.
    let mut answered = norte_ui_host::ModalId(0);
    for _ in 0..2 {
        h.dispatch(press("F8")).await.expect("host alive");
        let id = loop {
            h.dispatch(UiAction::Resync).await.expect("host alive");
            if let Some(d) = next_snapshot(&mut sub).await.dialogs.last()
                && d.id != answered
            {
                break d.id;
            }
        };
        answered = id;
        h.dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    }

    // Focus is cycled to the processes panel. Through the SCREEN's traversal
    // (`alt+o`): `Tab` cycles listings and does not stop on the side ones
    // (ADR 0102).
    let mut in_processes = false;
    for _ in 0..8 {
        h.dispatch(key_alt("o")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        let active = snap
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        if let Some(id) = active
            && snap
                .slots
                .iter()
                .any(|s| matches!(s, SlotView::Processes { slot_id, .. } if *slot_id == id))
        {
            in_processes = true;
            break;
        }
    }
    assert!(in_processes, "the ring reaches the processes panel");

    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(
        primer_listing(&snap).cursor,
        cursor_before,
        "moving down with focus on processes does NOT move the listing: the \
         role painted it focused and the keys went to the panel next to it"
    );
}

/// A snapshot's places side bar, if it is placed.
pub(super) fn places(
    snap: &norte_ui_host::ViewSnapshot,
) -> Option<&norte_ui_host::dto::PlacesSlotView> {
    snap.slots.iter().find_map(|s| match s {
        SlotView::Places(p) => Some(p.as_ref()),
        _ => None,
    })
}

/// The places side bar arrives with its TWO headers from the first frame,
/// and volumes get added to it when the host answers.
#[tokio::test]
async fn the_places_bar_does_not_jump_when_the_drives_arrive() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.volumes = vec![volume("mem:///otro", "ext4", false)];
    let (h, snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let first = places(&snap).expect("the `full` layout places the bar");
    let headers = first
        .rows
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Header { .. }))
        .count();
    assert_eq!(
        headers, 2,
        "both headers are there from the start, even with nothing under them"
    );

    // The volumes arrive later: the bar does not wait for them to paint.
    let mut with_drives = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        let v = places(&snap).expect("still placed").clone();
        if v.rows
            .iter()
            .any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            with_drives = Some(v);
            break;
        }
    }
    let v = with_drives.expect("the volumes reach the bar");
    let drive = v
        .rows
        .iter()
        .find_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Drive { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .expect("there is a drive");
    assert!(!drive.is_empty(), "and it says how much space it has");
}

/// With focus on the side bar, down moves through IT, and entering navigates
/// the LISTING — which is what makes having it open not change where
/// operations go.
#[tokio::test]
async fn the_places_bar_navigates_the_listing_and_does_not_keep_it() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumes = vec![volume("mem:///otro", "ext4", false)];
    let (h, _snap) = host_full(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // It waits for the drives to be there, and looks for their row.
    let mut drive_row = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        let v = places(&snap).expect("placed");
        if let Some(i) = v
            .rows
            .iter()
            .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
        {
            drive_row = Some((i, v.generation));
            break;
        }
    }
    let (i, generation) = drive_row.expect("the drives arrive");

    // A click on the drive: it selects AND activates, because a side bar
    // exists to go to places.
    h.dispatch(UiAction::PlaceActivateRow {
        row: u32::try_from(i).expect("fits"),
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let mut arrived = false;
    for _ in 0..20 {
        let snap = next_snapshot(&mut sub).await;
        if primer_listing(&snap).path_display.contains("otro") {
            arrived = true;
            break;
        }
        h.dispatch(UiAction::Resync).await.expect("host alive");
    }
    assert!(arrived, "the LISTING navigated to the volume, not the bar");
}

/// A click on the side bar cannot navigate to a place that was not pressed.
///
/// Volumes arrive from a background task and get inserted IN THE MIDDLE —
/// drives go before favorites — so between the user releasing the button
/// over a favorite and the host handling the action, that row is a different
/// one. With no generation the host used to accept it, and `set_cursor`
/// clamps instead of rejecting, so the worst case was navigating to the
/// LAST place in the list with an `Applied` ack. It is the race ADR 0068
/// exists to close.
#[tokio::test]
async fn a_click_on_the_bar_does_not_navigate_elsewhere_if_the_list_changed() {
    let mut cfg = test_settings();
    cfg.common.hotlist = vec![norte_config::HotlistItem {
        name: "proyectos".to_owned(),
        target: norte_proto::VPath::parse("mem:///proyectos").map_err(|_| "err".to_owned()),
    }];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///proyectos", vec![(b"p.txt".to_vec(), false)]);
    f.put("mem:///boot", vec![(b"vmlinuz".to_vec(), false)]);
    f.put("mem:///datos", vec![(b"d.txt".to_vec(), false)]);
    // TWO drives: they shift the favorite just enough for its index to land
    // on a drive and not on a header. With one, the click would have
    // collapsed a section, which is also wrong but shows less.
    f.volumes = vec![
        volume("mem:///boot", "ext4", false),
        volume("mem:///datos", "ext4", false),
    ];
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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

    // The snapshot the user HAS IN FRONT OF THEM, before the drives arrive.
    let before = places(&snap).expect("placed").clone();
    let clicked_row = before
        .rows
        .iter()
        .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))
        .expect("the favorite is there from the start");

    // The drives land and the list is DIFFERENT.
    let mut after = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let v = places(&next_snapshot(&mut sub).await)
            .expect("placed")
            .clone();
        if v.generation != before.generation {
            after = Some(v);
            break;
        }
    }
    let after = after.expect("the volumes arrive and bump the generation");
    assert!(
        matches!(
            after.rows.get(clicked_row),
            Some(norte_ui_host::dto::PlaceRowView::Drive { .. })
        ),
        "the row that was pressed is now a DRIVE, which is what makes the \
         bare index dangerous: {:?}",
        after.rows.get(clicked_row)
    );

    // The in-flight click, with the generation of the screen that was seen.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(clicked_row).expect("fits"),
            generation: before.generation,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "it is rejected instead of navigating elsewhere: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    for _ in 0..8 {
        let snap = next_snapshot(&mut sub).await;
        assert!(
            !primer_listing(&snap).path_display.contains("boot"),
            "and the pane did NOT go to the drive nobody pressed"
        );
        h.dispatch(UiAction::Resync).await.expect("host alive");
    }

    // With the right generation, the same click does go through.
    let ack = h
        .dispatch(UiAction::PlaceActivateRow {
            row: u32::try_from(clicked_row).expect("fits"),
            generation: after.generation,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "and the right generation does work: {ack:?}"
    );
}

/// A favorite whose path fails to parse is PAINTED with its reason: one that
/// disappears silently is a configuration failure nobody can see.
#[tokio::test]
async fn a_broken_favorite_shows_and_says_why() {
    let mut cfg = test_settings();
    cfg.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("full").expect("layout"),
        viewport: (200, 60),
        settings: cfg,
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
    let _ = &h;

    let v = places(&snap).expect("placed");
    let favorites: Vec<(String, String)> = v
        .rows
        .iter()
        .filter_map(|r| match r {
            norte_ui_host::dto::PlaceRowView::Favorite { name, broken, .. } => {
                Some((name.clone(), broken.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        favorites.len(),
        2,
        "both favorites are visible: {favorites:?}"
    );
    let broken = favorites
        .iter()
        .find(|(n, _)| n == "roto")
        .expect("the broken one is there");
    assert!(!broken.1.is_empty(), "and it says why it is broken");
    assert!(
        !broken.1.starts_with("err-"),
        "translated, not the key: {broken:?}"
    );
}
