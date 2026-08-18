# Five screens you can pick, and a session that survives the daemon

> Status: accepted, not built. Successor to
> [a screen made of slots](2026-08-17-layout-slots-tabs-design.md), which built
> **L1** (the layout engine), **L1b** (sizing), **P6** (n panels) and **L3**
> (the `places` sidebar and the docked `preview`), and left **L2** — the UI
> session in the daemon — sketched.
>
> This document specifies two phases:
>
> - **Phase A**, client-side, no wire: five factory layouts, two new panel
>   kinds (`processes`, `metadata`) and the dialog that switches between them.
> - **Phase B**, **L2**: `session.get` / `session.put`, a versioned file on
>   disk, and a screen that comes back unchanged after a daemon handover.
>
> Two things asked for alongside these are **deliberately out** and get their
> own specification: the `norte` home panel (a whole new surface) and the
> `clipboard` panel (which needs a clipboard *model* first, and that is core
> work touching the journal and the policy engine).

## The differential that started this

L1 built a screen out of slots and gave it exactly one arrangement:
`panel::orthodox()`, compiled in Rust, identical to the screen norte has always
had. Everything else the engine can express — a sidebar, a docked viewer, a
single panel, four panels — is reachable only by splitting and docking by hand,
one command at a time, every time you start the program.

Two gaps follow from that, and they are what this document closes:

- **There is nothing to pick.** The engine can describe five useful screens and
  ships one. `[ui] layout = "<name>"` loads a file the user has to write by
  hand, in a format the specification itself says nobody will write by hand.
- **Nothing survives.** Not a restart, and — worse — not a daemon handover,
  which is the event `daemon.going_away` (ADR 0055) exists to make invisible.
  A session held in one frontend's memory is erased by exactly the upgrade the
  handover was built to hide.

## Decisions taken before this document

1. **Phase A ships before Phase B.** Phase A is client-side and touches no
   protocol; Phase B is a wire change with a version bump and a mandatory
   `protocol-guardian` review. Building the presets first means that when L2
   arrives there is real content to persist and a real consumer to settle the
   format against, instead of a format designed in the abstract.
2. **A layout preset and a keymap preset are independent knobs.** There is
   already a keymap preset called `krusader`; there will now be a layout called
   `krusader`, and choosing one does **not** choose the other. The picker says
   so on the row where the names collide. Rejected: making the keymap preset
   offer its layout, because a dialog that appears while you are changing
   something else is the dialog people learn to dismiss unread.
3. **The `processes` panel does not replace the `tasks` strip.** They coexist.
   `orthodox` keeps the strip and its snapshot; `processes` is opt-in and
   appears in `explorer` and `full`.
4. **There is no `session.changed` notification.** A second client *clones and
   diverges* (decision 7), so a session has exactly one writer, and there is
   nobody to notify. Live mirroring between clients stays out of scope, as it
   was in L1.
5. **One format, three uses.** The tree serialisation is the same for a factory
   preset, for `layouts/<name>.toml`, and for the session blob on the wire and
   on disk. This was already ADR 0058's decision; phase A is where it stops
   being theoretical, because the presets become TOML files instead of Rust.
6. **The session persists with or without a daemon.** norte runs with the core
   embedded in the TUI or the CLI, which is how it is mostly used. A session
   that only exists under a daemon would be a feature for the minority
   configuration.
7. **One owner at a time, by file lock.** Whoever holds the lock persists.
   Anyone else — a second client of the same daemon, or a second embedded core
   — starts from a **copy** of the owner's arrangement and runs detached,
   persisting nothing.

## Phase A: five screens and two kinds

### The five

`status` is a one-row slot at the foot of all five and is left out of the
drawings.

