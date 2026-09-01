# 0089 - The protocol gets a catalogue, so forgetting a surface turns red

- Status: accepted
- Date: 2026-09-01
- Decision makers: Oscar González
- Related: ADR 0004 (unknown notifications are dropped), ADR 0011 (the
  handshake), ADR 0038 (the published JSON Schema), ADR 0066 (the SDK
  boundary).

## Context and problem statement

`norte-proto` declares about seventy method and notification names. Adding one
means touching, at minimum: the constant and its types here, the daemon's
dispatch, the remote client, the notification routes, the embedded and remote
backends, the schema aggregate, the goldens, MCP or the frontends, and the
N/N-1 compatibility window.

None of those places is redundant, and the daemon's flat dispatch is
deliberate: a hundred-arm `match` where each arm reads end to end beats ten
layers you have to walk to find out what `fs.stat` does. The problem was never
that there are many surfaces. **The problem is that forgetting one did not
show.** A method the daemon serves and the client cannot call is invisible to
every window; a type outside the schema aggregate is absent from what
implementors read; a notification nobody emits is a screen waiting forever.

## Decision

### A declarative catalogue, and the completeness tests it makes possible

`norte_proto::catalog` lists every method: wire name, `Kind`
(request/notification), `Shape` (direct/task/stream/handshake) and its params
and result types. It generates no handler, no daemon body and no policy — it
generates the **list**, and the tests use it to ask each surface whether it is
there.

Six gates now fail on an omission: every constant is catalogued and every
catalogue entry still exists; the daemon dispatches every request and does
*not* dispatch a notification; the daemon (or, for `rpc.cancel`, the client)
emits every notification; the remote client can ask for everything, minus
exceptions that must carry a written reason; the catalogued types are in the
schema aggregate; and the declared shape matches what the daemon actually does.

### The constant stays where it is

The macro names the constant; it does not redeclare it. In this protocol the
doc comment on each constant *is* the explanation of why the method is the way
it is — thousands of lines of it — and moving that inside a macro would hide
exactly what a reader needs. A test keeps the two inseparable: a method
constant missing from the catalogue turns the gate red.

### What the catalogue deliberately does not say

No access field (human/agent), and nothing about policy. An access field here
would be a second source of truth about who may call what, and one nobody
consults starts lying the moment the first one changes. The daemon decides
that, where it always did. When there is a test that verifies *real* access
against a declaration, the declaration will have earned its place.

### Every field is tied to something, because two of them lied on day one

This is the part worth recording. The first version of the catalogue was
reviewed before commit, and the review found that **two of sixty-nine entries
were wrong**:

- `index.build` was declared `Direct` returning `IndexBuildResult`. The daemon
  registers a task and answers `FsTaskResult`; `IndexBuildResult` is the task's
  *outcome*, which does not travel on the wire at all today.
- `rpc.cancel` was declared a request. It is a notification — no id, no reply,
  dropped in silence by an N-1 daemon (ADR 0004). As a request, a reader of the
  catalogue would send an envelope with an id and wait for an answer that never
  comes.

A catalogue that can be wrong without anything noticing is the same defect it
was built to fix, one level up. So each field is tied down:

- `name` — two tests, in both directions.
- `params_ty` / `result_ty` — the compiler. The macro emits a function that
  mentions every type, so a misspelled one does not build.
- `kind` — a request has a `methods::X =>` arm in the dispatch; a notification
  does not. Both halves are asserted, and that is what catches `rpc.cancel`.
- `shape` — the dispatch is sliced by arm, and an arm that registers a task
  while declaring itself `Direct` fails. That is what catches `index.build`.

The same review found that the first tests could not have caught any of it:
they matched with `contains`, so `methods::SYNC_PLAN` was satisfied by
`methods::SYNC_PLAN_DONE`. About fifteen methods have a longer sibling
constant — `FS_CHECKSUM`/`_REPORT`, `PLUGIN_PREVIEW`/`_STYLED`,
`FS_READ`/`FS_READ_MAX_CHUNK` — so for those the check could never go red.
Matching is now on a whole identifier, and requests are matched on the arm.

### The catalogue has a golden, because nothing protected the method names

Also from the review, and the highest-value thing in this change. The published
schema carries **types**, not methods; the only golden containing method names
holds four of them. Deleting `fs.rename_batch` — a textbook wire break — turned
nothing red beyond its callers failing to compile.

The catalogue is now the only complete list of names, and without a snapshot it
would not protect either: the "no phantom entries" test only checks the
catalogue does not name what is gone, which *accompanies* a deletion instead of
resisting it. `tests/golden/catalogo.tsv` resists it. Any addition, removal or
change of shape is a diff a reviewer sees.

## Consequences

- The wire is unchanged: no serde type moved, no constant changed value, no
  golden regenerated, `PROTOCOL_VERSION` stays at `0.63.0`. The only addition
  is a public module in `norte-proto`, which is semver-minor for the crate and
  nothing at all for the protocol. Confirmed by review.
- The gate found one real gap on its first run: `policy.grant_scope` is served
  by the daemon and absent from the SDK. It turned out not to be an oversight —
  granting a scope to an agent is done from the terminal (`norte policy grant`)
  with the daemon's low-level client — but *that it was deliberate was written
  down nowhere*. Now it is, as an exception with its reason.
- `Kind`, `Shape` and `MethodInfo` are `non_exhaustive` from the start, so the
  phase that adds access is not a major version of the crate.

### Out of scope, on purpose

- **MCP.** `norte-mcp` names nine of the sixty-nine methods, a deliberate
  subset, so "every request reaches this surface" is the wrong test there — it
  would report fifty-two false positives. The right one is the inverse (every
  method the bridge names is catalogued, plus an explicit allowlist, so
  widening an agent's surface is visible in the diff). That is a security
  check, not a completeness one, and it belongs to the next phase.
- **The `Backend` enum** in `norte-core` is what frontends actually use, and it
  names no method constants, so it cannot be swept by text. Its two variants
  are tied by exhaustiveness, so the embedded backend is covered for free — but
  a method that reaches `RemoteBackend` and is not exposed on `Backend` is
  still invisible to every window. Known limit.
- **A `since` field.** The per-version history already lives, in prose, in the
  doc comments; duplicating it here would create the same second source of
  truth the access field was kept out to avoid. It starts paying when there is
  a test that checks compatibility for real.
