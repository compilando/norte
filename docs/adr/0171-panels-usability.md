# 0171 — Side panels: shared keys, a "behind" state, docks that yield

- Status: accepted
- Date: 2026-10-07
- Decision makers: Oscar González
- Protocol: unchanged. Shared crate: `panelbar::PanelState::Behind`,
  `PanelBarInput.present`, `panelbar::toggled_kind`.
- Related: ADR 0168 (the panel column), ADR 0170 (open panels shared),
  #329 / #331 (a hidden panel is not "open")

## Context and problem statement

A usability review of the panels (plan
`docs/superpowers/plans/2026-10-07-panels-usability.md`, both frontends)
found four things that were decisions, not bugs:

1. Each TUI side panel had its own allowlist of keys it let through, and
   they disagreed: from places, processes or the disk map, the other
   panels' keys did nothing, and `alt+x` closed nothing.
2. A panel in the layout but not in view (behind a tab, or dropped for
   lack of room) painted as closed — #329 and #331 refused to call it
   open, and the reader concluded it was gone.
3. A bottom dock with a fixed height kept it on any terminal: 8 rows of
   processes on a 24-row screen.
4. Details and Timeline had no key in any preset.

## Decision

1. **One shared set, looked up in the listing's keymap.** Every panel
   toggle, `layout.close-slot` and focus next/prev run from inside any
   side panel, through the same dispatch as from a listing, before the
   panel's own keys. `close-slot` there closes that panel.
2. **A third state, `Behind`.** The bar takes the tree's kinds
   (`present`) next to the placed ones (`open`). In the column it paints
   lit without the open rule; in the row bar and in the window, lit like
   open. That reverses #329's "closed" on purpose: a dimmed button read
   as "gone", and pressing it brings the panel forward rather than
   opening a second one.
3. **A bottom dock OPENS at most a third of the column**
   (`layout::dock_rows`), from 12 rows; below that the collapse sets it
   aside. Applied when the dock is inserted, never in `resolve`: capping
   at resolve time snapped every drag back and made `grow` invisible. A
   size the reader drags or grows afterwards is theirs, and preset
   layouts keep their own sizes.
4. **`alt+I` (Details, "Info") and `alt+T` (Timeline) in all seven
   presets.** Shifted because every lowercase alt+letter is taken in at
   least one preset; none of the transcribed managers has either panel.

5. **One name per panel, and the bar may use its short form**: "Map" /
   "Mapa" for the disk map, so the row bar keeps its names at 80
   columns.

## Consequences

- A panel's own keys can no longer use a chord the listing binds to a
  panel command — the shared set wins — except while the log's filter is
  being typed.
- A dock opened on a short terminal stays at that size when the
  terminal grows; the reader grows it.
