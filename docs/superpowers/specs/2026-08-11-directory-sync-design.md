# Directory synchronisation — design

**Date:** 2026-08-11
**Status:** approved
**Roadmap item:** 1 of `2026-08-07-post-alpha-roadmap.md`, spec 2 of 3
**Builds on:** `2026-08-11-directory-comparison-design.md` (spec 1), ADR 0048

## Why

Spec 1 answers "are these two trees the same?" and writes nothing. That is
half of what §17 asks for: the other half is acting on the answer — an
approved, journalled, undoable plan that makes the destination match the
source.

The gap is not the copying. `fs.copy`, the trash, the journal with `batch_id`,
the scheduler and the policy gate all exist. What is missing is **the plan as a
first-class wire type**: something a human can read before approving, an agent
can produce, and the core can execute as one unit it knows how to undo.

## Scope

In:

- One-way synchronisation, `source` → `dest`, in two modes: `Update` (copy
  what is missing and what differs) and `Mirror` (`Update`, plus delete what
  the destination has and the source does not).
- The plan as a streamed, cancellable task with a summary that carries a
  `plan_hash`, counters and blockers.
- The approved plan **retained server-side as a spool file**, so `sync.apply`
  carries nothing but its hash.
- Execution as a journalled batch with a partial-tolerant undo, and an honest
  per-step declaration of what can and cannot be reversed.
- The TUI surface: the plan opened from the diff pane, approved from a dialog.

Out, and deliberately so:

- **Two-way synchronisation.** A bidirectional plan proposes a direction from
  `newer`, and `newer` comes from mtime — `Probable` confidence. Deciding
  writes from `Probable` evidence is the part that deserves its own design,
  not a subsection of this one.
- **CLI, MCP and GUI** (spec 3). Filed as debt the day this lands, the way
  spec 1 filed #158; #147 is what happens otherwise.
- **Conflict resolution rules beyond `on_unknown`.** No per-pattern policies,
  no "keep both". A plan is approved or it is not.
- **Resuming an interrupted apply.** A cancelled apply leaves a closed,
  undoable batch; re-planning is how you continue. Resumption is a real
  feature and it is not this one.

## Approaches considered

**The planner inside `norte-compare`.** It already owns the walk, the pairing
key and the cascade, and planning is the same walk with a different decision
per row. Rejected: the crate's name would stop describing its contents, and
comparison would lose the boundary that makes it exhaustively testable on its
own.

**Everything in `norte-core::sync/`.** Fewest moving parts, planner and
executor sharing types. Rejected for the reason spec 1 gave when it put the
comparison engine in its own crate: a planner inside `norte-core` is only
testable through a daemon, and `norte-core` is already the largest crate in
the workspace.

**A new `norte-sync` crate above `norte-compare` — chosen.** The planner
becomes a transducer with almost no I/O, which is what makes the matrix of
cases (five step kinds × two modes × trash/no-trash × three confidences)
affordable to test exhaustively.

## Architecture

### Crates

**`norte-sync`** (new, via the `new-crate` skill) holds the planner. It depends
on `norte-compare` (the row stream), `norte-vfs` (`Capabilities`, `VPath`) and
`norte-proto` (step types), with `norte-testkit` as a dev dependency.

It is a **transducer**:

```
Stream<CompareRow> + Capabilities×2 + SyncOptions  →  Stream<SyncStep>
```

It touches no provider except one `Capabilities` read per side at start-up —
does the destination have a trash, is it case sensitive. Everything else is a
function of the rows.

**`norte-core::sync/`** owns the task, the spool, the policy gate, the executor
and the journal batch. **`norte-proto`** owns the wire types.
**`norte-frontend`** holds the presentation, **`norte-tui`** the keys and
painting.

### Params name sides, not hands

`sync.plan` takes `source` and `dest`, never `left` and `right`. Comparison is
symmetric and synchronisation is not; translating the direction once, in the
frontend that knows which pane the user was standing in, removes a whole class
of bug from everything downstream.

