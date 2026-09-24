//! Test-first matrix of the copy engine v0 (M0 phase 10), against
//! `MemProvider` with injected faults: happy path, hostile recursive,
//! collisions, clean per-chunk cancellation, byte-exact failures,
//! disconnection, move=rename, post-order delete and `copy_native`.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk goes in");
    sink.commit().await.expect("commit publishes");
}

async fn read_all(mem: &MemProvider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = mem.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Engine with a registered `MemProvider`; also returns the provider.
fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

// ---------- copy ----------

#[tokio::test]
async fn copy_file_happy_path() {
    let (engine, mem) = engine_with_mem();
    let content = vec![0xAB; 5000];
    write_file(&mem, "mem:///src.bin", &content).await;

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .expect("submit");
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(read_all(&mem, "mem:///dst.bin").await.unwrap(), content);
    let last = rx.borrow().clone();
    assert_eq!(last.bytes_done, 5000);
    assert_eq!(last.bytes_total, Some(5000));
    assert_eq!(last.entries_total, Some(1));
}

#[tokio::test]
async fn copy_dir_recursive_with_hostile_names() {
    let (engine, mem) = engine_with_mem();
    // 3-level tree with hostile names from the corpus.
    let hostile = norte_testkit::corpus::hostile_names();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub/deep")).await.unwrap();
    let root = MemProvider::root();
    let mut paths = Vec::new();
    for (i, h) in hostile.iter().take(3).enumerate() {
        let dir = ["src", "src/sub", "src/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let mut p = vp(&format!("mem:///{dir}"));
        p = p.join(seg);
        let mut sink = mem.write(&p).await.expect("hostile write");
        sink.write(Bytes::from_static(b"data")).await.unwrap();
        sink.commit().await.unwrap();
        paths.push(p);
    }
    drop(root);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);

    // Every hostile file exists at the destination with intact bytes.
    for (i, h) in hostile.iter().take(3).enumerate() {
        let dir = ["dst", "dst/sub", "dst/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let p = vp(&format!("mem:///{dir}")).join(seg);
        let e = mem.stat(&p).await.unwrap_or_else(|err| {
            panic!("[{}] missing at the destination: {err:?}", h.id);
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            h.bytes.as_slice(),
            "[{}] intact bytes",
            h.id
        );
    }
}

#[tokio::test]
async fn copy_collision_fails_without_writing() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"new").await;
    write_file(&mem, "mem:///dst", b"precious previous content").await;

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { conflict },
        } => assert_eq!(conflict, ConflictKind::Exists),
        other => panic!("expected Conflict, was {other:?}"),
    }
    // The destination is left EXACTLY as it was.
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"precious previous content"
    );
}

#[tokio::test]
async fn copy_cancel_leaves_no_partial_destination() {
    let (engine, mem) = engine_with_mem();
    let content = vec![0x5A; 512 * 1024];
    write_file(&mem, "mem:///big", &content).await;

    let handle = engine
        .copy(&vp("mem:///big"), &vp("mem:///copy"))
        .await
        .unwrap();
    // Cancels as soon as there is byte progress (the engine checks per chunk).
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.bytes_done > 0 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    // It may already have finished (a legitimate race); if not, it must be Cancelled.
    let final_state = handle.join().await;
    match final_state {
        TaskState::Cancelled => {
            // Clean cancellation: neither a file nor a trace at the destination.
            assert_eq!(
                mem.stat(&vp("mem:///copy")).await.unwrap_err(),
                Error::NotFound,
                "the destination must be clean after cancelling"
            );
        }
        TaskState::Completed => {
            assert_eq!(read_all(&mem, "mem:///copy").await.unwrap(), content);
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

#[tokio::test]
async fn copy_read_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![1u8; 4000]).await;
    mem.faults().fail_read_at(&vp("mem:///src"), 2000);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("expected Failed{{Io}}, was {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "clean destination after a read failure"
    );
}

#[tokio::test]
async fn copy_write_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![2u8; 4000]).await;
    mem.faults().fail_write_at(&vp("mem:///dst"), 1000);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("expected Failed{{Io}}, was {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "clean destination after a write failure"
    );
}

#[tokio::test]
async fn copy_disconnect_maps_to_provider_unavailable() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"x").await;
    mem.faults().disconnect_after(1);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed { error } => {
            assert_eq!(error, Error::ProviderUnavailable { retryable: true });
        }
        other => panic!("expected ProviderUnavailable, was {other:?}"),
    }
}

