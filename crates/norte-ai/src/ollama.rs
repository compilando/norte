//! Proveedor Ollama (ADR 0031): chat por `/api/chat` (NDJSON streaming) y
//! embeddings por `/api/embed`. Local: sin credenciales (no recibe secreto) y
//! `is_local()` = `true` — v1 confía en que el host configurado es local
//! (apuntarlo a un host no-loopback es decisión del operador; el gate
//! `local_only` del core es la barrera real).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// URL base por defecto del daemon local de Ollama.
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

/// Cliente de un daemon Ollama local (`/api/chat` NDJSON, `/api/embed`).
///
/// - `capabilities()` = `STREAMING | EMBEDDINGS`.
/// - `is_local()` = `true`: el modo `local_only` del core lo deja pasar.
///
/// # Ejemplos
/// ```
/// use norte_ai::AiProvider as _;
/// use norte_ai::ollama::OllamaProvider;
///
/// let p = OllamaProvider::new(None, "llama3".to_string());
/// assert_eq!(p.id(), "ollama");
/// assert!(p.is_local());
/// ```
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    base_url: String,
    model: String,
    client: reqwest::Client,
    /// `true` solo si el `base_url` apunta a loopback (`127.0.0.0/8`, `::1`,
    /// `localhost`). Un host remoto configurado como ollama NO es local — el
    /// gate `local_only` del core lo rechaza (security MAJOR del review #M4:
    /// `is_local()` incondicional dejaba exfiltrar nombres a un host remoto).
    local: bool,
}

impl OllamaProvider {
    /// Construye el proveedor. `base_url` `None` = el daemon local por
    /// defecto (`http://127.0.0.1:11434`). Sin secreto: Ollama es local. El
    /// flag `is_local` se DERIVA del host del `base_url` (solo loopback).
    #[must_use]
    pub fn new(base_url: Option<String>, model: String) -> Self {
        let base_url = base_url
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let local = base_url_is_loopback(&base_url);
        Self {
            base_url,
            model,
            // Client::new() solo panica si la pila TLS no inicializa; con
            // rustls compilado estático es un invariante del build.
            client: reqwest::Client::new(),
            local,
        }
    }

    /// Body de `/api/chat`: Ollama acepta el rol `system` inline, así que
    /// `req.system` se antepone como primer mensaje `system`. `max_tokens`
    /// se mapea a `options.num_predict` (el equivalente de Ollama).
    fn build_body(&self, req: &ChatRequest) -> Result<Value, AiError> {
        http::validate_turns(req)?;
        let mut messages = Vec::new();
        if let Some(s) = &req.system {
            messages.push(json!({"role": "system", "content": s}));
        }
        for m in &req.messages {
            let role = match m.role {
                ChatRole::System => "system",
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            messages.push(json!({"role": role, "content": m.content}));
        }
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "messages": messages,
        });
        if let Some(n) = req.max_tokens {
            body["options"] = json!({ "num_predict": n });
        }
        Ok(body)
    }
}

/// `true` si el host del `base_url` es loopback (`127.0.0.0/8`, `::1`,
/// `localhost`). Un `base_url` sin host parseable = NO local (fail-closed:
/// ante la duda, el gate `local_only` lo rechaza). No hace resolución DNS —
/// un nombre que no sea literalmente `localhost` se trata como remoto.
fn base_url_is_loopback(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    match url.host_str() {
        Some("localhost") => true,
        Some(h) => h
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// Interpreta UNA línea NDJSON de `/api/chat`: `.message.content` es un
/// delta; `.done == true` termina; `.error` → [`AiError::Protocol`].
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    if line.trim().is_empty() {
        return Ok(WireEvent::Skip);
    }
    let v: Value = serde_json::from_str(line)
        .map_err(|e| AiError::Protocol(format!("línea NDJSON inválida: {e}")))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .as_str()
            .map_or_else(|| err.to_string(), ToString::to_string);
        return Err(AiError::Protocol(msg));
    }
    if v.get("done").and_then(Value::as_bool) == Some(true) {
        return Ok(WireEvent::Stop);
    }
    match v.pointer("/message/content").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => Ok(WireEvent::Delta(t.to_string())),
        _ => Ok(WireEvent::Skip),
    }
}

