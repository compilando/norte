//! engine↔journal integration (M3-1b): the engine's mutations with a real
//! `SqliteJournal` produce the expected entries, in order and with a valid
//! hash chain. In-memory `MemProvider` → deterministic, no harness.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Engine, Journal, MutationObserver, SqliteJournal};
use norte_proto::{DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Engine with an in-memory journal + `MemProvider` (its default caps include
/// `TRASH`). Returns the `SqliteJournal` to inspect the entries.
async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn MutationObserver>);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

#[tokio::test]
async fn copy_records_created_with_valid_chain() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///src.txt", b"hello").await;

    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///dst.txt");
    assert_eq!(es[0].reversal, "delete");
    assert_eq!(es[0].actor_kind, "user");
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn move_records_renamed() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"x").await;

    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "renamed");
    assert_eq!(es[0].path, b"mem:///b.txt");
    assert_eq!(es[0].path_to.as_deref(), Some(&b"mem:///a.txt"[..]));
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn permanent_delete_records_removed_irreversible() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///gone.txt", b"x").await;

    let h = engine.delete(&vp("mem:///gone.txt")).await.expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    let last = es.last().expect("entry");
    assert_eq!(last.op, "removed");
    assert_eq!(last.reversal, "irreversible");
    assert_eq!(last.reversal_ref, None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn trash_records_trashed_restore() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///t.txt", b"x").await;

    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].reversal, "restore_trash");
    // MemProvider = "vanish" trash: no recoverable path.
    assert_eq!(es[0].reversal_ref, None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

/// #99 — NATIVE trash (dest `None`) after an applied-then-ambiguous mutation
/// degrades to `Ok(None)` (undo without `reversal_ref`) but does NOT fail the
/// task: the item was already trashed, never lost. It used to fail by
/// propagating the ambiguous result.
#[tokio::test]
async fn native_trash_ambiguity_degrades_without_failing() {
    let (engine, mem, journal) = setup().await; // MemProvider vanish (dest None)
    write_file(&mem, "mem:///n.txt", b"x").await;

    mem.faults().ambiguous_mutations(1);
    let h = engine
        .delete_with(&vp("mem:///n.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es[0].op, "trashed");
    assert_eq!(
        es[0].reversal_ref, None,
        "native trash: the undo degrades, the task does NOT fail"
    );
}

/// #99 — a logical trash that APPLIES the move but returns an ambiguous result
/// must not fail the task nor lose the `reversal_ref`: the engine retries with
/// the SAME deterministic id and the provider recovers the payload. Before
/// (without `trash_retrying`) the task failed on the first ambiguous result.
#[tokio::test]
async fn logical_trash_ambiguity_keeps_the_reversal_ref() {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn MutationObserver>);
    let mem = Arc::new(MemProvider::new().with_logical_trash());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///t.txt", b"x").await;

    // The move applies and still returns an ambiguous result (once).
    mem.faults().ambiguous_mutations(1);
    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "trashed");
    let comp_ref = es[0].reversal_ref.clone().expect("reversal_ref preserved");
    assert!(
        String::from_utf8_lossy(&comp_ref).contains(".norte-trash/"),
        "the recoverable payload survives the ambiguous result: {:?}",
        String::from_utf8_lossy(&comp_ref)
    );
}

/// #32.1 — a write COMMIT that APPLIES (rename staging→final) but returns an
/// ambiguous result must not fail the task nor lose the `Created`: it is
/// disambiguated by the destination's presence + size. Before: the retry
/// recopied, its non-replace commit gave a Conflict → the task FAILED with the
/// file correctly copied and no journal event (rule 4).
#[tokio::test]
async fn ambiguous_commit_does_not_lose_the_created() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///src.bin", b"test content").await;
    // The NEXT commit applies its effect and returns an ambiguous result (once).
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .expect("copy");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "the commit applied: the task must not fail over the ambiguous result"
    );
    // The destination exists with the source's size.
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap().size,
        Some(19)
    );
    // And there IS exactly one `Created` (rule 4): undo will know about it.
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1, "a single Created despite the ambiguous commit");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///dst.bin");
}

/// #32.1 with a 0-byte file: `final_size == 0` disambiguates the same way (the
/// destination exists with size 0), a single `Created`.
#[tokio::test]
async fn ambiguous_commit_empty_file() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///empty.bin", b"").await;
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///empty.bin"), &vp("mem:///dst.bin"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mem.stat(&vp("mem:///dst.bin")).await.unwrap().size, Some(0));
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
}

