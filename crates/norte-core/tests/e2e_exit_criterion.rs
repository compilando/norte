//! Fase 10b — E2E del criterio de salida M2 ("sin sorpresas"): leer desde un
//! zip local y mover su contenido por S3 (`object-fs`) y de vuelta, byte-exacto,
//! con nombres hostiles preservados y cancelación limpia.
//!
//! Remoto = `object-fs` (S3 stand-in); sftp queda en su suite + nightly. El
//! resume está cubierto en `engine_resume.rs`; object no reanuda (ADR 0016).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Authority, Segment, TaskState, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Nombre hostil UTF-8 (S3 y sftp son UTF-8-only): unicode + emoji + espacio.
const HOSTILE: &[u8] = "año 名前 😀.txt".as_bytes();

/// Engine con: `MemProvider` (contiene `a.zip` + hace de "local"),
/// `ObjectProvider` (`object-fs` sobre tempdir, scheme `s3`).
async fn setup() -> Engine {
    let engine = Engine::new();

    // Zip con nombre hostil + un binario "grande" (256 KiB).
    let big = vec![0xABu8; 256 * 1024];
    let zip = ZipSmith::new()
        .file(HOSTILE, b"contenido hostil")
        .file(b"grande.bin", &big)
        .build();
    let mem = Arc::new(MemProvider::new());
    let mut sink = mem.write(&vp("mem:///a.zip")).await.expect("write zip");
    sink.write(Bytes::copy_from_slice(&zip))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    engine.register_provider(mem as Arc<dyn Provider>);

    // object-fs sobre tempdir (idiom de vfs-object/tests/common::fs_operator).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    opendal::install_default();
    let op = opendal::Operator::new(
        opendal::services::Fs::default()
            .root(root.to_str().expect("utf8"))
            .atomic_write_dir(atomic.to_str().expect("utf8")),
    )
    .expect("operator fs");
    std::mem::forget(dir);
    engine.register_provider(Arc::new(ObjectProvider::new(op, "s3")) as Arc<dyn Provider>);

    engine
}

/// Drena `read` a bytes.
async fn read_all(engine: &Engine, p: &VPath) -> Vec<u8> {
    let mut s = engine.read(p, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// La raíz S3 de test (`s3://norte-test/`).
fn s3_root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

fn s3(name: &[u8]) -> VPath {
    s3_root().join(Segment::new(name.to_vec()).expect("seg"))
}

#[tokio::test]
async fn reads_hostile_name_from_zip_byte_exact() {
    let engine = setup().await;
    let inside = vp("zip+mem:///a.zip/!").join(Segment::new(HOSTILE.to_vec()).unwrap());
    assert_eq!(read_all(&engine, &inside).await, b"contenido hostil");
}

#[tokio::test]
async fn zip_to_s3_to_local_roundtrip_byte_exact() {
    let engine = setup().await;
    let from_zip = vp("zip+mem:///a.zip/!").join(Segment::new(HOSTILE.to_vec()).unwrap());

    // 1. zip → S3 (nombre hostil preservado).
    let on_s3 = s3(HOSTILE);
    assert_eq!(
        engine
            .copy(&from_zip, &on_s3)
            .await
            .expect("copy zip→s3")
            .join()
            .await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &on_s3).await, b"contenido hostil");

    // 2. S3 → "local" (Mem), byte-exacto.
    let local = vp("mem:///restaurado.txt");
    assert_eq!(
        engine
            .copy(&on_s3, &local)
            .await
            .expect("copy s3→local")
            .join()
            .await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &local).await, b"contenido hostil");

    // 3. Round-trip S3 → S3 (otra key), byte-exacto.
    let on_s3_b = s3(b"copia.txt");
    assert_eq!(
        engine
            .copy(&on_s3, &on_s3_b)
            .await
            .expect("copy s3→s3")
            .join()
            .await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &on_s3_b).await, b"contenido hostil");
}

#[tokio::test]
async fn cancel_zip_to_s3_leaves_no_partial() {
    let engine = setup().await;
    let big_from = vp("zip+mem:///a.zip/!/grande.bin");
    let big_to = s3(b"grande.bin");

    let handle = engine.copy(&big_from, &big_to).await.expect("copy");
    handle.cancel();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Cancelled | TaskState::Completed),
        "cancel: o cancelada o completada antes de cancelar, nunca a medias: {state:?}"
    );

    // Cancelado → destino ausente (sin objeto a medias); completado → íntegro.
    match engine.stat(&big_to).await {
        Ok(_) => assert_eq!(read_all(&engine, &big_to).await.len(), 256 * 1024),
        Err(norte_proto::Error::NotFound) => {}
        Err(e) => panic!("estado inesperado del destino: {e:?}"),
    }
}
