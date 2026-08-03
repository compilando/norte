//! E2E M4-IA-2: corpus pequeño → index.build → index.embed → `search_semantic`
//! devuelve el fichero relevante; un nombre hostil sobrevive byte-exacto.
//!
//! In-process (estilo `e2e_m3.rs`): `MemProvider` + `Index::open_memory` +
//! `FakeEmbed` determinista — el criterio de salida de la spec sin red ni disco.

use std::sync::Arc;

use bytes::Bytes;
use norte_ai::fake::FakeEmbed;
use norte_core::ai::{AiConfig, AiProviderConfig};
use norte_core::{Actor, Engine};
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, path: &VPath, content: &[u8]) {
    let mut sink = mem.write(path).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Config `[ai]` habilitada con un único proveedor (→ `embed_provider_config`
/// lo resuelve como "el único").
fn ai_cfg() -> AiConfig {
    AiConfig {
        enabled: true,
        providers: vec![AiProviderConfig {
            name: "fake".into(),
            kind: "ollama".into(),
            model: "fake-model".into(),
            base_url: None,
        }],
        ..AiConfig::default()
    }
}

/// Engine con índice in-memory + `FakeEmbed` inyectado + `[ai]` habilitada.
async fn setup() -> (Engine, Arc<MemProvider>) {
    let index = norte_index::Index::open_memory().await.expect("index");
    let engine = Engine::new().with_index(Arc::new(index));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let fake = Arc::new(FakeEmbed::new(16));
    engine.set_ai_embed_provider(fake as norte_ai::SharedAiProvider);
    engine.set_ai_config(ai_cfg());
    (engine, mem)
}

#[tokio::test]
async fn semantic_e2e_small_corpus() {
    let (engine, mem) = setup().await;

    // Corpus: 3 textos con contenidos distintos + 1 nombre HOSTIL no-UTF8
    // (bytes 0xFF 0xFE del corpus canónico) con extensión .txt para pasar la
    // heurística de texto del embed.
    write_file(&mem, &vp("mem:///informe.txt"), b"informe anual 2024").await;
    write_file(&mem, &vp("mem:///receta.txt"), b"receta de cocina").await;
    write_file(&mem, &vp("mem:///notas.txt"), b"notas de reuni\xc3\xb3n").await;
    let hostile = vp("mem:///informe-a%FF%FE.txt");
    write_file(&mem, &hostile, b"contenido secreto hostil").await;

    // index.build → Completed.
    let (h, report) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().unwrap().expect("report");
    assert_eq!(r.indexed, 4, "los 4 ficheros del corpus");

    // index.embed → Completed.
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    // Query == contenido exacto del fichero hostil ⇒ FakeEmbed determinista ⇒
    // vector idéntico ⇒ top-1 es el hostil, y su path vuelve BYTE-EXACTO.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "contenido secreto hostil", 10)
        .await
        .expect("search hostil");
    assert!(!hits.is_empty(), "hay embeddings, tiene que haber hits");
    assert_eq!(hits[0].0, hostile, "top-1 es el path hostil, byte-exacto");
    assert_eq!(
        hits[0].0.file_name().expect("file_name").as_bytes(),
        b"informe-a\xff\xfe.txt",
        "los bytes no-UTF8 sobreviven el round-trip completo"
    );
    assert!((hits[0].1 - 1.0).abs() < 1e-5, "score top-1: {}", hits[0].1);
    // Los 4 vectores existen: el hostil no desplazó a nadie ni fue saltado.
    assert_eq!(hits.len(), 4, "los 4 .txt embebidos, hostil incluido");
    assert!(hits.iter().all(|(_, s)| s.is_finite()));
    assert!(
        hits.windows(2).all(|w| w[0].1 >= w[1].1),
        "orden descendente"
    );

    // k=1 ⇒ exactamente 1 hit.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "contenido secreto hostil", 1)
        .await
        .expect("search k=1");
    assert_eq!(hits.len(), 1, "k=1 ⇒ exactamente 1 hit");
    assert_eq!(hits[0].0, hostile);

    // Relevancia más allá de un único fichero: otro contenido distinto trae SU
    // fichero en top-1.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "receta de cocina", 10)
        .await
        .expect("search receta");
    assert_eq!(hits[0].0, vp("mem:///receta.txt"), "top-1 es la receta");
}
