//! Several shells in the terminal panel (spec 2026-10-09). Real `/bin/sh`
//! over a real local directory: the fake backend serves `file://` for the
//! listing, the shell sits in the temp dir.

use super::*;
use norte_ui_host::dto::{TerminalSlotView, ViewChange};

/// A host whose listing is a real local directory, with these shell
/// profiles (`None` = the implicit one).
async fn local_host(
    profiles: Option<&str>,
) -> (UiHost, norte_ui_host::UiSubscription, tempfile::TempDir) {
    local_host_with(profiles, None).await
}

/// [`local_host`] with a `commands.toml` too.
async fn local_host_with(
    profiles: Option<&str>,
    user_commands: Option<&str>,
) -> (UiHost, norte_ui_host::UiSubscription, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tmp");
    let wire = format!("file://{}", tmp.path().display());
    let mut fake = Fake::default();
    fake.put(&wire, Vec::new());
    let mut settings = test_settings();
    if let Some(toml) = profiles {
        let file = norte_frontend::shell_profiles::ShellProfiles::parse(toml).expect("parses");
        settings.shell_profiles =
            norte_frontend::shell_profiles::ShellProfiles::merge(vec![file]).expect("merges");
    }
    if let Some(src) = user_commands {
        settings.user_commands =
            norte_frontend::user_commands::UserCommands::parse(src).expect("valid");
    }
    let (h, _) = Box::pin(UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: VPath::parse(&wire).expect("vpath"),
        initial_dir_requested: true,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: norte_ui_host::commands::Effects::Full,
        log_ring: None,
    }))
    .await
    .expect("starts");
    let sub = h.subscribe();
    (h, sub, tmp)
}

/// Waits (bounded, no sleep) for a terminal view that satisfies `ok`.
async fn terminal_until(
    sub: &mut norte_ui_host::UiSubscription,
    ok: impl Fn(&TerminalSlotView) -> bool,
) -> TerminalSlotView {
    let wait = async {
        loop {
            let found = match sub.recv().await.expect("host alive") {
                Update::Message(m) => match m.payload {
                    UiUpdate::Snapshot(s) => s.slots.into_iter().find_map(|x| match x {
                        SlotView::Terminal(t) => Some(*t),
                        _ => None,
                    }),
                    UiUpdate::Patch(p) => p.changes.into_iter().find_map(|c| match c {
                        ViewChange::Terminal { terminal } => Some(*terminal),
                        _ => None,
                    }),
                    UiUpdate::Notice(_) => None,
                },
                Update::Lagged => None,
            };
            if let Some(t) = found
                && ok(&t)
            {
                return t;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), wait)
        .await
        .expect("the terminal view arrived in time")
}

/// Opens the panel with its key (orthodox: `ctrl+alt+s`) and waits for the
/// first live shell.
async fn open_panel(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) -> TerminalSlotView {
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "s".to_owned(),
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    terminal_until(sub, |t| t.instances.len() == 1 && !t.no_shell).await
}

/// Types a line into the shell in front (the panel has the keyboard).
async fn type_line(h: &UiHost, line: &str) {
    for c in line.chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("enter")).await.expect("host alive");
}

#[tokio::test]
async fn new_adds_a_second_instance_and_activates_it() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let first = open_panel(&h, &mut sub).await;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    let t = terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    assert_ne!(t.active, first.active);
    assert_eq!(t.active, t.instances.last().map(|i| i.id));
    // The list on the right takes its cells from the shell, and says how
    // many: one shell shows no list and takes none.
    assert_eq!(first.list_cols, 0);
    assert!(t.list_cols > 0);
}

/// From INSIDE the panel, `terminal.prev`'s chord (orthodox
/// `ctrl+alt+pgup`) switches instances instead of reaching the shell.
#[tokio::test]
async fn a_pass_through_chord_switches_instead_of_typing() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let first = open_panel(&h, &mut sub).await.active;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "pgup".to_owned(),
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    terminal_until(&mut sub, |t| t.active == first).await;
}

/// Closing the LAST shell closes its panel. It used to leave an empty "no
/// shell" box that the panel's key could not close.
#[tokio::test]
async fn closing_the_last_shell_closes_the_panel() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let t = open_panel(&h, &mut sub).await;
    let id = t.active.expect("a shell in front");
    h.dispatch(UiAction::TerminalClose { id })
        .await
        .expect("host alive");
    snapshot_until(&h, &mut sub, "the terminal slot gone", |s| {
        (!s.slots.iter().any(|x| matches!(x, SlotView::Terminal(_)))).then_some(())
    })
    .await;
}

