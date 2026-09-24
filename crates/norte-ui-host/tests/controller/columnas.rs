use super::*;

// ---------------------------------------------------------------------------
// Headers and sorting (phase 4, task 4.2).
// ---------------------------------------------------------------------------

/// The listing travels with its HEADERS: already-translated label, alignment
/// and which one drives the sort. The renderer paints them; it does not
/// invent them nor translate them.
#[tokio::test]
async fn the_listing_carries_its_headers() {
    let (_h, snap) = host_arbol(arbol()).await;
    let b = listado(&snap);
    let ids: Vec<&str> = b.columns.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["name", "size", "mtime"],
        "the configured columns, in order and with the name up front"
    );
    for c in &b.columns {
        assert!(!c.label.is_empty(), "every header carries its label: {c:?}");
    }
    let name = &b.columns[0];
    assert_eq!(
        name.sort.as_deref(),
        Some("asc"),
        "and it says which one sorts and in which direction"
    );
    assert!(b.columns[1].sort.is_none(), "the others do not");
}

/// Sorting by a column is the host's: the same rule as the TUI's — the same
/// column reverses, another column starts ascending — and the cursor stays
/// on the SAME entry, not on the same row.
#[tokio::test]
async fn sorting_by_column_uses_the_shared_rule() {
    let (h, snap) = host_arbol(arbol()).await;
    // FILES, without the directory: `dirs_first` groups it separately and
    // that group always goes ascending — reversing the sort does not touch
    // it.
    let files = |s: &norte_ui_host::ViewSnapshot| -> Vec<String> {
        listado(s)
            .rows
            .iter()
            .filter(|r| r.kind != norte_ui_host::dto::RowKind::Dir)
            .map(|r| r.display_name.clone())
            .collect()
    };
    let before = files(&snap);
    assert!(before.len() >= 2, "there are files to sort: {before:?}");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SortBy {
        slot_id: 1,
        column: "name".to_owned(),
    })
    .await
    .expect("host alive");
    let _ = sub.recv().await.expect("the host is still alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let reversed = siguiente_foto(&mut sub).await;
    let after = files(&reversed);
    let mut backward = before.clone();
    backward.reverse();
    assert_eq!(
        after, backward,
        "the same column twice reverses the direction"
    );
    assert_eq!(
        listado(&reversed).rows[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "and directories still come first: reversing does not touch their group"
    );
    assert_eq!(
        listado(&reversed).columns[0].sort.as_deref(),
        Some("desc"),
        "and the header says so"
    );
}

/// A column that does not sort — or is not there — leaves the listing
/// untouched, and it is answered instead of staying silent.
#[tokio::test]
async fn sorting_by_a_non_sortable_column_says_so() {
    let (h, _snap) = host_arbol(arbol()).await;
    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "no-existe".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "it says that column does not sort: {ack:?}"
    );
    // Nor does an attribute the scheme has not configured (ADR 0144): the
    // text comes from the renderer, and it must not end up in the sort or
    // the session without anyone having requested that column.
    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "attr:no-existe".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "an unconfigured attr does not sort: {ack:?}"
    );
}

/// ADR 0144: an attribute's header sorts like the others — it is clickable,
/// and carries the arrow after being pressed — with no change to the bridge:
/// the column travels as the same `attr:<id>` text that already named the
/// header.
#[tokio::test]
async fn an_attribute_sorts_from_its_header() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol() as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let before = siguiente_foto(&mut sub).await;
    let mode = |s: &norte_ui_host::ViewSnapshot| {
        listado(s)
            .columns
            .iter()
            .find(|c| c.id == "attr:posix.mode")
            .cloned()
            .expect("the mode header")
    };
    assert!(
        mode(&before).sortable,
        "the attribute's header is clickable"
    );
    assert!(
        mode(&before).sort.is_none(),
        "and does not yet drive the sort"
    );

    let ack = h
        .dispatch(UiAction::SortBy {
            slot_id: 1,
            column: "attr:posix.mode".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, ActionAck::Unavailable { .. }),
        "an attribute sorts: {ack:?}"
    );
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        if mode(&snap).sort.as_deref() == Some("asc") {
            assert!(
                listado(&snap).columns[0].sort.is_none(),
                "the name stops driving it"
            );
            return;
        }
    }
    panic!("the attribute's header has to carry the arrow after sorting");
}

