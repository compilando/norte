# Batch Rename Executor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a transactional batch-rename executor in the core — whole-plan
collision preview, cycle-safe ordering, one journal batch, rollback on failure or
cancellation — and move the AI rename path in both frontends onto it.

**Architecture:** A pure planner (`norte-core::rename::plan`) turns pairs plus a
directory listing plus that directory's `Capabilities` into an ordered plan with
temporaries, classified collisions and a `plan_hash`. A Task
(`norte-core::rename::exec`) walks the plan against one `Provider`, records each
step in the journal under a shared `batch_id`, and unwinds on failure or
cancellation. Two wire methods (`fs.rename_batch_plan`, `fs.rename_batch`) bind
preview to execution through the hash, so the core always re-plans and never
trusts a client-supplied order. Session undo consumes a `batch_id` group as one
block by feeding the inverse pairs back through the same planner and executor.

**Tech Stack:** Rust, tokio, sqlx/SQLite (journal), sha2 (plan hash and journal
chain), `unicode-normalization` (NFC equality), proptest, nextest.

**Spec:** `docs/superpowers/specs/2026-08-08-batch-rename-executor-design.md`

---

## Read before starting

- `CLAUDE.md` — the ten hard rules. Rules 1 (filenames are bytes), 3 (every long
  operation is a cancellable task), 4 (every mutation goes through the journal),
  6 (typed errors, no `unwrap`) and 7 (no business logic in frontends) all bite
  in this work.
- `just t <crate>` for the red/green loop, `just ci-fast` before calling a task
  done, `just ci` before a commit. Never bare `cargo`.
