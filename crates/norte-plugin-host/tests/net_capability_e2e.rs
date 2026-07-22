//! E2E del gating de la capability `net` (#30 stage 3a): compila el guest
//! `net-probe` (conecta por TCP y devuelve el eco) y verifica que la RED se
//! concede SOLO con `net` declarada y RESTRINGIDA al allow-list de hosts:
//!
//! - CON `net` (allow-list = el listener local) → conecta y recibe el eco.
//! - SIN `net` → `connect` FALLA (el `socket_addr_check` por defecto rechaza).
//! - CON `net` pero el listener NO está en el allow-list → conexión RECHAZADA.
//!
//! Todo con un `TcpListener` local — sin servidor externo. SKIP sin el target
//! `wasm32-wasip2`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

use norte_plugin_host::{Capabilities, PluginRuntime};

/// Arranca un servidor de eco local en `127.0.0.1:0`; devuelve su `addr` y
/// atiende conexiones en un hilo (lee un chunk, lo devuelve) hasta que el
/// listener se cierra al terminar el test.
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
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };
    let rt = PluginRuntime::new().expect("runtime");
    let addr = spawn_echo();
    let target = addr.to_string(); // "127.0.0.1:PORT"

    // 1) CON `net` (allow-list = el propio addr): conecta y recibe el eco.
    let mut inst = rt
        .instantiate(&wasm, net_caps(&[&target]))
        .expect("instanciar con net");
    let out = inst
        .run_command("connect", &target)
        .expect("connect con net");
    assert_eq!(out, "ping", "con net concedida, la sonda recibe el eco");

    // 2) SIN `net`: el socket_addr_check por defecto RECHAZA → connect falla.
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instanciar sin net");
    let err = inst
        .run_command("connect", &target)
        .expect_err("sin net, connect debe fallar");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("connect") || msg.contains("Guest"),
        "el fallo viene del connect rechazado: {msg}"
    );

    // 3) CON `net` pero el listener NO está en el allow-list: RECHAZADO.
    let mut inst = rt
        .instantiate(&wasm, net_caps(&["10.0.0.1", "10.0.0.1:1"]))
        .expect("instanciar net allow-list ajeno");
    let err = inst
        .run_command("connect", &target)
        .expect_err("host fuera del allow-list debe fallar");
    assert!(
        format!("{err:?}").contains("connect") || format!("{err:?}").contains("Guest"),
        "conexión a un host fuera del allow-list rechazada"
    );
}

fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
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
        .expect("cargo build del guest");
    assert!(status.success(), "el guest {name} no compiló");
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
