//! `Engine::index_embed_as` integration (M4-IA-2, ADR 0031 A3): the
//! `index.embed` task filters BEFORE reading (`denied_prefixes`, text
//! heuristic), skips unchanged hashes, retries rate limits within a bound, and
//! cancels cleanly. In-memory `MemProvider` + `FakeEmbed` → deterministic.

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
    VPath::parse(wire).expect("valid wire")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write opens");
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

/// Engine with an in-memory index + an injected `FakeEmbed` + the given config.
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

/// `index.build` of the root and join (a precondition for almost every test).
async fn build(engine: &Engine, root: &str) {
    let (h, _report) = engine
        .index_build_as(vp(root), Actor::User)
        .await
        .expect("index_build_as");
    assert_eq!(h.join().await, TaskState::Completed);
}

/// Every text that went out to the provider, in order.
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
    // Without a prior `index.build`: the error is in the RESPONSE, not the join.
    match engine.index_embed_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::NotFound) => {}
        Err(e) => panic!("expected NotFound, was {e:?}"),
        Ok(_) => panic!("expected NotFound without a prior build, it opened the Task"),
    }
}

#[tokio::test]
async fn embed_skips_non_text_and_records_only_text() {
    let (engine, mem, fake) = setup().await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    write_file(&mem, "mem:///b.md", b"beta content").await;
    write_file(&mem, "mem:///c.bin", &[0u8, 159, 146, 150]).await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    let inputs = sent_inputs(&fake);
    assert_eq!(inputs.len(), 2, "only the 2 text files: {inputs:?}");
    assert!(
        inputs.iter().any(|t| t.contains("alpha")),
        "a.txt's content went out to the provider"
    );
}

#[tokio::test]
async fn embed_skip_unchanged_hash_and_reembed_on_model_change() {
    let (engine, mem, fake) = setup().await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    write_file(&mem, "mem:///b.md", b"beta content").await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("first embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(sent_inputs(&fake).len(), 2, "the first run embeds both");

    // Second run: unchanged hashes → 0 new inputs.
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("second embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        sent_inputs(&fake).len(),
        2,
        "unchanged hash ⇒ nothing new goes to the provider"
    );

    // Configured MODEL change: the previous embeddings are stale → everything
    // gets re-embedded (the total doubles).
    let mut cfg = ai_cfg();
    cfg.providers[0].model = "another-model".into();
    engine.set_ai_config(cfg);
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("third embed");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        sent_inputs(&fake).len(),
        4,
        "new model ⇒ re-embeds everything"
    );
}

#[tokio::test]
async fn embed_denied_prefixes_excluded_before_read() {
    let mut cfg = ai_cfg();
    cfg.denied_prefixes = vec![vp("mem:///secret")];
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8), cfg).await;
    mem.mkdir(&vp("mem:///secret")).await.expect("mkdir");
    write_file(&mem, "mem:///secret/key.txt", b"SECRET").await;
    write_file(&mem, "mem:///normal.txt", b"normal content").await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    let inputs = sent_inputs(&fake);
    assert!(
        inputs.iter().all(|t| !t.contains("SECRET")),
        "content under denied_prefixes NEVER goes to the provider: {inputs:?}"
    );
    assert_eq!(inputs.len(), 1, "only the file outside the denied prefix");
}

/// #122: a LINK placed between the `build` and the `embed` does not sneak in
/// the content of a denied prefix.
///
/// The candidate comes from the row the build left behind (`kind = file`,
/// path), and the read happens afterward: whoever can write to the indexed
/// tree swaps a `.txt` for a link to a denied file and its 32 KiB would go to
/// the embedding provider, which would make "not a single byte of a denied
/// prefix is read" stop being true exactly where the module promises it.
#[tokio::test]
async fn a_link_placed_after_the_build_does_not_sneak_in_a_denied_prefix() {
    let mut cfg = ai_cfg();
    cfg.denied_prefixes = vec![vp("mem:///secret")];
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8), cfg).await;
    mem.mkdir(&vp("mem:///secret")).await.expect("mkdir");
    write_file(&mem, "mem:///secret/key.txt", b"SECRET").await;
    // A legitimate candidate, indexed as a text file.
    write_file(&mem, "mem:///normal.txt", b"normal content").await;
    write_file(&mem, "mem:///trap.txt", b"looks like text").await;
    build(&engine, "mem:///").await;

    // And BETWEEN the build and the embed, the swap.
    mem.remove(&vp("mem:///trap.txt")).await.expect("remove");
    mem.symlink(
        &vp("mem:///trap.txt"),
        b"secret/key.txt",
        norte_vfs::SymlinkKind::File,
    )
    .await
    .expect("symlink");

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);

    let inputs = sent_inputs(&fake);
    assert!(
        inputs.iter().all(|t| !t.contains("SECRET")),
        "the link cannot bring in what is denied: {inputs:?}"
    );
    assert_eq!(
        inputs.len(),
        1,
        "the swapped candidate is skipped, and the legitimate one still goes: {inputs:?}"
    );
}

