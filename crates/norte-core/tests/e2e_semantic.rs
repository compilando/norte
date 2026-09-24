//! E2E M4-IA-2: small corpus → index.build → index.embed → `search_semantic`
//! returns the relevant file; a hostile name survives byte-exact.
//!
//! In-process (in the style of `e2e_m3.rs`): `MemProvider` + `Index::open_memory` +
//! a deterministic `FakeEmbed` — the spec's exit criterion, with no network or disk.

use std::sync::Arc;

use bytes::Bytes;
use norte_ai::fake::FakeEmbed;
use norte_core::ai::{AiConfig, AiProviderConfig};
use norte_core::{Actor, Engine};
use norte_proto::{TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, path: &VPath, content: &[u8]) {
    let mut sink = mem.write(path).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// `[ai]` config enabled with a single provider (→ `embed_provider_config`
/// resolves it as "the only one").
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

/// Engine with an in-memory index + an injected `FakeEmbed` + `[ai]` enabled.
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

    // Corpus: 3 texts with different content + 1 non-UTF-8 HOSTILE name
    // (bytes 0xFF 0xFE from the canonical corpus) with a .txt extension to pass
    // the embed's text heuristic.
    write_file(&mem, &vp("mem:///report.txt"), b"annual report 2024").await;
    write_file(&mem, &vp("mem:///recipe.txt"), b"cooking recipe").await;
    write_file(&mem, &vp("mem:///notes.txt"), b"meeting notes").await;
    let hostile = vp("mem:///report-a%FF%FE.txt");
    write_file(&mem, &hostile, b"hostile secret content").await;

    // index.build → Completed.
    let (h, report) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().unwrap().expect("report");
    assert_eq!(r.indexed, 4, "the corpus's 4 files");

    // index.embed → Completed.
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    // Query == the hostile file's exact content ⇒ deterministic FakeEmbed ⇒
    // identical vector ⇒ top-1 is the hostile one, and its path comes back BYTE-EXACT.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "hostile secret content", 10)
        .await
        .expect("search hostile");
    assert!(!hits.is_empty(), "there are embeddings, there must be hits");
    assert_eq!(hits[0].0, hostile, "top-1 is the hostile path, byte-exact");
    assert_eq!(
        hits[0].0.file_name().expect("file_name").as_bytes(),
        b"report-a\xff\xfe.txt",
        "the non-UTF-8 bytes survive the full round trip"
    );
    assert!((hits[0].1 - 1.0).abs() < 1e-5, "top-1 score: {}", hits[0].1);
    // All 4 vectors exist: the hostile one did not displace anyone or get skipped.
    assert_eq!(hits.len(), 4, "all 4 .txt files embedded, hostile included");
    assert!(hits.iter().all(|(_, s)| s.is_finite()));
    assert!(
        hits.windows(2).all(|w| w[0].1 >= w[1].1),
        "descending order"
    );

    // k=1 ⇒ exactly 1 hit.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "hostile secret content", 1)
        .await
        .expect("search k=1");
    assert_eq!(hits.len(), 1, "k=1 ⇒ exactly 1 hit");
    assert_eq!(hits[0].0, hostile);

    // Relevance beyond a single file: different content brings back ITS
    // file at top-1.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "cooking recipe", 10)
        .await
        .expect("search recipe");
    assert_eq!(hits[0].0, vp("mem:///recipe.txt"), "top-1 is the recipe");
}

/// **A denial applies BACKWARD** (#122).
///
/// The `denied_prefixes` filter decides what gets READ, so it protects what
/// has not been embedded yet. A file embedded BEFORE the user denied it kept
/// its vector stored forever — and a vector is invertible to an approximation
/// of the text — so the only way to honor a new denial was to delete the whole
/// `index.db`.
///
/// This is checked against the SEARCH, not the table: what matters to the
/// user is that the denied file stops answering.
#[tokio::test]
async fn denying_after_embedding_forgets_the_vector() {
    let (engine, mem) = setup().await;
    write_file(&mem, &vp("mem:///public.txt"), b"annual report 2024").await;
    mem.mkdir(&vp("mem:///private")).await.expect("private");
    write_file(&mem, &vp("mem:///private/diary.txt"), b"intimate content").await;

    let (h, _) = engine
        .index_build_as(vp("mem:///"), Actor::User)
        .await
        .expect("build");
    assert_eq!(h.join().await, TaskState::Completed);
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("embed");
    assert_eq!(h.join().await, TaskState::Completed);

    // With both embedded, the private one answers to its own content.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "intimate content", 10)
        .await
        .expect("search");
    assert_eq!(hits[0].0, vp("mem:///private/diary.txt"));

    // The user denies it AFTERWARD.
    engine.set_ai_config(AiConfig {
        denied_prefixes: vec![vp("mem:///private")],
        ..ai_cfg()
    });
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("embed after denying");
    assert_eq!(h.join().await, TaskState::Completed);

    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "intimate content", 10)
        .await
        .expect("search after denying");
    assert!(
        hits.iter()
            .all(|(p, _)| p != &vp("mem:///private/diary.txt")),
        "the denied file's vector still answers: {hits:?}"
    );
    // And what is allowed is NOT swept away too: over-purging would mean
    // throwing away the index at the first denial.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "annual report 2024", 10)
        .await
        .expect("search public");
    assert_eq!(hits[0].0, vp("mem:///public.txt"));
}
