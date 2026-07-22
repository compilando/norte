//! Abstracción de proveedores de modelos de IA (spec §9, ADR 0031): chat
//! streaming, embeddings opcionales y capabilities. Las credenciales se
//! INYECTAN (jamás se leen aquí); los proveedores reciben CONTENIDO, nunca
//! paths del filesystem.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod http;
mod provider;

pub mod anthropic;
pub mod ollama;
pub mod openai_compat;

pub use provider::{
    AiCaps, AiError, AiProvider, ChatMessage, ChatRequest, ChatRole, ChatStream, ModelInfo,
    SharedAiProvider,
};
