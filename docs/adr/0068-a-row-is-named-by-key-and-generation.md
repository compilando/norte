# 0068 - A row is named by key AND generation, and the bridge breaks on purpose

- Status: accepted
- Date: 2026-08-21
- Decision makers: Oscar González
- Related: ADR 0066 (renderers use a Rust UI host, decisions D7 and D8),
  ADR 0067 (the reference renderer paints), the review of `7ffd004b..HEAD`
  (protocol-guardian B1, rust-reviewer B2), issue #52's lazy listing.

## Context and problem statement

`norte-ui-host` hands a renderer opaque `RowKey`s and promises, in
`bridge.rs`, that a key from an earlier listing is refused:

> Cuando el host re-lista, la generación sube: un click que llega con la
> anterior se responde `StaleAction::Generation` y no hace nada. Es lo que
> impide que un doble click tardío actúe sobre el fichero que ocupó esa fila
> DESPUÉS.

Two independent reviewers found that nothing implemented it. `RowKey` was the
row's index, no action carried a generation, and the guard was a bounds check.
The `Stale { Generation }` answers that existed meant "wrong slot" or "index
out of range" — never "that screen is gone".

The window for the race is not theoretical and is wider here than in the
terminal. A listing fills in background batches (`FIRST_PAGE` then
`FILL_BATCH`), `PaneState::extend` merges each batch into sorted position and
bumps the listing epoch, and sorting by a column re-orders everything. Between
the frame the user saw and the click arriving there is an IPC hop and a
debounced renderer. The terminal frontend already carries a per-pane epoch
guard for exactly this (`norte-tui`'s `Validity`); the bridge had none.

`MarkRange`, added in the same session, made it sharp rather than latent:
`PaneState::mark_range` clamps its endpoints by contract, so
`{"from":0,"to":18446744073709551615}` marked the entire listing — including
rows the renderer was never sent — and what is marked is the input to a delete.

A second question arrived with it. `BRIDGE_VERSION` went 1 → 4 in one session,
and every bump was treated as incompatible even though three of them only
added fields. There was no written rule for when the number moves.

## Options considered

### Option A — Make `RowKey` unforgeable instead of adding a generation

Mint a random or hashed key per row, keep a host-side table.

- **Advantage:** the renderer cannot construct a key at all, so an
  out-of-window range is impossible by construction.
- **Drawback:** a table per slot that must be reaped, and a key that is no
  longer stable across a refill, so the renderer's own bookkeeping (which row
  is the cursor) breaks on every batch.
- **Drawback:** it does not answer the staleness question. A minted key from
  the previous listing is still a valid key.

### Option B — Send the generation with every row-addressed action

Every action that names a row also names the screen it was named on. The host
compares against `PaneState::listing_epoch()` and refuses on mismatch.

- **Advantage:** it is the invariant the contract already claimed, and the
  epoch already exists and is already the `generation` the renderer receives.
- **Advantage:** it is checkable in one place, and the answer is the
  `StaleAction::Generation` the contract already defines.
- **Drawback:** a breaking wire change, and four actions grow a field.
- **Drawback:** a renderer that forgets to update its stored generation gets
  every gesture refused — loudly, which is the failure we want.

### Option C — Keep the bounds check and document the limitation

- **Advantage:** nothing to change.
- **Drawback:** the documented guarantee stays false, and the failure it
  allows is a delete of a file the user did not choose.

## Decision

**Option B.** `SelectRow`, `ToggleMark`, `MarkRange` and `Activate` carry
`generation: u64`. `Estado::fila_de` refuses when it differs from the slot's
current `listing_epoch()`, and both endpoints of a range must resolve in that
generation — a range whose end is out of the window is refused rather than
clamped, because clamping widens a destructive operation to rows nobody chose.
`BRIDGE_VERSION` is 5.

The key is still opaque in the sense that matters: nothing derives a path from
it, and the host never accepts a path from the renderer. It is *not* opaque in
the sense of unguessable, and the generation is what makes that acceptable.

### And the version rule, written down

The bridge has **one** number, and it means incompatible. There is no
compatible tier, and that is deliberate for now:

- A renderer and this host ship together. There is exactly one renderer, in
  this repository, built from the same commit.
- A compatible tier is only honest if an older peer can decode a newer
  payload, which requires `#[serde(default)]` on every added field and a
  renderer that resyncs on an unknown patch kind instead of dropping it.
  Neither holds today, and adding defaults would let a v4 payload decode as a
  v5 one with an empty layout — a screen that is silently wrong, which is
  worse than a screen that says it cannot read.
- So: **any shape change moves the number.** A renderer that sees a version it
  does not know shows an incompatibility screen and stops sending.

When a second renderer exists — a Flutter shell, a headless client someone
else maintains — this rule is reopened, and the reopening is a new ADR that
must supply the two mechanical preconditions above before it claims a
compatible tier.

## Consequences

### Positive

- The guarantee the contract advertised is now the guarantee it enforces, with
  a test for the ordinary race (sort, then act on a pre-sort key) and one for
  the range whose endpoint never existed.
- A destructive operation can only ever see rows the renderer was actually
  shown.
- The version number means one thing, and the meaning is written down where
  the next bump will be argued.

### Negative

- Four actions grew a field, and every renderer must track the generation it
  painted. The reference renderer keeps it per slot and refreshes it on every
  paint, which is one line, but a renderer that gets it wrong will find every
  gesture refused.
- Refusing instead of clamping means a legitimate gesture that races a refill
  is now dropped rather than partially applied. That is the intended trade —
  the user repeats a click; they cannot un-delete — but it will look like a
  missed click under a fast fill, and the renderer has no retry.
- Single-tier versioning means a purely additive field still breaks every
  older renderer. With one renderer that costs nothing; the day it costs
  something is the day the rule is reopened.
