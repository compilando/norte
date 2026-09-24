//! [`Backend`] parity: the SAME script through the embedded arm and the
//! remote one, and the two transcripts have to match line by line.
//!
//! This is the module's promise ("the SAME surface for the embedded core and
//! the daemon", rule 7) made checkable method by method. The `Backend`'s other
//! tests test each arm on its own and almost always in copy, listing and
//! search; task reports, the journal, the index or the registry were not
//! walked by anyone from here, so one arm could drift from the other without
//! it showing. It is also the safety net for the wave that splits
//! `backend.rs` by area: the script does not know which file each method
//! lives in.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::backend::remote::RemoteBackend;
use norte_core::backend::{Backend, TaskRef};
use norte_core::daemon::{Daemon, DaemonConfig};
use norte_proto::methods::{self, ClientInfo};
use norte_proto::{Error, Segment, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

fn seg(name: &str) -> Segment {
    Segment::new(name.as_bytes().to_vec()).expect("valid segment")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

/// Engine with an in-memory journal and a spool: what undoable mutations and
/// sync need. Both arms start from an identical one.
async fn engine(spool: &std::path::Path) -> (Arc<Engine>, Arc<MemProvider>) {
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(spool));
    (Arc::new(engine), mem)
}

/// A task's terminal state, with a cap: if a task does not finish, the test
/// fails here instead of hanging.
async fn join(task: TaskRef) -> TaskState {
    tokio::time::timeout(Duration::from_secs(10), task.join())
        .await
        .expect("terminal before the timeout")
}

/// `ok` or the error's variant, without its fields: messages can carry ids or
/// paths that are not part of the contract.
fn result<T>(r: &Result<T, Error>) -> String {
    match r {
        Ok(_) => "ok".to_owned(),
        Err(e) => {
            let d = format!("{e:?}");
            d.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
                .to_owned()
        }
    }
}

/// A launched task's terminal state, or why it was not launched.
async fn task(r: Result<TaskRef, Error>) -> (String, Option<norte_proto::TaskId>) {
    match r {
        Ok(t) => {
            let id = t.id();
            (format!("{:?}", join(t).await), Some(id))
        }
        Err(e) => (result::<()>(&Err(e)), None),
    }
}

/// The script. Each step leaves a line; what is asserted here is what a
/// frontend takes for granted, and the comparison between arms covers the
/// rest.
///
/// NOTE: the content "hola" / "adios" below is kept verbatim because
/// `sha256("hola")` is asserted against a hardcoded digest — translating it
/// would require recomputing that hash by hand. See the T05 report.
#[expect(clippy::too_many_lines, reason = "a script: one step after another")]
async fn script(b: &Backend, mem: &MemProvider) -> Vec<String> {
    let mut t = Vec::new();
    for d in ["mem:///d", "mem:///e", "mem:///parts"] {
        mem.mkdir(&vp(d)).await.expect("mkdir");
    }
    write_file(mem, "mem:///d/a.txt", b"hola").await;
    write_file(mem, "mem:///d/b.txt", b"adios").await;
    write_file(mem, "mem:///e/a.txt", b"hola").await;

    // Checksums and their report.
    let (state, id) = task(
        b.checksum(methods::FsChecksumParams {
            paths: vec![vp("mem:///d/a.txt")],
            algo: methods::ChecksumAlgo::Sha256,
        })
        .await,
    )
    .await;
    assert_eq!(state, "Completed");
    let report = b
        .checksum_report(id.expect("id"))
        .await
        .expect("checksum_report");
    assert_eq!(report.entries.len(), 1);
    assert_eq!(
        report.entries[0].digest.as_deref(),
        Some("b221d9dbb083a7f33428d7c2a3c3198ae925614d70210e28716ccaa7cd4ddb79"),
        "sha256(\"hola\")"
    );
    t.push(format!("checksum {state} {report:?}"));

    // A directory's size and usage.
    let (state, _) = task(
        b.dir_size(methods::FsDirSizeParams {
            paths: vec![vp("mem:///d")],
        })
        .await,
    )
    .await;
    t.push(format!("dir_size {state}"));
    let (state, id) = task(
        b.dir_usage(methods::FsDirUsageParams {
            path: vp("mem:///d"),
            depth: 1,
        })
        .await,
    )
    .await;
    let usage = b
        .dir_usage_report(id.expect("id"))
        .await
        .expect("dir_usage_report");
    assert_eq!(usage.total_bytes, 9, "hola + adios");
    t.push(format!("dir_usage {state} {usage:?}"));

    // Packing, testing what was packed, and their reports.
    let (state, id) = task(
        b.pack(methods::ArchivePackParams {
            sources: vec![vp("mem:///d/a.txt"), vp("mem:///d/b.txt")],
            dest: vp("mem:///p.zip"),
            format: methods::ArchiveFormat::Zip,
            level: None,
            base: vp("mem:///d"),
        })
        .await,
    )
    .await;
    assert_eq!(state, "Completed");
    let packed = b
        .archive_pack_report(id.expect("id"))
        .await
        .expect("archive_pack_report");
    assert_eq!(packed.entries, 2);
    t.push(format!("pack {state} {packed:?}"));
    let (state, id) = task(
        b.test_archive(methods::ArchiveTestParams {
            path: vp("mem:///p.zip"),
        })
        .await,
    )
    .await;
    let tested = b
        .archive_test_report(id.expect("id"))
        .await
        .expect("archive_test_report");
    assert!(tested.failed.is_empty(), "{tested:?}");
    t.push(format!("test_archive {state} {tested:?}"));

    // Split and combine: three parts of the minimum that is allowed.
    let part = methods::FILE_SPLIT_MIN_BYTES;
    let big: Vec<u8> = (0..part * 2 + 5).map(|i| (i % 251) as u8).collect();
    write_file(mem, "mem:///big.bin", &big).await;
    let (state, _) = task(
        b.split_file(methods::FileSplitParams {
            path: vp("mem:///big.bin"),
            part_bytes: part,
            dest_dir: vp("mem:///parts"),
        })
        .await,
    )
    .await;
    assert_eq!(state, "Completed");
    let mut parts: Vec<VPath> = b
        .list(&vp("mem:///parts"))
        .await
        .expect("list parts")
        .into_iter()
        .map(|e| e.path)
        .collect();
    parts.sort_by_key(VPath::display_lossy);
    assert_eq!(parts.len(), 3);
    t.push(format!(
        "split {state} {:?}",
        parts.iter().map(VPath::display_lossy).collect::<Vec<_>>()
    ));
    let (state, _) = task(
        b.combine_files(methods::FileCombineParams {
            first: parts[0].clone(),
            dest: vp("mem:///joined.bin"),
        })
        .await,
    )
    .await;
    assert_eq!(state, "Completed");
    let size = b.stat(&vp("mem:///joined.bin")).await.expect("stat").size;
    assert_eq!(size, Some(big.len() as u64));
    t.push(format!("combine {state}"));

    // Permissions: the in-memory provider does not have them, and both arms
    // say so the same way.
    let r = b
        .set_mode(methods::FsSetModeParams {
            paths: vec![vp("mem:///d/a.txt")],
            mode: 0o600,
            recursive: false,
            dir_mode: None,
        })
        .await;
    t.push(format!("set_mode {}", task(r).await.0));

    // Batch rename: plan, run with its hash, report.
    let pairs = [methods::RenamePair {
        from: seg("a.txt"),
        to: seg("c.txt"),
    }];
    let plan = b
        .rename_batch_plan(&vp("mem:///d"), &pairs)
        .await
        .expect("rename_batch_plan");
    assert!(plan.executable, "{plan:?}");
    t.push(format!(
        "rename_plan {:?} {:?}",
        plan.steps, plan.collisions
    ));
    let (state, batch) = task(
        b.rename_batch(&vp("mem:///d"), &pairs, &plan.plan_hash)
            .await,
    )
    .await;
    let batch = batch.expect("id");
    assert_eq!(state, "Completed");
    let renamed = b
        .rename_batch_report(batch)
        .await
        .expect("rename_batch_report");
    assert_eq!(renamed.applied, 1);
    t.push(format!("rename {state} {renamed:?}"));

    // The journal recorded it, and undoing from there returns it.
    let journal = b.journal_list(None, 50, None).await.expect("journal_list");
    let ops: Vec<String> = journal
        .rows
        .iter()
        .map(|r| format!("{} {}", r.actor_kind, r.op))
        .collect();
    assert!(!journal.rows.is_empty());
    t.push(format!("journal {ops:?}"));
    let last = journal.rows.iter().map(|r| r.seq).max().expect("one row");
    let (state, id) = task(b.undo_after(last - 1, None).await).await;
    assert_eq!(state, "Completed");
    let undone = b.undo_report(id.expect("id")).await.expect("undo_report");
    assert_eq!(undone.undone, 1, "{undone:?}");
    t.push(format!("undo_after {state} {undone:?}"));
    // An id that exists but was not an undo: `NotFound` from both arms since
    // 0.79.0. Before that the daemon answered `INVALID_PARAMS`, which the
    // client read as `Internal`, and this test pinned it down per arm.
    t.push(format!(
        "undo_report someone else's {}",
        result(&b.undo_report(batch).await)
    ));
    // A documented divergence, and on purpose: the session being undone
    // belongs to an agent, and agents live in the daemon. It is asserted per
    // arm and does not enter the transcript.
    let session = task(b.undo_session("nobody").await).await.0;
    let expected = match b {
        Backend::Embedded(_) => "Unsupported",
        Backend::Remote(_) => "Completed",
    };
    assert_eq!(session, expected, "undo_session");

    // Compare two folders.
    match b
        .compare(methods::FsCompareParams {
            left: vp("mem:///d"),
            right: vp("mem:///e"),
            criteria: methods::CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 0,
            follow_symlinks: false,
            descend_orphans: None,
        })
        .await
    {
        Ok((task, mut rx)) => {
            let state = join(task).await;
            let mut rows = 0;
            while let Ok(Some(batch)) =
                tokio::time::timeout(Duration::from_secs(5), rx.recv()).await
            {
                rows += batch.rows.len();
            }
            t.push(format!("compare {state:?} {rows}"));
        }
        Err(e) => t.push(format!("compare {}", result::<()>(&Err(e)))),
    }

    // Index.
    t.push(format!(
        "index_build {}",
        task(b.index_build(&vp("mem:///d")).await).await.0
    ));
    let hits = b.index_query(&vp("mem:///d"), "c", 10).await;
    t.push(format!(
        "index_query {} {:?}",
        result(&hits),
        hits.map(|h| h.len()).ok()
    ));

    // What is not about files.
    t.push(format!("volumes {}", result(&b.volumes(false).await)));
    // A second documented divergence: there is no wire method for GC (a
    // protocol change deferred until there is demand).
    let gc = b.gc_partials(&vp("mem:///d"), Duration::ZERO).await;
    match b {
        Backend::Embedded(_) => assert_eq!(gc.ok(), Some(0), "nothing to sweep"),
        Backend::Remote(_) => assert_eq!(result(&gc), "Unsupported"),
    }
    let plugins = b.plugins_list().await;
    t.push(format!("plugins_list {}", result(&plugins)));
    t.push(format!(
        "close_connection {:?}",
        b.close_connection(&vp("mem:///")).await.ok()
    ));
    // A third, and also on purpose: it says what the TYPE can promise
    // ("there is a daemon behind it"), not whether this particular engine
    // carries a journal — and this one does. See its rustdoc.
    assert_eq!(b.is_journalled(), matches!(b, Backend::Remote(_)));
    b.drop_retained_plans().await;

    t
}

#[tokio::test(flavor = "multi_thread")]
async fn both_arms_tell_the_same_story() {
    let dir_e = tempfile::tempdir().expect("tempdir");
    let (e, mem_e) = engine(dir_e.path()).await;
    let embedded = script(&Backend::Embedded(e), &mem_e).await;

    let dir_r = tempfile::tempdir().expect("tempdir");
    let socket = dir_r.path().join("d.sock");
    let (e, mem_r) = engine(dir_r.path()).await;
    let daemon = Daemon::bind(
        e,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir_r.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let _run = tokio::spawn(daemon.run());
    let r = RemoteBackend::connect(
        socket,
        None,
        ClientInfo {
            name: "parity".into(),
            version: "0.0.0".into(),
        },
    )
    .await
    .expect("connect");
    let remote = script(&Backend::Remote(r), &mem_r).await;

    for (i, (a, b)) in embedded.iter().zip(&remote).enumerate() {
        assert_eq!(a, b, "step {i}: embedded ≠ remote");
    }
    assert_eq!(embedded.len(), remote.len());
}
