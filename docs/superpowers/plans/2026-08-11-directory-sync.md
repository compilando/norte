# Directory Synchronisation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn spec 1's read-only comparison into an approved, journalled,
undoable one-way synchronisation — `Update` and `Mirror`, `source` → `dest`.

**Architecture:** A new `norte-sync` crate is a pure transducer over
`norte-compare`'s row stream (`Stream<CompareRow> + Capabilities×2 + options →
Stream<SyncStep>`), so the whole matrix of step kinds, modes, trash
availability and confidences is testable without a daemon. `norte-core::sync/`
owns the task, a **spool file** that retains the approved plan keyed to the
connection that produced it, the executor, and the journal batch.
`sync.apply` carries nothing but the `plan_hash`, so it cannot execute
anything other than what was approved.

**Tech Stack:** Rust, tokio, futures streams, `sqlx`/SQLite (journal, unchanged
schema), `serde`/`schemars` (wire), `nextest`, `proptest`.

**Spec:** `docs/superpowers/specs/2026-08-11-directory-sync-design.md`
**Inherited context:** `docs/superpowers/specs/2026-08-11-directory-comparison-design.md`, ADR 0048.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — the wire vocabulary | done | `c279988` |

### What Task 1 changed in this plan

`protocol-guardian` found three shapes wrong before they shipped. Later tasks
must follow the corrected ones, not the snippets as originally written:

- **`rel` is a `RelPath`, not a `VPath`.** `VPath` is always absolute and always
  carries a scheme, so a relative path would have invented one — and
  `plan_hash` covers `rel`, so a field that cannot be ignored cannot be
  hand-waved either. API: `RelPath::parse_wire(&str)`, `new(Vec<Segment>)`,
  `to_wire()`, `segments()`, `is_root()`, `Default` = root. Wire form is
  `"sub/informe%FF%FE.dat"`; `..`, `.`, `/`, NUL and `%2E%2E` die in the
  deserialiser on every peer. Snippets below that say `VPath` for a `rel`, or
  call `to_wire_bytes()` / `VPath::join_rel`, need adapting.
- **`Error::OverlappingRoots` carries `RootOverlap`, not `Side`** —
  `{ Same, SourceInsideDest, DestInsideSource, Unknown }`. Two values could not
  describe three cases and the `Display` lied. Task 8's assertions change
  accordingly: `source=/a, dest=/a/sub` is `DestInsideSource`, the reverse is
  `SourceInsideDest`, identical roots are `Same`.
- **The compare options on the sync wire are `SyncCompareOptions`**, not
  `CompareOptions` — the latter name already belongs to `norte_compare` and
  Task 13 would have had both in one file.

Also settled early: **ADR 0049 is already written** (Task 14 step 1 is done —
review and extend it, do not recreate it), and these types exist because later
tasks need them: `SyncStepsBatch { task_id, steps }`, `SyncPlanDone.task_id`
(one connection can have two plans in flight, and the hash is unknown until
this arrives), `SyncReportParams { task_id }`, `SYNC_MAX_FAILURES_REPORTED`.

`SyncCounts::bytes` is a straight sum of `SyncStep::size`, which is now
normatively **absent** on `Skip` and `DeleteTree`.

---

## Gate budget

Follow `CLAUDE.md`. Per-task loop: `just t <crate>` (plus `just c` when you
touched lint surface). `just ci-fast` **once** after tasks 6, 9 and 12.
`just ci` **once**, at task 14. Never re-run the gate to check whether a fix
worked — reproduce the single failure with `just t <crate>`.

## File structure

**Created**

| file | responsibility |
| --- | --- |
| `crates/norte-sync/Cargo.toml` | crate manifest (via the `new-crate` skill) |
| `crates/norte-sync/src/lib.rs` | `SyncOptions`, `SyncError`, re-exports, crate docs |
| `crates/norte-sync/src/plan.rs` | the transducer: rows → steps |
| `crates/norte-sync/src/hash.rs` | streaming `plan_hash` accumulator |
| `crates/norte-core/src/sync/mod.rs` | task body, batching, `SyncCounts` roll-up |
| `crates/norte-core/src/sync/spool.rs` | spool write/read, TTL, sweep |
| `crates/norte-core/src/sync/exec.rs` | the executor: revalidate, act, journal, report |
| `crates/norte-frontend/src/sync.rs` | `SyncState`, approval-dialog model, row rendering |
| `docs/adr/0049-the-retained-sync-plan.md` | ADR |

**Modified**

| file | change |
| --- | --- |
| `crates/norte-proto/src/methods.rs` | sync vocabulary, constants, params/results, `PROTOCOL_VERSION` |
| `crates/norte-proto/src/task.rs` | `TaskKind::SyncPlan`, `TaskKind::Sync` |
| `crates/norte-proto/src/error.rs` | `Error::OverlappingRoots` (`lib.rs` only re-exports) |
| `crates/norte-proto/tests/golden_types.rs` | goldens for every new type |
| `crates/norte-compare/src/lib.rs` | `CompareOptions::descend_orphans` |
| `crates/norte-compare/src/walk.rs` | honour it |
| `crates/norte-core/src/lib.rs` | `pub mod sync;` |
| `crates/norte-core/src/engine.rs` | `sync_plan_as`, `sync_apply_as` |
| `crates/norte-core/src/daemon/server.rs` | three handlers + the notification pump |
| `crates/norte-core/src/undo.rs` | `revert_sync_batch`, `UndoReport::irreversible_skipped` |
| `crates/norte-tui/src/*` | keys, painting, keymap catalogue |
| `i18n/en/*.ftl`, `i18n/es/*.ftl` | user-facing strings |

---

### Task 1: The wire vocabulary

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`
- Modify: `crates/norte-proto/src/task.rs` (`TaskKind`)
- Modify: `crates/norte-proto/src/lib.rs` (the `Error` enum)
- Test: `crates/norte-proto/tests/golden_types.rs`, `crates/norte-proto/tests/types.rs`

Read `CompareRow` and its four enums in `methods.rs` first (around line 2564):
every decision here — `#[non_exhaustive]`, `#[serde(other)]`, the invariant
helper, the rustdoc that states the normative rule — copies that family
deliberately. ADR 0048 is the reasoning.

- [ ] **Step 1: Write the failing invariant tests**

In `crates/norte-proto/tests/types.rs`:

```rust
use norte_proto::methods::{
    OnUnknown, StepReversal, SyncBlockerKind, SyncMode, SyncReason, SyncStep, SyncStepKind,
};
use norte_proto::{CompareConfidence, CompareCriterion, VPath};

fn step(kind: SyncStepKind, reversal: Option<StepReversal>, reason: Option<SyncReason>) -> SyncStep {
    SyncStep {
        id: 1,
        kind,
        rel: VPath::parse("file:///a/b.txt").expect("path"),
        size: Some(12),
        criterion: CompareCriterion::Size,
        confidence: CompareConfidence::Certain,
        reversal,
        reason,
    }
}

#[test]
fn a_skip_has_no_reversal_and_every_other_kind_has_one() {
    assert!(step(SyncStepKind::Skip, None, Some(SyncReason::AmbiguousSource)).shape_is_consistent());
    assert!(
        !step(SyncStepKind::Skip, Some(StepReversal::Delete), Some(SyncReason::AmbiguousSource))
            .shape_is_consistent(),
        "a step that does nothing cannot claim a reversal"
    );
    assert!(step(SyncStepKind::Copy, Some(StepReversal::Delete), None).shape_is_consistent());
    assert!(
        !step(SyncStepKind::Copy, None, None).shape_is_consistent(),
        "an acting step must say how it comes back"
    );
}

#[test]
fn reason_is_present_for_exactly_skip_and_irreversible() {
    assert!(step(SyncStepKind::Skip, None, Some(SyncReason::UnknownConfidence)).shape_is_consistent());
    assert!(!step(SyncStepKind::Skip, None, None).shape_is_consistent());
    assert!(
        step(
            SyncStepKind::Overwrite,
            Some(StepReversal::Irreversible),
            Some(SyncReason::NoTrashOnTarget)
        )
        .shape_is_consistent()
    );
    assert!(
        !step(SyncStepKind::Overwrite, Some(StepReversal::Irreversible), None)
            .shape_is_consistent(),
        "an irreversible step owes a reason"
    );
    assert!(
        !step(SyncStepKind::Copy, Some(StepReversal::Delete), Some(SyncReason::Unreadable))
            .shape_is_consistent(),
        "a reversible acting step has no reason to carry"
    );
}

#[test]
fn an_unknown_step_kind_degrades_instead_of_killing_the_batch() {
    // A daemon one version ahead adds a kind. The row still parses.
    let v = serde_json::json!({
        "id": 7, "kind": "teleport", "rel": "file:///a",
        "size": null, "criterion": "size", "confidence": "certain",
        "reversal": "delete", "reason": null
    });
    let s: SyncStep = serde_json::from_value(v).expect("degrades");
    assert_eq!(s.kind, SyncStepKind::Unknown);
}

#[test]
fn a_mode_this_daemon_does_not_know_is_refused_not_defaulted() {
    // Client→daemon: accepting an unknown mode by default is accepting to
    // delete by default.
    assert!(serde_json::from_value::<SyncMode>(serde_json::json!("obliterate")).is_err());
    assert!(serde_json::from_value::<OnUnknown>(serde_json::json!("maybe")).is_err());
}

#[test]
fn every_blocker_kind_round_trips() {
    for k in [
        SyncBlockerKind::AmbiguousDest,
        SyncBlockerKind::OverlapDetected,
        SyncBlockerKind::DestReadOnly,
        SyncBlockerKind::DirTooLarge,
    ] {
        let j = serde_json::to_value(k).expect("json");
        assert_eq!(serde_json::from_value::<SyncBlockerKind>(j).expect("back"), k);
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `just t norte-proto`
Expected: FAIL — `SyncStep` and friends do not exist.

- [ ] **Step 3: Add the vocabulary**

In `methods.rs`, next to the compare family:

```rust
/// `sync.plan` — plans a one-way synchronisation as a cancellable Task
/// (0.40.0). Answers [`FsTaskResult`]; the steps arrive as [`SYNC_STEPS`]
/// notifications and the plan closes with [`SYNC_PLAN_DONE`].
pub const SYNC_PLAN: &str = "sync.plan";
/// `sync.steps` — a bounded batch of [`SyncStep`], routed to the owner only.
pub const SYNC_STEPS: &str = "sync.steps";
/// `sync.plan_done` — closes a plan and carries its [`PlanHash`].
pub const SYNC_PLAN_DONE: &str = "sync.plan_done";
/// `sync.apply` — executes a retained plan. Carries NOTHING but the hash.
pub const SYNC_APPLY: &str = "sync.apply";
/// `sync.report` — the outcome of a [`SYNC_APPLY`] Task.
pub const SYNC_REPORT: &str = "sync.report";

pub const SYNC_STEPS_MAX_BATCH: usize = 256;
pub const SYNC_PLAN_TTL_MS: u64 = 600_000;
pub const SYNC_MAX_BLOCKERS_REPORTED: usize = 256;
pub const SYNC_MAX_INCLUDE: usize = 4096;
```

Then the types exactly as the spec's "Step types" and "Wire" sections give
them, plus the four the spec names without spelling out:

```rust
/// What a plan adds up to. The approval dialog leads with `irreversible`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCounts {
    pub create_dir: u64,
    pub copy: u64,
    pub overwrite: u64,
    pub delete_tree: u64,
    pub skip: u64,
    /// Steps whose `reversal` is [`StepReversal::Irreversible`]. Counted
    /// apart because it is the one number a human must not have to derive.
    pub irreversible: u64,
    /// Bytes the plan moves. A delete and a skip move none.
    pub bytes: u64,
}

/// Why a plan cannot run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBlocker {
    /// Relative to the roots. Segments of BYTES, never a String.
    pub rel: RelPath,
    pub kind: SyncBlockerKind,
    /// The side it happened on, when it happened on one.
    pub side: Option<Side>,
}

/// Result of [`SYNC_REPORT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReportResult {
    pub done: u64,
    pub failed: u64,
    pub skipped: u64,
    pub bytes: u64,
    /// Capped at [`SYNC_MAX_BLOCKERS_REPORTED`]; `failed` is not capped.
    pub failures: Vec<SyncFailure>,
    /// The journal batch, which is what an undo needs. `None` only when the
    /// apply died before it could allocate one.
    pub batch_id: Option<i64>,
}

/// One step that did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    pub rel: RelPath,
    pub cause: SyncFailureCause,
}

/// Why a step did not happen. Daemon→client: `#[serde(other)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SyncFailureCause {
    /// The destination stopped matching what the plan recorded. The
    /// revalidation `stat` caught it and nothing was written.
    Conflict,
    /// The provider refused the write.
    Denied,
    /// The read or the write broke.
    Io,
    #[serde(other)]
    Unknown,
}
```

Non-negotiable details:

- `SyncStepKind`, `SyncReason`, `StepReversal`, `SyncBlockerKind` are
  `#[non_exhaustive]` **and** carry `#[serde(other)] Unknown` — daemon→client.
- `SyncMode` and `OnUnknown` are `#[non_exhaustive]` and carry **no**
  `serde(other)` — client→daemon.
- `SyncStep::rel` is a `RelPath` and never a `String` (rule 1).
- `SyncPlanParams::include` gets
  `#[cfg_attr(feature = "schema", schemars(extend("maxItems" = SYNC_MAX_INCLUDE)))]`,
  the way `FsRenameBatchPlanParams::pairs` does.
- `SyncPlanDone::plan_hash` reuses `PlanHash` from the rename-batch family
  unchanged.

`shape_is_consistent()` is an inherent method on `SyncStep`, modelled on
`CompareRow::reason_is_consistent`: it **never** rejects at `Deserialize`
time, because a malformed step must degrade like a bad attribute cell rather
than kill a batch of 256.

```rust
impl SyncStep {
    /// Do `kind`, `reversal` and `reason` agree?
    ///
    /// The invariant the wire cannot express, stated once, here:
    /// `reversal` is `None` if and only if `kind` is [`SyncStepKind::Skip`],
    /// and `reason` is `Some` for exactly a `Skip` and a step whose reversal
    /// is [`StepReversal::Irreversible`].
    ///
    /// [`SyncStepKind::Unknown`] has no rule to break — a step from a daemon
    /// one version ahead is not something this client can judge, and claiming
    /// otherwise would make it distrust legitimate steps.
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool { /* … */ }
}
```

Add `TaskKind::SyncPlan` and `TaskKind::Sync` in `task.rs` (mind the
`#[serde(other)]` fallback already there — read the note about #126), and
`Error::OverlappingRoots { inner: Side }` to the protocol `Error` enum.
Bump `PROTOCOL_VERSION` to `"0.40.0"`.

