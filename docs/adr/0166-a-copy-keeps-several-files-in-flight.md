# 0166 — A copy keeps several files in flight

- Status: accepted
- Date: 2026-10-04
- Decision makers: Oscar González
- Related: ADR 0147 (pause), ADR 0149 (serial queue), ADR 0151 (root
  re-check), ADR 0164 (a paused task lends its slot), ADR 0165, #394

## Context

`copy_tree` copied one file after another. For many small files the cost
is not the bytes but the per-file round trips and the flush before
publishing: copying a `node_modules` took minutes where `cp -r` takes
seconds, and against a remote every file paid its latency in series.

A first attempt (4 files in flight over runs of consecutive files) was
reviewed and withdrawn. Doing it naively broke five things the copy
promises:

1. **The journal.** Dropping an in-flight copy — because a sibling failed,
   or on cancel — can leave its rename already applied (it runs on a
   blocking thread) and its `Created` never recorded: a file undo does not
   know (rule 4).
2. **Progress.** Each file set `bytes_done = base + written` with `base`
   read when it started: files in flight overwrote each other, the bar went
   backwards and never reached its total.
3. **The slot.** `TaskCtx::checkpoint` assumed one caller: on resume only
   the caller that lent the slot asked for it again, and the others ran with
   none (ADR 0164, 0149).
4. **Pause and the root re-check** (ADR 0147, 0151) are per entry; per run
   they would let gigabytes through.
5. **Folding.** On a case- or normalization-folding destination, `A.txt`
   and `a.txt` in flight together race for one name, and the result stops
   being the collision policy's.

## Decision

A run of CONSECUTIVE FILES of the plan is copied [`FILES_IN_FLIGHT`] = 4 at
a time; directories and symlinks stay serial and in plan order (the plan
is pre-order, so a file's directory always exists before it). Not with
`RenameAuto`, whose free-name search would race.

- **Never drop nor cancel a copy in flight because of a sibling.** On the
  first failure the run only stops starting files; the ones in flight end
  by their own path — committed and journaled, or released — and then the
  first error is returned. A cancel from a sibling would cut a
  server-side copy already sent (S3 `CopyObject`) or a post-commit
  disambiguation, leaving a published file with no `Created`. Only the
  task's own cancel cuts them, as before.
- **Progress by share.** Each file owns `FileBytes`: how much of itself is
  done; the task's `bytes_done` moves by the difference.
- **The slot is held or lent.** `PauseGate` tracks whether the slot is
  held; after a resume, every caller that did not lend it — at a
  checkpoint during the pause or arriving afterwards — waits until the
  lender holds it again.
- **Pause and root re-check stay per entry** (`EntryGate`), before every
  file a run starts, and run WHILE the files in flight are polled:
  awaited alone, the lender could sit unpolled and the task hang.
- **A run ends at a folded repeat**: a file whose WHOLE path folds (every
  segment, full case folding) to one already in the
  run starts the next run — `A.txt`/`a.txt`, and `D/x`/`d/x` whose
  folders the destination merges — so the two meet in order.

## Consequences

- Many small files copy several times faster locally and remotely; one
  large file is unchanged.
- Journal rows of a run are recorded in completion order, not plan order.
  Undo reverses by row, and the rows are of distinct files.
- Ending on a failure now waits for up to three files in flight to finish
  copying: a failure is reported a file's length later, never with a file
  undo cannot see.
- The run's key always folds FULLY, whatever the destination does: a
  stricter key only splits runs, so no destination — nor a subfolder
  mounted with another fold mode — can have two colliding names in flight.
  A run holds at most 256 files.
