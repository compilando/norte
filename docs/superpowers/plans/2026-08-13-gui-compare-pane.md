# GUI Compare Pane Implementation Plan (spec 3, phase C1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #158 — the GUI gets a diff pane, so `fs.compare` stops being a
TUI-only capability.

**Architecture:** Almost nothing new is computed. `norte-frontend::compare`
already owns the model (`ComparePane`, `cells_for`, `CATEGORIES`, the glyphs and
every label) and the TUI already owns the run lifecycle — but that half lives in
`norte-tui` and has to move first, because a second copy of it is how the two
frontends start disagreeing about what "the comparison finished" means. Then the
GUI holds a `CompareView` per side, feeds it from the same
`Backend::compare` stream the TUI uses, and renders it with the
`Option<XView>` + pure-`on_key` + `Outcome` shape that `settings_view.rs` and
`help_view.rs` already established in this crate.

**Tech Stack:** Rust, GPUI, `norte-frontend::compare`, `norte-core::Backend`,
`norte_i18n` (both locales), `nextest`.

**Gate: `just gui-ci`, not `just ci`.** `norte-gui` is excluded from the
workspace. A task that touches `norte-frontend` or `norte-tui` needs
`just t norte-frontend` / `just t norte-tui` as well — those are in the
workspace and `just gui-ci` will not run them.

**Spec:** `docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md` §5.1

