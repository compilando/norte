//! The [`Backend::Embedded`] surface (phase 3 M2): the same contract as the
//! remote one, against the in-process `Engine` — no socket.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_core::TransferOptions;
use norte_core::backend::Backend;
use norte_proto::methods::FsSearchParams;
use norte_proto::{ByteRange, CapabilityFlags, DeleteMode, Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

fn embedded() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (Backend::Embedded(Arc::new(engine)), mem)
}

#[tokio::test]
async fn embedded_list_read_capabilities() {
    let (mut backend, mem) = embedded();
    write_file(&mem, "mem:///f.txt", b"0123456789").await;

    let entries = backend.list(&vp("mem:///")).await.expect("list");
    assert_eq!(entries.len(), 1);

    let bytes = backend
        .read(
            &vp("mem:///f.txt"),
            Some(ByteRange {
                offset: 3,
                len: Some(4),
            }),
        )
        .await
        .expect("read");
    assert_eq!(bytes, b"3456");

    let caps = backend.capabilities(&vp("mem:///")).await.expect("caps");
    assert!(caps.flags.contains(CapabilityFlags::TRASH));

    // Embedded has no daemon channels.
    assert!(backend.take_foreign_tasks().is_none());
    assert!(backend.take_conn_events().is_none());
}

#[tokio::test]
async fn embedded_copy_move_delete_as_tasks() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///a", b"data").await;

    let task = backend
        .copy(&vp("mem:///a"), &vp("mem:///b"), TransferOptions::default())
        .await
        .expect("copy");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///b")).await.is_ok());

    let task = backend
        .move_(&vp("mem:///b"), &vp("mem:///c"), TransferOptions::default())
        .await
        .expect("move");
    assert_eq!(task.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///b")).await.unwrap_err(),
        Error::NotFound
    );

    let task = backend
        .delete(&vp("mem:///c"), DeleteMode::Permanent)
        .await
        .expect("delete");
    assert_eq!(task.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///c")).await.unwrap_err(),
        Error::NotFound
    );
}

/// #104: mkdir as a Task through the embedded backend — it creates, and an
/// occupied destination fails (no -p, no idempotence).
#[tokio::test]
async fn embedded_mkdir_as_a_task() {
    let (backend, mem) = embedded();
    let task = backend.mkdir(&vp("mem:///new")).await.expect("mkdir");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///new")).await.is_ok());

    let task = backend.mkdir(&vp("mem:///new")).await.expect("submit");
    assert!(matches!(task.join().await, TaskState::Failed { .. }));
}

#[tokio::test]
async fn embedded_cancel_via_canceller() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///big", &vec![0xAB; 100_000]).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));
    let task = backend
        .copy(
            &vp("mem:///big"),
            &vp("mem:///copy"),
            TransferOptions::default(),
        )
        .await
        .expect("copy");
    task.canceller().cancel();
    assert_eq!(task.join().await, TaskState::Cancelled);
}

#[tokio::test]
async fn a_clone_shares_the_engine_and_stat_works() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///f", b"x").await;

    // The clone must share the same Engine (Arc), not mount a new one.
    let clone = backend.clone();
    let entry = clone.stat(&vp("mem:///f")).await.expect("stat");
    assert_eq!(entry.size, Some(1));

    // The original still works after cloning (the Arc was not moved).
    let entry2 = backend.stat(&vp("mem:///f")).await.expect("original stat");
    assert_eq!(entry2.size, Some(1));
}

/// Embedded `Backend::search` = a passthrough to the engine's walker as
/// `Actor::User`: it returns `(TaskRef, rx)` and the hit batches arrive over
/// the direct channel (the walker closes `tx` when it finishes, so `rx`
/// closes on its own). Same contract as the remote one (see
/// `backend_remote.rs`).
#[tokio::test]
async fn embedded_search_hits_stream() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///a.rs", b"").await;
    write_file(&mem, "mem:///b.txt", b"").await;
    mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&mem, "mem:///sub/c.rs", b"").await;

    let (task, mut rx) = backend
        .search(FsSearchParams {
            name_glob: Some("*.rs".into()),
            ..FsSearchParams::new(vp("mem:///"))
        })
        .await
        .expect("search");

    let mut got = Vec::new();
    while let Some(hits) = rx.recv().await {
        for e in hits.entries {
            got.push(e.path.display_lossy());
        }
    }
    got.sort();
    assert_eq!(
        got,
        vec![
            vp("mem:///a.rs").display_lossy(),
            vp("mem:///sub/c.rs").display_lossy()
        ]
    );
    assert_eq!(task.join().await, TaskState::Completed);
}

