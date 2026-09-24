use super::*;

// ---------------------------------------------------------------------------
// Searching the subtree (task 6.1).
// ---------------------------------------------------------------------------

/// Waits for the next update with the search.
pub(super) async fn siguiente_busqueda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SearchView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Search { search } = c {
                    return search.clone();
                }
            }
        }
    }
    panic!("no update with a search ever arrived");
}

/// A tree with hits prepared for a pattern.
pub(super) fn arbol_con_hallazgos(pattern: &str, paths: &[&str]) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        hallazgos: [(
            pattern.to_owned(),
            paths
                .iter()
                .map(|r| norte_proto::VPath::parse(r).expect("vpath"))
                .collect(),
        )]
        .into_iter()
        .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// Searching opens its prompt, launches the Task and hits arrive in batches:
/// the view opens ALREADY saying it is running, and fills up afterward.
#[tokio::test]
async fn searching_opens_its_view_and_hits_arrive_in_batches() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt", "mem:///casa/docs"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    // The prompt asks for the pattern.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt asks for a pattern");
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "name".to_owned(),
        value: norte_ui_host::action::DialogFieldValue::Text {
            text: "*.txt".to_owned(),
        },
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let first = siguiente_busqueda(&mut sub).await.expect("the view opens");
    assert_eq!(first.query, "*.txt");
    assert!(
        first.running,
        "it opens ALREADY saying it is running: waiting for the first \
         batch is a window that does not react to a key that did do something"
    );

    let mut with_rows = None;
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            with_rows = Some(v);
            break;
        }
    }
    let v = with_rows.expect("hits arrive");
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].name, "notas.txt");
    assert!(
        !v.rows[0].parent.is_empty(),
        "and where it is: {:?}",
        v.rows[0]
    );
    assert!(
        !v.status.is_empty() && !v.status.starts_with("search-status"),
        "the status phrase comes translated: {:?}",
        v.status
    );
    assert_eq!(
        backend.busquedas.lock().expect("mutex").as_slice(),
        &["*.txt".to_owned()],
        "and the pattern reached the wire as-is"
    );
}

/// Zero skipped is NOT a warning, and any that there are read as a warning.
///
/// The window used to use its own key — "N entries were skipped", with no
/// mark that makes it read as a warning — and painted it EVEN with N equal
/// to zero: i.e. it announced an incomplete listing that was complete,
/// spending the only signal there is for when something is really missing.
#[tokio::test]
async fn the_skipped_notice_stays_silent_at_zero_and_is_marked_above_it() {
    for (skipped, expects_notice) in [(None, false), (Some(0), false), (Some(2), true)] {
        let mut f = Falso::default();
        f.arbol.clone_from(&arbol().arbol);
        f.omitidas = skipped;
        let (_h, snap) = host_arbol(Arc::new(f)).await;
        let note = &listado_de(&snap, 1).skipped_note;

        assert_eq!(
            !note.is_empty(),
            expects_notice,
            "with {skipped:?} skipped the note was {note:?}"
        );
        if expects_notice {
            assert!(
                note.contains('⚠'),
                "a warning with no mark reads as a plain counter: {note:?}"
            );
            assert!(note.contains('2'), "{note:?}");
        }
    }
}

