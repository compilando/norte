use super::*;

// ---------------------------------------------------------------------------
// Help (phase 4, task 4.4).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries help.
pub(super) async fn siguiente_ayuda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::HelpView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Help { help } = c {
                    return help.clone();
                }
            }
        }
    }
    panic!("no update with help ever arrived");
}

/// Opens help and returns what would be painted.
pub(super) async fn abrir_ayuda(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::HelpView {
    h.dispatch(tecla("F1")).await.expect("host alive");
    siguiente_ayuda(sub).await.expect("help opens")
}

/// `F1` opens help on the CONTEXT page where the reader is, with its prose
/// already in blocks and not a single unresolved marker.
#[tokio::test]
async fn f1_opens_the_contexts_help_and_its_prose_arrives_in_blocks() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;

    assert_eq!(
        help.topic_id, "panes",
        "opens the CONTEXT's page (the listing), not the index"
    );
    assert!(!help.title.is_empty(), "the page has a title");
    assert!(!help.blocks.is_empty(), "and a body");
    assert!(
        !help.sidebar.is_empty(),
        "and the side bar lists what there is"
    );
    assert!(
        !help.can_back,
        "the context's page is the ROOT: `⌫` closes, it does not go back to \
         an index the reader was never on"
    );
    // Not a single live unresolved marker, nor a raw Fluent key: both are
    // text the reader should never see.
    let text = format!("{:?}", help.blocks);
    assert!(!text.contains("{{cmd:"), "an unresolved marker: {text}");
    assert!(!text.contains("[["), "an unresolved link: {text}");
    assert!(!text.contains("help-cmd-"), "a raw Fluent key");
    // A group header arrives TRANSLATED, not as its tag.
    let groups: Vec<&norte_ui_host::dto::HelpSidebarRowView> = help
        .sidebar
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::HelpSidebarRowView::Group { .. }))
        .collect();
    assert!(!groups.is_empty(), "there are group headers");
    for g in groups {
        let norte_ui_host::dto::HelpSidebarRowView::Group { label } = g else {
            unreachable!("filtered above")
        };
        assert!(!label.starts_with("help-group-"), "untranslated: {label}");
    }
}

/// The keyboard sheet is GENERATED from the effective map: a rebind changes
/// it, and a key this frontend does not run comes out disabled and with its
/// reason.
#[tokio::test]
async fn the_keyboard_sheet_comes_from_the_effective_keymap() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;

    // The keyboard page is the side bar's last one (a single-row group): it
    // is reached with the cursor, the way the reader would reach it.
    let last = help.sidebar.len() - 1;
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(last).expect("fits"),
    })
    .await
    .expect("host alive");
    let keys = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_eq!(keys.topic_id, "keys", "the keyboard page opened");

    let rows: Vec<&norte_ui_host::dto::HelpKeyRowView> = keys
        .blocks
        .iter()
        .filter_map(|b| match b {
            norte_ui_host::dto::HelpBlockView::Keys { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!rows.is_empty(), "the sheet has rows");
    assert!(
        rows.iter().any(|r| r.chord == "F5"),
        "and writes them the way the documentation writes them: {:?}",
        rows.iter().map(|r| &r.chord).collect::<Vec<_>>()
    );
    assert!(
        rows.iter().all(|r| !r.label.starts_with("help-cmd-")),
        "no row paints a Fluent key"
    );
    // There used to be disabled rows while `app.quit` was not the window's:
    // it was the last command the orthodox preset binds that this window did
    // not do. None are left now, so what is pinned down is the RULE: if a
    // row comes disabled, it says why — dimming it without saying so leaves
    // the reader guessing whether the app is broken.
    let disabled: Vec<&&norte_ui_host::dto::HelpKeyRowView> =
        rows.iter().filter(|r| !r.enabled).collect();
    assert!(
        disabled.iter().all(|r| !r.reason.is_empty()),
        "every disabled row says WHY"
    );
    assert!(
        rows.iter().any(|r| r.chord == "F10" && r.enabled),
        "and quit, which used to be disabled in this window, no longer is: {:?}",
        rows.iter()
            .filter(|r| r.chord == "F10")
            .map(|r| (&r.label, r.enabled))
            .collect::<Vec<_>>()
    );
}

/// A runnable row for a command this frontend does NOT implement is offered
/// disabled and with its reason, instead of promising an `enter` that would
/// answer "not here".
#[tokio::test]
async fn a_row_this_window_does_not_run_arrives_disabled() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut help = abrir_ayuda(&h, &mut sub).await;

    // The side bar is walked until a page documenting commands is found.
    for row in 0..help.sidebar.len() {
        if help.actions.iter().any(|a| !a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("fits"),
        })
        .await
        .expect("host alive");
        help = siguiente_ayuda(&mut sub).await.expect("still open");
    }
    let runnable: Vec<&norte_ui_host::dto::HelpActionView> =
        help.actions.iter().filter(|a| !a.opens_topic).collect();
    assert!(
        !runnable.is_empty(),
        "some page in the corpus documents commands"
    );
    for a in runnable {
        assert!(!a.label.is_empty(), "every row is named something");
        assert_eq!(
            a.enabled,
            a.reason.is_empty(),
            "a disabled row says why, and a live one invents no reason: {a:?}"
        );
    }
}

