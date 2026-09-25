//! Local-provider-specific tests the generic contract doesn't cover:
//! symlinks (never followed), multi-chunk chunking, partial cleanup,
//! capability probing.

use bytes::Bytes;
use futures::StreamExt;
// EntryKind is only used by the symlink tests, which are cfg(unix).
#[cfg(unix)]
use norte_proto::EntryKind;
use norte_proto::{CapabilityFlags, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn provider() -> (LocalProvider, VPath, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));
    (p, LocalProvider::root(), base)
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("valid segment"))
}

/// Counts staging files in `base` (name `.norte-partial.<hash>.…`: short
/// and unique, never derived from the final name — issue #4).
fn partials_with_prefix(base: &std::path::Path, prefix: &str) -> usize {
    std::fs::read_dir(base)
        .expect("read_dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

#[tokio::test]
async fn capabilities_are_probed() {
    let (p, root, _) = provider();
    // The probe is lazy: it runs on the first async operation (rule 2).
    let _ = p.stat(&root).await;
    let caps = p.capabilities();
    assert!(caps.flags.contains(CapabilityFlags::RENAME_ATOMIC));
    assert!(caps.flags.contains(CapabilityFlags::CASE_PRESERVING));
    // The probe decides CASE_SENSITIVE based on the tempdir's real FS: we
    // only require consistency with the OS default in CI (linux=yes, macos=no).
    if cfg!(target_os = "linux") {
        assert!(caps.flags.contains(CapabilityFlags::CASE_SENSITIVE));
    }
    if cfg!(windows) {
        assert_eq!(caps.max_path, Some(32767));
    }
}

/// Issue #5: a FOREIGN file matching the probe's name can't lie to the
/// case probe (in M0 a leftover `.norte-probe-cs-a` turned an ext4 into
/// "insensitive"). The probe uses a unique suffix and identity (dev,ino).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn probe_not_fooled_by_leftover_lowercase_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".norte-probe-cs-a"), b"from the user").unwrap();
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    // First async operation: triggers the real probe.
    let _ = p.stat(&LocalProvider::root()).await;
    assert!(
        p.capabilities()
            .flags
            .contains(CapabilityFlags::CASE_SENSITIVE),
        "an unrelated file with the same name can't turn an ext4 'insensitive'"
    );
    assert_eq!(
        std::fs::read(dir.path().join(".norte-probe-cs-a")).unwrap(),
        b"from the user",
        "the user's file stays intact"
    );
}

/// Issue #5: construction does NOT mutate the base directory — the probe
/// is lazy (it happens, at most, on the first `capabilities()`).
#[cfg(unix)]
#[tokio::test]
async fn construction_does_not_touch_base_dir() {
    let dir = tempfile::tempdir().unwrap();
    let before = std::fs::metadata(dir.path()).unwrap().modified().unwrap();
    let _p = LocalProvider::rooted(dir.path().to_path_buf());
    let after = std::fs::metadata(dir.path()).unwrap().modified().unwrap();
    assert_eq!(
        before, after,
        "constructing creates no probes and deletes none"
    );
}

#[tokio::test]
async fn read_streams_multiple_chunks() {
    let (p, root, _) = provider();
    let f = child(&root, b"big.bin");
    // 600 KiB > 2 chunks of 256 KiB.
    let content: Vec<u8> = (0..600_usize * 1024)
        .map(|i| u8::try_from(i % 251).expect("i % 251 < 256"))
        .collect();
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(content.clone())).await.unwrap();
    sink.commit().await.unwrap();

    let mut stream = p.read(&f, None).await.unwrap();
    let mut chunks = 0usize;
    let mut got = Vec::new();
    while let Some(item) = stream.next().await {
        got.extend_from_slice(&item.expect("chunk ok"));
        chunks += 1;
    }
    assert!(
        chunks >= 3,
        "600 KiB must arrive in ≥3 chunks, there were {chunks}"
    );
    assert_eq!(got, content, "identical bytes");
}

#[tokio::test]
async fn partial_file_cleaned_on_abort() {
    let (p, root, base) = provider();
    let f = child(&root, b"work");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"halfway")).await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        1,
        "the staging exists during the write"
    );
    sink.abort().await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "abort leaves no trace"
    );
    assert!(!base.join("work").exists());
}

#[tokio::test]
async fn partial_file_cleaned_on_drop() {
    let (p, root, base) = provider();
    let f = child(&root, b"dropped");
    {
        let mut sink = p.write(&f).await.unwrap();
        sink.write(Bytes::from_static(b"x")).await.unwrap();
        assert_eq!(partials_with_prefix(&base, ".norte-partial"), 1);
        // Dropped without commit: best-effort abort in Drop.
    }
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "Drop cleans up the staging"
    );
}

#[tokio::test]
async fn commit_renames_partial_to_final() {
    let (p, root, base) = provider();
    let f = child(&root, b"final");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"content")).await.unwrap();
    sink.commit().await.unwrap();
    assert_eq!(
        partials_with_prefix(&base, ".norte-partial"),
        0,
        "no staging after commit"
    );
    assert_eq!(std::fs::read(base.join("final")).unwrap(), b"content");
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_stat_never_follows() {
    let (p, root, base) = provider();
    std::fs::write(base.join("target"), b"real").unwrap();
    std::os::unix::fs::symlink(base.join("target"), base.join("link")).unwrap();

    let e = p.stat(&child(&root, b"link")).await.unwrap();
    assert_eq!(
        e.kind,
        EntryKind::Symlink,
        "describes the LINK, not the target"
    );

    // In list too.
    let kinds: Vec<(Vec<u8>, EntryKind)> = p
        .list(&root)
        .await
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.path.file_name().unwrap().as_bytes().to_vec(), e.kind)
        })
        .collect()
        .await;
    let link = kinds.iter().find(|(n, _)| n == b"link").expect("listed");
    assert_eq!(link.1, EntryKind::Symlink);
}

#[cfg(unix)]
#[tokio::test]
async fn remove_symlink_not_target() {
    let (p, root, base) = provider();
    std::fs::write(base.join("target"), b"real").unwrap();
    std::os::unix::fs::symlink(base.join("target"), base.join("link")).unwrap();
    p.remove(&child(&root, b"link")).await.unwrap();
    assert!(!base.join("link").exists(), "the link is gone");
    assert!(base.join("target").exists(), "the target stays intact");
}

