# #99 — Idempotent trash with an engine-deterministic id

- Date: 2026-07-25
- Status: approved (brainstorming)
- Issue: #99 (remnant of #32.3); ADR 0009 (trash), ADR 0019 (remote logical
  trash), #17/#32 disambiguation precedents.

## Problem

`delete_task` in `norte-core/src/ops.rs` calls `provider.trash(&path)` exactly
once — no retry, no post-effect disambiguation, unlike `remove_retrying` /
`mkdir_retrying`. Two consequences:

1. A transient error (network blip on a remote provider) fails the whole trash
   op even when the move already applied.
2. A naive retry would break worse: the logical-trash destination is
   `.norte-trash/<id>/payload` where `<id>` = `<deleted_ms>-<counter>` is
   regenerated on each attempt. After a partial success (rename applied, ack
   lost), a retry re-plans with a *new* id, `stat(p)` is `NotFound`, and the
   first destination is unrecoverable — so the journal records `Trashed { dest:
   None }` and session undo can no longer restore that item (`reversal_ref`
   lost).

The consequence is bounded: worst case is a degraded undo, never data loss (the
payload is safely in the trash, just unlinked from the journal). Priority is low,
but the fix removes a real correctness gap and closes the last remnant of #32.

## Decision

Make `trash` **idempotent, keyed by an id the engine generates once per op**, so
every retry of the same op targets the same deterministic destination and can
recover it.

### 1. `norte-vfs::trash` — `TrashId`

A newtype over the validated `<deleted_ms>-<counter>` string (already a valid
`Segment`). Constructed once from `(deleted_ms, counter)`; exposes `as_str()`.
`plan(p, id)` keeps taking the id string. The engine owns generation (wall-clock
ms + an engine-session monotonic counter), so the id is stable across retries of
one op and unique across ops within an engine session.

### 2. Trait change

```rust
async fn trash(&self, p: &VPath, id: &TrashId) -> Result<Option<VPath>, Error>;
```

Default impl stays `Unsupported`. The op's contract becomes **idempotent**: given
the same `id`, repeating it converges on the same trash entry rather than
creating a second one.

### 3. Provider implementations

- **local (native OS trash):** ignores `id`, returns `None` as today — there is
  no stable, recoverable destination to key on. Cross-device freedesktop
  copy+delete stays out of scope (#26).
- **sftp / object (logical `.norte-trash/<id>/`):** the entry directory is now
  derived from the engine `id` instead of an internally generated one. The
  internal "collision → regenerate id" loop is removed (the id is fixed by the
  caller). Idempotent branches:
  - `stat(p)` is `NotFound` **and** `.norte-trash/<id>/payload` exists with our
    `.norte-info` → return `Some(payload)`: this op already applied on a prior
    transient attempt.
  - `mkdir` returns `Conflict::Exists` on our own `<id>` directory → it is our
    partial retry → skip rewriting `.norte-info`, ensure the rename, return
    `Some(payload)`.
  - Otherwise behave exactly as today (fresh dir → info → rename → return
    payload).

### 4. Engine `trash_retrying`

Mirrors `remove_retrying`: `with_retry` semantics + a per-op `CancellationToken`
check (rule 3) + an `ambiguous` flag.

- Logical providers return `Some(payload)` directly on retry (verified via id),
  so `reversal_ref` **survives** the transient.
- For native (`None`-dest) providers, `Err(NotFound) if ambiguous → Ok(None)`:
  the file was trashed on a prior transient attempt; there is no recoverable
  dest, and undo degrades exactly as native trash always does.
- A genuine `NotFound` with no prior transient still propagates (the victim
  never existed).

`delete_task` generates the `TrashId` once and calls `trash_retrying` instead of
`provider.trash` directly; the `Mutation::Trashed { path, dest }` and its journal
`reversal_ref` are unchanged downstream.

### 5. Test harness

`MemProvider` gains **logical trash** (closes debt H2) plus a fault hook that
injects a transient error *after* the rename step. Tests:

- **recovery:** transient-after-move → retry returns `Some(payload)`; the journal
  keeps `reversal_ref`.
- **clean cancellation:** cancel during the retry backoff leaves no orphan and
  returns `Cancelled` (rule 3).
- **genuine NotFound propagates:** a victim that never existed is not masked by
  the idempotency.
- **native degradation:** a `None`-dest provider under transient-then-gone
  returns `Ok(None)` (undo degrades, documented), never a false failure.

## Options considered

- **Engine-deterministic id (chosen).** One trait-signature change; idempotency
  logic centralized in the engine mirror plus a small per-provider recovery
  branch; `reversal_ref` recoverable. Cost: touches the four providers and the
  trait.
- **Provider-internal retry.** Trait unchanged, but each logical provider
  duplicates retry + idempotency; more code, N copies, harder to keep uniform.
  Rejected.
- **Light mitigation, no trait change.** Engine pre-stat + single re-stat: on
  transient-then-gone record `Trashed { dest: None }`. Closes "no data loss" but
  never recovers `reversal_ref` — leaves the actual gap open. Rejected.

## Consequences

### Positive

- Transient trash failures on remote providers recover the destination; session
  undo keeps working across a blip.
- The last remnant of #32 closes; trash joins remove/mkdir in the
  disambiguation family with consistent semantics.
- `MemProvider` finally supports logical trash, unblocking hostile trash e2e
  (debt H2).

### Negative

- `Provider::trash` gains a parameter — a breaking trait change touching all
  four providers and every call site (pre-release, acceptable).
- Native local trash still degrades undo (no recoverable dest); this is inherent
  to the OS trash and unchanged by this work.

## Isolation note

The current main working tree has unrelated in-flight ADR 0039 work
(provider attributes). This change will be implemented in a dedicated git
worktree to avoid tangling with those uncommitted edits (both touch
`norte-core/src/ops.rs` and `norte-proto`).
