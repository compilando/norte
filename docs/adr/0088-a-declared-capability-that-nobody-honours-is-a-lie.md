# 0088 - A declared capability that nobody honours is a lie

- Status: accepted
- Date: 2026-09-01
- Decision makers: Oscar González
- Related: ADR 0031 (`norte-ai` and the provider abstraction), spec §9 (the AI
  produces a reviewable plan and never applies it), #121/#122 (AI rename).

## Context and problem statement

`norte-ai` had all three pieces of structured output and none of the wiring:

- `AiCaps::JSON_OUTPUT` existed, and both the Anthropic and the
  OpenAI-compatible providers declared it — Anthropic **unconditionally**, for
  whatever model happened to be configured.
- `ChatRequest::json_schema: Option<Value>` existed, documented as "honoured by
  a provider that declares `JSON_OUTPUT`".
- Neither provider's body builder read the field. Not once.
- And `build_rename_prompt`, the only caller, set `json_schema: None` while
  asking for JSON *in prose* in the system prompt.

So the capability was declared and not effective. That is worse than absent:
absent, the core knows to be careful; declared, the core is told it may rely on
a format that nothing guarantees. Nothing failed, because the local validator
has always done the real work — which is precisely why it could sit like that.

## Decision

### The contract travels, and it is a named type

`ChatRequest::json_schema` becomes `Option<JsonContract>` — a name plus a
schema. The name is not decoration: the OpenAI-compatible mechanism requires
`response_format.json_schema.name`, and without it every caller would invent
one. Anthropic ignores it. The type exists so that the project's *second* typed
response does not start by re-deciding where the schema goes.

Each provider maps it to its own native mechanism:

- **Anthropic** — `output_config.format` with `{"type": "json_schema",
  "schema": …}`, which is the current Messages API parameter (`output_format`
  is deprecated).
- **OpenAI-compatible** — `response_format` with `{"type": "json_schema",
  "json_schema": {name, schema, strict: true}}`. `strict: true` is what makes
  it a contract rather than a suggestion.
- **Ollama** — declares neither the capability nor sends a schema, and was
  already honest about it. It is the fallback path, and a test now asserts
  that a contract in the request changes neither its body nor its
  capabilities, instead of that being assumed.

Each of the three has a fixture asserting the bytes on the socket. The
OpenAI-compatible one matters most, not least: that path is unconditional —
there is no model list to check against an arbitrary server — so it is the one
pointed at other people's machines.

### The capability follows the model, not the vendor

Anthropic's structured output is supported on a specific set of models, not on
everything the endpoint accepts. `AnthropicProvider::capabilities()` now checks
the configured model by prefix and declares `JSON_OUTPUT` only when it matches;
`build_body` gates the `output_config` on the same check.

Both halves are deliberately driven by one predicate, and a test asserts them
together. The failure this prevents is the one that just happened: a
declaration drifting away from what is actually sent. An unrecognised model
declares nothing and falls back to the prompt, which is where the project
already knew how to stand.

### The schema is an object, and it says nothing about safety

The rename contract is `{"renames": [{"from", "to"}]}` — an object at the root
because the native mechanisms expect one, with `additionalProperties: false`
and complete `required` throughout.

It carries **no** `minLength`, `maxLength`, `pattern`, `minimum` or `maximum`,
and a test forbids them: Anthropic's structured output does not support string
or numeric constraints, and including one gets the whole schema rejected.

That limitation is worth stating plainly rather than working around, because it
makes the security boundary obvious: **the rules that matter cannot be
expressed in a JSON Schema at all.** That every `from` exists in the directory,
that `to` is a plain basename with no traversal and no `\`, that there are no
duplicate destinations, that a `to` does not collide with a file that is not
itself being renamed — none of that is a shape. `{"from": "a", "to": "../x"}`
validates perfectly against the schema.

So `validate_rename_reply` is unchanged and still runs on every reply,
whichever provider answered and whether or not it honoured the contract. A test
feeds it hostile *envelopes* to hold that line. Structured output buys fewer
format errors. It buys nothing else, and it is not allowed to look like it does.

### Both shapes parse, so nothing breaks by not supporting it

The reply is accepted as either the `{"renames": […]}` envelope (from a
provider that honoured the contract) or a bare array (from one that only had
the prompt). Same validation either way. This is what makes the contract an
improvement rather than a migration: Ollama, an OpenAI-compatible server that
ignores `response_format`, and an older Anthropic model all keep working
exactly as before.

### What gets measured, and what must not be

One `tracing::info!` per rename exchange: provider id, whether the structured
contract actually travelled, entry count, reply bytes, elapsed milliseconds,
and an outcome category (`ok` / `parse` / `auth` / `rate_limit` / `transport`).

Never the instruction, never the filenames, never the reply, and never the
error's `Display`. The category is what a log is for; the content is not
(rule 10).

**Writing that down is what exposed a leak that predates this change.** The
security review of this diff found that `ai_to_proto_error` logged
`error = %e` for every unmatched variant, `Protocol` among them — and a
`Protocol` from `validate_rename_reply` embeds the fragment that motivated the
rejection: a name the model wrote, or, in the collision case, **a real
filename from the user's directory**. It went to WARN on every failed plan.
Anyone with the daemon's log or a diagnostic bundle had them.

It was there before, and it would have stayed: the diff's own comment, this
ADR and the changelog were about to assert a guarantee the code did not give,
which is the same shape of defect as the one the ADR is named after. `Protocol`
now logs its category and nothing else. `Http` still logs its text — a provider
status line has seen no filename.

The measurement also covers the exchanges that fail *before* parsing.
Reporting `estructurada` only for replies that got far enough to be parsed
would describe the contract's behaviour on the sample where things were
already working.

The `estructurada` field is the one that answers the question this ADR is
about: whether the contract is reaching the wire, in production, on the model
someone actually configured.

## Consequences

- The Anthropic path sends the schema, and a fixture asserts the exact bytes on
  the socket — not the behaviour, the body. The companion fixture asserts that
  an unsupported model sends nothing and declares nothing.
- Capabilities and behaviour now agree, and are checked together.
- Zero mutations entered `norte-ai`; it still only produces text. The plan is
  still reviewed by a human, and policy, journal and undo still govern what
  happens after (spec §9).
- The reply parser now denies unknown fields on the envelope, because the
  schema says `additionalProperties: false` and the two must agree. Without
  it, `{"renames": [], "cambios": [...the real ones...]}` came back as an
  **empty plan, silently** — "nothing to rename" instead of "that is not the
  answer I asked for".

### Remaining, named rather than implied

- **An OpenAI-compatible endpoint is an arbitrary server**, and this declares
  `JSON_OUTPUT` for all of them on the strength of the name, sending
  `strict: true` unconditionally with no fallback. A server that validates its
  body strictly answers 400 and a working feature turns into an opaque
  `Internal`. The local validator covers correctness, not that; a config flag
  or a retry without the contract is a separate change, and the trigger for it
  is a real server misbehaving.
- **That Anthropic returns structured output as an ordinary text block is
  documented, not observed.** `norte-ai` is offline-testable by design (ADR
  0031) — the fixtures assert what leaves the socket, not what comes back from
  the live API. If the assumption is wrong, the symptom is an empty reply on
  exactly the models that enable this path, and that is the first place to
  look.
- The model list is matched by prefix and will age. A model missing from it
  falls back to the prompt, which is safe; an invented id that matches a prefix
  gets a 400 from the provider. `JSON_OUTPUT` is read in exactly one place in
  the repository — the metric — so neither direction relaxes a check.