#[tokio::test]
async fn embedded_errors_are_taxonomy() {
    let (backend, _mem) = embedded();
    assert_eq!(
        backend
            .list(&vp("mem:///does-not-exist"))
            .await
            .unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        backend
            .read(&vp("mem:///does-not-exist"), None)
            .await
            .unwrap_err(),
        Error::NotFound
    );
}

// ---------- attrs (#108 block 2) ----------

#[tokio::test]
async fn embedded_sanitizes_catalogue_and_requests_attrs() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_synthetic_attrs());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let backend = Backend::Embedded(Arc::new(engine));
    write_file(&mem, "mem:///f.txt", b"x").await;

    // Catalogue through the embedded path: goes through AttrCatalog::new (ADR 0039 §4).
    let cat = backend
        .attr_catalog(&vp("mem:///"))
        .await
        .expect("catalogue");
    assert!(
        cat.iter().any(|a| a.id == "mem.owner"),
        "catalogue: {cat:?}"
    );

    // stat_attrs materializes what was requested; raw bytes survive (rule 1).
    let e = backend
        .stat_attrs(&vp("mem:///f.txt"), &["mem.owner".to_owned()])
        .await
        .expect("stat_attrs");
    assert!(matches!(
        e.attrs.get("mem.owner"),
        Some(norte_proto::AttrValue::Bytes(_))
    ));

    // list_with_skipped_attrs: every entry carries what was requested; without
    // asking, nothing.
    let (entries, _) = backend
        .list_with_skipped_attrs(&vp("mem:///"), &["mem.mode".to_owned()])
        .await
        .expect("list attrs");
    assert!(entries.iter().all(|e| e.attrs.contains_key("mem.mode")));
    let (bare, _) = backend
        .list_with_skipped(&vp("mem:///"))
        .await
        .expect("list");
    assert!(bare.iter().all(|e| e.attrs.is_empty()));
}

/// H3d: BOTH halves of `fs.capabilities` in ONE call, and the same values the
/// two accessors return separately.
///
/// This exists because frontends want both: the TUI caches the catalogue for
/// its columns and the flags to answer "is this read-only?" without asking
/// again. With `capabilities` and `attr_catalog` each throwing away the other
/// half, that was two round trips for a message that already carried them
/// together.
#[tokio::test]
async fn embedded_capabilities_and_attrs_in_one_call() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_synthetic_attrs());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let backend = Backend::Embedded(Arc::new(engine));

    let (caps, cat) = backend
        .capabilities_and_attrs(&vp("mem:///"))
        .await
        .expect("both halves");
    assert_eq!(
        caps,
        backend.capabilities(&vp("mem:///")).await.expect("caps"),
        "the flags are the same as the usual accessor's"
    );
    assert_eq!(
        cat.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        backend
            .attr_catalog(&vp("mem:///"))
            .await
            .expect("catalogue")
            .iter()
            .map(|a| a.id.clone())
            .collect::<Vec<_>>(),
        "and so is the catalogue: this is not a path with different sanitizing"
    );
}

// ------------------------------------------------- sync.plan / sync.apply

/// An embedded backend with a journal and a spool: what is needed to really
/// sync. Without either, the engine refuses (and there is a test below).
async fn embedded_sync() -> (Backend, Arc<MemProvider>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    (Backend::Embedded(Arc::new(engine)), mem, dir)
}

fn sync_params(source: &str, dest: &str) -> norte_proto::methods::SyncPlanParams {
    norte_proto::methods::SyncPlanParams {
        source: vp(source),
        dest: vp(dest),
        mode: norte_proto::methods::SyncMode::Update,
        compare: norte_proto::methods::SyncCompareOptions::default(),
        on_unknown: norte_proto::methods::OnUnknown::Copy,
        include: None,
    }
}

