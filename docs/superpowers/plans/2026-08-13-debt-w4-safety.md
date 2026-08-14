# Debt wave W4 — journal, policy and the wire

**Tier T2. This wave keeps the full pipeline**: a real plan doc written before
any code, `security-reviewer` on every mutation path, `protocol-guardian` on the
proto bump. It is the wave whose failures are silent and expensive, and the
budget saved in W1–W3 is what pays for it.

## THREE branches, not one

Seventeen issues under one branch would cost more than W1, W2 and W3 together,
and it would spend its single `just ci` on a diff nobody can hold in their head.
Worse, it would give one `security-reviewer` pass a diff spanning the journal,
the wire and four providers — which is how a review becomes a checklist.

Split by **what the reviewer has to be**, not by subsystem size:

| branch | issues | the pass it needs |
| --- | --- | --- |
| `debt/w4a-journal` | #160, #178, #179, #146, #186 | `security-reviewer`, and a plan doc: these five are ONE design |
| `debt/w4b-wire` | #170, #152, #195 | `protocol-guardian`, one version bump, one set of goldens |
| `debt/w4c-tasks-and-providers` | #171, #165, #176, #163, #168, #26, #190, #173, #196 | `rust-reviewer` per agent; `security-reviewer` only on #165 and #26 |

Three gate runs instead of one, which is the cost. What it buys: three reviews
each small enough to be a review, and a wire bump that can be reasoned about
without a journal migration in the same diff. Merge them in that order — `w4b`
should land on top of a journal whose ownership model is already settled.

**`w4a` is the one to write a plan for.** #160, #178 and #179 are the embedded
journal's ownership and failure model told three ways, and #186 is the same
story reaching the executor: a `DeleteTree` cancelled mid-tree that writes no
entry is hard rule 3 meeting hard rule 4. One design, not four patches.

## `w4a` — the embedded journal's ownership and failure model

| issue | crate(s) | what |
| --- | --- | --- |
| #160 | norte-core | a journal write that fails after a successful trash leaves the file moved and unrecorded |
| #178 | norte-core | an embedded session whose journal is corrupt or squatted runs unjournalled behind a warning |
| #179 | norte-core | the embedded journal's ownership window is all-or-nothing: no retry after a busy open, no release when idle |
| #146 | norte-core | anchor the journal's format marker, so a re-declaration is caught outright |
| #186 | norte-core | a `DeleteTree` cancelled mid-tree writes no journal entry, so a half-deleted subtree is lost silently |

#160, #178 and #179 are one story — the embedded journal's ownership and
failure model — and want one design, not three patches.

**#186 is hard rule 3 meeting hard rule 4**, and it is the worst failure in this
wave: a cancellation that is supposed to leave a clean destination instead
leaves a half-deleted subtree with nothing in the journal to undo it. It also
touches #176 (a `DeleteTree` revalidates the directory, not its content) —
same operation, both halves of "what does a `DeleteTree` actually promise".

## `w4b` — the wire, ONE bump for three fields

| issue | what |
| --- | --- |
| #170 | `SyncReportResult` carries no trash information, so a client that lost `sync.plan_done` cannot tell whether a batch is recoverable |
| #152 | `fs.compare`: two distinct files paired under an NFC singleton, with no marker on the wire |
| #195 | `SyncFailure` carries no `kind`, so a report row's anchor rests on an invariant the wire never states |

**THREE fields, ONE protocol version bump, one set of golden tests, one
`protocol-guardian` pass.**

**Correction from W4a: this is now the SECOND bump, not the first.** The split
was made by "what the reviewer has to be", and that put the wire in `w4b` — but
`w4a` needed `Error::JournalUnavailable` on the wire to refuse a mutation
against an unreadable journal, so it bumped 0.40.0 → 0.41.0 with its own golden
and its own `protocol-guardian` pass. `w4b` starts from 0.41.0. The split was
still right; what was wrong was assuming a non-wire branch could stay non-wire,
and the tell was there in the issue — a refusal a client has to understand is a
wire concern whatever branch it lands on. Bumping twice for two fields is the mistake this grouping exists to
prevent. An ADR if the pairing semantics change, not just the schema.

**Close:** `just ci` once **per branch**, at that branch's close — three runs,
which is what the split costs and what makes each review small enough to be one.

`w4a` and `w4b` also want the second, external pass: they are the journal and
the wire. `w4c` does not, except on #165 and #26.

## `w4c` — tasks, providers and the policy boundary

| issue | crate(s) | what |
| --- | --- | --- |
| #171 | norte-core | the undo gate parses `unit.len() * 2` VPaths on the caller's thread before the `Task` exists |
| #165 | norte-core, policy | the daemon state directory is 0700 but nothing excludes it from a policy scope over `$HOME` |
| --- | --- | --- |
| #176 | norte-sync | a `DeleteTree` revalidates the directory, not its content |
| #163 | norte-sync | destination name legality is not validated at planning time, only discovered at execution |
| #168 | norte-vfs-sftp, norte-vfs-object | the provider contract never runs with the logical trash enabled |
| #26 | norte-vfs-local | cross-device trash on freedesktop is an uncancellable copy+delete (hard rule 3) |
| #190 | norte-gui, norte-core | `compare`/`sync_plan`/`sync_apply` register a canceller with NO clean-cancellation test (hard rule 3) |
| #173 | norte-core, norte-tui | an applying sync is invisible to the task board: `TaskRef` is not `Clone`, so the board would take the only cancel handle |
| #196 | norte-frontend | a plan's steps are held unbounded in client memory |
| #155 | norte-core | `fs.compare`/`fs.search`: a client that does not drain is evicted from the subscriber map and loses the completeness signal — a `security-reviewer` MAJOR applied half way |
| #122 | norte-index, norte-ai | the M4-IA-2 bag, and its symlink-swap TOCTOU: a swapped indexed file gets its 32 KiB prefix sent to the embedding provider, against the module's own claim. Relative of #164. Also a vector-retention story |
