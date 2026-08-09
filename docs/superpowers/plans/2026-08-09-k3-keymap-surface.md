# K3 — the keyboard surface: which-key, the reference sheet, the editor

**Spec:** `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`,
section "K3 — the surface". **Depends on:** K1 (ADR 0043), K2a (ADR 0044),
K2b (ADR 0045, seven bundled presets).

K2b is what makes this worth building: seven presets, four of them transcribed
from other programs, and about thirty bindings that name a command norte has
not built. Today the only way to see any of that is to press the key.

Three pieces, in this order, because each one's data feeds the next:

| | what | why it is second/third |
| --- | --- | --- |
| **K3a** | which-key overlay on a pending prefix | needs a continuations query the engine does not have |
| **K3b** | the reference sheet, unavailable rows included | needs one row builder to replace three duplicated cheatsheet generators |
| **K3c** | the shortcut editor in settings | needs K3b's rows to list from, and a `keymap.toml` writer that does not exist |

## Facts this plan is built on (verified 2026-08-09)

- `Resolution::Pending(usize)` carries a depth, not the candidates. The
  continuation scan exists inline and discards its matches:
  `crates/norte-frontend/src/keymap/effective.rs:526`. `Effective::lookup` and
  `Lookup` are `pub(super)`.
- `Effective::bindings()` (`effective.rs:440`) **filters out anything not
  `Availability::Here`** — all three cheatsheet generators call it.
  `bindings_all()` (`effective.rs:462`) was written for K3 and still has zero
  production callers.
- The cheatsheet is generated three times, with three different padding rules
  and different section sets: `norte-tui/src/help.rs:73` (three screens,
  cell-width padding), `norte-gui/src/help_view.rs:480` (**no dialog
  section**, char padding), `norte-cli/src/help.rs:415`.
- Two unrelated `Availability` enums: `norte_help::Availability`
  (`norte-help/src/model.rs:212`, about *right now* — read-only backend, wrong
  target) and `norte_frontend::keymap::Availability`
  (`effective.rs:26`, about *this build*). `ChordResolver`
  (`norte-help/src/resolve.rs:48`) knows only the first. **Do not conflate
  them and do not widen `ChordResolver`**: the reference sheet carries the
  keymap one as its own column.
- `unavailable_message` (`keymap/mod.rs:249`) is the one place that turns an
  `Availability` into user text (`keymap-unavailable-not-built` with
  `command`/`reason`/`issue`, `keymap-unavailable-not-here`).
- The TUI's overlays are `Option<T>` fields on `App`, not `Modal` variants
  (`app.rs:812` states the idiom); each needs a `draw_*` in the ordered chain
  `ui.rs:262-322` and a bit in `keyboard_owner` (`main.rs:7223`), or K2a's
  count loop will not see it.
- The TUI's `Pending | Counting` arm is `main.rs:3035-3037` and its whole body
  is `app.pending = pending_display(active)`. The status bar's
  "no cheatsheet: the which-key overlay arrives in phase 5" comment is at
  `ui.rs:3177`.
- The GUI has no pending field: it reads the resolver inside `render` and
  paints a strip at `norte-gui/src/main.rs:8330-8356`, **suppressed** while
  settings/extensions/palette/columns_picker/context_menu is open
  (`main.rs:8338`).
- The GUI's viewer `flash` — which carries `unavailable_message` — is **not
  painted while the viewer is open** (`main.rs:3688-3693`, `3724-3731`); both
  comments name K3 as the fix.
- `norte-config` already persists comment-preserving, atomic, cross-process
  locked writes — but **only to `norte.toml`**: `write_user_toml`
  (`load.rs:279`) hardcodes `norte.toml.tmp`, `lock_user_toml` (`load.rs:254`)
  hardcodes `norte.toml.lock`. **Nothing writes `keymap.toml`.**
- `KeymapFile` derives `Deserialize` only; `RawSection`/`RawBinding` and every
  field are `pub(super)` (`layer.rs:12`, `:22`, `:64`). There is no serializer
  and no public accessor for the binding lists.
- `norte help keys` is the CLI surface (`norte-cli/src/main.rs:258`,
  `help.rs:694`); `--json` emits it under `JSON_VERSION = 1` (`help.rs:546`).
  **The spec's `ntc keys` is a spec error**: `ntc` is the TUI binary. Fix the
  spec, do not add a subcommand to the TUI.
