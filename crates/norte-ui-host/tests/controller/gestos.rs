use super::*;

// ---------------------------------------------------------------------------
// #290 phase A: the panel gestures the TUI had and the window did not.
//
// None of them invents a model. What these tests check is that the window
// uses the SHARED one — `SortSpec::after_click`, `PaneState::toggle_hidden`,
// `History::entries`, ADR 0058's `Target` role — and that what it cannot do
// it SAYS, instead of staying silent.
// ---------------------------------------------------------------------------

/// The column being sorted by, and in which direction, read from the headers
/// that cross the bridge: it is the only thing the renderer knows about the
/// sort.
pub(super) fn orden_de(b: &norte_ui_host::dto::BrowserSlotView) -> (String, String) {
    let marked: Vec<&norte_ui_host::dto::ColumnHeader> =
        b.columns.iter().filter(|c| c.sort.is_some()).collect();
    assert_eq!(
        marked.len(),
        1,
        "a single column carries the sort mark: {:?}",
        b.columns
    );
    (
        marked[0].id.clone(),
        marked[0].sort.clone().expect("marked"),
    )
}

/// A snapshot right now.
pub(super) async fn foto(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host alive");
    siguiente_foto(sub).await
}

/// Waits for a SNAPSHOT to satisfy `cond`, with no interval clocks.
///
/// It has to be ASKED FOR: neither `total_rows` nor `path_display` travel in
/// a patch — only in a snapshot, and a snapshot is requested by `Resync` —
/// so just listening is not enough. What this helper does not do is sleep 25
/// ms between attempt and attempt: it BLOCKS on the host's next message, so
/// it moves at the host's pace and not the scheduler's. The overall deadline
/// is there so a condition that never holds reads as a failure with its own
/// message, and not as a hung test.
pub(super) async fn esperar_foto(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    que: &str,
    cond: impl Fn(&norte_ui_host::ViewSnapshot) -> bool,
) -> norte_ui_host::ViewSnapshot {
    let wait = async {
        loop {
            let f = foto(h, sub).await;
            if cond(&f) {
                return f;
            }
            // ANY message from the host, not the next SNAPSHOT: snapshots
            // are requested by the renderer, and a drain travels whole in
            // PATCHES. Waiting for another snapshot was waiting for the host
            // to send one on its own, which is exactly what it does not do:
            // the loop used to hang instead of exhausting the deadline, and
            // a hung test does not say what failed.
            let _ = sub.recv().await.expect("the host is still alive");
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("deadline exhausted waiting for {que}"))
}

/// `pane.sort-size` sorts by size and repeating it REVERSES it.
///
/// The same semantics as a click on the header because it is the SAME path:
/// what decides is `SortSpec::after_click`, not a table per surface.
#[tokio::test]
async fn the_sort_command_is_the_click_on_the_header() {
    let (h, snap) = host_arbol(arbol()).await;
    let (initial_column, _) = orden_de(listado(&snap));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let after = foto(&h, &mut sub).await;
    let (column, direction) = orden_de(listado(&after));
    assert_ne!(column, initial_column, "it sorts by ANOTHER column");
    assert_eq!(direction, "asc", "a new column starts ascending");

    ejecutar_por_paleta(&h, &mut sub, "pane.sort-size").await;
    let again = foto(&h, &mut sub).await;
    let (same, direction) = orden_de(listado(&again));
    assert_eq!(same, column, "it is still the same column");
    assert_eq!(direction, "desc", "the active column REVERSES");
}

/// `pane.sort-menu` opens no new screen: it opens the COLUMNS picker, where
/// the column, the direction and `dirs_first` all live. It is the TUI's
/// decision, and two screens for the same thing would be another one to
/// maintain and another one to learn.
#[tokio::test]
async fn the_sort_menu_is_the_columns_picker() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.sort-menu").await;
    let after = foto(&h, &mut sub).await;
    assert!(
        after.columns.is_some(),
        "the sort menu is the columns picker"
    );
}

