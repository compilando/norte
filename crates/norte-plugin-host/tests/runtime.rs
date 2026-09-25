//! Tests of the wasmtime runtime that do NOT require a real WASM
//! component: that the engine builds and that a garbage artifact fails
//! with a clear error.

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

mod support;

#[test]
fn runtime_builds() {
    let _rt = PluginRuntime::new().expect("engine");
}

#[test]
fn loading_a_non_component_fails_clearly() {
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("no.wasm");
    std::fs::write(&fake, b"this is not a wasm component").unwrap();
    let fake = norte_plugin_host::WasmArtifact::trusting_current(fake).expect("read");
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("garbage bytes");
    assert!(matches!(err, RuntimeError::Component(_)), "was {err:?}");
}

#[test]
fn an_artifact_too_large_is_rejected_before_compiling() {
    // A `.wasm` that exceeds the cap (issue #68) is rejected without ever
    // reaching `Component::from_file`. A SPARSE file is created (`set_len`)
    // so as not to actually write tens of MiB: `metadata().len()` returns
    // the logical size, which is what the cap looks at.
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("giant.wasm");
    let f = std::fs::File::create(&fake).unwrap();
    // 64 MiB + 1: just above MAX_ARTIFACT_BYTES.
    f.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(f);
    // The fingerprint does not matter: the cap is checked before reading
    // anything.
    let fake = norte_plugin_host::WasmArtifact::approved(fake, "0".repeat(64));
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("oversized artifact");
    assert!(
        matches!(err, RuntimeError::ArtifactTooLarge { .. }),
        "was {err:?}"
    );
}

#[test]
fn previewer_demo_renders_and_logs() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    let out = inst
        .render_preview("text/plain", b"line one\nline two\nline three\nline four")
        .expect("render");
    assert!(out.contains("text/plain"), "header: {out}");
    assert!(out.contains("line one") && out.contains("line three"));
    assert!(!out.contains("line four"), "only 3 lines");
    assert!(
        inst.logs().iter().any(|l| l.contains("previewer-demo")),
        "host-log: {:?}",
        inst.logs()
    );
}

/// ADR 0141: the same plugin is compiled ONCE per runtime, and every
/// instance is still new — what is reused is the code, not the state.
#[test]
fn the_same_plugin_is_compiled_once_and_every_instance_is_new() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut first = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    let _ = first.render_preview("text/plain", b"one").expect("render");
    let mut second = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    assert_eq!(rt.compiled_components(), 1, "compiled only once");
    assert!(
        second.logs().is_empty(),
        "the second does not inherit the first's logs: {:?}",
        second.logs()
    );
    let out = second.render_preview("text/plain", b"two").expect("render");
    assert!(out.contains("two"));
    // Different bytes, different entry: the key is the CONTENT.
    let copy = tempfile::tempdir().expect("tmp");
    let other = copy.path().join("other.wasm");
    let mut bytes = std::fs::read(&wasm).expect("wasm");
    // A custom section at the end is still a valid component and changes
    // the digest: id 0, one-byte name, no content.
    bytes.extend_from_slice(&[0, 2, 1, b'x']);
    std::fs::write(&other, &bytes).expect("write");
    let approved = norte_plugin_host::WasmArtifact::trusting_current(&other).expect("read");
    let _ = rt
        .instantiate(&approved, norte_plugin_host::Capabilities::default())
        .expect("instance");
    assert_eq!(rt.compiled_components(), 2);
    // ADR 0142: the file rewritten AFTER being approved does not load with
    // the earlier approval, even though the path is the same and its old
    // version is compiled in the cache.
    bytes.extend_from_slice(&[0, 2, 1, b'y']);
    std::fs::write(&other, &bytes).expect("rewrite");
    let Err(err) = rt.instantiate(&approved, norte_plugin_host::Capabilities::default()) else {
        panic!("a binary changed after approval must not instantiate")
    };
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::DigestMismatch),
        "was {err:?}"
    );
    // And approved again, the new version REPLACES the old one in the
    // cache instead of adding to it: nobody is going to ask for the old
    // one anymore.
    let new = norte_plugin_host::WasmArtifact::trusting_current(&other).expect("read");
    let _ = rt
        .instantiate(&new, norte_plugin_host::Capabilities::default())
        .expect("instance");
    assert_eq!(rt.compiled_components(), 2, "one version per path");
}

