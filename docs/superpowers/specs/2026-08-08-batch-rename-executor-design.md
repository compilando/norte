# The batch rename executor — design

**Date:** 2026-08-08
**Spec:** §17, "Batch rename: counters, slices, regular expressions, case
changes and character cleanup with collision preview and transactional undo.
AI rename feeds the same executor."
**Closes on the way:** nothing yet — #121 stays out on purpose.
**Status:** approved

## The problem

§17 says AI rename feeds the same executor as batch rename. Today the sentence
is inverted: AI rename exists and the executor does not.

`apply_ai_rename` (`crates/norte-tui/src/main.rs`) validates the approved plan
and then submits one `fs.move` per pair, in plan order. The GUI does the same.
Three consequences, all of them visible to a user:

- **No transaction.** The fifth move failing leaves four applied. Undo is
  per-task, so undoing the batch means undoing four things by hand.
- **No collision preview.** Each move meets the engine's collision handling on
  its own. The plan is never checked against itself, so the user approves a
  list without knowing which entries will clobber something.
- **Chained renames cannot work.** `a→b, b→c` collides on the first move even
  though the plan is perfectly consistent, and a permutation
  (`1→2, 2→3, 3→1`) can never succeed. "Number these episodes correctly" is
  the normal case for AI rename, and it is exactly the case that fails.

So the executor is not a feature bolted next to the AI one. It is the thing
that makes the AI one correct.

## Scope

This project builds the planner, the transactional executor, the journal
grouping and the two wire methods, and migrates the AI rename path in both
frontends onto them.

Out of scope, deliberately:

- the rules engine (counters, slices, regex, case, cleanup) — it only produces
  pairs, and it can be built on top later;
- a dedicated batch-rename dialog;
- CLI and MCP surfaces;
- #121 (AI plan over the selection instead of the directory) — a separate
  signature change in the engine and two frontends;
- cross-directory and cross-provider plans. Directory comparison and
  synchronisation needs a *transfer* plan executor, where one step is a long
  copy with progress and rollback means deleting copied bytes. That is another
  animal; it will share the *shape* of a plan, not this engine.

## Decisions

### 1. One directory, one provider

Every pair is `name → name` under the same parent, served by one provider.
This is what AI rename produces and what §17 asks for, and it is what makes
cycles and temporary names sound: a single provider `rename` is atomic, so the
inverse of a step is another `rename` rather than a copy.

Consequence: the batch does **not** go through `Engine::move_with_as`. It calls
`Provider::rename` directly, which is also why rollback is cheap.

### 2. The planner is pure, and it lives in the core

Rule 7: no business logic in a frontend. The frontend sends intent (pairs) and
renders what the core decided. It never computes the order.

Three units, each testable alone:

- **`norte-core::rename::plan`** — no I/O, no `async`. In: source and
  destination names as bytes, the current directory listing, and the
  `Capabilities` of that directory (case sensitivity, `RENAME`). Out: a
  `RenamePlan` — ordered steps, inserted temporaries, classified collisions,
  `plan_hash`. Tested with tables and proptest.
- **`norte-core::rename::exec`** — the Task. Takes a validated `RenamePlan`,
  walks it against a `Provider`, checks the `CancellationToken` between steps,
  and on failure or cancellation unwinds what it applied. It is what talks to
  the journal.
- **`Engine::rename_batch_plan` / `Engine::rename_batch`** — the façade:
  resolves the provider, fetches capabilities and the listing, applies the
  policy gate **once** with every path involved (`gate` already takes a slice
  and resolves most-restrictive), and on the execution path re-plans and
  compares `plan_hash`.

### 3. Name equality is decided by the destination directory, not by bytes alone

`same_name(a, b, caps)`:

- both valid UTF-8 → compare in NFC, and case-insensitively when the directory
  is case-insensitive;
- either one not UTF-8 → compare bytes exactly.

The original bytes are never modified; normalisation only decides equality.
This is the macOS NFD pitfall from CLAUDE.md, handled in one place and covered
by a fixture. Case sensitivity comes from `fs.capabilities`, which
`norte-vfs-local` already **probes per directory** (`pathconf`
`_PC_CASE_SENSITIVE` on macOS, a real probe elsewhere) rather than assuming it
per OS.

### 4. Collisions are classified, and a plan with any of them is not executable

Given the listing and the pairs:

| verdict | condition |
| --- | --- |
| internal collision | two pairs target the same destination |
| external collision | destination exists in the listing and is not any pair's source |
| dependency | destination is another pair's source — ordering, not a collision |
| absent source | a pair's source is not in the listing (the plan was computed against a stale listing) |
| null step | `from == to` under `same_name` — dropped from the plan |
| case-only | `Foo → foo` where the directory is case-insensitive — a real rename, not a null step; some providers need the detour through a temporary |

Any collision → the plan is returned **with the verdicts** and
`executable: false`. `fs.rename_batch` on a non-executable plan fails before
touching anything.

### 5. Cycles are broken with one temporary per cycle

Directed graph `source → destination`. Steps whose destination is free are
emitted first; repeat. What remains are pure cycles. Each cycle is broken by
renaming ONE node to a temporary name, running the rest of the cycle, and
landing the temporary last. Cost: one extra rename per cycle, not per entry.

Temporary names are `.norte-rename-<batch>-<n>`, checked against both the
listing and the plan's destinations, incrementing `n` on conflict. The name is
length-bounded so it cannot exceed the 255-byte component limit that MinIO
already taught us about; when the base name is long it is truncated by bytes,
respecting a character boundary when the name is UTF-8.

### 6. `plan_hash` hashes conclusions, so irrelevant changes do not invalidate it

sha256 over: the directory bytes, the sorted pairs, the resulting steps
including temporaries, the collision verdicts, and the case/normalisation flags
that were used.