```text
orthodox (today, the default)      simple
┌──────┬──────┐                    ┌────────────┐
│ brow │ brow │                    │  browser   │
├──────┴──────┤                    ├────────────┤
│    tasks    │                    │   tasks    │
└─────────────┘                    └────────────┘

krusader                           explorer
┌───┬─────┬─────┐                  ┌───┬──────┬─────┐
│pla│ brow│ brow│                  │pla│ brow │ pre │
├───┴─────┴─────┤                  ├───┴──────┴─────┤
│     tasks     │                  │   processes    │
└───────────────┘                  └────────────────┘

full
┌───┬─────┬─────┬─────┐
│pla│ brow│ brow│ pre │
│   │     │     ├─────┤
│   │     │     │ met │
├───┴─────┴─────┴─────┤
│      processes      │
└─────────────────────┘
```

The sidebar sits beside the **panes**, not beside the chrome, and the task
strip runs the full width under it. That is not a drawing choice: it is exactly
what `Node::dock` produces from `orthodox` today, and the cheapest possible
test says so — `orthodox` docked with a `places` slot **is** `krusader`. A
preset that could not be reached by pressing keys would be a second, silent
definition of what docking means.

| name | what it is for |
| --- | --- |
| `orthodox` | the default; two panels, the task strip, the status bar. Unchanged. |
| `simple` | one panel. A narrow terminal, an ssh session, a screen shared on a call. |
| `krusader` | two panels with the places sidebar. |
| `explorer` | one panel with the sidebar and the docked viewer, and processes as a panel. |
| `full` | everything on: sidebar, two panels, viewer, metadata, processes. |

`simple` is not a degenerate case. With a single `browser` there is **no other
pane**, so `F5` has no default target — the L1 specification already decided
what happens there: the operation *asks*, and opens the path dialog instead of
failing. `simple` is the layout that makes that path reachable by choice rather
than by accident, and it is where its test lives.

### Presets are TOML, not Rust

```text
crates/norte-frontend/presets/layout/
  orthodox.toml  simple.toml  krusader.toml  explorer.toml  full.toml
crates/norte-frontend/src/layout/presets.rs
```

`presets.rs` mirrors `keymap/presets.rs` exactly, because that module already
solved this problem and its comments say what it cost: `NAMES` and `source()`
are separate items **tested against each other**, so a preset added to one and
not the other fails CI instead of drifting silently.

```rust
pub const ORTHODOX: &str = include_str!("../../presets/layout/orthodox.toml");
// … simple, krusader, explorer, full

pub const NAMES: &[&str] = &["orthodox", "simple", "krusader", "explorer", "full"];

pub fn source(name: &str) -> Option<&'static str>;

/// Parsed and validated. Infallible in practice — the tests below prove every
/// bundled preset parses — but it returns a `Result` because it goes through
/// the same `config::load` path a user file does.
pub fn tree(name: &str) -> Result<Node, LayoutError>;
```

`crate::panel::orthodox()` becomes `presets::tree("orthodox")`. **The equality
is pinned by a test before the function is deleted**, not after: the whole
value of a snapshot-anchored refactor is lost if the anchor moves in the same
commit as the thing it anchors.

Two properties, table-tested over `NAMES`, that are cheap and catch the entire
class of mistake a hand-written TOML tree invites:

- every preset parses, `validate`s, and names only kinds that
  `KindRegistry::builtin()` declares;
- every preset has unique `SlotId`s, and the ids the TUI knows by name
  (`SLOT_LEFT`, `SLOT_RIGHT`, `SLOT_TASKS`, `SLOT_STATUS`) mean the same thing
  in all five, so switching layouts does not renumber the panels underneath.

### `processes`

```rust
decl("processes", (30, 4), /*focusable*/ true, /*takes_keys*/ true,
     /*multi*/ false, SIN_ROLES)
```

A real panel over the `TaskBoard` the strip already renders: one row per task
with a progress bar, the rate and what it is working on, `Enter`/arrows to
move, and cancel.

**There is no pause.** The protocol has `task.cancel` and nothing else, and a
control that does not do what it says is worse than a control that is missing.
If pausing is wanted it is a protocol change with a scheduler behind it, and it
is not this.

