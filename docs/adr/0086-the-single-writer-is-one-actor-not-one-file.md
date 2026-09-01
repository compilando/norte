# 0086 - The single writer is one actor, not one file

- Status: accepted
- Date: 2026-09-01
- Decision makers: Oscar González
- Related: ADR 0066 (`norte-ui-host` over the SDK), ADR 0085 (deterministic
  async tests), ADR 0058 (layout by slots and tabs).

## Context and problem statement

`crates/norte-ui-host/src/controller.rs` was **18 725 lines**, and **16 203 of
them were a single `impl Estado` block** holding 422 methods. Everything the
window can do lived in it: navigation and listings, tabs, tree and places,
quick/normal/semantic search, dialogs, the viewer, compare and sync, tasks and
progress, plugins and extensions, agents and approvals, profiles, themes,
settings, sessions and persistence.

The single-writer design is correct and is not what was wrong. Renderer
actions, daemon replies and timers all enter one bounded mailbox and one task
applies them in order; that is what lets the bridge promise ordering and
non-skipping sequences (ADR 0066). Nothing here changes that.

What was wrong is that **the actor's discipline was being confused with a
filing decision**. One writer does not require one file. The cost of the
conflation was paid on every change: reading any capability meant paging
through all of them, and a change to one had no structural reason not to touch
another.

## Decision

### The `impl Estado` block is split by capability; the actor is not

`controller.rs` becomes `controller/mod.rs` plus **32 sibling modules**, each
holding one `impl Estado` block for one capability: `input`, `listing`,
`search`, `layout`, `tabs`, `tree`, `places`, `profiles`, `menu`, `selectors`,
`agents`, `extensions`, `settings`, `help`, `viewer`, `effects`, `sync`,
`sums`, `ai`, `transfer`, `approvals`, `fileops`, `dialogs`, `tasks`,
`gestures`, `views`, `nav`, `lifecycle`, `patches`, `session`, `panel`,
`palette`.

`mod.rs` keeps what the actor is: the mailbox, `UiHost`, `actor()`, `Mensaje`,
the `Estado` struct and the auxiliary types its fields are made of, and the
action dispatch. Largest child: 1 484 lines. There is still exactly one writer,
one mailbox, one order.

### It was a pure move, and that is checkable

Not a single method body changed. The check is not a claim, it is a script
that trims the scaffolding, strips the `pub(super)` and normalises whitespace,
then compares the 435 methods of the old file against the 435 of the new ones
**character by character**. It reports identical. `cargo fmt` reflowed some
signatures that `pub(super) ` pushed past column 100; that is the only textual
difference, and normalising whitespace and trailing commas absorbs it.

Doing it by hand would have been neither cheaper nor safer: 16 000 lines
retyped is about 200 000 tokens of output and a fresh opportunity for error on
every one of them. The script's first attempt got it **wrong** in a way worth
recording, because it is the failure mode a reassembly check does not catch:
the cut walked backwards from each `fn` over doc comments, and a multi-line
`#[expect(...)]` ends in `)]`, which does not look like a comment. So the cut
landed *between* `aplicar_efecto`'s `#[expect(clippy::too_many_lines)]` and
`aplicar_efecto`, the attribute crossed into another module, and it stopped
applying. Every line was still present and accounted for — the reassembly
check passed. Clippy caught it. The check that now catches it is that **every
chunk must end by closing its method**; a chunk ending in an attribute or a
doc line means that attribute belongs to the next one.

### Methods open to `pub(super)`; nothing opens further

A child module sees its parent's private items, so `Estado`'s fields need no
change. The reverse is not true: a method defined in `controller::sync` is
private to `controller::sync`, and the parent and siblings cannot call it. The
compiler listed which ones that affects — and the answer was **about 250 of
them**, which is itself the finding: these are not independent slices. The
dispatcher, the views and the effect table call across nearly every boundary.

So all 406 moved methods are `pub(super)`: visible inside `controller`,
invisible outside it. The crate's public surface is unchanged — `UiAction`,
`UiUpdate`, the snapshots and the bridge are untouched, and the dependency
boundary test still passes.

### `use super::*` is allowed here, on purpose

The pedantic lint forbids glob imports. These 32 modules are one `impl` block
cut into pieces and need the parent's imports exactly; enumerating forty lines
of them per file, in 32 files, is a list that desynchronises the first time the
parent imports something. The allow is written with that reason at each site.

### The key sweep now reads the directory, not a list

`tests/catalogo_del_host.rs` checks that every Fluent key the host *chooses*
exists in both catalogues, and it did so by `include_str!`-ing five named
files. Its own header says a hand-written list separates from the code at the
first new surface — and this was that surface: the list named `controller.rs`,
which no longer exists, and would have silently stopped covering the 32 new
files. It now walks `src/` and reads every `.rs`.

Widening it immediately found something the five-file list had never looked at,
which is the point: `bridge.rs` declares `reason_key: String`. That is a field
*declaration*, not a key choice, so the sweep now tells the two apart — what
follows a chosen key is a literal, a `self.` or a call, never a type.

## Consequences

- `controller.rs` 18 725 lines → `controller/` with `mod.rs` at 3 236 and 32
  modules, the largest 1 484.
- `mod.rs` is above the 2 500 that was aimed for, and the remainder is
  deliberate: 2 522 of its lines are the imports, constants, `Mensaje`, the
  actor, `Estado` and the ~20 auxiliary types its fields are made of. Moving
  those types to a sibling would make every one of their ~200 fields need
  `pub(super)`, and would split "what the state is" from "what the actor does"
  — the two things you read together. If the number matters more than that,
  it is a separate decision, not a consequence of this one.
- 423 tests pass, clippy and rustdoc are clean, and the wire goldens did not
  move because nothing they describe was touched.
- The next change to one capability now has a structural reason not to touch
  another. That is the whole return.