/// Waits (bounded) for the fake provider to have received at least one batch:
/// the task is provably in flight.
async fn wait_first_call(fake: &FakeEmbed) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while fake.calls.lock().expect("calls lock").is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the first batch never reached the provider"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn embed_clean_cancellation() {
    // 40 files → 3 batches; 50ms of latency per batch. It cancels when the
    // first batch is provably IN FLIGHT (observed in `calls`): there are
    // batches ahead, so the cut is deterministic.
    let (engine, mem, fake) = setup_with(
        FakeEmbed::new(8).with_delay(Duration::from_millis(50)),
        ai_cfg(),
    )
    .await;
    for i in 0..40 {
        write_file(
            &mem,
            &format!("mem:///f{i}.txt"),
            format!("text {i}").as_bytes(),
        )
        .await;
    }
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    wait_first_call(&fake).await;
    h.cancel();
    assert_eq!(h.join().await, TaskState::Cancelled);

    // The index stays coherent: a later embed completes without trouble.
    let h2 = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("embed after cancelling");
    assert_eq!(h2.join().await, TaskState::Completed);
}

#[tokio::test]
async fn embed_cancel_during_rate_limit_retry_is_prompt() {
    // Persistent rate limit with a high retry_after: the task sits in the
    // retry sleep (clamped to 30s). Cancel must cut it RIGHT AWAY (select),
    // not when the sleep expires (which would end in Failed after ~60s).
    let mut fake = FakeEmbed::new(8).with_rate_limited(99);
    fake.retry_after = Some(60);
    let (engine, mem, fake) = setup_with(fake, ai_cfg()).await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    build(&engine, "mem:///").await;

    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    wait_first_call(&fake).await;
    h.cancel();
    let start = std::time::Instant::now();
    assert_eq!(h.join().await, TaskState::Cancelled);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the cancellation does not wait out the retry sleep"
    );
}

#[tokio::test]
async fn embed_rate_limit_retries_then_fails() {
    // 2 rate limits < EMBED_RETRY_MAX → the task retries internally and completes.
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8).with_rate_limited(2), ai_cfg()).await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    build(&engine, "mem:///").await;
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        fake.calls.lock().expect("calls lock").len(),
        3,
        "2 rate-limited attempts + 1 good one"
    );

    // Persistent rate limit → the task FAILS with ProviderUnavailable (never
    // hangs in infinite retries).
    let (engine, mem, _fake) = setup_with(FakeEmbed::new(8).with_rate_limited(99), ai_cfg()).await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    build(&engine, "mem:///").await;
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    match h.join().await {
        TaskState::Failed {
            error: norte_proto::Error::ProviderUnavailable { .. },
        } => {}
        st => panic!("expected Failed(ProviderUnavailable), was {st:?}"),
    }
}

#[tokio::test]
async fn embed_gate_disabled_is_policy_denied() {
    let mut cfg = ai_cfg();
    cfg.enabled = false;
    let (engine, mem, fake) = setup_with(FakeEmbed::new(8), cfg).await;
    write_file(&mem, "mem:///a.txt", b"alpha content").await;
    build(&engine, "mem:///").await;

    match engine.index_embed_as(vp("mem:///"), Actor::User).await {
        Err(norte_proto::Error::PolicyDenied { .. }) => {}
        Err(e) => panic!("expected PolicyDenied with AI off, was {e:?}"),
        Ok(_) => panic!("expected PolicyDenied with AI off, it opened the Task"),
    }
    assert!(
        fake.calls.lock().expect("calls lock").is_empty(),
        "with AI disabled nothing goes to the provider"
    );
}

/// Seed + build + embed (Completed): precondition for the search tests.
async fn seed_and_embed(engine: &Engine, mem: &MemProvider) {
    write_file(mem, "mem:///a.txt", b"alpha content").await;
    write_file(mem, "mem:///b.md", b"beta content").await;
    build(engine, "mem:///").await;
    let h = engine
        .index_embed_as(vp("mem:///"), Actor::User)
        .await
        .expect("index_embed_as");
    assert_eq!(h.join().await, TaskState::Completed);
}

