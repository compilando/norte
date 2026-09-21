# 0135 — The scrollbar carries a marks ruler

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **89** — `BrowserSlotView.mark_ruler` and
  `ViewChange::BrowserHeader.mark_ruler`.
- Related: ADR 0131–0134 (the same VS Code pass), ADR 0066 (the window
  never sees the whole listing)

## Context and problem statement

The window paints only the rows in view; the listing itself stays in the
host. With marks spread over a long directory, nothing on screen said
where the others were: the footer counts them, but a count is not a place.
VS Code answers the same question for an editor with the overview ruler, a
thin strip beside the scrollbar with a tick wherever something is.

## Decision

**The host sends which stretches of the listing hold marks, not the marks.**
`PaneState::mark_ruler(spans)` cuts the listing into `MARK_RULER_SPANS`
(256) equal spans by position in `entries` — the same space as
`total_rows` — and returns, in order, the indices of the spans with at
least one mark. The payload is bounded by 256, not by the number of marks:
ten thousand marks do not cross the bridge as ten thousand numbers. It
travels in the listing's header, which always goes with the rows, so
marking without moving updates it.

**The window paints it as a background of the scroller.** A scrolling
element's own background stays put while its content moves, which is
exactly what a ruler of the whole listing needs, with no node to place and
nothing to measure at paint time. `markRulerImage` turns runs of
consecutive spans into bands of one `linear-gradient`, three pixels wide at
the right edge under the slider. Its colour is the mark's (`--mark-bg`)
mixed towards the text so a thin band still reads; no new theme role.

The span count is a constant on both sides, and a test in the window's
crate fails if `ui/src/types.ts` stops declaring the host's number.

**The terminal does not get one.** Its listings have no scrollbar to sit
beside, and a column of the pane is worth more than a ruler; the footer
already counts the marks.

## Consequences

- Marks outside the view are visible at a glance, in their place.
- The ruler shows where marks are, not how many: a span with one mark and
  a span with fifty paint the same.
- The ruler is a picture, not a control: clicking it scrolls like any
  click on the scrollbar track, not to the mark.