/// Activating a runnable row closes help AND runs the command — through the
/// SAME path as a keystroke, which is what makes help another door to the
/// catalogue and not a second dispatcher.
#[tokio::test]
async fn activating_in_help_closes_and_runs_through_the_same_path() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;

    // The selection page documents `mark.toggle`, which this window DOES
    // run: it is reached through the side bar, the way the reader would
    // reach it.
    let mut page = help;
    for row in 0..40 {
        if page.topic_id == "selection" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host alive");
        page = siguiente_ayuda(&mut sub).await.expect("still open");
    }
    assert_eq!(page.topic_id, "selection", "the selection page exists");
    let i = page
        .actions
        .iter()
        .position(|a| !a.opens_topic && a.enabled)
        .expect("some of its rows are run by this window");

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("fits"),
    })
    .await
    .expect("host alive");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "running closes help, and the closing travels as a patch"
    );

    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(
        snap.help.is_none(),
        "and it is still closed in the snapshot"
    );
    assert!(
        listado(&snap).rows.iter().any(|r| r.marked),
        "and the mark command really ran"
    );
}

/// Following a "see also" link opens the other page and LEAVES help open: it
/// is navigation, not an action on the listing.
#[tokio::test]
async fn following_a_link_opens_the_other_page_and_leaves_a_way_back() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut page = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if page.actions.iter().any(|a| a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host alive");
        page = siguiente_ayuda(&mut sub).await.expect("still open");
    }
    let i = page
        .actions
        .iter()
        .position(|a| a.opens_topic)
        .expect("some page links to another");
    let before = page.topic_id.clone();

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("fits"),
    })
    .await
    .expect("host alive");
    let followed = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_ne!(followed.topic_id, before, "the page changed");
    assert!(followed.can_back, "and there is somewhere to go back to");

    h.dispatch(tecla("Backspace")).await.expect("host alive");
    let back = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_eq!(back.topic_id, before, "`⌫` goes back the way it came");
}

/// `esc` closes it; `/` opens the filter and then text keys are its own.
#[tokio::test]
async fn the_bar_filters_and_escape_closes() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;
    assert!(!help.filtering, "starts with no filter");

    h.dispatch(tecla("/")).await.expect("host alive");
    let filtering = siguiente_ayuda(&mut sub).await.expect("still open");
    assert!(filtering.filtering, "`/` opens the filter");

    h.dispatch(tecla("c")).await.expect("host alive");
    let typed = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_eq!(typed.filter, "c", "and the letter is written by the filter");

    // The first `esc` stops filtering; the second closes.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    let no_filter = siguiente_ayuda(&mut sub).await.expect("still open");
    assert!(!no_filter.filtering, "the first esc leaves the filter");
    h.dispatch(tecla("Escape")).await.expect("host alive");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "the second closes help"
    );
}

/// With help open, a listing key does NOT slip through: the screen is its
/// own, like the viewer's.
#[tokio::test]
async fn with_help_open_the_listing_does_not_move() {
    let (h, snap) = host_arbol(arbol()).await;
    let before = listado(&snap).cursor;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    // `j` in the preset moves the cursor down; with help open it does not
    // belong to the listing, and with no filter open it does not type
    // anything either.
    h.dispatch(tecla("j")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&snap).cursor, before, "the listing did not move");
    assert!(
        snap.help.is_some(),
        "and help is still open in the snapshot"
    );
}

/// `Ctrl+P` switches from help to the palette, and BOTH changes travel in the
/// same patch: a renderer that only received the palette's would keep
/// painting help underneath.
#[tokio::test]
async fn ctrl_p_switches_help_for_the_palette_in_one_patch() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");

    let mut saw_close = false;
    let mut saw_palette = false;
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                match c {
                    norte_ui_host::dto::ViewChange::Help { help } => saw_close = help.is_none(),
                    norte_ui_host::dto::ViewChange::Palette { palette } => {
                        saw_palette = palette.is_some();
                    }
                    _ => {}
                }
            }
            if saw_close && saw_palette {
                return;
            }
        }
    }
    panic!("the handoff did not travel whole: close={saw_close}, palette={saw_palette}");
}