- [ ] **Step 4: Run the tests**

Run: `just t norte-proto`
Expected: PASS for the five new tests. Existing golden tests will now FAIL on
`methods.json` and the version — that is Step 5.

- [ ] **Step 5: Regenerate and hand-check the goldens**

Add goldens in `golden_types.rs` for `sync_step` (one per kind, including a
`skip` and an `overwrite` that is irreversible), `sync_plan_params`,
`sync_plan_done`, `sync_apply_params`, `sync_report_result`, `sync_blocker`,
and the enum families. Follow the file's existing helper style
(`compare_row(...)` at line ~2520 is the model).

Run: `just t norte-proto`
Expected: PASS.

**Read the JSON diff of `methods.json` by eye before committing.** A golden
accepted without reading is a wire change nobody reviewed.

- [ ] **Step 6: Dispatch `protocol-guardian`**

Mandatory for any `norte-proto` change. Give it the commit range, tell it this
is spec 2 of roadmap item 1, and ask the two questions you actually cannot
settle alone: is the `serde(other)` asymmetry (present daemon→client, absent
client→daemon) right, and does `Error::OverlappingRoots` belong in the
protocol error enum rather than as an `INVALID_PARAMS` with a message?
Apply BLOCKER and MAJOR findings before committing; say which MINORs you
skipped.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-proto
git commit -m "feat(proto): the synchronisation plan on the wire (0.40.0)"
```

---

### Task 2: `descend_orphans` in the comparison engine

**Files:**
- Modify: `crates/norte-compare/src/lib.rs` (`CompareOptions`)
- Modify: `crates/norte-compare/src/walk.rs`
- Modify: `crates/norte-proto/src/methods.rs` (`FsCompareParams`)
- Modify: `crates/norte-core/src/daemon/server.rs` (`handle_fs_compare`)
- Test: `crates/norte-compare/src/walk.rs` (its `mod tests`)

**The trap Task 1 left you.** `Side` carries `#[serde(other)]`, so
`"descend_orphans": "lft"` deserialises to `Some(Side::Unknown)` and would
silently descend **neither** side — a different row set produced by a typo.
The deserialiser cannot catch this; `handle_fs_compare` must reject
`Some(Side::Unknown)` with `INVALID_PARAMS` explicitly, and a test must pin it:

```rust
#[tokio::test]
async fn a_misspelt_side_is_refused_and_not_read_as_neither() {
    let e = fs_compare_raw(serde_json::json!({
        "left": "file:///a", "right": "file:///b", "descend_orphans": "lft"
    })).await.expect_err("refused");
    assert_invalid_params(&e);
}
```

`SyncCompareOptions` already carries the field, so it and `FsCompareParams`
are divergent until this task lands. `protocol-guardian` deferred a test
pinning the two field sets against each other to this task — add it.

- [ ] **Step 1: Write the failing tests**

In `walk.rs`'s test module, next to the existing walk tests (reuse whatever
`MemProvider` fixture builder is already there rather than inventing one):

