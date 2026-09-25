//! End-to-end E2E of `plugin.decorate`/`plugin.column_values` (G3b, ADR 0037
//! decision 2) THROUGH `Backend::Remote` (a real UDS daemon), against the
//! REAL guests `examples-wasm/decorator-demo` and `examples-wasm/columns-demo`
//! (`norte-plugin-host`).
//!
//! Same harness as the `styled` section of `plugins_preview_e2e.rs`: unix
//! only (the UDS daemon is `#[cfg(unix)]`, ADR 0011); `Backend::Embedded` is
//! deliberately NOT exercised here (it ALWAYS resolves the plugins directory
//! via `norte_core::connect::config_dir()`, a process global — see that
//! suite's rustdoc for the full reasoning).
//!
//! If the `wasm32-wasip2` target is not installed, SKIP (there is no artifact
//! to run).
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

/// The seeded plugin's `decorator` manifest (same behavior as
/// `norte-plugin-host`'s WIT e2e: badge `"M"` if the name contains `"mod"`).
const DECORATOR_MANIFEST: &str = r#"
[plugin]
id = "org.norte.decor"
name = "Decor Demo"
publisher = "norte"
version = "0.1.0"
category = "decorator"

[[contributions.decorator]]
"#;

/// The seeded plugin's `columns` manifest: declares `name-len`.
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
    VPath::parse(wire).expect("valid test wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

/// Compiles `examples-wasm/<name>/` to `wasm32-wasip2` (release). A replica of
/// the helper in `plugins_preview_e2e.rs` (not reachable across test trees).
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
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
        .expect("could not launch cargo to compile the guest");
    assert!(
        status.success(),
        "the {name} guest did not build (wasm32-wasip2 target present)"
    );
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "{} not found", wasm.display());
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
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end e2e: setup+wire+assert, not split up"
)]
async fn plugin_decorate_and_column_values_e2e_real_wasm_through_the_backend() {
    let Some(decor_wasm) = build_guest("decorator-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };
    let Some(cols_wasm) = build_guest("columns-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
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
            state_dir: None,
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

    // NOT approved yet: fail-closed — neither decorations nor column.
    let none_yet = backend
        .plugin_decorate(&paths, &[])
        .await
        .expect("plugin.decorate is not an error without approval");
    assert!(
        none_yet.is_empty(),
        "unapproved, no consented decorator: []"
    );
    let none_col_yet = backend
        .plugin_column_values("org.norte.cols", "name-len", &paths)
        .await
        .expect("plugin.column_values is not an error without approval");
    assert_eq!(
        none_col_yet,
        vec![None, None, None],
        "unapproved, empty cells 1:1 with paths"
    );

    backend
        .plugins_set_approval("org.norte.decor", true, None)
        .await
        .expect("approve the decorator over the wire");
    backend
        .plugins_set_enabled("org.norte.decor", true)
        .await
        .expect("enable the decorator over the wire");
    backend
        .plugins_set_approval("org.norte.cols", true, None)
        .await
        .expect("approve columns over the wire");
    backend
        .plugins_set_enabled("org.norte.cols", true)
        .await
        .expect("enable columns over the wire");

    // --- plugin.decorate ---
    let plugins = backend
        .plugin_decorate(&paths, &[])
        .await
        .expect("plugin.decorate is not an error");
    assert_eq!(
        plugins.len(),
        1,
        "a single consented decorator: {plugins:?}"
    );
    let pd = &plugins[0];
    assert_eq!(pd.plugin_id, "org.norte.decor");
    assert_eq!(pd.decorations.len(), 3, "positional 1:1 with paths");
    assert_eq!(
        pd.decorations[0].badge.as_deref(),
        Some("M"),
        "\"module.rs\" contains \"mod\": badge"
    );
    assert_eq!(pd.decorations[0].role.as_deref(), Some("warning"));
    assert_eq!(
        pd.decorations[1].badge, None,
        "\"README.md\" does not contain \"mod\": no badge"
    );
    assert_eq!(
        pd.decorations[2].badge.as_deref(),
        Some("M"),
        "\"my_mod_2.rs\" contains \"mod\": badge"
    );

    // --- plugin.column_values ---
    let values = backend
        .plugin_column_values("org.norte.cols", "name-len", &paths)
        .await
        .expect("plugin.column_values is not an error");
    assert_eq!(
        values,
        vec![
            Some("module.rs".len().to_string()),
            Some("README.md".len().to_string()),
            Some("my_mod_2.rs".len().to_string()),
        ],
        "basename length per entry, positional 1:1"
    );

    // A column id no plugin declares: empty cells, not an error.
    let unknown_col = backend
        .plugin_column_values("org.norte.cols", "not-declared", &paths)
        .await
        .expect("plugin.column_values is not an error for an unknown id");
    assert_eq!(unknown_col, vec![None, None, None]);

    // An empty batch must not call the wire (see the rustdoc of
    // `Backend::plugin_decorate`/`plugin_column_values`): the result is empty
    // all the same.
    let empty = backend
        .plugin_decorate(&[], &[])
        .await
        .expect("an empty batch is not an error");
    assert!(empty.is_empty());
}
