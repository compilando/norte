# 0117 — The disk map is a task with a report, and the core measures it

- Status: accepted
- Date: 2026-09-16
- Decision makers: Oscar González
- Related: ADR 0037 (plugin kinds, the closed set of roles), ADR 0057 (the
  location capability and its budget), ADR 0080 (a read with a content gate),
  ADR 0089 (the RPC catalogue and its completeness gates), ADR 0116 (a plugin
  describes a panel; a zone's argument is never a path), spec
  `2026-09-15-historia-y-wow-design.md` (phase 4), plan
  `2026-09-16-fase4-mapa-de-disco.md`

## Context and problem statement

Phase 4 asks for a disk map: a slot that shows what the directory you are
looking at is made of, as a treemap, where clicking a rectangle enters that
child. Four questions had no obvious answer, and each is a place where this
could have gone wrong quietly.

1. **Who measures?** Phase 3 had just shown that a plugin can paint a whole
   slot, so a disk map looks like a plugin.
2. **How does the answer come back?** `fs.dir_size` already walks a tree and
   reports its total with no result type at all.
3. **What does "the map is incomplete" mean?** A walk over millions of entries
   meets directories it cannot read, gets cancelled, and finds directories with
   more children than any reply can carry — three different kinds of
   incomplete.
4. **What does a rectangle carry when it is clicked?** ADR 0116 pinned that a
   zone's argument is never a path.

## Decision

**The core measures it; it is not a plugin.** A `$HOME` is millions of entries
and needs a cancellable task (hard rule 3). The location capability a plugin
would use is budgeted at 4096 calls (ADR 0057) — deliberately, because that
budget is what makes granting it safe — so a plugin-based disk map would fail
on any real tree. The capability is not the obstacle here; it is working as
designed, and the conclusion is that this work belongs where tasks and
providers already live.

**A task WITH a report, not a task alone.** `fs.dir_size` carries its total in
the progress (`bytes_done`/`entries_done`) and explicitly invents no result
type: "the last snapshot IS the result". That works for one number. A list of
children with their sizes is the case this protocol already describes for its
siblings — "N digests do not fit in a task's outcome, and progress only knows
how to count" — so `fs.dir_usage` takes the shape of `fs.checksum`,
`archive.pack` and `fs.rename_batch`: the task does the work, a separate method
says what came out. Not a parameter on `fs.dir_size` either: different result
shape, different task class, and `fs.dir_size` takes N roots where this takes
one, because a map of two directories merged is a map of nothing.

**A stream was considered and rejected.** `compare.rows` and `search.hits` push
batches because their consumer paints them as they arrive. A treemap cannot lay
out a single rectangle until it knows every size, so a stream would deliver one
batch in the normal case — all of the machinery, none of the benefit. What a
stream would have bought is a bound, and a bound is cheaper stated directly.

**Three signals, because "incomplete" is three different facts.**

- `pending` — how many known children are still being measured. Unlike
  `fs.checksum`, whose denominator is the list the client sent, here nobody
  knows how many children exist until the root's own listing finishes.
- `listed` — whether that listing finished. Without it, a task cancelled while
  still listing reports `pending: 0` over a handful of children and reads as a
  finished map. This is the field that keeps `pending` honest.
- `omitted` — children that exist and did not fit the cap. Their bytes are
  still in the totals, so a map can paint the remainder as one rectangle: what
  is lost is their names, not their size.

**And `partial` goes on the CHILD, not on the report.** A whole-report flag can
only grey out the entire map; what a treemap can actually paint is *which*
rectangle is a lower bound. `ChecksumEntry.miss` is per entry for the same
reason. An unreadable directory never kills the count — dying on an `EACCES` at
leaf 40 000 would return nothing in exchange for all the work — so the map is
drawn and says which parts are floors.

**`children` is capped** (`DIR_USAGE_MAX_CHILDREN`). It would otherwise be the
only unbounded list in this protocol, and the decoder does not degrade: past
`MAX_FRAME_BYTES` the frame fails to decode rather than truncating. A
`/nix/store`, a Maildir or a `node_modules` root is 10^5 depth-1 children. Past
the cap the largest travel — which is what a map paints — and the rest are
counted in `omitted`, never dropped in silence: a silently truncated report
reads as the whole directory, which is the failure this protocol forbids by
hand in `fs.checksum`.

**A child's name is a `Segment`.** Not raw bytes, not a path. It is this
protocol's name type: it encodes non-UTF8 losslessly (hard rule 1) and rejects
separators, NUL and `.`/`..` *after* decoding, so `%2E%2E` cannot smuggle a
`..`. That makes ADR 0116's "the argument is never a path" an invariant of the
type rather than a check someone must remember to write, and it is what lets a
clicked rectangle resolve against the directory that was asked for.

**The map's frame is ours.** It reuses phase 3's `StyledFrame`, but its hits do
not pass through `norte_frontend::frame::zona_puede`: that filter exists
because in a plugin panel a third party picks both the label and the command
(ADR 0116). Here `squarify` picks them. A plugin cannot produce a `disk-map`
frame either — the kind is built in, and its frame comes from that function.

## Consequences

- `TaskKind::DirUsage` is observation, not board work: it measures and mutates
  nothing, so the processes panel does not open itself because someone looked
  at a directory. Both frontends must give it a label, or a running map paints
  as an unnamed "task" — which is exactly why `DirSize` and `Checksum` have
  theirs.
- The classification gate that was supposed to force that decision does not
  fire on its own: `counts_as_work` is a negative match, and the test beside it
  iterates a hand-written array, so a new kind is classified by default and
  never exercised. Both places are updated by hand; the test's rustdoc now says
  so instead of promising a safety net it does not have.
- `depth` ships in the wire although only `1` is served, so adding depth 2
  later needs no distinction between "absent" and "asked for 1". The type
  states the contract — `0` and over `DIR_USAGE_MAX_DEPTH` are rejected — since
  a server that silently clamps leaves a client believing it got what it asked
  for.
- The catalogue promises both methods in the commit that defines them, so the
  daemon and remote-client completeness tests are red until the core lands
  them. That is ADR 0089 working: the catalogue is the promise, the next task
  is the delivery.
- A 0.74 client against a 0.75 daemon loses the map and sees someone else's
  running measurement as an unnamed task. The other direction does not exist: a
  client whose minor is greater than the server's dies in `initialize`.
