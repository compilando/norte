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
| 2 — `descend_orphans` | done | `6cb6cd4` |
| 3 — `norte-sync` + the Update transducer | done | `aa2242c` |
| 4 — reversal, `on_unknown`, the `Skip` reasons, `dest_rel` | done | `02f95cd` |
| 5 — `Mirror`, `Ambiguous`, blockers, the overlap guard | done | `fb7a37a` |
| 6 — the streaming `plan_hash` and the counters | done | `19b7408` |
| 7 — the spool | done | `ca9f791` (rename) + `0969203` (spool) |
| 8 — `sync.plan` as a task | done | `8174c6d` |
| 9 — the executor | done | `d749166` (containment) + `26f26d3` (executor) |
| 10 — `sync.apply`, `sync.report`, the `Backend` | done | `15a36f3` (cross-provider tests) + `40f87d5` |

**The embedded TUI does not synchronise, and Task 13 says so out loud.**
`make_backend` builds `Engine::new()` — no journal, no spool — and Task 9 made
`sync.apply` require a journal, fail-closed. That is the right answer rather
than an accident: synchronising is strictly more dangerous than copying, and
hard rule 4 has no exception for the convenient transport. So Task 13 does
**not** wire a journal into the embedded engine; it renders `pane.sync-dirs`
through the keymap catalogue's existing `availability` machinery with a reason
the user can act on — synchronisation needs the daemon, because it has to be
journalled and undoable. Task 14 files the larger question (an embedded
backend performs mutations no journal records) as its own issue; it predates
this branch and changes every mutation, not just this one.

**Task 11 is not optional and it must land before this branch merges.**
`revert_batch` demands every entry of a batch be `rename_back` and answers
`Blocked` otherwise, and `undo_session` is strict LIFO. So as of Task 9,
after any `sync.apply`, session undo is dead for that user. Nothing in Tasks
10, 12 or 13 repairs it.

**A proto bump breaks tests outside `norte-proto`.** Task 1 ran only
`just t norte-proto` and left two `norte-core` tests red on the branch — both
hardcode a protocol version. After any change to `PROTOCOL_VERSION`, run
`just t norte-core` as well.

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

### What Task 2 changed in this plan

- **`descend_orphans` is a `DescendSide` on the wire, not an `Option<Side>`.**
  New enum in `methods.rs`: `{ Left, Right }`, `#[non_exhaustive]`, **no**
  `serde(other)`, plus `impl From<DescendSide> for Side`. Both
  `FsCompareParams` and `SyncCompareOptions` carry `Option<DescendSide>`;
  `norte_compare::CompareOptions` keeps `Option<Side>` and the engine converts.
  The plan's INVALID_PARAMS check in `handle_fs_compare` **is gone** — the type
  refuses `"lft"` and `"unknown"` in the deserialiser of every peer, which the
  handler could not do for `CoreBackend::Embedded` (it calls the engine without
  passing through the daemon at all). Task 8 still has to refuse the field in
  `sync.plan`, but only because it is not the caller's there — not because a
  value could be malformed.