/// A name at the `NAME_MAX` limit (corpus fixture `name_max_255`: 255
/// bytes, legal on ext4/APFS/NTFS): the staging can't derive from the
/// final name or it blows up with an opaque ENAMETOOLONG (issue #4).
/// Verified via the provider (the native path without verbatim would
/// exceed `MAX_PATH` on Windows).
#[tokio::test]
async fn write_commits_names_at_name_max() {
    let (p, root, _) = provider();
    let name = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "name_max_255")
        .expect("fixture in the corpus");
    let f = child(&root, &name.bytes);
    let mut sink = p.write(&f).await.expect("write opens despite NAME_MAX");
    sink.write(Bytes::from_static(b"fits")).await.unwrap();
    sink.commit().await.expect("commit publishes");
    let mut stream = p.read(&f, None).await.expect("read opens");
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        got.extend_from_slice(&chunk.expect("chunk ok"));
    }
    assert_eq!(got, b"fits");
}

#[tokio::test]
async fn mtime_is_recent_and_positive() {
    let (p, root, _) = provider();
    let f = child(&root, b"with-date");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.commit().await.unwrap();
    let e = p.stat(&f).await.unwrap();
    let mtime = e.mtime_ms.expect("the local FS always has an mtime");
    // After 2020-01-01 and before 2100: sanity, not exactness.
    assert!(mtime > 1_577_836_800_000, "suspicious mtime: {mtime}");
    assert!(mtime < 4_102_444_800_000, "suspicious mtime: {mtime}");
}

/// #52: listing doesn't stat (kind from `d_type`, size/mtime None);
/// `stat()` still brings the full metadata on demand.
#[tokio::test]
async fn list_is_lazy_and_stat_hydrates() {
    let (p, root, _) = provider();
    let f = child(&root, b"five");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from_static(b"12345")).await.unwrap();
    sink.commit().await.unwrap();

    let entries: Vec<norte_proto::Entry> = p
        .list(&root)
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    let e = entries
        .iter()
        .find(|e| e.path.file_name().unwrap().as_bytes() == b"five")
        .expect("listed");
    assert_eq!(e.kind, norte_proto::EntryKind::File);
    assert!(
        e.size.is_none() && e.mtime_ms.is_none(),
        "lazy listing (#52)"
    );

    let st = p.stat(&f).await.expect("stat");
    assert_eq!(st.size, Some(5));
    assert!(st.mtime_ms.is_some());
}

/// Encoding B2: list→stat round-trip over the hostile corpus. Lazy
/// hydration (#52) stats with the BYTES `list` returned, not the ones
/// requested when creating the file — on a normalizing FS (APFS/NFD)
/// those two differ, and a stat with the "original" bytes could fail or,
/// worse, succeed by accident without proving anything. For each corpus
/// name: if the OS accepts it, list the dir and stat EVERY path it
/// returned, expecting `size == Some(1)`.
#[tokio::test]
async fn list_lazy_stat_hydrates_corpus_names() {
    for name in norte_testkit::corpus::hostile_names() {
        let (p, root, _guard) = provider();
        let f = child(&root, &name.bytes);
        // Clean OS rejection of the name: skip (not what this test proves
        // — see prop_filename_bytes_survive_fs for that coverage).
        let Ok(mut sink) = p.write(&f).await else {
            continue;
        };
        sink.write(Bytes::from_static(b"1")).await.expect("chunk");
        match sink.commit().await {
            Ok(()) => {}
            Err(norte_proto::Error::InvalidPath | norte_proto::Error::Conflict { .. }) => {
                continue;
            }
            Err(e) => panic!("{}: unexpected commit: {e:?}", name.id),
        }

        let listed: Vec<norte_proto::VPath> = p
            .list(&root)
            .await
            .unwrap_or_else(|e| panic!("{}: list: {e:?}", name.id))
            .map(|r| r.unwrap_or_else(|e| panic!("{}: entry: {e:?}", name.id)))
            .map(|e| e.path)
            .collect()
            .await;
        assert!(
            !listed.is_empty(),
            "{}: the listing must see the freshly written file",
            name.id
        );
        for path in &listed {
            let st = p
                .stat(path)
                .await
                .unwrap_or_else(|e| panic!("{}: stat of {path:?}: {e:?}", name.id));
            assert_eq!(
                st.size,
                Some(1),
                "{}: stat of a LISTED path must hydrate the real size",
                name.id
            );
        }
    }
}

/// Case-rename (`box` → `BOX`) on a case-insensitive FS: the "destination"
/// is the source itself under a different case and must proceed (issue
/// #2: on Windows M0 returned `Conflict` for not being able to check the
/// file's real identity).
#[cfg(any(windows, target_os = "macos"))]
#[tokio::test]
async fn case_rename_succeeds_on_insensitive_fs() {
    let (p, root, base) = provider();
    if p.capabilities()
        .flags
        .contains(CapabilityFlags::CASE_SENSITIVE)
    {
        // The tempdir lives on a case-sensitive FS (possible on macOS):
        // the case-collision contract test covers this case.
        return;
    }
    std::fs::write(base.join("box"), b"x").unwrap();
    p.rename(&child(&root, b"box"), &child(&root, b"BOX"))
        .await
        .expect("file case-rename allowed");
    // Also for DIRECTORIES (a dir's nlink is never 1: the hardlink guard
    // can't block it).
    std::fs::create_dir(base.join("folder")).unwrap();
    p.rename(&child(&root, b"folder"), &child(&root, b"FOLDER"))
        .await
        .expect("dir case-rename allowed");
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["BOX".to_owned(), "FOLDER".to_owned()],
        "two dirents, new case preserved"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rename_between_hardlinks_is_conflict() {
    // rename(2) between two hardlinks of the same inode is a successful
    // no-op: reporting it as a move would lie to the journal. It must be
    // Conflict.
    let (p, root, base) = provider();
    std::fs::write(base.join("a"), b"x").unwrap();
    std::fs::hard_link(base.join("a"), base.join("b")).unwrap();
    match p.rename(&child(&root, b"a"), &child(&root, b"b")).await {
        Err(norte_proto::Error::Conflict { .. }) => {}
        other => panic!("expected Conflict, was {other:?}"),
    }
    assert!(base.join("a").exists(), "source intact");
    assert!(base.join("b").exists(), "destination intact");
}

// ---------- issue #10: dropping a stream releases the producer's fd ----------

/// Number of fds the process has open (includes `read_dir`'s own:
/// constant between calls, valid for comparison).
#[cfg(target_os = "linux")]
fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd")
        .count()
}

