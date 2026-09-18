# 0121 — The journal timeline, and undoing back to a point

- Status: accepted
- Date: 2026-09-18
- Decision makers: Oscar González
- Protocol: 0.75.0 → **0.76.0** (`journal.list`, `journal.undo_after`)
- Related: ADR 0077 (the same command means the same thing in both
  frontends), ADR 0089 (the RPC catalogue), ADR 0114 (navigation history),
  ADR 0120 (phase 6), #178 (an unreadable journal refuses), #171 (the undo's
  policy gate runs per unit inside the Task), #358 (the concurrency finding
  this review opened), spec
  `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md` (phase 7)

## Context and problem statement

Every mutation already went through the journal, hash-chained, with its
reversal — that is rule 4, and it has been true since M1. What did not exist
was a way to LOOK at it. `norte audit export` dumps the whole thing for a
machine; `policy.undo_session` undoes one agent session entirely. A human who
wanted to know "what have I done in the last ten minutes, and can I go back to
before I started" had nothing.

Phase 7 gives that: a timeline you read, and a cut you point at.

## Decision

**1. Two methods, and the undo is the SAME undo.** `journal.list` pages the
journal backwards; `journal.undo_after {seq}` reverts the human's entries
after `seq`. The second returns a Task and reports through the existing
`policy.undo_report`, because it is `undo_session` with a different selection
criterion — same units, same per-unit policy gate, same strict LIFO, same
counters. Both call one private body (`Engine::undo_entries`), so the rules
cannot drift apart; a second "similar" undo written alongside would have
diverged at the first rule anyone tuned.

**2. Pointing at a row KEEPS that row.** The cut is exclusive (`seq >`), and
the timeline sends the NEWEST `seq` of the group under the cursor. A human
who points at a row means "put me back to this", not "start destroying here".

**3. A batch enters whole or not at all.** This is the correction that matters
most, and it was a BLOCKER found in review, not a design I got right:
`revertible_for` returned every entry of an actor, so a `batch_id` always
reached `undo_units` complete. Adding `seq > ?` can slice one, and
`revert_batch` — which reverts "whole or nothing" — would have received half a
unit believing it whole, because its `debug_assert` only proves the slice it
was handed is internally consistent, and a slice is. The result would have
been a `fs.rename_batch` with half the names put back.

The query now excludes any batch with a member at or before the cut. Excluded
whole and not INCLUDED whole, because including it would undo entries before
the cut — the very row the human kept. Undoing too little is asked again;
undoing too much is not. And this cannot be left to "the cut will fall between
batches": batch seqs are not contiguous, since concurrent batch tasks
interleave (`alloc_batch` says so).

**4. The cut must NAME an existing entry.** `seq <= 0` would mean "everything I
have ever done", and that is exactly what a stale cursor or a client mapping
"nothing selected" to zero produces. `undo_after` checks the entry exists and
answers `NotFound` otherwise.

**5. Both methods are human-only, gated BEFORE params are parsed.** Same
criterion and same error (`PolicyDenied { rule: "not-approved" }`) as
`log.tail` and `host.volumes`. The journal is the complete list of what has
been touched on this machine, with source and destination: for an agent under
a scope it is an existence oracle over everything outside its enclosure, plus
a view of what other sessions did. And `undo_after` reverts the human's work —
an agent that could ask for it would erase the trace of its own. Gating before
the parse is what stops an agent telling "forbidden" from "bad params" by
fuzzing shapes. The embedded `Backend` has no actor and needs none: the socket
admits only the same uid, and embedded means the human's own process.

**6. The wire carries what a confirmation needs to be honest.** `JournalRow`
has `undoes_seq` and `undone` because without them a client cannot know what
the undo will skip: a compensation is written with the human's actor and a
real reversal, so it reads as undoable. The symptom the review found: undo five
things, reload, point at the same cut — the dialog promises ten and the undo
does zero. The rule of this screen is that a confirmation that does not say how
much is not a confirmation, so the count is computed with the same three
conditions the core's query applies.

**7. Paths on the wire are SANITISED text, not `VPath`.** Text, so one
unreadable entry cannot fail the page — a timeline missing a row is worse than
one with an odd row, because a mutation you cannot see is indistinguishable
from one that did not happen. Sanitised (`mask_terminal_hazards`, the same
treatment `fs.search` gives its lines) because a filename is chosen by whoever
creates the file, including an agent inside its scope, and this is the screen
where a human decides what to revert: a bidi override here repaints that
decision. `hostile` travels with the row, because sanitised text reads as
faithful. And the rustdoc says not to parse it for action: `seq` is the
identity.

**8. An unknown `reversal` token counts as NOT reversible.** In a tampered
journal, claiming something can be undone is the expensive lie.

**9. The panel has no keyboard shortcut in any preset, deliberately.** The
`alt+<letter>` space for panels is exhausted — b, j, l, t, z and the rest are
taken — and none is free across all seven. Binding it in three and not four
would be a capability half the readers do not have and are never told why,
which is the failure the seven-preset rule exists to prevent. It is reachable
in all seven from the panel bar, which is generated from the kind registry,
and from the View menu.

## Consequences

- The window does not have the timeline. The model
  (`norte_frontend::timeline`) is shared and holds everything that must not be
  decided twice: what a row is, that a batch is one row, which `seq` the cut
  sends, and how the count is computed. What is missing there is a renderer.
- #358 is open from this review: selection and execution of an undo are
  separated in time and undos are not serialized, so two concurrent
  `undo_after` calls can select the same entries. Reachable now in a way it
  was not when the only undo was per agent session.
- `Journal::page` returns `PageEntry` (entry + `undone`) rather than
  `JournalEntry`, because "already undone" is a computed property of the whole
  table, not a column.
- The cursor rule (`next_before_seq` only when the page came back full, and it
  is the last served `seq`, never `seq - 1`) lives in one function that both
  the daemon and the embedded backend call. It was written twice and tested
  zero times until the review said so.
