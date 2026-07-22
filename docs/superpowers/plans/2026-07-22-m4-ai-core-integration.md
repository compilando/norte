# M4 AI — core integration (A2) Implementation Plan

> Follows ADR 0031. A1 (norte-ai providers) lands first; this plan wires them
> into norte-core with the opt-in gate and the reviewable rename flow.

**Goal:** `[ai]` config, a hard opt-in/local-only/denied-paths gate in the
core, and `Engine::ai_rename_plan` that turns basenames + an instruction into
a validated, reviewable rename plan the existing mutation engine executes.

**Architecture:** `norte-core/src/ai.rs` — `AiConfig` (loads `[ai]` from
norte.toml, user layer only), `AiGate` (the opt-in/local-only/denied-prefix
checks, applied BEFORE any content leaves the process), and the rename
plan builder/validator (pure — no network in its tests). The provider is
injected like the connector: `Engine::set_ai_provider(Arc<dyn AiProvider>)`.
Applying a plan is N ordinary `fs.move` Tasks — nothing new to govern.

---

### Task 1: `[ai]` config loader

- `norte-core/src/ai.rs`: `AiConfig { enabled: bool, local_only: bool, denied_prefixes: Vec<VPath>, rename_provider: Option<String>, providers: Vec<AiProviderConfig> }` where `AiProviderConfig { name, kind ("anthropic"|"ollama"|"openai-compat"), model, base_url: Option<String> }`.
- `load()` from `config_dir()/norte.toml`, `[ai]` section, USER layer only (project layer ignored fail-closed — same rule as `[archive]`/policy). Absent = `AiConfig::default()` with `enabled: false`. Fail-loud on malformed TOML.
- `denied_prefixes` are parsed as `VPath` (reject invalid = error, don't silently drop a denied path).
- Tests: default is disabled; parse a full section; malformed → err; denied prefix that doesn't parse → err.

### Task 2: `AiGate` — the opt-in barrier

- `AiGate::check(&self, op: AiOp, paths: &[&VPath]) -> Result<(), AiError>` where `AiOp` is `Rename` for v1.
- Refuse when `!enabled` (→ a typed "ai disabled" error). Refuse a remote provider (`!provider.is_local()`) when `local_only`. Refuse if ANY input path is under a `denied_prefix` (byte-exact `is_under`, reuse the policy scope prefix check) — BEFORE any content/name leaves the process.
- Tests: disabled refuses; local_only refuses a remote provider but allows a local one; a path under a denied prefix refuses; sibling-prefix false-positive does not refuse (segment-aware, like the policy scope).

### Task 3: rename plan builder + validator (pure)

- `build_rename_prompt(names: &[Vec<u8>], instruction: &str) -> ChatRequest`: sends ONLY basenames (raw bytes → lossy-marked display strings; a hostile name with U+FFFD is refused fail-loud, not sent) + the instruction. System prompt asks for STRICT JSON `[{"from": "...", "to": "..."}]`, no prose.
- `RenamePlan { entries: Vec<RenameEntry> }`, `RenameEntry { from: VPath-relative Segment, to: Segment }`.
- `validate_rename_reply(reply: &str, inputs: &[Segment], existing: &[Segment]) -> Result<RenamePlan, AiError>`: parse JSON; every `from` ∈ inputs; every `to` a valid `Segment` (no `/`, `..`, NUL, `!`); no duplicate targets; no `to` collides with an existing entry UNLESS that entry is itself renamed away in the same plan; a hostile/non-UTF8 model output is a typed error, never a partial apply. The PLAN is the product.
- Tests (all offline): valid reply → plan; a `from` not in inputs → err; a `to` with `/` or `..` → err; duplicate targets → err; collision-with-existing → err; a self-consistent swap (a→b, b→a) → ok; garbage JSON → Protocol err; a `to` that's non-UTF8-via-escape → err.

### Task 4: `Engine::ai_rename_plan` + injection

- `Engine::set_ai_provider(Arc<dyn AiProvider>)` + `set_ai_config(AiConfig)`; a `Provider`-less engine returns "ai unsupported".
- `async fn ai_rename_plan(&self, dir: &VPath, instruction: &str) -> Result<RenamePlan, AiError>`: list `dir` (through the engine's providers — never direct FS), collect basenames, run `AiGate::check(Rename, [dir])`, build the prompt, call `provider.chat`, drain the stream to a full string, `validate_rename_reply`. Returns the plan; DOES NOT mutate.
- Applying the plan is the caller's job via existing `Engine::move_` per entry (journal + undo + policy) — add a thin `Engine::apply_rename_plan(dir, &RenamePlan, actor)` that submits the moves as Tasks and returns their handles, OR leave application to the frontend calling `move_` in a loop (decide during impl; the moves must be ordinary governed Tasks).
- Tests: with a fake in-process AiProvider (implement a test double returning a canned JSON stream) + MemProvider dir, `ai_rename_plan` returns the expected plan; gate-disabled → err before the provider is touched; a denied dir → err.

### Task 5: wire — `ai.rename_plan` (proto bump, guardian)

- proto: `ai.rename_plan` method + params `{ dir: VPath, instruction: String }` + result `{ entries: [{from, to}] }` (raw-byte-safe: `from`/`to` as Segments/percent-encoded). Version bump; goldens; N-1 window. protocol-guardian mandatory.
- daemon handler: gated (the daemon fixes the actor; an agent session calling ai.rename_plan is subject to the same policy as any op). Application still goes through fs.move — no new mutation wire.
- Backend + a CLI `norte ai rename <dir> "<instruction>"` that prints the plan and asks before applying (reviewable — the human confirms).

### Task 6: gate + reviewers

- `just ci`; reviewers: protocol-guardian (Task 5), security (the gate is the denied-paths enforcement point — content must not leak before the check; local_only must be a hard gate; no api key in logs), rust, encoding-auditor (basenames → prompt: hostile names must be refused, never sent silently; the plan's `to` must round-trip bytes).

### Deferred to a follow-up (A3)

Semantic index (`norte-index` crate: SQLite embeddings, `index.build`/
`index.search_semantic`) is its own milestone — not in this plan.
