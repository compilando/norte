# 0170 — Which panels are open is shared between the frontends

- Status: accepted
- Date: 2026-10-06
- Decision makers: Oscar González
- Protocol: unchanged (the session body is opaque to the core). Session:
  `SessionBody.open_panels`, additive, no schema bump.
- Related: ADR 0139 (each frontend remembers its own layout), ADR 0123
  (the handoff), ADR 0059 (the session is one document)

## Context and problem statement

ADR 0139 split the layout: the window and the terminal each keep their own
tree, so one's sizes and places no longer overwrite the other's. It also
split something the reader expects to share: opening the disk map in the
window and starting the terminal showed no map there.

## Decision

**Share the set, keep the arrangement.** The session gains
`open_panels`, by profile key: the panel bar's kinds placed in a
frontend's tree (`session::open_panels_of`, in the registry's order).
Each frontend writes it from its own tree when it saves the session. On
start, after restoring its own layout, each one opens what the set has
and its tree lacks — in its own default place — and closes what its tree
has and the set lacks (`session::panels_to_sync`). Sizes, positions and
tabs stay each frontend's.

**Not live.** Like the rest of the session, it is applied on start: with
both open, the second is detached (ADR 0059) and does not write.

**A session from before** has no field and changes nothing.

## Consequences

- The window opens on the tree before anything is published
  (`tree_opening`, the half of `open_slot_of_kind` that only builds the
  tree), the same path a restored layout takes; the terminal opens through
  each kind's own path and gives the keyboard back to the listings.
- Plugin panels are not in the set, as they are not in the panel bar.