/// Waits (with a deadline) for the blocking producer to notice the closed
/// channel and release its resources.
#[cfg(target_os = "linux")]
async fn wait_fds_back_to(baseline: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let now = open_fds();
        if now <= baseline || std::time::Instant::now() > deadline {
            return now;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Dropping a `ByteStream` mid-read must release the file's fd (issue
/// #10): the producer notices the closed channel on the next send.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_byte_stream_mid_read_releases_fd() {
    let (p, root, _) = provider();
    let f = child(&root, b"big.bin");
    // 8 MiB = 32 chunks: much more than the channel's buffer (8) — the
    // producer stays BLOCKED with the file open when the stream is dropped.
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(vec![0x5A; 8 * 1024 * 1024]))
        .await
        .unwrap();
    sink.commit().await.unwrap();

    let baseline = open_fds();
    let mut stream = p.read(&f, None).await.unwrap();
    let first = stream
        .next()
        .await
        .expect("there is data")
        .expect("chunk ok");
    assert!(!first.is_empty());
    assert!(
        open_fds() > baseline,
        "sanity: the producer has the file open"
    );
    drop(stream);
    let now = wait_fds_back_to(baseline).await;
    assert!(
        now <= baseline,
        "producer's fd leaked after dropping the ByteStream: {now} > {baseline}"
    );
}

/// Dropping an `EntryStream` mid-listing must release the producer's
/// `read_dir` fd (issue #10).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_entry_stream_mid_list_releases_fd() {
    let (p, root, base) = provider();
    // More entries (200) than the channel's buffer (64): producer blocked.
    for i in 0..200 {
        std::fs::write(base.join(format!("f{i:03}")), b"x").unwrap();
    }
    let baseline = open_fds();
    let mut stream = p.list(&root).await.unwrap();
    let first = stream.next().await.expect("there are entries");
    assert!(first.is_ok());
    drop(stream);
    let now = wait_fds_back_to(baseline).await;
    assert!(
        now <= baseline,
        "producer's fd leaked after dropping the EntryStream: {now} > {baseline}"
    );
}

/// Windows variant of issue #10: if the producer leaked its handle, the
/// tree deletion would never finish (delete-pending file → non-empty dir).
#[cfg(windows)]
#[tokio::test]
async fn dropping_byte_stream_mid_read_releases_handle() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let p = LocalProvider::rooted(base.clone());
    let f = child(&LocalProvider::root(), b"big.bin");
    let mut sink = p.write(&f).await.unwrap();
    sink.write(Bytes::from(vec![0x5A; 8 * 1024 * 1024]))
        .await
        .unwrap();
    sink.commit().await.unwrap();

    let mut stream = p.read(&f, None).await.unwrap();
    let _ = stream
        .next()
        .await
        .expect("there is data")
        .expect("chunk ok");
    drop(stream);
    drop(p);

    // The deletion only completes once the producer releases the handle.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match std::fs::remove_dir_all(&base) {
            Ok(()) => break,
            Err(e) if std::time::Instant::now() > deadline => {
                panic!("producer's handle leaked: {e}");
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
}

/// Purges the REAL trash after the suite (phase 8 finding M3): the
/// contract trashed `norte-contract-trash-<pid>` on every run — without
/// this, the developer's trash grew forever. macOS has no `os_limited`:
/// accepted and documented in ADR 0009.
///
/// Since task 11b the local contract carries its trash inside the tempdir
/// (`with_trash_home`), so it no longer litters anything; this stays to
/// sweep what earlier runs left behind, and because the
/// `restore_trashed` tests below DO use the real trash (that's what
/// they're testing: that the `trash` crate can read what we write).
#[cfg(any(target_os = "linux", windows))]
#[test]
fn purges_the_contracts_leftovers_from_the_trash() {
    let Ok(items) = trash::os_limited::list() else {
        return; // no queryable trash: nothing to purge
    };
    let ours: Vec<_> = items
        .into_iter()
        .filter(|i| {
            i.name
                .to_string_lossy()
                .starts_with("norte-contract-trash-")
        })
        .collect();
    if !ours.is_empty() {
        let _ = trash::os_limited::purge_all(ours);
    }
}

/// Rule 3 via `read`: a FIFO (or a symlink to one) never hangs the thread
/// — `read` rejects it with `Unsupported` BEFORE the open (opening a FIFO
/// with no writer blocks forever and cancellation can't interrupt it).
#[cfg(unix)]
#[tokio::test]
async fn read_of_a_fifo_does_not_hang() {
    use norte_proto::Error;
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join("pipe");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // Test SAFETY: a valid, NUL-terminated CString; mkfifo doesn't retain the pointer.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo");
    std::os::unix::fs::symlink("pipe", dir.path().join("lpipe")).unwrap();

    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();
    let deadline = std::time::Duration::from_secs(5);
    for name in [&b"pipe"[..], &b"lpipe"[..]] {
        let path = root.join(seg(name));
        let res = tokio::time::timeout(deadline, p.read(&path, None))
            .await
            .expect("read answers, never hangs");
        let err = res.err().expect("non-regular honestly rejected");
        assert_eq!(err, Error::Unsupported);
    }
}

