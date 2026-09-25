//! The provider end-to-end: with the delegate installed where needed, and
//! with the index pinned where policy does not need a `.rar` that genuinely
//! contains it.

use futures::StreamExt;
use norte_proto::{ByteRange, Error, Scheme, Segment, VPath};
use norte_testkit::RarSmith;
use norte_vfs::Provider;
use norte_vfs_rar::{ArchiveIndex, Delegate, RarLimits, RarProvider, RawEntry};

/// A host archive's `VPath` `rar+file://…/!/…`.
fn rar_root(archive: &std::path::Path) -> VPath {
    use std::os::unix::ffi::OsStrExt;
    let mut outer = VPath::root(Scheme::new("file").unwrap(), None);
    for comp in archive.components().skip(1) {
        outer = outer.join(Segment::new(comp.as_os_str().as_bytes().to_vec()).unwrap());
    }
    VPath::archive_compose("rar", &outer, &[]).expect("compose")
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).unwrap())
}

async fn names(p: &RarProvider, at: &VPath) -> Vec<Vec<u8>> {
    p.list(at)
        .await
        .expect("list")
        .map(|e| {
            e.expect("entry")
                .path
                .file_name()
                .unwrap()
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await
}

async fn read_all(p: &RarProvider, at: &VPath, range: Option<ByteRange>) -> Vec<u8> {
    p.read(at, range)
        .await
        .expect("read")
        .fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk.expect("chunk"));
            acc
        })
        .await
}

fn write_rar(path: &std::path::Path, bytes: Vec<u8>) {
    std::fs::write(path, bytes).unwrap();
}

/// The provider for the `.rar` at `path`, or `None` if this machine brings
/// no delegate at all (in which case the test bows out saying so).
fn provider(path: &std::path::Path) -> Option<RarProvider> {
    let delegate = Delegate::discover().ok()?;
    Some(RarProvider::new(
        path.to_path_buf(),
        delegate,
        RarLimits::default(),
    ))
}

#[tokio::test]
async fn listing_and_reading_against_a_real_delegate() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"docs/hello.txt", b"hello norte\n")
            .file(b"cp437-\xa4\xa5.txt", b"bytes\n")
            .build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("no delegate installed: test withdrawn");
        return;
    };
    let root = rar_root(&archive);
    let mut top = names(&p, &root).await;
    top.sort();
    assert_eq!(top, vec![b"cp437-\xa4\xa5.txt".to_vec(), b"docs".to_vec()]);

    let leaf = child(&child(&root, b"docs"), b"hello.txt");
    assert_eq!(read_all(&p, &leaf, None).await, b"hello norte\n");
    let stat = p.stat(&leaf).await.expect("stat");
    assert_eq!(stat.size, Some(12));
    assert_eq!(p.list_skipped(&root).await.unwrap(), Some(0));
}

#[tokio::test]
async fn a_range_returns_the_slice_and_does_not_wait_for_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new().file(b"hello.txt", b"hello norte\n").build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("no delegate installed: test withdrawn");
        return;
    };
    let leaf = child(&rar_root(&archive), b"hello.txt");
    let range = Some(ByteRange {
        offset: 6,
        len: Some(5),
    });
    // `hello norte\n`: byte 6 is the `n`, not the space.
    assert_eq!(read_all(&p, &leaf, range).await, b"norte");
    let to_the_end = Some(ByteRange {
        offset: 6,
        len: None,
    });
    assert_eq!(read_all(&p, &leaf, to_the_end).await, b"norte\n");
}

/// MEASURED: both delegates treat the name as a pattern. With the twin
/// present, asking for the `star?name.txt` entry would pull out TWO files
/// stuck together and the stream would look healthy — so it is refused.
#[tokio::test]
async fn a_name_that_is_another_ones_glob_is_not_read_but_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"star?name.txt", b"pattern\n")
            .file(b"starXname.txt", b"twin\n")
            .build(),
    );
    let Some(p) = provider(&archive) else {
        eprintln!("no delegate installed: test withdrawn");
        return;
    };
    let root = rar_root(&archive);
    assert_eq!(names(&p, &root).await.len(), 2, "both are LISTED");
    let ambiguous = child(&root, b"star?name.txt");
    assert!(
        matches!(p.read(&ambiguous, None).await, Err(Error::Unsupported)),
        "the ambiguous one is refused"
    );
    let literal = child(&root, b"starXname.txt");
    assert_eq!(read_all(&p, &literal, None).await, b"twin\n");
}

/// The flag comes from the parser: no genuinely encrypted `.rar` is needed
/// to pin the policy.
#[tokio::test]
async fn an_encrypted_entry_is_listed_and_refuses_to_be_read() {
    let index = ArchiveIndex::from_raw(vec![RawEntry {
        name: b"secret.txt".to_vec(),
        size: 10,
        is_dir: false,
        mtime: None,
        encrypted: true,
        solid: false,
    }]);
    let p = RarProvider::with_index_for_test(index);
    let leaf = child(&rar_root(std::path::Path::new("/tmp/t.rar")), b"secret.txt");
    assert!(p.stat(&leaf).await.is_ok(), "encrypted but VISIBLE");
    assert!(
        matches!(p.read(&leaf, None).await, Err(Error::Unsupported)),
        "reading it is what cannot happen, and it says so"
    );
}

/// Same invalidation as `norte-vfs-archive`: `(mtime, size)`. A stale index
/// would show files that are no longer there.
#[tokio::test]
async fn touching_the_archive_invalidates_the_cached_index() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("t.rar");
    write_rar(&archive, RarSmith::new().file(b"one.txt", b"1\n").build());
    let Some(p) = provider(&archive) else {
        eprintln!("no delegate installed: test withdrawn");
        return;
    };
    let root = rar_root(&archive);
    assert_eq!(names(&p, &root).await.len(), 1);
    write_rar(
        &archive,
        RarSmith::new()
            .file(b"one.txt", b"1\n")
            .file(b"two.txt", b"2\n")
            .build(),
    );
    assert_eq!(
        names(&p, &root).await.len(),
        2,
        "the index was rebuilt when the archive changed"
    );
}

#[tokio::test]
async fn every_mutation_answers_unsupported() {
    let p = RarProvider::with_index_for_test(ArchiveIndex::from_raw(vec![]));
    let root = rar_root(std::path::Path::new("/tmp/t.rar"));
    let leaf = child(&root, b"x");
    assert!(matches!(
        p.write(&leaf).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(p.mkdir(&leaf).await, Err(Error::Unsupported)));
    assert!(matches!(p.remove(&leaf).await, Err(Error::Unsupported)));
    assert!(matches!(
        p.rename(&leaf, &root).await,
        Err(Error::Unsupported)
    ));
    assert!(
        p.capabilities()
            .flags
            .contains(norte_proto::CapabilityFlags::READ_ONLY)
    );
}

/// A `.rar` that does not exist is not an empty listing: it is `NotFound`.
#[tokio::test]
async fn a_missing_archive_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("does-not-exist.rar");
    let Some(p) = provider(&archive) else {
        return;
    };
    assert!(matches!(
        p.list(&rar_root(&archive)).await,
        Err(Error::NotFound)
    ));
}