/// And the header says the other three things only the terminal used to say.
///
/// All three under the same rule: a listing that shows less than there is —
/// or does not show what there is — is never silent. The NAMES one was the
/// most costly: the window transcribed with another encoding and said so
/// nowhere except the toggle's message, which the next key carries away.
#[tokio::test]
async fn the_header_says_names_are_reinterpreted_and_how_much_is_marked() {
    let (h, snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let before = listado_de(&snap, 1);
    assert_eq!(
        before.names_note, "",
        "with nothing reinterpreted it says nothing"
    );
    assert_eq!(
        before.marked_note, "",
        "whoever marks nothing gains no noise"
    );

    ejecutar_por_paleta(&h, &mut sub, "pane.names-encoding").await;
    let with_names = foto_hasta(&h, &mut sub, "the header with the encoding", |s| {
        let b = listado_de(s, 1);
        (!b.names_note.is_empty()).then(|| b.names_note.clone())
    })
    .await;
    assert!(
        !with_names.contains("status-names"),
        "translated, not the key: {with_names}"
    );

    marca_todo(&h, &mut sub, 1).await;
    let marked = foto_hasta(&h, &mut sub, "the header with what is marked", |s| {
        let b = listado_de(s, 1);
        (!b.marked_note.is_empty()).then(|| b.marked_note.clone())
    })
    .await;
    assert!(
        !marked.contains("status-marked"),
        "translated, not the key: {marked}"
    );
}

/// A search that FAILED does not read as one that finished with no hits.
///
/// The host used to mark any terminal state as "no longer alive" and paint
/// `search-status-done`, so a search that broke on the second directory and
/// another that walked the whole tree said the same thing: "0 hits". That is
/// not an interface imprecision — it is a false claim about the disk, and
/// whoever reads it stops searching.
#[tokio::test]
async fn a_failed_search_says_so_and_does_not_fake_zero_hits() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.desenlace_de_busqueda = Some(norte_proto::TaskState::Failed {
        error: norte_proto::Error::PermissionDenied,
    });
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let view = foto_hasta(&h, &mut sub, "the search with its outcome", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    let zero_hits = norte_i18n::ta_in(norte_i18n::Lang::Es, "search-status-done", &[("n", "0")]);
    assert_ne!(
        view.status, zero_hits,
        "a broken search is NOT a search with no results"
    );
    assert!(
        view.status
            .contains(&norte_frontend::error::error_category_in(
                norte_i18n::Lang::Es,
                &norte_proto::Error::PermissionDenied
            )),
        "and it says WHY it broke: {}",
        view.status
    );
}

/// And one that did not even get to be QUEUED stops saying it is searching.
///
/// The other path, and the one with no test: there is no Task there, so
/// there is no progress to carry the outcome. The view used to stay at
/// "searching…" forever while the error went through the bar and the next
/// key carried it away.
#[tokio::test]
async fn a_search_that_never_gets_queued_stops_saying_it_is_searching() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.error_de_busqueda = Some(norte_proto::Error::ProviderUnavailable { retryable: false });
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let view = foto_hasta(&h, &mut sub, "the search that never started", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    assert!(
        view.status
            .contains(&norte_frontend::error::error_category_in(
                norte_i18n::Lang::Es,
                &norte_proto::Error::ProviderUnavailable { retryable: false }
            )),
        "it says why it never started, and persistently: {}",
        view.status
    );
}

/// And one the reader CANCELLED does not either: what was found holds, what
/// is missing was never looked at.
#[tokio::test]
async fn a_cancelled_search_does_not_read_as_finished() {
    let mut f = Falso::default();
    f.arbol.clone_from(&arbol().arbol);
    f.desenlace_de_busqueda = Some(norte_proto::TaskState::Cancelled);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    let _ = buscar(&h, &mut sub, "*.txt").await;
    let view = foto_hasta(&h, &mut sub, "the cancelled search", |s| {
        s.search.clone().filter(|b| !b.running)
    })
    .await;

    assert_eq!(
        view.status,
        norte_i18n::ta_in(
            norte_i18n::Lang::Es,
            "search-status-cancelled",
            &[("n", &view.rows.len().to_string())]
        ),
        "cancelled has its own phrase, distinct from the terminal one"
    );
}

/// Launches the `pattern` search through the prompt and returns its first
/// view.
pub(super) async fn buscar(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    pattern: &str,
) -> norte_ui_host::dto::SearchView {
    por_la_paleta(h, sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(sub).await;
    // Search's is a FORM since bridge 91: it is located by its fields, not by
    // a single-box dialog's `input` — which is still what semantic search's
    // is.
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt asks for a pattern");
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "name".to_owned(),
        value: norte_ui_host::action::DialogFieldValue::Text {
            text: pattern.to_owned(),
        },
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    siguiente_busqueda(sub).await.expect("the view opens")
}

/// Closing a search with NO hits still cancels it.
///
/// The search used to be named by its first batch, and the core does NOT
/// send empty batches (`norte-core/src/search.rs`: `if batch.is_empty() {
/// return FlushOutcome::Continue }`). So over a tree with no matches the id
/// never arrived, `esc` had nothing to cancel, and the daemon kept walking
/// the whole subtree for a surface already closed. Cancellation existed and
/// was unreachable: rule 3 broken from the UI side.
#[tokio::test]
async fn closing_a_search_with_no_hits_cancels_it_anyway() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let v = buscar(&h, &mut sub, "*.zzz").await;
    assert_eq!(v.query, "*.zzz");
    assert!(v.rows.is_empty(), "there is nothing to find");

    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub).await.search.is_none(),
        "the view closes"
    );
    assert_eq!(
        backend.cancelaciones.load(Ordering::SeqCst),
        1,
        "and the Task gets cancelled EVEN THOUGH not one batch arrived: it \
         is the only thing that stops the daemon"
    );
}