/// Real resume over the FS (ADR 0012): keep preserves the `.norte-partial`
/// with a STABLE name, `open_resumable` finds it again and resumes; the GC
/// sweeps orphans by age.
#[tokio::test]
async fn local_resume_keeps_resumes_and_gcs() {
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();
    let f = root.join(seg(b"big.bin"));

    // First stage: 4 bytes, keep (preserves the partial, doesn't publish).
    let (mut sink, already) = p.open_resumable(&f).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hi")).await.unwrap();
    sink.keep().await.expect("keep");
    assert_eq!(
        p.stat(&f).await.unwrap_err(),
        norte_proto::Error::NotFound,
        "keep does not publish"
    );
    // There's ONE .norte-partial on disk (stable name).
    let partials: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with(".norte-partial.")
        })
        .collect();
    assert_eq!(partials.len(), 1, "one stable partial kept");

    // Second stage: resumes from the 2 bytes.
    let (mut sink, already) = p.open_resumable(&f).await.expect("open 2");
    assert_eq!(already, 2, "resumes after what was kept");
    sink.write(Bytes::from_static(b"world")).await.unwrap();
    sink.commit().await.expect("commit");
    assert_eq!(
        std::fs::read(dir.path().join("big.bin")).unwrap(),
        b"hiworld"
    );
    // The partial disappeared on commit.
    let left = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|d| {
            d.file_name()
                .to_string_lossy()
                .starts_with(".norte-partial.")
        })
        .count();
    assert_eq!(left, 0, "commit publishes and cleans up the partial");

    // GC of an orphan: keep another partial and sweep it with older_than=0.
    let g = root.join(seg(b"other.bin"));
    let (mut sink, _) = p.open_resumable(&g).await.expect("open g");
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.keep().await.expect("keep g");
    let removed = p
        .gc_partials(&root, std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 1, "the orphan gets swept");
}

/// encoding-auditor H2: `gc_partials` recognizes staging by its exact
/// SHAPE, not by the prefix — a REAL user file starting with
/// `.norte-partial.` is NEVER deleted.
#[tokio::test]
async fn gc_partials_does_not_touch_user_files() {
    use norte_vfs::Provider;
    let dir = tempfile::tempdir().expect("tempdir");
    let p = norte_vfs_local::LocalProvider::rooted(dir.path());
    let root = norte_vfs_local::LocalProvider::root();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).unwrap();

    // User file with the prefix but NOT the shape of a staging.
    std::fs::write(dir.path().join(".norte-partial.backup"), b"mine").unwrap();
    std::fs::write(dir.path().join(".norte-partial.notes.txt"), b"mine").unwrap();
    // A real partial (stable shape: 32 hex).
    let g = root.join(seg(b"big.bin"));
    let (mut sink, _) = p.open_resumable(&g).await.expect("open");
    sink.write(Bytes::from_static(b"x")).await.unwrap();
    sink.keep().await.expect("keep");

    let removed = p
        .gc_partials(&root, std::time::Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(removed, 1, "only the real partial gets swept");
    assert!(
        dir.path().join(".norte-partial.backup").exists(),
        "the user's file survives"
    );
    assert!(dir.path().join(".norte-partial.notes.txt").exists());
}

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_brings_back_by_original_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("v.txt"), b"data").expect("seed");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let victim = child(&LocalProvider::root(), b"v.txt");

    // If the OS trash isn't available on the runner, clean skip.
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: OS trash not available");
        return;
    }
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    p.restore_trashed(&victim).await.expect("restore");
    assert_eq!(
        std::fs::read(dir.path().join("v.txt")).expect("restored"),
        b"data"
    );
}

/// Rule 1: matching by original path and restoring preserve hostile bytes
/// (a non-UTF8 name) — byte-exact round-trip through the real trash.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_preserves_hostile_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let name: &[u8] = b"h\xffstile.bin"; // 0xFF: invalid in any UTF-8
    let dir = tempfile::tempdir().expect("tempdir");
    let native = dir.path().join(std::ffi::OsStr::from_bytes(name));
    std::fs::write(&native, b"payload").expect("seed");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let victim = child(&LocalProvider::root(), name);
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: OS trash not available");
        return;
    }
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));
    p.restore_trashed(&victim).await.expect("restore");
    assert_eq!(
        std::fs::read(&native).expect("byte-exact restore"),
        b"payload"
    );
}

/// Strict: no item in the trash → `NotFound`; an occupied destination →
/// `Conflict` (never overwrites — undo's security invariant).
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
#[tokio::test]
async fn restore_trashed_is_strict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = LocalProvider::rooted(dir.path().to_path_buf());
    let ghost = child(&LocalProvider::root(), b"never_deleted.bin");
    // No matching item in the trash → NotFound (or Unsupported if the
    // runner has no trash; both are clean failures, never overwrites).
    assert!(matches!(
        p.restore_trashed(&ghost).await,
        Err(norte_proto::Error::NotFound | norte_proto::Error::Unsupported)
    ));

    // Occupied destination: trash it, recreate something in its place, restore → Conflict.
    std::fs::write(dir.path().join("v.txt"), b"original").expect("seed");
    let victim = child(&LocalProvider::root(), b"v.txt");
    if p.trash(
        &victim,
        &norte_vfs::trash::TrashId::new(0, u64::from(line!())),
    )
    .await
    .is_err()
    {
        eprintln!("skip: OS trash not available");
        return;
    }
    std::fs::write(dir.path().join("v.txt"), b"new").expect("recreate");
    assert!(matches!(
        p.restore_trashed(&victim).await,
        Err(norte_proto::Error::Conflict { .. })
    ));
    // Didn't overwrite: the new content is still intact.
    assert_eq!(std::fs::read(dir.path().join("v.txt")).unwrap(), b"new");
}

// ---------- posix attrs (#108 block 2) ----------

