# Plan (c): the activity strip

Spec: `docs/superpowers/specs/2026-10-14-command-box-ai-design.md` §5.
ADR to write: 0187. Branch: `feat/activity-strip`, merged once.

## What exists and what changes

The status bar's `tasks` item already follows a burst of work (ADR 0146,
`norte-frontend/src/task_strip.rs`): threshold, ✓/✗, one bar, summed rate.
Both boards already keep a `tasks::Rate` per task. This plan adds a
**throughput history**: one ring per task and one for the burst, sampled
once a second, painted as braille in the terminal and as SVG in the window.
No wire change. Bridge 116 → 117.

Two decisions taken here, recorded in ADR 0187:

1. **No ASCII fallback.** The spec cites "the existing capability probe";
   there is none. The strip already paints block eighths (`▏▎▍`) and `⟳`
   that a console without braille lacks too, and a config key for a
   sparkline is surface nobody asked for. Braille or nothing.
2. **Sampling is the strip's clock, not the progress stream.** A stalled
   copy publishes nothing; sampling only on progress would freeze the last
   speed on screen. The TUI samples in `TaskBoard::tick`; the host in
   `annotate_strip`, which `next_change_ms` now wakes every second while a
   burst runs.

## Task 1: `norte-frontend::spark`

New `crates/norte-frontend/src/spark.rs`, `pub mod spark;` in `lib.rs`.

```rust
/// Seconds of history kept.
pub const SAMPLES: usize = 32;
/// Terminal cells of a painted sparkline: each braille cell holds two
/// columns, each column the max of two seconds.
pub const CELLS: usize = 8;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spark { /* ring: VecDeque<f64>, last_sec: Option<i64> */ }

impl Spark {
    /// Records `bps` (None = unknown, stored as 0) in second `now_ms/1000`.
    /// Same second: the sample is replaced (last wins). Skipped seconds are
    /// filled with the NEW value (a gap is ticks that did not run, not a
    /// stop). Clock going back: ignored. Keeps the last SAMPLES.
    pub fn record(&mut self, now_ms: i64, bps: Option<f64>);
    /// The series, oldest first, 0–100 against its own maximum, always
    /// SAMPLES long (left-padded with 0). Any non-zero sample is >= 1, so
    /// a slow trickle is visible.
    pub fn series(&self) -> Vec<u8>;
    pub fn is_empty(&self) -> bool;
}

/// `series` as CELLS braille cells: column i = max of samples 2i, 2i+1,
/// quantised to 0–4 dots from the bottom (non-zero → at least 1).
pub fn braille(series: &[u8]) -> String;
```

Braille bits, bottom-up: left column `0x40 0x04 0x02 0x01`, right column
`0x80 0x20 0x10 0x08`, base `U+2800`.

Tests (in the module): `record` same-second replace, gap fill, clock back,
cap at SAMPLES; `series` normalisation and the ≥1 floor; `braille` of
`[0;32]` is eight `U+2800`, of a full series is `⣿⣿⣿⣿⣿⣿⣿⣿`, of the ramp
`0,25,50,100` repeated gives `⣀⣤⣶⣿`-shaped cells (pin exact string); a
property test that `braille` is always CELLS chars for any series. Doctest on
`braille`.

## Task 2: the strip carries the burst's series

`task_strip.rs`:

- `TaskStrip` gains `spark: Spark`; `update` records the summed `bps` of the
  RUNNING tasks (paused contribute nothing, as for the rate). A new burst
  starts a fresh `Spark`.
- `StripView` gains `pub spark: Vec<u8>` — the series, filled only in
  `Running`; empty otherwise (Paused/Done/Failed paint no graph). Five
  literal sites in `task_strip.rs`, two in `statusbar.rs`.
- `next_change_ms`: while a burst runs, also the next whole second
  (`(now/1000 + 1) * 1000`), so the host wakes to sample.
- `forms`: in `Running`, the two LONGEST forms get `braille(&v.spark) + " "`
  in front when `spark` is non-empty and not all zero. The shorter forms stay
  as they are, so `statusbar::fit` drops the graph before anything else.

Tests: a burst fed 3 seconds of rising bps has a non-empty series whose last
value is 100; `next_change_ms` returns the next second while running and
nothing after the outcome expires; the longest form starts with eight braille
cells and the shortest has none; a paused burst has an empty `spark`. Fix the
statusbar tests' widths (an extra 9 cells on the longest form).

`cargo test -p norte-frontend --doc` after (struct gained a field).

## Task 3: the TUI

- `TaskRow` (`norte-tui/src/tasks.rs`) gains `spark: Spark`; `tick` records
  `row.rate.bps()` right after `row.rate.observe`, only while not terminal.
- The processes row (`ui/panels.rs`, near line 1038): `braille(&row.spark
  .series())` + space before `measured`, only while running and the spark
  is non-empty; add its `CELLS + 1` to `fixed`. A narrow panel (where
  `slot` would go below 8) drops the graph instead of the operand.
- The status bar needs nothing: it already paints `forms`.

Tests: `TestBackend` render of the processes panel with a row whose spark
has samples shows braille; a finished row shows none; the status-bar test in
`ui/status.rs` (line ~808) with three seconds of progress shows the graph at
120 columns and not at 60.

## Task 4: the window's host (bridge 117)

- `dto.rs`: `StatusProgressView` gains `#[serde(default, skip_serializing_if
  = "Vec::is_empty")] pub spark: Vec<u8>`; `TaskView` gains the same. Doc
  both: 0–100 against the series' own maximum, oldest first, `SAMPLES` long
  or empty.
- `BRIDGE_VERSION` 117, and its history line.
- `TaskViva` gains `spark: Spark`. `annotate_strip` records every live
  task's `rate.bps()` before `strip.update`, and writes `vista.spark` from
  it. `views.rs` fills `StatusProgressView.spark` from `StripView.spark`.
- Goldens: `NORTE_UPDATE_GOLDEN=1 just t norte-ui-host`; the diff shows the
  two fields and the bridge number only.

Tests: a controller test that feeds two progress notifications a second
apart plus a `Message::Strip` and finds a non-empty `spark` on both the
status item and the task view; a terminal task's `spark` is empty.

## Task 5: the webview

- `types.ts`: `spark?: number[]` on both views.
- New `render/spark.ts`: `sparkSvg(series: number[]): SVGSVGElement` — a
  `viewBox="0 0 32 8"` polyline plus a filled area, `stroke: currentColor`,
  fill `currentColor` at low opacity (NO new CSS variable: themes would have
  to feed it, the trap of the last `gui-ci`).
- `dom.ts`: the status item draws it before its text when `progress.spark`
  is present; the task row draws it before the rate.
- vitest: empty series renders nothing; a series renders one polyline with
  32 points; prettier + eslint clean.

`just gui-ci` before pushing `main`.

## Task 6: close

- ADR 0187 (the two decisions above), README index row.
- CHANGELOG "Added": the activity graph, both frontends.
- Help: one sentence in the topic that documents the status bar and the
  processes panel, en and es.
- Reviewers: `rust-reviewer` over the whole branch (no proto, no security
  surface). One `ci-fast` after task 3, the push of `main` is the second.
- Memory `caja-de-ordenes-0185`: (c) merged; next is the `ntc --demo` spec.
