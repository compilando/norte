use super::*;

// ---------------------------------------------------------------------------
// Which-key: what continues a half-typed prefix (phase 4, task 4.4).
// ---------------------------------------------------------------------------

/// A half-typed prefix shows WHAT can follow, with each key's label in the
/// user's language and saying which ones cannot be done here.
///
/// It is built by `norte_frontend::whichkey`, the same model that paints the
/// TUI: the renderer does not know how to resolve a prefix, only how to
/// paint what continues it.
#[tokio::test]
async fn a_half_typed_prefix_shows_what_follows() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};

    let preset =
        parse_keymap(norte_frontend::keymap::presets::source("orthodox").expect("factory preset"))
            .expect("preset parses");
    // A two-key sequence, which is what which-key exists to show. No factory
    // preset uses them in `pane`.
    let layer = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [
    { on = ["ctrl+x", "g"], run = "cursor.top" },
    { on = ["ctrl+x", "b"], run = "cursor.bottom" },
]
"#,
    )
    .expect("layer parses");
    let keymap = Effective::build_for(
        &preset,
        &[layer],
        &norte_ui_host::commands::todos(),
        Screen::Browse,
    )
    .expect("effective");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
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
    let mut sub = h.subscribe();

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "x".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");

    let panel = siguiente_whichkey(&mut sub)
        .await
        .expect("there is a panel");
    assert!(
        !panel.title.is_empty(),
        "the panel says which prefix it describes"
    );
    let keys: Vec<&str> = panel.rows.iter().map(|r| r.chord.as_str()).collect();
    assert!(
        keys.contains(&"g") && keys.contains(&"b"),
        "it shows both continuations: {keys:?}"
    );
    for row in &panel.rows {
        assert!(!row.label.is_empty(), "each key says what it does: {row:?}");
    }

    // And once the sequence completes, the panel goes away: it described
    // keys that are no longer alive.
    h.dispatch(tecla("g")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.whichkey.is_none(), "the sequence closed");
}

/// Waits for the next update that carries the continuations panel.
pub(super) async fn siguiente_whichkey(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::WhichKeyView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::WhichKey { whichkey } = c {
                    return whichkey.clone();
                }
            }
        }
    }
    panic!("no update with a panel ever arrived");
}

// ---------------------------------------------------------------------------
// The command palette (phase 4, task 4.4).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries the palette.
pub(super) async fn siguiente_paleta(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::PaletteView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Palette { palette } = c {
                    return palette.clone();
                }
            }
        }
    }
    panic!("no update with a palette ever arrived");
}

pub(super) fn tecla_de(k: &str) -> UiAction {
    tecla(k)
}

/// `ctrl+p` opens the palette with EVERYTHING the host implements, each row
/// with its description and its real shortcut.
#[tokio::test]
async fn the_palette_offers_what_the_host_implements() {
    let (h, _snap) = host_arbol(arbol()).await;
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

    let p = siguiente_paleta(&mut sub).await.expect("the palette opens");
    assert!(p.query.is_empty(), "starts with no filter");
    assert_eq!(
        usize::try_from(p.total).unwrap_or(usize::MAX),
        norte_ui_host::commands::todos().len(),
        "offers everything implemented, no more and no less"
    );
    assert_eq!(p.rows.len() as u64, p.total, "with no filter, all are seen");
    let enter = p
        .rows
        .iter()
        .find(|r| r.text == "nav.enter")
        .expect("nav.enter is there");
    assert!(!enter.desc.is_empty(), "each row says what it does");
    assert_ne!(
        enter.chord, "—",
        "and the shortcut comes from the preset, not a hand-written list"
    );
}

