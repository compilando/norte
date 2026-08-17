# L3: `places` and a docked viewer

> Status: accepted, not built. Third slice of the layout line
> (`2026-08-17-layout-slots-tabs-design.md`, ADR 0058), after L1a, L1b and P6.
> L2 (`session.*` in the daemon) is independent of this and is not touched here.

## What this is for

The layout engine can describe any screen the model allows, and every panel it
has ever been asked to place is a `browser`. Two of the three kinds L3 sketched
are the first consumers of the parts the engine already has and nobody uses:

- `Bindings::follows` exists, is resolved by `resolve_follow`, degrades to the
  `active` role with a `LayoutDiagnostic::FollowRetargeted` — and **has no
  consumer**. A binding nothing binds is a rule nobody has tested against a
  real panel.
- `Size::Fixed` was built in L1b for the status bar. A sidebar the user can
  widen is its second consumer, and the one that decides whether the stored
  format got it right.

The user-visible half: a **places sidebar** (drives with their free space, the
hotlist) that stays on screen instead of being a popup you dismiss, and a
**viewer docked in a slot** that follows the cursor of the active listing —
Ctrl+Q in the orthodox tradition, `preview` in the L3 sketch.

## Scope

**In:** the `places` kind, the docked viewer, the two toggle commands, their
Fluent strings, and the tests that pin the rules below.

**Out:**

- **`metadata`.** What makes that panel worth having is properties and
  recursive directory size, which is #139 — a core task with cancellation, not
  a panel. Building the box before the data is a box with four fields the
  listing already shows.
- **A `Remotes` section in the sidebar.** There is no source of remotes:
  connections are #140. A `sftp://` entry in the hotlist already appears under
  Favorites, which is the honest amount of remote norte knows about today.
- **Dragging the sidebar edge with the mouse.** `layout.grow`/`layout.shrink`
  already resize a slot and work on this one for free. A drag gesture is mouse
  work in `norte-frontend/src/mouse.rs`, independent, and it can come later.
- **`clipboard`** — as the parent spec says, core work with its own spec.

## Two model decisions

### The docked preview is not a new kind

It is the **`viewer` kind that `KindRegistry::builtin()` already declares**,
placed in a slot, with `Bindings { follows: Some(Follow::Role(RoleId::Active)) }`.

This is what the model in ADR 0058 says: a kind is *what is inside*, a binding
is *whose view I am*. A pinned viewer (opened on one file, stays there) and a
preview that tracks the cursor are the same renderer with a different binding,
so they are the same kind. `Params` carries `path` for the pinned case; when a
`follows` binding resolves, the followed slot's cursor wins and `path` is
ignored.

Rejected: a `preview` kind of its own. It would be a second file reader to keep
in step with the first, and the two would diverge on encodings, hex and plugin
previews — the three things the viewer already gets right.

**The full-screen viewer stays exactly as it is.** `App.viewer: Option<Viewer>`
is the modal one, opened by `pane.view`, and this change does not move it.
Full screen stays full screen; only the new docked instance lives in a slot.

### `places` is a new kind, and it holds no role

```rust
decl("places", (14, 5), /* focusable */ true, /* takes_keys */ true,
     /* multi */ false, SIN_ROLES)
```

No roles: a sidebar is never the destination of a copy. `multi: false`: two
identical sidebars are a bug, not a layout. The 14-cell minimum is what fits
`/boot 402M` plus the frame.

## Components

### `norte-frontend/src/places.rs` — state, no I/O

Follows the shape of `help.rs`: pure state over data handed to it, no
`Backend`, so it is testable without a daemon.

```rust
pub struct PlacesState { /* sections, cursor, collapsed set */ }

pub enum PlaceRow {
    Header(Section),                 // Drives / Favorites
    Drive { label: String, mount: VPath, free: Option<u64>, total: Option<u64> },
    Favorite { name: String, target: Result<VPath, String> },
}

impl PlacesState {
    pub fn set_drives(&mut self, volumes: &[Volume]);
    pub fn set_favorites(&mut self, items: &[HotlistItem]);
    pub fn rows(&self) -> &[PlaceRow];
    pub fn activate(&self) -> Option<VPath>;   // None on a header or a broken favourite
    // cursor movement, section fold/unfold
}
```

