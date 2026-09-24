use super::*;

// ---------- plugin.* (M4-P3) ----------

/// Minimal valid manifest (the same one from `norte_core::plugins`'s test).
pub(super) const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

/// A daemon with `plugins_dir` pointing at a tempdir SEEDED with a
/// discoverable plugin (`plugins/org.norte.demo/plugin.toml`). NEVER touches
/// the real `~/.config`: the explicit `plugins_dir` isolates the test's
/// state.
pub(super) async fn spawn_daemon_plugins() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    // Plugin root INSIDE the same tempdir (cleaned up with `_dir`).
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.list` over the socket sees the seeded plugin, born unapproved/disabled.
#[tokio::test]
async fn plugin_list_sees_the_seeded_catalogue() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert_eq!(list.plugins.len(), 1, "the seeded plugin is discovered");
    let p = &list.plugins[0];
    assert_eq!(p.id, "org.norte.demo");
    assert!(!p.approved, "born unapproved");
    assert!(!p.enabled, "born disabled");
    assert!(list.errors.is_empty());
}

/// A HUMAN approves over the socket; `plugin.list` reflects it (and it
/// persisted, so a NEW connection also sees it approved).
#[tokio::test]
async fn plugin_set_approval_by_a_human_is_reflected_and_persists() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect("approval accepted");

    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list after approving");
    assert!(list.plugins[0].approved, "the approval is reflected");

    // A NEW connection reads the persisted state (same daemon, same dir).
    let other = connected_client(&d).await;
    let list2: methods::PluginListResult = other
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list on another connection");
    assert!(list2.plugins[0].approved, "the approval persisted");
}

/// And with the GOOD anchor — the one `plugin.list` itself just gave — it
/// does grant: the field closes a window, not the door.
#[tokio::test]
async fn plugin_set_approval_with_the_just_read_anchor_grants() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    let anchor = list.plugins[0]
        .manifest_digest
        .clone()
        .expect("the catalogue carries the anchor a human reads");

    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: Some(anchor),
            },
        )
        .await
        .expect("the anchor that was read grants");

    let after: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list after approving");
    assert!(after.plugins[0].approved);
}

/// `plugin.run_command` of an UNAPPROVED plugin is `INVALID_REQUEST` and does
/// NOT run it (fail-closed): the human has not consented, so the runtime
/// does not start. The seeded demo is born unapproved/disabled (M4-P4). The
/// success case with a real `.wasm` is the next task's E2E.
#[tokio::test]
async fn plugin_run_command_when_unapproved_is_invalid_request() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.demo".into(),
                command: "echo".into(),
                arg: "hola".into(),
            },
        )
        .await
        .expect_err("an unapproved plugin never runs");
    assert!(
        matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_REQUEST),
        "unapproved = INVALID_REQUEST, does not run: {err:?}"
    );
}

/// `plugin.run_command` of an UNKNOWN id is `INVALID_PARAMS` (the client
/// asked for a plugin that does not exist): nothing runs and nothing leaks.
#[tokio::test]
async fn plugin_run_command_unknown_id_is_invalid_params() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.fantasma".into(),
                command: "echo".into(),
                arg: String::new(),
            },
        )
        .await
        .expect_err("unknown id");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

// ---------- plugin.help (H3e) ----------

/// A daemon seeded with the demo plugin, giving the test a chance to write
/// its own `help.md` (H3e). `seed` receives `(tempdir_root, plugin_dir)` —
/// the root so it can leave files OUTSIDE the plugin's directory, which is
/// exactly what the escaped-symlink case needs.
pub(super) async fn spawn_daemon_help_plugin(
    seed: impl FnOnce(&std::path::Path, &std::path::Path),
) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");
    seed(dir.path(), &plugin_dir);

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.help` over the socket returns the plugin's `help.md`, already
/// capped by the host: the whole body, no trimming and no loss.
#[tokio::test]
async fn plugin_help_returns_the_plugins_capped_page() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(
            plugin_dir.join("help.md"),
            "# Demo\n\nThe demo plugin's page.\n",
        )
        .expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let help: methods::PluginHelpResult = c
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("plugin.help answers");
    assert!(help.markdown.contains("demo"), "the body arrives: {help:?}");
    assert!(!help.truncated && !help.lossy, "nothing to trim: {help:?}");

    // And `plugin.list` advertises it, so the frontend does not ask in vain.
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(list.plugins[0].has_help, "has_help advertises it");
}

/// An id that is NOT in the catalogue is `INVALID_PARAMS` — same treatment
/// `plugin.set_approval` gives a phantom plugin. The id is never composed
/// into a path, so a `../` only fails the lookup.
#[tokio::test]
async fn plugin_help_of_an_unknown_id_is_invalid_params() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(plugin_dir.join("help.md"), "# Demo\n").expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "no.existe".into(),
            },
        )
        .await
        .expect_err("a phantom plugin has no page");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let err2 = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "../../etc/passwd".into(),
            },
        )
        .await
        .expect_err("an id with traversal is just an unknown id");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// The hole the host's guard closes, checked at the point where an AGENT
