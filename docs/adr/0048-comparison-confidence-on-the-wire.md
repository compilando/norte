# 0048 - A comparison declares what its criterion is worth

- Status: accepted
- Date: 2026-08-11
- Decision makers: Oscar González
- Related: design
  `docs/superpowers/specs/2026-08-11-directory-comparison-design.md` (the
  cascade table and the "Wire" section this ADR records); plan
  `docs/superpowers/plans/2026-08-11-directory-comparison.md` (task C1); ADR
  0039 (provider attributes on the wire — the precedent for a vocabulary that
  degrades rather than fails); ADR 0042 (batch rename wire — the precedent for
  a closed verdict vocabulary the core never invents outside of); roadmap item
  1 of `docs/superpowers/specs/2026-08-07-post-alpha-roadmap.md`.

## Context and problem statement

`fs.compare` (proto 0.39.0) answers "are these two trees the same?" — the
question a user asks after every copy and the one an agent should be able to
ask itself. It emits one `CompareRow` per pair, streamed in batches.

The obvious wire for that is a verdict per row: `same`, `different`,
`only_left`, and so on. It is also wrong, and it is wrong in a way that only
shows up once real providers are on both sides.

A comparison is a cascade of criteria, cheapest first: presence, kind, symlink
target, size, mtime, and — only when the caller asks for it — a streaming
sha256. Each rung answers the same question with a *different amount of
evidence*:

- Two different sizes **prove** two different byte streams.
- Two mtimes 9 seconds apart **suggest** a difference and prove nothing: a
  restored backup, a `touch`, a copy that preserved timestamps, and a genuine
  edit are indistinguishable at that rung.
- An entry inside a zip has no size and no trustworthy mtime at all
  (`norte-vfs-archive` is read-only and its dates come from a container that
  may predate the files); an object store's ETag is a content hash only
  sometimes. The provider **cannot say**.

A bare `same` collapses those three into one token. The user reads "same" and
believes the copy is verified, when what actually happened was that two dates
were two seconds apart, or that neither side could produce a size. That is not
an imprecision, it is the wire telling the user something the engine never
concluded — and it is the exact failure the whole roadmap item exists to
avoid.

The third case is worse than the second, because "the provider cannot answer"
has an obvious wrong home: the error channel. Making an archive comparison
fail, or making the row an `Error`, would mean that comparing a directory
against a zip — the ordinary case of "did this backup extract correctly?" —
produces a screen full of failures, none of which are failures.

So the problem is two questions the wire has to answer separately: **which
rung decided this row**, and **what is that rung's answer worth**.

## Decision drivers

- Rows are emitted **final**: the cascade stops at the first rung that decides
  and never revisits, so a row cannot be corrected later by a more expensive
  rung. Whatever the row does not say, it will never say.
- The distinction must reach the user, not just the engine: §17 requires
  textual cues rather than colour alone, so `same`/`probable` and
  `same`/`certain` have to be two different things a pane can paint.
- Spec 2 (the synchronisation plan) will *write files* based on these rows. A
  plan that copies right-over-left because a row said `different` must be able
  to see that "different" came from a date, not from bytes.
- Forward compatibility: an N+1 daemon will add criteria (an ETag rung, an
  `rsync`-style rolling checksum). An N-1 frontend must degrade, never fail a
  whole batch — the shape `EntryKind::Other`, `TaskKind::Unknown` and
  `VolumeKind::Unknown` already use.

## Considered options

1. **Verdict only** (`same` | `different` | `only_left` | …). Smallest wire,
   and every file manager in the category ships this. Rejected: it is the
   collapse described above. The pane cannot distinguish a verified copy from
   a plausible one, the sync plan of spec 2 cannot tell a proof from a guess,
   and "the provider cannot say" has nowhere to go but the error channel,
   where it does not belong.

2. **Verdict + criterion, no confidence.** Say which rung decided
   (`mtime`, `size`, `hash`) and let the client infer what that is worth.
   Rejected: it moves the semantics into every client. Each frontend — TUI,
   GUI, CLI, MCP, and any third-party peer — would have to hard-code its own
   table of "which criteria are proofs", and the day a rung is added they all
   silently disagree about the new one. The knowledge belongs where the
   decision is made. It also cannot express the third case at all: `mtime`
   with no mtime available is still `mtime`.

3. **A numeric score** (0.0–1.0, or a percentage). Rejected: a number invites
   arithmetic nobody can justify — is a size match 0.8 or 0.9? — and every
   consumer would bucket it back into words for display anyway, each with its
   own thresholds. It also has no honest value for "cannot say": 0.0 reads as
   "certainly different", which is the opposite of what is meant.

4. **Verdict + criterion + a closed confidence vocabulary**, with "the
   provider cannot say" as a first-class *value* of that vocabulary rather
   than an error. Chosen.

## Decision

Every `CompareRow` carries three fields that travel together and mean three
different things:

