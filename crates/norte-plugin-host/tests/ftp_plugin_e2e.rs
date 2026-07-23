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

// ---- #30 M1: guardas de la caché RETR guest-side (lectura O(n)) ----

/// Configura un `ProviderInstance` del guest ftp-provider contra un libunftp
/// sobre `home`, listo para leer. `None` (SKIP) sin el target wasm. Devuelve el
/// runtime junto a la instancia para mantener vivo el ticker de época.
fn configured_ftp_provider(
    home: PathBuf,
) -> Option<(PluginRuntime, norte_plugin_host::ProviderInstance)> {
    let wasm = build_guest("ftp-provider")?;
    let port = spawn_ftp_server(home);
    let rt = PluginRuntime::new().expect("runtime");
    let mut inst = rt
        .instantiate_provider(&wasm, Capabilities::with_net(vec!["127.0.0.1".to_owned()]))
        .expect("instanciar provider");
    let cfg = norte_plugin_host::provider_iface::ProviderConfig {
        endpoint: format!("127.0.0.1:{port}"),
        user: "anonymous".to_owned(),
        password: "anonymous".to_owned(),
        base: "/".to_owned(),
    };
    inst.configure(&cfg)
        .expect("configure sin trap")
        .expect("configure ok");
    Some((rt, inst))
}

/// Lee `segments` por chunks de `chunk` bytes (como el adapter), reensamblando;
/// para en el primer chunk corto (EOF).
fn read_all_chunked(
    inst: &mut norte_plugin_host::ProviderInstance,
    segments: &[Vec<u8>],
    chunk: u64,
) -> Vec<u8> {
    // El helper trata un chunk corto como EOF: sólo es válido si `chunk` no supera
    // el techo del guest (1 MiB), donde un short-read = EOF de verdad.
    assert!(chunk <= 1 << 20, "el helper asume chunk <= techo del guest");
    let mut out = Vec::new();
    let mut off = 0u64;
    loop {
        let c = inst
            .read(segments, off, chunk)
            .expect("read sin trap")
            .expect("read ok");
        if c.is_empty() {
            break;
        }
        off += c.len() as u64;
        let short = (c.len() as u64) < chunk;
        out.extend_from_slice(&c);
        if short {
            break;
        }
    }
    out
}

#[test]
fn ftp_secuencial_grande_byte_exacto() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 512 KiB > varios chunks de 64 KiB: ejercita el reuso del RETR.
    let content: Vec<u8> = (0u32..512 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("big.bin"), &content).expect("sembrar");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    let got = read_all_chunked(&mut inst, &[b"big.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "lectura secuencial byte-exacta");
}

#[test]
fn ftp_intercalar_stat_no_desincroniza() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content: Vec<u8> = (0u32..200 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("f.bin"), &content).expect("sembrar f");
    std::fs::write(dir.path().join("otro.txt"), b"hola").expect("sembrar otro");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Lee un chunk (medio fichero), ABANDONA sin llegar a EOF, luego stat de otro
    // path: si la caché no se drenara, el 226 pendiente desincronizaría el stat.
    let half = inst
        .read(&[b"f.bin".to_vec()], 0, 64 * 1024)
        .expect("read sin trap")
        .expect("read ok");
    assert_eq!(half.len(), 64 * 1024);
    let st = inst
        .stat(&[b"otro.txt".to_vec()])
        .expect("stat sin trap")
        .expect("otro.txt existe");
    assert_eq!(
        st.size,
        Some(4),
        "stat tras lectura abandonada NO desincroniza"
    );
    // Relectura entera del primero sigue byte-exacta.
    let got = read_all_chunked(&mut inst, &[b"f.bin".to_vec()], 64 * 1024);
    assert_eq!(got, content, "relectura entera byte-exacta");
}

#[test]
fn ftp_rango_luego_list_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("r.bin"), b"0123456789").expect("sembrar");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    // Rango acotado (offset 2, 3 bytes) = "234"; deja la caché viva.
    let slice = inst
        .read(&[b"r.bin".to_vec()], 2, 3)
        .expect("read sin trap")
        .expect("read ok");
    assert_eq!(slice, b"234");
    // list_dir de la raíz debe funcionar (flush de la caché antes del comando).
    let page = inst
        .list_dir(&[], None)
        .expect("list sin trap")
        .expect("raíz lista");
    assert!(
        page.entries.iter().any(|e| e.name == b"r.bin"),
        "list tras rango ve el fichero: la caché se drenó limpio"
    );
}

#[test]
fn ftp_reread_no_secuencial_sobre_cache_viva() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 130 KiB NO alineado a 64 KiB: cruza fronteras de chunk con cola parcial.
    let content: Vec<u8> = (0u32..130 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("g.bin"), &content).expect("sembrar");
    let Some((_rt, mut inst)) = configured_ftp_provider(dir.path().to_path_buf()) else {
        return;
    };
    let seg = [b"g.bin".to_vec()];
    // Un chunk desde 0 (deja la caché VIVA en next_offset=64Ki).
    let a = inst.read(&seg, 0, 64 * 1024).expect("read").expect("ok");
    assert_eq!(a.as_slice(), &content[..64 * 1024]);
    // Re-lee desde 0 SIN op de flush intermedia: offset no casa (next_offset=64Ki)
    // → miss → flush+re-RETR. Debe dar los MISMOS primeros bytes, no basura.
    let b = inst.read(&seg, 0, 64 * 1024).expect("read").expect("ok");
    assert_eq!(b.as_slice(), &content[..64 * 1024], "re-lectura desde 0 byte-exacta");
    // Salto hacia delante a 128 KiB (miss otra vez) → cola de 2 KiB.
    let c = inst.read(&seg, 128 * 1024, 64 * 1024).expect("read").expect("ok");
    assert_eq!(c.as_slice(), &content[128 * 1024..], "salto adelante byte-exacto (cola)");
    // Y una lectura secuencial entera desde cero sigue correcta.
    let whole = read_all_chunked(&mut inst, &seg, 64 * 1024);
    assert_eq!(whole, content, "lectura entera byte-exacta tras los saltos");
}
