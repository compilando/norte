//! Integración `Engine::index_build_as` / `index_query_as` (M4, ADR 0034): el
//! walk BFS cancelable alimenta el índice FTS5, la query lo lee, y un nombre
//! no-UTF8 sobrevive byte-exacto. `MemProvider` in-memory → determinista.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::{TaskState, VPath};
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
    write_file(&mem, "mem:///docs/informe-anual.txt", b"x").await;
    // Nombre HOSTIL no-UTF8 sembrado en el FS.
    let hostile = "mem:///docs/informe-a%FF%FE.txt";
    write_file(&mem, hostile, b"y").await;

    // build
    let (h, report) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().unwrap().expect("report");
    // docs (dir) + 2 ficheros = 3 entradas.
    assert_eq!(r.indexed, 3, "docs + 2 ficheros");

    // query "informe" → ambos ficheros.
    let hits = engine
        .index_query_as(&vp("mem:///"), "informe", 10, Actor::User)
        .await
        .expect("query");
    assert_eq!(hits.len(), 2, "prefijo 'informe' casa ambos");
    let names: Vec<Vec<u8>> = hits
        .iter()
        .map(|h| h.path.file_name().unwrap().as_bytes().to_vec())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.as_slice() == b"informe-a\xff\xfe.txt"),
        "el nombre no-UTF8 vuelve byte-exacto"
    );
}

#[tokio::test]
async fn build_without_index_is_unsupported() {
    let engine = Engine::new(); // sin índice
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    match engine.index_build_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::Unsupported) => {}
        Err(e) => panic!("esperaba Unsupported, fue {e:?}"),
        Ok(_) => panic!("esperaba Unsupported sin índice, abrió la Task"),
    }
}

#[tokio::test]
async fn cancelled_build_leaves_index_coherent() {
    let (engine, mem) = setup().await;
    // Siembra un árbol; luego un build normal para tener un índice base.
    for i in 0..5 {
        write_file(&mem, &format!("mem:///f{i}.txt"), b"x").await;
    }
    let (h, _r) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    // La query base funciona.
    let base = engine
        .index_query_as(&vp("mem:///"), "f0", 10, Actor::User)
        .await
        .unwrap();
    assert_eq!(base.len(), 1);

    // Un segundo build CANCELADO de inmediato: la Task termina Cancelled y el
    // índice previo sigue coherente (el walk corta antes del build → no se toca).
    let (h2, _r2) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .unwrap();
    h2.cancel();
    let st = h2.join().await;
    assert!(
        matches!(st, TaskState::Cancelled | TaskState::Completed),
        "cancelado o completado antes del corte, fue {st:?}"
    );
    // El índice sigue consultable y coherente (f0 sigue).
    let after = engine
        .index_query_as(&vp("mem:///"), "f0", 10, Actor::User)
        .await
        .unwrap();
    assert_eq!(after.len(), 1, "el índice previo intacto tras cancelar");
}
