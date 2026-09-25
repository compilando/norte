//! MCP (stdio) bridge → norte daemon (M3-4, ADR 0024): an MCP agent speaks
//! NDJSON JSON-RPC over stdio; each tool is forwarded to the daemon over a
//! UDS as a client with `agent_session` — enforcement (scope, policy, ask,
//! journal) lives ENTIRELY in the daemon (rule 9).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bridge;
