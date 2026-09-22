# 0148 — Repeating a failed transfer, and the thin line

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: 93 → 94.
- Related: ADR 0146 (the light progress bar), ADR 0147 (pausing),
  `docs/estudio-cola-y-progreso-2026-09-22.md` §4.4 and §3.1

## Context and problem statement

Two small pieces left over from the progress study.

A transfer that failed for a reason that was not the reader's doing — a
network that dropped, a destination that filled — had to be redone by hand,
even though both frontends already keep everything needed to repeat it: the
retry context captured for the collision dialog (#274, #98).

And the light bar tells you work is happening, but not WHERE it is landing.
The study's optional idea was a browser-style loading line on the destination
pane.

## Decision

1. **`task.retry` repeats the most recent failed transfer** with the same
   verb and the same options, reusing the retry context that was already
   stored. It asks again on a collision, as the first attempt did — repeating
   is not deciding something new. Bound to `Ctrl+Alt+R` beside pause's
   `Ctrl+Alt+K` in orthodox, cua and vim; the four transcribed presets leave
   it unbound with the reason in their header, as they do for cancel.
2. **A pane whose directory is receiving work draws a 2 px line** along its
   bottom border in the window, filling with the percentage of the least
   advanced task landing there — the same "the slowest one decides" rule the
   row progress already uses. No text, no row taken from the listing.
3. **The line travels in its own bridge change** (`slot_progress`, bridge 94)
   rather than in the listing: progress arrives at 30 Hz, and resending rows
   for two pixels would be paying a whole listing for them.
4. **Which panes count**: only tasks of this connection, alive, that count as
   work, whose recorded affected directories contain the pane's directory.
   Those directories are recorded when the task is queued, so the line never
   has to guess from a path in flight.

## Consequences

- A failed batch still fails per file; `task.retry` repeats the last
  transfer, not the batch. A per-batch retry would need the list of what did
  not land, which is what a report is for.
- The terminal gets no line: colouring a pane border fights the themes and
  the focus border, as the study said. The light bar already serves it.

## Alternatives considered

- **Retry from the processes panel row.** Nicer to point at, but it needs a
  panel open; a command works from anywhere and the panel can offer it later.
- **Reusing the collision dialog's policy on retry.** It would silently
  overwrite on the second attempt; asking again is the honest repeat.
