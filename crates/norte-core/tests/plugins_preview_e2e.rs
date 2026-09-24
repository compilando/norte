//! E2E of the plugin previewer (M4-P5, closing): the whole chain
//! discover → (deny without approving) → approve → enable → resolve → run,
//! against a **real** WASM component compiled from
//! `norte-plugin-host/examples-wasm/previewer-demo` (M4-P2) and run sandboxed
//! by the runtime.
//!
//! This is M4-P5's closing: `PluginRegistry::resolve_previewer` chooses,
//! fail-closed, the consented previewer for a mimetype, and the runtime
//! really RUNS it, returning the render (header + first 3 lines of content).
//!
//! If the `wasm32-wasip2` target is not installed the test SKIPs (there is no
//! artifact to run): it passes on toolchains without that target and the rest
//! of the suite stays green. With the target present it runs the real
//! `.wasm`.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// The seeded plugin's `previewer` manifest: declares `text/*` as its
/// mimetype glob and `fs-read=scoped` (the render does not touch the FS, but
/// this pins that the manifest's capabilities travel to the runtime). The id
/// carries dots (reverse-DNS).
const PREV_MANIFEST: &str = r#"
[plugin]
id = "org.norte.prev"
name = "Preview Demo"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"
"#;

/// Test content: four lines — previewer-demo only takes the first 3, so
/// "line four" must NOT appear in the render.
const SAMPLE: &[u8] = b"line one\nline two\nline three\nline four";

/// Same as [`PREV_MANIFEST`] but with `[config.banner]` (P2 Task 4a): to test
/// that `resolve_previewer` + `set_settings` deliver `[config]` to the
/// previewer, not just to `command` (Task 3).
const PREV_MANIFEST_WITH_CONFIG: &str = r#"
[plugin]
id = "org.norte.prev-cfg"
name = "Preview Demo Config"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"

[config.banner]
type = "string"
default = "Default Banner"
"#;

