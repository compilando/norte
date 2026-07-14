//! Papelera lógica `.norte-trash/` de object/S3 (fase 9c, ADR 0019) contra
//! el harness `services-fs` de opendal.
mod common;

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_object::ObjectProvider;

/// Provider fresco sobre un tempdir vía `services-fs`, con la papelera
/// lógica en el estado pedido.
fn fresh(logical_trash: bool) -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3").with_logical_trash(logical_trash)
}

/// La raíz del provider (`s3://norte-test/`).
fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

/// Lista los nombres (bytes) de los hijos de un dir (drena el `EntryStream`).
async fn child_names(p: &ObjectProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(entry) = stream.try_next().await.expect("entry") {
        names.push(
            entry
                .path
                .file_name()
                .expect("hijo con nombre")
                .as_bytes()
                .to_vec(),
        );
    }
    names
}

/// El único `<id>` bajo `.norte-trash/`.
async fn sole_entry(p: &ObjectProvider) -> VPath {
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "una entrada de papelera");
    trash_dir.join(Segment::new(ids[0].clone()).unwrap())
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false);
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true);
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}

#[tokio::test]
async fn trash_moves_file_and_writes_info() {
    let p = fresh(true);
    let victim = root().join(Segment::new(b"victim.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"contenido").await;

    p.trash(&victim).await.expect("trash");

    // Origen desaparece.
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    let entry = sole_entry(&p).await;
    // Payload preserva el contenido.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(
        common::read_all(&p, &payload).await.expect("read payload"),
        b"contenido"
    );
    // `.norte-info` decodifica a la ruta original, anclado a la conexión.
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = common::read_all(&p, &info_path).await.expect("read info");
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_moves_directory_tree() {
    // El rename copy-all→delete-all se lleva el ÁRBOL entero.
    let p = fresh(true);
    let dir = root().join(Segment::new(b"proj".to_vec()).unwrap());
    p.mkdir(&dir).await.expect("mkdir proj");
    let sub = dir.join(Segment::new(b"sub".to_vec()).unwrap());
    p.mkdir(&sub).await.expect("mkdir sub");
    let deep = sub.join(Segment::new(b"b.txt".to_vec()).unwrap());
    common::write_all(&p, &deep, b"hondo").await;

    p.trash(&dir).await.expect("trash");
    assert!(matches!(
        p.stat(&dir).await,
        Err(norte_proto::Error::NotFound)
    ));

    let entry = sole_entry(&p).await;
    let moved_deep = entry
        .join(Segment::new(b"proj".to_vec()).unwrap())
        .join(Segment::new(b"sub".to_vec()).unwrap())
        .join(Segment::new(b"b.txt".to_vec()).unwrap());
    assert_eq!(
        common::read_all(&p, &moved_deep).await.expect("read deep"),
        b"hondo"
    );
}

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    let p = fresh(false);
    let victim = root().join(Segment::new(b"x.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"y").await;

    assert!(matches!(
        p.trash(&victim).await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&victim).await.is_ok());
}

#[tokio::test]
async fn trash_refuses_to_trash_itself() {
    let p = fresh(true);
    let victim = root().join(Segment::new(b"v.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"a").await;
    p.trash(&victim).await.expect("trash");

    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    assert!(matches!(
        p.trash(&trash_dir).await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&trash_dir).await.is_ok());
}

#[tokio::test]
async fn trash_preserves_hostile_basename() {
    // S3 keys son UTF-8-only (como sftp): el nombre hostil es UTF-8 retorcido.
    let p = fresh(true);
    let hostile = "año 名前 😀.txt".as_bytes().to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());
    common::write_all(&p, &victim, b"z").await;

    p.trash(&victim).await.expect("trash");
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));
    let entry = sole_entry(&p).await;
    let names = child_names(&p, &entry).await;
    assert!(
        names.iter().any(|n| n == &hostile),
        "basename hostil preservado"
    );
}