/// `pane.toggle-hidden` sets dotfiles aside from the panel and ANNOUNCES it.
///
/// A bar notice expires after `[ui] notice_seconds` seconds (spec
/// 2026-09-10): it leaves `status.message` and `notices_unread` counts one
/// more. The SNAPSHOT is waited for, with no sleeping: the one-second tick
/// belongs to the host.
#[tokio::test]
async fn a_notice_expires_and_leaves_a_badge() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b".oculto".to_vec(), false)],
    );
    let backend = Arc::new(f);
    let mut settings = ajustes_de_prueba();
    settings.common.ui_chrome.notice_seconds = Some(1);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    assert_eq!(snap.status.notices_unread, 0);
    let mut sub = h.subscribe();
    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let after = foto(&h, &mut sub).await;
    assert!(after.status.message.is_some(), "hiding is said in the bar");
    foto_hasta(&h, &mut sub, "the notice expired into the badge", |f| {
        (f.status.message.is_none() && f.status.notices_unread == 1).then_some(())
    })
    .await;
}

/// Presentation-only (#107): the provider does not list again, so the
/// backend sees no extra request.
#[tokio::test]
async fn hiding_is_presentation_and_says_so() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let before = listado(&snap).rows.len();
    assert!(
        listado(&snap)
            .rows
            .iter()
            .any(|r| r.display_name == ".oculto"),
        "with no `[ui] show_hidden` in the config, everything shows"
    );
    let listings = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let after = foto(&h, &mut sub).await;
    assert!(
        listado(&after)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "the hidden ones got set aside"
    );
    assert_eq!(
        backend.listados(),
        listings,
        "and they got set aside WITHOUT re-requesting the directory"
    );
    assert!(
        after.status.message.is_some(),
        "a listing that shrinks without saying why reads as a bug"
    );

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let again = foto(&h, &mut sub).await;
    assert_eq!(
        listado(&again).rows.len(),
        before,
        "and it returns them, without re-listing"
    );
}

/// `pane.names-encoding` changes how a non-UTF-8 name is PAINTED, and not the
/// bytes: the row stays marked as hostile and its key is still valid.
#[tokio::test]
async fn cycling_the_encoding_repaints_without_touching_the_bytes() {
    let (h, snap) = host_arbol(arbol()).await;
    let hostile = listado(&snap)
        .rows
        .iter()
        .find(|r| r.hostile)
        .expect("the tree brings a non-UTF-8 name");
    let (key, painted) = (hostile.key, hostile.display_name.clone());
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.names-encoding").await;
    let after = foto(&h, &mut sub).await;
    let same = listado(&after)
        .rows
        .iter()
        .find(|r| r.key == key)
        .expect("the key is still valid: the bytes did not change");
    assert_ne!(same.display_name, painted, "it paints differently");
    assert!(
        after.status.message.is_some(),
        "and it says what it is being reinterpreted with"
    );
}

/// And the HEADER repaints along with the rows, with no snapshot requested.
///
/// `pane.names-encoding` transcribes the names, and the directory's own path
/// is one more name: if its bytes are not UTF-8, the header has to be
/// reinterpreted the same as the rows. It used to answer with a rows patch
/// and a status patch, and the header only travels in the whole snapshot —
/// so rows got retranscribed and the title kept the old reading, which is
/// exactly the half-fix #57 and #293 say cannot happen: the mojibake stays
/// on top and the reader does not know whether the command did anything.
///
/// Checked with NO `Resync`, which is the only thing the renderer has, and
/// without going through the palette — opening and closing it sends
/// snapshots that would fix the header by accident: the preset's key, like a
/// human.
#[tokio::test]
async fn cycling_the_encoding_also_repaints_the_header() {
    let mut fake = Falso::default();
    // A directory whose OWN name is not UTF-8.
    fake.pon("mem:///caf%FF", vec![(b"a.txt".to_vec(), false)]);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: norte_proto::VPath::parse("mem:///caf%FF").expect("wire"),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
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
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let before = listado(&snap).path_display.clone();
    assert!(
        before.contains('\u{fffd}'),
        "to start with, the bytes cannot be painted: {before}"
    );
    let mut sub = h.subscribe();

    // `alt+e` is `pane.names-encoding` in the `orthodox` preset. By hand and
    // not through `tecla_mod`, which fixes `alt: false`.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "e".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    asentar().await;

    let mut header = None;
    tokio::time::pause();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Update::Message(m) = sub.recv().await.expect("host alive") {
                match m.payload {
                    UiUpdate::Patch(p) => {
                        for c in &p.changes {
                            if let norte_ui_host::dto::ViewChange::BrowserHeader {
                                path_display,
                                ..
                            } = c
                            {
                                header = Some(path_display.clone());
                            }
                        }
                    }
                    UiUpdate::Snapshot(s) => {
                        header = Some(listado(&s).path_display.clone());
                    }
                    UiUpdate::Notice(_) => {}
                }
                if header.is_some() {
                    return;
                }
            }
        }
    })
    .await;
    tokio::time::resume();

    let after = header.expect(
        "cycling the encoding does not repaint the header: rows get \
         retranscribed and the title keeps the old reading",
    );
    assert_ne!(
        after, before,
        "the path is reinterpreted the same as the rows"
    );
}