/// A shell that EXITS on its own leaves the panel, empty; the panel's ✕
/// (a close naming no live shell) closes it. Nothing could before.
#[tokio::test]
async fn an_empty_panel_closes_with_its_close_button() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    type_line(&h, "exit").await;
    terminal_until(&mut sub, |t| t.instances.is_empty()).await;
    h.dispatch(UiAction::TerminalClose { id: 0 })
        .await
        .expect("host alive");
    snapshot_until(&h, &mut sub, "the empty terminal slot gone", |s| {
        (!s.slots.iter().any(|x| matches!(x, SlotView::Terminal(_)))).then_some(())
    })
    .await;
}

/// `alt+x` inside the panel is the shell's (Emacs's `M-x`), NOT
/// `layout.close-slot`: closing the slot would kill every shell in it.
#[tokio::test]
async fn alt_x_inside_reaches_the_shell_and_closes_nothing() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "x".to_owned(),
        ctrl: false,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the panel is still there: {ack:?}"
    );
}

/// Exit 0 removes the instance; exit 3 keeps it, saying so.
#[tokio::test]
async fn exit_zero_removes_exit_three_stays() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    type_line(&h, "echo lastword; exit 3").await;
    let t = terminal_until(&mut sub, |t| t.exited == Some(3)).await;
    assert_eq!(t.instances.len(), 2, "a failed shell stays");
    let screen: String = t.rows.iter().flatten().map(|s| s.text.as_str()).collect();
    assert!(
        screen.matches("lastword").count() >= 2,
        "with its last screen, the echo's OUTPUT included: {screen:?}"
    );
    let first = t.instances[0].id;
    h.dispatch(UiAction::TerminalSelect { id: first })
        .await
        .expect("host alive");
    terminal_until(&mut sub, |t| t.active == Some(first)).await;
    type_line(&h, "exit").await;
    let t = terminal_until(&mut sub, |t| t.instances.len() == 1).await;
    assert_eq!(t.instances[0].exited, Some(3), "the clean one left");
}

/// With every shell exited (kept, failed), coming back into the panel
/// starts a live one: otherwise the panel is a still picture forever.
#[tokio::test]
async fn coming_back_with_only_failed_shells_starts_a_new_one() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    type_line(&h, "exit 3").await;
    terminal_until(&mut sub, |t| t.exited == Some(3)).await;
    let door = || {
        UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "s".to_owned(),
            ctrl: true,
            alt: true,
            shift: false,
            meta: false,
        })
    };
    // Out, and back in.
    h.dispatch(door()).await.expect("host alive");
    h.dispatch(door()).await.expect("host alive");
    let t = terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    assert_eq!(t.exited, None, "the new one is in front, alive");
}

/// Inside a panel with no shell, its key starts one instead of leaving.
#[tokio::test]
async fn the_door_key_inside_an_empty_panel_starts_a_shell() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    type_line(&h, "exit").await;
    terminal_until(&mut sub, |t| t.no_shell).await;
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "s".to_owned(),
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host alive");
    terminal_until(&mut sub, |t| t.instances.len() == 1 && !t.no_shell).await;
}

/// `terminal.close` by key with the panel BEHIND a tab kills nothing: the
/// reader cannot see which shell that would be.
#[tokio::test]
async fn closing_a_hidden_terminal_kills_nothing() {
    let key = |k: &str, ctrl: bool, alt: bool| {
        UiAction::Key(norte_ui_host::keys::KeyInput {
            key: k.to_owned(),
            ctrl,
            alt,
            shift: false,
            meta: false,
        })
    };
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    // Out of the panel, then the log over it (same bottom group).
    h.dispatch(key("s", true, true)).await.expect("host alive");
    h.dispatch(key("l", false, true)).await.expect("host alive");
    let ack = h.dispatch(key("w", true, true)).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "refused, not applied: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let t = terminal_until(&mut sub, |_| true).await;
    assert_eq!(t.instances.len(), 1, "the shell lives");
}

/// The last shell leaving hands the keyboard back to the listing, as the
/// TUI does: keys sent into a panel with no shell went nowhere.
#[tokio::test]
async fn the_last_clean_exit_gives_the_keyboard_back() {
    use norte_ui_host::dto::SlotRole;
    let (h, mut sub, _tmp) = local_host(None).await;
    let t = open_panel(&h, &mut sub).await;
    type_line(&h, "exit").await;
    terminal_until(&mut sub, |t| t.no_shell).await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    let active = snap
        .layout
        .placements
        .iter()
        .find(|p| p.role == Some(SlotRole::Active))
        .map(|p| p.slot_id);
    assert_ne!(active, Some(t.slot_id), "the listing has the keyboard");
}

/// The last shell leaving leaves the panel saying "no shell".
#[tokio::test]
async fn the_last_exit_leaves_no_shell() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    type_line(&h, "exit").await;
    let t = terminal_until(&mut sub, |t| t.no_shell).await;
    assert!(t.instances.is_empty());
}

