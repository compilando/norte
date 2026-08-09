# K1 — Shared command catalogue and declared availability: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a keymap binding to a command this build does not run a *declared,
explained* state instead of a load error or a silent disappearance, so the
Total Commander / Krusader / Norton / Far presets of K2 can exist at all.

**Architecture:** The command vocabulary moves out of the two frontends into one
shared catalogue in `norte-frontend`, where each command is `Live` or
`Planned { reason, issue }`. The effective keymap keeps unavailable bindings
instead of dropping them, tagged with why; the resolver returns a third outcome,
`Unavailable`, and both frontends say it out loud. `Effective::build_for`'s
signature does not change, so the ~60 existing call sites keep compiling, and
`build_for_subset` — the silent filter that already cost us a dead F1 in the GUI
— is deleted.

**Tech Stack:** Rust, `norte-frontend` (pure, no crossterm/gpui), `norte-tui`,
`norte-gui`, `norte-cli`, Fluent via `norte-i18n`, nextest.

**Spec:** `docs/superpowers/specs/2026-08-09-keymap-catalogue-and-presets-design.md`

---

## Gate budget for this plan

Per CLAUDE.md, the gate is billed per plan:

- **Every task:** `just t norte-frontend` and `just t norte-tui` only.
- **After Task 3:** one `just ci-fast`.
- **After Task 6:** one `just ci`, then one `just gui-ci`.
- A red gate is never re-run to check a fix. Reproduce with `just t <crate>`.