#[tokio::test]
async fn copy_native_used_when_server_copy_declared() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"native content").await;
    // If the engine tried streaming, this fault would take it down:
    // copy_native does not read via stream, so it must complete all the same.
    mem.faults().fail_read_at(&vp("mem:///src"), 0);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    mem.faults().clear();
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"native content"
    );
}

// ---------- move ----------

#[tokio::test]
async fn move_same_provider_is_rename_zero_bytes() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///source", b"content").await;

    let handle = engine
        .move_(&vp("mem:///source"), &vp("mem:///destination"))
        .await
        .unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(
        mem.stat(&vp("mem:///source")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        read_all(&mem, "mem:///destination").await.unwrap(),
        b"content"
    );
    assert_eq!(rx.borrow().bytes_done, 0, "rename copies no bytes");
}

#[tokio::test]
async fn move_collision_fails_and_source_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"1").await;
    write_file(&mem, "mem:///b", b"2").await;

    let handle = engine
        .move_(&vp("mem:///a"), &vp("mem:///b"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { .. },
        } => {}
        other => panic!("expected Conflict, was {other:?}"),
    }
    assert_eq!(read_all(&mem, "mem:///a").await.unwrap(), b"1");
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"2");
}

// ---------- delete ----------

#[tokio::test]
async fn delete_tree_post_order() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    mem.mkdir(&vp("mem:///d/sub")).await.unwrap();
    write_file(&mem, "mem:///d/f1", b"x").await;
    write_file(&mem, "mem:///d/sub/f2", b"y").await;

    let handle = engine.delete(&vp("mem:///d")).await.unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///d")).await.unwrap_err(),
        Error::NotFound
    );
    // 4 entries: d, d/sub, d/f1, d/sub/f2.
    assert_eq!(rx.borrow().entries_total, Some(4));
    assert_eq!(rx.borrow().entries_done, 4);
}

#[tokio::test]
async fn delete_missing_fails_not_found() {
    let (engine, mem) = engine_with_mem();
    let _ = &mem;
    let handle = engine.delete(&vp("mem:///nothing")).await.unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::NotFound
        }
    );
}

// ---------- registration / passthrough ----------

#[tokio::test]
async fn unknown_scheme_rejected_at_submit() {
    let (engine, _mem) = engine_with_mem();
    assert!(
        engine
            .copy(&vp("sftp://h/x"), &vp("mem:///y"))
            .await
            .is_err_and(|e| e == Error::Unsupported)
    );
    assert!(
        engine
            .copy(&vp("mem:///x"), &vp("sftp://h/y"))
            .await
            .is_err_and(|e| e == Error::Unsupported)
    );
}

#[tokio::test]
async fn stat_and_list_passthrough() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"xyz").await;
    let e = engine.stat(&vp("mem:///f")).await.unwrap();
    assert_eq!(e.size, Some(3));
    let n = engine.list(&vp("mem:///")).await.unwrap().count().await;
    assert_eq!(n, 1);
}

// ---------- clean Task cancellation (hard rule 3, rust-reviewer findings) ----------

/// A source tree with `n` files under `mem:///src`.
async fn build_tree(mem: &MemProvider, n: usize) {
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    for i in 0..n {
        write_file(mem, &format!("mem:///src/f{i:03}"), b"data").await;
    }
}

#[tokio::test]
async fn copy_tree_cancel_leaves_complete_files_only() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 30).await;
    // Real per-op latency: the task advances slowly and the cancellation
    // reliably lands halfway through the tree.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 2 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    assert_eq!(final_state, TaskState::Cancelled);
    // A partial tree is allowed (copy_task's doc), but EVERY file present is
    // complete: never a half file with no mark.
    let mut listed = mem.list(&vp("mem:///dst")).await.unwrap();
    while let Some(e) = listed.next().await {
        let e = e.unwrap();
        if e.kind == norte_proto::EntryKind::File {
            let name = String::from_utf8(e.path.file_name().unwrap().as_bytes().to_vec()).unwrap();
            assert_eq!(
                read_all(&mem, &format!("mem:///dst/{name}")).await.unwrap(),
                b"data",
                "half a file at the destination: {name}"
            );
        }
    }
}

