//! The window's context menu (spec 2026-10-09).
use super::*;
use norte_ui_host::dto::ContextMenuView;

fn browser(s: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    s.slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) => Some(&**b),
            _ => None,
        })
        .expect("a listing")
}

async fn open_on(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    row: usize,
) -> ContextMenuView {
    let s = snapshot(h, sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ContextMenuRow {
        slot_id: b.slot_id,
        key: b.rows[row].key,
        generation: b.generation,
        x: 10,
        y: 20,
    })
    .await
    .expect("host alive");
    snapshot_until(h, sub, "the menu open", |s| s.context_menu.clone()).await
}

async fn mark(h: &UiHost, sub: &mut norte_ui_host::UiSubscription, rows: &[usize]) {
    let s = snapshot(h, sub).await;
    let b = browser(&s);
    for &r in rows {
        h.dispatch(UiAction::ToggleMark {
            slot_id: b.slot_id,
            key: b.rows[r].key,
            generation: b.generation,
        })
        .await
        .expect("host alive");
    }
}

#[tokio::test]
async fn an_unmarked_row_drops_the_marks_and_names_itself() {
    let (h, _) = host(vec!["a.txt", "b.txt", "c.txt"]).await;
    let mut sub = h.subscribe();
    mark(&h, &mut sub, &[0, 1]).await;
    let m = open_on(&h, &mut sub, 2).await;
    assert!(m.header.contains("c.txt"), "{}", m.header);
    assert_eq!((m.x, m.y), (Some(10), Some(20)));
    let after = snapshot(&h, &mut sub).await;
    assert_eq!(browser(&after).marks, 0, "the marks went");
    assert_eq!(browser(&after).cursor, Some(browser(&after).rows[2].key));
}

#[tokio::test]
async fn a_marked_row_keeps_the_marks_and_says_how_many() {
    let (h, _) = host(vec!["a.txt", "b.txt", "c.txt"]).await;
    let mut sub = h.subscribe();
    mark(&h, &mut sub, &[0, 1]).await;
    let m = open_on(&h, &mut sub, 1).await;
    // The whole string, from the same two keys in the host's locale (`es`):
    // the count goes INSIDE the "acts on" frame, not instead of it.
    let lang = norte_i18n::Lang::Es;
    let marks = norte_i18n::ta_in(lang, "gui-menu-target-marks", &[("n", "2")]);
    let expected = norte_i18n::ta_in(lang, "gui-menu-acts-on", &[("target", &marks)]);
    assert_eq!(m.header, expected);
    assert_eq!(browser(&snapshot(&h, &mut sub).await).marks, 2);
}

#[tokio::test]
async fn the_core_comes_first_with_chords_and_reasons() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    assert!(m.items.len() >= 9);
    let copy = &m.items[3];
    assert_eq!(copy.chord, "F5", "orthodox preset");
    assert!(copy.enabled && copy.reason.is_empty(), "{copy:?}");
    let delete = &m.items[6];
    assert_eq!(delete.role, "destructive");
}

#[tokio::test]
async fn a_stale_generation_opens_nothing() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    let ack = h
        .dispatch(UiAction::ContextMenuRow {
            slot_id: b.slot_id,
            key: b.rows[0].key,
            generation: b.generation + 99,
            x: 0,
            y: 0,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

#[tokio::test]
async fn the_empty_area_touches_no_marks() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    mark(&h, &mut sub, &[0]).await;
    let slot_id = browser(&snapshot(&h, &mut sub).await).slot_id;
    h.dispatch(UiAction::ContextMenuEmpty {
        slot_id,
        x: 5,
        y: 5,
    })
    .await
    .expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(
        m.items.iter().any(|i| i.role == "ai"),
        "the AI section is there"
    );
    assert_eq!(browser(&snapshot(&h, &mut sub).await).marks, 1);
}

#[tokio::test]
async fn the_name_column_cannot_be_hidden_and_says_why() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let b = browser(&snapshot(&h, &mut sub).await).slot_id;
    h.dispatch(UiAction::ContextMenuHeader {
        slot_id: b,
        column: "name".into(),
        x: 0,
        y: 0,
    })
    .await
    .expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    let hide = &m.items[1];
    assert!(!hide.enabled && !hide.reason.is_empty(), "{hide:?}");
}