- Hot reload after a write already exists: `keymap.toml` is in
  `norte_config::watch::CONFIG_FILES` (`watch.rs:176`), TUI `reload_config`
  (`main.rs:5303`) rebuilds all three resolvers and refreshes an open settings
  overlay, GUI `apply_keymap_live` (`main.rs:2520`) does the same.

---

# K3a — the which-key overlay

## Task a1 — the engine answers "what can follow this?"

**File:** `crates/norte-frontend/src/keymap/effective.rs` (+ re-export in
`mod.rs`).

```rust
/// Every binding whose sequence CONTINUES `prefix`: the rows a which-key
/// overlay paints while `prefix` is pending. The next chord of each match,
/// its command and its availability — unavailable ones INCLUDED, because a
/// which-key panel that hides them recreates the silence K1 removed: the key
/// still resolves, it just says why it does nothing.
pub fn continuations(&self, prefix: &[Chord]) -> Vec<Continuation>
```

- `Continuation { next: Chord, seq_len: usize, command: &str, avail: Availability }`.
  `seq_len > prefix.len() + 1` means the continuation is itself a prefix of a
  longer sequence; the overlay marks those with a trailing `…` rather than
  claiming they run something.
- Deduplicate by `next`: two bindings under the same next chord (`g g` and
  `g h`) are two rows, but `g g` reachable twice is one.
- Deterministic order: by painted chord, so the panel does not reshuffle
  between builds and the tests can pin it.
- An empty `prefix` returns every single-chord binding — legal, and it is what
  a future "show me everything" key would use, but **K3a's frontends never
  call it that way** (see a2 on the bare-count case).

Tests: continuations of `g` in a vim-shaped fixture; a prefix that is also a
complete binding; an unavailable continuation appears with its availability;
determinism; empty prefix returns the singles.

**Verify:** `just t norte-frontend`, `just c`.

## Task a2 — the TUI overlay

**Files:** `crates/norte-tui/src/{app,ui,main}.rs`, `crates/norte-frontend/src/whichkey.rs` (new).

The shared model lives in `norte-frontend` so both frontends paint the same
rows in the same order: `WhichKeyRows::build(eff, prefix, count, lang)` →
rows of `{ chord: String, label: String, avail: Availability, opens_sequence: bool }`,
where `label` is the catalogue's Fluent label (same `help-cmd-*` /
`dialog-cmd-*` routing the help surfaces use) and the *rendering* stays
per-frontend.

- **Opens on `Resolution::Pending` only.** A bare count (`Counting`) does NOT
  open it: the continuation of a count is "any key at all", so the panel would
  be the whole keymap, and K2a already paints the count in the status bar. The
  overlay DOES show the live count in its title when a count is in flight
  behind a prefix (`12` then `g`), because that is the state a user most often
  cannot explain.
- No timers, ever. ADR 0006's resolution is timing-free and a which-key that
  appears after 400 ms would smuggle timing back in through the paint layer.
- Closes on anything that ends the pending state: `Run`, `Unavailable`,
  `Reset`, a non-modelled key, `Esc`.
- New `Option<WhichKey>` field on `App` next to the other overlay options; new
  `ui::draw_which_key` in the ordered chain **before** the modal; a bit in
  `keyboard_owner` — but it takes **no keys of its own**: the pane resolver
  keeps owning the keyboard while it is up, which is the whole point. Say so
  in the doc comment, because it is the one overlay for which that is true.
- Replace the stale comment at `ui.rs:3177` with what actually happens now.
- Unavailable rows are painted dimmed, with the same reason text
  `unavailable_message` produces (short form: reason + `#issue`).

Tests: the pending arm opens it and a `Run` closes it (drive `App` through the
resolver, not the terminal); a count behind a prefix shows in the title; a bare
count does not open it; rows include an unavailable binding.

**Verify:** `just t norte-tui`, `just c`.

## Task a3 — the GUI overlay, and the viewer flash debt

**Files:** `crates/norte-gui/src/main.rs`.

- Same rows from `norte-frontend::whichkey`, painted as a panel anchored where
  the pending strip is (`main.rs:8330-8356`), reading the resolver live in
  `render` — the GUI keeps its no-extra-state idiom.
- It inherits the strip's suppression list (`main.rs:8338`) **and adds the
  viewer**: while the viewer owns the keyboard the panel describes the viewer
  resolver, not the pane one.
- Pay the documented debt in the same task: the viewer's `flash`
  (`main.rs:3688-3693`, `3724-3731`) is not painted with the viewer open, so
  an unavailable key in the viewer says nothing at all. Paint it. Both
  comments name K3; delete them when they stop being true.