/// A clicked breadcrumb (bridge 65) takes the slot to the ancestor at that
/// depth — the breadcrumb's OWN slot, not the active one — and the current
/// directory's does not move anything.
#[tokio::test]
async fn a_breadcrumb_takes_you_to_its_slots_ancestor() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    assert_eq!(
        listado_de(&snap, 2).path_segments,
        vec!["⟨mem⟩", "casa", "docs"],
        "the root and one segment per directory"
    );
    let mut sub = h.subscribe();
    let generation = listado_de(&snap, 2).generation;

    // The current directory's breadcrumb: applied, and nothing moves.
    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 2,
        generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let same = siguiente_foto(&mut sub).await;
    assert!(listado_de(&same, 2).path_display.ends_with("/casa/docs"));

    // A breadcrumb from ANOTHER generation — the slot navigated between
    // painting and the click — is stale: it is not reinterpreted over the
    // new path.
    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 1,
        generation: generation + 1,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let stale = siguiente_foto(&mut sub).await;
    assert!(listado_de(&stale, 2).path_display.ends_with("/casa/docs"));

    h.dispatch(UiAction::BreadcrumbActivate {
        slot_id: 2,
        depth: 1,
        generation,
    })
    .await
    .expect("host alive");
    let f = esperar_foto(&h, &mut sub, "slot 2 goes up to /casa", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
    })
    .await;
    assert_eq!(listado_de(&f, 2).path_segments, vec!["⟨mem⟩", "casa"]);
    assert!(
        listado_de(&f, 1).path_display.ends_with("/casa"),
        "the active slot did not move"
    );
}

/// Activating a row of the panel that does NOT have focus focuses it AND
/// enters it: it is the mouse's double click, and the renderer sends focus
/// and activation as two messages. If the first one did not apply, the
/// second one cannot stay silent — `fila_de` used to refuse it for "another
/// slot" and a double click on the panel next to it did nothing.
#[tokio::test]
async fn activating_a_row_in_the_other_panel_focuses_it_and_enters() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    // Slot 1 is on `/casa` and has the `docs` directory; focus is moved to 2
    // so 1 becomes "the other panel".
    let a = listado_de(&snap, 1);
    let (generation, key) = a
        .rows
        .iter()
        .find(|r| r.display_name.contains("docs"))
        .map(|r| (a.generation, r.key))
        .expect("the directory's row");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    let _ = sub.recv().await.expect("the host is still alive");

    // And now the OTHER panel's activation, with no `focus_slot` in front.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let f = esperar_foto(&h, &mut sub, "slot 1 enters /casa/docs", |f| {
        listado_de(f, 1).path_display.ends_with("/casa/docs")
    })
    .await;
    assert!(
        listado_de(&f, 2).path_display.ends_with("/casa/docs"),
        "and the other panel stays where it was"
    );
}

/// `pane.refresh` re-requests ALL visible listings, not just the focused
/// one: what changes a directory underneath is a change on DISK, and a disk
/// change does not respect focus.
#[tokio::test]
async fn refreshing_re_lists_both_panels() {
    let backend = arbol();
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let before = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.refresh").await;
    esperar_foto(&h, &mut sub, "both panels re-list", |_| {
        backend.listados() >= before + 2
    })
    .await;
}