```rust
#[tokio::test]
async fn an_orphan_directory_is_one_row_by_default() {
    // left has  a/ (with a/1.txt, a/deep/2.txt); right has nothing.
    let (l, r) = orphan_tree_fixture();
    let rows = collect(compare(&l, &root(), &r, &root(), CompareOptions::cheap(), tok())).await;
    let only_left: Vec<_> = rows.iter().filter(|x| x.verdict == CompareVerdict::OnlyLeft).collect();
    assert_eq!(only_left.len(), 1, "spec 1: the orphan is not descended");
}

#[tokio::test]
async fn descend_orphans_left_enumerates_the_left_orphan_and_not_the_right_one() {
    let (l, r) = orphan_tree_fixture(); // right also has b/ with b/3.txt
    let opts = CompareOptions { descend_orphans: Some(Side::Left), ..CompareOptions::cheap() };
    let rows = collect(compare(&l, &root(), &r, &root(), opts, tok())).await;

    let left_names = names_of(&rows, CompareVerdict::OnlyLeft);
    assert!(left_names.contains(&"a".into()));
    assert!(left_names.contains(&"1.txt".into()));
    assert!(left_names.contains(&"2.txt".into()), "descends recursively");
    assert!(left_names.contains(&"deep".into()));

    let right_names = names_of(&rows, CompareVerdict::OnlyRight);
    assert_eq!(right_names, vec!["b".to_owned()], "the other side is untouched");
}

#[tokio::test]
async fn descending_an_orphan_still_respects_max_depth() {
    let (l, r) = orphan_tree_fixture();
    let opts = CompareOptions {
        descend_orphans: Some(Side::Left),
        max_depth: Some(1),
        ..CompareOptions::cheap()
    };
    let rows = collect(compare(&l, &root(), &r, &root(), opts, tok())).await;
    assert!(!names_of(&rows, CompareVerdict::OnlyLeft).contains(&"2.txt".into()));
}

#[tokio::test]
async fn descending_an_orphan_honours_cancellation() {
    let (l, r) = orphan_tree_fixture();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let opts = CompareOptions { descend_orphans: Some(Side::Left), ..CompareOptions::cheap() };
    let rows = collect_results(compare(&l, &root(), &r, &root(), opts, cancel)).await;
    assert!(matches!(rows.last(), Some(Err(CompareError::Cancelled))));
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `just t norte-compare`
Expected: FAIL — no field `descend_orphans`.

- [ ] **Step 3: Implement**

Add the field with the rustdoc from the spec's "One addition to
`norte-compare`" section, defaulted to `None`, and honour it in the walk: when
an orphan directory is emitted **and** its side matches, push it onto the same
explicit stack the paired directories use, listing only that side. Reuse the
existing `COMPARE_MAX_DIR_ENTRIES` guard and the existing per-directory
cancellation check — do not add a second code path for one-sided listing if
the walk already has one.

Add `descend_orphans: Option<Side>` to `FsCompareParams` too. In
`handle_fs_compare`, `fs.compare` accepts it as any other option; the sync
handler is what constrains it (Task 8).

- [ ] **Step 4: Run**

Run: `just t norte-compare`
Expected: PASS, all four.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-compare crates/norte-proto crates/norte-core
git commit -m "feat(compare): descend into a one-sided orphan on request"
```

---

### Task 3: `norte-sync` scaffold and the Update transducer

**Files:**
- Create: `crates/norte-sync/` (via the `new-crate` skill)
- Create: `crates/norte-sync/src/lib.rs`, `crates/norte-sync/src/plan.rs`
- Modify: `Cargo.toml` (workspace members), `ARCHITECTURE.md`

Use the `new-crate` skill — it wires the lints (`#![forbid(unsafe_code)]`,
`#![warn(missing_docs)]`), the licence header and the workspace membership.
Do not hand-roll the manifest.

Dependencies: `norte-compare`, `norte-vfs`, `norte-proto`, `futures`,
`thiserror`, `sha2`. Dev: `norte-testkit`, `tokio`, `proptest`.

- [ ] **Step 1: Define the crate's surface (no logic yet)**

`lib.rs`:

```rust
//! `norte-sync`: turns spec 1's comparison rows into a plan.
//!
//! A transducer, not a walker. It reads a `Stream<CompareRow>` and writes a
//! `Stream<SyncStep>`, and the only provider it ever touches is a
//! `Capabilities` read per side at start-up. That is what makes the matrix —
//! five step kinds × two modes × trash/no-trash × three confidences —
//! affordable to test exhaustively without a daemon.

/// Everything the transducer needs that the rows do not carry.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Where the bytes come from…
    pub source_root: VPath,
    /// …and where they go. `rel` on every step is relative to these two.
    pub dest_root: VPath,
    pub mode: SyncMode,
    pub on_unknown: OnUnknown,
    /// Which side of a `CompareRow` is the source. The frontend translated
    /// the user's direction once, here it is a fact.
    pub source_side: Side,
    /// Does the DESTINATION provider have a trash? Decides `StepReversal`
    /// on every overwrite and delete.
    pub dest_has_trash: bool,
    /// Can the destination be written to at all?
    pub dest_writable: bool,
}

/// What can end a plan early. Everything else is a step or a blocker.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    #[error("planificación cancelada")]
    Cancelled,
}

/// A plan is a stream of steps plus what it learned along the way.
pub fn plan<'a>(
    rows: impl Stream<Item = Result<CompareRow, CompareError>> + 'a,
    opts: SyncOptions,
    cancel: CancellationToken,
) -> impl Stream<Item = Result<PlanItem, SyncError>> + 'a;

/// What the stream carries: a step, or a reason the plan cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanItem {
    Step(SyncStep),
    Blocker(SyncBlocker),
}
```

- [ ] **Step 2: Write the failing Update tests**

`plan.rs`'s test module. Rows are built by hand — that is the whole point of
the transducer:

```rust
fn opts_update() -> SyncOptions { /* source=left, dest_has_trash=true, mode=Update */ }

fn row(verdict: CompareVerdict, criterion: CompareCriterion, conf: CompareConfidence,
       left: Option<Entry>, right: Option<Entry>) -> CompareRow { /* … */ }

#[tokio::test]
async fn only_on_the_source_becomes_a_copy() {
    let items = run(vec![row(CompareVerdict::OnlyLeft, CompareCriterion::Presence,
        CompareConfidence::Certain, Some(file("a.txt", 10)), None)], opts_update()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Copy);
    assert_eq!(s.rel, rel("a.txt"));
    assert_eq!(s.size, Some(10));
    assert_eq!(s.reversal, Some(StepReversal::Delete));
    assert_eq!(s.reason, None);
    assert_eq!(s.criterion, CompareCriterion::Presence);
}

#[tokio::test]
async fn a_directory_only_on_the_source_becomes_create_dir() {
    let items = run(vec![row(CompareVerdict::OnlyLeft, CompareCriterion::Presence,
        CompareConfidence::Certain, Some(dir("sub")), None)], opts_update()).await;
    assert_eq!(one_step(&items).kind, SyncStepKind::CreateDir);
}

#[tokio::test]
async fn different_becomes_overwrite_and_keeps_the_criterion_that_decided_it() {
    let items = run(vec![row(CompareVerdict::Different, CompareCriterion::Mtime,
        CompareConfidence::Probable, Some(file("a.txt", 10)), Some(file("a.txt", 9)))],
        opts_update()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(s.criterion, CompareCriterion::Mtime);
    assert_eq!(s.confidence, CompareConfidence::Probable,
        "the report has to be able to say WHY it overwrote");
}

#[tokio::test]
async fn same_produces_nothing_at_all() {
    let items = run(vec![row(CompareVerdict::Same, CompareCriterion::Hash,
        CompareConfidence::Certain, Some(file("a.txt", 10)), Some(file("a.txt", 10)))],
        opts_update()).await;
    assert!(items.is_empty(), "an identical tree must not produce a million no-ops");
}

#[tokio::test]
async fn only_on_the_destination_produces_nothing_under_update() {
    let items = run(vec![row(CompareVerdict::OnlyRight, CompareCriterion::Presence,
        CompareConfidence::Certain, None, Some(file("gone.txt", 3)))], opts_update()).await;
    assert!(items.is_empty(), "Update never deletes");
}

#[tokio::test]
async fn a_type_mismatch_overwrites_and_says_so() {
    let items = run(vec![row(CompareVerdict::TypeMismatch, CompareCriterion::Kind,
        CompareConfidence::Certain, Some(file("x", 1)), Some(dir("x")))], opts_update()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(s.criterion, CompareCriterion::Kind);
}

#[tokio::test]
async fn rel_is_relative_to_the_roots_and_keeps_its_bytes() {
    // A non-UTF-8 name from the hostile corpus survives the round trip.
    let raw = norte_testkit::hostile::NON_UTF8_NAME;
    let items = run(vec![row(CompareVerdict::OnlyLeft, CompareCriterion::Presence,
        CompareConfidence::Certain, Some(file_raw(raw, 1)), None)], opts_update()).await;
    assert_eq!(one_step(&items).rel.to_wire_bytes(), rel_bytes(raw));
}

#[tokio::test]
async fn cancellation_ends_the_stream_with_cancelled() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = run_with_cancel(vec![/* many rows */], opts_update(), cancel).await;
    assert!(matches!(out.last(), Some(Err(SyncError::Cancelled))));
}
```

- [ ] **Step 3: Run and watch them fail**

Run: `just t norte-sync`
Expected: FAIL — `plan` is unimplemented.

- [ ] **Step 4: Implement `Update`**

The mapping, and nothing beyond it yet:

| row verdict | step |
| --- | --- |
| `OnlyLeft`/`OnlyRight` on the **source** side, `EntryKind::Dir` | `CreateDir` |
| the same, any other kind | `Copy` |
| the same on the **destination** side | nothing (Task 5 adds `Mirror`) |
| `Different`, `TypeMismatch` | `Overwrite` |
| `Same` | nothing |
| everything else | Task 4 and Task 5 |

`rel` is the source entry's path with `source_root` stripped, as **bytes**.
Never `to_str()`. Cancellation is checked per row.

- [ ] **Step 5: Run**