/// The whole cycle through the embedded `Backend`: plan, close with the hash,
/// apply that hash and read the report. This is what the TUI will do, and
/// until this task there was no method to call.
///
/// The embedded connection is ONE: if planning and applying did not use the
/// same `conn_id`, this same `Backend`'s `sync_apply` would answer `PlanStale`
/// to its own plan. This test is what pins that down.
#[tokio::test]
async fn embedded_sync_plan_and_apply_close_the_cycle() {
    let (backend, mem, _dir) = embedded_sync().await;
    mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&mem, "mem:///s/a.txt", b"aaa").await;

    let (task, mut rx) = backend
        .sync_plan(sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let mut done = None;
    let mut steps = 0usize;
    while let Some(event) = rx.recv().await {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(b) => {
                assert!(done.is_none(), "the closing event has to be last");
                steps += b.steps.len();
            }
            norte_core::sync::SyncPlanEvent::Done(d) => done = Some(d),
        }
    }
    assert_eq!(task.join().await, TaskState::Completed);
    let done = done.expect("the plan closed with its hash");
    assert_eq!(steps, 1);
    assert!(done.executable);

    let applying = backend
        .sync_apply(&done.plan_hash)
        .await
        .expect("its own plan applies");
    let id = applying.id();
    assert_eq!(applying.join().await, TaskState::Completed);
    let report = backend.sync_report(id).await.expect("report");
    assert_eq!(report.done, 1);
    assert_eq!(report.failed, 0);
    assert!(report.batch_id.is_some());
    assert!(
        mem.stat(&vp("mem:///d/a.txt")).await.is_ok(),
        "the copy happened"
    );

    // The plan was spent: the same hash no longer runs anything.
    assert!(matches!(
        backend.sync_apply(&done.plan_hash).await,
        Err(Error::PlanStale)
    ));
}

/// Fail-closed, and not by wiring accident: an engine WITHOUT a spool does not
/// plan and one without a journal does not apply. That the CLI does not call
/// `set_spool` today is not what protects; what protects is the engine, and
/// this pins it down.
#[tokio::test]
async fn embedded_without_spool_does_not_plan_and_without_journal_does_not_apply() {
    // 1) Without a spool: there is no retention, so there is no plan to approve.
    let (backend, mem) = embedded();
    mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    assert!(matches!(
        backend.sync_plan(sync_params("mem:///s", "mem:///d")).await,
        Err(Error::Unsupported)
    ));

    // 2) With a spool but WITHOUT a journal: it plans, and applying is
    //    refused — a plan promises a reversal per step and only the journal
    //    can fulfill it.
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let backend = Backend::Embedded(Arc::new(engine));
    mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&mem, "mem:///s/a.txt", b"aaa").await;

    let (task, mut rx) = backend
        .sync_plan(sync_params("mem:///s", "mem:///d"))
        .await
        .expect("planning writes nothing");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let norte_core::sync::SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(task.join().await, TaskState::Completed);
    let done = done.expect("the plan closed");
    assert!(matches!(
        backend.sync_apply(&done.plan_hash).await,
        Err(Error::Unsupported)
    ));
    assert!(
        mem.stat(&vp("mem:///d/a.txt")).await.is_err(),
        "without a journal not a byte is written"
    );
}

/// The three fields that are not the caller's are rejected BEFORE choosing an
/// arm, with the same taxonomy the embedded one would give — the daemon
/// answers them with a bare `-32602`, which `to_taxonomy` would turn into
/// `Internal`. Same parity as `Backend::compare`.
#[tokio::test]
async fn embedded_sync_plan_rejects_what_is_not_the_callers() {
    let (backend, mem, _dir) = embedded_sync().await;
    mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    assert!(matches!(
        backend.sync_plan(p).await,
        Err(Error::Unsupported)
    ));

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(norte_proto::methods::DescendSide::Right);
    assert!(matches!(
        backend.sync_plan(p).await,
        Err(Error::Unsupported)
    ));

    let mut p = sync_params("mem:///s", "mem:///d");
    p.include = Some(vec![
        norte_proto::methods::RelPath::parse_wire("x")
            .expect("rel");
        norte_proto::methods::SYNC_MAX_INCLUDE + 1
    ]);
    assert!(matches!(
        backend.sync_plan(p).await,
        Err(Error::InvalidPath)
    ));
}
