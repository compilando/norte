//! `Engine::index_build_as` / `index_query_as` integration (M4, ADR 0034): the
//! cancelable BFS walk feeds the FTS5 index, the query reads it, and a
//! non-UTF-8 name survives byte-exact. In-memory `MemProvider` → deterministic.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::{TaskState, VPath};
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

async fn setup() -> (Engine, Arc<MemProvider>) {
    let index = norte_index::Index::open_memory().await.expect("index");
    let engine = Engine::new().with_index(Arc::new(index));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

#[tokio::test]
async fn build_then_query_finds_seeded_entries_byte_exact() {
    let (engine, mem) = setup().await;
    mem.mkdir(&vp("mem:///docs")).await.expect("mkdir");
    write_file(&mem, "mem:///docs/annual-report.txt", b"x").await;
    // HOSTILE non-UTF-8 name seeded on the FS.
    let hostile = "mem:///docs/report-a%FF%FE.txt";
    write_file(&mem, hostile, b"y").await;

    // build
    let (h, report) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().unwrap().expect("report");
    // docs (dir) + 2 files = 3 entries.
    assert_eq!(r.indexed, 3, "docs + 2 files");

    // query "report" → both files.
    let hits = engine
        .index_query_as(&vp("mem:///"), "report", 10, Actor::User)
        .await
        .expect("query");
    assert_eq!(hits.len(), 2, "the prefix 'report' matches both");
    let names: Vec<Vec<u8>> = hits
        .iter()
        .map(|h| h.path.file_name().unwrap().as_bytes().to_vec())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.as_slice() == b"report-a\xff\xfe.txt"),
        "the non-UTF-8 name comes back byte-exact"
    );
}

#[tokio::test]
async fn build_without_index_is_unsupported() {
    let engine = Engine::new(); // no index
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    match engine.index_build_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::Unsupported) => {}
        Err(e) => panic!("expected Unsupported, was {e:?}"),
        Ok(_) => panic!("expected Unsupported without an index, it opened the Task"),
    }
}

#[tokio::test]
async fn cancelled_build_leaves_index_coherent() {
    let (engine, mem) = setup().await;
    // Seed a tree; then a normal build to have a base index.
    for i in 0..5 {
        write_file(&mem, &format!("mem:///f{i}.txt"), b"x").await;
    }
    let (h, _r) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    // The base query works.
    let base = engine
        .index_query_as(&vp("mem:///"), "f0", 10, Actor::User)
        .await
        .unwrap();
    assert_eq!(base.len(), 1);

    // A second build CANCELLED right away: the Task ends Cancelled and the
    // previous index stays coherent (the walk cuts before the build → nothing
    // touched).
    let (h2, _r2) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .unwrap();
    h2.cancel();
    let st = h2.join().await;
    assert!(
        matches!(st, TaskState::Cancelled | TaskState::Completed),
        "cancelled or completed before the cut, was {st:?}"
    );
    // The index is still queryable and coherent (f0 is still there).
    let after = engine
        .index_query_as(&vp("mem:///"), "f0", 10, Actor::User)
        .await
        .unwrap();
    assert_eq!(after.len(), 1, "the previous index intact after cancelling");
}
