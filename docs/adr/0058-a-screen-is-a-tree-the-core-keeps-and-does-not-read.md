# 0058 - A screen is a tree the core keeps and does not read

- Status: accepted
- Date: 2026-08-17
- Decision makers: Oscar González
- Related: the design
  (`docs/superpowers/specs/2026-08-17-layout-slots-tabs-design.md`),
  specification §11 (protocol, where the `session.*` family is reserved) and
  §13 (configuration), ADR 0048 (`fs.compare` and `Side`), ADR 0049
  (synchronisation), ADR 0055 (daemon handover), ADR 0022/0037 (plugin
  capabilities and `columns`), hard rules 3 and 7.

## Context and problem statement

norte's screen is written by hand. `draw` (`crates/norte-tui/src/ui.rs`) splits
the frame into body, task strip and status bar; `pane_geometry` and
`pane_list_rows` replicate that arithmetic so the mouse and the pagination probe
do not guess, anchored by a render test against the real buffer. `App` holds
`panes: [Pane; 2]`, plus `viewer`, `compare` and `sync` as named fields. The GUI
mirrors the same shape (`panes: [PaneState; 2]`).

This is correct code, and it is correct *because* the layout is fixed. It has no
room for a fifth thing — a places sidebar, a metadata panel, a docked preview,
a second pair of browsers, tabs.

Wanting those raises four questions that must be answered together, because
answering them separately produces a system that cannot be extended without
touching the wire:

1. What is the unit of layout, and does it nest?
2. With N panes, what does "the other pane" mean for `F5`, compare and sync —
   all of which are defined over exactly two sides?
3. If a session is shared between the TUI and the GUI, who stores the layout?
4. Can something outside the core (a plugin) contribute a panel?

## Decision

**The screen is a tree of three node types. The core stores it, validates its
geometry, and never reads its contents.**

```
Node =
  | Split { dir, children, weights }      geometry -- typed
  | Tabs  { children, active }            geometry -- typed
  | Slot  { id, kind, params, bindings }  content  -- OPAQUE
```

### D1 — Tabs are a node type, not a feature

Where `Tabs` sits in the tree is what it means: at the root it is workspaces,
wrapping a slot it is panel tabs, halfway down it is one half of the screen
alternating between views. One mechanism, three products, and the user chooses
the depth.

### D2 — Kinds are open strings; roles and bindings are separate concepts

- **Kind** — what is inside a slot (`browser`, `viewer`, `tasks`, `compare`,
  `sync`, later `places`, `metadata`, `preview`). A `KindId` is a **string, not
  an enum**: an enum closes the registry, and closing it contradicts D4.
- **Role** — a named pointer into the tree, resolved every frame: `active`,
  `target`. A role points at a slot and demands a kind.
- **Binding** — per slot, `follows: <role | slot_id>`. It is how a metadata
  panel knows whose cursor to show. Without it, an auxiliary panel is a box with
  nothing in it, and "configurable layout" means a grid of dead boxes.

### D3 — Operations address paths, never slots

`Side::Left/Right` is untouched. It labels which of two compared trees an entry
came from, which is inherent to comparing two trees and unrelated to panes. A
client resolves `active` to `Left` and `target` to `Right` before it calls, and
the protocol learns nothing about geometry.

The rejected alternative is D3's whole point and is recorded below.

### D4 — The stored tree is typed at the geometry and opaque at the content

The core validates splits, tabs, weights and structure — everything that can
break the geometry — and never learns which kinds exist. A new kind is never a
wire change, which is what leaves the door open for a plugin to contribute one.

The cost is accepted deliberately: **a frontend that does not know a kind must
preserve it.** It renders a named box, and when it writes the layout back the
node and its `params` survive intact. A client that cannot render a kind must
not be able to delete it from the other client's layout.

### D5 — A stored layout is never rewritten because of screen size

Degradation happens during `resolve` and is discarded with the frame: a `Split`
whose children cannot reach their declared minimum collapses to `Tabs` for that
frame, propagating upward if it still does not fit, and reopening when the
window grows.

