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
        &norte_ui_host::commands::all(),
        Screen::Browse,
    )
    .expect("effective");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
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
        effects: norte_ui_host::commands::Effects::Full,
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

    let panel = next_whichkey(&mut sub).await.expect("there is a panel");
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
    h.dispatch(press("g")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.whichkey.is_none(), "the sequence closed");
}

/// Waits for the next update that carries the continuations panel.
pub(super) async fn next_whichkey(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::WhichKeyView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
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
// The command palette is the search box's `>` mode (spec 2026-10-09).
// ---------------------------------------------------------------------------

/// Waits for the next update that carries the search box.
pub(super) async fn next_goto(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::GotoView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update before the deadline")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Goto { goto } = c {
                    return goto.clone();
                }
            }
        }
    }
    panic!("no update with the search box ever arrived");
}

/// The `text` of every row the box paints, headers left out.
pub(super) fn goto_texts(g: &norte_ui_host::dto::GotoView) -> Vec<String> {
    g.lines
        .iter()
        .filter_map(|l| match l {
            norte_ui_host::dto::GotoLineView::Row { text, .. } => Some(text.clone()),
            norte_ui_host::dto::GotoLineView::Header { .. } => None,
        })
        .collect()
}

/// The `desc` of every row the box paints: a command row's id.
pub(super) fn goto_descs(g: &norte_ui_host::dto::GotoView) -> Vec<String> {
    g.lines
        .iter()
        .filter_map(|l| match l {
            norte_ui_host::dto::GotoLineView::Row { desc, .. } => Some(desc.clone()),
            norte_ui_host::dto::GotoLineView::Header { .. } => None,
        })
        .collect()
}

pub(super) fn key_for(k: &str) -> UiAction {
    press(k)
}

/// `ctrl+p` opens the box in commands mode with EVERYTHING the host
/// implements, each row with its name, its category and its real shortcut.
#[tokio::test]
async fn ctrl_p_opens_the_box_in_commands_mode() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");

    // The FIRST patch: the plugin rows have not arrived yet.
    let g = next_goto(&mut sub).await.expect("the box opens");
    assert_eq!(g.query, ">");
    assert_eq!(g.mode, norte_ui_host::dto::GotoModeView::Commands);
    assert_eq!(
        goto_texts(&g).len(),
        norte_ui_host::commands::all().len(),
        "offers everything implemented, no more and no less"
    );
    let enter = g
        .lines
        .iter()
        .find_map(|l| match l {
            norte_ui_host::dto::GotoLineView::Row {
                desc,
                text,
                chord,
                category,
                ..
            } if desc == "nav.enter" => Some((text, chord, category)),
            _ => None,
        })
        .expect("nav.enter is there");
    assert!(!enter.0.is_empty(), "each row says what it does");
    assert!(
        !enter.1.is_empty(),
        "and the shortcut comes from the preset, not a hand-written list"
    );
    assert!(!enter.2.is_empty(), "and where it is filed");
}