`norte-gui` is **excluded from `just ci`** (`core_pkgs` in the justfile, because
GPUI turns on `serde_json/preserve_order` and contaminates the core's goldens).
GUI work is verified with `just gui-ci`, and tasks that touch the GUI say so.

---

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-frontend/src/keymap/mod.rs` | public surface; re-exports so every existing `use norte_frontend::keymap::X` keeps working |
| `crates/norte-frontend/src/keymap/chord.rs` | `KeyCode`, `Mods`, `Chord`, `parse_chord`, `paint_chord`, the `mod+` alias |
| `crates/norte-frontend/src/keymap/catalogue.rs` | `CommandDef`, `Status`, `CATALOGUE`, `lookup` — the single command vocabulary |
| `crates/norte-frontend/src/keymap/layer.rs` | `KeymapFile`, `RawSection`, `RawBinding`, `parse_keymap`, `merge_ctx`, `check_layer_keys` |
| `crates/norte-frontend/src/keymap/effective.rs` | `Effective`, `Binding`, `Availability`, `check_binding`, `check_prefix_free`, the builders |
| `crates/norte-frontend/src/keymap/resolve.rs` | `Resolver`, `Resolution`, `Lookup` |
| `crates/norte-frontend/src/keymap/presets.rs` | the `presets` module and `preset_commands` |
| `crates/norte-tui/src/keymap.rs` | narrows to: which catalogue entries the TUI implements |
| `crates/norte-gui/src/keymap.rs` | narrows to: which catalogue entries the GUI implements |
| `crates/norte-i18n/i18n/{en,es}.ftl` | the two messages an unavailable key prints |

---

### Task 1: Split `keymap.rs` into a module directory

`crates/norte-frontend/src/keymap.rs` is 2 220 lines doing seven things, and
this plan adds three more. Split first, so every later diff is readable.

**This task is a pure move: no behaviour changes, no new tests.** TDD does not
apply because the existing suite *is* the test — it must stay green without a
single test edit. If a test needs changing, the move was not pure and you have
made a mistake.

**Files:**
- Create: `crates/norte-frontend/src/keymap/mod.rs`
- Create: `crates/norte-frontend/src/keymap/chord.rs`
- Create: `crates/norte-frontend/src/keymap/layer.rs`
- Create: `crates/norte-frontend/src/keymap/effective.rs`
- Create: `crates/norte-frontend/src/keymap/resolve.rs`
- Create: `crates/norte-frontend/src/keymap/presets.rs`
- Delete: `crates/norte-frontend/src/keymap.rs`

- [ ] **Step 1: Record the green baseline**

```bash
just t norte-frontend 2>&1 | tail -5
just t norte-tui 2>&1 | tail -5
```

Expected: both pass. Write down the two test counts — they must be identical at
the end of this task.

- [ ] **Step 2: Create the directory and move the file**

```bash
mkdir -p crates/norte-frontend/src/keymap
git mv crates/norte-frontend/src/keymap.rs crates/norte-frontend/src/keymap/mod.rs
```

- [ ] **Step 3: Move the chord layer into `chord.rs`**

Cut from `mod.rs` into `crates/norte-frontend/src/keymap/chord.rs`: `KeyCode`,
`Mods`, `Chord` (struct, `impl`, `impl Display`), `paint_chord`, `pretty_token`,
`parse_chord`. Head the new file with:

```rust
//! The neutral key: `KeyCode`, `Mods`, `Chord`, and the TOML spelling of a
//! chord. No crossterm, no gpui — each frontend converts its native event
//! with `Chord::new`.

use super::{KeymapError};
```

`Chord`'s fields are private and `effective.rs` needs `is_bare_esc`, so change
`fn is_bare_esc` to `pub(super) fn is_bare_esc`. `parse_chord` builds
`Chord { mods, code }` directly (it must not go through `Chord::new`, which
would drop the shift before the `ShiftWithChar` check), so that construction
stays inside `chord.rs` — do not move it out.

- [ ] **Step 4: Move the layer/TOML types into `layer.rs`**

Cut into `crates/norte-frontend/src/keymap/layer.rs`: `RawBinding`,
`RawSection`, `KeymapFile` (+ `impl`), `Screen`, `toml_diag`, `parse_keymap`,
`merge_ctx`, `Origin`, `check_layer_keys`, `merged_bindings`. Head it with:

```rust
//! `keymap.toml` as data: the raw binding lists, the layer file, and the
//! per-context merge order (ADR 0006/0007).

use serde::Deserialize;

use super::{KeymapError, chord::parse_chord};
```

`RawBinding`, `RawSection` and `Origin` are private today; the builders in
`effective.rs` need them, so mark each `pub(super)` along with their fields
(`pub(super) on`, `pub(super) run`, `pub(super) keymap`,
`pub(super) prepend_keymap`, `pub(super) append_keymap`). `KeymapFile`'s
`global`/`pane`/`viewer`/`dialog` fields likewise become `pub(super)`.

- [ ] **Step 5: Move the builders into `effective.rs`**

Cut into `crates/norte-frontend/src/keymap/effective.rs`: `Effective`,
`Strictness`, `valid_lua_name`, `check_binding`, `check_prefix_free`, the whole
`impl Effective`, `Lookup`. Head it with:

```rust
//! The EFFECTIVE keymap: layers and contexts merged, validated prefix-free at
//! load time (ADR 0006), immutable afterwards.

use std::collections::HashSet;

use super::chord::Chord;
use super::layer::{KeymapFile, Origin, RawBinding, Screen, check_layer_keys, merged_bindings};
use super::{KeymapError, KeymapDiagnostic};
```

- [ ] **Step 6: Move the resolver into `resolve.rs`**

Cut into `crates/norte-frontend/src/keymap/resolve.rs`: `Resolution`,
`Resolver`. `Lookup` stays in `effective.rs` (it is the return of
`Effective::lookup`); mark `Effective::lookup` and `enum Lookup` `pub(super)`.
Head it with:

```rust
//! Resolution state for ONE in-flight sequence. Owns its effective keymap:
//! hot-reload (ADR 0007) builds a new one and swaps the resolver whole.

use super::chord::Chord;
use super::effective::{Effective, Lookup};
```

- [ ] **Step 7: Move the presets into `presets.rs`**

The file `keymap/presets.rs` **is** the `presets` module — declared in `mod.rs`
as `pub mod presets;` — so `norte_frontend::keymap::presets::ORTHODOX` resolves
exactly as it does today, with no re-export gymnastics.

Cut into it the BODY of the old `pub mod presets { ... }` (the three
`include_str!` consts, `NAMES`, `source`) and its test module
`presets_catalog_tests`. Drop the now-redundant `pub mod presets {` wrapper and
its closing brace. The `include_str!` paths gain one level:
`include_str!("../../presets/keymap/orthodox.toml")`. Head the file with:

```rust
//! The embedded factory presets: sources, the name catalogue, and the lookup
//! each frontend uses for its "known preset" list.
```

`preset_commands` and its test module `preset_commands_tests` do **not** move
here — they are one function whose callers import it as
`keymap::preset_commands`. They stay in `mod.rs` (Step 8).

- [ ] **Step 8: Write `mod.rs`'s remaining content**

What stays in `mod.rs`: the crate-level `//!` docs (moved verbatim from the old
file head), `KeymapError`, `KeymapDiagnostic`, `preset_commands` +
`preset_commands_tests`, the big `mod tests` block, the module declarations and
the re-exports:

```rust
mod chord;
mod effective;
mod layer;
pub mod presets;
mod resolve;

pub use chord::{Chord, KeyCode, Mods, paint_chord, parse_chord};
pub use effective::{Effective, valid_lua_name};
pub use layer::{KeymapFile, Screen, parse_keymap};
pub use resolve::{Resolution, Resolver};
```

The `mod tests` block uses `super::*`, which now resolves through these
re-exports. Where a test reaches a private item (for example `RawBinding`), add
the specific `use super::layer::RawBinding;` inside the test module rather than
widening any visibility.

- [ ] **Step 9: Verify the move changed nothing**

```bash
just t norte-frontend 2>&1 | tail -5
just t norte-tui 2>&1 | tail -5
just c 2>&1 | tail -5
```

Expected: identical test counts to Step 1, clippy clean. If a test file outside
`crates/norte-frontend/` needed an edit, revert it — the public surface must be
byte-identical.

- [ ] **Step 10: Commit**

```bash
git add crates/norte-frontend/src/keymap crates/norte-frontend/src/keymap.rs
git commit -m "refactor(frontend): the keymap engine becomes six files, unchanged

2220 lines doing seven things, about to do ten. Pure move: no test
in the suite changed, and the public surface re-exports identically."
```

---

### Task 2: The shared catalogue

One table naming every command either frontend knows, each `Live` or
`Planned { reason, issue }`. It replaces nothing yet — Task 3 makes the engine
consult it. This task only creates it and pins it against both frontends.

**Files:**
- Create: `crates/norte-frontend/src/keymap/catalogue.rs`
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (declare + re-export)
- Modify: `crates/norte-tui/src/keymap.rs` (add the cross-check test)
- Modify: `crates/norte-gui/src/keymap.rs` (add the cross-check test)

- [ ] **Step 1: Read the issue the first `Planned` entry points at**

The catalogue needs at least one real `Planned` entry, and a planned entry with
an invented issue number is exactly the dishonesty this task exists to remove.
The issue already exists — **#131, "Volume enumeration: no command answers
Alt+F1"** — opened when this plan was written. Confirm it is open:

```bash
gh issue view 131 | head -5
```

Expected: state OPEN. If it is closed, volume enumeration has landed and
`pane.select-drive` should be `live(...)` instead — in which case pick another
genuinely unbuilt command from the K2 preset tables for the `Planned` case,
open its issue, and use that.

- [ ] **Step 2: Write the failing self-consistency test**

Create `crates/norte-frontend/src/keymap/catalogue.rs` containing ONLY the test
module for now, so it fails to compile against types that do not exist yet:

```rust
#[cfg(test)]
mod tests {
    use super::{CATALOGUE, Status, lookup};

    /// A duplicated name would make `lookup` order-dependent, and the table is
    /// hand-maintained: pin it.
    #[test]
    fn no_hay_nombres_duplicados() {
        let mut names: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "nombre duplicado en CATALOGUE");
    }

    /// A `Planned` entry with an empty reason or a zero issue is a promise
    /// nobody can chase — the exact failure this state exists to prevent.
    #[test]
    fn todo_planned_tiene_motivo_e_issue() {
        for d in CATALOGUE {
            if let Status::Planned { reason, issue } = d.status {
                assert!(!reason.is_empty(), "{} sin motivo", d.name);
                assert!(issue > 0, "{} sin issue", d.name);
            }
        }
    }

    #[test]
    fn lookup_encuentra_y_falla_bien() {
        assert!(lookup("pane.copy").is_some());
        assert!(lookup("pane.no-existe-jamas").is_none());
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — compile error, `cannot find type CATALOGUE / Status` and
`file not found for module` until Step 5 declares it.

- [ ] **Step 4: Write the catalogue**

Above the test module in `catalogue.rs`:

```rust
//! The command vocabulary, shared. Before this table each frontend owned a
//! private `COMMANDS` list and passed it to the engine as `known_commands`, so
//! the same preset resolved differently in the TUI and the GUI, silently —
//! that is how F1 did nothing in the GUI for several releases (see the H3f
//! comment in `norte-gui/src/keymap.rs`).
//!
//! A frontend still declares WHICH of these it implements. What it no longer
//! does is decide which names EXIST.

/// One command in the shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDef {
    /// Stable name, e.g. `pane.copy` — what a preset binds, the palette shows
    /// and `help_id` mangles into a Fluent id.
    pub name: &'static str,
    /// Whether a numeric count prefix means anything here (K2 consumes it:
    /// `5j` moves five, `5` before `app.quit` is nonsense). Declared with the
    /// command because that is where the answer is known.
    pub counts: bool,
    /// Why a binding to this name may resolve to nothing.
    pub status: Status,
}

/// Whether norte has built this command at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// At least one frontend implements it. WHICH ones is not this table's
    /// business — each frontend declares its own set.
    Live,
    /// A preset may legitimately bind it; norte has not built it yet. Carries
    /// the reason a user is owed and the issue that tracks it.
    Planned {
        /// Short, user-facing: "volume enumeration".
        reason: &'static str,
        /// The GitHub issue. Never zero, never invented — pinned by test.
        issue: u32,
    },
}

