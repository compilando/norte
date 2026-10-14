# 0187 - Throughput graphs sample on the strip's clock

- Status: accepted
- Date: 2026-10-14
- Decision makers: Oscar González
- Related: ADR 0146 (the tasks strip), ADR 0077 (one rule for both
  frontends), spec `docs/superpowers/specs/2026-10-14-command-box-ai-design.md`
  §5, plan `docs/superpowers/plans/2026-10-14-activity-strip.md`.

## Context and problem statement

The tasks item says how far a burst is and how fast it goes now. It does
not say whether the speed is steady, climbing or stalling, which is what a
reader watching a long copy over a network wants to know. The spec asked
for a sparkline on the item and one per row of the processes panel, with
no wire change.

## Decision outcome

- **One ring per series, one sample per second** (`norte_frontend::spark`),
  32 seconds kept. The burst's series lives in `TaskStrip`; each task's on
  its board row. Painted as 8 braille cells in the terminal; sent to the
  window as the series (0–100 against its own maximum), which draws SVG.
- **Sampled on the strip's clock, not on progress.** A stalled task
  publishes nothing; a graph fed only by progress would keep its last speed
  on screen. `TaskStrip::next_change_ms` now wakes every whole second while
  a task actually runs (not paused, not queued), so the window's host,
  which only repaints on change, samples too. The host's `Rate` moved from
  the wall clock to the same monotonic clock, and a task whose byte count
  stands still for 1.5 s loses its rate there: the TUI gets the same by
  re-measuring on every tick.
- **The graph is a flag on the form, not text.** `Form.graph` and
  `StatusItemView.spark`, like the bar: each frontend draws it its own way,
  and `statusbar::fit` gives it up first.
- **No ASCII fallback.** The spec cited a capability probe that does not
  exist. The strip already paints block eighths and `⟳`, which a console
  without braille lacks too; a configuration key for a sparkline is surface
  nobody asked for.

### Consequences

- Bridge 117 (`StatusProgressView.spark`, `TaskView.spark`); no protocol
  change.
- While work runs the window's host wakes once a second and resends the
  rows only when a graph changed. A wake carries its deadline, so one that
  a newer arming superseded does nothing instead of arming a second timer.
- The TUI samples on its paint clock, which is the wall clock: a clock
  step back freezes the graph until time catches up.
