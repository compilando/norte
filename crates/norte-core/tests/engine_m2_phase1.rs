//! Test-first matrix for M2's phase 1 — the engine's hard debt:
//! #16 real node identity in the guards, #17 mutation retry with
//! post-effect disambiguation, #18 real kind when preserving symlinks,
//! #19 Follow over dir-symlinks with a visited set.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Engine, Mutation, MutationObserver, TransferOptions};
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, CollisionPolicy, DeleteMode, Entry, Error,
    SymlinkPolicy, TaskState, VPath,
};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind};

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

async fn read_all(p: &dyn Provider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = p.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn on_collision(c: CollisionPolicy) -> TransferOptions {
    TransferOptions {
        on_collision: c,
        ..TransferOptions::default()
    }
}

fn follow() -> TransferOptions {
    TransferOptions {
        symlinks: SymlinkPolicy::Follow,
        ..TransferOptions::default()
    }
}

/// An observer that records every mutation (as the journal will in M3): it
/// checks that the retry with disambiguation emits EXACTLY once.
#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<String>>,
}

impl RecordingObserver {
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events lock sane").clone()
    }
}

#[async_trait::async_trait]
impl MutationObserver for RecordingObserver {
    async fn on_mutation(
        &self,
        mutation: &Mutation<'_>,
        _actor: &norte_core::journal::Actor,
    ) -> Result<(), norte_proto::Error> {
        let repr = match mutation {
            Mutation::Created { path, .. } => format!("created:{}", path.display_lossy()),
            Mutation::Removed(p) => format!("removed:{}", p.display_lossy()),
            Mutation::Trashed { path, .. } => format!("trashed:{}", path.display_lossy()),
            Mutation::Renamed { from, to, .. } => {
                format!("renamed:{}>{}", from.display_lossy(), to.display_lossy())
            }
            // #314: with the PREVIOUS mode inside, which is what the reversal
            // needs and the one thing an observer cannot reconstruct. The
            // batch (#315) does NOT enter the representation: what these
            // tests look at is which mutation was emitted, and including the
            // id would make every expectation depend on how many batches came
            // before.
            Mutation::ModeChanged { path, from, to, .. } => format!(
                "mode:{}:{}>{to:o}",
                path.display_lossy(),
                from.map_or_else(|| "?".to_owned(), |m| format!("{m:o}")),
            ),
        };
        self.events.lock().expect("events lock sane").push(repr);
        Ok(())
    }
}

fn engine_recording(mem: &Arc<MemProvider>) -> (Engine, Arc<RecordingObserver>) {
    let observer = Arc::new(RecordingObserver::default());
    let engine = Engine::with_observer(Arc::clone(&observer) as Arc<dyn MutationObserver>);
    engine.register_provider(Arc::clone(mem) as Arc<dyn Provider>);
    (engine, observer)
}

/// A provider that DELEGATES everything to a Mem but with its own scheme and
/// capabilities. Two uses: (a) announce a case-insensitive box over a
/// case-sensitive Mem — the WSL/NTFS case where the case heuristic LIES and
/// only real identity (#16) answers correctly; (b) a different scheme to
/// force the cross-provider path (like `engine.rs`'s `Alias`).
struct CapsMask {
    inner: Arc<MemProvider>,
    scheme: &'static str,
    flags: CapabilityFlags,
}