/// A stray batch from the PREVIOUS search does not fill the new one's list.
///
/// The old search's forwarder is not aborted — its `tokio::spawn` keeps no
/// handle — so it can keep spitting out batches after `esc`. With the search
/// being named by its first batch, whichever arrived first christened it:
/// the PREVIOUS search's hits filled the list labelled with the NEW query,
/// and `enter` navigated to a file matching the old pattern.
#[tokio::test]
async fn a_batch_from_the_previous_search_does_not_fill_the_new_one() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The first finds something; it is closed before looking at it.
    let _ = buscar(&h, &mut sub, "*.txt").await;
    h.dispatch(tecla("Escape")).await.expect("host alive");

    // The second finds nothing.
    let v = buscar(&h, &mut sub, "*.zzz").await;
    assert_eq!(v.query, "*.zzz");

    // And it keeps finding nothing no matter how much the mailbox is
    // drained: whatever is left of the first is not its own.
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        let Some(s) = snap.search else { continue };
        assert!(
            s.rows.is_empty(),
            "a `*.txt` batch cannot show up under `*.zzz`: {:?}",
            s.rows
        );
    }
}

/// A modal dialog keeps the keyboard.
///
/// `tecla_de_un_overlay` routed nine surfaces and NOT the dialog, which is
/// the only one with a real `aria-modal`, so keys fell to the listing
/// UNDERNEATH: with a name prompt open, `Backspace` navigated to the parent
/// and `Enter` entered the directory under the cursor instead of confirming.
/// It is the surface where a file name's bytes get approved, and the one
/// that in phase 5 will ask before deleting.
#[tokio::test]
async fn a_dialog_keeps_the_keyboard() {
    let backend = arbol_con_hallazgos("*.txt", &["mem:///casa/notas.txt"]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // The cursor is put on a DIRECTORY, which is what `Enter` would open.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let before = siguiente_foto(&mut sub).await;
    let SlotView::Browser(b0) = &before.slots[0] else {
        panic!("the first slot is a listing");
    };
    let where_ = b0.path_display.clone();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the prompt asks for a pattern")
        .clone();

    // Navigation keys do NOT reach the listing underneath.
    for k in ["Backspace", "ArrowDown", "Home"] {
        h.dispatch(tecla(k)).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let during = siguiente_foto(&mut sub).await;
    let SlotView::Browser(b1) = &during.slots[0] else {
        panic!("the first slot is a listing");
    };
    assert_eq!(
        b1.path_display, where_,
        "the panel underneath has not moved: the modal keeps the keys"
    );
    assert!(
        during.dialogs.iter().any(|d| d.id == dialog.id),
        "and the dialog is still open"
    );

    // `Enter` CONFIRMS the dialog, it does not open the directory under the
    // cursor.
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "name".to_owned(),
        value: norte_ui_host::action::DialogFieldValue::Text {
            text: "*.txt".to_owned(),
        },
    })
    .await
    .expect("host alive");
    h.dispatch(tecla("Enter")).await.expect("host alive");
    let v = siguiente_busqueda(&mut sub)
        .await
        .expect("confirming with the keyboard launches the search");
    assert_eq!(v.query, "*.txt");
}

