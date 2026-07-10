//! Matriz test-first del copy engine v0 (fase 10 de M0), contra `MemProvider`
//! con fallos inyectados: feliz, recursivo hostil, colisiones, cancelación
//! limpia por chunk, fallos en byte exacto, desconexión, move=rename,
//! delete post-order y `copy_native`.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{CapabilityFlags, ConflictKind, Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(mem: &MemProvider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = mem.read(&vp(wire)).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Engine con un `MemProvider` registrado; devuelve también el provider.
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
    // Árbol de 3 niveles con nombres hostiles del corpus.
    let hostiles = norte_testkit::corpus::hostile_names();
    mem.mkdir(&vp("mem:///src")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub")).await.unwrap();
    mem.mkdir(&vp("mem:///src/sub/deep")).await.unwrap();
    let root = MemProvider::root();
    let mut paths = Vec::new();
    for (i, h) in hostiles.iter().take(3).enumerate() {
        let dir = ["src", "src/sub", "src/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let mut p = vp(&format!("mem:///{dir}"));
        p = p.join(seg);
        let mut sink = mem.write(&p).await.expect("write hostil");
        sink.write(Bytes::from_static(b"data")).await.unwrap();
        sink.commit().await.unwrap();
        paths.push(p);
    }
    drop(root);

    let handle = engine
        .copy(&vp("mem:///src"), &vp("mem:///dst"))
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);

    // Cada archivo hostil existe en el destino con bytes intactos.
    for (i, h) in hostiles.iter().take(3).enumerate() {
        let dir = ["dst", "dst/sub", "dst/sub/deep"][i];
        let seg = norte_proto::Segment::new(h.bytes.clone()).unwrap();
        let p = vp(&format!("mem:///{dir}")).join(seg);
        let e = mem.stat(&p).await.unwrap_or_else(|err| {
            panic!("[{}] falta en el destino: {err:?}", h.id);
        });
        assert_eq!(
            e.path.file_name().unwrap().as_bytes(),
            h.bytes.as_slice(),
            "[{}] bytes intactos",
            h.id
        );
    }
}

#[tokio::test]
async fn copy_collision_fails_without_writing() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"nuevo").await;
    write_file(&mem, "mem:///dst", b"precioso contenido previo").await;

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { conflict },
        } => assert_eq!(conflict, ConflictKind::Exists),
        other => panic!("esperaba Conflict, fue {other:?}"),
    }
    // El destino queda EXACTAMENTE como estaba.
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"precioso contenido previo"
    );
}

#[tokio::test]
async fn copy_cancel_leaves_no_partial_destination() {
    let (engine, mem) = engine_with_mem();
    let content = vec![0x5A; 512 * 1024];
    write_file(&mem, "mem:///grande", &content).await;

    let handle = engine
        .copy(&vp("mem:///grande"), &vp("mem:///copia"))
        .unwrap();
    // Cancela en cuanto haya progreso de bytes (el engine chequea por chunk).
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
    // Puede haber terminado ya (carrera legítima); si no, debe ser Cancelled.
    let final_state = handle.join().await;
    match final_state {
        TaskState::Cancelled => {
            // Cancelación limpia: ni archivo ni rastro en el destino.
            assert_eq!(
                mem.stat(&vp("mem:///copia")).await.unwrap_err(),
                Error::NotFound,
                "el destino debe quedar limpio tras cancelar"
            );
        }
        TaskState::Completed => {
            assert_eq!(read_all(&mem, "mem:///copia").await.unwrap(), content);
        }
        other => panic!("estado inesperado: {other:?}"),
    }
}

#[tokio::test]
async fn copy_read_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![1u8; 4000]).await;
    mem.faults().fail_read_at(&vp("mem:///src"), 2000);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("esperaba Failed{{Io}}, fue {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "destino limpio tras fallo de lectura"
    );
}

#[tokio::test]
async fn copy_write_fault_fails_and_cleans_destination() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", &vec![2u8; 4000]).await;
    mem.faults().fail_write_at(&vp("mem:///dst"), 1000);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Io { retryable: false }),
        other => panic!("esperaba Failed{{Io}}, fue {other:?}"),
    }
    assert_eq!(
        mem.stat(&vp("mem:///dst")).await.unwrap_err(),
        Error::NotFound,
        "destino limpio tras fallo de escritura"
    );
}

#[tokio::test]
async fn copy_disconnect_maps_to_provider_unavailable() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///src", b"x").await;
    mem.faults().disconnect_after(1);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    match handle.join().await {
        TaskState::Failed { error } => {
            assert_eq!(error, Error::ProviderUnavailable { retryable: true });
        }
        other => panic!("esperaba ProviderUnavailable, fue {other:?}"),
    }
}

#[tokio::test]
async fn copy_native_used_when_server_copy_declared() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"contenido nativo").await;
    // Si el engine intentara streaming, este fallo lo tumbaría: copy_native
    // no lee por stream, así que debe completar igual.
    mem.faults().fail_read_at(&vp("mem:///src"), 0);

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    mem.faults().clear();
    assert_eq!(
        read_all(&mem, "mem:///dst").await.unwrap(),
        b"contenido nativo"
    );
}