const fn live(name: &'static str, counts: bool) -> CommandDef {
    CommandDef {
        name,
        counts,
        status: Status::Live,
    }
}

const fn planned(name: &'static str, reason: &'static str, issue: u32) -> CommandDef {
    CommandDef {
        name,
        counts: false,
        status: Status::Planned { reason, issue },
    }
}

/// Every command either frontend knows, plus the ones a preset may honestly
/// bind before norte builds them.
pub const CATALOGUE: &[CommandDef] = &[
    // --- app ---
    live("app.quit", false),
    live("app.help", false),
    live("app.theme", false),
    live("app.settings", false),
    live("app.extensions", false),
    live("app.palette", false),
    // --- pane ---
    live("pane.switch", false),
    live("pane.mirror", false),
    live("pane.pull", false),
    live("pane.swap", false),
    live("pane.copy", false),
    live("pane.move", false),
    live("pane.delete", false),
    live("pane.delete-permanent", false),
    live("pane.mkdir", false),
    live("pane.rename", false),
    live("pane.refresh", false),
    live("pane.view", false),
    live("pane.open", false),
    live("pane.quick-search", false),
    live("pane.history", false),
    live("pane.hotlist", false),
    live("pane.search", false),
    live("pane.names-encoding", false),
    live("pane.toggle-hidden", false),
    live("pane.columns", false),
    live("pane.ai-rename", false),
    live("pane.semantic-search", false),
    live("pane.copy-path", false),
    // --- cursor (the count-aware family) ---
    live("cursor.up", true),
    live("cursor.down", true),
    live("cursor.page-up", true),
    live("cursor.page-down", true),
    live("cursor.top", false),
    live("cursor.bottom", false),
    // --- nav ---
    live("nav.enter", false),
    live("nav.parent", false),
    live("nav.back", true),
    live("nav.forward", true),
    // --- mark ---
    live("mark.toggle", false),
    live("mark.all", false),
    live("mark.invert", false),
    live("mark.clear", false),
    live("mark.pattern-add", false),
    live("mark.pattern-remove", false),
    // --- task ---
    live("task.cancel", false),
    live("task.next", false),
    live("task.prev", false),
    live("task.dismiss", false),
    // --- viewer ---
    live("viewer.close", false),
    live("viewer.up", true),
    live("viewer.down", true),
    live("viewer.page-up", true),
    live("viewer.page-down", true),
    live("viewer.top", false),
    live("viewer.bottom", false),
    live("viewer.encoding", false),
    live("viewer.encoding-auto", false),
    live("viewer.hex", false),
    // --- dialog ---
    live("dialog.confirm", false),
    live("dialog.cancel", false),
    live("dialog.approve", false),
    live("dialog.deny", false),
    live("dialog.overwrite", false),
    live("dialog.skip", false),
    live("dialog.rename", false),
    live("dialog.newer", false),
    live("dialog.up", true),
    live("dialog.down", true),
    live("dialog.page-up", true),
    live("dialog.page-down", true),
    live("dialog.add", false),
    live("dialog.toggle-enabled", false),
    live("dialog.remove", false),
    live("dialog.move-up", false),
    live("dialog.move-down", false),
    live("dialog.sort", false),
    live("dialog.cycle-format", false),
    live("dialog.pane", false),
    live("dialog.back", false),
    live("dialog.filter", false),
    // --- planned: named by a preset, not built yet ---
    planned("pane.select-drive", "volume enumeration", 131),
];

/// The entry for `name`, or `None` if the vocabulary has never heard of it —
/// which is a typo, and Task 3 keeps failing the load on it.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static CommandDef> {
    CATALOGUE.iter().find(|d| d.name == name)
}
```

- [ ] **Step 5: Declare and re-export the module**

In `crates/norte-frontend/src/keymap/mod.rs`, next to the other `mod` lines:

```rust
pub mod catalogue;
```

and next to the other re-exports:

```rust
pub use catalogue::{CATALOGUE, CommandDef, Status};
```

- [ ] **Step 6: Run the catalogue's own tests**

```bash
just t norte-frontend 2>&1 | tail -10
```

Expected: PASS, including the three new tests.

- [ ] **Step 7: Write the TUI cross-check test**

At the end of `crates/norte-tui/src/keymap.rs`, inside its existing
`#[cfg(test)] mod tests` block (or a new one if it has none at file scope):

```rust
/// The TUI's `COMMANDS`/`DIALOG_COMMANDS` are now a SUBSET declaration, not a
/// vocabulary. A name the shared catalogue has never heard of means the two
/// have drifted — which is the whole class of bug this catalogue removes.
#[test]
fn todo_comando_del_tui_esta_en_el_catalogo_compartido() {
    use norte_frontend::keymap::catalogue::{Status, lookup};
    for name in COMMANDS.iter().chain(DIALOG_COMMANDS.iter()) {
        let def = lookup(name)
            .unwrap_or_else(|| panic!("{name} lo implementa el TUI y no está en CATALOGUE"));
        assert_eq!(
            def.status,
            Status::Live,
            "{name} lo implementa el TUI pero el catálogo lo declara Planned"
        );
    }
}
```

- [ ] **Step 8: Run it**

```bash
just t norte-tui 2>&1 | tail -10
```

Expected: PASS. If a name is missing, add it to `CATALOGUE` as `live(name,
false)` — do not weaken the test.

- [ ] **Step 9: Write the GUI cross-check test**

At the end of `crates/norte-gui/src/keymap.rs`, inside its existing test module:

```rust
/// Same pin as the TUI's: the GUI declares a SUBSET of the shared vocabulary,
/// never its own. `COMMANDS` and `VIEWER_COMMANDS` are that subset.
#[test]
fn todo_comando_de_la_gui_esta_en_el_catalogo_compartido() {
    use norte_frontend::keymap::catalogue::{Status, lookup};
    for name in COMMANDS.iter().chain(VIEWER_COMMANDS.iter()) {
        let def = lookup(name)
            .unwrap_or_else(|| panic!("{name} lo implementa la GUI y no está en CATALOGUE"));
        assert_eq!(
            def.status,
            Status::Live,
            "{name} lo implementa la GUI pero el catálogo lo declara Planned"
        );
    }
}
```

- [ ] **Step 10: Run the GUI test**

The GUI is outside `just ci`, so it has its own recipe:

```bash
just gui-ci 2>&1 | tail -15
```

Expected: PASS. If `just gui-ci` does not exist under that name, run
`just --list | grep gui` and use what it prints.

- [ ] **Step 11: Commit**

```bash
git add crates/norte-frontend/src/keymap/catalogue.rs \
        crates/norte-frontend/src/keymap/mod.rs \
        crates/norte-tui/src/keymap.rs \
        crates/norte-gui/src/keymap.rs
git commit -m "feat(frontend): one command vocabulary, and a Planned state for what is not built

The TUI and the GUI each owned a private COMMANDS list and handed it to
the engine as known_commands, so the same preset resolved differently in
each — silently. That is how F1 did nothing in the GUI for releases.

The catalogue is now shared and each frontend declares only WHICH entries
it implements, pinned by a test on both sides. Planned{reason,issue} is
what will let a faithful Total Commander preset bind Alt+F1 to a drive
selector norte has not built."
```