#[cfg(unix)]
#[tokio::test]
async fn attrs_posix_in_stat_and_list() {
    use norte_proto::AttrValue;
    use norte_vfs::{AttrRequest, ListOptions};
    use std::os::unix::fs::MetadataExt;
    let (p, root, base) = provider();
    std::fs::write(base.join("a.txt"), b"hi").expect("seed");
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(
            [
                "posix.mode",
                "posix.uid",
                "posix.gid",
                "posix.nlink",
                "posix.ctime_ms",
            ]
            .map(str::to_owned),
        ),
    };
    let e = p
        .stat_with(&child(&root, b"a.txt"), &opt)
        .await
        .expect("stat_with");
    let md = std::fs::symlink_metadata(base.join("a.txt")).expect("md");
    assert_eq!(
        e.attrs.get("posix.mode"),
        Some(&AttrValue::Uint(u64::from(md.mode())))
    );
    assert_eq!(
        e.attrs.get("posix.uid"),
        Some(&AttrValue::Uint(u64::from(md.uid())))
    );
    assert_eq!(
        e.attrs.get("posix.gid"),
        Some(&AttrValue::Uint(u64::from(md.gid())))
    );
    assert_eq!(
        e.attrs.get("posix.nlink"),
        Some(&AttrValue::Uint(md.nlink()))
    );
    assert!(matches!(
        e.attrs.get("posix.ctime_ms"),
        Some(AttrValue::TimeMs(_))
    ));

    // list_with promotes: attrs present AND size/mtime hydrated along the way.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    let le = s.next().await.expect("one entry").expect("ok");
    assert!(le.attrs.contains_key("posix.mode"));
    assert!(le.size.is_some(), "promotion to metadata fills size");

    // Fast path intact (#52): no request, lazy as always.
    let mut s = p.list(&root).await.expect("list");
    let le = s.next().await.expect("one entry").expect("ok");
    assert!(le.attrs.is_empty() && le.size.is_none());

    // Request with NO locally advertised attr: also the lazy path.
    let foreign = ListOptions {
        attrs: AttrRequest::sanitized(["s3.etag".to_owned()]),
    };
    let mut s = p.list_with(&root, &foreign).await.expect("list_with");
    let le = s.next().await.expect("one entry").expect("ok");
    assert!(le.attrs.is_empty() && le.size.is_none());
}

/// ADR 0145: owner and group BY NAME, in bytes, through both paths (`stat`
/// and `list`'s promotion). Which name it is depends on the machine; what
/// gets fixed is that it arrives, with no C NUL, and that it's the same
/// through both paths.
#[cfg(unix)]
#[tokio::test]
async fn attrs_posix_owner_and_group_by_name() {
    use norte_proto::AttrValue;
    use norte_vfs::{AttrRequest, ListOptions};
    let (p, root, base) = provider();
    std::fs::write(base.join("a.txt"), b"hi").expect("seed");
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["posix.owner", "posix.group"].map(str::to_owned)),
    };
    let e = p
        .stat_with(&child(&root, b"a.txt"), &opt)
        .await
        .expect("stat_with");
    for id in ["posix.owner", "posix.group"] {
        match e.attrs.get(id) {
            Some(AttrValue::Bytes(n)) => {
                assert!(!n.is_empty() && !n.contains(&0), "{id}: {n:?}");
            }
            other => panic!("{id} by name in bytes: {other:?}"),
        }
    }
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    let le = s.next().await.expect("one entry").expect("ok");
    assert_eq!(le.attrs.get("posix.owner"), e.attrs.get("posix.owner"));
    assert_eq!(le.attrs.get("posix.group"), e.attrs.get("posix.group"));
}

// ---------------------------------------------------------------------------
// freedesktop trash (`trash_fdo`): the one that KNOWS where it left the file.
//
// All these tests inject their own XDG root under the tempdir: they neither
// touch the developer's real trash nor depend on which device it lives on.
// ---------------------------------------------------------------------------

#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "ios"),
    not(target_os = "android")
))]
mod freedesktop_trash {
    use super::{LocalProvider, Provider, Segment, VPath, child};
    use std::os::unix::ffi::OsStrExt;