#[tokio::test]
async fn closing_and_pointing() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuPointRow { row: 3 })
        .await
        .expect("host alive");
    assert_eq!(
        snapshot(&h, &mut sub)
            .await
            .context_menu
            .expect("open")
            .cursor,
        3
    );
    h.dispatch(UiAction::ContextMenuClose)
        .await
        .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

/// Review Focus 5.
#[tokio::test]
async fn a_hostile_long_name_is_masked_and_elided() {
    let name: &'static str = Box::leak(format!("\u{202e}{}.txt", "x".repeat(300)).into_boxed_str());
    let (h, _) = host(vec![name]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    assert!(!m.header.contains('\u{202e}'), "masked");
    assert!(m.header.contains('…'), "elided: {}", m.header);
}

/// Review Focus 2: a right click on the OTHER listing focuses it first (the
/// renderer sends `FocusSlot`), and the row then resolves against it.
#[tokio::test]
async fn a_row_of_the_other_listing_opens_after_its_focus() {
    let (h, _) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    let s = snapshot(&h, &mut sub).await;
    let other = s
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) if Some(b.slot_id) != s.focus => Some((**b).clone()),
            _ => None,
        })
        .expect("a second listing");
    h.dispatch(UiAction::FocusSlot {
        slot_id: other.slot_id,
    })
    .await
    .expect("host alive");
    // By name and not by index: `fake_tree` sorts a non-UTF-8 name between
    // `docs` and `notas.txt`.
    let notas = other
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("notas.txt is listed");
    let ack = h
        .dispatch(UiAction::ContextMenuRow {
            slot_id: other.slot_id,
            key: notas.key,
            generation: other.generation,
            x: 1,
            y: 1,
        })
        .await
        .expect("host alive");
    assert!(!matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(m.header.contains("notas.txt"), "{}", m.header);
}

/// The menu row of the entry that runs `command`, found by its label in the
/// host's locale (`es`) — the label key is the catalogue's `menu-item-*`.
fn row_of_command(m: &ContextMenuView, command: &str) -> u32 {
    let want = norte_i18n::t_in(
        norte_i18n::Lang::Es,
        &format!("menu-item-{}", command.replace('.', "-")),
    );
    let i = m
        .items
        .iter()
        .position(|i| i.label == want)
        .unwrap_or_else(|| panic!("{command} is not in the menu"));
    u32::try_from(i).expect("fits")
}

/// Opens the menu on the row NAMED `name`: `fake_tree` sorts a non-UTF-8
/// name among the others, so an index would name a different file.
async fn open_on_named(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    name: &str,
) -> ContextMenuView {
    let s = snapshot(h, sub).await;
    let row = browser(&s)
        .rows
        .iter()
        .position(|r| r.display_name == name)
        .unwrap_or_else(|| panic!("{name} is not listed"));
    open_on(h, sub, row).await
}

/// Choosing "rename" opens the SAME dialog shift+F6 opens.
#[tokio::test]
async fn an_entry_runs_what_its_key_runs() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_of_command(&m, "pane.rename"),
    })
    .await
    .expect("host alive");
    let s = snapshot_until(&h, &mut sub, "the rename dialog", |s| {
        (!s.dialogs.is_empty()).then(|| s.clone())
    })
    .await;
    assert!(s.context_menu.is_none(), "closed before the effect");
    let by_key = {
        let (h2, _) = host(vec!["a.txt"]).await;
        let mut sub2 = h2.subscribe();
        h2.dispatch(key_mod("F6", false, true))
            .await
            .expect("host alive");
        snapshot_until(&h2, &mut sub2, "dialog", |s| {
            s.dialogs.first().map(|d| d.title_key.clone())
        })
        .await
    };
    assert_eq!(s.dialogs[0].title_key, by_key);
}