// ---------------------------------------------------------------------------
// Decorations and plugin columns (task 4.2).
// ---------------------------------------------------------------------------

/// A backend with `n` entries, a badge on the first one and a plugin column
/// with a value for all of them.
pub(super) fn arbol_grande_con_plugins(n: usize) -> Arc<Falso> {
    let names: Vec<(Vec<u8>, bool)> = (0..n)
        .map(|i| (format!("f{i:05}.txt").into_bytes(), false))
        .collect();
    let mut f = Falso::default();
    f.arbol.insert("mem:///casa".to_owned(), names);
    f.decoraciones
        .insert("mem:///casa/f00000.txt".to_owned(), "M".to_owned());
    *f.plugins.lock().expect("plugins") = vec![{
        let mut p = extension("acme.git", "Git", true);
        // DECLARED in the catalogue: `validated_plugin_requests` does not
        // request a column its plugin does not say it has, so as not to
        // attribute it to the wrong one.
        p.columns = vec![norte_proto::methods::PluginColumnInfo {
            id: "status".to_owned(),
            header: "Estado".to_owned(),
        }];
        p
    }];
    for i in 0..n {
        f.valores_de_columna.insert(
            ("status".to_owned(), format!("mem:///casa/f{i:05}.txt")),
            "limpio".to_owned(),
        );
    }
    Arc::new(f)
}

/// Plugins are only asked about what is VISIBLE.
///
/// Every call spins up a wasm instance per plugin: #224 measured 167 ms per
/// page of 20 over 2000 entries. Asking about the whole directory multiplies
/// that cost by the directory's size, for nothing — the renderer can only
/// paint its window. This is where it parts ways with the TUI, which
/// decorates everything loaded because its pane declares no window.
/// What a plugin answers REACHES its row's cell, and its header is named
/// what the manifest says (`[[contributions.columns]] header`) and not its
/// id. Until now only whether the column was REQUESTED got checked.
#[tokio::test]
async fn a_plugin_columns_value_reaches_the_row_named_by_its_manifest() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 20,
    })
    .await
    .expect("host alive");

    let snap = esperar_foto(&h, &mut sub, "the plugin's cell to arrive", |f| {
        listado(f)
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| c.text.as_deref() == Some("limpio")))
    })
    .await;
    let b = listado(&snap);
    let column = b
        .columns
        .iter()
        .find(|c| c.id == "plugin:acme.git/status")
        .expect("the configured column paints");
    assert_eq!(
        column.label, "Estado",
        "the manifest's label, not the raw id"
    );
    let row = &b.rows[0];
    let cell = row
        .cells
        .iter()
        .find(|c| c.column == "plugin:acme.git/status")
        .expect("the row carries that column's cell");
    assert_eq!(cell.text.as_deref(), Some("limpio"));
}

#[tokio::test]
async fn plugins_are_only_asked_about_the_window() {
    const TOTAL: usize = 2000;
    let backend = arbol_grande_con_plugins(TOTAL);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "size", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 20,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let _ = siguiente_foto(&mut sub).await;
    }

    let batches = backend.decorados.lock().expect("mutex").clone();
    assert!(!batches.is_empty(), "plugins are asked");
    for batch in &batches {
        assert!(
            batch.len() <= 20,
            "a batch of {} paths over {TOTAL} entries: more than the window \
             is being requested",
            batch.len()
        );
    }
    let requested: usize = batches.iter().map(Vec::len).sum();
    assert!(
        requested <= 40,
        "in total {requested} of {TOTAL} were requested: the window is 20"
    );

    // And the plugin column travels in the same batch, not a separate sweep.
    let cols = backend.columnas_pedidas.lock().expect("mutex").clone();
    assert!(!cols.is_empty(), "the configured column is requested");
    for (plugin, column, paths) in &cols {
        assert_eq!(plugin, "acme.git");
        assert_eq!(column, "status");
        assert!(
            paths.len() <= 20,
            "the column is requested for {} paths, not for the window",
            paths.len()
        );
    }
}