A hidden `processes` slot — behind a tab, or collapsed out — subscribes to
nothing, like every other hidden slot since L1.

### `metadata`

```rust
decl("metadata", (24, 4), /*focusable*/ true, /*takes_keys*/ true,
     /*multi*/ false, SIN_ROLES)
```

Attributes of the entry under the cursor: size, times, mode and owner, the
target of a symlink, what the providers and plugins have to say about it. It
**follows the `active` role**, with the same `Bindings` and the same
`preview::want` shape L3 built, which is what makes it testable without a
renderer: a hidden slot produces no target, so there is no request to count.

Two rules inherited from the docked viewer, and they are the ones that make a
following panel usable rather than hostile:

- it never interrupts. A file it may not stat paints the reason where the
  attributes would go — it does not raise a dialog for every row you pass over
  on the way down a listing;
- a slot nobody can see reads nothing at all.

Neither kind holds a role. A sidebar, a task list and an attribute sheet are
never the destination of a copy.

### Picking a layout

```text
layout.pick                           open the picker
layout.processes, layout.metadata     toggle those two panels
ntc --layout <name>                   by name, beside the existing --preset
```

There is no `layout.use <name>` command. The catalogue maps a chord to a
command id with no arguments, and inventing a parameterised command for this
would be a new concept in the keymap for one caller. By name is what
`[ui] layout` and the flag are for; the picker is what the keyboard gets.

The picker lists the five factory layouts and whatever is in
`<config>/layouts/*.toml`, each with an **ASCII preview rendered from the tree**
by running `resolve` over a small `Rect` and drawing the boxes it returns —
never a drawing stored beside the file, which is how a preview starts lying.
Rows whose name matches a keymap preset carry the one-line note from decision 2.