#[async_trait]
impl Provider for CapsMask {
    fn scheme(&self) -> &str {
        self.scheme
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            flags: self.flags,
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.inner.list(p).await
    }
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        self.inner.read(p, range).await
    }
    async fn node_id(&self, p: &VPath, follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        self.inner.node_id(p, follow).await
    }
    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.inner.read_link(p).await
    }
    async fn symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind) -> Result<(), Error> {
        self.inner.symlink(link, target, kind).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
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

// ---------- #16: real identity in the anti-self-destruction guard ----------

/// The FS announces case-insensitive but DISTINGUISHES these two names (WSL
/// over case-sensitive NTFS, exotic folds): real identity says they are
/// DIFFERENT nodes and the Overwrite copy must PROCEED — the `to_lowercase`
/// heuristic blocked them as a false positive.
#[tokio::test]
async fn overwrite_between_case_variants_the_fs_distinguishes_proceeds() {
    let engine = Engine::new();
    let inner = Arc::new(MemProvider::new()); // really case-SENSITIVE
    write_file(&inner, "mem:///HOUSE", b"upper").await;
    write_file(&inner, "mem:///house", b"lower").await;
    let masked = Arc::new(CapsMask {
        inner: Arc::clone(&inner),
        scheme: "mem",
        // Announces insensitive (without CASE_SENSITIVE): the heuristic would suspect.
        flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    });
    engine.register_provider(masked as Arc<dyn Provider>);

    let handle = engine
        .copy_with(
            &vp("mem:///HOUSE"),
            &vp("mem:///house"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*inner, "mem:///house").await.unwrap(), b"upper");
    assert_eq!(read_all(&*inner, "mem:///HOUSE").await.unwrap(), b"upper");
}

/// Without identity (the `without_node_ids` provider), M1's conservative
/// heuristic is still in force: the case variant onto itself is rejected.
#[tokio::test]
async fn without_identity_the_conservative_heuristic_still_blocks() {
    let engine = Engine::new();
    let mem = Arc::new(
        MemProvider::with_flags(CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING)
            .without_node_ids(),
    );
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
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    assert_eq!(read_all(&*mem, "mem:///unique").await.unwrap(), b"precious");
}

// ---------- #17: mutation retry with disambiguation ----------

/// The PRE-effect transient failure in a mutation is now retried (M1 only
/// retried reads): a delete with the provider flickering ends up fine.
#[tokio::test]
async fn a_mutation_with_a_pre_effect_transient_failure_is_retried() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///victim", b"x").await;
    mem.faults().unavailable_for_next(1);

    let handle = engine
        .delete_with(&vp("mem:///victim"), DeleteMode::Permanent)
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///victim")).await.unwrap_err(),
        Error::NotFound
    );
}

/// POST-effect timeout in remove: the retry sees `NotFound` and reads it as
/// "the effect applied" — success, and the journal gets a SINGLE Removed.
#[tokio::test]
async fn an_ambiguous_remove_is_disambiguated_as_success() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///victim", b"x").await;
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .delete_with(&vp("mem:///victim"), DeleteMode::Permanent)
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///victim")).await.unwrap_err(),
        Error::NotFound
    );
    let removed: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("removed:"))
        .collect();
    assert_eq!(removed.len(), 1, "ONE Removed, never zero or two");
}

/// POST-effect timeout in the destination tree's mkdir, with a policy that
/// allows merge: the retry's Conflict is absorbed as a merge and the copy
/// finishes.
#[tokio::test]
async fn an_ambiguous_mkdir_under_merge_completes_the_copy() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"data").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy_with(
            &vp("mem:///src"),
            &vp("mem:///dst"),
            on_collision(CollisionPolicy::Overwrite),
        )
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// The same post-effect timeout under `Fail`: with #32.2's pre-stat the
/// engine no longer guesses — it KNOWS the destination did not preexist, so
/// the retry's Conflict is our first application: the copy COMPLETES (before:
/// a fail-safe Conflict with the dir correctly created and the task failed).
#[tokio::test]
async fn an_ambiguous_mkdir_under_fail_completes() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    write_file(&mem, "mem:///src/f", b"data").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// POST-effect timeout while preserving a symlink: the retry sees `Conflict`,
/// verifies through `read_link` that the link is OURS (same target) and calls
/// it created. ONE Created for the journal.
#[tokio::test]
async fn an_ambiguous_symlink_preserve_is_disambiguated() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///target", b"x").await;
    mem.symlink(&vp("mem:///ln"), b"target", SymlinkKind::File)
        .await
        .unwrap();
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .copy(&vp("mem:///ln"), &vp("mem:///ln2"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(mem.read_link(&vp("mem:///ln2")).await.unwrap(), b"target");
    let created: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("created:"))
        .collect();
    assert_eq!(created.len(), 1, "ONE Created for the journal");
}

