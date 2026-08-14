# 0053 - A pairing that may join two files is skipped, not acted on

- Status: accepted
- Date: 2026-08-14
- Decision makers: Oscar González
- Related: #207 (a plan still acts on a pair joined only by an NFC singleton),
  #152 (the marker on the wire), ADR 0048 (comparison confidence), ADR 0049
  (the retained plan), ADR 0051 (the shared fold key), protocol 0.43.0.

## Context and problem statement

`fs.compare` pairs the two sides by a key that folds case and normalises. Most
of the time that is exactly right: `café` on macOS is NFD and on Linux is NFC,
and a synchroniser that treated them as two files would copy the tree twice.

But NFC normalisation is **not injective**. A handful of characters have
singleton decompositions — U+212A KELVIN SIGN normalises to the ASCII `K`,
U+2126 OHM SIGN to U+03A9 — so `K.txt` (KELVIN) and `K.txt` (ASCII) fold to one
key. Unicode calls them canonically equivalent; ext4 stores two files; a reader
sees two different characters.

0.42.0 put the fact on the wire (`CompareRow::paired_under`, with
`PairTransform::names_one_text()` as the predicate). Nothing read it. A
`Different` row for such a pair became an `Overwrite`, and applying it wrote
one file's bytes over an unrelated one. That is the data loss #152 described,
with the evidence already in hand.

## Decision drivers

- The dangerous case is rare and the ordinary cases are the reason the pairing
  key exists. Refusing `CaseFold` and `Normalization` would break the
  macOS↔Linux case the whole feature serves.
- Whatever the plan does must be visible BEFORE approval. A plan is approved by
  a human reading rows.
- A newer daemon may name a transformation this binary has never heard of, and
  "unknown" cannot mean "safe".

## Options considered

### Option A — a blocker: the whole plan refuses

Add a `SyncBlockerKind`, refuse the plan the way `AmbiguousDest` does for a
collision on the destination side.

- **Good:** the strongest possible statement, and consistent with how the same
  class of danger is treated when it lands on one side.
- **Bad:** one odd pair leaves an entire tree unsynchronised. The failure mode
  of a mirror that refuses to run is that people stop running it.
- **Bad:** new blocker vocabulary on the wire either way.

### Option B — skip the pair, with a reason

The row becomes a `SyncStepKind::Skip` carrying a reason. The plan stays
approvable and everything else synchronises.

- **Good:** the dangerous row is visible in the plan, before approval, with a
  sentence saying why.
- **Good:** the rest of the tree still syncs, so the feature keeps being used.
- **Bad:** the two files stay different, and nothing tells the human what to do
  about it beyond "these two names are not the same text".

### Option C — act, and warn in the pane

Keep the `Overwrite` and paint the warning.

- **Bad:** a warning attached to a destructive default is the shape that gets
  clicked through. The overwrite is not recoverable by reading it afterwards.

## Decision

**Option B.**

- The planner skips a pair whose `paired_under` says
  `names_one_text() == false`. That is the predicate, NOT the specific variant:
  a transformation named by a newer daemon is skipped too, because a build that
  cannot say what a transformation preserves cannot authorise an overwrite
  under it.
- `CaseFold` and `Normalization` keep acting. They are what the key is for.
- The reason is a NEW token, `SyncReason::NonInjectivePairing` (protocol
  0.43.0). Reusing `AmbiguousSource` was considered and rejected: it means "two
  names on the SOURCE collapse into one key", which is a different fact, and
  painting it here would explain the row to the human incorrectly. A wrong
  sentence is worse than a bump.
- The step carries BOTH spellings (`rel` and `dest_rel`) whenever the
  destination's differs, because that is the whole point of the row: the reader
  has to be able to see that the two names are not the same text.

## Consequences

- A plan that used to silently overwrite now shows a `Skip` with a reason. On
  the corpus pair (`singleton_kelvin_sign` / `ascii_capital_k`) that is a
  visible change in both panes and in `norte sync --json`.
- **The two files stay out of sync, and norte does not offer a way to resolve
  it.** Renaming one of them is the human's call, and this decision says the
  plan will not make it for them.
- The protocol window shifts to N/N-1 = 0.43/0.42. A 0.42 client decodes the
  reason as `Unknown` and paints "a reason this version cannot name" over a
  step that is already a `Skip` on the wire, so it acts neither more nor less.
- The predicate is best-effort in the daemon that produces it (#152 says so:
  it looks for a singleton character anywhere in either name, not for the one
  that separates them). A false positive costs one skipped pair with an
  explanation; a false negative would cost a file. The asymmetry is the point.