/// `pane.mirror` sends the ACTIVE panel's location to the target panel, and
/// the target comes from the shared role — never from "the one next to it".
#[tokio::test]
async fn mirroring_sends_the_location_to_the_target() {
    let (h, snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    // The scenario leaves 2 on `/casa/docs` and focus on 1, which is still on
    // `/casa`: mirroring has to bring 2 back.
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let f = esperar_foto(&h, &mut sub, "the target follows the active one", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
            && listado_de(f, 1).path_display.ends_with("/casa")
    })
    .await;
    assert_eq!(f.focus, Some(1), "mirroring does not move focus");
}

/// `pane.mirror-target` sends the FOLDER UNDER THE CURSOR, not the location:
/// Krusader's `Ctrl+←`/`Ctrl+→`, and the same answer the TUI gives because
/// what decides it is `PaneState::target_dir` (ADR 0077).
#[tokio::test]
async fn mirroring_the_target_sends_the_cursors_folder() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    // Both on `/casa` to start: this way what gets measured afterward is the
    // cursor's TARGET and not the scenario's drift.
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    esperar_foto(&h, &mut sub, "both on /casa", |f| {
        listado_de(f, 2).path_display.ends_with("/casa")
    })
    .await;

    // The cursor to row 0, which here is the `docs` directory: these
    // settings bring the `..` row OFF (`ui_parent_entry = false`), and the
    // scenario leaves the cursor where its own navigation left it.
    ejecutar_por_paleta(&h, &mut sub, "cursor.top").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.mirror-target").await;
    let f = esperar_foto(&h, &mut sub, "the target enters the folder", |f| {
        listado_de(f, 2).path_display.ends_with("/casa/docs")
    })
    .await;
    assert!(
        listado_de(&f, 1).path_display.ends_with("/casa"),
        "the focused panel does not move: {}",
        listado_de(&f, 1).path_display
    );
    assert_eq!(f.focus, Some(1), "nor does focus");
}

/// `pane.pull` is the same gesture backward: the location comes from the
/// target and the focused panel travels.
#[tokio::test]
async fn pulling_moves_the_focused_panel() {
    let (h, _snap) = crate::dos_paneles_con_destino_aparte(arbol()).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.pull").await;
    let f = esperar_foto(
        &h,
        &mut sub,
        "the focused panel brings in the other location",
        |f| listado_de(f, 1).path_display.ends_with("/casa/docs"),
    )
    .await;
    assert!(
        listado_de(&f, 2).path_display.ends_with("/casa/docs"),
        "the other one stays where it was"
    );
}

/// `pane.swap` swaps the two listings' places WITHOUT touching disk: nobody
/// requests a directory again, and focus stays where it was.
#[tokio::test]
async fn swapping_requests_nothing_from_the_backend() {
    let backend = arbol();
    let (h, snap) = crate::dos_paneles_con_destino_aparte(Arc::clone(&backend)).await;
    assert!(listado_de(&snap, 1).path_display.ends_with("/casa"));
    assert!(listado_de(&snap, 2).path_display.ends_with("/casa/docs"));
    let listings = backend.listados();
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.swap").await;
    let after = foto(&h, &mut sub).await;
    assert!(
        listado_de(&after, 1).path_display.ends_with("/casa/docs"),
        "the focused panel shows the other one's: {}",
        listado_de(&after, 1).path_display
    );
    assert!(listado_de(&after, 2).path_display.ends_with("/casa"));
    assert_eq!(after.focus, Some(1), "focus does not move with the gesture");
    assert_eq!(
        backend.listados(),
        listings,
        "both listings already existed: swapping them does not touch disk"
    );
}

/// The PAINTING window does not travel with the swap.
///
/// `first_visible`/`visible` are set by the renderer per slot with
/// `set_visible_range`, and its `scrollTop` is its own: a swap does not move
/// it nor fire a scroll event. If the window travelled with the slot, each
/// panel would paint rows from a band the reader does not have in front of
/// them and BOTH would look EMPTY. With short listings and both scrolled to
/// the top, nothing shows, which is why this test is needed and not the one
/// next to it.
#[tokio::test]
async fn swapping_does_not_move_the_painting_window() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..300).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 200,
        count: 30,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 2,
        first: 0,
        count: 30,
    })
    .await
    .expect("host alive");
    let before = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&before, 1).first_visible, 200);
    assert_eq!(listado_de(&before, 2).first_visible, 0);

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host alive");
    let after = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&after, 1).first_visible,
        200,
        "the slot that was on row 200 still paints from 200: the \
         renderer's scroll has not moved"
    );
    assert_eq!(
        listado_de(&after, 2).first_visible,
        0,
        "and the one that was at the top is still at the top"
    );
}

