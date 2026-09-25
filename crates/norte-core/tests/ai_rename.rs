//! M4-A2 (ADR 0031): `Engine::ai_rename_plan` with a FAKE AI provider (canned
//! reply) and a `MemProvider` — the reviewable rename's end-to-end flow WITHOUT
//! a network: gate → listing → prompt → validation → plan.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_ai::{AiCaps, AiError, AiProvider, ChatRequest, ChatStream, ModelInfo};
use norte_core::Engine;
use norte_core::ai::AiConfig;
use norte_proto::{Error, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

/// Fake provider: returns a fixed rename JSON, markable as local or remote.
/// Records whether `chat` ever got called (to prove the gate cuts BEFORE
/// touching the provider).
struct FakeAi {
    reply: String,
    local: bool,
    called: Arc<std::sync::atomic::AtomicBool>,
    /// What it was SENT, in full. It is the only thing that lets a test check
    /// that a plan over what is marked does not carry the other names (#121):
    /// the AI gate exists to bound what leaves the machine, and without
    /// looking at the prompt a test only checks what comes back.
    prompt: Arc<std::sync::Mutex<String>>,
}

#[async_trait]
impl AiProvider for FakeAi {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature (&self→&str)"
    )]
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self) -> AiCaps {
        AiCaps::STREAMING
    }
    fn is_local(&self) -> bool {
        self.local
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, AiError> {
        self.called.store(true, std::sync::atomic::Ordering::SeqCst);
        *self.prompt.lock().expect("prompt") = format!("{_req:?}");
        // Delivered in TWO deltas to exercise draining the stream.
        let (a, b) = self.reply.split_at(self.reply.len() / 2);
        let items = vec![Ok(a.to_owned()), Ok(b.to_owned())];
        Ok(futures::stream::iter(items).boxed())
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: "fake".into(),
            context_window: None,
        }])
    }
}

fn vp(s: &str) -> VPath {
    VPath::parse(s).expect("wire")
}

async fn mkdirp(mem: &MemProvider, wire: &str) {
    // MemProvider requires the parent; ignore if it already exists.
    let _ = mem.mkdir(&vp(wire)).await;
}

async fn write_file(mem: &MemProvider, wire: &str) {
    let mut sink = mem.write(&vp(wire)).await.expect("write");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
}

fn engine_with(
    reply: &str,
    local: bool,
    config: AiConfig,
) -> (Engine, Arc<MemProvider>, Arc<std::sync::atomic::AtomicBool>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    engine.set_ai_provider(Arc::new(FakeAi {
        reply: reply.to_owned(),
        local,
        called: Arc::clone(&called),
        prompt: Arc::new(std::sync::Mutex::new(String::new())),
    }));
    engine.set_ai_config(config);
    (engine, mem, called)
}

/// The same setup, also returning WHAT WAS SENT to the provider.
fn engine_spying(
    reply: &str,
    config: AiConfig,
) -> (Engine, Arc<MemProvider>, Arc<std::sync::Mutex<String>>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let prompt = Arc::new(std::sync::Mutex::new(String::new()));
    engine.set_ai_provider(Arc::new(FakeAi {
        reply: reply.to_owned(),
        local: true,
        called: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        prompt: Arc::clone(&prompt),
    }));
    engine.set_ai_config(config);
    (engine, mem, prompt)
}