#[tokio::test]
async fn delete_tree_cancel_keeps_root_and_rest_intact() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 30).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine.delete(&vp("mem:///src")).await.unwrap();
    let mut rx = handle.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 2 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    assert_eq!(final_state, TaskState::Cancelled);
    // Post-order: the root falls LAST — cancelled halfway, it is still there.
    assert!(
        mem.stat(&vp("mem:///src")).await.is_ok(),
        "the root only falls at the end; cancelling halfway leaves it"
    );
}

#[tokio::test]
async fn move_cancel_before_start_leaves_everything_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///source", b"content").await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));

    let handle = engine
        .move_(&vp("mem:///source"), &vp("mem:///destination"))
        .await
        .unwrap();
    // Cancels immediately: the task observes it before the rename.
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    match final_state {
        TaskState::Cancelled => {
            assert_eq!(read_all(&mem, "mem:///source").await.unwrap(), b"content");
            assert_eq!(
                mem.stat(&vp("mem:///destination")).await.unwrap_err(),
                Error::NotFound
            );
        }
        // A legitimate race: the rename beat the cancellation (atomic, clean).
        TaskState::Completed => {
            assert_eq!(
                read_all(&mem, "mem:///destination").await.unwrap(),
                b"content"
            );
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

#[tokio::test]
async fn copy_dir_into_itself_rejected() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    let handle = engine
        .copy(&vp("mem:///a"), &vp("mem:///a/b"))
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    // The tree is left intact: no phantom nested copy.
    let n = mem.list(&vp("mem:///a")).await.unwrap().count().await;
    assert_eq!(n, 0);
}

// ---------- M0 hard debt: EXDEV (#3) and move with a single walk (#9) ----------

/// Delegates EVERYTHING to a [`MemProvider`] except `rename`, which returns
/// `Unsupported` — like a real FS facing EXDEV (different mounts).
struct NoRename(Arc<MemProvider>);

#[async_trait::async_trait]
impl Provider for NoRename {
    // The trait's signature is `-> &str`; the literal here is correct.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature is `-> &str`; the literal here is correct"
    )]
    fn scheme(&self) -> &str {
        "mem"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.0.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.0.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.0.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.0.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.0.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.0.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.0.remove(p).await
    }
    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// EXDEV (issue #3): a rename impossible on the same provider is NOT a
/// terminal error — the move degrades to copy+delete.
#[tokio::test]
async fn move_degrades_to_copy_delete_when_rename_unsupported() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(NoRename(Arc::clone(&inner))) as Arc<dyn Provider>);
    write_file(&inner, "mem:///source", b"content").await;

    let handle = engine
        .move_(&vp("mem:///source"), &vp("mem:///destination"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(
        read_all(&inner, "mem:///destination").await.unwrap(),
        b"content"
    );
    assert_eq!(
        inner.stat(&vp("mem:///source")).await.unwrap_err(),
        Error::NotFound
    );
}

/// A source whose first `read` INJECTS a new file into the directory being
/// moved: simulates an entry that appeared after the copy's walk (issue #9's
/// window).
struct InjectOnRead {
    inner: Arc<MemProvider>,
    done: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Provider for InjectOnRead {
    // The trait's signature is `-> &str`; the literal here is correct.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature is `-> &str`; the literal here is correct"
    )]
    fn scheme(&self) -> &str {
        "src"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.inner.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        if !self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            write_file(&self.inner, "src:///dir/late", b"arrived after the walk").await;
        }
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.inner.rename(from, to).await
    }
}

/// Issue #9: what appears at the source AFTER the copy's walk is never
/// deleted without having been copied. With separate copy and delete walks,
/// `late` used to be deleted silently; with the single plan it survives (and
/// the move fails with Conflict because it cannot empty the dir — no loss,
/// never silently).
#[tokio::test]
async fn move_cross_provider_never_deletes_uncopied_entries() {
    let engine = Engine::new();
    let src_inner = Arc::new(MemProvider::new());
    let dst = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(InjectOnRead {
        inner: Arc::clone(&src_inner),
        done: std::sync::atomic::AtomicBool::new(false),
    }) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst) as Arc<dyn Provider>);

    src_inner.mkdir(&vp("src:///dir")).await.unwrap();
    write_file(&src_inner, "src:///dir/a", b"planned").await;

    let handle = engine
        .move_(&vp("src:///dir"), &vp("mem:///dir"))
        .await
        .unwrap();
    let state = handle.join().await;

    // What was planned reached the destination.
    assert_eq!(read_all(&dst, "mem:///dir/a").await.unwrap(), b"planned");
    // `late` exists SOMEWHERE (source or destination): never silent loss.
    let at_source = src_inner.stat(&vp("src:///dir/late")).await.is_ok();
    let at_dest = dst.stat(&vp("mem:///dir/late")).await.is_ok();
    assert!(
        at_source || at_dest,
        "late entry deleted without being copied (state: {state:?})"
    );
}

