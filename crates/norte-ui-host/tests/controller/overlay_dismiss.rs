use super::*;

// ---------------------------------------------------------------------------
// A press on the veil behind a popup (`overlay_dismiss`, bridge 112).
// ---------------------------------------------------------------------------

/// The sort menu at the foot of the window (the columns picker) closes with
/// a click outside it, as with `Escape`.
#[tokio::test]
async fn a_click_outside_closes_the_sort_menu() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.sort-menu").await;
    assert!(snapshot(&h, &mut sub).await.columns.is_some(), "open");
    h.dispatch(UiAction::OverlayDismiss)
        .await
        .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.columns.is_none(), "closed");
}

/// The search box closes with a click outside it.
#[tokio::test]
async fn a_click_outside_closes_the_search_box() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "app.goto").await;
    assert!(snapshot(&h, &mut sub).await.goto.is_some(), "open");
    h.dispatch(UiAction::OverlayDismiss)
        .await
        .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.goto.is_none(), "closed");
}

/// The search form travels as a dialog, and is a form, not a question: a
/// click outside closes it, as in the terminal.
#[tokio::test]
async fn a_click_outside_closes_the_search_form() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.search").await;
    assert_eq!(snapshot(&h, &mut sub).await.dialogs.len(), 1, "open");
    h.dispatch(UiAction::OverlayDismiss)
        .await
        .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.dialogs.is_empty(), "closed");
}

/// A whole screen stays: settings.
#[tokio::test]
async fn a_click_outside_leaves_settings_open() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "app.settings").await;
    assert!(snapshot(&h, &mut sub).await.settings.is_some(), "open");
    h.dispatch(UiAction::OverlayDismiss)
        .await
        .expect("host alive");
    assert!(snapshot(&h, &mut sub).await.settings.is_some(), "stays");
}

/// A question is not answered by a stray click: the delete confirmation
/// stays.
#[tokio::test]
async fn a_click_outside_leaves_a_question_open() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    assert_eq!(next_dialogs(&mut sub).await.len(), 1);
    h.dispatch(UiAction::OverlayDismiss)
        .await
        .expect("host alive");
    assert_eq!(snapshot(&h, &mut sub).await.dialogs.len(), 1, "still asks");
}