// ---------- move ----------

#[tokio::test]
async fn move_same_provider_is_rename_zero_bytes() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///origen", b"contenido").await;

    let handle = engine
        .move_(&vp("mem:///origen"), &vp("mem:///destino"))
        .unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);

    assert_eq!(
        mem.stat(&vp("mem:///origen")).await.unwrap_err(),
        Error::NotFound
    );
    assert_eq!(
        read_all(&mem, "mem:///destino").await.unwrap(),
        b"contenido"
    );
    assert_eq!(rx.borrow().bytes_done, 0, "rename no copia bytes");
}

#[tokio::test]
async fn move_collision_fails_and_source_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///a", b"1").await;
    write_file(&mem, "mem:///b", b"2").await;

    let handle = engine.move_(&vp("mem:///a"), &vp("mem:///b")).unwrap();
    match handle.join().await {
        TaskState::Failed {
            error: Error::Conflict { .. },
        } => {}
        other => panic!("esperaba Conflict, fue {other:?}"),
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

    let handle = engine.delete(&vp("mem:///d")).unwrap();
    let rx = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        mem.stat(&vp("mem:///d")).await.unwrap_err(),
        Error::NotFound
    );
    // 4 entradas: d, d/sub, d/f1, d/sub/f2.
    assert_eq!(rx.borrow().entries_total, Some(4));
    assert_eq!(rx.borrow().entries_done, 4);
}

#[tokio::test]
async fn delete_missing_fails_not_found() {
    let (engine, mem) = engine_with_mem();
    let _ = &mem;
    let handle = engine.delete(&vp("mem:///nada")).unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::NotFound
        }
    );
}

// ---------- registro / passthrough ----------

#[tokio::test]
async fn unknown_scheme_rejected_at_submit() {
    let (engine, _mem) = engine_with_mem();
    assert!(
        engine
            .copy(&vp("sftp://h/x"), &vp("mem:///y"))
            .is_err_and(|e| e == Error::Unsupported)
    );
    assert!(
        engine
            .copy(&vp("mem:///x"), &vp("sftp://h/y"))
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

// ---------- cancelación limpia por Task (regla dura 3, hallazgos rust-reviewer) ----------

/// Árbol src con `n` archivos bajo `mem:///src`.
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
    // Latencia real por op: la task avanza despacio y la cancelación
    // aterriza a mitad de árbol de forma fiable.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(3)));

    let handle = engine.copy(&vp("mem:///src"), &vp("mem:///dst")).unwrap();
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
    // Árbol parcial permitido (doc de copy_task), pero CADA archivo presente
    // está completo: jamás un archivo a medias sin marcar.
    let mut listed = mem.list(&vp("mem:///dst")).await.unwrap();
    while let Some(e) = listed.next().await {
        let e = e.unwrap();
        if e.kind == norte_proto::EntryKind::File {
            let name = String::from_utf8(e.path.file_name().unwrap().as_bytes().to_vec()).unwrap();
            assert_eq!(
                read_all(&mem, &format!("mem:///dst/{name}")).await.unwrap(),
                b"data",
                "archivo a medias en el destino: {name}"
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

    let handle = engine.delete(&vp("mem:///src")).unwrap();
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
    // Post-order: la raíz cae la ÚLTIMA — cancelado a mitad, sigue ahí.
    assert!(
        mem.stat(&vp("mem:///src")).await.is_ok(),
        "la raíz solo cae al final; cancelar a mitad la deja"
    );
}

#[tokio::test]
async fn move_cancel_before_start_leaves_everything_intact() {
    let (engine, mem) = engine_with_mem();
    write_file(&mem, "mem:///origen", b"contenido").await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));

    let handle = engine
        .move_(&vp("mem:///origen"), &vp("mem:///destino"))
        .unwrap();
    // Cancela inmediatamente: la task lo observa antes del rename.
    handle.cancel();
    let final_state = handle.join().await;
    mem.faults().clear();
    match final_state {
        TaskState::Cancelled => {
            assert_eq!(read_all(&mem, "mem:///origen").await.unwrap(), b"contenido");
            assert_eq!(
                mem.stat(&vp("mem:///destino")).await.unwrap_err(),
                Error::NotFound
            );
        }
        // Carrera legítima: el rename ganó a la cancelación (atómico, limpio).
        TaskState::Completed => {
            assert_eq!(
                read_all(&mem, "mem:///destino").await.unwrap(),
                b"contenido"
            );
        }
        other => panic!("estado inesperado: {other:?}"),
    }
}

#[tokio::test]
async fn copy_dir_into_itself_rejected() {
    let (engine, mem) = engine_with_mem();
    mem.mkdir(&vp("mem:///a")).await.unwrap();
    let handle = engine.copy(&vp("mem:///a"), &vp("mem:///a/b")).unwrap();
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::InvalidPath
        }
    );
    // El árbol queda intacto: sin copia anidada fantasma.
    let n = mem.list(&vp("mem:///a")).await.unwrap().count().await;
    assert_eq!(n, 0);
}