Tests: `just gui-ci` (the GUI is out of the workspace); a unit test over the
row builder for the viewer resolver, and one that the flash reaches the
viewer's render path.

**Verify:** `just gui-ci`.

---

# K3b — the reference sheet

## Task b1 — one row builder, and the CLI page

**Files:** `crates/norte-frontend/src/keysheet.rs` (new),
`crates/norte-cli/src/help.rs`.

```rust
/// One row of the keyboard reference sheet: what the key IS, not what the
/// preset wished it were.
pub struct SheetRow {
    pub screen: Screen,
    pub chord: String,       // painted, `paint_chord`
    pub command: &'static str | String,
    pub avail: Availability, // Here | NotBuilt { reason, issue } | NotHere
}
pub fn sheet(effectives: &[(Screen, Effective)]) -> Vec<SheetRow>
```

- Built on `bindings_all()` (`effective.rs:462`) — its first production
  caller, so its shape finally gets tested against a renderer.
- Order: screen order as today (browse, viewer, dialog), then the effective's
  own precedence order. Available and unavailable rows are **interleaved in
  key order, not segregated**: the sheet answers "what does this key do", and
  a user scanning F-keys must find `alt+f5` where it belongs, greyed, saying
  "pack — not built yet (#132)".
- Label lookup stays per-surface (Fluent id derivation is already shared).

CLI (`keys_page`, `help.rs:415`): iterate `sheet(...)`, and suffix an
unavailable row with the short reason and `#issue`. `CliChords::availability`
(`help.rs:207`) keeps returning `norte_help::Availability::Available` — that
is the *runtime* question, which the CLI genuinely cannot answer; the keymap
availability arrives through `SheetRow`, and the doc comment at `help.rs:57`
must be extended to say which of the two questions the page now answers.

`CliChords::build` (`help.rs:112`) currently **filters `Planned` commands out
of `known`** so the page cannot print them (`help.rs:~140`, the MAJOR-4
comment). That filter is exactly what K3b reverses: keep the filter's intent —
never present an unbuilt command as a working shortcut — by rendering it as
unavailable rather than by hiding it. Update that comment; it is load-bearing
documentation of a real past bug.

**`--json` (`help.rs:546`, `JSON_VERSION = 1`)**: the keys topic gains rows
that were never emitted before and each row gains an availability object.
Bump to `2` and note the change where the constant is defined — a consumer
that keyed on "every listed key works" would silently start reading keys that
do not.

Tests: a preset with a `Planned` binding produces a row with `NotBuilt` and
the right issue; the row is not silently dropped; the JSON version bump has a
golden.

**Verify:** `just t norte-frontend`, `just t norte-cli`, `just c`.

## Task b2 — the TUI and GUI sheets

**Files:** `crates/norte-tui/src/help.rs`, `crates/norte-gui/src/help_view.rs`.

- Both `build`/`keys_lines` become renderers over `keysheet::sheet` rows:
  padding and styling stay theirs, the data stops being theirs.
- Unavailable rows are dimmed (TUI: the theme's dim style; GUI: the muted
  colour already used for disabled palette rows) and carry the reason.
- The GUI's sheet gains the **dialog** section it never had — that omission is
  a bug the shared builder fixes for free, and it should be called out in the
  commit rather than slipped in.

Tests: TUI render test showing a greyed unavailable row; GUI test that the
dialog section is present; both assert the same row count as
`keysheet::sheet`.

**Verify:** `just t norte-tui`, `just gui-ci`.

---

# K3c — the shortcut editor

The largest of the three, and the only one that writes to disk. Four tasks.

## Task c1 — a writer for `keymap.toml`

**File:** `crates/norte-config/src/load.rs`.

- Parameterise the file name: `write_user_toml` (`load.rs:279`) and
  `lock_user_toml` (`load.rs:254`) currently hardcode `norte.toml.tmp` /
  `norte.toml.lock`. Give both the file name and derive the sibling names from
  it. **A `keymap.toml` write must never take `norte.toml`'s lock** — that is
  the whole risk of reusing them as-is.
- New `persist_keymap_append(dir, section, chords: &[String], command) -> Result<…>`:
  appends `{ on = [...], run = "..." }` to `[<section>] append_keymap`,
  creating the file, the section and the array as needed, preserving comments
  and formatting via `toml_edit` exactly as the existing family does.
  A third value shape after `persist_set`'s scalar and `persist_columns`'
  array-of-strings; it lives beside them.