Run: `just t norte-sync` — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-sync Cargo.toml ARCHITECTURE.md
git commit -m "feat(sync): the Update transducer, rows to steps"
```

---

### Task 4: Reversal from capabilities, `on_unknown`, and the `Skip` reasons

**Files:**
- Modify: `crates/norte-sync/src/plan.rs`
- Test: same file

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn an_overwrite_is_reversible_when_the_destination_has_a_trash() {
    let items = run(vec![different_row()], SyncOptions { dest_has_trash: true, ..opts_update() }).await;
    let s = one_step(&items);
    assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
    assert_eq!(s.reason, None);
}

#[tokio::test]
async fn an_overwrite_without_a_trash_is_irreversible_and_says_why() {
    let items = run(vec![different_row()], SyncOptions { dest_has_trash: false, ..opts_update() }).await;
    let s = one_step(&items);
    assert_eq!(s.reversal, Some(StepReversal::Irreversible));
    assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
}

#[tokio::test]
async fn a_plain_copy_is_reversible_even_without_a_trash() {
    // Nothing was destroyed: undo deletes what was created.
    let items = run(vec![only_left_row()], SyncOptions { dest_has_trash: false, ..opts_update() }).await;
    assert_eq!(one_step(&items).reversal, Some(StepReversal::Delete));
}

#[tokio::test]
async fn unknown_confidence_copies_by_default() {
    let items = run(vec![row(CompareVerdict::Same, CompareCriterion::Mtime,
        CompareConfidence::Unknown, Some(file("a", 1)), Some(file("a", 1)))], opts_update()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(s.confidence, CompareConfidence::Unknown,
        "the report must be able to say it copied because nobody could tell");
}

#[tokio::test]
async fn unknown_confidence_skips_when_asked_to() {
    let opts = SyncOptions { on_unknown: OnUnknown::Skip, ..opts_update() };
    let items = run(vec![row(CompareVerdict::Same, CompareCriterion::Mtime,
        CompareConfidence::Unknown, Some(file("a", 1)), Some(file("a", 1)))], opts).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Skip);
    assert_eq!(s.reversal, None);
    assert_eq!(s.reason, Some(SyncReason::UnknownConfidence));
}

#[tokio::test]
async fn an_error_row_becomes_a_skip_that_names_the_read_that_failed() {
    let items = run(vec![error_row(CompareReason::Unreadable)], opts_update()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::Skip);
    assert_eq!(s.reason, Some(SyncReason::Unreadable));
}

#[tokio::test]
async fn a_certain_same_is_never_a_skip_step() {
    // Bounded by strangeness, not by tree size: a million identical files
    // must produce zero items.
    let rows: Vec<_> = (0..1000).map(|i| same_row(i)).collect();
    assert!(run(rows, opts_update()).await.is_empty());
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-sync`

- [ ] **Step 3: Implement**

`StepReversal` is a function of `(kind, dest_has_trash)`:

| kind | trash | reversal | reason |
| --- | --- | --- | --- |
| `CreateDir`, `Copy` | either | `Delete` | `None` |
| `Overwrite` | yes | `RestoreTrash` | `None` |
| `Overwrite` | no | `Irreversible` | `NoTrashOnTarget` |
| `Skip` | — | `None` | the skip's reason |

`Unknown` confidence is evaluated **before** the verdict decides: a
`Same`/`Unknown` under `OnUnknown::Copy` becomes an `Overwrite`, under
`OnUnknown::Skip` a `Skip`. A `Different`/`Unknown` is an `Overwrite` either
way — `on_unknown` breaks the tie on "looks the same but nobody can promise
it", not on "is different".

- [ ] **Step 4: Run** — `just t norte-sync`, expected PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-sync
git commit -m "feat(sync): what a step can promise to undo, and what it cannot"
```

---

### Task 5: `Mirror`, `Ambiguous`, blockers and the overlap guard

**Files:**
- Modify: `crates/norte-sync/src/plan.rs`
- Test: same file

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn mirror_turns_a_destination_orphan_into_one_delete_tree() {
    let items = run(vec![row(CompareVerdict::OnlyRight, CompareCriterion::Presence,
        CompareConfidence::Certain, None, Some(dir("stale")))], opts_mirror()).await;
    let s = one_step(&items);
    assert_eq!(s.kind, SyncStepKind::DeleteTree);
    assert_eq!(s.rel, rel("stale"));
    assert_eq!(s.reversal, Some(StepReversal::RestoreTrash));
}

#[tokio::test]
async fn a_delete_without_a_trash_is_irreversible_and_says_why() {
    let opts = SyncOptions { dest_has_trash: false, ..opts_mirror() };
    let items = run(vec![only_right_row()], opts).await;
    let s = one_step(&items);
    assert_eq!(s.reversal, Some(StepReversal::Irreversible));
    assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
}

#[tokio::test]
async fn update_never_emits_a_delete_tree() {
    let rows = every_verdict_once();
    let items = run(rows, opts_update()).await;
    assert!(items.iter().all(|i| step_kind(i) != Some(SyncStepKind::DeleteTree)));
}

#[tokio::test]
async fn an_ambiguous_source_is_skipped_and_the_rest_of_the_plan_stands() {
    let items = run(vec![
        ambiguous_row(Side::Left, CompareReason::CaseFold),
        only_left_row(),
    ], opts_update()).await;
    let steps = steps_of(&items);
    assert_eq!(steps[0].kind, SyncStepKind::Skip);
    assert_eq!(steps[0].reason, Some(SyncReason::AmbiguousSource));
    assert_eq!(steps[1].kind, SyncStepKind::Copy, "one collision does not stop the plan");
    assert!(blockers_of(&items).is_empty());
}

#[tokio::test]
async fn an_ambiguous_destination_blocks_the_plan() {
    let items = run(vec![ambiguous_row(Side::Right, CompareReason::Normalization)],
        opts_update()).await;
    let b = one_blocker(&items);
    assert_eq!(b.kind, SyncBlockerKind::AmbiguousDest);
    assert_eq!(b.rel, rel("README"));
}

#[tokio::test]
async fn a_read_only_destination_blocks_before_a_single_step() {
    let opts = SyncOptions { dest_writable: false, ..opts_update() };
    let items = run(vec![only_left_row()], opts).await;
    assert_eq!(one_blocker(&items).kind, SyncBlockerKind::DestReadOnly);
    assert!(steps_of(&items).is_empty(), "do not plan writes into a tree that refuses them");
}

#[tokio::test]
async fn a_destination_directory_over_the_entry_limit_blocks() {
    let items = run(vec![error_row_with(CompareReason::DirTooLarge, Side::Right)],
        opts_update()).await;
    assert_eq!(one_blocker(&items).kind, SyncBlockerKind::DirTooLarge);
}

#[tokio::test]
async fn reaching_the_other_root_prunes_and_blocks() {
    // /a against /a/sub: two unequal VPaths naming one tree. The structural
    // check in the daemon can be defeated by a symlink; this one cannot.
    let opts = SyncOptions {
        source_root: vpath("file:///a"),
        dest_root: vpath("file:///a/sub"),
        ..opts_update()
    };
    let items = run(vec![
        only_left_row_at("file:///a/sub"),   // the walk reached the destination root
        only_left_row_at("file:///a/sub/x"), // …and everything under it
    ], opts).await;
    assert_eq!(one_blocker(&items).kind, SyncBlockerKind::OverlapDetected);
    assert!(steps_of(&items).is_empty(), "the subtree is pruned, not copied into itself");
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-sync`

- [ ] **Step 3: Implement**

- `Mirror` adds exactly one rule to Task 3's table: a destination-side orphan
  becomes one `DeleteTree`. It is **not** descended, and the reason is in the
  spec: one move to the trash, one journal entry, one thing to restore.
- `Ambiguous` splits on `CompareRow::side`: the source side is a `Skip` with
  `AmbiguousSource`; the destination side is a `Blocker`.
- `dest_writable == false` emits one `DestReadOnly` blocker and no steps at
  all.
- The overlap guard keeps one `VPath` of state: once a row's absolute path on
  either side is at or under `dest_root` while walking `source_root` (or the
  mirror image), raise `OverlapDetected` once and swallow every subsequent row
  under that prefix. Pre-order guarantees the parent arrives first, so one
  prefix is enough.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-sync
git commit -m "feat(sync): Mirror, the collisions that block, and the overlap the walk finds"
```

---

### Task 6: The streaming `plan_hash` and the counters

**Files:**
- Create: `crates/norte-sync/src/hash.rs`
- Modify: `crates/norte-sync/src/lib.rs`
- Test: `crates/norte-sync/src/hash.rs`, plus a property test in
  `crates/norte-sync/tests/props.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_hash_covers_the_conclusions_and_not_the_ids() {
    // `id` is presentation. Two plans that do the same thing hash the same,
    // even if a filter renumbered the pane.
    let mut a = PlanHasher::new(&opts_update());
    let mut b = PlanHasher::new(&opts_update());
    a.step(&step_with_id(1));
    b.step(&step_with_id(99));
    assert_eq!(a.finish(), b.finish());
}

#[test]
fn changing_a_step_changes_the_hash() {
    let mut a = PlanHasher::new(&opts_update());
    a.step(&copy_step("a.txt", 10));
    let mut b = PlanHasher::new(&opts_update());
    b.step(&copy_step("a.txt", 11));
    assert_ne!(a.finish(), b.finish(), "size is a conclusion");
}

#[test]
fn changing_the_mode_changes_the_hash_with_identical_steps() {
    let mut a = PlanHasher::new(&opts_update());
    let mut b = PlanHasher::new(&opts_mirror());
    a.step(&copy_step("a.txt", 10));
    b.step(&copy_step("a.txt", 10));
    assert_ne!(a.finish(), b.finish());
}

#[test]
fn order_is_part_of_the_plan() {
    let mut a = PlanHasher::new(&opts_update());
    a.step(&copy_step("a", 1)); a.step(&copy_step("b", 2));
    let mut b = PlanHasher::new(&opts_update());
    b.step(&copy_step("b", 2)); b.step(&copy_step("a", 1));
    assert_ne!(a.finish(), b.finish(), "CreateDir before Copy is a conclusion too");
}

