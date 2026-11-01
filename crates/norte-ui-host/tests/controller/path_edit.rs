use super::*;

// ---------------------------------------------------------------------------
// The path line (`pane.edit-path`, ADR 0190): the slot's path, editable in
// place of the breadcrumbs.
// ---------------------------------------------------------------------------

fn edit_of(snap: &norte_ui_host::ViewSnapshot) -> Option<norte_ui_host::dto::PathEditView> {
    listing(snap).path_edit.clone()
}

/// Opens the line and returns the slot it opened on and what the renderer
/// is told to paint.
async fn opened(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> (u32, norte_ui_host::dto::PathEditView) {
    let ack = execute_via_palette_ack(h, sub, "pane.edit-path").await;
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    snapshot_until(h, sub, "the path line open", |s| {
        edit_of(s).map(|v| (listing(s).slot_id, v))
    })
    .await
}

fn commit(slot_id: u32, revision: u32, text: &str) -> UiAction {
    UiAction::PathEditCommit {
        slot_id,
        revision,
        text: text.to_owned(),
    }
}

/// It opens on the slot's own path, all of it selected, the caret at the
/// end in the `<input>`'s units; Escape puts it away.
#[tokio::test]
async fn it_opens_on_the_slots_path_selected() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, v) = opened(&h, &mut sub).await;
    assert_eq!(v.text, "mem:///casa");
    assert!(v.select_all);
    assert_eq!(v.cursor, 11);
    h.dispatch(UiAction::PathEditCancel {
        slot_id: id,
        revision: v.revision,
    })
    .await
    .expect("host alive");
    snapshot_until(&h, &mut sub, "the line put away", |s| {
        edit_of(s).is_none().then_some(())
    })
    .await;
}

/// A cancel from an `<input>` that was already replaced (its blur arrives
/// late) does not close the line that replaced it.
#[tokio::test]
async fn a_stale_cancel_leaves_the_current_line() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, v) = opened(&h, &mut sub).await;
    h.dispatch(UiAction::PathEditCancel {
        slot_id: id,
        revision: v.revision.wrapping_sub(1),
    })
    .await
    .expect("host alive");
    let still = snapshot_until(&h, &mut sub, "the line still open", edit_of).await;
    assert_eq!(still.revision, v.revision);
}

/// Enter goes to what the line says, and the line goes with it.
#[tokio::test]
async fn enter_goes_to_the_typed_path() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, v) = opened(&h, &mut sub).await;
    let ack = h
        .dispatch(commit(id, v.revision, "mem:///casa/docs"))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    snapshot_until(&h, &mut sub, "the slot in docs, line closed", |s| {
        (listing(s).path_display.ends_with("docs") && edit_of(s).is_none()).then_some(())
    })
    .await;
}

/// A text that names no folder says why and leaves the line open with
/// what was typed: the reader fixes it instead of retyping.
#[tokio::test]
async fn a_text_that_is_no_path_keeps_the_line() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, before) = opened(&h, &mut sub).await;
    let ack = h
        .dispatch(commit(id, before.revision, "docs"))
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let es = norte_i18n::t_in(norte_i18n::Lang::Es, "msg-goto-bad-path");
    snapshot_until(&h, &mut sub, "the reason in the status bar", |s| {
        (s.status.message.as_deref() == Some(es.as_str())).then_some(())
    })
    .await;
    let v = snapshot_until(&h, &mut sub, "the line still open with the text", |s| {
        edit_of(s).filter(|v| v.text == "docs")
    })
    .await;
    assert_eq!(v.revision, before.revision, "the <input> is not replaced");
}

/// The `<input>` lets in what the terminal's line never would: a bidi
/// override is refused by the host, not navigated.
#[tokio::test]
async fn a_text_the_line_refuses_is_not_navigated() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, before) = opened(&h, &mut sub).await;
    h.dispatch(commit(id, before.revision, "mem:///casa/do\u{202E}cs"))
        .await
        .expect("host alive");
    let es = norte_i18n::t_in(norte_i18n::Lang::Es, "msg-path-edit-refused");
    snapshot_until(&h, &mut sub, "the refusal in the status bar", |s| {
        (s.status.message.as_deref() == Some(es.as_str()) && edit_of(s).is_some()).then_some(())
    })
    .await;
}

/// A folder that cannot be listed gives the typed text back, unselected,
/// once its listing fails.
#[tokio::test]
async fn a_failed_listing_gives_the_text_back() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, before) = opened(&h, &mut sub).await;
    h.dispatch(commit(id, before.revision, "mem:///casa/nada"))
        .await
        .expect("host alive");
    let v = snapshot_until(&h, &mut sub, "the line given back", |s| {
        edit_of(s).filter(|v| v.revision != before.revision)
    })
    .await;
    assert_eq!(v.text, "mem:///casa/nada");
    assert!(!v.select_all);
}

/// Tab completes a folder of the slot (`docs`, not `notas.txt`), and the
/// answer is a new revision with the caret after it.
#[tokio::test]
async fn tab_completes_a_folder_of_the_slot() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let (id, before) = opened(&h, &mut sub).await;
    h.dispatch(UiAction::PathEditComplete {
        slot_id: id,
        revision: before.revision,
        text: "mem:///casa/".to_owned(),
        cursor: 12,
    })
    .await
    .expect("host alive");
    let v = snapshot_until(&h, &mut sub, "the completed line", |s| {
        edit_of(s).filter(|v| v.revision != before.revision)
    })
    .await;
    assert_eq!(v.text, "mem:///casa/docs/");
    assert_eq!(v.cursor, 17);
    assert!(!v.select_all);
}
