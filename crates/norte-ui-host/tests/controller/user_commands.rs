//! User commands in the window (ADR 0177): a key or the box asks, the shared
//! module decides, and a background run leaves as a native effect.

use super::*;

use norte_frontend::user_commands::Finished;
use norte_ui_host::dto::NativeEffect;
use std::time::Duration;

fn cfg_with(src: &str) -> norte_frontend::config::FrontendConfig {
    let mut cfg = test_settings();
    cfg.user_commands = norte_frontend::user_commands::UserCommands::parse(src).expect("valid");
    cfg
}

const PNG: &str = "[[command]]\nid = \"png\"\nlabel = \"Optimize\"\nargv = [\"oxipng\", \"%F\"]\n";

/// The host's language in these tests (`host_en_con` starts it in `es`).
fn es(key: &str, args: &[(&str, &str)]) -> String {
    norte_i18n::ta_in(norte_i18n::Lang::Es, key, args)
}

#[tokio::test]
async fn the_box_runs_a_background_user_command_as_a_native_effect() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg_with(PNG)).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();
    run_by_palette(&h, &mut sub, "user:png").await;
    let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
        .await
        .expect("in time")
        .expect("an effect");
    let NativeEffect::RunUserCommand { launch } = effect else {
        panic!("{effect:?}")
    };
    assert_eq!(launch.id, "png");
    assert_eq!(launch.args, [std::ffi::OsString::from("/casa/notas.txt")]);
    assert_eq!(launch.cwd, std::path::PathBuf::from("/casa"));
    assert_eq!(
        next_status_message(&h, &mut sub).await,
        es("user-cmd-started", &[("label", "Optimize")])
    );
}

#[tokio::test]
async fn confirm_true_asks_before_the_effect() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let cfg = cfg_with(&format!("{PNG}confirm = true\n"));
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();
    run_by_palette(&h, &mut sub, "user:png").await;
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1);
    let d = &dialogs[0];
    assert_eq!(d.title_key, "user-cmd-confirm-title");
    assert_eq!(
        d.body[0].text,
        es(
            "user-cmd-confirm-one",
            &[("label", "Optimize"), ("count", "1")]
        )
    );
    assert!(
        matches!(
            effects.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "nothing runs before the answer"
    );
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".into(),
        secret: None,
    })
    .await
    .expect("host alive");
    let effect = tokio::time::timeout(Duration::from_secs(2), effects.recv())
        .await
        .expect("in time")
        .expect("an effect");
    assert!(
        matches!(effect, NativeEffect::RunUserCommand { ref launch } if launch.id == "png"),
        "{effect:?}"
    );
}

