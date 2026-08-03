//! Integración `Engine::index_embed_as` (M4-IA-2, ADR 0031 A3): la task
//! `index.embed` filtra ANTES de leer (`denied_prefixes`, heurística de texto),
//! salta hashes sin cambios, reintenta rate-limits acotadamente y se cancela
//! limpio. `MemProvider` + `FakeEmbed` in-memory → determinista.

use std::sync::Arc;
use std::time::Duration;

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

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
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

/// Engine con índice in-memory + `FakeEmbed` inyectado + config dada.
async fn setup_with(fake: FakeEmbed, cfg: AiConfig) -> (Engine, Arc<MemProvider>, Arc<FakeEmbed>) {
    let index = norte_index::Index::open_memory().await.expect("index");
    let engine = Engine::new().with_index(Arc::new(index));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let fake = Arc::new(fake);
    engine.set_ai_embed_provider(Arc::clone(&fake) as norte_ai::SharedAiProvider);
    engine.set_ai_config(cfg);
    (engine, mem, fake)
}

async fn setup() -> (Engine, Arc<MemProvider>, Arc<FakeEmbed>) {
    setup_with(FakeEmbed::new(8), ai_cfg()).await
}

/// `index.build` del root y join (precondición de casi todos los tests).
async fn build(engine: &Engine, root: &str) {
    let (h, _report) = engine
        .index_build_as(vp(root), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
}

/// Todos los textos que salieron hacia el proveedor, en orden.
fn sent_inputs(fake: &FakeEmbed) -> Vec<String> {
    fake.calls
        .lock()
        .expect("calls lock")
        .iter()
        .flatten()
        .cloned()
        .collect()
}

#[tokio::test]
async fn embed_requires_prior_build() {
    let (engine, _mem, _fake) = setup().await;
    // Sin `index.build` previo: el error va en la RESPUESTA, no en el join.
    match engine.index_embed_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::NotFound) => {}
        Err(e) => panic!("esperaba NotFound, fue {e:?}"),
        Ok(_) => panic!("esperaba NotFound sin build previo, abrió la Task"),
    }
}

#[tokio::test]
async fn embed_skips_non_text_and_records_only_text() {
    let (engine, mem, fake) = setup().await;
    write_file(&mem, "mem:///a.txt", b"contenido alfa").await;
    write_file(&mem, "mem:///b.md", b"contenido beta").await;
    write_file(&mem, "mem:///c.bin", &[0u8, 159, 146, 150]).await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    let inputs = sent_inputs(&fake);
    assert_eq!(inputs.len(), 2, "solo los 2 ficheros de texto: {inputs:?}");
    assert!(
        inputs.iter().any(|t| t.contains("alfa")),
        "el contenido de a.txt salió hacia el proveedor"
    );
}

#[tokio::test]
async fn embed_skip_unchanged_hash_and_reembed_on_model_change() {
    let (engine, mem, fake) = setup().await;
    write_file(&mem, "mem:///a.txt", b"contenido alfa").await;
    write_file(&mem, "mem:///b.md", b"contenido beta").await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("primer embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(sent_inputs(&fake).len(), 2, "primer run embebe ambos");

    // Segundo run: hashes sin cambios → 0 inputs nuevos.
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("segundo embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        sent_inputs(&fake).len(),
        2,
        "hash sin cambios ⇒ nada nuevo sale al proveedor"
    );

    // Cambio de MODELO configurado: los embeddings previos son stale → todo
    // se re-embebe (el total se dobla).
    let mut cfg = ai_cfg();
    cfg.providers[0].model = "otro-modelo".into();
    engine.set_ai_config(cfg);
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("tercer embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(sent_inputs(&fake).len(), 4, "modelo nuevo ⇒ re-embebe todo");
}

#[tokio::test]
async fn embed_denied_prefixes_excluded_before_read() {
    let mut cfg = ai_cfg();
    cfg.denied_prefixes = vec![vp("mem:///secreto")];
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8), cfg).await;
    mem.mkdir(&vp("mem:///secreto")).await.expect("mkdir");
    write_file(&mem, "mem:///secreto/clave.txt", b"SECRETO").await;
    write_file(&mem, "mem:///normal.txt", b"contenido normal").await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    let inputs = sent_inputs(&fake);
    assert!(
        inputs.iter().all(|t| !t.contains("SECRETO")),
        "contenido bajo denied_prefixes JAMÁS sale al proveedor: {inputs:?}"
    );
    assert_eq!(
        inputs.len(),
        1,
        "solo el fichero fuera del prefijo denegado"
    );
}

#[tokio::test]
async fn embed_clean_cancellation() {
    // 40 ficheros → 3 batches; 50ms de latencia por batch = ventana amplia
    // para que el cancel del medio corte el bucle entre batches.
    let (engine, mem, _fake) = setup_with(
        FakeEmbed::new(8).with_delay(Duration::from_millis(50)),
        ai_cfg(),
    )
    .await;
    for i in 0..40 {
        write_file(
            &mem,
            &format!("mem:///f{i}.txt"),
            format!("texto {i}").as_bytes(),
        )
        .await;
    }
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    tokio::time::sleep(Duration::from_millis(10)).await;
    h.cancel();
    let st = h.join().await;
    assert!(
        matches!(st, TaskState::Cancelled | TaskState::Completed),
        "cancelado o completado antes del corte, fue {st:?}"
    );

    // El índice queda coherente: un embed posterior completa sin problemas.
    let h2 = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("embed tras cancelar");
    assert_eq!(h2.join().await, TaskState::Completed);
}

#[tokio::test]
async fn embed_rate_limit_retries_then_fails() {
    // 2 rate-limits < EMBED_RETRY_MAX → la task reintenta por dentro y completa.
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8).with_rate_limited(2), ai_cfg()).await;
    write_file(&mem, "mem:///a.txt", b"contenido alfa").await;
    build(&engine, "mem:///").await;
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        fake.calls.lock().expect("calls lock").len(),
        3,
        "2 intentos rate-limited + 1 bueno"
    );

    // Rate-limit persistente → la task FALLA con ProviderUnavailable (jamás
    // cuelga en reintentos infinitos).
    let (engine, mem, _fake) = setup_with(FakeEmbed::new(8).with_rate_limited(99), ai_cfg()).await;
    write_file(&mem, "mem:///a.txt", b"contenido alfa").await;
    build(&engine, "mem:///").await;
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    match h.join().await {
        TaskState::Failed {
            error: norte_proto::Error::ProviderUnavailable { .. },
        } => {}
        st => panic!("esperaba Failed(ProviderUnavailable), fue {st:?}"),
    }
}

#[tokio::test]
async fn embed_gate_disabled_is_policy_denied() {
    let mut cfg = ai_cfg();
    cfg.enabled = false;
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8), cfg).await;
    write_file(&mem, "mem:///a.txt", b"contenido alfa").await;
    build(&engine, "mem:///").await;

    match engine.index_embed_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::PolicyDenied { .. }) => {}
        Err(e) => panic!("esperaba PolicyDenied con IA off, fue {e:?}"),
        Ok(_) => panic!("esperaba PolicyDenied con IA off, abrió la Task"),
    }
    assert!(
        fake.calls.lock().expect("calls lock").is_empty(),
        "con IA deshabilitada nada sale al proveedor"
    );
}
