//! Test de integración NIGHTLY contra un servidor FTP **real**
//! (`delfer/alpine-ftp-server`, pure-ftpd) vía testcontainers (ADR 0014).
//!
//! Fuera del gate de PR: exige Docker y, por el PASV de FTP, pinnea los puertos
//! (control 21→2121 y pasivos `MIN_PORT`..`MAX_PORT` 1:1, con
//! `ADDRESS=127.0.0.1`) — así el `227` que anuncia el servidor es alcanzable
//! desde el host. Lo corre el workflow nightly (`--features it-ftp`), nunca
//! `just ci`. La suite in-process de libunftp ya cubre la lógica en CI normal;
//! esto valida que el provider habla con un servidor de PRODUCCIÓN distinto
//! (pure-ftpd, no libunftp).
//!
//! Un solo test por contenedor (los puertos pinneados no permiten dos a la vez);
//! la conexión es un helper de test MÍNIMO (auth cleartext) — la gestión real
//! de conexión/secretos/FTPS es fase 6.
#![cfg(feature = "it-ftp")]

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{EntryKind, VPath};
use norte_vfs::Provider;
use norte_vfs_ftp::FtpProvider;
use suppaftp::tokio::AsyncRustlsFtpStream;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

const USER: &str = "test";
const PASS: &str = "test";
/// Home del usuario de test en la imagen (la base remota del provider).
const BASE: &str = "/home/test";
/// Puerto de control en el host (fijo: el nightly corre este test solo).
const CONTROL_PORT: u16 = 2121;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("ftp://127.0.0.1:21{p}")).expect("wire válido")
}

async fn read_all(p: &FtpProvider, path: &VPath) -> Vec<u8> {
    let mut rd = p.read(path, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    out
}

/// Roundtrip (write/stat/list/read) + resume por APPE contra pure-ftpd real,
/// todo en un contenedor (los puertos pinneados no permiten dos a la vez).
#[tokio::test]
async fn realftp_roundtrip_y_resume() {
    let mut req = GenericImage::new("delfer/alpine-ftp-server", "latest")
        .with_wait_for(WaitFor::message_on_stderr("passwd:"))
        .with_env_var("USERS", "test|test|/home/test")
        .with_env_var("ADDRESS", "127.0.0.1")
        .with_env_var("MIN_PORT", "30000")
        .with_env_var("MAX_PORT", "30009")
        .with_mapped_port(CONTROL_PORT, 21.tcp());
    // PASV: los puertos pasivos deben ser alcanzables en el host con el MISMO
    // número (el servidor anuncia 127.0.0.1:3000x).
    for port in 30000..=30009u16 {
        req = req.with_mapped_port(port, port.tcp());
    }
    let _container = req.start().await.expect("arrancar contenedor ftp");

    // "passwd:" (creación del usuario) no garantiza que el ftpd ya escuche:
    // reintenta connect+login hasta que responda.
    let addr = format!("127.0.0.1:{CONTROL_PORT}");
    let mut ftp = None;
    for _ in 0..40 {
        if let Ok(mut s) = AsyncRustlsFtpStream::connect(&addr).await
            && s.login(USER, PASS).await.is_ok()
        {
            ftp = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let ftp = ftp.expect("el ftpd real no aceptó conexión/login a tiempo");
    let p = FtpProvider::new(ftp, BASE).await.expect("provider");

    // --- roundtrip ---
    let mut sink = p.write(&vp("/hola.txt")).await.expect("write abre");
    sink.write(Bytes::from_static(b"contenido real ftp"))
        .await
        .unwrap();
    sink.commit().await.expect("commit");

    let e = p.stat(&vp("/hola.txt")).await.expect("stat");
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(18));

    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut visto = false;
    while let Some(item) = stream.next().await {
        let entry = item.expect("entrada válida");
        if let Some(n) = entry.path.file_name()
            && n.as_bytes() == b"hola.txt"
        {
            visto = true;
        }
    }
    assert!(visto, "el archivo escrito aparece en el listado");
    assert_eq!(read_all(&p, &vp("/hola.txt")).await, b"contenido real ftp");

    // --- resume por APPE ---
    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep");

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 4, "reanuda desde lo conservado");
    sink.write(Bytes::from_static(b"mundo")).await.unwrap();
    sink.commit().await.expect("commit");
    assert_eq!(read_all(&p, &vp("/big.bin")).await, b"holamundo");
}