---

### Task 3: Availability replaces the silent filter

The engine consults the catalogue. A binding to a command this frontend does
not run stops disappearing and starts being kept, tagged.

**Files:**
- Modify: `crates/norte-frontend/src/keymap/effective.rs`
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (re-export `Availability`)
- Modify: `crates/norte-gui/src/keymap.rs:333,334,365,367,453`
- Modify: `crates/norte-cli/src/help.rs:132`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/norte-frontend/src/keymap/mod.rs`:

```rust
/// A preset that binds a Planned command LOADS, and the binding survives
/// carrying its reason. Without this a faithful Total Commander preset
/// cannot exist: a third of it names commands norte has not built.
#[test]
fn un_binding_a_comando_planned_sobrevive_marcado() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
    let all = eff.bindings_all();
    let (_, run, avail) = all
        .iter()
        .find(|(_, run, _)| *run == "pane.select-drive")
        .expect("el binding no puede desaparecer");
    assert_eq!(*run, "pane.select-drive");
    assert!(
        matches!(avail, Availability::NotBuilt { .. }),
        "{avail:?}"
    );
}

/// A command the catalogue calls Live but THIS frontend does not implement is
/// kept as NotHere instead of being filtered away in silence — the H3f bug.
#[test]
fn un_comando_live_que_este_frontend_no_implementa_es_not_here() {
    let preset = parse_keymap(
        r#"
[global]
keymap = [ { on = ["f1"], run = "app.help" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
    let all = eff.bindings_all();
    let (_, _, avail) = all
        .iter()
        .find(|(_, run, _)| *run == "app.help")
        .expect("no puede desaparecer");
    assert_eq!(*avail, Availability::NotHere);
}

/// `bindings()` keeps its old meaning — only what actually runs — so the help
/// and the hints render exactly as before this change.
#[test]
fn bindings_solo_devuelve_lo_ejecutable() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [
    { on = ["f5"], run = "pane.copy" },
    { on = ["alt+f1"], run = "pane.select-drive" },
]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
    let runs: Vec<&str> = eff.bindings().into_iter().map(|(_, run)| run).collect();
    assert_eq!(runs, vec!["pane.copy"]);
}

/// A name absent from the CATALOGUE is a typo and still dies loudly, in a
/// preset and in a user layer alike. "Not built yet" and "you misspelled it"
/// stop being the same event; they must not become the same event again.
#[test]
fn un_nombre_fuera_del_catalogo_sigue_siendo_error() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["f5"], run = "pane.copyy" } ]
"#,
    )
    .unwrap();
    let e = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::UnknownCommand { .. }), "{e:?}");
}

/// An unavailable binding SHADOWS a lower-precedence available one. If a
/// preset puts `alt+f1` in `[pane]`, the key must say "drives are not built"
/// rather than quietly falling through to whatever `[global]` had — falling
/// through is how a Total Commander user gets a surprise instead of an answer.
#[test]
fn un_binding_no_disponible_ensombrece_al_de_global() {
    let preset = parse_keymap(
        r#"
[global]
keymap = [ { on = ["alt+f1"], run = "pane.refresh" } ]

[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["pane.refresh"], Screen::Browse).unwrap();
    let all = eff.bindings_all();
    let hits: Vec<_> = all.iter().filter(|(seq, _, _)| seq == "alt+f1").collect();
    assert_eq!(hits.len(), 1, "el dedup deja UNA por secuencia: {all:?}");
    assert_eq!(hits[0].1, "pane.select-drive", "gana el contexto específico");
    assert!(matches!(hits[0].2, Availability::NotBuilt { .. }));
}

/// Unavailable bindings take part in the prefix-free check: the shape of the
/// map is a load-time property (ADR 0006), independent of what runs.
#[test]
fn un_binding_no_disponible_sigue_contando_para_prefix_free() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [
    { on = ["g"], run = "pane.select-drive" },
    { on = ["g", "g"], run = "cursor.top" },
]
"#,
    )
    .unwrap();
    let e = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse).unwrap_err();
    assert!(matches!(e, KeymapError::AmbiguousPrefix { .. }), "{e:?}");
}
```

- [ ] **Step 2: Run them to verify they fail**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — `cannot find type Availability`, `no method bindings_all`.

- [ ] **Step 3: Add `Availability` and thread it through the effective map**

In `crates/norte-frontend/src/keymap/effective.rs`, add above `Effective`:

```rust
/// Why a bound key may not run anything here. Kept ON the binding instead of
/// deleting it, so a key can explain itself instead of doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Bound and runnable.
    Here,
    /// In the catalogue as `Status::Planned` — norte has not built it.
    NotBuilt {
        /// User-facing reason, from the catalogue.
        reason: &'static str,
        /// The issue that tracks it.
        issue: u32,
    },
    /// `Status::Live`, but this frontend does not implement it (a TUI-only
    /// command bound while running the GUI, or the reverse).
    NotHere,
}
```

Change `Effective`'s storage from `Vec<(Vec<Chord>, String)>` to a named
struct, so the third field cannot be mixed up with the command:

```rust
#[derive(Debug, Clone)]
pub(super) struct Binding {
    pub(super) seq: Vec<Chord>,
    pub(super) run: String,
    pub(super) avail: Availability,
}

