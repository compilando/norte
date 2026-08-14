# Debt wave W3 — comparison, names and the index

**Tier T1/T2.** Correctness of the folding and pairing rules, plus the
deferred review items of the semantic index.

**Rules:** as [W1](2026-08-13-debt-w1-mechanical.md), including the private git
index and the "each agent dispatches its own `rust-reviewer`" rule.

**Plus one `encoding-auditor` over the whole branch diff at the close** — this
wave keeps its controller pass, and it is one of the two that earns it: the
folding rules are exactly the kind of thing where each task is right on its own
and the branch is wrong anyway, and W0's audit of that surface found four MAJORs
no per-task review would have seen.

Model: the largest for #153 and #154; mid-tier for the rest; cheap for the
per-agent reviews.

**Branch:** `debt/w3-names`

| issue | crate(s) | what |
| --- | --- | --- |
| #154 | norte-encoding, norte-compare | one invalid byte disables NFC **and** case folding for the whole filename |
| #145 | norte-encoding | `name_key`: ext4 `+F` uses FULL fold, not simple fold — `straße`/`strasse` still miss as a collision |
| #151 | norte-compare, norte-core | unify the filename collision key: `name_key`/`fold_delta` duplicated. **Wants an ADR first** — the move relicenses AGPL-3.0-only code into MIT OR Apache-2.0 and changes a structural dependency. Settle #174's home question with it |
| #153 | norte-compare, norte-core | case folding is decided per PROVIDER, not per mount |
| #156 | norte-core | `fs.compare` hydrates on demand IN SERIES: over a network mount that is 2N chained round trips |
| #189 | norte-cli, norte-frontend | `cli-sync-blocker` joins in band AND drops the `side` the wire carries; wants a shared `blocker_anchor` |
| #192 | norte-frontend | NFC/NFD twins render as two identical strings with nothing to explain the arrow |
| #193 | norte-frontend | the root `rel` renders as nothing, against its own documented contract |

**#153 is the one that can grow.** "Per mount" needs somewhere to hang the
mount's identity; check whether volumes (#roadmap item 3, already built) already
carries it before inventing a second registry. If it needs a wire field, it
belongs in W4's single proto bump — split the issue rather than bumping twice.

**#152 (NFC singleton, no marker on the wire) is deliberately NOT here.** It is
a `norte-proto` change and rides W4's bump.

**#122 is a bag of unknown size.** Read it first and split it: whatever is
mechanical joins W1's tail, whatever is semantic stays.

## Two left this wave, and one is not what its title says

**#155 lives in the daemon, not in the names.** `send_to_conn_impl`
(`crates/norte-core/src/daemon/server.rs`) evicts a connection from the
subscriber map, and the issue is a `security-reviewer` MAJOR that was applied
only half way. That is W4c's surface and W4c's reviewer.

**#122 is a bag, and there is a TOCTOU inside it.** A symlink swapped between
`index.build` and `index.embed` gets its 32 KiB prefix sent to the embedding
provider, which falsifies the module's own claim that not a byte of a denied
prefix is read; hard links defeat the path filter without even racing. There is
also a retention story: vectors are invertible to an approximation of the text
and survive a file being added to `denied_prefixes`. Neither is a naming
problem. The whole bag goes to W4, and its symlink half is a relative of #164.

## Two decisions taken up front, because two issues cannot start without them

**#153 — folding per mount.** The issue offers two ways out. Taken: **`compare()`
takes `Sides` as a parameter**, supplied by `norte-core`, which is the layer
that knows both roots. The alternative — a per-path capability query on
`Provider` — is a trait change every provider has to answer, which is #164's
shape and wants its own ADR. This does not need one.

**#145 — ext4 `+F`.** The unified key takes its fold MODE from capabilities:
simple (APFS/HFS+/NTFS/SMB) or full (ext4/f2fs `casefold`, whose kernel table is
built from `C + F` rows). Simple stays the default. Full can EXPAND a name —
`ß` → `ss`, `ﬁ` → `fi` — so it is not a cosmetic flag and must never be
switched on for a filesystem nobody probed.

**#189 is the one to design first.** `blocker_anchor` is the third member of
the family `anchor_of` and `render_failure` already form, and until it exists
the obvious code for anyone listing blockers reproduces #152 verbatim against
three destination paths.

**Close:** `just ci-fast`, then `just ci` once.
