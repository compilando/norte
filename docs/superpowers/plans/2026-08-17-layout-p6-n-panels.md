# P6 — more than two panels

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.

**Goal:** retire the two-side ceiling so a layout can hold N panels, and land the
three things that were waiting on it: `layout.split-h`/`-v`, the target marker in
the chrome, and `[ui.layout]` with its layout files.

**Design:** `docs/superpowers/specs/2026-08-17-layout-slots-tabs-design.md`
**Decision:** ADR 0058. **Predecessors:** the L1a and L1b plans — read their
*What actually happened* sections first.

## What is actually in the way

Not the ~212 call sites. `PaneSlots` already absorbed those: `app.panes[i]`
means "the panel at visible position `i`", and that generalizes to N for free.

What blocks N is **state keyed by POSITION**:

| where | what |
| --- | --- |
| `panel.rs` | `visible: [SlotId; 2]` |
| `app.rs` | `history: [History; 2]`, `focus: usize` |
| `mouse.rs` | `epochs: [u64; 2]`, `geometry: Option<[PaneGeometry; 2]>` |
| `ui.rs` | `[Option<Rect>; 2]`, `[PaneGeometry; 2]` |
| `main.rs` | ~50 sites: `fill`, `decorate_fetch`, `refreshed`, `watch_targets`, the stat-probe dedup, the live search |

And position-keyed state is not only a ceiling, **it is already a latent bug**:
a fill in flight for position 1 applies to whatever is at position 1 when it
lands. Today only `swap_panes` can move a pane between positions, which is why
`swap_seq` and `main::reconcile_swap` exist. Closing a panel would do the same
thing with no such guard. **Keying by `SlotId` fixes the ceiling and that bug in
one move**, and it is the reason this refactor is worth its size.

## Order

| stage | what | why here |
| --- | --- | --- |
| **A** | `BySlot<T>` in `norte-frontend`; `PaneSlots.visible` becomes a `Vec<SlotId>` | the container everything else needs |
| **B** | `App.history` and the mouse move to `BySlot`; `focus` stays a POSITION but is reconciled after every layout change | the model |
| **C** | the run loop's per-pane state moves to `BySlot` | the 50 sites, mechanical, one commit |
| **D** | `layout.split-h` / `layout.split-v`, and `Node::split_slot` | the payoff |
| **E** | the target marker in the chrome | only now does it say anything |
| **F** | `[ui.layout]`, `~/.config/norte/layouts/*.toml`, `norte doctor` | only now is there a second layout to name |

## The anchor, again

`layout_anchor.rs` and the `orthodox` snapshot carry over unchanged, and the
rule is the same: **through stage C the screen does not move.** Stages D–F are
where the user sees anything new.

Add one anchor before stage C: a test that a fill landing for a slot that has
since been closed is DROPPED rather than applied to whoever now sits at that
position. Write it against the current code, where it should fail, and let stage
C make it pass — it is the bug this refactor pays for.

## Stage A — `BySlot<T>`

```rust
/// Estado por hueco, con la ergonomía del array que sustituye.
pub struct BySlot<T> { /* BTreeMap<SlotId, T> */ }

impl<T: Default> BySlot<T> {
    #[must_use] pub fn new() -> Self;
    /// El valor del hueco, creándolo por defecto si no estaba.
    pub fn entry(&mut self, id: SlotId) -> &mut T;
}
impl<T> BySlot<T> {
    #[must_use] pub fn get(&self, id: SlotId) -> Option<&T>;
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut T>;
    pub fn insert(&mut self, id: SlotId, v: T) -> Option<T>;
    pub fn remove(&mut self, id: SlotId) -> Option<T>;
    /// Tira lo que no esté en el árbol. Se llama tras cada cambio de layout.
    pub fn retain_tree(&mut self, tree: &Node);
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &T)>;
}
```

`PaneSlots.visible: Vec<SlotId>`, `len()` returns its length, `Index<usize>` is
by visible position as it already is, and `slot_of(side)` clamps instead of
branching on `side == 0`.

- [ ] **A.1** `BySlot<T>` with tests: `retain_tree` drops a closed slot's value;
      `entry` on an unknown slot creates a default; `iter` is in `SlotId` order.
- [ ] **A.2** `PaneSlots.visible` becomes a `Vec`; `refresh_visible` and
      `set_visible` take a slice. The single-browser duplication rule disappears:
      with a `Vec` there is simply one entry, and `len()` says so.
- [ ] **A.3** `just t norte-frontend`, `just t norte-tui`, snapshot unchanged.

## Stage B — the model

- [ ] **B.1** `App.history: BySlot<History>`.
- [ ] **B.2** `mouse::MouseState.epochs: BySlot<u64>`, `geometry: Vec<PaneGeometry>`
      carrying its `SlotId`. The hit test resolves to a `SlotId`, not an index —
      that is what makes a click land on the pane the reader saw.
