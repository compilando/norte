//! OpenAI-compatible provider (ADR 0031): `/v1/chat/completions` (SSE) and
//! `/v1/embeddings`. Covers `OpenAI`, llama.cpp server, vLLM, Groq… The api
//! key is INJECTED as a [`norte_connect::Secret`] (zeroizing, redacted
//! `Debug`) and is never logged (rule 10).

use async_trait::async_trait;
use norte_connect::Secret;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// Generic client for an OpenAI-compatible API.
///
/// - `capabilities()` = `STREAMING | EMBEDDINGS | JSON_OUTPUT`.
/// - `is_local()` = ALWAYS `false`, even if `base_url` points to localhost:
///   v1 does not guess; the core's `local_only` gate decides (spec §9).
/// - With no secret configured, `chat`/`embed` return [`AiError::Auth`].
///
/// # Examples
/// ```
/// use norte_ai::AiProvider as _;
/// use norte_ai::openai_compat::OpenAiCompatProvider;
///
/// let p = OpenAiCompatProvider::new(
///     "http://localhost:8080".to_string(),
///     "gpt-4o-mini".to_string(),
///     None,
/// );
/// assert_eq!(p.id(), "openai-compat");
/// assert!(!p.is_local());
/// ```
#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    base_url: String,
    model: String,
    // The derived Debug is safe: `Secret` redacts its content (rule 10).
    secret: Option<Secret>,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    /// Builds the provider. `base_url` is MANDATORY (there is no reasonable
    /// default: `https://api.openai.com`, `http://localhost:8080`…); the
    /// secret arrives INJECTED by the core.
    #[must_use]
    pub fn new(mut base_url: String, model: String, secret: Option<Secret>) -> Self {
        // Normalized IN PLACE (consuming the received String).
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            base_url,
            model,
            secret,
            // Client::new() only panics if the TLS stack fails to init; with
            // rustls compiled statically that is a build invariant.
            client: reqwest::Client::new(),
        }
    }

    /// The api key, or [`AiError::Auth`] if there is none (fail-closed: with
    /// no credential nothing gets sent).
    fn secret(&self) -> Result<&Secret, AiError> {
        self.secret.as_ref().ok_or(AiError::Auth)
    }

    /// A chat request. Separate from `chat` so it can be repeated without
    /// the contract when the server rejects it.
    async fn send(&self, req: &ChatRequest) -> Result<reqwest::Response, AiError> {
        let secret = self.secret()?;
        let body = self.build_body(req)?;
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(secret.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        http::check_status(resp)
    }

    /// `/v1/chat/completions` body: `req.system` is prepended as a message
    /// with role `system` (the `OpenAI` dialect accepts it inline).
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
            body["max_tokens"] = json!(n);
        }
        // The typed-output contract, in THIS API's mechanism: it is not
        // `output_config` like Anthropic's, it is `response_format`, and
        // here the contract's name does travel because the field requires
        // it.
        //
        // `strict: true` is what turns the schema into a contract instead of
        // a suggestion. A compatible server that does not understand it
        // still answers with text: that is why local validation is not
        // optional.
        if let Some(contract) = &req.json_schema {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": contract.name,
                    "schema": contract.schema,
                    "strict": true,
                }
            });
        }
        Ok(body)
    }
}

/// Interprets ONE SSE line from `/v1/chat/completions`: the delta is
/// `.choices[0].delta.content` (content-less chunks — role, tool calls,
/// usage — are skipped); `data: [DONE]` ends it; `.error` →
/// [`AiError::Protocol`].
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    let Some(payload) = http::sse_data(line) else {
        return Ok(WireEvent::Skip);
    };
    if payload.trim() == "[DONE]" {
        return Ok(WireEvent::Stop);
    }
    let v: Value = serde_json::from_str(payload)
        .map_err(|e| AiError::Protocol(format!("invalid SSE data: {e}")))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .pointer("/message")
            .and_then(Value::as_str)
            .map_or_else(|| err.to_string(), ToString::to_string);
        return Err(AiError::Protocol(msg));
    }
    match v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
    {
        Some(t) if !t.is_empty() => Ok(WireEvent::Delta(t.to_string())),
        _ => Ok(WireEvent::Skip),
    }
}

/// `/v1/embeddings` response.
#[derive(serde::Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