/// A shell behind that writes gets the unseen mark.
#[tokio::test]
async fn output_behind_marks_the_instance_unseen() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let first = open_panel(&h, &mut sub).await;
    let first = first.active.expect("one in front");
    type_line(&h, "sleep 1; echo late").await;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    let t = terminal_until(&mut sub, |t| {
        t.instances.iter().any(|i| i.id == first && i.unseen)
    })
    .await;
    assert_ne!(t.active, Some(first));
}

/// A shell profile whose program does not exist adds nothing and touches
/// nothing.
#[tokio::test]
async fn a_missing_program_adds_nothing() {
    let (h, mut sub, _tmp) = local_host(Some(
        "default = \"sh\"\n[[shell]]\nname = \"sh\"\nprogram = \"/bin/sh\"\n\
         [[shell]]\nname = \"ghost\"\nprogram = \"/nonexistent/ghost-shell\"\n",
    ))
    .await;
    open_panel(&h, &mut sub).await;
    let ack = h
        .dispatch(UiAction::TerminalNew {
            profile: Some("ghost".to_owned()),
        })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, ActionAck::Applied { .. }),
        "a shell that did not start is not applied: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let t = terminal_until(&mut sub, |_| true).await;
    assert_eq!(t.instances.len(), 1);
}

/// An id that is not there is refused, never a panic.
#[tokio::test]
async fn an_unknown_id_is_refused() {
    let (h, mut sub, _tmp) = local_host(None).await;
    open_panel(&h, &mut sub).await;
    for a in [
        UiAction::TerminalSelect { id: 999 },
        UiAction::TerminalClose { id: 999 },
        UiAction::TerminalRename {
            id: 999,
            name: "x".into(),
        },
    ] {
        let ack = h.dispatch(a).await.expect("host alive");
        assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    }
}

/// Bad colour or icon from the renderer is refused, not stored.
#[tokio::test]
async fn a_bad_decoration_is_refused() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let t = open_panel(&h, &mut sub).await;
    let id = t.active.expect("one");
    for (icon, color) in [(Some("rocket".to_owned()), None), (None, Some(9))] {
        let ack = h
            .dispatch(UiAction::TerminalDecorate { id, icon, color })
            .await
            .expect("host alive");
        assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    }
    h.dispatch(UiAction::TerminalDecorate {
        id,
        icon: Some("server".into()),
        color: Some(2),
    })
    .await
    .expect("host alive");
    let t = terminal_until(&mut sub, |t| t.instances[0].color == Some(2)).await;
    assert_eq!(t.instances[0].icon.as_deref(), Some("server"));
}

/// Rename names the tab; closing the slot kills every shell and the panel
/// stops answering the instance actions.
#[tokio::test]
async fn rename_then_closing_the_slot_kills_them_all() {
    let (h, mut sub, _tmp) = local_host(None).await;
    let t = open_panel(&h, &mut sub).await;
    let id = t.active.expect("one");
    h.dispatch(UiAction::TerminalRename {
        id,
        name: "build".into(),
    })
    .await
    .expect("host alive");
    terminal_until(&mut sub, |t| t.instances[0].title == "build").await;
    h.dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    terminal_until(&mut sub, |t| t.instances.len() == 2).await;
    // `layout.close-slot` with the panel focused, by NAME from the menu:
    // its chord no longer reaches norte from inside (that `alt+x` is
    // Emacs's `M-x`).
    let (menu, row) = norte_frontend::menu::MENUS
        .iter()
        .enumerate()
        .find_map(|(m, menu)| {
            menu.items()
                .position(|c| c == "layout.close-slot")
                .map(|r| (m, r))
        })
        .expect("close-slot is in a menu");
    h.dispatch(UiAction::MenuOpen {
        menu: u32::try_from(menu).expect("fits"),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::MenuActivateRow {
        row: u32::try_from(row).expect("fits"),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::TerminalNew { profile: None })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Unavailable { .. }),
        "no panel, no instances: {ack:?}"
    );
}

/// A terminal-mode user command with NO panel open opens the slot and runs
/// the command in it, and nothing else: no default login shell beside it
/// (real `/bin/sh`, so the PTY is the real one, not faked).
#[tokio::test]
async fn a_terminal_user_command_with_no_panel_leaves_exactly_one_instance() {
    let src = "[[command]]\nid = \"nap\"\nlabel = \"Nap\"\nargv = [\"sh\", \"-c\", \"sleep 30\"]\nrun_in = \"terminal\"\n";
    let (h, mut sub, _tmp) = local_host_with(None, Some(src)).await;
    run_by_palette(&h, &mut sub, "user:nap").await;
    let view = terminal_until(&mut sub, |t| t.instances.iter().any(|i| i.title == "Nap")).await;
    assert_eq!(
        view.instances.len(),
        1,
        "only the command, no default shell: {:?}",
        view.instances.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
}
