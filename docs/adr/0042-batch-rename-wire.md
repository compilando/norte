# 0042 - Two-phase batch rename: a reviewed plan, executed by hash

- Status: accepted
- Date: 2026-08-08
- Decision makers: Oscar González
- Related: spec §17 (batch rename), §11 (protocol versioning); hard rules 1
  (filenames are bytes), 3 (long operations are tasks), 4 (mutations go through
  the journal) and 7 (no business logic in frontends); ADR 0001 (VPath wire
  format and the percent-encoding codec), ADR 0004 (wire evolution: an unknown
  value degrades, it never breaks), ADR 0031 (AI subsystem, which produces the
  pairs), ADR 0038 (protocol JSON Schema gate). Design:
  `docs/superpowers/specs/2026-08-08-batch-rename-executor-design.md`.

## Context

§17 says AI rename feeds the same executor as batch rename. The sentence was
inverted in the tree: AI rename existed, the executor did not. Applying an
approved plan meant submitting one `fs.move` per pair, in plan order, from the
frontend.

That has three consequences a user can see. There is no transaction, so the
fifth move failing leaves four applied and four separate undos. There is no
whole-plan collision preview, because each move meets the engine's collision
handling alone and the plan is never checked against itself. And a permutation
can never work: `a→b, b→a` collides on the first move even though the plan is
perfectly consistent. "Number these episodes correctly" is the normal case for
AI rename and it is exactly the case that fails.

The executor is therefore not a feature next to the AI one; it is what makes the
AI one correct. This ADR records the decisions its **wire** commits us to. The
planner, the executor and the frontends land behind it, constrained by what is
written here.

## Decision

### 1. Two methods, bound by a hash

`fs.rename_batch_plan` previews and `fs.rename_batch` executes.

The preview is a **direct response**: no task, no journal, no mutation. It
returns ordered steps, the temporaries it had to insert, classified verdicts,
`executable`, and a `plan_hash` over the plan's *conclusions* — the directory,
the sorted pairs, the resulting steps, the verdicts, and the case and
normalisation flags that were used.

The execution carries back the `plan_hash` the human approved. The core re-plans
the same pairs against the **current** directory and compares. Adding an
unrelated file changes no verdict, so the hash still matches and the batch runs.
Adding a file that creates a collision changes a verdict, so it does not, and
the call fails `PlanStale` having touched nothing.

One exception, and it is deliberate: a file appearing under exactly the name the
planner picked for a temporary *does* change a step, and therefore the hash. The
temporary is derived from the intent, so it is predictable, which means an agent
with write access to that directory can invalidate a human's approval loop
indefinitely by re-creating that name. We accept it. The alternative — excluding
the temporaries from the hash — would let the plan the human approved and the
plan the core executes differ in exactly the steps the human never saw, which
trades a nuisance for the property this whole design exists to hold. Griefing by
an agent that already has write access to the directory is bounded by the scope
grant that gave it that access.

The alternative — returning a plan token the client hands back — was rejected.
A token makes the server hold state per preview, with a lifetime, an eviction
policy and a denial-of-service surface, to answer a question the core can answer
by recomputing. Hashing the conclusions rather than the inputs is what makes
recomputation cheap enough to be the whole mechanism.

### 2. The client sends intent; the order comes back

Params carry `pairs` only. Ordering, temporaries and verdicts are outputs, and
`fs.rename_batch` recomputes them rather than trusting anything the client
sends.

This is the anti-smuggling property, and it is the reason the planner lives in
the core (rule 7) rather than in the frontend that renders it. A client may be
an agent reaching the daemon over MCP. If the wire accepted an ordered step
list, an agent could present one plan to the human and submit another; the human
would approve `ep1 → ep01` and the core would execute whatever arrived. Because
the only thing that crosses is intent, the worst an agent can do is send pairs —
and pairs are what the human reviewed.

The same reasoning is why a `RenameStep` does not say which pair it came from.
Steps are the core's machinery: a frontend renders the user's own pairs, which
it already holds, plus the verdicts, which carry `pair_index`. Nothing in this
design renders a step-to-pair mapping, and a temporary deliberately splits one
pair across two steps — the order is the core's business. Should a frontend ever
demonstrate that it needs the mapping, an optional field is an additive change,
which is the right shape for a need that has not been shown.

### 3. `dir` plus base names, never full paths

A pair is two base names, and the directory is a separate field. A pair
therefore **cannot** address anything outside `dir`; there is no traversal to
validate because there is no syntax for it. That is a structural property, not a
check that could be forgotten in one code path. It also bounds the blast radius
of a scope grant: one directory, one provider, which is what makes the executor's
rollback a plain inverse `rename` rather than a copy.

### 4. Names cross as `Segment`, not `String`

Rule 1: filenames are bytes. `Segment` (ADR 0001's codec, percent-encoded on the
wire) carries them losslessly, and its invariants — non-empty, no NUL, no `/`,
not `.` or `..` — are validated **after** decoding, so `%2E%2E` cannot smuggle a
`..` past a check performed on the encoded form.

`AiRenameEntry` uses `String` because the AI engine rejects hostile names
fail-loud before it ever calls a provider. The batch executor has no such
excuse: renaming files with non-UTF-8 names is a thing users do, and a batch is
where such a name must survive byte for byte. Every golden fixture for these
types carries one.

### 5. A closed verdict vocabulary that still carries `serde(other)`

`RenameCollisionKind` is closed — the core emits `internal`, `external` or
`absent_source` and never invents a value. It nevertheless carries
`#[serde(other)] Unknown` and `#[non_exhaustive]`, like `ConflictKind`.

