# 0019 - Logical trash for remote providers

- Status: accepted
- Date: 2026-07-14
- Decision makers: Oscar González
- Related: ADRs 0009, 0013, and 0016; specification section 5

## Context

ADR 0009 added native trash for local and memory providers but left
`.norte-trash/` for remote providers. SFTP and object storage have no operating
system trash and need a recoverable delete operation that never silently
becomes permanent.

## Decision

- Logical trash is enabled per connection and defaults to off. When disabled,
  the provider does not advertise `TRASH`, so frontends follow ADR 0009 and ask
  before sending a permanent delete. This also avoids surprising S3 copy costs.
- Store each item at `.norte-trash/<id>/{<basename>,.norte-info}` in the
  provider root. IDs contain an epoch millisecond and a per-session monotonic
  counter. `.norte-info` stores the original `VPath::to_wire()` and deletion
  timestamp.
- `trash::info_decode` requires the expected connection root. It rejects an
  original path with a different scheme or authority, preventing an
  attacker-controlled sidecar from making restore write to another connection.
  `VPath::parse` already rejects traversal, encoded separators, and NUL.
  Same-connection overwrite remains a restore policy decision.
- Keep `Provider::trash(&self, path)` unchanged. Object storage guarantees no
  loss by completing copy-all before starting delete-all.
- SFTP creates the directory, renames the payload, and writes metadata. Object
  storage copies all objects before deleting the originals.
- Put deterministic ID, path, and `.norte-info` encoding helpers in the pure
  `norte-vfs::trash` module. Providers remain independent.

## Robustness rules

- Reject trashing `.norte-trash` or any descendant so the trash cannot be moved
  into itself.
- On an ID collision between sessions, advance the counter and retry.
- Treat creation of the shared trash root as idempotent.
- Stat the source first and return `NotFound` without creating an orphan.
- Reject a payload named `.norte-info`, which would collide with its metadata.

## Deferred work

- Fine-grained cancellation during object-store copy/delete walks is tracked by
  #51.
- A user-created `.norte-trash` file currently produces a safe but imprecise
  final error instead of `TypeMismatch`.
- SFTP and object providers duplicate roughly forty orchestration lines that may
  move into a closure-based `trash::execute` helper.
- Rename-failure injection, poisoned cross-connection metadata, and
  same-connection overwrite tests belong to M3 restore work.

## Consequences

Remote deletion is recoverable with no new dependency, stable metadata for M3
restore, and an opt-in default that avoids unexpected cost. An S3 crash during
the delete phase may leave a complete trash copy plus a partially deleted
source. That state is recoverable and reflects S3's non-atomic prefix semantics.
