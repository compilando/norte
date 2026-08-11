# Directory comparison implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Shift+F2` compares the two panes and produces an operable diff pane,
where every row says which criterion decided it and how much that criterion is
worth. No mutation, no sync plan — those are the next spec.

**Architecture:** The engine is a new crate, `norte-compare`: a pure function of
two `&dyn Provider` returning a stream of rows, testable exhaustively against
`MemProvider` with no daemon. `norte-core` owns the task, the policy gate and
the batched notification, exactly as `fs.search` does. The walk is depth-first
with an explicit stack, pairing one directory against one directory, because
`fs.list` guarantees no ordering.

**Tech stack:** Rust, `sha2` (already in the tree), `futures::Stream`,
`unicode-normalization` if it is already a dependency — check before adding it.

**Spec:** `docs/superpowers/specs/2026-08-11-directory-comparison-design.md`

---

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-proto/src/methods.rs` | `fs.compare`, `compare.rows`, the four enums, `CompareRow`, the three constants. |
| `crates/norte-proto/tests/golden/types/` | Golden payloads for `CompareRow` and `CompareRowsBatch`. |
| `crates/norte-compare/src/lib.rs` (new) | `compare()`, `CompareOptions`, `Criteria`, `CompareError`. Re-exports the proto row types. |
| `crates/norte-compare/src/key.rs` (new) | The pairing key: NFC, case folding, ambiguity detection. Pure; carries most of the tests. |
| `crates/norte-compare/src/cascade.rs` (new) | One pair in, one `CompareRow` out. Pure, except the hash rung's reads. |
| `crates/norte-compare/src/walk.rs` (new) | The explicit-stack DFS, the directory cap, error rows, cancellation. |
| `crates/norte-compare/src/hash.rs` (new) | Streaming sha256 of one entry through `Provider::read`. |
| `crates/norte-core/src/compare.rs` (new) | `Engine::compare_as`: the task, the batching pump, the coalescing timer. |
| `crates/norte-core/src/daemon/server.rs` | `handle_fs_compare`, the read gate, the notification pump. |
| `crates/norte-core/src/backend.rs` | `Backend::compare` in embedded and remote modes. |
| `crates/norte-frontend/src/compare.rs` (new) | Row presentation: glyphs, columns, filters. Pure, no TTY. |
| `crates/norte-tui/src/app.rs`, `main.rs`, `ui.rs` | `CompareState`, the pane, the keys, the active side. |
| `crates/norte-frontend/src/keymap/catalogue.rs` | `pane.compare-dirs` from `Planned` to `Live`. |
| `crates/norte-i18n/i18n/{en,es}.ftl` | Verdict, confidence and reason strings. |
| `docs/adr/0048-comparison-confidence-on-the-wire.md` (new) | The ADR. |

---

## Task C1: the wire

**Files:**
- Modify: `crates/norte-proto/src/methods.rs`
- Modify: `crates/norte-proto/tests/golden.rs`, `crates/norte-proto/tests/golden_types.rs`
- Create: `crates/norte-proto/tests/golden/types/compare_row_*.json` (follow the naming the directory already uses)
- Create: `docs/adr/0048-comparison-confidence-on-the-wire.md`

Read `methods.rs:1992-2140` first (`VolumeKind`, `Volume`, `HostVolumesResult`).
That block is the shape to copy: `#[serde(other)]`, rustdoc with a doctest, and
the version-bump note.

- [ ] **Step 1: Write the failing tests**

In `methods.rs`'s test module. These four are the ones that matter; the
mechanical round-trips go alongside them.

