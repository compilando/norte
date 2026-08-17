# A screen made of slots: layout, tabs and the UI session

> A new line of work, prompted by comparing norte's fixed two-pane screen with
> superfile's composed one. The ambition is a configurable window system shared
> by every frontend. This document specifies **L1**, the client-side layout
> engine, and marks the boundaries of L2 (the UI session on the wire), L3 (new
> panel kinds) and L4 (menus) so that each can be specified on its own later.

## The differential that started this

superfile shows four browser panes, a places sidebar, a docked preview, and
three auxiliary panels along the bottom — processes, metadata, clipboard. norte
shows two panes, a task strip and a status bar.

The interesting gap is not the count. It is that **norte's layout is written by
hand and superfile's is composed**. `draw` in `crates/norte-tui/src/ui.rs`
splits the frame into body, task strip and status bar; `pane_geometry` and
`pane_list_rows` then *replicate* that arithmetic so the mouse and the
pagination probe do not have to guess, anchored by a render test against the
real buffer. That is correct code, and it is correct precisely because the
layout is fixed. It has no room for a fifth thing.

So the work is not "add panels". It is to replace a hand-written layout with a
**tree of slots** that both frontends resolve from the same pure code, and then
let panels be things that fill slots.

## Decisions taken before this document

Recorded here because they bound everything below, and their rationale is in
ADR 0058.

1. **The UI session lives in the daemon** and carries layout *and* panel state:
   close the TUI, open the GUI, carry on where you were. One owner at a time;
   no live mirroring. The `session.*` method family is already reserved for
   exactly this (`crates/norte-proto/src/methods.rs:1183`, specification §11)
   and nothing implements it. This is L2.
2. **Operations keep addressing paths, never slots.** `Side::Left/Right` is
   untouched: it labels which of two compared trees an entry came from, which
   is inherent to comparing two trees and has nothing to do with panes. No
   `SlotRef` on the wire, ever.
3. **The stored tree is typed at the geometry and opaque at the content.** The
   core validates splits, tabs and weights; it never learns which kinds exist.
   That is what lets a plugin contribute a kind.
4. **A stored layout is never rewritten because of screen size.** Degradation
   happens at render time and is discarded with the frame.

## The model

Four concepts that do not overlap:

```
Session (daemon, persisted, survives handover)          -- L2
 └─ Layout: a tree
     ├─ Split { dir, children, weights }    geometry -- typed
     ├─ Tabs  { children, active }          geometry -- typed
     └─ Slot  { id, kind, params, bindings } content -- OPAQUE

Kinds     -- WHAT is inside.  browser, viewer, tasks, compare, sync, ...
Roles     -- WHO is who.      named pointers, resolved every frame: active, target
Bindings  -- WHOSE view am I. per slot: follows: <role | slot_id>
```

**Tabs are not a new concept.** They are a node type, and where you place them
in the tree decides what they mean:

| `Tabs` at | behaves as |
| --- | --- |
| the root | workspaces — each tab is a whole layout |
| wrapping one slot | panel tabs — four directories, one visible |
| halfway down | the right half alternates viewer/compare, the left never notices |

The user picks the depth; there is one mechanism. A tab's title comes from its
kind (`title(state) -> String`): a `browser` gives the directory name, a
`viewer` the file name.

**Bindings are what make auxiliary panels real.** A metadata panel shows *whose*
cursor; a docked preview previews *whose* selection. Its default is
`follows: Role(active)`. Without bindings, a viewer in a slot is a viewer of
nothing, and a configurable layout is a pretty grid of dead boxes.

### Four rules the model does not survive without

