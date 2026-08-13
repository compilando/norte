# 0050 - The agent plans and does not apply

- Status: accepted
- Date: 2026-08-13
- Decision makers: Oscar González
- Related: design
  `docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md` (spec 3 of
  roadmap item 1, §2.1 and §2.2, whose decision this records); plan
  `docs/superpowers/plans/2026-08-13-mcp-compare-and-plan.md` (phase B); ADR
  0049 (the retained plan and `plan_hash`, which this leans on entirely); ADR
  0024 (the MCP stdio bridge, whose "surface = what the wire already offers"
  premise this amends); issue #162.

## Context and problem statement

Specs 1 and 2 of roadmap item 1 built comparison and one-way synchronisation:
`fs.compare`, `sync.plan`, `sync.apply`, `sync.report`, journalled, undoable and
gated. Spec 3 gives them their surfaces. The command line was phase A and raised
no question of principle — a human at a terminal is the actor the whole design
already assumes.

The agent surface does raise one. An MCP tool that reaches synchronisation is
not a smaller version of the CLI: it is a new actor, on a path whose blast
radius is a whole subtree, and the decision of what it may do cannot be deferred
to the tool definition.

Three facts constrain it, and none of them is a matter of taste:

**The embedded arm has no policy gate.** `EMBEDDED_CONN_ID` runs as
`Actor::User`. Its own rustdoc says these two methods must not be wired to the
Lua sandbox or the plugin host without an actor of their own. Phase A is the
first thing to make that arm reachable in-process, which sharpens rather than
softens the warning.

**A `plan_hash` is not a secret.** It is an unkeyed deterministic digest (ADR
0049) that anyone who can read both trees can compute. Nothing about holding one
demonstrates authorisation. The only thing binding a plan to its requester is
`conn_id`.

**Applying is the asymmetric operation.** Planning reads. Applying rewrites and,
under `Mirror`, deletes — one call, one hash, an entire subtree. Everything else
in this subsystem fails closed (no spool → no plan; no journal → no apply); an
apply tool would be the one place where an agent's mistake is not recoverable by
reading more carefully.

## Options considered

### Option 1 — `compare` and `sync_plan` only (chosen)

The agent produces a plan, explains it, and reports it. Applying is a human
action in the human's own client.

- **Advantage:** no new destructive surface. The most an agent can do wrong is
  describe a synchronisation badly, which a human reads before acting.
- **Advantage:** it needs no actor work, no approval routing and no policy
  decision — the read gate the agent already passes is the gate this needs.
- **Advantage:** it is genuinely useful. "What differs, and what would a mirror
  do to it" is the question an agent is good at answering and a human is slow at
  computing.
- **Drawback:** the agent cannot finish the job it just described. A human has
  to re-plan in their own client, and that is real friction, not a formality —
  see the consequence below about the hash.

### Option 2 — `sync_apply` behind a human approval

The agent may apply, but the call suspends on the `Ask` approval resolver with a
human deciding.

- **Advantage:** the agent completes the task, which is what an agent is for.
- **Drawback:** `sync.apply` has no approval path today. Building one is not
  wiring: it needs an actor of its own for the embedded bridge, a decision about
  what an approval *shows* (the plan is up to 5000 steps), and an answer to what
  `Mirror` means when the actor holds a scope narrower than the tree it would
  delete.
- **Drawback:** it puts the most dangerous method behind the newest, least
  exercised gate in the system. #166 had just been filed for a daemon over a
  policy-less engine gating nothing at all.

### Option 3 — no synchronisation tools at all

Leave the agent with the eight tools it has.

- **Advantage:** nothing to get wrong.
- **Drawback:** leaves #162 half-open and the plan-as-a-wire-type without the
  consumer specs 1 and 2 kept paying for. Comparison is *read-only*; refusing it
  buys no safety.

## Decision

**Option 1.** The agent gets `compare` and `sync_plan`. There is no
`sync_apply` tool, and a test in `crates/norte-mcp/tests/e2e_m3.rs` pins its
absence so that adding one is a deliberate act rather than an oversight.

Option 2 is not rejected on principle — it is rejected as premature. It is a
spec of its own, and its prerequisites are named above so that whoever writes it
does not have to rediscover them.

## Consequences

### The agent's plan is a report, not a token

This falls out of ADR 0049's retention model and is the consequence most likely
to be misread, so it is stated here rather than left to be discovered.

A plan is retained **per connection**. The agent's `sync.plan` runs on the
agent's connection; a human applying from the TUI or the CLI is a different
connection, and `sync.apply` will refuse the agent's hash. The agent therefore
cannot hand its hash to anyone, and the tool does not emit `plan_hash` at all —
a value that cannot be used is an invitation to try. The tool description says
so in words, because an agent that reads only the schema will otherwise attempt
exactly that and report a confusing refusal.

This is a property worth having, not a limitation to work around: **the only
apply that can happen is one a human's own client planned.** An agent cannot
arrange a subtree rewrite even by persuasion.

### The bridge grew a second connection, and it is the same actor

Phase B found that ADR 0024's premise — "surface = what the wire already offers"
— quietly assumed request/response. The eight original tools either return
immediately or start a task and poll `task.list`. `fs.compare` and `sync.plan`
deliver through *notifications*, and nothing in the bridge drained them.

Rather than build a second notification router inside `norte-mcp` — the thing
#155 shows is easy to get wrong — the bridge lazily opens a `RemoteBackend`,
which already owns that pump. `RemoteBackend::connect_as_agent` declares the
same `agent_session`, and policy scopes are keyed by **session**, not by
connection (`ScopeRegistry::grant(session, …)`), so the arm carries the same
actor and the same grants. It is opened on the first streaming tool call, so an
agent that only lists and reads never opens it.

The two connections have different `conn_id`s. That makes the retained plan
unreachable from the tools connection as well — consistent with the decision
above rather than in tension with it.

### Negative: an incomplete answer must not read as a clean one

Both tools cap their output (5000 rows or steps) and cancel the task at the cap.
A truncated result that a model read as complete would report two trees as
matching when they do not — the same failure the CLI's exit code 2 exists to
prevent, and the reason both payloads carry `truncated` and `complete`. When a
plan does not close, the fields derived from `sync.plan_done` are **absent**
rather than zeroed: a `counts` of zero reads as "nothing to do".

### Negative: repeated planning can exhaust the agent's own retention

The daemon retains at most 16 plans per connection with a 10-minute TTL, and
there is no discard tool. An agent that re-plans in a loop will start losing its
own earlier plans. The cap and the TTL are documented in the tool description so
that an agent does not retry blindly; a discard method would be a wire change
and is not this.

### Negative: ADR 0024's tool count is now wrong

It says eight. It is ten. The premise it states about the surface is amended
above.