```rust
/// An N+1 daemon that adds a criterion must not break an N-1 frontend: the
/// unknown token lands in the forward-compat variant, it does not error.
#[test]
fn unknown_enum_tokens_degrade_and_do_not_error() {
    let v: CompareVerdict = serde_json::from_str("\"teleported\"").expect("degrades");
    assert_eq!(v, CompareVerdict::Unknown);
    let c: CompareCriterion = serde_json::from_str("\"vibes\"").expect("degrades");
    assert_eq!(c, CompareCriterion::Unknown);
    let r: CompareReason = serde_json::from_str("\"gremlins\"").expect("degrades");
    assert_eq!(r, CompareReason::Unknown);
}

/// `Unknown` on CONFIDENCE is a VALUE — "the provider cannot say" — so the
/// forward-compat fallback there had to be given a different name. Losing this
/// distinction would turn an honest answer into a protocol mismatch.
#[test]
fn confidence_unknown_is_a_value_not_the_fallback() {
    let known: CompareConfidence = serde_json::from_str("\"unknown\"").expect("a real value");
    assert_eq!(known, CompareConfidence::Unknown);
    let newer: CompareConfidence = serde_json::from_str("\"quantum\"").expect("degrades");
    assert_eq!(newer, CompareConfidence::Unrecognised);
    assert_ne!(known, newer);
}

/// The invariant the wire cannot express: a verdict determines which sides are
/// present. A row that says `OnlyLeft` while carrying a right entry is a bug in
/// whatever produced it, and this is where it gets caught.
#[test]
fn verdict_determines_which_sides_are_present() {
    let e = |p: &str| Entry {
        path: VPath::parse(p).expect("path"),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: Some(0),
        attrs: Default::default(),
    };
    let row = |v, l, r| CompareRow {
        id: 1,
        left: l,
        right: r,
        verdict: v,
        criterion: CompareCriterion::Presence,
        confidence: CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
    };
    let a = || e("file:///a");
    let b = || e("file:///b");
    assert!(row(CompareVerdict::OnlyLeft, Some(a()), None).sides_are_consistent());
    assert!(!row(CompareVerdict::OnlyLeft, Some(a()), Some(b())).sides_are_consistent());
    assert!(row(CompareVerdict::Same, Some(a()), Some(b())).sides_are_consistent());
    assert!(!row(CompareVerdict::Same, Some(a()), None).sides_are_consistent());
}

/// `reason` answers "why" for exactly the two verdicts that have a why.
/// Anywhere else it is noise a client would have to guess about.
#[test]
fn reason_belongs_to_ambiguous_and_error_only() {
    for (verdict, reason, ok) in [
        (CompareVerdict::Ambiguous, Some(CompareReason::CaseFold), true),
        (CompareVerdict::Error, Some(CompareReason::Unreadable), true),
        (CompareVerdict::Ambiguous, None, false),
        (CompareVerdict::Same, Some(CompareReason::CaseFold), false),
    ] {
        let row = CompareRow {
            id: 1,
            left: None,
            right: None,
            verdict,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason,
            side: None,
        };
        assert_eq!(row.reason_is_consistent(), ok, "{verdict:?} + {reason:?}");
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `just t norte-proto`
Expected: FAIL — `CompareVerdict` and friends do not exist.

- [ ] **Step 3: Write the types**

In `methods.rs`, next to the `Volume` block. Every public item needs rustdoc
with a doctest (`#![warn(missing_docs)]` is on in this crate).

```rust
pub const FS_COMPARE: &str = "fs.compare";
pub const COMPARE_ROWS: &str = "compare.rows";
pub const COMPARE_ROWS_MAX_BATCH: usize = 256;
pub const COMPARE_MAX_DIR_ENTRIES: usize = 200_000;

// Every enum below carries these derives and this attribute — the same set
// `VolumeKind` uses at methods.rs:1996-2001:
//   #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
//   #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
//   #[serde(rename_all = "snake_case")]

pub enum CompareVerdict {
    Same, Different, OnlyLeft, OnlyRight, TypeMismatch, Ambiguous, Error,
    #[serde(other)] Unknown,
}

pub enum CompareCriterion {
    Presence, Kind, LinkTarget, Size, Mtime, Hash,
    #[serde(other)] Unknown,
}

pub enum CompareConfidence {
    Certain, Probable, Unknown,
    #[serde(other)] Unrecognised,
}

pub enum CompareReason {
    CaseFold, Normalization, Unreadable, DirTooLarge, ReadFailed,
    #[serde(other)] Unknown,
}

pub enum Side { Left, Right, #[serde(other)] Unknown }

/// Which rungs run. `size` and `mtime` default to true, `hash` to false.
pub struct CompareCriteria { pub size: bool, pub mtime: bool, pub hash: bool }

pub struct CompareRow {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub left: Option<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub right: Option<Entry>,
    pub verdict: CompareVerdict,
    pub criterion: CompareCriterion,
    pub confidence: CompareConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub newer: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub reason: Option<CompareReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub side: Option<Side>,
}

pub struct FsCompareParams {
    pub left: VPath,
    pub right: VPath,
    #[serde(default)] pub criteria: CompareCriteria,
    #[serde(default)] pub max_depth: Option<u32>,
    #[serde(default = "default_mtime_tolerance_ms")] pub mtime_tolerance_ms: i64, // 2000
    #[serde(default)] pub follow_symlinks: bool,
}

pub struct CompareRowsBatch { pub task_id: TaskId, pub rows: Vec<CompareRow> }
```

`sides_are_consistent()` and `reason_is_consistent()` are inherent methods on
`CompareRow`, not `Deserialize` rejections: a malformed row must degrade like a
bad attribute cell, not kill the batch. The daemon asserts them in its own
tests; a client uses them to decide whether to trust a row.

- [ ] **Step 4: Run the tests**

Run: `just t norte-proto`
Expected: PASS.

- [ ] **Step 5: Bump the protocol and add the goldens**

