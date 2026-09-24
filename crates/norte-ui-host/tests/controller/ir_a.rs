use super::*;

// ---------------------------------------------------------------------------
// "Go anywhere" in the window (#357, phase 6).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries "goto".
async fn next_goto(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::GotoView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Goto { goto } = c {
                    return goto.clone();
                }
            }
        }
    }
    panic!("no update with \"goto\" ever arrived");
}

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
/// are this window's palette's.
#[tokio::test]
async fn a_command_runs_like_its_key() {
    let (h, snap) = host_tree(fake_tree()).await;
    let cursor_before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(ctrl_g()).await.expect("host alive");
    let _ = next_goto(&mut sub).await;
    for c in "cursor.bottom".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let v = next_snapshot(&mut sub).await.goto.expect("open");
    assert!(
        v.lines.iter().any(|l| matches!(
            l,
            norte_ui_host::dto::GotoLineView::Header { title } if title == "Comandos"
        )),
        "commands come out with their header: {:?}",
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
            let p = f.semanticas_pedidas.lock().expect("semantics").clone();
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