/// Rule 3 for the move's new copy+delete path (issues #3/#9): cancelling in
/// the middle of the DELETE phase leaves a complete destination + partial
/// source — duplicated, never lost nor a half file.
#[tokio::test]
async fn move_by_copy_cancel_mid_delete_loses_nothing() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(NoRename(Arc::clone(&inner))) as Arc<dyn Provider>);
    build_tree(&inner, 12).await;
    inner
        .faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine
        .move_(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    let mut rx = handle.progress();
    // Copy phase = 13 steps (12 files + root); from 14 onward the task is
    // deleting the source.
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.entries_done >= 14 || snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    handle.cancel();
    let state = handle.join().await;
    inner.faults().clear();
    assert_eq!(state, TaskState::Cancelled);
    // Invariant: every original file exists COMPLETE at the source or the destination.
    for i in 0..12 {
        let name = format!("f{i:03}");
        let content = match read_all(&inner, &format!("mem:///dst/{name}")).await {
            Ok(c) => c,
            Err(_) => read_all(&inner, &format!("mem:///src/{name}"))
                .await
                .unwrap_or_else(|_| panic!("{name} lost during cancellation")),
        };
        assert_eq!(content, b"data", "{name} left halfway");
    }
}

// ---------- phase 2: collision policies, symlinks and retries (ADR 0005) ----------

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, SymlinkPolicy};

fn on_collision(c: CollisionPolicy) -> TransferOptions {
    TransferOptions {
        on_collision: c,
        ..Default::default()
    }
}

fn on_symlinks(s: SymlinkPolicy) -> TransferOptions {
    TransferOptions {
        symlinks: s,
        ..Default::default()
    }
}

/// A pure delegation wrapper with its own scheme: two distinct "providers"
/// over independent Mem trees to force the cross-provider path.
struct Alias(Arc<MemProvider>);

#[async_trait::async_trait]
impl Provider for Alias {
    // The trait's signature is `-> &str`; the literal here is correct.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature is `-> &str`; the literal here is correct"
    )]
    fn scheme(&self) -> &str {
        "src"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.0.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<norte_proto::Entry, Error> {
        self.0.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.0.list(p).await
    }
    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.0.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.0.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.0.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.0.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.0.rename(from, to).await
    }
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.0.read_link(p).await
    }
    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.0.symlink(link, target, kind).await
    }
}

/// Two Mem trees with different schemes, registered on an engine.
fn engine_cross() -> (Engine, Arc<MemProvider>, Arc<MemProvider>) {
    let engine = Engine::new();
    let src = Arc::new(MemProvider::new());
    let dst = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(Alias(Arc::clone(&src))) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst) as Arc<dyn Provider>);
    (engine, src, dst)
}

#[tokio::test]
async fn copy_skip_merges_and_keeps_existing() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/a", b"new").await;
    write_file(&mem, "mem:///src/b", b"extra").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/a", b"old").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Skip),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/a").await.unwrap(), b"old");
    assert_eq!(read_all(&mem, "mem:///dst/b").await.unwrap(), b"extra");
}

#[tokio::test]
async fn copy_overwrite_replaces_colliding_file() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/a", b"new").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/a", b"old").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/a").await.unwrap(), b"new");
}

#[tokio::test]
async fn copy_overwrite_file_over_dir_is_type_mismatch() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"file").await;
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    mem.mkdir(&vp("mem:///dst/a")).await.unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///a"),
            &vp("mem:///dst/a"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error:
                Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                },
        } => {}
        other => panic!("expected TypeMismatch, was {other:?}"),
    }
    // The dir survives: a dir is never deleted to plant a file.
    assert!(mem.stat(&vp("mem:///dst/a")).await.is_ok());
}