/// The close travels in its OWN patch, BEFORE the effect's: the command may
/// open a dialog that must get the keys, and a renderer applying patches in
/// order would otherwise paint the menu over it for a frame.
#[tokio::test]
async fn the_close_patch_precedes_the_effect() {
    use norte_ui_host::dto::ViewChange;
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_of_command(&m, "pane.rename"),
    })
    .await
    .expect("host alive");
    let mut closed_at = None;
    let mut dialog_at = None;
    for n in 0..20 {
        let Ok(Some(Update::Message(env))) =
            tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await
        else {
            break;
        };
        if let UiUpdate::Patch(p) = &env.payload {
            for c in &p.changes {
                match c {
                    ViewChange::ContextMenu { context_menu: None } => {
                        closed_at.get_or_insert(n);
                    }
                    ViewChange::Dialogs { dialogs } if !dialogs.is_empty() => {
                        dialog_at.get_or_insert(n);
                    }
                    _ => {}
                }
            }
        }
        if dialog_at.is_some() {
            break;
        }
    }
    let (Some(c), Some(d)) = (closed_at, dialog_at) else {
        panic!("closed at {closed_at:?}, dialog at {dialog_at:?}");
    };
    assert!(
        c < d,
        "the close ({c}) goes in its own patch before the dialog ({d})"
    );
}

/// A read-only WINDOW (`host_solo_read`, effects `SoloRead`): Move is not in
/// `all_with(effects)`, so it travels disabled, and clicking it does nothing.
#[tokio::test]
async fn a_disabled_entry_runs_nothing_and_stays_open() {
    let (h, _) = host_solo_read(fake_tree()).await;
    let mut sub = h.subscribe();
    let m = open_on_named(&h, &mut sub, "notas.txt").await;
    let mv = row_of_command(&m, "pane.move") as usize;
    assert!(
        !m.items[mv].enabled && !m.items[mv].reason.is_empty(),
        "{:?}",
        m.items[mv]
    );
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: u32::try_from(mv).expect("fits"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_some(), "still open");
    assert!(s.dialogs.is_empty(), "nothing ran");
}

/// Review Focus 3.
#[tokio::test]
async fn a_target_that_changed_runs_nothing() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    // Change the target under the menu: mark another row through a renderer
    // action the menu does not own (`ToggleMark` bypasses its keys).
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ToggleMark {
        slot_id: b.slot_id,
        key: b.rows[1].key,
        generation: b.generation,
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: row_of_command(&m, "pane.rename"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_none() && s.dialogs.is_empty());
}

#[tokio::test]
async fn keys_walk_and_run_the_menu_and_escape_closes() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    assert_eq!(s.context_menu.as_ref().expect("open").cursor, 1);
    // The arrow walked the MENU, not the listing underneath.
    assert_eq!(browser(&s).cursor, Some(browser(&s).rows[0].key));
    h.dispatch(press("ArrowUp")).await.expect("host alive");
    h.dispatch(press("ArrowUp")).await.expect("host alive");
    let m = snapshot(&h, &mut sub).await.context_menu.expect("open");
    assert_eq!(
        usize::try_from(m.cursor).expect("fits"),
        m.items.len() - 1,
        "wraps at the top"
    );
    h.dispatch(press("Escape")).await.expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.context_menu.is_none());
    // Escape was spent closing the menu and on nothing else: the listing
    // under it kept its cursor, and nothing opened in its place.
    assert_eq!(
        browser(&s).cursor,
        Some(browser(&s).rows[0].key),
        "Escape did not leave the panel"
    );
    assert!(s.dialogs.is_empty() && s.palette.is_none());
}

#[tokio::test]
async fn enter_runs_the_highlighted_entry() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let m = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::ContextMenuPointRow {
        row: row_of_command(&m, "pane.rename"),
    })
    .await
    .expect("host alive");
    h.dispatch(press("Enter")).await.expect("host alive");
    let s = snapshot_until(&h, &mut sub, "the rename dialog", |s| {
        (!s.dialogs.is_empty()).then(|| s.clone())
    })
    .await;
    assert!(s.context_menu.is_none());
}

