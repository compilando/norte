//! Humo end-to-end del provider FTP contra el servidor libunftp in-process:
//! valida el harness (libunftp + suppaftp) y el roundtrip básico antes de la
//! suite contractual completa.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{EntryKind, VPath};
use norte_vfs::Provider;
use norte_vfs_ftp::FtpProvider;

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("ftp://test:21{p}")).expect("wire válido")
}

async fn provider(base: &std::path::Path) -> FtpProvider {
    let ftp = common::connect(base).await;
    FtpProvider::new(ftp, "/").await.expect("provider")
}

#[tokio::test]
async fn write_stat_list_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;

    let mut sink = p.write(&vp("/hola.txt")).await.expect("write abre");
    sink.write(Bytes::from_static(b"contenido ftp"))
        .await
        .unwrap();
    sink.commit().await.expect("commit");

    let e = p.stat(&vp("/hola.txt")).await.expect("stat");
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(13));

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

    let mut rd = p.read(&vp("/hola.txt"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"contenido ftp");
}

#[tokio::test]
async fn resume_por_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = provider(dir.path()).await;

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep");

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 4, "reanuda desde lo conservado");
    sink.write(Bytes::from_static(b"mundo")).await.unwrap();
    sink.commit().await.expect("commit");

    let mut rd = p.read(&vp("/big.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"holamundo");
}