/// `Escape` cancels the topmost dialog, and only the dialog.
#[tokio::test]
async fn escape_cancels_the_topmost_dialog() {
    let backend = arbol_con_hallazgos("*.txt", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        siguiente_foto(&mut sub)
            .await
            .dialogs
            .iter()
            .any(|d| !d.fields.is_empty()),
        "the prompt opens"
    );
    h.dispatch(tecla("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.dialogs.is_empty(), "and `esc` closes it");
    assert!(
        snap.search.is_none(),
        "with nothing launched: cancelling is cancelling"
    );
}

/// Going to a result navigates to its DIRECTORY and leaves the cursor on top
/// of it, with no path reconstruction.
#[tokio::test]
async fn going_to_a_result_navigates_and_leaves_the_cursor_on_it() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"hallado.md".to_vec(), false)],
    );
    f.hallazgos = [(
        "hallado*".to_owned(),
        vec![norte_proto::VPath::parse("mem:///casa/docs/hallado.md").expect("vpath")],
    )]
    .into_iter()
    .collect();
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the prompt is there");
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "name".to_owned(),
        value: norte_ui_host::action::DialogFieldValue::Text {
            text: "hallado*".to_owned(),
        },
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            break;
        }
    }

    h.dispatch(UiAction::SearchActivateRow { row: 0 })
        .await
        .expect("host alive");
    let mut arrived = false;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        if listado(&snap).path_display.contains("docs") {
            assert!(snap.search.is_none(), "the search closes on going");
            let under_cursor = listado(&snap)
                .rows
                .iter()
                .find(|r| Some(r.key) == listado(&snap).cursor)
                .map(|r| r.display_name.clone());
            assert_eq!(
                under_cursor.as_deref(),
                Some("hallado.md"),
                "and the cursor ends up ON the hit, matched byte for byte"
            );
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the panel navigated to the hit's directory");
}

/// An empty pattern launches nothing and says so: it would match the whole
/// tree.
#[tokio::test]
async fn an_empty_pattern_launches_nothing() {
    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the prompt is there");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = siguiente_foto(&mut sub).await;
    assert!(after.search.is_none(), "no search was opened");
    assert!(
        backend.busquedas.lock().expect("mutex").is_empty(),
        "and nothing reached the wire"
    );
}

/// **The window's filters reach the wire** (bridge 91).
///
/// This was the gap: the terminal has offered seven fields and four switches
/// since protocol 0.81.0 and this window sent a name glob and nothing else.
/// A filter that does not apply does not look like a missing feature — it
/// looks like a search that found more things.
///
/// Checked against what REACHED THE WIRE and not against the view: what has
/// to be shown is that the form turns into parameters, not that it paints
/// nicely.
#[tokio::test]
async fn the_forms_filters_reach_the_wire() {
    use norte_ui_host::action::DialogFieldValue as Value;

    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt is a form");

    // The seven fields and the five controls, with their stable ids.
    let ids: Vec<&str> = dialog.fields.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "name",
            "content",
            "exclude",
            "min-size",
            "max-size",
            "days",
            "encoding",
            "regex",
            "case",
            "whole-word",
            "recursive",
            "kinds",
        ],
        "the fields travel in the order they are painted"
    );

    // A FILTER alone is already a search: "everything heavier than a
    // megabyte" needs no name at all.
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "min-size".to_owned(),
        value: Value::Text {
            text: "1M".to_owned(),
        },
    })
    .await
    .expect("host alive");
    // And a switch does not send its target state: it says it was TOUCHED.
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "recursive".to_owned(),
        value: Value::Toggled,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let requested = backend.params_busqueda.lock().expect("mutex").clone();
    let params = requested.first().expect("the search reached the wire");
    assert_eq!(params.min_size, Some(1024 * 1024), "\"1M\" is bytes");
    assert!(
        !params.recursive,
        "the subfolders switch got turned off: {params:?}"
    );
    assert!(
        params.name_glob.is_none() && params.name_regex.is_none(),
        "and no name, because none was typed"
    );
}

