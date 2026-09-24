use super::*;

// ---------------------------------------------------------------------------
// Read only: the window still does not mutate (task 3.3's security review;
// phase 4's exit gate demands it literally).
// ---------------------------------------------------------------------------

pub(super) async fn host_solo_read(backend: Arc<Fake>) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset_con(
            "orthodox",
            norte_ui_host::commands::Effects::SoloRead,
        )
        .expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::SoloRead,
        log_ring: None,
    })
    .await
    .expect("starts")
}

/// In read-only, F8 opens no delete confirmation: it SAYS so.
///
/// A frontend that does not yet have phase 5's safe path cannot have the key
/// alive with the dialog behind it; the key existing in the preset is not
/// permission.
#[tokio::test]
async fn in_read_only_delete_opens_nothing() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let ack = h.dispatch(press("F8")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "F8 gets answered, not executed: {ack:?}"
    );
    asentar().await;
    assert!(
        backend.borrados.lock().expect("borrados").is_empty(),
        "and it deletes nothing"
    );
}

/// Same with creating a directory.
#[tokio::test]
async fn in_read_only_create_creates_nothing() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let ack = h.dispatch(press("F7")).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(backend.creados.lock().expect("creados").is_empty());
}

/// And a policy approval does not even get raised: a renderer that cannot
/// mutate also cannot approve an agent mutating.
#[tokio::test]
async fn in_read_only_there_are_no_approvals_to_answer() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::Dialog {
            id: norte_ui_host::ModalId(1),
            choice: "approve".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Modal
        },
        "there is no dialog to answer"
    );
    assert!(
        backend.decisiones.lock().expect("decisiones").is_empty(),
        "and no decision reached the daemon"
    );
}

/// In full mode, the same key DOES open the confirmation: the gate is a
/// startup decision, not an amputation of the host.
#[tokio::test]
async fn in_full_mode_delete_still_asks_for_confirmation() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1);
}

// ---------------------------------------------------------------------------
// The review's two BLOCKERs.
// ---------------------------------------------------------------------------

/// Navigating to a large directory brings the WHOLE directory, not the first
/// page.
///
/// `lands_on` clears the witness when the first page lands, and the drain
/// task kept sending its batches with that same witness: `apply_batch`
/// rejected them all. Startup did not see it because `list_inicial`
/// restores the witness by hand.
#[tokio::test]
async fn navigating_to_a_large_directory_brings_it_whole() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"docs".to_vec(), true)]);
    let many: Vec<(Vec<u8>, bool)> = (0..300)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.put("mem:///casa/docs", many);
    let (h, snap) = host_tree(Arc::new(f)).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs is there")
        .key;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");

    // Each round is a trip to the actor: the fill advances between one
    // snapshot and the next.
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        if listing(&snap).total_rows == Some(300) {
            return;
        }
    }
    panic!("the listing stayed on the first page: the fill never lands");
}

/// A row from a previous generation is NOT touched.
///
/// The bridge's contract has promised this from the start ("a late double
/// click does not act on the file that took that row's place AFTERWARD") and
/// nothing implemented it: actions did not carry a generation.
#[tokio::test]
async fn a_row_from_another_generation_does_not_get_marked() {
    let (h, snap) = host(vec!["a", "b", "c"]).await;
    let old = listing(&snap).generation;
    // Sorting moves ALL rows and bumps the generation.
    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host alive");

    let ack = h
        .dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: RowKey(0),
            generation: old,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "the key was from the previous screen: {ack:?}"
    );
}

/// And a range with one end outside the window is not clamped: it is
/// rejected.
///
/// `PaneState::mark_range` clamps on purpose (its contract), so a
/// `to: u64::MAX` marked the WHOLE listing — rows the renderer never
/// received included — and what is marked is a delete's input.
#[tokio::test]
async fn an_overflowing_range_does_not_mark_the_whole_listing() {
    let (h, snap) = host(vec!["a", "b", "c", "d", "e"]).await;
    let generation = listing(&snap).generation;
    let ack = h
        .dispatch(UiAction::MarkRange {
            slot_id: 1,
            from: RowKey(0),
            to: RowKey(u64::MAX),
            generation,
        })
        .await
        .expect("host alive");
    assert_eq!(
        ack,
        ActionAck::Stale {
            reason: StaleAction::Generation
        },
        "an end that does not exist invalidates the whole range"
    );
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(listing(&snap).marks, 0, "and it marked nothing");
}

