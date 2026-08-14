# W4a — the embedded journal's ownership and failure model

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development` or `superpowers:executing-plans`. Steps use `- [ ]` for tracking.

**Goal:** close #179, #178, #160, #186 and #146. They are one design told five
ways, not five patches — which is why this wave gets a written plan and the
others did not.

**Done, with two deliberate half-closes.** #160, #186 and #146 are closed.
**#179** is closed for its primitive half only — `release()` exists and nothing
calls it on a timer, so "a TUI owns `journal.db` all day" survives; and **#178**
for its clumsy half only — a process that merely HOLDS the lock still runs a
session unjournalled, which is the case that must keep working (#203). Four
issues came out of the reviews: #203 (the squatter), #204 (`trust_host_key`
writes `known_hosts` ungated), #205 (one operation can now be journalled in
part, a regression of the #179 retry), #206 (the `created`-after-`trashed` half
of #160).

**The one sentence.** Today the embedded journal's ownership is decided ONCE,
never revisited, and its two failure modes are collapsed into one that fails
OPEN — while two executor paths can land a mutation and not record it. Every
one of the five is a consequence.

**Gate:** `just t norte-core` in the loop, `just ci` once at the close.
**Reviewer:** `security-reviewer` is mandatory on every task that touches a
mutation path, dispatched by the agent doing the work. This branch also wants
the second, external pass — it is the journal.

**Hard rules in play:** 3 (cancellation), 4 (every mutation through the
journal), 6 (typed errors).

## What the implementer must know before task 1

- `LazyJournal` (`crates/norte-core/src/embedded.rs`) holds
  `cell: tokio::sync::OnceCell<Result<Arc<SqliteJournal>, NoJournal>>` and a
  `sink: Mutex<SinkSlot>`. `NoJournal` is `Busy` or `Failed`.
  `ESPERA_POR_EL_LOCK` is 250 ms.
- The `OnceCell` is load-bearing for two properties, and whatever replaces it
  must keep both: **two concurrent mutations share one attempt** (otherwise the
  second sees `Busy` against the first one's own lock), and **the warning is
  emitted exactly once**.
- **The `ChainState` trap is real and it is the reason this is not small.**
  Re-opening a journal this process once owned must RE-READ `last_seq` and
  `last_hash` from the file. A stale pair collides with the `seq` primary key,
  and because `last_seq` only advances on success, every later mutation of that
  process fails — an effect applied with no row, in a loop. #179 names this;
  design against it first, not last.
- The reason written in `embedded.rs` for never retrying is **partly wrong**,
  and the review said so: the `ChainState` hazard applies to reopening a
  journal this process ONCE OWNED. A failed open never built a `ChainState`, so
  retrying after `Err` is safe.
- The TUI paints a persistent "NOT journalled" indicator (#177). A session that
  silently starts recording again leaves that indicator lying, so every
  transition — including RECOVERY — must reach the sink.

---

## Task 1: an ownership window that can open and close more than once

Closes the primitive half of #179. No behaviour change to the failure
CLASSES yet; that is task 2.

**Files:** `crates/norte-core/src/embedded.rs`, `crates/norte-core/src/journal.rs`

- [x] **Step 1: the failing tests.** Four, and the third is the one that matters:
  1. a busy first attempt followed by a free journal RECORDS on a later mutation
     (today it never retries);
  2. a busy attempt does not pay `ESPERA_POR_EL_LOCK` again within the brake
     window (assert on attempt count, not on wall clock);
  3. **re-acquiring after a release re-reads the chain state** — build a journal,
     mutate, release, have another writer append, re-acquire, mutate, and assert
     the new row's `seq` follows the OTHER writer's, not this process's stale
     one. This is the test that would have caught the trap;
  4. two concurrent mutations against a free journal open ONE handle.
- [x] **Step 2:** `just t norte-core` — watch them fail.
- [x] **Step 3: implement.** Replace the `OnceCell` with a state machine under
  one lock: the handle when held, the last attempt's instant and verdict when
  not. Re-read the chain state on every acquisition. Serialise attempts through
  the same lock the `OnceCell` used to serialise through. Brake: at most one
  attempt per 30 s after a `Busy`.
- [x] **Step 4:** every transition reaches the sink, RECOVERY included.
- [x] **Step 5:** `just t norte-core`, `just c`, `security-reviewer`, commit.

## Task 2: a corrupt or squatted journal fails CLOSED

Closes #178. Today `Busy` and `Failed` have the same consequence — no journal,
one `warn!` — so the classification buys nothing and anyone who can write the
state directory disables journalling for every embedded session, quietly and
permanently.

- [x] **Step 1: the failing test.** A `journal.db` that is not a database (or
  is mode 000) makes a mutation REFUSE, with a typed error naming the file —
  not proceed behind a warning. And its twin: a `Busy` journal still proceeds,
  because a transient holder must not brick the session.
- [x] **Step 2:** watch it fail.
- [x] **Step 3: implement.** `Failed` refuses the mutation; `Busy` continues
  unjournalled and retries under task 1's brake. Match `daemon run`, which
  already aborts on the same input — the asymmetry is the bug.
- [x] **Step 4:** the refusal is a Fluent string in both locales, and it says
  what to do (the file, and that removing or fixing it restores journalling).
- [x] **Step 5:** `security-reviewer` — this is the task that changes what an
  attacker with write access to the state directory can achieve.

## Task 3: an effect that lands is recorded, including when it lands partly

Closes #160 and #186. Two paths where the mutation happens and the record does
not.

- **#160** was hit live, not theorised: `sync.apply` trashed the destination and
  then failed to write its row. The executor logs at `error!` and stops, which
  is the best it can do today, but the file has moved and nothing records it.
- **#186** is `destroy_tree` cancelled mid-tree: the entry is written only when
  the removal was not cancelled, so a half-deleted subtree leaves no row.

- [x] **Step 1: the failing tests.** A cancelled `DeleteTree` that removed part
  of a subtree writes an entry for what it REMOVED; a journal write that fails
  after a successful trash surfaces an unrecoverable-state error naming the
  buried path and its trash destination, and does not continue to the next step.
- [x] **Step 2:** watch them fail.
- [x] **Step 3: implement.** `destroy_tree` returns what it removed even on
  cancellation, and the entry is written for that subset — a partial removal is
  a real state and hard rule 4 does not exempt it. For the write failure, the
  error must reach the caller as its own variant (rule 6), not a log line: the
  operation stops, and what was buried is named in the error so a human can
  find it.
- [x] **Step 4:** `security-reviewer`, commit.

**Do not "fix" #160 by writing the row first.** A row for an effect that then
fails is the same lie in the other direction, and the chain cannot be rewound.

## Task 4: anchor the format marker

Closes #146. ADR 0046 concedes the hole: three column writes and no key
re-declare the format, turning `Broken { first_bad_seq: k }` into
`UnknownFormat`. The alarm survives; the blame does not. HMAC anchors do NOT
close it — the edit changes no `entry_hash` at `seq >= 1`, so every anchor still
verifies.

- [x] **Step 1: the failing test.** Re-declare the format on an anchored
  journal and assert `AnchorVerdict::HashMismatch` at `seq 0`.
- [x] **Step 2:** watch it fail.
- [x] **Step 3: implement.** One extra MAC'd line at `seq 0`, written at
  creation or on the first `norte audit anchor`. `entry_hash_at` filters
  `seq >= 1` on purpose, so this needs a dedicated accessor for the marker's
  digest — do not relax the filter.
- [x] **Step 4:** amend ADR 0046 in place where it concedes the hole, rather
  than appending a correction nobody reads. `protocol-guardian` is NOT needed
  (no wire change); `security-reviewer` is.

## Task 5: close the branch

- [x] Whole-branch `security-reviewer` and a coherence pass: do tasks 1 and 2
  agree about what a session that lost and regained its journal tells the user,
  and can task 3's refusal path leave a task half-applied with no way to say so.
- [x] `just ci` once. Grep for `^error` and `Summary`, not a pipe's exit code.
- [x] Close #160, #186, #146; half-close #179 and #178 with what each
      established and what each left behind.