- `verdict` — **what** was concluded (`same`, `different`, `only_left`,
  `only_right`, `type_mismatch`, `ambiguous`, `error`).
- `criterion` — **which rung** concluded it (`presence`, `kind`,
  `link_target`, `size`, `mtime`, `hash`). Also "how far the cascade had to
  go", so `different` says whether a size or 40 GB of bytes were compared.
- `confidence` — **what that rung's answer is worth** (`certain`, `probable`,
  `unknown`).

*(Amended in protocol 0.42.0, #152: a fourth, OPTIONAL field, `paired_under`.
It is not a fourth part of the verdict — it says nothing about what was
concluded — but about the PAIRING that produced the row at all. The key pairs
names by their NFC form and NFC is not injective: U+212A KELVIN SIGN normalises
to `K`, so two files that coexist on ext4 with no case-insensitivity involved
paired and were compared as one, and the resulting ordinary `same`/`different`
row had nowhere to say that its two halves are not the same name —
`reason_is_consistent` forbids a `CompareReason` outside `ambiguous`/`error`,
and relaxing that would make an N-1 client's own consistency check reject good
rows. `PairTransform` keeps `normalization_singleton` separate from `case_fold`
and `normalization` because a consumer that could only see "these differ in
bytes" would have to choose between trusting every normalised pairing — the bug
— and rejecting all of them, which breaks the macOS-to-Linux case this key
exists to serve. Note this against the reasoning below on `ambiguous`: a
collision is one-sided and is reported as one row per entry, whereas this marks
a row that HAS two sides, which is why it is a field and not a verdict. It
describes the two NAMES of this row, not its whole path: a consumer deciding
about a subtree must propagate an ancestor's mark itself, which pre-order
delivery makes possible. What a synchronisation should DO about such a pair is
deliberately not decided here — first the fact, then the policy.)*

`certain` means the criterion proves its verdict: presence, kind, symlink
target, a differing size, a hash. `probable` means it suggests it without
proof, which today is exactly the mtime rung in both directions. `unknown`
means the provider could not answer the criterion — no size, no trustworthy
date — and the row still carries the honest verdict the cascade fell through
to, typically `same`.

**`unknown` is an answer, not a failure.** A comparison against an archive or
an object store produces `same`/`unknown` rows and a completed task, not an
error and not an empty screen. The pane paints that differently from
`same`/`certain`, with a glyph rather than colour alone, and the user is
entitled to know exactly where the answer is not known.

Each of the four enums (`CompareVerdict`, `CompareCriterion`,
`CompareConfidence`, `CompareReason`) plus `Side` carries a `#[serde(other)]`
fallback so an N+1 daemon's new token degrades on decode instead of failing
the batch.

**On `CompareConfidence` that fallback is named `Unrecognised`, not
`Unknown`.** Everywhere else in this protocol the house convention for the
forward-compat variant is `Unknown`, and it is followed here — except on the
one enum where `Unknown` is already a meaningful value. "The provider cannot
say" and "a newer peer said something this build has never heard of" are
different facts about different things: the first is a property of the data
being compared, the second is a property of the two programs talking. Sharing
one name would turn an honest answer into a protocol mismatch, and a protocol
mismatch into an honest answer — in both directions, silently, with no way for
a client to tell which it was holding. The naming asymmetry is deliberate and
is the price of that distinction; a reviewer who "fixes" it for consistency is
removing information from the wire.

