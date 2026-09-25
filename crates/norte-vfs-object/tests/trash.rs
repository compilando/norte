//! Object/S3's logical `.norte-trash/` trash (phase 9c, ADR 0019) against
//! opendal's `services-fs` harness.
mod common;

use futures::TryStreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_object::ObjectProvider;

/// A fresh provider over a tempdir via `services-fs`, with the logical
/// trash in the requested state.
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

/// The provider's root (`s3://norte-test/`).
fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

/// Lists a dir's children's names (bytes) (drains the `EntryStream`).
async fn child_names(p: &ObjectProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(entry) = stream.try_next().await.expect("entry") {
        names.push(
            entry
                .path
                .file_name()
                .expect("named child")
                .as_bytes()
                .to_vec(),
        );
    }
    names
}

/// The one `<id>` under `.norte-trash/`.
async fn sole_entry(p: &ObjectProvider) -> VPath {
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "one trash entry");
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

    let dest = p
        .trash(
            &victim,
            &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
        )
        .await
        .expect("trash");

    // The source disappears.
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    let entry = sole_entry(&p).await;
    // The entry contains EXACTLY {payload, .norte-info}, no extra markers.
    let mut names = child_names(&p, &entry).await;
    names.sort();
    let mut expected = vec![b".norte-info".to_vec(), b"victim.txt".to_vec()];
    expected.sort();
    assert_eq!(names, expected);
    // The payload preserves the content.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(
        common::read_all(&p, &payload).await.expect("read payload"),
        b"contenido"
    );
    // The LOGICAL trash returns the recoverable destination → the
    // journal's reversal_ref (M3-1b): it is EXACTLY the payload inside
    // `.norte-trash/<id>`.
    assert_eq!(
        dest.expect("logical trash returns a recoverable destination"),
        payload,
        "the returned dest is the payload's path"
    );
    // `.norte-info` decodes to the original path, anchored to the connection.
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = common::read_all(&p, &info_path).await.expect("read info");
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_moves_directory_tree() {
    // The copy-all→delete-all rename takes the WHOLE tree.
    let p = fresh(true);
    let dir = root().join(Segment::new(b"proj".to_vec()).unwrap());
    p.mkdir(&dir).await.expect("mkdir proj");
    let sub = dir.join(Segment::new(b"sub".to_vec()).unwrap());
    p.mkdir(&sub).await.expect("mkdir sub");
    let deep = sub.join(Segment::new(b"b.txt".to_vec()).unwrap());
    common::write_all(&p, &deep, b"hondo").await;

    p.trash(&dir, &norte_vfs::trash::TrashId::new(0, u64::from(line!())))
        .await
        .expect("trash");
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
        p.trash(
            &victim,
            &norte_vfs::trash::TrashId::new(0, u64::from(line!()))
        )
        .await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&victim).await.is_ok());
}

#[tokio::test]
async fn trash_refuses_to_trash_itself() {
    let p = fresh(true);
    let victim = root().join(Segment::new(b"v.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"a").await;
    p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .expect("trash");

    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    assert!(matches!(
        p.trash(
            &trash_dir,
            &norte_vfs::trash::TrashId::new(0, u64::from(line!()))
        )
        .await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&trash_dir).await.is_ok());
}

#[tokio::test]
async fn trash_preserves_hostile_basename() {
    // S3 keys are UTF-8-only (like sftp): the hostile name is twisted UTF-8.
    let p = fresh(true);
    let hostile = "año 名前 😀.txt".as_bytes().to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());
    common::write_all(&p, &victim, b"z").await;

    p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .expect("trash");
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));
    let entry = sole_entry(&p).await;
    let names = child_names(&p, &entry).await;
    assert!(
        names.iter().any(|n| n == &hostile),
        "hostile basename preserved"
    );
}