The stored tree is the user's intent and is immutable with respect to screen
size. With one session shared between an 80x24 terminal and a large GUI window,
persistent reflow means opening the TUI for a minute silently destroys the GUI's
layout.

### D6 — A hidden slot is suspended

No watch, no pagination probe, no plugin column computation. This is a
correctness-and-cost rule, not an aesthetic one: inotify watch counts are
limited, and a columns plugin can cost ~167 ms per 20-entry page (#224). Twenty
tabs each watching a directory is exhaustion by design. `resolve` returns
`hidden` alongside `placements` precisely so this signal exists without a
separate channel.

### D7 — An operation role only ever points at something visible

With N panes, a copy toward a destination the user did not have in mind is
silent data loss; with tabs, that destination can be behind another tab. When
the slot holding `target` becomes hidden, the role relocates to a visible
candidate; when there is none, the operation **asks** for a path rather than
guessing. The target is always drawn in the chrome.

This makes a single-`browser` layout legitimate rather than half broken: `F5`
with no other pane opens the destination dialog.

### D8 — The UI session lives in the daemon and carries layout and panel state

Close the TUI, open the GUI, carry on where you were. One owner at a time; no
live mirroring. `session.*` is already reserved for this
(`crates/norte-proto/src/methods.rs:1183`, specification §11) and nothing
implements it, so this is a greenfield family in a hole the original design left
open.

Two consequences are part of the work, not extras:

- **It must survive the handover.** ADR 0055 exists so frontends do not lose
  their session when a daemon is replaced. A session held only in memory would
  be erased by exactly that event, so a versioned on-disk format and its
  migration belong to it.
- **Layout and state are stored separately.** `layouts: Map<ProfileId, Layout>`
  — one entry in v1 — and panel state keyed by `slot_id`, shared across
  profiles. A layout that does not mention a `slot_id` does not delete its
  state; orphan state is kept with a cap and an age sweep.

### D9 — A command that names a SIDE resolves it by geometry

Some commands in the shared catalogue name a side of the screen rather than a
role: `pane.select-drive-left`/`-right` are Total Commander's `Alt+F1`/`Alt+F2`,
and in a two-pane frontend they mean `panes[0]`/`panes[1]` — deliberately NOT
the focus, so that a reader can mount a volume in the pane they are not
standing in.

A tree of slots has no `panes[0]`. It does have a resolved layout, so "left" is
answered the only way that cannot lie: the **leftmost visible slot** of the
resolved placement (`x`, then `y`, then id), among the slots that are listings.
Hidden slots are not on any side of the screen, and a side with no listing is
said out loud rather than falling back to the focused pane — mounting a volume
in the wrong pane is exactly what the sided variant exists to prevent.

The side is resolved when the picker OPENS, and the picker carries the slot it
will navigate. Reading the focus at the moment of choosing would mean that
moving the focus while the list is up changes which pane ends up somewhere
else.

Two neighbouring rules follow the same principle — the window answers a
command with the surface it already has, rather than growing a second one:

- **`pane.sort-*` is the header click.** Both doors end at
  `SortSpec::after_click`, so the active column inverts and a new one starts
  ascending, whoever asked. `pane.sort-menu` is the columns dialog, where the
  column, the direction and `dirs_first` already live.
- **`pane.properties` is the `metadata` slot**, which already paints name,
  kind, size and date of the highlighted entry. A properties dialog with
  permissions and owner is a separate surface, and it is deferred as such.

And `pane.swap` exchanges the CONTENT of two slots, never the slot itself.
What travels is the listing, the cursor, the marks, the trail and the sort; the
**paint window does not** (`first_visible`/`visible_count`). That pair is
geometry of the slot: the renderer sets it per slot and owns the `scrollTop`
behind it, which a swap neither moves nor causes to be recomputed. Let it
travel and both panes paint rows outside the band the reader is looking at —
both appear EMPTY, with no scroll event to correct it.

The same gesture has to re-issue what was in flight, and "in flight" is two
things, not one. A listing response is labelled with its slot, so after the
exchange it lands on the wrong slot and is discarded by token: the pane would
load forever. But the first page landing clears only the first of the two
flags; the drain that carries the rest of the stream is still alive, and in any
directory over a page long that is the state a swap will actually find. Both
are re-issued, and they are re-issued DIFFERENTLY: a navigation keeps its
destination, whereas a drain is a refresh of what the reader already sees, so
its cursor and marks are restored. Which in turn requires the drain to say when
it ENDS — a flag raised at request time and lowered by nobody does not mean
"still arriving", it means "this was asked for once", and anything consulting
it decides wrong.

Two more rules of the same family, about not shrinking things silently:

- **A redundant `pane.mirror`/`pane.pull` is refused.** If both slots already
  show the directory, a `cd` re-lists the receiving pane: `set_listing` clears
  its marks —a navigation, unlike a refresh, does not restore them— and slides
  the listing under its cursor, for nothing.
- **`pane.toggle-hidden` says how many marks it pruned.** Stashing the hidden
  entries drops the marks on them, and the shared contract is that a selection
  feeding a bulk op never shrinks in silence.

D8's storage has one rule that reading this ADR should not let anyone forget:
**what the session writes, the session reads.** Panel state carries the sort
spec and the hidden-entries toggle; a frontend that writes them and restores
only the path remembers where you were and forgets how you were looking at it,
which is worse than not storing them at all.

## Consequences

**Positive**

- `placements` replaces `pane_geometry` and `pane_list_rows`: the split is
  computed once and read by the painter, the mouse and the pagination window.
  Less code than today, and the class of bug where one copy drifts from another
  disappears.
- Today's screen becomes a compiled-in preset (`orthodox`). That it is expressible
  as a special case is the evidence the model is placed correctly.
- No migration for existing users, and no protocol change at all in L1.
- The orthodox doctrine survives: the default is still two panes side by side.
  The machinery permits more; the default does not impose it.

**Negative, accepted**

- `App.panes[2]`, `App.viewer`, `App.compare` and `App.sync` stop being named
  fields and become state indexed by `slot_id`. That is ~187 call sites. It is
  mechanical — `PaneState` itself does not change — but it is wide.
- The number of live panel instances loses its known bound. Every extra
  `browser` carries its own history, marks and window.
- Fifteen new command ids meet seven keymap presets, the reference sheet,
  which-key and the help. Only a core set is bound by default; the rest ship
  unbound and greyed, as with #132–#140.
- L1 shows nothing new to the user until its fifth step. The `orthodox` snapshot
  test is what makes stopping earlier safe.

## Alternatives considered

**`SlotRef` on the wire — operations address slots.** Rejected. It puts UI
geometry into a protocol that operates on paths, and N-1 compatibility means
carrying both `Side` and `SlotRef` forever while compare and sync duplicate
their surface. The expressiveness bought is unused: nobody compares four trees.

**A closed `enum` of kinds.** Rejected. It is more type-safe and it makes every
new kind a wire change with a version bump and golden fixtures — which in
practice forbids a plugin from ever contributing one, contradicting the M4 exit
criterion that a third party can ship without changing the core.

**Persistent reflow — the client rewrites the layout to fit and stores it.**
Rejected; see D5. Simple to implement and destructive across a shared session.

**Two file panes forever, extra slots auxiliary only.** Rejected as the product
answer, though it is the cheapest and keeps the orthodox doctrine wholly intact.
It reduces "more windows" to "support panels", which is not what was asked for.

**The layout tree written by hand in `norte.toml`.** Rejected. Deeply nested
TOML is unreadable and nobody would write it. Layouts live in their own files in
the same format the session serialises, so there is one format across the config
file, the session blob and the layout editor.

**Live mirroring between clients.** Deferred, not rejected. Storing state in the
daemon leaves it reachable — what is missing is broadcast and an arbiter — but
two writers over shared state is conflict resolution, and that is designed on
its own or not at all.