/// Review Focus 4.
#[tokio::test]
async fn under_a_dialog_nothing_opens() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    // The rename dialog.
    h.dispatch(key_mod("F6", false, true))
        .await
        .expect("host alive");
    let () = snapshot_until(&h, &mut sub, "dialog", |s| {
        (!s.dialogs.is_empty()).then_some(())
    })
    .await;
    let s = snapshot(&h, &mut sub).await;
    let b = browser(&s);
    h.dispatch(UiAction::ContextMenuRow {
        slot_id: b.slot_id,
        key: b.rows[0].key,
        generation: b.generation,
        x: 0,
        y: 0,
    })
    .await
    .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

/// A dialog that opens WITHOUT going through the menu closes it. Not by the
/// palette: with the menu open, the palette's key is the menu's and is
/// swallowed. A drop from another application is a dialog the menu never
/// saw coming, and it does not go through `apply_effect` either.
#[tokio::test]
async fn a_dialog_opening_closes_the_menu() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let _ = open_on_named(&h, &mut sub, "notas.txt").await;
    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned()],
    })
    .await
    .expect("host alive");
    let s = snapshot_until(&h, &mut sub, "the drop dialog", |s| {
        (!s.dialogs.is_empty()).then(|| s.clone())
    })
    .await;
    assert!(s.context_menu.is_none());
}

/// The menu bar's dropdown and the context menu never share the screen.
#[tokio::test]
async fn the_menu_bar_opening_closes_the_menu() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    let _ = open_on(&h, &mut sub, 0).await;
    h.dispatch(UiAction::MenuOpen { menu: 0 })
        .await
        .expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    assert!(s.menu.open.is_some(), "the bar opened");
    assert!(s.context_menu.is_none());
}

#[tokio::test]
async fn shift_f10_opens_on_the_cursor_row_with_no_anchor() {
    let (h, _) = host(vec!["a.txt", "b.txt"]).await;
    let mut sub = h.subscribe();
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let ack = h
        .dispatch(key_mod("F10", false, true))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert_eq!((m.x, m.y), (None, None));
    assert!(m.header.contains("b.txt"), "{}", m.header);
}

/// The Menu key ALONE opens it — the browser's `ContextMenu`. The presets
/// first bound `on = ["shift+f10", "menu"]`, which is a two-key SEQUENCE:
/// neither key opened anything by itself.
#[tokio::test]
async fn the_menu_key_alone_opens_it() {
    let (h, _) = host(vec!["a.txt"]).await;
    let mut sub = h.subscribe();
    h.dispatch(press("ContextMenu")).await.expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(m.header.contains("a.txt"), "{}", m.header);
}

/// An empty listing has no row to act on: the key opens the folder's menu.
#[tokio::test]
async fn on_an_empty_listing_the_key_opens_the_folder_menu() {
    let (h, _) = host(vec![]).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("F10", false, true))
        .await
        .expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert!(m.items.iter().any(|i| i.role == "ai"), "{m:?}");
}

// ---------------------------------------------------------------------------
// Task 5: Places, the tree and the header verbs.
// ---------------------------------------------------------------------------

fn t(key: &str) -> String {
    norte_i18n::t_in(norte_i18n::Lang::Es, key)
}

/// The menu row whose label is `key`'s, in the host's locale.
fn row_labelled(m: &ContextMenuView, key: &str) -> u32 {
    let want = t(key);
    u32::try_from(m.items.iter().position(|i| i.label == want).expect(key)).expect("fits")
}

