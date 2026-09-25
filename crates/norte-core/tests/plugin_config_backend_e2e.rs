//! E2E of `Backend::Remote::plugin_get_config`/`plugin_set_config` (G3c)
//! against a real UDS daemon — closes the coverage gap
//! `plugins_decorate_columns_e2e.rs` left for `plugin.decorate`/
//! `plugin.column_values`: the `RemoteBackend` get/set config wrappers had
//! NO test exercising them directly (only `daemon.rs`, which calls the raw
//! JSON-RPC `Client`, without going through `Backend`). Without WASM:
//! `plugin.get_config`/`plugin.set_config` are pure REGISTRY operations
//! (they never instantiate the runtime), so this test needs to compile no
//! guest nor any conditional `SKIP`.
#![cfg(unix)]

use std::sync::Arc;

use norte_core::Engine;
use norte_core::backend::Backend;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::ClientInfo;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hello"
"#;

#[tokio::test]
async fn plugin_get_set_config_e2e_through_the_remote_backend() {
    let cfg = tempfile::tempdir().expect("tempdir cfg");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");

    let dir = tempfile::tempdir().expect("tempdir daemon");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: Some(cfg.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let _run = tokio::spawn(daemon.run());

    let remote = norte_core::backend::remote::RemoteBackend::connect(
        socket,
        None,
        ClientInfo {
            name: "plugin-config-backend-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect");
    let backend = Backend::Remote(remote);

    // OPEN: reading consents to nothing, not even approved/enabled.
    let res = backend
        .plugin_get_config("org.norte.cfg")
        .await
        .expect("plugin_get_config");
    assert_eq!(res.keys.len(), 1);
    assert_eq!(res.keys[0].key, "greeting");
    assert_eq!(res.keys[0].value, "hello");

    // Unknown id: `keys: []`, not an error.
    let empty = backend
        .plugin_get_config("org.norte.ghost")
        .await
        .expect("plugin_get_config with an unknown id is not an error");
    assert!(empty.keys.is_empty());

    backend
        .plugin_set_config("org.norte.cfg", "greeting", "hello G3c")
        .await
        .expect("plugin_set_config with a valid value");

    let after = backend
        .plugin_get_config("org.norte.cfg")
        .await
        .expect("plugin_get_config after set_config");
    assert_eq!(after.keys[0].value, "hello G3c");

    // Unknown key: error, honest taxonomy (not exhaustive here —
    // `daemon.rs` already pins the exact JSON-RPC code).
    let err = backend
        .plugin_set_config("org.norte.cfg", "no-such-key", "x")
        .await
        .expect_err("an unknown key must be rejected");
    assert!(!matches!(err, norte_proto::Error::Unknown));
}