/// A swap during a NAVIGATION re-requests it, pointed at where it was going.
///
/// The in-flight response travels tagged with its slot: after the swap it
/// arrives at the wrong slot and gets discarded by witness. Without
/// re-requesting it, the panel stays `Loading` forever.
#[tokio::test]
async fn swapping_re_requests_the_in_flight_navigation() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"informe.pdf".to_vec(), false)]);
    // Slow enough for the swap to land WITHIN the navigation.
    f.retraso_ms = 400;
    let (h, snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    let b1 = listado_de(&snap, 1);
    let docs = b1
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs.key,
        generation: b1.generation,
    })
    .await
    .expect("host alive");
    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host alive");

    let f = esperar_foto(
        &h,
        &mut sub,
        "the interrupted navigation reaches its destination",
        |f| {
            listado_de(f, 2).path_display.ends_with("/casa/docs")
                && listado_de(f, 1).path_display.ends_with("/casa")
        },
    )
    .await;
    let (left, right) = (listado_de(&f, 1), listado_de(&f, 2));
    assert!(
        !matches!(left.state, norte_ui_host::dto::SlotState::Loading { .. })
            && !matches!(right.state, norte_ui_host::dto::SlotState::Loading { .. }),
        "no panel stays loading: {:?} {:?}",
        left.state,
        right.state
    );
}

/// A swap during the DRAIN also re-requests it.
///
/// `en_vuelo` dies with the first page and `drenando` stays alive: in a
/// directory of more than a hundred entries — i.e. almost any — there is a
/// window in which only the drain is alive. Looking only at `en_vuelo` left
/// the listing frozen at a hundred entries, in `Ready` and saying nothing,
/// and marking everything acted on that slice.
#[tokio::test]
async fn swapping_re_requests_the_drain() {
    let gate = Arc::new(backend_falso::Puerta::default());
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        (0..250).map(|i| (format!("f{i:03}").into_bytes(), false)),
    );
    f.puerta_drenaje = Some(Arc::clone(&gate));
    let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();

    // The first page is already on screen and the rest is still stopped at
    // the gate: THIS is the state the bug needed.
    esperar_foto(&h, &mut sub, "the first page lands", |f| {
        listado_de(f, 1).total_rows == Some(100)
    })
    .await;

    h.dispatch(tecla_mod("u", true, false))
        .await
        .expect("host alive");
    gate.abrir();

    esperar_foto(
        &h,
        &mut sub,
        "the drain gets re-requested and arrives whole",
        |f| listado_de(f, 1).total_rows == Some(250) && listado_de(f, 2).total_rows == Some(250),
    )
    .await;
}

/// `pane.toggle-hidden` PRUNES the marks of what it sets aside, and says so.
///
/// `PaneState::pruned_marks`'s contract is that a selection feeding a
/// batch operation never shrinks silently: staying quiet about it would copy
/// fewer files than the reader marked, while they believe all of them are
/// going.
#[tokio::test]
async fn hiding_says_which_marks_it_takes_with_it() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".env".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let hidden = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == ".env")
        .expect("hidden ones show by default");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 1,
        key: hidden.key,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host alive");
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.toggle-hidden").await;
    let after = foto(&h, &mut sub).await;

    assert_eq!(listado_de(&after, 1).marks, 0, "the mark left with the row");
    let said = after.status.message.clone().unwrap_or_default();
    assert!(
        said.contains('1') && said.contains("caída") || said.contains("caídas"),
        "and it SAYS how many fell off: {said:?}"
    );
}

