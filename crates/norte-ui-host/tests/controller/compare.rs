use super::*;

// ---------------------------------------------------------------------------
// Comparing directories (task 6.2).
// ---------------------------------------------------------------------------

/// Waits for the next update with the diff panel.
pub(super) async fn next_comparison(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::CompareView> {
    for _ in 0..40 {
        match tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("arrives")
            .expect("the host is still alive")
        {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Compare { compare } = c {
                            return compare.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no update with a comparison ever arrived");
}

/// A compared row, with the bare minimum to paint it.
pub(super) fn row_compared(
    id: u64,
    left: Option<&str>,
    right: Option<&str>,
    verdict: norte_proto::methods::CompareVerdict,
) -> norte_proto::methods::CompareRow {
    let entry = |wire: &str| norte_proto::Entry {
        path: VPath::parse(wire).expect("vpath"),
        kind: norte_proto::EntryKind::File,
        size: Some(10),
        mtime_ms: Some(1),
        attrs: std::collections::BTreeMap::new(),
    };
    norte_proto::methods::CompareRow {
        id,
        left: left.map(entry),
        right: right.map(entry),
        verdict,
        criterion: norte_proto::methods::CompareCriterion::Size,
        confidence: norte_proto::methods::CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
        paired_under: None,
    }
}

/// Comparing the two panes opens the diff panel with what the core answered,
/// without pairing anything again here.
#[tokio::test]
async fn comparing_the_two_panes_opens_the_diff_panel() {
    let fake = tree_as_fake();
    *fake.rows_compared.lock().expect("filas") = Some(vec![
        row_compared(
            1,
            Some("mem:///casa/notas.txt"),
            Some("mem:///casa/docs/notas.txt"),
            norte_proto::methods::CompareVerdict::Same,
        ),
        row_compared(
            2,
            Some("mem:///casa/solo.txt"),
            None,
            norte_proto::methods::CompareVerdict::OnlyLeft,
        ),
    ]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;

    run_by_palette(&h, &mut sub, "pane.compare-dirs").await;

    let mut view = next_comparison(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if !view.rows.is_empty() {
            break;
        }
        view = next_comparison(&mut sub).await.expect("still open");
    }
    assert_eq!(view.rows.len(), 2, "{view:?}");
    assert_eq!(view.total, 2);
    // Verdicts and categories come from the SHARED model, already
    // translated: the renderer does not decide what "same" is.
    assert_eq!(view.rows[0].category, "same");
    assert_eq!(view.rows[1].category, "only-left");
    assert!(view.rows[1].right.is_none(), "an orphan has no right side");
    // And it did request comparing the two real directories.
    let requested = backend.comparisons.lock().expect("comparaciones").clone();
    assert_eq!(requested.len(), 1);
    assert_ne!(requested[0].0, requested[0].1);
}

/// A filter hides a whole category, and does NOT renumber: the selection
/// still names the same row.
#[tokio::test]
async fn a_filter_hides_a_category_and_does_not_renumber() {
    let fake = tree_as_fake();
    *fake.rows_compared.lock().expect("filas") = Some(vec![
        row_compared(
            1,
            Some("mem:///casa/notas.txt"),
            Some("mem:///casa/docs/notas.txt"),
            norte_proto::methods::CompareVerdict::Same,
        ),
        row_compared(
            2,
            Some("mem:///casa/solo.txt"),
            None,
            norte_proto::methods::CompareVerdict::OnlyLeft,
        ),
    ]);
    let (h, _snap) = host_con_layout(Arc::new(fake), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.compare-dirs").await;
    let mut view = next_comparison(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if view.rows.len() == 2 {
            break;
        }
        view = next_comparison(&mut sub).await.expect("still open");
    }

    h.dispatch(UiAction::CompareSelectRow { id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::CompareToggleFilter {
        category: "same".to_owned(),
    })
    .await
    .expect("host alive");
    // There are patches queued (the selection produced its own): it reads
    // until the one that already carries the filter set.
    let mut filtered = next_comparison(&mut sub).await.expect("still open");
    for _ in 0..20 {
        if filtered.rows.len() == 1 {
            break;
        }
        filtered = next_comparison(&mut sub).await.expect("still open");
    }
    assert_eq!(
        filtered.rows.len(),
        1,
        "the hidden category does not travel"
    );
    assert_eq!(
        filtered.selected,
        Some(2),
        "and the selection is still its own"
    );
    assert!(
        filtered
            .filters
            .iter()
            .any(|f| f.id == "same" && f.hidden && f.count == 1),
        "the filter says how many it hides: {:?}",
        filtered.filters
    );
}

/// Opening a row navigates to the ACTIVE side, and a row whose active side
/// is empty does not fall back to the other side.
#[tokio::test]
async fn opening_an_orphan_on_its_empty_side_does_not_fall_back() {
    let fake = tree_as_fake();
    *fake.rows_compared.lock().expect("filas") = Some(vec![row_compared(
        1,
        None,
        Some("mem:///casa/docs/a.md"),
        norte_proto::methods::CompareVerdict::OnlyRight,
    )]);
    let (h, _snap) = host_con_layout(Arc::new(fake), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.compare-dirs").await;
    let mut view = next_comparison(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if !view.rows.is_empty() {
            break;
        }
        view = next_comparison(&mut sub).await.expect("still open");
    }

    // The active side is the LEFT one, and this row has no left.
    let ack = h
        .dispatch(UiAction::CompareActivateRow { id: 1 })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "compare-no-target".to_owned()
        },
        "{ack:?}"
    );
}

/// Leaves both panes in DIFFERENT directories: comparing the same one with
/// itself twice is not a comparison, and the host refuses it before queuing
/// anything.
pub(super) async fn separate_the_panes(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
) {
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    // The first row is the `docs` directory: directories come first.
    h.dispatch(press("Enter")).await.expect("host alive");
    for _ in 0..20 {
        let snap = {
            h.dispatch(UiAction::Resync).await.expect("host alive");
            next_snapshot(sub).await
        };
        let in_docs = snap.slots.iter().any(|s| match s {
            SlotView::Browser(b) => b.path_display.ends_with("/casa/docs"),
            _ => false,
        });
        if in_docs {
            break;
        }
    }
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
}