### One addition to `norte-compare`: `descend_orphans`

Today a directory that exists on one side only emits one row and is not
descended (spec 1: "push the common subdirectories"). The planner needs more
than that on the **source** side: a human approving a plan wants to know how
many files and how many bytes, and the executor needs a step per file to
journal per file and to isolate a failure to one file.

```rust
pub struct CompareOptions {
    // …spec 1…
    /// Descend into directories that exist on this side only. `None` — the
    /// default and spec 1's behaviour — emits one row for the orphan and does
    /// not walk it.
    pub descend_orphans: Option<Side>,
}
```

Additive, defaulted off, and it stands on its own as a comparison feature
("show me everything that is only on the left, not just the top of it").

The **destination** side is deliberately not descended. There an orphan is a
`DeleteTree`: one move to the trash, one journal entry, one thing to restore.
Splitting it into forty thousand steps makes the undo worse, not better, and
costs forty thousand listings to learn nothing the plan needed. The asymmetry
has that exact reason and this paragraph is where it is written down.

### Step types

```rust
pub enum SyncMode { Update, Mirror }
pub enum SyncStepKind { CreateDir, Copy, Overwrite, DeleteTree, Skip }
pub enum StepReversal { Delete, RestoreTrash, Irreversible }
pub enum OnUnknown { Copy, Skip }

pub struct SyncStep {
    /// Monotonic, plan-local. The pane's cursor anchors to it.
    pub id: u64,
    pub kind: SyncStepKind,
    /// Relative to the two roots. BYTES — rule 1.
    pub rel: VPath,
    /// Bytes this step moves, when known.
    pub size: Option<u64>,
    /// Why this step exists…
    pub criterion: CompareCriterion,
    /// …and what that reason is worth.
    pub confidence: CompareConfidence,
    /// `None` if and only if `kind` is `Skip` — a step that does nothing has
    /// nothing to reverse, and inventing a token for it would be a lie a
    /// consumer could act on.
    pub reversal: Option<StepReversal>,
    /// `Some` for exactly a `Skip` and an `Irreversible` step; `None`
    /// otherwise.
    pub reason: Option<SyncReason>,
}

pub enum SyncReason {
    /// Two source names collapsed to one pairing key.
    AmbiguousSource,
    /// `on_unknown: Skip` and the criterion earned `Unknown`.
    UnknownConfidence,
    /// The entry could not be read on the side that mattered.
    Unreadable,
    /// The destination has no trash, so the overwrite cannot be undone.
    NoTrashOnTarget,
}

pub enum SyncBlockerKind {
    /// Two destination names collapsed to one pairing key.
    AmbiguousDest,
    /// The walk reached the other root: the roots are the same tree.
    OverlapDetected,
    /// The destination provider cannot be written to.
    DestReadOnly,
    /// A directory over `COMPARE_MAX_DIR_ENTRIES` on the destination side.
    DirTooLarge,
    /// A directory on one side against a non-directory on the other.
    /// (Added while building: replacing a tree with a file, or a file with a
    /// tree, is a destructive structural change that deserves a human, and
    /// `Overwrite` normatively means "trash and copy bytes". It carries a
    /// `side` — the only blocker whose side is not implied by its kind — and
    /// it blocks on BOTH sides, which is the exception to the
    /// "source skips, destination blocks" rule this family otherwise follows.)
    TypeMismatchDir,
}
```

`criterion` and `confidence` travel **per step**, not per plan. That is the
standing obligation ADR 0048 created, discharged: the report can say "copied
because the mtime could not be read" instead of "copied", and a reviewer of a
sync that went wrong can see which rung authorised each write.

**Rows that decided `Same` produce nothing at all.** `Skip` is only for the
notable: `Ambiguous` on the source, `Unknown` under `on_unknown: Skip`, an
unreadable entry. Its volume is bounded by how strange the tree is, not by how
big it is.

### `Unknown` copies by default

`on_unknown: Copy` is the default. Faced with "the provider cannot say",
copying costs bandwidth and skipping costs silently stale data. The user can
ask for `Skip`, and either way the step records `confidence: Unknown` so the
report explains itself.