    /// Provider rooted in a tempdir with its trash INSIDE: this way the
    /// recoverable destination is a path this same provider knows how to
    /// resolve.
    fn provider() -> (tempfile::TempDir, LocalProvider) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = LocalProvider::rooted(dir.path()).with_trash_home(dir.path().join(".xdg"));
        (dir, p)
    }

    /// A distinct engine id per operation. The instant is what matters —
    /// it's what travels to the sidecar, with SECOND resolution —, so the
    /// test ids are separated by whole seconds.
    fn id(n: u64) -> norte_vfs::trash::TrashId {
        norte_vfs::trash::TrashId::new(1_726_000_000_000 + n * 1000, n)
    }

    /// The trash's root on the test's real disk.
    fn trash_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join(".xdg").join("Trash")
    }

    /// Seeds a file with arbitrary name BYTES (rule 1: never goes through `str`).
    fn seed(dir: &tempfile::TempDir, name: &[u8], body: &[u8]) -> VPath {
        let native = dir.path().join(std::ffi::OsStr::from_bytes(name));
        std::fs::write(&native, body).expect("seed");
        child(&LocalProvider::root(), name)
    }

    fn native_of(dir: &tempfile::TempDir, p: &VPath) -> std::path::PathBuf {
        let mut out = dir.path().to_path_buf();
        for seg in p.segments() {
            out.push(std::ffi::OsStr::from_bytes(seg));
        }
        out
    }

    /// The sidecar that belongs to a `…/files/<n>` destination.
    fn sidecar_of(dir: &tempfile::TempDir, dest: &VPath) -> std::path::PathBuf {
        let native = native_of(dir, dest);
        let name = native.file_name().expect("name");
        let mut file = name.as_bytes().to_vec();
        file.extend_from_slice(b".trashinfo");
        trash_root(dir)
            .join("info")
            .join(std::ffi::OsStr::from_bytes(&file))
    }

    #[tokio::test]
    async fn trashing_returns_the_path_it_actually_used() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"data");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("freedesktop names its destination");
        assert!(
            dest.to_wire().contains("/Trash/files/"),
            "{}",
            dest.to_wire()
        );
        p.stat(&dest).await.expect("the file IS there");
        assert_eq!(
            std::fs::read(native_of(&dir, &dest)).expect("bytes"),
            b"data"
        );
        assert!(p.trash_restorable(), "and the provider promises it");
    }

    /// The bug in a test: without distinct destinations, undoing a
    /// `trashed`+`created` pair digs up its own burial.
    #[tokio::test]
    async fn two_victims_with_one_name_get_two_destinations() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"first");
        let first = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        let victim = seed(&dir, b"a.txt", b"second");
        let second = p
            .trash(&victim, &id(2))
            .await
            .expect("trash")
            .expect("dest");

        assert_ne!(first.to_wire(), second.to_wire());
        assert_eq!(std::fs::read(native_of(&dir, &first)).expect("1"), b"first");
        assert_eq!(
            std::fs::read(native_of(&dir, &second)).expect("2"),
            b"second"
        );
        // The deduplication the spec describes.
        assert!(
            second.to_wire().ends_with("a.txt.2"),
            "{}",
            second.to_wire()
        );
    }

    #[tokio::test]
    async fn the_trashinfo_sidecar_is_written_and_names_the_original() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");

        let bytes = std::fs::read(sidecar_of(&dir, &dest)).expect("sidecar");
        let text = String::from_utf8(bytes).expect("the trashinfo is UTF-8 by spec");
        assert!(text.starts_with("[Trash Info]\n"), "{text}");
        let expected = dir.path().canonicalize().expect("canon").join("a.txt");
        assert!(
            text.contains(&format!("Path={}\n", expected.display())),
            "{text}"
        );
        assert!(text.contains("DeletionDate="), "{text}");
    }

    /// Rule 1. The sidecar percent-encodes; the FILE keeps its bytes.
    /// What this test really checks — and what the first version did NOT
    /// check (encoding-auditor MINOR-1) — is that the sidecar's path,
    /// DECODED, is byte-for-byte the original path: `restore_from`
    /// doesn't read the sidecar, so without this assertion a systematic
    /// escaping failure would pass green and only a graphical trash would
    /// ever notice.
    #[tokio::test]
    async fn a_hostile_name_survives_the_round_trip() {
        let (dir, p) = provider();
        let root_dir = dir.path().canonicalize().expect("canon");
        let mut tested = 0usize;
        for (n, name) in norte_testkit::corpus::hostile_names()
            .into_iter()
            .enumerate()
        {
            let native = dir.path().join(std::ffi::OsStr::from_bytes(&name.bytes));
            // A name this FS doesn't accept simply isn't there (APFS/NTFS).
            // The payload is DIFFERENT per fixture: with a common one,
            // returning another entry's destination would go unnoticed.
            if std::fs::write(&native, name.id.as_bytes()).is_err() {
                continue;
            }
            let Ok(victim) =
                Segment::new(name.bytes.clone()).map(|s| LocalProvider::root().join(s))
            else {
                continue;
            };
            tested += 1;
            let dest = p
                .trash(&victim, &id(1000 + n as u64))
                .await
                .expect("trash")
                .expect("dest");

            // The sidecar is pure ASCII even when the name is neither, and
            // it's exactly three lines: a `\n` in a name can't inject a
            // fourth line nor a second `Path=`.
            let text = std::fs::read_to_string(sidecar_of(&dir, &dest)).expect("sidecar");
            assert!(text.is_ascii(), "{}: {text}", name.id);
            assert_eq!(text.lines().count(), 3, "{}: {text}", name.id);
            assert_eq!(text.lines().next(), Some("[Trash Info]"), "{}", name.id);
            assert_eq!(
                text.lines().filter(|l| l.starts_with("Path=")).count(),
                1,
                "{}: {text}",
                name.id
            );

            // And the path it stores is, decoded, the original BYTE FOR BYTE.
            let encoded = text
                .lines()
                .find_map(|l| l.strip_prefix("Path="))
                .expect("Path=");
            assert_eq!(
                percent_decode(encoded),
                root_dir
                    .join(std::ffi::OsStr::from_bytes(&name.bytes))
                    .as_os_str()
                    .as_bytes(),
                "{}",
                name.id
            );

            // And it goes back to ITS path, with ITS bytes.
            p.restore_from(&dest, &victim).await.expect("restore_from");
            assert_eq!(
                std::fs::read(&native).expect("back"),
                name.id.as_bytes(),
                "{}",
                name.id
            );
            std::fs::remove_file(&native).expect("cleans up");
        }
        assert!(
            tested >= 40,
            "the corpus was almost entirely skipped: {tested}"
        );
    }

    /// Decodes a `.trashinfo`'s `Path=` to BYTES (never to `String`: the
    /// original path may not be UTF-8).
    fn percent_decode(s: &str) -> Vec<u8> {
        let raw = s.as_bytes();
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'%' && i + 2 < raw.len() {
                let hex = std::str::from_utf8(&raw[i + 1..i + 3]).expect("ascii");
                out.push(u8::from_str_radix(hex, 16).expect("valid hex"));
                i += 3;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        out
    }

    #[tokio::test]
    async fn restoring_from_the_recorded_destination_needs_no_guessing() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        let sidecar = sidecar_of(&dir, &dest);
        assert!(sidecar.exists(), "the sidecar was there");

        p.restore_from(&dest, &victim).await.expect("restore");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).expect("back"), b"x");
        assert!(!sidecar.exists(), "the sidecar goes with it");
        assert!(
            p.stat(&dest).await.is_err(),
            "and the payload is no longer there"
        );
    }

    /// #99: the engine generates the `id`, and a retry after a transient
    /// failure has to CONVERGE on the same entry, not create a second one
    /// nor lose the `reversal_ref`.
    #[tokio::test]
    async fn a_retry_with_the_same_id_converges_on_the_same_entry() {
        let (dir, p) = provider();
        let victim = seed(&dir, b"a.txt", b"x");
        let first = p
            .trash(&victim, &id(7))
            .await
            .expect("trash")
            .expect("dest");
        // The victim is no longer there: the retry recognizes its own entry.
        let again = p
            .trash(&victim, &id(7))
            .await
            .expect("the retry converges")
            .expect("and keeps the destination");
        assert_eq!(first.to_wire(), again.to_wire());
        // And it hasn't created a second entry.
        let n = std::fs::read_dir(trash_root(&dir).join("files"))
            .expect("files")
            .count();
        assert_eq!(n, 1, "a single entry");
    }

    /// With no earlier entry of ours, a missing victim is `NotFound` — the
    /// entry of ANOTHER operation on the same path isn't claimed.
    #[tokio::test]
    async fn a_missing_victim_without_our_entry_is_not_found() {
        let (dir, p) = provider();
        let ghost = child(&LocalProvider::root(), b"never.txt");
        assert_eq!(
            p.trash(&ghost, &id(1)).await,
            Err(norte_proto::Error::NotFound)
        );
        // And a FOREIGN entry with the same name isn't claimed either.
        let victim = seed(&dir, b"a.txt", b"from the neighbor");
        p.trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert_eq!(
            p.trash(&victim, &id(2)).await,
            Err(norte_proto::Error::NotFound),
            "another operation inherits nobody's entry"
        );
    }

    /// A sidecar PLANTED in `info/` is neither overwritten nor claimed:
    /// the victim goes to the next free name.
    #[tokio::test]
    async fn a_planted_sidecar_is_never_overwritten() {
        let (dir, p) = provider();
        let info = trash_root(&dir).join("info");
        std::fs::create_dir_all(&info).expect("info");
        std::fs::write(info.join("a.txt.trashinfo"), b"[Trash Info]\nPath=/other\n")
            .expect("plant");

        let victim = seed(&dir, b"a.txt", b"mine");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert!(dest.to_wire().ends_with("a.txt.2"), "{}", dest.to_wire());
        assert_eq!(
            std::fs::read(info.join("a.txt.trashinfo")).expect("intact"),
            b"[Trash Info]\nPath=/other\n"
        );
    }

    /// A PLANTED `files/<n>` (here a symlink to something valuable) isn't
    /// overwritten either: the move is no-replace and the victim goes to
    /// the next name.
    #[tokio::test]
    async fn a_planted_payload_is_never_clobbered() {
        let (dir, p) = provider();
        let files = trash_root(&dir).join("files");
        std::fs::create_dir_all(&files).expect("files");
        let valuable = dir.path().join("valuable.txt");
        std::fs::write(&valuable, b"do not touch me").expect("seed");
        std::os::unix::fs::symlink(&valuable, files.join("a.txt")).expect("plant");

        let victim = seed(&dir, b"a.txt", b"mine");
        let dest = p
            .trash(&victim, &id(1))
            .await
            .expect("trash")
            .expect("dest");
        assert!(dest.to_wire().ends_with("a.txt.2"), "{}", dest.to_wire());
        assert_eq!(
            std::fs::read(&valuable).expect("intact"),
            b"do not touch me",
            "the planted symlink was neither followed nor overwritten"
        );
    }
}