- **`SyncCounts::bytes` cannot be summed from the rows, and the fix is a
  second counter, not a `stat` pass.** An orphan row is never hydrated (#157)
  and `norte-vfs-local` lists with `size: None`, so on `file://` every
  descended row carries no size. Hydrating costs 2N chained round trips over a
  network mount (#156) and would put provider I/O inside a transducer whose
  whole value is that it has none. So **`SyncCounts` gains
  `unmeasured_steps: u64`** — how many steps carry no size — and the approval
  dialog reads "1.2 GB + 340 files of unknown size" rather than a confident
  zero. This is the rule ADR 0048 already set for this feature: "the provider
  cannot say" is an answer, not an error, and it never hides inside a number
  that looks certain. Hydration stays available as a later optimisation under
  #156, and it would only shrink `unmeasured_steps`, never change the shape.
  Proto 0.40.0 is unreleased on this branch, so the field costs nothing.
  Task 6 counts it, Task 12 renders it.
- **The container row of a descended orphan still comes out**, before its
  children, and carries no marker saying the subtree follows. That is
  deliberate: descending is a parameter of the REQUEST, so the caller already
  knows. Task 3's mapping is unaffected — a dir row is `CreateDir`, never a
  recursive copy.
- **Inside an orphan the pairing key still folds with BOTH sides'
  capabilities.** Two names the destination could not tell apart come out
  `Ambiguous` inside a source-side orphan, and their subtree is not descended.
  That is what Task 5's `AmbiguousSource` → `Skip` rule will see.
- Two tests that Task 1's bump had left red are fixed here
  (`initialize_rechaza_version_incompatible`,
  `frames_hostiles_y_formas_canonicas_crudas`): both hardcoded a protocol
  version string. Neither has anything to do with sync.

### What Task 3 changed in this plan

**`SyncError` has five variants, not one.** The plan's `Cancelled` is there;
the other four all say the same thing in different words — *the caller wired
this wrong, and a plan that looks approvable is worse than no plan*:

- `SourceSideUnknown` — `source_side` is a `Side`, and `Side` carries
  `#[serde(other)] Unknown`. Planning nothing, silently, is the trap Task 2
  documented for `descend_orphans`, one layer down.
- `OutsideRoot { root, path }` — a row whose path does not hang from the root
  it was measured against. Both `VPath`s are `Box`ed (`result_large_err`).
- `RootIsNotAStep { root }` — a row whose `rel` would be the ROOT. Reachable
  from a caller whose `SyncOptions` roots are deeper than the compare roots,
  and from the error row the walk emits when it cannot list the root itself.
  An acting step there means "the whole destination tree". **Task 4 must let a
  `Skip` carry a root `rel`** — the guard is in `absorb`, not in `rel_under`,
  precisely so it can.
- `ModeNotPlanned(SyncMode)` — `Mirror` is refused rather than served the
  `Update` plan, which is a strict SUBSET of it. **Task 5 deletes this
  variant** when it implements the mode.
- `Compare(CompareError)` — so a future `CompareError` variant is not
  mistranslated to "cancelled" by a wildcard arm.

**Task 4's reversal table is already implemented and already green.** The full
`(kind, dest_has_trash)` function landed in Task 3 because it had to:
`SyncStep::shape_is_consistent()` forbids a non-`Skip` step with no reversal,
so an `Overwrite` could not be emitted at all without deciding the trash
branch, and hardcoding `RestoreTrash` puts a false promise on the wire before
a human approves it. Task 4's step-1 tests for it will pass on arrival; its
real work is `on_unknown`, the `Skip` reasons and the `Error` rows.

**Two bugs the reviewers found, and how they are resolved.** Both are pinned
by a test asserting today's behaviour, and neither may reach Task 9. Both
resolutions are decided; the tests that pin the old behaviour get replaced.

**Bug 1 — a folded pair with different bytes gets the SOURCE's name** (#152's
reachable edge). `norte-compare` pairs by a key that NFC-normalises always and
case-folds when either side is case-insensitive, and the row carries no
marker. `rel` comes from the source entry, so an `Overwrite` of an NFC `café`
against an NFD `café` writes a SECOND file on ext4. Its `RestoreTrash`
reversal is a lie — nothing was buried — so undo cannot repair it, and under
`Mirror` the original is not an orphan either, because it paired.

**Resolution — `SyncStep` gains `dest_rel: Option<RelPath>`, `Some` only when
the destination's name bytes differ from the source's.** The row already
carries both `Entry`s, so the transducer has both names and `norte-compare`
needs no change. The executor reads `source_root + rel` and writes
`dest_root + dest_rel.unwrap_or(rel)` — the file that exists, not the one the
source spells. The trash reference and the reversal then describe something
real. The destination is **not** renamed to the source's spelling: renaming on
a normalisation difference is the macOS↔Linux churn this repo exists to avoid.
Task 4 lands the field, the transducer's emission of it, and the tests; the
pane shows both names when they differ.

**Bug 2 — a `TypeMismatch` involving a directory becomes one `Overwrite`**,
which normatively means "trash and copy bytes", with no `EntryKind` on the
step and the subtree absent from the plan entirely.

**Resolution — a directory on either side is a blocker; anything else keeps
overwriting.** New `SyncBlockerKind::TypeMismatchDir`. Replacing a tree with a
file, or a file with a tree, is a destructive structural change that deserves
a human, and the spec never promised it. File-against-symlink stays an
`Overwrite`, because that is plain byte replacement and is correct;
symlink-against-symlink never reaches here at all — spec 1 decides it on the
target bytes. Task 5 lands it with the other blockers.

**A third gap: destination name legality — deliberately out of scope.**
Nothing checks that a name legal under the source root is legal under the
DESTINATION root: 86 NFC `é` (172 bytes) become 258 under NFD and blow
`NAME_MAX`; `CON`, a trailing dot and `f:ads` are not names on Windows (the
last writes an alternate data stream and "succeeds"). Validating this at
planning time means modelling every destination filesystem's naming rules,
which `Capabilities` does not carry and this spec did not budget.

So it stays an **execution** failure, which the design already accommodates —
a failed step is a report row and the task continues. What is missing is
precision, and that is cheap: **Task 9 adds `SyncFailureCause::IllegalName`**
so the report names the real cause instead of a generic `Io`, and **Task 14
files an issue** for planning-time validation, cross-referencing this
paragraph. Do not silently let it surface as `Io`.

**The manifest is not the plan's.** `norte-vfs` is a dev-dependency, not a
dependency — the transducer never talks to a provider, which is its whole
point — and `sha2`/`proptest` are not there at all: they arrive with `hash.rs`
and `tests/props.rs` in Task 6. `bytes` and `norte-vfs` are dev-deps for the
one test that drives REAL `norte-compare` rows through the transducer
(`real_compare_rows_plan_without_a_single_outside_root`); the other 23 build
their rows by hand, and none of them can touch a contract that spans two
crates.

**The stream is `FusedStream`, not `Stream`.** `futures`' raw `Unfold` panics
if polled once past its end, which is what any `select!` with a flush tick
does — and Task 8's notification pump is exactly that shape. `plan()` returns
a fused stream and a test polls it twice past the end. The same unfused
construction is in `norte_compare::walk`; worth an issue there.

**Still open, deliberately.** The token is checked once per row rather than
raced against `rows.next()`, so `plan()` requires the SAME token as the walk
— stated in its rustdoc, and how `norte-core`'s `run_compare` already wires
it. Racing it would mean a `tokio::select!` and a new runtime dependency
(rule 8) for a case the caller controls. And `!` (0x21) is absent from all 47
names in the canonical corpus although it is the ADR 0018 archive marker and a
legal Unix filename; adding it is its own change, because every
`hostile_names().len() == 47` assertion in the workspace moves with it.

### What Task 4 changed in this plan

**Task 3's report was right: the reversal table arrived green.** All four rows
of the `(kind, dest_has_trash)` table and the three step-1 tests that cover
them were already passing when task 4 started. Task 4's real content was
`on_unknown`, the `Skip` reasons, the `Error` rows and `dest_rel`.

- **`SyncStep::dest_rel: Option<RelPath>` compares the WHOLE relative path, not
  the last segment.** The task text said "`Some` only when the destination
  entry's NAME bytes differ". That predicate has a hole: the pairing key folds
  at EVERY level, so `café/x.txt` on the source can hang off an NFD `café` on
  the destination while `x.txt` is spelt identically — and pasting `rel` over
  `dest_root` then names a directory that does not exist on ext4. The
  transducer computes `rel_under(dest_root, dest.path)` and populates the field
  when it differs from `rel` byte for byte. `encoding-auditor` confirmed the
  generalisation is required, not extra, and that with it the target is exact
  by construction: `dest_root + dest_rel.unwrap_or(rel)` IS `dest.path`.
- **The other half of #152 is still open, and is pinned rather than fixed.** A
  file that exists only on the SOURCE, under a paired directory whose two
  spellings differ, gets `dest_rel: None` — the row carries no destination
  entry, and the directory pair itself is a `Same` row that produces no step,
  so the destination's spelling is nowhere in the plan. On ext4 the executor
  then grows a second `café/`. Closing it needs a prefix stack —
  `Vec<(RelPath source, RelPath dest)>` recorded on paired-dir rows BEFORE the
  three early returns, popped by prefix as the pre-order walk leaves a subtree
  — which is new transducer state that lands next to Task 5's overlap guard.
  **Task 5 decides whether to build it; Task 9 must not assume it is closed.**
  Pinned by `a_copy_under_a_folder_the_two_sides_spell_differently_still_takes_the_source_spelling`.
- **`rel` is not always relative to the source root, and now says so.** A
  destination-only `Error` row (an unlistable destination directory) produces a
  `Skip` whose `rel` is measured against `dest_root`; Task 5's `DeleteTree`
  inherits the same convention. `SyncStep` carries no side field — adding one
  for two shapes that do not write was not worth a wire field — so a pane that
  anchors every `rel` to the source column will paint those two in the wrong
  place. Stated normatively in `SyncStep::rel`'s rustdoc. **Task 12 has to
  handle it, and Task 14 must fix ADR 0049 line 115, which still says a step's
  path is relative to the two roots.**
- **Every `Error` row is a `Skip` with `SyncReason::Unreadable`**, whatever
  `CompareReason` it carried — `Unreadable`, `ReadFailed` and `DirTooLarge` are
  three ways of "the walk could not answer for this entry", and none authorises
  a write. Task 5 takes the one case that is genuinely different out of here:
  a `DirTooLarge` on the DESTINATION is a blocker, not a skipped step.
- **`on_unknown` breaks the tie only on `Same`.** A `Different`/`Unknown` is an
  `Overwrite` either way, and an orphan with `Unknown` confidence is still a
  `Copy`: not copying a file that is definitely absent because "nobody could
  verify it" would be skipping a CERTAIN fact. `OnUnknown` is
  `#[non_exhaustive]`, so the wildcard falls on the side of NOT writing.
- **`Ambiguous` rows still produce nothing at all, and that is load-bearing
  for Task 5.** The reason two source spellings that the destination folds
  together cannot overwrite each other is that `norte-compare` reports them as
  `Ambiguous`. Until Task 5 turns those into a `Skip` (source) or a blocker
  (destination), they leave the plan without a step AND without a blocker —
  invisible. Task 5 is what makes that safe; it is not optional.
- **A golden cannot freeze an NFC/NFD pair.** Both forms are valid UTF-8 and
  the segment codec leaves valid UTF-8 literal, so the two strings render
  IDENTICALLY in a checked-in JSON file: the diff would be unadjudicable and
  one editor normalisation would turn the test into a tautology. The golden
  freezes a case-folded ANCESTOR (`NOTAS/informe%FF%FE.dat` against
  `notas/…`); the NFC/NFD half is pinned in `types.rs` with `\u{e9}`/`\u{301}`
  escapes. Note for anyone writing another one: a non-UTF-8 name is NEVER
  folded (`key_for` returns it raw, so Shift-JIS tail bytes survive), so a pair
  differing only in the case of a non-UTF-8 leaf cannot exist.
- **Suggested and NOT done, for whoever wants it:** `norte-testkit` has no
  `corpus::spelling_twins()`, so the NFC/NFD and case-fold twin pairs are
  hardcoded in `norte-compare::key` and would be hardcoded a second time by any
  exhaustive `dest_rel` test. A shared fixture is its own change (it moves
  every `hostile_names().len() == 47` assertion's neighbourhood), so task 4
  used the plain corpus instead.

### The overlap guard is overclaimed, and Task 8 gets the missing half

`rust-reviewer` caught this on Task 5 and it is correct: **none** of the three
examples ADR 0048 and the sync spec cite for the walk-time overlap guard is
actually caught by it. Walking `/data` (a symlink to `/srv/data`) against
`/srv/data` produces rows whose paths all hang from `/data`, so no row ever
"reaches the other root". The same goes for one SFTP host under two
authorities and one archive opened by two paths.

What the walk-time guard really is: **defence in depth against a provider that
returns paths outside the root it was asked to list.** That is worth having
and it is not what was advertised.

The missing half is cheap and Task 8 lands it: **compare `Provider::node_id`
of the two roots.** Spec 1 already put it on the trait — `(dev, ino)` on
local, `FILE_ID_INFO` on Windows — so two roots that are the same directory
under different spellings answer with the same id no matter how they were
written. One `stat` per root, once per plan. Equal and `Some` →
`Error::OverlappingRoots { overlap: RootOverlap::Same }`.

It does **not** close everything, and the plan says so rather than repeating
the overclaim: `node_id` is `None` on SFTP and FTP, so one host under two
authorities stays undetected. The residual is bounded by the executor's
per-step revalidation and by the fact that the two roots' *contents* would
have to be identical for the plan to be a no-op — but it is residual, and
Task 14 must correct ADR 0049 and the spec to say exactly this instead of
claiming the walk guard handles it.

### Two gaps Task 8's reviews found, reassigned rather than deferred

**Case-insensitive containment is caught by nothing — Task 9 fixes it before
it executes anything.** `/Data` against `/data/backup` passes the structural
check, the `node_id` check and the walk-time guard on APFS and NTFS, and then
copies a tree into itself. That is precisely the outcome the whole overlap
apparatus exists to prevent, so it is not a note for Task 14.

The fix is the rule this repository already owns: when **either** root's
provider does not declare `CASE_SENSITIVE`, the structural containment check
folds before comparing, exactly as `norte-compare::key` folds a pairing key —
case FOLDING, not `to_lowercase`, for the 22 code points where they diverge
(#129). It is the same duplicated fold key #151 already tracks; do not write a
third copy, and if reuse means moving the helper, say so rather than copying.
Refusal is the existing one: `Error::OverlappingRoots`.

**`Backend` has no `sync_plan`, and no task claimed it — Task 10 does.** The
TUI reaches the core through `Backend`, so Tasks 12 and 13 have nothing to
call. Task 10 adds `sync_plan` and `sync_apply` to both arms, routes
`sync.steps` and `sync.plan_done`, and repeats the up-front refusals
`Backend::compare` already makes. The embedded arm is fail-closed today only
because the CLI never calls `set_spool`; that is an accident of wiring, not a
design, and Task 10 must not leave it as the only protection.

### What Task 5 changed in this plan

**Task 4's prescription for #152 was wrong, and the wrongness is instructive.**
It said a prefix STACK of `(source RelPath, dest RelPath)` "popped by prefix as
the pre-order walk leaves a subtree". `norte_compare::walk` does not emit rows
that way: `visit` pushes ALL of a directory's rows into `pending` and only then
pushes the subdirectory frames, so between a folder's row and its children's
come **every one of its siblings**. A stack popped on the first row that does
not hang from its top loses the spelling right before it is needed, and two
levels down it does worse — it keeps the grandparent's entry and produces a
`dest_rel` naming a directory that exists on NEITHER side. Five hand-written
tests passed, in the one order the walk never produces. The shipped state is a
`BTreeMap<RelPath, RelPath>` resolved by deepest ancestor, which needs only
"parent before child"; the regression test drives real `compare()` rows.

The rule this leaves behind, for any task that adds streaming state: **the only
ordering the row stream guarantees is parent-before-child.** Not
child-immediately-after-parent.

- **`SyncError::ModeNotPlanned` is NOT deleted.** `SyncMode` is
  `#[non_exhaustive]`, so `absorb`'s wildcard is mandatory, and refusing is its
  only safe behaviour: degrading to `Update` would silently serve a plan that
  does not do what a future mode asked for. Unreachable from the wire
  (`SyncMode` has no `serde(other)`), therefore untestable, therefore kept
  deliberately rather than by omission.
- **Both wiring errors are now decided when the stream is BUILT**, not per row,
  because per row they never fired for an empty comparison — two empty trees
  under a bad mode produced an empty, approvable plan. `plan()` seeds them.
- **A read-only destination ends the stream after its one blocker and never
  pulls a row.** Task 8 must not assume the compare task it feeds runs to
  completion; dropping the row stream is what stops the walk.
- **Blocker `rel` is measured against the root of the side the blocker speaks
  ABOUT** (`AmbiguousDest`, `DirTooLarge`, `TypeMismatchDir` with `side: Right`
  → `dest_root`), and `SyncBlocker::side` uses the plan convention — source is
  `Left`, destination `Right`, whatever `source_side` says. `SyncPlanParams`
  carries no `Side` at all, so there is no second coordinate system inside the
  sync family; a frontend that synced right→left paints a `right` in its LEFT
  panel. Task 12 owns that.
- **`SyncBlocker::shape_is_consistent()` is new**, the twin of
  `SyncStep::shape_is_consistent()`: `TypeMismatchDir` carries `side` always,
  because it is the only blocker whose side is not implied by its kind and the
  only one where the side IS the sentence.
- **`TypeMismatchDir` blocks on BOTH sides**, which is the exception to the
  "source skips, destination blocks" rule this family follows twice. The
  reasoning is in its rustdoc: skipping loses nothing immediate but breaks the
  MODE's promise structurally (a file where a subtree should be), and the
  alternative costs a new `SyncReason` — closed daemon→client vocabulary, so a
  bump — after 0.40.0 ships. `protocol-guardian` asked for it recorded rather
  than defaulted; it is recorded.
- **`include` must filter the plan's OUTPUT, never its input.** A folder's two
  spellings travel in the folder's own row, which is `Same` and produces no
  step, so a caller that filters `CompareRow`s to a selection reopens #152 on
  the code path that closed it. Stated in `plan()`'s rustdoc; **Task 8 has to
  honour it.**
- **The overlap guard is narrower than ADR 0049 and the spec claim.** It
  catches structural containment only — `is_at_or_under` compares scheme,
  authority and segments literally, so a symlinked root, one SFTP host under two
  authorities and an archive opened by two paths all slip past it, and those are
  exactly the three examples both documents cite. Equal roots are deliberately
  not an overlap (a tree against itself yields `Same` rows and zero steps, and
  the transducer cannot tell two providers that spell their root alike apart —
  two `mem:///` in the tests). It fires ONCE and prunes only the side that
  reached the other root. **Task 14 must correct ADR 0049's and the spec's claim
  about what it catches**, along with the line-115 fix Task 4 flagged and the
  fifth blocker kind (the spec still lists four).
- **For Task 6.** One row produces at most one item, so nothing interleaves
  inside a row. Order is: the `DestReadOnly` blocker (if any) before everything,
  then walk order. Blockers are UNBOUNDED in number — `TypeMismatchDir` is the
  first kind whose count scales with the tree — so `plan_hash` must fold all of
  them, not the list truncated to `SYNC_MAX_BLOCKERS_REPORTED`, and must tag
  step-vs-blocker so a `Skip` and a blocker at the same `rel` cannot collide.
  Hash the serde NAME of an enum, never its discriminant: `TypeMismatchDir` was
  inserted before `Unknown` and the next variant will be too. Every read-only
  plan produces exactly one item regardless of the tree, so they all hash alike
  — `sync.apply` must gate on `executable`, not on hash match alone.
- **`Mirror` trusts the request not to have descended destination orphans.** A
  compare run with `descend_orphans` on the destination side would yield a
  `DeleteTree` for a directory and another for each descendant inside it.
  Task 8 makes that a params error, which is the enforcement.
- **MINORs skipped, with reasons.** An unrecognised `CompareVerdict` still
  produces nothing at all (in-process the two crates ship together, so it cannot
  arrive; a `Skip` for it would need a `SyncReason` that means "a newer daemon
  said something"). The `norte-testkit` `corpus::spelling_twins()` accessor and
  the six extra fixtures `encoding-auditor` proposed (a case-folded directory
  pair, a length-changing fold, the lossy `\xFF`/`\xFE` twins, `U+0130`) are a
  change of their own — every `hostile_names().len() == 47` assertion in the
  workspace moves with it — and Task 4 deferred them for the same reason. The
  non-UTF-8-leaf-under-a-folded-folder case IS covered, in `plan.rs`.

### What Task 6 changed in this plan

- **`PlanHasher::new` takes TWO arguments, not one.** The task's own constraint
  says the seed carries "the compare options", and those are NOT in
  `SyncOptions` — the transducer does not compare, so it never needed them. The
  signature is `PlanHasher::new(&SyncOptions, &SyncCompareOptions)`; every test
  snippet in step 1 that reads `PlanHasher::new(&opts_update())` gains the
  second argument. **Task 7 and Task 8 must therefore retain the
  `SyncCompareOptions` alongside the plan**, or hash while planning and store
  only the digest: a plan run with `hash` on is not the same plan as one run on
  sizes alone, even when the steps come out identical.
- **The counters live in `norte-proto` and so do their tests.** `SyncCounts` is
  on the wire, so `add` went next to the type (`methods.rs`) and its five tests
  next to the other sync invariants (`norte-proto/tests/types.rs`), not in
  `hash.rs` as the plan's step 1 grouped them.
- **`SyncCounts::add`'s rules, decided here.** Only `Copy` and `Overwrite` move
  bytes: a `size` on a `DeleteTree`, a `Skip` or a `CreateDir` is IGNORED rather
  than summed, because the dialog's number is bytes that will be WRITTEN.
  `SyncStepKind::Unknown` counts in no per-kind counter (there is none that is
  its) but DOES count as `irreversible` when it says it is — not knowing what a
  step does makes it less countable, not less final. All the arithmetic
  saturates: a weird number beats a dead Task.
- **Enum tokens are hashed by their serde name, with a fallback that is still a
  NAME.** A variant this binary does not know falls to `format!("?{value:?}")`
  — the Debug name behind a `?`, which no snake_case serde name can start with —
  so two future variants cannot collapse onto each other or onto a known one.
- **`PlanHasher` destructures everything it hashes** (`SyncOptions`,
  `SyncCompareOptions`, `CompareCriteria`, `SyncStep`, `SyncBlocker`). A field
  added to any of them stops the build here instead of quietly leaving the
  digest. If one of those types ever becomes `#[non_exhaustive]`, this is the
  file that breaks, and the fix is to feed the new field, not to loosen the
  pattern.
- **The framing helpers are DUPLICATED from `norte_core::hashing`** (`feed`,
  `feed_opt`, `hex_lower`), which is `pub(crate)` and lives in a crate that
  depends on this one, not the other way round. ~20 lines, deliberate. Task 14
  should decide whether they move to a shared crate; until then the two copies
  must stay byte-identical in behaviour — `norte-core`'s copy is also the
  journal's tamper-evident chain and cannot change at all.
- **Not a persisted format.** The digest identifies a plan retained in the spool
  for `SYNC_PLAN_TTL_MS`, produced and consumed by the same binary inside that
  window, so there is no frozen-vector test like the journal's: changing the
  framing invalidates in-flight plans and nothing on disk. Task 7 should not add
  a compatibility promise the TTL does not need.
- **`just c` does not cover rustdoc.** Task 5 left three
  `rustdoc::redundant_explicit_links` errors on the branch — two in `norte-sync`,
  one in `norte-proto` — that clippy is blind to and that would have turned this
  task's single `ci-fast` run red for reasons unrelated to it. Fixed here. Any
  task that writes rustdoc between two `ci-fast` runs should spend the ~10s of
  `RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps` rather than
  discover it four minutes into the gate.
- **`SyncCounts` gained a SECOND new counter, `unknown_kind`.** `protocol-guardian`
  found the hole: a step of a kind this decoder does not know counted in no
  counter at all, so a client at version N summing a daemon N+1's batches would
  under-report the size of the plan it is approving — the same lie
  `unmeasured_steps` exists to prevent, one field over. The core never emits it
  (its own `match` is exhaustive), so it is zero in every plan this binary
  produces. Free now, a compatibility argument after 0.40.0 ships.
  `SyncCounts::exact_bytes()` also landed: the `Option<u64>` that `bytes`
  deliberately is not, for a caller that wants the total or nothing.
- **`PlanHash::from_digest(&[u8; 32])` is new in `norte-proto`**, and the hex
  encoder plus the `expect` it forced are gone from `norte-sync`. A second hex
  encoder is a second chance to write uppercase, which is the detail that makes
  two spellings of one hash compare differently. **`norte-core` still has its
  own** (`hashing::hex_lower`, shared with the journal and the audit export);
  moving it is Task 14's call, not a silent edit of ADR 0023's neighbourhood.
- **`bytes_unknown` is renamed to `unmeasured_steps` — Task 7 lands it.** Both
  reviewers challenged the name independently and both are right: on the wire,
  next to `bytes`, it reads as "7 bytes we do not know" rather than "7 steps we
  could not measure". Task 6 kept it only because this plan had pinned it, which
  is not a reason. The name came from the controller, and the controller is
  changing it: 0.40.0 does not ship until this branch merges, so it is a
  one-line rename plus goldens now and a permanent wart later. Task 12's dialog
  text follows the field. The invariant becomes
  `unmeasured_steps <= copy + overwrite`.
- **`rel_never_escapes` is stronger than the plan wrote it.** `VPath` has no
  `join_rel` and no `starts_with`, and the property as drafted was a tautology
  anyway (joining a `RelPath` onto a root cannot leave it — `Segment` forbids
  `..` at the type level). What the proptest asserts instead is that **the plan
  never invents a path**: pasted onto one of the two roots, every step's `rel` is
  a path that some input row actually carried, and no acting step names a root.
  `dest_rel` is deliberately exempt — when it is composed from a remembered
  folder spelling (#152) it names a destination path that no row carried, which
  is precisely its job.
- **The proptest corpus now twins spellings WITHIN a row, not just between
  rows.** `rust-reviewer`'s sharpest finding: the first version built both sides
  of every row from the same segments, so `dest_rel` was always `None` and the
  entire #152 machinery — `dest_rel_of`, `remember_spelling`,
  `spelt_at_the_destination` — had zero property coverage. There is now a twin
  table (`café` NFC↔NFD, `README`↔`readme`) and two properties over it: a paired
  step's target is byte-for-byte the destination entry that EXISTS, and a child
  of a folded folder lands inside it. The scenario strategy also varies
  `on_unknown`, `source_side`, the trash and the writability, which the first
  version pinned.
- **Three things Task 7/8 should know, from the reviews.**
  1. `PlanHasher` must be fed the item sequence that becomes `SyncPlanDone` —
     i.e. AFTER `include` filtering. Hashing the unfiltered stream and showing
     the filtered plan mints a token for a plan nobody approved, and no type
     catches it. Stated normatively in `hash.rs`'s module doc.
  2. `finish()` is called only on a stream that ended `None`. A cancelled plan's
     partial digest is indistinguishable from a shorter complete one; the
     protection is that `SyncPlanDone` is never emitted, so the spool never
     stores it.
  3. The spool's stored `SyncCounts` has **no serde default** for the two new
     counters, deliberately: a spool file written by an older binary fails to
     deserialise instead of silently reading zero. That failure must surface as
     a stale-plan answer, not a dead task.
- **A known, harmless hash collision, documented rather than fixed.** A `Skip`
  from an unreadable listing on the SOURCE and one on the DESTINATION, at the
  same name, are byte-identical steps (`rel` is measured against the root of the
  side the step speaks about, and `SyncStep` carries no side). Same for an
  overlap blocker reached from either side. Neither shape writes anything, so no
  two plans that WRITE differently can share a digest — the loss is a reading
  distinction, and closing it would cost the wire field the design already
  refused.

### What Task 7 changed in this plan

**The task's own test snippets could not compile, and the reason matters.**
They read `Spool::create(dir, conn(1), hash("aa"))` — the plan_hash as an
argument to `create`. The hash does not exist at that moment: it is a STREAMING
digest over the plan's items and is not known until the stream ends. So the
shape is:

- `Spool::new(state_dir)` anchors the directory once; `create`, `open`,
  `remove`, `drop_connection` and `sweep` are methods on it rather than free
  functions each re-deriving the path.
- `spool.create(conn_id, &SyncOptions, &SyncCompareOptions)` opens the file
  under a `<conn_id>-<pid>-<seq>.part` name.
- `writer.finish()` returns a `SpoolSummary` and only THEN renames the file to
  `<conn_id>-<plan_hash>.jsonl`.

That rename is what makes "a crash mid-plan leaves nothing approvable" a
property rather than a check: an unfinished plan never has the name `open`
looks for. **The terminator record is still there and is not redundant** — the
rename is atomic with respect to the directory but promises nothing about the
data reaching the platter, so a truncated file with the right name is caught by
its last line not being the terminator.

- **The writer owns the `PlanHasher`, and that is the point.** Task 6's note 1
  says the hasher must be fed the post-`include` items, and that no type
  catches it if it is not. Now one does, structurally: `SpoolWriter::push`
  takes a `PlanItem` and hashes, counts and writes it in the same call, so
  there is no way to hash one sequence and retain another. `finish()` returns
  the hash, the counts, the capped blockers, `blockers_total` and
  `executable` — **Task 8 must build `SyncPlanDone` from this summary and not
  recompute any of it**, and must push exactly the items it sends to the client.
- **`executable` is `blockers_total == 0`, computed in one place.** Task 8 does
  not get to weaken it, and `open` now *enforces* the equivalence: a spool whose
  `executable` disagrees with its blocker count is malformed.

#### The name is not a capability, and both reviewers said so

The task text — and my first implementation — treated "the connection id is in
the filename" as the whole binding. It is not, and the gap is the one thing on
this branch I would not have shipped:

- a **filename is not a secret** (anything that can list the directory reads it),
- a **`plan_hash` is not a secret either** (`PlanHasher` is unkeyed, so anyone
  who can write a plan can compute its digest and name a file after it), and
- **`open` was comparing the requested hash against a field the file states
  about itself**, so an edited step still opened.

So the filename is now accompanied by two things, and both are in Task 7:

1. **An in-memory registry of what this process issued.** `finish()` records the
   `(conn_id, plan_hash)` on the `Spool`; `open` requires it *before touching
   the disk*. A file this daemon did not write does not open however it is
   named. This also closes the `conn_id` reuse hole structurally (the registry
   is empty at start-up, so a spool from a previous run is unopenable) and stops
   two processes sharing a state directory — the CLI's embedded engine takes no
   journal lock — from applying each other's plans.
2. **The digest is recomputed at `open`**, over the steps, from the header's
   seed, and compared with the name. One extra sequential read, which is noise
   next to executing the plan. This is what makes ADR 0049's headline claim true
   at the last hop rather than only on the wire.
- **The registry also makes a plan single-use: `open` takes the claim.** Two
  concurrent `sync.apply` of one hash would otherwise both succeed, both pass
  the per-step revalidation, and write the same destinations twice under two
  `batch_id`s — leaving an undo that describes no state the tree was ever in.
  The second caller gets `PlanStale`, which is true.
- **Therefore: the daemon constructs ONE `Spool` and clones it.** A second
  `Spool::new` on the same directory is not another handle, it is a spool that
  recognises no plan. Task 8 puts it in the daemon's shared state; nothing else
  may call `Spool::new`.
- **`finish` takes a `PlanOutcome`.** `rust-reviewer`'s sharpest finding: Task 6
  note 2 ("`finish()` only on a stream that ended `None`") was enforced by a
  comment, and the loop that breaks it is the one that writes itself —
  `while let Some(Ok(item))` routes `Some(Err(Cancelled))` out the same door as
  `None` and then closes a perfectly valid-looking plan over a third of a tree.
  The human approves 412 files, 412 copy, 400k silently never do. `finish` now
  requires `PlanOutcome::{Ended, Interrupted}`; `Interrupted` deletes the spool
  and returns an error. **Only the `None` arm may pass `Ended`.**
- **On-disk step records are validated, not degraded.** `SyncStepKind::Unknown`
  and `!shape_is_consistent()` are `Malformed` when read from a spool. The wire's
  `#[serde(other)]` tolerance exists so one bad token cannot kill a batch of 256;
  in a file this binary wrote minutes ago there is no compatibility to defend,
  and the tolerant path was about to hand the executor a step it could not name.
- **The spool has a HEADER, which the task text did not mention and the
  executor cannot work without.** `sync.apply` carries only the hash, so the
  two roots have to live somewhere; that somewhere is line 1, together with the
  `SyncCompareOptions` (Task 6's note 1) and the `conn_id`. `SyncOptions` gained
  `Serialize`/`Deserialize` in `norte-sync` for this — with a rustdoc paragraph
  saying it is still not a wire type — because the alternative is a parallel
  struct in `norte-core` that silently drops any field added later.
- **`sweep` deletes EVERY spool at start-up, not only the expired ones**, and
  the plan's test asserting `swept == 1` is replaced. At daemon start there is
  no live connection, so every spool that exists belongs to a dead one; and
  `conn_id` is a counter that restarts at zero, so keeping a fresh spool means
  keeping a file that authorises writes under an id the daemon is about to hand
  out again. It is safe because one daemon per state directory is already
  enforced upstream by the journal's exclusive lock.
- **Three deletions are wired and two are not.** `sweep` is wired in
  `norte-cli`'s `daemon run`, next to `SqliteJournal::open`. `Spool::remove`
  (applied) and `Spool::drop_connection` (connection closed) exist, are tested,
  and **have no caller yet: Task 8 wires `drop_connection` into the connection
  teardown in `daemon/server.rs`, and Task 9 calls `remove` when the apply task
  reaches any terminal state.** Until then the TTL and the start-up sweep are
  the only collectors.
- **`SpoolError::is_stale()` is the answer to Task 6's note 3.** `NotFound`,
  `Expired` and `Malformed` all mean "there is no live plan with that hash" and
  all map to `Error::PlanStale`; only `Io` is a daemon fault. A spool written by
  a binary that did not know `unmeasured_steps` fails to deserialise — the
  counters deliberately have no serde default — and comes out as a stale plan,
  not a dead task. Pinned by
  `a_spool_from_a_binary_that_did_not_know_a_counter_is_stale_not_fatal`, which
  mutilates a real spool file rather than describing the intent.
- **The "no content" test is not a tautology.** It drives a real
  `norte_compare::compare` over two `MemProvider`s whose files hold a secret,
  through `norte_sync::plan` and into the spool, then asserts the secret is
  absent from the file's bytes AND that a path that should be there IS — the
  second half is what proves the first half's search would have found the
  secret had it leaked.
- **`sweep` reports what it could not delete.** It returns a `SweepReport
  { removed, failed }`, because a bare count of deletions cannot distinguish
  "nothing was there" from "nothing could be removed", and the CLI printed
  reassurance either way. It stays non-fatal at start-up: with the in-memory
  registry, an undeletable spool is disk litter and a disclosure-at-rest
  problem, not an applicable plan, and a daemon that refuses to start over one
  is a worse failure than the one it prevents.
- **Smaller review fixes, all applied:** the TTL unlink compares `dev`/`ino`
  before removing (re-planning an identical tree yields the same hash, so a late
  `open` was able to delete the freshly approved spool by name); `encode` now
  enforces `SPOOL_MAX_RECORD` on the *write* side, so an over-large terminator
  fails where an operator can see it instead of producing a plan that answers
  "stale" forever; `read_last_line`'s ceiling is `SPOOL_MAX_RECORD + 2`, since
  finding an N-byte line needs to see two newlines; `UnexpectedEof` maps to
  `Malformed` (stale) rather than `Io` (daemon fault); `steps()` and `open` both
  refuse anything after the terminator, so a second terminator cannot make the
  approved summary and the executed plan two different things; `Drop` unlinks
  synchronously, because `Handle::spawn_blocking` panics during runtime shutdown
  and a panic in `Drop` while unwinding aborts; the state directory is created
  `0o700` with a `DirBuilder` rather than `create_dir_all`'s umask, matching
  `Journal::open`; `remove_matching` matches on the name's BYTES; and the module
  is instrumented, with a `warn!` whenever a spool we wrote comes back
  unreadable — which is the signal that someone is editing them.
- **MINORs skipped, with reasons.** `O_NOFOLLOW` on the spool open would need
  `libc` as a new `norte-core` dependency (rule 8) to close a hole that is
  already harmless: a symlinked spool yields `NotFound` or `Malformed`, both of
  which answer `PlanStale`, so there is not even a file-existence oracle. And
  the hostile non-UTF-8 name in `steps_fixture` is inlined rather than added to
  the `norte-testkit` corpus, for the reason Tasks 4 and 5 already recorded:
  every `hostile_names().len() == 47` assertion in the workspace moves with it.
- **For Task 14: ADR 0049 needs three corrections.** It says the spool is "keyed
  by `(connection, plan_hash)`" without saying the key is the FILENAME *and* that
  a filename alone is not a capability — the in-memory issuance registry and the
  recomputed digest are what make the binding hold, and the ADR should say so.
  Its start-up sweep bullet should say "every spool", with the conn-id reuse
  argument. And its negative-consequences list should add that a spool discloses
  a full relative listing of both trees, with sizes and verdicts, to anything
  that can read the state directory.
- **Also for Task 14, an issue to file that is bigger than this feature.** The
  daemon's state directory is `0o700`, but nothing in the policy/VFS layer
  excludes it from a scope grant over `$HOME` — so an agent with read scope there
  can read `journal.db` (the whole mutation history) and now the spools too. It
  is pre-existing and the spool only adds to the pile, but it wants an issue.

#### Two things Task 8 and Task 9 must not discover the hard way

1. **Task 9's gate runs over the roots read FROM THE SPOOL, at apply time.**
   `sync.apply` carries no paths, so the roots come out of a file; the gate that
   `sync.plan` passed ten minutes earlier does not carry over, because scope
   TTLs expire and policy rules change inside the window. Rule 9 with a file in
   the middle of it.
2. **`steps()` can fail mid-stream**, after N steps have already executed — a
   truncated or edited spool, not only a cancellation. The executor's journal
   batch has to be closed and undoable at that point too.

### What Task 8 changed in this plan

**The two notifications share ONE channel, and that is what makes their order a
property.** `sync.plan_done` normatively CLOSES a plan, so it must arrive after
the last `sync.steps`. With two channels that ordering would depend on how the
runtime woke two receivers; with one `mpsc` of
`norte_core::sync::SyncPlanEvent { Steps, Done }` it is the FIFO, and the
daemon's pump is a `while let Some(event)` that forwards whatever it is handed.
The pump is otherwise `handle_fs_compare`'s, including the "a batch that is not
delivered stops the walk" rule.

- **`Engine` gained `set_spool`/`spool()`, and a spool-less engine REFUSES to
  plan** (`Error::Unsupported`, fail-closed like the index). A plan that cannot
  be retained cannot be applied, so serving one would put an approval dialog in
  front of something that stops existing the moment it is approved. `norte-cli`
  installs the same handle it already used to sweep — **one `Spool::new` in the
  process**, which is Task 7's non-negotiable. The daemon reads it back out of
  the engine (`shared.engine.spool()`) rather than keeping a second copy in
  `Shared`, so the two can never disagree.
- **`include` filters the transducer's OUTPUT, and the rules it needed are new.**
  Three decisions, none of which the wire types state:
  1. **Membership is by segment PREFIX**, so a selected folder drags its
     subtree — a descended orphan is a `CreateDir` plus one step per
     descendant, and exact equality would have created the folder empty. The
     comparison is over the canonical wire form (percent-encoded, `/`-separated,
     so a `/` in it can only be a separator), which is byte-exact: `café` NFC
     does not match NFD, `README` does not match `readme`, and `café` cannot
     drag `cafétière`.
  2. **Blockers are NEVER filtered.** A blocker is not a step, and some are
     about the whole tree — `DestReadOnly` hangs off the root, which no
     selection names. Recorting them by the selection would turn a read-only
     destination into an executable plan. So `executable` always speaks about
     the whole comparison. **Task 12 should decide whether the pane says so**;
     a selection of three files can come back non-executable because of a
     `TypeMismatchDir` forty thousand rows away.
  3. **`Some([])` is a selection of nothing**, not "everything": an empty,
     executable plan. `None` is the tree.
  The filter runs BEFORE `SpoolWriter::push`, so the hash covers exactly what
  the client saw (Task 6's note 1), and two selections over one tree cannot
  share a digest.
- **The `node_id` half of the overlap guard is in, with its residual stated
  rather than papered over.** `Engine::sync_plan_as` does the structural check
  first (`structural_overlap`, which returns the three-valued `RootOverlap` and
  not a `Side`), then stats both roots with `Provider::node_id` and
  `FollowLinks::Yes`. **`Yes` and not `No`, and that is the whole point**: with
  the link's own identity, `/data` and `/srv/data` answer differently and the
  check catches nothing — the case it exists for is exactly a symlinked root,
  and listing a directory traverses the link anyway. Two more deliberate
  narrowings: the ids are only compared when `Arc::ptr_eq` says it is the SAME
  provider object (a `NodeId` from two backends is not comparable — a
  `MemProvider` index and an ext4 inode can collide meaning nothing), and an
  error or a `None` is **not** an overlap (`None` on SFTP/FTP is the known
  residual; a `NotFound` means the root is absent, which the comparison below
  reports as an error row exactly as `fs.compare` does today, instead of a
  second taxonomy for the same fact).
- **The engine refuses what the handler refuses, with its own taxonomy.**
  `follow_symlinks`, a caller-set `descend_orphans` and an over-cap `include`
  are `-32602` in `handle_sync_plan` and `Unsupported`/`InvalidPath` from
  `sync_plan_as` — the same asymmetry `fs.compare` already has, and for the same
  reason: the EMBEDDED arm calls the engine without passing through the daemon,
  so a check that lives only in the handler is a check that arm does not have.
  Overlapping roots are the exception and live in the engine ALONE: they are a
  wire category (`Error::OverlappingRoots`), so `RpcError::from` carries them
  intact and a second copy in the handler would be a second place to get the
  three-way relation wrong.
- **A cancelled plan and an abandoned one are the same thing, and neither
  leaves anything approvable.** Only the `None` arm of the stream reaches
  `finish(PlanOutcome::Ended)`; the error arm, a cancelled flush AND a flush
  whose receiver went away all reach `finish(PlanOutcome::Interrupted)`, which
  unlinks the `.part`. The last of those three is the one worth naming: an owner
  that stopped receiving would otherwise leave a fully valid `plan_hash` for a
  plan nobody ever saw whole.
- **Two tests that look like they are about timing are not, and must not be
  rewritten as if they were.** `cancelar_el_plan_no_deja_spool` and
  `un_dueno_que_deja_de_recibir_no_deja_plan_aprobable` plan 3 000 files against
  a channel of 8 batches × 256 steps, so the producer is BLOCKED in `send` when
  the test cuts. The first version used 400 files and a per-op latency fault and
  was a race the plan won every time — it finished in 48 ms and the assertion
  found a complete spool. Fill the channel; do not add a `sleep`.
- **For Task 9.** The spool a completed plan leaves is opened with the hash that
  travelled in `sync.plan_done`, from the same `conn_id`
  (`un_plan_completo_deja_un_spool_abrible_con_su_hash` pins it end to end,
  including that another connection cannot). And Task 7's warning still stands
  above everything else here: **the gate runs over the roots read FROM THE
  SPOOL, at apply time** — nothing about the gate that `sync.plan` passed
  carries over.

#### What the two reviews changed, and it was the retention side every time

Neither reviewer found a BLOCKER, and neither questioned the gate order, the
`finish(Ended)` discipline or the `node_id` check — all three verified in the
code rather than taken on the word of the prompt. Seven MAJORs between them, and
**six were about what happens to a plan after it is written**, not about writing
it. Task 7 built the spool; Task 8 is the first thing that can drive it from the
wire, and that is where the design met its first adversary.

- **The TTL was not a reaper, and nothing else was either.** `SYNC_PLAN_TTL_MS`
  was only ever checked inside `Spool::open`, so a plan nobody opens was never
  collected: the effective lifetime was "until the connection closes or the
  daemon restarts". ADR 0049's "its lifetime is closed on all four sides" was
  therefore false, and **Task 14 must correct it**. `Spool::create` now reaps
  expired `.jsonl` before opening a new plan — the reaper runs where the growth
  happens and needs no timer task. `.part` files are exempt: a plan over a
  network tree takes hours legitimately.
- **A retention cap, because time alone does not bound a burst.** A client
  planning in a loop with different `include` lists mints a new digest, and
  therefore a new file, every time. `MAX_RETAINED_SYNC_PLANS = 16` per
  connection, refused with `OVERLOADED` (the request is fine, the moment is
  not — the same code and the same reasoning as the live-task cap). The residual
  is that 16 plans over half a million entries is still gigabytes; a byte budget
  for the state directory is a change of its own.
- **A plan could outlive its connection and be retained forever, and the case
  needs no race at all.** `flush` returns `Continue(0)` *without touching the
  channel* when the batch is empty, so a plan over two identical trees — zero
  steps — never learns that its owner left, closes normally, and is reported
  `Completed`. The daemon's teardown had already swept. The fix is in the spool,
  because the channel cannot answer this: `drop_connection` now leaves a
  tombstone when the connection still has a writer open, and `finish` refuses
  `Ended` for a tombstoned connection. The tombstone is bounded by plans in
  flight, not by the connection counter. Two more nets behind it: `tx.is_closed()`
  before closing, and the pump sweeping the connection's plans when a delivery
  fails (it is the only observer of delivery, and it runs *after* teardown).
- **`finish` re-minted a claim that `open` had consumed.** Replanning the same
  tree with the same options yields the same digest, so — once Task 9 exists —
  planning again during an apply would hand out a second right to execute the
  same plan against the same destination, two `batch_id`s, and an undo
  describing no state the tree was ever in. `Spool` now tracks what is being
  applied (`open` moves the claim there, `remove` releases it) and `finish`
  refuses to land on it. **Task 9 must call `Spool::remove` at every terminal
  state**: until it does, that hash cannot be re-planned. That is the safe side
  of the trade, and it is a user-visible failure.
- **`include` dropped the `CreateDir` a kept `Copy` depends on.** The drag was
  one-directional. Selecting the file row of `nueva/a.txt` filtered out
  `CreateDir nueva` and left an `executable` plan whose only copy goes into a
  directory that does not exist — and broke the rule `SyncStepsBatch::steps`
  publishes. A `CreateDir` whose `rel` is a strict ancestor of something selected
  now survives; **only that kind**, because a `DeleteTree` on an ancestor would
  delete the very subtree the user asked to sync.
- **`include`'s semantics are wire semantics and lived in a private struct.**
  Five rules now stated normatively on `SyncPlanParams::include` (output not
  input, prefix drag, the `CreateDir` exception, byte-exact matching that folds
  nothing even on a filesystem that does, and `[]` meaning nothing while `[""]`
  means everything), plus the blockers-are-not-filtered consequence on
  `SyncPlanDone::executable` and the "buffer batches by `task_id`, they can
  precede the response" note on `SYNC_STEPS`. Doc-only and additive — no version
  bump, but `docs/schema/proto.schema.json` embeds rustdoc as `description`, so
  it regenerates with `NORTE_UPDATE_SCHEMA=1`.
- **The 100 ms drip did not drip.** The time check hung off the arrival of a
  step, and the transducer emits nothing for a `Same` row: three steps followed
  by a 200 000-file identical subtree sat in the buffer for the whole walk.
  `fs.compare` does not have this problem because every pair is a row. Now a
  `tokio::select!` against a timer — which is the shape Task 3 made the stream
  `FusedStream` for in the first place.
- **Two MAJORs deliberately NOT fixed here, both recorded for Task 14.**
  1. **The spool is a large new disclosure to an in-daemon actor.** `0700`/`0600`
     protects against other OS users; nothing in the policy layer excludes the
     state directory from a scope grant over `$HOME`, so an agent can `fs.read`
     a spool and get a full recursive inventory of two trees it has no scope
     over. Task 7 already scheduled the issue; the reviews sharpen it — the
     class is pre-existing (`journal.db` is the same directory) but the *volume*
     and the *trigger* are new, since anyone who can plan can now produce one on
     demand. ADR 0049's "owner-only" line must say what that does and does not
     cover.
  2. **Case-insensitive containment is not caught by anything.** Both root
     checks compare segment bytes, so on APFS or NTFS `source=/Data` against
     `dest=/data/backup` passes the structural check (not byte-equal, not
     byte-nested), passes `node_id` (genuinely different directories), and passes
     the walk guard (which compares bytes too) — and the plan copies a tree into
     a subdirectory of itself. Stated in `sync_plan_as`'s rustdoc as a residual;
     closing it needs a containment check aware of the destination's folding
     regime, which is more than a `stat`. **Task 14 lists this with the other
     two corrections ADR 0049 owes.**
- **MINORs skipped, with reasons.** A plan can be `Ended` and `executable` while
  a subtree was never read (an unlistable source directory is a
  `Skip{Unreadable}` step, not a blocker) — not a Task 8 regression, not
  destructive (`Mirror` produces no phantom `DeleteTree`, because the walk
  refuses to pair the other side after a failed listing), and the right place to
  fix the *impression* is **Task 12's dialog**, which should lead with the
  `Skip{Unreadable}` count the way it leads with the irreversible one. And the
  `include` drag does not cross the two spellings of a folded directory pair, so
  a `DeleteTree` whose `rel` is destination-relative can fall outside a
  source-anchored selection — it fails in the safe direction (fewer deletions)
  and is now documented on the wire rather than fixed, because fixing it means
  the transducer publishing both spellings per step.
- **Also for Task 9 and Task 12: `Backend` has no `sync_plan`.** No task in this
  plan claims it, and the TUI goes through `Backend`. It needs the method, the
  two notification routes (`SYNC_STEPS`, `SYNC_PLAN_DONE`) next to
  `compare_routes`, and the same up-front refusals `Backend::compare` makes. Note
  the embedded arm has no actor to gate on: today it is fail-closed only because
  the CLI never calls `set_spool`, and whoever adds `Backend::sync_plan` must not
  quietly change that.

### What Task 9 changed in this plan

**The spec's revalidation was not implementable, and that is the finding of this
task.** «Before an `Overwrite` or a `DeleteTree`, one `stat`: does the
destination still look the way the plan recorded it — same size, same mtime?»
The plan records nothing of the sort. `SyncStep` carries `id`, `kind`, `rel`,
`dest_rel`, `size`, `criterion`, `confidence`, `reversal` and `reason`, and
`size` is normatively **the bytes the step MOVES**, i.e. the SOURCE's — Task 3
put it there and Task 6's `SyncCounts::add` depends on it. There is no field
that describes the destination's prior state, so a `stat` at apply time has
nothing to compare against and the whole «the only thing standing between the
TTL and a lost file» sentence was decorative.

The fix is a **witness that travels in the spool and not on the wire**:

- `norte_sync::DestWitness { kind, size, mtime_ms }`, taken from the
  destination `Entry` the row already carried.
- `PlanItem::Step` becomes a struct variant, `{ step, dest }`. The transducer
  populates `dest` for **`Overwrite` and `DeleteTree` only** — the two kinds
  that destroy; a witness per step in a half-million-step plan is spool nobody
  reads.
- `PlanHasher` explicitly does **not** feed it (destructured with `dest: _` and
  a comment). It is where the conclusion came FROM, not the conclusion; feeding
  it would make two plans over an untouched tree differ because a mtime moved,
  and would have moved the golden for nothing.
- The spool record is `SpoolStep { step, dest }`, `SPOOL_FORMAT` goes to 2.
  Nothing on the wire changed, nothing is persisted across binaries, and an old
  spool fails to deserialise → `Malformed` → `PlanStale`, which is Task 7's
  rule already.

**What the witness can and cannot do, stated rather than promised.** A provider
that lists without size or mtime — `file://` is one — leaves both `None`, and
the revalidation degrades to «still exists, still the same kind». Only fields
present on BOTH sides are compared, because declaring a conflict on a `None`
would refuse every plan on the most common filesystem. The `Overwrite` case is
covered in practice: a PAIRED row is hydrated (the cascade needs size and mtime
to decide). A one-second mtime resolution leaves a one-second window in which a
same-size change goes unseen.

**Two more deviations from the task text, both deliberate.**

1. **`ResumePolicy::Off`, so a cancelled copy leaves the destination CLEAN**
   rather than a `.norte-partial`. The task's test
   (`cancelling_mid_copy_leaves_a_marked_partial_never_a_bare_one`) asserts the
   partial exists; the domain rule in `CLAUDE.md` allows either («a clean
   destination or a `.norte-partial`, never an unmarked partial»). A partial
   left in the destination tree is an orphan to the NEXT comparison, and under
   `Mirror` that orphan is a `DeleteTree` — the feature would litter its own
   input. Resuming a large sync is a later addition; littering is not.
2. **A non-executable plan is `Error::PlanNotExecutable`, not
   `Error::InvalidParams`.** The category already exists, `rename_batch` already
   uses it for exactly this, and `InvalidParams` would have said the request was
   malformed when it was the plan that was blocked.

**`sync.apply` REQUIRES a journal** (`Error::Unsupported` without one),
fail-closed like the spool. The rename batch tolerates a journal-less engine
because a rename is reversible by inspection; this writes, buries and destroys,
and every step of the plan carries a `StepReversal` that only the journal can
honour. There is therefore no `ObserverJournal` arm here.

**The `Overwrite` without a trash journals ONE entry, `created`/`Irreversible`,
and the removal gets none of its own.** The spec's table says so and the reason
is worth keeping: with two entries (`removed`/irreversible + `created`/delete) a
batch undo would delete the new file and be unable to restore the old one,
leaving the path EMPTY where the user had something — worse than the state it
came to fix. The trashless `DeleteTree` follows the same shape: one `removed`
entry for the whole tree, mirroring the trash case's one `trashed`.

- **The gate runs over the roots read from the spool, and gates the ROOTS.**
  `PolicyOp::Copy` over both, plus `Mkdir` and `Delete{mode}` over the
  destination when the plan's counters say the plan does those things — the mode
  being the one that will really be used. Per-STEP gating was considered and
  refused: it is the same coverage `Engine::copy_with_as` gives a recursive copy
  today, and a per-step gate would put an `ask` per step in front of a
  half-million-step plan. The residual is a `policy.toml` deny rule on a path
  INSIDE the tree, which this does not see — pre-existing and shared with
  `fs.copy`.
- **`Spool::remove` runs at every terminal state**, inside the task body, plus
  on every early refusal after `open` took the claim (the refusal path is split
  into `sync_apply_opened` precisely so no `?` can forget it). A test pins that
  a refused plan can be re-planned with the same digest.
- **`steps()` failing mid-stream is NOT a step failure.** No row is written to
  the report — there is no step to attribute it to, and what came after is
  unknown — the task fails, and what was applied stays journalled under the
  closed batch. Pinned by
  `un_plan_que_deja_de_leerse_a_mitad_para_la_task_sin_anotar_fila`.
- **A `CreateDir` whose destination already exists is a `Conflict`, not a
  silent success.** Adopting a directory somebody else made would put a
  `created` entry on it, and undo routes `Created` through the trash — so the
  undo would bury a stranger's directory with its contents. Failing the step
  costs a report row and the copies into that directory still work.
- **`SyncFailureCause::IllegalName` is in.** `Error::InvalidPath` from the write
  path maps to it. Goldens and `docs/schema/proto.schema.json` regenerated; no
  version bump (0.40.0 is unreleased on this branch and the enum is
  daemon→client with `#[serde(other)]`).
- **For Task 10.** `Engine::sync_apply_as(&PlanHash, conn_id, actor)` returns
  `(TaskHandle, Arc<Mutex<SyncReportResult>>)` — the report is the WIRE type, not
  a core-private twin, so `sync.report` returns a clone of it and translates
  nothing. `handle_sync_apply` must own the `task_id → report` map the way
  `fs.rename_batch_report` does, and mirror its ownership check. The refusals it
  has to repeat up front are the ones `sync_apply_as` already makes
  (`Unsupported`, `PlanStale`, `PlanNotExecutable`, `PolicyDenied`) — the engine
  makes them itself because the EMBEDDED arm never passes through the daemon.
- **For Task 11.** Every entry of a sync batch is `created` (reversal `delete` or
  `irreversible`), `trashed` (`restore_trash`, with `reversal_ref` when the
  trash is logical) or `removed` (`irreversible`). No `renamed`. `undo_units`
  groups by `batch_id` unchanged, and reverse-`seq` gives the overwrite pair its
  correct order for free.

#### What the three reviews changed, and two of the three blockers were about the plan's own retention

Three BLOCKERs, ten MAJORs. None of them was in the step→effect mapping; they
were in what happens when a step fails, and in what the spool is trusted for.

- **`remove_tree` listed a leaf, so a trashless `DeleteTree` of a FILE always
  failed.** `ops::walk` opens with a `list`, and a `list` on a file is
  `Conflict{TypeMismatch}` — reported as `Conflict`, i.e. as destination drift
  that never happened. `Mirror` against a bucket or an SFTP could not delete a
  single file. `ops::delete_task`, which this claimed to copy, guards with a
  `stat` first; now so does this.
- **`Spool::open` took the single-use claim BEFORE it could fail**, and the
  engine's `remove` was after the `?`. A plan opened eleven minutes late got
  `PlanStale`, and re-planning the same tree — same digest by construction —
  then hit `finish` refusing to re-mint a claim that was stuck in `applying`,
  deleted the plan it had just written, and answered `Internal`. Neither apply
  nor re-plan, for the life of the connection, diagnosed as "internal error".
  `open` now releases on every failure after the claim, into NEITHER set: a
  malformed plan must not become applicable again, only re-plannable.
- **One `TrashId` per Task, reused for every victim.** Copied from
  `ops::delete_task`, where one task buries one thing. A logical trash names its
  directory `<ms>-<counter>`, so two burials in the same millisecond collided
  and the second came back `Conflict{Exists}`. The id is now per STEP
  (`SyncStep::id`, monotone within one plan). Invisible until the integration
  harness moved to `with_logical_trash()`, which a review also asked for.
- **`SpoolSummary.counts` was not covered by the digest and decided which policy
  gates ran.** Zero the terminator's `overwrite`/`delete_tree`/`create_dir`,
  leave the steps intact, and the digest still verified while the `Delete` and
  `Mkdir` gates silently did not run. `verify_digest` now recomputes the counters
  with `SyncCounts::add` in the same pass it already makes and refuses a
  mismatch. The state directory matters here: `policy.toml` is read once at
  start-up, but a spool takes effect on the next `sync.apply`.
- **The root-only gate was weaker than `fs.delete`, not equal to `fs.copy`.**
  `policy.toml` rules match by containment of the path presented to the gate, so
  gating `dest_root` alone means a `deny` on an inner path is not seen — and
  `Engine::delete_with_as` gates the real target. A `Mirror` with an empty
  source would have deleted a subtree `fs.delete` refuses. The executor now asks
  the policy per ACTING step, on the real path, with the op the step performs
  (`Mkdir`, `Copy`, `Delete{mode}` — an `Overwrite` asks both). `Deny` and `Ask`
  are both a report row (`Denied`): a modal per step in a half-million-step plan
  is not an interface, and the root gate already asked once for the batch.
- **A destructive step with no witness now REFUSES.** The witness is
  deliberately outside the `plan_hash`, which makes deleting it exactly the edit
  the digest cannot see — and with the old code a missing witness degraded the
  revalidation to "something exists here". The transducer always populates it for
  the two destructive kinds, so absence means the file is not what this binary
  wrote.
- **A panic in the task body leaked the claim**, because the scheduler's own
  `catch_unwind` swallows it past the `remove`. The body now catches it itself,
  spends the plan, and then answers what the scheduler would have.
- **The source is stat'd BEFORE the destination is destroyed.** Found while
  restructuring for a clippy line limit, and confirmed by a review: with the
  stat after, a source that vanished between approval and application left the
  destination path EMPTY — buried, with nothing to put back.
- **A trashless `Overwrite` whose copy then fails now journals the destruction.**
  On the success path it is still ONE `created`/`Irreversible` entry (the spec's
  table, for the reason recorded above). On the failure path there was no entry
  at all: permanently destroyed bytes outside the journal, which rule 4 has no
  exception for. Same for a `DeleteTree` whose recursive remove dies mid-tree.
- **`SyncFailure` gains `dest_rel`.** `IllegalName`'s headline case is a name
  that blows `NAME_MAX` once recomposed in NFD — which is the DESTINATION's
  spelling. Reporting `rel` alone points at the source's spelling, which is the
  short, legal one: "this name is illegal", pointing at a legal name. Free now
  (0.40.0 unreleased), a bump later.
- **`IllegalName` is best-effort and the rustdoc now says so.** `file://`
  classifies it; SFTP v3 answers a generic `Failure` and object storage does not
  distinguish an over-long key from any other rejection, so on those two an
  illegal name arrives as `Io`. Its absence proves nothing; its presence proves
  one name was bad.
- **The golden said `failed: 3` over four rows.** `failures.len() <= failed` is
  the type's own contract and the core maintains it unconditionally; the fixture
  a client author reads taught the opposite.

**MAJORs deliberately NOT fixed, with reasons.**

1. **A sync batch blocks `undo_session` until Task 11 lands.** `revert_batch`
   demands every entry of a batch be `rename_back` and answers `Blocked` — and
   `undo_session` is strict LIFO, so it stops there. This is real and it is
   exactly what Task 11 exists to fix; inventing interim undo semantics here
   would be writing code Task 11 replaces. **It must not merge without Task 11.**
2. **A symlink at an INTERMEDIATE component can redirect a `Copy` or a
   `CreateDir` outside `dest_root`.** No provider in this tree opens with
   `O_NOFOLLOW`/`RESOLVE_BENEATH`; closing it is a `norte-vfs-local` change, not
   an executor one. The destructive kinds dodge it by accident (`stat` is an
   `lstat`, so the witness's `Dir` meets a `Symlink` and conflicts; `walk` does
   not descend links). Stated in `SyncTargets::dest_root`'s rustdoc rather than
   promised away — **Task 14 files the issue.**
3. **A `DeleteTree` revalidates the DIRECTORY, not its contents.** A directory's
   mtime moves only for direct children, so a subtree that gained a hundred files
   two levels down revalidates clean and is destroyed whole. It is the step with
   the largest blast radius and the weakest check; closing it costs a listing per
   destructive step. Stated in `revalidate`'s rustdoc.
4. **A failed `created` insert after a successful trash leaves a batch whose undo
   BLOCKS** (the `RestoreTrash` finds the path occupied by the unrecorded new
   file). Compensating that one step means another unjournalled mutation on the
   journal-is-broken path. What is fixed is the diagnosis: both fatal arms now
   log the buried path and its trash destination at `error!`, so "where did my
   file go" has an answer.
5. **The approval dialog counts `Copy`/`CreateDir` as reversible on a trashless
   destination**, but undo of a `created` routes through the trash and is skipped
   without one. Task 11/12 decide whether the plan counts them irreversible or the
   dialog says "reversible only where the destination has a trash".
6. **The report cannot tell "nothing was touched" from "the destination was
   buried and the copy failed"** — both are one `Conflict` row, and the user's
   next action differs. Distinguishing them costs a closed daemon→client cause;
   documented on `run` instead.
7. **The plan's three cross-provider tests are not here.** Two of them
   (`planning_into_an_archive_blocks_...`,
   `comparing_against_an_archive_source_...`) test the PLANNER, not the executor,
   and belong with Task 8's surface; the third
   (`a_destination_without_a_trash_takes_the_irreversible_path_for_real`) is
   covered by `una_sobrescritura_sin_papelera_se_declara_irreversible`, which
   drives a real `MemProvider` with no `TRASH` flag end to end.

### What Task 10 changed in this plan

**The three cross-provider tests the spec names are IN, and Task 9's note that
two of them were already covered was wrong about one of them.** They live in
`engine_sync_plan.rs` (they test the planner) and all three pass. The archive
one needed a fixture that did not exist: `ZipSmith` hardcoded a valid DOS date
(2020-01-01) for every entry, so a zip could never reach `CompareConfidence::
Unknown` by way of the mtime — the size rung matched, the mtime rung answered
`Different`/`Probable`, and "an archive whose mtime deserves no trust" was
untestable. `ZipSmith::undated()` writes the zero DOS pair, which is *invalid*
(month 0, day 0) and is what a writer that leaves the field blank produces; the
index then honestly reports `mtime_ms: None`. Additive, no corpus assertion
moves.

- **`Backend` gained `sync_plan`, `sync_apply` AND `sync_report`.** The plan
  named the first two; the third is not optional in practice — a failed step is
  a report row and not a task failure, so without it a frontend that applies a
  plan cannot find out what happened. It is the twin of
  `Backend::rename_batch_report`, and it reads the same ring the socket reads
  (`Engine::sync_report`), because two rings would be two retention policies
  that contradict each other on the first eviction.
- **The embedded connection is `EMBEDDED_CONN_ID = u64::MAX`, a constant.**
  Planning and applying have to agree on the spool key or a `Backend` answers
  `PlanStale` to its own plan. `u64::MAX` rather than `0` because the daemon's
  `conn_id` counter starts at zero, so the two can never collide in a process
  that has both. **The corollary is now on the constant and matters for Tasks
  12–13: a `plan_hash` is not a secret** (it is a deterministic digest anyone
  who can read both trees can compute), so the *only* thing binding a plan to
  its requester is that `conn_id` — and here it is a constant, with
  `Actor::User` and therefore no policy gate. These two methods must not be
  wired to the Lua sandbox or the plugin host without an actor of their own.
- **The embedded arm's fail-closedness is now the engine's, and it is pinned.**
  `embedded_sin_spool_no_planifica_y_sin_journal_no_aplica` drives both halves
  and asserts the destination is untouched. The CLI still never calls
  `set_spool` on its embedded engine and the TUI's embedded engine has neither
  journal nor spool — **so embedded sync is unavailable until Task 13 wires it**,
  which is a wiring decision, not a protection.
- **The step feed FAILS CLOSED when the client is the one that cannot keep up,
  and the other two feeds still do not.** `route_batch` grew an `OnFull`
  parameter. `search.hits` and `compare.rows` keep dropping the batch: a lost
  row is paint. `sync.steps` closes the feed instead, because a dropped batch
  with the `sync.plan_done` delivered behind it leaves a human approving a hash
  that covers `DeleteTree` and `Overwrite` rows that never reached the screen —
  the one thing this design exists to prevent. No close means no hash means
  nothing applicable. Both reviewers found this independently and it was the
  only real defect in the task. **Task 12 should still cross-check** the steps
  it received against `SyncPlanDone::counts` (`create_dir + copy + overwrite +
  delete_tree + skip`), which is now stated on `Backend::sync_plan`.
- **The live-task cap is checked BEFORE the plan is opened.** `Spool::open`
  takes the single-use right and the task body spends it at every terminal
  state, so an `OVERLOADED` from `register_task_id` destroyed an approved plan
  and answered "retry later" to a hash that could never work again — the only
  way forward being to re-walk both trees. `tasks_at_capacity` is a TOCTOU
  approximation and `register_task_id` remains the authority; it moves the
  common case from "plan destroyed" to "plan intact".
- **`sync.apply` is cancelable, and that needed a claim guard.** Its gate can
  suspend in a policy `ask` for up to the approval TTL, which is longer than the
  client's 30 s RPC timeout: without withdrawal the human approves a minute
  later and the tree is rewritten for a client that already gave up and was told
  the daemon was unreachable. Adding it to the daemon's `cancelable` set alone
  would have been a NEW bug — dropping the dispatch skips
  `sync_apply_as`'s release, stranding the hash as "applying" for the life of
  the connection, neither applicable nor re-plannable. So `ApplyClaim` (a `Drop`
  guard) plus `Spool::abandon`, which releases the in-memory claim
  synchronously and, like every other failure path, does **not** return the plan
  to `issued`: not applicable again, but the same tree is re-plannable. The
  remote arm uses `call_timed_guarded` so an abandoned call actually sends the
  `rpc.cancel`; it loses the `METHOD_NOT_FOUND` translation, which is
  unreachable for this method (a `plan_hash` can only have come from the same
  daemon).
- **`evict_batch_reports` is now a thin wrapper over a generic `evict_reports`**
  shared with the sync ring. Behaviour on the rename path is unchanged
  (verified by both reviewers, line by line). What the docstring no longer
  oversells: the agent sub-cap is a FLOOR — above it the second pass evicts by
  age without looking at the owner, so an agent can still push out an old, quiet
  human report. The loud ones survive, which is the point.
- **MINORs skipped, with reasons.** (1) `handle_sync_report` clones the report
  before the ownership check, so the denied branch does more work than the
  unknown branch — a weak timing signal, inherited verbatim from
  `fs.rename_batch_report`, and fixing one twin and not the other is worse than
  fixing neither; it wants one change touching both. (2) The poisoned-lock
  asymmetry (the read path tolerates poison, the eviction path panics while
  holding the ring lock) is likewise inherited and unreachable — `exec::run`
  never holds a report guard across an `.await`. (3) The remote arm's
  `METHOD_NOT_FOUND` → `Unsupported` branch is unreachable over the socket at
  all, since `version_compatible` accepts only older-client-on-newer-daemon; it
  is kept because `compare` has it and consistency is worth more than deleting
  four lines.
- **Two things for Task 14 to file or record**, neither new to this task:
  1. **`SyncReportResult::batch_id` is the first journal-internal identifier a
     non-`User` actor receives.** It is opaque as a capability — no method takes
     one, undo is by session — but it is a dense global counter, i.e. a coarse
     measure of the daemon's cumulative journal batches, and the rename twin
     carries nothing like it. ADR 0049 does not discuss report visibility at
     all; it should.
  2. **A `Daemon` bound over an `Engine` with no `with_policy` gates nothing**
     (`AllowAll` is the default), so one `sync.apply` from any actor rewrites a
     whole tree. Pre-existing and shared with `fs.copy`/`fs.delete` since M3, no
     shipped binary does it — but sync is the first method where the blast
     radius of that hole is an entire subtree, and the fail-closed treatment of
     the spool and the journal invites the reader to assume the third leg is
     fail-closed too. Now stated in `sync_apply_as`'s rustdoc; a start-up
     `warn!` would be cheap.

---

---

## Gate budget

Follow `CLAUDE.md`. Per-task loop: `just t <crate>` (plus `just c` when you
touched lint surface). `just ci-fast` **once** after tasks 6, 9 and 12.
`just ci` **once**, at task 14. Never re-run the gate to check whether a fix
worked — reproduce the single failure with `just t <crate>`.

**`just c` cannot see a rustdoc error.** Task 5 left three
`rustdoc::redundant_explicit_links` on the branch that clippy is blind to and
that would have turned any later task's `ci-fast` red for reasons that task
did not cause. Before committing, run

```sh
RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps
```

It costs about ten seconds and it is the cheapest insurance on this branch.

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
fn a_step_with_no_size_is_counted_apart_and_never_as_zero() {
    // Orphan rows are not hydrated (#157) and local lists with `size: None`,
    // so on file:// this is the common case, not the edge one. A confident
    // zero in the approval dialog would be a lie.
    let mut c = SyncCounts::default();
    c.add(&copy_step("known", 10));
    c.add(&copy_step_without_size("unknown-a"));
    c.add(&copy_step_without_size("unknown-b"));
    assert_eq!(c.bytes, 10);
    assert_eq!(c.unmeasured_steps, 2);
    assert_eq!(c.copy, 3, "an unmeasured file is still a file to copy");
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
`rel` bytes, **`dest_rel` bytes**, size, criterion, confidence, reversal,
reason) and each blocker. `id` is excluded, with a comment saying why.

**`dest_rel` is in that list and is not optional** (task 4). It is the only
thing distinguishing "overwrite the file that is there" from "create a second
one beside it", so two plans that differ only in it write to different paths —
and `plan_hash` is the token that authorises execution. Feed a presence tag,
not just the bytes: a `Skip` may legally carry a ROOT `rel`, which encodes to
zero bytes, so "absent" and "present and empty" must not collide even though a
well-formed step cannot produce the ambiguity.

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
fn unmeasured_files_are_shown_and_never_folded_into_the_byte_total() {
    let s = SyncState::ready(done_with(SyncCounts {
        copy: 5, bytes: 1_200_000_000, unmeasured_steps: 340, ..d()
    }));
    let lines = s.summary_lines();
    assert!(lines.iter().any(|l| l.contains("340")),
        "a confident byte total that hides 340 unmeasured files is a lie");
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
