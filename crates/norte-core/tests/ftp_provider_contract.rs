//! `provider_contract!` sobre el guest FTP-por-plugin (#30 stage 3c, ADR 0033)
//! contra un servidor `libunftp` IN-PROCESS — la MISMA suite que pasaban
//! `MemProvider`, `SftpProvider` y el difunto `norte-vfs-ftp`, ahora sobre el
//! provider ejecutándose en WASM. Usa el artefacto `.wasm` EMBEBIDO en
//! `norte-core`, así que valida exactamente lo que se envía.
//!
//! Solo-Linux (como el contrato sftp/ftp original): `libunftp` mapea las ops
//! sobre el FS del host, cuya fidelidad exige un FS POSIX (case-sensitive,
//! byte-preserving). El provider es OS-agnóstico.
//!
//! A diferencia de los E2E de wasm que hacen SKIP, `provider_contract!` NO sabe
//! saltar: este test REQUIERE el runtime wasmtime (siempre presente) + el
//! artefacto embebido (siempre presente). No compila ni ejecuta ningún guest en
//! tiempo de test — el `.wasm` ya está dentro del binario.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use norte_core::ftp_plugin::connect_ftp_plugin;
use norte_core::plugin_provider::PluginProvider;
use norte_proto::{Scheme, VPath};

/// Arranca `libunftp` sobre `home` en un puerto efímero (hilo con su propio
/// runtime tokio) y devuelve el puerto. Espera a que escuche. Copiado del
/// helper `spawn_ftp_server` de `norte-plugin-host/tests/ftp_plugin_e2e.rs`.
fn spawn_libunftp(home: PathBuf) -> u16 {
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
            .greeting("norte ftp-por-plugin contract")
            .build()
            .expect("build server");
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });

    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("el servidor ftp no arrancó en :{port}");
}

/// Un provider FTP-por-plugin FRESCO sobre un tempdir + servidor libunftp
/// in-process, conectado por el wiring real (resuelve → net → configure).
async fn fresh() -> PluginProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    // El tempdir vive tanto como el provider (tests efímeros; el SO limpia /tmp).
    std::mem::forget(dir);
    connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("provider ftp-por-plugin conectado")
}

/// La raíz del `PluginProvider` es SCHEME-ONLY (sin authority): `segments()`
/// del adapter lo asume (a diferencia del root con authority del difunto
/// `norte-vfs-ftp`).
fn ftp_root() -> VPath {
    VPath::root(Scheme::new("ftp").expect("scheme ftp"), None)
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod ftp_plugin_inproc,
    factory: fresh().await,
    root: ftp_root(),
    hostile_names: hostile_names(),
}