fn enabled() -> AiConfig {
    AiConfig {
        enabled: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn rename_plan_end_to_end() {
    let reply = r#"[{"from":"A.TXT","to":"a.txt"},{"from":"B.TXT","to":"b.txt"}]"#;
    let (engine, mem, called) = engine_with(reply, true, enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/A.TXT").await;
    write_file(&mem, "mem:///d/B.TXT").await;

    let plan = engine
        .ai_rename_plan(&vp("mem:///d"), "lowercase all")
        .await
        .expect("plan");
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = plan
        .entries
        .iter()
        .map(|e| (e.from.as_bytes().to_vec(), e.to.as_bytes().to_vec()))
        .collect();
    assert!(pairs.contains(&(b"A.TXT".to_vec(), b"a.txt".to_vec())));
    assert!(pairs.contains(&(b"B.TXT".to_vec(), b"b.txt".to_vec())));
    assert!(called.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn disabled_gate_cuts_before_the_provider() {
    let (engine, mem, called) = engine_with("[]", true, AiConfig::default());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/A").await;
    let err = engine
        .ai_rename_plan(&vp("mem:///d"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PolicyDenied { ref rule } if rule == "ai-disabled"));
    assert!(
        !called.load(std::sync::atomic::Ordering::SeqCst),
        "the gate cut BEFORE touching the provider (nothing went out)"
    );
}

#[tokio::test]
async fn local_only_gate_rejects_a_remote_provider() {
    let cfg = AiConfig {
        enabled: true,
        local_only: true,
        ..Default::default()
    };
    let (engine, mem, called) = engine_with("[]", false, cfg);
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/A").await;
    let err = engine
        .ai_rename_plan(&vp("mem:///d"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PolicyDenied { ref rule } if rule == "ai-local-only"));
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn denied_prefix_gate_rejects() {
    let cfg = AiConfig {
        enabled: true,
        denied_prefixes: vec![vp("mem:///secret")],
        ..Default::default()
    };
    let (engine, mem, called) = engine_with("[]", true, cfg);
    mkdirp(&mem, "mem:///secret").await;
    write_file(&mem, "mem:///secret/A").await;
    let err = engine
        .ai_rename_plan(&vp("mem:///secret"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PolicyDenied { ref rule } if rule == "ai-denied-path"));
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
}

/// Without a provider installed: Unsupported (not a panic).
#[tokio::test]
async fn without_a_provider_is_unsupported() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_ai_config(enabled());
    let err = engine
        .ai_rename_plan(&vp("mem:///d"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported));
}

/// A reply that violates validation (a destination with traversal) comes out
/// as a typed error, never as a partial plan.
#[tokio::test]
async fn a_hostile_reply_produces_no_partial_plan() {
    let (engine, mem, _) = engine_with(r#"[{"from":"A","to":"../evil"}]"#, true, enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/A").await;
    assert!(engine.ai_rename_plan(&vp("mem:///d"), "x").await.is_err());
}

/// The `Embedded` arm of [`norte_core::backend::Backend`] returns the plan
/// already mapped to protocol types (parity with the remote arm).
#[tokio::test]
async fn the_embedded_backend_returns_the_plan_in_proto_types() {
    let (engine, mem, _) = engine_with(r#"[{"from":"a.txt","to":"b.txt"}]"#, true, enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/a.txt").await;
    let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
    let plan = backend
        .ai_rename_plan(&vp("mem:///d"), "rename", &[])
        .await
        .expect("plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].from, "a.txt");
    assert_eq!(plan.entries[0].to, "b.txt");
}

/// The embedded arm's timeout does not swallow the engine's errors:
/// `Unsupported` (no provider) crosses the wrapper intact.
#[tokio::test]
async fn the_embedded_backend_propagates_unsupported_without_a_provider() {
    let engine = Engine::new(); // no set_ai_provider
    let backend = norte_core::backend::Backend::Embedded(Arc::new(engine));
    let err = backend
        .ai_rename_plan(&vp("mem:///"), "x", &[])
        .await
        .expect_err("no provider");
    assert!(matches!(err, Error::Unsupported), "was {err:?}");
}

struct CapturingAi {
    captured: Arc<std::sync::Mutex<String>>,
}
#[async_trait]
impl AiProvider for CapturingAi {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "the trait's signature (&self→&str)"
    )]
    fn id(&self) -> &str {
        "cap"
    }
    fn capabilities(&self) -> AiCaps {
        AiCaps::STREAMING
    }
    fn is_local(&self) -> bool {
        true
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        self.captured
            .lock()
            .unwrap()
            .clone_from(&req.messages[0].content);
        Ok(futures::stream::iter(vec![Ok("[]".to_owned())]).boxed())
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![])
    }
}

/// security MINOR #M4: the name of a CHILD dir under a `denied_prefix` does not
/// go to the provider — it is omitted from the listing before building the
/// prompt (the gate only checks the root `dir`).
#[tokio::test]
async fn a_child_under_a_denied_prefix_does_not_go_out() {
    let cfg = AiConfig {
        enabled: true,
        denied_prefixes: vec![vp("mem:///work/secret")],
        ..Default::default()
    };
    // The provider CAPTURES the prompt to verify 'secret' does not appear.
    let captured = Arc::new(std::sync::Mutex::new(String::new()));
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_ai_provider(Arc::new(CapturingAi {
        captured: Arc::clone(&captured),
    }));
    engine.set_ai_config(cfg);
    mkdirp(&mem, "mem:///work").await;
    mkdirp(&mem, "mem:///work/secret").await;
    write_file(&mem, "mem:///work/visible.txt").await;

    engine
        .ai_rename_plan(&vp("mem:///work"), "x")
        .await
        .expect("plan (empty)");
    let prompt = captured.lock().unwrap().clone();
    assert!(
        prompt.contains("visible.txt"),
        "the visible one does go out: {prompt}"
    );
    assert!(
        !prompt.contains("secret"),
        "the denied dir must NOT go out: {prompt}"
    );
}

/// **A plan over what is MARKED does not send the other names** (#121).
///
/// With first-class selection, marking five files and asking for a plan used
/// to send the directory's thousand names to the provider: more than what the
/// human pointed at, and the AI gate exists precisely to bound what leaves the
/// machine.
#[tokio::test]
async fn a_plan_over_whats_marked_does_not_send_the_rest() {
    let (engine, mem, prompt) = engine_spying("[]", enabled());
    mkdirp(&mem, "mem:///d").await;
    for n in ["marked.txt", "other.txt", "third.txt"] {
        write_file(&mem, &format!("mem:///d/{n}")).await;
    }

    engine
        .ai_rename_plan_for(&vp("mem:///d"), "x", &["marked.txt".to_owned()])
        .await
        .expect("plan");

    let seen = prompt.lock().expect("prompt").clone();
    assert!(seen.contains("marked.txt"), "{seen}");
    assert!(
        !seen.contains("other.txt"),
        "what is not marked does not go out: {seen}"
    );
    assert!(!seen.contains("third.txt"), "{seen}");
}

/// Without names, the WHOLE directory: this is what it did before the field
/// existed, and what a client that does not send it expects.
#[tokio::test]
async fn without_names_the_plan_is_still_the_whole_directorys() {
    let (engine, mem, prompt) = engine_spying("[]", enabled());
    mkdirp(&mem, "mem:///d").await;
    for n in ["a.txt", "b.txt"] {
        write_file(&mem, &format!("mem:///d/{n}")).await;
    }

    engine
        .ai_rename_plan_for(&vp("mem:///d"), "x", &[])
        .await
        .expect("plan");

    let seen = prompt.lock().expect("prompt").clone();
    assert!(seen.contains("a.txt") && seen.contains("b.txt"), "{seen}");
}

/// And if what was marked is already gone, the provider is not called: a plan
/// over nothing is not a question, and sending the instruction with an empty
/// list spends quota just to get the same answer back.
#[tokio::test]
async fn if_whats_marked_is_already_gone_it_does_not_ask() {
    let (engine, mem, prompt) = engine_spying("[]", enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/a.txt").await;

    let plan = engine
        .ai_rename_plan_for(&vp("mem:///d"), "x", &["gone.txt".to_owned()])
        .await
        .expect("empty plan");

    assert!(plan.entries.is_empty());
    assert!(
        prompt.lock().expect("prompt").is_empty(),
        "the provider was not called"
    );
}
