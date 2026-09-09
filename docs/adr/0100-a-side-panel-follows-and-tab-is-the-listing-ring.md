# 0100 — A side panel FOLLOWS the active listing, and `Tab` is the listing ring

- Status: accepted
- Date: 2026-09-09
- Decision makers: Oscar González
- Related: ADR 0058 (layout by slots, roles and bindings), ADR 0068 (a painted
  index is only valid for the generation it was painted in), ADR 0077 (a
  decision taken once, in `norte-frontend`), ADR 0096 (what is operated on and
  what is pointed at are two questions), ADR 0097 (parity is a test)

## Context

Two reports from the same session of real use, and they turn out to be the same
shape of mistake made in two places.

**The tree does not follow the panel.** Open the tree, navigate a listing, and
the tree keeps pointing at the folder you were in when you opened it. That was
deliberate: `Tree::anchor` EMPTIES the tree when the root changes — branches
from another root say nothing about this one — so re-anchoring on every `cd`
would close the whole tree every time the reader entered a folder. Both
frontends therefore anchored once, at open, and never again. The comment saying
so is in `controller/tabs.rs` and in `app/layout.rs`. The model had no third
option: `anchor` (which empties) or nothing.

**`Tab` stops rotating with three listings.** `layout.split-v` puts a third
listing on screen, `[left, new, right]`. In the terminal, `pane.switch` was
`focus ^= 1` — a count of TWO. From index 2 that yields 3, which does not
exist, and `PaneSlots` CLAMPS out of range rather than panicking, so the key
did nothing and did not say so: the panel you had not split was unreachable.
In the window the opposite was true — `pane.switch` shared an arm with
`layout.focus-next`, so with the places bar, the tree and the viewer open,
getting back to the listing beside you took five keystrokes.

Behind both is one question this project keeps meeting: **when a panel's job is
to describe where the reader is working, what tells it that "where" moved?**

## Options

### A — Re-anchor the tree on every navigation, and leave `Tab` alone

- Advantage: two lines, no new model API.
- Drawback: it is the behaviour the existing comment rejects, for a good
  reason. Every step into a folder would close every open branch, which makes
  a tree panel worse than useless — it becomes a panel that punishes you for
  using the panel beside it.
- Drawback: says nothing about `Tab`, which is a second bug.

### B — Wire the follow into every gesture that navigates

- Advantage: no new funnel.
- Drawback: this is exactly the list that already went stale once. The
  terminal's `settle_cd` exists BECAUSE the ritual after a `cd` was copied into
  the nine event-loop sites that cause one; the host's `aterrizar_listado` is
  the same funnel for the same reason. Adding a tenth item to a list that had
  to be deduplicated is repeating the fixed bug.

### C — Give the model a `follow`, wire it in the two funnels, and split the two focus rings

- The shared model gains `Tree::follow(dir)`: expand the ANCESTORS of `dir`,
  move the cursor there, and leave everything else open. Re-anchor only when
  `dir` does not hang from the root at all.
- It is called from the one funnel each frontend already has —
  `Estado::aterrizar_listado` in the host, beside the capabilities, the probes
  and the decorations; `navigate::settle_cd` in the terminal — plus the focus
  change, which is the other moment "where the panel is looking" changes.
- `pane.switch` cycles the LISTINGS, all of them, and nothing else.
  `layout.focus-*` stays the ring of the whole screen.

## Decision

**C.**

A side panel that describes the active listing follows it by REVEALING, never
by re-anchoring, and it learns that the listing moved from the funnel every
navigation already passes through — not from the gesture that caused it.

Three details are load-bearing:

- **The destination itself is not expanded.** Whoever navigated there is
  already looking at its contents in the listing beside the tree; expanding it
  would cost one more directory listing per step the reader takes.
- **Revealing a deep branch takes several turns of the run loop**, because each
  level has to be listed. So the target is written down (`Tree::revealing`) and
  the cursor lands when the row finally exists. Without that, the cursor
  stopped at the deepest ancestor that happened to be listed when the reveal
  was asked for.
- **Only the ACTIVE listing.** One on the other side finishing its load is not
  where the reader is working, and moving the tree for it would leave the panel
  describing a pane nobody is looking at.

And the two rings get two names, because they answer two questions.
`pane.switch` is "the other panel" of every orthodox manager; `layout.focus-*`
is "walk the screen". One ring with two names got both wrong at once. A side
panel is reached with `layout.focus-next` and with its own key, and `Tab` still
takes you OUT of one — which is what guarantees no combination leaves the
reader stuck inside a panel.

## A third report from the same session, decided the same way

The viewer could not reach the right-hand half of a long line: it does not
wrap, so a minified HTML file was painted clipped and the rest was nowhere.
That is a missing feature rather than a decision, but where the horizontal cut
LIVES is one, and it is settled by the same rule as the tree: **once, in the
shared model** (`Viewer::rows`, on the already-rendered line), never in each
frontend. Two cuts are two ways of counting columns that one day disagree.

It counts CELLS of terminal, not bytes and not characters: by bytes the text
would jump when it reached an accent, and by characters any line with CJK would
end up misaligned against its neighbours. A wide character straddling the cut
goes ENTIRELY — half a cell cannot be painted, and keeping the whole character
would slide that row one column relative to the rows around it.

## Consequences

### Positive

- The tree is useful with the panel beside it instead of in spite of it, and
  what the reader opened by hand stays open.
- The rule lives in `norte_frontend::tree`, once, so the two frontends cannot
  drift on it (ADR 0077). The wiring is one line per funnel.
- `Tab` means the same thing in both frontends and with any number of
  listings, and the parity test says so.
- The funnels get a second load-bearing job, which makes them harder to
  bypass by accident next time.

### Negative

- `Tree` now holds a pending target, so it has a state that is neither "root"
  nor "rows": a reveal in flight. It is settled in `insert_children` and
  nowhere else, and that is the only place rows appear.
- A reveal of a deep, unlisted branch costs one listing per level. It is the
  same lazy walk the tree already did for a hand-opened branch, one per turn,
  so the cost is bounded — but a navigation into a very deep path does ask for
  more directories than it used to.
- Navigating outside the root still empties the tree. That is deliberate — a
  tree showing one place next to a listing showing another answers nothing —
  but it means a reader who alternates between `$HOME` and `/tmp` loses their
  branches each way. Anchoring at a common ancestor was considered and
  rejected: it would silently move the root the reader chose.
- Changing what `pane.switch` means in the window is a behaviour change for
  anyone who had learned to `Tab` into the tree. `layout.focus-next` does it,
  and so does the tree's own key.