- Idempotence: appending a binding that is already present (same chords, same
  command) rewrites nothing and says so, so a double-confirm cannot double-write.
- Removal is in scope too — `persist_keymap_remove` — because an editor that
  can only add is an editor that cannot fix a mistake.

Tests: creates the file; appends into an existing section keeping comments;
refuses a non-table section like `persist_set` does; the lock file is
`keymap.toml.lock` and not `norte.toml.lock`; idempotent append; remove.

**Verify:** `just t norte-config`, `just c`.

## Task c2 — "what does this key already do?"

**File:** `crates/norte-frontend/src/keymap/effective.rs` + a new
`rebind.rs`.

`Effective::single_chord_runs` answers "does this chord run X"; the editor
needs the reverse and the refusals:

```rust
/// What `seq` would collide with, if the user bound it right now.
pub enum Rebind {
    Free,
    Replaces { command: String, avail: Availability },
    /// `seq` is a prefix of, or extends, an existing sequence — ADR 0006's
    /// prefix-free rule, which is a LOAD error, so the editor must refuse
    /// before writing rather than produce a config that will not load.
    PrefixClash { with: String, command: String },
    /// Sacred (`tab` in Browse, spec §12, K2a's `SacredKey`).
    Sacred,
    /// A digit while the active preset has `counts` (K2a's load rule).
    DigitWithCounts,
}
pub fn rebind_check(eff: &Effective, seq: &[Chord], counts: bool) -> Rebind
```

Pure, in the engine, tested there — the two frontends must not each decide
what a collision is. `Replaces` carries the availability so the editor can say
"replaces `pane.pack`, which is not built yet".

Tests: one per variant, plus the case where the chord is bound in another
screen only (not a collision).

**Verify:** `just t norte-frontend`, `just c`.

## Task c3 — the TUI editor

**Files:** `crates/norte-tui/src/{app,ui,main}.rs`,
`crates/norte-frontend/src/shortcuts.rs` (new, shared state model).

- A **Shortcuts screen**, opened from Settings, listing K3b's rows plus every
  Live catalogue command that has no key (the sheet answers "what does this
  key do"; the editor also has to answer "how do I press X", and an unbound
  command must be visible to be bindable).
- Enter on a row enters **capture mode**: the next chord is read raw, with the
  overlay's own key handling suspended — the TUI's `on_settings_key`
  (`main.rs:4598`) hardcodes its keys, so capture needs an explicit mode, not
  a new branch in that match.
- The captured chord is run through `rebind_check` and the verdict is shown
  BEFORE confirming: free, replaces X, refused (sacred / prefix clash /
  digit). Refusals cannot be confirmed at all.
- Confirm writes through `persist_keymap_append`; the existing `keymap.toml`
  watcher and `reload_config` (`main.rs:5303`) apply it live. **Test that
  path end to end**, because `reload_config` reverts everything on any error:
  a write that produces an invalid layer would silently keep the old map, and
  the user would see their new key do nothing.
- `Esc` in capture mode cancels; the one chord the editor cannot capture is
  the one that cancels it, and the screen says so.
- The TUI cannot see Cmd (`keymap.rs:27`): the screen states that `mod+`
  resolves to Ctrl here, rather than letting a user capture something the
  terminal will never deliver.

**Verify:** `just t norte-tui`, `just c`.

## Task c4 — the GUI editor

**Files:** `crates/norte-gui/src/main.rs`.

Same shared model, GUI capture. Note `on_settings_key`
(`main.rs:2285-2287`) returns early on `control | alt | platform`, so capture
mode must bypass that gate deliberately and narrowly. A chord captured in the
GUI may be unreachable in the TUI (Cmd); the row says so at capture time
rather than after the fact.

**Verify:** `just gui-ci`.

---

## Reviewers

Dispatched by the agent doing the work, before committing:

- a1, b1, c2: `rust-reviewer` (new public engine API, and in b1 a versioned
  JSON contract change).
- c1: `rust-reviewer` **and** `security-reviewer` — it is a new file writer
  with a lock, a tmp file and a rename; the failure mode is a corrupted or
  cross-locked user config.
- c3, c4: `rust-reviewer`, with the specific question of whether a captured
  chord can escape `rebind_check` on its way to disk.

## Out of scope

Timers of any kind (a which-key that waits is timing-dependent resolution),
importing another program's config, per-OS overrides beyond `mod+`, and a
which-key for the bare-count state.
