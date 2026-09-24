use super::*;

// ---------------------------------------------------------------------------
// Settings: what they show (task 4.5). Writing them is
// `ajustes_escritura.rs`.
// ---------------------------------------------------------------------------

/// Waits for the next update that carries settings.
pub(super) async fn siguiente_ajustes(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::SettingsView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Settings { settings } = c {
                    return settings.clone();
                }
            }
        }
    }
    panic!("no update with settings ever arrived");
}

/// A host with some given paths, for the diagnostics section.
pub(super) async fn host_con_rutas(paths: norte_ui_host::settings::HostPaths) -> UiHost {
    UiHost::start(UiHostOptions {
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
        settings: ajustes_de_prueba(),
        paths,
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts")
    .0
}

/// `F11` opens settings with the SHARED registry and its effective value, and
/// each row says whether changing it takes effect now or on restart.
#[tokio::test]
async fn settings_show_the_shared_registry_with_its_value() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");

    // All settings sections together: since there are seven, none alone
    // carries the whole catalogue.
    let general: Vec<_> = a
        .sections
        .iter()
        .filter_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Paths { .. } => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        general.len(),
        norte_frontend::settings::catalog().len(),
        "not a single entry of the shared catalogue is left out"
    );
    for r in &general {
        assert!(!r.id.is_empty(), "each row carries its stable id");
        assert!(!r.name.is_empty(), "and its translated name: {r:?}");
        assert!(
            !r.name.starts_with("setting-"),
            "none paints a Fluent key: {r:?}"
        );
    }
    // What applies live is said by the shared catalogue, which is what the
    // terminal shows; and what the window cannot apply — language, fonts,
    // motion — is said by `fuera_de_alcance_en_caliente` when writing.
    let live = general
        .iter()
        .find(|r| r.id == "ui.theme")
        .expect("the theme is there");
    assert!(
        !live.restart_required,
        "the theme applies live when written, and the row does not say otherwise"
    );
    let cold = general
        .iter()
        .find(|r| r.id == "ui.lang")
        .expect("the language is there");
    assert!(
        cold.restart_required,
        "the language asks to restart the window, and the row says so"
    );
}

/// A location with its existence resolved, the way startup resolves it.
pub(super) fn sitio(p: std::path::PathBuf) -> norte_ui_host::settings::HostPath {
    norte_ui_host::settings::HostPath {
        missing: !p.exists(),
        path: p,
    }
}

/// The paths section says where each thing lives, marks what is missing, and
/// shows not a single value.
#[tokio::test]
async fn paths_are_said_and_whats_missing_is_marked() {
    let tmp = tempfile::tempdir().expect("tmp");
    let exists = tmp.path().join("config");
    std::fs::create_dir(&exists).expect("mkdir");
    let missing = tmp.path().join("no-esta");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        // `missing` is brought ALREADY resolved by whoever starts it: the
        // host does no I/O while projecting, and the test says so because it
        // is the contract.
        config_layers: vec![
            (
                norte_ui_host::settings::ConfigLayer::User,
                sitio(exists.clone()),
            ),
            (
                norte_ui_host::settings::ConfigLayer::Project,
                sitio(missing),
            ),
        ],
        state_dir: None,
        logs_dir: None,
        socket: Some(sitio(tmp.path().join("daemon.sock"))),
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");

    let paths = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("there is a paths section");
    assert_eq!(paths.len(), 3, "two layers and the socket");
    assert!(!paths[0].missing, "the layer that exists is not marked");
    assert!(
        paths[1].missing,
        "the one that does not exist IS: it is not painted as if it were there"
    );
    for r in paths {
        assert!(!r.label.is_empty(), "each one says WHAT it is: {r:?}");
        assert!(!r.display.is_empty(), "and where: {r:?}");
    }
}

