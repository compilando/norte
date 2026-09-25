//! Abstraction over AI model providers (spec §9, ADR 0031): streaming chat,
//! optional embeddings and capabilities. Credentials are INJECTED (never
//! read here); providers receive CONTENT, never filesystem paths.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod http;
mod provider;

pub mod anthropic;
#[cfg(feature = "testutil")]
pub mod fake;
pub mod ollama;
pub mod openai_compat;

pub use provider::{
    AiCaps, AiError, AiProvider, ChatMessage, ChatRequest, ChatRole, ChatStream, JsonContract,
    ModelInfo, SharedAiProvider,
};
