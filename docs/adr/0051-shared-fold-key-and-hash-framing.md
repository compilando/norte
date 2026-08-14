# 0051 - The fold key moves to the permissive layer; the journal's framing stays and is pinned equal

- Status: accepted
- Date: 2026-08-14
- Decision makers: Oscar González
- Related: #151 (the collision key duplicated between `norte-compare::key` and
  `norte-core::rename::plan`), #174 (the `plan_hash` framing helpers duplicated
  from `norte-core::hashing`), #129 (the 22-codepoint fold delta these two
  copies implement), ADR 0023 (the journal's tamper-evident hash chain),
  ADR 0025 (the audit export), ADR 0003 (crate structure and lints), debt wave
  W3 (`docs/superpowers/plans/2026-08-13-debt-w3-names.md`).

## Context and problem statement

Two functions are written twice in this workspace, and both duplications were
recorded as deliberate rather than fixed, because in each case the obvious
"extract and delete the copy" crosses a licence boundary.

**The filename collision key (#151).** `norte-compare::key::key_for` and
`norte-core::rename::plan::name_key` both answer "when are two filenames the
same name on this filesystem", and both carry their own implementation of the
same 22-codepoint fold delta (#129). The batched review of the comparison
engine found `key_for` shipping the pre-#129 key — `str::to_lowercase` without
the delta — which under-reports collisions on the final sigma, U+00B5, U+017F,
the Greek symbol variants, the historic Cyrillic letterforms, U+0345 and
U+FB05. It was fixed by copying the delta rather than sharing it.

**The hash framing (#174).** `norte-sync::hash` carries its own `feed`,
`feed_opt` and `hex_lower`, about twenty lines copied from
`norte_core::hashing`. They are the length-prefixed framing that stops two
different field sequences hashing alike. They are byte-identical in behaviour
and must stay that way.

The obstacle is the same in both cases and it is not technical:

- `norte-core` and `norte-compare` are **AGPL-3.0-only**. The natural shared
  homes — `norte-vfs`, `norte-encoding`, `norte-proto` — are
  **MIT OR Apache-2.0**. Moving code there relicenses it.
- Either move is a structural dependency change, which `CLAUDE.md` says is
  ADR-sized on its own.

And in the hashing case there is a second obstacle that has nothing to do with
licences: **`norte-core`'s copy is not a helper.** It is the journal's
tamper-evident hash chain (ADR 0023) and the anchor of the audit export
(ADR 0025). Its framing cannot change *at all* without invalidating every
existing `journal.db`. That is a migration, not a refactor.

Both copies will drift. That is the actual risk, and it has already happened
once: `key_for` shipped a stale key for an entire release cycle, and nothing
detected it because there was nothing to compare against.

## Decision drivers

- A duplication that "must stay byte-identical" needs something that *enforces*
  it, not a comment asking for it.
- Relicensing is the author's to grant, so the question is policy, not law:
  does this code belong in the publishable layer?
- Nothing may put the journal's existing chain at risk. A `journal.db` that
  stops verifying is data loss with a good conscience.
- Fewer crates beats more crates, unless the extra crate earns its
  maintenance.

## Options considered

### Option A — a new AGPL-3.0-only crate for shared primitives

Create `norte-primitives` (or similar) under AGPL, move both the fold key and
the framing there, and have `norte-core`, `norte-compare` and `norte-sync`
depend on it.

- **Good:** no relicensing decision at all; the AGPL boundary is preserved
  without thinking about it.
- **Good:** one home for both problems, decided once.
- **Bad:** a crate whose entire content is two small pure functions, added to a
  workspace that already has twenty-five. It has to be versioned, published,
  documented and kept alive.
- **Bad:** it puts an encoding primitive somewhere no one looking for encoding
  primitives will look. `norte-encoding` already owns NFC, case folding and
  terminal-hazard detection; a fold delta that is not there is a fold delta
  someone reimplements.
- **Bad:** it does nothing about the journal's copy, which is the half that
  carries real risk.

### Option B — relicense the fold key into `norte-encoding`, and share the framing through `norte-proto` while leaving the journal's copy in place, pinned

Move `fold_delta` and the `name_key` shape into `norte-encoding`
(MIT OR Apache-2.0). Move the framing primitives into `norte-proto`, which
already holds `PlanHash::from_digest` — so the hex half is already one and a
half copies rather than two — and migrate `norte-sync` onto them.

**Do not touch `norte-core::hashing`.** Instead, add a test that feeds both
implementations the same inputs, including the hostile corpus, and asserts the
bytes are equal.

- **Good:** the fold key ends up with its siblings. Both consumers already
  depend on `norte-encoding`, so this *removes* graph, it does not add any.
- **Good:** a pure function over bytes is exactly what the publishable layer is
  for. Someone else's file manager has the same problem.
- **Good:** the drift risk is closed by a failing test rather than by deleting
  code, which means the journal's chain is never in the blast radius of a
  refactor of the sync side.
- **Bad:** the hashing duplication *remains* a duplication. Two copies still
  exist; what changes is that they can no longer disagree in silence.
- **Bad:** relicensing is irreversible in practice. Code that goes to
  MIT OR Apache-2.0 cannot be walked back for anyone who already has it.

### Option C — leave both as they are, documented

What W1 did as a stopgap: state in each file that the duplication is
deliberate.

- **Good:** free, and it stops the next reader "tidying" the journal's copy.
- **Bad:** it is exactly the state that let `key_for` ship a stale key. A
  comment is not a mechanism.

## Decision

**Option B.**

The fold key is an encoding primitive, and it belongs with NFC, case folding
and hazard detection in `norte-encoding`, under MIT OR Apache-2.0. Both
consumers already depend on that crate, so the structural change is a
reduction. The relicensing is a policy call, and the policy is that a pure
function over bytes — one every file manager needs and gets wrong — is not
where this project's value lives, and is worth more shared than kept.

The framing goes to `norte-proto` for `norte-sync`'s use, and
**`norte-core::hashing` keeps its own copy, unchanged.** The journal's chain
does not move house to satisfy a tidiness argument. What closes the risk is a
golden test asserting the two produce identical bytes over the hostile corpus:
if anyone changes either one, that test says so, and the choice of which copy
was wrong stays with a human.

## Consequences

### Positive

- One implementation of the collision key. The class of bug that hid a stale
  key for a release cycle cannot recur silently.
- `norte-encoding` becomes the honest answer to "where do filename comparison
  rules live", which is the question that produced both copies.
- The journal's chain is untouched, so no `journal.db` is invalidated and no
  migration is needed.
- Two open issues close on one decision.

### Negative

- Relicensed code cannot be relicensed back for anyone who already received it.
  This is accepted deliberately, and only for primitives — nothing that
  encodes how norte works may cross that line on this precedent.
- The hashing duplication survives. Anyone reading `norte-sync::hash` still
  sees twenty lines that exist elsewhere, and the comment explaining why is
  load-bearing.
- One more golden test to keep alive, and it fails for a *good* reason exactly
  when someone is doing something dangerous — which is the point, but it will
  read as noise to whoever hits it first.

### Neutral

- `norte-compare` and `norte-core` stay AGPL-3.0-only. This ADR moves two
  primitives, not a boundary.