/// Starts with `layout`, `settings` and `paths`; the rest as `host_full`.
/// Boxed: the settings travel inside the future, and an unboxed one is
/// tens of KB on every caller's stack (`clippy::large_futures`).
fn host_places(
    f: Fake,
    layout: &str,
    settings: norte_frontend::config::FrontendConfig,
    paths: norte_ui_host::settings::HostPaths,
) -> std::pin::Pin<Box<impl std::future::Future<Output = UiHost>>> {
    let layout = norte_frontend::layout::presets::tree(layout).expect("layout");
    Box::pin(async move {
        let (h, _) = UiHost::start(UiHostOptions {
            backend: Arc::new(f),
            initial_dir: dir(),
            initial_dir_requested: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
            layout,
            viewport: (200, 60),
            settings,
            paths,
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
    })
}

/// A host on `layout` with the places bar and one drive at `mem:///otro`;
/// returns the drive's row and the generation it was painted with.
async fn with_a_drive_in(layout: &str) -> (UiHost, norte_ui_host::UiSubscription, u32, u64) {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumes = vec![volume("mem:///otro", "ext4", false)];
    let h = host_places(
        f,
        layout,
        test_settings(),
        norte_ui_host::settings::HostPaths::default(),
    )
    .await;
    let mut sub = h.subscribe();
    let (row, generation) = snapshot_until(&h, &mut sub, "the drive row", |s| {
        let v = places(s)?;
        let i = v
            .rows
            .iter()
            .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))?;
        Some((u32::try_from(i).expect("fits"), v.generation))
    })
    .await;
    (h, sub, row, generation)
}

async fn with_a_drive() -> (UiHost, norte_ui_host::UiSubscription, u32, u64) {
    with_a_drive_in("full").await
}

/// Opens the menu on places row `row` and waits for it.
async fn open_on_place(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    row: u32,
    generation: u64,
) -> ContextMenuView {
    let ack = h
        .dispatch(UiAction::ContextMenuPlace {
            row,
            generation,
            x: 0,
            y: 0,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    snapshot_until(h, sub, "menu", |s| s.context_menu.clone()).await
}

/// Opens the menu on tree branch `row` and waits for it.
async fn open_on_branch(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    row: u32,
    generation: u64,
) -> ContextMenuView {
    let ack = h
        .dispatch(UiAction::ContextMenuBranch {
            row,
            generation,
            x: 0,
            y: 0,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    snapshot_until(h, sub, "menu", |s| s.context_menu.clone()).await
}

/// The listing with `slot_id`, by id.
fn listing_of(
    s: &norte_ui_host::ViewSnapshot,
    slot_id: u32,
) -> &norte_ui_host::dto::BrowserSlotView {
    s.slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) if b.slot_id == slot_id => Some(&**b),
            _ => None,
        })
        .expect("that listing")
}

#[tokio::test]
async fn a_place_menu_names_the_place_and_offers_its_verbs() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let m = open_on_place(&h, &mut sub, row, generation).await;
    assert!(m.header.contains("otro"), "{}", m.header);
    let labels: Vec<&str> = m.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            t("ctx-open-here"),
            t("ctx-open-in-other"),
            t("ctx-open-in-new-tab"),
            t("ctx-copy-path"),
            t("ctx-add-favorite"),
        ]
    );
    assert!(m.items.iter().all(|i| i.enabled), "{m:?}");
    assert_eq!(
        places(&snapshot(&h, &mut sub).await)
            .expect("placed")
            .cursor,
        u64::from(row),
        "the bar's cursor went to the row"
    );
}

#[tokio::test]
async fn a_place_from_another_painted_list_opens_nothing() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let ack = h
        .dispatch(UiAction::ContextMenuPlace {
            row,
            generation: generation + 99,
            x: 0,
            y: 0,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_none());
}

#[tokio::test]
async fn copy_path_of_a_place_copies_that_path() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let mut native = h.native_effects();
    let m = open_on_place(&h, &mut sub, row, generation).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_labelled(&m, "ctx-copy-path"),
    })
    .await
    .expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("an effect")
        .expect("channel alive");
    match effect {
        norte_ui_host::dto::NativeEffect::CopyBytes { bytes, count } => {
            assert_eq!(count, 1);
            assert!(
                String::from_utf8_lossy(&bytes).contains("otro"),
                "{bytes:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn open_here_from_a_place_is_the_click() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let m = open_on_place(&h, &mut sub, row, generation).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_labelled(&m, "ctx-open-here"),
    })
    .await
    .expect("host alive");
    snapshot_until(&h, &mut sub, "the listing went to the drive", |s| {
        (s.context_menu.is_none() && primer_listing(s).path_display.contains("otro")).then_some(())
    })
    .await;
}

