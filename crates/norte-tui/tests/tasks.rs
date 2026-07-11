//! Tests del panel de tasks (fase 5): snapshots vivos desde el watch del
//! `TaskHandle`, detección de terminales y cancelación — sin terminal,
//! con el `Engine` real sobre `MemProvider`.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::Engine;
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_tui::tasks::TaskBoard;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.unwrap();
    sink.write(Bytes::copy_from_slice(content)).await.unwrap();
    sink.commit().await.unwrap();
}

fn engine_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

#[tokio::test]
async fn el_board_ve_terminar_una_task() {
    let (engine, mem) = engine_mem();
    write_file(&mem, "mem:///a", b"datos").await;
    let mut board = TaskBoard::default();
    let handle = engine.copy(&vp("mem:///a"), &vp("mem:///b")).unwrap();
    board.push(handle, None);
    assert_eq!(board.rows().len(), 1);

    // La task termina; el tick la detecta como terminal UNA sola vez.
    let mut terminales = Vec::new();
    for _ in 0..200 {
        terminales.extend(board.tick());
        if !terminales.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(terminales.len(), 1, "exactamente un evento terminal");
    assert_eq!(terminales[0].state, TaskState::Completed);
    assert!(board.tick().is_empty(), "no se re-emite");
}

#[tokio::test]
async fn cancelar_la_ultima_en_marcha() {
    let (engine, mem) = engine_mem();
    write_file(&mem, "mem:///a", b"datos").await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));
    let mut board = TaskBoard::default();
    board.push(engine.copy(&vp("mem:///a"), &vp("mem:///b")).unwrap(), None);
    assert!(board.cancel_last_running(), "había una en marcha");

    let mut terminales = Vec::new();
    for _ in 0..200 {
        terminales.extend(board.tick());
        if !terminales.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    mem.faults().clear();
    assert_eq!(terminales[0].state, TaskState::Cancelled);
    assert!(!board.cancel_last_running(), "ya no queda nada en marcha");
}