/// ADR 0105: the icon reaches the row in its own field, the badge in its own
/// — both slots coexist — and the batch requested to decorate carries each
/// path's CLASS, without which an icon decorator does not know what a
/// folder is.
#[tokio::test]
async fn a_plugin_icon_reaches_the_row_and_the_class_travels() {
    let mut f = Falso::default();
    f.arbol.insert(
        "mem:///casa".to_owned(),
        vec![(b"src".to_vec(), true), (b"a.rs".to_vec(), false)],
    );
    f.iconos
        .insert("mem:///casa/src".to_owned(), "📁".to_owned());
    f.iconos
        .insert("mem:///casa/a.rs".to_owned(), "🦀".to_owned());
    f.decoraciones
        .insert("mem:///casa/a.rs".to_owned(), "M".to_owned());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host alive");

    let mut rows = None;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        let b = listado(&snap);
        if b.rows.iter().any(|r| !r.icon.is_empty()) {
            rows = Some(b.rows.clone());
            break;
        }
    }
    let rows = rows.expect("the icon reaches the row");
    let src = rows.iter().find(|r| r.display_name == "src").expect("src");
    assert_eq!(src.icon, "📁");
    assert!(src.badge.is_empty());
    let a = rows
        .iter()
        .find(|r| r.display_name == "a.rs")
        .expect("a.rs");
    assert_eq!(a.icon, "🦀", "the icon, in its own slot");
    assert_eq!(
        a.badge, "M",
        "and the badge, in its own: they do not overlap"
    );
    assert_eq!(a.badge_role, "warning");
    // And the class travelled with the batch, positionally: `src` is a
    // folder.
    let classes = backend.clases_decoradas.lock().expect("clases");
    let batches = backend.decorados.lock().expect("decorados");
    let (paths, kinds) = (&batches[0], &classes[0]);
    assert_eq!(paths.len(), kinds.len(), "one class per path");
    let src_idx = paths
        .iter()
        .position(|p| p.to_wire() == "mem:///casa/src")
        .expect("src in the batch");
    assert_eq!(kinds[src_idx], norte_proto::EntryKind::Dir);
}

/// Disabling a decorator from the manager REMOVES its badges from the rows
/// already painted: open listings forget what the plugins said and request
/// it again. It used to stay until the next `cd`, and the reader concluded
/// that disabling does not disable.
#[tokio::test]
async fn disabling_a_decorator_from_the_manager_removes_its_badges() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host alive");
    let mut with_badge = false;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        if listado(&snap).rows.iter().any(|r| !r.badge.is_empty()) {
            with_badge = true;
            break;
        }
    }
    assert!(with_badge, "the badge arrives first");
    let rounds_before = backend.decorados.lock().expect("decorados").len();

    // F12, and `e` on the only extension: it gets disabled.
    h.dispatch(tecla("F12")).await.expect("host alive");
    let v = extensiones_cargadas(&mut sub).await;
    assert_eq!(v.rows[0].id, "acme.git");
    h.dispatch(tecla("e")).await.expect("host alive");

    let mut without_badge = false;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        let b = listado(&snap);
        if b.rows.iter().all(|r| r.badge.is_empty())
            && b.rows
                .iter()
                .all(|r| r.cells.iter().all(|c| c.text.is_none()))
        {
            without_badge = true;
            break;
        }
    }
    assert!(
        without_badge,
        "rows end up with no badge from the disabled plugin"
    );
    assert!(
        backend.decorados.lock().expect("decorados").len() > rounds_before,
        "and the decoration was requested again, not guessed"
    );
}

