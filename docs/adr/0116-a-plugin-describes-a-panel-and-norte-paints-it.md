# 0116 — A plugin describes a panel, and norte paints it

- Status: accepted
- Date: 2026-09-16
- Decision makers: Oscar González
- Related: ADR 0037 (plugin kinds and the closed set of roles), ADR 0057 (the
  location capability), ADR 0058 (a screen is a tree), ADR 0059 (preserve what
  you do not understand), ADR 0077 (parity), ADR 0094 (one served version per
  WIT package), ADR 0105 (a kind is a package, not a bump), spec
  `2026-09-15-historia-y-wow-design.md` (phase 3), plan
  `2026-09-16-fase3-kind-panel.md`

## Context and problem statement

Until now a plugin could decorate a row, add a column, preview a file, propose
a rename or answer a hook. All of those are contributions to a surface norte
owns. Phase 3 asks for the next thing: a plugin that owns a **hueco** — a slot
of the layout, placed like any other panel, with its own content.

Four questions had no obvious answer, and each of them is a place where this
could have gone wrong quietly.

1. **Does the guest draw, or describe?** A guest that draws needs a drawing
   API, and the same API must exist twice — ratatui and the DOM — or the two
   frontends diverge the first time one of them grows a primitive.
2. **What can a clickable zone do?** A panel that can only be read is a poster.
   A panel that can act needs a way to act that does not become "a plugin can
   do anything the reader can do".
3. **What may a panel remember?** A panel that recomputes everything on every
   cursor move is either slow or wrong; one that remembers needs somewhere to
   put it, and that somewhere crosses a trust boundary.
4. **Who resolves a click — the renderer or the host?** The window has a
   renderer that already knows where things are on screen. Letting it resolve
   the click is the short path.

## Decision

**A panel kind is its own WIT package, `norte:panel@0.1.0`.** Same reason as
`norte:thumbnail` and `norte:renamer` (ADR 0094, ADR 0105): the host serves one
version of each package, so growing `norte:plugin` would invalidate every
installed guest, and none of them paints panels.

**The guest DESCRIBES.** It returns lines of styled spans and a list of zones.
The border, the title and the focus ring are norte's, in both frontends. A
plugin therefore cannot paint a panel that looks like another panel, and the
two frontends share the model (`norte_frontend::frame::StyledFrame`) rather
than two drawing APIs.

**A zone names a command of the catalogue, and the command is filtered.** The
plugin chooses the label AND the command, and nothing ties them together: a
zone labelled "Refresh" can name whatever it likes. So a zone may only name
what `norte_frontend::frame::zona_puede` allows — the same scope the panel's
KEYS have, which is chrome: move between panels, open or close one, resize.
The consent the reader gave was for painting; the manifest capability is
`panel`, not "drive the file manager". The list lives beside `Hit`, in the
shared crate, so the window cannot decide differently (ADR 0077).

**What a panel remembers is opaque, per slot AND per kind.** The guest gets
back, untouched, the bytes it returned last time; norte never reads them. They
are capped by the protocol, they never reach the session file, and they are
dropped when the slot's kind changes — because slot ids are reused (a preset
carries small fixed ones), and without that the next plugin in that slot
inherits the previous one's memory. The permission to READ, by contrast, is
minted per call and withdrawn when the call ends: what persists is the guest's
own note, never its access.

**The renderer reports the cell; the host resolves it.** `HitView` crosses the
bridge with `row`, `col` and `width` and no command. A click sends
`panel_click { slot_id, row, col }`, and the host resolves it against the
frame it holds and filters it. A command on the wire would be a command that
anyone talking to the renderer could send, and it would also be a second place
where "what may a zone do" gets decided.

## Consequences

- A plugin panel is placed, focused and offered by the layout picker in both
  frontends, and a contributed kind that the reader has not consented to does
  not exist for the layout at all.
- `Hit.arg` travels and is read by nobody, because no catalogue command in the
  allowed set takes an operand. **It is never a path.** Whoever allows a
  command with an operand decides `arg` at the same time, in both frontends.
- The guest does not yet receive `Click` or `Command` events: a zone runs a
  command of norte's and the plugin is not told. The place to wire it is the
  event argument of the render call.
- The embedded backend discovers the plugin catalogue per call, where the
  daemon holds it in memory. It is paid per context change, not per frame,
  because a signature already asked — or already answered empty — is not asked
  again.
- Every conversion of a guest's span goes through one function that masks the
  text and narrows the role to what a plugin may request. A second, hand-rolled
  conversion is how the first hole appeared, and it is the thing to watch for
  when a third surface starts painting frames.