/// A configuration directory with hostile bytes arrives MASKED and marked,
/// through the same path as a listing name.
#[tokio::test]
async fn a_hostile_path_arrives_masked_and_marked() {
    let tmp = tempfile::tempdir().expect("tmp");
    // A name with a bidi override: legal as a file, and a lie on screen if
    // painted raw.
    let hostile = tmp.path().join("conf\u{202e}gif");
    std::fs::create_dir(&hostile).expect("mkdir");
    let h = host_con_rutas(norte_ui_host::settings::HostPaths {
        config_layers: vec![(norte_ui_host::settings::ConfigLayer::User, sitio(hostile))],
        state_dir: None,
        logs_dir: None,
        socket: None,
    })
    .await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");
    let paths = a
        .sections
        .iter()
        .find_map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => Some(rows),
            norte_ui_host::dto::SettingsSectionView::Settings { .. } => None,
        })
        .expect("there is a paths section");
    assert!(
        !paths[0].display.contains('\u{202e}'),
        "a bidi override crossed over raw: {:?}",
        paths[0].display
    );
    assert!(
        paths[0].hostile,
        "and it is MARKED as differing from the real name"
    );
}

/// The cursor moves and does not run off the edges, and `enter` with nowhere
/// to write says so.
#[tokio::test]
async fn the_cursor_stays_in_bounds_and_enter_says_so() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let a = siguiente_ajustes(&mut sub).await.expect("opens");
    assert_eq!(a.cursor, 0);

    h.dispatch(tecla("ArrowUp")).await.expect("host alive");
    let up = siguiente_ajustes(&mut sub).await.expect("still open");
    assert_eq!(up.cursor, 0, "at the very top it does not run off the top");

    h.dispatch(tecla("End")).await.expect("host alive");
    let end = siguiente_ajustes(&mut sub).await.expect("still open");
    let total: usize = end
        .sections
        .iter()
        .map(|s| match s {
            norte_ui_host::dto::SettingsSectionView::Settings { rows, .. } => rows.len(),
            norte_ui_host::dto::SettingsSectionView::Paths { rows, .. } => rows.len(),
        })
        .sum();
    assert_eq!(
        usize::try_from(end.cursor).expect("fits"),
        total - 1,
        "nor at the bottom"
    );

    // The last row of a host with no paths is `keymap.preset`, which cycles;
    // and this host has no user layer, so there is nowhere to write it. It
    // is said, instead of doing nothing.
    let ack = h.dispatch(tecla("Enter")).await.expect("host alive");
    assert_eq!(
        ack,
        norte_ui_host::ActionAck::Unavailable {
            reason_key: "host-no-config-dir".to_owned()
        },
        "enter with nowhere to write says so: {ack:?}"
    );

    h.dispatch(tecla("Escape")).await.expect("host alive");
    foto_hasta(&h, &mut sub, "esc closes them", |s| {
        s.settings.is_none().then_some(())
    })
    .await;
}

/// With settings open, a listing key does not slip through.
#[tokio::test]
async fn with_settings_open_the_listing_does_not_move() {
    let (h, snap) = host_arbol(arbol()).await;
    let before = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F11")).await.expect("host alive");
    let _ = siguiente_ajustes(&mut sub).await.expect("opens");

    h.dispatch(tecla("j")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&snap).cursor, before, "the listing did not move");
    assert!(snap.settings.is_some(), "and settings are still open");
}

// ---------------------------------------------------------------------------
// The extensions manager (task 4.5).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries the manager.
pub(super) async fn siguiente_extensiones(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ExtensionsView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Extensions { extensions } = c {
                    return extensions.clone();
                }
            }
        }
    }
    panic!("no update with extensions ever arrived");
}

/// Waits until the catalogue has arrived (stops loading).
pub(super) async fn extensiones_cargadas(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ExtensionsView {
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(sub).await else {
            continue;
        };
        if !v.loading {
            return v;
        }
    }
    panic!("the catalogue never arrived");
}