/// Typing NARROWS it, and what runs is whatever is selected — through the
/// same path as a keystroke.
#[tokio::test]
async fn typing_in_the_palette_narrows_it_and_enter_runs_it() {
    let (h, snap) = host_tree(fake_tree()).await;
    let cursor_before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await;

    for c in "cursor.down".chars() {
        h.dispatch(key_for(&c.to_string()))
            .await
            .expect("host alive");
    }
    // A snapshot, not the next patch: there are several queued and the first
    // one describes the box after the FIRST letter.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let filtered = next_snapshot(&mut sub).await.goto.expect("still open");
    assert_eq!(filtered.query, ">cursor.down");
    let descs = goto_descs(&filtered);
    assert_eq!(
        descs.first().map(String::as_str),
        Some("cursor.down"),
        "{descs:?}"
    );

    // Run: the box closes and the command runs.
    h.dispatch(key_for("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.goto.is_none(), "the box closes on running");
    assert_ne!(
        listing(&snap).cursor,
        cursor_before,
        "and the cursor command ran"
    );
}

/// A command run from the box leads the next empty `>` list, marked recent.
#[tokio::test]
async fn a_command_run_from_the_box_comes_back_first() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    for c in "cursor.down".chars() {
        h.dispatch(key_for(&c.to_string()))
            .await
            .expect("host alive");
    }
    h.dispatch(key_for("Enter")).await.expect("host alive");
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let g = next_snapshot(&mut sub).await.goto.expect("open again");
    let first = g.lines.iter().find_map(|l| match l {
        norte_ui_host::dto::GotoLineView::Row { desc, recent, .. } => Some((desc, *recent)),
        norte_ui_host::dto::GotoLineView::Header { .. } => None,
    });
    assert_eq!(first, Some((&"cursor.down".to_owned(), true)), "{g:?}");
}

/// `esc` closes it without running anything.
#[tokio::test]
async fn escape_closes_the_palette_without_running_anything() {
    let (h, snap) = host_tree(fake_tree()).await;
    let before = listing(&snap).cursor;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await;
    h.dispatch(key_for("Escape")).await.expect("host alive");
    assert!(next_goto(&mut sub).await.is_none(), "the box closes");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(listing(&snap).cursor, before, "and it ran nothing");
}

/// Running a command CLOSES the box in the patch stream, not only in the
/// next snapshot.
///
/// A renderer that applies patches — which is what the reference one does,
/// and what the sequence exists for — cannot find out the box closed unless
/// it requests a `Resync`. Before this test, `enter` sent the COMMAND's
/// patch and none of the palette's: the list stayed painted over the
/// listing until something, for another reason, triggered a snapshot.
#[tokio::test]
async fn running_from_the_palette_sends_its_closing_in_a_patch() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await.expect("the box opens");

    h.dispatch(key_for("Enter")).await.expect("host alive");
    assert!(
        next_goto(&mut sub).await.is_none(),
        "the closing travels as a patch, with no wait for a snapshot"
    );
}

/// Opening it while it is open switches the mode in place; a page moves by
/// what the box shows; F1 opens the page of the command under the cursor.
#[tokio::test]
async fn the_box_switches_pages_and_explains() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let g = next_goto(&mut sub).await.expect("open");
    assert_eq!(g.query, ">");
    h.dispatch(key_mod("g", true, false))
        .await
        .expect("host alive");
    let g = next_goto(&mut sub).await.expect("still open");
    assert_eq!(
        (g.query.as_str(), g.mode),
        ("", norte_ui_host::dto::GotoModeView::Places)
    );
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await;
    h.dispatch(press("PageDown")).await.expect("host alive");
    let g = next_goto(&mut sub).await.expect("open");
    assert!(
        g.cursor.is_some_and(|c| c > 1),
        "a page is more than one row: {:?}",
        g.cursor
    );
    h.dispatch(press("Home")).await.expect("host alive");
    for c in "pane.copy".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("F1")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(
        snap.goto.is_none() && snap.help.is_some(),
        "F1 swaps the box for its page"
    );
}

/// A box with its three `keymap*` fields built from `preset`; otherwise
/// `main.rs`'s `host_tree`.
async fn host_tree_with_preset(
    backend: Arc<Fake>,
    preset: &str,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    UiHost::start(UiHostOptions {
        backend,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset(preset).expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset(preset).expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap(preset).expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .await
    .expect("starts")
}

/// Vim binds `:` to the palette; inside the box `:` is a colon (Review
/// Focus 2).
#[tokio::test]
async fn vims_colon_types_inside_the_box() {
    let (h, _snap) = host_tree_with_preset(fake_tree(), "vim").await;
    let mut sub = h.subscribe();
    h.dispatch(press(":")).await.expect("host alive");
    let g = next_goto(&mut sub).await.expect("`:` opens the box");
    assert_eq!(g.query, ">");
    h.dispatch(press(":")).await.expect("host alive");
    let g = next_goto(&mut sub).await.expect("open");
    assert_eq!(g.query, ">:");
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
        &norte_ui_host::commands::all(),
        Screen::Browse,
    )
    .expect("a lua: key with a valid name also loads in the window");

    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
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
        effects: norte_ui_host::commands::Effects::Full,
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

/// The orthodox mark keys mark in the window, as the browser spells them:
/// `Insert` and `" "` (#378). Marking was only tested through the row
/// action, never through a key.
#[tokio::test]
async fn the_mark_keys_mark_in_the_window() {
    let (h, _snap) = host(vec!["a.txt", "b.txt", "c.txt", "d.txt"]).await;
    let mut sub = h.subscribe();

    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(press("Insert")).await.expect("host alive");
    h.dispatch(press(" ")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert_eq!(
        listing(&snap).marks,
        2,
        "Insert and space each marked a row"
    );
}