#[tokio::test]
async fn copy_rename_auto_creates_numbered_variant() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///source.txt", b"v2").await;
    write_file(&mem, "mem:///destination.txt", b"v1").await;

    for expected in ["mem:///destination (1).txt", "mem:///destination (2).txt"] {
        let handle = engine
            .copy_with(
                &vp("mem:///source.txt"),
                &vp("mem:///destination.txt"),
                on_collision(CollisionPolicy::RenameAuto),
            )
            .await
            .unwrap();
        assert_eq!(handle.join().await, TaskState::Completed);
        assert_eq!(read_all(&mem, expected).await.unwrap(), b"v2");
    }
    assert_eq!(
        read_all(&mem, "mem:///destination.txt").await.unwrap(),
        b"v1",
        "the original is never touched"
    );
}

#[tokio::test]
async fn copy_newer_replaces_only_older_destination() {
    // Case A: the source is NEWER (it was written after) → replaces.
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///dst-old", b"v1").await;
    write_file(&mem, "mem:///src-new", b"v2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///src-new"),
            &vp("mem:///dst-old"),
            on_collision(CollisionPolicy::Newer),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst-old").await.unwrap(), b"v2");

    // Case B: the destination is newer → skip, content intact.
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src-old", b"v1").await;
    write_file(&mem, "mem:///dst-new", b"v2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///src-old"),
            &vp("mem:///dst-new"),
            on_collision(CollisionPolicy::Newer),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst-new").await.unwrap(), b"v2");
}

#[tokio::test]
async fn copy_ask_behaves_as_fail_in_m1() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"1").await;
    write_file(&mem, "mem:///b", b"2").await;
    let handle = engine
        .copy_with(
            &vp("mem:///a"),
            &vp("mem:///b"),
            on_collision(CollisionPolicy::Ask),
        )
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { .. },
        } => {}
        other => panic!("expected Conflict, was {other:?}"),
    }
}

#[tokio::test]
async fn symlink_preserve_recreates_link_bytes() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"content").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "default Preserve"
    );
    assert_eq!(
        mem.read_link(&vp("mem:///dst/ln")).await.unwrap(),
        b"f",
        "target bytes intact"
    );
    assert_eq!(read_all(&mem, "mem:///dst/f").await.unwrap(), b"content");
}

#[tokio::test]
async fn symlink_skip_copies_the_rest() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"content").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Skip),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///dst/f").await.unwrap(), b"content");
    assert_eq!(
        mem.stat(&vp("mem:///dst/ln")).await.unwrap_err(),
        Error::NotFound,
        "the link is not copied"
    );
}

#[tokio::test]
async fn symlink_follow_copies_target_content_as_file() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"content").await;
    mem.symlink(&vp("mem:///src/ln"), b"f", norte_vfs::SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Follow),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///dst/ln")).await.unwrap();
    assert_eq!(e.kind, norte_proto::EntryKind::File, "content, not a link");
    assert_eq!(read_all(&mem, "mem:///dst/ln").await.unwrap(), b"content");
}

/// M2 phase 1 (#19): Follow over a dir-symlink is no longer Unsupported — it
/// expands as a real dir (the fine-grained matrix lives in
/// `engine_m2_fase1.rs`).
#[tokio::test]
async fn symlink_follow_dir_symlink_expands() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.symlink(&vp("mem:///src/ln"), b"sub", norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_symlinks(SymlinkPolicy::Follow),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///dst/ln")).await.unwrap();
    assert_eq!(
        e.kind,
        norte_proto::EntryKind::Dir,
        "expanded as a real dir"
    );
}

#[tokio::test]
async fn move_skip_keeps_skipped_in_source() {
    let (engine, src, dst) = engine_cross();
    src.mkdir(&vp("src:///dir")).await.unwrap();
    write_file(&src, "src:///dir/a", b"collides").await;
    write_file(&src, "src:///dir/b", b"goes through").await;
    dst.mkdir(&vp("mem:///dir")).await.unwrap();
    write_file(&dst, "mem:///dir/a", b"old").await;

    let handle = engine
        .move_with(
            &vp("src:///dir"),
            &vp("mem:///dir"),
            on_collision(CollisionPolicy::Skip),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // What was skipped STAYS at the source (never deleted without copying).
    assert_eq!(read_all(&src, "src:///dir/a").await.unwrap(), b"collides");
    // What was moved left the source and is at the destination.
    assert_eq!(
        src.stat(&vp("src:///dir/b")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        read_all(&dst, "mem:///dir/b").await.unwrap(),
        b"goes through"
    );
    assert_eq!(read_all(&dst, "mem:///dir/a").await.unwrap(), b"old");
}

#[tokio::test]
async fn move_overwrite_same_provider_replaces() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"new").await;
    write_file(&mem, "mem:///b", b"old").await;
    let handle = engine
        .move_with(
            &vp("mem:///a"),
            &vp("mem:///b"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&mem, "mem:///b").await.unwrap(), b"new");
    assert_eq!(
        mem.stat(&vp("mem:///a")).await.unwrap_err(),
        Error::NotFound
    );
}

