//! Live search E2E (Alt+F7, M4 T7): acceptance criteria against the real
//! `Backend::Embedded` (Engine + `MemProvider`), the same `backend_mem`
//! harness as `tests/lua_fs.rs`. Covers the four criterion axes
//! (`name_glob`/`name_regex`/`content`/`content_regex`), clean cancellation
//! with partial hits kept, and a hostile name (raw non-UTF8 bytes) intact
//! end to end.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::backend::Backend;
use norte_core::{Engine, TransferOptions};
use norte_proto::methods::{FsSearchParams, MatchInfo};
use norte_proto::{Entry, Segment, TaskState, VPath};
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

/// Writes `content` under the root with a RAW-BYTES name (hostile corpus:
/// the name is never assumed to be UTF-8, rule 1).
async fn write_named(mem: &MemProvider, name: &[u8], content: &[u8]) -> VPath {
    let p = MemProvider::root().join(Segment::new(name.to_vec()).expect("segment"));
    let mut sink = mem.write(&p).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
    p
}

fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (Backend::Embedded(Arc::new(engine)), mem)
}

/// Base params: only the root, the rest empty/false (same mold as
/// `engine_search.rs`, so the four axes can be set with `..`).
fn params(root: &str) -> FsSearchParams {
    FsSearchParams::new(vp(root))
}

/// Drains the hits channel until it closes; flattens entries+matches.
async fn drain(
    mut rx: tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
) -> Vec<(Entry, Option<MatchInfo>)> {
    let mut out = Vec::new();
    while let Some(batch) = rx.recv().await {
        let matches = batch.matches.unwrap_or_default();
        for (i, e) in batch.entries.into_iter().enumerate() {
            out.push((e, matches.get(i).cloned()));
        }
    }
    out
}

// 1 ───────────────────────────────────────────────────────────────────────
// M4 live search acceptance criterion: "año" under a UTF-8 + Latin-1 tree
// finds both (and the nested UTF-8 one), streaming; F5 from a hit copies
// the real file and the destination is byte-exact against the original.
#[tokio::test]
async fn exit_criterion_año() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///f1", "un año".as_bytes()).await;
    // Raw Latin-1: 'a' 0xF1 'o' = "año" with the ñ as a single high byte.
    write_file(&mem, "mem:///f2", b"a\xF1o").await;
    write_file(&mem, "mem:///f3.rs", b"").await;
    mem.mkdir(&vp("mem:///sub")).await.expect("mkdir sub");
    write_file(&mem, "mem:///sub/f4", "año".as_bytes()).await;

    let (task, rx) = backend
        .search(FsSearchParams {
            content: Some("año".to_owned()),
            case_sensitive: false,
            ..params("mem:///")
        })
        .await
        .expect("search");

    let mut hits = drain(rx).await;
    assert_eq!(task.join().await, TaskState::Completed);
    hits.sort_by_key(|(e, _)| e.path.display_lossy());

    let paths: Vec<String> = hits.iter().map(|(e, _)| e.path.display_lossy()).collect();
    assert_eq!(
        paths,
        vec![
            vp("mem:///f1").display_lossy(),
            vp("mem:///f2").display_lossy(),
            vp("mem:///sub/f4").display_lossy(),
        ],
        "f1 (UTF-8), f2 (Latin-1) and sub/f4 (UTF-8) match; f3.rs (empty) does not"
    );
    // All three are CONTENT hits: line/preview populated.
    for (e, m) in &hits {
        let m = m
            .as_ref()
            .unwrap_or_else(|| panic!("match info for {}", e.path.display_lossy()));
        assert_eq!(m.line, Some(1), "{}", e.path.display_lossy());
        assert!(
            m.preview.as_deref().is_some_and(|s| !s.is_empty()),
            "preview not empty for {}",
            e.path.display_lossy()
        );
    }

    // F5-equivalent: copies the FIRST hit (mem:///f1, UTF-8) to another dir
    // and verifies the destination is byte-exact against the original.
    mem.mkdir(&vp("mem:///otro")).await.expect("mkdir otro");
    let first = &hits[0].0;
    assert_eq!(first.path.display_lossy(), vp("mem:///f1").display_lossy());
    let name = first.path.file_name().expect("name").clone();
    let dest = vp("mem:///otro").join(name);

    let copy_task = backend
        .copy(&first.path, &dest, TransferOptions::default())
        .await
        .expect("copy");
    assert_eq!(copy_task.join().await, TaskState::Completed);

    let original = backend
        .read(&first.path, None)
        .await
        .expect("read original");
    let copied = backend.read(&dest, None).await.expect("read copy");
    assert_eq!(copied, original, "F5 from a hit is byte-exact");
    assert_eq!(original, "un año".as_bytes());
}