// ---------------------------------------------------------------------------
// Help's extension pages (H3e over the graphical host).
// ---------------------------------------------------------------------------

/// A catalogue plugin, with the bare minimum help looks at.
pub(super) fn extension(id: &str, name: &str, has_help: bool) -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        publisher: "ACME".to_owned(),
        version: "1.0.0".to_owned(),
        category: "previewer".to_owned(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: Vec::new(),
        has_help,
        // The anchor the core sends (#282): the window returns it on
        // confirming, and without it in the double the whole thread would
        // not get exercised.
        manifest_digest: Some(format!("digest-de-{id}")),
    }
}

/// A tree with an extension catalogue.
pub(super) fn arbol_con_plugins(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    pages: &[(&str, &str)],
) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        plugins: plugins.into(),
        paginas: pages
            .iter()
            .map(|(id, md)| ((*id).to_owned(), (*md).to_owned()))
            .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// Waits until the side bar has a row whose title contains `needle`.
pub(super) async fn ayuda_con_fila(
    sub: &mut norte_ui_host::UiSubscription,
    needle: &str,
) -> norte_ui_host::dto::HelpView {
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(sub).await else {
            continue;
        };
        if v.sidebar.iter().any(|r| match r {
            norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => title.contains(needle),
            norte_ui_host::dto::HelpSidebarRowView::Group { .. } => false,
        }) {
            return v;
        }
    }
    panic!("the side bar never brought a row with {needle:?}");
}

/// An extension with a page appears in the side bar, and opening it
/// REQUESTS its page and installs it with its provenance line.
#[tokio::test]
async fn an_extension_with_a_page_is_read_from_help() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[("acme.ftp", "Conecta con un servidor FTP.")],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host alive");
    let help = ayuda_con_fila(&mut sub, "FTP de ACME").await;

    let row = help
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("the row is there");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(row).expect("fits"),
    })
    .await
    .expect("host alive");

    // The page arrives ASYNCHRONOUSLY: first the empty page with its name,
    // then the body once the daemon answers.
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
    let page = page.expect("the plugin's page installs");
    let text = format!("{:?}", page.blocks);
    assert!(text.contains("Conecta con un servidor FTP"), "{text}");
    // And it carries its provenance: a third-party page ALWAYS carries it,
    // or it would have the same shape as one from the binary.
    let badge = page.badge.expect("a plugin page carries a badge");
    assert!(badge.contains("ACME"), "it says who publishes it: {badge}");

    assert_eq!(
        backend
            .paginas_pedidas
            .lock()
            .expect("mutex")
            .as_slice()
            .iter()
            .filter(|i| i.as_str() == "acme.ftp")
            .count(),
        1,
        "the page is requested ONCE per opening"
    );
}

/// An id that is not valid reverse-DNS is DISCARDED at the entry: no row, no
/// request to the wire. Masking it would not work — it is not injective, so
/// two different plugins would land on the same row.
#[tokio::test]
async fn an_invalid_extension_id_neither_paints_nor_reaches_the_wire() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.\u{202e}ftp", "Malicioso", true),
            extension("sinpunto", "Tampoco", true),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;

    // The loop is given several rounds: if a row were going to arrive, it
    // would arrive here.
    for _ in 0..4 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snap = siguiente_foto(&mut sub).await;
        let sidebar = snap.help.map_or_else(Vec::new, |v| v.sidebar);
        for r in &sidebar {
            if let norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } = r {
                assert!(!title.contains("Malicioso"), "an invalid id got in");
                assert!(!title.contains("Tampoco"), "a dotless id got in");
            }
        }
    }
    assert!(
        backend.paginas_pedidas.lock().expect("mutex").is_empty(),
        "an invalid id is never sent to the wire"
    );
    let _ = help;
}