/// A REDUNDANT mirror does not touch the other panel.
///
/// Both already show the same thing, so a cd would be re-listing:
/// `set_listing` erases its marks — a navigation, unlike a refresh, does not
/// restore them — and shifts its listing under the cursor, for nothing. The
/// TUI refuses it for the same reason (`gestures::mirror_plan`).
#[tokio::test]
async fn a_redundant_mirror_does_not_erase_the_others_marks() {
    let backend = arbol();
    let (h, snap) = host_con_layout(Arc::clone(&backend), "orthodox", (120, 40)).await;
    let b2 = listado_de(&snap, 2);
    // `fila_de` REJECTS a slot that is not the active one, so focus goes to
    // 2 first and back to 1. What is tested here is the mirror.
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::ToggleMark {
        slot_id: 2,
        key: b2.rows[0].key,
        generation: b2.generation,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host alive");
    let mut sub = h.subscribe();
    let before = foto(&h, &mut sub).await;
    assert_eq!(listado_de(&before, 2).marks, 1);
    let listings = backend.listados();

    ejecutar_por_paleta(&h, &mut sub, "pane.mirror").await;
    let after = foto(&h, &mut sub).await;

    assert_eq!(
        listado_de(&after, 2).marks,
        1,
        "both were already in the same place: the mark is still set"
    );
    assert_eq!(
        backend.listados(),
        listings,
        "and nothing was requested again"
    );
}

/// A panel gesture with no other panel SAYS so, and with the phrase that
/// tells apart "there is no other" from "there are several, pick one".
#[tokio::test]
async fn a_gesture_with_no_other_panel_says_so() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    for cmd in ["pane.mirror", "pane.pull", "pane.swap"] {
        let ack = ejecutar_por_paleta_ack(&h, &mut sub, cmd).await;
        match ack {
            ActionAck::Unavailable { reason_key } => {
                assert_eq!(reason_key, "host-no-other-slot", "{cmd}");
            }
            other => panic!("{cmd} with a single panel: {other:?}"),
        }
    }
}

/// `pane.history` shows the panel's trail, and choosing a row navigates.
///
/// The rows are `History::entries`'s — the shared MRU: what a panel
/// remembers and in what order cannot depend on who paints it.
#[tokio::test]
async fn history_is_the_shared_trail() {
    let (h, snap) = host_arbol(arbol()).await;
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there");
    let (key, generation) = (docs.key, listado(&snap).generation);
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host alive");
    let mut after = siguiente_foto(&mut sub).await;
    while !listado(&after).path_display.ends_with("/casa/docs") {
        after = siguiente_foto(&mut sub).await;
    }

    ejecutar_por_paleta(&h, &mut sub, "pane.history").await;
    let open = foto(&h, &mut sub).await;
    let picker = open.picker.expect("history is open");
    assert!(
        picker.rows.iter().any(|r| r.label.ends_with("/casa")),
        "the place it left is in the trail: {:?}",
        picker.rows
    );

    h.dispatch(tecla("Enter")).await.expect("host alive");
    esperar_foto(&h, &mut sub, "picking from history navigates", |f| {
        f.picker.is_none() && listado(f).path_display.ends_with("/casa")
    })
    .await;
}

/// `pane.hotlist` shows the configuration's favorites, and one whose path
/// fails to parse STAYS with its notice: the hotlist is user data, and a
/// favorite that disappears silently is a bug nobody can see.
#[tokio::test]
async fn an_invalid_favorite_stays_and_says_so() {
    let mut settings = ajustes_de_prueba();
    settings.common.hotlist = vec![
        norte_config::HotlistItem {
            name: "casa".to_owned(),
            target: Ok(dir()),
        },
        norte_config::HotlistItem {
            name: "roto".to_owned(),
            target: Err("err-invalid-path".to_owned()),
        },
    ];
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.hotlist").await;
    let open = foto(&h, &mut sub).await;
    let picker = open.picker.expect("favorites are open");
    assert_eq!(
        picker.rows.len(),
        2,
        "the invalid one does NOT fall off the list"
    );
    assert_eq!(picker.rows[1].label, "roto");
    assert!(
        !picker.rows[1].detail.is_empty(),
        "and its detail says the path is invalid"
    );

    // The cursor on the invalid one: choosing it cannot navigate anywhere,
    // and staying silent is indistinguishable from a broken key.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let ack = h.dispatch(tecla("Enter")).await.expect("host alive");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "hotlist-invalid"),
        other => panic!("an invalid favorite: {other:?}"),
    }
}

