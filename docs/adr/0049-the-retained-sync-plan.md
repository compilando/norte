# 0049 - The approved synchronisation plan is retained, and `sync.apply` carries nothing but its hash

- Status: accepted
- Date: 2026-08-11
- Amended: 2026-08-12, when the branch closed. This ADR was written from the
  design spec, in task 1, before anything was built — the schema published in
  that same commit cites it thirty times. Tasks 2–13 moved five of its claims,
  and the amendment corrects them in place rather than appending a correction
  nobody would read: the payload of `sync.plan_done`, what the approval dialog
  leads with, what actually closes a spool's lifetime, what the overlap checks
  do and do not catch, and what the revalidation compares against. Each is
  marked below where the reasoning changed.
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
closes it with the `task_id`, the `plan_hash`, the counters, the blockers,
`blockers_total`, `executable` and **`dest_trash`**.

*(Amended: `dest_trash` was added mid-branch, in task 12, and is the field the
approval dialog leads with — see the next section for why it had to exist. The
`task_id` is there because one connection can have two plans in flight and the
hash is unknown until this notification arrives.)*

**`SyncApplyParams` has exactly one field.** There is no second parameter
through which a different intention could arrive, so "it executes what was
approved" is a property of the shape rather than a check someone can forget.

A hash that names no live spool is `Error::PlanStale`; a *malformed* hash dies
in `PlanHash`'s deserialiser as a params error, because "this is not a hash" and
"the world moved" are different facts and answering the second to someone who
sent the first lies to them about the state of the world. `PlanHash` itself is
reused from ADR 0042 unchanged.

#### The spool's lifetime, and what actually closes it

*(Amended. As first written this said the lifetime was "closed on all four
sides — applied, TTL, connection closed, and a sweep at daemon start-up". Three
of the four were true; the TTL was not. `SYNC_PLAN_TTL_MS` was only ever checked
inside `Spool::open`, so a plan **nobody opened was never collected** and the
effective lifetime was "until the connection closes or the daemon restarts".
Task 8 made the sentence true, and added a fifth side.)*

Five things end a spool, and each exists because the one before it does not
cover the case:

1. **Applied** — `Spool::remove` at every terminal state of the apply Task,
   including every early refusal after `open`, and a `Drop` guard for a
   cancelled dispatch.
2. **TTL** — `SYNC_PLAN_TTL_MS`, ten minutes, checked in `Spool::open` *and*
   reaped by `Spool::create`, which is where the growth happens and therefore
   needs no timer task. `.part` files are exempt: a plan over a network tree
   takes hours legitimately.
3. **The connection closes** — `drop_connection`, which leaves a tombstone when
   a writer is still open, because a plan over two identical trees produces zero
   steps, never touches the channel, and would otherwise close normally into a
   connection that had already gone.
4. **A sweep at daemon start-up**, which deletes **every** spool and not only
   the expired ones. At start-up there is no live connection, so every spool
   belongs to a dead one — and `conn_id` is a counter that restarts at zero, so
   keeping a fresh spool means keeping a file that authorises writes under an id
   the daemon is about to hand out again. Safe because one daemon per state
   directory is already enforced by the journal's exclusive lock.
5. **A retention cap**, `MAX_RETAINED_SYNC_PLANS = 16` per connection, refused
   with `OVERLOADED`. Time alone does not bound a burst: a client planning in a
   loop with different `include` lists mints a new digest, and therefore a new
   file, every time.

#### The filename is the key, and a filename is not a capability

The spool is keyed by `(connection, plan_hash)` — literally, as the file's name
under the state directory. That is what makes **nobody applies a plan they did
not produce** a property of the lookup rather than a rule in a handler, but the
name alone would not carry it, and this is the one thing on this branch that
would not have shipped as first designed. A filename is not a secret (anything
that can list the directory reads it) and neither is a `plan_hash`: `PlanHasher`
is unkeyed, so anyone who can write a plan can compute its digest and name a
file after it. Two things make the binding hold:

- **An in-memory registry of what this process issued.** `open` requires the
  `(conn_id, plan_hash)` pair *before touching the disk*, so a file this daemon
  did not write does not open however it is named. It closes `conn_id` reuse
  structurally (the registry is empty at start-up), stops two processes sharing
  a state directory from applying each other's plans, and makes a plan
  **single-use**: `open` takes the claim, so two concurrent applies of one hash
  cannot both write and leave an undo describing no state the tree was ever in.
- **The digest is recomputed at `open`**, over the steps, from the header's
  seed, and compared with the name — because the file states its own hash and a
  field a file states about itself is not evidence. The counters are recomputed
  in the same pass, since they decide which policy gates run.

#### The revalidation, and what it compares against

