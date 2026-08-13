# GUI Synchronisation Surface Implementation Plan (spec 3, phase C2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #161 — the GUI can plan, approve and apply a one-way
synchronisation over the diff pane phase C1 built. That closes spec 3 and, with
it, roadmap item 1.

**Architecture:** The same shape C1 used, for the same reason. `norte-frontend::sync`
already owns everything that decides anything — `SyncState`, `SyncPlan`,
`StepUndo`, `UndoOutlook`, `PlanIntegrity`, `render_step`, `summary_lines`,
`confirmation` — and ~45 Fluent ids exist in both locales. What lives in
`norte-tui` and must move first is `SyncView`, the run wrapper. Then the GUI
holds one, feeds it from `Backend::sync_plan`, renders the plan and the approval
dialog, and applies with `Backend::sync_apply`.

**Tech Stack:** Rust, GPUI, `norte-frontend::sync`, `norte-core::Backend`,
`norte_i18n` (both locales), `nextest`.

**Gate: `just gui-ci`.** Tasks that touch `norte-frontend` or `norte-tui` also
need `just t norte-frontend` and `just t norte-tui`; the branch as a whole needs
**both** `just ci` and `just gui-ci` before it merges.

**Spec:** `docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md` §5.2

**This is the destructive half of the spec.** C1's pane is read-only; this one
rewrites and, under `Mirror`, deletes a subtree. Every task below that touches
the apply path gets a `security-reviewer` as well as a `rust-reviewer`.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — the sync run state moves to `norte-frontend` | done | `3ef7102` |
| 2 — the GUI plans a synchronisation | done | `aa79289` |
| 3 — the GUI renders the plan | done | `a61076e` |
| 4 — approve and apply | done | `9fefd2c` |
| 5 — the command, the key and the reference sheet | done | |
| 6 — close the branch | pending | |

## What the implementer needs to know before task 1

Verified against `132dd3d`.

**Read `docs/superpowers/plans/2026-08-13-gui-compare-pane.md`'s closing section
"What the reviews changed" first.** C1 is this plan's twin and its mistakes are
the ones available here: a "move" that moved half a decision under a comment
claiming it moved all of it, a cached count with no test whose failure mode was
silent, and fixes applied to the GUI copy while the TUI kept the defect.

**The model is shared and tested without a terminal.** `norte_frontend::sync`
has `SyncState::{ready, on_steps, on_plan_done, on_apply_started}`, `SyncPlan`
with `summary_lines(lang)` / `confirmation(lang)` / `counts` / `dest_trash` /
`outlook` / `integrity` / `can_approve` / `steps`, and `render_step(&SyncStep,
DestTrash, reinterpret) -> StepCells`. `Planning`, `Applying` and `Applied` are
the other states.

**The run wrapper is not shared, and that is task 1.** `SyncView` lives in
`crates/norte-tui/src/app.rs` (near line 145): `{ state: SyncState, confirming:
Option<Confirmation>, … }`. Move it the way C1 moved `CompareView` — and
**move the whole decision, not one arm of it.** C1's task 1 left three arms
behind and the coherence review had to put them back.

**The trap this plan exists to carry forward.** `crates/norte-tui/src/main.rs`
near line 9341 documents it: ask **`SyncState::can_approve`, not
`SyncPlan::can_approve`** — the second keeps answering yes about a plan that has
already been approved, because `SyncState::plan()` still hands it out. Phase A's
CLI shipped a related defect (it never asked at all, so a plan with
`PlanIntegrity::Malformed` was prompted and then applied in full from the
spool). Ask the state.

**The GUI is remote-only**, so `Backend::is_journalled()` is true and
`pane.sync-dirs` becomes genuinely available here. Phase A added an assertion
pinning it as `NotHere`; task 5 is what earns the right to flip it.

**The GUI's view precedent** is `settings_view.rs` / `help_view.rs`, and now
`compare_view.rs`, which is the closest twin: a module, a pure `on_key`
returning an outcome, and the `NorteGui` method that acts on it. Rendering goes
in that module, never into `main.rs`'s 14 000 lines.

---