/// `pane.select-drive-left` names a SIDE of the screen, not the focus: with
/// focus on the right panel, the picker is still the left one's.
#[tokio::test]
async fn per_side_volumes_do_not_follow_focus() {
    // Both directions, because an implementation that read focus would pass
    // either one on its own: what has to be pinned down is that the panel
    // that MOVES is the named side's and the other stays untouched.
    for (command, moved, still) in [
        ("pane.select-drive-left", 1_u32, 2_u32),
        ("pane.select-drive-right", 2, 1),
    ] {
        let mut f = Falso::default();
        f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
        f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
        f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
        let (h, _snap) = host_con_layout(Arc::new(f), "orthodox", (200, 60)).await;
        // Focus is set on the OPPOSITE panel from the one the command names.
        h.dispatch(UiAction::FocusSlot { slot_id: still })
            .await
            .expect("host alive");
        let mut sub = h.subscribe();

        ejecutar_por_paleta(&h, &mut sub, command).await;
        let with_rows = foto_hasta(&h, &mut sub, "the mount table", |f| {
            f.picker
                .as_ref()
                .is_some_and(|p| !p.rows.is_empty())
                .then(|| f.picker.clone())
        })
        .await;
        assert!(
            with_rows.is_some(),
            "{command}: the mount table reaches the picker"
        );

        h.dispatch(tecla("Enter")).await.expect("host alive");
        let mounted = foto_hasta(
            &h,
            &mut sub,
            &format!("{command}: the volume mounted on side {moved}'s slot"),
            |f| {
                (f.picker.is_none() && listado_de(f, moved).path_display.contains("otro"))
                    .then(|| f.clone())
            },
        )
        .await;
        assert!(
            listado_de(&mounted, still).path_display.ends_with("/casa"),
            "{command}: the focused panel has NOT moved: {}",
            listado_de(&mounted, still).path_display
        );
    }
}

/// This window's properties are the `metadata` slot, which already shows the
/// name, class, size and date of what is pointed at. The window does it
/// differently, just like it sorts by pressing the header.
#[tokio::test]
async fn properties_opens_the_attributes_sheet() {
    let (h, snap) = host_arbol(arbol()).await;
    assert!(
        !snap
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "by default `simple` brings no attributes sheet"
    );
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.properties").await;
    let after = foto(&h, &mut sub).await;
    assert!(
        after
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Metadata(_))),
        "properties opens the sheet"
    );
}

/// Sorting and hiding COME BACK from the session.
///
/// They used to be written and nobody read them back: the window remembered
/// where you were and forgot how you were looking at it, so sorting by size
/// or setting dotfiles aside only lasted until closing.
#[tokio::test]
async fn the_session_returns_the_sort_and_the_hidden_ones() {
    let mut fake = Falso::default();
    fake.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b".oculto".to_vec(), false),
            (b"notas.txt".to_vec(), false),
        ],
    );
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: dir(),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec {
                column: norte_frontend::SortColumn::Size,
                dir: norte_frontend::SortDir::Desc,
                dirs_first: true,
            },
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    *fake.sesion.lock().expect("session") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 7,
            body: serde_json::to_value(&body).expect("json"),
        },
        true,
    );

    let (_h, snap) = host_arbol(Arc::new(fake)).await;
    let b = listado(&snap);
    assert!(
        b.rows.iter().all(|r| r.display_name != ".oculto"),
        "the session said they were set aside"
    );
    let (_, direction) = orden_de(b);
    assert_eq!(direction, "desc", "and that it was sorted in reverse");
}

/// `[ui] show_hidden = false` seeds the panels' initial state, just like in
/// the TUI (#107). Without this, the key was dead in this window.
#[tokio::test]
async fn the_config_seeds_hiding() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b".oculto".to_vec(), false), (b"notas.txt".to_vec(), false)],
    );
    let mut settings = ajustes_de_prueba();
    settings.common.ui_show_hidden = Some(false);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(f),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    assert!(
        listado(&snap)
            .rows
            .iter()
            .all(|r| r.display_name != ".oculto"),
        "the configuration said not to show them"
    );
}
