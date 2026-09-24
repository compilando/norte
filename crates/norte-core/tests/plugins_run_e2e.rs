//! E2E of plugin execution (M4-P4, closing): the whole chain
//! discover → (deny without approving) → approve → enable → run, against a
//! **real** WASM component compiled from
//! `norte-plugin-host/examples-wasm/command-demo` and run sandboxed by the
//! M4-P2 runtime.
//!
//! If the `wasm32-wasip2` target is not installed the test SKIPs (there is no
//! artifact to run): it passes on toolchains without that target and the rest
//! of the suite stays green. With the target present it runs the real
//! `.wasm` and checks the output (`echo`/`shout`) and the guest's error.

use std::path::PathBuf;
use std::process::Command;

use norte_core::{PluginRegistry, PluginRunError};
use norte_plugin_host::PluginRuntime;

/// The seeded plugin's minimal `command` manifest — WITHOUT special
/// capabilities (echo/shout do not touch the FS). The id carries dots
/// (reverse-DNS).
const CMD_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cmd"
name = "Command Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

/// The M4-P4 closing chain with a REAL WASM component.
#[test]
fn plugin_run_command_e2e_real_wasm() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    // config_dir/plugins/org.norte.cmd/{plugin.toml, plugin.wasm}
    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cmd");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CMD_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // 1) UNAPPROVED: fail-closed. The .wasm IS present, but consent rules:
    //    nothing runs.
    let err = reg
        .run_command(&rt, "org.norte.cmd", "echo", "hello")
        .expect_err("an unapproved plugin never runs, even with .wasm present");
    assert!(
        matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.cmd"),
        "unapproved = NotApproved: {err:?}"
    );

    // 2) Approve + enable (persists to plugins-state.toml, embedded usage).
    assert!(
        reg.set_approval("org.norte.cmd", true)
            .expect("set_approval"),
        "the plugin exists: the approval applies"
    );
    assert!(
        reg.set_enabled("org.norte.cmd", true).expect("set_enabled"),
        "the plugin exists: the enable applies"
    );

    // 3) Actually run the sandboxed WASM component.
    let out = reg
        .run_command(&rt, "org.norte.cmd", "echo", "hello")
        .expect("echo must run the real guest and return the arg");
    assert_eq!(out, "hello", "the `echo` guest returns the arg as is");

    let out = reg
        .run_command(&rt, "org.norte.cmd", "shout", "hello")
        .expect("shout must run the real guest");
    assert_eq!(
        out, "HELLO",
        "the `shout` guest returns the arg in uppercase"
    );

    // 4) A command the guest does not know → Err from the guest → Runtime(Guest).
    let err = reg
        .run_command(&rt, "org.norte.cmd", "unknown", "")
        .expect_err("an unknown command returns Err from the guest");
    assert!(
        matches!(err, PluginRunError::Runtime(_)),
        "the guest's logic Err maps to Runtime: {err:?}"
    );
}

/// Compiles `norte-plugin-host`'s `examples-wasm/<name>/` guest to
/// `wasm32-wasip2` (release) and returns the `.wasm`'s path.
///
/// A replica of the helper in `norte-plugin-host/tests/support/mod.rs` (not
/// reachable from `norte-core`'s test tree). Returns `None` (SKIP) if the
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