- Coverage gate is 85% on `norte-proto`, `norte-vfs`, `norte-core` and the
  current margin is 0.12 points. Every task here adds core code; every task here
  adds tests.

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-proto/src/vpath.rs` (modify) | `Serialize`/`Deserialize`/`JsonSchema` for `Segment`, percent-encoded with the existing codec |
| `crates/norte-proto/src/methods.rs` (modify) | method constants, `RenamePair`, `RenameStep`, `RenameCollision(Kind)`, params/result types, `PROTOCOL_VERSION` |
| `crates/norte-proto/src/task.rs` (modify) | `TaskKind::RenameBatch` |
| `crates/norte-proto/src/error.rs` (modify) | `Error::PlanStale`, `Error::PlanNotExecutable` |
| `crates/norte-proto/tests/golden/types/*.json` (create) | wire freeze for the new types |
| `crates/norte-core/src/rename/mod.rs` (create) | module docs, re-exports |
| `crates/norte-core/src/rename/plan.rs` (create) | pure planner: name equality, classification, ordering, temporaries, `plan_hash` |
| `crates/norte-core/src/rename/exec.rs` (create) | the Task body: step loop, journal recording, rollback |
| `crates/norte-core/src/journal.rs` (modify) | `batch_id` column + migration, `NewEntry`/`record_entry`, `alloc_batch`, hash rule, `JournalEntry.batch_id` |
| `crates/norte-core/src/observer.rs` (modify) | `Mutation::Renamed.batch` |
| `crates/norte-core/src/ops.rs` (modify) | five `Mutation::Renamed` call sites gain `batch: None` |
| `crates/norte-core/src/engine.rs` (modify) | `rename_batch_plan`, `rename_batch`, batch grouping in `undo_session_for` |
| `crates/norte-core/src/backend.rs` (modify) | `Backend::rename_batch_plan` / `rename_batch`, both arms |
| `crates/norte-core/src/daemon/server.rs` (modify) | dispatch for the two methods |
| `crates/norte-testkit/src/faults.rs` (modify) | `fail_rename_at` |
| `crates/norte-tui/src/app.rs`, `ui.rs`, `main.rs` (modify) | plan in the AI modal, apply through the executor |
| `crates/norte-gui/src/*` (modify) | same for the GUI |
| `crates/norte-core/tests/rename_batch.rs` (create) | engine↔journal integration |
| `crates/norte-core/tests/e2e_rename_batch.rs` (create) | real local provider, permutation + hostile name + undo |
| `i18n/en-US/*.ftl`, `i18n/es-ES/*.ftl` (modify) | user-facing strings |

---

## Task 1: `Segment` on the wire

A single filename has never crossed the wire on its own — `AiRenameEntry` uses
`String` and gets away with it because the engine rejects non-UTF-8 names before
calling the AI provider. The batch executor has no such excuse (rule 1), so
`Segment` becomes a wire type using the codec `VPath` already uses.

**Files:**
- Modify: `crates/norte-proto/src/vpath.rs` (after the `VPath` serde impls, ~line 649)
- Test: `crates/norte-proto/tests/types.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/norte-proto/tests/types.rs`:

```rust
/// A `Segment` crosses the wire as ONE percent-encoded string, with the same
/// codec `VPath` uses, and non-UTF-8 bytes survive the round trip (rule 1).
#[test]
fn segment_round_trips_non_utf8_bytes_as_percent_encoded_string() {
    let raw = b"caf\xff\xfe.txt".to_vec();
    let seg = norte_proto::Segment::new(raw.clone()).expect("segment");
    let json = serde_json::to_string(&seg).expect("serialize");
    assert_eq!(json, "\"caf%FF%FE.txt\"");
    let back: norte_proto::Segment = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.as_bytes(), &raw[..]);
}

/// A malformed escape is a deserialization ERROR, never a lossy salvage.
#[test]
fn segment_rejects_a_malformed_percent_escape() {
    let e = serde_json::from_str::<norte_proto::Segment>("\"a%G1\"");
    assert!(e.is_err(), "a malformed escape must not deserialize");
}

/// A segment that `Segment::new` would refuse (a `/`, a NUL, `.`, `..`) is
/// refused on the wire too — the invariant is not bypassable by deserializing.
#[test]
fn segment_rejects_wire_forms_that_break_its_invariant() {
    for wire in ["\"a%2Fb\"", "\"a%00b\"", "\"..\"", "\"\""] {
        assert!(
            serde_json::from_str::<norte_proto::Segment>(wire).is_err(),
            "{wire} must not deserialize into a Segment",
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-proto`
Expected: FAIL — `the trait bound 'Segment: Serialize' is not satisfied`.

- [ ] **Step 3: Implement the impls**

In `crates/norte-proto/src/vpath.rs`, next to the `VPath` serde impls:

```rust
impl Serialize for Segment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = String::new();
        crate::wire::vpath_codec::encode_segment(&self.0, &mut out);
        serializer.serialize_str(&out)
    }
}

impl<'de> Deserialize<'de> for Segment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl de::Visitor<'_> for V {
            type Value = Segment;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a percent-encoded path segment")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Segment, E> {
                let bytes = crate::wire::vpath_codec::decode_segment(v)
                    .map_err(|e| E::custom(format!("invalid segment: {e}")))?;
                Segment::new(bytes).map_err(|e| E::custom(format!("invalid segment: {e}")))
            }
        }
        deserializer.deserialize_str(V)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Segment {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Segment".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "One path component in its percent-encoded wire form \
                            (ADR 0001). Never assume UTF-8: the decoded bytes are \
                            the name.",
        })
    }
}
```

Mirror whatever the neighbouring `VPath` `JsonSchema` impl does — copy its
shape rather than the snippet above if the two disagree.

- [ ] **Step 4: Run the tests**

Run: `just t norte-proto`
Expected: PASS, including the pre-existing `schema.rs` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-proto/src/vpath.rs crates/norte-proto/tests/types.rs
git commit -m "feat(proto): a bare filename crosses the wire percent-encoded"
```

---

## Task 2: the wire types for plan and execution

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`
- Modify: `crates/norte-proto/src/task.rs`
- Modify: `crates/norte-proto/src/error.rs`
- Create: `crates/norte-proto/tests/golden/types/rename_pair.json`, `rename_step.json`, `rename_collision.json`, `fs_rename_batch_plan_result.json`
- Test: `crates/norte-proto/tests/golden_types.rs`, `crates/norte-proto/tests/golden/types/methods.json`

Read `crates/norte-proto/tests/golden_types.rs` first: it defines how a type gets
a golden. Follow its existing registration pattern exactly.

- [ ] **Step 1: Write the failing golden test**

Add to `crates/norte-proto/tests/golden_types.rs`, following the file's own
helper (`golden!`, `check`, or whatever it uses — do not invent a new one):

```rust
/// The plan result is wire-frozen: a step, a collision and the hash all have
/// fixed field names, and names are percent-encoded segments.
#[test]
fn golden_fs_rename_batch_plan_result() {
    let value = methods::FsRenameBatchPlanResult {
        steps: vec![
            methods::RenameStep {
                from: seg(b"a"),
                to: seg(b".norte-rename-0a1b2c3d-0"),
                temp: true,
            },
            methods::RenameStep {
                from: seg(b"b"),
                to: seg(b"a"),
                temp: false,
            },
        ],
        collisions: vec![methods::RenameCollision {
            name: seg(b"caf\xff.txt"),
            kind: methods::RenameCollisionKind::External,
        }],
        executable: false,
        plan_hash: "0".repeat(64),
    };
    check_golden("fs_rename_batch_plan_result", &value);
}
```

with a local helper `fn seg(b: &[u8]) -> norte_proto::Segment { norte_proto::Segment::new(b.to_vec()).expect("segment") }`.

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-proto`
Expected: FAIL — `methods::FsRenameBatchPlanResult` does not exist.

- [ ] **Step 3: Add the types**

In `crates/norte-proto/src/methods.rs`, next to the `AI_RENAME_PLAN` block:

```rust
/// `fs.rename_batch_plan` — the reviewable plan for a batch of renames inside
/// ONE directory (0.36.0). A DIRECT response: no task, no journal, no mutation.
/// The client sends intent (pairs); the core decides order, temporaries and
/// collisions, so a client — possibly an agent — can never smuggle an order the
/// human did not see.
pub const FS_RENAME_BATCH_PLAN: &str = "fs.rename_batch_plan";
/// `fs.rename_batch` — execute a batch of renames as ONE task and ONE undoable
/// journal unit (0.36.0). Carries the `plan_hash` of the plan the human
/// approved; the core re-plans and refuses with [`crate::Error::PlanStale`] if
/// the directory drifted.
pub const FS_RENAME_BATCH: &str = "fs.rename_batch";

/// One requested rename inside a directory: base names, not paths.
///
/// ```
/// use norte_proto::{Segment, methods::RenamePair};
/// let p = RenamePair {
///     from: Segment::new(b"ep1.mkv".to_vec()).expect("segment"),
///     to: Segment::new(b"ep01.mkv".to_vec()).expect("segment"),
/// };
/// assert_eq!(serde_json::to_string(&p).expect("json"),
///            r#"{"from":"ep1.mkv","to":"ep01.mkv"}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePair {
    /// Existing name in the directory.
    pub from: Segment,
    /// Proposed name.
    pub to: Segment,
}

/// One step of the ordered plan. `temp` marks a rename to a temporary name
/// inserted to break a cycle — it is not something the user asked for, and a
/// frontend should render it as machinery, not as a proposal.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStep {
    /// Name before this step.
    pub from: Segment,
    /// Name after this step.
    pub to: Segment,
    /// `true` if `to` is a temporary name owned by the planner.
    pub temp: bool,
}

/// Why a plan cannot be executed. A CLOSED vocabulary: a frontend may render an
/// unknown kind generically, but the core never invents one.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenameCollisionKind {
    /// Two pairs target the same destination.
    Internal,
    /// The destination already exists and is not any pair's source.
    External,
    /// The pair's source is not in the directory (the plan was built against a
    /// stale listing).
    AbsentSource,
}

/// One rejected name plus its verdict.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameCollision {
    /// The offending name: the destination for `Internal`/`External`, the
    /// missing source for `AbsentSource`.
    pub name: Segment,
    /// The verdict.
    pub kind: RenameCollisionKind,
}

/// Params of [`FS_RENAME_BATCH_PLAN`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanParams {
    /// The directory every pair lives in.
    pub dir: VPath,
    /// The requested renames.
    pub pairs: Vec<RenamePair>,
}

/// Result of [`FS_RENAME_BATCH_PLAN`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanResult {
    /// Steps in execution order, temporaries included.
    pub steps: Vec<RenameStep>,
    /// Everything that stops the plan; empty when `executable`.
    pub collisions: Vec<RenameCollision>,
    /// `true` when the plan can be executed as-is.
    pub executable: bool,
    /// Lowercase hex sha256 over the plan's CONCLUSIONS (directory, sorted
    /// pairs, resulting steps, verdicts, case/normalisation flags). Send it back
    /// in [`FsRenameBatchParams`]: the core re-plans and compares.
    pub plan_hash: String,
}

/// Params of [`FS_RENAME_BATCH`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchParams {
    /// The directory every pair lives in.
    pub dir: VPath,
    /// The requested renames — the SAME intent that produced `plan_hash`.
    pub pairs: Vec<RenamePair>,
    /// The hash of the plan the human approved.
    pub plan_hash: String,
}
```

Bump `PROTOCOL_VERSION` to `"0.36.0"`.

In `crates/norte-proto/src/task.rs` add, after `Embed`:

```rust
    /// A batch of renames inside one directory executed as ONE transaction with
    /// ONE undoable journal unit (`fs.rename_batch`, 0.36.0). A client N-1
    /// (0.35.x) degrades it to [`TaskKind::Unknown`] through the `serde(other)`
    /// fallback, same as `Search`/`Index`/`Embed`.
    RenameBatch,
```

In `crates/norte-proto/src/error.rs` add, following the neighbouring variants'
exact style (including whatever `#[serde]` tagging and RPC-code mapping the file
already applies — check the `impl From<Error> for RpcError` or code table and add
both variants there too):

```rust
    /// The directory drifted between the preview and the execution: re-planning
    /// the same pairs produced a different plan than the `plan_hash` the caller
    /// approved. Actionable: re-plan and confirm again. NOT retryable as-is.
    PlanStale,
    /// The plan has collisions, so nothing was attempted. Actionable: fix the
    /// names (or the source directory) and re-plan.
    PlanNotExecutable,
```

- [ ] **Step 4: Regenerate and inspect the goldens**

Read the top of `crates/norte-proto/tests/golden_types.rs` for the regeneration
switch (an env var such as `UPDATE_GOLDEN=1`, or a documented manual step) and use
it. Then **open every changed JSON and read it**: a golden you did not read is a
wire format you did not freeze. `methods.json` must gain the two new method
names.

Run: `just t norte-proto`
Expected: PASS.

- [ ] **Step 5: Run the schema test**

Run: `just t norte-proto`
Expected: PASS — the `schema.rs` test proves the JSON Schema still builds with
the new types. If it fails on ordering, remember the GPUI
`serde_json/preserve_order` trap: build through `just`, never bare `cargo`.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto
git commit -m "feat(proto)!: fs.rename_batch_plan and fs.rename_batch (0.36.0)"
```

- [ ] **Step 7: Guardian review**

Ask the `protocol-guardian` agent to review the diff of this task. It is
mandatory for any `norte-proto` change. Apply what it finds before moving on.

---

## Task 3: the pure planner

**Files:**
- Create: `crates/norte-core/src/rename/mod.rs`
- Create: `crates/norte-core/src/rename/plan.rs`
- Modify: `crates/norte-core/src/lib.rs` (add `pub mod rename;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/norte-core/src/rename/plan.rs` containing ONLY this test module
at first (the module under test comes in step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn name(b: &[u8]) -> Vec<u8> {
        b.to_vec()
    }

    fn pairs(v: &[(&[u8], &[u8])]) -> Vec<(Vec<u8>, Vec<u8>)> {
        v.iter().map(|(f, t)| (name(f), name(t))).collect()
    }

    const SENSITIVE: NameCaps = NameCaps {
        case_sensitive: true,
    };
    const INSENSITIVE: NameCaps = NameCaps {
        case_sensitive: false,
    };

    /// The ordinary case: three independent renames stay in one step each.
    #[test]
    fn independent_renames_need_no_temporaries() {
        let p = plan_batch(
            &pairs(&[(b"a", b"x"), (b"b", b"y")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 2);
        assert!(p.steps.iter().all(|s| !s.temp));
    }

    /// A chain `a→b, b→c` is legal and ORDERED: `b→c` must run before `a→b`.
    /// This is the case the per-move loop could never do.
    #[test]
    fn a_chain_is_ordered_so_the_freed_name_comes_first() {
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"c")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(
            p.steps
                .iter()
                .map(|s| (s.from.clone(), s.to.clone()))
                .collect::<Vec<_>>(),
            vec![(name(b"b"), name(b"c")), (name(b"a"), name(b"b"))],
        );
        assert!(p.steps.iter().all(|s| !s.temp));
    }

    /// A permutation is a pure cycle: exactly ONE temporary breaks it, and the
    /// temporary lands last.
    #[test]
    fn a_two_cycle_is_broken_with_exactly_one_temporary() {
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 3, "two renames plus one detour: {:?}", p.steps);
        assert_eq!(p.steps.iter().filter(|s| s.temp).count(), 1);
        let last = p.steps.last().expect("a step");
        assert!(
            last.from.starts_with(b".norte-rename-"),
            "the temporary lands last",
        );
    }

    /// Two pairs targeting one name is an internal collision and stops the plan.
    #[test]
    fn two_pairs_targeting_one_name_is_an_internal_collision() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z"), (b"b", b"z")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"z"),
                kind: CollisionKind::Internal,
            }],
        );
        assert!(p.steps.is_empty(), "nothing is ordered for a dead plan");
    }

    /// A destination that already exists and is nobody's source is external.
    #[test]
    fn an_existing_bystander_destination_is_an_external_collision() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z")]),
            &[name(b"a"), name(b"z")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(p.collisions[0].kind, CollisionKind::External);
    }

    /// A source that is not in the listing means the plan was built against a
    /// stale directory.
    #[test]
    fn a_missing_source_is_an_absent_source_collision() {
        let p = plan_batch(&pairs(&[(b"gone", b"z")]), &[name(b"a")], SENSITIVE);
        assert!(!p.executable());
        assert_eq!(p.collisions[0].kind, CollisionKind::AbsentSource);
        assert_eq!(p.collisions[0].name, name(b"gone"));
    }

    /// `from == to` is dropped: it is not work, and it is not a collision.
    #[test]
    fn an_identity_pair_is_dropped_as_a_null_step() {
        let p = plan_batch(&pairs(&[(b"a", b"a")]), &[name(b"a")], SENSITIVE);
        assert!(p.executable());
        assert!(p.steps.is_empty());
    }

    /// On macOS a name can arrive NFD and the same name NFC: they are the SAME
    /// name, so renaming the NFD form onto the NFC form is a null step and the
    /// bystander is not an external collision.
    #[test]
    fn nfd_and_nfc_spellings_are_the_same_name() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(&[(nfd.clone(), nfc.clone())], &[nfd], SENSITIVE);
        assert!(p.executable(), "{:?}", p.collisions);
        assert!(p.steps.is_empty(), "same name in two spellings: no work");
    }

    /// `Foo → foo` on a case-INSENSITIVE directory is a real rename (the bytes
    /// change), not a null step — and the destination is not an external
    /// collision against its own source.
    #[test]
    fn a_case_only_rename_is_real_work_on_a_case_insensitive_directory() {
        let p = plan_batch(
            &pairs(&[(b"Foo", b"foo")]),
            &[name(b"Foo")],
            INSENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 1);
        assert!(!p.steps[0].temp, "one rename is enough at this layer");
    }

    /// On a case-insensitive directory, `a→B` collides with an existing `b`.
    /// On a case-sensitive one it does not. The DIRECTORY decides, not the OS
    /// this test runs on.
    #[test]
    fn case_insensitivity_is_decided_by_the_directory() {
        let insensitive = plan_batch(
            &pairs(&[(b"a", b"B")]),
            &[name(b"a"), name(b"b")],
            INSENSITIVE,
        );
        assert!(!insensitive.executable());
        assert_eq!(insensitive.collisions[0].kind, CollisionKind::External);

        let sensitive = plan_batch(
            &pairs(&[(b"a", b"B")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(sensitive.executable(), "{:?}", sensitive.collisions);
    }

    /// Rule 1: a non-UTF-8 name is never normalised and never folded. It is
    /// compared byte to byte and it survives into the steps intact.
    #[test]
    fn non_utf8_names_are_compared_byte_to_byte() {
        let hostile = name(b"caf\xff\xfe.txt");
        let p = plan_batch(
            &[(hostile.clone(), name(b"ok.txt"))],
            &[hostile.clone()],
            INSENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps[0].from, hostile);
    }

    /// The temporary is fixed-length and derived from the pairs, so it is the
    /// SAME across a plan and its re-plan, and it can never blow the 255-byte
    /// component limit no matter how long the real names are.
    #[test]
    fn the_temporary_name_is_short_and_stable_across_replans() {
        let long = name(&[b'x'; 250]);
        let other = name(&[b'y'; 250]);
        let listing = vec![long.clone(), other.clone()];
        let ps = vec![(long.clone(), other.clone()), (other, long)];
        let first = plan_batch(&ps, &listing, SENSITIVE);
        let again = plan_batch(&ps, &listing, SENSITIVE);
        let temp = first
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        assert!(temp.len() <= 40, "temp name is bounded: {}", temp.len());
        assert_eq!(first.hash, again.hash, "re-planning is deterministic");
        assert!(again.steps.iter().any(|s| s.to == temp));
    }

    /// A temporary never lands on a name that already exists in the directory.
    #[test]
    fn the_temporary_avoids_an_existing_name_that_looks_like_one() {
        let squatter = name(b".norte-rename-");
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a")]),
            &[name(b"a"), name(b"b"), squatter.clone()],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        for s in &p.steps {
            assert_ne!(s.to, squatter, "a temporary must not clobber a real name");
        }
    }

    /// The hash covers CONCLUSIONS: an unrelated new file does not invalidate a
    /// plan, a file that creates a collision does.
    #[test]
    fn the_hash_ignores_irrelevant_drift_and_catches_relevant_drift() {
        let ps = pairs(&[(b"a", b"z")]);
        let base = plan_batch(&ps, &[name(b"a")], SENSITIVE);
        let unrelated = plan_batch(&ps, &[name(b"a"), name(b"unrelated")], SENSITIVE);
        assert_eq!(base.hash, unrelated.hash);
        let colliding = plan_batch(&ps, &[name(b"a"), name(b"z")], SENSITIVE);
        assert_ne!(base.hash, colliding.hash);
    }

    /// The hash separates the two case regimes: the same pairs and the same
    /// listing under a different directory answer a different plan.
    #[test]
    fn the_hash_covers_the_case_regime() {
        let ps = pairs(&[(b"a", b"B")]);
        let listing = [name(b"a")];
        assert_ne!(
            plan_batch(&ps, &listing, SENSITIVE).hash,
            plan_batch(&ps, &listing, INSENSITIVE).hash,
        );
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Add `pub mod rename;` to `crates/norte-core/src/lib.rs` and create
`crates/norte-core/src/rename/mod.rs`:

```rust
//! Batch rename inside ONE directory (§17): a pure planner and a transactional
//! executor. AI rename feeds this, and the rules engine (counters, slices,
//! regex, case, cleanup) will feed it too — it only produces pairs.

pub mod plan;
```

Run: `just t norte-core`
Expected: FAIL — `plan_batch`, `NameCaps`, `Collision` and friends do not exist.

- [ ] **Step 3: Write the planner**

Prepend to `crates/norte-core/src/rename/plan.rs` (above the test module):

```rust
//! The pure part of a batch rename: given names, a listing and the destination
//! directory's capabilities, decide WHAT to do and in WHAT ORDER. No I/O, no
//! `async`, no provider — so it is cheap to test exhaustively, which is where
//! the correctness of this feature actually lives.

use std::borrow::Cow;
use std::collections::HashMap;

use sha2::{Digest, Sha256};

/// What the destination directory says about names. Comes from
/// `fs.capabilities` of THAT directory (`norte-vfs-local` probes it per
/// directory), never from `cfg!(target_os)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameCaps {
    /// `true` when the directory distinguishes case (ext4 does; APFS and NTFS
    /// do not by default).
    pub case_sensitive: bool,
}

/// Why a plan cannot run. Mirrors [`norte_proto::methods::RenameCollisionKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionKind {
    /// Two pairs target the same destination.
    Internal,
    /// The destination exists and is nobody's source.
    External,
    /// The source is not in the listing.
    AbsentSource,
}

/// One rejected name plus its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The offending name, raw bytes.
    pub name: Vec<u8>,
    /// The verdict.
    pub kind: CollisionKind,
}

/// One ordered step. `temp` marks a planner-owned temporary name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Name before the step, raw bytes.
    pub from: Vec<u8>,
    /// Name after the step, raw bytes.
    pub to: Vec<u8>,
    /// `true` when `to` is a temporary that breaks a cycle.
    pub temp: bool,
}

/// The plan: ordered steps, verdicts, and the hash that binds a preview to its
/// execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    /// Steps in execution order. EMPTY when the plan is not executable — a dead
    /// plan is never half-ordered.
    pub steps: Vec<Step>,
    /// Everything that stops the plan.
    pub collisions: Vec<Collision>,
    /// sha256 over the plan's conclusions.
    pub hash: [u8; 32],
}

impl RenamePlan {
    /// `true` when the plan can be executed as-is.
    #[must_use]
    pub fn executable(&self) -> bool {
        self.collisions.is_empty()
    }

    /// The hash as lowercase hex — the wire form.
    #[must_use]
    pub fn hash_hex(&self) -> String {
        self.hash.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

/// The comparison key of a name under `caps`: NFC when the name is UTF-8, plus
/// a lowercase fold when the directory does not distinguish case. A non-UTF-8
/// name is its own bytes — never normalised, never folded (rule 1). The
/// ORIGINAL bytes are what gets renamed; this value only decides equality.
#[must_use]
pub fn name_key(name: &[u8], caps: NameCaps) -> Cow<'_, [u8]> {
    use unicode_normalization::{UnicodeNormalization, is_nfc};
    let Ok(s) = std::str::from_utf8(name) else {
        return Cow::Borrowed(name);
    };
    let nfc: Cow<'_, str> = if is_nfc(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfc().collect())
    };
    if caps.case_sensitive {
        match nfc {
            Cow::Borrowed(_) => Cow::Borrowed(name),
            Cow::Owned(o) => Cow::Owned(o.into_bytes()),
        }
    } else {
        Cow::Owned(nfc.to_lowercase().into_bytes())
    }
}

/// Prefix of every planner-owned temporary name.
const TEMP_PREFIX: &[u8] = b".norte-rename-";

fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Digest of the INTENT (sorted pairs + case regime). Temporary names derive
/// from it, so they are identical across a plan and its re-plan without the
/// circularity of hashing the steps that contain them.
fn pairs_digest(pairs: &[(Vec<u8>, Vec<u8>)], caps: NameCaps) -> [u8; 32] {
    let mut sorted: Vec<&(Vec<u8>, Vec<u8>)> = pairs.iter().collect();
    sorted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut h = Sha256::new();
    h.update([u8::from(caps.case_sensitive)]);
    for (f, t) in sorted {
        feed(&mut h, f);
        feed(&mut h, t);
    }
    h.finalize().into()
}

/// Plan a batch. `pairs` are `(from, to)` base names, `listing` the directory's
/// current base names, `caps` that directory's name rules.
///
/// Verdicts are computed against the listing; a plan with ANY collision comes
/// back with `steps` empty, so a caller that ignores `executable()` still
/// cannot do damage.
#[must_use]
pub fn plan_batch(
    pairs: &[(Vec<u8>, Vec<u8>)],
    listing: &[Vec<u8>],
    caps: NameCaps,
) -> RenamePlan {
    // Index the listing and the pairs by comparison key.
    let mut present: HashMap<Vec<u8>, ()> = HashMap::new();
    for n in listing {
        present.insert(name_key(n, caps).into_owned(), ());
    }

    let mut collisions: Vec<Collision> = Vec::new();
    // Non-null pairs, keyed for lookup, in the caller's order.
    let mut work: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut sources: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut dests: HashMap<Vec<u8>, usize> = HashMap::new();

    for (from, to) in pairs {
        let fk = name_key(from, caps).into_owned();
        if !present.contains_key(&fk) {
            collisions.push(Collision {
                name: from.clone(),
                kind: CollisionKind::AbsentSource,
            });
            continue;
        }
        let tk = name_key(to, caps).into_owned();
        // Null step: the same name in either spelling, and the bytes are equal
        // too. `Foo→foo` on a case-insensitive directory has EQUAL keys but
        // DIFFERENT bytes, so it stays as real work.
        if fk == tk && from == to {
            continue;
        }
        if let Some(_prev) = dests.get(&tk) {
            collisions.push(Collision {
                name: to.clone(),
                kind: CollisionKind::Internal,
            });
            continue;
        }
        dests.insert(tk, work.len());
        sources.insert(fk, work.len());
        work.push((from.clone(), to.clone()));
    }

    // External: the destination exists and is not a source of this batch. A
    // case-only rename (`Foo→foo`) targets its OWN source, which is exactly the
    // `sources` hit that saves it here.
    for (from, to) in &work {
        let tk = name_key(to, caps).into_owned();
        let fk = name_key(from, caps).into_owned();
        if tk == fk {
            continue; // case-only: the occupant is the source itself.
        }
        if present.contains_key(&tk) && !sources.contains_key(&tk) {
            collisions.push(Collision {
                name: to.clone(),
                kind: CollisionKind::External,
            });
        }
    }

    let digest = pairs_digest(pairs, caps);
    let steps = if collisions.is_empty() {
        order(&work, &sources, &present, caps, &digest)
    } else {
        Vec::new()
    };

    RenamePlan {
        hash: plan_hash(&steps, &collisions, caps),
        steps,
        collisions,
    }
}

/// Topological order with one temporary per cycle.
fn order(
    work: &[(Vec<u8>, Vec<u8>)],
    sources: &HashMap<Vec<u8>, usize>,
    present: &HashMap<Vec<u8>, ()>,
    caps: NameCaps,
    digest: &[u8; 32],
) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::with_capacity(work.len() + 1);
    let mut done = vec![false; work.len()];
    // Names currently occupied: the listing, mutated as steps are emitted.
    let mut occupied: HashMap<Vec<u8>, ()> = present.clone();
    let mut temp_n: u32 = 0;

    let free = |occupied: &HashMap<Vec<u8>, ()>, key: &Vec<u8>| !occupied.contains_key(key);

    let mut progress = true;
    while progress {
        progress = false;
        for i in 0..work.len() {
            if done[i] {
                continue;
            }
            let (from, to) = &work[i];
            let fk = name_key(from, caps).into_owned();
            let tk = name_key(to, caps).into_owned();
            // A case-only rename occupies its own destination; it is always ready.
            if fk != tk && !free(&occupied, &tk) {
                continue;
            }
            occupied.remove(&fk);
            occupied.insert(tk, ());
            steps.push(Step {
                from: from.clone(),
                to: to.clone(),
                temp: false,
            });
            done[i] = true;
            progress = true;
        }
        // Nothing moved and work remains: every remaining step is in a cycle.
        // Break ONE of them with a temporary and loop again.
        if !progress && done.iter().any(|d| !d) {
            let i = done.iter().position(|d| !d).expect("an undone step");
            let (from, to) = work[i].clone();
            let temp = temp_name(digest, &mut temp_n, &occupied, caps);
            let fk = name_key(&from, caps).into_owned();
            occupied.remove(&fk);
            occupied.insert(name_key(&temp, caps).into_owned(), ());
            steps.push(Step {
                from,
                to: temp.clone(),
                temp: true,
            });
            // The real step now starts from the temporary; it will be emitted
            // once its destination frees up.
            // (Rewriting `work[i].0` is not possible on a shared slice, so the
            // deferred step is pushed onto a tail list.)
            done[i] = true;
            steps.extend(order_tail(temp, to));
            progress = true;
        }
    }
    steps
}

/// The deferred half of a broken cycle: the temporary lands on the real
/// destination. Emitted LAST by construction, because the caller appends it
/// after every other step of the cycle has been ordered.
fn order_tail(temp: Vec<u8>, to: Vec<u8>) -> Vec<Step> {
    vec![Step {
        from: temp,
        to,
        temp: true,
    }]
}

/// A short, deterministic temporary name that is free in `occupied`.
fn temp_name(
    digest: &[u8; 32],
    n: &mut u32,
    occupied: &HashMap<Vec<u8>, ()>,
    caps: NameCaps,
) -> Vec<u8> {
    loop {
        let mut name = TEMP_PREFIX.to_vec();
        for b in &digest[..4] {
            name.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        name.push(b'-');
        name.extend_from_slice(n.to_string().as_bytes());
        *n += 1;
        if !occupied.contains_key(&name_key(&name, caps).into_owned()) {
            return name;
        }
    }
}

/// sha256 over the plan's CONCLUSIONS: the ordered steps (temporaries
/// included), the verdicts, and the case regime that produced them. Deliberately
/// NOT the whole listing — an unrelated new file must not invalidate a plan the
/// human already read.
fn plan_hash(steps: &[Step], collisions: &[Collision], caps: NameCaps) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([u8::from(caps.case_sensitive)]);
    h.update((steps.len() as u64).to_le_bytes());
    for s in steps {
        feed(&mut h, &s.from);
        feed(&mut h, &s.to);
        h.update([u8::from(s.temp)]);
    }
    h.update((collisions.len() as u64).to_le_bytes());
    for c in collisions {
        feed(&mut h, &c.name);
        h.update([match c.kind {
            CollisionKind::Internal => 1u8,
            CollisionKind::External => 2,
            CollisionKind::AbsentSource => 3,
        }]);
    }
    h.finalize().into()
}
```

The `order`/`order_tail` split above is the one part of this task where the
straightforward implementation is awkward. If you find a cleaner formulation
(for example, collecting the cycle's members first and emitting
`temp → … → temp` in one pass), take it — the tests define the contract, not the
shape of the loop. What must hold: exactly one temporary per cycle, the
temporary's landing step is emitted after every other step of that cycle, and
`plan_batch` is deterministic.

Note the hash does NOT include the directory path, even though the spec's prose
mentions it: `plan_batch` is pure and never sees a `VPath`. Task 4 feeds the
directory into the hash at the engine boundary — see its `plan_for` helper.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS, all planner tests.

- [ ] **Step 5: Add the property tests**

Append to the test module:

```rust
    use proptest::prelude::*;

    proptest! {
        /// Whatever the pairs, planning TERMINATES and either refuses or
        /// produces steps that, applied in order to the listing, end at exactly
        /// the requested destinations. This is the whole contract in one test.
        #[test]
        fn an_executable_plan_lands_every_destination(
            n in 1usize..6,
            perm in proptest::collection::vec(0usize..6, 1..6),
        ) {
            let listing: Vec<Vec<u8>> =
                (0..n).map(|i| format!("f{i}").into_bytes()).collect();
            let ps: Vec<(Vec<u8>, Vec<u8>)> = listing
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let j = perm[i % perm.len()] % n;
                    (f.clone(), format!("f{j}").into_bytes())
                })
                .collect();
            let plan = plan_batch(&ps, &listing, SENSITIVE);
            if !plan.executable() {
                return Ok(());
            }
            // Simulate.
            let mut state: Vec<Vec<u8>> = listing.clone();
            for s in &plan.steps {
                let idx = state
                    .iter()
                    .position(|x| x == &s.from)
                    .expect("a step renames something that is there");
                prop_assert!(
                    !state.iter().any(|x| x == &s.to),
                    "a step never clobbers an occupied name",
                );
                state[idx] = s.to.clone();
            }
            state.sort();
            let mut want: Vec<Vec<u8>> = ps.iter().map(|(_, t)| t.clone()).collect();
            want.sort();
            prop_assert_eq!(state, want);
        }
    }
```

Run: `just t norte-core`
Expected: PASS. If the proptest finds a counterexample, that is the planner
being wrong — fix the planner, keep the regression file that proptest writes.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core/src/lib.rs crates/norte-core/src/rename
git commit -m "feat(core): a pure planner for a batch of renames in one directory"
```

---

## Task 4: the journal learns about batches

**Files:**
- Modify: `crates/norte-core/src/journal.rs`
- Modify: `crates/norte-core/src/observer.rs`
- Modify: `crates/norte-core/src/ops.rs` (five `Mutation::Renamed` sites)

- [ ] **Step 1: Write the failing tests**

Add to the test module at the bottom of `crates/norte-core/src/journal.rs`:

```rust
    /// A journal written BEFORE `batch_id` existed still verifies: an absent
    /// batch feeds NOTHING into the hash, so old entries hash exactly as they
    /// did. Simulated by recording without a batch and checking the chain.
    #[tokio::test]
    async fn entries_without_a_batch_hash_as_they_always_did() {
        let j = Journal::open_in_memory().await.expect("journal");
        let p = VPath::parse("mem:///a").expect("path");
        j.record("created", &p.to_wire().into_bytes(), None, Reversal::Delete, None, &Actor::User)
            .await
            .expect("record");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        // The pin: this exact hash is what a pre-migration journal holds.
        let head = j.head().await.expect("head").expect("some");
        let plain = chain_hash(
            &[0u8; 32],
            &Record {
                seq: 1,
                ts_ms: 0,
                actor_kind: "user",
                actor_id: None,
                op: "created",
                path: &p.to_wire().into_bytes(),
                path_to: None,
                reversal: "delete",
                reversal_ref: None,
                undoes_seq: None,
                batch_id: None,
            },
        );
        // ts_ms differs, so compare the SHAPE: recomputing with batch_id: None
        // must be what verify_chain already accepted.
        assert_eq!(head.1.len(), 32);
        assert_eq!(plain.len(), 32);
    }

    /// A batch id is monotonic and never reused, even across two allocations
        /// with no insert in between.
    #[tokio::test]
    async fn batch_ids_are_monotonic() {
        let j = Journal::open_in_memory().await.expect("journal");
        let a = j.alloc_batch().await.expect("alloc");
        let b = j.alloc_batch().await.expect("alloc");
        assert_eq!(b, a + 1);
    }

    /// The batch id is part of the chain: stripping it from a row breaks
    /// `verify_chain` at that entry.
    #[tokio::test]
    async fn stripping_a_batch_id_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("journal");
        let from = VPath::parse("mem:///a").expect("path");
        let to = VPath::parse("mem:///b").expect("path");
        let batch = j.alloc_batch().await.expect("alloc");
        let seq = j
            .record_entry(&NewEntry {
                op: "renamed",
                path: &to.to_wire().into_bytes(),
                path_to: Some(&from.to_wire().into_bytes()),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &Actor::User,
                undoes_seq: None,
                batch_id: Some(batch),
            })
            .await
            .expect("record");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        j.clear_batch_for_test(seq).await.expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: seq },
        );
    }

    /// The reader surfaces the batch, so undo can group by it.
    #[tokio::test]
    async fn entries_report_their_batch() {
        let j = Journal::open_in_memory().await.expect("journal");
        let p = VPath::parse("mem:///a").expect("path");
        let batch = j.alloc_batch().await.expect("alloc");
        j.record_entry(&NewEntry {
            op: "renamed",
            path: &p.to_wire().into_bytes(),
            path_to: Some(&p.to_wire().into_bytes()),
            reversal: Reversal::RenameBack,
            reversal_ref: None,
            actor: &Actor::User,
            undoes_seq: None,
            batch_id: Some(batch),
        })
        .await
        .expect("record");
        let es = j.entries().await.expect("entries");
        assert_eq!(es[0].batch_id, Some(batch));
    }
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — `NewEntry`, `alloc_batch`, `record_entry`, `batch_id` missing.

- [ ] **Step 3: Implement**

In `crates/norte-core/src/journal.rs`:

1. Schema: leave `SCHEMA` as-is and add an idempotent migration run right after
   it in `from_options`:

```rust
const MIGRATE_BATCH_ID: &str = "ALTER TABLE journal ADD COLUMN batch_id INTEGER";
```

```rust
        sqlx::query(SCHEMA).execute(&pool).await?;
        // Idempotent migration: an already-migrated database answers
        // "duplicate column name" and is left alone. A fresh database gets the
        // column here rather than in SCHEMA so both paths converge on ONE
        // definition of the table.
        if let Err(e) = sqlx::query(MIGRATE_BATCH_ID).execute(&pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(JournalError::Sqlx(e));
            }
        }
```

2. `Record` gains `pub batch_id: Option<i64>`, and `chain_hash` gains — **at the
   very end, and only when `Some`** — the rule that keeps old journals valid:

```rust
    // `None` feeds NOTHING (not even a presence byte): an entry written before
    // `batch_id` existed must hash exactly as it did, or `verify_chain` would
    // cry tampering over a migration. `Some` feeds a presence byte plus the
    // length-prefixed id, so neither stripping nor inventing a batch survives.
    if let Some(b) = r.batch_id {
        h.update([1u8]);
        feed(&mut h, &b.to_le_bytes());
    }
```

3. `ChainState` gains `next_batch: i64`, initialised in `from_options` from
   `SELECT COALESCE(MAX(batch_id), 0) FROM journal`, and:

```rust
    /// Hands out a fresh batch id. Monotonic and race-free: the counter lives
    /// in the chain state, under the same lock that assigns `seq`, so two
    /// concurrent batch tasks in one daemon can never share an id (a
    /// `MAX(batch_id) + 1` query could).
    ///
    /// # Errors
    /// Never fails today; `Result` is kept so a future persisted counter does
    /// not change the signature.
    pub async fn alloc_batch(&self) -> Result<i64, JournalError> {
        let mut chain = self.chain.lock().await;
        chain.next_batch += 1;
        Ok(chain.next_batch)
    }
```

4. Collapse the recording arguments into a struct and keep the old entry points
   as wrappers (this is also what silences `clippy::too_many_arguments` for
   good):

```rust
/// Everything one journal entry needs. A struct rather than nine positional
/// arguments: the call sites read, and adding a field later does not reshuffle
/// them.
pub struct NewEntry<'a> {
    /// `"created" | "removed" | "trashed" | "renamed"`.
    pub op: &'a str,
    /// Affected path, `to_wire` bytes.
    pub path: &'a [u8],
    /// Destination of a `renamed`, `to_wire` bytes.
    pub path_to: Option<&'a [u8]>,
    /// How to revert.
    pub reversal: Reversal,
    /// Reference needed to revert (a logical-trash destination, say).
    pub reversal_ref: Option<&'a [u8]>,
    /// Who caused it.
    pub actor: &'a Actor,
    /// The `seq` this entry compensates, if it is an undo.
    pub undoes_seq: Option<i64>,
    /// The batch this entry belongs to, if it was part of one. Entries sharing
    /// a batch are ONE undoable unit.
    pub batch_id: Option<i64>,
}
```

`record_entry(&self, e: &NewEntry<'_>) -> Result<i64, JournalError>` holds the
body that `record_undoing` has today (plus `batch_id` in the `Record`, the
`INSERT` column list and the bind list). `record` and `record_undoing` become
thin wrappers over it, so no existing call site changes.

5. `JournalEntry` gains `pub batch_id: Option<i64>`; `row_to_entry` reads it
   (append `batch_id` to the SELECT column lists in `entries` and
   `revertible_for`, and to the `row_to_entry` doc comment listing the order);
   `verify_chain`'s SELECT and `Record` construction gain it too.

6. Test-only tamper helper, next to `corrupt_path_for_test`:

```rust
    /// TESTS ONLY: drops the `batch_id` of an entry without recomputing its
    /// hash (simulates an attacker un-grouping a batch).
    #[cfg(test)]
    async fn clear_batch_for_test(&self, seq: i64) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET batch_id = NULL WHERE seq = ?")
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
```

7. In `crates/norte-core/src/observer.rs`, `Mutation::Renamed` gains:

```rust
    Renamed {
        /// Path original.
        from: &'a VPath,
        /// Path nuevo.
        to: &'a VPath,
        /// Lote al que pertenece el rename (`fs.rename_batch`): las entradas
        /// que comparten `batch` son UNA unidad deshacible. `None` para un
        /// rename suelto.
        batch: Option<i64>,
    },
```

8. `SqliteJournal::on_mutation` threads it through: destructure
   `Mutation::Renamed { from, to, batch }`, keep the other arms' `batch_id:
   None`, and build a `NewEntry`.

9. The five `Mutation::Renamed` sites in `crates/norte-core/src/ops.rs`
   (~1266, 1283, 1296, 1317 — check with
   `grep -n 'Mutation::Renamed' crates/norte-core/src/ops.rs`) gain
   `batch: None`.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS. Then `just t norte-proto` to be sure nothing leaked, and:

Run: `just c`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/journal.rs crates/norte-core/src/observer.rs crates/norte-core/src/ops.rs
git commit -m "feat(core): the journal groups a batch of mutations under one id"
```

---

## Task 5: the executor and the engine façade

**Files:**
- Create: `crates/norte-core/src/rename/exec.rs`
- Modify: `crates/norte-core/src/rename/mod.rs`
- Modify: `crates/norte-core/src/engine.rs`
- Modify: `crates/norte-testkit/src/faults.rs`
- Create: `crates/norte-core/tests/rename_batch.rs`

- [ ] **Step 1: Add the fault injector the tests need**

In `crates/norte-testkit/src/faults.rs`, mirroring `fail_list_at`:

```rust
    /// The `rename` whose SOURCE is this path (byte-exact) fails with
    /// [`Error::Io`](norte_proto::Error::Io), without applying its effect.
    /// For a transactional executor: the step that must trigger the rollback.
    pub fn fail_rename_at(&self, path: &VPath) {
        self.lock().fail_rename_at = Some(seg_path(path));
    }
```

plus the `fail_rename_at: Option<SegPath>` field in `FaultState` and the check in
`MemProvider::rename` (before applying anything), following how `fail_list_at` is
consumed in `crates/norte-testkit/src/mem.rs`.

Run: `just t norte-testkit`
Expected: PASS (nothing uses it yet).

- [ ] **Step 2: Write the failing integration tests**

Create `crates/norte-core/tests/rename_batch.rs`:

```rust
//! Batch rename end to end inside the core: engine → planner → executor →
//! journal, over `MemProvider`.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_core::journal::{Actor, Journal, SqliteJournal};
use norte_proto::{Error, Segment, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn seg(b: &[u8]) -> Segment {
    Segment::new(b.to_vec()).expect("segment")
}

/// Seeds a one-byte file at `path` (the shape `tests/engine.rs` uses).
async fn write_file(mem: &MemProvider, path: &VPath, content: &[u8]) {
    let mut sink = mem.write(path).await.expect("write opens");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk lands");
    sink.commit().await.expect("commit publishes");
}

/// Engine with a journal and a `MemProvider` seeded with `names` in its root.
async fn engine_with(
    names: &[&[u8]],
) -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>, VPath) {
    let provider = Arc::new(MemProvider::new());
    let dir = MemProvider::root();
    for n in names {
        write_file(&provider, &dir.join(seg(n)), n).await;
    }
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    engine.register_provider(Arc::clone(&provider) as Arc<dyn Provider>);
    (engine, provider, journal, dir)
}

/// The directory's base names, sorted — the assertion surface of every test
/// here. Contents identify each file, so a test that cares about WHICH file
/// ended up where reads the bytes instead.
async fn names_in(provider: &MemProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = provider.list(dir).await.expect("list");
    let mut v = Vec::new();
    while let Some(e) = stream.next().await {
        let e = e.expect("entry");
        if let Some(n) = e.path.file_name() {
            v.push(n.as_bytes().to_vec());
        }
    }
    v.sort();
    v
}

/// A permutation — the case the per-move loop could never do — lands, and it
/// lands as ONE journal batch.
#[tokio::test]
async fn a_permutation_lands_and_is_one_batch() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b"]).await;
    let pairs = vec![(b"a".to_vec(), b"b".to_vec()), (b"b".to_vec(), b"a".to_vec())];
    let plan = engine
        .rename_batch_plan(&dir, &pairs)
        .await
        .expect("plan");
    assert!(plan.executable(), "{:?}", plan.collisions);
    let handle = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect("submit");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(names_in(&provider, &dir).await, vec![b"a".to_vec(), b"b".to_vec()]);
    // The permutation actually swapped: the file seeded as `a` now answers to
    // `b`. Names alone would pass even if nothing moved.
    let mut s = provider.read(&dir.join(seg(b"b")), None).await.expect("read");
    let mut bytes = Vec::new();
    while let Some(c) = s.next().await {
        bytes.extend_from_slice(&c.expect("chunk"));
    }
    assert_eq!(bytes, b"a".to_vec(), "the file that was `a` is now `b`");
    let es = journal.journal().entries().await.expect("entries");
    let batches: std::collections::HashSet<_> =
        es.iter().filter_map(|e| e.batch_id).collect();
    assert_eq!(batches.len(), 1, "one batch for the whole permutation");
    assert_eq!(
        es.iter().filter(|e| e.batch_id.is_some()).count(),
        3,
        "two renames plus the temporary detour, all journalled",
    );
}

/// The stale-plan guard: the directory changed after the preview, so the
/// execution refuses instead of doing something the human did not approve.
#[tokio::test]
async fn a_drifted_directory_refuses_with_plan_stale() {
    let (engine, provider, _j, dir) = engine_with(&[b"a"]).await;
    let pairs = vec![(b"a".to_vec(), b"z".to_vec())];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    provider
        .put_file(&dir.join(seg(b"z")), b"x")
        .await
        .expect("drift");
    let e = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect_err("must refuse");
    assert_eq!(e, Error::PlanStale);
    assert_eq!(
        names_in(&provider, &dir).await,
        vec![b"a".to_vec(), b"z".to_vec()],
        "nothing was touched",
    );
}

/// A plan with a collision is refused before any effect, with its own error.
#[tokio::test]
async fn a_colliding_plan_is_refused_before_any_effect() {
    let (engine, provider, _j, dir) = engine_with(&[b"a", b"z"]).await;
    let pairs = vec![(b"a".to_vec(), b"z".to_vec())];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    assert!(!plan.executable());
    let e = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect_err("must refuse");
    assert_eq!(e, Error::PlanNotExecutable);
    assert_eq!(names_in(&provider, &dir).await, vec![b"a".to_vec(), b"z".to_vec()]);
}

/// Step k fails → the directory is IDENTICAL to how it started. This is the
/// whole point of the feature.
#[tokio::test]
async fn a_failing_step_rolls_the_whole_batch_back() {
    let (engine, provider, journal, dir) = engine_with(&[b"a", b"b", b"c"]).await;
    let before = names_in(&provider, &dir).await;
    // `c → d` is the last step (independent pairs keep the caller's order); make
    // it fail. If the planner ever reorders independent steps, this test tells
    // you by failing on the count of compensations, not by silently passing.
    provider.faults().fail_rename_at(&dir.join(seg(b"c")));
    let pairs = vec![
        (b"a".to_vec(), b"x".to_vec()),
        (b"b".to_vec(), b"y".to_vec()),
        (b"c".to_vec(), b"d".to_vec()),
    ];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    let handle = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect("submit");
    match handle.join().await {
        TaskState::Failed { error } => assert!(matches!(error, Error::Io { .. }), "{error:?}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(names_in(&provider, &dir).await, before, "rolled all the way back");
    // The journal tells the truth: every applied step and every compensation.
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(
        es.iter().filter(|e| e.undoes_seq.is_some()).count(),
        2,
        "two applied steps, two compensations",
    );
    assert!(
        journal.journal().verify_chain().await.expect("verify").is_intact(),
        "the chain survives a rollback",
    );
}

/// Cancellation is the same rollback: a cancelled batch leaves the tree as it
/// was (rule 3, and the same promise a cancelled copy makes).
#[tokio::test]
async fn a_cancelled_batch_rolls_back() {
    let (engine, provider, _j, dir) = engine_with(&[b"a", b"b", b"c"]).await;
    let before = names_in(&provider, &dir).await;
    provider
        .faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(50)));
    let pairs = vec![
        (b"a".to_vec(), b"x".to_vec()),
        (b"b".to_vec(), b"y".to_vec()),
        (b"c".to_vec(), b"z".to_vec()),
    ];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    let handle = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect("submit");
    // Wait for the first step to have happened, then cancel — no blind sleep:
    // poll the provider until the first destination appears.
    loop {
        if provider.stat(&dir.join(seg(b"x"))).await.is_ok() {
            break;
        }
        tokio::task::yield_now().await;
    }
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert_eq!(names_in(&provider, &dir).await, before);
}
```

Then, in `crates/norte-core/src/rename/exec.rs`, the third failure shape the
spec names — the rename succeeded but its journal entry did not become durable
(rule 4). It needs a recorder that fails on demand, which is a unit test of
`exec`, not an engine test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A recorder that fails on the Nth call. Rule 4: a step whose entry is not
    /// durable DID NOT HAPPEN, so the whole batch unwinds — including that step.
    struct FailAt {
        n: std::sync::atomic::AtomicUsize,
        fail_on: usize,
    }

    #[async_trait::async_trait]
    impl StepJournal for FailAt {
        async fn renamed(
            &self,
            _from: &VPath,
            _to: &VPath,
            _undoes: Option<i64>,
        ) -> Result<Option<i64>, Error> {
            let i = self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if i + 1 == self.fail_on {
                return Err(Error::Internal { panic: false });
            }
            Ok(None)
        }
    }

    #[tokio::test]
    async fn a_step_whose_journal_entry_fails_unwinds_the_batch() {
        let provider = norte_testkit::MemProvider::new();
        let dir = norte_testkit::MemProvider::root();
        // Seed a, b; plan a→x, b→y; make the SECOND journal write fail.
        // … seed with the same `write_file` helper the integration test uses …
        let steps = vec![
            /* a→x */
            /* b→y */
        ];
        let recorder = FailAt {
            n: std::sync::atomic::AtomicUsize::new(0),
            fail_on: 2,
        };
        let report = std::sync::Mutex::new(BatchReport::default());
        let err = run(
            &provider,
            &recorder,
            &steps,
            &tokio_util::sync::CancellationToken::new(),
            &crate::progress::ProgressReporter::for_test(),
            &report,
        )
        .await
        .expect_err("the batch fails");
        assert!(matches!(err, Error::Internal { .. }), "{err:?}");
        // Both names are back: the step whose entry failed was rolled back too.
        assert!(provider.stat(&dir.join(seg(b"a"))).await.is_ok());
        assert!(provider.stat(&dir.join(seg(b"b"))).await.is_ok());
        assert_eq!(report.lock().expect("lock").rolled_back, 2);
    }
}
```

`ProgressReporter::for_test()` may not exist — check
`crates/norte-core/src/progress.rs` for how other unit tests build one (there may
be a `ProgressReporter::new(...)` taking a watch sender). Use what is there;
do not add a constructor just for this.

`MemProvider::put_file` / `list_names` / `stat` may be named differently — check
`crates/norte-testkit/src/mem.rs` and `crates/norte-core/tests/engine.rs` for the
established seeding and listing helpers and use those. Same for
`TaskHandle::wait`/`cancel`: copy the shape from an existing engine test.

- [ ] **Step 3: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — `Engine::rename_batch_plan` does not exist.

- [ ] **Step 4: Write the executor**

Create `crates/norte-core/src/rename/exec.rs`:

```rust
//! The transactional half: walk a [`crate::rename::plan::RenamePlan`] against
//! one provider, journal each step under one batch id, and unwind on failure or
//! cancellation.
//!
//! Rule 4 is what makes the two failure shapes one path: a rename that
//! succeeded but whose journal entry did not become durable DID NOT HAPPEN, so
//! it is rolled back like any other failure.

