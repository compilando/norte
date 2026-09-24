//! sftp's logical `.norte-trash/` trash (phase 9b, ADR 0019) against the
//! in-process sftp server.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs::trash;
use norte_vfs_sftp::SftpProvider;

/// Fresh provider over a tempdir + in-process server, with the logical
/// trash in the requested state.
async fn fresh(logical_trash: bool) -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    std::mem::forget(dir);
    SftpProvider::new(session, "/").with_logical_trash(logical_trash)
}

/// The client's remote root (`/`), with a test authority.
fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

/// Drains a `ByteStream` into bytes.
async fn read_all(p: &SftpProvider, path: &VPath) -> Vec<u8> {
    let mut rd = p.read(path, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// Lists the names (bytes) of a remote dir's children (drains the
/// `EntryStream`).
async fn child_names(p: &SftpProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(item) = stream.next().await {
        let entry = item.expect("entry");
        names.push(
            entry
                .path
                .file_name()
                .expect("child has a name")
                .as_bytes()
                .to_vec(),
        );
    }
    names
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false).await;
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true).await;
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}

#[tokio::test]
async fn trash_moves_tree_and_writes_info() {
    let p = fresh(true).await;
    let victim = root().join(Segment::new(b"victim.txt".to_vec()).unwrap());

    // Seeds the file.
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"content"))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");

    // To the trash.
    p.trash(
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

    // `.norte-trash/<id>/` exists with ONE entry.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "one trash entry");
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());

    // Contains the payload + `.norte-info`.
    let mut names = child_names(&p, &entry).await;
    names.sort();
    let mut expected = vec![b".norte-info".to_vec(), b"victim.txt".to_vec()];
    expected.sort();
    assert_eq!(names, expected);

    // The payload keeps the content.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(read_all(&p, &payload).await, b"content");

    // The `.norte-info` decodes to the original path (anchored to the
    // connection).
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = read_all(&p, &info_path).await;
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    let p = fresh(false).await;
    let victim = root().join(Segment::new(b"x.txt".to_vec()).unwrap());
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"y")).await.expect("chunk");
    sink.commit().await.expect("commit");

    assert!(matches!(
        p.trash(
            &victim,
            &norte_vfs::trash::TrashId::new(0, u64::from(line!()))
        )
        .await,
        Err(norte_proto::Error::Unsupported)
    ));
    // The source is still there (did not degrade to permanent).
    assert!(p.stat(&victim).await.is_ok());
}

#[tokio::test]
async fn trash_preserves_hostile_basename() {
    let p = fresh(true).await;
    // sftp (russh-sftp) uses String paths → does NOT represent non-UTF8
    // bytes (rejects with InvalidPath, issue #37); the hostile name it CAN
    // handle is twisted UTF-8: spaces, unicode, emoji, leading dot.
    let hostile = "año 名前 😀 .txt".as_bytes().to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());

    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"z")).await.expect("chunk");
    sink.commit().await.expect("commit");

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

    // The payload inside the trash keeps the hostile bytes.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());
    let names = child_names(&p, &entry).await;
    assert!(
        names.iter().any(|n| n == &hostile),
        "hostile basename preserved"
    );
}

#[tokio::test]
async fn trash_moves_directory_tree() {
    // Actually tests ADR 0009's claim: a rename takes the WHOLE tree
    // (entries_total = 1), not just a single loose file.
    let p = fresh(true).await;
    let dir = root().join(Segment::new(b"proj".to_vec()).unwrap());
    p.mkdir(&dir).await.expect("mkdir proj");
    let sub = dir.join(Segment::new(b"sub".to_vec()).unwrap());
    p.mkdir(&sub).await.expect("mkdir sub");
    let deep = sub.join(Segment::new(b"b.txt".to_vec()).unwrap());
    let mut sink = p.write(&deep).await.expect("write");
    sink.write(Bytes::from_static(b"deep"))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");

    p.trash(&dir, &norte_vfs::trash::TrashId::new(0, u64::from(line!())))
        .await
        .expect("trash");
    assert!(matches!(
        p.stat(&dir).await,
        Err(norte_proto::Error::NotFound)
    ));

    // The whole subtree landed at `.norte-trash/<id>/proj/sub/b.txt`.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());
    let moved_deep = entry
        .join(Segment::new(b"proj".to_vec()).unwrap())
        .join(Segment::new(b"sub".to_vec()).unwrap())
        .join(Segment::new(b"b.txt".to_vec()).unwrap());
    assert_eq!(read_all(&p, &moved_deep).await, b"deep");
}

#[tokio::test]
async fn trash_refuses_to_trash_itself() {
    // Trashing `.norte-trash` (or something inside it) = Unsupported
    // (self-reference), without leaving garbage or touching the existing
    // trash.
    let p = fresh(true).await;

    // Creates the trash by trashing an arbitrary file.
    let victim = root().join(Segment::new(b"v.txt".to_vec()).unwrap());
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"a")).await.expect("chunk");
    sink.commit().await.expect("commit");
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
    // The trash is still standing.
    assert!(p.stat(&trash_dir).await.is_ok());
}
