# 0064 - What a plan can promise about the destination

- Status: accepted
- Date: 2026-08-19
- Decision makers: Oscar González
- Related: ADR 0049 (the retained sync plan), ADR 0054 (a provider answers
  about a location), protocol 0.52.0, issues #176, #163, #206, #215.

## Context and problem statement

A synchronisation plan is approved by a human and applied up to ten minutes
later. Between those two moments the destination can change, and three
separate reviews had named three different holes in what the plan knows about
it. They are one question asked three times: **what can a plan honestly
promise about a tree it does not own?**

1. **A `DeleteTree` revalidated the directory, not its contents** (#176). The
   `stat` of a directory only moves when its DIRECT children change, so a
   subtree that gained a hundred files two levels down revalidated clean and
   was deleted whole. The step with the widest blast radius had the weakest
   check.
2. **Nothing checked that a name legal at the source was legal at the
   destination** (#163). `CON`, `f:ads`, a trailing dot: all legal on ext4,
   none legal on NTFS. `f:ads` is the worst, because there it SUCCEEDS — it
   writes an alternate data stream, so the copy reports fine and the file is
   not there.
3. **A journal row that failed after a successful trash left a batch that
   cannot be undone** (#206). An `Overwrite` writes `trashed` then `created`;
   if the second fails, undo would have to delete a file it has no row for
   before restoring the buried one.

## Options considered

### For the tree deletion (#176)

- **Witness the root directory's `mtime`** — already done, and the issue
  itself says most filesystems do not move it when a grandchild changes.
- **Count the first level** — cheap, catches "somebody put something there
  while I was deciding".
- **A depth-bounded revalidation walk** — catches more, and costs a walk per
  destructive step at both plan and apply time. Over a network that is #156
  again.

### For the name legality (#163)

- **A table in the core** of "what NTFS forbids" — rejected: the core does not
  know what is on the other side of an `sftp://`, and a table that guesses is
  worse than no table.
- **A capability flag** (`WINDOWS_NAMING`) — a wire change, and it still puts
  the rules in the core rather than in the thing that knows them.
- **Ask the destination provider** — it is the only party that knows.

### For the blocked undo (#206)

- **Allocate both rows before either effect** — rejected for #160's own
  reason: a row for an effect that then fails is the same lie in the other
  direction, and the chain cannot be rewound.
- **Make the pair one journal entry with two paths** — a schema and
  hash-preimage change; ADR 0046's "bumping `JOURNAL_FORMAT` is not enough"
  clause applies.
- **Compensate**: delete the copy just written and un-bury the old one — two
  more mutations down the path where the journal has already proven
  unreliable, neither of which would be recorded either.
- **Say it** — cheapest, and does not pretend to fix it.

## Decision

**A plan promises what the destination can be asked, and says plainly what it
cannot.** Concretely:

1. **The witness of a `DeleteTree` carries the count of its first level.** It
   is filled in by the core's wiring at PLAN time — the transducer stays pure
   and providerless — and re-counted before destroying. Bounded at 4096
   entries: past that the count is not cheap any more, and both sides record
   "not counted", which compares equal to nothing. A count that could not be
   taken is `None`, never zero: the same rule the size and mtime already
   follow.
2. **Name legality is `Provider::name_is_legal`**, a pure default-true method
   the destination's provider overrides. An illegal name becomes
   `SyncBlockerKind::IllegalDestName` at plan time — a blocker and not a skip,
   for the same reason `TypeMismatchDir` is one: whoever asked for a mirror
   asked for the destination to end up like the source, and a name that cannot
   exist there is a structural divergence no later report repairs. Protocol
   0.52.0.
3. **The half-written journal pair is reported, not compensated.** It returns
   `StepError::Unrecoverable` with the buried path and its place in the trash
   inside the error, which is what turns "it failed" into "your file is here".

And, from #215, the same principle one layer down: **a comparison asks each
DIRECTORY**, not the two roots, because under one `file://` there are mounts
and the roots' answer is not theirs.

## Consequences

### Positive

- The three holes are closed as far as the destination can be asked, and each
  remaining gap is written where the check is instead of in an issue nobody
  reading the code will find.
- The count costs one listing per destructive step at plan time and one at
  apply time — on the step that was about to list the whole tree anyway.
- Name legality costs nothing: the default is a pure `true`, so a backend
  whose names are POSIX pays a function call.
- Asking per directory is free for every provider whose locations are alike
  (the trait default does no I/O) and cached by directory identity in the one
  that probes.

### Negative

- `norte-vfs-local`'s Win32 rules are written from the documentation, not from
  a run: no Windows machine verifies them here, exactly like #217, #220, #221
  and #222. What IS verified is the wiring — that a provider's "no" becomes a
  blocker instead of a mid-copy failure — with a test provider that refuses on
  purpose.
- A `DeleteTree` still cannot see a change in a GRANDCHILD. Closing that needs
  a revalidation walk, which is the cost the "an orphan is ONE journal entry"
  design exists to avoid.
- The blocked undo remains blocked. It is now legible, which is not the same
  as fixed, and this ADR is not pretending otherwise.

### Neutral

- Protocol 0.52.0 is additive: `SyncBlockerKind` is `#[non_exhaustive]` with
  `#[serde(other)] Unknown`, so a 0.51 client paints the blocker as an unknown
  class and **does not approve the plan** — which is the right degradation. A
  blocker nobody understands still blocks.