/// A hostile `help.md` is PARSED before painting: what crosses over are
/// blocks, and not a single terminal hazard travels inside them.
#[tokio::test]
async fn a_hostile_page_crosses_already_parsed_and_masked() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[(
            "acme.ftp",
            "Texto \u{202e}con override\u{7} y un pitido.\n\n{{cmd:pane.copy}}\n",
        )],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host alive");
    let help = ayuda_con_fila(&mut sub, "FTP de ACME").await;
    let row = help
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("the row is there");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(row).expect("fits"),
    })
    .await
    .expect("host alive");

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
    let page = page.expect("the page installs");
    let text = format!("{:?}", page.blocks);
    assert!(
        !text.contains('\u{202e}'),
        "a bidi override crossed over: {text}"
    );
    assert!(
        !text.contains('\u{7}'),
        "a control character crossed over: {text}"
    );
    // And a marker for ANOTHER's command — the binary's — does not resolve
    // in a plugin page: a third party does not borrow the host's shortcut.
    assert!(
        !text.contains("F5"),
        "a plugin page does not resolve someone else's markers: {text}"
    );
}

// ---------------------------------------------------------------------------
// What 4.4's reviews found.
// ---------------------------------------------------------------------------

/// `F1` with the VIEWER open opens help AND keeps the keys.
///
/// It used to not: the viewer went first in routing, so help got built,
/// travelled, and no key ever reached it — not even the one that closes it.
/// On top of that, in the DOM help was BEFORE the viewer, whose background is
/// opaque, so it was not even visible. A window with an open overlay that
/// answers nothing is the closest thing to being hung.
#[tokio::test]
async fn with_the_viewer_open_help_keeps_the_keys() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), b"hola".to_vec());
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if siguiente_foto(&mut sub).await.viewer.is_some() {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("help opens");
    assert_eq!(
        help.topic_id, "viewer",
        "and on the viewer's page, which is where the reader is"
    );

    // `esc` belongs to HELP, not the viewer: help is on top.
    h.dispatch(tecla("Escape")).await.expect("host alive");
    assert!(siguiente_ayuda(&mut sub).await.is_none(), "esc closes help");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.help.is_none(), "help left");
    assert!(
        snap.viewer.is_some(),
        "and the viewer is still where it was"
    );
}

/// The keys that scroll the BODY are resolved by the host with the reader's
/// keymap and travel as a REQUEST (bridge 76), numbered: the renderer
/// measures and scrolls once. It used to be that the renderer handled them
/// as fixed keys, and a rebind never reached the window.
#[tokio::test]
async fn the_body_scroll_keys_travel_as_a_request() {
    use norte_ui_host::dto::{HelpFocusView, HelpScrollTo};
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("help opens");
    assert_eq!(help.scroll, None, "just opened, nothing to scroll");

    // In the SIDE BAR, the page key moves the index: there is no request.
    h.dispatch(tecla("PageDown")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_eq!(help.scroll, None);

    h.dispatch(tecla("Tab")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("still open");
    assert_eq!(help.focus, HelpFocusView::Body);

    h.dispatch(tecla("PageDown")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("still open");
    let requested = help.scroll.expect("a request");
    assert_eq!(requested.to, HelpScrollTo::PageDown);

    h.dispatch(tecla("]")).await.expect("host alive");
    let help = siguiente_ayuda(&mut sub).await.expect("still open");
    let next = help.scroll.expect("another request");
    assert_eq!(next.to, HelpScrollTo::SectionNext);
    assert!(next.seq > requested.seq, "numbered: each one applies once");
}

/// Over a dialog that is being TYPED into, `F1` opens nothing.
///
/// Help keeps the keyboard, so opening it over a text field would turn the
/// `⌫` that fixes a typo into a step back in help.
#[tokio::test]
async fn help_does_not_open_over_a_text_field() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    // F7 opens the create-directory prompt.
    h.dispatch(tecla("F7")).await.expect("host alive");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if siguiente_foto(&mut sub)
            .await
            .dialogs
            .iter()
            .any(|d| d.input.is_some())
        {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.help.is_none(), "help did not open over the field");
    assert!(
        snap.dialogs.iter().any(|d| d.input.is_some()),
        "and the dialog is still waiting for the name"
    );
}

/// A row from ANOTHER screen is not offered enabled, and its reason says so.
///
/// The host's command list is flat — listing and viewer together — so asking
/// it plainly used to enable `viewer.close` with the viewer closed, only to
/// refuse when it was pressed. And a `dialog.*` verb is not run by this
/// window and has no reason to be: the dialog itself answers it with its own
/// buttons.
#[tokio::test]
async fn a_row_from_another_screen_is_not_offered_enabled() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut page = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if page.topic_id == "viewer" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host alive");
        page = siguiente_ayuda(&mut sub).await.expect("still open");
    }
    assert_eq!(page.topic_id, "viewer", "the viewer's page exists");
    // `pane.open`, `pane.edit` and `pane.edit-new` appear on this page and
    // are NOT the viewer's: all three act on the LISTING with the desktop's
    // application, so being live here is correct (#290 made editing the
    // second one, and create-and-edit the third). The other rows do need the
    // viewer.
    let from_listing = [
        norte_frontend::keymap::paint_chord("alt+f4"),
        norte_frontend::keymap::paint_chord("f4"),
        norte_frontend::keymap::paint_chord("shift+f4"),
    ];
    let runnable: Vec<&norte_ui_host::dto::HelpActionView> = page
        .actions
        .iter()
        .filter(|a| !a.opens_topic && !from_listing.contains(&a.chord))
        .collect();
    assert!(!runnable.is_empty(), "it documents commands");
    for a in runnable {
        assert!(
            !a.enabled,
            "with no viewer open, none of its rows can run: {a:?}"
        );
        assert!(!a.reason.is_empty(), "and each one says why: {a:?}");
    }
}

