# Debt wave W4 — journal, policy and the wire

**Tier T2. This wave keeps the full pipeline**: a real plan doc written before
any code, `security-reviewer` on every mutation path, `protocol-guardian` on the
proto bump. It is the wave whose failures are silent and expensive, and the
budget saved in W1–W3 is what pays for it.

**Branch:** `debt/w4-safety`

## Journal and policy

| issue | crate(s) | what |
| --- | --- | --- |
| #160 | norte-core | a journal write that fails after a successful trash leaves the file moved and unrecorded |
| #178 | norte-core | an embedded session whose journal is corrupt or squatted runs unjournalled behind a warning |
| #179 | norte-core | the embedded journal's ownership window is all-or-nothing: no retry after a busy open, no release when idle |
| #146 | norte-core | anchor the journal's format marker, so a re-declaration is caught outright |
| #171 | norte-core | the undo gate parses `unit.len() * 2` VPaths on the caller's thread before the `Task` exists |
| #165 | norte-core, policy | the daemon state directory is 0700 but nothing excludes it from a policy scope over `$HOME` |
| #186 | norte-core | a `DeleteTree` cancelled mid-tree writes no journal entry, so a half-deleted subtree is lost silently |

#160, #178 and #179 are one story — the embedded journal's ownership and
failure model — and want one design, not three patches.

**#186 is hard rule 3 meeting hard rule 4**, and it is the worst failure in this
wave: a cancellation that is supposed to leave a clean destination instead
leaves a half-deleted subtree with nothing in the journal to undo it. It also
touches #176 (a `DeleteTree` revalidates the directory, not its content) —
same operation, both halves of "what does a `DeleteTree` actually promise".

## Sync semantics

| issue | crate(s) | what |
| --- | --- | --- |
| #176 | norte-sync | a `DeleteTree` revalidates the directory, not its content |
| #163 | norte-sync | destination name legality is not validated at planning time, only discovered at execution |
| #168 | norte-vfs-sftp, norte-vfs-object | the provider contract never runs with the logical trash enabled |
| #26 | norte-vfs-local | cross-device trash on freedesktop is an uncancellable copy+delete (hard rule 3) |
| #190 | norte-gui, norte-core | `compare`/`sync_plan`/`sync_apply` register a canceller with NO clean-cancellation test (hard rule 3) |
| #173 | norte-core, norte-tui | an applying sync is invisible to the task board: `TaskRef` is not `Clone`, so the board would take the only cancel handle |
| #196 | norte-frontend | a plan's steps are held unbounded in client memory |

## The wire — ONE bump for both

| issue | what |
| --- | --- |
| #170 | `SyncReportResult` carries no trash information, so a client that lost `sync.plan_done` cannot tell whether a batch is recoverable |
| #152 | `fs.compare`: two distinct files paired under an NFC singleton, with no marker on the wire |
| #195 | `SyncFailure` carries no `kind`, so a report row's anchor rests on an invariant the wire never states |

**THREE fields, ONE protocol version bump, one set of golden tests, one
`protocol-guardian` pass.** Bumping twice for two fields is the mistake this grouping exists to
prevent. An ADR if the pairing semantics change, not just the schema.

**Close:** `just ci` once. This branch also wants the second, external review
pass — it is the journal, the wire and the policy gate.