#[derive(Debug, Clone)]
pub struct Effective {
    bindings: Vec<Binding>,
    discarded_lua_bindings: usize,
}
```

- [ ] **Step 4: Rewrite `check_binding` against the catalogue**

Replace the `lua:`/`known_commands` block at the end of `check_binding` and
change its signature — `Strictness` and `Origin` are no longer needed for this
decision, because the decision no longer depends on where the binding came
from:

```rust
fn check_binding(
    raw: &RawBinding,
    known_commands: &[&str],
) -> Result<Binding, KeymapError> {
    let seq: Vec<Chord> = raw
        .on
        .iter()
        .map(|s| parse_chord(s))
        .collect::<Result<_, _>>()?;
    if seq.is_empty() {
        return Err(KeymapError::EmptySequence {
            run: raw.run.clone(),
        });
    }
    if seq.len() > 1 && seq.iter().any(|c| c.is_bare_esc()) {
        return Err(KeymapError::EscInSequence {
            sequence: format!("{:?}", raw.on),
        });
    }
    // `lua:<name>` is registered at RUNTIME, so it is never in the catalogue;
    // only its charset is checked (single source, `valid_lua_name`). An
    // unregistered lua command is not a keymap error — the host reports it
    // when invoked.
    let avail = if let Some(lua_name) = raw.run.strip_prefix("lua:") {
        if !valid_lua_name(lua_name) {
            return Err(KeymapError::UnknownCommand {
                run: raw.run.clone(),
            });
        }
        Availability::Here
    } else if known_commands.contains(&raw.run.as_str()) {
        Availability::Here
    } else {
        // Absent from THIS frontend's set. The catalogue decides whether that
        // is "norte has not built it" or "you misspelled it".
        match super::catalogue::lookup(&raw.run).map(|d| d.status) {
            Some(super::catalogue::Status::Planned { reason, issue }) => {
                Availability::NotBuilt { reason, issue }
            }
            Some(super::catalogue::Status::Live) => Availability::NotHere,
            None => {
                return Err(KeymapError::UnknownCommand {
                    run: raw.run.clone(),
                });
            }
        }
    };
    Ok(Binding {
        seq,
        run: raw.run.clone(),
        avail,
    })
}
```

- [ ] **Step 5: Delete `Strictness` and `build_for_subset`**

Delete `enum Strictness` entirely. Delete `pub fn build_for_subset`. Change
`build_for_impl` to drop its `preset_strictness` parameter and its
`Origin`-based skipping — `merged_bindings` still returns `Origin` for
`merge_ctx`'s lua-discarding, so keep the tuple and ignore the second element
at the call site with `for (raw, _origin) in ordered`. `check_prefix_free`
now takes `&[Binding]`:

```rust
fn check_prefix_free(bindings: &[Binding]) -> Result<(), KeymapError> {
    for (i, a) in bindings.iter().enumerate() {
        for b in bindings.iter().skip(i + 1) {
            let (short, long) = if a.seq.len() < b.seq.len() {
                (&a.seq, &b.seq)
            } else {
                (&b.seq, &a.seq)
            };
            if short.len() < long.len() && long[..short.len()] == short[..] {
                return Err(KeymapError::AmbiguousPrefix {
                    shorter: format!("{short:?}"),
                    longer: format!("{long:?}"),
                });
            }
        }
    }
    Ok(())
}
```

`build_for` keeps its exact signature — `(preset, layers, known_commands,
screen)` — so the ~60 call sites across the TUI and its tests are untouched.

- [ ] **Step 6: Keep `bindings()` honest and add `bindings_all()`**

```rust
    /// The effective bindings that RUN, in precedence order. The help and the
    /// hints are built from this, so its meaning is unchanged by availability:
    /// a key that cannot run is not a key the help should advertise.
    #[must_use]
    pub fn bindings(&self) -> Vec<(String, &str)> {
        self.bindings
            .iter()
            .filter(|b| b.avail == Availability::Here)
            .map(|b| (render_seq(&b.seq), b.run.as_str()))
            .collect()
    }

    /// Every binding, available or not, with why. The reference sheet (K3)
    /// renders the unavailable ones in grey; nothing else should need this.
    #[must_use]
    pub fn bindings_all(&self) -> Vec<(String, &str, Availability)> {
        self.bindings
            .iter()
            .map(|b| (render_seq(&b.seq), b.run.as_str(), b.avail))
            .collect()
    }
```

with the shared helper next to them:

```rust
fn render_seq(seq: &[Chord]) -> String {
    let keys: Vec<String> = seq.iter().map(ToString::to_string).collect();
    keys.join(" ")
}
```

Update `Effective::lookup` to read `b.seq` / `b.run` and to return the
availability too — `resolve.rs` needs it in Task 4, so give `Lookup::Exact`
both fields now:

```rust
pub(super) enum Lookup<'a> {
    Exact(&'a str, Availability),
    Prefix,
    Miss,
}
```

and in `Resolver::push` (in `resolve.rs`), for this task only, keep the old
behaviour by ignoring the second field:

```rust
            Lookup::Exact(run, _avail) => {
                self.pending.clear();
                Resolution::Run(run.to_owned())
            }