use std::sync::Arc;

use norte_proto::{Error, VPath};
use norte_vfs::Provider;

use crate::journal::{Actor, NewEntry, Reversal, SqliteJournal};
use crate::observer::{Mutation, MutationObserver};
use crate::rename::plan::Step;

/// One step ready to execute: absolute paths plus, for a compensating batch,
/// the original `seq` this step reverts.
#[derive(Debug, Clone)]
pub(crate) struct PlannedStep {
    pub from: VPath,
    pub to: VPath,
    pub undoes: Option<i64>,
}

/// Turns a planner step into an absolute one under `dir`.
///
/// # Errors
/// [`Error::InvalidPath`] if a planned name is not a valid segment (cannot
/// happen for names that came from a listing; the guard is not free but it is
/// cheap and it is rule 6).
pub(crate) fn absolute(dir: &VPath, s: &Step) -> Result<PlannedStep, Error> {
    let seg = |b: &[u8]| {
        norte_proto::Segment::new(b.to_vec()).map_err(|_| Error::InvalidPath)
    };
    Ok(PlannedStep {
        from: dir.join(seg(&s.from)?),
        to: dir.join(seg(&s.to)?),
        undoes: None,
    })
}

/// How a step gets recorded. Two implementations: through the observer (no
/// journal wired: no seq, no compensation link — honest, since without a
/// journal there is no undo either) and straight into the journal (the daemon's
/// case, which needs the `seq` back so the rollback can compensate it).
#[async_trait::async_trait]
pub(crate) trait StepJournal: Send + Sync {
    /// Records `from → to`. Returns the assigned `seq` when there is one.
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        undoes: Option<i64>,
    ) -> Result<Option<i64>, Error>;
}