`PROTOCOL_VERSION` goes `0.38.0` → `0.39.0` (additive: a new method, new types,
nothing existing changes shape). Add golden payloads for `CompareRow` — one per
verdict, including an `Ambiguous` row with non-UTF-8 name bytes on both sides —
following whatever the `tests/golden/types/` directory already does.

- [ ] **Step 6: Write ADR 0048**

`docs/adr/0048-comparison-confidence-on-the-wire.md`, MADR, via the `adr` skill.
The decision to record is not the method — it is that **a comparison declares
its own confidence per criterion**, and that `Unknown` is a first-class answer
rather than an error. Include why `CompareConfidence` names its fallback
`Unrecognised`. Consequence to state plainly: every future criterion must
declare what confidence it earns, or the vocabulary rots.

- [ ] **Step 7: Review and commit**

Dispatch `protocol-guardian` (mandatory for `norte-proto`) with the commit
range, and ask it the two things that are genuinely uncertain: whether a new
method plus new types is really a minor bump under this repo's semver gate, and
whether `CompareRow` carrying two whole `Entry` values is the right call against
carrying paths and refetching. Apply BLOCKER and MAJOR findings.

```bash
git add crates/norte-proto docs/adr/0048-comparison-confidence-on-the-wire.md
git commit -m "feat(proto): comparison rows that declare their own confidence"
```

---

## Task C2: the crate, and the pairing key

**Files:**
- Create: `crates/norte-compare/` via the `new-crate` skill
- Create: `crates/norte-compare/src/key.rs`
- Modify: `Cargo.toml` (workspace members), `justfile` if the gate enumerates crates

Dependencies: `norte-proto`, `norte-vfs`, `futures`, `sha2`. Dev: `norte-testkit`,
`tokio` with `macros`/`rt`. **Check whether `unicode-normalization` is already in
the workspace before adding it** — if it is not, justify it in the commit
message per hard rule 8 (`norte-encoding` may already have what is needed; look
there first).

- [ ] **Step 1: Write the failing tests**

