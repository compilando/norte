# 0134 — Panels on one edge share it as tabs

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: **88** — `TabGroupView.panels`.
- Related: ADR 0058 (layout by slots and tabs), ADR 0131–0133 (the same
  VS Code pass), ADR 0077 (parity)

## Context and problem statement

Opening the timeline, the viewer and the details on the right split that
edge in three: each panel got a third of a column that was already narrow,
and the viewer — the one that needs room — ended up a strip. VS Code puts
the views of one side into one container with a tab per view; only one is
shown, at the full size of the side.

The layout model already had `Node::Tabs`. What was missing was docking
into it.

## Decision

**A panel docked on an edge that already holds a panel joins it as a
tab.** `Tree::dock_grouped` looks at the neighbour on that edge — the
first child for a front edge, the last before the trailing status and
tasks rows for a back edge — and, if it is a panel or a group of panels,
adds the new one as a tab and makes it active. Otherwise it docks as
`Tree::dock` always did. `dock` itself keeps its meaning: layouts written
by hand, and the presets, are not regrouped.

Every edge groups, the left one too: places and the tree share the left
column as VS Code's side bar shares its views. The group takes the room of
whichever panel asks for more — a weight beats a fixed size, and of two
fixed sizes the larger wins — so the viewer joining the details does not
inherit their thirty columns.

A panel is any slot that is not a browser, the status bar or the tasks
row. A browser never joins a panel group, and a panel never joins a
browser's tabs.

**Toggling respects the group.** The panel's command (`layout.preview`,
`layout.metadata`, …) on a panel that sits in a group but is not the
active tab brings it forward instead of closing it: the reader asked to
see it. On the active one it closes as before, and a group left with one
tab dissolves back into a plain slot. "In the background" is read from the
tree, not from what was placed: a group that did not fit this frame is not
placed at all, and deciding from placements made the front tab "reveal"
itself forever instead of closing. In the window, bringing a tab forward
also gives it the keyboard (it is a tab choice, as for a browser's tabs);
the terminal leaves the keyboard where it was, as its panel toggles always
did.

The terminal measures a grouped panel's content one row below the strip in
ONE place (`contenido_de_hueco`), for painting and for the mouse alike; with
two measures, a click on a grouped disk map picked the neighbouring child.

**Both frontends paint the strip.** The bridge marks such a group with
`panels: true`; the window paints its tabs in the UI font with the active
one underlined, drops the `+` and `×` (a panel is not a document, and its
own command closes it), and hides the panel's title, which the tab already
says. The terminal paints a one-row strip on top of the group with the
same labels (`panelbar::label_in`, one wording per panel), active
underlined, the rest dimmed; a click on an inactive tab runs that panel's
command, which brings it forward.

## Consequences

- The viewer opened beside the timeline gets the whole right edge.
- Two panels on one edge are no longer visible at once. Someone who wants
  them side by side can still build that layout by hand with `dock` or a
  preset; grouping only applies to the panel toggles.
- No new command, so the seven keymap presets are untouched.
