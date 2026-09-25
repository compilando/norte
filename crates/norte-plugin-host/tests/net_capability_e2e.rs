//! E2E of the `net` capability's gating (#30 stage 3a): compiles the
//! `net-probe` guest (connects over TCP and returns the echo) and verifies
//! that NETWORK access is granted ONLY with `net` declared and RESTRICTED
//! to the host allow-list:
//!
//! - WITH `net` (allow-list = the local listener) → connects and receives
//!   the echo.
//! - WITHOUT `net` → `connect` FAILS (the default `socket_addr_check`
//!   rejects).
//! - WITH `net` but the listener is NOT in the allow-list → connection
//!   REJECTED.
//!
//! All with a local `TcpListener` — no external server. SKIP without the
//! `wasm32-wasip2` target.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use norte_plugin_host::{Capabilities, PluginRuntime};

/// Starts a local echo server on `127.0.0.1:0`; returns its `addr` and
/// serves connections on a thread (reads a chunk, returns it) until the
/// listener closes when the test ends.
fn spawn_echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 32];
            if let Ok(n) = s.read(&mut buf) {
                let _ = s.write_all(&buf[..n]);
            }
        }
    });
    addr
}

fn net_caps(hosts: &[&str]) -> Capabilities {
    Capabilities::with_net(hosts.iter().map(|h| (*h).to_owned()).collect())
}

#[test]
fn net_capability_gating_e2e() {
    let Some(wasm) = build_guest("net-probe") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let addr = spawn_echo();
    let target = addr.to_string(); // "127.0.0.1:PORT"

    // 1) WITH `net` (allow-list = the addr itself): connects and receives
    //    the echo.
    let mut inst = rt
        .instantiate(&wasm, net_caps(&[&target]))
        .expect("instantiate with net");
    let out = inst
        .run_command("connect", &target)
        .expect("connect with net");
    assert_eq!(out, "ping", "with net granted, the probe receives the echo");

    // 2) WITHOUT `net`: the default socket_addr_check REJECTS → connect
    //    fails.
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instantiate without net");
    let err = inst
        .run_command("connect", &target)
        .expect_err("without net, connect must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("connect") || msg.contains("Guest"),
        "the failure comes from the rejected connect: {msg}"
    );

    // 3) WITH `net` but the listener is NOT in the allow-list: REJECTED.
    let mut inst = rt
        .instantiate(&wasm, net_caps(&["10.0.0.1", "10.0.0.1:1"]))
        .expect("instantiate with a foreign net allow-list");
    let err = inst
        .run_command("connect", &target)
        .expect_err("a host outside the allow-list must fail");
    assert!(
        format!("{err:?}").contains("connect") || format!("{err:?}").contains("Guest"),
        "connection to a host outside the allow-list rejected"
    );
}

fn build_guest(name: &str) -> Option<norte_plugin_host::WasmArtifact> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
    assert!(status.success(), "guest {name} did not compile");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "{} was not found", wasm.display());
    // With the fingerprint of what was just compiled (ADR 0142).
    Some(norte_plugin_host::WasmArtifact::trusting_current(wasm).expect("guest"))
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
