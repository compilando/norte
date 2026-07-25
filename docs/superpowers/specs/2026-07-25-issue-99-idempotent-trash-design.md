# #99 — Idempotent trash with an engine-deterministic id

- Date: 2026-07-25
- Status: implemented
- Issue: #99 (remnant of #32.3); ADR 0009 (trash), ADR 0019 (remote logical
  trash), #17/#32 disambiguation precedents.

## Problem

`delete_task` in `norte-core/src/ops.rs` called `provider.trash(&path)` exactly
once — no retry, no post-effect disambiguation, unlike `remove_retrying` /
`mkdir_retrying`. Two consequences:

1. A transient error (network blip on a remote provider) failed the whole trash
   op even when the move already applied.
2. A naive retry would break worse: the logical-trash destination is
   `.norte-trash/<id>/payload` where `<id>` = `<deleted_ms>-<counter>` was
   regenerated on each attempt. After a partial success (rename applied, ack
   lost), a retry re-planned with a *new* id, `stat(p)` was `NotFound`, and the
   first destination was unrecoverable — so the journal recorded `Trashed { dest:
   None }` and session undo could no longer restore that item (`reversal_ref`
   lost).

The consequence is bounded: worst case is a degraded undo, never data loss (the
payload is safely in the trash, just unlinked from the journal).

## Decision

Make `trash` **idempotent, keyed by an id the engine generates once per op**, so
every retry of the same op targets the same deterministic destination and can
recover it.

### 1. `norte-vfs::trash` — `TrashId`

A newtype carrying `(deleted_ms, counter)`. `as_segment()` yields the
`<deleted_ms>-<counter>` string (always a valid `Segment`); `deleted_ms()` feeds
the `.norte-info`. The engine owns generation (wall-clock ms + a per-op counter —
the task id for a delete, the journal `seq` for an undo compensation), so the id
is stable across retries of one op and unique across ops.

### 2. Trait change

```rust
async fn trash(&self, p: &VPath, id: &TrashId) -> Result<Option<VPath>, Error>;
```

Default impl stays `Unsupported`. Given the same `id`, repeating the op converges
on the same trash entry rather than creating a second one.

### 3. Provider implementations

- **local (native OS trash):** ignores `id`, returns `None` — no stable,
  recoverable destination to key on. Cross-device freedesktop copy+delete stays
  out of scope (#26).
- **sftp / object / MemProvider (logical `.norte-trash/<id>/`):** the entry
  directory is derived from the engine `id`. Idempotent branches:
  - `stat(p)` is `NotFound` **and** `.norte-trash/<id>/payload` exists **and its
    `.norte-info` decodes to `p`** → return `Some(payload)`: this op already
    applied on a prior transient attempt. A payload whose `.norte-info` belongs
    to a *different* victim (a foreign session colliding on the same id) is
    rejected as a real `Conflict`, never reclaimed.
  - `mkdir` `Conflict::Exists` on the `<id>` dir → if the `.norte-info` is absent
    (our partial before the info write) or decodes to `p`, proceed; a foreign
    `.norte-info` is a real collision and propagates (the id is fixed, no longer
    regenerated).

### 4. Engine `trash_retrying`

Mirrors `remove_retrying`: `with_retry` semantics + a `CancellationToken` check
in the loop + an `ambiguous` flag.

- Logical providers return `Some(payload)` on retry (verified via id), so
  `reversal_ref` survives the transient.
- For native (`None`-dest) providers, `Err(NotFound) if ambiguous → Ok(None)`:
  the file was trashed on a prior transient attempt; there is no recoverable
  dest, and undo degrades exactly as native trash always does.
- A genuine `NotFound` with no prior transient still propagates.

`delete_task` generates the `TrashId` once before the loop; undo's compensating
trash also routes through `trash_retrying` with a `seq`-derived id, closing the
same `reversal_ref`-loss class on the undo path.

### 5. Cancellation guarantee (honest)

The loop honors the token: a cancel is observed at the top of each iteration and
before each backoff. The guarantee is **no data loss** — a cancel during the
backoff *after* the effect already applied leaves the payload safely in
`.norte-trash/<id>/` but returns before the journal `Trashed` entry is written,
so session undo may lose the link for that one item (it degrades, exactly like a
mid-backoff cancel of `remove_retrying`). This is not an orphaned-partial-file
situation; the item is recoverable from the trash directory.

### 6. Test harness

`MemProvider` gains logical trash (closes debt H2) plus, on both the logical and
the vanish paths, an `ambiguous_gate` that simulates a transient *after* the
effect applied. Tests cover: recovery keeps `reversal_ref`; native degrades to
`None` without failing; genuine `NotFound` is not masked; a hostile non-UTF-8
basename (from the canonical corpus) survives the subtree re-key and the
idempotent recovery byte-exact; and `trash_retrying` honors a cancelled token.

## Options considered

- **Engine-deterministic id (chosen).** One trait-signature change; idempotency
  centralized in the engine mirror plus a small per-provider recovery branch;
  `reversal_ref` recoverable.
- **Provider-internal retry.** Trait unchanged, but each logical provider
  duplicates retry + idempotency; more code, N copies. Rejected.
- **Light mitigation, no trait change.** Engine pre-stat + single re-stat →
  `Trashed { dest: None }` on transient-then-gone. Closes "no data loss" but
  never recovers `reversal_ref`. Rejected.

## Consequences

- Transient trash failures on remote providers recover the destination; session
  undo keeps working across a blip. The last remnant of #32 closes.
- `Provider::trash` gains a parameter — a breaking trait change touching all four
  providers and every call site (pre-release, acceptable).
- Native local trash still degrades undo (no recoverable dest); inherent to OS
  trash, unchanged by this work.