```

Also update `build_diagnostics` in the same file: it calls `check_binding` with
the old four-argument signature and collects errors. Give it the new
two-argument call and keep its behaviour (collect every `Err`, keep walking) —
it no longer needs to special-case a lenient skip, because there is no longer
such a thing.

- [ ] **Step 7: Re-export `Availability`**

In `crates/norte-frontend/src/keymap/mod.rs`:

```rust
pub use effective::{Availability, Effective, valid_lua_name};
```

- [ ] **Step 8: Run the frontend tests**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: PASS, including the five new tests.

- [ ] **Step 9: Point the four `build_for_subset` call sites at `build_for`**

In `crates/norte-gui/src/keymap.rs`, lines 333, 334, 365, 367 and 453, and in
`crates/norte-cli/src/help.rs` line 132: replace `Effective::build_for_subset(`
with `Effective::build_for(`. The arguments are identical.

Update the three GUI comments that describe the deleted function (lines ~30,
~219, ~300, ~491) to say what happens now. The one at line 30 is the H3f bug
report and should read:

```rust
    // H3f: the three shared presets have bound `f1` → `app.help` since H3a,
    // but this table did not list it — and the engine's lenient filter
    // dropped the binding, so F1 did nothing, silently. Since K1 there is no
    // lenient filter: a binding this frontend cannot run survives as
    // `Availability::NotHere` and says so when pressed.
```

- [ ] **Step 9b: Retire the two tests that pinned the silent filter**

Task 1's implementer found two existing tests in
`crates/norte-frontend/src/keymap/mod.rs` whose whole job was to pin the
lenient filter's side effects. They now assert the opposite of the decided
behaviour, and they must be **inverted, not deleted** — the behaviour they
describe is still worth pinning, in its new direction:

- `subset_prefijo_filtrado_no_bloquea_secuencia` — pinned that a filtered
  binding does not block a longer sequence that has it as a prefix. Rename it
  to `un_binding_no_disponible_si_bloquea_el_prefijo` and flip the assertion to
  expect `KeymapError::AmbiguousPrefix`. (The new test in Step 1 covers the
  same ground from the other side; keeping both is fine, but do not leave a
  test asserting the old behaviour.)
- `subset_dedup_desenmascara_binding_global` — pinned that filtering a `pane`
  binding unmasks the `global` one it shadowed. Rename it to
  `un_binding_no_disponible_sigue_ensombreciendo` and flip it: the specific
  context still wins, and the surviving binding is the unavailable one.

Both renames carry a comment saying what they used to pin and why the direction
changed, so a future reader does not "fix" them back.

- [ ] **Step 10: Run the TUI and the CLI**

```bash
just t norte-tui 2>&1 | tail -10
just t norte-cli 2>&1 | tail -10
```

Expected: PASS. A TUI test that asserted a preset binding to a GUI-only command
FAILS to build is now wrong and should assert `Availability::NotHere` instead —
that is a real behaviour change, and it is the point of the task.

- [ ] **Step 11: Run the GUI**

```bash
just gui-ci 2>&1 | tail -15
```

Expected: PASS.

- [ ] **Step 12: Commit**

```bash
git add crates/norte-frontend/src/keymap crates/norte-gui/src/keymap.rs crates/norte-cli/src/help.rs
git commit -m "feat(frontend): a binding this build cannot run survives, tagged

build_for_subset filtered such bindings away without a word, which is how
F1 died in the GUI. It is deleted. check_binding now asks the catalogue:
Planned means NotBuilt with a reason and an issue, Live-but-not-here means
NotHere, and absent means UnknownCommand — a typo still dies loudly.

bindings() keeps returning only what runs, so the help and the hints render
exactly as before; bindings_all() is what K3's reference sheet will use."
```

- [ ] **Step 13: The plan's first `ci-fast`**

```bash
just ci-fast 2>&1 | tail -20
```

Expected: EXIT 0. This is one of the two gate runs this plan is allowed. If it
is red, reproduce the single failure with `just t <crate>` — do not re-run
`ci-fast` to check a fix.

---

### Task 4: The resolver says it, and both frontends print it

**Files:**
- Modify: `crates/norte-frontend/src/keymap/resolve.rs`
- Modify: `crates/norte-i18n/i18n/en.ftl`
- Modify: `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-tui/src/main.rs:2906,3033,3113,3292`
- Modify: `crates/norte-gui/src/main.rs:3632,3678`

- [ ] **Step 1: Write the failing resolver test**

In `crates/norte-frontend/src/keymap/mod.rs`'s `mod tests`:

```rust
/// Pressing a key bound to something norte has not built returns a third
/// outcome. `Reset` would be indistinguishable from an unbound key, which is
/// precisely the silence this work exists to remove.
#[test]
fn una_tecla_no_disponible_resuelve_a_unavailable() {
    let preset = parse_keymap(
        r#"
[pane]
keymap = [ { on = ["alt+f1"], run = "pane.select-drive" } ]
"#,
    )
    .unwrap();
    let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
    let mut r = Resolver::new(eff);
    let chord = parse_chord("alt+f1").unwrap();
    match r.push(chord) {
        Resolution::Unavailable { command, why } => {
            assert_eq!(command, "pane.select-drive");
            assert!(matches!(why, Availability::NotBuilt { .. }), "{why:?}");
        }
        other => panic!("esperaba Unavailable, salió {other:?}"),
    }
    assert!(r.pending().is_empty(), "la secuencia debe quedar limpia");
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — `no variant named Unavailable`.

- [ ] **Step 3: Add the variant and return it**

In `crates/norte-frontend/src/keymap/resolve.rs`:

```rust
pub enum Resolution {
    /// Complete sequence: run this command.
    Run(String),
    /// Valid prefix of some sequence: waiting (current depth).
    Pending(usize),
    /// The key IS bound, and what it is bound to cannot run here. The
    /// frontend says so; it never does nothing.
    Unavailable {
        /// The command the key is bound to.
        command: String,
        /// Why it cannot run.
        why: Availability,
    },
    /// No binding (or cancellation): clean state, key discarded.
    Reset,
}
```

and in `push`:

```rust
            Lookup::Exact(run, Availability::Here) => {
                self.pending.clear();
                Resolution::Run(run.to_owned())
            }
            Lookup::Exact(run, why) => {
                self.pending.clear();
                Resolution::Unavailable {
                    command: run.to_owned(),
                    why,
                }
            }
```

Import `Availability` at the top of `resolve.rs`:

```rust
use super::effective::{Availability, Effective, Lookup};
```

- [ ] **Step 4: Run it to verify it passes**

```bash
just t norte-frontend 2>&1 | tail -10
```

Expected: PASS.

- [ ] **Step 5: Add the two Fluent messages**

Append to `crates/norte-i18n/i18n/en.ftl`:

```
keymap-unavailable-not-built = { $command }: not built yet ({ $reason }, issue #{ $issue })
keymap-unavailable-not-here = { $command }: not available in this interface
```

Append to `crates/norte-i18n/i18n/es.ftl`:

```
keymap-unavailable-not-built = { $command }: aún no está construido ({ $reason }, issue #{ $issue })
keymap-unavailable-not-here = { $command }: no está disponible en esta interfaz
```

- [ ] **Step 6: Verify the locale parity test still passes**

```bash
just t norte-i18n 2>&1 | tail -10
```

Expected: PASS. The suite pins that both locales carry the same message ids; a
key added to one only will fail here.

- [ ] **Step 7: Write the message-building helper, with its test**

In `crates/norte-frontend/src/keymap/mod.rs`, after the re-exports:

```rust
/// The user-facing sentence for an unavailable key. Lives here rather than in
/// each frontend so the TUI and the GUI cannot word it differently.
#[must_use]
pub fn unavailable_message(command: &str, why: Availability) -> String {
    match why {
        Availability::Here => String::new(),
        Availability::NotBuilt { reason, issue } => norte_i18n::ta(
            "keymap-unavailable-not-built",
            &[
                ("command", command),
                ("reason", reason),
                ("issue", &issue.to_string()),
            ],
        ),
        Availability::NotHere => {
            norte_i18n::ta("keymap-unavailable-not-here", &[("command", command)])
        }
    }
}
```

and its test in `mod tests`:

```rust
/// The message must NAME the command and, when the reason exists, carry it —
/// a "not available" with no subject is the silence with extra steps.
#[test]
fn el_mensaje_de_no_disponible_nombra_el_comando_y_el_motivo() {
    let m = unavailable_message(
        "pane.select-drive",
        Availability::NotBuilt {
            reason: "volume enumeration",
            issue: 131,
        },
    );
    assert!(m.contains("pane.select-drive"), "{m}");
    assert!(m.contains("volume enumeration"), "{m}");
    assert!(m.contains("131"), "{m}");

    let m = unavailable_message("pane.hotlist", Availability::NotHere);
    assert!(m.contains("pane.hotlist"), "{m}");
}
```

If `norte-frontend` does not already depend on `norte-i18n`, add it to
`crates/norte-frontend/Cargo.toml` under `[dependencies]`:

```toml
norte-i18n.workspace = true
```

- [ ] **Step 8: Run it**

```bash
just t norte-frontend 2>&1 | tail -10
```

Expected: PASS.

- [ ] **Step 9: Make the TUI print it**

`crates/norte-tui/src/main.rs` has four `match … push(chord)` sites: line 2906
(the main loop), 3033, 3113 and 3292 (overlay dispatchers). The main loop's
arm sets the status message; the three overlay dispatchers reset and return, as
they do for `Pending`.

At line 2906's match, after the `Resolution::Run(cmd) => { … }` arm:

```rust
                                Resolution::Unavailable { command, why } => {
                                    app.pending.clear();
                                    app.message = Some(
                                        norte_frontend::keymap::unavailable_message(&command, why),
                                    );
                                }
```

At each of the three overlay matches (3033, 3113, 3292), extend the existing
`Pending` arm's neighbourhood:

```rust
        // A key the overlay's keymap binds to something this build cannot run:
        // same treatment as a pending sequence — reset and ignore. The main
        // loop is where the user gets told; an overlay has no status bar.
        Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
```

- [ ] **Step 10: Run the TUI tests**

```bash
just t norte-tui 2>&1 | tail -10
```

Expected: PASS. A non-exhaustive-match compile error is the expected way to
find any site this step missed — fix each the same way.

- [ ] **Step 11: Make the GUI print it**

`crates/norte-gui/src/main.rs` lines 3632 and 3678. At both, add:

```rust
                    norte_frontend::keymap::Resolution::Unavailable { command, why } => {
                        self.status = Some(norte_frontend::keymap::unavailable_message(
                            &command, why,
                        ));
                    }
```

Use whatever the surrounding code already calls its transient message field; if
the GUI has no such field, set the same one the keymap-error banner uses
(`NorteGui::keymap_error`, referenced at `settings_view.rs:25`).

- [ ] **Step 12: Run the GUI**

```bash
just gui-ci 2>&1 | tail -15
```

Expected: PASS.

- [ ] **Step 13: Commit**

```bash
git add crates/norte-frontend crates/norte-i18n crates/norte-tui/src/main.rs crates/norte-gui/src/main.rs
git commit -m "feat(tui,gui): a key that cannot run says why instead of nothing

Resolution gains Unavailable{command,why}, and the sentence is built once
in norte-frontend so the two frontends cannot word it differently. Both
locales carry the two messages."
```

---

### Task 5: The `mod+` alias

One preset file, Cmd on macOS and Ctrl elsewhere.

**Files:**
- Modify: `crates/norte-frontend/src/keymap/chord.rs`
- Modify: `crates/norte-tui/src/keymap.rs` (crossterm adapter: `cmd: false`)
- Modify: `crates/norte-tui/src/mouse.rs`, `crates/norte-frontend/src/mouse.rs` (`Mods` literals)
- Modify: `crates/norte-gui/src/keymap.rs`, `crates/norte-gui/src/main.rs` (gpui adapter)
- Modify: `crates/norte-tui/tests/keymap.rs` (`Mods` literals)

- [ ] **Step 1: Write the failing tests**

In `crates/norte-frontend/src/keymap/mod.rs`'s `mod tests`:

```rust
/// `mod+` is the one per-OS mechanism: a preset stays a single file. The
/// process picks which physical modifier it means, once, at startup.
#[test]
fn mod_es_ctrl_por_defecto() {
    let c = parse_chord("mod+c").unwrap();
    assert_eq!(c, parse_chord("ctrl+c").unwrap());
}

/// `cmd+` is literal, for a preset that means Cmd and nothing else.
#[test]
fn cmd_es_su_propio_modificador_y_no_es_ctrl() {
    let cmd = parse_chord("cmd+c").unwrap();
    let ctrl = parse_chord("ctrl+c").unwrap();
    assert_ne!(cmd, ctrl);
}

/// The resolution is a pure function of the policy, so it is testable without
/// a macOS machine — which matters, because CI is off and nobody here has one.
#[test]
fn la_politica_decide_a_que_se_traduce_mod() {
    use norte_frontend::keymap::ModKey;
    assert_eq!(
        ModKey::Ctrl.apply(Mods::default()),
        Mods { ctrl: true, ..Mods::default() }
    );
    assert_eq!(
        ModKey::Cmd.apply(Mods::default()),
        Mods { cmd: true, ..Mods::default() }
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

```bash
just t norte-frontend 2>&1 | tail -20
```

Expected: FAIL — `BadChord` for `mod+c` and `cmd+c`, and `cannot find ModKey`.

- [ ] **Step 3: Add the `cmd` modifier**

In `crates/norte-frontend/src/keymap/chord.rs`, extend `Mods`:

```rust
pub struct Mods {
    /// Ctrl.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Cmd / Super / Meta. Only a frontend that can OBSERVE it ever sets it:
    /// the TUI cannot, because crossterm does not report super without
    /// `PushKeyboardEnhancementFlags`, which norte does not enable.
    pub cmd: bool,
}
```

`Mods` derives `Default`, so every construction that already uses
`..Mods::default()` or `Mods::default()` is unaffected. The 20 struct literals
across the tree that spell all three fields will fail to compile; fix each by
adding `cmd: false` — or, where the literal is inside a test that does not care,
by appending `..Mods::default()`. Find them with:

```bash
grep -rn "Mods {" --include="*.rs" crates/ | grep -v "keymap/chord.rs"
```

Add `cmd` to `Display` for `Chord`, before `ctrl` (so `cmd+ctrl+x` has one
canonical spelling):

```rust
        if self.mods.cmd {
            f.write_str("cmd+")?;
        }
```

- [ ] **Step 4: Add `ModKey` and the process policy**

In `chord.rs`:

```rust
/// Which physical modifier `mod+` means in THIS process. Set once at startup
/// by the frontend, before any keymap is built — the same shape as the
/// language in `norte_i18n` (a process-wide platform fact, decided once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    /// `mod+` is Ctrl. The default, and the only honest answer for the TUI on
    /// every platform: crossterm cannot deliver Cmd.
    Ctrl,
    /// `mod+` is Cmd. A frontend that can observe Cmd — the GUI on macOS.
    Cmd,
}

impl ModKey {
    /// `mods` with this policy's bit set. Pure, so the mapping is testable
    /// without the platform it describes.
    #[must_use]
    pub fn apply(self, mods: Mods) -> Mods {
        match self {
            Self::Ctrl => Mods { ctrl: true, ..mods },
            Self::Cmd => Mods { cmd: true, ..mods },
        }
    }
}

static MOD_KEY: std::sync::OnceLock<ModKey> = std::sync::OnceLock::new();

/// Fix what `mod+` means. Call once, at startup, BEFORE building any keymap.
/// Returns `false` if the policy was already fixed to a different value —
/// never panics, never silently changes a keymap that is already resolved.
pub fn set_mod_key(k: ModKey) -> bool {
    *MOD_KEY.get_or_init(|| k) == k
}

/// The active policy; `ModKey::Ctrl` if nobody set one.
#[must_use]
pub fn mod_key() -> ModKey {
    *MOD_KEY.get_or_init(|| ModKey::Ctrl)
}
```

- [ ] **Step 5: Teach `parse_chord` the two tokens**

In `parse_chord`'s modifier loop, replace the `match *m` arms with:

```rust
        let slot = match *m {
            "ctrl" => &mut mods.ctrl,
            "alt" => &mut mods.alt,
            "shift" => &mut mods.shift,
            "cmd" => &mut mods.cmd,
            // `mod` is the alias: whichever physical modifier this process
            // decided at startup. One preset file, two platforms.
            "mod" => match mod_key() {
                ModKey::Ctrl => &mut mods.ctrl,
                ModKey::Cmd => &mut mods.cmd,
            },
            _ => return Err(bad()),
        };
```

- [ ] **Step 6: Re-export and run**

In `mod.rs`:

```rust
pub use chord::{Chord, KeyCode, ModKey, Mods, mod_key, paint_chord, parse_chord, set_mod_key};
```

```bash
just t norte-frontend 2>&1 | tail -10
```

Expected: PASS.

- [ ] **Step 7: Set the policy in the GUI, and only there**

In the GUI's startup path (`crates/norte-gui/src/main.rs`, before the first
keymap build), add:

```rust
    // The GUI can observe Cmd (gpui reports it); the TUI cannot. macOS users
    // of the GUI get Cmd for `mod+`, everyone else gets Ctrl.
    norte_frontend::keymap::set_mod_key(if cfg!(target_os = "macos") {
        norte_frontend::keymap::ModKey::Cmd
    } else {
        norte_frontend::keymap::ModKey::Ctrl
    });
```

Map gpui's `cmd`/`platform` modifier into `Mods.cmd` in the GUI's chord adapter
(the `Mods { … }` literal in `crates/norte-gui/src/keymap.rs`). The TUI needs
no call: the default is already the only truthful answer for it.

- [ ] **Step 8: Run everything this touched**

```bash
just t norte-frontend 2>&1 | tail -5
just t norte-tui 2>&1 | tail -5
just gui-ci 2>&1 | tail -15
```

Expected: all PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/norte-frontend crates/norte-tui crates/norte-gui
git commit -m "feat(frontend): mod+ is Cmd where Cmd exists, Ctrl where it does not

One preset file for both platforms. The policy is process-wide and set
once at startup, like the language in norte-i18n, so parse_chord stays a
pure function of it and the mapping is testable without a mac.

The TUI never sets it: crossterm does not report super without
PushKeyboardEnhancementFlags, so Ctrl is the only truthful answer there."
```

---

### Task 6: Record the decision, then pay the gate once

**Files:**
- Create: `docs/adr/0043-keymap-availability-and-the-mod-alias.md`
- Modify: `docs/adr/0006-keymap-resolution.md` (a "superseded in part" note)
- Modify: `crates/norte-frontend/src/keymap/mod.rs` (module rustdoc pointing at the ADR)

- [ ] **Step 1: Write the ADR**

Use the project command so the numbering and the MADR shape are right:

```
/adr keymap availability and the mod alias
```

Content it must contain, because these are the decisions a future reader will
question:

- Why a binding to an unbuilt command is kept and tagged rather than rejected
  (a faithful preset for another manager is a third unbuilt commands) or
  silently dropped (the H3f dead-F1 bug, cited).
- Why "not in the catalogue" stays a hard load error: a typo and an unbuilt
  command must not be the same event.
- Why unavailable bindings still take part in the prefix-free check: the shape
  of the map is a load-time property, independent of what runs.
- Why the `mod+` policy is process-wide state rather than a builder parameter:
  ~60 call sites, and the same justification `norte_i18n`'s global language
  already uses.
- The honest limitation: the TUI cannot observe Cmd, so `mod+` is Ctrl there on
  every platform, including macOS.

It supersedes ADR 0006 in part — 0006's `known_commands` validation rule — and
must say so.

- [ ] **Step 2: Note it in ADR 0006**

At the top of `docs/adr/0006-keymap-resolution.md`, under `- Status: accepted`:

```markdown
- Superseded in part by [0043](0043-keymap-availability-and-the-mod-alias.md):
  a binding to a command this build does not implement is no longer a load
  error, it is a declared unavailability. Everything else here stands.
```

- [ ] **Step 3: Point the module rustdoc at it**

In `crates/norte-frontend/src/keymap/mod.rs`'s `//!` header, extend the ADR
reference from `(ADR 0006/0007)` to `(ADR 0006/0007/0043)`.

- [ ] **Step 4: Commit the docs**

```bash
git add docs/adr crates/norte-frontend/src/keymap/mod.rs
git commit -m "docs(adr): 0043 — availability is declared, and mod+ is a process policy"
```

- [ ] **Step 5: The plan's second and last gate run**

```bash
just ci 2>&1 | tail -30
```

Expected: EXIT 0. Coverage must stay at or above 85% on proto/vfs/core — this
plan does not touch those three crates, so it cannot move, but the gate checks
it anyway.

- [ ] **Step 6: The GUI gate**

```bash
just gui-ci 2>&1 | tail -20
```

Expected: EXIT 0.

- [ ] **Step 7: Dispatch the reviewers before merging**

Per CLAUDE.md, the agent that did the work dispatches its own reviewers and
reports with the findings already applied. For this surface:

- `rust-reviewer` — substantial Rust diff across three crates.
- `encoding-auditor` — the chord parser and the `Display` round trip changed;
  ask specifically whether `cmd+`/`mod+` can produce a chord that parses to one
  thing and renders as another.

Not needed: `protocol-guardian` (no `norte-proto`, no JSON-RPC handler),
`security-reviewer` (no journal, policy, auth, plugin-host or MCP).

Give each the commit range, what the change is for, and the two questions this
plan is genuinely unsure about:

1. `Effective::lookup` is a linear scan over every binding including the
   unavailable ones, and the presets of K2 add ~400. Is the scan still fine at
   that size, or does it want the trie ADR 0006 mentions?
2. `MOD_KEY` is a `OnceLock` read inside `parse_chord`. Is there a path in
   either frontend where a keymap is parsed BEFORE `set_mod_key` runs, which
   would silently freeze the policy to `Ctrl`?

Apply BLOCKER and MAJOR findings. Say which MINORs you skipped and why.

---

## Self-review

**Spec coverage.** Every K1 requirement in the design maps to a task: shared
catalogue with `Status` (Task 2), unavailable bindings surviving tagged and the
three consequences — a message, prefix-free participation, the GUI reachability
pin untouched (Tasks 3 and 4), `UnknownCommand` keeping its meaning (Task 3
Step 1's fourth test), `mod+` with the TUI's stated limitation (Task 5), the
sacred-key rule (**not implemented here** — see the gap below), the file split
(Task 1), and the testing list (distributed across the tasks that add each
behaviour).

**One deliberate gap.** The spec's "sacred keys are not negotiable" rule — a
preset may not rebind `Tab` — has no task. It has no consumer until a preset
tries it, all four target managers use `Tab` the same way, and implementing a
prohibition with no violator is how a rule ends up untested. It belongs in K2,
with the first preset, and K2's plan must carry it. Recorded here so it is
skipped on purpose rather than lost.

**Type consistency.** `Availability` is the one name for the state, used
identically in `effective.rs`, `resolve.rs`, `unavailable_message` and both
frontends. `Binding { seq, run, avail }` is the storage everywhere after Task 3
— no call site keeps the old `(Vec<Chord>, String)` tuple. `bindings()` and
`bindings_all()` are the two accessors, spelled the same in Task 3 and in the
K3 sketch. `ModKey::{Ctrl, Cmd}` and `Mods.cmd` agree across Task 5.

**Known risk, with its fallback.** Task 3 makes unavailable bindings count
toward prefix-freeness, which the lenient filter used to exempt. If a bundled
preset already contains such a pair, `just t norte-frontend` goes red in Task 3
Step 8 with `AmbiguousPrefix`. That is a real ambiguity that was being hidden,
not a regression: fix the preset, do not exempt the binding.