In `key.rs`. These encode the whole pairing contract.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// macOS hands out NFD, Linux NFC. The same file copied between them must
    /// pair, and BOTH original byte strings must survive for display — the key
    /// is for pairing and nothing else (rule 1).
    #[test]
    fn nfd_and_nfc_of_one_name_share_a_key() {
        let nfc = "café".as_bytes();          // e-acute as one code point
        let nfd = b"cafe\xcc\x81";            // e + combining acute
        let sensitive = Sides::both_case_sensitive();
        assert_eq!(key_for(nfc, sensitive), key_for(nfd, sensitive));
    }

    /// Bytes that are not UTF-8 are not text, cannot be normalised, and must
    /// pass through untouched rather than through a lossy conversion.
    #[test]
    fn non_utf8_names_pass_through_raw() {
        let raw = b"broken\xff\xfename";
        assert_eq!(key_for(raw, Sides::both_case_sensitive()).as_bytes(), raw);
    }

    /// Case folding is decided by the PAIR, not by one side: a
    /// case-insensitive side cannot hold both spellings, so pairing against it
    /// must fold even when the other side is ext4.
    #[test]
    fn one_case_insensitive_side_folds_the_pairing() {
        let both = Sides::both_case_sensitive();
        assert_ne!(key_for(b"README", both), key_for(b"readme", both));
        let mixed = Sides::right_case_insensitive();
        assert_eq!(key_for(b"README", mixed), key_for(b"readme", mixed));
    }

    /// Two entries on ONE side collapsing to one key is the collision a later
    /// synchronisation has to see BEFORE it writes. They are reported, never
    /// paired, and never silently deduplicated.
    #[test]
    fn same_side_collision_is_reported_with_its_reason() {
        let names: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
        let folded = index_side(&names, Sides::right_case_insensitive());
        assert_eq!(folded.ambiguous_reason(b"README"), Some(CompareReason::CaseFold));
        assert_eq!(folded.ambiguous_reason(b"readme"), Some(CompareReason::CaseFold));
        assert_eq!(folded.ambiguous_reason(b"NOTES"), None);

        let nfd: Vec<&[u8]> = vec!["café".as_bytes(), b"cafe\xcc\x81"];
        let normalised = index_side(&nfd, Sides::both_case_sensitive());
        assert_eq!(
            normalised.ambiguous_reason("café".as_bytes()),
            Some(CompareReason::Normalization)
        );
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-compare`
Expected: FAIL — nothing exists yet.

- [ ] **Step 3: Implement `key.rs`**

`Sides` is built from the two `Capabilities` (`CASE_SENSITIVE` on each).
`key_for` **folds case first and normalises to NFC after**, and only when the
bytes are valid UTF-8. That order is not interchangeable: `J`+U+030C has no
precomposed uppercase, so NFC leaves it alone, and its lowercase `j`+U+030C
composes to U+01F0 `ǰ` — normalising first answers two keys for two names every
case-insensitive volume calls one file. The fold is **case folding**, not
`str::to_lowercase`: `norte-core::rename::plan::fold_delta` is the 22-code-point
delta between the two, and skipping it is issue #129 all over again. `index_side` builds the per-directory map from key to
entries and marks any key holding more than one entry as ambiguous, with the
reason being whichever transformation caused the collapse (fold if the raw bytes
differ only in case, normalisation otherwise).

- [ ] **Step 4: Run the tests**

Run: `just t norte-compare`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-compare Cargo.toml
git commit -m "feat(compare): pairing keys that fold and normalise without losing bytes"
```

---

## Task C3: the cascade

**Files:**
- Create: `crates/norte-compare/src/cascade.rs`
- Modify: `crates/norte-compare/src/lib.rs` (`mod cascade;`, `CompareOptions`, `Criteria`)

The hash rung is Task C5. Here it is a `Criteria` flag that is off, and a pair
that would reach it is left at the mtime rung's verdict.

- [ ] **Step 1: Write the failing tests**

The table in the spec, one assertion per row. Write a small `pair()` helper that
builds two `Entry` values from `(kind, size, mtime)` so the table reads as a
table.

```rust
/// The cascade's whole contract, one row per rung. The confidences are the
/// point: a different size PROVES different bytes, a different mtime only
/// suggests it, and a provider that cannot say leaves `Unknown` rather than
/// having something invented for it.
#[test]
fn the_cascade_decides_and_says_how_sure_it_is() {
    use CompareConfidence::*;
    use CompareCriterion as C;
    use CompareVerdict::*;
    let opts = CompareOptions::cheap();  // tolerance 2000 ms, no hash

    let cases = [
        // (left, right, verdict, criterion, confidence)
        (file(10, 0), file(20, 0), Different, C::Size, Certain),
        (file(10, 0), dir(), TypeMismatch, C::Kind, Certain),
        (file(10, 0), file(10, 1_000), Same, C::Mtime, Probable),
        (file(10, 0), file(10, 5_000), Different, C::Mtime, Probable),
        (file(10, 0), file(10, None), Same, C::Mtime, Unknown),
        (file(None, 0), file(10, 0), Same, C::Size, Unknown),
    ];
    for (l, r, verdict, criterion, confidence) in cases {
        let row = decide(&l, &r, &opts, &no_hash());
        assert_eq!((row.verdict, row.criterion, row.confidence),
                   (verdict, criterion, confidence), "{l:?} vs {r:?}");
    }
}

/// Spec 2 proposes a direction from this field, so it is produced here even
/// though nothing in this spec reads it.
#[test]
fn a_row_that_differs_by_mtime_records_the_newer_side() {
    let row = decide(&file(10, 0), &file(10, 9_000), &CompareOptions::cheap(), &no_hash());
    assert_eq!(row.newer, Some(Side::Right));
}

/// Symlinks are compared, not followed: no cycle detection needed, and a link
/// whose target changed is a real difference.
#[test]
fn symlink_targets_are_compared_as_bytes() {
    let same = decide(&link(b"../a"), &link(b"../a"), &CompareOptions::cheap(), &no_hash());
    assert_eq!((same.verdict, same.criterion), (Same, CompareCriterion::LinkTarget));
    let diff = decide(&link(b"../a"), &link(b"../b"), &CompareOptions::cheap(), &no_hash());
    assert_eq!(diff.verdict, Different);
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-compare`
Expected: FAIL — `decide` does not exist.

- [ ] **Step 3: Implement the cascade**

Descend the rungs in the spec's order and return on the first that decides.
`decide` takes the hash rung as an injected trait object so that C5 plugs into
it without touching this file's tests.

- [ ] **Step 4: Run the tests**

Run: `just t norte-compare`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-compare/src/cascade.rs crates/norte-compare/src/lib.rs
git commit -m "feat(compare): the cheap-to-expensive cascade and what each rung is worth"
```

**Gate checkpoint:** this is the third task. Run `just ci-fast` ONCE here. Do not
run it again until the checkpoint after C6.

---

## Task C4: the walk

**Files:**
- Create: `crates/norte-compare/src/walk.rs`
- Modify: `crates/norte-compare/src/lib.rs` (the public `compare()`)

- [ ] **Step 1: Write the failing tests**

Against `MemProvider` from `norte-testkit`. Build the two trees with a helper;
do not hand-roll six providers.

```rust
/// The base case, and the one a user runs after every copy: two identical
/// trees produce nothing but `Same`, at every depth.
#[tokio::test]
async fn identical_trees_are_all_same() {
    let (l, r) = twin_trees(&["a.txt", "sub/b.txt", "sub/deep/c.txt"]).await;
    let rows = collect(compare_default(&l, &r)).await;
    assert!(rows.iter().all(|row| row.verdict == CompareVerdict::Same), "{rows:#?}");
}

/// A directory that exists on one side only is ONE row, not its whole subtree:
/// the plan will copy it with a recursive `fs.copy`, so enumerating it buys
/// nothing and costs the walk everything.
#[tokio::test]
async fn an_orphan_directory_is_one_row_and_is_not_enumerated() {
    let l = tree(&["only/1.txt", "only/2.txt", "only/deep/3.txt"]).await;
    let r = tree(&[]).await;
    let rows = collect(compare_default(&l, &r)).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, CompareVerdict::OnlyLeft);
    assert_eq!(rows[0].left.as_ref().unwrap().kind, EntryKind::Dir);
}

