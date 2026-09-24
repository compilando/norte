//! Anthropic provider (Messages API, SSE streaming — ADR 0031). Chat in
//! deltas; NO embeddings (Anthropic does not offer that API: the capability
//! stays absent, honestly). The api key is INJECTED as a
//! [`norte_connect::Secret`] (zeroizing, redacted `Debug`) and is never
//! logged (rule 10).

use async_trait::async_trait;
use norte_connect::Secret;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// Default base URL of Anthropic's public API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// `anthropic-version` header (mandatory in the Messages API).
const API_VERSION: &str = "2023-06-01";
/// Output token cap when the request does not set one (the Messages API
/// always requires it in the body).
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The model families whose structured output (`output_config.format`) is
/// supported.
///
/// Checked by PREFIX because ids carry variants and dates, and the
/// capability is only declared if it matches: `JSON_OUTPUT` used to be a
/// declared capability nobody honored —the body never carried the schema—,
/// and the way it stays that way is for the declaration to depend on what is
/// genuinely sent. A model not in this list does NOT declare the capability
/// and falls back to the prompt path, which is where the project already
/// knew how to be.
///
/// **Source and date, because this list EXPIRES**: the "Compatibility"
/// section of
/// `platform.claude.com/docs/en/build-with-claude/structured-outputs`,
/// consulted on 2026-09-01. Erring on the side of missing is cheap —it
/// falls back to the prompt, which works— and erring on the side of
/// including is a 400: when in doubt, leave it out.
///
/// `claude-sonnet-4-5` carries a DATE on purpose: the documentation lists
/// `claude-sonnet-4-5-20250929` and not the short alias, so a bare
/// `claude-sonnet-4-5` falls back to the prompt. The rest carry the short
/// prefix because the alias IS the model's current id.
///
/// `claude-opus-4-1` was here and should not have been: it does not appear
/// in the list.
const STRUCTURED_OUTPUT_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-mythos-5",
    "claude-mythos-preview",
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-opus-4-5",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-sonnet-4-5-20250929",
    "claude-haiku-4-5",
];

/// Does this model support `output_config.format`?
fn supports_structured_output(model: &str) -> bool {
    STRUCTURED_OUTPUT_MODELS
        .iter()
        .any(|m| model.starts_with(m))
}

/// Client for Anthropic's Messages API (`POST /v1/messages`, SSE).
///
/// - `capabilities()` = `STREAMING`, plus `JSON_OUTPUT` **if the configured
///   model supports structured output** (no `EMBEDDINGS`, which Anthropic
///   does not offer). Declaring it unconditionally used to promise a format
///   an old model does not give (ADR 0088).
/// - Remote: `is_local()` is `false`; the core's `local_only` gate vetoes it.
/// - With no secret configured, `chat` returns [`AiError::Auth`] (it never
///   sends a request without a credential).
///
/// # Examples
/// ```
/// use norte_ai::AiProvider as _;
/// use norte_ai::anthropic::AnthropicProvider;
///
/// let p = AnthropicProvider::new(None, "claude-opus-4-8".to_string(), None);
/// assert_eq!(p.id(), "anthropic");
/// assert!(!p.is_local());
/// ```
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
    model: String,
    // The derived Debug is safe: `Secret` redacts its content (rule 10).
    secret: Option<Secret>,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Builds the provider. `base_url` `None` = the public API
    /// (`https://api.anthropic.com`); the secret arrives INJECTED by the
    /// core (env → keyring → age resolution in `norte-connect`, never here).
    #[must_use]
    pub fn new(base_url: Option<String>, model: String, secret: Option<Secret>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            model,
            secret,
            // Client::new() only panics if the TLS stack fails to init; with
            // rustls compiled statically that is a build invariant.
            client: reqwest::Client::new(),
        }
    }

    /// `/v1/messages` body: `req.messages`'s `System` turns are merged
    /// (together with `req.system`, joined by `\n`) into the top-level
    /// `system` field — the Messages API does not accept an inline `system`
    /// role.
    fn build_body(&self, req: &ChatRequest) -> Result<Value, AiError> {
        http::validate_turns(req)?;
        let mut system_parts: Vec<&str> = Vec::new();
        if let Some(s) = &req.system {
            system_parts.push(s);
        }
        let mut messages = Vec::new();
        for m in &req.messages {
            match m.role {
                ChatRole::System => system_parts.push(&m.content),
                ChatRole::User => messages.push(json!({"role": "user", "content": m.content})),
                ChatRole::Assistant => {
                    messages.push(json!({"role": "assistant", "content": m.content}));
                }
            }
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).max(1),
            "stream": true,
            "messages": messages,
        });
        if !system_parts.is_empty() {
            body["system"] = Value::String(system_parts.join("\n"));
        }
        // The typed-output contract, if there is one AND this model honors
        // it. The Messages API reads it from `output_config.format`; the
        // contract's name does not travel because it is not used here.
        //
        // The RESPONSE is still a text block: `output_config.format`
        // restricts that block's content, it does not introduce a new block
        // type, so `parse_line` reads it through the same `text_delta` as
        // everything else. This is documented and NOT observed against the
        // live API — no network call happens here (ADR 0031). If that
        // assumption were false, the symptom would be an empty `reply`
        // exactly in the models that activate this path; that is the first
        // thing to look at.
        if let Some(contract) = &req.json_schema
            && supports_structured_output(&self.model)
        {
            body["output_config"] = json!({
                "format": {
                    "type": "json_schema",
                    "schema": contract.schema,
                }
            });
        }
        Ok(body)
    }
}

