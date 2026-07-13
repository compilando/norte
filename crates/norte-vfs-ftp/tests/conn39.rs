//! Issue #39 (fase 6e): la conexión de control ÚNICA de FTP rompía dos
//! escenarios que el wire al engine vuelve reales — cancelar un RETR a mitad
//! (desincroniza el control: la siguiente op lee la respuesta rancia) y la
//! copia FTP→FTP mismo host (el read retiene el lock, el write deadlockea).
//!
//! Solo-Linux, como el resto de la suite in-process (el harness usa el FS).
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::VPath;
use norte_vfs::Provider;
use norte_vfs_ftp::FtpProvider;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("ftp://test:21{p}")).expect("wire válido")
}

/// M1 de #39: soltar (cancelar) un stream de lectura a MITAD de un RETR debe
/// dejar la conexión de control utilizable — la siguiente op resincroniza
/// (drena la respuesta 226/426 pendiente) en vez de leerla como suya.
#[tokio::test]
async fn cancelar_read_a_mitad_no_desincroniza_la_conexion() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Fichero grande: garantiza que el RETR sigue en vuelo al cancelar.
    std::fs::write(dir.path().join("grande.bin"), vec![7u8; 4 * 1024 * 1024]).unwrap();
    std::fs::write(dir.path().join("otro.txt"), b"ok").unwrap();
    let ftp = common::connect(dir.path()).await;
    let p = FtpProvider::new(ftp, "/").await.expect("provider");

    let mut stream = p.read(&vp("/grande.bin"), None).await.expect("read abre");
    // Un chunk y CANCELACIÓN (drop del stream con el RETR a mitad).
    let primero = stream.next().await.expect("primer chunk").expect("bytes");
    assert!(!primero.is_empty());
    drop(stream);

    // La siguiente operación sobre la MISMA conexión debe funcionar.
    let e = tokio::time::timeout(Duration::from_secs(10), p.stat(&vp("/otro.txt")))
        .await
        .expect("stat no se cuelga")
        .expect("stat funciona tras cancelar un read");
    assert_eq!(e.size, Some(2));

    // Y una lectura completa posterior también.
    let mut rd = p.read(&vp("/otro.txt"), None).await.expect("read 2");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.expect("chunk válido"));
    }
    assert_eq!(out, b"ok");
}

/// B1 de #39: copia FTP→FTP en el MISMO host (mismo provider). El engine
/// intercala `read.next()` → `sink.write()`: con una sola conexión de control
/// el read la retiene y el write deadlockea. El provider con conexión de
/// LECTURA dedicada (`with_reader`) debe completar la copia.
#[tokio::test]
async fn copia_mismo_host_no_deadlockea() {
    let dir = tempfile::tempdir().expect("tempdir");
    let contenido: Vec<u8> = (0..2 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.path().join("src.bin"), &contenido).unwrap();

    // Dos conexiones al MISMO servidor: control principal + lectura dedicada.
    let main = common::connect(dir.path()).await;
    let reader = common::connect(dir.path()).await;
    let p = FtpProvider::with_reader(main, reader, "/")
        .await
        .expect("provider con lectura dedicada");

    let copia = async {
        let mut rd = p.read(&vp("/src.bin"), None).await.expect("read abre");
        let mut sink = p.write(&vp("/dst.bin")).await.expect("write abre");
        while let Some(chunk) = rd.next().await {
            let chunk: Bytes = chunk.expect("chunk válido");
            sink.write(chunk).await.expect("write chunk");
        }
        sink.commit().await.expect("commit");
    };
    tokio::time::timeout(Duration::from_mins(1), copia)
        .await
        .expect("la copia mismo-host no debe deadlockear");

    let e = p.stat(&vp("/dst.bin")).await.expect("stat dst");
    assert_eq!(e.size, Some(contenido.len() as u64));
}

/// La cancelación con conexión de lectura dedicada tampoco contamina: el
/// resync aplica a CADA conexión por separado.
#[tokio::test]
async fn cancelar_read_con_conexion_dedicada_resincroniza() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("grande.bin"), vec![7u8; 4 * 1024 * 1024]).unwrap();
    let main = common::connect(dir.path()).await;
    let reader = common::connect(dir.path()).await;
    let p = FtpProvider::with_reader(main, reader, "/")
        .await
        .expect("provider");

    let mut stream = p.read(&vp("/grande.bin"), None).await.expect("read abre");
    let _ = stream.next().await.expect("primer chunk").expect("bytes");
    drop(stream);

    // Una segunda lectura reutiliza la conexión de lectura resincronizada.
    let mut rd = p.read(&vp("/grande.bin"), None).await.expect("read 2");
    let mut total = 0usize;
    while let Some(c) = rd.next().await {
        total += c.expect("chunk válido").len();
    }
    assert_eq!(total, 4 * 1024 * 1024);
}