/// Swapping the sides mirrors the verdicts and nothing else. A comparison that
/// is not symmetric is a comparison that has a favourite.
#[tokio::test]
async fn comparing_the_other_way_round_mirrors_the_verdicts() {
    let (l, r) = trees_that_differ().await;
    let forward = collect(compare_default(&l, &r)).await;
    let backward = collect(compare_default(&r, &l)).await;
    assert_eq!(mirror(&forward), backward);
}

/// One unreadable subdirectory must cost ITSELF, not the other 40 000 leaves.
/// This is the difference between a three-hour comparison that answers and one
/// that dies at the first EACCES.
#[tokio::test]
async fn an_unreadable_directory_is_a_row_and_the_walk_continues() {
    let (l, r) = twin_trees(&["ok.txt", "denied/x.txt", "after/y.txt"]).await;
    l.deny_list("/denied").await;
    let rows = collect(compare_default(&l, &r)).await;
    let bad = rows.iter().find(|row| row.verdict == CompareVerdict::Error).expect("error row");
    assert_eq!(bad.reason, Some(CompareReason::Unreadable));
    assert_eq!(bad.side, Some(Side::Left));
    assert!(rows.iter().any(|row| named(row, b"y.txt")), "the walk stopped at the error");
}

/// A directory over the declared cap costs that directory, not an OOM.
#[tokio::test]
async fn a_directory_over_the_cap_is_a_row_not_an_oom() {
    let (l, r) = twin_trees_with_wide_dir(COMPARE_MAX_DIR_ENTRIES + 1).await;
    let rows = collect(compare_default(&l, &r)).await;
    assert!(rows.iter().any(|row| row.reason == Some(CompareReason::DirTooLarge)));
}

/// Hard rule 3. Cancelling stops the stream — no row after the cut, no work
/// after the cut, and nothing to clean up because nothing is written.
#[tokio::test]
async fn cancelling_stops_the_stream_cleanly() {
    let (l, r) = twin_trees_with_wide_dir(5_000).await;
    let cancel = CancellationToken::new();
    let mut stream = Box::pin(compare(&l, &root(), &r, &root(), CompareOptions::cheap(), cancel.clone()));
    let first = stream.next().await.expect("at least one row");
    assert!(first.is_ok());
    cancel.cancel();
    let rest = stream.count().await;
    assert!(rest < 5_000, "the walk kept going after cancellation: {rest} more rows");
}

/// `max_depth` bounds the descent and says so by not emitting deeper rows.
#[tokio::test]
async fn max_depth_bounds_the_descent() {
    let (l, r) = twin_trees(&["a.txt", "one/b.txt", "one/two/c.txt"]).await;
    let rows = collect(compare_with(&l, &r, CompareOptions::cheap().max_depth(1))).await;
    assert!(!rows.iter().any(|row| named(row, b"c.txt")));
}

/// `Unknown` has to be exercised against a provider that really cannot answer,
/// not a mock told to say so. An archive is read-only and its mtime deserves no
/// trust, which is exactly the case the confidence vocabulary exists for.
#[tokio::test]
async fn a_real_archive_produces_unknown_rather_than_a_guess() {
    let local = local_provider_with(&["a.txt"]).await;
    let zip = archive_provider_from_fixture("one-file.zip").await;
    let rows = collect(compare_default(&local, &zip)).await;
    let row = rows.iter().find(|r| named(r, b"a.txt")).expect("the paired row");
    assert_eq!(row.confidence, CompareConfidence::Unknown);
    assert_ne!(row.verdict, CompareVerdict::Error, "unknown is an answer, not a failure");
}
```

`named(row, b"c.txt")` is the test helper for "either side's entry has this file
name": `VPath` has no `as_bytes`, so it goes through
`path.file_name().map(Segment::as_bytes)`. Write it once in the test support
module and use it everywhere.

The archive test needs `norte-vfs-local` and `norte-vfs-archive` as dev
dependencies of `norte-compare`, and a small zip fixture — reuse one from
`norte-testkit`'s corpus rather than adding another.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-compare`
Expected: FAIL — `walk` does not exist.

- [ ] **Step 3: Implement the walk**