/// arrives: `plugin.help` cannot turn into an arbitrary file read that goes
/// around the policy engine. A `help.md` that is a symlink to something
/// OUTSIDE the plugin's directory is served as a blank page.
#[cfg(unix)]
#[tokio::test]
async fn plugin_help_does_not_serve_a_help_md_that_escapes_the_directory() {
    let d = spawn_daemon_help_plugin(|root, plugin_dir| {
        let secret = root.join("secreto.md");
        std::fs::write(&secret, "PRIVATE-KEY-THAT-MUST-NOT-CROSS-THE-WIRE").expect("write secret");
        std::os::unix::fs::symlink(&secret, plugin_dir.join("help.md")).expect("symlink");
    })
    .await;
    let agent = connected_agent(&d, "claude-01").await;
    let help: methods::PluginHelpResult = agent
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("a known plugin always answers");
    assert_eq!(
        help.markdown, "",
        "a symlink leaving the directory is not served"
    );
}

/// A manifest with `[config]` (G3c): three keys of different types, to
/// exercise `plugin.get_config`/`plugin.set_config` end to end over the
/// socket.
pub(super) const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"

[config.retries]
type = "int"
default = 3
min = 0
max = 10

[config.mode]
type = "enum"
default = "fast"
values = ["fast", "thorough"]
"#;

/// A daemon seeded with [`CONFIG_MANIFEST`] (G3c) — mirror of
/// `spawn_daemon_plugins`, a different manifest.
pub(super) async fn spawn_daemon_config_plugin() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.get_config` over the socket: schema + effective value of the
/// THREE keys, OPEN to any connection (reading consents to nothing) — even
/// with the plugin unapproved/disabled (same criterion as `plugin.list`).
#[tokio::test]
async fn plugin_get_config_sees_the_schema_and_the_defaults() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("plugin.get_config");
    assert_eq!(res.keys.len(), 3);
    let greeting = res.keys.iter().find(|k| k.key == "greeting").unwrap();
    assert_eq!(greeting.kind, "string");
    assert_eq!(greeting.value, "hola");
    let retries = res.keys.iter().find(|k| k.key == "retries").unwrap();
    assert_eq!(retries.kind, "int");
    assert_eq!(retries.min, Some(0));
    assert_eq!(retries.max, Some(10));
    let mode = res.keys.iter().find(|k| k.key == "mode").unwrap();
    assert_eq!(mode.kind, "enum");
    assert_eq!(
        mode.values,
        vec!["fast".to_string(), "thorough".to_string()]
    );
}

/// `plugin.get_config` of an UNKNOWN id answers `keys: []` — never an error
/// (same lenient criterion as `plugin.list` with an empty catalogue).
#[tokio::test]
async fn plugin_get_config_unknown_id_is_empty_keys() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.fantasma".into(),
            },
        )
        .await
        .expect("plugin.get_config is not an error with an unknown id");
    assert!(res.keys.is_empty());
}

/// A HUMAN sets a valid value; `plugin.get_config` reflects it AND it
/// persisted (a NEW connection also sees it).
#[tokio::test]
async fn plugin_set_config_by_a_human_is_reflected_and_persists() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetConfigResult = human
        .call(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "greeting".into(),
                value: "hola mundo".into(),
            },
        )
        .await
        .expect("set_config with a valid value");

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config after set_config");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "greeting").unwrap().value,
        "hola mundo"
    );

    let other = connected_client(&d).await;
    let res2: methods::PluginGetConfigResult = other
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config on another connection");
    assert_eq!(
        res2.keys
            .iter()
            .find(|k| k.key == "greeting")
            .unwrap()
            .value,
        "hola mundo",
        "the value persisted"
    );
}

/// An INVALID value (outside `[min,max]`) is `INVALID_PARAMS` and does NOT
/// persist — `plugin.get_config` still sees the default.
#[tokio::test]
async fn plugin_set_config_invalid_value_does_not_persist() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "retries".into(),
                value: "999".into(),
            },
        )
        .await
        .expect_err("999 is outside [0,10]");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config after the rejection");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "retries").unwrap().value,
        "3",
        "the rejection must not have touched the default"
    );
}

/// An UNKNOWN key is `INVALID_PARAMS` (`config.toml` is not dirtied with keys
/// the schema does not declare).
#[tokio::test]
async fn plugin_set_config_unknown_key_is_invalid_params() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "no-such-key".into(),
                value: "x".into(),
            },
        )
        .await
        .expect_err("unknown key");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// `plugin.preview` of a file when there is NO previewer installed at all
/// (empty registry, `plugins_dir: None`) returns `preview: None` — NOT an
/// error: no consented previewer matches the mimetype, so the frontend falls
/// back to the raw view. The file's bytes are not even read (resolution
/// fails before that). The case with a real `.wasm` previewer is the next
/// task's E2E.
#[tokio::test]
async fn plugin_preview_with_no_previewer_is_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewResult>(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams {
                path: vp("mem:///nota.txt"),
            },
        )
        .await
        .expect("plugin.preview is not an error when there is no previewer");
    assert!(
        res.preview.is_none(),
        "with no previewer installed the preview is None (raw view), not an error: {res:?}"
    );
}

/// G3a (ADR 0037): `plugin.preview_styled` with no previewer installed
/// returns `preview: None` — SAME criterion as its plain twin, not an error.
/// The embedded `Backend::plugin_preview_styled` client has its own test for
/// the `Ok(None)` case; this covers the DAEMON handler against a real
/// socket.
#[tokio::test]
async fn plugin_preview_styled_with_no_previewer_is_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewStyledResult>(
            methods::PLUGIN_PREVIEW_STYLED,
            &methods::PluginPreviewStyledParams {
                path: vp("mem:///nota.txt"),
                columns: None,
            },
        )
        .await
        .expect("plugin.preview_styled is not an error when there is no previewer");
    assert!(
        res.preview.is_none(),
        "with no previewer installed the styled preview is None: {res:?}"
    );
}