/// One vector from `/v1/embeddings`'s response (in input order).
#[derive(serde::Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}

#[async_trait]
impl AiProvider for OpenAiCompatProvider {
    fn id(&self) -> &'static str {
        "openai-compat"
    }

    fn capabilities(&self) -> AiCaps {
        AiCaps::STREAMING | AiCaps::EMBEDDINGS | AiCaps::JSON_OUTPUT
    }

    fn is_local(&self) -> bool {
        false
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "openai-compat"))]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        let resp = match self.send(&req).await {
            Ok(r) => r,
            // "OpenAI-compatible" is a name, not a guarantee: on the other
            // side there are servers that do not know `response_format` or
            // that reject `strict`, and answer 400. Without this, enabling
            // typed output turned a working feature into an opaque error.
            //
            // ONE retry, and only with a contract and only on a 400 —the
            // "your request is no good to me" status—: a 500 is not retried
            // without a contract because it says nothing about the
            // contract, and retrying blindly is spending the reader's quota
            // to fail again.
            Err(AiError::Http { status: 400 }) if req.json_schema.is_some() => {
                tracing::info!(
                    provider = "openai-compat",
                    "the server rejected typed output; retrying without the contract"
                );
                let mut without_contract = req.clone();
                without_contract.json_schema = None;
                self.send(&without_contract).await?
            }
            Err(e) => return Err(e),
        };
        // The returned stream owns the body: dropping it aborts the HTTP
        // request (rule 3, drop-based cancellation).
        Ok(http::delta_stream(resp, parse_line))
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "openai-compat"))]
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        let secret = self.secret()?;
        let body = json!({ "model": self.model, "input": inputs });
        let resp = self
            .client
            .post(format!("{}/v1/embeddings", self.base_url))
            .bearer_auth(secret.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        let resp = http::check_status(resp)?;
        let raw = resp.bytes().await.map_err(|e| http::transport(&e))?;
        let parsed: EmbeddingsResponse = serde_json::from_slice(&raw)
            .map_err(|e| AiError::Protocol(format!("invalid /v1/embeddings response: {e}")))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }

    /// The configured model, without touching the network (v1 does not
    /// query `/v1/models`: offline-testable, ADR 0031).
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
    use crate::http::testutil::{response, serve_once, serve_seq};
    use crate::provider::ChatMessage;

    /// `chat()` must fail at setup time (`ChatStream` is not `Debug`, so
    /// `unwrap_err` does not apply).
    async fn chat_err(p: &OpenAiCompatProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("expected a setup error"),
            Err(e) => e,
        }
    }

    fn provider(base_url: &str, secret: Option<&str>) -> OpenAiCompatProvider {
        OpenAiCompatProvider::new(
            base_url.to_string(),
            "gpt-test".to_string(),
            secret.map(|s| Secret::new(s.to_string())),
        )
    }

    /// Happy SSE: chunks with no `content` (initial role, usage) are
    /// skipped, `data: [DONE]` ends it, and the request carries the bearer
    /// and the system.
    #[tokio::test]
    async fn chat_concatenates_skipping_content_less_chunks() {
        let body = [
            r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"He"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"llo"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "",
            "data: [DONE]",
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url, Some("sk-oa-1"));
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.system = Some("dry tone".to_string());
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hello");

        let raw = srv.request().await;
        assert!(raw.contains("POST /v1/chat/completions"), "{raw}");
        assert!(raw.contains("authorization: Bearer sk-oa-1"), "{raw}");
        assert!(
            raw.contains(r#"{"content":"dry tone","role":"system"}"#),
            "{raw}"
        );
        // With no contract, none is invented.
        assert!(!raw.contains("response_format"), "{raw}");
    }

    /// **The contract travels in `response_format`, with its name and
    /// `strict`.**
    ///
    /// This path is UNCONDITIONAL —there is no model list that would hold
    /// against an arbitrary server— which is why its fixture matters more
    /// than Anthropic's: it is the one that points at other people's
    /// machines.
    #[tokio::test]
    async fn the_contract_travels_in_response_format() {
        let body = ["data: [DONE]", ""].join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url, Some("sk-oa-1"));
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new(
            "norte_rename_plan",
            json!({"type": "object", "additionalProperties": false}),
        ));
        let stream = p.chat(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let raw = srv.request().await;
        assert!(raw.contains(r#""response_format""#), "{raw}");
        assert!(raw.contains(r#""type":"json_schema""#), "{raw}");
        // The name travels: this mechanism requires it, unlike Anthropic's.
        assert!(raw.contains(r#""name":"norte_rename_plan""#), "{raw}");
        // And `strict`, which is what turns it into a contract rather than a
        // suggestion.
        assert!(raw.contains(r#""strict":true"#), "{raw}");
    }

    /// **A server that rejects the contract with 400 does not break the
    /// feature.**
    ///
    /// "OpenAI-compatible" is a name, not a guarantee: on the other side
    /// there could be a server that does not know `response_format`.
    /// Without this retry, enabling typed output turned a rename that
    /// worked into an opaque error — and ADR 0088 acknowledged it without
    /// fixing it.
    #[tokio::test]
    async fn a_400_on_the_contract_retries_without_it() {
        let ok = ["data: [DONE]", ""].join("\n");
        let srv = serve_seq(vec![
            response(
                400,
                "Bad Request",
                &[],
                r#"{"error":"unknown response_format"}"#,
            ),
            response(200, "OK", &[], &ok),
        ])
        .await;
        let p = provider(&srv.base_url, Some("sk-oa-1"));
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new("plan", json!({})));

        let stream = p.chat(req).await.expect("the retry pulls through");
        let _: Vec<_> = stream.collect().await;

        let reqs = srv.requests().await;
        assert_eq!(reqs.len(), 2, "one rejection, one retry");
        assert!(reqs[0].contains("response_format"), "{}", reqs[0]);
        // And the second goes WITHOUT the contract: repeating the same thing
        // would be spending the reader's quota to fail again.
        assert!(!reqs[1].contains("response_format"), "{}", reqs[1]);
    }

    /// But a 500 is NOT retried without the contract: it says nothing about
    /// the contract.
    #[tokio::test]
    async fn a_500_is_not_retried() {
        let srv = serve_seq(vec![response(500, "Server Error", &[], "boom")]).await;
        let p = provider(&srv.base_url, Some("sk-oa-1"));
        let mut req = ChatRequest::new(vec![ChatMessage::user("hello")]);
        req.json_schema = Some(crate::provider::JsonContract::new("plan", json!({})));

        assert!(matches!(
            chat_err(&p, req).await,
            AiError::Http { status: 500 }
        ));
        assert_eq!(srv.requests().await.len(), 1, "a single round trip");
    }

    /// `/v1/embeddings`: extracts the vectors in order.
    #[tokio::test]
    async fn embed_extracts_in_order() {
        let body = r#"{"object":"list","data":[{"index":0,"embedding":[0.5]},{"index":1,"embedding":[1.5,2.5]}]}"#;
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url, Some("sk"));
        let vecs = p
            .embed(&["one".to_string(), "two".to_string()])
            .await
            .unwrap();
        assert_eq!(vecs, vec![vec![0.5], vec![1.5, 2.5]]);
    }

    #[tokio::test]
    async fn a_401_is_auth() {
        let srv = serve_once(response(401, "Unauthorized", &[], "{}")).await;
        let p = provider(&srv.base_url, Some("sk-bad"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// With no secret, nothing gets sent: immediate `Auth` in chat AND embed.
    #[tokio::test]
    async fn no_secret_is_auth() {
        let p = provider("http://127.0.0.1:9", None);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
        let err = p.embed(&["x".to_string()]).await.unwrap_err();
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// A `data:` line with an `error` field is `Protocol` with the message.
    #[tokio::test]
    async fn data_with_error_is_protocol() {
        let body = [
            r#"data: {"choices":[{"delta":{"content":"a"}}]}"#,
            "",
            r#"data: {"error":{"message":"context length exceeded","type":"invalid_request_error"}}"#,
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
            matches!(&err, AiError::Protocol(m) if m.contains("context length")),
            "{err:?}"
        );
        assert!(stream.next().await.is_none());
    }

    /// The provider's Debug never leaks the api key (rule 10).
    #[test]
    fn debug_redacts_the_secret() {
        let p = provider("http://x", Some("sk-super-secret"));
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-super-secret"), "{dbg}");
    }
}