*(Amended. This said the executor checks whether the destination "still looks
the way the plan recorded it". The plan records nothing of the sort:
`SyncStep::size` is normatively the bytes the step **moves**, i.e. the source's,
and no field on a step describes the destination's prior state. The check had
nothing to compare against and the sentence was decorative.)*

The ten-minute TTL is the window in which the destination can change under an
approved plan, so before every `Overwrite` and every `DeleteTree` the executor
revalidates against a **`DestWitness { kind, size, mtime_ms }`** taken from the
destination entry the compare row already carried. The witness travels **in the
spool and not on the wire** — it is not something a client approves — and is
deliberately **outside `plan_hash`, so re-planning an unchanged tree still
yields the same digest**. Being outside the digest is also why a destructive
step that arrives with **no** witness is refused rather than degraded: deleting
the witness is exactly the edit the digest cannot see.

Its limits are stated rather than promised. A provider that lists without size
or mtime — `file://` is one — degrades the check to "still exists, still the
same kind", because declaring a conflict on a `None` would refuse every plan on
the most common filesystem. And a `DeleteTree` witnesses the directory, not its
contents: a directory's mtime moves only for direct children, so a subtree that
gained a hundred files two levels down revalidates clean and is destroyed whole.
That is the step with the largest blast radius and the weakest check.

### A step declares what it can undo, before approval

`StepReversal` travels **per step and in the plan**, not in the report:
`Delete` for something created, `RestoreTrash` for something buried, and
`Irreversible` with a `SyncReason` when the destination cannot give it back.
That is rule 4 applied step by step rather than apologised for afterwards, and
the human sees it while it is still possible to say no.

**The dialog leads with `dest_trash`, not with a count of irreversible steps.**

