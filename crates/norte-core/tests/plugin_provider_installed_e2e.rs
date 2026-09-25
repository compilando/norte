//! An INSTALLED provider plugin serves the scheme it declares.
//!
//! Until now `[[contributions.provider]]` was declared, approved and enabled,
//! and nothing resolved it: the `ConnectionManager` matched schemes by hand
//! against the core's providers and an embedded FTP guest. This test walks
//! the whole distribution path with the `provider-mem` guest really compiled:
//! install → consent → connect by scheme → list. And the negative: without
//! consent, the scheme does not exist.

use std::path::{Path, PathBuf};
use std::process::Command;

use futures::StreamExt;
use norte_core::connect::{ConnectionManager, RemoteConnector};
use norte_core::plugins::{PluginRegistry, install};
use norte_proto::{Error, Scheme, VPath};

const MANIFEST: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"

[[contributions.provider]]
scheme = "memplug"
"#;

/// An installable source with the REAL `.wasm` from `provider-mem`.
fn source(dir: &Path, wasm: &Path) -> PathBuf {
    let src = dir.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), MANIFEST).expect("manifest");
    std::fs::copy(wasm, src.join("plugin.wasm")).expect("wasm");
    src
}

#[tokio::test]
async fn an_installed_and_consented_provider_serves_its_scheme() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = source(cfg.path(), &wasm);
    install(cfg.path(), &src, false).expect("installs");
    let manager = ConnectionManager::new(cfg.path());

    // Installed but not consented: the scheme does not exist for the manager.
    // Fail-closed, and with the same answer as a scheme nobody serves.
    let err = manager
        .connect("memplug", "host")
        .await
        .err()
        .expect("without consent it does not connect");
    assert_eq!(err.error, Error::Unsupported);

    {
        let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
        assert!(
            reg.set_approval("org.norte.memplug", true)
                .expect("approves")
        );
        assert!(reg.set_enabled("org.norte.memplug", true).expect("enables"));
    }

    let connected = manager
        .connect("memplug", "host")
        .await
        .map_err(|d| d.error)
        .expect("consented: it connects");
    assert!(
        connected.warnings.is_empty(),
        "an in-memory provider warns about nothing"
    );

    // And it is the real guest answering: `provider-mem` seeds a known tree
    // at its root.
    let root = VPath::root(Scheme::new("memplug").expect("scheme"), None);
    let names: Vec<Vec<u8>> = connected
        .provider
        .list(&root)
        .await
        .expect("lists the root")
        .map(|e| {
            e.expect("entry")
                .path
                .file_name()
                .expect("has a name")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    assert!(!names.is_empty(), "the guest seeds its root");
}

/// The same guest, declaring `net`: the network it receives is `ip:port` of
/// the connection's endpoint, resolved by the host with the anti-SSRF filter.
const MANIFEST_WITH_NET: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"

[[contributions.provider]]
scheme = "memplug"

[capabilities]
net = { hosts = [] }
"#;

fn install_consented(cfg: &Path, wasm: &Path, manifest: &str) {
    let src = cfg.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), manifest).expect("manifest");
    std::fs::copy(wasm, src.join("plugin.wasm")).expect("wasm");
    install(cfg, &src, false).expect("installs");
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(
        reg.set_approval("org.norte.memplug", true)
            .expect("approves")
    );
    assert!(reg.set_enabled("org.norte.memplug", true).expect("enables"));
}

/// ADR 0093 §3: the host resolves and filters. An endpoint in the metadata
/// range is not granted even if the plugin has `net`; loopback typed
/// literally IS; and without a port there is nothing to grant.
#[tokio::test]
async fn a_provider_plugins_network_is_the_filtered_endpoint_with_a_port() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_consented(cfg.path(), &wasm, MANIFEST_WITH_NET);
    let manager = ConnectionManager::new(cfg.path());

    // Cloud metadata: rejected by the filter, before instantiating anything.
    let err = manager
        .connect("memplug", "169.254.169.254:80")
        .await
        .err()
        .expect("link-local is not granted");
    assert_eq!(err.error, Error::ProviderUnavailable { retryable: true });

    // Without a port or `default-port`: there is nothing to grant against.
    let err = manager
        .connect("memplug", "127.0.0.1")
        .await
        .err()
        .expect("without a port there is no grant");
    assert_eq!(err.error, Error::Unsupported);

    // Literal loopback with a port: `127.0.0.1:1` is granted and the guest
    // (which opens no connection) ends up configured.
    manager
        .connect("memplug", "127.0.0.1:1")
        .await
        .map_err(|d| d.error)
        .expect("literal loopback with a port");
}

/// What runs is what was approved: changing the `.wasm` on disk after
/// approving stops serving the scheme, even if the manifest has not changed.
#[tokio::test]
async fn a_binary_changed_after_approving_is_not_instantiated() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_consented(cfg.path(), &wasm, MANIFEST);
    let manager = ConnectionManager::new(cfg.path());
    manager
        .connect("memplug", "host")
        .await
        .map_err(|d| d.error)
        .expect("intact: it connects");

    let installed = cfg.path().join("plugins/org.norte.memplug/plugin.wasm");
    let mut bytes = std::fs::read(&installed).expect("read");
    bytes.push(0);
    std::fs::write(&installed, bytes).expect("rewrite");
    // The catalogue re-anchors the digest on discovery, so approval stops
    // being valid (#241) and the scheme stops existing.
    let err = manager
        .connect("memplug", "host")
        .await
        .err()
        .expect("different binary: does not serve");
    assert_eq!(err.error, Error::Unsupported);
}

/// The core's scheme stays the core's: a plugin cannot claim it, and even if
/// someone planted the directory by hand the manager does not ask the
/// catalogue about `sftp`. (The real gate is tested in the registry:
/// `resolve_provider_nunca_sirve_un_scheme_del_core`.)
#[tokio::test]
async fn a_core_scheme_is_not_looked_up_in_the_catalogue() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let manager = ConnectionManager::new(cfg.path());
    // Without connections.toml or a server, `sftp://` fails at the transport,
    // not with `Unsupported`: that is the proof it went through the core's arm.
    let err = manager
        .connect("sftp", "127.0.0.1:1")
        .await
        .err()
        .expect("there is no server");
    assert_ne!(err.error, Error::Unsupported);
}

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
        .expect("cargo build of the guest");
    assert!(status.success(), "the {name} guest did not build");
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
