# 0049 - The approved synchronisation plan is retained, and `sync.apply` carries nothing but its hash

- Status: accepted
- Date: 2026-08-11
- Decision makers: Oscar González
- Related: design
  `docs/superpowers/specs/2026-08-11-directory-sync-design.md` (spec 2 of
  roadmap item 1, whose "Wire", "The retained plan" and "Journal and undo"
  sections this ADR records); plan
  `docs/superpowers/plans/2026-08-11-directory-sync.md` (task 1); ADR 0048
  (comparison confidence — the rows this plan is a transducer over, and the
  overlap warning it explicitly hands down); ADR 0042 (batch rename wire — the
  `plan_hash` this reuses unchanged, and the closed-vocabulary precedent);
  roadmap item 1 of `docs/superpowers/specs/2026-08-07-post-alpha-roadmap.md`.

## Context and problem statement

ADR 0048 made a comparison declare what its criterion is worth. Nothing in it
writes a byte. This ADR is about the other half: turning those rows into an
approved, journalled, undoable one-way synchronisation.

The copying machinery already exists — `fs.copy`, the trash, the journal with
its `batch_id`, the scheduler, the policy gate. What is missing is **the plan as
a first-class wire type**, and three questions the wire has to answer before a
single file moves:

1. **How does a human approve half a million steps?** A plan over a large tree
   does not fit in one response, and re-deriving it at apply time means walking
   both trees twice — a three-hour comparison becomes six, with the tree
   changing in the gap. On a live tree a re-derived plan may never match the
   one that was approved, so a freshness token alone converges on nothing.
2. **What exactly gets executed?** If `sync.apply` takes the steps back from
   the client, then what runs is what the client last sent, not what the human
   last saw. That gap is the whole attack surface, and an agent is a client.
3. **What can be undone, and when is that known?** Rule 4 says every mutation
   is journalled with an undo path or an explicit `Irreversible` classification
   with a reason. Overwriting a file on a destination without a trash destroys
   data. Learning that from the report is learning it too late.

## Decision drivers

- **`fs.rename_batch` (ADR 0042) is the closest precedent and it is not enough
  here.** There, plan and apply both carry `pairs` and the hash is a freshness
  token the client can compute itself. That works because a batch is at most
  `FS_RENAME_BATCH_MAX_PAIRS` names in one directory. A sync plan is unbounded
  and cross-provider.
- **The rows already carry the evidence** (`criterion`, `confidence`), and ADR
  0048's standing obligation was that a consumer which writes files must be
  able to see which rung authorised each write.
- **The dangerous shape is overlap, not equality** — ADR 0048's inherited
  warning, addressed to exactly this spec.
- Forward compatibility, as always: an N+1 daemon will add step kinds and
  blocker classes; an N-1 frontend must degrade rather than fail a batch.

## Considered options

1. **`sync.apply` takes the plan back.** Symmetric with `fs.rename_batch`, no
   server state, no lifetime to manage. Rejected: the plan does not fit, and
   what executes stops being what was approved. The invariant would be a
   promise rather than a property.
2. **`sync.apply` takes only a hash, and the daemon re-derives the plan.** No
   retention, no expiry, no spool files on disk. Rejected: it doubles the
   walk, and on a tree being written to it may never converge — the honest
   failure mode is "`PlanStale`, forever", which is not a feature.
3. **`sync.apply` takes only a hash, and the daemon RETAINED the plan.**
   Chosen. The plan is written to a spool file as it is streamed to the client,
   keyed by `(connection, plan_hash)`.

## Decision

### The plan is retained, and the hash is all that travels

`sync.plan` is a cancellable Task. Its steps stream to the owner connection in
batches of at most `SYNC_STEPS_MAX_BATCH` and are written to a spool file at the
same time, so memory stays O(1) in the size of the plan. `sync.plan_done`
closes it with the `plan_hash`, the counters, the blockers and `executable`.

**`SyncApplyParams` has exactly one field.** There is no second parameter
through which a different intention could arrive, so "it executes what was
approved" is a property of the shape rather than a check someone can forget.

The spool's lifetime is closed on all four sides — applied, TTL
(`SYNC_PLAN_TTL_MS`, ten minutes), connection closed, and a sweep at daemon
start-up — and it is keyed to the connection that produced it, which is what
makes **nobody applies a plan they did not produce** a property of the lookup
rather than a rule in a handler. A hash that names no live spool is
`Error::PlanStale`; a *malformed* hash dies in `PlanHash`'s deserialiser as a
params error, because "this is not a hash" and "the world moved" are different
facts and answering the second to someone who sent the first lies to them about
the state of the world. `PlanHash` itself is reused from ADR 0042 unchanged.

The ten-minute TTL is the window in which the destination can change under an
approved plan, so the executor revalidates with one `stat` before every
destructive step. That `stat` is the only thing standing between the TTL and a
lost file.

### A step declares what it can undo, before approval

`StepReversal` travels **per step and in the plan**, not in the report:
`Delete` for something created, `RestoreTrash` for something buried, and
`Irreversible` with a `SyncReason` when the destination has no trash. The
approval dialog therefore leads with a count of irreversible steps while the
human can still say no. That is rule 4 applied step by step rather than
apologised for afterwards.

`criterion` and `confidence` travel per step too, discharging ADR 0048's
standing obligation: a report can say "overwritten because the mtime could not
be read" instead of "overwritten".

The journal does not change shape. Overwriting *is* `trashed` + `created` under
one `batch_id`, and undo walking `seq` descending gets the order right without
any care taken here.

### `rel` is a `RelPath`, not a `VPath`

A step's path is relative to the two roots, and it is a new wire type — zero or
more `Segment`s, percent-encoded and joined by `/`, with the empty string
meaning the root.