/// The badge and the column value reach the row, marked as what they are:
/// THIRD-PARTY text.
#[tokio::test]
async fn a_plugins_badge_reaches_the_row() {
    let backend = arbol_grande_con_plugins(3);
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        columns: columnas_de(&["name", "plugin:acme.git/status"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();

    // What the renderer does right after mounting. Startup does NOT decorate
    // nor probe: it waits for the window to be declared, same as with sizes
    // — asking about an invented window is asking for too much.
    h.dispatch(UiAction::SetVisibleRange {
        slot_id: 1,
        first: 0,
        count: 10,
    })
    .await
    .expect("host alive");

    let mut decorated = None;
    for _ in 0..30 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        let b = listado(&snap);
        if let Some(f) = b.rows.iter().find(|r| !r.badge.is_empty()) {
            decorated = Some(f.clone());
            break;
        }
    }
    let row = decorated.expect("the badge reaches the row");
    assert_eq!(row.display_name, "f00000.txt");
    assert_eq!(row.badge, "M");
    assert_eq!(
        row.badge_role, "warning",
        "the role comes from the theme's CLOSED vocabulary, not a free \
         string the plugin picks"
    );

    // And the plugin column's cell.
    let cell = row
        .cells
        .iter()
        .find(|c| c.column == "plugin:acme.git/status")
        .expect("the configured column has its cell");
    assert_eq!(cell.text.as_deref(), Some("limpio"));
}

/// What the provider SKIPPED while listing is said, and translated.
///
/// It is the kind of bug that cannot be discovered by looking: what is
/// missing is not there, so there is no row where the reader could stumble
/// on it. An incomplete listing that says nothing lies by omission. The
/// provider gives the count — `FsListResult::skipped` — and `HostBackend::list`
/// used to THROW IT AWAY.
#[tokio::test]
async fn what_the_provider_skipped_is_said() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = Some(3);
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;

    let b = listado(&snap);
    assert_eq!(b.rows.len(), 1, "what did come is painted");
    assert!(
        b.skipped_note.contains('3'),
        "and it says how many are missing: {:?}",
        b.skipped_note
    );
    assert!(
        !b.skipped_note.starts_with("listing-"),
        "translated, not the key: {:?}",
        b.skipped_note
    );
}

/// A provider that does not keep count does NOT say that none were skipped.
///
/// `None` and `Some(0)` are not the same, and asserting "nothing is missing"
/// when nobody has checked is worse than staying silent.
#[tokio::test]
async fn a_provider_with_no_count_asserts_nothing() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    f.omitidas = None;
    let (h, snap) = host_arbol(Arc::new(f)).await;
    let _ = &h;
    assert!(listado(&snap).skipped_note.is_empty());
}

/// And the count belongs to THIS listing: it does not carry over to the next
/// directory.
#[tokio::test]
async fn the_skipped_count_does_not_survive_a_cd() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"docs".to_vec(), true)]);
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.omitidas = Some(2);
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(
        !listado(&snap).skipped_note.is_empty(),
        "the first one does"
    );

    // The second directory also skips them — the double answers the same —
    // but what matters is that the count gets SET AGAIN and is not
    // inherited: `set_listing` clears it, so without `set_skipped`
    // afterward it would stay empty. It is checked that it still gets said.
    let docs = listado(&snap)
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("the directory is there")
        .key;
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key: docs,
        generation: listado(&snap).generation,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        let snap = siguiente_foto(&mut sub).await;
        if listado(&snap).path_display.contains("docs") {
            assert!(
                !listado(&snap).skipped_note.is_empty(),
                "the count gets set again after the `cd`"
            );
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host alive");
    }
    panic!("the docs listing never arrived");
}

// ---------------------------------------------------------------------------
// The columns picker (task 4.2).
// ---------------------------------------------------------------------------

/// The picker's first view with the cursor wherever `needle` says.
pub(super) async fn selector_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    por_la_paleta(h, sub, "pane.columns").await;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("the columns picker does not open");
}

/// The picker shows what is configured, says its SCOPE and warns that what
/// is chosen is not saved.
///
/// The last part matters: this phase writes no configuration, and a picker
/// that stays silent about it leaves the user thinking they just configured
/// norte.
#[tokio::test]
async fn the_columns_picker_says_its_scope_and_that_it_does_not_save() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    assert!(!c.rows.is_empty(), "there are columns to show");
    assert!(
        c.rows[0].fixed,
        "the first one is the NAME, and it is neither disabled nor moved: {:?}",
        c.rows[0]
    );
    assert!(
        !c.title.starts_with("columns-picker"),
        "the title comes translated: {:?}",
        c.title
    );
    assert!(
        !c.note.is_empty() && !c.note.starts_with("columns-picker"),
        "and it says it does not save, translated: {:?}",
        c.note
    );
    for r in &c.rows {
        assert!(!r.label.is_empty(), "every row says its name: {r:?}");
    }
}