// ---------------------------------------------------------------------------
// Probing (review: rust M2/M3/m1, encoding M4).
// ---------------------------------------------------------------------------

/// A window taller than one probe batch fills up ENTIRELY.
///
/// `MAX_SONDEOS` bounds each batch, and nothing requested the next one: 200
/// rows with a size and the rest blank until the user moved something. A
/// ceiling that does not rearm itself is a silent ceiling.
#[tokio::test]
async fn a_large_window_gets_probed_in_batches_to_the_end() {
    let mut f = Fake {
        lazy: true,
        ..Fake::default()
    };
    let many: Vec<(Vec<u8>, bool)> = (0..500)
        .map(|i| (format!("f{i:04}.txt").into_bytes(), false))
        .collect();
    f.put("mem:///casa", many);
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 500,
    })
    .await
    .expect("host alive");

    until(&backend, "the 500 entries probed", |f| {
        (f.sondeos.lock().expect("sondeos").len() >= 500).then_some(())
    })
    .await;
}

/// A probe that lands when the listing is ALREADY another one sticks
/// nothing.
///
/// The guard looked at the in-flight request's witness, which after landing
/// is `None` — so it was worth zero and the comparison was always false.
/// What tells one listing apart from another is its EPOCH, which is always
/// defined.
#[tokio::test]
async fn a_probe_from_another_listing_does_not_hydrate() {
    let mut f = Fake {
        lazy: true,
        // The stat takes a while: there is time to navigate underneath.
        retraso_ms: 120,
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"a.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let docs = listing(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("docs")
        .key;
    // Navigate while the previous listing's probe is in flight.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listing(&snap).generation,
    })
    .await
    .expect("host alive");
    // What this case puts in flight: casa's listing, the probe of the
    // OUTER `a.txt` — the one that arrives late and must not hydrate — and
    // docs's listing. It waits for none to still be in flight.
    until(&backend, "the late probe already served", |f| {
        (f.listings() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    // The INNER `a.txt` is a different file from the outer `a.txt`; what is
    // checked is that the screen is coherent, not whether it has a size or
    // not.
    assert!(
        listing(&snap).path_display.ends_with("/docs"),
        "it navigated: {}",
        listing(&snap).path_display
    );
}

/// Hydration matches by the path that was REQUESTED, not the one the
/// provider returns.
///
/// An HFS+ that returns NFD, an SMB that returns another case, or a `stat`
/// that follows a link all produce a response whose path is not in the
/// listing. Since the requested path was already marked as probed, the cell
/// would stay blank forever.
#[tokio::test]
async fn a_provider_returning_another_spelling_does_not_leave_the_cell_blank() {
    let mut f = Fake {
        lazy: true,
        // The stat answers with the name in UPPERCASE: another spelling of
        // the same thing, like a case-insensitive server would.
        stat_grita: true,
        ..Fake::default()
    };
    f.put("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = next_snapshot(&mut sub).await;
        let filled = listing(&snap)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.is_some()));
        if filled {
            return;
        }
    }
    panic!("the cell is still blank: it matched by the returned path");
}

// ---------------------------------------------------------------------------
// Names and text (encoding review).
// ---------------------------------------------------------------------------

/// What is typed in the dialog is what gets created, byte for byte.
///
/// The name used to travel through `clamp_display`, which trims to 4 KiB and
/// ADDS `…`, and `Segment::new` accepts the ellipsis: a directory got created
/// with a name nobody typed. It is ADR 0061 in miniature — screen text that
/// ends up being a file name.
#[tokio::test]
async fn the_name_that_is_typed_is_the_one_that_gets_created() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F7")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;

    // A name with a control character inside: legal on Unix, and what gets
    // created has to be EXACTLY that.
    let raw = "caf\u{202e}e.txt";
    h.dispatch(UiAction::DialogInput {
        id,
        text: raw.to_owned(),
    })
    .await
    .expect("host alive");

    // What is PAINTED is masked and says so.
    let painted = next_dialogs(&mut sub).await;
    assert!(
        painted[0].input_hostile,
        "a name with a direction mark SAYS so: {:?}",
        painted[0].input
    );
    assert!(
        !painted[0]
            .input
            .as_deref()
            .unwrap_or_default()
            .contains('\u{202e}'),
        "and it is not painted raw"
    );

    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let created = until(&backend, "the queued creation", |f| {
        let c = f.creados.lock().expect("creados").clone();
        (!c.is_empty()).then_some(c)
    })
    .await;
    assert_eq!(created.len(), 1, "one creation got queued");
    let name = created[0]
        .file_name()
        .expect("has a name")
        .as_bytes()
        .to_vec();
    assert_eq!(
        name,
        raw.as_bytes(),
        "what was created is the typed bytes, not their projection"
    );
}