/// `full` places TWO listings: "open in the other pane" moves the
/// destination, and the active one stays where it was.
#[tokio::test]
async fn open_in_other_navigates_the_destination_listing() {
    let (h, mut sub, row, generation) = with_a_drive().await;
    let before = snapshot(&h, &mut sub).await;
    let active = before.focus.expect("a focused slot");
    let other = before
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) if b.slot_id != active => Some(b.slot_id),
            _ => None,
        })
        .expect("a second listing");
    let m = open_on_place(&h, &mut sub, row, generation).await;
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, "ctx-open-in-other"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let s = snapshot_until(&h, &mut sub, "the other listing went to the drive", |s| {
        listing_of(s, other)
            .path_display
            .contains("otro")
            .then(|| s.clone())
    })
    .await;
    assert!(
        !listing_of(&s, active).path_display.contains("otro"),
        "the active one did not move"
    );
}

/// `explorer` places ONE listing: there is no destination, and the refusal
/// is the transfer one, not a silent nothing.
#[tokio::test]
async fn open_in_other_with_no_other_pane_says_so() {
    let (h, mut sub, row, generation) = with_a_drive_in("explorer").await;
    let m = open_on_place(&h, &mut sub, row, generation).await;
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, "ctx-open-in-other"),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "host-no-other-slot"),
        "{ack:?}"
    );
    assert!(
        !primer_listing(&snapshot(&h, &mut sub).await)
            .path_display
            .contains("otro")
    );
}

/// `fake_tree` with a subfolder in `docs`, so folding `docs` changes the
/// rows.
fn deep_tree() -> Arc<Fake> {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put(
        "mem:///casa/docs",
        vec![(b"sub".to_vec(), true), (b"a.md".to_vec(), false)],
    );
    f.put("mem:///casa/docs/sub", vec![(b"z.md".to_vec(), false)]);
    Arc::new(f)
}

/// The tree's row labelled `label`, and the generation it was painted with.
fn branch_named(s: &norte_ui_host::ViewSnapshot, label: &str) -> (u32, u64) {
    let tree = tree_of(s);
    let i = tree
        .rows
        .iter()
        .position(|r| r.label == label)
        .unwrap_or_else(|| panic!("{label} is not a branch: {:?}", tree.rows));
    (
        u32::try_from(tree.first).expect("fits") + u32::try_from(i).expect("fits"),
        tree.generation,
    )
}

#[tokio::test]
async fn a_branch_opens_in_a_new_tab() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    // With the keys on the TREE: the new tab still goes next to the listing.
    h.dispatch(UiAction::FocusSlot {
        slot_id: tree_of(&s).slot_id,
    })
    .await
    .expect("host alive");
    let s = snapshot(&h, &mut sub).await;
    let (row, generation) = branch_named(&s, "docs");
    let m = open_on_branch(&h, &mut sub, row, generation).await;
    assert!(m.header.ends_with("/casa/docs"), "{}", m.header);
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_labelled(&m, "ctx-open-in-new-tab"),
    })
    .await
    .expect("host alive");
    let s = snapshot_until(&h, &mut sub, "a second tab listing docs", |s| {
        let two_tabs = s.layout.tabs.iter().any(|g| g.tabs.len() == 2);
        let docs = s
            .slots
            .iter()
            .any(|v| matches!(v, SlotView::Browser(b) if b.path_display.ends_with("/casa/docs")));
        (two_tabs && docs).then(|| s.clone())
    })
    .await;
    let titles: Vec<&str> = s
        .layout
        .tabs
        .iter()
        .flat_map(|g| g.tabs.iter().map(|t| t.title.as_str()))
        .collect();
    assert!(
        titles.contains(&"casa"),
        "the first tab stayed home: {titles:?}"
    );
}

#[tokio::test]
async fn adding_a_branch_to_favorites_asks_its_name() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    let (row, generation) = branch_named(&s, "docs");
    let m = open_on_branch(&h, &mut sub, row, generation).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_labelled(&m, "ctx-add-favorite"),
    })
    .await
    .expect("host alive");
    let d = snapshot_until(&h, &mut sub, "the name dialog", |s| {
        s.dialogs
            .iter()
            .find(|d| d.title_key == "modal-hotlist-name-title")
            .cloned()
    })
    .await;
    assert_eq!(
        d.input.as_deref(),
        Some("docs"),
        "it suggests the branch's name"
    );
}

