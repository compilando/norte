//! Phase 8f: resolving composite schemes (ADR 0018) in the Engine — a
//! `tar+mem`/`zip+mem` is served by composing an `ArchiveProvider` over the
//! container's provider, with no prior registration.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Error, VPath};
use norte_testkit::{MemProvider, TarSmith, ZipSmith};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

async fn engine_with_container(name: &str, bytes: &[u8]) -> Engine {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    let mut sink = mem
        .write(&vp(&format!("mem:///{name}")))
        .await
        .expect("write opens");
    sink.write(Bytes::copy_from_slice(bytes))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    engine.register_provider(mem as Arc<dyn Provider>);
    engine
}

#[tokio::test]
async fn lists_and_reads_inside_a_tar() {
    let tar = TarSmith::new().file(b"docs/x.txt", b"inside").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("list inner root")
        .map(|e| e.expect("entry ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+mem:///a.tar/!/docs");
    let e = engine
        .stat(&vp("tar+mem:///a.tar/!/docs/x.txt"))
        .await
        .expect("inner stat");
    assert_eq!(e.size, Some(6));
}

#[tokio::test]
async fn reads_inside_a_zip() {
    let zip = ZipSmith::new().file(b"hello.txt", b"from the zip").build();
    let engine = engine_with_container("a.zip", &zip).await;
    let mut stream = engine
        .read(&vp("zip+mem:///a.zip/!/hello.txt"), None)
        .await
        .expect("inner read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"from the zip");
}

/// Gzips `bytes` into a single gzip member (the same idiom as
/// `norte-vfs-archive/tests/common::gzip`, not re-exported outside the crate).
fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(bytes).expect("write gz");
    enc.finish().expect("finish gz")
}

/// #55: a composite `tar+gz` (`tar+gz+mem://…`) resolves through the same
/// Engine path as `tar`/`zip` — wiring of `provider_for`'s match (ADR 0028).
/// List + read byte-exact through the real composite provider.
#[tokio::test]
async fn lists_and_reads_inside_a_targz() {
    let tar = TarSmith::new()
        .file(b"docs/x.txt", b"inside the tgz")
        .build();
    let tgz = gzip(&tar);
    let engine = engine_with_container("a.tar.gz", &tgz).await;
    let entries: Vec<_> = engine
        .list(&vp("tar+gz+mem:///a.tar.gz/!"))
        .await
        .expect("list inner root")
        .map(|e| e.expect("entry ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.to_wire(), "tar+gz+mem:///a.tar.gz/!/docs");
    let mut stream = engine
        .read(&vp("tar+gz+mem:///a.tar.gz/!/docs/x.txt"), None)
        .await
        .expect("inner read");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(out, b"inside the tgz");
}

#[tokio::test]
async fn the_virtual_tree_declares_read_only() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    let caps = engine
        .capabilities(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("composite's capabilities");
    assert!(
        caps.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
        "the UI and the copy engine veto mutations without a round trip (ADR 0018 E2)"
    );
}

/// #56 (before: v1 rejected with InvalidPath): a zip INSIDE a tar navigates
/// and reads byte-exact — the engine composes layer by layer (recursion of
/// `provider_for`) and the zip's compressed interior is served by ranges over
/// the tar layer (composable, ADR 0018 A3).
#[tokio::test]
async fn zip_inside_tar_lists_and_reads() {
    use norte_testkit::ZipSmith;
    let zip = ZipSmith::new().file(b"one.txt", b"inner content").build();
    let tar = TarSmith::new().file(b"i.zip", &zip).build();
    let engine = engine_with_container("a.tar", &tar).await;

    let names: Vec<Vec<u8>> = engine
        .list(&vp("zip+tar+mem:///a.tar/!/i.zip/!"))
        .await
        .expect("nested list")
        .map(|e| {
            e.expect("entry")
                .path
                .segments()
                .last()
                .expect("segment")
                .to_vec()
        })
        .collect()
        .await;
    assert_eq!(names, vec![b"one.txt".to_vec()]);

    let mut stream = engine
        .read(&vp("zip+tar+mem:///a.tar/!/i.zip/!/one.txt"), None)
        .await
        .expect("nested read");
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        got.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(got, b"inner content", "byte-exact through 2 layers");
}

/// #56: the layer cap (`max_nesting`) cuts BEFORE composing — with the cap at
/// 1, a two-layer path answers `LimitExceeded("nesting")`.
#[tokio::test]
async fn nesting_over_the_cap_is_limit_exceeded() {
    use norte_testkit::ZipSmith;
    let zip = ZipSmith::new().file(b"one.txt", b"x").build();
    let tar = TarSmith::new().file(b"i.zip", &zip).build();
    let engine = engine_with_container("a.tar", &tar).await;
    engine.set_archive_limits(norte_core::ArchiveLimits {
        max_nesting: 1,
        ..norte_core::ArchiveLimits::default()
    });
    match engine
        .stat(&vp("zip+tar+mem:///a.tar/!/i.zip/!/one.txt"))
        .await
    {
        Err(Error::LimitExceeded { limit }) if limit == "nesting" => {}
        other => panic!("expected LimitExceeded(nesting), was {other:?}"),
    }
    // The SINGLE layer still works under the same cap.
    engine
        .stat(&vp("tar+mem:///a.tar/!/i.zip"))
        .await
        .expect("one layer within the cap");
}

#[tokio::test]
async fn composite_without_a_marker_is_invalid_path() {
    let engine = engine_with_container("a.tar", b"does not matter").await;
    assert_eq!(
        engine.stat(&vp("tar+mem:///a.tar")).await.unwrap_err(),
        Error::InvalidPath
    );
}

#[tokio::test]
async fn container_on_a_scheme_without_a_provider_or_connector() {
    let engine = Engine::new();
    assert_eq!(
        engine
            .stat(&vp("tar+sftp://host/a.tar/!/x"))
            .await
            .unwrap_err(),
        Error::Unsupported,
        "the interior needs a provider/connector: it is the usual error"
    );
}

#[tokio::test]
async fn two_containers_share_a_provider_without_mixing() {
    // The ArchiveProvider is cached by SCHEME (`tar+mem`), not by container:
    // every path re-mounts its exterior and the index is pinned by
    // (wire, mtime, size). Two different tars cannot mix.
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    for (name, content) in [("a.tar", b"I am A".as_slice()), ("b.tar", b"I am B!")] {
        let tar = TarSmith::new().file(b"who.txt", content).build();
        let mut sink = mem
            .write(&vp(&format!("mem:///{name}")))
            .await
            .expect("write");
        sink.write(Bytes::copy_from_slice(&tar))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    for (name, content) in [("a.tar", b"I am A".as_slice()), ("b.tar", b"I am B!")] {
        let mut stream = engine
            .read(&vp(&format!("tar+mem:///{name}/!/who.txt")), None)
            .await
            .expect("read");
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("chunk ok"));
        }
        assert_eq!(out, content, "{name}");
    }
}

#[tokio::test]
async fn copy_into_an_archive_fails_unsupported() {
    let tar = TarSmith::new().file(b"x", b"1").build();
    let engine = engine_with_container("a.tar", &tar).await;
    // Any local source (the container itself serves).
    let handle = engine
        .copy(&vp("mem:///a.tar"), &vp("tar+mem:///a.tar/!/copy"))
        .await
        .expect("the task starts; the failure is the destination's write");
    match handle.join().await {
        norte_proto::TaskState::Failed { error, .. } => {
            assert_eq!(error, Error::Unsupported, "READ_ONLY vetoes the write");
        }
        other => panic!("expected Failed(Unsupported), was {other:?}"),
    }
}

/// #95.2: `Engine::set_archive_limits` governs composite providers — with
/// `max_entries` lowered, a 3-entry tar answers `LimitExceeded("entries")`
/// instead of listing. With the defaults, the same tar lists without drama.
#[tokio::test]
async fn set_archive_limits_governs_the_composition() {
    let tar = TarSmith::new()
        .file(b"one", b"1")
        .file(b"two", b"2")
        .file(b"three", b"3")
        .build();
    let engine = engine_with_container("a.tar", &tar).await;
    engine.set_archive_limits(norte_core::ArchiveLimits {
        max_entries: 1,
        ..norte_core::ArchiveLimits::default()
    });
    match engine.list(&vp("tar+mem:///a.tar/!")).await.map(|_| ()) {
        Err(Error::LimitExceeded { limit }) if limit == "entries" => {}
        other => panic!("expected LimitExceeded(entries), was {other:?}"),
    }

    // Without touching the limits: the same container lists in full.
    let engine = engine_with_container("a.tar", &tar).await;
    let n = engine
        .list(&vp("tar+mem:///a.tar/!"))
        .await
        .expect("list with defaults")
        .count()
        .await;
    assert_eq!(n, 3);
}

// ---------- rar (roadmap item 11): only over a LOCAL file ----------

/// The delegate needs a filesystem path. Fetching the whole `.rar` from
/// sftp/s3 would be a download nobody asked for, so the composition is refused
/// BEFORE it happens, with `Unsupported`.
#[tokio::test]
async fn rar_over_a_remote_interior_is_refused_with_a_reason() {
    let engine = Engine::new();
    let p = vp("rar+sftp://host/a.rar/!/x.txt");
    assert!(matches!(engine.stat(&p).await, Err(Error::Unsupported)));
}

/// A `.rar` INSIDE another archive is not a local file either: there is no
/// path to hand the delegate, and the outer layer is not materialized to a
/// temp file behind anyone's back.
#[tokio::test]
async fn nested_rar_in_another_archive_is_not_local_either() {
    let engine = Engine::new();
    let p = vp("rar+zip+file:///o.zip/!/a.rar/!/x.txt");
    assert!(matches!(engine.stat(&p).await, Err(Error::Unsupported)));
}

/// A `rar+mem://` is the same refusal: `mem` is a test provider, not a
/// filesystem, and the dispatch arm does not look at who is registered.
#[tokio::test]
async fn rar_over_mem_is_refused_even_though_the_provider_is_registered() {
    let engine = engine_with_container("a.rar", b"Rar!\x1a\x07\x01\x00").await;
    assert!(matches!(
        engine.stat(&vp("rar+mem:///a.rar/!/x.txt")).await,
        Err(Error::Unsupported)
    ));
}

/// And over a local file it DOES compose: the engine lists what the delegate
/// reads. Without `7z` or `unrar` on the machine, the test bows out saying so.
#[tokio::test]
async fn rar_over_a_local_file_lists_for_real() {
    if norte_testkit::which_7z().is_none() {
        eprintln!("no 7z installed: test withdrawn");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = dir.path().join("a.rar");
    std::fs::write(
        &archive,
        norte_testkit::RarSmith::new()
            .file(b"docs/hello.txt", b"hello norte\n")
            .build(),
    )
    .expect("write");
    let engine = Engine::new();
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted("/")) as Arc<dyn Provider>
    );
    let root = VPath::archive_compose(
        "rar",
        &norte_vfs_local::vpath_from_native(&archive).expect("file's vpath"),
        &[],
    )
    .expect("compose");
    let entries: Vec<_> = engine
        .list(&root)
        .await
        .expect("list of the rar's root")
        .map(|e| e.expect("entry ok"))
        .collect()
        .await;
    assert_eq!(entries.len(), 1, "the `docs` directory");
    assert_eq!(entries[0].path.file_name().unwrap().as_bytes(), b"docs");
}