/// POST-effect timeout in rename (same-provider move): the retry sees
/// `NotFound` at the source, verifies through `node_id` that the destination
/// IS the original node and calls it renamed. ONE Renamed for the journal.
#[tokio::test]
async fn an_ambiguous_rename_is_disambiguated_as_success() {
    let mem = Arc::new(MemProvider::new());
    write_file(&mem, "mem:///before", b"content").await;
    let (engine, observer) = engine_recording(&mem);
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .move_(&vp("mem:///before"), &vp("mem:///after"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///after").await.unwrap(), b"content");
    assert_eq!(
        mem.stat(&vp("mem:///before")).await.unwrap_err(),
        Error::NotFound
    );
    let renamed: Vec<_> = observer
        .events()
        .into_iter()
        .filter(|e| e.starts_with("renamed:"))
        .collect();
    assert_eq!(renamed.len(), 1, "ONE Renamed for the journal");
}

/// Without node identity an ambiguous rename CANNOT be verified: the engine
/// does not guess — the original transient error surfaces (fail-safe; the
/// effect may have applied and the user retries against the real state).
#[tokio::test]
async fn an_ambiguous_rename_without_identity_does_not_guess() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().without_node_ids());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///before", b"content").await;
    mem.faults().ambiguous_mutations(1);

    let handle = engine
        .move_(&vp("mem:///before"), &vp("mem:///after"))
        .await
        .unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::ProviderUnavailable { .. },
        } => {}
        other => panic!("expected the original transient error, was {other:?}"),
    }
    // No loss: the content is SOMEWHERE (here: already renamed).
    assert_eq!(read_all(&*mem, "mem:///after").await.unwrap(), b"content");
}

// ---------- #18: real kind when preserving symlinks ----------

/// Preserve of a dir-symlink: the kind that reaches the destination provider
/// is no longer hardcoded `File` — the destination resolves it (Unknown) and
/// the link ends up as a DIR-symlink. The target ("asub") is copied BEFORE
/// the link ("zln") by walk order: the resolution finds it.
#[tokio::test]
async fn preserve_resolves_the_dir_symlinks_kind() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/asub")).await.unwrap();
    mem.symlink(&vp("mem:///src/zln"), b"asub", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.symlink_kind_of(&vp("mem:///dst/zln")),
        Some(SymlinkKind::Dir),
        "the kind was resolved against the real target, not a blind File"
    );
}

// ---------- #19: Follow over dir-symlinks ----------

/// Follow expands a dir-symlink as a REAL directory at the destination, with
/// its content copied (`cp -RL` semantics). In M1 this was Unsupported.
#[tokio::test]
async fn follow_expands_a_dir_symlink_as_a_directory() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/at/f").await.unwrap(), b"data");
    assert_eq!(read_all(&*mem, "mem:///dst/zln/f").await.unwrap(), b"data");
    let e = mem.stat(&vp("mem:///dst/zln")).await.unwrap();
    assert_eq!(
        e.kind,
        norte_proto::EntryKind::Dir,
        "the expanded link is a REAL dir"
    );
}