#[tokio::test]
async fn fold_from_the_menu_is_the_twisty() {
    let (h, _) = host_tree(deep_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    let before = tree_of(&s).total;
    for (what, grows) in [("docs unfolded", true), ("docs folded", false)] {
        let s = snapshot(&h, &mut sub).await;
        let (row, generation) = branch_named(&s, "docs");
        let m = open_on_branch(&h, &mut sub, row, generation).await;
        h.dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, "ctx-toggle-fold"),
        })
        .await
        .expect("host alive");
        snapshot_until(&h, &mut sub, what, |s| {
            let total = tree_of(s).total;
            (if grows {
                total > before
            } else {
                total == before
            })
            .then_some(())
        })
        .await;
    }
    assert!(
        primer_listing(&snapshot(&h, &mut sub).await)
            .path_display
            .ends_with("/casa"),
        "folding does not navigate"
    );
}

/// A branch menu whose tree moved underneath (another click folded or
/// unfolded something) runs nothing: the index names a different branch.
#[tokio::test]
async fn a_branch_that_moved_runs_nothing() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    let (row, generation) = branch_named(&s, "docs");
    let m = open_on_branch(&h, &mut sub, row, generation).await;
    h.dispatch(UiAction::TreeToggleRow { row: 0, generation })
        .await
        .expect("host alive");
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, "ctx-open-here"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    // The toggle's own snapshot (menu still up) may still be queued.
    let s = snapshot_until(&h, &mut sub, "the menu closed", |s| {
        s.context_menu.is_none().then(|| s.clone())
    })
    .await;
    assert!(primer_listing(&s).path_display.ends_with("/casa"));
}

#[tokio::test]
async fn sort_and_hide_from_the_header() {
    let (h, _) = host(vec!["a.txt", "bb.txt"]).await;
    let mut sub = h.subscribe();
    let slot = browser(&snapshot(&h, &mut sub).await).slot_id;
    for key in ["ctx-sort-by-column", "ctx-hide-column"] {
        h.dispatch(UiAction::ContextMenuHeader {
            slot_id: slot,
            column: "size".into(),
            x: 0,
            y: 0,
        })
        .await
        .expect("host alive");
        let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
        h.dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, key),
        })
        .await
        .expect("host alive");
        if key == "ctx-sort-by-column" {
            snapshot_until(&h, &mut sub, "sorted by size", |s| {
                browser(s)
                    .columns
                    .iter()
                    .any(|c| c.id == "size" && c.sort.is_some())
                    .then_some(())
            })
            .await;
        } else {
            snapshot_until(&h, &mut sub, "size hidden", |s| {
                (!browser(s).columns.iter().any(|c| c.id == "size")).then_some(())
            })
            .await;
        }
    }
    let after = snapshot(&h, &mut sub).await;
    assert!(
        browser(&after).columns.iter().any(|c| c.id == "name"),
        "the rest of the columns stay"
    );
}

#[tokio::test]
async fn shift_f10_in_places_opens_on_its_cursor() {
    let (h, mut sub, _, _) = with_a_drive().await;
    let places_slot = places(&snapshot(&h, &mut sub).await)
        .expect("placed")
        .slot_id;
    h.dispatch(UiAction::FocusSlot {
        slot_id: places_slot,
    })
    .await
    .expect("host alive");
    h.dispatch(key_mod("F10", false, true))
        .await
        .expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert_eq!((m.x, m.y), (None, None));
    // The cursor is born on the first row, the Drives header: fold only.
    let labels: Vec<&str> = m.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, [t("ctx-toggle-fold")], "{m:?}");
}