A `VPath` is "always absolute with respect to the provider's root" and always
carries a scheme and an authority, so using one for a relative path means
inventing both. The invention is not cosmetic. A plan from `file:///…` to
`sftp://nas/…` would have to pick one of them for a path that names an entry
under *both*; a third party reading the schema would pick the other, equally
"correctly", and the two would not compare equal. Worse, `plan_hash` covers the
plan's conclusions and `rel` is one of them — a field the daemon is instructed
to ignore cannot also be inside the token that authorises execution.

With `Segment`, "a `rel` never escapes its root" becomes a property of the
deserialiser: `/`, `.`, `..` and NUL are rejected after percent-decoding, so
`%2E%2E` smuggles nothing, and it holds on every peer rather than wherever
someone remembered to check.

### Where `#[serde(other)]` goes, and where it must not

The vocabulary that travels **daemon→client** — `SyncStepKind`,
`StepReversal`, `SyncReason`, `SyncBlockerKind`, `SyncFailureCause`,
`RootOverlap` — carries the fallback variant, as ADR 0048's enums do. These
tokens ride inside batches, and a hard parse failure would destroy 255 innocent
steps along with the one it did not understand.

`SyncMode` and `OnUnknown` travel **client→daemon** and carry **no** fallback.
An unrecognised mode dies in the deserialiser, because accepting an unknown
mode by default is accepting to delete by default, and there is no neutral
value between `Update` and `Mirror`. A request either happens or does not, so
refusing costs one round trip and buys refusal to act on a misunderstanding.

`OnUnknown` does have a serde *default* (`Copy`), and that is not the same
thing in smaller form: an **absent key** says "the client has no opinion" and
deserves a documented default, while an **unknown token** says "the client has
an opinion this daemon cannot honour" and dies in both enums. The default is
also safe where a `mode` default would not be — `mode` decides whether deleting
steps exist at all, whereas `on_unknown` only moves rows between two step kinds
the mode already authorised, and the human sees the result, with its
`confidence` and its reversal, before approving.

All of them are `#[non_exhaustive]`, for the reason #126 paid once.

### Overlap is refused twice, and the two refusals are different things

ADR 0048's inherited warning, discharged:

1. **Structurally, before planning.** If the roots are the same tree — equal,
   or one inside the other — `Error::OverlappingRoots`. `fs.compare` still
   permits the pair, because comparing `/a` against `/a/sub` costs a walk and
   writes nothing; planning writes into it has no such licence.
2. **During the walk.** Structural equality of two `VPath`s is not identity of
   two locations (a symlinked root, one SFTP host under two authorities, an
   archive opened by two paths), so a row whose absolute path reaches the other
   root prunes that subtree and raises a `SyncBlockerKind::OverlapDetected`.

`Error::OverlappingRoots` carries a `RootOverlap`, which has **three** values —
`same`, `source_inside_dest`, `dest_inside_source` — and not a two-valued side.
"These two folders are the same" is not a degenerate case of "one is inside the
other"; it is the sentence a frontend paints, and a two-valued payload would
have to pick one by convention and then say something false.

### The bump

0.40.0 is **additive**: new methods, new notifications, new types, two new
`TaskKind` variants and one new `Error` variant, with no existing type changing
shape. The published JSON Schema artifact gains lines and loses none. The two
`TaskKind` variants ship in the same bump as their methods, for ADR 0048's
reason: `task.progress` is broadcast to every human connection, so an N-1
client receives those two tokens without ever calling `sync.plan`, and degrades
them through `#[serde(other)]`.

## Consequences

### Positive

- What executes is what was approved, as a property of the wire's shape.
- A human sees the count of irreversible steps before approving, not after.
- A plan of any size is approvable, at O(1) memory on both sides.
- Nobody applies a plan they did not produce, including an agent applying a
  human's.
- A report can explain each write by the rung that authorised it.
- A newer daemon can add step kinds and blockers without breaking an older
  frontend.

### Negative

- **The daemon now keeps files on disk that authorise writes.** They are
  owner-only, live under the daemon's state directory, hold only paths, sizes
  and verdicts — never content — and expire four ways. It is still new
  security-relevant state where there was none, and the sweep at start-up
  exists because a crash leaves files behind and nobody else will collect them.
- **The ten-minute TTL is a window in which the world can move.** One `stat`
  per destructive step is what closes it, at the cost of a round trip per
  overwrite and per delete on a remote provider.
- **A cancelled apply is not resumable.** It leaves a closed, undoable batch,
  and re-planning is how you continue. Resumption is a real feature and it is
  deliberately not this one.
- **`SyncCompareOptions` duplicates five fields of `FsCompareParams`.**
  Restructuring the published type with `#[serde(flatten)]` would have changed
  its schema shape, which this bump promises not to do. The two must now be
  moved together when the cascade gains a rung, and nothing mechanical enforces
  that yet.
- **Two fields of that struct are present precisely so they can be refused.**
  `follow_symlinks` and `descend_orphans` are not the caller's to set in
  `sync.plan`, and a request that sets either is a params error. Removing them
  from the type would be worse: serde ignores unknown fields, so the request
  would be accepted and the value silently dropped — the quietly overwritten
  value the design refuses.
- **`RelPath` is a third path-ish type on the wire**, next to `VPath` and
  `Segment`. Every consumer has to learn when each applies. The rule is simple
  and stated on the type — absolute is a `VPath`, one name is a `Segment`,
  relative to a pair of roots is a `RelPath` — but it is one more thing.
- **A `Skip` step is emitted only for the notable.** Rows that decided `Same`
  produce nothing at all, so the plan's volume is bounded by how strange the
  tree is rather than by how big it is — but it also means a plan is not a
  transcript of the comparison, and a frontend that wanted one would have to
  ask for both.
