//! E2E de `Backend::Remote::plugin_get_config`/`plugin_set_config` (G3c)
//! contra un daemon UDS real — cierra la brecha de cobertura que
//! `plugins_decorate_columns_e2e.rs` dejó para `plugin.decorate`/
//! `plugin.column_values`: las envolturas `RemoteBackend` de get/set
//! config no tenían NINGÚN test que las ejercitara directamente (solo
//! `daemon.rs`, que llama al `Client` JSON-RPC crudo, sin pasar por
//! `Backend`). Sin WASM: `plugin.get_config`/`plugin.set_config` son
//! operaciones de REGISTRO puras (nunca instancian el runtime), así que
//! este test no necesita compilar ningún guest ni un `SKIP` condicional.
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
default = "hola"
"#;

#[tokio::test]
async fn plugin_get_set_config_e2e_a_traves_del_backend_remote() {
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

    // ABIERTO: leer no consiente nada, ni siquiera aprobado/activado.
    let res = backend
        .plugin_get_config("org.norte.cfg")
        .await
        .expect("plugin_get_config");
    assert_eq!(res.keys.len(), 1);
    assert_eq!(res.keys[0].key, "greeting");
    assert_eq!(res.keys[0].value, "hola");

    // Id desconocido: `keys: []`, no un error.
    let empty = backend
        .plugin_get_config("org.norte.fantasma")
        .await
        .expect("plugin_get_config con id desconocido no es error");
    assert!(empty.keys.is_empty());

    backend
        .plugin_set_config("org.norte.cfg", "greeting", "hola G3c")
        .await
        .expect("plugin_set_config con un valor válido");

    let after = backend
        .plugin_get_config("org.norte.cfg")
        .await
        .expect("plugin_get_config tras set_config");
    assert_eq!(after.keys[0].value, "hola G3c");

    // Clave desconocida: error, taxonomía honesta (no exhaustiva aquí —
    // `daemon.rs` ya pinea el código JSON-RPC exacto).
    let err = backend
        .plugin_set_config("org.norte.cfg", "no-such-key", "x")
        .await
        .expect_err("clave desconocida debe rechazarse");
    assert!(!matches!(err, norte_proto::Error::Unknown));
}