Two sections in v1, in this order: **Drives** (a snapshot of `host.volumes`)
and **Favorites** (the `hotlist` already merged into `LoadedConfig`). Both are
data the frontend already fetches for the existing popups; neither is a new
wire method, so **`norte-proto` is untouched and no protocol bump is needed.**

### `TuiPanel` gains two variants

```rust
pub enum TuiPanel {
    Browser(Box<Pane>),
    Places(Box<PlacesState>),
    Viewer(Box<Viewer>),
    Unknown { kind: KindId, raw: Params },
}
```

This is the first time something other than a `browser` lives in a slot, which
is the point: L1a deliberately left `viewer`/`compare`/`sync` as named fields
because moving them bought nothing then. It buys something now.

`PaneSlots::refresh_visible` filters browsers for the side index and must keep
doing exactly that — a sidebar is not "the left pane", and `app.panes[0]` must
go on meaning the leftmost **listing** with the sidebar open. That is a test,
not a comment.

### Where they sit in the tree

`orthodox()` does not change. The two toggles rewrite the body split in place:

```
Split(V) [ Weight(1), Auto, Fixed(1) ]
├── Split(H) [ Fixed(16), Weight(1), Weight(1), Weight(1) ]
│   ├── Slot(places)                       ← Fixed(16), toggled
│   ├── Slot(left,  browser)
│   ├── Slot(right, browser)
│   └── Slot(preview, viewer) follows Role(Active)   ← Weight(1), toggled
├── Slot(tasks)   Auto
└── Slot(status)  Fixed(1)
```

Slot ids are minted the way splits already mint them; `places` and the docked
viewer are ordinary slots and are serialised into `layouts/<n>.toml` with no
format change (`Bindings` already serialises, `Size::Fixed` already
serialises).

### The fetch: one in-flight per slot, keyed by slot

`open_viewer` in `main.rs` cannot be reused as-is: it is a modal fetch that
`select!`s over the event stream so Esc can abandon it. A docked preview reads
while you keep pressing arrow keys.

It reuses the machinery P6 stage C built instead: a `BySlot<PreviewFetch>`
beside `fill` and `decorate_fetch`, polled in the same rotating `poll_fn` scan.
Starting a new read for a slot **supersedes and aborts** the one in flight for
that slot; a reply whose slot no longer holds a following viewer is dropped.
Reads use the same `VIEW_CAP` header budget (256 KiB) and the same plugin
preview fallback chain as `open_viewer`, extracted so both call one function.

## Commands, keys and menu

Two new command ids. **Checked against `keymap/catalogue.rs` first** — neither
name is taken, and neither is reserved as `planned`; that check is here because
L1b built `tab.*` and had to rename everything to `pane.tab-*`.

| id | does |
| --- | --- |
| `layout.places` | opens/closes the sidebar; when open and unfocused, focuses it |
| `layout.preview` | opens/closes the docked viewer |

They join the `layout.*` family (`split-h`, `split-v`, `close-slot`,
`equalize`, `grow`, `shrink`, `focus-next`, `focus-prev`, `set-target`), which
is where a command that rearranges the screen belongs. Both go in the **View**
menu over the shared catalogue, with `menu-item-*` labels — a menu label is not
a help sentence, and the 74-column dropdown that tmux caught once is not to be
repeated.

Binding: only the core gets a key, as with every batch since K2. `alt+b` for
the sidebar and `alt+q` for the preview in the presets that have room; the rest
stay unbound and visible in the palette. The exact keys are the plan's to
settle against the seven presets, not this spec's.

Inside the sidebar: `up`/`down`/`enter` through the existing `dialog.*` family
where the semantics match, and `pane.refresh` re-reads the drives.

## Hard rules

These are the design. Each one has a test named after it.