/// An impossible name is REJECTED instead of trimmed.
#[tokio::test]
async fn an_oversized_name_does_not_get_trimmed() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F7")).await.expect("host alive");
    let id = next_dialogs(&mut sub).await[0].id;
    let ack = h
        .dispatch(UiAction::DialogInput {
            id,
            text: "a".repeat(5000),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "it says it does not fit: {ack:?}"
    );
}

/// A column's id is an IDENTITY: it travels whole, and what gets masked is
/// the LABEL.
///
/// Masking the id is not injective. Two configured columns that only differ
/// by an invisible character gave the SAME masked id, and the click's
/// resolution does a `find`: pressing the second one sorted by the first.
/// It is ADR 0061's rule over a surface the ADR did not cover. What the
/// renderer PAINTS is `label`; the id only goes into a `data-` attribute.
#[tokio::test]
async fn two_columns_masked_the_same_are_still_two() {
    let backend = fake_tree();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        // Both ids mask to the SAME thing: U+200B and U+202E are the two
        // terminal hazards and `display_name` replaces them with U+FFFD.
        // They go through `plugin:` and not `attr:`: `attr:` ones are
        // already filtered by `is_valid_attr_id` — an id that is not legal
        // on the wire would take down the whole listing — and plugin ones
        // are filtered by nobody.
        columns: columns_of(&[
            "name",
            "plugin:acme.a\u{200b}b/x",
            "plugin:acme.a\u{202e}b/x",
        ]),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");
    drop(h);
    let b = listing(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "all three columns get painted: {ids:?}");
    assert_ne!(
        ids[1], ids[2],
        "and they are still TWO: masking the id merged them into one, and \
         the resolution's `find` would have always sorted by the first one"
    );
    assert!(
        ids[1].contains('\u{200b}') && ids[2].contains('\u{202e}'),
        "the id travels WHOLE, which is what makes it match itself: {ids:?}"
    );
    // What is PAINTED does go masked.
    for c in &b.columns {
        assert!(
            !c.label.contains('\u{202e}') && !c.label.contains('\u{200b}'),
            "the label goes through raw: {:?}",
            c.label
        );
    }
    // And the cell names its column with the same identity.
    let cell_columns: std::collections::BTreeSet<&str> = b
        .rows
        .iter()
        .flat_map(|f| f.cells.iter().map(|c| c.column.as_str()))
        .collect();
    for c in &cell_columns {
        assert!(
            ids.contains(c),
            "a cell names a column that is not in the header: {c:?}"
        );
    }
}

