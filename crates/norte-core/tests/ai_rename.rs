//! M4-A2 (ADR 0031): `Engine::ai_rename_plan` con un proveedor de IA FALSO
//! (respuesta canned) y un `MemProvider` — el flujo end-to-end del rename
//! revisable SIN red: gate → listado → prompt → validación → plan.

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

/// Proveedor falso: devuelve un JSON de rename fijo, marcable como local o
/// remoto. Registra si `chat` llegó a llamarse (para probar que el gate corta
/// ANTES de tocar el proveedor).
struct FakeAi {
    reply: String,
    local: bool,
    called: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl AiProvider for FakeAi {
    #[allow(clippy::unnecessary_literal_bound)] // firma del trait (&self→&str)
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
        // Entrega en DOS deltas para ejercitar el drenado del stream.
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
    // MemProvider exige el padre; ignora si ya existe.
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
    }));
    engine.set_ai_config(config);
    (engine, mem, called)
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
async fn gate_deshabilitado_corta_antes_del_proveedor() {
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
        "el gate cortó ANTES de tocar el proveedor (nada salió)"
    );
}

#[tokio::test]
async fn gate_local_only_rechaza_proveedor_remoto() {
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
async fn gate_denied_prefix_rechaza() {
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

/// Sin proveedor instalado: Unsupported (no un panic).
#[tokio::test]
async fn sin_proveedor_es_unsupported() {
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

/// Una respuesta que viola la validación (destino con traversal) sale como
/// error tipado, jamás como un plan parcial.
#[tokio::test]
async fn respuesta_hostil_no_produce_plan_parcial() {
    let (engine, mem, _) = engine_with(r#"[{"from":"A","to":"../evil"}]"#, true, enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/A").await;
    assert!(engine.ai_rename_plan(&vp("mem:///d"), "x").await.is_err());
}

/// El brazo `Embedded` de [`norte_core::backend::Backend`] devuelve el plan
/// ya mapeado
/// a tipos del protocolo (paridad con el brazo remoto).
#[tokio::test]
async fn backend_embebido_devuelve_el_plan_en_tipos_proto() {
    let (engine, mem, _) = engine_with(r#"[{"from":"a.txt","to":"b.txt"}]"#, true, enabled());
    mkdirp(&mem, "mem:///d").await;
    write_file(&mem, "mem:///d/a.txt").await;
    let backend = norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine));
    let plan = backend
        .ai_rename_plan(&vp("mem:///d"), "renombra")
        .await
        .expect("plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].from, "a.txt");
    assert_eq!(plan.entries[0].to, "b.txt");
}

struct CapturingAi {
    captured: Arc<std::sync::Mutex<String>>,
}
#[async_trait]
impl AiProvider for CapturingAi {
    #[allow(clippy::unnecessary_literal_bound)]
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

/// security MINOR #M4: el nombre de un dir HIJO bajo un `denied_prefix` no
/// sale al proveedor — se omite del listado antes de construir el prompt (el
/// gate solo comprueba el `dir` raíz).
#[tokio::test]
async fn child_bajo_denied_prefix_no_sale() {
    let cfg = AiConfig {
        enabled: true,
        denied_prefixes: vec![vp("mem:///work/secret")],
        ..Default::default()
    };
    // El proveedor CAPTURA el prompt para verificar que 'secret' no aparece.
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
        .expect("plan (vacío)");
    let prompt = captured.lock().unwrap().clone();
    assert!(
        prompt.contains("visible.txt"),
        "el visible sí sale: {prompt}"
    );
    assert!(
        !prompt.contains("secret"),
        "el dir denegado NO debe salir: {prompt}"
    );
}