1. **The preview never asks.** It follows the cursor, so a policy denial cannot
   raise a dialog per keystroke: the reason is painted inside the slot and
   nothing else happens. The modal viewer keeps its current behaviour.
2. **A hidden slot reads nothing.** A preview behind a tab, or in a collapsed
   split, issues zero requests. The suspension comes free from iterating
   visible panes — and L1b already had one leak that only a test saw, so this
   one is pinned with a counting backend.
3. **Every read is keyed by slot.** Never by position. P6 stage C is the
   precedent: a listing grew with another directory's entries because a reply
   in flight was applied to whoever occupied that position when it landed, and
   no green suite saw it.
4. **`places` does not poll.** Drives are fetched when the sidebar opens and on
   `pane.refresh` while it has focus. Nothing else. A sidebar polling
   `host.volumes` is the inotify problem with a different name.
5. **A directory under the cursor is not read.** The preview paints the name
   and "directory" and issues no request.
6. **Both are closed by default.** Acceptance criterion, same as L1a and P6:
   the `orthodox` snapshot at 100×30 is byte-identical before and after.
7. **`app.panes[i]` still means the i-th listing.** The sidebar and the preview
   are not sides.

## Errors and degradation

- `Volume::free_bytes` absent means *did not answer*, never zero — the rule
  `space.rs` already states, and its `human_bytes`/`volumes-size-unknown`
  formatting is reused rather than re-derived.
- A hotlist entry with `target: Err(key)` is painted greyed with its reason,
  never hidden: a favourite that silently vanishes is a config bug you cannot
  see.
- A read that fails paints its error category in the slot. It does not clear
  the previous file silently and it does not retry on its own.
- Names are bytes: `VPath` to display goes through the explicit lossy
  conversion. No `to_str().unwrap()`.
- A followed slot that dies degrades to the `active` role and emits
  `FollowRetargeted`, which is existing engine behaviour this is the first code
  to exercise.

## Testing

- `orthodox` snapshot unchanged with both toggles off (the acceptance gate).
- New snapshots: sidebar open at 100×30, preview open, both open, and a 40-cell
  terminal where the sidebar's `Fixed(16)` competes with two browsers'
  minimums.
- **Geometry assertions read cells, never `contains`.** `TestBackend::
  to_string()` wraps each row in quotes; a `contains` hid a one-cell offset for
  a whole batch once.
- A counting backend proves a hidden preview reads nothing, and that moving the
  cursor N times with a superseding read in flight produces at most one applied
  result.
- A denied read paints and does not enqueue a prompt.
- `follows` retarget: kill the followed slot, assert the diagnostic and that
  the preview switches to the new `active`.
- `places.rs` unit tests without a backend: rows, cursor over headers,
  `activate()` on a broken favourite is `None`.
- Doctests on the new public items in `norte-frontend`, and `cargo doc -p
  norte-frontend --no-deps` after each task — the intra-doc link lint fired
  three times in L1a with `just t` and `just c` both green.

## Order of work

1. `places.rs` state + the kind declaration + its tests. No screen yet.
2. `TuiPanel::Places`, the toggle command, the tree rewrite, rendering, keys.
3. Extract the read+plugin-preview chain out of `open_viewer`; `TuiPanel::
   Viewer` and `BySlot<PreviewFetch>` on top of it.
4. `layout.preview`, the follows binding, suspension and denial rules.
5. Menu entries, Fluent strings in both locales, changelog.

Reviewers before committing, by surface: `rust-reviewer` on the whole diff, and
`encoding-auditor` on step 1 and 3 (drive labels and filenames are bytes, and
the preview inherits the viewer's encoding surface). No `protocol-guardian`:
the wire is untouched.

## Debt this deliberately leaves

- `metadata` (#139 first), remotes in the sidebar (#140 first), mouse drag on
  the sidebar edge, and the P6 note about swapping ids in the tree instead of
  contents.
- No new ADR: every decision here is an application of ADR 0058's model. If
  implementation forces a change to one of its rules, that change gets the ADR.