pub(crate) struct ObserverJournal {
    pub observer: Arc<dyn MutationObserver>,
    pub actor: Actor,
}

#[async_trait::async_trait]
impl StepJournal for ObserverJournal {
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        _undoes: Option<i64>,
    ) -> Result<Option<i64>, Error> {
        self.observer
            .on_mutation(&Mutation::Renamed { from, to, batch: None }, &self.actor)
            .await?;
        Ok(None)
    }
}

pub(crate) struct BatchJournal {
    pub journal: Arc<SqliteJournal>,
    pub actor: Actor,
    pub batch_id: i64,
}

#[async_trait::async_trait]
impl StepJournal for BatchJournal {
    async fn renamed(
        &self,
        from: &VPath,
        to: &VPath,
        undoes: Option<i64>,
    ) -> Result<Option<i64>, Error> {
        let seq = self
            .journal
            .journal()
            .record_entry(&NewEntry {
                op: "renamed",
                path: &to.to_wire().into_bytes(),
                path_to: Some(&from.to_wire().into_bytes()),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &self.actor,
                undoes_seq: undoes,
                batch_id: Some(self.batch_id),
            })
            .await
            .map_err(Error::from)?;
        Ok(Some(seq))
    }
}

/// What a batch left behind when it could not finish cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// Steps applied and journalled.
    pub applied: u64,
    /// Steps unwound by the rollback.
    pub rolled_back: u64,
    /// The step the rollback could NOT undo, as `(from, to)` of the rename that
    /// stayed applied. `Some` means the directory is half-renamed and the user
    /// needs to know exactly where — never report a bare error for this.
    pub stuck: Option<(VPath, VPath)>,
}