*(Amended. This ADR said the count of irreversible steps was the headline. Task
12 disproved it: a plan of nothing but copies against a destination with no
trash has `irreversible == 0` and gives back **nothing**, and it is byte for
byte identical on the wire to the same plan against a restorable trash — same
`Copy` steps, same `StepReversal::Delete`. What separates them is that undoing a
`created` entry routes through the trash (#65), so without one the undo skips it
into `skipped_created_no_trash`. No amount of reading `reversal` finds that.)*

So `SyncPlanDone` carries **`dest_trash: DestTrash`** —
`{ Restorable, Opaque, Absent, Unknown }` — required, and it is the first thing
the dialog says. Three values and not a boolean, because the two bad answers are
not the same news: with `Opaque` (macOS, Windows) what was replaced is sitting
in the system trash and can be fished out by hand; with `Absent` it is gone.
`DestTrash::of(has_trash, restorable)` lives next to the type so the wire's
mapping and the transducer's reversal table cannot drift. It does **not** enter
`plan_hash` and must not: both booleans already seed it, so two destinations
with different trashes already produce different digests.

A consumer therefore reads `step_undo(step, dest_trash)` and never
`step.reversal` alone. Painting the reversal column raw is the exact claim this
field exists to stop.

`criterion` and `confidence` travel per step too, discharging ADR 0048's
standing obligation: a report can say "overwritten because the mtime could not
be read" instead of "overwritten".

### The report is visible to agents, and it carries a journal-internal id

`SyncReportResult::batch_id` is **the first journal-internal identifier a
non-`User` actor receives.** It is opaque as a capability — no method takes a
`batch_id`, undo is by session — but it is a dense global counter, and therefore
a coarse measure of the daemon's cumulative journal batches, leaked to whoever
can call `sync.report` for a task they own. The rename-batch twin carries
nothing like it. It is accepted rather than removed because a client that
applied a plan has to be able to say *which* batch to talk about when undo grows
a per-batch entry point, and the alternative is a second opaque handle that maps
to it one-to-one and fools nobody.

What the report does **not** carry is any trash information, so a client that
lost `sync.plan_done` cannot tell after the fact whether a batch is recoverable.
That is a known gap, not an oversight — the applying client holds the `done` —
and it is filed as #170 so that it stays a decision rather than becoming a
surprise.

The journal does not change shape. Overwriting *is* `trashed` + `created` under
one `batch_id`, and undo walking `seq` descending gets the order right without
any care taken here.

### `rel` is a `RelPath`, not a `VPath`

*(This decision, and the three-valued `RootOverlap` two sections down, came out
of task 1's own protocol review rather than from the design spec, which said
`VPath` for a `rel` and a two-valued `Side` for the overlap. They are recorded
here because every later task was written against the corrected shapes.)*

A step's path is relative to **one** of the two roots, and it is a new wire type
— zero or more `Segment`s, percent-encoded and joined by `/`, with the empty
string meaning the root.

**Which root, is not always the source's**, and the ADR originally said
otherwise. Two shapes are measured against `dest_root`: a `Skip` produced by an
`Error` row on the destination side (an unlistable destination directory) and,
under `Mirror`, a `DeleteTree`. `SyncStep` carries no side field — adding one
for two shapes that write nothing was not worth a wire field — so a pane that
anchors every `rel` to the source column paints those in a tree they may not be
in. The rule is stated normatively on `SyncStep::rel`, and the frontend model
answers it with `anchor_of`, which returns `Either` for anything that *might* be
destination-relative rather than guessing.

Separately, `dest_rel: Option<RelPath>` is `Some` whenever the destination's
path bytes differ from the source's — the pairing key folds NFC and case at
every level, so `café/x.txt` under an NFD `café` is a real and common case. The
executor writes to `dest_root + dest_rel.unwrap_or(rel)`, which is the entry
that exists rather than the one the source spells; without it an `Overwrite`
grows a second file on ext4 and its `RestoreTrash` reversal is a lie, because
nothing was buried.

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

### Overlap is refused three times, and none of the three is complete

ADR 0048's inherited warning, discharged as far as it can be — and the amendment
here is a retraction, not an extension.

*(Amended. This ADR, and the design spec, said the walk-time guard catches "a
symlinked root, one SFTP host under two authorities, an archive opened by two
paths", and cited those three examples by name. It catches **none** of them.
Walking `/data` — a symlink to `/srv/data` — against `/srv/data` produces rows
whose paths all hang from `/data`, so no row ever "reaches the other root". The
same goes for the other two. What the walk-time guard really is, and is worth
having as, is **defence in depth against a provider that returns paths outside
the root it was asked to list**.)*

Three checks, in the order they run:

1. **Structurally, before planning.** If the roots are the same tree — equal, or
   one inside the other — `Error::OverlappingRoots`. `fs.compare` still permits
   the pair, because comparing `/a` against `/a/sub` costs a walk and writes
   nothing; planning writes into it has no such licence. When **either** root's
   provider does not declare `CASE_SENSITIVE`, the containment comparison case
   **folds** first — `fold_delta`, not `to_lowercase`, for the 22 code points
   where they diverge (#129) — because on APFS and NTFS `/Data` against
   `/data/backup` is byte-distinct, byte-unnested, and copies a tree into
   itself.
2. **By identity, before planning.** `Provider::node_id` on both roots with
   `FollowLinks::Yes` — `Yes` and not `No` is the whole point, since with the
   link's own identity `/data` and `/srv/data` answer differently and the check
   catches exactly nothing. One `stat` per root, once per plan. Equal and `Some`
   → `RootOverlap::Same`. Two deliberate narrowings: the ids are compared only
   when the provider *object* is the same (`Arc::ptr_eq`), because a `NodeId`
   from two backends is not comparable and a `MemProvider` index colliding with
   an ext4 inode would mean nothing; and an error or a `None` is **not** an
   overlap.
3. **During the walk.** A row whose absolute path reaches the other root prunes
   that subtree and raises a `SyncBlockerKind::OverlapDetected`. This is the
   defence-in-depth layer described above, not the one that catches aliasing.

**The residual, stated rather than papered over.** Check 2 closes the symlinked
root and the archive opened by two paths — the two cases where the provider can
answer with an identity. It does **not** close one SFTP host reached under two
authorities, because `node_id` is `None` on SFTP and FTP. That case remains
undetected by all three checks. What bounds it is the executor's per-step
revalidation and the fact that two roots which really are one tree yield `Same`
rows and a plan of no steps; it is a bound, not a refusal, and any future
provider that can answer an identity should implement `node_id` rather than rely
on it.

`Error::OverlappingRoots` carries a `RootOverlap`, which has **three** values —
`same`, `source_inside_dest`, `dest_inside_source` — and not a two-valued side.
"These two folders are the same" is not a degenerate case of "one is inside the
other"; it is the sentence a frontend paints, and a two-valued payload would
have to pick one by convention and then say something false.

### The bump

*(Amended in protocol 0.42.0: two of this ADR's types gain a MANDATORY field,
and both are the same omission — a message read without the context that
produced it. `SyncReportResult::dest_trash` (#170) repeats what
`SyncPlanDone::dest_trash` already carried, because the report is read by
clients that did not plan, reconnected, or dropped the notification, and
"what was copied and deleted" without "does any of it come back" is the
question this ADR says must be answered before a decision, not after.
`SyncFailure::kind` (#195) is the class the executor holds in hand and threw
away; without it, which root a failure's `rel` hangs from was inferred from
whether `dest_rel` happened to be present, which is sound only while a
`DeleteTree` never carries one — an invariant this ADR relied on and never
stated. Both are mandatory and neither has a `serde` default, for the reason
`dest_trash` was mandatory on `SyncPlanDone`: a default is an invented answer
to "can this be undone" and "which tree is this path in". The N/N-1 window is
what makes that safe — a daemon one minor behind does not negotiate with a
client one ahead — and a test now pins that the 0.41 shape is REFUSED rather
than defaulted.)*

0.40.0 is **additive**: new methods, new notifications, new types, two new
`TaskKind` variants and one new `Error` variant, with no existing type changing
shape. The published JSON Schema artifact gains lines and loses none. The two
`TaskKind` variants ship in the same bump as their methods, for ADR 0048's
reason: `task.progress` is broadcast to every human connection, so an N-1
client receives those two tokens without ever calling `sync.plan`, and degrades
them through `#[serde(other)]`.

## Consequences

### Positive

- What executes is what was approved, as a property of the wire's shape — and
  at the last hop too, since the digest is recomputed from the spool's steps at
  `open` rather than read off a field the file states about itself.
- A human is told what the destination's trash can give back, and what each step
  would cost, before approving rather than after.
- A plan of any size is approvable, at O(1) memory on both sides.
- Nobody applies a plan they did not produce, including an agent applying a
  human's, and nobody applies one twice.
- A report can explain each write by the rung that authorised it.
- A newer daemon can add step kinds and blockers without breaking an older
  frontend.

### Negative

- **The daemon now keeps files on disk that authorise writes.** They are
  owner-only (`0600` under a `0700` state directory), hold only paths, sizes and
  verdicts — never content — and expire five ways. It is still new
  security-relevant state where there was none, and the sweep at start-up exists
  because a crash leaves files behind and nobody else will collect them.
- **A spool is a full relative listing of both trees, with sizes and verdicts,
  and "owner-only" does not mean what a reader would hope.** The mode protects
  against other OS users. It does **not** protect against an in-daemon actor:
  nothing in the policy or VFS layer excludes the state directory from a scope
  grant over `$HOME`, so an agent with read scope there can `fs.read` a spool and
  obtain a recursive inventory of two trees it has no scope over — and, in the
  same directory, `journal.db`, the whole mutation history. The *class* of
  problem predates this ADR; what is new is the volume and the trigger, since
  anyone who can plan can now produce one on demand. Filed as #165 rather than fixed
  here, because the fix is a policy-layer exclusion that touches every method,
  not a spool change.
- **The ten-minute TTL is a window in which the world can move.** A `DestWitness`
  and one `stat` per destructive step are what close it, at the cost of a round
  trip per overwrite and per delete on a remote provider — and they close it only
  as far as the section above admits.
- **A `Daemon` bound over an `Engine` with no `with_policy` gates nothing**
  (`AllowAll` is the default), so one `sync.apply` from any actor rewrites a
  whole subtree. This is pre-existing and shared with `fs.copy`/`fs.delete`
  since M3, and no shipped binary does it — but sync is the first method whose
  blast radius under that hole is an entire tree, and the fail-closed treatment
  of the spool and of the journal invites a reader to assume the third leg is
  fail-closed too. It is not. Stated on `sync_apply_as`, and filed as #166.
- **Every future step kind owes a `StepReversal` and, where it cannot give one,
  a `SyncReason`.** `SyncStep::shape_is_consistent()` makes that structural for
  this binary, but a kind added by an N+1 daemon reaches an N−1 client as
  `Unknown` with whatever reversal it chose. The vocabulary rots exactly the way
  ADR 0048 says `confidence` would if a criterion were ever added without one:
  the counters degrade honestly (`unknown_kind`, `unmeasured_steps`, and
  `SyncPlan::outlook()` downgrading to `Unclear` on a reversal this build cannot
  name), but only because each of those was added deliberately, and the next
  addition has to do the same.
- **Two binaries at 0.40.0 from different commits of this branch are not
  interchangeable.** `SyncPlanDone` gained a required field (`dest_trash`)
  mid-branch, and `version_compatible` cannot see a change that does not move the
  number. A newer client against an older daemon fails to decode the close and
  waits forever for a plan that never closes. Harmless once 0.40.0 ships, and
  recorded in the `PROTOCOL_VERSION` doc so nobody diagnoses it as a hang.
- **A cancelled apply is not resumable.** It leaves a closed, undoable batch,
  and re-planning is how you continue. Resumption is a real feature and it is
  deliberately not this one.
- **`SyncCompareOptions` duplicates five fields of `FsCompareParams`.**
  Restructuring the published type with `#[serde(flatten)]` would have changed
  how it deserialises — a buffered map, a different path for type errors —
  even though the JSON object looks the same, and that is the kind of change
  this bump does not make. Adding an *optional* field is not that, and 0.40.0
  does add one: `descend_orphans` on both types, absent by default, so a 0.39
  request is still byte for byte a 0.39 request. What now enforces that the two
  move together is a test,
  `las_dos_caras_de_las_opciones_de_comparacion_no_divergen`, which pins their
  field sets, their values and their defaults against each other.
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
