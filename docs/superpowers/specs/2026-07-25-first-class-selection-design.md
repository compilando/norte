# First-class selection — design

**Date:** 2026-07-25
**Status:** approved
**Issue:** #103
**Related:** spec §17 (required product capabilities: first-class selection),
hard rule 1 (filenames are bytes), hard rule 7 (no business logic in
frontends), ADR 0006 (keymap resolution), ADR 0017 (cursor pagination),
ADR 0020 (semantic theme roles).

## Scope

Selection is the missing half of every orthodox operation: mark a set of
entries, then act on the set. `norte-frontend` already models marks and the
GUI reaches them; the TUI cannot mark at all and every operation runs on the
cursor entry alone. This design makes selection a shared, first-class concept
for both frontends: toggle, select all, invert, select/deselect by glob,
marks that survive a refresh, and bulk copy/move/delete.

### Starting point

- `PaneState` holds `marks: HashSet<VPath>` with `toggle_mark`, `is_marked`,
  `marks_len`, `marked_paths` (`crates/norte-frontend/src/pane.rs:436`).
  `marked_paths` falls back to the cursor entry when nothing is marked.
- The GUI dispatches `mark.toggle` (`crates/norte-gui/src/main.rs:1175`) and
  builds transfers and deletes from `marked_paths()`
  (`open_transfer_modal`, `open_delete_modal`), with a conflict backlog
  (`queue_conflict` / `open_next_conflict`).
- The GUI reaches `mark.toggle` only through a **local patch**:
  `crates/norte-gui/src/keymap.rs::gui_supplement` prepends `insert` →
  `mark.toggle` because the shared presets do not know the command. The
  divergence is documented there as a regression fix.
- The TUI has no mark command in `COMMANDS`
  (`crates/norte-tui/src/keymap.rs:53`), no binding in any shared preset, no
  mark rendering in `ui.rs::entry_item`, and `pane.copy`/`pane.move` compute
  a single `from`/`to` from the cursor
  (`crates/norte-tui/src/main.rs:3288-3293`).
- `Role::Mark` already exists (`crates/norte-theme/src/role.rs:50`), with a
  `dim` monochrome fallback. No theme change is needed.
- `globset` is already a workspace dependency, used by `norte-core` for
  `fs.search` name globs. No new dependency enters the graph.

### Out of scope

- Saved/named selections (spec §17 mentions them; a later block).
- Recursive directory size (the `Space` key stays free for it; separate
  work).
- Mouse marking in the GUI.
- Marking `..`: it is not an entry of the listing and stays unmarkable.

## Design

### 1. Model — `norte-frontend::pane`

Marks stay a `HashSet<VPath>`: identity is the byte-exact absolute path.
Sorting is already immune, because marks are keyed by path and never by
index. New public API on `PaneState`:

```rust
pub fn toggle_mark(&mut self);                          // exists
pub fn marks_len(&self) -> usize;                       // exists, keeps its name
pub fn marked_paths(&self) -> Vec<VPath>;               // exists
pub fn mark_all(&mut self);
pub fn clear_marks(&mut self);
pub fn invert_marks(&mut self);
pub fn mark_glob(&mut self, pattern: &str, mark: bool) -> Result<usize, PatternError>;
pub fn marked_bytes(&self) -> u64;
```

`PatternError` is a new `thiserror` type in `norte-frontend` wrapping the
`globset` compile failure (hard rule 6: typed errors in libraries). It
carries the compiler diagnostic so the dialog can show *why* the pattern was
rejected — the same treatment `norte-core`'s search already gives an invalid
glob.

`marked_bytes` sums `Entry.size`; directories contribute 0. This block does
not walk directories, and the status bar must not imply otherwise.

`marked_paths` keeps its cursor fallback: F5 with nothing marked still
operates on the entry under the cursor, exactly as today.

**Visible set semantics.** When the quick-search filter is active,
`mark_all`, `invert_marks`, and `mark_glob` operate on the **visible** subset
(`quick_visible`), not on the whole listing: what you see is what you mark.
When no filter is active, the visible set is the whole listing.

**Partial listings.** While the pane is still filling (`loading()`, ADR
0017), these operations reach only the entries already drained. The pane
footer already prints a "partial" marker for quick-search; the same marker
covers a bulk mark taken over an incomplete listing. Marking must never
silently claim to cover entries that were never listed.

