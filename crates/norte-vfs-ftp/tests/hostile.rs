//! Contención del provider FTP (ADR 0014): nombres no representables y paths
//! fuera de base se rechazan LIMPIO. FTP no tiene symlinks ni un servidor
//! hostil fácil de inyectar (libunftp es honesto); la contención estructural
//! (`/`, `.`, `..` en un nombre) la garantizan el `VPath` y el builder de
//! `Segment` (cubierto por el corpus hostil del contrato). Aquí se cubre lo
//! específico de FTP: el encoding lossy de suppaftp.
//!
//! Solo-Linux (como el contrato): el harness se respalda en el FS del host.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, Error, VPath};
use norte_vfs::Provider;
use norte_vfs_ftp::FtpProvider;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("ftp://test:21{p}")).expect("wire válido")
}

async fn provider(base: &std::path::Path) -> FtpProvider {
    let ftp = common::connect(base).await;
    FtpProvider::new(ftp, "/").await.expect("provider")
}

/// suppaftp decodifica los nombres del servidor con `from_utf8_lossy`: un
/// nombre no-UTF8 llega sustituido por U+FFFD y los bytes originales se pierden
/// bajo la frontera. El provider lo RECHAZA (`InvalidPath`) en vez de emitir un
/// `Entry` con bytes corruptos que colisionaría o apuntaría a un fichero
/// inexistente (regla 1 / ADR 0014 D2, issue #37).
#[tokio::test]
async fn readdir_nombre_no_utf8_no_se_corrompe() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().expect("tempdir");
    // `café` en Latin-1: byte 0xE9 crudo, imposible de crear vía el provider.
    let raw = std::ffi::OsStr::from_bytes(b"caf\xE9.txt");
    std::fs::write(dir.path().join(raw), b"x").unwrap();
    let p = provider(dir.path()).await;

    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut corrupto = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                let name = entry.path.file_name().expect("con nombre");
                // Si el provider emite una entrada, JAMÁS con U+FFFD sustituido.
                if name.as_bytes().windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]) {
                    corrupto = true;
                }
            }
            // Rechazo limpio: contención OK.
            Err(Error::InvalidPath) => {}
            Err(other) => panic!("error inesperado: {other:?}"),
        }
    }
    assert!(
        !corrupto,
        "un nombre no-UTF8 se emitió corrupto (U+FFFD) en silencio"
    );
}

/// Un nombre no-UTF8 construido por el cliente se rechaza LIMPIO en la
/// frontera de escritura (`from_utf8` en `remote()`), jamás lossy.
#[tokio::test]
async fn nombre_no_utf8_se_rechaza_limpio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;
    let seg = norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap();
    let hostil = FtpProvider::root(Authority::new("test:21").unwrap()).join(seg);
    assert_eq!(p.stat(&hostil).await.unwrap_err(), Error::InvalidPath);
    assert!(p.write(&hostil).await.is_err());
}

/// Inyección CRLF: FTP es un protocolo de LÍNEAS (el comando termina en CRLF).
/// Un nombre con `\r`/`\n` intentaría inyectar un comando FTP arbitrario
/// (`STOR x\r\nDELE víctima`). `Segment` lo admite (es válido en POSIX), pero
/// el provider FTP lo RECHAZA limpio en `remote()` — contención específica de
/// FTP (sftp, binario, es inmune).
#[tokio::test]
async fn nombre_con_crlf_se_rechaza() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;
    let seg =
        norte_proto::Segment::new(b"x\r\nDELE victima".to_vec()).expect("segment admite CRLF");
    let hostil = FtpProvider::root(Authority::new("test:21").unwrap()).join(seg);
    assert_eq!(p.stat(&hostil).await.unwrap_err(), Error::InvalidPath);
    assert!(p.write(&hostil).await.is_err());
    // También un `\n` suelto.
    let seg2 = norte_proto::Segment::new(b"y\nNOOP".to_vec()).unwrap();
    let hostil2 = FtpProvider::root(Authority::new("test:21").unwrap()).join(seg2);
    assert_eq!(p.stat(&hostil2).await.unwrap_err(), Error::InvalidPath);
}

/// Nombres con `;` y con espacio inicial roundtripean byte-exacto por MLSD
/// (Hallazgo B del encoding-auditor): el extractor de nombre de suppaftp
/// (`split(';').last().trim_start()`) truncaría `a;b.txt` a `b.txt` y perdería
/// el espacio inicial; el provider saca el nombre CRUDO de la línea MLSD
/// (`split_once(' ')`, RFC 3659) para no corromperlo.
#[tokio::test]
async fn nombres_con_punto_y_coma_y_espacio_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;
    let hostiles: [&[u8]; 2] = [b"a;b.txt", b" sp.txt"];
    for raw in hostiles {
        let seg = norte_proto::Segment::new(raw.to_vec()).unwrap();
        let path = FtpProvider::root(Authority::new("test:21").unwrap()).join(seg);
        let mut sink = p.write(&path).await.expect("write abre");
        sink.write(Bytes::from_static(b"x")).await.unwrap();
        sink.commit().await.expect("commit");
    }
    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut names: Vec<Vec<u8>> = Vec::new();
    while let Some(item) = stream.next().await {
        let entry = item.expect("entrada válida");
        names.push(
            entry
                .path
                .file_name()
                .expect("con nombre")
                .as_bytes()
                .to_vec(),
        );
    }
    for raw in hostiles {
        assert!(
            names.iter().any(|n| n.as_slice() == raw),
            "el nombre {raw:?} debe volver byte-exacto por MLSD, no truncado/strippeado"
        );
    }
}

/// El provider nunca sale de su base: escribir crea el fichero DENTRO del
/// tempdir que respalda al servidor, en ningún otro sitio.
#[tokio::test]
async fn escritura_queda_dentro_de_la_base() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;

    let mut sink = p.write(&vp("/dentro.bin")).await.expect("write abre");
    sink.write(Bytes::from_static(b"contenido")).await.unwrap();
    sink.commit().await.expect("commit");
    assert_eq!(
        std::fs::read(dir.path().join("dentro.bin")).unwrap(),
        b"contenido"
    );
}