/// Executes `steps` in order. On any failure — including a journal write that
/// did not land — unwinds every applied step in reverse and returns the
/// original error. Checks `cancel` between steps; a cancellation unwinds the
/// same way and returns [`Error::Cancelled`].
///
/// # Errors
/// The provider's error for the failing step, [`Error::Cancelled`], or the
/// journal's error surfaced as [`Error::Internal`].
pub(crate) async fn run(
    provider: &dyn Provider,
    recorder: &dyn StepJournal,
    steps: &[PlannedStep],
    cancel: &tokio_util::sync::CancellationToken,
    progress: &crate::progress::ProgressReporter,
    report: &std::sync::Mutex<BatchReport>,
) -> Result<(), Error> {
    progress.update(|p| p.entries_total = Some(steps.len() as u64));
    // Applied steps, newest last: (from, to, seq assigned).
    let mut applied: Vec<(VPath, VPath, Option<i64>)> = Vec::with_capacity(steps.len());

    for s in steps {
        if cancel.is_cancelled() {
            unwind(provider, recorder, &mut applied, report).await;
            return Err(Error::Cancelled);
        }
        if let Err(e) = provider.rename(&s.from, &s.to).await {
            unwind(provider, recorder, &mut applied, report).await;
            return Err(e);
        }
        match recorder.renamed(&s.from, &s.to, s.undoes).await {
            Ok(seq) => {
                applied.push((s.from.clone(), s.to.clone(), seq));
                report.lock().expect("batch report lock").applied += 1;
                progress.update(|p| p.entries_done += 1);
            }
            Err(e) => {
                // Rule 4: not durable, so it did not happen. Put the name back
                // and unwind everything, this step included.
                applied.push((s.from.clone(), s.to.clone(), None));
                unwind(provider, recorder, &mut applied, report).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Renames every applied step back, newest first, journalling each reversal as
/// a compensation of the entry it undoes. Stops at the first reversal the
/// provider refuses and records WHERE in the report — the caller reports that
/// verbatim instead of a bare error.
async fn unwind(
    provider: &dyn Provider,
    recorder: &dyn StepJournal,
    applied: &mut Vec<(VPath, VPath, Option<i64>)>,
    report: &std::sync::Mutex<BatchReport>,
) {
    while let Some((from, to, seq)) = applied.pop() {
        if let Err(e) = provider.rename(&to, &from).await {
            tracing::error!(
                error = %e,
                "batch rollback blocked: the directory is half renamed",
            );
            report.lock().expect("batch report lock").stuck = Some((from, to));
            return;
        }
        // The compensation is itself a mutation; if IT cannot be journalled the
        // effect still happened, so the loop continues and the error is logged.
        // (Recording a reversal is what makes a later `verify_chain` agree with
        // the tree.)
        if let Err(e) = recorder.renamed(&to, &from, seq).await {
            tracing::error!(error = %e, "batch rollback step could not be journalled");
        }
        report.lock().expect("batch report lock").rolled_back += 1;
    }
}
```

Add `pub mod exec;` to `crates/norte-core/src/rename/mod.rs` and re-export
`BatchReport`.

- [ ] **Step 5: Write the engine façade**

In `crates/norte-core/src/engine.rs`, next to `move_with_as`:

```rust
    /// The reviewable plan of a batch of renames inside `dir` (§17). Reads the
    /// directory and its capabilities; NEVER mutates and never journals.
    ///
    /// `pairs` are `(from, to)` BASE NAMES as raw bytes (rule 1). The caller
    /// sends intent; the order, the temporaries and the verdicts are decided
    /// here, so a client — possibly an agent — cannot smuggle a plan the human
    /// did not see.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if `dir`'s scheme has no provider or the provider
    /// is read-only; [`Error::PolicyDenied`] from the gate; the provider's error
    /// while listing.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<crate::rename::plan::RenamePlan, Error> {
        self.rename_batch_plan_as(dir, pairs, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch_plan`] with an explicit actor (the agentic path).
    ///
    /// # Errors
    /// As [`Self::rename_batch_plan`].
    pub async fn rename_batch_plan_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        actor: crate::journal::Actor,
    ) -> Result<crate::rename::plan::RenamePlan, Error> {
        // Planning READS the directory, so it passes the read gate (#80) — not
        // the mutation gate: nothing is written here.
        self.gate(&actor, crate::policy::PolicyOp::Read, &[dir]).await?;
        let (plan, _provider) = self.plan_for(dir, pairs).await?;
        Ok(plan)
    }

    /// Plans against the CURRENT directory and returns the plan plus the
    /// provider that served it, so the execution path can re-plan with exactly
    /// the same inputs.
    async fn plan_for(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<(crate::rename::plan::RenamePlan, Arc<dyn Provider>), Error> {
        let provider = self.provider_for(dir).await?;
        let caps = provider.capabilities();
        if caps.flags.contains(CapabilityFlags::READ_ONLY) {
            return Err(Error::Unsupported);
        }
        let names = list_base_names(&*provider, dir).await?;
        let plan = crate::rename::plan::plan_batch(
            pairs,
            &names,
            crate::rename::plan::NameCaps {
                case_sensitive: caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
            },
        );
        Ok((plan, provider))
    }

    /// Executes a batch of renames as ONE task and ONE undoable journal unit.
    ///
    /// `plan_hash` is the hash of the plan the human approved
    /// ([`Self::rename_batch_plan`]). The directory is re-planned here and the
    /// hashes must match: a drift that changes a verdict answers
    /// [`Error::PlanStale`] rather than executing something nobody approved.
    ///
    /// # Errors
    /// [`Error::PlanNotExecutable`] if the plan has collisions;
    /// [`Error::PlanStale`] if the directory drifted; [`Error::PolicyDenied`]
    /// from the gate; [`Error::Unsupported`] with no provider or a read-only
    /// one.
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &str,
    ) -> Result<TaskHandle, Error> {
        self.rename_batch_as(dir, pairs, plan_hash, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch`] with an explicit actor (the agentic path).
    ///
    /// # Errors
    /// As [`Self::rename_batch`].
    pub async fn rename_batch_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &str,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let (plan, provider) = self.plan_for(dir, pairs).await?;
        if !plan.executable() {
            return Err(Error::PlanNotExecutable);
        }
        if plan.hash_hex() != plan_hash {
            return Err(Error::PlanStale);
        }
        // ONE gate for the whole batch, with every path involved: the policy
        // resolves most-restrictive over the slice, so a batch that touches a
        // denied name is denied whole (never half-applied).
        let steps: Vec<crate::rename::exec::PlannedStep> = plan
            .steps
            .iter()
            .map(|s| crate::rename::exec::absolute(dir, s))
            .collect::<Result<_, _>>()?;
        let mut gate_paths: Vec<&VPath> = Vec::with_capacity(steps.len() * 2);
        for s in &steps {
            gate_paths.push(&s.from);
            gate_paths.push(&s.to);
        }
        self.gate(&actor, crate::policy::PolicyOp::Move, &gate_paths)
            .await?;

        let recorder: Arc<dyn crate::rename::exec::StepJournal> = match &self.journal {
            Some(j) => {
                let batch_id = j.journal().alloc_batch().await.map_err(Error::from)?;
                Arc::new(crate::rename::exec::BatchJournal {
                    journal: Arc::clone(j),
                    actor: actor.clone(),
                    batch_id,
                })
            }
            // No journal wired (embedded tests, `Engine::new`): the renames
            // still happen and still reach the observer, but there is no batch
            // to group and no undo to serve.
            None => Arc::new(crate::rename::exec::ObserverJournal {
                observer: Arc::clone(&self.observer),
                actor: actor.clone(),
            }),
        };

        let key = dir.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::RenameBatch,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let report = std::sync::Mutex::new(crate::rename::exec::BatchReport::default());
                    let out = crate::rename::exec::run(
                        &*provider,
                        &*recorder,
                        &steps,
                        &ctx.cancel,
                        &*ctx.progress,
                        &report,
                    )
                    .await;
                    // A rollback that could not finish is not a detail: name the
                    // step that stayed applied.
                    if let Some((from, to)) = report.lock().expect("batch report lock").stuck.clone()
                    {
                        tracing::error!(
                            from = %span_path(&from),
                            to = %span_path(&to),
                            "batch rename left a rename applied that could not be undone",
                        );
                    }
                    out
                })
            }),
        ))
    }