/// Copying the ROOT dir-symlink with Follow: the destination is the target's
/// tree, as a real dir.
#[tokio::test]
async fn follow_of_a_root_dir_symlink_copies_the_tree() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///at")).await.unwrap();
    write_file(&mem, "mem:///at/f", b"data").await;
    mem.symlink(&vp("mem:///zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///zln"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/f").await.unwrap(), b"data");
}

/// A symlink cycle under Follow fails CLEANLY (a visited set by `node_id`,
/// spec §17.9): never infinite recursion nor a hang.
#[tokio::test]
async fn follow_a_symlink_cycle_fails_cleanly() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/d")).await.unwrap();
    // Empty target: resolves to the link's PARENT (src/d) — cycle d → d.
    mem.symlink(&vp("mem:///src/d/loop"), b"", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed { error: Error::Loop },
        "cycle detected and rejected with its own category (#31)"
    );
}

/// A DAG (two paths to the same dir WITHOUT a cycle) is NOT a cycle: it is
/// copied twice, like `cp -RL`.
#[tokio::test]
async fn follow_a_dag_without_a_cycle_copies_twice() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/y1"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();
    mem.symlink(&vp("mem:///src/y2"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst/y1/f").await.unwrap(), b"data");
    assert_eq!(read_all(&*mem, "mem:///dst/y2/f").await.unwrap(), b"data");
}

/// Follow over a dir-symlink on a provider WITHOUT node identity: no visited
/// set possible → Unsupported (M1's behavior is preserved exactly where
/// there is no safety net).
#[tokio::test]
async fn follow_without_identity_is_unsupported() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().without_node_ids());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///at")).await.unwrap();
    mem.symlink(&vp("mem:///zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(&vp("mem:///zln"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::Unsupported
        }
    );
}

/// Move with Follow: phase 2's delete removes the LINK, never the target's
/// content THROUGH the link (that would be a loss outside the tree being
/// moved). The source ends up completely empty; the destination, expanded.
#[tokio::test]
async fn move_follow_does_not_delete_through_the_link() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    write_file(&mem, "mem:///src/at/f", b"data").await;
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .move_with(&vp("mem:///src"), &vp("mem:///dst2"), follow())
        .await
        .unwrap();
    // Same provider: the rename wins and there is no expansion — forcing the
    // copy+delete path with a CROSS-provider destination would be the pure
    // case; here it is enough to check the move finished without touching
    // anything extra.
    assert_eq!(handle.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///src")).await.is_err(), "source moved");
}

