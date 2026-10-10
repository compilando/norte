use super::*;

// ---------------------------------------------------------------------------
// "Go anywhere" in the window (#357, phase 6).
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

/// The rows that get painted, without the headers.
fn rows(v: &norte_ui_host::dto::GotoView) -> Vec<&str> {
    v.lines
        .iter()
        .filter_map(|l| match l {
            norte_ui_host::dto::GotoLineView::Row { text, .. } => Some(text.as_str()),
            norte_ui_host::dto::GotoLineView::Header { .. } => None,
        })
        .collect()
}

/// `ctrl+g` opens "goto", and typing a PATH offers it at the top; `Enter`
/// goes there.
///
/// The typed path is the only row that does not come from a list, and the
/// decision of what counts as a path and where it leads belongs to the
/// shared model: the same as in the TUI.
#[tokio::test]
async fn a_typed_path_is_offered_and_enter_goes_there() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let opened = next_goto(&mut sub).await.expect("\"goto\" opens");
    assert!(opened.query.is_empty(), "starts with no query");

    for c in "mem:///casa/docs".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let v = snap.goto.expect("still open");
    assert_eq!(
        rows(&v).first().copied(),
        Some("mem:///casa/docs"),
        "the typed path goes first: {:?}",
        v.lines
    );
    assert!(
        matches!(
            v.lines
                .get(usize::try_from(v.cursor.expect("cursor")).expect("index")),
            Some(norte_ui_host::dto::GotoLineView::Row { .. })
        ),
        "the cursor is on a row, never on a header"
    );

    h.dispatch(press("Enter")).await.expect("host alive");
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        assert!(snap.goto.is_none(), "\"goto\" closes on confirm");
        if listing(&snap).path_display.ends_with("docs") {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("Enter did not take the pane to the typed path");
}

/// A COMMAND runs from "goto" through the same path as a keystroke: its rows
/// are this window's palette's, behind `>`, in one flat list.
#[tokio::test]
async fn a_command_runs_like_its_key() {
    let (h, snap) = host_tree(fake_tree()).await;
    let cursor_before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    for c in ">cursor.bottom".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let v = next_snapshot(&mut sub).await.goto.expect("open");
    assert!(
        !v.lines.is_empty()
            && v.lines
                .iter()
                .all(|l| matches!(l, norte_ui_host::dto::GotoLineView::Row { .. })),
        "commands come out as one flat list: {:?}",
        v.lines
    );
    h.dispatch(press("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.goto.is_none(), "closes");
    assert_ne!(listing(&snap).cursor, cursor_before, "and the command ran");
}

/// A typed PATH is never asked of the semantic index, but a word is.
///
/// Sending `/home/u/secret` to an embeddings provider — maybe a remote one —
/// is sending it the reader's directory name, and a path is not a
/// meaning query.
#[tokio::test]
async fn a_typed_path_does_not_go_to_the_index() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    for c in "/casa/secreto".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(ctrl_g()).await.expect("host alive");
    for c in "facturas".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    let requested = backend
        .until("a query to the index", |f| {
            let p = f.semanticas_requested.lock().expect("semantics").clone();
            (!p.is_empty()).then_some(p)
        })
        .await;
    assert!(
        requested.iter().all(|(q, _)| !q.starts_with('/')),
        "no path went to the index: {requested:?}"
    );
}

/// `Escape` closes without going anywhere.
#[tokio::test]
async fn escape_closes_without_going_anywhere() {
    let (h, snap) = host_tree(fake_tree()).await;
    let before = listing(&snap).path_display.clone();
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    h.dispatch(press("/")).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    h.dispatch(press("Escape")).await.expect("host alive");
    // A snapshot and not the next patch: connections answer in the
    // background and their patch can arrive in between.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.goto.is_none(), "closes");
    assert_eq!(listing(&snap).path_display, before, "and did not navigate");
}

/// `?` lists help; Enter on a topic opens help AT that page, and the box
/// closes in a patch of its own.
#[tokio::test]
async fn a_help_row_opens_that_page() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    // The window's language in tests is Spanish: type its title's first word.
    for c in "?Copiar".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.goto.is_none(), "the box closed");
    let help = snap.help.expect("help is open");
    assert_eq!(help.topic_id, "copying");
}

/// A hover moves the cursor (a header is not a row and is ignored); a click
/// runs the line the host has open — the same as Enter on it.
#[tokio::test]
async fn hovering_points_and_clicking_runs_a_line() {
    let (h, snap) = host_tree(fake_tree()).await;
    let before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    for c in ">cursor.down".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let g = next_snapshot(&mut sub).await.goto.expect("open");
    assert_eq!(g.mode, norte_ui_host::dto::GotoModeView::Commands);
    let norte_ui_host::dto::GotoLineView::Row {
        category,
        chord,
        positions,
        ..
    } = &g.lines[0]
    else {
        panic!("commands mode has no headers");
    };
    assert!(!category.is_empty() && !chord.is_empty());
    assert!(
        positions.is_empty(),
        "matched by the id (desc), nothing marked in the label"
    );
    h.dispatch(UiAction::GotoPointRow { row: 0 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::GotoActivateRow { row: 0 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.goto.is_none(), "running closes the box");
    assert_ne!(listing(&snap).cursor, before, "cursor.down ran");
}

/// A click on a row the host does not have is ignored; with no box open it
/// is stale.
#[tokio::test]
async fn a_click_on_a_row_the_host_lacks_is_ignored() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    h.dispatch(UiAction::GotoActivateRow { row: 99_999 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(next_snapshot(&mut sub).await.goto.is_some(), "still open");
    h.dispatch(press("Escape")).await.expect("host alive");
    let ack = h
        .dispatch(UiAction::GotoActivateRow { row: 0 })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "{ack:?}"
    );
}

/// The empty places box carries its hint; the menu carries the box's key.
#[tokio::test]
async fn the_places_box_says_its_prefixes_and_the_menu_its_key() {
    let (h, snap) = host_tree(fake_tree()).await;
    assert!(!snap.menu.goto_chord.is_empty(), "orthodox binds app.goto");
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let g = next_snapshot(&mut sub).await.goto.expect("open");
    assert_eq!(g.mode, norte_ui_host::dto::GotoModeView::Places);
    assert_eq!(g.hint, norte_i18n::t_in(norte_i18n::Lang::Es, "goto-hint"));
}

/// A paste lands in the query — its first line only: a newline must never
/// confirm, and the rest of a multi-line paste is not a query.
#[tokio::test]
async fn a_paste_lands_in_the_query_first_line_only() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    h.dispatch(UiAction::GotoPaste {
        text: ">copy\nrm -rf".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let g = next_snapshot(&mut sub).await.goto.expect("still open");
    assert_eq!(g.query, ">copy");
    // Without the box open, a paste is stale and changes nothing.
    h.dispatch(press("Escape")).await.expect("host alive");
    let ack = h
        .dispatch(UiAction::GotoPaste {
            text: "x".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "{ack:?}"
    );
}