/// A viewer read that arrives late opens nothing.
///
/// F3 on a file on a slow mount, `esc`, and seconds later the viewer showed
/// up on its own — and since keys are routed by "there is a viewer", the
/// next key was interpreted by a different map without anyone asking for
/// it.
#[tokio::test]
async fn a_late_viewer_does_not_open_on_its_own() {
    let mut f = Fake {
        // The read takes a while; there is time to close.
        retraso_ms: 150,
        ..Fake::default()
    };
    f.put("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.content
        .insert("mem:///casa/notas.txt".to_owned(), b"hola\n".to_vec());
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(press("F3")).await.expect("host alive");
    // Before the content arrives, it is closed.
    h.dispatch(press("Escape")).await.expect("host alive");
    // The late read already came back: none is left in flight.
    until(&backend, "the late read served", |f| {
        (f.servidos() >= 2 && f.en_calma()).then_some(())
    })
    .await;
    asentar().await;

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(
        snap.viewer.is_none(),
        "the viewer does not open on its own after closing it"
    );
}

/// A layout with no listing at all is rejected at STARTUP.
///
/// It is #242 on this surface: it did not panic at startup but on the first
/// keystroke, inside the actor's task — with no log, no visible crash — and
/// the window stayed dead answering `Down` forever.
#[tokio::test]
async fn a_layout_with_no_listing_does_not_start() {
    let no_listing_tree = norte_frontend::layout::Node::slot(
        norte_frontend::layout::SlotId(1),
        norte_frontend::layout::KindId::new("status"),
    );
    let outcome = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: no_listing_tree,
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await;
    assert!(
        matches!(
            outcome,
            Err(norte_ui_host::controller::UiError::NoBrowserSlot)
        ),
        "a screen with no listing is not a screen"
    );
}

/// Columns resolve PER SCHEME, not once at startup.
///
/// With a list resolved at startup, `[ui.columns.schemes.sftp]` stayed dead:
/// its columns did not get painted and its attributes were never requested,
/// because the ones that travel with each listing had frozen with the
/// initial scheme's.
#[tokio::test]
async fn columns_from_another_scheme_are_not_dead() {
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".to_owned(), "size".to_owned()]),
        schemes: [(
            "mem".to_owned(),
            norte_config::SchemeColumns {
                columns: Some(vec!["name".to_owned(), "attr:mem.mode".to_owned()]),
                ..norte_config::SchemeColumns::default()
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    let backend = fake_tree();
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: test_settings(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_frontend::columns::ColumnsSettings::resolve(&cfg),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    })
    .await
    .expect("starts");
    drop(h);

    let ids: Vec<&str> = listing(&snap)
        .columns
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["name", "attr:mem.mode"],
        "it sends the `mem` scheme's configuration, not the default one's"
    );
    assert!(
        backend
            .attrs_pedidos
            .lock()
            .expect("attrs")
            .iter()
            .any(|a| a.iter().any(|id| id == "mem.mode")),
        "and its attribute IS requested in the listing"
    );
}

/// The window adjusts columns with the SAME rule as the terminal: in a
/// narrow slot with long names it gives up the class and the date goes
/// short, and in a wide one it gives up nothing. Header and cells come from
/// the same adjustment.
#[tokio::test]
async fn the_window_gives_up_columns_to_read_the_names() {
    async fn start(width: u16) -> norte_ui_host::ViewSnapshot {
        let mut f = Fake::default();
        f.put(
            "mem:///casa",
            (0..5).map(|i| {
                (
                    format!("Captura de pantalla 202{i}.png").into_bytes(),
                    false,
                )
            }),
        );
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(
                ["name", "size", "mtime", "kind"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            ..norte_config::ColumnsConfig::default()
        };
        let (h, snap) = UiHost::start(UiHostOptions {
            backend: Arc::new(f) as Arc<dyn norte_ui_host::HostBackend>,
            initial_dir: dir(),
            initial_dir_requested: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
            layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
            viewport: (width, 40),
            settings: test_settings(),
            paths: norte_ui_host::settings::HostPaths::default(),
            theme: norte_ui_host::pickers::HostTheme::default(),
            user_layouts: Vec::new(),
            profile: None,
            columns: norte_frontend::columns::ColumnsSettings::resolve(&cfg),
            effects: norte_ui_host::commands::Effects::Full,
            log_ring: None,
        })
        .await
        .expect("starts");
        drop(h);
        snap
    }

    let narrow = Box::pin(start(50)).await;
    let b = listing(&narrow);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["name", "size", "mtime"], "the class gives up first");
    let date = b.columns.iter().find(|c| c.id == "mtime").expect("date");
    assert_eq!(
        date.width,
        Some(norte_frontend::columns::COMPACT_WIDTH),
        "and the date goes short"
    );
    for row in &b.rows {
        let cells: Vec<&str> = row.cells.iter().map(|c| c.column.as_str()).collect();
        assert_eq!(cells, ["size", "mtime"], "cells follow the header");
    }

    let wide = Box::pin(start(200)).await;
    let ids: Vec<&str> = listing(&wide)
        .columns
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["name", "size", "mtime", "kind"],
        "with room it gives up nothing"
    );
}