/// **An unreadable field does NOT close the form.**
///
/// Validation lives before popping the dialog off the stack, and not inside
/// what runs afterward: there the form is already gone, and a badly written
/// `1 gigabyte` used to take down all twelve controls while the notice
/// pointed at a field that no longer existed — advice that cannot be
/// followed.
#[tokio::test]
async fn an_unreadable_field_does_not_take_down_the_form() {
    use norte_ui_host::action::DialogFieldValue as Value;

    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt is a form");

    for (field, text) in [("name", "*.rs"), ("min-size", "1 gigabyte")] {
        h.dispatch(UiAction::DialogField {
            id: dialog.id,
            field: field.to_owned(),
            value: Value::Text {
                text: text.to_owned(),
            },
        })
        .await
        .expect("host alive");
    }

    let ack = h
        .dispatch(UiAction::Dialog {
            id: dialog.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "search-bad-field"),
        "it refuses naming the reason: {ack:?}"
    );

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = siguiente_foto(&mut sub).await;
    let still = after
        .dialogs
        .iter()
        .find(|d| d.id == dialog.id)
        .expect("the form is still open");
    let value = |id: &str| {
        still
            .fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    assert_eq!(value("name"), "*.rs", "and what was typed was not lost");
    assert_eq!(value("min-size"), "1 gigabyte");
    assert!(
        backend.busquedas.lock().expect("mutex").is_empty(),
        "and nothing reached the wire"
    );
}

/// The class cycle goes and comes back, and what reaches the wire is its
/// class.
#[tokio::test]
async fn the_class_cycle_sends_the_class_to_the_wire() {
    use norte_ui_host::action::DialogFieldValue as Value;

    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt is a form");

    // One turn of the cycle: "anything" → "files".
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "kinds".to_owned(),
        value: Value::Cycled,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    let requested = backend.params_busqueda.lock().expect("mutex").clone();
    let params = requested.first().expect("the search reached the wire");
    assert_eq!(
        params.kinds,
        vec![norte_proto::EntryKind::File],
        "the cycle left \"files\", and a single class is already a criterion"
    );
}

/// A field this form does not have is refused NAMING IT, and it is not
/// answered "resync": the dialog is open and is the same one, so saying it
/// is stale would hide the renderer's bug.
#[tokio::test]
async fn a_field_that_does_not_exist_is_refused_without_faking_a_stale_modal() {
    use norte_ui_host::action::DialogFieldValue as Value;

    let backend = arbol_con_hallazgos("*", &[]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the search prompt is a form");

    let ack = h
        .dispatch(UiAction::DialogField {
            id: dialog.id,
            field: "no-existe".to_owned(),
            value: Value::Text {
                text: "x".to_owned(),
            },
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-unknown-field"),
        "{ack:?}"
    );

    // And a switch receives no text nor is a text field "toggled".
    let ack = h
        .dispatch(UiAction::DialogField {
            id: dialog.id,
            field: "name".to_owned(),
            value: Value::Toggled,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-unknown-field"),
        "each control answers its own: {ack:?}"
    );
}

/// A hostile name reaches the results masked and MARKED.
#[tokio::test]
async fn a_hostile_hit_is_marked() {
    let hostile = "mem:///casa/ca%CC%81f%C3%A9%E2%80%AE.txt";
    let backend = arbol_con_hallazgos("*", &[hostile]);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    por_la_paleta(&h, &mut sub, "pane.search").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    let dialog = snap
        .dialogs
        .iter()
        .find(|d| !d.fields.is_empty())
        .expect("the prompt is there");
    h.dispatch(UiAction::DialogField {
        id: dialog.id,
        field: "name".to_owned(),
        value: norte_ui_host::action::DialogFieldValue::Text {
            text: "*".to_owned(),
        },
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: dialog.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        let Some(v) = siguiente_busqueda(&mut sub).await else {
            continue;
        };
        if let Some(row) = v.rows.first() {
            assert!(
                !row.name.contains('\u{202e}'),
                "a bidi override crossed over raw: {:?}",
                row.name
            );
            assert!(row.hostile, "and it is MARKED: {row:?}");
            return;
        }
    }
    panic!("the hits never arrived");
}

// ---------------------------------------------------------------------------
// Semantic search (task 6.1).
// ---------------------------------------------------------------------------

/// The window asks the index by MEANING, and what comes back is navigated
/// like any other hit.
///
/// The catalogue bound `pane.semantic-search` from the keymap and the host
/// answered `NotHere`: the capability existed in the daemon and in the TUI,
/// and here there was no way to request it.
#[tokio::test]
async fn the_window_searches_by_meaning() {
    let fake = arbol_como_falso();
    *fake.semanticos.lock().expect("semánticos") = Some(vec![
        norte_proto::methods::SemanticHit {
            path: VPath::parse("mem:///casa/docs/a.md").expect("vpath"),
            score: 0.91,
        },
        norte_proto::methods::SemanticHit {
            path: VPath::parse("mem:///casa/notas.txt").expect("vpath"),
            score: 0.42,
        },
    ]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "facturas del año pasado".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    // The view opens ALREADY, empty and running; hits arrive afterward.
    let mut view = siguiente_busqueda(&mut sub)
        .await
        .expect("the search opens");
    for _ in 0..20 {
        if !view.rows.is_empty() {
            break;
        }
        view = siguiente_busqueda(&mut sub).await.expect("still open");
    }
    assert!(view.semantic, "the view says this is semantic");
    assert_eq!(view.rows.len(), 2);
    assert_eq!(view.rows[0].name, "a.md");
    // The score is SHOWN: without it, two hits at 0.91 and 0.42 read as
    // equally good and the order looks arbitrary.
    assert!(view.rows[0].score.is_some_and(|s| s > 0.9));
    let requested = backend.semanticas_pedidas.lock().expect("pedidas").clone();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0].0, "facturas del año pasado");
    assert!(
        requested[0].1 <= norte_proto::methods::INDEX_SEMANTIC_MAX_K,
        "k is clamped to what the daemon accepts: {}",
        requested[0].1
    );
}

/// With no index, it SAYS what is missing and how to fix it.
///
/// `NotFound` here is not "no results": it is "this root has no rows in the
/// index", and confusing it with an empty search leaves the reader believing
/// there is nothing like what they searched for.
#[tokio::test]
async fn a_semantic_search_with_no_index_says_it_needs_building() {
    let fake = arbol_como_falso();
    // With no `semanticos`: the fake answers `NotFound`.
    let (h, _snap) = host_arbol(Arc::new(fake)).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "lo que sea".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    for _ in 0..40 {
        if siguiente_aviso(&mut sub).await == "msg-semantic-no-index" {
            return;
        }
    }
    panic!("nobody said the index needs building");
}

/// An EMPTY query does not leave the process.
#[tokio::test]
async fn an_empty_semantic_query_is_not_sent() {
    let fake = arbol_como_falso();
    let backend = Arc::new(fake);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.semantic-search").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    asentar().await;
    assert!(
        backend
            .semanticas_pedidas
            .lock()
            .expect("pedidas")
            .is_empty()
    );
}

/// An empty AI instruction leaves the field IN FRONT, like its twin.
///
/// The terminal leaves the modal open with the error underneath. The window
/// had already swallowed the dialog and put the message on the bar: a "write
/// an instruction" over a screen with nowhere to write it is not a refusal,
/// it is a dead end. Semantic search — the same case, three files over — had
/// already been fixed this way.
#[tokio::test]
async fn an_empty_ai_instruction_returns_the_field() {
    let backend = Arc::new(arbol_como_falso());
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.ai-rename").await;
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    asentar().await;

    let snap = foto(&h, &mut sub).await;
    assert!(
        snap.dialogs.iter().any(|d| d.input.is_some()),
        "the field comes back: {:?}",
        snap.dialogs
    );
    assert!(
        backend.instrucciones.lock().expect("pedidas").is_empty(),
        "and nothing goes out to the AI provider"
    );
}

/// In READ ONLY it is not even asked: the query leaves the process toward the
/// AI provider, same as the rename plan.
#[tokio::test]
async fn in_read_only_there_is_no_semantic_search() {
    let fake = arbol_como_falso();
    let backend = Arc::new(fake);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset_con(
            "orthodox",
            norte_ui_host::commands::Efectos::SoloLectura,
        )
        .expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::SoloLectura,
        log_ring: None,
    })
    .await
    .expect("starts");

    let mut sub = h.subscribe();
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    let palette = siguiente_paleta(&mut sub).await.expect("the palette opens");
    // The palette does not carry the dispatch key — it is chosen by index —
    // so it is looked up by label, which is what the reader sees.
    let label =
        norte_frontend::whichkey::command_label("pane.semantic-search", norte_i18n::Lang::Es);
    assert!(
        !palette.rows.iter().any(|r| r.text == label),
        "a window with no effects does not offer asking a model: {:?}",
        palette
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        backend
            .semanticas_pedidas
            .lock()
            .expect("pedidas")
            .is_empty()
    );
}

/// Runs a command through the PALETTE, which is the way to reach whatever no
/// preset binds (semantic search is one).
pub(super) async fn ejecutar_por_paleta(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    command: &str,
) {
    let ack = ejecutar_por_paleta_ack(h, sub, command).await;
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the palette could not run `{command}`: {ack:?}"
    );
}

/// Throws away everything the host has already published and nobody has
/// read.
///
/// It does not wait: what is not there now was not pending. A zero deadline
/// does not work — `recv` needs one turn to see what is already queued — so
/// it is given one millisecond, which is plenty for what is already
/// published and far too little to wait for something that has not happened
/// yet.
async fn drenar_fotos(sub: &mut norte_ui_host::controller::UiSubscription) {
    while tokio::time::timeout(std::time::Duration::from_millis(1), sub.recv())
        .await
        .is_ok()
    {}
}

/// Like [`ejecutar_por_paleta`], but returning the ACK: what is sometimes
/// checked is the refusal.
pub(super) async fn ejecutar_por_paleta_ack(
    h: &UiHost,
    sub: &mut norte_ui_host::controller::UiSubscription,
    command: &str,
) -> ActionAck {
    let label = command.to_owned();
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = siguiente_paleta(sub).await;
    for c in label.chars().skip(5).take(6) {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    for _ in 0..40 {
        // What is PENDING is thrown away before requesting the snapshot.
        // This loop is a stateful walk — it reads the cursor, presses down,
        // reads again — so a stale snapshot makes it count the same step
        // twice and land on the command next to it: this was seen with
        // `pane.edit`, which ended up running `pane.edit-new`, a dialog
        // instead of an effect.
        //
        // Anything that publishes a snapshot on its own triggers it: the
        // answer to an attribute catalogue, a volume, a capability. Throwing
        // them away is correct because the only one that matters is the one
        // after `Resync`, which is by definition the newest.
        drenar_fotos(sub).await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = siguiente_foto(sub)
            .await
            .palette
            .expect("the palette is still open");
        assert!(
            !p.rows.is_empty(),
            "`{command}` does not show up in the palette with the query `{}`",
            p.query
        );
        let i = p
            .cursor
            .and_then(|c| usize::try_from(c).ok())
            .unwrap_or(0)
            .min(p.rows.len() - 1);
        if p.rows[i].text == label {
            return h.dispatch(tecla("Enter")).await.expect("host alive");
        }
        // `ArrowDown`, not `Down`: the palette accepts the browser's name or
        // the project's, lowercase, and `Down` is neither — this helper had
        // been working since phase 2 only when the sought command happened
        // to land FIRST.
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    panic!("`{command}` does not appear among what the filter leaves");
}
