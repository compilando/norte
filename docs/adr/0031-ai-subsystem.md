# AI subsystem: norte-ai providers, reviewable AI rename, semantic index

- Status: accepted
- Date: 2026-07-22
- Spec: §9 (AI providers), M4 milestone ("model providers, AI rename, semantic search").

## Context and problem statement

M4's plugin half shipped (ADR 0022); the AI half does not exist: no `norte-ai`
or `norte-index` crates. Spec §9 fixes the constraints: AI is opt-in;
credentials live in keyring/env, never in config; the core enforces denied
paths before content reaches a provider; local-only mode disables remote
providers; AI rename/organization always produce a reviewable plan executed
by the existing mutation engine (journal + undo); AI providers receive
content, not filesystem paths.

## Decision

### Crate `norte-ai` (A1)

`AiProvider` trait (async, object-safe): `id()`, `capabilities()` (bitflags:
`STREAMING`, `EMBEDDINGS`, `JSON_OUTPUT`), `chat(ChatRequest) -> ChatStream`
(stream of text deltas; the full text is the concatenation), `embed(&[String])
-> Vec<Vec<f32>>` (default `Unsupported`), `list_models()`. Typed
`AiError` (thiserror): `Auth`, `RateLimited { retry_after }`, `Http`,
`Protocol`, `Unsupported`, `Cancelled`.

v1 implementations, all over reqwest+rustls (the tree's existing HTTP stack —
transitive via opendal, now a direct dep of norte-ai; regla 8 justification:
only maintained pure-Rust client already in the tree):

- **`anthropic`** — Messages API, SSE streaming. No embeddings (capability
  absent, honest).
- **`ollama`** — local `/api/chat` (NDJSON streaming) + `/api/embed`.
  Qualifies as local for local-only mode (loopback/configured host).
- **`openai-compat`** — generic `/v1/chat/completions` (SSE) +
  `/v1/embeddings`; covers OpenAI itself, llama.cpp server, vLLM, Groq…
  Gemini/Vertex deferred (spec lists them as planned, not v1).

Secrets are INJECTED: constructors take `Option<norte_connect::Secret>`
(zeroizing). Resolution (env → keyring → secrets.age) happens in the core via
the existing `SecretResolver` under names `ai:<provider-id>`; norte-ai never
reads env/keyring itself and never logs the key (Debug redacted, same
discipline as norte-connect). Base URLs/models come from config.

Cancellation: dropping the `ChatStream` aborts the HTTP request (reqwest
drop semantics) — drop-based, composes with `rpc.cancel` like everything
else.

Tests: in-process `axum`? No — plain `tokio::net::TcpListener` + hand-rolled
HTTP/1.1 fake server (no new dev-dep) serving canned SSE/NDJSON, including
hostile cases (mid-stream cut, oversized line, garbage SSE). No network in
the gate, ever.

### Config and gating (A2a)

`[ai]` in norte.toml (user layer; PROJECT layer ignored fail-closed, same
rule as `[archive]`): `enabled = false` (default: everything off),
`local_only = false`, `[ai.providers.<name>] kind/model/base_url`, task
routing `[ai.tasks] rename = "<name>"`, `denied_prefixes = ["file:///…"]`.
The core gate (`norte-core/src/ai.rs`): refuses any AI op when disabled;
refuses remote (non-ollama-loopback) providers when `local_only`; refuses an
op whose input paths fall under `denied_prefixes` BEFORE any content/name
leaves the process. What is sent = exactly the op's declared payload
(basenames for rename), surfaced to the UI verbatim.

### AI rename (A2b)

`Engine::ai_rename_plan(dir, names, instruction)` sends ONLY basenames (raw
bytes → lossy-marked strings, hostile names refused fail-loud) + the user
instruction, requests strict JSON (`[{from, to}]`), and validates the reply:
every `from` ∈ input, every `to` a valid `Segment` (no `/`, `..`, NUL, no
`!`), no duplicate targets, no collision with existing entries unless the
plan renames them away — invalid plan = typed error, never a partial apply.
The PLAN is the product; applying it is N ordinary `fs.move` operations
(existing Tasks: journal, undo, policy — nothing new to govern). Wire: new
method `ai.rename_plan` (proto bump, guardian) so Remote frontends get it;
plan application needs no new wire (it is `fs.move`).

### Semantic index (A3, crate `norte-index`)

SQLite via sqlx (already in tree): `files(path, generation, chunk, text_hash)`
+ `embeddings(file_id, vec BLOB f32-le)`. Indexing runs as a cancelable Task
walking a subtree THROUGH the engine's providers (never direct FS), reading
bounded text prefixes, embedding via the configured provider (local by
default per task routing). Search: embed the query, brute-force cosine top-k
(SQLite scan; ANN only if a real corpus proves it necessary). Wire:
`index.build` (Task) + `index.search_semantic` — own bump when it lands.

## Consequences

- reqwest becomes a direct dependency of norte-ai only; deny-audited.
- Anthropic client details (endpoints, model ids, SSE event kinds) are pinned
  by golden fixtures in norte-ai tests, not by live calls.
- Local-only mode is a hard gate in the core, not a provider courtesy.
- The rename flow never mutates on its own: worst case of a hostile/confused
  model output is a rejected plan or a user-visible bad suggestion, never an
  unjournaled mutation (regla 4 intact by construction).
- Gemini/Vertex, vision, tool-use and prompt-side file CONTENT (beyond
  names) stay out of v1; each needs its own denied-paths/consent review.

### Hardening from the security review (applied)

- `OllamaProvider::is_local()` derives the flag from the configured
  `base_url` host and returns `true` only for loopback (`127.0.0.0/8`,
  `::1`, `localhost`). A remote host configured as an ollama provider is
  NOT local, so `local_only` refuses it — the flag was unconditionally
  `true` and let a remote endpoint bypass `local_only`.
- The core caps the accumulated model reply at 512 KiB (a legitimate
  `[{from,to}]` plan fits with room to spare) — the per-line 1 MiB cap in
  the HTTP layer did not bound the total, so an endless stream of small
  deltas could OOM the process.
- `ai_rename_plan` omits from the prompt any listed entry whose path is
  under a `denied_prefix` — the gate only checks the root `dir`, so a
  denied directory that is a direct child of `dir` would otherwise have
  its name leaked in the basenames.
- `validate_rename_reply` rejects a `to` containing `\` (Windows path
  separator → traversal), in addition to the POSIX cases `Segment`
  already rejects.