#[tokio::test]
async fn a_remote_pane_refuses_with_the_reason() {
    let mut fake = Fake::default();
    fake.put("sftp://h/d", vec![(b"notas.txt".to_vec(), false)]);
    let (h, _snap) = host_en_con(Arc::new(fake), "sftp://h/d", cfg_with(PNG)).await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();
    run_by_palette(&h, &mut sub, "user:png").await;
    assert_eq!(
        next_status_message(&h, &mut sub).await,
        norte_i18n::t_in(norte_i18n::Lang::Es, "user-cmd-remote")
    );
    assert!(matches!(
        effects.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn finished_says_how_it_ended() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg_with(PNG)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::UserCommandFinished {
        label: "Optimize".into(),
        finished: Finished::Failed { code: 2 },
        refresh: false,
    })
    .await
    .expect("host alive");
    let said = next_status_message(&h, &mut sub).await;
    assert_eq!(
        said,
        es("user-cmd-failed", &[("label", "Optimize"), ("code", "2")])
    );
    assert!(said.contains("Optimize") && said.contains('2'), "{said}");
}

/// A finished run re-lists the panes only when it asked (`refresh`) and the
/// program actually ran.
#[tokio::test]
async fn finished_re_lists_only_when_asked_and_it_ran() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let fake = Arc::new(fake);
    let (h, _snap) = host_en_con(fake.clone(), "file:///casa", cfg_with(PNG)).await;
    let mut sub = h.subscribe();
    let finish = |finished, refresh| UiAction::UserCommandFinished {
        label: "Optimize".into(),
        finished,
        refresh,
    };
    // Settle: the first listing has been served.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = next_snapshot(&mut sub).await;
    let base = fake.listings.load(SeqCst);

    // It did not run: no re-list, however much it asked.
    for f in [
        Finished::Missing {
            program: "oxipng".into(),
        },
        Finished::NotStarted { error: "x".into() },
    ] {
        h.dispatch(finish(f, true)).await.expect("host alive");
    }
    // It ran but did not ask: none either.
    h.dispatch(finish(Finished::Ok, false))
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = next_snapshot(&mut sub).await;
    assert_eq!(fake.listings.load(SeqCst), base, "nothing re-listed");

    // It ran and asked: the panes are listed again.
    h.dispatch(finish(Finished::Ok, true))
        .await
        .expect("host alive");
    let wait = async {
        while fake.listings.load(SeqCst) == base {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("re-listed in time");
}

/// A key bound to `user:<id>` that commands.toml does not define loads (the
/// window knows the `user:` namespace) and says so when pressed.
#[tokio::test]
async fn a_key_to_an_undefined_user_command_says_it_is_not_defined() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let layer = norte_frontend::keymap::parse_keymap(
        "[pane]\nprepend_keymap = [{ on = [\"alt+o\"], run = \"user:gone\" }]\n",
    )
    .expect("a valid keymap");
    let keymap = norte_ui_host::keys::preset_keymap_with_layers(
        "orthodox",
        &[layer],
        norte_ui_host::commands::Effects::Full,
    )
    .expect("`user:` bindings load in a full window");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: VPath::parse("file:///casa").expect("vpath"),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: cfg_with(PNG),
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
    let ack = h.dispatch(key_alt("o")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "user-cmd-unknown"),
        "{ack:?}"
    );
    assert_eq!(
        next_status_message(&h, &mut sub).await,
        es("user-cmd-unknown", &[("id", "gone")])
    );
}

/// A read-only window never runs a user command: the bound key is refused,
/// the box offers no user row, and nothing leaves as a native effect.
#[tokio::test]
async fn a_read_only_window_never_runs_them() {
    let solo = norte_ui_host::commands::Effects::SoloRead;
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let layer = norte_frontend::keymap::parse_keymap(
        "[pane]\nprepend_keymap = [{ on = [\"alt+o\"], run = \"user:png\" }]\n",
    )
    .expect("a valid keymap");
    let keymap = norte_ui_host::keys::preset_keymap_with_layers("orthodox", &[layer], solo)
        .expect("a `user:` binding still loads, as unavailable");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(fake),
        initial_dir: VPath::parse("file:///casa").expect("vpath"),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap,
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: cfg_with(PNG),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::default_columns(),
        effects: solo,
        log_ring: None,
    })
    .await
    .expect("starts");
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    // The key: refused, never run.
    let ack = h.dispatch(key_alt("o")).await.expect("host alive");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");

    // The box under `>`: no user row to pick, so the box path cannot reach it.
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await;
    for c in "optimize".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let g = next_snapshot(&mut sub).await.goto.expect("the box is open");
    assert!(
        !goto_descs(&g).iter().any(|d| d.starts_with("user:")),
        "{:?}",
        goto_descs(&g)
    );
    assert!(!goto_texts(&g).iter().any(|t| t == "Optimize"));
    h.dispatch(press("Enter")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = next_snapshot(&mut sub).await;

    assert!(
        matches!(
            effects.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "no program started"
    );
}

/// `F1` on a user row says its description on the bar; the box stays open.
#[tokio::test]
async fn f1_on_a_user_row_says_its_description() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let cfg = cfg_with(&format!("{PNG}description = \"Shrinks the PNGs\"\n"));
    let (h, _snap) = host_en_con(Arc::new(fake), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_goto(&mut sub).await;
    for c in "optimize".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("F1")).await.expect("host alive");
    assert_eq!(next_status_message(&h, &mut sub).await, "Shrinks the PNGs");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        next_snapshot(&mut sub).await.goto.is_some(),
        "the box stays"
    );
}