#[tokio::test]
async fn shift_f10_in_the_tree_opens_on_its_branch() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let s = tree_with_branches(&mut sub, 2).await;
    h.dispatch(UiAction::FocusSlot {
        slot_id: tree_of(&s).slot_id,
    })
    .await
    .expect("host alive");
    h.dispatch(key_mod("F10", false, true))
        .await
        .expect("host alive");
    let m = snapshot_until(&h, &mut sub, "menu", |s| s.context_menu.clone()).await;
    assert_eq!((m.x, m.y), (None, None));
    assert!(
        m.items.iter().any(|i| i.label == t("ctx-toggle-fold")),
        "{m:?}"
    );
    assert!(m.header.ends_with("/casa"), "{}", m.header);
}

/// One favorite `proyectos` in the settings AND on disk, in a temp user
/// layer; `broken` makes its target the config's parse error instead.
async fn with_a_favorite(
    tmp: &std::path::Path,
    broken: bool,
) -> (UiHost, norte_ui_host::UiSubscription, u32, u64) {
    norte_config::persist_hotlist_add(tmp, "proyectos", "mem:///proyectos").expect("seeded");
    let mut settings = test_settings();
    settings.common.hotlist = vec![norte_config::HotlistItem {
        name: "proyectos".to_owned(),
        target: if broken {
            Err("err-invalid-path".to_owned())
        } else {
            Ok(VPath::parse("mem:///proyectos").expect("vpath"))
        },
    }];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.put("mem:///proyectos", vec![(b"p.txt".to_vec(), false)]);
    // A drive, so "the volumes landed" (which renumbers the bar) is
    // something the wait below can see.
    f.volumes = vec![volume("mem:///otro", "ext4", false)];
    let h = host_places(
        f,
        "full",
        settings,
        norte_ui_host::settings::HostPaths {
            config_layers: vec![(
                norte_ui_host::settings::ConfigLayer::User,
                place(tmp.to_path_buf()),
            )],
            state_dir: None,
            logs_dir: None,
            socket: None,
        },
    )
    .await;
    let mut sub = h.subscribe();
    let (row, generation) = snapshot_until(&h, &mut sub, "the favorite row", |s| {
        let v = places(s)?;
        v.rows
            .iter()
            .any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Drive { .. }))
            .then_some(())?;
        let i = v
            .rows
            .iter()
            .position(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. }))?;
        Some((u32::try_from(i).expect("fits"), v.generation))
    })
    .await;
    (h, sub, row, generation)
}

#[tokio::test]
async fn removing_a_favorite_from_places() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (h, mut sub, row, generation) = with_a_favorite(tmp.path(), false).await;
    let m = open_on_place(&h, &mut sub, row, generation).await;
    h.dispatch(UiAction::ContextMenuActivateRow {
        row: row_labelled(&m, "ctx-remove-favorite"),
    })
    .await
    .expect("host alive");
    snapshot_until(&h, &mut sub, "the favorite is gone", |s| {
        (!places(s)?
            .rows
            .iter()
            .any(|r| matches!(r, norte_ui_host::dto::PlaceRowView::Favorite { .. })))
        .then_some(())
    })
    .await;
    let toml = std::fs::read_to_string(tmp.path().join("norte.toml")).expect("the file");
    assert!(
        !toml.contains("proyectos"),
        "and the disk forgot it: {toml}"
    );
}

/// A favorite whose target does not parse: the three Open verbs dim with
/// the broken reason; Copy path and Remove stay.
#[tokio::test]
async fn a_broken_favorite_dims_its_open_verbs() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (h, mut sub, row, generation) = with_a_favorite(tmp.path(), true).await;
    let m = open_on_place(&h, &mut sub, row, generation).await;
    for key in ["ctx-open-here", "ctx-open-in-other", "ctx-open-in-new-tab"] {
        let item = &m.items[row_labelled(&m, key) as usize];
        assert!(!item.enabled, "{key}: {item:?}");
        assert_eq!(item.reason, t("err-invalid-path"), "{key}");
    }
    let remove = &m.items[row_labelled(&m, "ctx-remove-favorite") as usize];
    assert!(remove.enabled, "{remove:?}");
    // A click on the dimmed one runs nothing and leaves the menu up.
    let ack = h
        .dispatch(UiAction::ContextMenuActivateRow {
            row: row_labelled(&m, "ctx-open-here"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert!(snapshot(&h, &mut sub).await.context_menu.is_some());
}