```

Add the listing helper near the other private helpers in `engine.rs`:

```rust
/// Every base name in `dir`, raw bytes. Materialises the listing: a batch
/// rename is bounded by what a human reviewed, and the planner needs the whole
/// directory to judge collisions.
async fn list_base_names(provider: &dyn Provider, dir: &VPath) -> Result<Vec<Vec<u8>>, Error> {
    use futures::StreamExt;
    let mut stream = provider.list(dir).await?;
    let mut names = Vec::new();
    while let Some(entry) = stream.next().await {
        let e = entry?;
        if let Some(n) = e.path.file_name() {
            names.push(n.as_bytes().to_vec());
        }
    }
    Ok(names)
}
```

Check how `ops.rs` consumes an `EntryStream` and copy that shape — including
whether it uses `list_with` and how it handles a partial listing.

`PolicyOp::Read` may be spelled differently (`PolicyOp::Read`, `PolicyOp::List`,
or the `read_gate` helper the daemon uses for `#80`). Grep
`crates/norte-core/src/policy.rs` for the variant list and use the one that
`fs.stat`/`fs.list` already use.

- [ ] **Step 6: Run the tests**

Run: `just t norte-core`
Expected: PASS, all five integration tests.

Run: `just c`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-core/src/rename crates/norte-core/src/engine.rs crates/norte-core/tests/rename_batch.rs crates/norte-testkit/src/faults.rs crates/norte-testkit/src/mem.rs
git commit -m "feat(core): a batch of renames is one task, one journal unit, one rollback"
```

---

## Task 6: undo consumes a batch as one block

**Files:**
- Modify: `crates/norte-core/src/engine.rs` (`undo_session_for`)
- Modify: `crates/norte-core/src/undo.rs`
- Test: `crates/norte-core/tests/rename_batch.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-core/tests/rename_batch.rs`:

```rust
/// Undo of a batch is ONE step for the user: the permutation goes back whole.
#[tokio::test]
async fn undoing_a_session_reverts_the_whole_batch() {
    let (engine, provider, _j, dir) = engine_with(&[b"a", b"b"]).await;
    let before = names_in(&provider, &dir).await;
    let pairs = vec![(b"a".to_vec(), b"b".to_vec()), (b"b".to_vec(), b"a".to_vec())];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    let applied = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);
    let (handle, report) = engine.undo_session(Actor::User).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(names_in(&provider, &dir).await, before);
    let r = report.lock().expect("report lock");
    assert!(r.blocked.is_none(), "{:?}", r.blocked);
}

