# 0146 — A light progress bar in the status bar

- Status: accepted
- Date: 2026-09-22
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: 91 → 92.
- Related: ADR 0077 (one decision, one place), ADR 0115 (the processes panel
  opens by itself), ADR 0132 (status bar items),
  `docs/estudio-cola-y-progreso-2026-09-22.md` §3

## Context and problem statement

Copying a 300 MB file changed one thing on screen: `⟳ 1` in the status bar.
The percentage, rate and time left lived in the processes panel — which,
with `processes_panel = "auto"`, opened for EVERY task, a third of the screen
for a copy that took half a second, and then vanished. A quick copy was a
flash; a slow one needed a panel to be readable.

Other managers and editors converge on the same answer (VS Code, Nautilus,
Krusader's JobMan, browsers): a small bar that lives in chrome that already
exists, does not appear for instant work, aggregates several tasks, leaves a
brief "done" behind, and keeps failures around.

## Decision

**The `tasks` status item becomes a light progress bar, driven by one shared
state machine, `norte_frontend::task_strip::TaskStrip`.**

1. **It follows the burst, not each task.** Work arrives in bursts (mark,
   F5). The bar appears once the burst has run for 400 ms; with several tasks
   it shows ONE bar, the total by bytes (by entries if bytes are unknown,
   animated if neither: unknown is not 0 %). Finished tasks of the burst keep
   counting, so the bar never moves backwards.
2. **It says how it ended.** `✓ copied photo.jpg` for 1.5 s — also for a
   burst too short to show the bar, or a quick copy gives no sign of having
   happened — or `✗ n failed` for 10 s, as long as the failed row stays in the
   processes panel, where the reason is.
3. **It shrinks before anything is dropped.** A status item may now carry
   shorter forms; `statusbar::fit` tries them before removing any item. The
   bar loses the name, then the rate, then itself, and ends at `⟳ 3 41 %`.
4. **The automatic processes panel waits 2 s** before opening, and closes as
   before, when no work rows remain. What finishes sooner is told by the bar.
5. **Same machine, both frontends, injected clock.** The terminal feeds it
   on its 100 ms tick and draws the bar with eighth blocks; the window host
   feeds it on progress, schedules a wake-up for the changes that happen
   without progress (threshold, panel, end of `✓`), and sends
   `StatusItemView.progress` (bridge 92) so the renderer draws a real bar.
   The host's clock is tokio's, so tests pause and advance it.

## Consequences

- A single-file copy no longer opens a panel; it shows a bar if it takes
  longer than 400 ms and a `✓` either way.
- `[ui] processes_panel = "auto"` changes meaning slightly: "when work has
  lasted 2 s" instead of "when work starts". `"manual"` is unchanged.
- The terminal bar is 10 cells plus its frame; the window's is the same width
  in `ch`, so both status bars share one layout rule.
- Pausing and queueing (the rest of the study) will add states the bar must
  show; the timing policy is now in one place for them to extend.

## Alternatives considered

- **A separate toast or strip above the status bar.** More room, but a new
  surface that takes a line from the listing, which is what the panel already
  did too much of.
- **Progress drawn on the destination pane's border.** Pleasant, but it
  fights themes and the focus border; left as a later, optional step.
- **Making the thresholds configurable.** Not until someone needs it; the
  whole item can already be removed with `status_items`.
