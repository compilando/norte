//! Session undo integration (M3-2): `Engine::undo_session` undoes in strict
//! LIFO order (never overwrites), with append-only compensations.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::journal::Actor;
use norte_core::{Engine, Journal, Reversal, SqliteJournal, UndoReport};
use norte_proto::{DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

async fn write_file(mem: &MemProvider, w: &str, c: &[u8]) {
    let mut s = mem.write(&vp(w)).await.expect("open");
    s.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    s.commit().await.expect("commit");
}

async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

async fn run_undo(engine: &Engine, actor: Actor) -> (TaskState, UndoReport) {
    let (h, report) = engine.undo_session(actor).await.expect("undo submit");
    let state = h.join().await;
    let r = report.lock().expect("lock").clone();
    (state, r)
}

/// **Undoing a SPELLING change goes back to the previous name** (#274).
///
/// The source "is occupied" always: on the folding volume — the only one
/// where that rename happens — `stat("Foo.txt")` finds the `foo.txt` that was
/// just created, so the "free" check said no and undo was guaranteed to
/// block. And a `Blocked` strangles LIFO: it strands everything earlier in
/// the session.
///
/// What breaks the tie is identity: what occupies the source IS the node
/// being returned.
#[tokio::test]
async fn undoing_a_spelling_change_goes_back_to_the_previous_name() {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(
        MemProvider::with_flags(
            norte_proto::CapabilityFlags::RENAME_ATOMIC
                | norte_proto::CapabilityFlags::CASE_PRESERVING,
        )
        .with_folding_noreplace(),
    );
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///Foo.txt", b"hola").await;

    let h = engine
        .move_(&vp("mem:///Foo.txt"), &vp("mem:///foo.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1, "and not BLOCKED: {r:?}");
    assert!(
        mem.stat(&vp("mem:///Foo.txt")).await.is_ok(),
        "it is named the way it was named again"
    );
    let left: Vec<Vec<u8>> = {
        let mut s = mem.list(&vp("mem:///")).await.expect("list");
        let mut out = Vec::new();
        while let Some(e) = s.next().await {
            out.push(
                e.expect("entry")
                    .path
                    .file_name()
                    .expect("leaf")
                    .as_bytes()
                    .to_vec(),
            );
        }
        out
    };
    assert!(
        !left.iter().any(|n| n.starts_with(b".norte-rename-")),
        "no detour residue: {left:?}"
    );
}

/// **`fs.create` creates an EMPTY file, and undoing it deletes it** (#290).
///
/// It is a mutation like any other: journal `Created` with its reversal. What
/// this test pins down is that there is no exception for being small.
#[tokio::test]
async fn creating_an_empty_file_undoes() {
    let (engine, mem, _j) = setup().await;
    let h = engine
        .create_file(&vp("mem:///new.txt"))
        .await
        .expect("create");
    assert_eq!(h.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///new.txt")).await.expect("exists");
    assert_eq!(e.size, Some(0), "and EMPTY: creating invents no content");

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(matches!(
        mem.stat(&vp("mem:///new.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

/// And it NEVER overwrites what is there.
///
/// There is no reading of "create" that means "empty out what is there", and a
/// method that silently truncates is data loss with an innocent name.
#[tokio::test]
async fn creating_over_something_that_exists_fails_without_touching_it() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///taken.txt", b"content that matters").await;
    let h = engine
        .create_file(&vp("mem:///taken.txt"))
        .await
        .expect("submit");
    assert!(
        matches!(
            h.join().await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict { .. }
            }
        ),
        "a taken name is a conflict"
    );
    let e = mem
        .stat(&vp("mem:///taken.txt"))
        .await
        .expect("still there");
    assert_eq!(
        e.size,
        Some(21),
        "and with its bytes intact: the failure emptied nothing"
    );
}

#[tokio::test]
async fn undo_deletes_created() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(r.blocked.is_none());
    assert!(matches!(
        mem.stat(&vp("mem:///dst.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_restores_renamed() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.txt", b"x").await;
    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(
        mem.stat(&vp("mem:///a.txt")).await.is_ok(),
        "source restored"
    );
    assert!(matches!(
        mem.stat(&vp("mem:///b.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_lifo_reverts_all_then_double_undo_is_noop() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///s.txt", b"x").await;
    for d in ["mem:///a", "mem:///b", "mem:///c"] {
        let h = engine
            .copy(&vp("mem:///s.txt"), &vp(d))
            .await
            .expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    let (_s, r1) = run_undo(&engine, Actor::User).await;
    assert_eq!(r1.undone, 3);
    for d in ["mem:///a", "mem:///b", "mem:///c"] {
        assert!(matches!(
            mem.stat(&vp(d)).await,
            Err(norte_proto::Error::NotFound)
        ));
    }
    // 2nd undo: everything already compensated → 0.
    let (_s2, r2) = run_undo(&engine, Actor::User).await;
    assert_eq!(r2.undone, 0);
}

#[tokio::test]
async fn undo_blocks_when_target_occupied_by_foreign_state() {
    let (engine, mem, journal) = setup().await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    // FS state coherent with a Renamed a→b by the agent: b exists, a does not.
    write_file(&mem, "mem:///b.txt", b"x").await;
    journal
        .journal()
        .record(
            "renamed",
            b"mem:///b.txt",
            Some(b"mem:///a.txt"),
            Reversal::RenameBack,
            None,
            &agent,
        )
        .await
        .expect("seed");
    // Drift: someone occupies the source `a` (NOT in the agent's session).
    write_file(&mem, "mem:///a.txt", b"taken").await;

    let (state, r) = run_undo(&engine, agent).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert!(
        matches!(r.blocked, Some((_, norte_proto::Error::Conflict { .. }))),
        "an occupied source blocks, {:?}",
        r.blocked
    );
    // Nothing overwritten: both intact.
    assert!(mem.stat(&vp("mem:///a.txt")).await.is_ok());
    assert!(mem.stat(&vp("mem:///b.txt")).await.is_ok());
}

#[tokio::test]
async fn undo_skips_irreversible_permanent_delete() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///keep.txt", b"x").await;
    write_file(&mem, "mem:///gone.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///keep.txt"), &vp("mem:///copy.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let h = engine.delete(&vp("mem:///gone.txt")).await.expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1, "undoes the copy");
    assert_eq!(r.skipped_irreversible, 1, "skips the permanent delete");
    assert!(r.blocked.is_none(), "irreversible does NOT block");
    assert!(matches!(
        mem.stat(&vp("mem:///copy.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_filters_by_actor() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///u.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///u.txt"), &vp("mem:///user_copy.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    // The agent's Created (seeded; coherent FS).
    write_file(&mem, "mem:///agent_made.txt", b"x").await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    journal
        .journal()
        .record(
            "created",
            b"mem:///agent_made.txt",
            None,
            Reversal::Delete,
            None,
            &agent,
        )
        .await
        .expect("seed");

    let (_s, r) = run_undo(&engine, agent).await;
    assert_eq!(r.undone, 1);
    assert!(matches!(
        mem.stat(&vp("mem:///agent_made.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
    assert!(
        mem.stat(&vp("mem:///user_copy.txt")).await.is_ok(),
        "did not touch the User's"
    );
}

#[tokio::test]
async fn undo_trashed_logical_restores_from_dest() {
    // Seeded logical trash: a Trashed with reversal_ref = the payload's path.
    let (engine, mem, journal) = setup().await;
    // Coherent FS: the original does NOT exist; the payload in the trash DOES.
    mem.mkdir(&vp("mem:///.trash")).await.expect("mkdir trash");
    mem.mkdir(&vp("mem:///.trash/1")).await.expect("mkdir id");
    write_file(&mem, "mem:///.trash/1/v.txt", b"payload").await;
    journal
        .journal()
        .record(
            "trashed",
            b"mem:///v.txt",
            None,
            Reversal::RestoreTrash,
            Some(b"mem:///.trash/1/v.txt"),
            &Actor::User,
        )
        .await
        .expect("seed");

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert_eq!(
        // Restored to the original from the payload.
        {
            let mut s = mem.read(&vp("mem:///v.txt"), None).await.expect("read");
            let mut out = Vec::new();
            while let Some(c) = s.next().await {
                out.extend_from_slice(&c.expect("chunk"));
            }
            out
        },
        b"payload"
    );
    assert!(
        matches!(
            mem.stat(&vp("mem:///.trash/1/v.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "the payload was moved out of the trash"
    );
}

#[tokio::test]
async fn undo_cancellation_is_clean() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///s.txt", b"x").await;
    // 4 Created to have work to cancel halfway through.
    for d in ["mem:///a", "mem:///b", "mem:///c", "mem:///d"] {
        let h = engine
            .copy(&vp("mem:///s.txt"), &vp(d))
            .await
            .expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    // Per-op latency → a deterministic window to cancel before finishing.
    mem.faults()
        // The clock cannot be paused here: sqlx's journal pool times out
        // (`PoolTimedOut`) when tokio fast-forwards time. A wide window
        // instead: 200 ms per op against a 15 ms wait, 50x of margin.
        .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

    let (h, report) = engine.undo_session(Actor::User).await.expect("submit");
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    h.cancel();
    let state = h.join().await;

    assert_eq!(state, TaskState::Cancelled, "clean cooperative cut");
    let r = report.lock().expect("lock").clone();
    assert!(
        r.undone < 4,
        "cancelled before finishing (undone={})",
        r.undone
    );
    assert!(r.blocked.is_none(), "cancellation is not a block");
    // Coherence: the cut is BETWEEN entries (each step is an op+compensation
    // per entry), never halfway through one — the chain stays intact.
    mem.faults().set_latency_per_op(None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
        "hash chain intact after cancelling"
    );
}

#[tokio::test]
async fn undo_without_journal_is_unsupported() {
    let engine = Engine::new(); // no-op observer, no journal
    let err = engine
        .undo_session(Actor::User)
        .await
        .err()
        .expect("no journal");
    assert!(matches!(err, norte_proto::Error::Unsupported));
}

#[tokio::test]
async fn undo_delete_mode_trash_then_restore_via_engine() {
    // MemProvider trashes with "vanish" (dest=None) → restore_trashed default
    // Unsupported → undo BLOCKS cleanly (no native trash to query).
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///t.txt", b"x").await;
    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert!(
        matches!(r.blocked, Some((_, norte_proto::Error::Unsupported))),
        "MemProvider vanish: no native restore, it blocks, {:?}",
        r.blocked
    );
}

/// M3-4 T2: a HUMAN undoes the session of an agent whose scope no longer
/// exists (expired / never renewed). The target selects the entries; the
/// EXECUTOR (User, allow-all) passes the gate and signs the compensations —
/// without the split, the undo would die at the agent's own `out-of-scope`.
#[tokio::test]
async fn undo_of_an_agents_session_run_by_a_human() {
    use norte_core::approval::DenyAll;
    use norte_core::{PolicyConfig, ScopeRegistry, ScopedPolicy};
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    // A REAL policy installed and an EMPTY scope registry: the agent is
    // outside every scope (as if its TTL had expired).
    let engine = Engine::with_journal(Arc::clone(&journal)).with_policy(
        Arc::new(ScopedPolicy::new(
            ScopeRegistry::new(),
            PolicyConfig::default(),
        )),
        Arc::new(DenyAll),
    );
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    // The agent's mutation seeded (the pattern from undo_filters_by_actor).
    write_file(&mem, "mem:///agent_made.txt", b"x").await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    journal
        .journal()
        .record(
            "created",
            b"mem:///agent_made.txt",
            None,
            Reversal::Delete,
            None,
            &agent,
        )
        .await
        .expect("seed");

    // Sanity: the agent without scope CANNOT undo itself. Since #171 that is
    // a `denied` row and not a `blocked` — policy is asked unit by unit,
    // inside the Task — but the effect on the tree is the same: nothing is
    // touched.
    let (h, report) = engine
        .undo_session(agent.clone())
        .await
        .expect("submit self-undo");
    let _ = h.join().await;
    let r = report.lock().expect("lock").clone();
    assert_eq!(r.denied_total, 1, "out-of-scope denies the self-undo");
    assert!(r.blocked.is_none(), "and it is not a block from drift");
    assert_eq!(r.undone, 0);
    assert!(mem.stat(&vp("mem:///agent_made.txt")).await.is_ok());

    // The human undoes the agent's session: target=agent, executor=User.
    let (h, report) = engine
        .undo_session_for(&agent, Actor::User)
        .await
        .expect("submit human undo");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();
    assert_eq!(r.undone, 1);
    assert!(r.blocked.is_none());
    assert!(matches!(
        mem.stat(&vp("mem:///agent_made.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // The compensation is signed by the EXECUTOR (User), not the agent.
    let entries = journal.journal().entries().await.expect("entries");
    let comp = entries
        .iter()
        .find(|e| e.undoes_seq.is_some())
        .expect("there is a compensation");
    assert_eq!(comp.actor_kind, "user", "signed by the human executor");
}

/// #65: the reversal of a `Created` on a provider WITHOUT the `TRASH` cap
/// (sftp/object with `logical_trash` OFF — the common remote case) does NOT
/// fall back to permanent delete: it is SKIPPED with its own counter and the
/// node stays. Without `node_id` in `Created`, "what lives at that path today"
/// can be human work done after the creation; undo never destroys it
/// unrecoverably.
#[tokio::test]
async fn undo_created_without_trash_is_skipped_not_permanently_deleted() {
    use norte_proto::CapabilityFlags;
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    // Like MemProvider::new() but WITHOUT TRASH.
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC
            | CapabilityFlags::CASE_SENSITIVE
            | CapabilityFlags::CASE_PRESERVING
            | CapabilityFlags::SYMLINKS,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert_eq!(
        r.skipped_created_no_trash, 1,
        "the permanent reversal is skipped and counted"
    );
    assert!(
        r.blocked.is_none(),
        "skipping is not blocking: LIFO continues"
    );
    assert!(
        mem.stat(&vp("mem:///dst.txt")).await.is_ok(),
        "the created node STAYS: never permanently deleted by undo"
    );
}

/// Order matters (#65): a DRIFT (the created node is no longer there) ALWAYS
/// blocks, even without the `TRASH` cap — classifying it as a skip would
/// swallow strict mode's divergence signal.
#[tokio::test]
async fn undo_created_without_trash_with_drift_blocks() {
    use norte_proto::CapabilityFlags;
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC
            | CapabilityFlags::CASE_SENSITIVE
            | CapabilityFlags::CASE_PRESERVING,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    // Drift: someone removed the node outside of undo.
    mem.remove(&vp("mem:///dst.txt")).await.expect("remove");

    let (_state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(r.skipped_created_no_trash, 0, "drift is NOT a skip");
    let (_seq, err) = r.blocked.expect("blocks on the drift");
    assert!(matches!(err, norte_proto::Error::NotFound));
}

/// **Organizing undoes ENTIRELY: files come back and the folders it created
/// disappear** (phase 8).
///
/// This is the property that requires `fs.organize` to be one method and not
/// N calls from the client. With the `fs.create`s outside the batch, undoing
/// would return the files and leave a tree of empty directories the human did
/// not make — and that they would have to delete by hand without knowing
/// which ones were theirs.
#[tokio::test]
async fn organizing_undoes_entirely_with_its_folders() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/invoice.pdf", b"x").await;
    write_file(&mem, "mem:///d/note.txt", b"y").await;

    let moves = vec![
        norte_proto::methods::OrganizeMove {
            current: "invoice.pdf".to_owned(),
            proposed_rel: "invoices/2026/march.pdf".to_owned(),
        },
        norte_proto::methods::OrganizeMove {
            current: "note.txt".to_owned(),
            proposed_rel: "notes/note.txt".to_owned(),
        },
    ];
    let dir = vp("mem:///d");
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &moves).expect("valid plan");
    let h = engine
        .organize(&dir, &moves, plan.hash(), Actor::User)
        .await
        .expect("organize");
    assert_eq!(h.join().await, TaskState::Completed);

    assert!(
        mem.stat(&vp("mem:///d/invoices/2026/march.pdf"))
            .await
            .is_ok()
    );
    assert!(mem.stat(&vp("mem:///d/notes/note.txt")).await.is_ok());
    assert!(matches!(
        mem.stat(&vp("mem:///d/invoice.pdf")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // And now entirely back.
    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert!(r.blocked.is_none(), "nothing should block: {:?}", r.blocked);

    assert!(
        mem.stat(&vp("mem:///d/invoice.pdf")).await.is_ok(),
        "the file goes back to its place"
    );
    assert!(mem.stat(&vp("mem:///d/note.txt")).await.is_ok());
    for folder in [
        "mem:///d/invoices/2026",
        "mem:///d/invoices",
        "mem:///d/notes",
    ] {
        assert!(
            matches!(
                mem.stat(&vp(folder)).await,
                Err(norte_proto::Error::NotFound)
            ),
            "folder {folder} was created by the batch, so undo takes it down"
        );
    }

    // And the batch's entries share a `batch_id`: that is what makes them ONE
    // undoable unit, and without it none of the above holds.
    let rows = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page");
    let from_batch: Vec<_> = rows
        .iter()
        .filter(|e| e.entry.undoes_seq.is_none())
        .collect();
    let first = from_batch[0].entry.batch_id;
    assert!(first.is_some(), "the batch has an id");
    assert!(
        from_batch.iter().all(|e| e.entry.batch_id == first),
        "the folders and the moves go in the SAME batch"
    );
}

/// A destination that escapes the directory is not even attempted: the whole
/// plan is rejected before creating a single folder.
#[tokio::test]
async fn organizing_rejects_a_destination_that_escapes() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/a.txt", b"x").await;
    let dir = vp("mem:///d");
    let good = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "sub/a.txt".to_owned(),
    }];
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &good).expect("plan");

    let bad = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "../outside.txt".to_owned(),
    }];
    let Err(err) = engine.organize(&dir, &bad, plan.hash(), Actor::User).await else {
        panic!("a `..` is not applied");
    };
    assert!(matches!(err, norte_proto::Error::InvalidPath));
    assert!(
        mem.stat(&vp("mem:///d/a.txt")).await.is_ok(),
        "and nothing was touched"
    );
}

/// A `plan_hash` that is not the one from the reviewed plan is rejected: what
/// gets applied has to be what a human read.
#[tokio::test]
async fn organizing_rejects_a_plan_that_is_not_the_reviewed_one() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/a.txt", b"x").await;
    let dir = vp("mem:///d");
    let reviewed = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "sub/a.txt".to_owned(),
    }];
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &reviewed).expect("plan");

    // The human approved "to sub/", and what arrives moves somewhere else.
    let other = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "other/a.txt".to_owned(),
    }];
    let Err(err) = engine
        .organize(&dir, &other, plan.hash(), Actor::User)
        .await
    else {
        panic!("the token is the reviewed plan's");
    };
    assert!(matches!(err, norte_proto::Error::PlanStale));
}

/// **Undoing UP TO A POINT leaves what came before intact** (phase 7,
/// `journal.undo_after`).
///
/// This is the property the whole timeline depends on: the human points at a
/// row and says "go back here". If the cut moved one entry backward, undo
/// would take down a mutation the human was looking at and wanted to keep —
/// and the journal does not tell a requested compensation apart from one
/// requested by mistake.
#[tokio::test]
async fn undoing_up_to_a_point_respects_what_came_before() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///one.txt", b"1").await;
    write_file(&mem, "mem:///two.txt", b"2").await;

    // Two human copies, one after the other.
    for (src, dst) in [
        ("mem:///one.txt", "mem:///one.copy"),
        ("mem:///two.txt", "mem:///two.copy"),
    ] {
        let h = engine.copy(&vp(src), &vp(dst)).await.expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }

    // The cut: the FIRST copy's `seq`. It is kept.
    let seq_first = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .last()
        .expect("there are entries")
        .entry
        .seq;

    let (h, report) = engine
        .undo_after(seq_first, None)
        .await
        .expect("undo submit");
    let id = h.id();
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();

    // The engine retains it for `policy.undo_report`, and releases it when the
    // daemon fails to deliver the id (OVERLOADED): a report for a task nobody
    // received has nobody to serve it to.
    assert_eq!(engine.undo_report(id).map(|(_, r)| r.undone), Some(1));
    engine.forget_undo_report(id);
    assert!(engine.undo_report(id).is_none(), "forgotten");

    assert_eq!(r.undone, 1, "only the second copy");
    assert!(r.blocked.is_none());
    assert!(
        mem.stat(&vp("mem:///one.copy")).await.is_ok(),
        "what came before the cut is NOT touched"
    );
    assert!(
        matches!(
            mem.stat(&vp("mem:///two.copy")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "what came after is"
    );
}

/// And it does not take down an AGENT's, even if it comes after the cut:
/// "undo mine" is mine. An agent's is undone through `policy.undo_session`,
/// which is a different question with a different answer.
#[tokio::test]
async fn undoing_up_to_a_point_does_not_touch_an_agents() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"a").await;

    let h = engine
        .copy(&vp("mem:///a.txt"), &vp("mem:///human.copy"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let cut = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .first()
        .expect("there are entries")
        .entry
        .seq;

    // The agent's mutation is SEEDED in the journal with the file already in
    // place, the same way the other tests in this file do it: what is being
    // checked is who undo looks at, not which door the entry came through.
    write_file(&mem, "mem:///agent.copy", b"a").await;
    journal
        .journal()
        .record(
            "created",
            b"mem:///agent.copy",
            None,
            Reversal::Delete,
            None,
            &Actor::Agent {
                session: "s-1".into(),
            },
        )
        .await
        .expect("agent record");

    let (h, report) = engine.undo_after(cut, None).await.expect("undo submit");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();

    assert_eq!(r.undone, 0, "nothing of the human's after the cut");
    assert!(
        mem.stat(&vp("mem:///agent.copy")).await.is_ok(),
        "the agent's is still where it was"
    );
}

/// **An undo queued behind another cancels cleanly** (#358, rule 3).
///
/// An undo's turn is a new wait, and every wait for a Task has to honor its
/// token: an undo the human cancels while waiting behind another long one ends
/// `Cancelled` without touching anything, and the one ahead of it finishes its
/// own.
#[tokio::test]
async fn a_queued_undo_cancels_cleanly() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"a").await;
    let h = engine
        .copy(&vp("mem:///a.txt"), &vp("mem:///anchor.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let cut = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .first()
        .expect("the copy")
        .entry
        .seq;
    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("mv");
    assert_eq!(h.join().await, TaskState::Completed);

    // The first one, slow: it has the turn while the second one waits.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(300)));
    let (h1, _r1) = engine.undo_after(cut, None).await.expect("first undo");
    let (h2, r2) = engine.undo_after(cut, None).await.expect("second undo");
    h2.cancel();
    assert_eq!(
        h2.join().await,
        TaskState::Cancelled,
        "cancelled in the queue"
    );
    assert_eq!(
        h1.join().await,
        TaskState::Completed,
        "the one ahead finishes"
    );
    assert_eq!(
        r2.lock().expect("lock").undone,
        0,
        "and the cancelled one touched nothing"
    );
    assert!(mem.stat(&vp("mem:///a.txt")).await.is_ok());
}

/// **Two undos that chose the same thing do not undo it twice** (#358).
///
/// Choosing and running are separate: entries are picked when the undo is
/// requested, and the reversals run afterward inside the Task. A double
/// click, two frontends against one daemon, or a retry after a timeout give
/// two Tasks the SAME stack. Without looking inside the Task again, the
/// second one used to revert what the first had already returned — and if the
/// human had recreated the destination in the meantime, it would rename over
/// it.
///
/// Both `undo_after` calls are requested BEFORE either runs: selection
/// happens at request time and the Task runs afterward, which is exactly the
/// window.
#[tokio::test]
async fn two_undos_that_chose_the_same_thing_do_not_undo_it_twice() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"a").await;
    // The cut has to be an entry that exists: a copy that is kept.
    let h = engine
        .copy(&vp("mem:///a.txt"), &vp("mem:///anchor.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let cut = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .first()
        .expect("the copy")
        .entry
        .seq;
    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("mv");
    assert_eq!(h.join().await, TaskState::Completed);

    // Per-op latency on the provider: the first one's Task stays halfway
    // while the second one is chosen (one journal query, milliseconds).
    // Without it, on this single-thread runtime the first Task would run
    // ENTIRELY during that query and the second one would choose nothing at
    // all anymore. Under heavy load, the worst that happens is the test stops
    // seeing the race (a green that proves nothing), never a false red.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(200)));
    let (h1, r1) = engine.undo_after(cut, None).await.expect("first undo");
    let (h2, r2) = engine.undo_after(cut, None).await.expect("second undo");
    assert_eq!(h1.join().await, TaskState::Completed);
    assert_eq!(h2.join().await, TaskState::Completed);
    let (r1, r2) = (
        r1.lock().expect("lock").clone(),
        r2.lock().expect("lock").clone(),
    );

    assert!(mem.stat(&vp("mem:///a.txt")).await.is_ok(), "it came back");
    assert_eq!(
        r1.undone + r2.undone,
        1,
        "ONE of the two undid it: {r1:?} / {r2:?}"
    );
    assert!(
        r1.blocked.is_none() && r2.blocked.is_none(),
        "and the other did not trip over the tree already returned: {r1:?} / {r2:?}"
    );
    let compensations = journal
        .journal()
        .page(None, 50, None)
        .await
        .expect("page")
        .iter()
        .filter(|p| p.entry.undoes_seq.is_some())
        .count();
    assert_eq!(compensations, 1, "a single compensation in the journal");
}

/// **The ceiling (`upto_seq`, 0.80.0): what was not counted is not undone.**
///
/// The timeline counts over what it has loaded. What was done afterward —
/// with the panel open, which is normal — used to enter undo without having
/// been counted: the question promised one and two got undone. With the
/// ceiling, anything newer than what was counted stays where it is.
#[tokio::test]
async fn the_ceiling_leaves_out_what_was_not_counted() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"a").await;
    let copy = |dst: &'static str| {
        let engine = &engine;
        async move {
            let h = engine
                .copy(&vp("mem:///a.txt"), &vp(dst))
                .await
                .expect("copy");
            assert_eq!(h.join().await, TaskState::Completed);
        }
    };
    let last = || async {
        journal
            .journal()
            .page(None, 1, Some("user"))
            .await
            .expect("page")
            .first()
            .expect("entry")
            .entry
            .seq
    };
    copy("mem:///cut.txt").await;
    let cut = last().await;
    copy("mem:///counted.txt").await;
    let ceiling = last().await;
    // What is done AFTER the list is painted: the count never saw it.
    copy("mem:///new.txt").await;

    let (h, report) = engine
        .undo_after(cut, Some(ceiling))
        .await
        .expect("undo submit");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        report.lock().expect("lock").undone,
        1,
        "only the counted one"
    );
    assert!(
        mem.stat(&vp("mem:///counted.txt")).await.is_err(),
        "the counted one was undone"
    );
    assert!(
        mem.stat(&vp("mem:///new.txt")).await.is_ok(),
        "the one that was not counted stays"
    );
    assert!(
        mem.stat(&vp("mem:///cut.txt")).await.is_ok(),
        "and the pointed-at one, as always"
    );
}