/// All or nothing: if ONE member of the batch cannot be reverted, the batch is
/// not touched at all — no half-undo.
#[tokio::test]
async fn a_batch_whose_member_is_blocked_is_left_untouched() {
    let (engine, provider, _j, dir) = engine_with(&[b"a", b"b"]).await;
    let pairs = vec![(b"a".to_vec(), b"x".to_vec()), (b"b".to_vec(), b"y".to_vec())];
    let plan = engine.rename_batch_plan(&dir, &pairs).await.expect("plan");
    let applied = engine
        .rename_batch(&dir, &pairs, &plan.hash_hex())
        .await
        .expect("submit");
    assert_eq!(applied.join().await, TaskState::Completed);
    // Someone put `a` back by hand: reverting `x → a` would clobber it.
    write_file(&provider, &dir.join(seg(b"a")), b"mine").await;
    let after_squat = names_in(&provider, &dir).await;
    let (handle, report) = engine.undo_session(Actor::User).await.expect("undo");
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        names_in(&provider, &dir).await,
        after_squat,
        "the blocked batch is not half-undone",
    );
    assert!(report.lock().expect("report lock").blocked.is_some());
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — today the second test half-undoes the batch (`y → b` succeeds,
`x → a` blocks).

- [ ] **Step 3: Implement grouping**

In `crates/norte-core/src/engine.rs`, `undo_session_for` walks
`revertible_for(target)` in `seq DESC`. Change the planning loop to consume a
run of consecutive entries sharing a `batch_id` as one unit:

```rust
    /// Splits LIFO entries into units: a lone entry, or every consecutive
    /// entry sharing one `batch_id`. A batch is reverted whole or not at all,
    /// so it must reach the task as ONE item.
    fn undo_units(entries: Vec<crate::journal::JournalEntry>) -> Vec<Vec<crate::journal::JournalEntry>> {
        let mut units: Vec<Vec<crate::journal::JournalEntry>> = Vec::new();
        for e in entries {
            match (e.batch_id, units.last_mut()) {
                (Some(b), Some(last)) if last.first().and_then(|f| f.batch_id) == Some(b) => {
                    last.push(e);
                }
                _ => units.push(vec![e]),
            }
        }
        units
    }
```

The task body then, per unit:

- a unit of one entry keeps today's behaviour exactly (`revert_entry`);
- a unit of many is a batch. Build the inverse pairs (`to → from` in LIFO
  order), re-plan them with `rename::plan::plan_batch` against the CURRENT
  listing, and:
  - if the plan is not executable, the unit is `Blocked` with
    `Error::Conflict { conflict: ConflictKind::Exists }` (or the verdict's own
    error) and NOTHING is applied — this is the all-or-nothing test;
  - if it is executable, run it through `rename::exec::run` with a
    `BatchJournal` whose `batch_id` is a FRESH id and whose steps carry
    `undoes: Some(original_seq)`; a temporary step inherits the `undoes` of the
    step it serves, so no compensating entry is ever left looking like a fresh
    revertible mutation.

Put that logic in `crates/norte-core/src/undo.rs` as
`pub(crate) async fn revert_batch(...) -> Result<Reverted, Error>`, next to
`revert_entry`, so the engine keeps one dispatch and both paths report through
the same `Reverted` enum. The policy gate in the planning loop already collects
both endpoints per entry; for a batch, collect them for every member before
gating.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS, and the pre-existing `crates/norte-core/tests/undo.rs` must stay
green — the single-entry path is unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/engine.rs crates/norte-core/src/undo.rs crates/norte-core/tests/rename_batch.rs
git commit -m "feat(core): session undo reverts a rename batch whole or not at all"
```

---

## Task 7: daemon and `Backend`

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs`
- Modify: `crates/norte-core/src/backend.rs`
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: Write the failing daemon tests**

Append to `crates/norte-core/tests/daemon.rs`, following the file's existing
harness (find `ai_rename_plan_responde_por_el_socket` and copy its shape):

```rust
/// The plan crosses the socket, hostile names included, and it does not mutate.
#[tokio::test]
async fn rename_batch_plan_answers_over_the_socket() {
    // … harness setup as the neighbouring tests do …
    let params = serde_json::json!({
        "dir": dir.to_wire(),
        "pairs": [{"from": "caf%FF.txt", "to": "cafe.txt"}],
    });
    let result = call(&mut client, methods::FS_RENAME_BATCH_PLAN, params).await;
    let plan: methods::FsRenameBatchPlanResult =
        serde_json::from_value(result).expect("plan");
    assert!(plan.executable);
    assert_eq!(plan.plan_hash.len(), 64);
}

/// A stale hash is refused over the wire with the actionable error, not with a
/// generic internal one.
#[tokio::test]
async fn rename_batch_with_a_stale_hash_is_refused_over_the_socket() {
    // … plan, then create the destination behind the daemon's back, then …
    let err = call_err(&mut client, methods::FS_RENAME_BATCH, params).await;
    assert_eq!(err_code(&err), expected_code_for(norte_proto::Error::PlanStale));
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL — method not found.

- [ ] **Step 3: Wire the daemon**

In `dispatch_fs_task` (`crates/norte-core/src/daemon/server.rs`, ~2891):

```rust
        // fs.rename_batch_plan (0.36.0): the reviewable plan. DIRECT response,
        // no task, no mutation — so it goes through the READ gate like fs.stat.
        methods::FS_RENAME_BATCH_PLAN => {
            let p: methods::FsRenameBatchPlanParams = parse_params(req.params)?;
            let pairs = crate::rename::pairs_from_wire(&p.pairs);
            let plan = shared
                .engine
                .rename_batch_plan_as(&p.dir, &pairs, actor.clone())
                .await
                .map_err(RpcError::from)?;
            to_value(&crate::rename::plan_to_proto(&plan))
        }
        // fs.rename_batch (0.36.0): ONE task, ONE journal batch, rollback on
        // failure. The engine re-plans and checks the hash.
        methods::FS_RENAME_BATCH => {
            let p: methods::FsRenameBatchParams = parse_params(req.params)?;
            let pairs = crate::rename::pairs_from_wire(&p.pairs);
            let handle = shared
                .engine
                .rename_batch_as(&p.dir, &pairs, &p.plan_hash, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // Same task-registration shape as fs.move: copy it verbatim from
            // the FS_MOVE arm, including the progress subscription.
            register_fs_task(handle, conn_id, shared)
        }
```

with two converters. They are a proto↔core mapping and BOTH the daemon and
`Backend`'s embedded arm need them, so they live in
`crates/norte-core/src/rename/mod.rs` as `pub fn`, not in the daemon:

```rust
/// Wire pairs → planner pairs (raw bytes). The `Segment` invariant already ran
/// during deserialisation.
#[must_use]
pub fn pairs_from_wire(pairs: &[norte_proto::methods::RenamePair]) -> Vec<(Vec<u8>, Vec<u8>)> {
    pairs
        .iter()
        .map(|p| (p.from.as_bytes().to_vec(), p.to.as_bytes().to_vec()))
        .collect()
}

/// Planner plan → wire plan. A name that cannot be a `Segment` is impossible
/// here (it came from a listing or from a `Segment` on the way in), so the
/// filter is a belt, not a policy.
#[must_use]
pub fn plan_to_proto(plan: &plan::RenamePlan) -> norte_proto::methods::FsRenameBatchPlanResult {
    let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).ok();
    methods::FsRenameBatchPlanResult {
        steps: plan
            .steps
            .iter()
            .filter_map(|s| {
                Some(norte_proto::methods::RenameStep {
                    from: seg(&s.from)?,
                    to: seg(&s.to)?,
                    temp: s.temp,
                })
            })
            .collect(),
        collisions: plan
            .collisions
            .iter()
            .filter_map(|c| {
                use norte_proto::methods::RenameCollisionKind as K;
                Some(norte_proto::methods::RenameCollision {
                    name: seg(&c.name)?,
                    kind: match c.kind {
                        plan::CollisionKind::Internal => K::Internal,
                        plan::CollisionKind::External => K::External,
                        plan::CollisionKind::AbsentSource => K::AbsentSource,
                    },
                })
            })
            .collect(),
        executable: plan.executable(),
        plan_hash: plan.hash_hex(),
    }
}
```

Also add both methods to the allow-list at `server.rs:~1579` (the match that
decides which methods reach `dispatch_fs_task`), and check whether that list also
gates methods by handshake state.

- [ ] **Step 4: Wire `Backend`**

In `crates/norte-core/src/backend.rs`, both arms, following `ai_rename_plan`
(direct response) and `delete` (task):

```rust
    /// The reviewable plan of a batch of renames in `dir` (§17). NEVER mutates.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no provider or a read-only one;
    /// [`Error::PolicyDenied`] from the gate; the protocol taxonomy.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
    ) -> Result<norte_proto::methods::FsRenameBatchPlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let plan = engine.rename_batch_plan(dir, &raw).await?;
                Ok(crate::rename::plan_to_proto(&plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_plan(dir, pairs).await,
        }
    }

    /// Executes the approved batch as ONE task and ONE undoable journal unit.
    ///
    /// # Errors
    /// [`Error::PlanStale`] if the directory drifted since the plan;
    /// [`Error::PlanNotExecutable`] if it has collisions; plus the above.
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
        plan_hash: &str,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let handle = engine.rename_batch(dir, &raw, plan_hash).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch(dir, pairs, plan_hash).await,
        }
    }
```

and, in the `Remote` impl block (next to `ai_rename_plan` and `delete`):

```rust
        /// `fs.rename_batch_plan` (0.36.0): direct response, no task.
        pub(super) async fn rename_batch_plan(
            &self,
            dir: &VPath,
            pairs: &[methods::RenamePair],
        ) -> Result<methods::FsRenameBatchPlanResult, Error> {
            self.call_timed_guarded(
                methods::FS_RENAME_BATCH_PLAN,
                &methods::FsRenameBatchPlanParams {
                    dir: dir.clone(),
                    pairs: pairs.to_vec(),
                },
            )
            .await
        }

        /// `fs.rename_batch` (0.36.0): one task for the whole batch.
        pub(super) async fn rename_batch(
            &self,
            dir: &VPath,
            pairs: &[methods::RenamePair],
            plan_hash: &str,
        ) -> Result<TaskRef, Error> {
            let result: FsTaskResult = self
                .call_timed_guarded(
                    methods::FS_RENAME_BATCH,
                    &methods::FsRenameBatchParams {
                        dir: dir.clone(),
                        pairs: pairs.to_vec(),
                        plan_hash: plan_hash.to_owned(),
                    },
                )
                .await?;
            Ok(self.own_task(result.task_id, TaskKind::RenameBatch))
        }
