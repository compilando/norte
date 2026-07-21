# 0024 - SDK-free MCP stdio bridge to the daemon

- Status: accepted
- Date: 2026-07-16
- Decision makers: Oscar González
- Related: specification section 10; ADR 0023

## Context

MCP clients must manage files through norte's human-granted scopes, interactive
policy, journal, and undo rather than access the filesystem directly. The design
must decide where MCP lives, whether it needs an SDK, which tools it exposes,
and which process owns the journal.

## Decision

### Separate bridge process

`norte mcp serve` is a local bridge. It speaks MCP over stdio and forwards each
tool over the Unix socket as another daemon client, identifying an
`agent_session` during initialization.

The daemon remains the sole security authority for scopes, policy, suspended
approvals, actor identity, and journalling. The bridge never touches the
filesystem or makes a security decision. A compromised bridge still appears as
an agent client subject to server-side policy.

This policy governs cooperative local agents; it is not a sandbox against
arbitrary hostile code already running under the same user ID.

### No MCP SDK in v1

MCP stdio uses one JSON-RPC message per line, matching norte's tested NDJSON
framing. v1 needs only `initialize`, `ping`, `tools/list`, and `tools/call`, so
implement it directly with Serde JSON and Tokio instead of adding an HTTP/SSE,
macro, and schema-heavy SDK for a small subset. Respond with MCP version
`2025-06-18`. Reconsider an SDK and version negotiation when streamable HTTP is
required.

### Tool surface

Expose `list_dir`, `stat`, `read_file`, `copy`, `move`, `delete`,
`task_status`, and `request_scope`: only operations already present in the norte
wire protocol. Do not create bridge-only shortcuts for write, mkdir, or search.
Agent delete defaults to trash; permanent deletion remains subject to policy.

`request_scope` returns a pending request ID for human approval. Listing pending
requests, agent self-undo, and TUI scope-grant UI remain later protocol work.

### Journal ownership and undo

The daemon opens the SQLite journal under the configuration directory, installs
scoped policy and the approval resolver, and is its only long-lived writer.
Embedded clients currently run without that journal. Missing `policy.toml`
means no matching rules and therefore fail-closed; the example policy begins
with explicit `ask` rules.

`policy.undo_session` is available only to human connections and undoes an
agent session in strict LIFO order. The engine distinguishes the session whose
entries are targeted from the human actor executing and signing compensations,
so expired agent scope does not prevent human recovery.

## Consequences

MCP configuration points to `norte mcp serve --session <name>`. The expected
flow is request scope, human grant, policy approval for each gated operation,
then human full-session undo if necessary. The extra local-socket hop is
negligible beside filesystem and model latency. stdout is reserved exclusively
for MCP; diagnostics go to stderr and do not expose secrets or raw paths.

Any same-UID process that can access the daemon socket may identify as a human
client. Containing an agent therefore also requires its runtime sandbox to
expose only the bridge, not the socket. The journal uses an exclusive file lock
so a second daemon fails instead of creating a forked hash chain.
