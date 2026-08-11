# Directory comparison — design

**Date:** 2026-08-11
**Status:** approved
**Roadmap item:** 1 of `2026-08-07-post-alpha-roadmap.md`, spec 1 of 3

## Why

Comparing two directories is the capability an orthodox file manager is judged
on, and the one §17 names that has never been built. Nothing in the tree
answers "are these two trees the same?", which is also the honest answer to
"did the copy work" — the question a user asks after every transfer and the
one an agent should be able to ask itself.

The machinery is nearly all here already: two panes with independent listings,
first-class selection that survives sorts and refreshes, a task scheduler with
progress and cancellation, a policy gate, and `sha2` in the dependency tree.
What is missing is the middle.

## Scope

Roadmap item 1 is three independent subsystems, and this spec is the first:

1. **This spec.** The comparison engine, its wire surface, and an operable
   diff pane in the TUI.
2. **Spec 2.** The synchronisation plan as a first-class wire type, approved
   and executed as one journalled, undoable unit.
3. **Spec 3.** The CLI, MCP and GUI surfaces.

In:

- A streaming, cancellable comparison of two directory trees across any two
  providers.
- A cheap-to-expensive criterion cascade where every decision records the
  criterion that produced it and the confidence that criterion earns.
- `fs.compare` as a task, `compare.rows` as its batched notification.
- A virtual diff pane in the TUI: two faces per row, filters by category,
  first-class selection, and the file operations that already exist.

Out, and deliberately so:

- The synchronisation plan and its execution (spec 2). This spec produces no
  mutation and writes nothing.
- CLI, MCP and GUI surfaces (spec 3). **The GUI gap is filed as debt the day
  this lands**, not later — #147 is what happens when it is not.
- Following symlinks. Link targets are compared as bytes instead, which makes
  cycle detection unnecessary.
- `mtime_granularity_ns` on `Capabilities`. Touching the capability wire for a
  single consumer is not worth it; the tolerance is a request parameter.

## Architecture

### Crates

**`norte-compare`** (new, via the `new-crate` skill) holds the engine. It
depends on `norte-vfs` (`Provider`, `Entry`, `Capabilities`, `VPath`),
`norte-proto` (row types), `sha2` and `futures`, with `norte-testkit` as a dev
dependency for `MemProvider` and the hostile corpus. It knows nothing about the
daemon, the policy engine or the scheduler: it takes two `&dyn Provider` and
returns a stream of rows.

```rust
pub struct CompareOptions {
    /// Which rungs of the cascade run. Hash is opt-in.
    pub criteria: Criteria,
    pub max_depth: Option<u32>,
    /// Default 2000 — the FAT rule, and the widest real granularity.
    pub mtime_tolerance_ms: i64,
    /// Default false. See "Out".
    pub follow_symlinks: bool,
}

pub fn compare<'a>(
    left: &'a dyn Provider,
    left_root: &'a VPath,
    right: &'a dyn Provider,
    right_root: &'a VPath,
    opts: CompareOptions,
    cancel: CancellationToken,
) -> impl Stream<Item = Result<Row, CompareError>> + 'a;
```

A stream, not a `Vec`: how much is accumulated is the core's decision, not the
engine's.

**`norte-core`** owns the task — scheduler, `CancellationToken`,
`task.progress`, batching, the policy gate over both roots, and the
engine→wire mapping. **`norte-proto`** owns the row types. **`norte-frontend`**
holds the pane's presentation, **`norte-tui`** its keys and painting.

A separate crate rather than a `norte-core` module because the engine is a pure
function of two providers and can be tested exhaustively against `MemProvider`
without a daemon, the way `norte-index` and `norte-ai` already are.

### The walk

`FsListResult.entries` documents its order as "the provider's, no guarantee",
so there are no two sorted streams to merge-join. Pairing is therefore
**directory by directory**: drain `fs.list` on both sides of one directory,
sort by the pairing key, merge-join, emit the rows, push the common
subdirectories.

Depth-first with an **explicit stack**, not async recursion — no boxed future
per level, and no blown stack on a deep tree. Memory is the two directories
being paired plus the stack depth.

One directory with millions of entries would still be O(n) in RAM, so
`COMPARE_MAX_DIR_ENTRIES` (200 000) is a declared limit: a directory over it
emits an error row for *that directory* and the walk continues, instead of
dying by OOM.

### Pairing

The key is **the bytes of the name**, under two transformations, **in this
order**:

