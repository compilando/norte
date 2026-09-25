//! Tests of the tasks panel (phase 5): live snapshots from the
//! `TaskHandle`'s watch, terminal detection and cancellation — no terminal,
//! with the real `Engine` over `MemProvider`.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::backend::Backend;
use norte_core::{Engine, TransferOptions};
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_tui::tasks::TaskBoard;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

fn backend_mem() -> (Backend, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (Backend::Embedded(Arc::new(engine)), mem)
}

#[tokio::test]
async fn the_board_sees_a_task_finish() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"data").await;
    let mut board = TaskBoard::default();
    let task = backend
        .copy(&vp("mem:///a"), &vp("mem:///b"), TransferOptions::default())
        .await
        .unwrap();
    board.push(&task, None);
    assert_eq!(board.rows().len(), 1);

    // The task finishes; the tick detects it as terminal EXACTLY once.
    let mut terminals = Vec::new();
    for _ in 0..200 {
        // The paint clock: no pacing is measured here, so zero is fine.
        terminals.extend(board.tick(0));
        if !terminals.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(terminals.len(), 1, "exactly one terminal event");
    assert_eq!(terminals[0].state, TaskState::Completed);
    assert!(board.tick(0).is_empty(), "not re-emitted");
}

#[tokio::test]
async fn cancel_the_last_running_one() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///a", b"data").await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));
    let mut board = TaskBoard::default();
    let task = backend
        .copy(&vp("mem:///a"), &vp("mem:///b"), TransferOptions::default())
        .await
        .unwrap();
    board.push(&task, None);
    assert!(board.cancel_last_running(), "one was running");

    let mut terminals = Vec::new();
    for _ in 0..200 {
        // The paint clock: no pacing is measured here, so zero is fine.
        terminals.extend(board.tick(0));
        if !terminals.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    mem.faults().clear();
    assert_eq!(terminals[0].state, TaskState::Cancelled);
    assert!(!board.cancel_last_running(), "nothing left running");
}
