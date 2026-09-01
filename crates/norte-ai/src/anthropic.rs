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

/// Las familias de modelo cuya salida estructurada (`output_config.format`)
/// está soportada.
///
/// Se comprueba por PREFIJO porque los ids llevan variantes y fechas, y se
/// declara la capability sólo si casa: `JSON_OUTPUT` era una capacidad
/// declarada que nadie atendía —el cuerpo nunca llevaba el schema—, y la
/// forma de que no vuelva a serlo es que la declaración dependa de lo que de
/// verdad se manda. Un modelo que no está aquí NO declara la capability y cae
/// al camino del prompt, que es donde el proyecto ya sabía estar.
const MODELOS_CON_SALIDA_ESTRUCTURADA: &[&str] = &[
    "claude-fable-5",
    "claude-mythos-5",
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-5",
    "claude-opus-4-1",
    "claude-sonnet-5",
    "claude-haiku-4-5",
];

/// ¿Este modelo admite `output_config.format`?
fn admite_salida_estructurada(modelo: &str) -> bool {
    MODELOS_CON_SALIDA_ESTRUCTURADA
        .iter()
        .any(|m| modelo.starts_with(m))
}

/// Cliente de la Messages API de Anthropic (`POST /v1/messages`, SSE).
///
/// - `capabilities()` = `STREAMING`, más `JSON_OUTPUT` **si el modelo
///   configurado admite salida estructurada** (sin `EMBEDDINGS`, que
///   Anthropic no ofrece). Declararla siempre era prometer un formato que un
///   modelo viejo no da (ADR 0088).
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
        // El contrato de salida tipada, si lo hay Y este modelo lo atiende.
        // La Messages API lo lee de `output_config.format`; el nombre del
        // contrato no viaja porque aquí no se usa.
        //
        // La RESPUESTA sigue siendo un bloque de texto: `output_config.format`
        // restringe el contenido de ese bloque, no introduce un tipo de bloque
        // nuevo, así que `parse_line` la lee por el mismo `text_delta` que
        // todo lo demás. Está documentado y NO observado contra la API viva —
        // aquí no se llama a la red (ADR 0031). Si esa suposición fuera falsa,
        // el síntoma sería un `reply` vacío exactamente en los modelos que
        // activan este camino; es lo primero que hay que mirar.
        if let Some(contrato) = &req.json_schema
            && admite_salida_estructurada(&self.model)
        {
            body["output_config"] = json!({
                "format": {
                    "type": "json_schema",
                    "schema": contrato.schema,
                }
            });
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
        let mut caps = AiCaps::STREAMING;
        // Se declara sólo si el MODELO configurado la tiene. Declararla
        // siempre era decir que el core puede confiar en el formato cuando
        // con un modelo viejo no puede.
        if admite_salida_estructurada(&self.model) {
            caps |= AiCaps::JSON_OUTPUT;
        }
        caps
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
        // Sin contrato no se inventa uno.
        assert!(!raw.contains("output_config"), "{raw}");
    }

    /// **El contrato de salida tipada VIAJA en el cuerpo** (ADR 0088).
    ///
    /// Es el test que faltaba: `AiCaps::JSON_OUTPUT` se declaraba y
    /// `ChatRequest::json_schema` existía, pero el constructor del cuerpo no
    /// lo leía nunca. Una capacidad declarada y no efectiva no se ve en
    /// ningún test de comportamiento — se ve mirando lo que sale por el
    /// socket, que es lo que esto hace.
    #[tokio::test]
    async fn el_contrato_viaja_en_output_config() {
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
        let mut req = ChatRequest::new(vec![ChatMessage::user("hola")]);
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

    /// Y con un modelo que NO la soporta, no viaja — ni se declara.
    ///
    /// Mandar `output_config` a un modelo que no lo entiende es un 400, y
    /// declarar la capability sería decirle al core que puede confiar en un
    /// formato que nadie le garantiza. Las dos mitades tienen que decir lo
    /// mismo, y por eso se comprueban juntas.
    #[tokio::test]
    async fn un_modelo_sin_soporte_ni_lo_declara_ni_lo_manda() {
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
            "un modelo viejo no promete salida estructurada"
        );
        let mut req = ChatRequest::new(vec![ChatMessage::user("hola")]);
        req.json_schema = Some(crate::provider::JsonContract::new("plan", json!({})));
        let stream = p.chat(req).await.unwrap();
        let _: Vec<_> = stream.collect().await;

        let raw = srv.request().await;
        assert!(!raw.contains("output_config"), "{raw}");
    }

    /// La capability y la lista de modelos no se pueden separar.
    #[test]
    fn la_capability_sigue_al_modelo() {
        for (modelo, espera) in [
            ("claude-opus-5", true),
            ("claude-sonnet-5", true),
            ("claude-haiku-4-5", true),
            ("claude-opus-4-8", true),
            ("claude-fable-5", true),
            ("claude-3-opus-20240229", false),
            ("un-modelo-que-no-existe", false),
        ] {
            let p = AnthropicProvider::new(None, modelo.to_string(), None);
            assert_eq!(
                p.capabilities().contains(AiCaps::JSON_OUTPUT),
                espera,
                "{modelo}"
            );
        }
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
