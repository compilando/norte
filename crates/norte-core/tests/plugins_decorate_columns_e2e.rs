//! E2E de `plugin.decorate`/`plugin.column_values` (G3b, ADR 0037 decisión
//! 2) de punta a punta A TRAVÉS DE `Backend::Remote` (daemon UDS real),
//! contra los guests REALES `examples-wasm/decorator-demo` y
//! `examples-wasm/columns-demo` (`norte-plugin-host`).
//!
//! Mismo arnés que la sección `styled` de `plugins_preview_e2e.rs`: solo
//! unix (el daemon UDS es `#[cfg(unix)]`, ADR 0011); `Backend::Embedded` NO
//! se ejercita aquí a propósito (resuelve el directorio de plugins SIEMPRE
//! vía `norte_core::connect::config_dir()`, global del proceso — ver el
//! rustdoc de esa suite para el razonamiento completo).
//!
//! Si el target `wasm32-wasip2` no está instalado, SKIP (no hay artefacto
//! que ejecutar).
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::VPath;
use norte_proto::methods::ClientInfo;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

/// Manifiesto `decorator` del plugin sembrado (mismo comportamiento que el
/// e2e WIT de `norte-plugin-host`: badge `"M"` si el nombre contiene
/// `"mod"`).
const DECORATOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor Demo"
publisher = "norte"
version = "0.1.0"
category = "decorator"

[[contributions.decorator]]
"#;

/// Manifiesto `columns` del plugin sembrado: declara `name-len`.
const COLUMNS_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cols"
name = "Cols Demo"
publisher = "norte"
version = "0.1.0"
category = "columns"

[[contributions.columns]]
id = "name-len"
header = "Name Len"
"#;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

/// Compila `examples-wasm/<name>/` a `wasm32-wasip2` (release). Réplica del
/// helper de `plugins_preview_e2e.rs` (no accesible entre árboles de tests).
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("no se pudo lanzar cargo para compilar el guest");
    assert!(
        status.success(),
        "el guest {name} no compiló (target wasm32-wasip2 presente)"
    );
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "no se encontró {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // e2e de punta a punta: setup+wire+assert, sin trocear
async fn plugin_decorate_y_column_values_e2e_wasm_real_a_traves_del_backend() {
    let Some(decor_wasm) = build_guest("decorator-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };
    let Some(cols_wasm) = build_guest("columns-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir cfg");
    let decor_dir = cfg.path().join("plugins").join("org.norte.decor");
    std::fs::create_dir_all(&decor_dir).expect("mkdir decor dir");
    std::fs::write(decor_dir.join("plugin.toml"), DECORATOR_MANIFEST).expect("write manifest");
    std::fs::copy(&decor_wasm, decor_dir.join("plugin.wasm")).expect("copy .wasm");

    let cols_dir = cfg.path().join("plugins").join("org.norte.cols");
    std::fs::create_dir_all(&cols_dir).expect("mkdir cols dir");
    std::fs::write(cols_dir.join("plugin.toml"), COLUMNS_MANIFEST).expect("write manifest");
    std::fs::copy(&cols_wasm, cols_dir.join("plugin.wasm")).expect("copy .wasm");

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

    write_file(&mem, "mem:///module.rs", b"x").await;
    write_file(&mem, "mem:///README.md", b"x").await;
    write_file(&mem, "mem:///my_mod_2.rs", b"x").await;

    let remote = norte_core::backend::remote::RemoteBackend::connect(
        socket,
        None,
        ClientInfo {
            name: "plugins-decorate-columns-e2e".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect");
    let backend = Backend::Remote(remote.clone());

    let paths = vec![
        vp("mem:///module.rs"),
        vp("mem:///README.md"),
        vp("mem:///my_mod_2.rs"),
    ];

    // SIN aprobar todavía: fail-closed — ni decoraciones ni columna.
    let none_yet = backend
        .plugin_decorate(&paths)
        .await
        .expect("plugin.decorate no es error sin aprobar");
    assert!(
        none_yet.is_empty(),
        "sin aprobar, ningún decorator consentido: []"
    );
    let none_col_yet = backend
        .plugin_column_values("org.norte.cols", "name-len", &paths)
        .await
        .expect("plugin.column_values no es error sin aprobar");
    assert_eq!(
        none_col_yet,
        vec![None, None, None],
        "sin aprobar, celdas vacías 1:1 con paths"
    );

    backend
        .plugins_set_approval("org.norte.decor", true)
        .await
        .expect("aprobar decorator por el wire");
    backend
        .plugins_set_enabled("org.norte.decor", true)
        .await
        .expect("activar decorator por el wire");
    backend
        .plugins_set_approval("org.norte.cols", true)
        .await
        .expect("aprobar columns por el wire");
    backend
        .plugins_set_enabled("org.norte.cols", true)
        .await
        .expect("activar columns por el wire");

    // --- plugin.decorate ---
    let plugins = backend
        .plugin_decorate(&paths)
        .await
        .expect("plugin.decorate no es error");
    assert_eq!(
        plugins.len(),
        1,
        "un único decorator consentido: {plugins:?}"
    );
    let pd = &plugins[0];
    assert_eq!(pd.plugin_id, "org.norte.decor");
    assert_eq!(pd.decorations.len(), 3, "positional 1:1 con paths");
    assert_eq!(
        pd.decorations[0].badge.as_deref(),
        Some("M"),
        "\"module.rs\" contiene \"mod\": badge"
    );
    assert_eq!(pd.decorations[0].role.as_deref(), Some("warning"));
    assert_eq!(
        pd.decorations[1].badge, None,
        "\"README.md\" no contiene \"mod\": sin badge"
    );
    assert_eq!(
        pd.decorations[2].badge.as_deref(),
        Some("M"),
        "\"my_mod_2.rs\" contiene \"mod\": badge"
    );

    // --- plugin.column_values ---
    let values = backend
        .plugin_column_values("org.norte.cols", "name-len", &paths)
        .await
        .expect("plugin.column_values no es error");
    assert_eq!(
        values,
        vec![
            Some("module.rs".len().to_string()),
            Some("README.md".len().to_string()),
            Some("my_mod_2.rs".len().to_string()),
        ],
        "largo del basename por entrada, posicional 1:1"
    );

    // Un id de columna no declarado por ningún plugin: celdas vacías, no error.
    let unknown_col = backend
        .plugin_column_values("org.norte.cols", "no-declarada", &paths)
        .await
        .expect("plugin.column_values no es error para un id desconocido");
    assert_eq!(unknown_col, vec![None, None, None]);

    // Un lote vacío no debe llamar al wire (ver rustdoc de `Backend::
    // plugin_decorate`/`plugin_column_values`): el resultado es vacío igual.
    let empty = backend
        .plugin_decorate(&[])
        .await
        .expect("lote vacío no es error");
    assert!(empty.is_empty());
}