Depth-first with an explicit `Vec` stack of `(left_dir, right_dir, depth)`. Per
directory: drain both listings through their cursors, bail to a `DirTooLarge`
row past `COMPARE_MAX_DIR_ENTRIES`, `index_side` each, merge-join the key sets in
sorted key order (deterministic output is what makes the mirror test possible),
call `decide` per pair, push common subdirectories. Check the
`CancellationToken` once per directory and once per emitted batch of rows.

An orphan `Dir` emits its row and is **not** pushed. A listing that fails emits
an `Error` row for that directory with the failing side, and the walk continues.

- [ ] **Step 4: Run the tests**

Run: `just t norte-compare`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-compare/src/walk.rs crates/norte-compare/src/lib.rs
git commit -m "feat(compare): depth-first walk with bounded memory and errors as rows"
```

---

## Task C5: the hash rung

**Files:**
- Create: `crates/norte-compare/src/hash.rs`
- Modify: `crates/norte-compare/src/cascade.rs` (plug the real hasher in)

- [ ] **Step 1: Write the failing tests**

```rust
/// The point of the rung: same size, same mtime, different bytes. Every cheap
/// criterion says `Same`; only the hash tells the truth. This is the case a
/// user turns hashing on FOR.
#[tokio::test]
async fn same_size_same_mtime_different_bytes_is_caught_only_by_hash() {
    let (l, r) = twin_trees_with_content(&[("x.bin", b"aaaa"), ("x.bin", b"bbbb")]).await;
    let cheap = collect(compare_default(&l, &r)).await;
    assert_eq!(cheap[0].verdict, CompareVerdict::Same);
    assert_eq!(cheap[0].confidence, CompareConfidence::Probable);

    let hashed = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
    assert_eq!(hashed[0].verdict, CompareVerdict::Different);
    assert_eq!(hashed[0].criterion, CompareCriterion::Hash);
    assert_eq!(hashed[0].confidence, CompareConfidence::Certain);
}

/// With hash off, comparison reads no content at all. A user who did not ask
/// to hash a terabyte over SFTP must not be made to.
#[tokio::test]
async fn without_the_hash_rung_no_content_is_read() {
    let (l, r) = twin_trees(&["a.txt", "b.txt"]).await;
    collect(compare_default(&l, &r)).await;
    assert_eq!(l.read_calls(), 0);
    assert_eq!(r.read_calls(), 0);
}

/// The hash only reaches the pairs the cheap rungs called equal. Hashing a
/// pair already known to differ is pure waste.
#[tokio::test]
async fn the_hash_only_runs_on_pairs_the_cheap_rungs_called_equal() {
    let (l, r) = trees_where_one_pair_differs_in_size().await;
    collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
    assert_eq!(l.read_calls(), 1, "only the equal-looking pair should be read");
}

/// A read that fails mid-hash costs its row, not the walk — and says which
/// side failed.
#[tokio::test]
async fn a_read_that_fails_mid_hash_is_an_error_row() {
    let (l, r) = twin_trees_with_content(&[("x.bin", b"aaaa"), ("x.bin", b"aaaa")]).await;
    l.fail_read_after("/x.bin", 2).await;
    let rows = collect(compare_with(&l, &r, CompareOptions::cheap().with_hash())).await;
    assert_eq!(rows[0].verdict, CompareVerdict::Error);
    assert_eq!(rows[0].reason, Some(CompareReason::ReadFailed));
    assert_eq!(rows[0].side, Some(Side::Left));
}
```

If `MemProvider` has no `read_calls`/`fail_read_after`/`deny_list` hooks, add
them to `norte-testkit` in this task — a counter and two injected faults, no
more. C4's `deny_list` has the same requirement; whichever task runs first adds
what it needs.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-compare`
Expected: FAIL.

- [ ] **Step 3: Implement**

sha256 through `Provider::read` in chunks, both sides, comparing digests. Check
the `CancellationToken` once per chunk, not once per file — a 40 GB file must
not make cancellation wait for it.

- [ ] **Step 4: Run the tests**

Run: `just t norte-compare`
Expected: PASS.

- [ ] **Step 5: Review and commit**

Dispatch `rust-reviewer` and `encoding-auditor` over C2–C5 together (one review
round, not three). The specific questions worth asking: whether the pairing key
can lose bytes on any path, and whether cancellation can leave the walk spinning
inside a large directory.

```bash
git add crates/norte-compare crates/norte-testkit
git commit -m "feat(compare): the hash rung, and the reads it declines to make"
```

---

## Task C6: the task, the gate and the wire handler

**Files:**
- Create: `crates/norte-core/src/compare.rs`
- Modify: `crates/norte-core/src/lib.rs`, `crates/norte-core/src/engine.rs`
- Modify: `crates/norte-core/src/daemon/server.rs` (near `handle_fs_search`, line ~2900)
- Modify: `crates/norte-core/src/backend.rs` (near the `SEARCH_HITS_BUF` block, line ~1660 and ~2586)

