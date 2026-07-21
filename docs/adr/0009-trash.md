# 0009 - Trash support and explicit permanent-delete fallback

- Status: accepted
- Date: 2026-07-11
- Decision makers: Oscar González
- Related: specification section 5, M1 phase 8, ADR 0005

## Context

The specification requires native trash on freedesktop systems, Windows, and
macOS, with an explicit warning before falling back to permanent deletion.
Delete semantics must be unambiguous in the protocol, engine, and UI.

## Options considered

### Native implementation

- Implement three platform integrations in-house: freedesktop trash metadata,
  Windows `IFileOperation`, and macOS `NSFileManager`.
- Use the maintained MIT-licensed `trash` 5.x crate, whose MSRV is below
  norte's and whose Linux/Windows extensions include list, purge, and restore.

### Fallback semantics

- Let the engine silently turn an unsupported trash request into permanent
  deletion. This contradicts the requirement for an explicit choice and can
  lose data unexpectedly.
- Make `DeleteMode::Trash` return `Unsupported` when the provider lacks `TRASH`.
  A frontend may warn the user and send a second request with `Permanent` after
  confirmation.

## Decision

- Use the `trash` crate. Add `Provider::trash(path)`, defaulting to
  `Unsupported`. `LocalProvider` runs the platform call in `spawn_blocking` and
  advertises `TRASH`; `MemProvider` implements a logical equivalent for tests.
- In protocol 0.3.0, add the `TRASH` capability and
  `FsDeleteParams.mode: DeleteMode { Trash, Permanent }`. The optional field
  defaults to `Trash`, making the protocol's default recoverable.
- A trash task performs one operation on the root rather than walking the tree,
  and can be cancelled before dispatch. Permanent deletion retains the existing
  post-order walk. The future journal records a removal and, where supported,
  restoration through the crate's platform API.
- In the TUI, F8 requests trash when available. Without the capability, the
  confirmation explicitly says **PERMANENT** and resubmits with `Permanent`.
  Shift-F8 always requests permanent deletion.
- `norte rm`, an engine test-bed command, remains permanently destructive and is
  documented as such.

## Consequences

- The wire format has a safe default, and permanent fallback requires an
  informed user decision.
- M3 can build list, restore, and purge on the same platform integration.
- A client must gate trash on the provider's `TRASH` capability, not its own
  protocol version. A pre-0.3 core ignores the new `mode` field and would delete
  permanently, but it never advertises `TRASH`, so a conforming client warns and
  sends `Permanent` explicitly.
- Conversely, a 0.2 client against a 0.3 core fails safely with `Unsupported` on
  a provider without trash.
- Platform dependencies from the `trash` crate enter the local provider.
- Native trash reports a single unit of progress. Some platform operations are
  not cancellable after dispatch. In particular, Windows may permanently delete
  an item that the Recycle Bin cannot accept, and freedesktop cross-device trash
  may perform an internal copy and delete.

## M2 follow-up: ADR 0019

ADR 0019 resolves remote-provider behaviour by adding logical
`.norte-trash/` support to SFTP and object storage. Both now advertise the
existing `TRASH` capability; the read-only archive provider does not. No protocol
bump was required.

Remote logical-trash retention, garbage collection, list, and restore remain
future work, with `.norte-info` metadata already preserving the origin. Local
platform exceptions remain tracked separately.