1. **A hidden slot is suspended.** No watch, no pagination probe, no plugin
   column computation. This is a performance rule, not an aesthetic one:
   inotify watch counts are limited (a documented pitfall of this project), and
   the git-status plugin costs ~167 ms per 20-entry page (#224). Paying that
   twenty times over for what nobody is looking at is absurd. A slot revalidates
   and repaints when revealed.
2. **An operation role only ever points at something visible.** With N panes, a
   copy toward a destination the user did not have in mind is silent data loss;
   with tabs, the destination can be *behind another tab*. When the slot holding
   `target` is hidden, the role relocates to a visible candidate; if there is
   none, the operation **asks**. The target is always drawn in the chrome.
3. **An unknown kind degrades to a named box and is preserved.** The frontend
   that cannot render a kind must not drop the node when it writes the layout
   back. Otherwise opening the TUI deletes the GUI's terminal panel.
4. **Focus traversal skips hidden subtrees.** Otherwise you tab into panels
   that are not on screen.

## L1: the layout engine and slot state

Entirely client-side. No protocol, no persistence beyond config files.

### Where the code lives

A new module in `norte-frontend`, beside the pure models the frontends already
share (`pane`, `viewport`, `columns`, `mouse`):

```
norte-frontend/src/layout/
  tree.rs     Node, SlotId, serialisation
  kinds.rs    KindId, KindDecl, the registry
  resolve.rs  tree + Rect -> placements, collapse
  focus.rs    traversal order
  roles.rs    roles, bindings, and their reconciliation
  store.rs    SlotStore: state indexed by SlotId
```

No ratatui, no GPUI. Each frontend supplies its own `KindId -> renderer` table
and its own loop.

### Types

```rust
pub enum Node {
    Split { dir: Dir, children: Vec<Node>, weights: Vec<u16> },
    Tabs  { children: Vec<Node>, active: usize },
    Slot  { id: SlotId, kind: KindId, params: Params, bindings: Bindings },
}

pub struct KindDecl {
    pub id: KindId,
    pub min: (u16, u16),           // cells; the GUI scales by its font metric
    pub focusable: bool,
    pub takes_keys: bool,
    pub multi: bool,               // may several instances coexist?
    pub roles: &'static [RoleId],  // which roles it may hold
}

pub struct Roles(BTreeMap<RoleId, SlotId>);   // active, target
pub enum Follow { Role(RoleId), Slot(SlotId) }
pub struct Bindings { follows: Option<Follow> }
```

`KindId` is a **string, not an enum**. An enum closes the registry, and with it
the door to a plugin contributing a kind — which would contradict decision 3
downstream.

```rust
pub struct SlotStore<P> { slots: BTreeMap<SlotId, P> }
```

**The store is generic over the panel state, and each frontend supplies its
own.** `norte-frontend` owns the pure halves (`pane`, `viewer`, `compare`,
`sync`), but the TUI wraps several of them in view types of its own
(`crate::app::CompareView`, `crate::viewer::Viewer`) and the GUI does the same
differently. A concrete `PanelState` enum in the shared crate would drag one
frontend's view types into the other's dependency graph. So the shared crate
stores whatever it is given, and the TUI writes:

```rust
pub enum TuiPanel {
    Browser(crate::app::Pane),            // today's wrapper, UNCHANGED
    Viewer(crate::viewer::Viewer),
    Tasks(crate::tasks::TaskBoard),
    Compare(crate::app::CompareView),
    Sync(crate::app::SyncView),
    Unknown { kind: KindId, raw: Params },
}
```

Role eligibility does not need a trait on `P`: it is decided by the slot's
`kind` in the tree plus the registry, never by the state itself.

**`PaneState` does not change.** Its sixty fields, `listing_epoch`,
`sweep_baseline`, the pruned marks — all intact. The refactor is about *where
state is kept*, not what it holds. That is what makes the 187 call sites a
mechanical access change rather than a redesign.

### Slot identity and role defaults

Two things the types above leave open, pinned here because they decide how L2
serialises and whether state survives an edit.

- **`SlotId` is minted per layout and never reused within a session.** Closing a
  slot does not free its id: its state stays in the store as orphan state (kept
  with a cap and an age sweep, see L2), so reopening the same panel arrangement
  recovers the history rather than starting blank. The two slots that replace
  `App.panes[0]` and `App.panes[1]` in step 3 get fixed, well-known ids so that
  the `orthodox` preset and today's state are the same thing.
- **`Params` is an opaque, serialisable bag interpreted only by the kind.** For
  `browser` it holds the starting directory; for a future `preview` it might hold
  a wrap mode. The engine never reads it, which is the client-side half of the
  same decision that keeps the core from reading it.
- **Role defaults.** `active` is the focused slot, always. `target` defaults to
  *the other role-eligible visible slot* when there is exactly one candidate —
  which is what makes a two-`browser` layout behave exactly as today, with the
  concept present but invisible. With zero or more than one candidate there is
  no default: `target` stays unset until `layout.set-target` names one, and an
  operation that needs it asks (see errors, below).

### Resolution

```rust
pub fn resolve(area: Rect, tree: &Node, decls: &KindRegistry) -> Resolved

pub struct Resolved {
    pub placements: Vec<(SlotId, Rect)>,  // visible, and where
    pub hidden: Vec<SlotId>,              // inactive tab, or collapsed away
    pub focus_order: Vec<SlotId>,         // hidden subtrees already removed
}
```

Three properties follow from this being one function with one return value:

- **`hidden` is the suspension signal.** The run loop hands it back to the
  model, and there the watches, pagination probes and plugin columns of what
  nobody is looking at are dropped. No separate channel to invent.
- **`placements` replaces `pane_geometry` and `pane_list_rows`.** The split is
  computed once and read by the painter, the mouse and the pagination window
  instead of being replicated three times. This is *less* code than today, and
  it removes the class of bug where one copy drifts from another.
- **Collapse lives inside `resolve` and is never stored.** A `Split` whose
  children cannot reach their declared `min` degrades to `Tabs` for that frame;
  if it still does not fit, the collapse propagates upward. Widen the window and
  it opens back up. The input tree is never touched.

### Data flow per frame

```
resolve(area, tree, registry) -> Resolved
   |- hidden      -> suspend: drop watches, probes, plugin columns
   |- placements  -> per visible slot: reconcile window, then renderer[kind](state, rect)
   `- placements  -> back to the model, so the mouse resolves against what was painted
```

Same order as today (`before_frame` reconciles, `draw` paints,
`mouse::after_frame` returns geometry); the difference is that all three read
one `Resolved` instead of recomputing it.

### Keys

The keymap already resolves *chord -> command id* as a string (`pane.switch`,
`viewer.up`, `pane.compare-dirs`), globally, before anyone consults focus. Only
dispatch changes:

| namespace | goes to |
| --- | --- |
| `tab.*`, `layout.*` | the layout engine |
| `pane.*` | the **focused** slot, if its kind is `browser` |
| `viewer.*` | the focused slot, if its kind is `viewer` |
| everything else | global, as today |

"The focused slot" replaces "the active pane", and the help context
(`crates/norte-tui/src/help_context.rs`) derives from the focused slot's kind
rather than a global mode. **No existing key changes meaning.**

New command ids, additive:

```
tab.new  tab.close  tab.next  tab.prev  tab.move-left  tab.move-right  tab.goto-<n>
layout.split-h  layout.split-v  layout.close-slot  layout.focus-next
layout.focus-prev  layout.grow  layout.shrink  layout.equalize  layout.set-target
```

Fifteen commands times seven presets, plus the reference sheet, which-key and
the help, is where the time goes — and each preset imitates a different manager
with its own conventions, so it cannot be done with a `sed`. Therefore:
**bind only the core by default** (`tab.new`, `tab.close`, `tab.next`,
`tab.prev`, `layout.focus-next`, `layout.set-target`); the rest ship **unbound**,
reachable from the palette and shown greyed in the sheet. That is the pattern
the repository already uses for planned capabilities (#132–#140).

### Configuration

A layout tree nested in TOML is unreadable and nobody will write one by hand; it
is edited in the UI. So `norte.toml` carries only the choice:

```toml
[ui.layout]
preset = "orthodox"
```

Named layouts live in their own files, one per layout, under
`~/.config/norte/layouts/<name>.toml`, **in the same format L2 will serialise
into the session**. One format for the config file, the session blob and the
layout editor's output. Inventing two is how you end up migrating between them.

Compiled-in preset: **`orthodox`** — two `browser` in a horizontal `Split`,
`tasks` below. It is the default and it is exactly today's screen.

**Migration: none.** A user without `[ui.layout]` gets `orthodox`, which is what
they already saw. Their keys keep working because `pane.*` did not change. A new
user never learns slots exist until they open a tab.

### Errors and degradation

The pattern already exists: the keymap separates a hard error
(`KeymapError`) from a diagnostic (`KeymapDiagnostic`), and `norte doctor`
reports the latter (specification §13). Same pair: `LayoutError` /
`LayoutDiagnostic`, `thiserror` because `norte-frontend` is a library.

| case | behaviour |
| --- | --- |
| unknown kind | `PanelState::Unknown{kind, raw}`; named box; not focusable, not role-eligible; **`params` preserved on re-serialisation** |
| invalid but clampable (`active` out of range, zero weight) | clamp, diagnostic, carry on |
| invalid and incoherent (duplicate `SlotId`, `follows` to a slot that does not exist) | **no guessing**. On hot reload the last valid layout is retained (the project's config policy); on cold start it falls back to `orthodox` with a message |
| broken `follows` | degrades to `follows: Role(active)` with a diagnostic — what was meant 95% of the time |
| nothing fits (20x5 terminal, collapse exhausted) | **the focused slot is always painted**, even below its `min`, and the chrome says the window is too small. Never a blank screen |
| role with no candidate | the operation **asks**. `F5` with no visible target opens the path dialog instead of failing |

That last row has a consequence worth stating: **with a single-`browser` layout,
`F5` has no "other pane"**. Today that is impossible; tomorrow it is a legitimate
layout. Making the path "ask for the destination" rather than "error" is what
makes a one-panel layout usable instead of half broken.

### Testing

`resolve` is pure, so it is table-tested: tree plus `Rect` gives expected
placements, with cases for collapse, weights, minimums and hidden tabs. Four
properties on top: placements **do not overlap** and do not exceed the area;
every visible slot meets its `min` **or** we are in the nothing-fits case;
`focus_order` is an exact permutation of the visible focusable slots; and the
tree round-trips through serialisation.

**The place this refactor can be lost silently is the TUI's geometry tests.**
They anchor `pane_geometry` and `pane_list_rows` against the buffer that was
actually painted. They are not deleted — they are re-aimed at anchoring
`Resolved` against the buffer. The test that says *"what the engine believes it
painted is what is on screen"* still exists; there is now one of it instead of
three. **It is written first, before anything moves.**

Acceptance criterion for L1, written down because it is what keeps the refactor
honest:

> A snapshot of the `orthodox` screen taken **before** the refactor is identical
> **after** it. The user notices nothing at all until they open a tab.

Plus: a test that a hidden slot **releases its watch** and fires no probes
(with `MemProvider`), and one that closing a slot with a listing in flight
**cancels its Task** (hard rule 3).

### Order of work

1. The anchoring test and the `orthodox` snapshot. Nothing else touched.
2. `layout` complete in `norte-frontend`, pure, with its tests. Nobody uses it.
3. `SlotStore` with `Browser` only. `App.panes[2]` becomes two slots with fixed
   ids behind accessors that return what they always did. **The 187 sites land
   here**, mechanically, and the screen does not move.
4. The TUI paints from `resolve`, with `orthodox` compiled in. The snapshot must
   still be identical.
5. `Tabs`, the commands, the roles and the remaining kinds arrive.
6. The GUI repeats 4 and 5 with its own renderer table.

Step 3 is the only one worth splitting across agents; the rest is sequential.
The GUI goes **behind**, not in parallel: it is outside the workspace and has
its own gate (`just gui-ci`).

**Main risk, stated plainly:** L1 is a large refactor that shows nothing new
until step 5. The mitigation is that steps 1–4 are verified by the snapshot, so
stopping there breaks nothing and leaves the engine in the tree.

## L2: `session.*` — the UI session in the daemon

Sketched, not specified. Its boundary:

- The daemon stores `layouts: Map<ProfileId, Layout>` — **one entry in v1**
  (`default`) — and panel state **separately, keyed by `slot_id`**, shared
  across profiles. The day a TUI-shaped layout and a GUI-shaped one are wanted,
  it is one more map entry and no change of shape on the wire, while paths and
  cursors stay shared. That sharing is the reason the session is in the daemon
  at all.
- **A layout that does not mention a `slot_id` does not delete its state.**
  Otherwise switching profile throws away your history. Orphan state is kept,
  with a cap and an age-based sweep so it cannot grow without bound.
- **It must survive the daemon handover.** `daemon.going_away` already exists
  (ADR 0055) so frontends do not lose their session across an upgrade. A session
  held only in the daemon's memory would be erased by exactly the event the
  handover was built for, so a versioned on-disk format and its migration are
  part of L2, not an extra.
- Wire change means a protocol version bump, golden fixtures, and a mandatory
  `protocol-guardian` review.
- One owner at a time. A second client either opens a new session or clones.
  Live mirroring is deliberately out; two writers over shared state is conflict
  resolution, and that is not something you bolt onto a window system.

### Sizing: what building L1a found

A `Split` divides its axis **proportionally, and only that**. Two of the things
already on screen cannot be expressed that way:

- the **status bar** is a fixed height (one row);
- the **task strip** is sized by its *contents* — it grows with the number of
  running tasks, capped at six rows.

So the `orthodox` preset covers the **body**, where the panes live, and the
frame's outer chrome stays hand-written in `draw`. That is a real limit, not a
shortcut: the tree cannot describe a whole screen until a node can be sized
`Fixed(n)` or `Auto` (ask the panel) as well as by weight.

It belongs to **L1b**, and to the front of it, because the two consumers that
will settle its shape do not exist yet — the `places` sidebar wants a fixed
width the user can drag, and a docked task panel wants content sizing with a
cap — and because a `Size` enum changes the stored format, which is the one
thing that is expensive to get wrong twice.

The duplication that mattered is gone regardless: the 50/50 cut between panes
was computed in `draw` and again in `pane_geometry`, and is now computed once.

## L3: new kinds

Each is small on top of L1: `places` (the sidebar), `metadata`, docked
`preview`. **`clipboard` does not belong here** — norte has no clipboard model
today, and a first-class one (copy here, paste there, inspect it, edit it) is
core work touching the journal and the policy engine. It gets its own
specification.

## L4: menus

An F9-style menu bar over the command catalogue that already exists — presets,
which-key, palette, help. Independent of everything above and doable at any
time; mostly a new renderer over existing data.

## Out of scope

- Live mirroring between two clients (the C tier of the session question).
- `SlotRef` on the wire.
- A clipboard model.
- Layout profiles with more than one entry (the shape is carved; the feature is
  not built).
- Plugin-contributed kinds. The registry is open by design so this stays
  possible, but nothing in L1 loads one.
