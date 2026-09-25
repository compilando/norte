//! Tests for the layered config (ADR 0007): precedence, file-backed
//! diagnostics, and unknown keys as a clear error.

use norte_tui::config::{ConfigError, Layer, Layers, load};
use norte_tui::keymap::{Effective, Screen, presets};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        std::fs::write(d.path().join(name), content).expect("write");
    }
    d
}

#[test]
fn defaults_without_any_layer() {
    let cfg = load(&Layers { dirs: vec![] }).expect("defaults");
    assert_eq!(
        cfg.common.preset, "orthodox",
        "compiled default (decision 2026-07-10)"
    );
    assert!(cfg.keymap_layers.is_empty());
}

#[test]
fn last_one_wins_per_field_and_keymap_layers_accumulate() {
    let system = dir_with(&[
        ("norte.toml", "[keymap]\npreset = \"cua\"\n"),
        (
            "keymap.toml",
            "[pane]\nappend_keymap = [{ on = [\"x\"], run = \"app.quit\" }]\n",
        ),
    ]);
    let user = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let project = dir_with(&[(
        "keymap.toml",
        "[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"cursor.top\" }]\n",
    )]);
    let layers = Layers {
        dirs: vec![
            (system.path().to_path_buf(), Layer::System),
            (user.path().to_path_buf(), Layer::User),
            (project.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("load");
    assert_eq!(
        cfg.common.preset, "vim",
        "the user's preset overrides the system's"
    );
    assert_eq!(
        cfg.keymap_layers.len(),
        2,
        "keymap layers do NOT override each other: they stack (ADR 0007)"
    );
}

#[test]
fn a_broken_toml_names_the_file() {
    let mala = dir_with(&[("norte.toml", "esto no es toml ===")]);
    match load(&Layers {
        dirs: vec![(mala.path().to_path_buf(), Layer::User)],
    }) {
        Err(ConfigError::Toml { path, .. }) => {
            assert!(
                path.ends_with("norte.toml"),
                "file-backed diagnostic: {path:?}"
            );
        }
        other => panic!("expected Toml, got {other:?}"),
    }
}

#[test]
fn unknown_key_is_a_clear_error() {
    let mala = dir_with(&[("norte.toml", "[keymap]\npresett = \"vim\"\n")]);
    match load(&Layers {
        dirs: vec![(mala.path().to_path_buf(), Layer::User)],
    }) {
        Err(ConfigError::Toml { path, .. }) => {
            assert!(path.ends_with("norte.toml"));
        }
        other => panic!("expected Toml (deny_unknown_fields), got {other:?}"),
    }
}

#[test]
fn dir_without_files_does_not_bother() {
    let empty = dir_with(&[]);
    let cfg = load(&Layers {
        dirs: vec![
            (empty.path().to_path_buf(), Layer::User),
            ("/no/existe/en/absoluto".into(), Layer::Project),
        ],
    })
    .expect("absent layers = defaults");
    assert_eq!(cfg.common.preset, "orthodox");
}

/// Rule 3 for the watcher's poll: it detects changes, and dropping the
/// `Watch` CANCELS it cleanly (the task drops its sender → the channel
/// closes).
#[tokio::test]
async fn polling_detects_changes_and_cancels_cleanly() {
    let d = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = norte_tui::config::watch_polling(&layers, tx, std::time::Duration::from_millis(20));

    // The poll takes its base snapshot in its own task, so there is no
    // "you can change the file now" to wait for: sleeping a multiple of the
    // period was betting the base was taken before the write, and under
    // load it lost (wave W9, task 2.2). Instead a different change (mtime
    // AND size) is written every round until a tick sees it: if the base
    // arrived after the first write, the second one gives it away.
    let mut seen = None;
    for i in 0..15u32 {
        let mut content = String::from("[keymap]\npreset = \"orthodox\"\n");
        for _ in 0..i {
            content.push_str("# round\n");
        }
        std::fs::write(d.path().join("norte.toml"), content).unwrap();
        if let Ok(Some(())) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            seen = Some(());
            break;
        }
    }
    assert!(seen.is_some(), "the poll sees the change");

    drop(watch);
    // After cancelling, the task ends and drops the sender: recv → None
    // (draining whatever events were still in flight).
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await {
            Ok(Some(())) => {}
            Ok(None) => break,
            Err(timeout) => {
                panic!("the polling task did not end after dropping the Watch: {timeout}")
            }
        }
    }
}

/// HIGH (security review M4 Lua): `load` marks the `Layer::Project` layer's
/// `keymap.toml` (`./.norte`; debt #75 closed: the kind travels PER DIR) as
/// PROJECT, and that mark flows through to `Effective::build_for`, which
/// discards its `lua:` bindings (a hostile repo does not rebind keys to the
/// user's Lua commands). The user layer is NOT marked.
#[test]
fn the_last_layer_keymap_is_marked_as_project() {
    let binding = "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"lua:pwn\" }]\n";
    let user = dir_with(&[("keymap.toml", binding)]);
    let project = dir_with(&[("keymap.toml", binding)]);
    let layers = Layers {
        dirs: vec![
            (user.path().to_path_buf(), Layer::User),
            (project.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("load");
    assert_eq!(cfg.keymap_layers.len(), 2);
    assert!(!cfg.keymap_layers[0].is_project(), "user layer");
    assert!(cfg.keymap_layers[1].is_project(), "last layer = project");

    // And the security effect, end to end: the PROJECT's lua: is discarded
    // (counted); the USER's survives and wins precedence.
    let (_, preset) = &presets()[0];
    // The TUI's real set, with `LUA_HOST` (ADR 0110): `bindings()` only
    // counts what is available HERE.
    let known = norte_tui::shortcuts_editor::known_commands(Screen::Browse);
    let eff = Effective::build_for(preset, &cfg.keymap_layers, &known, Screen::Browse)
        .expect("discarding is not an error");
    assert_eq!(eff.discarded_lua_bindings(), 1, "only the project's");
    assert!(
        eff.bindings().iter().any(|(_, run)| *run == "lua:pwn"),
        "the USER layer's binding is still alive"
    );
}

/// #95.2: `[archive]` merges last-wins across TRUSTED layers and the
/// project layer is IGNORED — a foreign repo's `./.norte/norte.toml`
/// cannot raise the anti-bomb limits exactly where hostile archives live
/// (same fail-closed criterion as the hotlist).
#[test]
fn archive_limits_last_one_wins_and_project_does_not_touch_them() {
    let system = dir_with(&[(
        "norte.toml",
        "[archive]\nmax_entries = 1000\nmax_decompressed_bytes = 4096\n",
    )]);
    let user = dir_with(&[("norte.toml", "[archive]\nmax_entries = 50\n")]);
    let project = dir_with(&[(
        "norte.toml",
        "[archive]\nmax_entries = 999999999\nmax_decompressed_bytes = 999999999\n",
    )]);
    let layers = Layers {
        dirs: vec![
            (system.path().to_path_buf(), Layer::System),
            (user.path().to_path_buf(), Layer::User),
            (project.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("load");
    assert_eq!(
        cfg.common.archive.max_entries,
        Some(50),
        "user overrides system"
    );
    assert_eq!(
        cfg.common.archive.max_decompressed_bytes,
        Some(4096),
        "a field that was not overridden keeps the lower layer's value"
    );
}

/// Roadmap item 9, and the TWO halves are the assertion: what this binary
/// diagnoses reaches the FILE, and NOT stderr.
///
/// This test's first version launched `--version`, which exits thirty lines
/// BEFORE the subscriber is installed — so the process under test installed
/// none and the assertion passed on its own, including if someone swapped
/// `init_to_file` for the CLI's `init`, which is exactly the regression it
/// claimed to pin. Now it is given a broken `[ai]`, which is a real warning
/// through a path that does load config, install the subscriber, and exit
/// without opening the TTY.
#[test]
fn the_terminal_frontend_logs_to_the_file_and_not_to_the_screen() {
    let state = tempfile::tempdir().expect("tmp");
    let config = tempfile::tempdir().expect("tmp");
    // VALID config with an AI provider that does not resolve: it parses
    // (the provider's kind is not validated on read, on purpose — the AI
    // gate rejects it on use, where the diagnostic can name it), so the
    // subscriber does get installed and the warning goes out through
    // `tracing::warn!`. One that failed to parse would abort via stderr
    // BEFORE that, which is correct and is not this.
    std::fs::write(
        config.path().join("norte.toml"),
        "[ai]\nenabled = true\nrename_provider = \"x\"\n\n\
         [ai.providers.x]\nkind = \"inventado\"\nmodel = \"m\"\n",
    )
    .expect("config");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ntc"))
        .arg("--pick")
        .arg(state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("NORTE_CONFIG_DIR", config.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("run");

    // The process dies from not finding a TTY, and that error DOES go to
    // stderr on purpose: it is what tells the user why it did not start.
    // What must not show up there is the DIAGNOSTIC, which is what would
    // break the screen if there were a screen.
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !stderr.contains("AI provider not available"),
        "the warning must not reach the screen: {stderr}"
    );

    // And the other half: the warning IS there, in the file.
    let logs = state.path().join("norte").join("logs");
    let text: String = std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("no logs directory at {logs:?}: {e}"))
        .flatten()
        .map(|f| std::fs::read_to_string(f.path()).unwrap_or_default())
        .collect();
    assert!(
        text.contains("AI provider not available"),
        "the warning has to be in the log: {text}"
    );
}
