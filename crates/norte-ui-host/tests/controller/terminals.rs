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
    // `layout.close-slot` from inside the panel: orthodox `alt+x`, one of
    // the chords the panel lets through.
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
        matches!(ack, ActionAck::Unavailable { .. }),
        "no panel, no instances: {ack:?}"
    );
}