### 2. Lifecycle — a refresh keeps marks, a `cd` drops them

`set_listing` (`pane.rs:189`) and `begin_loading` (`pane.rs:234`) call
`marks.clear()`, and both are the **`cd` paths** — a new directory, or the
virtual pane of a live search. Clearing there is already the wanted
behaviour.

The **refresh** path is a different method: `refill` (`pane.rs:~592`, reached
from the TUI through `Pane::refresh_listing`), which replaces the listing of
the same directory and does *not* touch marks. So a same-directory refresh
already preserves them, by construction rather than by accident of naming.

What is missing is pruning. A mark whose entry no longer exists must not
linger:

- `refill` prunes the mark set to the paths present in the new listing.
- `set_loading(false)` prunes as well: with pagination (ADR 0017) the
  complete set is only known when the fill ends, so a mark placed before the
  fill finished is validated there.

Both call one private `prune_marks()`. Nothing else changes: `cd` keeps
clearing, refresh keeps preserving.

This satisfies spec §17 ("preserve selections by entry identity across sorts
and refreshes") without introducing per-directory memory of marks: a `cd`
never resurrects a stale selection.

**Consumption.** After a bulk operation is submitted, the marks are cleared —
the selection is consumed by the operation (mc / Total Commander behaviour).
Clearing on submit, not on completion, keeps the rule simple and testable:
there is never a half-consumed selection whose meaning depends on which task
finished.

### 3. Pattern dialog (`+` / `-`)

A single-field text dialog, reusing the machinery `pane.search` already has
for text input. The pattern is a **glob** (`*.rs`, `foto_??.jpg`), compiled
with `globset` — the same library `norte-core` uses for `fs.search`, so the
semantics a user learns in search hold here.

Matching reuses the quick-search fold pipeline (`nav::fold_with`, which is
lossy UTF-8 → NFC → lowercase → NFC, and honours the pane's name
reinterpretation), with the glob compiled `case_insensitive(true)`. One fold
pipeline for every substring/pattern filter in the frontend, never a
divergent copy — `fold_with` becomes `pub(crate)` for this.

The honest consequence, documented in rustdoc and in the dialog help: a
**pattern addresses the displayed text, not the raw bytes**. A non-UTF-8 name
is folded through `from_utf8_lossy`, so its bad bytes appear as U+FFFD and no
pattern can name them; such a name still matches a pattern that only
constrains its valid parts (`*.rs` does match a name whose suffix is a clean
`.rs`). Marking one by hand with Insert always works, and `marked_paths`
returns its original bytes untouched. A hostile-corpus fixture pins both
halves so nobody "fixes" this into byte matching or into a blanket exclusion.

An invalid glob reports the compile error in the dialog and marks nothing.

**Frontend coverage.** The model (`mark_glob`) is shared, but the dialog
ships in the TUI only in this block: the GUI has no text-input widget at all
(no field, no caret, no editing keys — every key it handles resolves through
the keymap), so a pattern prompt there means building text input from
scratch, which is its own piece of work. The GUI gets `mark.toggle` (already
has it), `mark.all`, `mark.invert`, and `mark.clear`, and picks up
`mark.pattern-*` when it grows text input. This gap is explicit and tracked,
not silent divergence.

### 4. Bulk operations in the TUI, converging with the GUI

The TUI modals take the shape the GUI already uses:

```rust
Modal::ConfirmTransfer { kind: TransferKind, items: Vec<VPath>, to: VPath },
Modal::ConfirmDelete   { items: Vec<VPath>, permanent: bool },
```

`to` is the destination **directory** — the other pane's directory, per the
orthodox default. Execution submits one engine call per item and queues
collisions in a conflict backlog, mirroring the GUI's `queue_conflict` /
`open_next_conflict`. The existing `TRASH` capability probe that decides
trash versus permanent delete is unchanged.

The modal lists the first N items and summarises the rest as "… and N more".
That truncation policy moves to `norte-frontend` so the two frontends share
one rule instead of two copies (`item_lines` / `MODAL_ITEM_LIMIT` in
`crates/norte-gui/src/main.rs:3404`).

**Boundary with #105 (editable destination / rename):** `to` is a directory
here. An editable full destination path is meaningful only for a
single-item operation, and that is where #105 will apply.

### 5. Rendering and the status bar

`Role::Mark` already exists, so no theme schema change. Each row gains a
one-cell gutter at its left: `*` when marked, a space when not, styled with
`Role::Mark`. The cue is **textual**, never colour alone — required by the
accessibility discipline, and additionally the monochrome fallback for
`Mark` is `dim`, which on its own reads as "inactive" rather than "selected".

The gutter sits before the hostile badge, so the existing badge column and
decorator badge keep their positions.

The status bar shows the marked count and the marked byte total. That needs a
size formatter, which does not exist anywhere in the workspace yet. It goes
into `norte-frontend::format` in this block, and the columns work (#108)
reuses it rather than adding a second one.

### 6. Commands, presets, i18n

Five commands, using the names the GUI already dispatches:

| Command | `orthodox` chord | TUI | GUI |
| --- | --- | --- | --- |
| `mark.toggle` | `insert` (mark and move down) | new | exists |
| `mark.all` | `ctrl+a` | new | new |
| `mark.invert` | `*` | new | new |
| `mark.clear` | `ctrl+shift+a` | new | new |
| `mark.pattern-add` | `plus` | new | later (no text input) |
| `mark.pattern-remove` | `-` | new | later (no text input) |

A command absent from a frontend's catalogue is not in its
`known_commands`, and the shared preset's binding for it is dropped silently
by `build_for_subset` (`Strictness::Lenient`, `keymap.rs:569`) instead of
failing the build. That leniency covers **preset** bindings only: a user or
project layer naming an unknown command still errors, so a typo never dies
quietly (ADR 0006). This is why `mark.pattern-*` can live in the shared
presets before the GUI implements it.

**The `+` chord does not parse today.** `parse_chord`
(`crates/norte-frontend/src/keymap.rs:218`) splits on `'+'` as the modifier
separator, so `"+"` yields an empty key token and is rejected as
`BadChord`. `-` and `*` parse fine as ordinary `Char` tokens. So the parser
gains one named token, `plus` → `KeyCode::Char('+')`, and `Display` renders
`Char('+')` as `plus` so that `parse(display(chord)) == chord` keeps holding
for every chord. This is additive and cannot break an existing user keymap:
any keymap that contains `+` as a key fails to parse today. No alias is added
for `-` or `*`; one spelling per key.

They are added to the **shared** presets (`orthodox`, `vim`, `cua`), to the
TUI `COMMANDS`, and to the GUI catalogue. `gui_supplement` then drops its
`insert` → `mark.toggle` line, since the shared preset covers it; its GUI-only
task chords stay.

Every new command needs a `help-cmd-*` entry in **both** locales — the suite
fails otherwise. The pattern dialog needs its own Fluent strings, and the
status-bar counter needs a localised, pluralised form.

### 7. Testing

Model, in `norte-frontend`:

- A mark survives a re-sort and a same-directory refresh (`refill`).
- A `cd` (`set_listing`, `begin_loading`) clears marks.
- `refill` prunes a mark whose entry disappeared from the new listing.
- `set_loading(false)` prunes at the end of a paginated fill.
- `mark_all` / `invert` / `mark_glob` respect the quick-search visible set.
- A pattern cannot name the invalid bytes of a non-UTF-8 entry, but does
  match one whose valid suffix satisfies it; `toggle_mark` marks it either
  way and `marked_paths` returns its bytes untouched (hostile-corpus
  fixture, byte-exact round-trip).
- An invalid glob returns an error and changes no marks.
- Marks are cleared when a bulk operation is submitted.

Frontends:

- A marked row and a marked hostile row render with the gutter and the badge
  in the expected order.
- `parse_chord("plus")` yields `Char('+')`, and the existing chord
  round-trip property (`parse(display(c)) == c`) still holds for it.
- The five chords resolve in the three presets; the GUI catalogue stays fully
  reachable (the existing `todo_comando_gui_es_alcanzable_desde_el_preset_default`
  pin keeps guarding this after `gui_supplement` shrinks).
- Bulk operations: N items submit N tasks, conflicts queue in the backlog,
  cancellation leaves the destination clean.