/// Enabling an `attr:` column RE-LISTS the slot.
///
/// An attribute's values only arrive if requested in `fs.list`, so a new
/// column over the old listing would stay blank — and blank means "this file
/// has no such attribute", which is a different thing. The fingerprint that
/// decides whether it is needed is the SHARED one (`pane_fingerprint`), the
/// same the TUI uses.
#[tokio::test]
async fn enabling_an_attr_column_re_lists() {
    let backend = arbol();
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::HostBackend>,
        initial_dir: dir(),
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
        // WITHOUT the mode column: enabling it is what changes the
        // fingerprint.
        columns: columnas_de(&["name", "size", "attr:posix.mode"]),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    let c = selector_columnas(&h, &mut sub).await;

    // It goes down to the attribute's row and DISABLES it: removing it also
    // changes the fingerprint, and it is the case that costs no extra trip
    // to the daemon... but does cost a re-listing, because `attrs_de` stops
    // requesting it.
    let row = c
        .rows
        .iter()
        .position(|r| r.id == "attr:posix.mode")
        .expect("the mode column is there");
    for _ in 0..row {
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    let before = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla(" ")).await.expect("host alive");
    h.dispatch(tecla("Enter")).await.expect("host alive");
    // The snapshot can lag behind, so it is drained until the effect shows.
    let mut closed = false;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if siguiente_foto(&mut sub).await.columns.is_none() {
            closed = true;
            break;
        }
    }
    assert!(closed, "the picker closes on applying");

    for _ in 0..20 {
        if backend.listados.load(Ordering::SeqCst) > before {
            return;
        }
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let _ = siguiente_foto(&mut sub).await;
    }
    panic!(
        "changing the `attr:` column set has to RE-LIST: an attribute's \
         values only arrive by requesting them in `fs.list`, and without \
         requesting it again the column stays blank — which means something \
         else"
    );
}

/// And changing only the ORDER does not re-list: it does not change what is
/// requested from the provider.
#[tokio::test]
async fn changing_the_column_order_does_not_re_list() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let _ = selector_columnas(&h, &mut sub).await;

    let before = backend.listados.load(Ordering::SeqCst);
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    h.dispatch(tecla("s")).await.expect("host alive");
    h.dispatch(tecla("Enter")).await.expect("host alive");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let _ = siguiente_foto(&mut sub).await;
    }
    assert_eq!(
        backend.listados.load(Ordering::SeqCst),
        before,
        "sorting belongs to the pane: there is nothing new to ask the provider"
    );
}

/// The picker's FOOTER announces keys, and those keys do what it says.
///
/// The footer is a catalogue string and the keys are a `match` in the host:
/// two places, no binding between them. This test's first version listened
/// for `J`/`K` and `→` while the footer promised `Shift+↑/↓` and `F` — a lie
/// only found by trying it, and no green test said so.
#[tokio::test]
async fn the_columns_pickers_keys_are_what_its_footer_announces() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let before = selector_columnas(&h, &mut sub).await;

    // The footer comes from the HOST and is built from the KEYMAP (#287): it
    // is not a string that names keys and can go stale when someone rebinds
    // them.
    let footer = before.hint.clone();
    assert!(!footer.is_empty(), "the footer arrives painted: {footer:?}");
    assert!(
        footer.contains("activa") && footer.contains("aplica"),
        "and it says what each chord does: {footer:?}"
    );
    // The cursor opens on the NAME, which is fixed: space there does
    // nothing, and that is the contract — the first column IS the name by
    // the render's contract — not a bug.
    assert_eq!(before.cursor, 0);
    assert!(before.rows[0].fixed);
    h.dispatch(tecla(" ")).await.expect("host alive");
    let still = siguiente_columnas(&h, &mut sub).await;
    assert!(
        still.rows[0].enabled,
        "the name cannot be disabled: {:?}",
        still.rows[0]
    );

    // A row that CAN be touched: space disables and enables it.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let on_another = siguiente_columnas(&h, &mut sub).await;
    let row = usize::try_from(on_another.cursor).expect("fits");
    assert!(row > 0 && !on_another.rows[row].fixed);
    let was_on = on_another.rows[row].enabled;
    h.dispatch(tecla(" ")).await.expect("host alive");
    let after = siguiente_columnas(&h, &mut sub).await;
    assert_ne!(
        after.rows[row].enabled, was_on,
        "space toggles it on and off"
    );

    // SHIFT+ARROW moves the ROW, not the cursor.
    assert!(footer.contains("Shift+"), "{footer:?}");
    let order_before: Vec<String> = after.rows.iter().map(|r| r.id.clone()).collect();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "ArrowDown".to_owned(),
        ctrl: false,
        alt: false,
        shift: true,
        meta: false,
    }))
    .await
    .expect("host alive");
    let moved = siguiente_columnas(&h, &mut sub).await;
    let order_after: Vec<String> = moved.rows.iter().map(|r| r.id.clone()).collect();
    assert_ne!(
        order_before, order_after,
        "shift+↓ moves the row: {order_before:?} → {order_after:?}"
    );

    // F cycles the cursor's row's format, if it supports one. It walks
    // through rows the way a person would — going down and looking where it
    // ended up — instead of pointing at an index computed over a list the
    // previous step just reordered. Bounded: a loop on a condition that may
    // never be reached is a test that HANGS instead of failing, and a hung
    // one says nothing.
    assert!(
        footer.to_lowercase().contains('f'),
        "and the format chord: {footer:?}"
    );
    let mut cycled = false;
    for _ in 0..moved.rows.len() + 2 {
        let v = siguiente_columnas(&h, &mut sub).await;
        let here = usize::try_from(v.cursor).expect("fits");
        let Some(row) = v.rows.get(here) else { break };
        if !row.format.is_empty() && !row.format_locked {
            let before = row.format.clone();
            h.dispatch(tecla("f")).await.expect("host alive");
            let after = siguiente_columnas(&h, &mut sub).await;
            assert_ne!(
                after.rows[here].format, before,
                "F cycles the cursor's row's format"
            );
            cycled = true;
            break;
        }
        h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    }
    assert!(
        cycled,
        "some column supports a format and it could be cycled"
    );
}

