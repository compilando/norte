# 0025 - HMAC journal-head anchors and audit export

- Status: accepted
- Date: 2026-07-17
- Decision makers: Oscar González
- Related: issue #63 and M3-5

## Context

ADR 0023's unkeyed SHA-256 chain detects corruption and edits that do not
recompute it. An attacker with database write access can still rewrite the full
chain, truncate its tail, or restore an older snapshot. M3-5 also needs stable
CSV/JSONL export and verification that identifies the first broken sequence.

The local threat model includes a same-UID process that can write the database.
Perfect protection through WORM storage or external notarization is outside a
standalone daemon's assumptions.

## Options considered

- Sign the head with Ed25519. This supports public verification but introduces
  key-pair rotation and another structural cryptography dependency, while a
  single-user local verifier still shares access to the keyring.
- Append HMAC-SHA256 head anchors using a key stored in the OS keyring. This is
  a small addition to the existing SHA-256 and keyring stack.
- Require external/WORM notarization. This provides a stronger same-UID boundary
  but requires infrastructure the application cannot assume.

## Decision

Append JSON lines to `journal-anchors.jsonl`:

```text
{seq, head_hex, mac_hex}
```

Compute
`HMAC-SHA256(key, "norte-anchor-v1" || seq_le || head)` so the context string
provides domain separation and format versioning. Store a generated key in the
system keyring under service `norte`, account `journal-anchor`; never write it
to plaintext disk. `NORTE_ANCHOR_KEY` exists only for controlled headless
environments and weakens protection against same-UID processes.

Verification recomputes the chain, reports `first_bad_seq`, validates every
anchor against the current chain, and reports maximum anchored sequence versus
the current head. Missing anchors fail unless `--allow-no-anchors` is explicit.
The anchor command also prints the line so an operator can store it externally;
an external copy is what detects deletion, rollback, or restoration of a
consistent database-plus-anchor snapshot.

Add `norte-core::journal::audit` with `ChainStatus`, deterministic JSONL/CSV
export, and injectable-key anchor verification. The core does not know how the
CLI resolves the key. `norte audit verify|export|anchor` opens the database
read-only. Because a live daemon holds SQLite exclusive locking, users currently
stop it before auditing; a future paged wire API could remove that limitation.

## Consequences

The implementation closes #63 with an explicit, bounded claim: rewriting
history without the key invalidates anchors and tail truncation behind an
anchor is detected. It does not protect against an attacker who can read the
keyring, the unanchored interval, or deletion/rollback of both local files
without an external anchor copy.

Exports are stable without a protocol change. The core adds the small
RustCrypto `hmac` dependency, and the former Boolean chain result becomes
`ChainStatus` with a useful break location.