1. **Case folding**, when *either* side does not declare `CASE_SENSITIVE`. A
   case-insensitive side cannot hold both spellings, so folding is what
   pairing against it means. Case FOLDING, not `str::to_lowercase` — the two
   diverge on 22 code points (final sigma, `U+00B5`, `U+017F`, the Greek
   symbol variants, the historic Cyrillic letterforms, `U+0345`, `U+FB05`),
   which is issue #129 and `fold_delta`'s reason to exist.
2. **NFC**, when the bytes are valid UTF-8. macOS hands out NFD; comparing in
   NFC while preserving the original bytes is the repo's standing rule.
   Non-UTF-8 bytes pass through raw — and are never folded either, because a
   DBCS trail byte lands where `A`–`Z` live.

The order is load-bearing: folding can COMPOSE what normalising left
decomposed (`J`+U+030C lowercases to `j`+U+030C, whose NFC is `ǰ`), so NFC has
to come second.

Each side's original bytes travel in the row. The key is for pairing only —
never for display, never for operating.

Two entries **on the same side** that collapse to one key (`README` and
`readme` seen against APFS; NFC and NFD of one name on ext4) are **not**
paired: they emit `Ambiguous` with a reason (`CaseFold` or `Normalization`) and
`Unknown` confidence. That is precisely the collision a later synchronisation
must see before it writes anything.

Same key with a different `EntryKind` is `TypeMismatch`.

## The cascade

One pass. When the merge-join reaches a pair it descends the cascade until a
rung decides, and the row is emitted **final** — no row is ever corrected
later, so the wire needs no row updates and the pane no reconciliation.

| rung | condition | verdict | confidence |
| --- | --- | --- | --- |
| Presence | one side missing | `OnlyLeft` / `OnlyRight` | `Certain` |
| Kind | `EntryKind` differs | `TypeMismatch` | `Certain` |
| Symlink | targets, as bytes | `Same` / `Different` | `Certain` |
| Size | both known, differ | `Different` | `Certain` — a different size is different bytes |
| Size | one unknown | `Same` | `Unknown` |
| Mtime | \|Δ\| > tolerance | `Different` | `Probable` — a different date does not prove different bytes |
| Mtime | \|Δ\| ≤ tolerance | `Same` | `Probable` |
| Mtime | unknown on a side | `Same` | `Unknown` |
| Hash | streaming sha256 of both | `Same` / `Different` | `Certain` |

Hash runs only when the caller asked for it, and only reaches the pairs the
cheap rungs called equal — which is literally "verify what looks the same".
With hash off, the comparison reads no file content at all.

A `Different`-by-mtime row also records **which side is newer**. Nothing in
this spec uses it; spec 2 needs it to propose a direction, and producing it
here costs nothing.

`Unknown` is not a failure, it is an honest provider. An archive with no
trustworthy mtime, or an object store whose ETag is a hash only sometimes,
yields `Same`/`Unknown`, and the pane paints that differently from
`Same`/`Certain`. That distinction is the point of the whole item.

## Wire

Additive. No existing type changes shape, so an N-1 client cannot tell.

```rust
pub const FS_COMPARE: &str = "fs.compare";       // → FsTaskResult { task_id }
pub const COMPARE_ROWS: &str = "compare.rows";   // notification, batched
pub const COMPARE_ROWS_MAX_BATCH: usize = 256;   // mirrors SEARCH_HITS_MAX_BATCH
pub const COMPARE_MAX_DIR_ENTRIES: usize = 200_000;

pub struct CompareRow {
    /// Monotonic. The pane's selection anchors to it.
    pub id: u64,
    /// The whole `Entry`: the pane paints size, date and the name's bytes.
    pub left: Option<Entry>,
    pub right: Option<Entry>,
    pub verdict: CompareVerdict,
    pub criterion: CompareCriterion,
    pub confidence: CompareConfidence,
    /// Which side is newer, when mtime decided the row.
    pub newer: Option<Side>,
    /// Why, for the two verdicts that need a why: `Ambiguous` and `Error`.
    /// `None` for every other verdict.
    pub reason: Option<CompareReason>,
    /// The side a `reason` applies to, when it applies to one — a read that
    /// failed on the left only.
    pub side: Option<Side>,
}
```

`CompareVerdict` is `Same`, `Different`, `OnlyLeft`, `OnlyRight`,
`TypeMismatch`, `Ambiguous` and `Error`; `CompareCriterion` is `Presence`,
`Kind`, `LinkTarget`, `Size`, `Mtime` and `Hash`; `CompareConfidence` is
`Certain`, `Probable` and `Unknown`; `CompareReason` is `CaseFold`,
`Normalization`, `Unreadable`, `DirTooLarge` and `ReadFailed`.

