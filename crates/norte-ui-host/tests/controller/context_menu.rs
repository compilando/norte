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
    assert!(m.header.contains('2'), "{}", m.header);
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