/// `F12` opens the manager: first saying it is loading, then with the
/// sanitized catalogue and its approval status.
#[tokio::test]
async fn the_manager_shows_whats_installed_and_its_status() {
    let mut backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true), {
            let mut p = extension("org.norte.demo", "Demo", false);
            p.approved = false;
            p.enabled = false;
            p.capabilities = vec!["fs-read".to_owned()];
            p
        }],
        &[],
    );
    // A directory that did not load: it is shown, because an extension that
    // disappears silently is one the user believes they have.
    std::sync::Arc::get_mut(&mut backend)
        .expect("single reference")
        .errores_de_carga = vec![(
        "/plugins/roto".to_owned(),
        "el manifiesto no parsea".to_owned(),
    )];
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F12")).await.expect("host alive");
    let first = siguiente_extensiones(&mut sub).await.expect("opens");
    assert!(
        first.loading,
        "it opens SAYING it is loading: an empty list with no such notice \
         reads as \"you have none\""
    );

    let v = extensiones_cargadas(&mut sub).await;
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].id, "acme.ftp");
    assert!(v.rows[0].approved && v.rows[0].enabled);
    assert!(!v.rows[1].approved, "and the unapproved one shows too");
    assert_eq!(
        v.rows[1].capabilities,
        vec!["fs-read".to_owned()],
        "capabilities go in the ROW: they are the decision being approved"
    );
    assert_eq!(v.errors.len(), 1, "and what did not load is said");
}

/// `enter` on an extension requests its `[config]` schema and shows it with
/// its effective value.
#[tokio::test]
async fn the_card_shows_the_schema_with_its_effective_value() {
    let mut backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    std::sync::Arc::get_mut(&mut backend)
        .expect("single reference")
        .esquemas
        .insert(
            "acme.ftp".to_owned(),
            vec![norte_proto::methods::PluginConfigKeyWire {
                key: "timeout".to_owned(),
                kind: "int".to_owned(),
                default: "10".to_owned(),
                min: Some(1),
                max: Some(300),
                values: Vec::new(),
                description: Some("Segundos antes de rendirse".to_owned()),
                value: "30".to_owned(),
            }],
        );
    let (h, _snap) = host_arbol(std::sync::Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(tecla("Enter")).await.expect("host alive");
    let mut card = None;
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            card = v.detail;
            break;
        }
    }
    let d = card.expect("the card arrives");
    assert_eq!(d.id, "acme.ftp");
    assert_eq!(d.config.len(), 1);
    let k = &d.config[0];
    assert_eq!(k.key, "timeout");
    assert_eq!(k.value, "30", "the EFFECTIVE value, not the schema's");
    assert_eq!(k.default, "10", "and the schema's, to see what changed");
    assert!(!k.domain.is_empty(), "and what bounds it: {k:?}");
    assert!(
        !k.domain.contains("ext-config-"),
        "with no Fluent key painted: {k:?}"
    );
}

/// Moving drops the card: it describes another extension.
#[tokio::test]
async fn moving_drops_the_card() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.ftp", "FTP de ACME", false),
            extension("org.norte.demo", "Demo", false),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }
    // With the card open, the arrows are ITS OWN: they walk its keys. This
    // catalogue declares none, and then it does not keep them — a card with
    // nothing to walk would leave the reader unable to move without closing
    // it — so this one drops the catalogue's cursor and the card.
    h.dispatch(tecla("ArrowDown")).await.expect("host alive");
    let v = siguiente_extensiones(&mut sub).await.expect("still open");
    assert_eq!(v.cursor, 1);
    assert!(
        v.detail.is_none(),
        "the previous one's card cannot stay describing another"
    );
}

/// The first `esc` closes the CARD; the second, the manager.
#[tokio::test]
async fn the_first_esc_closes_the_card_and_the_second_the_manager() {
    let backend = arbol_con_plugins(vec![extension("acme.ftp", "FTP de ACME", false)], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host alive");
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if v.detail.is_some() {
            break;
        }
    }

    h.dispatch(tecla("Escape")).await.expect("host alive");
    let no_card = siguiente_extensiones(&mut sub).await.expect("still open");
    assert!(no_card.detail.is_none(), "the first esc closes the card");
    h.dispatch(tecla("Escape")).await.expect("host alive");
    assert!(
        siguiente_extensiones(&mut sub).await.is_none(),
        "the second closes the manager"
    );
}