#[test]
fn a_blocker_is_in_the_hash() {
    // Approving a blocked plan and approving an unblocked one are different
    // acts, even when the steps match.
    let mut a = PlanHasher::new(&opts_update());
    a.step(&copy_step("a", 1));
    let mut b = PlanHasher::new(&opts_update());
    b.step(&copy_step("a", 1));
    b.blocker(&blocker(SyncBlockerKind::AmbiguousDest, "README"));
    assert_ne!(a.finish(), b.finish());
}

#[test]
fn the_hash_is_lowercase_hex_of_the_documented_length() {
    let h = PlanHasher::new(&opts_update()).finish();
    assert_eq!(h.as_str().len(), norte_proto::methods::PLAN_HASH_LEN);
    assert!(h.as_str().chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
}
```

And the counters:

```rust
#[test]
fn counts_add_up_per_kind_and_bytes_only_count_what_moves() {
    let mut c = SyncCounts::default();
    c.add(&copy_step("a", 10));
    c.add(&overwrite_step("b", 20));
    c.add(&delete_tree_step("c"));
    c.add(&skip_step("d"));
    assert_eq!(c.copy, 1);
    assert_eq!(c.overwrite, 1);
    assert_eq!(c.delete_tree, 1);
    assert_eq!(c.skip, 1);
    assert_eq!(c.bytes, 30, "a delete and a skip move no bytes");
}

#[test]
fn irreversible_steps_are_counted_separately_because_the_dialog_leads_with_them() {
    let mut c = SyncCounts::default();
    c.add(&irreversible_overwrite_step("a", 5));
    c.add(&copy_step("b", 5));
    assert_eq!(c.irreversible, 1);
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-sync`

- [ ] **Step 3: Implement**

`PlanHasher` is a `sha2::Sha256` fed length-prefixed fields, never
concatenated strings (`"a" + "bc"` and `"ab" + "c"` must not collide). It is
seeded with the plan's intention — both roots as wire bytes, mode,
`on_unknown`, and the `CompareOptions` — then each step's conclusions (kind,
`rel` bytes, size, criterion, confidence, reversal, reason) and each blocker.
`id` is excluded, with a comment saying why.

`SyncCounts` lives in `norte-proto` (it is on the wire) and gains an `add`
helper here or there — put it where the type is.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: Add the property tests**

`crates/norte-sync/tests/props.rs`:

```rust
/// Rows that all decided `Same` with `Certain`, at arbitrary paths.
fn same_rows_strategy() -> impl Strategy<Value = Vec<CompareRow>> { /* names × sizes */ }
/// Rows across every verdict, criterion and confidence.
fn any_rows_strategy() -> impl Strategy<Value = Vec<CompareRow>> { /* … */ }

proptest! {
    /// The plan of A against A is empty, whatever the mode.
    #[test]
    fn an_identical_tree_plans_nothing(rows in same_rows_strategy(), mirror in any::<bool>()) {
        let opts = if mirror { opts_mirror() } else { opts_update() };
        let items = block_on(run(rows, opts));
        prop_assert!(items.is_empty());
    }

    /// Update never deletes. This is the property the whole mode exists for.
    #[test]
    fn update_never_deletes(rows in any_rows_strategy()) {
        for i in block_on(run(rows, opts_update())) {
            if let PlanItem::Step(s) = i {
                prop_assert_ne!(s.kind, SyncStepKind::DeleteTree);
            }
        }
    }

    /// Every `rel` stays inside the roots — no `..`, no absolute escape.
    #[test]
    fn rel_never_escapes(rows in any_rows_strategy(), mirror in any::<bool>()) {
        let opts = if mirror { opts_mirror() } else { opts_update() };
        for i in block_on(run(rows, opts)) {
            if let PlanItem::Step(s) = i {
                let joined = opts_dest_root().join_rel(&s.rel).expect("joins");
                prop_assert!(joined.starts_with(&opts_dest_root()),
                    "rel escaped the destination root: {:?}", s.rel);
            }
        }
    }

    /// `Irreversible` appears if and only if the destination has no trash.
    #[test]
    fn irreversible_iff_no_trash(rows in any_rows_strategy(), trash in any::<bool>()) {
        let opts = SyncOptions { dest_has_trash: trash, ..opts_mirror() };
        for i in block_on(run(rows, opts)) {
            if let PlanItem::Step(s) = i {
                let destructive =
                    matches!(s.kind, SyncStepKind::Overwrite | SyncStepKind::DeleteTree);
                let irreversible = s.reversal == Some(StepReversal::Irreversible);
                prop_assert_eq!(irreversible, destructive && !trash);
            }
        }
    }
}
```

- [ ] **Step 6: Run and commit**

Run: `just t norte-sync` then `just c`
Expected: PASS, no clippy warnings.

```bash
git add crates/norte-sync
git commit -m "feat(sync): the plan hash covers conclusions, not presentation"
```

- [ ] **Step 7: `just ci-fast` — run #1 of three**

Run: `just ci-fast` (~4 min). Fix anything red with `just t <crate>`, not by
re-running the gate.

---

### Task 7: The spool

**Files:**
- Create: `crates/norte-core/src/sync/spool.rs`
- Create: `crates/norte-core/src/sync/mod.rs`
- Modify: `crates/norte-core/src/lib.rs` (`pub mod sync;`)
- Test: `crates/norte-core/src/sync/spool.rs` (its `mod tests`)

The spool lives in `norte_core::connect::config_dir().join("sync-spools")`
— the same directory as `journal.db` and `policy.toml`. It is created
owner-only (`0o700` on unix), and each spool file is `0o600`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_written_plan_reads_back_step_for_step() {
    let dir = tempdir();
    let mut w = Spool::create(dir.path(), conn(1), hash("aa")).await.expect("create");
    for s in &steps_fixture() { w.push(s).await.expect("push"); }
    w.finish().await.expect("finish");

    let read: Vec<SyncStep> = Spool::open(dir.path(), conn(1), hash("aa"))
        .await.expect("open").steps().try_collect().await.expect("steps");
    assert_eq!(read, steps_fixture());
}

#[tokio::test]
async fn another_connection_cannot_open_it() {
    let dir = tempdir();
    write_plan(&dir, conn(1), hash("aa")).await;
    assert!(matches!(
        Spool::open(dir.path(), conn(2), hash("aa")).await,
        Err(SpoolError::NotFound)
    ), "nobody applies a plan they did not produce");
}

#[tokio::test]
async fn an_unfinished_spool_cannot_be_opened() {
    // A crash mid-plan must not leave a half plan that looks approvable.
    let dir = tempdir();
    let mut w = Spool::create(dir.path(), conn(1), hash("aa")).await.expect("create");
    w.push(&copy_step("a", 1)).await.expect("push");
    drop(w); // no finish()
    assert!(matches!(Spool::open(dir.path(), conn(1), hash("aa")).await, Err(SpoolError::NotFound)));
}

#[tokio::test]
async fn a_plan_past_its_ttl_is_gone() {
    let dir = tempdir();
    write_plan_with_mtime(&dir, conn(1), hash("aa"), now_ms() - SYNC_PLAN_TTL_MS as i64 - 1).await;
    assert!(matches!(Spool::open(dir.path(), conn(1), hash("aa")).await, Err(SpoolError::Expired)));
}

#[tokio::test]
async fn closing_a_connection_drops_its_plans_and_only_its_plans() {
    let dir = tempdir();
    write_plan(&dir, conn(1), hash("aa")).await;
    write_plan(&dir, conn(2), hash("bb")).await;
    Spool::drop_connection(dir.path(), conn(1)).await.expect("drop");
    assert!(Spool::open(dir.path(), conn(1), hash("aa")).await.is_err());
    assert!(Spool::open(dir.path(), conn(2), hash("bb")).await.is_ok());
}

#[tokio::test]
async fn the_startup_sweep_collects_what_a_crash_left_behind() {
    let dir = tempdir();
    write_plan_with_mtime(&dir, conn(1), hash("aa"), now_ms() - SYNC_PLAN_TTL_MS as i64 - 1).await;
    write_plan(&dir, conn(2), hash("bb")).await;
    let swept = Spool::sweep(dir.path()).await.expect("sweep");
    assert_eq!(swept, 1);
    assert!(Spool::open(dir.path(), conn(2), hash("bb")).await.is_ok());
}

#[tokio::test]
async fn a_spool_holds_no_content_only_paths_and_verdicts() {
    // A file on disk that authorises writes must not also BE the data.
    let dir = tempdir();
    write_plan(&dir, conn(1), hash("aa")).await;
    let bytes = read_the_only_file(&dir).await;
    assert!(!bytes.windows(SECRET.len()).any(|w| w == SECRET));
}

#[cfg(unix)]
#[tokio::test]
async fn the_spool_directory_and_its_files_are_owner_only() {
    let dir = tempdir();
    write_plan(&dir, conn(1), hash("aa")).await;
    assert_eq!(mode_of(dir.path().join("sync-spools")) & 0o777, 0o700);
    assert_eq!(mode_of(the_only_file(&dir)) & 0o777, 0o600);
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-core`

- [ ] **Step 3: Implement**

- One file per plan, named `<conn_id>-<plan_hash>.jsonl`. The connection id is
  **in the name**, which is what makes "another connection cannot open it" a
  property of the lookup rather than a check someone can forget.
- Steps are written one JSON object per line as they are planned, so memory is
  O(1) in the size of the plan.
- The last line is a terminator record carrying the counts and blockers. A
  file without it is treated as absent: a crash mid-plan must not leave
  something that looks approvable.
- `open` checks the terminator and the mtime against `SYNC_PLAN_TTL_MS`, and
  deletes an expired file as it finds it.
- `Spool::sweep` runs at daemon start-up. Wire it where the journal is opened.

- [ ] **Step 4: Run** — `just t norte-core`, expected PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): the approved plan is a spool file with four ways to die"
```

---

### Task 8: `sync.plan` as a task

**Files:**
- Modify: `crates/norte-core/src/sync/mod.rs`
- Modify: `crates/norte-core/src/engine.rs` (`sync_plan_as`)
- Modify: `crates/norte-core/src/daemon/server.rs` (`handle_sync_plan` + the pump)
- Test: `crates/norte-core/tests/` (follow whatever integration test file the
  compare task uses; mirror it)

`crates/norte-core/src/compare.rs` is the model for the whole task body —
batching, `FLUSH_INTERVAL`, the `FlushOutcome` enum, the progress contract, the
cancellation `select!`. Read it before writing a line, and reuse its shape
rather than inventing a second one.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn steps_arrive_in_bounded_batches() {
    let (rows_out, _done) = plan_against(big_tree_fixture()).await;
    assert!(rows_out.iter().all(|b| b.steps.len() <= SYNC_STEPS_MAX_BATCH));
}

#[tokio::test]
async fn the_plan_closes_with_a_hash_counts_and_executable() {
    let (_steps, done) = plan_against(simple_fixture()).await;
    assert!(done.executable);
    assert!(done.blockers.is_empty());
    assert_eq!(done.counts.copy, 2);
    assert_eq!(done.plan_hash.as_str().len(), PLAN_HASH_LEN);
}

#[tokio::test]
async fn a_blocker_makes_the_plan_not_executable() {
    let (_steps, done) = plan_against(ambiguous_dest_fixture()).await;
    assert!(!done.executable);
    assert_eq!(done.blockers.len(), 1);
    assert_eq!(done.blockers_total, 1);
}

#[tokio::test]
async fn blockers_are_capped_but_the_total_is_not() {
    let (_steps, done) = plan_against(fixture_with_blockers(SYNC_MAX_BLOCKERS_REPORTED + 10)).await;
    assert_eq!(done.blockers.len(), SYNC_MAX_BLOCKERS_REPORTED);
    assert_eq!(done.blockers_total, (SYNC_MAX_BLOCKERS_REPORTED + 10) as u64);
}

#[tokio::test]
async fn overlapping_roots_are_refused_before_anything_walks() {
    let e = plan_call(vpath("file:///a"), vpath("file:///a/sub")).await.expect_err("refused");
    assert_matches_overlapping_roots(&e, RootOverlap::DestInsideSource);
    let e = plan_call(vpath("file:///a/sub"), vpath("file:///a")).await.expect_err("refused");
    assert_matches_overlapping_roots(&e, RootOverlap::SourceInsideDest);
}

#[tokio::test]
async fn identical_roots_say_so_rather_than_naming_a_side() {
    let e = plan_call(vpath("file:///a"), vpath("file:///a")).await.expect_err("refused");
    assert_matches_overlapping_roots(&e, RootOverlap::Same);
}

#[tokio::test]
async fn the_caller_may_not_set_the_planners_own_compare_options() {
    let mut p = plan_params();
    p.compare.descend_orphans = Some(Side::Right);
    assert_invalid_params(sync_plan(p).await);
    let mut p = plan_params();
    p.compare.follow_symlinks = true;
    assert_invalid_params(sync_plan(p).await);
}

#[tokio::test]
async fn an_include_list_over_the_cap_is_refused_not_truncated() {
    let mut p = plan_params();
    p.include = Some(vec![rel("x"); SYNC_MAX_INCLUDE + 1]);
    assert_invalid_params(sync_plan(p).await);
}

#[tokio::test]
async fn planning_without_read_scope_over_either_root_is_denied() { /* both directions */ }

#[tokio::test]
async fn planning_with_the_hash_criterion_needs_content_scope() { /* … */ }

#[tokio::test]
async fn cancelling_a_plan_leaves_no_spool_behind() {
    let (task, dir) = start_plan_against(big_tree_fixture()).await;
    task.cancel();
    task.await_terminal().await;
    assert_eq!(count_files(&dir), 0, "a cancelled plan is not an approvable one");
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-core`

- [ ] **Step 3: Implement**

`handle_sync_plan`, following `handle_fs_compare` (server.rs ~2961) exactly:

1. `parse_params`.
2. **Gates first**, before validating params — an actor without rights over
   the roots does not get to learn whether their request was also malformed.
   `read_gate` on both roots; `content_gate` on both when
   `p.compare.criteria.hash`.
3. Refuse `source == dest` **and** either containing the other with
   `Error::OverlappingRoots { inner }`.
4. Refuse a caller-set `descend_orphans` or `follow_symlinks` with
   `INVALID_PARAMS`, and `include.len() > SYNC_MAX_INCLUDE` likewise.
5. `engine.sync_plan_as(...)`, then `register_task_id` with **zero `.await`
   between them** — invariant #64, and the comment in `handle_fs_compare`
   explains why.
6. Spawn the notification pump: `sync.steps` to the owner connection only,
   then one `sync.plan_done`. The pump stops the moment a batch is not
   delivered, exactly as the compare pump does, and for the same reason.

The task body sets `descend_orphans` to the source side itself, drives
`norte_compare::compare` into `norte_sync::plan`, tees every step into the
spool **and** the batch, and finishes the spool before emitting
`sync.plan_done`. A cancelled task deletes its spool.

- [ ] **Step 4: Run** — `just t norte-core`, expected PASS.

- [ ] **Step 5: Dispatch `protocol-guardian` and `security-reviewer`**

`protocol-guardian` for the handler and the notification contract;
`security-reviewer` for the gate order, the spool's connection binding, and
what a spool file discloses to someone who can read the state directory.
Tell them what you chose and what you are unsure about, not "review this
diff". Apply BLOCKER and MAJOR findings.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): sync.plan as a task behind the read gate"
```

---

### Task 9: The executor

**Files:**
- Create: `crates/norte-core/src/sync/exec.rs`
- Modify: `crates/norte-core/src/engine.rs` (`sync_apply_as`)
- Test: `crates/norte-core/src/sync/exec.rs` and the integration file

`crates/norte-core/src/rename/exec.rs` is the model: one task executing many
steps, a `StepJournal` trait with a `BatchJournal` implementation sharing one
`batch_id`, a `BatchReport` behind a `Mutex`, cancellation checked **between**
steps. Read it first. Do **not** submit one `Engine::copy_with_as` task per
step — that would be half a million tasks; call the provider-level operations
inside this one task, the way rename's `run` does.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_copy_lands_and_is_journalled_as_created() {
    let (rep, journal) = apply(plan_with(vec![copy_step("a.txt", 10)])).await;
    assert_eq!(rep.done, 1);
    assert_eq!(dest_bytes("a.txt").await, source_bytes("a.txt").await);
    let e = last_entry(&journal).await;
    assert_eq!(e.op, "created");
    assert_eq!(e.reversal, "delete");
    assert!(e.batch_id.is_some());
}

#[tokio::test]
async fn an_overwrite_with_a_trash_is_two_entries_in_one_batch() {
    let (rep, journal) = apply(plan_with(vec![overwrite_step("a.txt", 10)])).await;
    assert_eq!(rep.done, 1);
    let es = last_entries(&journal, 2).await;
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].reversal, "restore_trash");
    assert!(es[0].reversal_ref.is_some(), "undo needs to know WHERE it was buried");
    assert_eq!(es[1].op, "created");
    assert_eq!(es[0].batch_id, es[1].batch_id, "one undoable unit");
    assert!(es[1].seq > es[0].seq, "reverse-seq undo deletes before it restores");
}

#[tokio::test]
async fn an_overwrite_without_a_trash_is_one_irreversible_entry() {
    let (_rep, journal) = apply_on_trashless_dest(plan_with(vec![overwrite_step("a.txt", 10)])).await;
    let e = last_entry(&journal).await;
    assert_eq!(e.reversal, "irreversible");
}

#[tokio::test]
async fn a_delete_tree_is_one_move_to_the_trash() {
    let (rep, journal) = apply(plan_with(vec![delete_tree_step("stale")])).await;
    assert_eq!(rep.done, 1);
    assert!(!dest_exists("stale").await);
    let es = last_entries(&journal, 1).await;
    assert_eq!(es[0].op, "trashed");
}

#[tokio::test]
async fn a_destination_that_changed_under_the_plan_is_a_conflict_not_a_write() {
    // The whole reason the revalidation stat exists.
    let plan = plan_with(vec![overwrite_step("a.txt", 10)]);
    mutate_dest_after_planning("a.txt", b"someone else got here first").await;
    let (rep, _) = apply(plan).await;
    assert_eq!(rep.done, 0);
    assert_eq!(rep.failed, 1);
    assert_eq!(rep.failures[0].cause, SyncFailureCause::Conflict);
    assert_eq!(dest_bytes("a.txt").await, b"someone else got here first");
}

#[tokio::test]
async fn a_delete_tree_whose_target_vanished_is_a_conflict_not_an_error() { /* … */ }

#[tokio::test]
async fn a_failure_at_step_n_does_not_kill_the_task() {
    let plan = plan_with(vec![copy_step("a", 1), copy_step("denied", 1), copy_step("c", 1)]);
    make_unwritable("denied").await;
    let (rep, _) = apply(plan).await;
    assert_eq!(rep.done, 2);
    assert_eq!(rep.failed, 1);
    assert_eq!(rep.failures[0].rel, rel("denied"));
    assert!(dest_exists("c").await, "the walk went on");
}

#[tokio::test]
async fn a_skip_step_touches_nothing_and_journals_nothing() {
    let (rep, journal) = apply(plan_with(vec![skip_step("a")])).await;
    assert_eq!(rep.skipped, 1);
    assert_eq!(rep.done, 0);
    assert_eq!(entry_count(&journal).await, 0);
}

#[tokio::test]
async fn cancelling_mid_apply_leaves_a_closed_undoable_batch() {
    // Rule 3, and the "revert what you can" path is not a special case.
    let (task, journal) = start_apply(plan_with(many_copies(100))).await;
    cancel_after_first_step(&task).await;
    let st = task.await_terminal().await;
    assert_eq!(st, TaskState::Cancelled);
    let entries = entries_of_last_batch(&journal).await;
    assert!(!entries.is_empty());
    assert!(entries.iter().all(|e| e.batch_id == entries[0].batch_id));
}

#[tokio::test]
async fn cancelling_mid_copy_leaves_a_marked_partial_never_a_bare_one() {
    let (task, _) = start_apply(plan_with(vec![copy_step("big", 100 * 1024 * 1024)])).await;
    cancel_mid_copy(&task).await;
    task.await_terminal().await;
    assert!(!dest_exists("big").await);
    assert!(dest_exists("big.norte-partial").await);
}

#[tokio::test]
async fn applying_a_plan_that_is_not_executable_is_refused() {
    let plan = blocked_plan();
    assert!(matches!(apply_result(plan).await, Err(Error::InvalidParams { .. })));
}
```

And the two cross-provider tests the spec asks for by name. These are the ones
that exercise the honest-provider paths for real instead of simulating them:

```rust
#[tokio::test]
async fn planning_into_an_archive_blocks_instead_of_attempting_and_failing() {
    // `norte-vfs-archive` is read-only. The blocker must come from the
    // plan, not from half a batch of failed writes.
    let dest = archive_fixture("corpus.zip").await;
    let (steps, done) = plan_between(local_fixture().await, dest).await;
    assert!(!done.executable);
    assert_eq!(done.blockers[0].kind, SyncBlockerKind::DestReadOnly);
    assert!(steps.is_empty());
}

#[tokio::test]
async fn comparing_against_an_archive_source_copies_on_unknown_and_says_so() {
    // An archive whose mtime deserves no trust: `Same`/`Unknown`. The
    // default copies, and the step records why — this is the whole point of
    // ADR 0048's confidence reaching a writer.
    let (steps, _done) = plan_between(archive_fixture("corpus.zip").await, local_fixture().await).await;
    let s = steps.iter().find(|s| s.rel == rel("same-bytes.txt")).expect("step");
    assert_eq!(s.kind, SyncStepKind::Overwrite);
    assert_eq!(s.confidence, CompareConfidence::Unknown);
}

#[tokio::test]
async fn a_destination_without_a_trash_takes_the_irreversible_path_for_real() {
    // MemProvider declares no trash. Nothing here is stubbed.
    let (steps, done) = plan_between(local_fixture().await, mem_fixture_no_trash()).await;
    assert!(done.counts.irreversible > 0);
    assert!(steps.iter().any(|s| s.reason == Some(SyncReason::NoTrashOnTarget)));
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-core`

- [ ] **Step 3: Implement**

- `sync_apply_as` opens the spool by `(conn_id, plan_hash)`; absent or expired
  is `Error::PlanStale`. A malformed hash never reaches here — `PlanHash`
  rejects it at `Deserialize`.
- Gate: write scope over `dest`, read over `source`, before anything runs.
- `alloc_batch()` once; every entry of the apply carries that `batch_id`.
- Streams the spool, executing in plan order. Pre-order means `CreateDir`
  precedes every `Copy` into it — do not sort.
- Before an `Overwrite` or a `DeleteTree`: one `stat` on the destination, and
  compare against what the step recorded. Mismatch → `SyncFailureCause::Conflict`,
  no write.
- Copy goes through the existing `ops` primitives so `.norte-partial`,
  progress and the cancellation semantics come for free.
- A failed step is a report row and the task continues. `report.failures` is
  capped; `failed` is not.
- Cancellation is checked between steps. What was applied stays journalled —
  no rollback, unlike rename, because half a sync is a real state and half a
  permutation is not.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: `just ci-fast` — run #2 of three**

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): the sync executor, revalidating before every destructive step"
```

---

### Task 10: `sync.apply`, `sync.report`, and the wire-up

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs`
- Test: the core integration test file

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn apply_executes_the_plan_that_was_approved() {
    let done = plan_over(fixture()).await;
    let task = sync_apply(done.plan_hash.clone()).await.expect("accepted");
    task.await_terminal().await;
    let rep = sync_report(task.id).await.expect("report");
    assert_eq!(rep.done, done.counts.copy + done.counts.overwrite);
    assert!(rep.batch_id.is_some(), "the undo needs it");
}