/// The PURE case of move+Follow (cross-provider copy+delete): the link
/// expands at the destination; at the SOURCE the link is deleted AS A LINK —
/// its content is never walked to delete it (that would be a loss through the
/// link) nor is anything left behind.
#[tokio::test]
async fn move_follow_cross_provider_expands_and_deletes_only_the_link() {
    let engine = Engine::new();
    let src_tree = Arc::new(MemProvider::new());
    let dst_tree = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(CapsMask {
        inner: Arc::clone(&src_tree),
        scheme: "src",
        flags: MemProvider::new().capabilities().flags,
    }) as Arc<dyn Provider>);
    engine.register_provider(Arc::clone(&dst_tree) as Arc<dyn Provider>);

    src_tree.mkdir(&vp("src:///m")).await.unwrap();
    src_tree.mkdir(&vp("src:///m/at")).await.unwrap();
    write_file(&src_tree, "src:///m/at/f", b"data").await;
    src_tree
        .symlink(&vp("src:///m/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .move_with(&vp("src:///m"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // Destination: really expanded (at real + zln expanded as a real dir).
    assert_eq!(
        read_all(&*dst_tree, "mem:///dst/at/f").await.unwrap(),
        b"data"
    );
    assert_eq!(
        read_all(&*dst_tree, "mem:///dst/zln/f").await.unwrap(),
        b"data"
    );
    // Source: EVERYTHING gone (at, its content and the link — deleted as a link).
    assert_eq!(
        src_tree.stat(&vp("src:///m")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Cancellation during a Follow copy: clean, no hang (rule 3 also applies to
/// the walk with expansion).
// Paused clock: `MemProvider`'s per-op latency runs on tokio's clock, so
// "sleep and cancel" stops being a race with the machine.
#[tokio::test(start_paused = true)]
async fn follow_cancellation_during_the_walk_is_clean() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    for i in 0..50 {
        write_file(&mem, &format!("mem:///src/at/f{i}"), b"data").await;
    }
    mem.symlink(&vp("mem:///src/zln"), b"at", SymlinkKind::Dir)
        .await
        .unwrap();
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(5)));

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// A HIGH finding from the encoding-auditor: copying a symlink OVER its own
/// target with Follow+Overwrite would destroy the target (remove before
/// reading through the link). The guard compares the source's RESOLVED
/// identity.
#[tokio::test]
async fn follow_overwrite_of_a_link_over_its_target_does_not_destroy() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///f", b"precious").await;
    mem.symlink(&vp("mem:///ln"), b"f", SymlinkKind::File)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///ln"),
            &vp("mem:///f"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
                ..TransferOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(read_all(&*mem, "mem:///f").await.unwrap(), b"precious");
}

/// The dir variant of the same finding: expanding a dir-symlink over its own
/// target dir.
#[tokio::test]
async fn follow_overwrite_of_a_dir_link_over_its_target_does_not_destroy() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///d")).await.unwrap();
    write_file(&mem, "mem:///d/child", b"precious").await;
    mem.symlink(&vp("mem:///ln"), b"d", SymlinkKind::Dir)
        .await
        .unwrap();

    let handle = engine
        .copy_with(
            &vp("mem:///ln"),
            &vp("mem:///d"),
            TransferOptions {
                on_collision: CollisionPolicy::Overwrite,
                symlinks: SymlinkPolicy::Follow,
                ..TransferOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    assert_eq!(
        read_all(&*mem, "mem:///d/child").await.unwrap(),
        b"precious"
    );
}

/// Follow's expansion loses no BYTES: a non-UTF-8 link name and content with
/// hostile names (NFD, control) arrive byte-exact at the destination under
/// the expanded link's path.
#[tokio::test]
async fn follow_expands_with_hostile_names_byte_exact() {
    let (engine, mem) = engine_with_mem();
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).expect("valid hostile segment");
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/at")).await.unwrap();
    // Hostile children: NFD (e + combining) and a line break.
    let nfd: &[u8] = b"e\xCC\x81.txt";
    let ctrl: &[u8] = b"a\nb";
    for name in [nfd, ctrl] {
        let p = vp("mem:///src/at").join(seg(name));
        let mut sink = mem.write(&p).await.expect("hostile write");
        sink.write(Bytes::from_static(b"data")).await.unwrap();
        sink.commit().await.unwrap();
    }
    // A link with a non-UTF-8 name (raw latin1 é).
    let link_name: &[u8] = b"z\xE9ln";
    let link = vp("mem:///src").join(seg(link_name));
    mem.symlink(&link, b"at", SymlinkKind::Dir).await.unwrap();

    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), follow())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    for name in [nfd, ctrl] {
        let expanded = vp("mem:///dst").join(seg(link_name)).join(seg(name));
        let e = mem.stat(&expanded).await.unwrap_or_else(|err| {
            panic!("missing {name:?} under the expanded link: {err:?}");
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            name,
            "bytes intact under the expansion"
        );
    }
}

/// Cancelling DURING a mutation retry's backoff answers fast (rule 3): it
/// never waits out the retries.
// Paused clock: `MemProvider`'s per-op latency runs on tokio's clock, so
// "sleep and cancel" stops being a race with the machine.
#[tokio::test(start_paused = true)]
async fn cancellation_during_a_mutations_backoff_is_fast() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///victim", b"x").await;
    // Plenty of transients: without cancellation it would take 100+200+400 ms.
    mem.faults().unavailable_for_next(10);

    let start = std::time::Instant::now();
    let handle = engine
        .delete_with(&vp("mem:///victim"), DeleteMode::Permanent)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "the cancellation does not wait out the backoff"
    );
}
