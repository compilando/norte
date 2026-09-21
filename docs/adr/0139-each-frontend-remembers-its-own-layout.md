# 0139 — Each frontend remembers its own layout

- Status: accepted
- Date: 2026-09-21
- Decision makers: Oscar González
- Protocol: unchanged. Bridge: unchanged. Session schema: unchanged — a new
  key in `layouts`, which is a map of names already.
- Supersedes: the layout half of ADR 0058 D8 ("close one frontend, open the
  other, carry on where you were").
- Related: ADR 0123 (handoff between frontends), ADR 0138 (moving panels)

## Context and problem statement

Since panels can be moved and resized with the mouse (ADR 0138), the sizes
and places a reader sets are worth keeping: they should survive closing
and reopening, until a template is chosen from the layout picker.

The window already wrote its tree to the session on every change and read
it back on start. But it wrote it under the same key as the terminal
(`default`, or the profile's name), as D8 of ADR 0058 wanted. The window
and the terminal are different screens — the window has a detail panel
and a log where the terminal has a tree — and whichever wrote last
overwrote what the other had arranged. From the reader's side: "the sizes
are not remembered".

## Decision

**Each frontend remembers its own.** The window stores its tree under
`<profile>@window` (`session::window_layout_key`); the terminal keeps the
profile's name. Writing one never touches the other.

**First start inherits.** A window with no key of its own yet — the first
start after this change, or a profile only the terminal has used — opens
with the shared one instead of the factory preset.

**A handoff still hands over the screen.** An explicit handoff (ADR 0123)
exists to continue in the other frontend with THIS screen, so it writes
the tree under both keys, in both directions.

**A profile's two layouts are one profile's state.** Pruning the session
(`PROFILE_STATE_CAP`) counts `X` and `X@window` as one profile, and drops
or keeps them together.

**Reset is choosing a template.** The layout picker replaces the tree with
the template's, and that is what is remembered from then on.

## Consequences

- Sizes and positions set in the window come back when it reopens, whatever
  the terminal did in between, and the other way round.
- Closing the window and opening the terminal (without a handoff) no longer
  carries the panel arrangement across; the directories, cursors and
  histories still do, because slots are shared.
