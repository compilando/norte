//! E2E de live search (Alt+F7, M4 T7): criterio de salida contra el
//! `Backend::Embedded` real (Engine + `MemProvider`), el mismo harness
//! `backend_mem` que `tests/lua_fs.rs`. Cubre los cuatro ejes de criterio
//! (`name_glob`/`name_regex`/`content`/`content_regex`), cancelación limpia
//! con hits parciales conservados, y un nombre hostil (bytes crudos no-UTF8)
//! intacto de punta a punta.

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
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Escribe `content` bajo la raíz con un nombre de BYTES crudos (corpus
/// hostil: nunca se asume UTF-8 en el nombre, regla 1).
async fn write_named(mem: &MemProvider, name: &[u8], content: &[u8]) -> VPath {
    let p = MemProvider::root().join(Segment::new(name.to_vec()).expect("segmento"));
    let mut sink = mem.write(&p).await.expect("write abre");
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

/// Params base: solo raíz, resto vacío/false (mismo molde que
/// `engine_search.rs`, para que los cuatro ejes se puedan setear con `..`).
fn params(root: &str) -> FsSearchParams {
    FsSearchParams {
        root: vp(root),
        name_glob: None,
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    }
}

/// Drena el canal de hits hasta que se cierra; aplana entries+matches.
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
// Criterio de salida M4 live search: "año" bajo un árbol UTF-8 + Latin-1
// encuentra ambos (y el UTF-8 anidado), en streaming; F5 desde un hit copia
// el fichero real y el destino es byte-exacto contra el original.
#[tokio::test]
async fn criterio_de_salida_año() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///f1", "un año".as_bytes()).await;
    // Latin-1 crudo: 'a' 0xF1 'o' = "año" con la ñ en un solo byte alto.
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
        "f1 (UTF-8), f2 (Latin-1) y sub/f4 (UTF-8) casan; f3.rs (vacío) no"
    );
    // Los tres son hits de CONTENIDO: line/preview poblados.
    for (e, m) in &hits {
        let m = m
            .as_ref()
            .unwrap_or_else(|| panic!("match info para {}", e.path.display_lossy()));
        assert_eq!(m.line, Some(1), "{}", e.path.display_lossy());
        assert!(
            m.preview.as_deref().is_some_and(|s| !s.is_empty()),
            "preview no vacío para {}",
            e.path.display_lossy()
        );
    }

    // F5-equivalente: copia el PRIMER hit (mem:///f1, UTF-8) a otro dir y
    // verifica que el destino es byte-exacto contra el original.
    mem.mkdir(&vp("mem:///otro")).await.expect("mkdir otro");
    let primero = &hits[0].0;
    assert_eq!(
        primero.path.display_lossy(),
        vp("mem:///f1").display_lossy()
    );
    let nombre = primero.path.file_name().expect("nombre").clone();
    let destino = vp("mem:///otro").join(nombre);

    let copy_task = backend
        .copy(&primero.path, &destino, TransferOptions::default())
        .await
        .expect("copy");
    assert_eq!(copy_task.join().await, TaskState::Completed);

    let original = backend
        .read(&primero.path, None)
        .await
        .expect("read original");
    let copiado = backend.read(&destino, None).await.expect("read copia");
    assert_eq!(copiado, original, "F5 desde un hit es byte-exacto");
    assert_eq!(original, "un año".as_bytes());
}

// 2 ───────────────────────────────────────────────────────────────────────
// Cancelación limpia (regla 3): tras el primer lote, cancel; los hits ya
// llegados se conservan en lo drenado, la Task termina Cancelled.
#[tokio::test]
async fn cancel_conserva_lo_llegado() {
    let (backend, mem) = backend_mem();
    for i in 0..200 {
        write_file(
            &mem,
            &format!("mem:///f{i:03}.txt"),
            b"contiene ano y mas\n",
        )
        .await;
    }
    // Latencia por op: da tiempo a cancelar a mitad del walk.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(3)));

    let (task, mut rx) = backend
        .search(FsSearchParams {
            content: Some("ano".to_owned()),
            ..params("mem:///")
        })
        .await
        .expect("search");

    let first = rx.recv().await.expect("primer lote");
    assert!(!first.entries.is_empty(), "primer lote no vacío");
    task.cancel();

    let mut got = first.entries.len();
    while let Some(batch) = rx.recv().await {
        got += batch.entries.len();
    }
    assert!(got >= 1, "al menos el primer lote se conservó: {got}");
    assert!(got < 200, "cancelada a mitad: {got} < 200");
    assert_eq!(task.join().await, TaskState::Cancelled);
}

// 3 ───────────────────────────────────────────────────────────────────────
// Nombre hostil: bytes crudos no-UTF8 (0xFF 0xFE) sobreviven intactos por
// glob "*" — ni panic ni corrupción del path (regla 1).
#[tokio::test]
async fn nombre_hostil() {
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
    assert_eq!(hits.len(), 1, "una entrada");
    let (entry, m) = &hits[0];
    assert!(
        m.is_none(),
        "búsqueda de solo nombre: sin contexto de contenido"
    );
    assert_eq!(
        entry.path, hostile,
        "el VPath del hit llega con los bytes crudos intactos"
    );
    assert_eq!(
        entry.path.file_name().expect("nombre").as_bytes(),
        &[0xFF, 0xFE],
        "el nombre hostil no se corrompe ni se decodifica"
    );
}

// 4 ───────────────────────────────────────────────────────────────────────
// Los cuatro ejes de criterio (name_glob cubierto en `nombre_hostil`,
// content en `criterio_de_salida_año`): aquí name_regex y content_regex.
#[tokio::test]
async fn ejes_name_regex_y_content_regex() {
    let (backend, mem) = backend_mem();
    write_file(&mem, "mem:///main.rs", b"fn main() {}\n").await;
    write_file(&mem, "mem:///notes.txt", b"nothing here\n").await;
    write_file(&mem, "mem:///lib.rs", b"struct Lib;\n").await;

    // Eje name_regex.
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

    // Eje content_regex.
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
