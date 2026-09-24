//! E2E of `[config]` delivery (P2 Task 3): a manifest with `[config]` +
//! `config.toml` (override or absent) → the **real** COMMAND guest (compiled
//! from `norte-plugin-host/examples-wasm/command-demo`) reads the resolved
//! value via `host-config::get` and echoes it in its output. Closes the whole
//! chain: `resolve_settings` (Task 2, catalogue) →
//! `PluginRegistry::run_command` (Task 3, this file) → `host-config` (WIT,
//! runtime) → the guest.
//!
//! Same SKIP criterion as `plugins_run_e2e.rs`: without the `wasm32-wasip2`
//! target there is no artifact to run, and the rest of the suite stays green.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// A `command` manifest with a SINGLE `string`-typed `[config]` — just as
/// minimal as `plugins_run_e2e.rs`'s `CMD_MANIFEST`, but with a config schema
/// to exercise the end-to-end delivery.
const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfgdemo"
name = "Config Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hello"
"#;

/// Without `config.toml`: the guest reads the DEFAULT declared in the schema.
#[test]
fn plugin_config_e2e_real_wasm_without_a_file_uses_the_default() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Deliberately WITHOUT writing config.toml.

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("the guest must read the setting via host-config");
    assert_eq!(
        out, "hello",
        "without config.toml, the guest sees the schema's default"
    );
}

/// With `config.toml`: the guest reads the validated OVERRIDE, not the default.
#[test]
fn plugin_config_e2e_real_wasm_with_an_override_reflects_it() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    std::fs::write(
        plugin_dir.join("config.toml"),
        "greeting = \"hello world\"\n",
    )
    .expect("write config.toml");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("the guest must read the override via host-config");
    assert_eq!(
        out, "hello world",
        "with config.toml, the guest sees the validated value, not the default"
    );
}

/// G3c (`plugin.set_config` wire): `PluginRegistry::set_config` persists ONE
/// new value and RE-RESOLVES `settings` in memory — the next `run_command`
/// (without going back to `discover`) must see the just-written value, not
/// the default nor the original `config.toml`. Closes the whole chain that
/// `plugin.get_config`/`plugin.set_config` expose over the wire: `set_config`
/// (validate against the schema → persist → re-`resolve_settings`) →
/// `run_command` → `host-config` → the REAL guest.
#[test]
fn plugin_config_e2e_real_wasm_set_config_updates_what_the_guest_sees() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Without config.toml: starts at the default ("hello").

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    // Before `set_config`: the guest still sees the default.
    let before = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest reads the default");
    assert_eq!(before, "hello");

    // `plugin.set_config` (via the registry's method, the same path as the
    // daemon's handler): validates against the schema, persists, re-resolves.
    reg.set_config("org.norte.cfgdemo", "greeting", "hello G3c")
        .expect("set_config with a valid string must apply");

    // After `set_config`, WITHOUT going back to `discover`: the guest sees the
    // new value — proof that `set_config` re-resolved `settings` IN MEMORY,
    // not only on disk.
    let after = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest reads the just-set value");
    assert_eq!(after, "hello G3c");

    // Real persistence: a FRESH `discover` (simulates a restart of the
    // daemon/embedded process) also sees the new value from `config.toml`.
    let reg2 = PluginRegistry::discover(cfg.path()).expect("discover after set_config");
    assert_eq!(
        reg2.config_keys("org.norte.cfgdemo")
            .expect("known plugin")
            .iter()
            .find(|(k, _, _)| k == "greeting")
            .map(|(_, _, v)| v.as_str()),
        Some("hello G3c"),
    );
}

/// G3c: a value that does NOT validate against the schema (here, an unknown
/// key) is rejected WITHOUT persisting — `config.toml` still does not exist
/// after the failed attempt, and the guest still sees the default.
#[test]
fn plugin_config_e2e_real_wasm_invalid_set_config_does_not_persist() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let err = reg
        .set_config("org.norte.cfgdemo", "no-such-key", "x")
        .expect_err("an unknown key must be rejected");
    assert!(matches!(
        err,
        norte_core::plugins::PluginConfigSetError::UnknownKey(_)
    ));
    assert!(
        !plugin_dir.join("config.toml").exists(),
        "a rejected set_config must not create config.toml"
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest still sees the default after the rejection");
    assert_eq!(out, "hello");
}

/// Compiles `norte-plugin-host`'s `examples-wasm/<name>/` guest to
/// `wasm32-wasip2` (release) and returns the `.wasm`'s path.
///
/// A replica of the helper in `norte-plugin-host/tests/support/mod.rs` (not
/// reachable from `norte-core`'s test tree) and of `plugins_run_e2e.rs` (same
/// crate, same SKIP criterion) — the same text on purpose, not a third
/// variant.
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
