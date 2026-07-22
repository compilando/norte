//! E2E del stack de red completo (#30 stage 3b, PROOF): un guest FTP REAL
//! (`ftp-probe`, suppaftp SYNC sobre wasi:sockets) conecta a un servidor FTP
//! IN-PROCESS (`libunftp`, sin Docker) A TRAVÉS del gating de la capability
//! `net`, y lista un directorio. Prueba: capability `net` gateada → cliente FTP
//! compilado a wasm → servidor FTP real, end-to-end.
//!
//! El puerto de DATOS pasivo lo negocia el servidor (dinámico): funciona porque
//! el allow-list de `net` es por IP (bare-ip = todos los puertos del host), lo
//! que el FTP pasivo EXIGE (justifica esa semántica del stage 3a).
//!
//! Solo-Linux (el harness libunftp mapea sobre el FS del host) + SKIP sin el
//! target `wasm32-wasip2`.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use norte_plugin_host::{Capabilities, PluginRuntime};

/// Arranca libunftp sobre `home` en un puerto efímero (hilo con su propio
/// runtime tokio) y devuelve el puerto. Espera a que escuche.
fn spawn_ftp_server(home: PathBuf) -> u16 {
    // Puerto efímero por bind-then-drop (misma técnica que el harness de
    // norte-vfs-ftp).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let server = libunftp::ServerBuilder::new(Box::new(move || {
                unftp_sbe_fs::Filesystem::new(home.clone()).expect("fs backend")
            }))
            .greeting("norte plugin ftp e2e")
            .build()
            .expect("build server");
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });

    // Espera a que el listen bindee.
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("el servidor ftp no arrancó en :{port}");
}

#[test]
fn ftp_plugin_stack_e2e() {
    let Some(wasm) = build_guest("ftp-probe") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return;
    };

    // Tempdir con DOS ficheros sembrados: el LIST del guest debe ver 2 entradas.
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["uno.txt", "dos.txt"] {
        let mut f = std::fs::File::create(dir.path().join(name)).expect("crear");
        f.write_all(b"x").expect("escribir");
    }
    let port = spawn_ftp_server(dir.path().to_path_buf());
    let target = format!("127.0.0.1:{port}");

    let rt = PluginRuntime::new().expect("runtime");
    // CON `net` (allow-list = 127.0.0.1, todos los puertos → control + datos
    // pasivos): el guest conecta al FTP y lista.
    let mut inst = rt
        .instantiate(&wasm, Capabilities::with_net(vec!["127.0.0.1".to_owned()]))
        .expect("instanciar con net");
    let out = inst
        .run_command("list", &target)
        .expect("el guest FTP debe conectar y listar sobre net gateada");
    assert_eq!(out, "2", "LIST de / ve los 2 ficheros sembrados: {out:?}");

    // SIN `net`: ni el connect de control sale (socket_addr_check rechaza).
    let mut inst = rt
        .instantiate(&wasm, Capabilities::default())
        .expect("instanciar sin net");
    assert!(
        inst.run_command("list", &target).is_err(),
        "sin la capability net el guest FTP no puede ni conectar"
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
