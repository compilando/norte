# 0004 - Protocol v0 wire conventions and evolution

- Status: accepted
- Date: 2026-07-09
- Decision makers: Oscar González

## Context

M0 introduced the first protocol types beyond `VPath`: entries, capabilities,
tasks, the error taxonomy, and `fs.*` and `task.*` methods. Golden tests make
these choices part of the protocol contract. The representation and evolution
rules must allow the core to support protocol versions N and N-1.

## Decision

### Representation

- Data-carrying enums use internal tagging with `tag = "kind"`; variants and
  fields use `snake_case`. Example:
  `{"kind":"conflict","conflict":"exists"}`.
- Unit enums such as `EntryKind`, `TaskKind`, and `ConflictKind` use plain
  `snake_case` strings.
- `CapabilityFlags` use a readable string such as
  `"RENAME_ATOMIC | SYMLINKS"`, including in future binary encodings.
- `TaskId` is a transparent JSON `u64`.
- `mtime_ms` is a signed count of milliseconds since the UTC epoch. Negative
  values represent dates before 1970.
- Canonical writers emit optional fields as explicit `null`; readers accept
  absence through `#[serde(default)]`. `None` means unknown, never a fabricated
  zero.
- Golden tests compare `serde_json::Value` in both directions. Key order and
  whitespace are not contractual; names, types, and values are.

### Forward compatibility

| Surface | Unknown input | Behaviour |
| --- | --- | --- |
| Struct | Extra field | Ignore it through Serde's default behaviour. |
| `EntryKind` | New kind | Map to `other` with `#[serde(other)]`. |
| `Error` | New category | Map to the hidden `Unknown` fallback, which the core never emits. |
| `TaskState` | New state | Map to `Unknown` and treat it as non-terminal. |
| `TaskKind` | New kind | Map to `Unknown` from protocol 0.10.0 onward. Older 0.9 clients still require version-gated emission of new kinds. |
| `CapabilityFlags` | Well-formed new name matching `[A-Z0-9_]+` | Ignore it; an unknown advertised feature is simply unavailable to that client. |
| `CapabilityFlags` | Hex value or malformed token | Reject it. Unnamed bits must not travel over the wire. |

Adding a variant, category, flag, or optional field is compatible. Removing or
renaming one, or adding a required field to an existing variant, is breaking
and requires a semantic `PROTOCOL_VERSION` bump. `Error` and `TaskState` are
`#[non_exhaustive]`, so frontends include a fallback match arm from the start.

Tests for invented `Unknown` values remain unit tests rather than golden
fixtures because canonical writers never emit them.

### Error taxonomy

In addition to the specification's `NotFound`, `PermissionDenied`,
`Conflict { kind }`, `ProviderUnavailable { retryable }`, `Cancelled`,
`PolicyDenied { rule }`, and `EncodingLoss`, M0 adds:

- `NoSpace` for ENOSPC and EDQUOT;
- `Io { retryable }` for an I/O failure during an operation, distinct from an
  unavailable provider;
- `Unsupported` and `InvalidPath`;
- `Internal { panic }` for the project's panic policy.

Human-readable detail belongs in the JSON-RPC error `message`, not in the
machine-readable taxonomy.

### Task states

The wire values are `pending`, `running`, `paused`, `completed`, `cancelled`,
and `failed`. The wire representation takes precedence over older specification
wording. `paused` is reserved from v0 even though M0 does not emit it.
`task.cancel` is part of M0; `task.pause` and `task.list` arrive with the daemon.

### Future `fs.list` pagination

M0 returns a complete listing. A newer core must continue returning complete
results to clients that omit a cursor; it must never silently truncate a result
for an N-1 client. ADR 0017 applied this rule when cursor pagination was added
in protocol 0.8.0.

## Consequences

- Most additions planned for M1 through M4 remain backward compatible.
- Frontends render typed categories with defined fallbacks rather than parsing
  messages.
- Hidden `Unknown` variants are part of the public API and must be documented as
  receive-only fallbacks.
- Capability parsing requires a small custom parser so it can ignore named
  future flags while rejecting hexadecimal or malformed values.