/// Interprets ONE SSE line from the Messages API: `text_delta` → delta,
/// `message_stop` → end, `error` event → [`AiError::Protocol`]; the rest
/// (`message_start`, `ping`, `event:`…) is ignored.
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    let Some(payload) = http::sse_data(line) else {
        return Ok(WireEvent::Skip);
    };
    let v: Value = serde_json::from_str(payload)
        .map_err(|e| AiError::Protocol(format!("invalid SSE data: {e}")))?;
    match v.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => {
            let delta = v.get("delta");
            if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta") {
                let text = delta
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| AiError::Protocol("text_delta with no `text` field".into()))?;
                Ok(WireEvent::Delta(text.to_string()))
            } else {
                Ok(WireEvent::Skip)
            }
        }
        Some("message_stop") => Ok(WireEvent::Stop),
        Some("error") => {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("provider error with no message");
            Err(AiError::Protocol(msg.to_string()))
        }
        _ => Ok(WireEvent::Skip),
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn capabilities(&self) -> AiCaps {
        let mut caps = AiCaps::STREAMING;
        // Only declared if the CONFIGURED model has it. Declaring it
        // unconditionally used to tell the core it can trust the format when
        // with an old model it cannot.
        if supports_structured_output(&self.model) {
            caps |= AiCaps::JSON_OUTPUT;
        }
        caps
    }

    fn is_local(&self) -> bool {
        false
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "anthropic"))]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        // With no credential, NOTHING gets sent (fail-closed).
        let Some(secret) = &self.secret else {
            return Err(AiError::Auth);
        };
        let body = self.build_body(&req)?;
        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", secret.expose())
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        let resp = http::check_status(resp)?;
        // The returned stream owns the body: dropping it aborts the HTTP
        // request (rule 3, drop-based cancellation).
        Ok(http::delta_stream(resp, parse_line))
    }

    /// The configured model, without touching the network. A live endpoint
    /// exists (`GET /v1/models`) but v1 stays offline-testable (ADR 0031:
    /// client details are pinned with fixtures, not with live calls).
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
    async fn chat_err(p: &AnthropicProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("expected a setup error"),
            Err(e) => e,
        }
    }

    fn provider(base_url: &str, secret: Option<&str>) -> AnthropicProvider {
        AnthropicProvider::new(
            Some(base_url.to_string()),
            "claude-test".to_string(),
            secret.map(|s| Secret::new(s.to_string())),
        )
    }

    fn sse_ok() -> String {
        [
            "event: message_start",
            r#"data: {"type":"message_start","message":{"id":"m1"}}"#,
            "",
            r#"data: {"type":"content_block_start","index":0}"#,
            "",
            r#"data: {"type":"ping"}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"world"}}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ]
        .join("\n")
    }

    /// Happy SSE: concatenating the deltas is the full text, and the
    /// request carries the api key, the version and the merged system.
    #[tokio::test]
    async fn chat_concatenates_deltas_and_sends_headers() {
        let srv = serve_once(response(
            200,
            "OK",
            &[("content-type", "text/event-stream")],
            &sse_ok(),
        ))
        .await;
        let p = provider(&srv.base_url, Some("sk-test-123"));
        let req = ChatRequest::new(vec![
            ChatMessage::system("dry tone"),
            ChatMessage::user("hello"),
        ]);
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hello world");

        let raw = srv.request().await;
        assert!(raw.contains("POST /v1/messages"), "{raw}");
        assert!(raw.contains("x-api-key: sk-test-123"), "{raw}");
        assert!(raw.contains("anthropic-version: 2023-06-01"), "{raw}");
        // The inline System turn rolls up into the top-level `system` field.
        assert!(raw.contains(r#""system":"dry tone""#), "{raw}");
        assert!(raw.contains(r#""max_tokens":4096"#), "{raw}");
        // With no contract, none is invented.
        assert!(!raw.contains("output_config"), "{raw}");
    }

    /// **The typed-output contract TRAVELS in the body** (ADR 0088).
    ///
    /// The test that was missing: `AiCaps::JSON_OUTPUT` was declared and
    /// `ChatRequest::json_schema` existed, but the body constructor never
    /// read it. A capability declared and not effective does not show up in
    /// any behavior test — it shows up by looking at what goes out over the
    /// socket, which is what this does.
    #[tokio::test]
    async fn the_contract_travels_in_output_config() {
        let srv = serve_once(response(
            200,
            "OK",
            &[("content-type", "text/event-stream")],
            &sse_ok(),
        ))
        .await;
        let p = AnthropicProvider::new(
            Some(srv.base_url.clone()),
            "claude-opus-5".to_string(),
            Some(Secret::new("sk-test-123".to_string())),
        );
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new(
            "plan",
            json!({
                "type": "object",
                "properties": {"renames": {"type": "array"}},
                "required": ["renames"],
                "additionalProperties": false
            }),
        ));
        let stream = p.chat(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let raw = srv.request().await;
        assert!(raw.contains(r#""output_config""#), "{raw}");
        assert!(raw.contains(r#""type":"json_schema""#), "{raw}");
        assert!(raw.contains(r#""additionalProperties":false"#), "{raw}");
        assert!(raw.contains(r#""required":["renames"]"#), "{raw}");
    }

    /// And with a model that does NOT support it, it does not travel — nor
    /// is it declared.
    ///
    /// Sending `output_config` to a model that does not understand it is a
    /// 400, and declaring the capability would be telling the core it can
    /// trust a format nobody guarantees it. Both halves have to say the same
    /// thing, which is why they are checked together.
    #[tokio::test]
    async fn an_unsupported_model_neither_declares_nor_sends_it() {
        let srv = serve_once(response(
            200,
            "OK",
            &[("content-type", "text/event-stream")],
            &sse_ok(),
        ))
        .await;
        let p = AnthropicProvider::new(
            Some(srv.base_url.clone()),
            "claude-3-haiku-20240307".to_string(),
            Some(Secret::new("sk-test-123".to_string())),
        );
        assert!(
            !p.capabilities().contains(AiCaps::JSON_OUTPUT),
            "an old model does not promise structured output"
        );
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new("plan", json!({})));
        let stream = p.chat(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let raw = srv.request().await;
        assert!(!raw.contains("output_config"), "{raw}");
    }

    /// The capability and the model list cannot be pulled apart.
    ///
    /// Parametrized with the FULL documentation list (2026-09-01), not with
    /// a sample: the first version was missing four models and carried a
    /// retired one, and a sample would not have shown it.
    #[test]
    fn the_capability_follows_the_model() {
        for (model, expect) in [
            // The ones the documentation lists as supported.
            ("claude-fable-5", true),
            ("claude-mythos-5", true),
            ("claude-mythos-preview", true),
            ("claude-opus-5", true),
            ("claude-opus-4-8", true),
            ("claude-opus-4-7", true),
            ("claude-opus-4-6", true),
            ("claude-opus-4-5-20251101", true),
            ("claude-sonnet-5", true),
            ("claude-sonnet-4-6", true),
            ("claude-sonnet-4-5-20250929", true),
            ("claude-haiku-4-5-20251001", true),
            // Sonnet 4.5's SHORT alias is not in the list: falls back to the prompt.
            ("claude-sonnet-4-5", false),
            // Retired, and never was in the structured-output list.
            ("claude-opus-4-1", false),
            ("claude-3-opus-20240229", false),
            ("a-model-that-does-not-exist", false),
        ] {
            let p = AnthropicProvider::new(None, model.to_string(), None);
            assert_eq!(
                p.capabilities().contains(AiCaps::JSON_OUTPUT),
                expect,
                "{model}"
            );
        }
    }

    #[tokio::test]
    async fn a_401_is_auth() {
        let srv = serve_once(response(401, "Unauthorized", &[], "{}")).await;
        let p = provider(&srv.base_url, Some("sk-bad"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    #[tokio::test]
    async fn a_429_carries_retry_after() {
        let srv = serve_once(response(
            429,
            "Too Many Requests",
            &[("retry-after", "5")],
            "{}",
        ))
        .await;
        let p = provider(&srv.base_url, Some("sk"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(
            matches!(
                err,
                AiError::RateLimited {
                    retry_after: Some(5)
                }
            ),
            "{err:?}"
        );
    }

    /// An `error` event mid-stream comes out as `Err(Protocol)` with the
    /// provider's message, and the stream ends there.
    #[tokio::test]
    async fn an_error_mid_stream_is_protocol() {
        let body = [
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"a"}}"#,
            "",
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"boom"}}"#,
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url, Some("sk"));
        let mut stream = p
            .chat(ChatRequest::new(vec![ChatMessage::user("x")]))
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "a");
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(
            matches!(&err, AiError::Protocol(m) if m.contains("boom")),
            "{err:?}"
        );
        assert!(stream.next().await.is_none());
    }

    /// A `data:` line with broken (truncated) JSON is `Protocol`.
    #[tokio::test]
    async fn truncated_data_is_protocol() {
        let body = "data: {\"type\":\"content_block_delta\",\"delta\":{\"ty\n";
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url, Some("sk"));
        let mut stream = p
            .chat(ChatRequest::new(vec![ChatMessage::user("x")]))
            .await
            .unwrap();
        let err = stream.next().await.unwrap().unwrap_err();
        assert!(matches!(err, AiError::Protocol(_)), "{err:?}");
    }

    /// With no secret, nothing gets sent: immediate `Auth`.
    #[tokio::test]
    async fn no_secret_is_auth() {
        let p = provider("http://127.0.0.1:9", None);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// `ChatRequest`'s contract: the first non-system turn must be `user`.
    #[tokio::test]
    async fn a_non_user_first_turn_is_protocol() {
        let p = provider("http://127.0.0.1:9", Some("sk"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::assistant("x")])).await;
        assert!(matches!(err, AiError::Protocol(_)), "{err:?}");
    }

    #[tokio::test]
    async fn embed_unsupported_and_list_models_offline() {
        let p = provider("http://127.0.0.1:9", Some("sk"));
        assert!(matches!(
            p.embed(&["x".to_string()]).await.unwrap_err(),
            AiError::Unsupported
        ));
        let models = p.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-test");
    }

    /// The provider's Debug never leaks the api key (rule 10).
    #[test]
    fn debug_redacts_the_secret() {
        let p = provider("http://x", Some("sk-super-secret"));
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-super-secret"), "{dbg}");
    }
}
