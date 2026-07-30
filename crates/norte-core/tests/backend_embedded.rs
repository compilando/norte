//! La superficie [`Backend::Embedded`] (fase 3 M2): el mismo contrato que
//! el remoto, contra el `Engine` in-process — sin socket.

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
    VPath::parse(wire).expect("wire válido")
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

    // Embebido no tiene canales de daemon.
    assert!(backend.take_foreign_tasks().is_none());
    assert!(backend.take_conn_events().is_none());
}

#[tokio::test]
async fn embedded_copy_move_delete_como_tasks() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///a", b"datos").await;

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

/// #104: mkdir como Task por el backend embebido — crea, y el destino
/// ocupado falla (sin -p, sin idempotencia).
#[tokio::test]
async fn embedded_mkdir_como_task() {
    let (backend, mem) = embedded();
    let task = backend.mkdir(&vp("mem:///nueva")).await.expect("mkdir");
    assert_eq!(task.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///nueva")).await.is_ok());

    let task = backend.mkdir(&vp("mem:///nueva")).await.expect("submit");
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
async fn embedded_clonado_comparte_engine_y_stat_funciona() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///f", b"x").await;

    // El clon debe compartir el mismo Engine (Arc), no montar uno nuevo.
    let clone = backend.clone();
    let entry = clone.stat(&vp("mem:///f")).await.expect("stat");
    assert_eq!(entry.size, Some(1));

    // El original sigue funcionando tras clonar (no se movió el Arc).
    let entry2 = backend.stat(&vp("mem:///f")).await.expect("stat original");
    assert_eq!(entry2.size, Some(1));
}

/// `Backend::search` embebido = passthrough al walker del engine como
/// `Actor::User`: devuelve `(TaskRef, rx)` y los lotes de hits llegan por el
/// canal directo (el walker cierra `tx` al terminar, así que `rx` se cierra
/// solo). Mismo contrato que el remoto (ver `backend_remote.rs`).
#[tokio::test]
async fn embedded_search_stream_de_hits() {
    let (backend, mem) = embedded();
    write_file(&mem, "mem:///a.rs", b"").await;
    write_file(&mem, "mem:///b.txt", b"").await;
    mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&mem, "mem:///sub/c.rs", b"").await;

    let (task, mut rx) = backend
        .search(FsSearchParams {
            root: vp("mem:///"),
            name_glob: Some("*.rs".into()),
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
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
async fn embedded_errores_son_taxonomia() {
    let (backend, _mem) = embedded();
    assert_eq!(
        backend.list(&vp("mem:///no-existe")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        backend
            .read(&vp("mem:///no-existe"), None)
            .await
            .unwrap_err(),
        Error::NotFound
    );
}
