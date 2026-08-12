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

- Use the `trash` crate. *(Superseded on Linux/BSD by the 2026-08-12 amendment
  at the foot of this ADR; macOS and Windows still use it.)* Add
  `Provider::trash(path)`, defaulting to `Unsupported`. `LocalProvider` runs the platform call in `spawn_blocking` and
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
  not cancellable after dispatch. In particular, **Windows may permanently
  delete an item that the Recycle Bin cannot accept** (#25), and macOS's
  `NSFileManager` gives no more say in where an item lands than Windows does.
  The freedesktop cross-device case is no longer one of these — see the
  amendment below.

## Amendment, 2026-08-12: freedesktop is ours now, and it says where it put things

The directory-synchronisation branch (`docs/superpowers/plans/2026-08-11-directory-sync.md`,
task 11b) replaced the `trash` crate on **Linux and BSD** with an in-tree
implementation of the freedesktop spec, `norte-vfs-local::trash_fdo`. macOS and
Windows still delegate to the crate and are unchanged by this amendment.

**Why.** The crate buries a file but never says *where*, so `Provider::trash`
answered `Ok(None)`, the journal got no `reversal_ref`, and an undo had to match
by original path and take the most recent item — which, when undoing the
`trashed` + `created` pair of an overwrite, is the file the undo itself has just
buried. It restored the new file over itself, left the user's original in the
trash, and reported success. Choosing the destination ourselves is the only way
to know it, and the freedesktop spec is short enough that this is a small module,
not a platform port. It is the same shape the remote logical trash of ADR 0019
already had.

**Two consequences that change what earlier sections of this ADR said.**

1. **The cross-device exception no longer applies on Linux/BSD, and the
   sentence in "Consequences" above is corrected accordingly.** The `trash`
   crate degraded a cross-device burial into copying the tree to the home trash
   and deleting the source — gigabytes inside a single `spawn_blocking`, with no
   progress and no cancellation (#26). The freedesktop trash we implement is
   always on the **victim's own device**: the home trash when the victim is on
   `$HOME`'s filesystem, otherwise the victim's mount (`$top/.Trash/$uid` or
   `$top/.Trash-$uid`). Burial is therefore always a `rename`, and when no trash
   can be established on that mount the answer is `Unsupported`, which this ADR
   already routes to "warn and offer permanent delete" — not a silent copy.
   **This is a Linux/BSD statement only.** Windows and macOS keep the behaviour
   described above.
2. **The topdir trash is created defensively.** `create_dir_all` follows
   symlinks and checks no ownership, so a pre-planted `/tmp/.Trash-1000` owned
   by another user captured everything this user trashed under `/tmp` — and the
   `reversal_ref` written to the journal then pointed into a tree that user
   controlled at undo time. The topdir trash now demands an `lstat` and our own
   `st_uid`, falling back `$top/.Trash/$uid` → `$top/.Trash-$uid` →
   `Unsupported`. The **home** trash still follows symlinks on purpose: it hangs
   off `$HOME`, and glib does the same.

Restoring goes through a new `Provider::restore_from` (default: `rename`), so
the freedesktop sidecar leaves with its file instead of becoming an orphan. A
companion promise, `Provider::trash_restorable()`, states per *instance* — not
per platform — whether a trash names what it buries; it is what lets a
synchronisation plan mark a step `Irreversible` up front instead of promising a
`reversal_ref` that would never arrive (ADR 0049).

## M2 follow-up: ADR 0019

ADR 0019 resolves remote-provider behaviour by adding logical
`.norte-trash/` support to SFTP and object storage. Both now advertise the
existing `TRASH` capability; the read-only archive provider does not. No protocol
bump was required.

Remote logical-trash retention, garbage collection, list, and restore remain
future work, with `.norte-info` metadata already preserving the origin. Local
platform exceptions remain tracked separately.