Read `handle_fs_search` in full first. `handle_fs_compare` is the same shape:
`read_gate`, validate before creating the task, `engine.compare_as`,
`register_task_id` with **zero `.await` in between** (the #64 invariant), then a
pump that routes batches to the owner only.

- [ ] **Step 1: Write the failing tests**

```rust
/// The gate: comparing reads two trees, so an agent needs a live scope over
/// BOTH roots. One is not enough, and the denial says only the coarse category.
#[tokio::test]
async fn an_agent_needs_a_live_scope_over_both_roots() {
    let h = harness().await;
    h.grant_agent_read("/allowed").await;
    let err = h.compare_as_agent("/allowed", "/elsewhere").await.expect_err("denied");
    assert!(matches!(err, Error::PolicyDenied { .. }));
    h.compare_as_agent("/allowed", "/allowed/sub").await.expect("both under scope");
}

/// With hash on, the comparison reads CONTENT, which a listing scope does not
/// cover. Read scope is not content scope, and the gate must not conflate them.
#[tokio::test]
async fn the_hash_rung_needs_content_scope() {
    let h = harness().await;
    h.grant_agent_read("/data").await;
    let err = h.compare_as_agent_with_hash("/data", "/data").await.expect_err("denied");
    assert!(matches!(err, Error::PolicyDenied { .. }));
}

/// Batches are bounded and coalesced, the same contract `search.hits` has: a
/// million rows must not become a million frames.
#[tokio::test]
async fn rows_arrive_in_bounded_batches() {
    let h = harness().await;
    let batches = h.compare_and_collect_batches(wide_tree(1_000)).await;
    assert!(batches.iter().all(|b| b.rows.len() <= COMPARE_ROWS_MAX_BATCH));
    assert!(batches.len() < 1_000, "one frame per row is not coalescing");
}

/// Hard rule 3, at the task boundary: cancelling ends the task as `Cancelled`
/// and stops the batches.
#[tokio::test]
async fn cancelling_the_task_stops_the_batches() {
    let h = harness().await;
    let (task_id, mut rx) = h.compare(wide_tree(5_000)).await.expect("task");
    let first = rx.recv().await.expect("at least one batch");
    assert!(!first.rows.is_empty());
    h.cancel(task_id).await.expect("cancel");
    let mut seen = first.rows.len();
    while let Some(b) = rx.recv().await {
        seen += b.rows.len();
    }
    assert!(seen < 5_000, "batches kept coming after cancel: {seen}");
    assert_eq!(h.task_state(task_id).await, TaskState::Cancelled);
}

/// Two roots that resolve to the same provider and path are a caller bug, and
/// they are refused before a task exists rather than compared against
/// themselves for an hour.
#[tokio::test]
async fn comparing_a_root_against_itself_is_invalid_params() {
    let h = harness().await;
    let before = h.task_count().await;
    let err = h.compare_paths("file:///data", "file:///data").await.expect_err("refused");
    assert_eq!(err.code, codes::INVALID_PARAMS);
    assert_eq!(h.task_count().await, before, "no task may be created");
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-core`
Expected: FAIL.

- [ ] **Step 3: Implement**

`Engine::compare_as` submits the walk as a task and returns
`(TaskHandle, Receiver<CompareRowsBatch>)`, coalescing rows up to
`COMPARE_ROWS_MAX_BATCH` or a flush interval — copy the shape from
`search.rs:577-620` rather than inventing a second one. `handle_fs_compare`
gates both roots, refuses self-comparison with `INVALID_PARAMS`, and pumps
batches to the owning connection. `Backend::compare` covers embedded and remote,
as `Backend::search` does.

It must also refuse **`follow_symlinks: true`** with `INVALID_PARAMS`: the
engine accepts that field and ignores it (C5's review), and accepting a request
the engine will not honour is worse than not offering it. And the stream's only
`Err` is `CompareError::Cancelled`, emitted once at the end — it means the task
is `Cancelled`, not failed. Every other failure is a row.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 5: Review and commit**

Dispatch `protocol-guardian` (the handler is JSON-RPC surface) and
`security-reviewer` (a new read amplifier for agents). Ask specifically whether
requiring content scope for the hash rung is the right boundary and whether the
self-comparison check can be tricked by two paths that resolve to one place.

```bash
git add crates/norte-core
git commit -m "feat(core): fs.compare as a cancellable task behind the read gate"
```

**Gate checkpoint:** run `just ci-fast` ONCE here.

---

## Task C7: the pane

**Files:**
- Create: `crates/norte-frontend/src/compare.rs`
- Modify: `crates/norte-frontend/src/lib.rs`, `crates/norte-frontend/src/keymap/catalogue.rs`
- Modify: `crates/norte-tui/src/app.rs`, `crates/norte-tui/src/main.rs`, `crates/norte-tui/src/ui.rs`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`

`CompareState` mirrors `SearchState` in `app.rs:59-90`. Read that first.

- [ ] **Step 1: Write the failing tests**

In `norte-frontend`, where presentation is testable without a TTY.

```rust
/// §17: textual cues, not colour alone. `Same/Probable` and `Same/Certain` are
/// different answers, and a user who cannot see colour must still be able to
/// tell them apart.
#[test]
fn confidence_is_visible_without_colour() {
    let certain = glyphs(&row(Same, Certain));
    let probable = glyphs(&row(Same, Probable));
    let unknown = glyphs(&row(Same, Unknown));
    assert_ne!(certain, probable);
    assert_ne!(probable, unknown);
    assert_ne!(certain, unknown);
}

/// Filters hide rows; they never renumber them. The selection is anchored to
/// the row id precisely so that a filter cannot move what is selected.
#[test]
fn filtering_hides_rows_without_disturbing_the_selection() {
    let mut pane = pane_with(vec![row_id(1, Same), row_id(2, OnlyLeft), row_id(3, Different)]);
    pane.select(2);
    pane.toggle_filter(Category::Same);
    assert_eq!(pane.visible_ids(), vec![2, 3]);
    assert_eq!(pane.selected_id(), Some(2));
    pane.toggle_filter(Category::Same);
    assert_eq!(pane.visible_ids(), vec![1, 2, 3]);
    assert_eq!(pane.selected_id(), Some(2));
}

/// Actions go to the ACTIVE side, never to a side inferred from the row. On
/// destructive operations, guessing is not a feature.
#[test]
fn actions_target_the_active_side_not_the_inferred_one() {
    let mut pane = pane_with(vec![row_id(1, Different)]);
    pane.select(1);
    assert_eq!(pane.target_path().unwrap(), left_path());
    pane.swap_active_side();
    assert_eq!(pane.target_path().unwrap(), right_path());
}

/// A row with nothing on the active side has no target — the caller gets
/// `None` and must not fall back to the other side behind the user's back.
#[test]
fn an_orphan_row_has_no_target_on_the_empty_side() {
    let mut pane = pane_with(vec![row_id(1, OnlyLeft)]);
    pane.select(1);
    pane.swap_active_side();
    assert_eq!(pane.target_path(), None);
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL.

- [ ] **Step 3: Implement the presentation, then the TUI**

`norte-frontend::compare` holds rows, filters, selection by id, the active side
and the glyph/column mapping. `norte-tui` holds `CompareState`, the launch key,
the filter keys, the side-swap key, and painting. All strings through Fluent in
both locales — verdicts, confidences, reasons, filter names.

The catalogue entry `pane.compare-dirs` flips `Planned` → `Live`, default
`Shift+F2`, with each preset mapping its own binding. It must appear in the
reference sheet, not greyed out.

- [ ] **Step 4: Run the tests, then drive it**

Run: `just t norte-frontend` then `just t norte-tui`
Expected: PASS.

Then drive the real TUI under the tmux harness against two directories that
differ in every category. The suite does not catch composition bugs; the harness
does.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend crates/norte-tui crates/norte-i18n
git commit -m "feat(tui,frontend): an operable diff pane with confidence on every row"
```

---

## Task C8: closing the branch

**Files:**
- Modify: `docs/superpowers/specs/2026-08-07-post-alpha-roadmap.md` (item 1's built-note)
- Modify: `ARCHITECTURE.md` (the new crate)
- Modify: `CHANGELOG.md` if the repo keeps one per release

- [ ] **Step 1: File the GUI debt**

One issue: the GUI has no compare pane. Reference this plan and the spec. **File
it the day this lands** — #147 exists because that did not happen.

- [ ] **Step 2: Update the roadmap and ARCHITECTURE.md**

Item 1 gets a built-note in the shape items 2–5 already use: what was built,
what was deliberately not, the protocol version, the ADR, and the debt filed.
Say plainly that specs 2 (the sync plan) and 3 (CLI/MCP/GUI) are still open.

- [ ] **Step 3: Run the full gate ONCE**

Run: `just ci`
Expected: green. `norte-compare` is a new crate — check whether it falls under
the 85% coverage gate and, if it does, that it clears it.

- [ ] **Step 4: Commit and finish the branch**

```bash
git add -A
git commit -m "docs: directory comparison lands, and what it deliberately is not"
```

Then use `superpowers:finishing-a-development-branch`.

---

## Notes for whoever executes this

- **The gate is billed per plan.** Two `just ci-fast` runs (after C3 and C6) and
  one `just ci` (C8). The RED→GREEN loop is `just t <crate>` and nothing else.
- **Reviewers do not compile.** They read the diff.
- **Review rounds are batched**: C1 has its own (the wire is expensive to get
  wrong), C2–C5 share one, C6 has its own.
- Do not use `sleep` or `timeout … tail -f /dev/null` anywhere, for any reason.
