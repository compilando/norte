# 0164 — A paused task gives its scheduler slot back

- Status: accepted
- Date: 2026-10-02
- Decision makers: Oscar González
- Related: ADR 0147 (pause), ADR 0149 (the serial queue), #385, #386

## Context

The scheduler admits up to four tasks per scheme at once, and one at a time
in the serial queue (ADR 0149). The slot is a semaphore permit the runner
held for the whole task.

Pause (ADR 0147) waits inside the body, at its checkpoints. So a paused task
kept its permit. Copy, search, compare, folder size, checksum, index and
sync planning all share the four `file` permits: four paused local copies
left every local search or folder size Pending for as long as the pause
lasted. In the serial queue, one paused task — even one paused before it
started — froze everything behind it.

## Options

1. **The pause lends the slot.** The permit lives in the task's pause gate.
   A checkpoint that pauses gives it back; on resume the task publishes
   `Pending` and asks the semaphore again, behind whoever is already
   waiting, still honouring cancel.
   - Good: pausing means "not using the machine now", for every lane. It
     is what Total Commander's queue does: pausing the running copy lets
     the next one go.
   - Bad: a resumed task does not continue at once if its slot was taken;
     it waits, and says so with `Pending`. In the serial queue, resuming
     no longer restores the order the user saw.
2. **A separate pool for short interactive reads** (folder size,
   checksum, search).
   - Good: those never wait behind long copies, paused or not.
   - Bad: does not touch the serial queue, which is the case that freezes
     outright; adds another limit to tune and explain.
3. **Lend the slot, and let the resumed task jump the queue.**
   - Good: resuming restores what was running.
   - Bad: the task that took the slot would have to be pre-empted or the
     limit exceeded; either starves others or breaks "four per scheme",
     "one in the queue".

## Decision

Option 1. `PauseGate` carries the task's slot (semaphore and permit). The
runner hands it the permit when the task starts; `TaskCtx::checkpoint`
lends it while paused and reacquires it on resume through the semaphore,
FIFO, racing the cancel token; `run_job` frees it when the body ends,
because the `TaskHandle` keeps a clone of the gate past the end. A task
paused before it starts lends it too.

A context built outside the scheduler (tests, direct callers) has an empty
slot, and pausing it lends nothing.

## Consequences

- Good: a paused task never blocks another one, in either lane.
- Good: no new knob, and no change to the wire: `Pending` already existed
  and every frontend paints it.
- Bad: "resume" can mean "wait for your turn". The task says `Pending`
  while it waits, which is the honest state, but a reader who resumed the
  head of the queue sees it go behind the one that started meanwhile.
- Bad: option 2's latency problem stays for tasks that are running, not
  paused — four long local copies still delay a folder size. Unchanged by
  this ADR.
