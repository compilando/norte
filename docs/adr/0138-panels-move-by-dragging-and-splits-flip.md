# 0138 — Panels move by dragging, and splits flip

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **90** — the `move_slot { slot_id, target,
  zone }` action. New command: `layout.flip`, with no key in any preset.
- Related: ADR 0058 (slots and tabs), ADR 0133 (layout buttons), ADR 0134
  (panel groups), ADR 0077 (parity)

## Context and problem statement

Borders between panels could already be dragged to resize them, in both
frontends. What a panel could not do was change PLACE: the only ways to
rearrange the screen were the split commands, the presets and the layout
picker. VS Code lets you take a view by its title and drop it on another,
to one of its sides or into its tabs; and it can turn a group of editors
side by side into one above the other.

## Decision

**Drag by the title, drop on another panel.** In the window, a panel's
title — or one of its tabs — is a drag handle; in the terminal, the row of
a panel's top border is. Past a small threshold (six pixels, two cells) a
press becomes a drag, so a click still focuses, sorts or follows a
breadcrumb. Over another panel, the part where it would land is marked:
one half for a side, the whole panel for its centre. Releasing sends
`move_slot` in the window and moves directly in the terminal; `Esc` in the
window, or releasing over nothing, cancels.

The zone is the side nearest the pointer if it is within a quarter of the
panel, and otherwise the centre — one rule, `DropZone::at` in the shared
crate and `zonaDe` in the renderer.

**The tree does it: `Node::move_slot`.** The slot is taken out
(`close_slot`, which dissolves a split or a group left with one child) and
put back next to the destination's UNIT — the destination slot, or the
tab group it lives in, so dropping beside a tab splits the group instead
of entering it. If the unit's parent already splits along that axis and
the unit is weighted, the slot goes in as a sibling with the same weight
(three panels are thirds, not 1/2, 1/4, 1/4) — also beside a fixed-width
panel, which keeps its width instead of giving half of it away; otherwise
the unit is wrapped in a new split, half and half. Nothing is created or
lost: the moved slot keeps its id, kind, parameters and bindings. The
status bar and the tasks row are chrome: they neither move nor receive,
and the split that holds them is never used for sibling insertion — so
whatever is dropped there can still be flipped.

**The centre joins families only.** A listing joins listings, a panel
joins panels or a panel group (ADR 0134). A listing dropped in the middle
of the places sidebar would live in sixteen columns, and a mixed group
would stop being a panel group for good.

**Nothing may disappear by being moved.** Before keeping the new tree,
both frontends resolve it against the current screen and refuse — with
a status message, as splitting does — if anything visible before would no
longer be placed (the centre's target excepted: it goes behind a tab on
purpose), or if fewer than two listings would be visible where there were
two: the other panel is where copies go. One rule,
`layout::keeps_on_screen`, for the terminal (against its last frame) and
the window (against its viewport). Stacking two listings in a short
terminal is refused rather than silently hiding one.

**`layout.flip` turns the panel's run.** It flips the innermost split
that holds the focused panel, side by side ↔ one above the other — but
only the run of weighted siblings around it: in `H[places Fixed(16), a,
b]`, `a` and `b` stack and places stays a sixteen-column sidebar;
flipping the whole row would turn it into a band and lose its width for
good. If the run is the whole split, the split flips; if it is part, it
moves into a split of its own carrying its total weight. A split holding
chrome, or a run of one, is not flipped, and that refusal stops there: it
does not go on to flip an outer split. Dropping a panel below another and
flipping gives the same tree as dropping it to the right.

It has no key in any preset, on purpose: each preset says so in its
header. It lives on a fifth layout button (`[/]` in the terminal, an icon
in the window), in the Panels menu and in the palette. In a narrow
terminal the flip button is the first to give way (`layoutbar::fitting`),
so the four older buttons never disappear because of it.

## Consequences

- Any arrangement the split commands could build can now be reached with
  the mouse, and undone with it.
- A move is a change to the live layout, like a border drag: it is saved
  with the session, and it does not touch the presets.
- Dragging a whole tab group at once is not possible; its tabs move one
  by one.
- Two known differences between the frontends. The window can drag any
  tab of a group (each tab is a handle); the terminal drags the panel by
  its title row, so only the one in front. And in the terminal the title
  row of the lower of two stacked panels is also the border between them:
  pressing there resizes, as it always did, so that panel moves only in
  the window.
- A moved panel arrives with the destination's weight, not its old size:
  after it leaves its split, the size it had there no longer means
  anything.