/// The picker's view after the last key.
pub(super) async fn siguiente_columnas(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ColumnsPickerView {
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(c) = siguiente_foto(sub).await.columns.clone() {
            return c;
        }
    }
    panic!("the picker is still open");
}

/// Bridge 64: dragging a header's edge fixes THAT column's width in session
/// — the header declares it in cells, clamped to what the configuration
/// accepts — and a column the slot does not paint is refused untouched.
#[tokio::test]
async fn resizing_a_column_fixes_its_width_in_the_header() {
    let (h, snap) = host_arbol(arbol()).await;
    let width_of = |s: &norte_ui_host::ViewSnapshot, id: &str| {
        listado(s)
            .columns
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.width)
            .expect("the column paints")
    };
    // By default `size` is already fixed (the shared width table), so what
    // is checked is that dragging CHANGES it, not that it introduces it for
    // the first time.
    assert_ne!(
        width_of(&snap, "size"),
        Some(12),
        "the starting width is not the requested one"
    );
    let mut sub = h.subscribe();

    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "size".to_owned(),
        cells: 12,
    })
    .await
    .expect("host alive");
    let _ = sub.recv().await.expect("the host is still alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = siguiente_foto(&mut sub).await;
    assert_eq!(width_of(&after, "size"), Some(12), "the header declares it");
    assert_eq!(
        listado(&after)
            .columns
            .iter()
            .find(|c| c.id == "size")
            .map(|c| c.align.as_str()),
        Some("right"),
        "and the configured alignment travels with it"
    );

    // Outside the loader's range: it gets clamped, never writing something
    // the next `load` would reject wholesale.
    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "size".to_owned(),
        cells: 900,
    })
    .await
    .expect("host alive");
    let _ = sub.recv().await.expect("the host is still alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let clamped = siguiente_foto(&mut sub).await;
    assert_eq!(width_of(&clamped, "size"), Some(64), "the loader's ceiling");

    // A column this slot does not paint: stale, and the header does not
    // change.
    h.dispatch(UiAction::ResizeColumn {
        slot_id: 1,
        column: "attr:nadie".to_owned(),
        cells: 5,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let same = siguiente_foto(&mut sub).await;
    assert_eq!(width_of(&same, "size"), Some(64));
    assert!(
        listado(&same).columns.iter().all(|c| c.id != "attr:nadie"),
        "no column is born from asking for its width"
    );
}
