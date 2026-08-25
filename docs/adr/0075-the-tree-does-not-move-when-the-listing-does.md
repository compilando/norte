# 0075 - The tree does not move when the listing does

- Status: accepted
- Date: 2026-08-25
- Decision makers: Oscar González
- Related: ADR 0058 (slots and tabs), ADR 0066 (multi-frontend, D14 no dual
  presentation), ADR 0068 (a row is named by key and generation), ADR 0059
  (preserve what the layout does not understand), issues #290, #136.

## Context and problem statement

`pane.tree` was the last of the "needs new surface" block of #290 that the
window could not do. The TUI has had a directory tree since #136; the window
had nothing, and the parity test carried the row that said so.

Three questions had to be settled, and they are the ones that make a tree panel
either useful or an ornament.

## Decision

### It occupies a slot

Not a fixed sidebar, not a floating window. The tree is a kind like any other:
it opens into a slot, splits, closes and resizes with the gestures that already
exist. A fixed sidebar would be a second layout system living next to the one
with tabs and splits, and ADR 0058 exists precisely so there is only one.

Docked left at width 24 — the same gesture and the same width as the places
sidebar, because two different widths for "a navigation column beside the
listing" are noticeable.

### Choosing a branch navigates the LISTING, and the tree stays put

The focused listing goes to that directory, by the same path as any other
navigation — which is what makes having the tree open not change where
operations go.

And the tree does **not** re-anchor. It anchors when it opens (and again when
it is reopened, because the listing may be somewhere else by then) and never
on navigation. Re-anchoring on every `cd` would throw away every open branch
each time the reader enters a folder, which is most of what a tree is for.

Two gestures per row, not one: the **twisty** folds and unfolds, the **name**
navigates. A single gesture would force a choice between them, and both are
needed — looking inside a branch without moving the listing is half the point.

### Lazy, one branch per turn

Unfolding a branch lists *that* directory and nothing else. A tree that read
itself whole on opening would take minutes on a large `$HOME` and hours against
a remote — the same reasoning that keeps sizes out of the local listing (#52).

One branch per turn, chained: the reply to one request triggers the next. No
clock, and no single directory or slow server can jam the rest.

A branch that cannot be read is marked as read and **empty**. Without that it
would be re-requested on every turn, which is a request loop against a
forbidden directory.

## Consequences

- The model **moved** from `norte-tui` to `norte-frontend` (`tree.rs`) and both
  frontends use it. Two copies of the same presentation state drift, and D14
  says there is one (`norte-tui` re-exports it so twenty call sites did not
  have to be rewritten).
- `SlotView::Tree`, `UiAction::TreeActivateRow`, `UiAction::TreeToggleRow`,
  bridge **40**.
- Both actions carry a **generation**, like the places sidebar and for the same
  reason: a branch's children land *in the middle* of the list, so an index
  without a generation can name the folder next to the one that was clicked
  (ADR 0068). One that does not match is refused, not navigated.
- `children: None` — not yet looked at — is painted folded, never as a leaf.
  Painting "nothing inside" over something nobody has read is an invented
  answer, and the three states stay three states all the way to the DOM.
- A branch is capped at `MAX_RAMAS` (2000) children. Without a cap, one open
  branch turns every host snapshot into a multi-megabyte message.
- The root row carries its **whole path**: "`/`" alone, or the last folder's
  name, does not say where this hangs from.
- The tree asks for **no attributes**. It shows directory names; paying for
  sizes and permissions per branch would be paying them for every folder
  anyone unfolds.
