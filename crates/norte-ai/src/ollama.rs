//! Ollama provider (ADR 0031): chat over `/api/chat` (NDJSON streaming) and
//! embeddings over `/api/embed`. Local: no credentials (receives no secret)
//! and `is_local()` = `true` — v1 trusts that the configured host is local
//! (pointing it at a non-loopback host is the operator's decision; the
//! core's `local_only` gate is the real barrier).

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// Default base URL of the local Ollama daemon.
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

/// Client for a local Ollama daemon (`/api/chat` NDJSON, `/api/embed`).
///
/// - `capabilities()` = `STREAMING | EMBEDDINGS`.
/// - `is_local()` = `true`: the core's `local_only` mode lets it through.
///
/// # Examples
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
    /// `true` only if `base_url` points to loopback (`127.0.0.0/8`, `::1`,
    /// `localhost`). A remote host configured as ollama is NOT local — the
    /// core's `local_only` gate rejects it (security MAJOR from review #M4:
    /// an unconditional `is_local()` let names be exfiltrated to a remote
    /// host).
    local: bool,
}

impl OllamaProvider {
    /// Builds the provider. `base_url` `None` = the default local daemon
    /// (`http://127.0.0.1:11434`). No secret: Ollama is local. The
    /// `is_local` flag is DERIVED from `base_url`'s host (loopback only).
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
            // Client::new() only panics if the TLS stack fails to init; with
            // rustls compiled statically that is a build invariant.
            client: reqwest::Client::new(),
            local,
        }
    }

    /// `/api/chat` body: Ollama accepts an inline `system` role, so
    /// `req.system` is prepended as the first `system` message.
    /// `max_tokens` maps to `options.num_predict` (Ollama's equivalent).
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

/// `true` if `base_url`'s host is loopback (`127.0.0.0/8`, `::1`,
/// `localhost`). A `base_url` with no parseable host = NOT local
/// (fail-closed: when in doubt, the `local_only` gate rejects it). Does no
/// DNS resolution — a name that is not literally `localhost` is treated as
/// remote.
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

/// Interprets ONE NDJSON line from `/api/chat`: `.message.content` is a
/// delta; `.done == true` ends it; `.error` → [`AiError::Protocol`].
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    if line.trim().is_empty() {
        return Ok(WireEvent::Skip);
    }
    let v: Value = serde_json::from_str(line)
        .map_err(|e| AiError::Protocol(format!("invalid NDJSON line: {e}")))?;
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

/// `/api/embed` response (modern Ollama: `embeddings` field).
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
        // The returned stream owns the body: dropping it aborts the HTTP
        // request (rule 3, drop-based cancellation).
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
            .map_err(|e| AiError::Protocol(format!("invalid /api/embed response: {e}")))?;
        Ok(parsed.embeddings)
    }

    /// The configured model, without touching the network (v1 does not
    /// query `/api/tags`: offline-testable, ADR 0031).
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

    /// `chat()` must fail at setup time (`ChatStream` is not `Debug`, so
    /// `unwrap_err` does not apply).
    async fn chat_err(p: &OllamaProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("expected a setup error"),
            Err(e) => e,
        }
    }

    fn provider(base_url: &str) -> OllamaProvider {
        OllamaProvider::new(Some(base_url.to_string()), "llama-test".to_string())
    }

    /// Happy NDJSON: the `.message.content`s concatenate, `done: true` ends
    /// the stream and later lines are ignored.
    #[tokio::test]
    async fn chat_concatenates_and_done_ends_it() {
        let body = [
            r#"{"model":"llama-test","message":{"role":"assistant","content":"He"},"done":false}"#,
            r#"{"model":"llama-test","message":{"role":"assistant","content":"llo"},"done":false}"#,
            r#"{"model":"llama-test","message":{"role":"assistant","content":""},"done":true}"#,
            r#"{"message":{"content":"IGNORED"},"done":false}"#,
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url);
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.system = Some("dry tone".to_string());
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hello");

        let raw = srv.request().await;
        assert!(raw.contains("POST /api/chat"), "{raw}");
        // system goes inline as the first message with role `system`.
        assert!(
            raw.contains(r#"{"content":"dry tone","role":"system"}"#),
            "{raw}"
        );
    }

    /// **Ollama does not promise structured output and does not send it**
    /// (ADR 0088).
    ///
    /// This is the FALLBACK path, and until now it was taken for granted.
    /// That a contract in the request changes neither the body nor the
    /// capability is what backs enabling typed output in other providers
    /// without breaking this one: here it keeps answering whatever the
    /// prompt asks for, and the core accepts that shape.
    #[tokio::test]
    async fn a_contract_changes_neither_the_body_nor_the_capability() {
        let body = [
            r#"{"model":"llama-test","message":{"role":"assistant","content":"[]"},"done":true}"#,
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url);
        assert!(
            !p.capabilities().contains(AiCaps::JSON_OUTPUT),
            "nothing is promised that is not honored"
        );
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new(
            "norte_rename_plan",
            json!({"type": "object"}),
        ));
        let stream = p.chat(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let raw = srv.request().await;
        assert!(!raw.contains("json_schema"), "{raw}");
        assert!(!raw.contains("response_format"), "{raw}");
        assert!(!raw.contains("output_config"), "{raw}");
    }

    /// The daemon's `.error` field comes out as `Err(Protocol)` with the message.
    #[tokio::test]
    async fn a_daemon_error_is_protocol() {
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

    /// A broken NDJSON line (a tail truncated with no `\n`) is `Protocol`.
    #[tokio::test]
    async fn truncated_ndjson_is_protocol() {
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

    /// `/api/embed`: returns the vectors in order.
    #[tokio::test]
    async fn embed_returns_vectors() {
        let body = r#"{"model":"llama-test","embeddings":[[1.0,2.0],[3.5]]}"#;
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url);
        let vecs = p
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert_eq!(vecs, vec![vec![1.0, 2.0], vec![3.5]]);

        let raw = srv.request().await;
        assert!(raw.contains("POST /api/embed"), "{raw}");
        assert!(raw.contains(r#""input":["one","two"]"#), "{raw}");
    }

    /// A non-2xx status from the daemon maps to `Http` with the code.
    #[tokio::test]
    async fn status_500_is_http() {
        let srv = serve_once(response(500, "Internal Server Error", &[], "")).await;
        let p = provider(&srv.base_url);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Http { status: 500 }), "{err:?}");
    }

    /// security MAJOR #M4: `is_local()` is `true` ONLY for loopback. A
    /// remote host configured as ollama is NOT local → the core's
    /// `local_only` gate rejects it.
    #[test]
    fn is_local_only_loopback() {
        let loc = |u: &str| OllamaProvider::new(Some(u.into()), "m".into()).is_local();
        assert!(loc("http://127.0.0.1:11434"));
        assert!(loc("http://localhost:11434"));
        assert!(loc("http://[::1]:11434"));
        assert!(loc("http://127.0.0.5"));
        assert!(!loc("http://attacker.example:11434"));
        assert!(!loc("http://10.0.0.9:11434"));
        assert!(!loc("http://192.168.1.5:11434"));
        // Default (None) is loopback.
        assert!(OllamaProvider::new(None, "m".into()).is_local());
    }
}