- [ ] **B.3** `ui::pane_cols` and `pane_geometry` return `Vec`, not `[_; 2]`.
- [ ] **B.4** `focus` stays a visible POSITION, and `before_frame` clamps it into
      range after every reparto. Making it a `SlotId` reads better and costs a
      translation at every one of the ~212 sites for nothing: the position is
      exactly what "the focused panel" means on screen.
- [ ] **B.5** Snapshot unchanged.

## Stage C — the run loop

- [ ] **C.1** `fill`, `decorate_fetch`, `refreshed`, `watch_targets`, the
      stat-probe dedup and the live search move to `BySlot`. Mechanical: index by
      `app.panes.slot_of(i)` at the boundary, and by `SlotId` inside.
- [ ] **C.2** The anchor from above goes GREEN: a fill for a closed slot is
      dropped.
- [ ] **C.3** `swap_seq` and `reconcile_swap` are re-read in this light. If
      keying by slot makes them redundant, they go, and the commit says why. If
      they are not, the comment explaining what still needs them is the
      deliverable.
- [ ] **C.4** `just ci-fast`. Snapshot unchanged. **Stop here if anything is
      shaky** — everything above is a refactor with no user-visible change, and
      it is worth landing on its own.

## Stage D — splits

```rust
/// El árbol con el hueco `id` partido en dos a lo largo de `dir`, con `nuevo`
/// al lado. Los dos quedan con el mismo peso.
#[must_use]
pub fn split_slot(&self, id: SlotId, dir: Dir, nuevo: &Node) -> Node;
```

- [ ] **D.1** `split_slot` with tests, including splitting a slot that is inside
      a `Tabs` (the split goes INSIDE the tab, not around the group).
- [ ] **D.2** `layout.split-h` / `layout.split-v`: mint a slot, inherit the
      focused panel's directory and entries as `pane.tab-new` does, and focus the
      new one.
- [ ] **D.3** Catalogue, both locales, the `panes` help topic, and the presets —
      **unbound by default**, as with the rest of `layout.*`.
- [ ] **D.4** The three-panel case in `layout_anchor.rs`: geometry matches the
      buffer, minimums still hold, and the focus survives a close.

## Stage E — the target

- [ ] **E.1** With three or more panels visible, the panel holding `target` is
      marked in its chrome. **This is the whole point of the role**: with two
      panels the destination is the other one and nobody needs telling, but a
      copy toward a panel the reader did not have in mind is silent data loss
      (ADR 0058 D7).
- [ ] **E.2** An operation that needs a destination and has no `target` ASKS for
      a path. Reachable now: three panels, no designation.
- [ ] **E.3** `layout.set-target` gets a default binding, since it now does
      something.

## Stage F — configuration

- [ ] **F.1** `[ui.layout] preset = "..."`, and `~/.config/norte/layouts/<n>.toml`
      in the SAME format L2 will serialise.
- [ ] **F.2** Invalid layout: hot reload keeps the previous one; cold start falls
      back to `orthodox` with a message. Uses `validate` and `LayoutDiagnostic`,
      which already exist.
- [ ] **F.3** `norte doctor` reports `LayoutDiagnostic`.
- [ ] **F.4** `just ci`, changelog, memory, branch review.

## What actually happened

Six stages, all landed.

- **The blocker was measured correctly**, and that was the single most useful
  thing in this plan: not ~212 call sites but a handful of position-keyed
  structures, because `PaneSlots` had already absorbed the rest.
- **The adapter trick worked a third time.** `Histories` keys by slot and is
  still indexed by position, so all 59 history call sites were untouched. Ask
  whether an adapter makes N zero *before* rewriting N sites.
- **Stage C paid for the whole refactor**, exactly as predicted. It also found
  something the plan did not: `tokio::select!` has fixed arity, so two arms
  became `poll_fn` loops, and the fill scan had to **rotate its starting
  point** — sweeping from the front every time lets a fast drainer in the first
  slot starve the rest, which `select!` avoided by choosing at random. That
  would have been a silent regression.
- **Stage D exposed a live bug the moment splits existed**: the destination was
  `focus ^ 1`, which with a third panel is not merely wrong but out of range.
- **`reconcile_swap` survives.** Swapping panels moves the CONTENTS between
  slots and leaves the ids in place, so work in flight still travels with its
  listing. Swapping the **ids in the tree** instead would make the whole
  reconciliation unnecessary and would make `swap_seq` redundant with the epoch
  vector. Worth doing; not worth doing at the end of a long session.
- **`layout.set-target` ships unbound.** Picking a chord that clears seven
  presets is churn with no payoff while the palette reaches it, and the same
  rule already applies to the rest of `layout.*`.
