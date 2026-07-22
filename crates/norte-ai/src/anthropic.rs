//! Proveedor Anthropic (Messages API, SSE streaming — ADR 0031). Chat en
//! deltas; SIN embeddings (Anthropic no ofrece esa API: la capability queda
//! ausente, honesta). La api key se INYECTA como [`norte_connect::Secret`]
//! (zeroizing, `Debug` redactado) y jamás se loguea (regla 10).

use async_trait::async_trait;
use norte_connect::Secret;
use serde_json::{Value, json};

use crate::http::{self, WireEvent};
use crate::provider::{AiCaps, AiError, AiProvider, ChatRequest, ChatRole, ChatStream, ModelInfo};

/// URL base por defecto de la API pública de Anthropic.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Header `anthropic-version` (obligatorio en la Messages API).
const API_VERSION: &str = "2023-06-01";
/// Tope de tokens de salida cuando la request no lo fija (la Messages API lo
/// exige siempre en el body).
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Cliente de la Messages API de Anthropic (`POST /v1/messages`, SSE).
///
/// - `capabilities()` = `STREAMING | JSON_OUTPUT` (sin `EMBEDDINGS`).
/// - Remoto: `is_local()` es `false`; el gate `local_only` del core lo veta.
/// - Sin secreto configurado, `chat` devuelve [`AiError::Auth`] (nunca manda
///   una petición sin credencial).
///
/// # Ejemplos
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
    // El Debug derivado es seguro: `Secret` redacta su contenido (regla 10).
    secret: Option<Secret>,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Construye el proveedor. `base_url` `None` = la API pública
    /// (`https://api.anthropic.com`); el secreto viene INYECTADO por el core
    /// (resolución env → keyring → age en `norte-connect`, jamás aquí).
    #[must_use]
    pub fn new(base_url: Option<String>, model: String, secret: Option<Secret>) -> Self {
        Self {
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            model,
            secret,
            // Client::new() solo panica si la pila TLS no inicializa; con
            // rustls compilado estático es un invariante del build.
            client: reqwest::Client::new(),
        }
    }

    /// Body de `/v1/messages`: los turnos `System` de `req.messages` se
    /// funden (junto con `req.system`, unidos por `\n`) en el campo
    /// `system` top-level — la Messages API no acepta rol `system` inline.
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
        Ok(body)
    }
}

/// Interpreta UNA línea SSE de la Messages API: `text_delta` → delta,
/// `message_stop` → fin, evento `error` → [`AiError::Protocol`]; el resto
/// (`message_start`, `ping`, `event:`…) se ignora.
fn parse_line(line: &str) -> Result<WireEvent, AiError> {
    let Some(payload) = http::sse_data(line) else {
        return Ok(WireEvent::Skip);
    };
    let v: Value = serde_json::from_str(payload)
        .map_err(|e| AiError::Protocol(format!("SSE data inválido: {e}")))?;
    match v.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => {
            let delta = v.get("delta");
            if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta") {
                let text = delta
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| AiError::Protocol("text_delta sin campo `text`".into()))?;
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
                .unwrap_or("error del proveedor sin mensaje");
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
        AiCaps::STREAMING | AiCaps::JSON_OUTPUT
    }

    fn is_local(&self) -> bool {
        false
    }

    #[tracing::instrument(level = "debug", skip_all, fields(provider = "anthropic"))]
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError> {
        // Sin credencial no se manda NADA (fail-closed).
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
        // El stream devuelto posee el body: dropearlo aborta la petición
        // HTTP (regla 3, cancelación drop-based).
        Ok(http::delta_stream(resp, parse_line))
    }

    /// El modelo configurado, sin tocar la red. Existe un endpoint vivo
    /// (`GET /v1/models`) pero v1 se mantiene offline-testable (ADR 0031:
    /// los detalles del cliente se fijan con fixtures, no con llamadas).
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
    async fn chat_err(p: &AnthropicProvider, req: ChatRequest) -> AiError {
        match p.chat(req).await {
            Ok(_) => panic!("esperaba un error de establecimiento"),
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
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hola "}}"#,
            "",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"mundo"}}"#,
            "",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            "",
            r#"data: {"type":"message_stop"}"#,
            "",
        ]
        .join("\n")
    }

    /// SSE feliz: la concatenación de los deltas es el texto completo, y la
    /// petición lleva la api key, la versión y el system fundido.
    #[tokio::test]
    async fn chat_concatena_deltas_y_manda_headers() {
        let srv = serve_once(response(
            200,
            "OK",
            &[("content-type", "text/event-stream")],
            &sse_ok(),
        ))
        .await;
        let p = provider(&srv.base_url, Some("sk-test-123"));
        let req = ChatRequest::new(vec![
            ChatMessage::system("tono seco"),
            ChatMessage::user("hola"),
        ]);
        let stream = p.chat(req).await.unwrap();
        let parts: Vec<String> = stream.map(Result::unwrap).collect().await;
        assert_eq!(parts.concat(), "Hola mundo");

        let raw = srv.request().await;
        assert!(raw.contains("POST /v1/messages"), "{raw}");
        assert!(raw.contains("x-api-key: sk-test-123"), "{raw}");
        assert!(raw.contains("anthropic-version: 2023-06-01"), "{raw}");
        // El turno System inline sube al campo `system` top-level.
        assert!(raw.contains(r#""system":"tono seco""#), "{raw}");
        assert!(raw.contains(r#""max_tokens":4096"#), "{raw}");
    }

    #[tokio::test]
    async fn un_401_es_auth() {
        let srv = serve_once(response(401, "Unauthorized", &[], "{}")).await;
        let p = provider(&srv.base_url, Some("sk-mala"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    #[tokio::test]
    async fn un_429_lleva_retry_after() {
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

    /// Un evento `error` a mitad de stream sale como `Err(Protocol)` con el
    /// mensaje del proveedor, y el stream termina ahí.
    #[tokio::test]
    async fn error_a_mitad_de_stream_es_protocol() {
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

    /// Una línea `data:` con JSON roto (truncado) es `Protocol`.
    #[tokio::test]
    async fn data_truncado_es_protocol() {
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

    /// Sin secreto no se manda nada: `Auth` inmediato.
    #[tokio::test]
    async fn sin_secreto_es_auth() {
        let p = provider("http://127.0.0.1:9", None);
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::user("x")])).await;
        assert!(matches!(err, AiError::Auth), "{err:?}");
    }

    /// El contrato de `ChatRequest`: primer turno no-system debe ser `user`.
    #[tokio::test]
    async fn primer_turno_no_user_es_protocol() {
        let p = provider("http://127.0.0.1:9", Some("sk"));
        let err = chat_err(&p, ChatRequest::new(vec![ChatMessage::assistant("x")])).await;
        assert!(matches!(err, AiError::Protocol(_)), "{err:?}");
    }

    #[tokio::test]
    async fn embed_no_soportado_y_list_models_offline() {
        let p = provider("http://127.0.0.1:9", Some("sk"));
        assert!(matches!(
            p.embed(&["x".to_string()]).await.unwrap_err(),
            AiError::Unsupported
        ));
        let models = p.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-test");
    }

    /// El Debug del proveedor jamás filtra la api key (regla 10).
    #[test]
    fn debug_redacta_el_secreto() {
        let p = provider("http://x", Some("sk-super-secreta"));
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-super-secreta"), "{dbg}");
    }
}