/// Two extensions for the buttons (bridge 61): one approved and enabled, and
/// one unapproved.
fn dos_para_gobernar() -> Arc<Falso> {
    arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true), {
            let mut p = extension("org.norte.demo", "Demo", false);
            p.approved = false;
            p.enabled = false;
            p.capabilities = vec!["fs-read".to_owned()];
            p
        }],
        &[("acme.ftp", "Conecta con un servidor FTP.")],
    )
}

/// The approve button opens the SAME question as the key, with the
/// capabilities inside, and points at the row: a button is not a shortcut
/// around consent.
#[tokio::test]
async fn the_approve_button_opens_the_same_question_as_the_key() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;

    h.dispatch(UiAction::ExtensionGovern {
        row: 1,
        id: "org.norte.demo".to_owned(),
        change: norte_ui_host::action::ExtensionChange::Approval,
    })
    .await
    .expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    let question = d.last().expect("question");
    assert_eq!(question.title_key, "modal-extension-approve-title");
    // With the question in front, no manager button does anything: it is
    // modal to the mouse just like to the keyboard. A click behind would
    // revoke without asking, or close the manager under the question.
    for action in [
        UiAction::ExtensionGovern {
            row: 0,
            id: "acme.ftp".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Approval,
        },
        UiAction::ExtensionHelp {
            row: 0,
            id: "acme.ftp".to_owned(),
        },
    ] {
        let ack = h.dispatch(action).await.expect("host alive");
        assert!(matches!(ack, ActionAck::Stale { .. }), "{ack:?}");
    }
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
    assert!(
        question.body.iter().any(|l| l.text == "fs-read"),
        "the capabilities go inside: {:?}",
        question.body
    );
    assert!(
        backend.gobierno.lock().expect("gobierno").is_empty(),
        "nothing travels before the yes"
    );
    // And the pointed-at row is the button's, not the one the cursor had.
    let v = siguiente_extensiones(&mut sub).await.expect("still open");
    assert_eq!(v.cursor, 1);

    h.dispatch(UiAction::Dialog {
        id: question.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    assert_eq!(
        backend.gobierno.lock().expect("gobierno").as_slice(),
        ["approval:org.norte.demo:true:digest-de-org.norte.demo"]
    );
}

/// Uninstalling ASKS — through the button and the key alike — and only yes
/// deletes; afterward the catalogue is re-requested and the row is gone.
#[tokio::test]
async fn uninstalling_asks_and_only_yes_deletes() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;

    // The key (`d` is `dialog.remove` in orthodox): asks.
    h.dispatch(tecla("d")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    let question = d.last().expect("question");
    assert_eq!(question.title_key, "modal-extension-uninstall-title");
    assert_eq!(
        question.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.ftp")
    );
    let yes = question
        .choices
        .iter()
        .find(|c| c.id == "confirm")
        .expect("the answer that deletes");
    assert!(yes.destructive, "and it comes marked as what it is");
    assert_eq!(
        yes.label_key, "dialog-uninstall",
        "and says WHAT it confirms"
    );
    // Cancelling deletes nothing.
    h.dispatch(UiAction::Dialog {
        id: question.id,
        choice: "cancel".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());

    // The button: the same question, and yes deletes.
    h.dispatch(UiAction::ExtensionGovern {
        row: 0,
        id: "acme.ftp".to_owned(),
        change: norte_ui_host::action::ExtensionChange::Uninstall,
    })
    .await
    .expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    let question = d.last().expect("question");
    assert_eq!(question.title_key, "modal-extension-uninstall-title");
    h.dispatch(UiAction::Dialog {
        id: question.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    // The catalogue gets RE-REQUESTED after the yes and arrives behind the
    // cursor's patch: it waits for the one that no longer carries the
    // deleted one, not the first one that comes by.
    let mut without_it = None;
    for _ in 0..20 {
        let Some(v) = siguiente_extensiones(&mut sub).await else {
            continue;
        };
        if !v.loading && v.rows.iter().all(|r| r.id != "acme.ftp") {
            without_it = Some(v);
            break;
        }
    }
    let v = without_it.expect("the uninstalled one stops being listed");
    assert_eq!(
        backend.gobierno.lock().expect("gobierno").as_slice(),
        ["uninstall:acme.ftp"]
    );
    assert_eq!(
        v.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["org.norte.demo"]
    );
}

/// An extension that did NOT LOAD is one more row: it is pointed at, and its
/// only verb is uninstalling — through the key and the button, with the same
/// question. ADR 0104 left it written as a gap: the handler deleted it, but
/// there was no way to ask for it from the window.
#[tokio::test]
async fn a_broken_extension_is_shown_and_only_uninstalls() {
    let Ok(mut f) = Arc::try_unwrap(dos_para_gobernar()) else {
        panic!("the freshly made double is not shared");
    };
    f.errores_de_carga = vec![
        ("acme.roto".to_owned(), "el manifiesto no parsea".to_owned()),
        ("no un id".to_owned(), "el manifiesto no parsea".to_owned()),
    ];
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let v = extensiones_cargadas(&mut sub).await;
    // The id only when the name is one: it is what the renderer needs to
    // offer the button, and what the host looks at before asking.
    assert_eq!(
        v.errors.iter().map(|e| e.id.as_deref()).collect::<Vec<_>>(),
        [Some("acme.roto"), None]
    );

    // Broken ones go AFTER the two loaded ones: row 2 is the first one.
    h.dispatch(UiAction::ExtensionSelectRow { row: 2 })
        .await
        .expect("host alive");
    let v = siguiente_extensiones(&mut sub)
        .await
        .expect("the cursor gets painted");
    assert_eq!(v.cursor, 2, "a broken one is pointed at like any row");

    // Approving it makes no sense — there are no capabilities to read — and
    // it is SAID.
    let ack = h
        .dispatch(UiAction::ExtensionGovern {
            row: 2,
            id: "acme.roto".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Approval,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "ext-broken-only-uninstall"),
        "{ack:?}"
    );

    // The key on the pointed-at one: the usual question, with its id.
    h.dispatch(tecla("d")).await.expect("host alive");
    let d = siguientes_dialogos(&mut sub).await;
    let question = d.last().expect("question");
    assert_eq!(question.title_key, "modal-extension-uninstall-title");
    assert_eq!(
        question.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.roto")
    );
    h.dispatch(UiAction::Dialog {
        id: question.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let _ = siguientes_dialogos(&mut sub).await;
    assert_eq!(
        backend.gobierno.lock().expect("gobierno").as_slice(),
        ["uninstall:acme.roto"]
    );

    // And a broken one whose directory is not named after an id does not
    // ask: there is no id to send, and it says why.
    h.dispatch(UiAction::ExtensionSelectRow { row: 3 })
        .await
        .expect("host alive");
    let ack = h.dispatch(tecla("d")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "ext-broken-not-id"),
        "{ack:?}"
    );
}

/// Enabling an unapproved one through the button is refused and says so,
/// like with the key.
#[tokio::test]
async fn enabling_an_unapproved_one_via_button_is_refused() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    let ack = h
        .dispatch(UiAction::ExtensionGovern {
            row: 1,
            id: "org.norte.demo".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Enabled,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-extension-not-approved"),
        "{ack:?}"
    );
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
    // A row that no longer exists, or is no longer the one the renderer
    // saw — the catalogue is re-requested in the background and a deletion
    // above shifts the ones below — governs nothing: disabling "row 0" would
    // have disabled its neighbor.
    for (row, id) in [(9, "acme.ftp"), (0, "org.norte.demo")] {
        let ack = h
            .dispatch(UiAction::ExtensionGovern {
                row,
                id: id.to_owned(),
                change: norte_ui_host::action::ExtensionChange::Enabled,
            })
            .await
            .expect("host alive");
        assert!(
            matches!(ack, ActionAck::Stale { .. }),
            "{row} {id}: {ack:?}"
        );
    }
    assert!(backend.gobierno.lock().expect("gobierno").is_empty());
}

/// The help button closes the manager and opens help on THAT extension's
/// page, like `F1` on the row in the terminal.
#[tokio::test]
async fn the_help_button_opens_that_extensions_page() {
    let backend = dos_para_gobernar();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;

    // With no page, nothing opens, and it is said.
    let ack = h
        .dispatch(UiAction::ExtensionHelp {
            row: 1,
            id: "org.norte.demo".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");

    h.dispatch(UiAction::ExtensionHelp {
        row: 0,
        id: "acme.ftp".to_owned(),
    })
    .await
    .expect("host alive");
    // The manager closes first: help replaces it, like in the terminal.
    assert!(
        siguiente_extensiones(&mut sub).await.is_none(),
        "the manager closes"
    );
    let mut page = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            page = Some(v);
            break;
        }
    }
    let page = page.expect("the plugin's page opens and installs");
    assert!(format!("{:?}", page.blocks).contains("Conecta con un servidor FTP"));
}

/// A hostile name, publisher and description arrive masked; an invalid id
/// does not arrive at all.
#[tokio::test]
async fn an_extensions_text_arrives_masked() {
    let mut bad = extension("acme.\u{202e}ftp", "Invisible", false);
    bad.description = Some("desc".to_owned());
    let mut hostile = extension("acme.ftp", "FTP\u{202e}de ACME", false);
    hostile.publisher = "ACME\u{7}".to_owned();
    hostile.description = Some("Sirve\u{202e}ficheros".to_owned());
    hostile.version = "1.0\u{7}".to_owned();
    let backend = arbol_con_plugins(vec![hostile, bad], &[]);
    let (h, _snap) = host_arbol(backend).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let v = extensiones_cargadas(&mut sub).await;

    assert_eq!(
        v.rows.len(),
        1,
        "the invalid id gets DISCARDED at the entry"
    );
    let text = format!("{:?}", v.rows[0]);
    assert!(
        !text.contains('\u{202e}') && !text.contains('\u{7}'),
        "third-party text unmasked: {text}"
    );
    assert!(
        !text.contains("\\u{202e}") && !text.contains("\\u{7}"),
        "third-party text unmasked: {text}"
    );
}

/// The VALUE of a configuration key, its default and an `enum`'s values are
/// written by the PLUGIN, and arrive masked and marked.
///
/// The manifest only bounds their LENGTH — `CONFIG_STRING_MAX_CHARS`,
/// `CONFIG_ENUM_MAX_VALUES` — and checks no charset at all, so a
/// `plugin.toml` could put a bidi override in an `enum` value and see it
/// arrive raw at a DOM text node. Three rustdocs said those fields were
/// "norte's own vocabulary, never the plugin's free text".
///
/// And the `·` that joins the domain is composed HERE: if the value were not
/// masked, a plugin could forge one and fake a domain it does not have.
#[tokio::test]
async fn a_plugin_keys_value_arrives_masked_and_marked() {
    let ext = extension("acme.ftp", "FTP", true);
    let mut f = Falso {
        plugins: vec![ext].into(),
        ..Falso::default()
    };
    f.arbol.clone_from(&arbol().arbol);
    f.esquemas.insert(
        "acme.ftp".to_owned(),
        vec![norte_proto::methods::PluginConfigKeyWire {
            key: "mode".to_owned(),
            kind: "enum".to_owned(),
            default: "safe\u{202e}".to_owned(),
            min: None,
            max: None,
            values: vec!["safe".to_owned(), "fast\u{202e} · read-only".to_owned()],
            description: None,
            value: "fast\u{7}".to_owned(),
        }],
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = extensiones_cargadas(&mut sub).await;
    h.dispatch(tecla("Enter")).await.expect("host alive");

    let mut card = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(d) = siguiente_foto(&mut sub)
            .await
            .extensions
            .and_then(|e| e.detail)
        {
            card = Some(d);
            break;
        }
    }
    let card = card.expect("the card arrives");
    let row = card.config.first().expect("the key is there");
    let text = format!("{row:?}");
    assert!(
        !text.contains('\u{202e}') && !text.contains('\u{7}'),
        "the plugin's text unmasked: {text}"
    );
    assert!(
        !text.contains("\\u{202e}") && !text.contains("\\u{7}"),
        "the plugin's text unmasked: {text}"
    );
    assert!(
        row.hostile,
        "and it SAYS what is painted differs from what it is: {row:?}"
    );
}

/// With the manager open, the listing does not move.
#[tokio::test]
async fn with_the_manager_open_the_listing_does_not_move() {
    let (h, snap) = host_arbol(arbol()).await;
    let before = listado(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F12")).await.expect("host alive");
    let _ = siguiente_extensiones(&mut sub).await.expect("opens");

    h.dispatch(tecla("j")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&snap).cursor, before);
    assert!(snap.extensions.is_some(), "and the manager is still open");
}

// ---------------------------------------------------------------------------
// The theme and the volumes picker (task 4.5).
// ---------------------------------------------------------------------------

/// Waits for the next update with the theme.
pub(super) async fn siguiente_tema(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::ThemeView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Theme { theme } = c {
                    return theme.clone();
                }
            }
        }
    }
    panic!("no update with a theme ever arrived");
}

/// Waits for the next update with the picker.
pub(super) async fn siguiente_selector(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PickerView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Picker { picker } = c {
                    return picker.clone();
                }
            }
        }
    }
    panic!("no update with a picker ever arrived");
}