The new commands ship **unbound**, reachable from the palette and the menu bar
and shown greyed in the reference sheet. That is the pattern the repository
already uses for planned capabilities (#132–#140), and #228 is the open issue
for binding the `layout.*` family across the seven presets — deliberately not
folded in here, because it is seven files of keyboard convention and it would
double this phase.

Applying a layout keeps what the panels were showing: `App::set_layout` already
carries the focused pane's directory across, because a saved layout names slots,
not paths.

### #227 comes with phase A

`layout.grow` and `layout.shrink` do nothing to a `Fixed` slot. Three of the
five presets have a fixed-width sidebar, so shipping them without this fix
means shipping three layouts whose sidebar cannot be resized. It is in scope
here, with the failing test first.

### What phase A touches

`norte-frontend` (presets, two `KindDecl`s), `norte-tui` (two renderers, the
picker, the commands), `norte-i18n` (every string), the help corpus and the
goldens. Budget the goldens: L3 measured that **adding one command moves four**
— the help corpus list, its `[&str; N]` size, the help overlay snapshot and the
CLI's `help-en.json`.

## Phase B: L2, the UI session

### The shape on the wire

Protocol **0.48.0**, additive, with golden fixtures and a mandatory
`protocol-guardian` review.

```text
session.get                          -> { version, revision, body }
session.put { version, revision, body }
                                     -> { revision } | Conflict | TooLarge
```

### Where the types live, and why the core does not read the session

The obvious shape — `layouts: Map<ProfileId, Node>` and
`slots: Map<SlotId, SlotState>` as protocol types — cannot be written. `Node`,
`SortSpec` and `ColumnId` live in `norte-frontend`, and `norte-frontend`
depends on `norte-proto`; mirroring them into the protocol would duplicate
four types across a dependency edge and make every new UI field a wire change
with a version bump and a golden.

It is also the wrong answer on its own terms. ADR 0058 already decided that
**the core keeps the screen and does not read it**. So the session crosses the
wire as an *opaque document*:

```rust
// norte-proto
pub struct Session {
    pub version: u32,        // schema of `body`, owned by the frontends. 1.
    pub revision: u64,       // bumped by the core on every accepted put
    pub body: RawDocument,   // serialised by norte-frontend, never parsed here
}
```

The core stores it, versions it, hands it back and writes it to disk. It
enforces exactly two things, and both are about protecting itself rather than
understanding the content: `revision`, and a **1 MiB ceiling** on `body`,
refused with a typed error rather than truncated.

What the frontends put inside `body` is theirs, and this is its v1 schema:

```rust
// norte-frontend
struct SessionBody {
    layouts: BTreeMap<ProfileId, Node>,   // exactly one in v1: `default`
    slots:   BTreeMap<SlotId, SlotState>, // shared across profiles
}

struct SlotState {
    path: VPath,                 // NOT a String. Rule 1.
    cursor: u64,
    back: Vec<VPath>,            // capped at 64
    forward: Vec<VPath>,         // capped at 64
    sort: SortSpec,
    columns: Vec<ColumnId>,
    show_hidden: bool,
}
```

Two consequences worth stating, because they are the reason for the shape:

- **Adding a field to `SlotState` is not a protocol change.** It is a bump of
  `version` inside the body, and the migration lives in the crate that owns the
  meaning. Over the life of a file manager that is the difference between one
  wire bump and a dozen.
- **The caps and the age sweep are the client's**, not the daemon's. A core
  that does not read the body cannot count slots in it. This is consistent:
  whoever writes the state prunes it.

**The whole blob travels, and the client coalesces.** The cursor moves on every
arrow key; a per-field method family would be fifteen methods, fifteen goldens
and a merge engine nobody asked for. Instead the client keeps the session in
memory, and pushes after a moment of quiet. The blob is kilobytes.

`revision` is the entire concurrency story: a `put` carrying a stale revision is
rejected with `Conflict` and the client re-reads. It is not there for
simultaneous editors — there are none — it is there for the client that
reconnects after a handover holding state from before it.

**`layouts` is a map with one entry.** The day a TUI-shaped and a GUI-shaped
arrangement are both wanted, it is one more entry in the body and nothing at
all on the wire, while paths and cursors stay shared, because `slots` is keyed
separately. That sharing is the reason the session lives in the core at all.

**Marks do not travel.** A selection is thousands of paths and it is the state
of an operation in progress, not of a session.

**A layout that does not mention a `SlotId` does not delete its state.**
Otherwise switching layout throws away your history — and with five layouts to
switch between, that stops being hypothetical. Orphan state is kept, capped at
128 entries and swept by age at 30 days, both applied when writing.

**Unknown kinds round-trip intact.** L1 already keeps `Unknown { kind, raw }`
with its `params` on re-serialisation. An opaque body makes this free across
the process boundary — the core cannot drop a field it never parses — but free
is not proven, so the test is still there: a golden whose body names a kind no
binary declares, through `put`, `get`, and a restart.

### On disk

`<state_dir>/norte/session.json`, written by a temporary file and a rename, the
way the rest of the configuration is written.

**JSON, and not TOML, and that does not break decision 5.** What is shared
between a factory preset, `layouts/<name>.toml` and the session is the *serde
shape* — one set of types, one `Node`, one meaning. The encoding differs
because the uses differ: the two files a human edits are TOML, and the session
is machine-written, rides a JSON-RPC wire, and carries an opaque body that TOML
nests badly. `serde` is what makes this a choice of writer rather than a second
format.

`state_dir()` already exists in
`norte-config::dirs` and already creates its directory with a mode the logging
module fought for; the session uses it, it does not invent a second one.

It is written:

- after **one second of quiet**, coalescing everything that happened;
- always on **`daemon.going_away`** — the handover is the event this exists for;
- always when the **last client disconnects**.

Not on every change. A file written on every keystroke is a database, and there
is already a database in this project that is not this one.

### Who owns it

An advisory lock on the session file. The holder persists. Anyone else — a
second client of the same daemon, or a second embedded core — is handed a
**copy** of the owner's session and runs detached: same screen, same paths, and
from there it diverges and writes nothing. Opening a second terminal gives you
what you expected, and there are never two writers over one state.

### When it goes wrong

| case | behaviour |
| --- | --- |
| no session file | start from `[ui] layout`, or `orthodox` |
| unreadable or corrupt | diagnostic, then start from `[ui] layout`. **Never a blank screen** |
| `version` from the future | refuse to overwrite it, run detached, and say so. Losing a newer session to an older binary is not recoverable |
| unknown kind inside | `Unknown { kind, raw }`, preserved on the way back out |
| stale `revision` on `put` | `Conflict`; the client re-reads and re-applies |
| lock held | clone and run detached, with a line in the status bar |
| `body` over 1 MiB | `TooLarge`. The put is refused and the stored session stands; the client drops history first and retries once |

## Testing

Phase A:

- table tests over `NAMES`: parses, validates, known kinds, unique ids;
- `presets::tree("orthodox")` equals `panel::orthodox()` — **written before the
  function is removed**;
- a snapshot per preset at 80×24 and at 40×10, the second one exercising the
  collapse path that three-column layouts reach first;
- `orthodox` at 80×24 is byte-identical to today's snapshot;
- a hidden `metadata` or `processes` slot issues no reads, with `MemProvider`;
- `simple` plus `F5` opens the path dialog instead of failing.

Phase B:

- round-trip: session → JSON → session, including a kind no binary declares;
- `Conflict` on a stale revision, and the client's re-read path;
- `TooLarge` over the ceiling: the stored session is untouched, and the client's
  drop-history-and-retry lands;
- orphan state survives a layout switch; the cap and the age sweep both fire;
- a non-UTF-8 filename survives `path` end to end — this is the corpus fixture
  the encoding rule asks for, and the exact place a `String` would have eaten it;
- **handover**: a daemon says `going_away`, a replacement starts, the client
  reconnects and the screen is what it was. Extends the existing test in
  `crates/norte-core/tests/daemon.rs`, which already drives that sequence;
- the lock: a second core clones, writes nothing, and leaves the owner's file
  untouched.

## Risks

**Phase A is content; phase B is a wire change.** They fail differently. A
wrong preset is a bad drawing and a five-minute fix. A wrong session format is
migration work forever, which is why `version` is in the blob from the first
byte and why the format is the one L1 already serialises rather than a new one.

**The blob grows with what it remembers.** History is capped at 64 per slot,
orphans at 128 slots. Both caps are in the specification rather than left to the
implementation, because a cap discovered later is a migration.

**An opaque body moves validation to the client.** The core cannot reject a
malformed session, so a client bug persists garbage and the next start reads it
back. The mitigation is the one the config layer already uses: validate on
load, and on failure keep a diagnostic and fall back to `[ui] layout` rather
than propagating the bad state. The 1 MiB ceiling is the only thing the core
can honestly check, and it checks it.

**`path: VPath`.** Stated twice on purpose. Every other field here is a number
or an enum, and it would be easy to type the one that is not as a `String` and
lose a filename that no test in the frontend crates would notice.

## Out of scope

- The **`norte` home panel** — a menu, shortcuts and toys on a start screen. A
  new surface, not a rearrangement of existing ones. Own specification.
- The **clipboard**, panel and model both. Copy here, paste there, inspect it,
  edit it: that is core work touching the journal and the policy engine, and
  the fastest way to get a filesystem bypass is to let it in through a layout
  document.
- Live mirroring between two clients.
- Layout profiles with more than one entry. The shape is carved; the feature is
  not built.
- Binding the `layout.*` family across the seven keymap presets (#228).
- Plugin-contributed kinds. The registry stays open; nothing loads one.

## Records

ADR **0059**, for the session on the wire and on disk: what is stored, who owns
it, and why there is no notification.