#[tokio::test]
async fn a_hash_this_daemon_never_issued_is_plan_stale() {
    let h = PlanHash::parse(&"0".repeat(PLAN_HASH_LEN)).expect("hex");
    assert!(matches!(sync_apply(h).await, Err(Error::PlanStale)));
}

#[tokio::test]
async fn a_malformed_hash_is_a_params_error_and_not_plan_stale() {
    // "this is not a hash" and "the world moved" are different facts.
    let e = sync_apply_raw(serde_json::json!({"plan_hash": "nope"})).await.expect_err("refused");
    assert_invalid_params(&e);
}

#[tokio::test]
async fn a_plan_from_another_connection_is_plan_stale() {
    let done = plan_over_on_conn(fixture(), conn(1)).await;
    assert!(matches!(sync_apply_on_conn(done.plan_hash, conn(2)).await, Err(Error::PlanStale)));
}

#[tokio::test]
async fn the_spool_is_gone_once_the_apply_terminates() {
    let done = plan_over(fixture()).await;
    let task = sync_apply(done.plan_hash.clone()).await.expect("accepted");
    task.await_terminal().await;
    assert!(matches!(sync_apply(done.plan_hash).await, Err(Error::PlanStale)),
        "a plan is approved once");
}

#[tokio::test]
async fn applying_without_write_scope_on_the_destination_is_denied() { /* … */ }
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-core`

- [ ] **Step 3: Implement**

`handle_sync_apply` and `handle_sync_report`, registered in the method
dispatch next to `FS_COMPARE` (server.rs ~3214). `sync.report` is the twin of
`fs.rename_batch_report` — read that handler and mirror its ownership check.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): sync.apply takes a hash and nothing else"
```