### `Ambiguous`: destination blocks, source skips

Two names that collapse to one pairing key are the collision ADR 0048 says a
later synchronisation must see before it writes anything. The two sides are
not the same problem:

- **On the destination** — writing there means writing over one of two files
  and not knowing which. Blocker; the plan is not executable.
- **On the source** — we do not know which of two files to copy. The entry is
  excluded as a `Skip` with reason `AmbiguousSource`, and the rest of the plan
  stands.

Only one of the two can lose data, and the plan treats them accordingly.

### Overlap

ADR 0048's inherited warning, discharged as far as it can be. **Corrected
2026-08-12**: as first written this section had two checks and credited the
second with catching three named cases it does not catch. The shipped shape is
three checks, and the residual is stated.

1. **Structural, before planning.** If one root contains the other, or they are
   equal, `Error::OverlappingRoots { relation: RootOverlap }` — three values
   (`same`, `source_inside_dest`, `dest_inside_source`), not a two-valued
   `Side`, because "these two folders are the same" is not a degenerate case of
   "one is inside the other" and is the sentence a frontend paints. `fs.compare`
   still permits the pair — comparing `/a` against `/a/sub` costs nothing but a
   walk. Planning a write into it does not have that licence. When **either**
   root's provider lacks `CASE_SENSITIVE`, containment case-**folds** before
   comparing (`fold_delta`, #129): on APFS and NTFS `/Data` against
   `/data/backup` is byte-distinct and byte-unnested, and would otherwise copy a
   tree into itself.
2. **By identity, before planning.** `Provider::node_id` on both roots with
   `FollowLinks::Yes`, compared only when the provider object is the same
   (`Arc::ptr_eq`); equal and `Some` is `RootOverlap::Same`. An error or a `None`
   is not an overlap. This is the check that closes a symlinked root and an
   archive opened by two paths — one `stat` per root, once per plan.
3. **During the walk.** If any row's absolute path on either side reaches the
   other root, that subtree is pruned and an `OverlapDetected` blocker is raised.
   **This is not the check that catches aliasing**, and the earlier claim that it
   was is wrong in all three of the cases it cited: walking `/data` — a symlink
   to `/srv/data` — against `/srv/data` produces rows that all hang from
   `/data`, so no row ever reaches the other root. What this layer really is, and
   is worth keeping as, is defence in depth against a provider that returns paths
   outside the root it was asked to list.

**Residual.** One SFTP or FTP host reached under two authorities is caught by
none of the three, because `node_id` is `None` on both providers. It is bounded
by the per-step revalidation and by two roots that really are one tree yielding
`Same` rows and no steps — a bound, not a refusal.

## Wire

Additive. No existing type changes shape. Minor bump to **0.40.0**, new
goldens, **ADR 0049**.

```rust
pub const SYNC_PLAN: &str      = "sync.plan";       // → FsTaskResult { task_id }
pub const SYNC_STEPS: &str     = "sync.steps";      // notification, batched
pub const SYNC_PLAN_DONE: &str = "sync.plan_done";  // notification, closes the plan
pub const SYNC_APPLY: &str     = "sync.apply";      // → FsTaskResult { task_id }
pub const SYNC_REPORT: &str    = "sync.report";     // twin of fs.rename_batch_report
pub const SYNC_STEPS_MAX_BATCH: usize = 256;        // mirrors COMPARE_ROWS_MAX_BATCH
pub const SYNC_PLAN_TTL_MS: u64 = 600_000;
pub const SYNC_MAX_BLOCKERS_REPORTED: usize = 256;
pub const SYNC_MAX_INCLUDE: usize = 4096;

pub struct SyncPlanParams {
    pub source: VPath,
    pub dest: VPath,
    pub mode: SyncMode,
    /// Reused from spec 1: criteria, mtime tolerance, depth. Two of its
    /// fields are NOT the caller's to set — see below. Named
    /// `SyncCompareOptions` as shipped: `CompareOptions` already belongs to
    /// `norte_compare`, and the TUI would have had both in one file.
    pub compare: SyncCompareOptions,
    pub on_unknown: OnUnknown,
    /// Relative paths the plan is restricted to; `None` means the whole tree.
    /// This is how the diff pane's first-class selection seeds a plan. More
    /// than `SYNC_MAX_INCLUDE` is a params error (`-32602`), not a truncation
    /// — the same rule `FS_RENAME_BATCH_MAX_PAIRS` follows, for the same
    /// reason: a silently shortened list plans a sync the user did not ask
    /// for. Shipped as `RelPath`, not `VPath`: it names entries relative to
    /// the roots, and it filters the plan's OUTPUT rather than its input.
    pub include: Option<Vec<RelPath>>,
}

pub struct SyncPlanDone {
    /// One connection can have two plans in flight, and the hash is unknown
    /// until this notification arrives — so the plan is identified by its task
    /// until then.
    pub task_id: TaskId,
    /// Reuses `PlanHash` from `fs.rename_batch_plan` unchanged.
    pub plan_hash: PlanHash,
    /// One count per `SyncStepKind`, plus the bytes the plan moves — which is
    /// a LOWER BOUND, not a total: steps whose size the provider did not give
    /// are counted in `unmeasured_steps` rather than summed as zero.
    pub counts: SyncCounts,
    pub blockers: Vec<SyncBlocker>,  // capped at SYNC_MAX_BLOCKERS_REPORTED
    pub blockers_total: u64,
    pub executable: bool,
    /// What the destination's trash can give back: `Restorable`, `Opaque`
    /// (buried, but not by a name the undo can use — macOS, Windows) or
    /// `Absent`. **This is what the approval dialog leads with**, not the
    /// count of irreversible steps: a copy-only plan against a trash-less
    /// destination has `irreversible == 0` and gives back nothing, and it is
    /// byte-identical on the wire to the same plan against a restorable trash.
    /// Added mid-branch; see ADR 0049.
    pub dest_trash: DestTrash,
}

pub struct SyncApplyParams { pub plan_hash: PlanHash }
```

`TaskKind` gains `SyncPlan` and `Sync`.

**Two fields of `CompareOptions` are the planner's, not the caller's.**
`descend_orphans` is fixed to the source side and `follow_symlinks` stays
false; a request that sets either is a params error rather than a value
quietly overwritten. Spec 1 already refuses `follow_symlinks` outright, and
letting a caller ask for orphan descent on the destination side would buy
40 000 listings that change no step.

**`Error::OverlappingRoots { relation: RootOverlap }` is a new variant** on the
protocol error enum — additive, with its own golden. The payload is the
three-valued `RootOverlap` and not a `Side`; the `inner: Side` this spec first
wrote could not describe three cases and would have made `Display` lie.
`Error::PlanStale` already exists and is reused unchanged.

**`sync.apply` carries nothing but the hash**, which is what makes "it executes
what was approved" an invariant rather than a promise: there is no second
parameter through which a different intention could arrive.

### Where `#[serde(other)]` goes, and where it must not

`SyncStepKind`, `SyncReason`, `StepReversal` and `SyncBlockerKind` travel
daemon→client and carry the fallback variant, as ADR 0048's four enums do: an
N+1 daemon that adds a step kind does not break an N−1 frontend.

`SyncMode` and `OnUnknown` travel client→daemon and carry **no** fallback. An
unrecognised mode dies in the deserialiser. Accepting an unknown mode by
default is accepting to delete by default.

All of them are `#[non_exhaustive]`, for the reason #126 already paid once.

### Invariants the wire cannot express and a golden can

- `reason` is `Some` for exactly a `Skip` and an `Irreversible` step, `None`
  otherwise.
- `reversal` is `None` if and only if `kind` is `Skip`.
- `DeleteTree` appears only under `Mirror`.
- `blockers` non-empty ⟹ `executable == false`.
- `executable == false` ⟹ `sync.apply` refuses even when the hash matches.
  The direction matters, as it does on `FsRenameBatchPlanResult::executable`:
  a future blocker with no name to list must still stop the plan.

## The retained plan

A plan over half a million files does not fit in a response, and re-deriving
it at apply time means walking both trees twice — six hours instead of three,
with the tree changing in the gap, so `PlanStale` might never converge on a
live tree.

So the plan is **retained**, and the mechanism is a **spool file**: written as
it is planned, streamed to the client at the same time, keyed by
`(connection, plan_hash)`.

Its lifetime is closed on all four sides:

- **Applied** — deleted when the apply task terminates, in any state.
- **TTL** — `SYNC_PLAN_TTL_MS`, ten minutes.
- **Connection closed** — the spool belongs to the connection that planned it.
- **Daemon start** — a sweep of the spool directory, because a crash leaves
  files behind and nobody else will collect them.

A hash that names no live spool is `Error::PlanStale`. As with rename batch,
a *malformed* hash is a params error and dies in the deserialiser: "this is
not a hash" and "the world moved" are different facts, and answering the
second to someone who sent the first lies to them about the state of the
world.

The spool is a file on disk that authorises writes. It is owner-only, lives
under the daemon's state directory, and holds no content — only paths,
sizes and verdicts.

## Journal and undo

**The journal does not change shape.** No new column, no new `Reversal`, no
new `op`. Overwriting *is* `trashed` + `created` under one `batch_id`:

| step | journal entries | reversal |
| --- | --- | --- |
| `CreateDir`, `Copy` | `created` | `Delete` |
| `Overwrite`, destination has a trash | `trashed` + `created` | `RestoreTrash` + `Delete` |
| `Overwrite`, destination has no trash | `created` | `Irreversible`, reason `NoTrashOnTarget` |
| `DeleteTree` | `trashed` (one, for the whole tree) | `RestoreTrash` |
| `Skip` | none | — |

Undo walks `seq` descending, so within the overwrite pair it deletes the
created file **before** restoring the buried one. The correct order falls out
of the existing mechanism rather than out of care taken here.

**Overwrite is trash-then-copy where a trash exists, and declared
`Irreversible` where it does not.** That is rule 4 applied step by step, and
the plan shows the count of irreversible steps *before* approval rather than
the report showing it after.

**`revert_sync_batch`, a sibling of `revert_batch`, not the same function.**
`fs.rename_batch`'s undo is all-or-nothing because half a permutation means
nothing. Half an undone sync means exactly what it says. So the sync path
reverts what it can, in reverse order, and names what it could not:
`UndoReport` gains `irreversible_skipped` and its capped list.
`undo_units`, which groups by `batch_id`, is already generic and is reused
unchanged.

## Execution

The executor streams the spool in plan order. The walk is pre-order, so
`CreateDir` precedes every `Copy` into it without a sort.

**Revalidation before every destructive step.** Up to ten minutes separate the
plan from its application. Before an `Overwrite` or a `DeleteTree`, one `stat`:
does the destination still look the way it looked when the plan was made? If
not, the step is not executed and appears in the report as `Conflict`. One
`stat` per destructive step is the only thing standing between the TTL and a
lost file.

**It has to be compared against something, and that something is not
`SyncStep::size`.** As first written this paragraph said "what the step
recorded", which does not exist: `size` on a step is normatively *the bytes
the step moves*, which is the **source's** size. Nothing on the step describes
the destination's prior state, so the check had nothing to compare and was
decorative. What the destination looked like travels instead as a
`DestWitness { kind, size, mtime_ms }`, recorded for `Overwrite` and
`DeleteTree` only, **in the spool and not on the wire** — it is not something
a client approves, and it is deliberately outside `plan_hash` so that a
re-plan of an unchanged tree still yields the same digest.

Its limits are stated rather than promised. A provider that lists without size
or mtime degrades the check to "still exists, still the same kind", and a
`DeleteTree` witnesses the directory, not its contents: a file changed three
levels down does not stop the delete. Both are honest degradations of a check
that is otherwise exact, and neither is a reason to skip it.

**Copying goes through the existing engine** in `norte-core::ops` —
`.norte-partial`, progress, the cancellation semantics already tested. Nothing
is reimplemented.

**A failure is a report row, not the end of the task.** A step that fails at
file 40 000 of 500 000 is recorded and the task continues, the way spec 1
made errors rows. `sync.report` — the twin of `fs.rename_batch_report` —
returns `done`, `failed`, `skipped`, `bytes`, a capped list of failures with
their cause, and the `batch_id` the undo needs.

**Cancellation.** Rule 3: the token is checked per step and per copied chunk.
What was applied stays journalled under its `batch_id` and is undoable —
which is precisely the "revert what you can" path, not a special case.

## Policy

- `sync.plan` requires **read** scope over both roots, and **content** scope
  when the hash criterion is enabled — it reads bytes.
- `sync.apply` requires **write** scope over `dest` and read over `source`.
- The spool is keyed to the connection that produced it, so **nobody applies a
  plan they did not produce** — not an agent a human's, not the reverse. This
  is a property of the retention mechanism, not a check that can be forgotten.

Agents may plan. Whether an agent may apply is the ordinary write-scope
question that `approval.rs` already answers; this spec adds no bypass.

## The pane

The plan opens from the diff pane spec 1 built.

- `SyncState { Planning, Ready, Applying, Done, Failed }` in `norte-tui::app`,
  presentation pure in `norte-frontend`, testable without a TTY.
- The **approval dialog** shows counts by step kind, total bytes, blockers,
  and — on its own line, not buried in a total — **how many steps are
  irreversible**.
- **`Mirror` asks a second time**, and the second question names how many trees
  it is about to delete.
- A plan that is not executable cannot be approved: the frontend disables
  confirmation on `!executable` and deduces nothing from the blocker list.
- The diff pane's first-class selection seeds `include`.
- `pane.sync-dirs` stops being greyed out (#134).

**The key is a plain letter inside the pane**, plus its catalogue entry with
`availability` and which-key, mapped by every preset. Not a modified function
key: #159 says none of them arrive under tmux, and shipping a documented dead
shortcut has already happened once.

User-facing strings through Fluent in `i18n/`, English and Spanish.

## Tests

- **`norte-sync`** — the transducer against synthetic row streams: every
  cascade rung × both modes × the capability matrix (trash / no trash) ×
  the three confidences. Properties: `Update` never emits `DeleteTree`;
  `Mirror` over two identical trees emits nothing; `Irreversible` appears if
  and only if the destination has no trash; every step's `rel` is a prefix-free
  relative path that never escapes the root.
- **`norte-compare`** — `descend_orphans` descends on the named side and not
  on the other; `None` reproduces spec 1's behaviour exactly.
- **`norte-proto`** — goldens for every new type, and the four invariants
  above.
- **`norte-core`** — spool lifetime on all four sides (apply, TTL, connection
  close, daemon-start sweep); `PlanStale` for an unknown hash and a params
  error for a malformed one; apply refused on `!executable`; revalidation
  catching a destination that changed under the plan; a failure at step N that
  does not kill the task; clean cancellation; the journal batch and a partial
  undo that reports its irreversible steps.
- **Cross-provider** — local → `norte-vfs-archive` (a read-only destination
  must raise a blocker, not attempt and fail), and local → a `MemProvider`
  without a trash, so the `Irreversible` path is exercised for real rather
  than simulated.
- **TUI** — pure presentation in `norte-frontend`; composition under the tmux
  harness, which is the only thing that surfaces painting bugs.

## Definition of done

Code, unit tests, cross-provider tests, rustdoc with doctests on the new public
items in `norte-proto` and `norte-sync`, ADR 0049, the 0.40.0 bump with its
goldens, Fluent strings in both locales, the keymap catalogue entry, #134
closed, and the spec 3 debt filed the same day — no CLI, no MCP, no GUI
surface for synchronisation. `just ci` green.
