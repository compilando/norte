//! Puente MCP (stdio) → daemon norte (M3-4, ADR 0024): un agente MCP habla
//! JSON-RPC NDJSON por stdio; cada tool se reenvía al daemon por UDS como
//! cliente con `agent_session` — el enforcement (scope, policy, ask,
//! journal) vive ÍNTEGRO en el daemon (regla 9).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bridge;