---

### Task 11: `revert_sync_batch`

**Files:**
- Modify: `crates/norte-core/src/undo.rs`
- Test: same file

`revert_batch` (undo.rs ~525) is the sibling to read. Its "all or nothing"
contract is **rename-specific** and must not be copied: half an undone sync is
a real state, half an undone permutation is not.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_sync_batch_reverts_in_reverse_order() {
    // The overwrite pair: delete what was created, then restore what was buried.
    let (journal, batch) = applied_overwrite_batch().await;
    let rep = revert_sync_batch(&journal, batch, task_id(), &tok()).await.expect("revert");
    assert_eq!(rep.reverted, 2);
    assert_eq!(dest_bytes("a.txt").await, original_bytes());
}

#[tokio::test]
async fn an_irreversible_step_is_skipped_and_named_not_a_refusal() {
    let (journal, batch) = batch_with(vec![reversible_copy("a"), irreversible_overwrite("b")]).await;
    let rep = revert_sync_batch(&journal, batch, task_id(), &tok()).await.expect("revert");
    assert_eq!(rep.reverted, 1);
    assert_eq!(rep.irreversible_skipped, 1);
    assert_eq!(rep.irreversible_paths, vec![wire_bytes("b")]);
    assert!(!dest_exists("a").await, "9,999 reversible steps are not held hostage by one");
}

#[tokio::test]
async fn the_undo_of_a_sync_is_itself_a_batch() {
    let (journal, batch) = applied_copy_batch().await;
    revert_sync_batch(&journal, batch, task_id(), &tok()).await.expect("revert");
    let comp = compensating_entries(&journal).await;
    assert!(comp.iter().all(|e| e.batch_id == comp[0].batch_id));
    assert_ne!(comp[0].batch_id, Some(batch), "a FRESH batch id");
    assert!(comp.iter().all(|e| e.undoes_seq.is_some()));
}