/// Typing NARROWS it, and what runs is whatever is selected — through the
/// same path as a keystroke.
#[tokio::test]
async fn typing_in_the_palette_narrows_it_and_enter_runs_it() {
    let (h, snap) = host_arbol(arbol()).await;
    let cursor_before = listado(&snap).cursor;
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
    let _ = siguiente_paleta(&mut sub).await;

    for c in ["c", "u", "r", "s", "o", "r"] {
        h.dispatch(tecla_de(c)).await.expect("host alive");
    }
    // A snapshot, not the next patch: there are six queued and the first one
    // describes the palette after the FIRST letter.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let filtered = siguiente_foto(&mut sub).await.palette.expect("still open");
    assert!(
        !filtered.rows.is_empty()
            && filtered.rows.len() < usize::try_from(filtered.total).unwrap_or(usize::MAX),
        "typing narrows it: {} of {}",
        filtered.rows.len(),
        filtered.total
    );
    assert!(
        filtered.rows.iter().all(|r| {
            // The filter matches against what is PAINTED — name and
            // description — which is what the shared model folds together:
            // a row whose description mentions the cursor matches too, and
            // that is correct.
            let haystack = format!("{} {}", r.text, r.desc).to_lowercase();
            haystack.contains("cursor")
        }),
        "and what is left matches what was typed: {:?}",
        filtered.rows
    );

    // Move down and run: the palette closes and the command runs.
    h.dispatch(tecla_de("ArrowDown")).await.expect("host alive");
    h.dispatch(tecla_de("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert!(snap.palette.is_none(), "the palette closes on running");
    assert_ne!(
        listado(&snap).cursor,
        cursor_before,
        "and the cursor command ran"
    );
}

/// `esc` closes it without running anything.
#[tokio::test]
async fn escape_closes_the_palette_without_running_anything() {
    let (h, snap) = host_arbol(arbol()).await;
    let before = listado(&snap).cursor;
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
    let _ = siguiente_paleta(&mut sub).await;
    h.dispatch(tecla_de("Escape")).await.expect("host alive");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "the palette closes"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&snap).cursor, before, "and it ran nothing");
}

/// Running from the palette CLOSES the palette in the patch stream, not only
/// in the next snapshot.
///
/// A renderer that applies patches — which is what the reference one does,
/// and what the sequence exists for — cannot find out the palette closed
/// unless it requests a `Resync`. Before this test, `enter` sent the
/// COMMAND's patch and none of the palette's: the list stayed painted over
/// the listing until something, for another reason, triggered a snapshot.
#[tokio::test]
async fn running_from_the_palette_sends_its_closing_in_a_patch() {
    let (h, _snap) = host_arbol(arbol()).await;
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

    h.dispatch(tecla_de("Enter")).await.expect("host alive");
    assert!(
        siguiente_paleta(&mut sub).await.is_none(),
        "the closing travels as a patch, with no wait for a snapshot"
    );
}

/// A key bound to `lua:` in the user's layer says it is not here (ADR 0110).
///
/// The window does not run Lua. It used to resolve it as available — the Lua
/// registry is dynamic and the keymap could not know who hosts it — so the
/// sheet, which-key and the palette all advertised it and the key did
/// nothing.
#[tokio::test]
async fn a_lua_key_says_it_is_not_here() {
    use norte_frontend::keymap::{Effective, Screen, parse_keymap, parse_keymap_layer};

    let preset =
        parse_keymap(norte_frontend::keymap::presets::source("orthodox").expect("factory preset"))
            .expect("preset parses");
    let layer = parse_keymap_layer(
        r#"
[pane]
prepend_keymap = [{ on = ["ctrl+x"], run = "lua:saluda" }]
"#,
    )
    .expect("layer parses");
    let keymap = Effective::build_for(
        &preset,
        &[layer],
        &norte_ui_host::commands::todos(),
        Screen::Browse,
    )
    .expect("a lua: key with a valid name also loads in the window");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: arbol(),
        initial_dir: dir(),
        initial_dir_pedido: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
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

    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "x".to_owned(),
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host alive");
    match ack {
        ActionAck::Unavailable { reason_key } => assert_eq!(reason_key, "cmd-not-here"),
        other => panic!("expected not available here: {other:?}"),
    }
}
