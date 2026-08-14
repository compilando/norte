# Debt wave W3 — comparison, names and the index

**Tier T1/T2.** Correctness of the folding and pairing rules, plus the
deferred review items of the semantic index.

**Rules:** as [W1](2026-08-13-debt-w1-mechanical.md), with two changes.
**One `encoding-auditor` over the whole branch diff at the close**, not one per
task — this is the wave it exists for. Model: the largest, for #153 and #154;
mid-tier for the rest.

**Branch:** `debt/w3-names`

| issue | crate(s) | what |
| --- | --- | --- |
| #154 | norte-encoding, norte-compare | one invalid byte disables NFC **and** case folding for the whole filename |
| #145 | norte-encoding | `name_key`: ext4 `+F` uses FULL fold, not simple fold — `straße`/`strasse` still miss as a collision |
| #151 | norte-compare, norte-core | unify the filename collision key: `name_key`/`fold_delta` duplicated. **Wants an ADR first** — the move relicenses AGPL-3.0-only code into MIT OR Apache-2.0 and changes a structural dependency. Settle #174's home question with it |
| #153 | norte-compare, norte-core | case folding is decided per PROVIDER, not per mount |
| #156 | norte-core | `fs.compare` hydrates on demand IN SERIES: over a network mount that is 2N chained round trips |
| #155 | norte-core | `fs.compare`/`fs.search`: a client that does not drain loses the subscription, and with it the completeness signal |
| #189 | norte-cli, norte-frontend | `cli-sync-blocker` joins in band AND drops the `side` the wire carries; wants a shared `blocker_anchor` |
| #192 | norte-frontend | NFC/NFD twins render as two identical strings with nothing to explain the arrow |
| #193 | norte-frontend | the root `rel` renders as nothing, against its own documented contract |
| #122 | norte-index, norte-ai | M4-IA-2 semantic index: deferred review items |

**#153 is the one that can grow.** "Per mount" needs somewhere to hang the
mount's identity; check whether volumes (#roadmap item 3, already built) already
carries it before inventing a second registry. If it needs a wire field, it
belongs in W4's single proto bump — split the issue rather than bumping twice.

**#152 (NFC singleton, no marker on the wire) is deliberately NOT here.** It is
a `norte-proto` change and rides W4's bump.

**#122 is a bag of unknown size.** Read it first and split it: whatever is
mechanical joins W1's tail, whatever is semantic stays.

**#189 is the one to design first.** `blocker_anchor` is the third member of
the family `anchor_of` and `render_failure` already form, and until it exists
the obvious code for anyone listing blockers reproduces #152 verbatim against
three destination paths.

**Close:** `just ci-fast`, then `just ci` once.