#[test]
fn command_demo_runs_and_reports_a_command_error() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    assert_eq!(inst.run_command("echo", "hola").expect("echo"), "hola");
    assert_eq!(inst.run_command("shout", "hola").expect("shout"), "HOLA");
    let err = inst.run_command("nope", "").expect_err("unknown");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("unknown")),
        "was {err:?}"
    );
}

#[test]
fn a_looping_guest_traps_on_deadline_without_hanging_the_host() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    // Short deadline ONLY for the test (~1 s: 20 ticks × 50 ms) so as not
    // to wait the ~10 s of the production default. The ticker cuts off
    // the loop → trap.
    let rt = norte_plugin_host::PluginRuntime::with_epoch_deadline(20).expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    let err = inst
        .run_command("spin", "")
        .expect_err("a looping guest must be cut off, not hang");
    // `Deadline` and NOT `Trap` (#211): the budget expired, which is not
    // the same as the guest crashing — and counting them the same turned
    // "your machine was under load" into "your plugin is broken", the one
    // reading that is guaranteed to be false.
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Deadline),
        "was {err:?}"
    );
}

/// The epoch budget is PER CALL, not per instance life (#211).
///
/// `Store::set_epoch_deadline` sets an ABSOLUTE instant, so arming it once
/// when the store is created gave the plugin a budget that got spent by
/// the CLOCK even if nothing ran: an FTP connection would stop working
/// ten seconds after being opened. Here it is checked with a short budget
/// and a wait LONGER than it between two quick calls: if the deadline
/// were per-life, the second one traps.
#[test]
fn the_epoch_budget_is_rearmed_on_every_call() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    // ~250 ms of budget (5 ticks × 50 ms) against a 600 ms wait.
    let rt = norte_plugin_host::PluginRuntime::with_epoch_deadline(5).expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    assert_eq!(inst.run_command("echo", "one").expect("first"), "one");
    // The wait is on the HOST, not the guest: the guest runs nothing here,
    // which is exactly what a CPU budget should not count.
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        inst.run_command("echo", "two")
            .expect("second, after the wait"),
        "two",
        "the budget rearms per call"
    );
    // And it still cuts off what it has to cut off: a loop within ONE
    // call.
    let err = inst
        .run_command("spin", "")
        .expect_err("a loop still gets cut off");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Deadline),
        "was {err:?}"
    );
}

#[test]
fn fs_read_scoped_gates_the_gate_on_the_host() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    // WITHOUT fs-read: the gate closes on the host even if the guest calls
    // it.
    let mut without = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    without.preload_scoped("demo", b"secret".to_vec());
    let err = without
        .run_command("read", "")
        .expect_err("without the capability");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("fs-read")),
        "was {err:?}"
    );
    // WITH fs-read=scoped: the gate opens and returns the seeded resource.
    let mut with = rt
        .instantiate(
            &wasm,
            norte_plugin_host::Capabilities::scoped_read_for_test(),
        )
        .expect("instance");
    with.preload_scoped("demo", b"content".to_vec());
    assert_eq!(with.run_command("read", "").expect("read"), "content");
}

#[test]
fn host_config_hands_settings_to_the_real_guest() {
    // P2 Task 3: `set_settings` + the real guest's `config` command
    // (`host_config::get` under the hood) — end-to-end without going
    // through the catalog/registry (that is covered by
    // `plugins_config_e2e.rs` in norte-core).
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");

    // Without `set_settings`: the default map is empty, `get` finds
    // nothing (same behavior as a plugin without `[config]`).
    let mut without = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    let err = without
        .run_command("config", "greeting")
        .expect_err("without set_settings there is nothing to read");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("greeting")),
        "was {err:?}"
    );

    // With `set_settings`: the guest reads the installed value as-is.
    let mut with = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instance");
    with.set_settings(std::collections::BTreeMap::from([(
        "greeting".to_string(),
        "hello world".to_string(),
    )]));
    assert_eq!(
        with.run_command("config", "greeting").expect("config"),
        "hello world"
    );
    // A key NOT installed still is not found, even though the map is not
    // empty (it's not "all or nothing": it's per-key).
    let err = with
        .run_command("config", "not-declared")
        .expect_err("key absent from the installed map");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("not-declared")),
        "was {err:?}"
    );
}