/// `enter` on a disabled row does NOT run it, and the page stays open.
///
/// The renderer puts no listener on a disabled row, but the keyboard does
/// not go through the renderer: the check has to be in the host or there is
/// an unlocked door.
#[tokio::test]
async fn enter_on_a_disabled_row_does_not_run_it() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut page = abrir_ayuda(&h, &mut sub).await;
    for row in 0..40 {
        if page.actions.iter().any(|a| !a.opens_topic && !a.enabled) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host alive");
        page = siguiente_ayuda(&mut sub).await.expect("still open");
    }
    let i = page
        .actions
        .iter()
        .position(|a| !a.opens_topic && !a.enabled)
        .expect("some page documents a command this window does not do");

    let ack = h
        .dispatch(UiAction::HelpActivate {
            index: u32::try_from(i).expect("fits"),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "it says it cannot be done: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(
        snap.help.is_some(),
        "and help is STILL open: the explanation is on the page"
    );
}

/// Every context this host declares has a page that claims it, in BOTH
/// languages.
///
/// Without this, renaming one of the corpus's front pages leaves `F1`
/// silently opening the index and no test turns red.
#[test]
fn every_declared_context_has_a_page() {
    for lang in [norte_help::Lang::En, norte_help::Lang::Es] {
        for c in norte_ui_host::controller::CONTEXTOS {
            assert!(
                norte_help::topic_for_context(lang, c).is_some(),
                "no page claims {c} in {lang:?}"
            );
        }
    }
}

/// NO string in projected help carries a terminal hazard, on any corpus page
/// and in both languages.
///
/// It is the invariant the whole design rests on — the renderer paints what
/// arrives and does not interpret it — and nothing asserted it. The WHOLE
/// projection is swept (title, badge, side bar, blocks, rows and reasons): a
/// new string that forgets to sanitize falls here without anyone having to
/// remember to add its own assertion.
#[tokio::test]
async fn no_help_string_carries_a_terminal_hazard() {
    /// Everything paintable in a projection, in a single string.
    fn everything(v: &norte_ui_host::dto::HelpView) -> String {
        use std::fmt::Write as _;

        let mut s = format!("{} {}", v.title, v.filter);
        if let Some(b) = &v.badge {
            s.push_str(b);
        }
        for r in &v.sidebar {
            match r {
                norte_ui_host::dto::HelpSidebarRowView::Group { label } => s.push_str(label),
                norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => s.push_str(title),
            }
        }
        // Blocks and rows are swept through their `Debug`, which includes
        // ALL their fields: that is exactly what makes a new string join the
        // sweep without touching this test.
        let _ = write!(s, "{:?}{:?}", v.blocks, v.actions);
        s
    }

    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let help = abrir_ayuda(&h, &mut sub).await;
    let rows = help.sidebar.len();
    let mut views = vec![help];
    for row in 0..rows {
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("fits"),
        })
        .await
        .expect("host alive");
        if let Some(v) = siguiente_ayuda(&mut sub).await {
            views.push(v);
        }
    }
    assert!(views.len() > 3, "several pages were walked through");
    for v in &views {
        let text = everything(v);
        // A `&str`'s `Debug` escapes controls as `\u{...}`, so it is
        // searched over the UNESCAPED text of flat fields and, for nested
        // ones, over the escaped form — which gives it away just the same.
        assert!(
            !text.chars().any(norte_encoding::is_terminal_hazard),
            "terminal hazard on page {}: {text:?}",
            v.topic_id
        );
        assert!(
            !text.contains("\\u{202e}") && !text.contains("\\u{7}"),
            "escaped hazard on page {}",
            v.topic_id
        );
    }
}