All four carry a `#[serde(other)]` fallback, as `TaskKind` and `VolumeKind`
do: an N+1 daemon that adds a criterion does not break an N-1 frontend. On
`CompareVerdict` and `CompareCriterion` that variant is named `Unknown`, the
house convention. On `CompareConfidence` it is named `Unrecognised`, because
there `Unknown` is already a meaningful value — "the provider cannot say" and
"a newer peer said something I do not know" are different facts and must not
share a name.

**Errors are rows, not the end of the task.** An unreadable subdirectory, a
directory over the entry limit, a read that fails mid-hash: `verdict: Error`
with a typed cause, and the walk continues. A three-hour comparison must not
die on an `EACCES` at leaf 40 000 — and the user is entitled to know exactly
where the answer is unknown.

Two roots that resolve to the same provider and path are `-32602`.

An invariant the wire cannot express and a test can: `OnlyLeft` implies
`right: None`, `Same` and `Different` imply both sides present. Golden plus a
validity test in `norte-proto`. Likewise `reason` is `Some` for exactly two
verdicts, `Ambiguous` and `Error`, and `None` for every other.

This needs a **minor protocol bump, new goldens and an ADR**. The ADR is not
for breaking the format — nothing breaks — but because "confidence declared per
criterion" is new wire vocabulary, and that decision belongs where it will be
looked up rather than in the body of a PR. That is the lesson of #131.

## The pane

The virtual-pane mechanism of live search (`Alt+F7`): state in
`norte-tui::app` (`CompareState { Running, Completed, Failed }` plus the
accumulated rows), presentation in `norte-frontend`, pure and testable without
a TTY.

- **Two faces per row**, with a glyph for the verdict and one for the
  confidence. Glyphs, not colour alone: §17 requires textual cues, and
  `Same`/`Probable` versus `Same`/`Certain` is exactly the distinction a
  colour-blind user must not lose.
- **Filters by category**: only-left, only-right, different, same, problems.
  Toggled by key.
- **First-class selection anchored to the row `id`**, surviving filters and
  ordering — and inherited as-is by spec 2 to seed the plan.
- **Active side.** The pane that launched the comparison is the left one; the
  ordinary actions (view, edit, copy, move, delete) apply to the active side,
  with a key to swap it. No implicit "the side the row suggests": that is
  guessing on destructive operations.
- **Opening a row** navigates to the real directory, which is how an orphan the
  walk did not enumerate gets expanded.
- **Keymap**: a catalogue entry with `availability` and which-key, defaulting to
  `Shift+F2` (Total Commander's), each preset mapping its own. It appears in
  the reference sheet the day it lands, not greyed out.

User-facing strings through Fluent in `i18n/`, English and Spanish.

## Policy, journal, cancellation

**Policy.** Comparing reads two whole trees, and with hash it reads *content* —
more than a listing reveals. `fs.compare` requires read scope over both roots,
and the hash rung requires content scope. Unlike `host.volumes` it is available
to agents: "did the copy work" is exactly the question an agent should be able
to ask, and under the gate it discloses nothing it could not already list.

**Journal.** Comparison mutates nothing, so it does not enter the journal. Hard
rule 4 does not apply here, and saying so in writing keeps a reviewer from
asking for an entry that would mean nothing.

**Cancellation.** Hard rule 3: the `CancellationToken` is checked per directory
and per hashed chunk, with a clean-cancellation test — not one row after the
cut, the task in `Cancelled`, and nothing temporary to clean up because nothing
is written.

## Tests

- `norte-compare` against `MemProvider`: a synthetic tree pair per rung of the
  cascade, and `norte-testkit`'s hostile corpus for the names — NFD/NFC, case,
  non-UTF-8 bytes, control characters.
- Properties: A against A is all `Same`; A against B and B against A are mirror
  verdicts (`OnlyLeft` ↔ `OnlyRight`); memory does not grow with depth beyond
  the stack.
- Cross-provider: local against `norte-vfs-archive` (read-only, with an mtime
  that deserves no trust) so `Unknown` is exercised for real rather than
  simulated.
- `norte-proto`: goldens and the verdict↔sides invariant.
- `norte-core`: bounded batches, an error row that does not kill the task,
  clean cancellation.
- TUI: pure presentation in `norte-frontend`; composition under the tmux
  harness, which is the only thing that surfaces painting bugs.

## Definition of done

Code, unit and cross-provider tests, rustdoc with doctests on the new public
items in `norte-proto` and `norte-compare`, the ADR, the protocol bump and
goldens, Fluent strings in both locales, the keymap catalogue entry, and the
GUI debt filed. `just ci` green.