/// Respuesta de `/api/embed` (Ollama moderno: campo `embeddings`).
#[derive(serde::Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl AiProvider for OllamaProvider {
    fn id(&self) -> &'static str {
        "ollama"
    }

    fn capabilities(&self) -> AiCaps {
        AiCaps::STREAMING | AiCaps::EMBEDDINGS
    }

    fn is_local(&self) -> bool {
        self.local
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "ollama"))]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        let body = self.build_body(&req)?;
        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        let resp = http::check_status(resp)?;
        // El stream devuelto posee el body: dropearlo aborta la petición
        // HTTP (regla 3, cancelación drop-based).
        Ok(http::delta_stream(resp, parse_line))
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "ollama"))]
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        let body = json!({ "model": self.model, "input": inputs });
        let resp = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        let resp = http::check_status(resp)?;
        let raw = resp.bytes().await.map_err(|e| http::transport(&e))?;
        let parsed: EmbedResponse = serde_json::from_slice(&raw)
            .map_err(|e| AiError::Protocol(format!("respuesta de /api/embed inválida: {e}")))?;
        Ok(parsed.embeddings)
    }

    /// El modelo configurado, sin tocar la red (v1 no consulta `/api/tags`:
    /// offline-testable, ADR 0031).
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(vec![ModelInfo {
            id: self.model.clone(),
            context_window: None,
        }])
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::http::testutil::{response, serve_once};
    use crate::provider::ChatMessage;

    /// `chat()` debe fallar en el establecimiento (el `ChatStream` no es
    /// `Debug`, así que `unwrap_err` no aplica).
    async fn chat_err(p: &OllamaProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("esperaba un error de establecimiento"),
            Err(e) => e,
        }
    }

    fn provider(base_url: &str) -> OllamaProvider {
        OllamaProvider::new(Some(base_url.to_string()), "llama-test".to_string())
    }

    /// NDJSON feliz: los `.message.content` se concatenan, `done: true` para
    /// el stream y las líneas posteriores se ignoran.
    #[tokio::test]
    async fn chat_concatena_y_done_para() {
        let body = [
            r#"{"model":"llama-test","message":{"role":"assistant","content":"Ho"},"done":false}"#,
            r#"{"model":"llama-test","message":{"role":"assistant","content":"la"},"done":false}"#,
            r#"{"model":"llama-test","message":{"role":"assistant","content":""},"done":true}"#,
            r#"{"message":{"content":"IGNORADO"},"done":false}"#,
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url);
        let mut req = ChatRequest::new(vec![ChatMessage::user("hola")]);
        req.system = Some("tono seco".to_string());
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hola");

        let raw = srv.request().await;
        assert!(raw.contains("POST /api/chat"), "{raw}");
        // El system va inline como primer mensaje con rol `system`.
        assert!(
            raw.contains(r#"{"content":"tono seco","role":"system"}"#),
            "{raw}"
        );
    }

    /// El campo `.error` del daemon sale como `Err(Protocol)` con el mensaje.
    #[tokio::test]
    async fn error_del_daemon_es_protocol() {
        let body = "{\"error\":\"model 'nope' not found\"}\n";
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url);
        let mut stream = p
            .chat(ChatRequest::new(vec![ChatMessage::user("x")]))
            .await
            .unwrap();
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(
            matches!(&err, AiError::Protocol(m) if m.contains("not found")),
            "{err:?}"
        );
        assert!(stream.next().await.is_none());
    }

    /// Una línea NDJSON rota (cola truncada sin `\n`) es `Protocol`.
    #[tokio::test]
    async fn ndjson_truncado_es_protocol() {
        let body = "{\"message\":{\"content\":\"a\"},\"done\":false}\n{\"mess";
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url);
        let mut stream = p
            .chat(ChatRequest::new(vec![ChatMessage::user("x")]))
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "a");
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(matches!(err, AiError::Protocol(_)), "{err:?}");
    }

    /// `/api/embed`: devuelve los vectores en orden.
    #[tokio::test]
    async fn embed_devuelve_vectores() {
        let body = r#"{"model":"llama-test","embeddings":[[1.0,2.0],[3.5]]}"#;
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url);
        let vecs = p
            .embed(&["uno".to_string(), "dos".to_string()])
            .await
            .unwrap();
        assert_eq!(vecs, vec![vec![1.0, 2.0], vec![3.5]]);

        let raw = srv.request().await;
        assert!(raw.contains("POST /api/embed"), "{raw}");
        assert!(raw.contains(r#""input":["uno","dos"]"#), "{raw}");
    }

    /// Un status no-2xx del daemon se mapea a `Http` con el código.
    #[tokio::test]
    async fn status_500_es_http() {
        let srv = serve_once(response(500, "Internal Server Error", &[], "")).await;
        let p = provider(&srv.base_url);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Http { status: 500 }), "{err:?}");
    }

    /// security MAJOR #M4: `is_local()` es `true` SOLO para loopback. Un host
    /// remoto configurado como ollama NO es local → el gate `local_only` del
    /// core lo rechaza.
    #[test]
    fn is_local_solo_loopback() {
        let loc = |u: &str| OllamaProvider::new(Some(u.into()), "m".into()).is_local();
        assert!(loc("http://127.0.0.1:11434"));
        assert!(loc("http://localhost:11434"));
        assert!(loc("http://[::1]:11434"));
        assert!(loc("http://127.0.0.5"));
        assert!(!loc("http://attacker.example:11434"));
        assert!(!loc("http://10.0.0.9:11434"));
        assert!(!loc("http://192.168.1.5:11434"));
        // Default (None) es loopback.
        assert!(OllamaProvider::new(None, "m".into()).is_local());
    }
}