// ---------- per-DIRECTORY capabilities (ADR 0054, #153/#145) ----------

/// `capabilities_at` NEVER writes, on any filesystem: it's answered behind
/// the READ gate, so a write probe there would be a file created by an
/// actor who only has permission to look. This test's tempdir is on
/// tmpfs, which the read-only ladder does NOT recognize — i.e. it's
/// exactly the case that used to fall to the write probe.
#[tokio::test]
async fn capabilities_at_never_writes_anywhere() {
    let (p, root, base) = provider();
    let sub = base.join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    let vsub = child(&root, b"sub");

    let _ = p.capabilities_at(&vsub).await.expect("answers");

    let leftover: Vec<_> = std::fs::read_dir(&sub)
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "neither during nor after: the ladder mutates nothing ({leftover:?})"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_directory_without_write_permission_still_gets_an_answer() {
    // A read-only mount can't be manufactured in CI; a directory without
    // write permission can, and it's what used to break the WRITE probe:
    // it failed and didn't distinguish "not writable" from "doesn't fold".
    use std::os::unix::fs::PermissionsExt;
    let (p, root, base) = provider();
    let ro = base.join("ro");
    std::fs::create_dir(&ro).expect("mkdir");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod");

    let caps = p
        .capabilities_at(&child(&root, b"ro"))
        .await
        .expect("answers all the same");

    // What it answers depends on CI's FS; what's asserted is that it
    // ANSWERS the same as the root, which is on the same filesystem —
    // and without having been able to write into the directory to find out.
    assert_eq!(
        caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        p.capabilities_at(&root)
            .await
            .expect("answers")
            .flags
            .contains(CapabilityFlags::CASE_SENSITIVE)
    );
}

#[tokio::test]
async fn two_paths_to_one_directory_probe_once() {
    let (p, root, base) = provider();
    std::fs::create_dir(base.join("sub")).expect("mkdir");
    let direct = child(&root, b"sub");

    let before = p.caps_at_probe_count();
    let _ = p.capabilities_at(&direct).await.expect("answers");
    let _ = p.capabilities_at(&direct).await.expect("answers");
    assert_eq!(
        p.caps_at_probe_count() - before,
        1,
        "the key is (dev, ino): the second question comes from the cache"
    );
}

#[tokio::test]
async fn a_file_is_answered_by_its_containing_directory() {
    let (p, root, base) = provider();
    std::fs::write(base.join("f.txt"), b"x").expect("write");

    let for_file = p
        .capabilities_at(&child(&root, b"f.txt"))
        .await
        .expect("answers");
    let for_dir = p.capabilities_at(&root).await.expect("answers");

    assert_eq!(
        for_file, for_dir,
        "the question is always about the directory that contains it"
    );
}

/// A path that isn't there is NOT an error: `capabilities()` could never
/// fail, and making its per-location version fail would break the common
/// case of planning toward a destination that doesn't exist yet.
///
/// What DOES take that degraded path is `CONFINED_WRITES`, and it isn't a
/// whimsical exception: confinement is a PLATFORM property — there's
/// `openat` or there isn't —, not one of the tree or of whether the path
/// exists yet. Without this, `file:///destination-that-does-not-exist`
/// would answer "I can't confine" and `file:///` would answer yes, two
/// different answers from the same machine — and the first is exactly
/// what a mirror sees while planning (W5 B's security review).
#[tokio::test]
async fn a_missing_path_answers_the_declaration() {
    let (p, root, _) = provider();
    let mut expected = p.capabilities();
    expected
        .flags
        .set(norte_proto::CapabilityFlags::CONFINED_WRITES, cfg!(unix));
    assert_eq!(
        p.capabilities_at(&child(&root, b"does-not-exist"))
            .await
            .expect("answers all the same"),
        expected
    );
}

/// The case that DISCRIMINATES the ladder: a directory without write
/// permission **on ext4**. The write probe can't answer there — that's
/// exactly its limit —, so a correct answer can only come from the
/// read-only step (`statfs` + `FS_IOC_GETFLAGS`).
///
/// Rooted in the repo's own tree and not in `/tmp`, which on this machine
/// is tmpfs: over tmpfs the ladder falls to the write step and the test
/// wouldn't prove anything.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_read_only_ext4_directory_is_answered_without_writing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("tempdir in the repo");
    let base = dir.path().to_path_buf();
    let ro = base.join("ro");
    std::fs::create_dir(&ro).expect("mkdir");
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let p = LocalProvider::rooted(base.clone()).with_guard(Box::new(dir));

    let caps = p
        .capabilities_at(&child(&LocalProvider::root(), b"ro"))
        .await
        .expect("answers");

    // If the repo's tree isn't on ext4/f2fs (a container with overlayfs,
    // for instance), the ladder falls to the write step, which can't
    // answer for this directory — and then the test doesn't apply.
    let on_ext4 = std::process::Command::new("stat")
        .args(["-f", "-c", "%T", base.to_str().expect("test path is ASCII")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if on_ext4.as_deref() != Some("ext2/ext3") {
        eprintln!("skip: the repo's tree isn't on ext4 ({on_ext4:?})");
        return;
    }

    assert!(
        caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        "ext4 without +F distinguishes case, and here it couldn't write to find out"
    );
    assert!(
        !caps.flags.contains(CapabilityFlags::FULL_FOLD),
        "and without +F it doesn't expand"
    );
}

/// Roadmap item 8: a file with a hole gets copied WITHOUT materializing it.
///
/// The two assertions say different things and both are needed: the bytes
/// are identical (which is correctness) and the BLOCKS aren't (which is
/// the only thing that proves the optimization happened). Without the
/// second, the test would pass just the same with the earlier
/// implementation.
#[cfg(unix)]
#[tokio::test]
async fn a_destination_with_a_hole_is_not_materialized() {
    use std::os::unix::fs::MetadataExt as _;

    // 64 MiB hole: the VM-image case the roadmap names.
    const HOLE: u64 = 64 * 1024 * 1024;

    let (p, root, base) = provider();
    let f = std::fs::File::create(base.join("source.img")).expect("create");
    f.set_len(HOLE).expect("hole");
    drop(f);

    let mut sink = p.write(&child(&root, b"target.img")).await.expect("write");
    let mut read_stream = p
        .read(&child(&root, b"source.img"), None)
        .await
        .expect("read");
    while let Some(chunk) = read_stream.next().await {
        sink.write(chunk.expect("chunk")).await.expect("writes");
    }
    sink.commit().await.expect("commit");

    let md = std::fs::metadata(base.join("target.img")).expect("stat");
    assert_eq!(md.len(), HOLE, "the LOGICAL size is kept whole");
    assert_eq!(
        std::fs::read(base.join("target.img")).expect("read"),
        vec![0u8; usize::try_from(HOLE).expect("fits")],
        "and the bytes read back are the same"
    );
    assert!(
        md.blocks() * 512 < HOLE / 8,
        "but the disk doesn't store them: {} blocks for {HOLE} bytes",
        md.blocks()
    );
}

/// The case the optimization must NOT break: zeros someone wrote on
/// purpose, in the middle of data. The destination coming out sparse is
/// allowed; a byte changing is not.
#[tokio::test]
async fn zeros_in_the_middle_read_back_the_same() {
    let (p, root, _base) = provider();
    let mut content = vec![b'a'; 1024];
    content.extend(std::iter::repeat_n(0u8, 256 * 1024));
    content.extend(std::iter::repeat_n(b'z', 1024));

    let mut sink = p.write(&child(&root, b"mixed.bin")).await.expect("write");
    sink.write(Bytes::from(content.clone()))
        .await
        .expect("writes");
    sink.commit().await.expect("commit");

    let mut read_stream = p
        .read(&child(&root, b"mixed.bin"), None)
        .await
        .expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = read_stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(out, content, "byte for byte, no exceptions");
}

/// And resume still knows where it was: the hole has to count in the
/// staging's LENGTH from the moment it's written, not from the commit —
/// that's what `open_resumable` (its `already`) and `partial_digest`
/// read. With the length deferred, a partial ending in a hole would claim
/// fewer bytes than it has and the retry would write over what was
/// already done.
#[tokio::test]
async fn a_hole_counts_in_the_resume_offset() {
    let (p, root, _base) = provider();
    let dest = child(&root, b"resume.bin");

    let (mut sink, already) = p.open_resumable(&dest).await.expect("opens");
    assert_eq!(already, 0, "fresh staging");
    sink.write(Bytes::from(vec![0u8; 128 * 1024]))
        .await
        .expect("all zeros");
    sink.keep().await.expect("keeps the partial");

    let (_sink, already) = p.open_resumable(&dest).await.expect("reopens");
    assert_eq!(
        already,
        128 * 1024,
        "the hole ALREADY counts: resuming from 0 would recopy what was done"
    );
}

/// **The case sparse writing almost broke, and it's silent corruption.**
///
/// A staging reopened to resume is opened with `O_APPEND`, and
/// `O_APPEND` does NOT place the offset at the end: it leaves it at 0 and
/// only repositions right before each `write`. So a RELATIVE seek over an
/// N-byte partial used to jump from 0, and the `set_len` that followed
/// didn't extend — it TRUNCATED, throwing away what had already been
/// copied with nothing checking it (the commit publishes and that's that).
///
/// It's exactly the common case the feature chases: a disk image,
/// interrupted once, resumed — and an image is mostly zeros, so the first
/// chunk after resuming being all zeros is the NORMAL case, not the edge.
#[tokio::test]
async fn resuming_with_a_zero_chunk_does_not_eat_what_was_already_copied() {
    let (p, root, base) = provider();
    let dest = child(&root, b"resumed.img");

    // First stage: real data, and the partial is kept.
    let (mut sink, already) = p.open_resumable(&dest).await.expect("opens");
    assert_eq!(already, 0);
    sink.write(Bytes::from(vec![b'a'; 64 * 1024]))
        .await
        .expect("data");
    sink.keep().await.expect("keeps");

    // Resumed, and the first thing that arrives is a hole.
    let (mut sink, already) = p.open_resumable(&dest).await.expect("reopens");
    assert_eq!(already, 64 * 1024, "the partial is still whole");
    sink.write(Bytes::from(vec![0u8; 256 * 1024]))
        .await
        .expect("zeros");
    sink.write(Bytes::from(vec![b'z'; 1024]))
        .await
        .expect("tail");
    sink.commit().await.expect("commit");

    let mut expected = vec![b'a'; 64 * 1024];
    expected.extend(std::iter::repeat_n(0u8, 256 * 1024));
    expected.extend(std::iter::repeat_n(b'z', 1024));
    assert_eq!(
        std::fs::read(base.join("resumed.img")).expect("read"),
        expected,
        "the bytes from before resuming have to still be there"
    );
}
