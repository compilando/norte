//! Integración engine↔journal (M3-1b): las mutaciones del engine con un
//! `SqliteJournal` real producen las entradas esperadas, en orden y con
//! hash-chain válida. `MemProvider` in-memory → determinista, sin harness.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Engine, Journal, MutationObserver, SqliteJournal};
use norte_proto::{DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Engine con journal in-memory + `MemProvider` (sus caps por defecto incluyen
/// `TRASH`). Devuelve el `SqliteJournal` para inspeccionar las entradas.
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
    write_file(&mem, "mem:///src.txt", b"hola").await;

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
    let last = es.last().expect("entrada");
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
    // MemProvider = papelera "vanish": sin ruta recuperable.
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