// ---------- retries with backoff (ADR 0005) ----------

#[tokio::test]
async fn retry_recovers_from_transient_unavailability() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"data").await;
    mem.faults().unavailable_for_next(2);

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "retries and passes"
    );
    assert_eq!(read_all(&mem, "mem:///dst.bin").await.unwrap(), b"data");
}

#[tokio::test]
async fn retry_gives_up_against_permanent_outage() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"data").await;
    mem.faults().disconnect_after(0);

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::ProviderUnavailable { .. },
        } => {}
        other => panic!("expected ProviderUnavailable, was {other:?}"),
    }
}

// Paused clock: `MemProvider`'s per-op latency runs on tokio's clock, so
// "sleep and cancel" stops being a race with the machine.
#[tokio::test(start_paused = true)]
async fn cancel_during_backoff_is_prompt() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src.bin", b"data").await;
    mem.faults().unavailable_for_next(u64::MAX);

    let start = std::time::Instant::now();
    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    // Lets the task enter the backoff wait and cancels.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "the cancellation does not wait for the backoff to finish"
    );
}

// ---------- phase 2 review findings ----------

/// B1: copying something ONTO ITSELF with Overwrite can never destroy the
/// source — it is `InvalidPath`, with the content intact.
#[tokio::test]
async fn copy_overwrite_onto_itself_never_destroys() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///unique", b"precious").await;
    let handle = engine
        .copy_with(
            &vp("mem:///unique"),
            &vp("mem:///unique"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(read_all(&mem, "mem:///unique").await.unwrap(), b"precious");
}

/// B1 case variant: on a case-insensitive provider, `a → A` is the SAME node.
#[tokio::test]
async fn copy_overwrite_case_variant_of_itself_never_destroys() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///unique", b"precious").await;
    let handle = engine
        .copy_with(
            &vp("mem:///UNIQUE"),
            &vp("mem:///unique"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Failed { .. }),
        "copying onto itself cannot 'work': {state:?}"
    );
    assert_eq!(read_all(&mem, "mem:///unique").await.unwrap(), b"precious");
}

/// B1 normalization variant: NFC → NFD of the same node on an insensitive Mem.
#[tokio::test]
async fn copy_overwrite_normalization_variant_never_destroys() {
    use norte_testkit::Normalization;
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_normalization(Normalization::Insensitive));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    // é NFC
    let nfc = "mem:///%C3%A9";
    let nfd = "mem:///e%CC%81";
    write_file(&mem, nfc, b"precious").await;
    let handle = engine
        .copy_with(&vp(nfc), &vp(nfd), on_collision(CollisionPolicy::Overwrite))
        .await
        .unwrap();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Failed { .. }),
        "a normalization variant of the source itself: {state:?}"
    );
    assert_eq!(read_all(&mem, nfc).await.unwrap(), b"precious");
}

/// M1: a same-provider move of a DIR over a FILE with Overwrite is
/// `TypeMismatch` — the file is never deleted to plant the dir.
#[tokio::test]
async fn move_overwrite_dir_over_file_is_type_mismatch() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///folder")).await.unwrap();
    write_file(&mem, "mem:///taken", b"file").await;
    let handle = engine
        .move_with(
            &vp("mem:///folder"),
            &vp("mem:///taken"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error:
                Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                },
        } => {}
        other => panic!("expected TypeMismatch, was {other:?}"),
    }
    assert_eq!(read_all(&mem, "mem:///taken").await.unwrap(), b"file");
}

