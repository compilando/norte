# 0178 - A click outside closes a light popup, never a question

- Status: accepted
- Date: 2026-10-12
- Decision makers: Oscar González
- Related: ADR 0069 (the renderer does not decide), ADR 0077 (same gesture,
  same meaning in both frontends), ADR 0103 (modals with roles).

## Context and problem statement

The sort menu at the foot of the window, the search box and the other
pickers did not close with a click outside them; only the menu bar and the
right-click menu did. Every desktop popup closes that way, so the click did
nothing and the reader had to find `Esc`. In the terminal frontend no
overlay answered the mouse at all.

But not every overlay is a popup. A delete confirmation, the first-run
wizard or a rename plan waiting for approval is a question, and a stray
click must not answer it.

## Decision

Two classes, decided by the host:

- **Light popups close as `Esc` closes them**: the search box, the pickers
  (sort/columns, theme, profiles, layouts, connections, the terminal's
  list and rename), the search form, the go-to popup, help, and a plugin's
  or a program's output.
- **Questions and whole screens stay**: dialogs, the wizard, settings, the
  extension and agent managers, the viewer, compare, sync, and the AI
  rename and organize reviews.

The click is translated into the `Esc` of the overlay in front, through the
same key path, so each popup keeps its own cancel (a theme preview
reverts). Where `Esc` only drops a filter — help, a history selector — the
click drops it and closes all the same. "In front" is the overlay that owns
the keys:

- **Window**: a press on an overlay's veil — its root, outside its box —
  sends `overlay_dismiss` (bridge 112). The renderer does not know which
  overlay is light; the host does, walking `key_of_an_overlay`'s order. The
  search form travels there as a dialog and is the one dialog that is
  light: it is a form, not a question, and a popup in the terminal.
- **Terminal**: a light popup paints its box with `clear_popup`, which
  records it; a question or a screen painted over it uses `clear_modal`,
  which forgets it. A click outside a recorded box synthesizes `Esc`. The box
  travels in `App::painted_popup`: the run loop awaits between the paint and
  the mouse, and the runtime may resume it on another thread.

## Consequences

- A click outside a question does nothing, as before.
- Inside a terminal popup the mouse still does nothing; only outside it
  acts. The layout, profile and connections pickers now block the mouse
  like the other pickers: a click inside them reached the listing beneath.
- In the terminal the `Esc` goes where the keys go, and that order is not
  the paint order everywhere: with compare open and the search box opened
  over it from the menu, `Esc` — typed or clicked — reaches compare. A
  mismatch of `on_key`, not of this decision.
- A new overlay has to pick a class: in the window, an arm in
  `dismiss_overlay`; in the terminal, `clear_popup` or `clear_modal`.