/// A host with a given theme.
pub(super) async fn host_con_tema(theme: norte_ui_host::pickers::HostTheme) -> UiHost {
    UiHost::start(UiHostOptions {
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
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme,
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("starts")
    .0
}

/// `F9` shows the theme role by role, and NAMES the effects this window
/// cannot paint: a retro theme that looks identical reads as broken.
#[tokio::test]
async fn the_theme_shows_from_inside_and_says_what_it_does_not_paint() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "retro".to_owned(),
        roles: vec![
            ("selection-bg".to_owned(), "#2d4f8a".to_owned()),
            ("error-fg".to_owned(), "#f7768e".to_owned()),
        ],
        effects: vec!["crt".to_owned(), "scanlines".to_owned()],
        resuelto: norte_theme::Theme::default(),
        variante_clara: None,
        variante_oscura: None,
    })
    .await;
    let mut sub = h.subscribe();
    // `alt+9` since spec 2026-09-10: F9 is the menu, as in the whole family.
    h.dispatch(tecla_alt("9")).await.expect("host alive");
    let t = siguiente_tema(&mut sub).await.expect("opens");

    assert_eq!(t.name, "retro");
    assert_eq!(t.roles.len(), 2);
    assert_eq!(t.roles[0].color, "#2d4f8a", "the color travels as a swatch");
    assert_eq!(
        t.unsupported_effects
            .iter()
            .map(|e| e.key.clone())
            .collect::<Vec<_>>(),
        vec!["crt".to_owned(), "scanlines".to_owned()],
        "the effects are NAMED, not ignored"
    );

    h.dispatch(tecla("Escape")).await.expect("host alive");
    assert!(siguiente_tema(&mut sub).await.is_none(), "esc closes it");
}

