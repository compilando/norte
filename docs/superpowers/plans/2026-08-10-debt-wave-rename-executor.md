# Debt wave: the batch-rename executor's four, plus #126

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #129, #128, #130, #126 and #127 — the residue the batch-rename
executor (ADR 0042) left behind, plus one Rust-API break that gets more
expensive the longer it waits.

**Architecture:** Four of the five are local fixes with the shape of the fix
already argued in the issue. The fifth (#127) is a design decision that was
taken during planning: **the journal's format version becomes an entry inside
the hash chain**, adjacent to the genesis, so the chain signs it and ADR 0025's
HMAC anchor covers it — a four-byte pragma outside the chain could be flipped
by anyone and the tamper evidence would never see it.

**Tech stack:** Rust, `unicode-normalization` (already in the tree), sqlx
SQLite, the canonical `norte-testkit` corpus, no new dependencies.

**Issues:** #129, #128, #130, #126, #127.

---

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-core/src/rename/plan.rs` | `name_key`: the fold delta between `to_lowercase` and case folding (D1). |
| `crates/norte-testkit/src/corpus/names.json` | Four new fixtures; the count pin in `corpus.rs`'s doctest moves 38 → 42 (D1). |
| `crates/norte-core/src/undo.rs` | `feasible`: two-tier occupancy, exact bytes block, fold-only twins do not (D2). |
| `crates/norte-core/tests/*rename_batch*.rs` | The five untested claims (D3). |
| `crates/norte-proto/src/task.rs` (wherever `TaskKind` lives) | `#[non_exhaustive]` (D4). |
| `crates/norte-core/src/journal.rs` | The format entry, its verification, and the honest downgrade report (D5). |
| `docs/adr/` | One new ADR for D5. |

---

## Task D1: `name_key` folds the way a filesystem folds (#129)

**Files:**
- Modify: `crates/norte-core/src/rename/plan.rs:200-230` (`name_key`)
- Modify: `crates/norte-testkit/src/corpus/names.json`
- Modify: `crates/norte-testkit/src/corpus.rs:30` (the `assert_eq!(names.len(), 38)` doctest pin)
- Modify: `crates/norte-core/src/rename/plan.rs` (the property test's `HOSTILE` literal)

- [ ] **Step 1: Write the failing test**

In `plan.rs`'s test module. The pairs come from the issue's table; each is a
name a case-insensitive volume calls ONE file and `to_lowercase` calls two:

```rust
    /// A filesystem folds case with case FOLDING. `to_lowercase` is a
    /// lowercase MAPPING, and they disagree on about twenty code points. On
    /// each of those the planner used to answer two keys where APFS, NTFS and
    /// an ext4 `+F` directory answer one file — a missed collision, which is
    /// the one verdict the preview exists to produce.
    #[test]
    fn folding_uses_case_folding_not_the_lowercase_mapping() {
        // Final sigma: `to_lowercase` implements the Final_Sigma context rule,
        // so a word-final Σ lowercases to ς while a medial σ stays σ. The name
        // has no extension, which is the ordinary case — `ΟΔΟΣ.txt` is saved
        // only by the `t` after the dot, and that must not be load-bearing.
        assert_eq!(
            name_key("ΟΔΟΣ".as_bytes(), INSENSITIVE),
            name_key("οδοσ".as_bytes(), INSENSITIVE),
        );
        // MICRO SIGN vs GREEK SMALL LETTER MU: what a CP1252 origin produces
        // against what a Greek keyboard produces. The pair a real corpus hits.
        assert_eq!(
            name_key("µm.txt".as_bytes(), INSENSITIVE),
            name_key("μm.txt".as_bytes(), INSENSITIVE),
        );
        // LATIN SMALL LETTER LONG S.
        assert_eq!(
            name_key("ſ.txt".as_bytes(), INSENSITIVE),
            name_key("s.txt".as_bytes(), INSENSITIVE),
        );
        // The Greek symbol variants, each folded onto its ordinary letter.
        for (variant, ordinary) in [
            ("ϐ", "β"), ("ϑ", "θ"), ("ϰ", "κ"),
            ("ϖ", "π"), ("ϱ", "ρ"), ("ϵ", "ε"), ("ϕ", "φ"),
        ] {
            assert_eq!(
                name_key(variant.as_bytes(), INSENSITIVE),
                name_key(ordinary.as_bytes(), INSENSITIVE),
                "{variant} and {ordinary} are one file on a folding volume",
            );
        }
        // ẛ (U+1E9B) folds onto ṡ (U+1E61).
        assert_eq!(
            name_key("\u{1E9B}".as_bytes(), INSENSITIVE),
            name_key("\u{1E61}".as_bytes(), INSENSITIVE),
        );
    }

    /// A case-SENSITIVE directory is untouched by any of it: two names that a
    /// folding volume calls one file are two files here, and saying otherwise
    /// would refuse a rename that is perfectly legal on ext4.
    #[test]
    fn the_fold_delta_does_not_leak_into_a_case_sensitive_directory() {
        assert_ne!(
            name_key("ΟΔΟΣ".as_bytes(), SENSITIVE),
            name_key("οδοσ".as_bytes(), SENSITIVE),
        );
        assert_eq!(name_key("µm.txt".as_bytes(), SENSITIVE).as_ref(), "µm.txt".as_bytes());
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-core`
Expected: FAIL on the sigma assertion first.

- [ ] **Step 3: Implement the delta**

In `name_key`, between `to_lowercase()` and the second NFC pass, remap the
enumerable delta. The set, from the issue: `ς→σ`, `U+00B5→U+03BC`,
`U+017F→s`, `U+1E9B→U+1E61`, `ϐ→β`, `ϑ→θ`, `ϰ→κ`, `ϖ→π`, `ϱ→ρ`, `ϵ→ε`,
`ϕ→φ`, `U+1C80..=U+1C88` onto their Cyrillic ordinaries, `U+0345` and
`U+1FBE` → `U+03B9`.

Write it as a `const fn`-shaped `match` over `char`, not a HashMap: it is a
closed set, it is on the hot path of every plan, and a `match` is what the
compiler turns into a jump table. Document at the function that this is the
**delta**, not a reimplementation of case folding — full folding also expands
(`ß→ss`, `ﬁ→fi`), and the issue does not ask for expansion because a
filesystem's own table does not expand either.

Update the rustdoc at `plan.rs:177` that currently NAMES this under-reporting
as known residue: it is no longer residue.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: PASS.

- [ ] **Step 5: Add the four corpus fixtures**

`crates/norte-testkit/src/corpus/names.json` — the shape is
`{"id": …, "hex": …, "why": …}`, and `why` is a paragraph explaining what
breaks without it (read the neighbouring entries; that register is the house
style and the corpus is the documentation).

| id | hex | pairs with |
| --- | --- | --- |
| `greek_uppercase_final_sigma` | `ce9fce94ce9fcea3` | the next row |
| `greek_medial_sigma_twin` | `cebfceb4cebfcf83` | the previous row |
| `micro_sign_mu` | `c2b56d2e747874` | the next row |
| `greek_mu_twin` | `cebc6d2e747874` | the previous row |

Then move the count pin: `crates/norte-testkit/src/corpus.rs:30`,
`assert_eq!(names.len(), 38)` → `42`.

- [ ] **Step 6: Point the property test at the canonical corpus**

`rename::plan`'s proptest draws from a private `HOSTILE` literal, which is why
it was green while this bug was live. Replace that literal with a draw from
the canonical corpus (`norte_testkit::corpus`), and make the `simulate` oracle
honour `caps` instead of modelling the directory byte-exactly — as written it
is structurally unable to catch a folding bug.

If honouring `caps` in the oracle turns out to re-implement `name_key` (making
the test tautological), stop and report it rather than shipping a test that
proves nothing: the alternative is a differential test against a byte-exact
model on a case-SENSITIVE directory only, plus the explicit pairs above.

- [ ] **Step 7: Run the suites**

Run: `just t norte-core`, `just t norte-testkit`, `just c`
Expected: green. Other crates pin the corpus count too — if one fails on 42,
that is the pin doing its job; update it.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-core/src/rename/plan.rs crates/norte-testkit/src/corpus/names.json \
        crates/norte-testkit/src/corpus.rs
git commit -m "fix(core): fold the way a filesystem folds, not the way to_lowercase does"
```

---

## Task D2: a fold-only twin does not block an undo (#128)

**Files:**
- Modify: `crates/norte-core/src/undo.rs:401-421` (`feasible`)
- Test: `crates/norte-core/tests/e2e_rename_batch.rs` (the existing pin
  `a_twin_directory_moves_the_right_file_and_refuses_a_half_undo` documents
  today's behaviour and must be updated, not deleted)

- [ ] **Step 1: Write the failing test**

```rust
/// An NFD twin sitting in the directory must not freeze a legitimate undo.
///
/// ext4 holds `é` NFC and `é` NFD as two files. A batch renames the NFD one
/// away; undoing it restores a name whose FOLDED key the surviving NFC twin
/// owns — but whose BYTES nobody owns. The executor's own no-clobber
/// (`renameat2(RENAME_NOREPLACE)`) compares bytes, so the restore is safe, and
/// refusing it costs the undo of everything older in the session too, because
/// session undo is strict LIFO.
#[tokio::test]
async fn a_fold_only_twin_does_not_block_the_undo() {
    // Directory: "é" NFC (untouched) and "é" NFD (renamed to plain.txt).
    // After the batch, undo_session must RUN, and the NFD name must come back
    // byte-for-byte — not as its NFC twin.
}

/// The other direction, unchanged: an occupant that owns the exact BYTES the
/// undo wants still blocks, and still blocks the whole unit.
#[tokio::test]
async fn an_exact_bytes_occupant_still_blocks_the_undo() {
}
```

Write both bodies against the existing test's setup in that file — it already
builds a twin directory, so copy its arrangement rather than inventing one.

- [ ] **Step 2: Run and watch the first fail**

Run: `just t norte-core`
Expected: `a_fold_only_twin_does_not_block_the_undo` FAILS with
`blocked: Some((seq, Conflict { Exists }))`.

- [ ] **Step 3: Give `feasible` the two-tier resolution**

`plan::Listing` already distinguishes an exact-bytes occupant from a fold-only
twin (`plan.rs:257-290`). `feasible` currently keeps one `HashSet` of folded
keys, which cannot tell them apart. Track both: the exact bytes present, and
the folded keys. A step is blocked when its destination's BYTES are occupied;
a fold-only collision is not a block, and the rustdoc must say why it is safe
(the executor compares bytes and never clobbers).

Keep the simulation honest about the steps' own effects: a step that vacates a
name must remove both its byte entry and its folded entry.

- [ ] **Step 4: Run the tests**

Run: `just t norte-core`
Expected: both new tests PASS; update
`a_twin_directory_moves_the_right_file_and_refuses_a_half_undo` to assert the
new, correct behaviour and say in its doc that #128 changed it.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/undo.rs crates/norte-core/tests/e2e_rename_batch.rs
git commit -m "fix(core): an undo is blocked by bytes, not by a folded twin"
```

---

## Task D3: the five claims nobody tests (#130)

**Files:**
- Modify: `crates/norte-core/tests/` (the batch-rename test files)
- Read first: `crates/norte-core/src/rename/exec.rs`

Read #130 in full (`gh issue view 130`) — it names each claim, the file that
argues it, and how to stage it. Five tests, in this order:

- [ ] **Step 1: A cancelled batch whose rollback got stuck must NOT answer `Cancelled`**

Combine the `CancelAfter` recorder with `fail_rename_at` on the rollback's
target. Assert `TaskState::Failed`. `Cancelled` promises a tree that came back;
a stuck rollback did not, and that distinction is the safety-critical one in
`exec.rs`.

- [ ] **Step 2: `Landed::Unknown` reaches `BatchReport::uncertain`**

Nothing in the workspace produces `uncertain = Some` today. Stage it with
`Faults::unavailable_for_next` / `disconnect_after` so both probe `stat`s fail
after a failed rename.

- [ ] **Step 3: `compensations_lost > 0`**

- [ ] **Step 4 and 5: the remaining two claims from the issue**

Read them from the issue body; each says how to stage it.

- [ ] **Step 6: Run and commit**

Run: `just t norte-core`, `just c`

```bash
git add crates/norte-core/tests
git commit -m "test(core): the executor's five arguments get a test each"
```

---

## Task D4: `TaskKind` is `#[non_exhaustive]` (#126)

**Files:**
- Modify: wherever `TaskKind` is defined in `crates/norte-proto`
- Modify: every `match` on it in the workspace that the compiler now rejects

- [ ] **Step 1: Add the attribute and let the compiler find the work**

`#[non_exhaustive]` on `TaskKind`. Every external `match` needs a wildcard arm;
matches INSIDE `norte-proto` are unaffected.

- [ ] **Step 2: Compile and fix**

Run: `just c`
Expected: errors listing each exhaustive match outside the crate. Add a
wildcard arm to each — and where the wildcard would silently do the wrong
thing, make it do the honest thing (`TaskKind::Unknown` already exists in the
enum for the wire's sake; follow whatever that variant's handler does).

- [ ] **Step 3: Does the wire move?**

It does not: `#[non_exhaustive]` is a Rust-API property, invisible in JSON. So
no protocol version bump — but say so explicitly in the commit body, and
dispatch `protocol-guardian` to confirm rather than assuming it.

- [ ] **Step 4: Run and commit**

Run: `just t norte-proto`, `just c`

```bash
git commit -am "fix(proto): TaskKind is non_exhaustive, so a new variant is not a break"
```

---

## Task D5: the journal says which format wrote it (#127)

**The decision, taken during planning:** the format version is an **entry
inside the hash chain**, adjacent to the genesis. Rejected alternatives, and
why, belong in the ADR: a `PRAGMA user_version` lives in the SQLite header,
outside the chain and outside what ADR 0025's HMAC anchor signs, so four bytes
nobody can see would make the audit refuse to open the journal and word it as
"upgrade norte"; a field in the anchor file is signed but absent for every
journal without an anchor, which is exactly the user who has configured
nothing.

**Files:**
- Create: `docs/adr/NNNN-journal-format-version.md` (use the `/adr` command so
  the number and MADR shape are right)
- Modify: `crates/norte-core/src/journal.rs`

- [ ] **Step 1: Write the ADR**

Cover: the failure (an older binary reports `Broken` on a file nobody touched —
it reads as tampering), the three candidates and why the in-chain entry wins,
what an older binary does when it meets the entry (this is the part that needs
care: today it does not ignore it), and the explicit acknowledgement that no
RELEASED binary carries a marker, so this cannot be fixed retroactively — only
forward.

- [ ] **Step 2: Write the failing test**

```rust
/// A journal written by a NEWER format must not read as tampering. The
/// difference matters more than it looks: `Broken` is an accusation, and
/// making it at a file nobody touched teaches a user to ignore the one signal
/// the journal exists to give.
#[tokio::test]
async fn a_newer_format_is_reported_as_a_newer_format_not_as_tampering() {
}

/// The format entry is INSIDE the chain, so altering it breaks the chain like
/// any other entry — which is the whole reason it is not a pragma.
#[tokio::test]
async fn tampering_with_the_format_entry_breaks_the_chain() {
}
```

- [ ] **Step 3: Implement**

The format entry is written at journal creation, adjacent to the genesis, and
carries the format version. `verify_chain` learns a third answer beyond
"intact" and "broken": written by a format this binary does not know. An
existing journal without the entry is a pre-migration journal and stays valid —
its absence is information, not a fault.

- [ ] **Step 4: Run, review, commit**

Run: `just t norte-core`, `just c`

Dispatch `security-reviewer` before committing: the question worth asking is
whether the new entry gives anyone a way to make a valid journal unreadable, or
to make an invalid one look pre-migration.

```bash
git add docs/adr crates/norte-core/src/journal.rs
git commit -m "feat(core): the journal says which format wrote it (#127)"
```

---

## Closing

- [ ] `just ci` — ONE run, at the end of the wave, not per task.
- [ ] Close #129, #128, #130, #126, #127 with what was built and what was not.
- [ ] Report which MINOR reviewer findings were skipped and why.

## Notes for whoever executes this

- **The gate is billed per plan.** `just t <crate>` freely; `just ci` once, at
  the end. Never re-run the gate to check a fix — reproduce with `just t`.
- **`just t` does not run doctests.** The corpus count pin in D1 lives in a
  doctest (`corpus.rs:30`); the whole shell-integration wave was left red by
  exactly this. Verify with the workspace doctest run the justfile's `test`
  recipe uses.
- **D1 changes what a collision IS.** Anything that pinned the old,
  under-reporting behaviour will now fail — read each failure before changing
  it, because one of them might be telling you the delta table is wrong.