#[tokio::test]
async fn undo_units_still_groups_a_sync_batch_as_one() {
    // The grouping is already generic. This pins it.
    let entries = vec![entry(1, Some(7)), entry(2, Some(7)), entry(3, None)];
    assert_eq!(undo_units(&entries).len(), 2);
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-core`

- [ ] **Step 3: Implement**

Add `irreversible_skipped: u64` and a capped `irreversible_paths: Vec<Vec<u8>>`
(wire bytes, rule 1) to `UndoReport`. Write `revert_sync_batch` next to
`revert_batch`, reusing `undo_units` unchanged. Route sync batches to it
wherever the undo entry point dispatches by batch — read that dispatch before
changing it; do not guess how it tells a rename batch from a sync one.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: Dispatch `rust-reviewer` and `security-reviewer`**

This is the journal and the undo path — the surface the CLAUDE.md review table
marks as expensive-and-silent when it fails. Ask specifically: can a
compensation land without its journal entry, and can the reverse-seq order be
wrong for any pair this executor can emit?

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): undo a sync batch, reverting what it can and naming what it cannot"
```

---

### Task 12: The frontend model

**Files:**
- Create: `crates/norte-frontend/src/sync.rs`
- Modify: `crates/norte-frontend/src/lib.rs`
- Test: `crates/norte-frontend/src/sync.rs`

Presentation only. No I/O, no TTY: this is the part that can be tested
exhaustively, and the TUI in Task 13 is only keys and painting.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_plan_with_blockers_cannot_be_approved() {
    let s = SyncState::ready(done_with_blockers());
    assert!(!s.can_approve(), "the frontend obeys `executable`…");
}

#[test]
fn approval_is_decided_by_executable_and_never_by_the_blocker_list() {
    // …and deduces nothing from the list, so a future blocker with no name
    // to show still stops the plan.
    let mut d = done_ok();
    d.executable = false;
    d.blockers.clear();
    assert!(!SyncState::ready(d).can_approve());
}

#[test]
fn the_summary_leads_with_the_irreversible_count_on_its_own_line() {
    let s = SyncState::ready(done_with(SyncCounts { copy: 3, overwrite: 2, irreversible: 2, ..d() }));
    let lines = s.summary_lines();
    assert!(lines.iter().any(|l| l.contains("irreversible") && l.contains('2')));
}

#[test]
fn mirror_asks_a_second_time_and_names_how_many_trees() {
    let s = SyncState::ready(done_mirror_with(SyncCounts { delete_tree: 4, ..d() }));
    let c = s.confirmation().expect("a second question");
    assert!(c.text.contains('4'));
}

#[test]
fn update_asks_only_once() {
    assert!(SyncState::ready(done_update()).confirmation().is_none());
}

#[test]
fn a_step_renders_its_verdict_and_its_confidence_as_distinct_glyphs() {
    // §17: textual cues, never colour alone — `Same`/`Probable` and
    // `Same`/`Certain` must not collapse for a colour-blind user.
    let a = render_step(&step_conf(CompareConfidence::Certain));
    let b = render_step(&step_conf(CompareConfidence::Probable));
    assert_ne!(a.glyphs, b.glyphs);
}

#[test]
fn an_unknown_step_kind_renders_without_panicking() {
    let _ = render_step(&SyncStep { kind: SyncStepKind::Unknown, ..step_fixture() });
}

#[test]
fn states_go_planning_ready_applying_done_and_never_backwards() {
    let mut s = SyncState::Planning(Default::default());
    s.on_plan_done(done_ok());
    assert!(matches!(s, SyncState::Ready(_)));
    s.on_apply_started(task_id());
    assert!(matches!(s, SyncState::Applying(_)));
    s.on_plan_done(done_ok()); // a late notification
    assert!(matches!(s, SyncState::Applying(_)), "a stale frame does not rewind the dialog");
}
```

- [ ] **Step 2: Run and watch them fail** — `just t norte-frontend`

- [ ] **Step 3: Implement** the state machine and the renderers.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: `just ci-fast` — run #3 of three**

- [ ] **Step 6: Commit**

```bash
git add crates/norte-frontend
git commit -m "feat(frontend): the sync approval model, irreversible steps on their own line"
```

---

### Task 13: The TUI surface, the keymap and the strings

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (or wherever `CompareState` lives — find it)
- Modify: the keymap catalogue and every preset
- Modify: `i18n/en/*.ftl`, `i18n/es/*.ftl`
- Test: `crates/norte-tui/` unit tests

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_diff_pane_opens_a_plan_and_seeds_include_from_the_selection() {
    let mut app = app_with_compare_rows(3);
    app.toggle_selection(row_id(1));
    app.toggle_selection(row_id(2));
    let params = app.start_sync_plan().expect("params");
    assert_eq!(params.include.expect("include").len(), 2);
}

#[test]
fn with_nothing_selected_the_plan_covers_the_whole_tree() {
    let app = app_with_compare_rows(3);
    assert!(app.start_sync_plan().expect("params").include.is_none());
}

#[test]
fn the_active_side_decides_the_direction_and_nothing_is_inferred() {
    let mut app = app_with_compare_rows(3);
    let a = app.start_sync_plan().expect("params");
    app.swap_active_side();
    let b = app.start_sync_plan().expect("params");
    assert_eq!(a.source, b.dest);
    assert_eq!(a.dest, b.source);
}

#[test]
fn pane_sync_dirs_is_available_and_not_greyed_out() {
    // #134 shipped its compare half and owed this one.
    let cat = keymap_catalogue();
    let e = cat.get("pane.sync-dirs").expect("entry");
    assert_eq!(e.availability, Availability::Available);
}

#[test]
fn every_preset_maps_pane_sync_dirs() {
    for p in all_presets() {
        assert!(p.binding_for("pane.sync-dirs").is_some(), "{} does not map it", p.name);
    }
}

#[test]
fn the_default_binding_is_not_a_modified_function_key() {
    // #159: under tmux none of them arrive, and a documented dead shortcut
    // has already shipped once.
    let b = default_preset().binding_for("pane.sync-dirs").expect("binding");
    assert!(!b.is_modified_function_key(), "{b:?}");
}

#[test]
fn every_sync_string_exists_in_both_locales() {
    for k in SYNC_STRING_KEYS {
        assert!(fluent_has("en", k), "en missing {k}");
        assert!(fluent_has("es", k), "es missing {k}");
    }
}

#[test]
fn overlapping_roots_renders_as_itself_and_not_as_a_generic_error() {
    // Task 1 added the variant; the error match arm lives here. Without it
    // the refusal renders as "internal error", which is the exact outcome
    // the variant exists to avoid.
    for o in [RootOverlap::Same, RootOverlap::SourceInsideDest, RootOverlap::DestInsideSource] {
        let s = render_error(&Error::OverlappingRoots { overlap: o });
        assert!(!s.contains("error interno") && !s.contains("internal"), "{o:?} → {s}");
    }
}
```

The match arm is at `crates/norte-tui/src/app.rs:4809` (the error rendering
table) and needs `err-overlapping-roots` in `i18n/en` and `i18n/es`. It is in
no other task of this plan; Task 1 flagged it precisely because it would
otherwise fall through the cracks.

- [ ] **Step 2: Run and watch them fail** — `just t norte-tui`

- [ ] **Step 3: Implement**

State in `norte-tui::app`, painting delegating to `norte-frontend`. Every
user-facing string through Fluent (`t!("sync.plan.confirm")` and friends) —
no literals. Add the catalogue entry with `availability` and which-key text,
map it in every preset, and add it to the reference sheet the day it lands,
not greyed out.

- [ ] **Step 4: Run** — expected PASS.

- [ ] **Step 5: Drive it under tmux**

The suite being green says nothing about composition. Use the tmux harness:
run a comparison, open the plan, approve an `Update`, then an `Mirror`, and
watch the second confirmation appear. Check the irreversible line renders when
the destination has no trash.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui i18n
git commit -m "feat(tui): approve a synchronisation from the diff pane"
```

---

### Task 14: ADR, docs, debt, and the gate

**Files:**
- Create: `docs/adr/0049-the-retained-sync-plan.md`
- Modify: `ARCHITECTURE.md`, `CHANGELOG.md`,
  `docs/superpowers/specs/2026-08-07-post-alpha-roadmap.md`

- [ ] **Step 1: Review and extend ADR 0049 — it already exists**

Task 1 wrote `docs/adr/0049-the-retained-sync-plan.md`, because the schema it
published cited the ADR ~30 times and ADR 0048 had shipped in the same commit
as its own version bump. **Do not recreate it.** Read it against everything
tasks 2–13 actually built and extend it where reality moved: any consequence
that turned out differently, and the `RelPath` and `RootOverlap` decisions
that came out of Task 1's own review.

What it has to say, and must still say after you edit it — not "we added
sync", but **the approved plan is retained
server-side**: a spool file that authorises writes, keyed to the connection
that produced it, with a TTL and four ways to die. Cover what was rejected and
why — re-deriving the plan walks both trees twice and never converges on a
live tree; a per-step revalidation alone makes `plan_hash` a statement of
intent rather than of fact. Record the consequences honestly, including the
one that costs: a plan is state the daemon holds, and every future step kind
must declare its `StepReversal` or the vocabulary rots the way ADR 0048 says
`confidence` would.

- [ ] **Step 2: File the debt, the same day**

Two issues, created now and not later — #147 is what happens otherwise:

- No GUI synchronisation surface (the twin of #158).
- No CLI and no MCP surface for `sync.plan`/`sync.apply` (spec 3).

Link both from the roadmap entry.

- [ ] **Step 3: Update the roadmap and the changelog**

Item 1 becomes "specs 1 and 2 built, spec 3 open". Say what spec 2
deliberately is not: no two-way sync, no resume, no conflict rules beyond
`on_unknown`. Close #134.

- [ ] **Step 4: Run the full gate — the one `just ci` of this plan**

Run: `just ci`
Expected: green. Check `just disk` first if the tree has been busy; `just ci`
refuses under 40 GB free.

- [ ] **Step 5: Commit**

```bash
git add docs ARCHITECTURE.md CHANGELOG.md
git commit -m "docs(sync): ADR 0049, the roadmap, and the debt this leaves"
```

---

## Definition of done

Code, unit tests, cross-provider tests, rustdoc with doctests on the new public
items in `norte-proto` and `norte-sync`, ADR 0049, the 0.40.0 bump with its
goldens, Fluent strings in `en` and `es`, the keymap catalogue entry, #134
closed, the GUI and CLI/MCP debt filed, and `just ci` green.