/// A theme with no effects invents none.
#[tokio::test]
async fn a_theme_with_no_effects_says_nothing_about_them() {
    let h = host_con_tema(norte_ui_host::pickers::HostTheme {
        name: "default".to_owned(),
        roles: vec![("fg".to_owned(), "#d4d8de".to_owned())],
        effects: Vec::new(),
        resuelto: norte_theme::Theme::default(),
        variante_clara: None,
        variante_oscura: None,
    })
    .await;
    let mut sub = h.subscribe();
    // `alt+9` since spec 2026-09-10: F9 is the menu, as in the whole family.
    h.dispatch(tecla_alt("9")).await.expect("host alive");
    let t = siguiente_tema(&mut sub).await.expect("opens");
    assert!(t.unsupported_effects.is_empty());
}

/// A host volume with what the view looks at.
pub(super) fn volumen(mount: &str, fs: &str, ro: bool) -> norte_proto::methods::Volume {
    norte_proto::methods::Volume {
        mount: norte_proto::VPath::parse(mount).expect("vpath"),
        label: None,
        fs_type: fs.to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(100 * 1024 * 1024 * 1024),
        free_bytes: Some(12 * 1024 * 1024 * 1024),
        read_only: ro,
    }
}

/// The volumes picker opens ASKING, and choosing one navigates the panel to
/// its mount point — which is a read, and that is why it is done.
#[tokio::test]
async fn choosing_a_volume_navigates_the_panel() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.pon("mem:///otro", vec![(b"raiz.txt".to_vec(), false)]);
    f.volumenes = vec![volumen("mem:///otro", "ext4", false)];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    // `pane.select-drive` is not bound by the orthodox preset: it is run
    // through the palette, which is another door to the SAME catalogue.
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let _ = siguiente_paleta(&mut sub).await.expect("the palette opens");
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");

    let first = siguiente_selector(&mut sub).await.expect("opens");
    assert!(
        !first.empty.is_empty() || !first.rows.is_empty(),
        "it either asks or brings rows, but is never silent"
    );

    let mut with_rows = None;
    for _ in 0..20 {
        let Some(v) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            with_rows = Some(v);
            break;
        }
    }
    let v = with_rows.expect("the mount table arrives");
    assert_eq!(v.rows.len(), 1);
    assert!(v.rows[0].detail.contains("ext4"), "{:?}", v.rows[0]);
    assert!(
        v.rows[0].detail.contains("12"),
        "and how much is left: {:?}",
        v.rows[0]
    );

    h.dispatch(tecla("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.picker.is_none(), "the picker closes");
    assert!(
        listado(&snap).path_display.contains("otro"),
        "and the panel navigated to the volume: {}",
        listado(&snap).path_display
    );
}

/// A space the system did not answer is SAID; a `0` is never painted, which
/// reads as "full" — the opposite of "I don't know".
#[tokio::test]
async fn a_volume_with_no_size_says_so() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let mut v = volumen("mem:///otro", "nfs4", true);
    v.total_bytes = None;
    v.free_bytes = None;
    f.volumenes = vec![v];
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let _ = siguiente_paleta(&mut sub).await.expect("the palette opens");
    for c in "select-drive".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(tecla("Enter")).await.expect("host alive");

    for _ in 0..20 {
        let Some(view) = siguiente_selector(&mut sub).await else {
            continue;
        };
        if let Some(row) = view.rows.first() {
            assert!(!row.detail.contains(" 0 "), "a zero reads as full");
            assert!(
                row.detail.contains("nfs4"),
                "and it still says what it does know: {row:?}"
            );
            return;
        }
    }
    panic!("the mount table never arrived");
}