#[tokio::test]
async fn semantic_search_finds_exact_content_top1() {
    let (engine, mem, _fake) = setup().await;
    seed_and_embed(&engine, &mem).await;

    // query == a.txt's exact content ⇒ deterministic FakeEmbed ⇒ identical
    // vector ⇒ cos ~1.0 and top-1.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "alpha content", 10)
        .await
        .expect("search alpha");
    assert!(!hits.is_empty(), "there are embeddings, there must be hits");
    assert_eq!(hits[0].0, vp("mem:///a.txt"));
    assert!((hits[0].1 - 1.0).abs() < 1e-5, "top-1 score: {}", hits[0].1);

    // root None also finds it (global sweep).
    let hits = engine
        .index_search_semantic(None, "beta content", 10)
        .await
        .expect("search beta");
    assert_eq!(hits[0].0, vp("mem:///b.md"));
    // All scores finite (the wire's anti-NaN belt) and descending order.
    assert!(hits.iter().all(|(_, s)| s.is_finite()));
    assert!(hits.windows(2).all(|w| w[0].1 >= w[1].1));
}

#[tokio::test]
async fn semantic_search_clamps_k_and_ignores_stale_model() {
    let (engine, mem, _fake) = setup().await;
    seed_and_embed(&engine, &mem).await;

    // k=0 ⇒ clamped to 1 ⇒ at most 1 hit (not an error).
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "alpha content", 0)
        .await
        .expect("k=0 is not an error");
    assert_eq!(hits.len(), 1, "k=0 is clamped to 1");

    // k=1000 ⇒ clamped to the wire's MAX, without error.
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "alpha content", 1000)
        .await
        .expect("k=1000 is not an error");
    assert_eq!(hits.len(), 2, "there are only 2 vectors");

    // Model change in config ⇒ the persisted vectors are stale for the new
    // model ⇒ invisible ⇒ 0 hits.
    let mut cfg = ai_cfg();
    cfg.providers[0].model = "another-model".into();
    engine.set_ai_config(cfg);
    let hits = engine
        .index_search_semantic(Some(&vp("mem:///")), "alpha content", 10)
        .await
        .expect("new model is not an error");
    assert!(hits.is_empty(), "vectors from another model are invisible");
}

#[tokio::test]
async fn semantic_search_garbage_query_vector_is_provider_error() {
    // `FakeEmbed` of dim 0 ⇒ an EMPTY query vector: a garbage provider. The
    // belt turns it into an honest error (same taxonomy as the 0-vectors lie),
    // never silent 0 hits. The NaN/zero-norm variants share the same branch
    // (norm2 not finite or ≤0) and are documented in the engine; FakeEmbed has
    // no knob to emit them.
    let (engine, _mem, _fake) = setup_with(FakeEmbed::new(0), ai_cfg()).await;
    match engine.index_search_semantic(None, "hello", 10).await {
        Err(norte_proto::Error::ProviderUnavailable { retryable: false }) => {}
        other => panic!("expected non-retryable ProviderUnavailable, was {other:?}"),
    }
}

#[tokio::test]
async fn semantic_search_unsupported_and_gate() {
    // Engine with an index but WITHOUT an embedding provider ⇒ Unsupported.
    let index = norte_index::Index::open_memory().await.expect("index");
    let engine = Engine::new().with_index(Arc::new(index));
    engine.set_ai_config(ai_cfg());
    match engine
        .index_search_semantic(Some(&vp("mem:///")), "hello", 10)
        .await
    {
        Err(norte_proto::Error::Unsupported) => {}
        other => panic!("expected Unsupported without a provider, was {other:?}"),
    }

    // With a provider but AI off ⇒ PolicyDenied (PRE-embed gate).
    let (engine, mem, fake) = setup().await;
    seed_and_embed(&engine, &mem).await;
    let mut cfg = ai_cfg();
    cfg.enabled = false;
    engine.set_ai_config(cfg);
    let before = fake.calls.lock().expect("calls lock").len();
    match engine
        .index_search_semantic(Some(&vp("mem:///")), "alpha content", 10)
        .await
    {
        Err(norte_proto::Error::PolicyDenied { .. }) => {}
        other => panic!("expected PolicyDenied with AI off, was {other:?}"),
    }
    assert_eq!(
        fake.calls.lock().expect("calls lock").len(),
        before,
        "with AI off the query never goes to the provider"
    );
}
