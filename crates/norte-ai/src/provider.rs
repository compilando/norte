//! The [`AiProvider`] trait and its types (ADR 0031, spec §9): the
//! abstraction over model providers. Chat in STREAMING (text deltas),
//! optional embeddings and declared capabilities. Providers receive
//! CONTENT, never filesystem paths; credentials are INJECTED.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

bitflags::bitflags! {
    /// Capabilities a provider declares: the core picks a strategy without
    /// probing the network. Serialize as a list of names (never the raw
    /// bitfield — same discipline as `norte_proto::CapabilityFlags`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AiCaps: u32 {
        /// `chat` delivers the response in incremental deltas.
        const STREAMING = 1 << 0;
        /// `embed` is supported (if not, returns [`AiError::Unsupported`]).
        const EMBEDDINGS = 1 << 1;
        /// The model supports structured output (strict JSON) — the AI
        /// rename (M4) takes advantage of it when present, always with
        /// local validation.
        const JSON_OUTPUT = 1 << 2;
    }
}

/// Role of a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// System instruction (persona, format, rules).
    System,
    /// The user's turn.
    User,
    /// A previous turn by the assistant (multi-turn context).
    Assistant,
}

/// A message in the conversation. `content` is plain text (v1 providers do
/// not receive images or files — spec §9: only text content the core has
/// already filtered against the denied paths).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Role of the sender.
    pub role: ChatRole,
    /// The message's text.
    pub content: String,
}

impl ChatMessage {
    /// Shortcut for a system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    /// Shortcut for a user turn.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    /// Shortcut for an assistant turn.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// A chat request. `system` is separate from the rest of the turns (some
/// providers treat it as a dedicated parameter, e.g. Anthropic).
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// System instruction (optional).
    pub system: Option<String>,
    /// Turns of the conversation (alternating user/assistant; the first must
    /// be `user` — the provider validates and returns `Protocol` if not).
    pub messages: Vec<ChatMessage>,
    /// Output token cap. `None` = the provider's default.
    pub max_tokens: Option<u32>,
    /// Asks for strict JSON output against this contract.
    ///
    /// HONORED by a provider that declares [`AiCaps::JSON_OUTPUT`]; one that
    /// does not ignores it and answers whatever the prompt asks for. In both
    /// cases the caller validates: a contract in the body reduces format
    /// errors, it does not replace local validation.
    pub json_schema: Option<JsonContract>,
}

/// A typed-output contract: what it is called and what shape it has.
///
/// The name is not decorative — the native mechanism of `OpenAI`-compatible
/// providers requires it (`response_format.json_schema.name`), and without
/// it every caller would have to invent one. Anthropic does not use it, so
/// it still travels along and gets ignored there.
///
/// It exists so that the project's second typed response does not have to
/// start over deciding where the schema goes.
///
/// ```
/// use norte_ai::JsonContract;
///
/// let c = JsonContract::new(
///     "rename_plan",
///     serde_json::json!({"type": "object", "additionalProperties": false}),
/// );
/// assert_eq!(c.name, "rename_plan");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonContract {
    /// Schema name, exactly as the provider that needs it asks for it.
    pub name: String,
    /// The JSON Schema. Objects with `additionalProperties: false` and a
    /// complete `required`: it is what the native mechanisms accept, and
    /// what makes "a field was missing" the provider's error rather than a
    /// surprise of ours.
    pub schema: serde_json::Value,
}

impl JsonContract {
    /// A contract with its name and its schema.
    #[must_use]
    pub fn new(name: impl Into<String>, schema: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            schema,
        }
    }
}

impl ChatRequest {
    /// Minimal request: just turns, no system and no cap.
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

/// Metadata of one of the provider's models.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Model id exactly as the provider expects it (`claude-opus-4-8`…).
    pub id: String,
    /// Context window in tokens, if the provider exposes it.
    pub context_window: Option<u64>,
}

/// Error from an AI provider. CLOSED vocabulary (`non_exhaustive` so it
/// does not break consumers when it grows): the frontend renders by
/// category, never parses the `Display`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiError {
    /// Missing, invalid or unauthorized credentials (HTTP 401/403).
    #[error("authentication failed")]
    Auth,
    /// Rate limit (HTTP 429); `retry_after` in seconds if the server gave one.
    #[error("rate limited")]
    RateLimited {
        /// Seconds to wait before retrying, if the server indicated one.
        retry_after: Option<u64>,
    },
    /// HTTP error not covered by the previous categories (with the code).
    #[error("http error: {status}")]
    Http {
        /// HTTP status code.
        status: u16,
    },
    /// Malformed or unexpected response (broken JSON, invalid SSE, missing
    /// field): the provider spoke but not in the expected format.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// This provider does not support the operation (e.g. embeddings on a
    /// chat-only provider).
    #[error("unsupported operation")]
    Unsupported,
    /// The operation was cancelled (the stream's future was dropped, or the
    /// core cancelled the Task — rule 3).
    #[error("cancelled")]
    Cancelled,
    /// Transport failure (network down, DNS, TLS): retryable.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Stream of text deltas from a chat response: concatenating every
/// `Ok(String)` is the full text. An `Err` ends the stream (the partial is
/// discarded, like the rest of the project's streams).
pub type ChatStream = BoxStream<'static, Result<String, AiError>>;

/// An AI model provider (spec §9, ADR 0031). Object-safe: the core handles
/// it behind `Arc<dyn AiProvider>`. Credentials are INJECTED in each impl's
/// constructor (the trait never reads them); providers receive CONTENT,
/// never filesystem paths.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Stable provider id (`anthropic`, `ollama`, `openai-compat`).
    fn id(&self) -> &str;

    /// Declared capabilities — the core consults this, it does not probe the
    /// network.
    fn capabilities(&self) -> AiCaps;

    /// `true` if the provider runs LOCALLY (Ollama on loopback): the core's
    /// `local_only` mode (spec §9) only lets through the ones that are.
    /// Remote ones return `false`; the core's gate is the hard barrier.
    fn is_local(&self) -> bool;

    /// Streaming chat: returns the stream of text deltas. Dropping the
    /// stream cancels the HTTP request (rule 3, drop-based).
    ///
    /// # Errors
    /// SET-UP failures (auth, network, invalid request) come out in the
    /// `Result`; mid-stream ones, as `Err` items of the stream.
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, AiError>;

    /// Embeddings for each input text (same order). Default
    /// [`AiError::Unsupported`]: only providers with [`AiCaps::EMBEDDINGS`]
    /// implement it.
    ///
    /// # Errors
    /// [`AiError::Unsupported`] (default) or the provider's own.
    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        let _ = inputs;
        Err(AiError::Unsupported)
    }

    /// The provider's available models. Default: the configured model as
    /// the sole element (providers that expose a catalogue override this).
    ///
    /// # Errors
    /// The provider's own, when querying its catalogue.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError>;
}

/// A provider behind `Arc` (registry/usage key in the core).
pub type SharedAiProvider = Arc<dyn AiProvider>;
