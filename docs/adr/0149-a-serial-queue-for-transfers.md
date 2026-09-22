# 0149 — A serial queue for transfers

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: 0.82.0 → 0.83.0 (`queued` on copy and move, `task.move`).
  Bridge: unchanged.
- Related: ADR 0147 (pausing), ADR 0146 (the light bar),
  `docs/estudio-cola-y-progreso-2026-09-22.md` §4.2 and §4.3

## Context and problem statement

The scheduler runs up to four tasks per scheme. On a spinning disk four
copies at once are SLOWER than four in a row — the heads spend the time
seeking — and the reader has no way to ask for one at a time. Total Commander
and Krusader both answer this with a queue.

## Decision

**One serial lane, chosen per transfer, with the order editable while the
work is still waiting.**

1. **`Lane::Cola` in the scheduler**, a queue with a single permit, separate
   from the per-scheme lanes. Its key cannot collide with a scheme's.
2. **ONE global queue**, not one per device. It is what Total Commander and
   Krusader do, it is what can be explained, and a per-device queue has to
   guess which device a path is on — across providers it cannot.
3. **`queued` travels with the transfer** (`FsCopyParams`, `FsMoveParams`,
   optional). It is not a daemon-wide mode: two clients on one daemon would
   fight over it, and the reader who wants a queue wants it for what THEY are
   about to launch.
4. **`task.move { task_id, up }`** reorders what has not started yet, by
   swapping the ordering key with its neighbour. A task that is already
   running does not move — pausing is for that — and the ack does not say
   which case it was, exactly like `task.cancel` and `task.pause`.
5. **The frontends carry a session switch**, `task.queue` (`Ctrl+Alt+Q`),
   plus `task.up` / `task.down` (`Ctrl+Alt+↑/↓`) on the selected task. The
   switch decides how the NEXT transfer enters; what is already queued stays
   queued. It is session state, not configuration: it is turned on for an
   afternoon of moving files and off afterwards.

## Consequences

- A client 0.83 against a daemon 0.82: `queued` is ignored, the transfer runs
  in parallel — slower on a spinning disk, never wrong — and `task.move`
  comes back `Unsupported`, which the frontends say.
- Reordering is a daemon operation: with the EMBEDDED scheduler it answers
  `Unsupported`, because the queue lives where the scheduler is.
- A renamed file and a sync plan never queue: the first is one step, and the
  second already decided the order of its own steps.

## Alternatives considered

- **A queue per destination device.** Smarter on paper; it needs a device
  identity that crosses providers, and it is hard to explain to the reader
  watching the second copy not start.
- **A daemon-wide "serial mode".** One switch for every client, and a client
  that never asked for it suddenly waiting behind another's copy.
- **Reordering by priority instead of by position.** `Priority` already
  exists, but it is a class ("the user is watching"), not a position; using
  it for order would make two unrelated things share one field.
