# Debt wave W1 — mechanical

**Tier T0.** Collapsing duplicated code onto one owner. No design decisions.

**Rules for this wave** (they are the wave, not decoration):

- **No plan doc per issue. The GitHub issue IS the spec** — read it with
  `gh issue view <n>`.
- **One agent per crate cluster, all its issues in one lifetime.** Serial, one
  warm tree. No worktrees: a cold `target/` costs 250s and 30 GB, a warm one
  costs 78s.
- **Agents run `just t <crate>` and `just c`. Never `just ci` or `ci-fast`.**
  The controller spends the gate once, at the close.
- **One commit per issue.** One branch for the wave.
- Test-first on every bug (repo rule). A dedup that changes no behaviour is
  proved by the existing suite passing unedited — say so instead of adding a
  test that asserts nothing.
- Model: cheap. Nothing here needs the largest.
- Reviewers: none. #174's half is a comment; nothing here touches behaviour
  that a reviewer would catch, and the two that DID warrant one left this wave.

**Branch:** `debt/w1-mechanical`

| issue | crate(s) | what |
| --- | --- | --- |
| #172 | norte-core, norte-sync | three hand-written `is_at_or_under` → `RelPath::under(..).is_some()` |
| #174 (half) | norte-core, norte-sync | state the duplication is DELIBERATE, in both files. Not the move — see below |
| #169 | norte-testkit + 5 consumers | the corpus count assertion blocks every new fixture; make adding one cheap |
| #175 | norte-compare | `walk()` returns an unfused stream — any `select!` with a second arm panics past its end |
| #185 | norte-tui | the diff pane's block title joins both roots in one string; a name containing `↔` spoofs the pair |

**Close:** `just ci-fast`, then `just ci` once. Then
`superpowers:finishing-a-development-branch`.

## Two issues left this wave after reading their bodies

Tiering from titles was wrong, and this is the correction the wave's own rule
asked for ("verify before designing anything").

**#151 is ADR-sized, not mechanical.** `norte-core` is AGPL-3.0-only and the
natural homes for the shared fold key (`norte-vfs`, `norte-encoding`) are
MIT OR Apache-2.0, so the move RELICENSES the code — and it is a structural
dependency change. CLAUDE.md says both are ADR material. It also carries two
behavioural divergences to settle at the same time. Goes to W3, which already
owns the folding rules, and wants `/adr` first.

**#174 is not a fix at all; it is a decision already taken NOT to move it.**
`norte-core::hashing`'s copy is the journal's tamper-evident chain (ADR 0023)
and the audit export (ADR 0025). Its framing cannot change without invalidating
every existing `journal.db` — a migration, not a refactor. What stays in this
wave is the issue's own cheap half: **say so in both files**, so the next reader
does not "tidy" one of them. The ADR half rides with #151, because it is the
same licence question.

**Note on #169, corrected by C2's own experience.** The issue says the count is
asserted in five crates. Verify that number — but the premise is real and worse
than a miscount, because the two places that DO assert it fail at different
times. Adding `cause_join_spoof` to close an encoding finding in C2 broke:

1. `crates/norte-testkit/tests/corpus.rs:8` — caught by `just t norte-testkit`;
2. the `hostile_names` **doctest** in `crates/norte-testkit/src/corpus.rs` —
   NOT caught, because `nextest` does not run doctests. It surfaced two gate
   runs later, in `just ci-fast`.

That second one is the whole cost of #169: the friction is not "edit N places",
it is "the gate that the RED→GREEN loop uses cannot see one of them". Whatever
replaces the magic number has to be reachable from `just t`, and the module-doc
prose (`corpus.rs:1`) counts as a third copy.