/// ADR 0100: a `hook` plugin's phrase reaches the window's bar attributed to
/// the plugin, and as an ephemeral notice — not a banner: it talks about a
/// mutation that already happened. The id goes IN FRONT, put there by norte.
#[tokio::test]
async fn a_hooks_notice_reaches_the_bar_attributed() {
    let fake = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.avisos_plugin.lock().expect("avisos_plugin") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(fake)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::PluginNotice {
        plugin_id: "org.norte.rename-log".to_owned(),
        kind: "notify".to_owned(),
        text: Some("renamed 3 files".to_owned()),
    })
    .expect("the host is listening");
    let line = foto_hasta(&h, &mut sub, "the notice in the bar", |f| {
        f.status
            .message
            .clone()
            .filter(|m| m.contains("renamed 3 files"))
    })
    .await;
    assert!(line.starts_with("⚑ org.norte.rename-log"), "{line}");
    assert!(
        f_banners_vacios(&h, &mut sub).await,
        "a hook notice lights up no persistent banner"
    );

    // And the notice also travels, with the same line.
    let mut sub2 = h.subscribe();
    tx.send(norte_proto::methods::PluginNotice {
        plugin_id: "org.norte.rename-log".to_owned(),
        kind: "hooks-disabled".to_owned(),
        text: None,
    })
    .expect("the host is listening");
    let notice = super::registro::foto_hasta_notice(&mut sub2, "msg-plugin-hooks-disabled").await;
    assert!(notice.contains("org.norte.rename-log"), "{notice}");
}

async fn f_banners_vacios(
    h: &norte_ui_host::UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> bool {
    foto_hasta(h, sub, "the banners", |f| Some(f.status.banners.is_empty())).await
}
