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