The bump to 0.39.0 is **additive**: a new method (`fs.compare`), a new
notification (`compare.rows`) and new types, with no existing type changing
shape. `TaskKind` gains a `Compare` variant in the same bump rather than
later — `fs.compare` returns a Task, and the `task_id` on every batch of rows
correlates to something a client has to be able to classify in `task.list`;
the variant is additive on the Rust side too, because `TaskKind` is
`#[non_exhaustive]` (#126), and an N-1 client degrades it to `TaskKind::Unknown`
through its `#[serde(other)]`. The published JSON Schema artifact gains lines
and loses none.

Three details of the shape are decided here rather than left to the engine,
because all three are cheap now and a wire break later (protocol-guardian's
review of task C1):

- **`CompareVerdict`, `CompareCriterion`, `CompareConfidence` and
  `CompareReason` are `#[non_exhaustive]`.** This ADR states outright that
  future rungs are coming; adding the attribute after a consumer has matched
  exhaustively is itself a breaking change, and #126 already paid that bill
  once for `TaskKind`. `Side` is not: left and right are closed by
  construction. `CompareCriteria` is not: it is a struct the engine and its
  tests construct by literal.
- **`mtime_tolerance_ms` is `u32`, not `i64`.** A negative tolerance makes
  `|Δ| > tolerance` true for every pair, so one typo turns two identical trees
  into a tree full of `different` — a quiet wrong answer on the hot path.
  `u32` moves the rejection into the deserialiser, which is stronger than a
  handler check that someone can forget, and 49 days of tolerance is more than
  any filesystem's granularity.
- **An `Ambiguous` row is one row per colliding entry**, carrying that entry in
  its own side's field with the other side absent and `side` naming where the
  collision happened. A collision is a property of one side, and the two entry
  fields on the row are *sides*, not collision members — a row that put the two
  colliding names in `left` and `right` would be describing a pair that paired
  successfully. Two names that collapse are two rows, never one and never
  deduplicated: the name that gets dropped here is exactly the file a later
  synchronisation was about to write over. The rule is normative on
  `CompareVerdict::Ambiguous`'s rustdoc and frozen in the `ambiguous_*`
  goldens, because the wire cannot express it: `sides_are_consistent()` exempts
  this verdict on purpose (there is no side rule a client could check, and
  claiming one would make an N-1 client distrust legitimate rows from an N+1
  daemon).

## Consequences

### Positive

- A `same` on the wire is never ambiguous again: the row says which rung
  earned it and how much that rung is worth, and every consumer reads the same
  answer.
- "The provider cannot say" has a home that is not the error channel, so
  comparing a directory against an archive is an ordinary comparison rather
  than a screen of failures.
- Spec 2 can propose a synchronisation direction from evidence rather than
  from a verdict alone: a `different` by mtime (which also records the newer
  side) is a different proposal from a `different` by hash.
- Errors are rows, not the end of the task, so an `EACCES` at leaf 40 000 of a
  three-hour comparison costs one row.
- A newer daemon can add a criterion without breaking any older client.

### Negative

- **Every future criterion must declare what confidence it earns, or the
  vocabulary rots.** This is the standing obligation this ADR creates. A rung
  added later that returns `certain` because that was the easiest default
  poisons every consumer downstream, including one that writes files. The
  cascade table in the design spec is the place that answer gets written down,
  and a new rung without a row there is an incomplete change.
- Three fields per row instead of one, on a notification that can carry
  millions of rows. Accepted: they are three short enum tokens, the batch is
  capped at `COMPARE_ROWS_MAX_BATCH` (256) and coalesced server-side, and the
  row already carries two whole `Entry` values, which dominate its size.
- The naming asymmetry (`Unrecognised` on one enum, `Unknown` on the other
  four) is a wart that every reader of the type will have to have explained
  once. The rustdoc on `CompareConfidence` explains it in place, and this ADR
  is where the reasoning is looked up.
- Showing both members of a collision in one row is now impossible without a
  new wire field, since `left`/`right` are sides. That is the deliberate cost
  of keeping the row's two entry fields meaning exactly one thing.
- `criterion` is not optional, so a row that no rung decided — an unreadable
  directory, a directory over `COMPARE_MAX_DIR_ENTRIES` — still has to name
  one, and names `presence` by convention. It is the least wrong token rather
  than a true one; the alternative (an optional field, or a "none" variant)
  was judged not worth a nullable field on every row of the hot path, given
  that `verdict: error` plus a typed `reason` already say precisely what
  happened.

### The warning spec 2 inherits: the dangerous shape is OVERLAP, not equality

Recorded here rather than left for spec 2 to rediscover, because both reviewers
of task C6 arrived at it independently and it is free to write down now and
expensive to learn later.

`fs.compare` refuses `left == right` with `INVALID_PARAMS`
(`handle_fs_compare`, `crates/norte-core/src/daemon/server.rs`). That refusal
exists because comparing a tree against itself is a screenful of `Same` and a
waste of a walk. **It is not a safety property, and spec 2 must not read it as
one.**

Two roots that OVERLAP are the dangerous shape, and they are allowed on
purpose. `/a` against `/a/sub` is a legitimate comparison here: `/a/sub` simply
appears twice, once as a subtree of the left root and once as the right root in
its entirety, and the answer — however odd — costs nothing but wasted work,
because **this spec writes nothing**. That is the entire reason it is permitted.

A synchronisation planner built on these rows has no such licence. "Copy
left→right" over an overlapping pair copies a subtree into itself: the
destination is inside the source, so every byte written creates more source to
walk, and the plan either recurses without end or duplicates a tree into its own
descendant. **Spec 2 must decide overlap itself, before it plans anything, and
must not assume the equality check above gave it anything.** Rejecting an
overlapping pair is not obviously the right answer either — comparing `/a`
against `/a/sub` is a reasonable thing to ask for — so the decision belongs at
the moment a plan is produced, not at the moment a comparison is.

The equality check is also **defeatable**, which is the same argument once more.
`p.left == p.right` is structural equality of two `VPath`s, not identity of two
locations. A symlinked root (`/data` → `/srv/data` compared against `/srv/data`),
one SFTP host reached under two authorities, an archive opened by two different
paths — each yields two unequal `VPath`s naming exactly one tree. Canonicalising
them is neither free nor always possible: a remote provider need not offer
`realpath`, and asking for one costs a round trip per comparison. Accepted here
for the reason overlap is accepted: the worst outcome is a wasted walk. Not
acceptable in spec 2, where the worst outcome is a write.