/// #32.2 — ambiguous mkdir: the destination dir's mkdir APPLIES its effect and
/// returns an ambiguous result; the retry sees `Conflict`. Before: under the
/// Fail policy the task FAILED with the dir correctly created, and under merge
/// it completed but WITHOUT the dir's `Created` (M3's undo did not know about
/// it). Now `ensure_dir` pre-stats the destination: if it did NOT preexist,
/// the ambiguous Conflict is our first application — the task completes and
/// the journal records the dir.
#[tokio::test]
async fn ambiguous_mkdir_does_not_lose_the_created() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir src");
    write_file(&mem, "mem:///d/f.bin", b"content").await;
    // The NEXT mutation (the mkdir for mem:///d2) applies and gives an ambiguous result.
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///d"), &vp("mem:///d2"))
        .await
        .expect("copy");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "the mkdir applied: the task must not fail over the ambiguous result"
    );
    // The tree copied entirely.
    assert_eq!(
        mem.stat(&vp("mem:///d2/f.bin")).await.unwrap().size,
        Some(9)
    );
    // And the journal has the DIR's `Created` (rule 4): undo knows about it.
    let es = journal.journal().entries().await.expect("entries");
    let dir_created = es
        .iter()
        .filter(|e| e.op == "created" && e.path == b"mem:///d2")
        .count();
    assert_eq!(
        dir_created, 1,
        "a single Created for the ambiguous dir: {es:?}"
    );
}

/// #32.2 (counterpart): a PREEXISTING destination dir under merge still gets
/// NO `Created` — the pre-stat knows it is not ours and undo will never touch
/// it.
#[tokio::test]
async fn mkdir_over_preexisting_dir_emits_no_created() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir src");
    write_file(&mem, "mem:///d/f.bin", b"content").await;
    mem.mkdir(&vp("mem:///d2")).await.expect("preexisting dst");

    let h = engine
        .copy_with(
            &vp("mem:///d"),
            &vp("mem:///d2"),
            norte_core::TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..Default::default()
            },
        )
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let es = journal.journal().entries().await.expect("entries");
    assert!(
        !es.iter()
            .any(|e| e.op == "created" && e.path == b"mem:///d2"),
        "a preexisting dir never earns a Created: {es:?}"
    );
}

/// #104 `fs.mkdir`: creating A SINGLE dir journals `Created` with undo (rule 4).
#[tokio::test]
async fn mkdir_records_created_with_valid_chain() {
    let (engine, mem, journal) = setup().await;
    let h = engine.mkdir(&vp("mem:///new")).await.expect("mkdir");
    assert_eq!(h.join().await, TaskState::Completed);

    let entry = mem.stat(&vp("mem:///new")).await.expect("stat");
    assert_eq!(entry.kind, norte_proto::EntryKind::Dir);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///new");
    assert_eq!(es[0].reversal, "delete");
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

/// #104: an existing node at the destination is `Conflict` — creating asserts
/// a FREE name, with no silent idempotence — and NEVER journals someone else's
/// Created.
#[tokio::test]
async fn mkdir_over_existing_node_is_conflict_without_created() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///taken", b"x").await;

    let h = engine.mkdir(&vp("mem:///taken")).await.expect("submit");
    assert!(matches!(h.join().await, TaskState::Failed { .. }));
    let es = journal.journal().entries().await.expect("entries");
    assert!(es.is_empty(), "a failure journals nothing");

    // A PREEXISTING dir is not a success either (we are neither mkdir -p nor merge).
    mem.mkdir(&vp("mem:///already"))
        .await
        .expect("direct mkdir");
    let h = engine.mkdir(&vp("mem:///already")).await.expect("submit");
    assert!(matches!(h.join().await, TaskState::Failed { .. }));
}

/// #104: without `-p` — the parent must exist.
#[tokio::test]
async fn mkdir_without_parent_fails() {
    let (engine, _mem, journal) = setup().await;
    let h = engine
        .mkdir(&vp("mem:///does-not-exist/child"))
        .await
        .expect("submit");
    assert!(matches!(h.join().await, TaskState::Failed { .. }));
    assert!(journal.journal().entries().await.expect("e").is_empty());
}