**This is C1 and it ships alone.** The synchronisation surface (#161) is C2, on
top of this. Do not build a sync dialog here; there is nothing to hang it on
until the pane exists.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — the run state moves to `norte-frontend` | done | 7f0b133 |
| 2 — the GUI runs a comparison and keeps its rows | done | 47cabdc |
| 3 — the GUI renders the diff pane | done | `af44595` |
| 4 — the command, the key and the reference sheet | done | `b402260` |
| 5 — close the branch | pending | |

## What the implementer needs to know before task 1

Verified against `56efbeb`.

**The model is already shared and already tested without a terminal.**
`norte_frontend::compare` has `ComparePane` (rows, hidden categories, selection
by id, active side), `cells_for(&CompareRow, left_reinterpret, right_reinterpret)
-> RowCells`, `CATEGORIES`, `Category`, `verdict_glyph`, `confidence_glyph`, and
`verdict_label` / `confidence_label` / `criterion_label` / `reason_label` /
`side_label`, all taking a `Lang`.

**The run lifecycle is not shared, and that is task 1.** `CompareState` and
`CompareView` live in `crates/norte-tui/src/app.rs` (near line 100). Neither
contains anything terminal-shaped: `CompareState` is a five-variant enum,
`CompareView` is `{ pane, state, error category, the two roots }`.

**Read `CompareState::Incomplete`'s rustdoc before anything else.** It exists
because the row channel closing does **not** mean every row arrived — the rows
pump and the terminal-snapshot pump are independent tasks, so the TUI compares
what it received against `TaskProgress::entries_done` and says `Incomplete` when
they disagree. This is not a nicety: the CLI (phase A) and the MCP tool (phase
B) each re-derived this lifecycle and **each got it wrong**, in both cases
reporting a complete answer for a run that had lost batches. The TUI has been
right all along. Moving this type is what stops the GUI from becoming the third
mistake.

**The GUI's view precedent** is `crates/norte-gui/src/settings_view.rs` and
`help_view.rs`: a module per view, an `Option<XView>` field on `NorteGui`
(`main.rs` near line 404), a **pure** `on_key(view, key, char) -> XOutcome`
tested without GPUI, and the `NorteGui` method that acts on the outcome. Follow
it. `main.rs` is 14 000 lines and the single `Render` impl is at line 9591 —
new rendering goes in its own module, not in there.

**Panes are `norte_frontend::PaneState`**, shared with the TUI. The diff pane is
*not* a `PaneState`: it is a different model rendered in the same place, exactly
as in the TUI.

**Colour is not the signal** (spec §17). A verdict must be legible in
monochrome; `verdict_glyph` and `confidence_glyph` exist for that. The TUI is
correct here and the GUI must not regress it into a colour-coded list.

---

## Task 1: The run state moves to `norte-frontend`

Pure move plus re-export. No behaviour change, and the TUI's suite is the proof.

**Files:**
- Modify: `crates/norte-frontend/src/compare.rs` (add `CompareState`, `CompareView`)
- Modify: `crates/norte-tui/src/app.rs` (delete both, import them)
- Test: `crates/norte-frontend/src/compare.rs` (its `mod tests`)

- [ ] **Step 1: Write the failing test**

In `crates/norte-frontend/src/compare.rs`'s `mod tests`:

```rust
    /// #158: el estado del run vivía en `norte-tui`, así que la GUI habría
    /// tenido que reimplementarlo — y las dos superficies que ya lo
    /// reimplementaron (el CLI en la fase A, la tool MCP en la fase B) se
    /// equivocaron en lo mismo: dieron por completa una respuesta a la que le
    /// faltaban lotes. Aquí, y una sola vez.
    #[test]
    fn el_estado_del_run_vive_con_el_modelo() {
        let mut v = CompareView::new(
            VPath::parse("file:///a").expect("wire"),
            VPath::parse("file:///b").expect("wire"),
        );
        assert_eq!(v.state, CompareState::Running, "nace corriendo");
        assert!(v.pane.is_empty());

        // Menos filas de las que la task contó NO es «hecho».
        v.finish(7, 3);
        assert_eq!(
            v.state,
            CompareState::Incomplete,
            "3 filas recibidas contra 7 contadas: la respuesta está a medias y lo dice"
        );
    }
```

**Note for the implementer:** `CompareView::new` and `finish(entries_done,
rows_received)` are this plan's names for what the TUI does today inline. Read
how `crates/norte-tui/src/app.rs` currently decides between `Done` and
`Incomplete` and move that decision into `finish`, rather than inventing a
second rule. If the TUI's version takes different arguments, keep the TUI's —
this task must not change behaviour.

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-frontend
```

Expected: FAIL — `cannot find type CompareState in this scope`.

- [ ] **Step 3: Move both types**

Cut `CompareState` and `CompareView` from `crates/norte-tui/src/app.rs` into
`crates/norte-frontend/src/compare.rs`, keeping their rustdoc **verbatim** —
`CompareState::Incomplete`'s doc is the reason this move exists. `norte-frontend`
has `#![warn(missing_docs)]`, so every public field needs its doc; they already
have them.

Add `finish` if the TUI's logic was inline, and make the TUI call it.

- [ ] **Step 4: Point the TUI at them**

`use norte_frontend::compare::{CompareState, CompareView};` in
`crates/norte-tui/src/app.rs`. There are 28 references to `CompareState` in
`crates/norte-tui/src/` — a re-export at the old path is acceptable if it keeps
the diff small, but prefer importing where used; a type with two homes is what
this task is removing.

- [ ] **Step 5: Both suites pass, unchanged**

```sh
just t norte-frontend
just t norte-tui
```

Expected: PASS, and **no test edited**. If a TUI test needed changing, the move
was not pure — find out why before continuing.

- [ ] **Step 6: Lint and commit**

```sh
just c
git add crates/norte-frontend/src/compare.rs crates/norte-tui/src/
git commit -m "refactor(frontend): the comparison's run state moves next to its model"
```

---

## Task 2: The GUI runs a comparison and keeps its rows

State and wiring, no rendering. The test is that rows arrive and the terminal
verdict is right.

**Files:**
- Create: `crates/norte-gui/src/compare_view.rs`
- Modify: `crates/norte-gui/src/main.rs` (a `compare: Option<CompareView>` field near the other `Option<XView>` fields at line 404; the `SessionCmd`/`SessionEvent` arms)
- Modify: `crates/norte-gui/src/session.rs` (the command that starts the task and pumps its rows)
- Test: `crates/norte-gui/src/compare_view.rs` (`mod tests`)

- [ ] **Step 1: Write the failing test**

The pure part, testable without GPUI — which is the whole point of the
`settings_view` shape:

```rust
    /// Los lotes que llegan se acumulan, y el veredicto terminal sale de
    /// contrastar lo recibido con lo que la task contó — no de que el canal se
    /// cerrara.
    #[test]
    fn los_lotes_se_acumulan_y_el_final_se_verifica() {
        let mut v = view_de_prueba();
        v.on_rows(vec![fila(1), fila(2)]);
        assert_eq!(v.pane.len(), 2);
        assert_eq!(v.state, CompareState::Running, "un lote no cierra nada");

        v.finish(2, v.pane.len() as u64);
        assert_eq!(v.state, CompareState::Done);
    }

    /// Un lote perdido se ve, y NO se pinta «hecho» encima.
    #[test]
    fn un_lote_perdido_no_se_pinta_como_hecho() {
        let mut v = view_de_prueba();
        v.on_rows(vec![fila(1)]);
        v.finish(2, v.pane.len() as u64);
        assert_eq!(v.state, CompareState::Incomplete);
    }
```

**Note for the implementer:** `view_de_prueba()` and `fila(n)` are helpers you
write; build a `CompareRow` the way `norte-frontend`'s own tests build one and
reuse that shape rather than a new fixture.

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

Expected: FAIL — no `compare_view` module.

- [ ] **Step 3: Wire the session**

`session.rs` already owns the async task that talks to the `Backend` and reports
back with `SessionEvent`. Add:

- a `SessionCmd::Compare { left, right }` that calls `Backend::compare` and, for
  each `CompareRowsBatch`, sends a `SessionEvent::CompareRows { task_id, rows }`;
- a terminal `SessionEvent::CompareDone { task_id, state, entries_done }` built
  from `task.join()` **and** the progress counter — read
  `crates/norte-tui/src/app.rs`'s handling and mirror it.

`task_id` is not decoration: it is the only thing that says a batch belongs to
*this* comparison rather than to one the user started and cancelled. The TUI
already filters on it; do the same.

- [ ] **Step 4: Hold it on `NorteGui`**

A `compare: Option<CompareView>` field, fed by those two events, with the same
"drop what is not mine" rule.

- [ ] **Step 5: Run the tests and watch them pass**

```sh
just gui-ci
```

- [ ] **Step 6: Commit**

```sh
git add crates/norte-gui/src/
git commit -m "feat(gui): the GUI can run a comparison and keep what it returns"
```

---

## Task 3: The GUI renders the diff pane

**Files:**
- Modify: `crates/norte-gui/src/compare_view.rs` (the rendering)
- Modify: `crates/norte-gui/src/main.rs` (render the pane in place of the listing for the side that has one)
- Test: `crates/norte-gui/src/compare_view.rs`

- [ ] **Step 1: Write the failing test**

Rendering in GPUI is not unit-testable here, so test what *decides* the render,
which is the part that can be wrong:

```rust
    /// Ocultar una categoría quita sus filas de lo que se pinta y NO de lo que
    /// se recibió: un filtro es una vista, no una pérdida.
    #[test]
    fn el_filtro_de_categoria_no_pierde_filas() {
        let mut v = view_de_prueba();
        v.on_rows(vec![fila_same(1), fila_distinta(2)]);
        v.pane.toggle(Category::Same);

        assert_eq!(v.visible_rows().count(), 1, "la fila Same está oculta");
        assert_eq!(v.pane.len(), 2, "pero sigue estando");
    }

    /// El veredicto se lee SIN color (spec §17): cada fila lleva su glifo.
    #[test]
    fn cada_fila_lleva_glifo_de_veredicto_y_de_confianza() {
        let cells = norte_frontend::compare::cells_for(&fila_distinta(1), None, None);
        assert_ne!(cells.glyphs.verdict, ' ');
        assert_ne!(cells.glyphs.confidence, ' ');
    }
```

**Note for the implementer:** check the real method names on `ComparePane` for
toggling and iterating (`toggle`/`is_hidden`, and whatever it offers for visible
rows) and use those; add `visible_rows` to `compare_view` only if the pane does
not already answer it.

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

- [ ] **Step 3: Render it**

Follow `settings_view.rs`/`help_view.rs` for the element tree, the theme
resolution and the row loop. What must appear:

- one row per visible row, built from `cells_for`, with both sides' names and
  **both glyphs** — verdict and confidence;
- hostile names marked, the same way the GUI already marks them in the listing
  (find it; do not invent a second marker);
- the category filter with its five counts, from `CATEGORIES`;
- a status line whose text comes from the `compare-status-*` Fluent ids the TUI
  already uses — **both locales already have them**, so no new ids unless
  something is genuinely GUI-only.

**Performance:** `main.rs`'s render doc warns that resolving the theme per row
is the known O(N)-per-frame trap (#87) and that the listing is not virtualised.
Resolve the palette once per frame as the listing does, and do not add a second
per-row theme lookup.

- [ ] **Step 4: Run the gate**

```sh
just gui-ci
```

- [ ] **Step 5: Commit**

```sh
git add crates/norte-gui/src/
git commit -m "feat(gui): the diff pane, legible without colour"
```

---

## Task 4: The command, the key, and the reference sheet

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs` (the GUI's `COMMANDS` list, and the `NotHere` assertion from phase A)
- Modify: `crates/norte-gui/src/main.rs` (the command dispatch, near the `"pane.semantic-search"` arm at line 2424)

- [ ] **Step 1: Write the failing test**

Phase A added an assertion pinning `pane.sync-dirs` as `NotHere` in the GUI.
The comparison command is the twin, and this task flips it the other way:

```rust
    /// #158: `pane.compare-dirs` deja de ser `NotHere` en la GUI — está
    /// construido. `pane.sync-dirs` sigue sin estarlo (es C2) y la hoja lo
    /// tiene que seguir diciendo.
    #[test]
    fn la_comparacion_ya_esta_y_la_sincronizacion_todavia_no() {
        // … el mismo molde que `means_command_ignora_secuencias_y_no_disponibles` …
    }
```

**Note for the implementer:** find the real command id — the TUI's binding for
the diff pane is `Shift+F2`; grep the catalogue for its id rather than trusting
`pane.compare-dirs`. Note #159: under tmux no modified function key arrives,
which is a TUI-only problem but explains why the id matters more than the key.

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

- [ ] **Step 3: Add the command and dispatch it**

Into the GUI's `COMMANDS`, and an arm that starts the comparison between the two
panes' directories. Cancelling is the other half — the TUI cancels the task and
keeps the rows that arrived, because a cancelled comparison's rows are still
true. Do the same.

- [ ] **Step 4: Run the gate and commit**

```sh
just gui-ci
git add crates/norte-gui/src/
git commit -m "feat(gui): pane.compare-dirs is live in the GUI"
```

---

## Task 5: Close the branch

- [ ] **Step 1: Dispatch the reviewers**

- `rust-reviewer` — the whole diff.
- `encoding-auditor` — tasks 2–4. Every row printed is a filename off a
  filesystem the user may not control.

Ask them the two questions that are actually open: whether a batch belonging to
a *previous* comparison can reach the current view, and whether the pane can
show `Done` for a run that lost rows.

- [ ] **Step 2: A whole-branch coherence review**

Both previous phases of this spec had blockers that no per-task review could
see, because each task was correct against its own specification. Ask
specifically: does the GUI's notion of "the comparison finished" match the
TUI's now that they share the type, and did task 1's move leave any decision
duplicated on the TUI side.

- [ ] **Step 3: Both gates**

```sh
just gui-ci
just ci
```

Both, because task 1 touched `norte-frontend` and `norte-tui`, which
`just gui-ci` does not cover.

- [ ] **Step 4: Close #158 and merge**

#161 stays open — the sync surface is C2. Follow
`superpowers:finishing-a-development-branch`.