```

- [ ] **Step 5: Run the tests**

Run: `just t norte-core`
Expected: PASS.

Run: `just ci-fast`
Expected: green. Docs are part of it — every new public item needs rustdoc, and
proto/vfs items need a doctest.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): fs.rename_batch{,_plan} over the socket and both Backend arms"
```

- [ ] **Step 7: Reviews**

Ask `protocol-guardian` (the new wire arms), `security-reviewer` (the gate: does
a batch pass one most-restrictive gate over every endpoint? can an agent reach
it? is `plan_hash` doing what it claims?) and `rust-reviewer` (the diff since
Task 4). Apply findings before the frontends.

---

## Task 8: the TUI applies through the executor

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (the `Modal::AiRenamePlan` variant)
- Modify: `crates/norte-tui/src/ui.rs` (`ai_rename_plan_modal_text`)
- Modify: `crates/norte-tui/src/main.rs` (`apply_ai_rename`, ~6157)
- Modify: `i18n/en-US/*.ftl`, `i18n/es-ES/*.ftl`

- [ ] **Step 1: Write the failing render test**

In `crates/norte-tui/src/ui.rs`, in `mod ai_rename_plan_modal_tests`:

```rust
    /// A collision is VISIBLE and the modal says the plan cannot be applied.
    /// One path per line, masked, elided in the middle — the discipline the
    /// encoding audit imposed on the agent-approval modal (M3-3b T5).
    #[test]
    fn a_collision_is_rendered_and_the_plan_is_marked_unapplicable() {
        let plan = norte_proto::methods::FsRenameBatchPlanResult {
            steps: vec![],
            collisions: vec![norte_proto::methods::RenameCollision {
                name: norte_proto::Segment::new(b"z".to_vec()).expect("segment"),
                kind: norte_proto::methods::RenameCollisionKind::External,
            }],
            executable: false,
            plan_hash: "0".repeat(64),
        };
        let (_, body) = ai_rename_plan_modal_text(
            &dir(),
            &[entry("a", "z")],
            0,
            &vivas(),
            Some(&plan),
        );
        assert!(body.iter().any(|l| l.contains('z')), "the name is shown");
        assert!(
            body.iter().any(|l| l.contains(&t("modal-rename-batch-collision-external"))),
            "the verdict is shown",
        );
    }

    /// A planner temporary is labelled as machinery, not as a proposal: a user
    /// must never think norte is renaming their file to `.norte-rename-…`.
    #[test]
    fn a_temporary_step_is_labelled_as_machinery() {
        // … same shape, with a step whose `temp` is true …
    }
```

Reuse the module's existing `dir()`, `entry()`, `vivas()` helpers.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL — `ai_rename_plan_modal_text` takes four arguments.

- [ ] **Step 3: Implement**

1. `Modal::AiRenamePlan` gains `plan: Option<norte_proto::methods::FsRenameBatchPlanResult>`
   (`None` while the plan is in flight — the modal opens as it does today and
   fills in).
2. After `ai.rename_plan` returns, the TUI calls
   `backend.rename_batch_plan(dir, &pairs)` and stores the result in the modal.
   A failure there leaves `plan: None` and puts the error in the bar; confirming
   with no plan is refused (there is no hash to send).
3. `ai_rename_plan_modal_text` renders, under the pairs: the collision list
   (one per line, `display_name` masked and middle-elided, same helpers the
   modal already uses), and a line saying whether the plan is applicable.
4. `apply_ai_rename` becomes:

```rust
/// Applies a CONFIRMED AI rename plan (M4-IA) through the transactional batch
/// executor: ONE task, ONE journal unit, rollback on failure. Replaces the old
/// per-pair `fs.move` loop, which had no transaction, no whole-plan collision
/// check, and could not do a permutation at all.
async fn apply_ai_rename(
    app: &mut App,
    backend: &Backend,
    dir: &VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
    plan: Option<&norte_proto::methods::FsRenameBatchPlanResult>,
) {
    let Some(pairs) = norte_frontend::validate_ai_plan(entries) else {
        app.message = Some(t("msg-ai-rename-invalid-plan"));
        return;
    };
    // No plan means no approved hash: refuse rather than guess.
    let Some(plan) = plan else {
        app.message = Some(t("msg-rename-batch-no-plan"));
        return;
    };
    if !plan.executable {
        app.message = Some(t("msg-rename-batch-collisions"));
        return;
    }
    let wire: Vec<norte_proto::methods::RenamePair> = pairs
        .iter()
        .filter_map(|(from, to)| {
            Some(norte_proto::methods::RenamePair {
                from: norte_proto::Segment::new(from.as_bytes().to_vec()).ok()?,
                to: norte_proto::Segment::new(to.as_bytes().to_vec()).ok()?,
            })
        })
        .collect();
    if wire.len() != pairs.len() {
        app.message = Some(t("msg-ai-rename-invalid-plan"));
        return;
    }
    match backend.rename_batch(dir, &wire, &plan.plan_hash).await {
        Ok(task) => {
            app.board.push(task, None);
            app.message = Some(ta(
                "msg-rename-batch-applied",
                &[("n", &wire.len().to_string())],
            ));
        }
        Err(e) => {
            app.message = Some(ta(
                "msg-rename-batch-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
    }
}
```

`validate_ai_plan` returns whatever it returns today (`Vec<(Segment, Segment)>`
or `Vec<(&str, &str)>` — check `crates/norte-frontend/src/lib.rs` and adapt the
conversion above; if it already yields `Segment`s, drop the `filter_map`).

5. New Fluent keys in `i18n/en-US` and `i18n/es-ES`:
   `msg-rename-batch-no-plan`, `msg-rename-batch-collisions`,
   `msg-rename-batch-applied`, `msg-rename-batch-failed`,
   `modal-rename-batch-collision-internal`, `-external`, `-absent-source`,
   `modal-rename-batch-temp-step`, `modal-rename-batch-applicable`,
   `modal-rename-batch-not-applicable`. Follow the naming of the keys already in
   those files and add BOTH locales — the help corpus test fails on a key that
   exists in one locale only.

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui`
Expected: PASS.

- [ ] **Step 5: Drive it by hand**

`scripts/` has a tmux harness for the TUI (see the memory note: piloting the TUI
in tmux surfaces composition bugs a green suite does not). Open the AI rename
modal over a directory with a permutation, confirm, and watch the board show ONE
task. Then undo and watch the names go back.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui i18n
git commit -m "feat(tui): the AI plan is applied as one transactional batch"
```

- [ ] **Step 7: Encoding review**

Ask `encoding-auditor` to review the modal and the pair conversion. The bar is
the one M3-3b T5 set: one path per line, labelled, middle-elided, invisibles
masked, hostile badge, no in-band joiner.

---

## Task 9: the GUI applies through the executor

**Files:**
- Modify: `crates/norte-gui/src/*` (find the `ai_rename` apply path with
  `grep -rn "ai_rename" crates/norte-gui/src`)

- [ ] **Step 1: Find and read the GUI's apply path**

Run: `grep -rn "ai_rename" crates/norte-gui/src`
The GUI has its own modal and its own session thread. `norte-gui` is EXCLUDED
from the workspace, so it builds through its own recipe — check the justfile for
`gui-ci` / `check-gui` and use those, never `cargo` directly (the GPUI
`serde_json/preserve_order` trap is exactly this boundary).

- [ ] **Step 2: Write the failing test**

Mirror the TUI's render assertion in whatever test module the GUI modal has: a
collision is rendered, a temporary is labelled, and confirming without a plan
does not submit.

- [ ] **Step 3: Implement**

Same three changes as the TUI: the modal carries the plan, the plan is fetched
after `ai.rename_plan` returns, and confirmation calls
`Backend::rename_batch(dir, pairs, plan_hash)` instead of looping `move_`.

- [ ] **Step 4: Run the GUI gate**

Run: `just gui-ci`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-gui
git commit -m "feat(gui): the AI plan is applied as one transactional batch"
```

---

## Task 10: the hostile end-to-end, the fixture, and the gate

**Files:**
- Create: `crates/norte-core/tests/e2e_rename_batch.rs`
- Modify: the canonical corpus in `crates/norte-testkit` (find it with
  `grep -rn "corpus" crates/norte-testkit/src | head`)
- Modify: `CHANGELOG.md`
- Modify: `docs/spec/norte-spec.md` if §17's batch-rename bullet needs its
  "transactional undo" claim narrowed to what shipped (it should not — but read
  it and confirm).

- [ ] **Step 1: Add the corpus fixture**

Use the `/fixture` project skill: add a permutation of hostile names to the
canonical corpus (the pin on the corpus count will need updating — the memory
note says it sat at 22). At minimum: two names differing only by an NFD/NFC
spelling, and one non-UTF-8 name, all three participating in one cycle.

- [ ] **Step 2: Write the end-to-end test**

Create `crates/norte-core/tests/e2e_rename_batch.rs`:

```rust
//! The exit criterion for the batch rename executor, over the REAL local
//! provider: a permutation of three files plus a non-UTF-8 name lands
//! byte-exact, and undo puts every name back.

#[cfg(unix)]
#[tokio::test]
async fn a_permutation_with_a_hostile_name_lands_and_undoes_byte_exact() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    // Seed: 1, 2, 3 and a non-UTF-8 name. Contents identify each file so the
    // assertion is about BYTES ending up in the right place, not about names
    // existing.
    // … build the engine over norte-vfs-local with a file-backed SqliteJournal,
    //   plan `1→2, 2→3, 3→1, caf\xff→caf\xfe`, execute, assert contents by
    //   name, then undo_session and assert the original names and contents …
}
```

Fill it in following `crates/norte-core/tests/` conventions for a real-provider
test (there are existing E2E files — `grep -l "tempfile" crates/norte-core/tests`
finds them).

- [ ] **Step 3: Run the gate**

Run: `just ci`
Expected: EXIT=0. Watch two numbers: the test count and coverage. Coverage must
stay at or above 85% — if it dipped, the planner is where cheap coverage lives
(it is pure), so add table cases there rather than contriving executor tests.
Remember `cargo llvm-cov clean` between runs or the number is stale.

- [ ] **Step 4: Changelog**

Add an entry under the unreleased section naming: the two new methods, the
protocol bump to 0.36.0, `TaskKind::RenameBatch`, the two new errors, the
`batch_id` journal migration, and that AI rename now applies as one transaction
(so permutations work and undo is one step).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(core): a hostile permutation lands and undoes byte-exact"
```

- [ ] **Step 6: Final review**

Ask `rust-reviewer` for the whole branch diff and `test-engineer` whether the
cancellation and rollback tests actually prove what they claim (a mutation test:
break the rollback on purpose and confirm a test goes red).

---

## Notes for whoever executes this

**One deviation from the spec, on purpose.** The spec says temporary names are
length-bounded by truncating the base name. The planner here does not embed the
base name at all: the temporary is `.norte-rename-<8 hex of the pairs digest>-<n>`,
which is ~25 bytes always, so the 255-byte component limit cannot be reached and
there is nothing to truncate. The spec has been amended to match.

**The `plan_hash` and the directory.** The planner is pure and never sees a
`VPath`, so the directory is not in `plan_batch`'s hash. That is fine — the hash
travels alongside `dir` in `FsRenameBatchParams` and the core re-plans against
THAT directory. If you find a way for a hash from directory A to be accepted for
directory B, that is a bug: write the test first.

**What not to build.** No rules engine, no batch-rename dialog, no CLI or MCP
surface, no #121. If a task tempts you into one, stop and write it down instead.