// 2 ───────────────────────────────────────────────────────────────────────
// Clean cancellation (rule 3): after the first batch, cancel; the hits
// already received are kept in what was drained, the Task ends Cancelled.
#[tokio::test]
async fn cancel_keeps_what_arrived() {
    let (backend, mem) = backend_mem();
    for i in 0..200 {
        write_file(
            &mem,
            &format!("mem:///f{i:03}.txt"),
            b"contiene ano y mas\n",
        )
        .await;
    }
    // Latency per op: gives time to cancel mid-walk.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(3)));

    let (task, mut rx) = backend
        .search(FsSearchParams {
            content: Some("ano".to_owned()),
            ..params("mem:///")
        })
        .await
        .expect("search");

    let first = rx.recv().await.expect("first batch");
    assert!(!first.entries.is_empty(), "first batch not empty");
    task.cancel();

    let mut got = first.entries.len();
    while let Some(batch) = rx.recv().await {
        got += batch.entries.len();
    }
    assert!(got >= 1, "at least the first batch was kept: {got}");
    assert!(got < 200, "cancelled halfway: {got} < 200");
    assert_eq!(task.join().await, TaskState::Cancelled);
}

// 3 ───────────────────────────────────────────────────────────────────────
// Hostile name: raw non-UTF8 bytes (0xFF 0xFE) survive intact through a
// "*" glob — no panic, no path corruption (rule 1).
#[tokio::test]
async fn hostile_name() {
    let (backend, mem) = backend_mem();
    let hostile = write_named(&mem, &[0xFF, 0xFE], b"x").await;

    let (task, rx) = backend
        .search(FsSearchParams {
            name_glob: Some("*".to_owned()),
            ..params("mem:///")
        })
        .await
        .expect("search");

    let hits = drain(rx).await;
    assert_eq!(task.join().await, TaskState::Completed);
    assert_eq!(hits.len(), 1, "one entry");
    let (entry, m) = &hits[0];
    assert!(m.is_none(), "name-only search: no content context");
    assert_eq!(
        entry.path, hostile,
        "the hit's VPath arrives with the raw bytes intact"
    );
    assert_eq!(
        entry.path.file_name().expect("name").as_bytes(),
        &[0xFF, 0xFE],
        "the hostile name is neither corrupted nor decoded"
    );
}

// 4 ───────────────────────────────────────────────────────────────────────
// The four criterion axes (name_glob covered in `hostile_name`, content
// in `exit_criterion_año`): here name_regex and content_regex.
#[tokio::test]
async fn axes_name_regex_and_content_regex() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///main.rs", b"fn main() {}\n").await;
    write_file(&mem, "mem:///notes.txt", b"nothing here\n").await;
    write_file(&mem, "mem:///lib.rs", b"struct Lib;\n").await;

    // name_regex axis.
    let (task, rx) = backend
        .search(FsSearchParams {
            name_regex: Some(r"^(main|lib)\.rs$".to_owned()),
            ..params("mem:///")
        })
        .await
        .expect("search name_regex");
    let mut hits = drain(rx).await;
    assert_eq!(task.join().await, TaskState::Completed);
    hits.sort_by_key(|(e, _)| e.path.display_lossy());
    let names: Vec<String> = hits.iter().map(|(e, _)| e.path.display_lossy()).collect();
    assert_eq!(
        names,
        vec![
            vp("mem:///lib.rs").display_lossy(),
            vp("mem:///main.rs").display_lossy()
        ]
    );

    // content_regex axis.
    let (task, rx) = backend
        .search(FsSearchParams {
            content_regex: Some(r"struct\s+Lib".to_owned()),
            ..params("mem:///")
        })
        .await
        .expect("search content_regex");
    let hits = drain(rx).await;
    assert_eq!(task.join().await, TaskState::Completed);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].0.path.display_lossy(),
        vp("mem:///lib.rs").display_lossy()
    );
}