/// The M4-P5 closing chain with a REAL WASM component.
#[test]
fn plugin_preview_e2e_real_wasm() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    // config_dir/plugins/org.norte.prev/{plugin.toml, plugin.wasm}
    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // 1) UNAPPROVED: fail-closed. The .wasm IS present and the mimetype
    //    matches, but consent rules: no previewer at all is chosen.
    assert!(
        reg.resolve_previewer("text/plain").is_none(),
        "an unconsented previewer is never chosen, even with .wasm present"
    );

    // 2) Approve + enable (in-memory: embedded usage in the test).
    assert!(
        reg.set_approval_in_memory("org.norte.prev", true),
        "the plugin exists: the approval applies"
    );
    assert!(
        reg.set_enabled_in_memory("org.norte.prev", true),
        "the plugin exists: the enable applies"
    );

    // 3) Now it does resolve for the mimetype matching the `text/*` glob.
    let (id, name, resolved_wasm, caps, _settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain matches text/* with the consented previewer");
    assert_eq!(id, "org.norte.prev", "resolved previewer's id");
    assert_eq!(name, "Preview Demo", "resolved previewer's name");
    assert!(
        resolved_wasm.path().ends_with("plugin.wasm"),
        "the resolved binary is <dir>/plugin.wasm"
    );

    // 4) A mimetype the previewer does NOT declare → None (it only declares text/*).
    assert!(
        reg.resolve_previewer("application/json").is_none(),
        "application/json does not match text/*: no previewer for it"
    );

    // 5) RUNS the real WASM component: the core would read the bounded bytes
    //    and pass them to the guest (rule 9: the plugin does not touch the FS
    //    directly).
    let render = rt
        .instantiate(&resolved_wasm, caps)
        .expect("instantiate the previewer")
        .render_preview("text/plain", SAMPLE)
        .expect("previewer-demo must render the content");

    assert!(
        render.contains("[text/plain]"),
        "the render carries the header with the mimetype: {render:?}"
    );
    assert!(
        render.contains("line one"),
        "the render includes the 1st line: {render:?}"
    );
    assert!(
        render.contains("line three"),
        "the render includes the 3rd line: {render:?}"
    );
    assert!(
        !render.contains("line four"),
        "previewer-demo only takes 3 lines: the 4th does not appear: {render:?}"
    );

    // 6) ADR 0142: a `plugin.wasm` rewritten AFTER being approved does not run
    //    with that approval, even if the registry already resolved it and the
    //    approved version is compiled in the runtime's cache.
    let mut bytes = std::fs::read(&resolved_wasm).expect("read the binary");
    // A custom section at the end: still a valid component, so the only thing
    // that gives it away is the fingerprint.
    bytes.extend_from_slice(&[0, 2, 1, b'z']);
    std::fs::write(&resolved_wasm, &bytes).expect("rewrite the binary");
    let Err(err) = rt.instantiate(&resolved_wasm, norte_plugin_host::Capabilities::default())
    else {
        panic!("a binary changed after being approved must not be instantiated")
    };
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::DigestMismatch),
        "was {err:?}"
    );
}

/// P2 Task 4a: the previewer receives `[config]` ALREADY resolved via
/// `host-config`, just like `command` (Task 3) — this test is the analogue of
/// `plugins_config_e2e.rs` but for the `resolve_previewer` + `set_settings` +
/// `render_preview` path. Without `config.toml`, the guest sees the schema's
/// DEFAULT.
#[test]
fn plugin_preview_e2e_real_wasm_config_banner_default() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev-cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST_WITH_CONFIG)
        .expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Deliberately WITHOUT config.toml.

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.prev-cfg", true));
    assert!(reg.set_enabled_in_memory("org.norte.prev-cfg", true));

    let (_id, _name, resolved_wasm, caps, settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain matches text/*");
    let mut inst = rt.instantiate(&resolved_wasm, caps).expect("instantiate");
    inst.set_settings(settings);
    let render = inst
        .render_preview("text/plain", SAMPLE)
        .expect("render with settings");
    assert!(
        render.starts_with("Default Banner\n"),
        "without config.toml, the guest sees the schema's default: {render:?}"
    );
}

/// Like the previous one, but WITH `config.toml` — the guest must see the
/// validated OVERRIDE, not the default.
#[test]
fn plugin_preview_e2e_real_wasm_config_banner_override() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev-cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST_WITH_CONFIG)
        .expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    std::fs::write(
        plugin_dir.join("config.toml"),
        "banner = \"Hello from config\"\n",
    )
    .expect("write config.toml");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.prev-cfg", true));
    assert!(reg.set_enabled_in_memory("org.norte.prev-cfg", true));

    let (_id, _name, resolved_wasm, caps, settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain matches text/*");
    let mut inst = rt.instantiate(&resolved_wasm, caps).expect("instantiate");
    inst.set_settings(settings);
    let render = inst
        .render_preview("text/plain", SAMPLE)
        .expect("render with settings");
    assert!(
        render.starts_with("Hello from config\n"),
        "with config.toml, the guest sees the validated override: {render:?}"
    );
}

/// Compiles `norte-plugin-host`'s `examples-wasm/<name>/` guest to
/// `wasm32-wasip2` (release) and returns the `.wasm`'s path.
///
/// A replica of the helper in `plugins_run_e2e.rs` / `norte-plugin-host/tests/support`
/// (not reachable across test trees). Returns `None` (SKIP) if the
/// `wasm32-wasip2` target is not installed; if the target is present but the
/// guest does not build, it is a real failure and aborts.
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }

    // `CARGO_MANIFEST_DIR` = .../crates/norte-core; the guest lives in the
    // sibling crate norte-plugin-host.
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
    assert!(wasm.exists(), "artifact {} not found", wasm.display());
    Some(wasm)
}

/// `true` if `rustup` reports `target` among the installed ones.
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

// ---------------------------------------------------------------------
// G3a (ADR 0037): `plugin.preview_styled` end to end WITH real WASM,
// through `Backend` (not the bare `PluginRuntime` as above). Unix only:
// the UDS daemon is `#[cfg(unix)]` (ADR 0011), same as
// `tests/backend_remote.rs`, from which this section takes the harness
// (`RemoteBackend::connect` + `DaemonConfig::plugins_dir`).
//
// `Backend::Embedded` is NOT exercised here on purpose: its plugins arm
// ALWAYS resolves the directory via `norte_core::connect::config_dir()`
// (a process global, with no override parameter) — changing it from a
// test would require `std::env::set_var` (`unsafe` in edition 2024,
// project rule 5: FORBIDDEN outside `norte-vfs-local`). `Backend::Remote`
// exercises the SAME public surface (`Backend::plugin_preview_styled`)
// against the daemon's REAL handler
// (`daemon::server::handle_plugin_preview_styled`, wired in this same
// task) without that problem — the daemon's `plugins_dir` CAN be
// parameterized by a test (`DaemonConfig`), as
// `spawn_daemon_plugins_ok_y_roto` in `tests/daemon.rs` already proves.
#[cfg(unix)]
mod styled {
    use std::sync::Arc;

    use bytes::Bytes;
    use norte_core::Engine;
    use norte_core::backend::Backend;
    use norte_core::backend::remote::RemoteBackend;
    use norte_core::daemon::{Daemon, DaemonConfig};
    use norte_proto::VPath;
    use norte_proto::methods::ClientInfo;
    use norte_testkit::MemProvider;
    use norte_vfs::Provider;

    use super::{PREV_MANIFEST, SAMPLE, build_guest};

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

    /// The G3a closing chain with a REAL WASM component, end to end THROUGH
    /// `Backend::Remote` (a real UDS daemon): discover → approve → enable
    /// (over the WIRE, `plugin.set_approval`/`plugin.set_enabled` — not
    /// `_in_memory`, unlike the synchronous test above) →
    /// `Backend::plugin_preview_styled` → REAL roles/fg from
    /// `previewer-demo`'s mini-highlighter (see its rustdoc: digits →
    /// `role: "number"`, `TODO`/`FIXME`/`norte` → `role: "keyword"` + a fixed
    /// `fg`).
    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "end-to-end e2e: setup+wire+assert, not split up"
    )]
    async fn plugin_preview_styled_e2e_real_wasm_through_the_backend() {
        let Some(wasm) = build_guest("previewer-demo") else {
            eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
            return;
        };

        let cfg = tempfile::tempdir().expect("tempdir cfg");
        let plugin_dir = cfg.path().join("plugins").join("org.norte.prev");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
        std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST).expect("write manifest");
        std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

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

        write_file(&mem, "mem:///doc.txt", SAMPLE).await;

        let remote = RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "plugins-preview-styled-e2e".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect");
        let backend = Backend::Remote(remote.clone());

        // NOT approved yet: consent rules, the classic `plugin.preview`
        // already proves it (above, in-memory); here it is enough to confirm
        // the WIRE respects the same fail-closed before approving.
        let none_yet = backend
            .plugin_preview_styled(&vp("mem:///doc.txt"), None)
            .await
            .expect("plugin.preview_styled is not an error without approval");
        assert!(
            none_yet.is_none(),
            "unapproved, no consented previewer matches: None"
        );

        backend
            .plugins_set_approval("org.norte.prev", true, None)
            .await
            .expect("approve over the wire");
        backend
            .plugins_set_enabled("org.norte.prev", true)
            .await
            .expect("enable over the wire");

        let preview = backend
            .plugin_preview_styled(&vp("mem:///doc.txt"), None)
            .await
            .expect("plugin.preview_styled is not an error")
            .expect("approved+enabled: the previewer applies");
        assert_eq!(preview.plugin_id, "org.norte.prev");
        assert_eq!(preview.plugin_name, "Preview Demo");

        // SAMPLE = "line one\nline two\nline three\nline four": the header
        // (1 plain line) + 3 lines of highlighted content.
        assert_eq!(
            preview.lines.len(),
            4,
            "header + 3 lines: {:?}",
            preview.lines
        );
        let header_text: String = preview.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(
            header_text.contains("[text/plain]"),
            "header with the mimetype: {header_text:?}"
        );
        assert!(
            preview.lines[0]
                .iter()
                .all(|s| s.role.is_none() && s.fg.is_none()),
            "the header is a single plain span: {:?}",
            preview.lines[0]
        );

        // "line one" has no digits nor keywords: all plain.
        assert!(
            preview.lines[1].iter().all(|s| s.role.is_none()),
            "a line without digits or keywords: no roles: {:?}",
            preview.lines[1]
        );

        // SAMPLE's content carries no real digits/keywords in its first 3
        // lines ("line one/two/three"); the exact conversion (role
        // unvalidated on the wire) is tested with a dedicated file.
        let mem2 = &mem;
        write_file(mem2, "mem:///code.txt", b"TODO 42 norte plain\nsecond").await;
        let preview2 = backend
            .plugin_preview_styled(&vp("mem:///code.txt"), None)
            .await
            .expect("preview_styled ok")
            .expect("previewer still approved+enabled");
        // lines[1] = first content line: "TODO 42 norte plain".
        let spans = &preview2.lines[1];
        let by_text = |t: &str| spans.iter().find(|s| s.text == t);
        assert_eq!(
            by_text("TODO").and_then(|s| s.role.as_deref()),
            Some("keyword"),
            "TODO is the guest's keyword (UNVALIDATED against norte_theme::Role on the wire): {spans:?}"
        );
        assert_eq!(
            by_text("TODO").and_then(|s| s.fg),
            Some([255, 200, 0]),
            "a keyword also carries a fixed fg: {spans:?}"
        );
        assert_eq!(
            by_text("42").and_then(|s| s.role.as_deref()),
            Some("number"),
            "42 is a number: {spans:?}"
        );
        assert_eq!(
            by_text("norte").and_then(|s| s.role.as_deref()),
            Some("keyword"),
            "norte is a keyword: {spans:?}"
        );
        assert_eq!(
            by_text("plain").and_then(|s| s.role.as_deref()),
            None,
            "plain matches no highlighter rule: {spans:?}"
        );

        // #101 (daemon↔embedded parity): the host-side decoding happens in
        // the DAEMON'S HANDLER and its `lossy` signal travels over the WIRE.
        // A valid file is not lossy...
        assert!(!preview.lossy, "SAMPLE valid UTF-8: not lossy");
        assert!(!preview2.lossy, "ASCII code: not lossy");
        // ...and one detected as text (UTF-8 BOM) with an invalid byte IS:
        // proof that the daemon DECODES (it does not pass raw bytes to the
        // guest) and flags the loss.
        let mut bad = vec![0xEF, 0xBB, 0xBF];
        bad.extend_from_slice(b"line\xFFbad\n");
        write_file(&mem, "mem:///bad.txt", &bad).await;
        let preview_lossy = backend
            .plugin_preview_styled(&vp("mem:///bad.txt"), None)
            .await
            .expect("preview_styled ok")
            .expect("previewer still approved+enabled");
        assert!(
            preview_lossy.lossy,
            "the daemon decoded text and flagged the loss over the wire: {preview_lossy:?}"
        );
    }
}
