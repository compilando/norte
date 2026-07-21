# 0023 - SQLite WAL journal with an application hash chain

- Status: accepted
- Date: 2026-07-14
- Decision makers: Oscar González
- Related: specification sections 4 and 10; hard rule 4; issue #11

## Context

M3 requires every mutation to be recorded with its operation, source, reversal,
and chain hash so later work can provide undo and audit export. The engine
already emits `Mutation` through `MutationObserver`; it needs durable storage
and real wiring.

## Decision

- Use `sqlx` with SQLite and the Tokio runtime. Runtime queries avoid a build
  database, and async inserts require no dedicated blocking actor. Configure
  SQLite in WAL mode with `synchronous=NORMAL` and serialize the single writer
  with the hash-chain mutex. `rusqlite` would require a blocking actor; `redb`
  would violate the specification's single SQLite-engine decision.
- Make `on_mutation` async and await the insert before acknowledging the
  operation.
- Compute
  `entry_hash = sha256(prev_hash || length-prefixed fields and presence bytes)`.
  This keyless chain detects corruption and unsophisticated edits. It does not
  prove tampering against an attacker who can rewrite or truncate the database;
  HMAC or signature anchoring is deferred to M3-5 and issue #63. Documentation
  must not call the unanchored chain tamper-evident.

## Consequences

sqlx and its dependency tree become structural dependencies, but journal,
index, tags, and future embeddings can share one database engine. Inserts are
serialized, and `seq` is allocated under the same lock so sequence order equals
hash order. Batching remains available if this ever becomes a bottleneck.

`synchronous=NORMAL` gives process durability but may lose a newly acknowledged
entry after power or OS failure. M3-5 may select `FULL` if audit requirements
justify the fsync cost. On Unix, create the database with mode 0600 and rely on
the per-user daemon directory being 0700; WAL and SHM sidecars inherit that
protection. cargo-deny verifies sqlx licensing.