`fs.rename_batch` carries the hash the user approved. The core re-plans from
the same pairs against the **current** directory and compares. Adding an
unrelated file does not change any verdict, so the hash still matches; adding a
file that creates a collision changes a verdict, so it does not, and the call
fails `PlanStale`. The client never sends the order, so it cannot smuggle a
plan the human did not see — which matters because the client may be an agent.

### 7. The batch is one undoable unit: n journal entries plus a `batch_id`

`Mutation::Renamed` gains `batch: Option<BatchId>`; only this path fills it, so
plain renames keep `None`. The journal table gains a `batch_id` column, and it
enters the hash chain — a batch cannot be un-grouped without breaking the
chain. `Journal::alloc_batch()` hands out a monotonic id under the same lock
that assigns `seq`.

**Old journals must keep verifying.** The chain hash feeds a presence byte for
every `Option` field, so naively adding `batch_id` to the hash would change the
hash of every entry written before this change and `verify_chain` would report
tampering on a journal nobody touched. The rule is therefore: `batch_id = None`
feeds **nothing at all** into the hash, and `Some(id)` feeds a presence byte
plus the length-prefixed id. Entries written before this change hash exactly as
they did, and tamper evidence is intact in both directions — stripping a
`batch_id`, or inventing one, changes the entry hash. The migration is
`ALTER TABLE journal ADD COLUMN batch_id INTEGER` guarded so an already-migrated
database is left alone, plus a test that opens a pre-migration journal and
verifies its chain.

The alternative — a single `Mutation::RenamedBatch { pairs }` — was rejected:
one row of unbounded size, poorer audit, and after a crash the row does not say
what actually happened.

`undo_session` already walks entries in reverse; on reaching an entry with a
`batch_id` it consumes the whole group as a block — all or nothing. If any step
of the block is not reversible the block is left untouched and the undo stops
there, which is the strict LIFO rule that already exists.

### 8. Failure and cancellation both mean rollback, and the report never lies

Per step: `provider.rename(from, to)`; on success record
`Renamed { from, to, batch: Some(id) }` and push the inverse.

Two failure shapes, one path:

- the rename fails;
- the rename succeeds and the journal insert fails — rule 4, if it is not
  durable it did not happen.

Either way the executor stops and walks the inverse stack. Temporary renames
unwind like any other. Each rollback step is journalled too, with `undoes_seq`
pointing at the entry it compensates — the M3-2 mechanism, not a new one.

**If the rollback itself fails** it stops there and the result states which
steps remain applied and which one could not be reverted, with names. Never a
bare error: the user has a half-renamed directory and needs to know where.

Cancellation (rule 3) checks the token between steps and unwinds the same way,
so the tree is either as it was or the report says otherwise.

After a process crash there is no "batch closed" marker: the entries say what
actually happened, so a later undo reverts the applied prefix. Surviving
temporaries are named `.norte-rename-*` on purpose — a human recognises them.
No automatic sweep (the M3-1b decision stands).

## The wire (proto 0.35.0 → 0.36.0)

Requires a protocol-guardian review.

- `fs.rename_batch_plan`: `{ dir: VPath, pairs: [{ from: Segment, to: Segment }] }`
  → `{ steps: [{ from, to, temp: bool }], collisions: [{ name, kind }], executable: bool, plan_hash }`.
  A direct response: no task, no journal.
- `fs.rename_batch`: `{ dir, pairs, plan_hash }` → an accepted task, like the
  rest of `fs.*`.
- `TaskKind::RenameBatch`; progress is `i/n` steps, not bytes.
- `Error::PlanStale` (the directory changed since the preview) and
  `Error::PlanNotExecutable` (the plan has collisions). Both actionable: the
  frontend re-plans and asks again.

Names travel as `Segment`, never as `String`. New goldens for every type and
for `methods.json`. The N-1 window shifts to 0.35.x.

## The frontends

The AI confirmation modal already exists in both. It gains the plan the core
returned: one path **per line**, labelled, elided in the middle, invisibles
masked — the discipline the encoding audit imposed on the agent-approval modal
(M3-3b T5), reused rather than reinvented. Collisions are rendered and the
confirm action is disabled while `executable` is false. Strings go through
Fluent in `i18n/`.

`apply_ai_rename` stops being a loop: it calls `rename_batch_plan` to render
and `rename_batch` on confirmation.

## Tests

- **Planner, tables:** permutation of three, chain `a→b→c`, internal collision,
  external collision, absent source, null step, `Foo→foo` on a case-sensitive
  and on a case-insensitive directory, non-UTF-8 name, 255-byte name needing a
  temporary.
- **Planner, proptest:** over `MemProvider`, every `executable` plan executed
  leaves exactly the expected set of destination names; and the planner always
  terminates.
- **Executor:** failure injected at step k leaves the directory **identical** to
  its initial state; cancellation at step k likewise; journal failure at step k
  likewise. Three tests over one helper.
- **Journal:** `batch_id` participates in the chain (tamper test); undo of a
  batch is all-or-nothing.
- **Encoding:** new fixture in the canonical `norte-testkit` corpus — a
  permutation of hostile names, byte-exact round trip.
- **E2E:** real local provider, permute three files plus one non-UTF-8 name,
  undo, compare bytes.

Coverage note: all new code lands in proto and core, both under the 85% gate,
and the margin is 0.12 points. The pure planner is cheap to cover thoroughly
and carries what the executor cannot reach.

## Definition of done

Code, unit tests, integration tests at the provider boundary, rustdoc with
doctests on the new public items in proto and core, journal migration,
protocol-guardian review of the wire, encoding-auditor review of the name
comparison and the modal, `just ci` green.
