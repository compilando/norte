//! Proveedor OpenAI-compatible (ADR 0031): `/v1/chat/completions` (SSE) y
//! `/v1/embeddings`. Cubre `OpenAI`, llama.cpp server, vLLM, Groq… La api key
//! se INYECTA como [`norte_connect::Secret`] (zeroizing, `Debug` redactado) y
//! jamás se loguea (regla 10).

use async_trait::async_trait;
use norte_connect::Secret;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// Cliente genérico de una API OpenAI-compatible.
///
/// - `capabilities()` = `STREAMING | EMBEDDINGS | JSON_OUTPUT`.
/// - `is_local()` = `false` SIEMPRE, aunque `base_url` apunte a localhost:
///   v1 no adivina; el gate `local_only` del core decide (spec §9).
/// - Sin secreto configurado, `chat`/`embed` devuelven [`AiError::Auth`].
///
/// # Ejemplos
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
    // El Debug derivado es seguro: `Secret` redacta su contenido (regla 10).
    secret: Option<Secret>,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    /// Construye el proveedor. `base_url` es OBLIGATORIA (no hay un default
    /// razonable: `https://api.openai.com`, `http://localhost:8080`…); el
    /// secreto viene INYECTADO por el core.
    #[must_use]
    pub fn new(mut base_url: String, model: String, secret: Option<Secret>) -> Self {
        // Se normaliza IN PLACE (consumiendo la String recibida).
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            base_url,
            model,
            secret,
            // Client::new() solo panica si la pila TLS no inicializa; con
            // rustls compilado estático es un invariante del build.
            client: reqwest::Client::new(),
        }
    }

    /// La api key, o [`AiError::Auth`] si no hay (fail-closed: sin
    /// credencial no se manda nada).
    fn secret(&self) -> Result<&Secret, AiError> {
        self.secret.as_ref().ok_or(AiError::Auth)
    }

    /// Body de `/v1/chat/completions`: `req.system` se antepone como mensaje
    /// con rol `system` (el dialecto `OpenAI` lo acepta inline).
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
        Ok(body)
    }
}

/// Interpreta UNA línea SSE de `/v1/chat/completions`: el delta es
/// `.choices[0].delta.content` (los chunks sin contenido — rol, tool calls,
/// usage — se saltan); `data: [DONE]` termina; `.error` →
/// [`AiError::Protocol`].
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    let Some(payload) = http::sse_data(line) else {
        return Ok(WireEvent::Skip);
    };
    if payload.trim() == "[DONE]" {
        return Ok(WireEvent::Stop);
    }
    let v: Value = serde_json::from_str(payload)
        .map_err(|e| AiError::Protocol(format!("SSE data inválido: {e}")))?;
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

/// Respuesta de `/v1/embeddings`.
#[derive(serde::Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

/// Un vector de la respuesta de `/v1/embeddings` (en orden de entrada).
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
        let secret = self.secret()?;
        let body = self.build_body(&req)?;
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(secret.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| http::transport(&e))?;
        let resp = http::check_status(resp)?;
        // El stream devuelto posee el body: dropearlo aborta la petición
        // HTTP (regla 3, cancelación drop-based).
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
            .map_err(|e| AiError::Protocol(format!("respuesta de /v1/embeddings inválida: {e}")))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }

    /// El modelo configurado, sin tocar la red (v1 no consulta `/v1/models`:
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
    async fn chat_err(p: &OpenAiCompatProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("esperaba un error de establecimiento"),
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

    /// SSE feliz: los chunks sin `content` (rol inicial, usage) se saltan,
    /// `data: [DONE]` termina, y la petición lleva el bearer y el system.
    #[tokio::test]
    async fn chat_concatena_saltando_chunks_sin_content() {
        let body = [
            r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"Ho"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"content":"la"}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "",
            "data: [DONE]",
            "",
        ]
        .join("\n");
        let srv = serve_once(response(200, "OK", &[], &body)).await;
        let p = provider(&srv.base_url, Some("sk-oa-1"));
        let mut req = ChatRequest::new(vec![ChatMessage::user("hola")]);
        req.system = Some("tono seco".to_string());
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hola");

        let raw = srv.request().await;
        assert!(raw.contains("POST /v1/chat/completions"), "{raw}");
        assert!(raw.contains("authorization: Bearer sk-oa-1"), "{raw}");
        assert!(
            raw.contains(r#"{"content":"tono seco","role":"system"}"#),
            "{raw}"
        );
    }

    /// `/v1/embeddings`: extrae los vectores en orden.
    #[tokio::test]
    async fn embed_extrae_en_orden() {
        let body = r#"{"object":"list","data":[{"index":0,"embedding":[0.5]},{"index":1,"embedding":[1.5,2.5]}]}"#;
        let srv = serve_once(response(200, "OK", &[], body)).await;
        let p = provider(&srv.base_url, Some("sk"));
        let vecs = p
            .embed(&["uno".to_string(), "dos".to_string()])
            .await
            .unwrap();
        assert_eq!(vecs, vec![vec![0.5], vec![1.5, 2.5]]);
    }

    #[tokio::test]
    async fn un_401_es_auth() {
        let srv = serve_once(response(401, "Unauthorized", &[], "{}")).await;
        let p = provider(&srv.base_url, Some("sk-mala"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// Sin secreto no se manda nada: `Auth` inmediato en chat Y embed.
    #[tokio::test]
    async fn sin_secreto_es_auth() {
        let p = provider("http://127.0.0.1:9", None);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
        let err = p.embed(&["x".to_string()]).await.unwrap_err();
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// Una línea `data:` con campo `error` es `Protocol` con el mensaje.
    #[tokio::test]
    async fn data_con_error_es_protocol() {
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

    /// El Debug del proveedor jamás filtra la api key (regla 10).
    #[test]
    fn debug_redacta_el_secreto() {
        let p = provider("http://x", Some("sk-super-secreta"));
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-super-secreta"), "{dbg}");
    }
}