## Task 1: The sync run state moves to `norte-frontend`

Pure move. The TUI's suite is the proof, and no test may need editing.

**Files:**
- Modify: `crates/norte-frontend/src/sync.rs`
- Modify: `crates/norte-tui/src/app.rs`, `crates/norte-tui/src/main.rs`
- Test: `crates/norte-frontend/src/sync.rs` (`mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
    /// #161: el envoltorio del run vivía en `norte-tui`, así que la GUI habría
    /// tenido que reimplementarlo. C1 aprendió que mover MEDIA decisión es
    /// peor que no moverla: el comentario dice «una sola regla» y hay tres
    /// copias. Aquí se mueve entera.
    #[test]
    fn el_envoltorio_del_run_vive_con_el_modelo() {
        let mut v = SyncView::default();
        assert!(v.confirming.is_none(), "nace sin pregunta pendiente");
        assert!(
            !v.can_approve(),
            "un plan que todavía no cerró NO se puede aprobar"
        );
    }

    /// La trampa que la TUI documenta y que el CLI de la fase A no vio:
    /// `SyncState::can_approve` sabe que un plan YA aprobado no se vuelve a
    /// aprobar; `SyncPlan::can_approve`, que sigue accesible por
    /// `SyncState::plan()`, contesta que sí.
    #[test]
    fn un_plan_ya_aprobado_no_se_aprueba_dos_veces() {
        let mut v = SyncView::default();
        v.state = SyncState::ready(vec![paso()], cierre());
        assert!(v.can_approve(), "cerrado y sano: se puede");

        v.on_apply_started(/* … */);
        assert!(
            !v.can_approve(),
            "ya aplicándose: la respuesta es NO, aunque el plan de dentro diga que sí"
        );
    }
```

**Note for the implementer:** `paso()` and `cierre()` are the helpers
`crates/norte-tui/src/main.rs`'s tests use around line 13745 — move or mirror
them. Check `SyncView`'s real fields and `on_apply_started`'s real signature
before writing; keep the TUI's behaviour exactly, and adjust this test rather
than the behaviour if they differ.

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-frontend
```

- [ ] **Step 3: Move it**

Cut `SyncView` into `crates/norte-frontend/src/sync.rs`, rustdoc verbatim.
`norte-frontend` has `#![warn(missing_docs)]`.

**Move every decision that goes with it.** Grep `crates/norte-tui/src/main.rs`
for the places that read `view.state` and decide something — anything that both
frontends will need is part of this task, not of task 2. C1's review found the
`TaskState` mapping transcribed by hand into both frontends precisely because
task 1 stopped at the type.

- [ ] **Step 4: Both suites pass, unchanged**

```sh
just t norte-frontend
just t norte-tui
```

Expected: PASS, **no test edited**. If a TUI test needed changing, the move was
not pure — stop and say why.

- [ ] **Step 5: Lint and commit**

```sh
just c
git commit -m "refactor(frontend): the synchronisation's run state moves next to its model"
```

---

## Task 2: The GUI plans a synchronisation

State and wiring, no rendering, no apply.

**Files:**
- Create: `crates/norte-gui/src/sync_view.rs`
- Modify: `crates/norte-gui/src/main.rs`, `crates/norte-gui/src/session.rs`
- Test: `crates/norte-gui/src/sync_view.rs`

- [ ] **Step 1: Write the failing test**

```rust
    /// Los lotes de pasos se acumulan y el plan NO se cierra hasta que llega
    /// `sync.plan_done`: sin él no hay `plan_hash`, y sin hash no se aplica
    /// nada. Que el canal se acabe no es que el plan esté completo.
    #[test]
    fn el_plan_se_cierra_con_su_notificacion_y_no_con_el_canal() {
        let mut v = vista_de_prueba();
        v.on_steps(lote(&[1, 2]));
        assert!(!v.can_approve(), "todavía llegando");

        v.on_plan_done(cierre_con(2));
        assert!(v.can_approve(), "cerrado, íntegro y con pasos: aprobable");
    }

    /// Un lote de OTRO plan no entra: el `task_id` es lo único que lo dice.
    #[test]
    fn un_lote_de_otro_plan_se_descarta() {
        let mut v = vista_de_prueba();
        let antes = v.steps_len();
        assert!(!v.on_steps(lote_de_otra_task(&[9])));
        assert_eq!(v.steps_len(), antes);
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

- [ ] **Step 3: Wire the session**

`SessionCmd::SyncPlan { generation, params }`; `SessionEvent::{SyncPlanStarted,
SyncSteps, SyncPlanDone, SyncPlanFailed}`. **Carry `generation` on every one of
them, the failure included** — C1 shipped `CompareFailed` without it and left a
banner describing a superseded request.

Mirror `compare`'s cancellation registration: the plan is a task and rule 3
applies.

- [ ] **Step 4: Hold it on `NorteGui`**

`sync: Option<SyncView>` plus the generation guard, fed by those events.

- [ ] **Step 5: Gate and commit**

```sh
just gui-ci
git commit -m "feat(gui): the GUI can plan a synchronisation"
```

---

## Task 3: The GUI renders the plan

**Files:**
- Modify: `crates/norte-gui/src/sync_view.rs`, `crates/norte-gui/src/main.rs`
- Test: `crates/norte-gui/src/sync_view.rs`

- [ ] **Step 1: Write the failing test**

```rust
    /// Cada paso lleva sus tres glifos —clase, confianza y qué devuelve el
    /// undo— porque el veredicto tiene que leerse SIN color. La fase A perdió
    /// el de confianza en el CLI y hubo que reponerlo: es justo la señal de
    /// «esto se sobrescribe fiándose sólo del mtime», en la pantalla donde se
    /// decide borrar un subárbol.
    #[test]
    fn cada_paso_lleva_sus_tres_glifos() {
        let cells = norte_frontend::sync::render_step(&paso(), DestTrash::Absent, None);
        assert_ne!(cells.glyphs.kind, ' ');
        assert_ne!(cells.glyphs.confidence, ' ');
        assert_ne!(cells.glyphs.undo, ' ');
    }

    /// El resumen sale de `summary_lines`, no de una segunda redacción.
    #[test]
    fn el_resumen_es_el_compartido() {
        let v = vista_cerrada();
        assert_eq!(
            v.summary_lines(Lang::Es),
            v.plan().expect("cerrado").summary_lines(Lang::Es)
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

- [ ] **Step 3: Render it**

Follow `compare_view.rs`, which is now the closest precedent in this crate and
already solved the parts that bite: the palette resolved once per frame (#87),
the `Role::List` wrapper with `Role::ListItem` rows, hostile names through the
listing's existing badge, and **each face as its own accessibility node** — a
step's name must not be able to impersonate a whole row to a screen reader, the
way C1's `aria_label` allowed before it was fixed.

`summary_lines`, `render_step` and the labels come from `norte-frontend`. Add a
Fluent id only if something is genuinely GUI-only, and then to **both** locales
in the same commit.

- [ ] **Step 4: Gate and commit**

```sh
just gui-ci
git commit -m "feat(gui): the synchronisation plan, legible without colour"
```

---

## Task 4: Approve and apply

The destructive half.

**Files:**
- Modify: `crates/norte-gui/src/sync_view.rs`, `crates/norte-gui/src/main.rs`, `crates/norte-gui/src/session.rs`
- Test: `crates/norte-gui/src/sync_view.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    /// Un plan que NO se puede aprobar no llega a preguntar. La fase A shipeó
    /// lo contrario: `confirmation()` devuelve `None` cuando `!can_approve()`,
    /// así que los planes menos fiables recibían el AVISO MÁS CORTO — un
    /// `mirror` bloqueado salía con un sí/no pelado y sin la frase de «esto
    /// borra N árboles».
    #[test]
    fn un_plan_no_aprobable_no_pregunta() {
        let mut v = vista_con_plan_bloqueado();
        assert!(!v.can_approve());
        assert!(
            v.ask().is_none(),
            "no se pregunta por un plan que no se puede aprobar: se explica por qué"
        );
    }

    /// Y uno aprobable pregunta con la frase compartida, la que cuenta los
    /// borrados y dice si van a la papelera.
    #[test]
    fn un_plan_aprobable_pregunta_con_la_frase_compartida() {
        let mut v = vista_cerrada();
        let q = v.ask().expect("aprobable");
        assert_eq!(q, v.plan().expect("cerrado").confirmation(Lang::Es).expect("hay pregunta"));
    }

    /// Aplicar dos veces el mismo plan no puede pasar: `SyncState` lo sabe
    /// aunque el `SyncPlan` de dentro siga diciendo que sí.
    #[test]
    fn no_se_aplica_dos_veces() {
        let mut v = vista_cerrada();
        assert!(v.approve().is_some(), "la primera da el hash");
        assert!(v.approve().is_none(), "la segunda, nada");
    }
```

- [ ] **Step 2: Run them and watch them fail**

```sh
just gui-ci
```

- [ ] **Step 3: Implement**

`SessionCmd::SyncApply { generation, plan_hash }` → `Backend::sync_apply` →
`Backend::sync_report`. `SessionEvent::{SyncApplyStarted, SyncApplied,
SyncApplyFailed}`, all carrying `generation`.

Requirements, each of which phase A got wrong once:

- **Ask `SyncState::can_approve`, never `SyncPlan::can_approve`.**
- The confirmation is `SyncPlan::confirmation(lang)` — do not write a second
  sentence about deletion; that function already computes it from `dest_trash`
  and the counts.
- A plan that cannot be approved gets an **explanation**, not a shorter prompt.
- The report's failures are shown one per line, from `sync.report`.
- Cancellation: the apply is a task, rule 3 applies, and the pane must not be
  closable into a state where an apply is running invisibly.

- [ ] **Step 4: Gate and commit**

```sh
just gui-ci
git commit -m "feat(gui): the GUI applies the plan the human approved"
```

---

## Task 5: The command, the key and the reference sheet

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs`, `crates/norte-gui/src/main.rs`

- [ ] **Step 1: Write the failing test**

Phase A pinned `pane.sync-dirs` as `NotHere` in the GUI because it was not
built. It is now, and this test is what earns the flip. Read C1's twin
(`la_comparacion_ya_esta_y_la_sincronizacion_todavia_no`) and update it rather
than adding a third assertion about the same table.

- [ ] **Step 2: Run it and watch it fail**

```sh
just gui-ci
```

- [ ] **Step 3: Declare it**

`pane.sync-dirs` into the GUI's `COMMANDS`, with the dispatch arm. Check the
seven presets: C1 found `far` and `norton` deliberately leave the comparison
unbound and needed a `gui_supplement` entry. Find out what they do with sync
before assuming the same answer — and note that **the supplement WINS over the
preset** (C1's comment said the opposite and was wrong), so a careless fallback
key silently steals whatever the preset had there.

- [ ] **Step 4: Gate and commit**

```sh
just gui-ci
git commit -m "feat(gui): pane.sync-dirs is live in the GUI"
```

---

## Task 6: Close the branch

- [ ] **Step 1: Dispatch the reviewers**

- `security-reviewer` — mandatory, whole branch. This is the one surface in
  phase C that rewrites and deletes a subtree.
- `rust-reviewer` — whole diff.
- `encoding-auditor` — tasks 3 and 4. Every step shows a filename, and the
  approval dialog is where a spoofed one does the most damage.

- [ ] **Step 2: A whole-branch coherence review**

**Not optional.** Three phases of this spec in a row have shipped their worst
defects between tasks, each task correct against its own specification. Ask it:
does the GUI's notion of "approvable" match the TUI's now that they share
`SyncView`; can the apply be reached without the whole plan having been shown;
and did task 1's move leave any decision duplicated, which is exactly what C1's
did.

- [ ] **Step 3: Both gates**

```sh
just ci
just gui-ci
```

Check them by grepping for `^error` and `Summary`, not by a pipe's exit code.

- [ ] **Step 4: Close #161 and merge**

That also closes spec 3 and roadmap item 1. Follow
`superpowers:finishing-a-development-branch`.