The reason is specific and was verified in the tree, not assumed.
`version_compatible` accepts N and N-1, so a **0.36 client legitimately talks to
a 0.37 daemon**. And `ConnState` (`crates/norte-core/src/daemon/server.rs`) does
not retain the negotiated `protocol_version`: it is checked during `initialize`
and discarded. The daemon has no per-peer version to gate emission on, even if a
future feature wanted one.

**The general rule, beyond this feature: every wire addition must be
fallback-degradable.** A field or variant that a peer one version behind cannot
tolerate has no gate to hide behind — the only mechanism available is the
receiver's tolerance. Adding a value to a closed enum without a fallback is
therefore not a small future change; it is a breaking one, discovered at
runtime, in a parse that takes the whole surrounding document down with it.

A verdict is per-row, so degrading one line of a plan costs the user one
unexplained row. Failing the parse costs the whole plan, which is the document
they were about to approve.

For the fallback to be worth anything, an unknown verdict must still be
*addressable*: `RenameCollision` therefore carries `pair_index`, the index into
the request's `pairs`. `name` changes meaning with `kind` (the destination for
`internal` and `external`, the missing source for `absent_source`), so under
`Unknown` a client could not tell what it was looking at. The index is
well-defined for every kind, present and future, so the offending row can always
be highlighted even when the reason cannot be explained.

### 6. Declared ceilings, because the preview is a direct response

`FS_RENAME_BATCH_MAX_PAIRS` (4096) is part of the contract, following the house
pattern of the five constants already in `methods.rs`: `FS_READ_MAX_CHUNK`,
`FS_LIST_MAX_PAGE`, `PLUGIN_HELP_MAX_BYTES`, `INDEX_SEMANTIC_MAX_K` and
`SEARCH_HITS_MAX_BATCH`.
Unlike `FS_LIST_MAX_PAGE` it does **not** clamp: truncating a rename batch would
execute a different plan from the one requested, so an oversized request is
rejected whole.

The ceiling matters more here than for the neighbouring methods. `fs.*` is
reachable by an agent, the planner is superlinear over the directory listing,
and the preview is a *direct response* — the work happens on the request path,
not inside a cancellable task. `steps` and `collisions` need no constants of
their own: one collision per pair at most, and one temporary per cycle where a
cycle consumes at least two pairs.

`plan_hash` is a lowercase hex string of exactly `PLAN_HASH_LEN` (64)
characters — readable in a log, no base64 ambiguity, no bytes on the wire. It is
a validating newtype (`PlanHash`) rather than a `String` for the reason
`Segment` is one: a rule written only in prose gets re-implemented in the daemon
dispatch, in the MCP bridge and in every frontend that echoes a hash back, and
the detail one of those copies drops is the lowercase. In the type it is
enforced once, at deserialization, for every layer. A string of the wrong shape
is a params error, deliberately **not** `PlanStale`:
"your hash is malformed" and "the directory changed" are different facts, and
answering the second to a client that sent garbage lies to it about the state of
the world.

### 7. N renames, one journal unit

`fs.rename_batch` is one task (`TaskKind::RenameBatch`) and one undoable journal
group, with rollback on failure or cancellation. Progress counts **steps**, not
bytes; a rename moves no bytes, and a frontend that drew a byte bar here would
draw zero forever. The golden progress fixture freezes that.

The executor arrives in a later task, but the wire is what commits us: a single
`task_id` for the whole batch is a promise that undoing it is one act, and
`PlanNotExecutable` is a promise that a rejected plan attempted nothing at all.

## Consequences

A permutation becomes expressible, so AI rename stops failing at its most
ordinary request. A user reviews verdicts before anything moves, and a batch
that fails halfway leaves either the original directory or a report naming what
remains applied.

An agent gains a directory-existence oracle. `fs.rename_batch_plan` mutates
nothing, but its `external` and `absent_source` verdicts report which names
exist, so it is a read and is gated as one — the same read gate as `fs.list` and
`fs.stat` (#80). The method's rustdoc says so explicitly, because "no task, no
journal, no mutation" reads as harmless and is not.

The N-1 window moves to 0.35.x. A 0.35 client never calls the new methods,
degrades `TaskKind::RenameBatch` to `Unknown` through its existing
`serde(other)`, and would degrade the two new error categories to
`Error::Unknown` if it somehow received them — nothing to gate on emission,
which is fortunate given decision 5.

The published JSON Schema (ADR 0038) enumerates the verdicts as a closed
`oneOf`, so the artifact describes N and not N+1: a strict external validator
will reject exactly the future values `serde(other)` exists to tolerate. This is
already true of `ConflictKind`, `TaskKind`, `TaskState` and `Error`, and is
recorded here rather than changed — the artifact is an honest description of
what this version emits, and the tolerance is a property of receivers.

## Alternatives considered

**One method that plans and executes, with a `dry_run` flag.** Rejected: the
two halves have different natures. The preview is a direct, non-mutating
response and the execution is a task with a `task_id`, so a single method would
return one shape or the other depending on a boolean — and the approval step,
which is the entire point, would have no place to sit between them.

**Send the approved steps back for execution.** Rejected on decision 2: it hands
a client the ability to execute an order the human never saw, and the client may
be an agent. It would also be *cheaper* — no re-planning — which is precisely
the trade being refused.

**One `Mutation::RenamedBatch { pairs }` journal row instead of N rows plus a
group id.** Rejected in the design doc: one row of unbounded size, poorer audit,
and after a crash the row does not say what actually happened, whereas N rows do.

**No cap on `pairs`, validating at the daemon only.** Rejected: the limit is
part of what a third-party implementer must know, and the five sibling
constants are already declared in `methods.rs`. A limit that lives only in the server is
a limit that only appears as a surprise.