/// M2: Follow + Overwrite over a dir-symlink does NOT destroy the
/// destination: probing the target happens BEFORE any destructive action.
#[tokio::test]
async fn follow_dir_symlink_with_overwrite_leaves_destination_intact() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.symlink(&vp("mem:///src/ln"), b"sub", norte_vfs::SymlinkKind::Dir)
        .await
        .unwrap();
    mem.mkdir(&vp("mem:///dst")).await.unwrap();
    write_file(&mem, "mem:///dst/ln", b"don't delete me").await;

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
                ..TransferOptions::default()
            },
        )
        .await
        .unwrap();
    // M2 phase 1 (#19): the dir-symlink expands as a DIR, and a dir never
    // overwrites a file, not even with Overwrite (TypeMismatch, ADR 0005).
    // The invariant this test pins stays intact: the destination is NOT touched.
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Conflict {
                conflict: ConflictKind::TypeMismatch
            }
        }
    );
    assert_eq!(
        read_all(&mem, "mem:///dst/ln").await.unwrap(),
        b"don't delete me",
        "the failure was 100% predictable: the destination is not touched"
    );
}

/// Phase 7: the TUI reads through the core (rule 7) — passthrough with a range.
#[tokio::test]
async fn engine_read_respects_the_range() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"0123456789").await;
    let mut stream = engine
        .read(
            &vp("mem:///f"),
            Some(norte_proto::ByteRange {
                offset: 2,
                len: Some(3),
            }),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(out, b"234");
}

// ---------- phase 8: trash (ADR 0009) ----------

/// Trash is ONE operation: the whole tree disappears, recoverable.
#[tokio::test]
async fn delete_trash_takes_the_whole_tree() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 3).await;
    let handle = engine
        .delete_with(&vp("mem:///src"), norte_proto::DeleteMode::Trash)
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///src")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Without the TRASH capability the engine NEVER degrades: Unsupported and
/// the tree stays intact (degradation is the user's decision, ADR 0009).
#[tokio::test]
async fn delete_trash_without_the_capability_does_not_degrade() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///valuable", b"data").await;
    let handle = engine
        .delete_with(&vp("mem:///valuable"), norte_proto::DeleteMode::Trash)
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Unsupported
        }
    );
    assert_eq!(read_all(&mem, "mem:///valuable").await.unwrap(), b"data");
}

/// Rule 3 for the Trash path (a single op): cancelable BEFORE firing — either
/// cancellation wins (an intact tree) or the trash won (a legitimate race,
/// like in move).
#[tokio::test]
async fn delete_trash_cancel_before_start_leaves_tree_intact() {
    let (engine, mem) = engine_with_mem();
    build_tree(&mem, 3).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(30)));
    let handle = engine
        .delete_with(&vp("mem:///src"), norte_proto::DeleteMode::Trash)
        .await
        .unwrap();
    handle.cancel();
    let state = handle.join().await;
    mem.faults().clear();
    match state {
        TaskState::Cancelled => {
            assert!(mem.stat(&vp("mem:///src")).await.is_ok(), "intact tree");
        }
        TaskState::Completed => {
            assert_eq!(
                mem.stat(&vp("mem:///src")).await.unwrap_err(),
                Error::NotFound,
                "the trash won the race: it went ENTIRELY"
            );
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

/// #51 (rule 3): the `copy_native` path (server-copy: an S3 multipart copy can
/// take minutes) MUST observe cancellation — waiting for the provider to
/// finish is not acceptable. Generous provider latency: if the engine does
/// not race the cancel against `copy_native`, the join returns Completed.
#[tokio::test]
async fn copy_native_is_cancelable_halfway() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"native content").await;
    // A deterministic gate: copy_native stays PENDING (an S3 multipart copy of
    // minutes); nothing else is affected. Only the cancellation ends it.
    mem.faults().hold_copy_native(true);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    // Deterministic synchronization: cancels ONLY once copy_native has
    // already entered — without this, a slow runner would cancel earlier and
    // the test would pass vacuously at the pre-copy checkpoint.
    while !mem.faults().copy_native_entered() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    handle.cancel();
    let state = tokio::time::timeout(std::time::Duration::from_secs(5), handle.join())
        .await
        .expect("the cancellation does not wait for the provider");
    assert_eq!(state, TaskState::Cancelled);
    // CLAUDE.md trap: a clean destination — the drop really stopped the
    // effect (the `MemProvider`'s mutation lives BEHIND the gate).
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "the destination stays clean after cancelling the native copy"
    );
}
