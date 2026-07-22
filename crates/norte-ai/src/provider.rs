//! El trait [`AiProvider`] y sus tipos (ADR 0031, spec §9): la abstracción
//! sobre proveedores de modelos. Chat en STREAMING (deltas de texto),
//! embeddings opcionales y capabilities declaradas. Los proveedores reciben
//! CONTENIDO, jamás paths del filesystem; las credenciales se INYECTAN.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

bitflags::bitflags! {
    /// Capacidades declaradas por un proveedor: el core elige estrategia sin
    /// tantear la red. Serializan como lista de nombres (jamás el bitfield
    /// crudo — misma disciplina que `norte_proto::CapabilityFlags`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AiCaps: u32 {
        /// `chat` entrega la respuesta en deltas incrementales.
        const STREAMING = 1 << 0;
        /// `embed` está soportado (si no, devuelve [`AiError::Unsupported`]).
        const EMBEDDINGS = 1 << 1;
        /// El modelo admite salida estructurada (JSON estricto) — el rename
        /// IA (M4) lo aprovecha cuando está, con validación local siempre.
        const JSON_OUTPUT = 1 << 2;
    }
}

/// Rol de un mensaje de chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// Instrucción de sistema (persona, formato, reglas).
    System,
    /// Turno del usuario.
    User,
    /// Turno previo del asistente (contexto multi-turno).
    Assistant,
}

/// Un mensaje de la conversación. `content` es texto plano (los proveedores
/// v1 no reciben imágenes ni archivos — spec §9: solo contenido de texto que
/// el core ya filtró contra los denied paths).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Rol del emisor.
    pub role: ChatRole,
    /// Texto del mensaje.
    pub content: String,
}

impl ChatMessage {
    /// Atajo para un mensaje de sistema.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    /// Atajo para un turno de usuario.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    /// Atajo para un turno del asistente.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// Petición de chat. El `system` va aparte del resto de turnos (algunos
/// proveedores lo tratan como parámetro dedicado, p. ej. Anthropic).
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Instrucción de sistema (opcional).
    pub system: Option<String>,
    /// Turnos de la conversación (user/assistant alternados; el primero debe
    /// ser `user` — el proveedor valida y devuelve `Protocol` si no).
    pub messages: Vec<ChatMessage>,
    /// Tope de tokens de salida. `None` = el default del proveedor.
    pub max_tokens: Option<u32>,
    /// Pide salida JSON estricta contra este schema (solo si el proveedor
    /// declara [`AiCaps::JSON_OUTPUT`]; si no, se ignora y el caller valida).
    pub json_schema: Option<serde_json::Value>,
}

impl ChatRequest {
    /// Petición mínima: solo turnos, sin system ni tope.
    #[must_use]
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            system: None,
            messages,
            max_tokens: None,
            json_schema: None,
        }
    }
}

/// Metadatos de un modelo del proveedor.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Id del modelo tal cual lo espera el proveedor (`claude-opus-4-8`…).
    pub id: String,
    /// Ventana de contexto en tokens, si el proveedor la expone.
    pub context_window: Option<u64>,
}

/// Error de un proveedor de IA. Vocabulario CERRADO (`non_exhaustive` para
/// no romper a los consumidores al crecer): el frontend renderiza por
/// categoría, jamás parsea el `Display`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiError {
    /// Credenciales ausentes, inválidas o sin permiso (HTTP 401/403).
    #[error("authentication failed")]
    Auth,
    /// Rate limit (HTTP 429); `retry_after` en segundos si el server lo dio.
    #[error("rate limited")]
    RateLimited {
        /// Segundos a esperar antes de reintentar, si el server lo indicó.
        retry_after: Option<u64>,
    },
    /// Error HTTP no cubierto por las categorías anteriores (con el código).
    #[error("http error: {status}")]
    Http {
        /// Código de estado HTTP.
        status: u16,
    },
    /// Respuesta malformada o inesperada (JSON roto, SSE inválido, campo
    /// ausente): el proveedor habló pero no en el formato esperado.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// La operación no la soporta este proveedor (p. ej. embeddings en un
    /// proveedor solo-chat).
    #[error("unsupported operation")]
    Unsupported,
    /// La operación se canceló (el future del stream se dropeó, o el core
    /// canceló la Task — regla 3).
    #[error("cancelled")]
    Cancelled,
    /// Fallo de transporte (red caída, DNS, TLS): reintentable.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Stream de deltas de texto de una respuesta de chat: la concatenación de
/// todos los `Ok(String)` es el texto completo. Un `Err` termina el stream
/// (parcial descartado, como el resto de streams del proyecto).
pub type ChatStream = BoxStream<'static, Result<String, AiError>>;

/// Un proveedor de modelos de IA (spec §9, ADR 0031). Object-safe: el core
/// lo maneja tras `Arc<dyn AiProvider>`. Las credenciales se INYECTAN en el
/// constructor de cada impl (jamás las lee el trait); los proveedores
/// reciben CONTENIDO, nunca paths del filesystem.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Id estable del proveedor (`anthropic`, `ollama`, `openai-compat`).
    fn id(&self) -> &str;

    /// Capacidades declaradas — el core consulta esto, no tantea la red.
    fn capabilities(&self) -> AiCaps;

    /// `true` si el proveedor corre LOCALMENTE (Ollama en loopback): el modo
    /// `local_only` del core (spec §9) solo deja pasar los que lo son. Los
    /// remotos devuelven `false`; el gate del core es la barrera dura.
    fn is_local(&self) -> bool;

    /// Chat en streaming: devuelve el stream de deltas de texto. Dropear el
    /// stream cancela la petición HTTP (regla 3, drop-based).
    ///
    /// # Errors
    /// Los fallos de ESTABLECIMIENTO (auth, red, request inválida) salen en
    /// el `Result`; los de mitad de stream, como items `Err` del stream.
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError>;

    /// Embeddings de cada texto de entrada (mismo orden). Default
    /// [`AiError::Unsupported`]: solo los proveedores con
    /// [`AiCaps::EMBEDDINGS`] lo implementan.
    ///
    /// # Errors
    /// [`AiError::Unsupported`] (default) o los del proveedor.
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        let _ = inputs;
        Err(AiError::Unsupported)
    }

    /// Modelos disponibles del proveedor. Default: el modelo configurado como
    /// único elemento (los proveedores que exponen un catálogo lo overridean).
    ///
    /// # Errors
    /// Los del proveedor al consultar su catálogo.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError>;
}

/// Un proveedor tras `Arc` (clave de registro/uso en el core).
pub type SharedAiProvider = Arc<dyn AiProvider>;
