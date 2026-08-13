# CLI Compare and Sync Implementation Plan (spec 3, phase A)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `fs.compare` and `sync.plan`/`sync.apply` a command line, so a
script can answer "did the copy work" and a human can synchronise two trees
without the TUI.

**Architecture:** Two subcommands over the `Backend` methods that already
exist. `norte compare` runs the compare task and prints its rows, answering in
the **exit code** (`diff`-style: 0 same, 1 differ, 2 could not tell). `norte
sync` is the plan-then-confirm shape `norte ai rename` already uses in this
CLI: plan, print the whole plan, resolve the journal, ask, apply — all on ONE
connection, because a retained plan is keyed to the connection that produced it
and dies with it. Every row and every step is rendered through
`norte-frontend`, so the CLI shows the vocabulary the TUI shows and no second
rendering can drift.

**Both commands answer in the exit code, and one rule governs both: only a run
that FINISHED may answer 0 or 1.**

| code | `norte compare` | `norte sync` |
| --- | --- | --- |
| 0 | the trees agree, and every row was answered with confidence | nothing to do (empty plan) |
| 1 | they differ | they differed and it was resolved — or, with `--dry-run`, shown |
| 2 | could not tell | it did not happen |

For `compare` the three accumulate by PRECEDENCE — could-not-tell beats differ
beats agree — and both the verdict AND the confidence feed it: a `Same` the
provider could not back up (`CompareConfidence::Unknown`, which `cascade.rs`
answers for a socket, a missing size, or two symlink targets it could not read)
is a 2, because collapsed into an exit code without the glyph beside it that
"same" would be read as "the trees agree".

For `sync`, 2 is everything that did not write: a plan that never closed, a
plan with blockers, a plan whose steps do not add up to what it claims to be,
no journal, no terminal to ask, a declined prompt, a refused apply, and an
apply that reported failures. **Nothing that returns `Err` may reach `main`'s
`ExitCode::FAILURE`**, which is 1 and therefore already spoken for — the
dispatch maps every error of these two commands to 2.

**Tech Stack:** Rust, `clap`, tokio, `norte-frontend::{compare,sync}`,
`norte_i18n` (Fluent, both locales), `assert_cmd` for the integration tests,
`nextest`.

**Spec:** `docs/superpowers/specs/2026-08-13-compare-sync-surfaces-design.md`

**Not in this plan:** the MCP tools (phase B) and the GUI panes (phase C). The
one GUI edit here is Task 4, which is independent of both and fixes a reference
sheet that currently lies.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — `norte compare` | done | `410ef29` |
| 2 — `norte sync`: plan, print, `--dry-run` | done | `68b9f6b` |
| 3 — `norte sync`: confirm, apply, report | done | `a88853c` |
| 4 — the GUI reference sheet stops lying (#161, half) | done | `c953eae` |
| 5 — close the branch | in progress | |

**The review of the whole branch is applied.** Its two BLOCKERs against THIS
document — a decline exiting `ExitCode::SUCCESS`, and "exit 1 if everything
applied" without saying what the other paths do — are corrected in place, in
the architecture section and in task 3, so phase B does not inherit the table.

## What the implementer needs to know before task 1

These are facts about this tree, verified at `c16c9c1`. They are not in the
spec because they are implementation surface, and they are not derivable from
the plan steps.

**The Backend API is already there.** No wire work, no proto bump:

```rust
Backend::compare(FsCompareParams)
    -> Result<(TaskRef, mpsc::Receiver<CompareRowsBatch>), Error>
Backend::sync_plan(SyncPlanParams)
    -> Result<(TaskRef, mpsc::Receiver<norte_core::sync::SyncPlanEvent>), Error>
Backend::sync_apply(PlanHash) -> Result<TaskRef, Error>
Backend::sync_report(TaskId) -> Result<SyncReportResult, Error>
```

`SyncPlanEvent` is `Steps(SyncStepsBatch)` | `Done(SyncPlanDone)`, and
`sync.plan_done` always arrives after the last batch — that ordering is the
protocol's, not a race to defend against.

**The rendering is already there**, in `norte-frontend`:

- `compare::cells_for(&CompareRow, left_reinterpret, right_reinterpret) -> RowCells`,
  plus `compare::{verdict_glyph, confidence_glyph, verdict_label, criterion_label, CATEGORIES, Category}`.
- `sync::render_step(&SyncStep, DestTrash, reinterpret) -> StepCells`, and on
  the closed plan: `SyncPlan::{summary_lines(lang), confirmation(lang), counts,
  dest_trash, outlook, integrity, can_approve, steps}`.
- `SyncState::{ready, on_steps, on_plan_done}` is the ONE place steps are
  checked against the counts. Build the plan through it; do not assemble a
  `SyncPlan` by hand.

**The precedent to read first** is `ai_cmd` in `crates/norte-cli/src/main.rs`
(around line 1855). It is this exact shape and it already solved the four
things that matter: printing the plan before asking, masking hostile names with
`norte_frontend::display_name` and MARKING the masking, resolving the journal
before the question (including under `--yes`), and confirming on stderr while
the plan goes to stdout.

**Strings** go through `norte_i18n::t` / `ta`, with the id added to BOTH
`crates/norte-i18n/i18n/en.ftl` and `es.ftl` in the same commit. `--json`
output stays locale-free.

---

## Task 1: `norte compare`

**Files:**
- Modify: `crates/norte-cli/src/main.rs` (the `Cmd` enum near line 116; the dispatch in `run()` near line 642; a new `compare_cmd` function next to `ls`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-cli/tests/compare_e2e.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-cli/tests/compare_e2e.rs`. Copy `config_dir_del_test()`
from `crates/norte-cli/tests/smoke.rs` verbatim, including its rustdoc — the
state directory must never be the developer's.

```rust
//! `norte compare`: el veredicto va en el CÓDIGO DE SALIDA, que es lo que un
//! script lee sin parsear nada.

use assert_cmd::Command;

// … config_dir_del_test() copiado de smoke.rs …

/// Dos árboles idénticos: 0, como `diff`.
#[test]
fn dos_arboles_iguales_salen_con_cero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("x.txt"), b"mismo").expect("write a");
    std::fs::write(b.join("x.txt"), b"mismo").expect("write b");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args(["compare", a.to_str().expect("utf8"), b.to_str().expect("utf8")])
        .assert()
        .code(0);
}

/// Un fichero que solo está en un lado: 1. NO es un error — es la respuesta.
#[test]
fn dos_arboles_distintos_salen_con_uno() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("solo-aqui.txt"), b"x").expect("write a");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args(["compare", a.to_str().expect("utf8"), b.to_str().expect("utf8")])
        .assert()
        .code(1);
}

/// `--json` sale sin traducir y una línea por fila, para que un script no tenga
/// que adivinar el idioma del que lo corre.
#[test]
fn json_no_lleva_idioma() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    std::fs::write(a.join("solo-aqui.txt"), b"x").expect("write a");

    let out = Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .env("NORTE_LANG", "es")
        .args([
            "compare",
            "--json",
            a.to_str().expect("utf8"),
            b.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let texto = String::from_utf8(out).expect("utf8");
    let primera = texto.lines().next().expect("al menos una fila");
    let fila: serde_json::Value = serde_json::from_str(primera).expect("json por línea");
    assert!(
        fila.get("verdict").is_some(),
        "cada línea es un CompareRow serializado: {primera}"
    );
}
```

**Verified for you:** `NORTE_CONFIG_DIR` is the state-directory override
`smoke.rs` uses, and `NORTE_LANG` is the first locale variable
`norte_i18n` reads (`NORTE_LANG` > `LC_ALL` > `LC_MESSAGES` > `LANG`). Both
test env vars above are correct as written.

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-cli
```

Expected: FAIL — `error: unrecognized subcommand 'compare'`.

- [ ] **Step 3: Add the subcommand**

In the `Cmd` enum in `crates/norte-cli/src/main.rs`:

```rust
    /// Compara dos árboles y contesta en el CÓDIGO DE SALIDA (0 iguales,
    /// 1 difieren, 2 no se pudo saber)
    Compare {
        /// Árbol izquierdo
        a: PathBuf,
        /// Árbol derecho
        b: PathBuf,
        /// Una fila por línea, en JSON, sin traducir
        #[arg(long)]
        json: bool,
        /// Criterios de comparación (por defecto los del wire)
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// Profundidad máxima del recorrido
        #[arg(long)]
        max_depth: Option<u32>,
        /// Tolerancia de mtime en milisegundos
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
```

- [ ] **Step 4: Write the handler**

Add next to `ls` in the same file. The shape, with the three things that are
easy to get wrong called out:

```rust
/// `norte compare`: `fs.compare` y su veredicto en el código de salida.
///
/// # Por qué el veredicto va en el código
/// Es la pregunta «¿funcionó la copia?», y quien la hace suele ser un script.
/// `diff` contesta así desde siempre y no hay nada que mejorar en esa
/// convención: 0 iguales, 1 difieren, y un tercer código para «no se pudo
/// saber» que es el que de verdad importa aquí — una comparación INCOMPLETA
/// que contestara 0 sería exactamente el fallo que este comando existe para
/// no cometer.
async fn compare_cmd(
    backend: &Backend,
    a: &std::path::Path,
    b: &std::path::Path,
    json: bool,
    criteria: &[String],
    max_depth: Option<u32>,
    mtime_tolerance_ms: Option<u32>,
) -> anyhow::Result<ExitCode> {
    // …
}
```

Body requirements, in order:

1. `vpath(a)?` / `vpath(b)?`, then build `FsCompareParams`. Leave
   `follow_symlinks` false and `descend_orphans` unset — `Backend::compare`
   rejects the first and `sync_plan` rejects both, and this command has no
   reason to differ.
2. `backend.compare(params).await`, mapping the error with
   `.map_err(|e| anyhow::anyhow!("{e}"))` as the neighbouring handlers do.
3. Drain the `mpsc::Receiver<CompareRowsBatch>`. For each row: with `--json`,
   `println!("{}", serde_json::to_string(&row)?)`; otherwise one line built
   from `norte_frontend::compare::cells_for(&row, None, None)` plus
   `verdict_glyph`. **Hostile names go through `norte_frontend::display_name`
   and the masking is MARKED** — the same `masked` closure `ai_cmd` uses;
   a filename from a tree the user does not control can carry RLO.
4. Track whether any row was not `Same` while draining — do not collect a
   million rows to count them afterwards (`ComparePane`'s rustdoc explains
   what that costs).
5. `task.join().await`, and map the terminal state to the exit code: completed
   → 0 or 1 by the flag from (4); anything else (failed, cancelled, incomplete)
   → 2, with the reason on stderr.

- [ ] **Step 5: Add the Fluent ids**

Both locales, same commit. At minimum: the failure message for (5) and the
column header if the human output prints one. Ids prefixed `cli-compare-`.

- [ ] **Step 6: Run the tests and watch them pass**

```sh
just t norte-cli
```

Expected: PASS, all three.

- [ ] **Step 7: Lint and commit**

```sh
just c
git add crates/norte-cli/src/main.rs crates/norte-i18n/i18n/en.ftl \
        crates/norte-i18n/i18n/es.ftl crates/norte-cli/tests/compare_e2e.rs
git commit -m "feat(cli): norte compare answers in its exit code"
```

---

## Task 2: `norte sync` — plan, print, `--dry-run`

Half the command: everything up to the question. It ships useful on its own —
`--dry-run` is the flag the "did the copy work" case wants — and it keeps the
destructive half in its own reviewable commit.

**Files:**
- Modify: `crates/norte-cli/src/main.rs` (the `Cmd` enum; the dispatch; a new `sync_cmd`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-cli/tests/sync_e2e.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-cli/tests/sync_e2e.rs`, with the same
`config_dir_del_test()` helper.

```rust
//! `norte sync`: el plan se imprime ENTERO antes de que haya pregunta, y
//! `--dry-run` es el plan sin el apply.

use assert_cmd::Command;

// … config_dir_del_test() copiado de smoke.rs …

/// `--dry-run` enseña lo que haría y NO toca el destino.
#[test]
fn dry_run_ensena_el_plan_y_no_escribe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("nuevo.txt"));

    assert!(
        !dst.join("nuevo.txt").exists(),
        "--dry-run no escribe en el destino"
    );
}

/// Nada que hacer: 0, y sin plan que enseñar.
#[test]
fn sin_diferencias_sale_con_cero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("igual.txt"), b"x").expect("write src");
    std::fs::write(dst.join("igual.txt"), b"x").expect("write dst");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--dry-run",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(0);
}
```

**Verified for you:** `norte-cli` has `assert_cmd` as a dev-dependency and
**not** `predicates`. Do not add it (rule 8 would want the dependency justified
in the PR, for an assertion that does not need it). Capture the output and
assert on it directly:

```rust
    let salida = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    assert!(salida.contains("nuevo.txt"), "el plan nombra el fichero: {salida}");
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-cli
```

Expected: FAIL — `error: unrecognized subcommand 'sync'`.

- [ ] **Step 3: Add the subcommand**

```rust
    /// Sincroniza un árbol sobre otro en UN sentido. Planifica, enseña el
    /// plan, y pregunta antes de aplicar
    Sync {
        /// De dónde se lee
        source: PathBuf,
        /// Dónde se escribe
        dest: PathBuf,
        /// `update` copia lo que falta o cambió; `mirror` además BORRA lo que
        /// sobra en el destino
        #[arg(long, value_enum)]
        mode: SyncModeArg,
        /// Enseña el plan y para: no aplica nada
        #[arg(long)]
        dry_run: bool,
        /// Aplica sin preguntar (el plan se imprime igual)
        #[arg(long)]
        yes: bool,
        /// Criterios de comparación
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// Tolerancia de mtime en milisegundos
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
```

Plus a `#[derive(clap::ValueEnum)] enum SyncModeArg { Update, Mirror }` mapping
to `norte_proto::methods::SyncMode`. Do not derive `ValueEnum` on the wire type
— the CLI's spelling of a mode is presentation and the wire's is a contract.

- [ ] **Step 4: Write the planning half of the handler**

```rust
/// `norte sync`: planifica, enseña, pregunta, aplica — TODO en una conexión.
///
/// # Por qué una sola invocación
/// Un plan aprobado se retiene POR CONEXIÓN, en un registro en memoria que
/// nace vacío, y `sync.apply` no lleva nada más que el `plan_hash`. Un CLI que
/// planease en un proceso y aplicara en otro no podría funcionar ni queriendo:
/// el registro del segundo no conoce ese hash. Así que la pregunta se hace con
/// la conexión viva, y `--dry-run` es esta misma función sin la segunda mitad.
async fn sync_cmd(/* … */) -> anyhow::Result<ExitCode> {
    // …
}
```

Body, this task's half:

1. `vpath()` both roots. Build `SyncPlanParams` with the `mode`, the
   `SyncCompareOptions` from the flags, and `on_unknown` left at its default.
2. `backend.sync_plan(params).await?` → `(TaskRef, rx)`.
3. Drive a `norte_frontend::sync::SyncState`: `on_steps(batch)` for each
   `SyncPlanEvent::Steps`, `on_plan_done(done)` for `Done`. **Do not assemble a
   `SyncPlan` by hand** — `SyncState` is the one place the steps are checked
   against the counts.
4. If the plan is **not** `executable`: print the blockers and exit 2. BEFORE
   the empty check, not after: the wire guarantees `!executable` ⟹ `steps`
   empty, so a plan stopped by a name collision or a read-only destination
   reaches step 5 looking exactly like two trees that already agree, and would
   answer 0.
5. If the closed plan `is_empty()`: print the "nothing to do" line, exit 0.
6. Print every step through `sync::render_step(step, plan.dest_trash(), None)`,
   with the same masking closure as `ai_cmd` (`display_name`, `!` marker) on
   the `rel` and `dest_rel`. Print **all three** glyphs (`kind`, `confidence`,
   `undo`) and the `reason` when there is one — the confidence glyph is "this
   overwrite is decided by an mtime alone", on the screen where a human agrees
   to delete a subtree, and rendering two of three IS the drift this phase
   exists to prevent. Then `plan.summary_lines(norte_i18n::active())`.
7. If `integrity()` is not `Complete`: say so and exit 2. What was just printed
   is not all of the plan `sync.apply` would run. True under `--dry-run` too: a
   plan that cannot be shown in full has not been "shown".
8. If `dry_run`: exit 1 (there are steps). Task 3 takes it from here.

The plan goes to stdout through ONE locked `BufWriter` whose write errors are
CHECKED. `println!` panics on `EPIPE` with exit 101, which is not in the table
this command documents, and `norte sync … | head` is the ordinary way to look
at a plan of ten thousand steps.

Exit code 2 for a plan that never closed — no `sync.plan_done` means no
`plan_hash` and nothing to approve, and that is a "could not tell", not a "no
differences".

- [ ] **Step 5: Add the Fluent ids**

Both locales, `cli-sync-` prefix: the empty-plan line and the plan header.
The step vocabulary itself is already translated in `norte-frontend::sync`.

- [ ] **Step 6: Run the tests and watch them pass**

```sh
just t norte-cli
```

Expected: PASS.

- [ ] **Step 7: Lint and commit**

```sh
just c
git add crates/norte-cli/src/main.rs crates/norte-i18n/i18n/*.ftl \
        crates/norte-cli/tests/sync_e2e.rs
git commit -m "feat(cli): norte sync --dry-run shows the plan it would apply"
```

---

## Task 3: `norte sync` — confirm, apply, report

The destructive half.

**Files:**
- Modify: `crates/norte-cli/src/main.rs` (`sync_cmd`)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-cli/tests/sync_e2e.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-cli/tests/sync_e2e.rs`:

```rust
/// `--yes` aplica, y el destino queda con lo que el plan prometía.
#[test]
fn con_yes_aplica_y_el_destino_recibe_el_fichero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            "--yes",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .assert()
        .code(1);

    assert_eq!(
        std::fs::read(dst.join("nuevo.txt")).expect("el destino recibió el fichero"),
        b"contenido"
    );
}

/// Sin terminal NO hay pregunta que hacer, así que no se hace: se rehúsa
/// ANTES, con el código de «no ocurrió» (2) y señalando `--yes`.
///
/// Los dos códigos que importan son los que NO puede devolver. `0` diría «los
/// árboles ya están sincronizados» —que es lo que un `norte sync src dst &&
/// echo ok` en un cron leería— habiendo escrito nada; y `1` diría «se
/// resolvió». Una respuesta vacía, un EOF y un stdin cerrado son el mismo
/// hecho: nadie consintió.
#[test]
fn sin_terminal_no_pregunta_y_no_aplica() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::create_dir_all(&dst).expect("mkdir dst");
    std::fs::write(src.join("nuevo.txt"), b"contenido").expect("write");

    Command::cargo_bin("norte")
        .expect("bin")
        .env("NORTE_CONFIG_DIR", config_dir_del_test())
        .args([
            "sync",
            "--mode",
            "update",
            src.to_str().expect("utf8"),
            dst.to_str().expect("utf8"),
        ])
        .write_stdin("\n")
        .assert()
        .code(2);

    assert!(
        !dst.join("nuevo.txt").exists(),
        "sin consentimiento no se aplica nada"
    );
}
```

Two more, for the gates step 4 adds:

```rust
/// Un plan BLOQUEADO no es «nada que hacer»: `src/x` es un DIRECTORIO y
/// `dst/x` un FICHERO, el transductor bloquea (`TypeMismatchDir`), y el wire
/// garantiza que un plan bloqueado viene SIN pasos. Leer esa lista vacía como
/// «los árboles ya coinciden» y contestar 0 es el fallo que el tercer código
/// existe para no cometer. → `--yes`, `code(2)`, y `dst/x` intacto.
///
/// El CLI DESMONTA su spool al salir: tras un `--dry-run` —que no aplica
/// nada— `<estado>/sync-spools/` tiene que quedar vacío. Directorio de estado
/// PROPIO en ese test: aquí se mira el spool, y el compartido lo puede estar
/// usando otro test.
```

- [ ] **Step 2: Run them and watch them fail**

```sh
just t norte-cli
```

Expected: FAIL — `--yes` is accepted but the destination stays empty, because
Task 2 stops at the plan.

- [ ] **Step 3: Resolve the journal BEFORE the question**

Immediately after printing the plan and before any prompt, and **outside** the
`if !yes`:

```rust
    // El journal se resuelve AQUÍ, antes de preguntar y antes de escribir, y no
    // en la primera mutación: lo que se está decidiendo es si se reescribe un
    // subárbol, y «esto no se va a poder deshacer» es parte de la pregunta, no
    // una nota a pie después del sí. FUERA del `if !yes` porque con `--yes` no
    // hay pregunta que completar pero sigue habiendo un log que alguien lee, y
    // ese es justamente el camino donde nadie mira la pantalla.
    if !backend.ensure_journal().await {
        eprintln!("norte: {}", norte_i18n::t("cli-sync-unjournalled"));
        return Ok(ExitCode::from(2));
    }
```

**It STOPS; it does not warn and carry on.** `Engine::sync_apply_as` refuses
anyway a few lines further down (`Unsupported`), so continuing only moves where
the "no" appears and makes it harder to read — and the string already says
norte refuses. Nor is it a corner case: the embedded journal is the SAME
`journal.db` a daemon opens exclusively, so this is the DEFAULT outcome for
every user with a live `ntc` or daemon. The message must name `--daemon`,
because going through that daemon rather than fighting it for the file is the
actual remedy. (`ai_cmd` warns and proceeds, and may: its string is an honest
warning and it genuinely does proceed. Do not copy its shape here.)

**Verified for you:** `Engine::ensure_journal()` is `async` and lives on
`Engine`; `Backend::is_journalled()` is synchronous and lives on `Backend`.
There is **no** `Backend::ensure_journal`, and `ai_cmd` gets away with it only
because it holds an `Engine` directly. `sync_cmd` has a `Backend`, which may be
`Remote`. Add `Backend::ensure_journal()` — embedded arm delegates to the
engine, remote arm answers `true` (the daemon owns the journal and refuses to
start without one) — rather than special-casing at the call site. That is a new
public method on `norte-core`, so it needs rustdoc saying what each arm means.

- [ ] **Step 4: Ask, and apply**

First the last gate, which comes BEFORE any question is printed:

```rust
    // Ni un paso que escriba: todo son omisiones. No hay nada que aprobar, y
    // aplicar no cambiaría un byte — pero la diferencia que las provocó
    // tampoco se ha resuelto, así que no es un 0.
    if plan.acting() == 0 {
        eprintln!("norte: {}", norte_i18n::t("cli-sync-nothing-to-apply"));
        return Ok(ExitCode::from(2));
    }
```

With step 3's `executable` and `integrity` gates this establishes every
condition of `SyncPlan::can_approve()`, and that matters twice. `sync.apply`
runs the **retained** plan, whole, so asking about a list that is not that plan
is asking a human to approve blind. And `SyncPlan::confirmation()` returns
`None` when `!can_approve()`, so without these gates the LEAST trustworthy
plans would be the ones asking with a bare `[y/N]` and no "this deletes N
trees" sentence.

```rust
    if !yes {
        use std::io::{IsTerminal as _, Write as _};
        // Sin terminal no hay a quién preguntar: se rehúsa ANTES, igual que el
        // prompt TOFU de este mismo fichero.
        if !std::io::stdin().is_terminal() {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-noninteractive"));
            return Ok(ExitCode::from(2));
        }
        if let Some(confirmation) = plan.confirmation(norte_i18n::active()) {
            eprintln!("{}", confirmation.text);
        }
        eprint!("{} ", norte_i18n::t("cli-sync-confirm"));
        std::io::stderr().flush().ok();
        // stdin es bloqueante: fuera del reactor (regla 2).
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map(|_| s)
        })
        .await
        .context(norte_i18n::t("cli-confirm-read"))??;
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            println!("{}", norte_i18n::t("cli-sync-abort"));
            return Ok(ExitCode::from(2));
        }
    }
```

Three things in there are not decoration:

- **A decline is a 2, never a 0.** Nothing was written, and 0 in this command
  means "there was nothing to do" — which is what `norte sync src dst && echo
  in-sync` prints, from a cron, having copied nothing. It also contradicts the
  other half of the same command: `--dry-run` on those two trees answers 1.
- **Refuse before prompting when stdin is not a terminal**, naming `--yes`. The
  precedent is the TOFU host-key prompt in this same file (`if
  !std::io::stdin().is_terminal() { bail!(…) }`). Treating the EOF of a `<
  /dev/null` as a "no" writes exactly as little, but nobody is there to read
  why; a message naming the remedy is read tomorrow, in the log.
- **`read_line` runs in `spawn_blocking` and its error PROPAGATES** — blocking
  stdin inside the reactor is hard rule 2 (the correct pattern is a few hundred
  lines up, with the comment that says so), and a swallowed `.ok()` makes an
  EOF, a read error and a deliberate "no" indistinguishable.

The prompt goes to **stderr** and the plan to **stdout**, as `ai_cmd` does, so
`norte sync … | less` still shows a question.

Then: `backend.sync_apply(plan.done().plan_hash).await?`, `task.join().await`,
and `backend.sync_report(task_id).await?`. Print the counts, then one line per
`SyncFailure`. **Exit 1 only for an apply that finished with no failures at
all**; `report.failed > 0` is 2, and so is every error out of `sync_apply` or
`sync_report`: a stale plan (`SYNC_PLAN_TTL_MS` is ten minutes against the
spool file's mtime, and the prompt above blocks on a human with no timeout —
eleven minutes spent thinking about a `mirror` is exactly the case that second
confirmation is FOR), a missing journal, a plan that is not executable. A bare
`?` on either call routes them through `main`'s `ExitCode::FAILURE`, which is
the 1 that means "applied cleanly".

**The confirmation text must say what `mirror` deletes.**
`SyncPlan::confirmation(lang)` already computes it from `dest_trash` and the
counts — use it; do not write a second sentence about deletion.

**Take the spool down on the way out.** `sync.plan` retains the plan in a file
under the state directory that names BOTH trees. The daemon sweeps at startup
and calls `Spool::drop_connection` whenever a connection closes; the CLI has
neither, so `--dry-run` and every declined prompt would leave one behind. Call
`Backend::drop_retained_plans()` on **every** exit path of `sync_cmd` — a thin
wrapper around the body, so the `?`s are covered too. Do NOT add a startup
sweep: this process shares the state directory with a possibly-live daemon and
holds no journal lock with which to prove otherwise. What the wrapper cannot
cover is a Ctrl+C during planning, which orphans a `.part` that no TTL reaps
(the TTL only looks at closed plans): that is #180.

- [ ] **Step 5: Add the Fluent ids**

`cli-sync-confirm`, `cli-sync-abort`, `cli-sync-unjournalled`,
`cli-sync-noninteractive`, `cli-sync-blocked`, `cli-sync-blocker`,
`cli-sync-blockers-more`, `cli-sync-integrity`, `cli-sync-nothing-to-apply`,
`cli-sync-done`, `cli-sync-failed`. Both locales.

- [ ] **Step 6: Run the tests and watch them pass**

```sh
just t norte-cli
```

Expected: PASS, all five in the file.

- [ ] **Step 7: Lint and commit**

```sh
just c
git add crates/norte-cli/src/main.rs crates/norte-i18n/i18n/*.ftl \
        crates/norte-cli/tests/sync_e2e.rs
git commit -m "feat(cli): norte sync applies the plan the human approved"
```

---

## Task 4: The GUI reference sheet stops lying (#161, the independent half)

`pane.sync-dirs` is `Live` in the keymap catalogue, so every frontend's
reference sheet claims the command exists. The GUI has no such command, so its
sheet must show `NotHere` — which is what it already does for other TUI-only
commands, and what it fails to do for this one. Phase C flips it back.

Second, unrelated to the sheet and recorded in the same issue: `norte-gui`
hardcodes `journalled: true`. The literal is correct today (the GUI is
remote-only, so it always has a daemon) but it is a literal where the TUI has a
fact, and it becomes wrong the moment the GUI gains an embedded backend.

**`norte-gui` is excluded from the workspace and has its own gate: `just gui-ci`,
not `just ci`.**

**Files:**
- Modify: `crates/norte-gui/src/keymap.rs` (or wherever the GUI declares its `NotHere` set — grep for the other TUI-only commands)
- Modify: whichever GUI file has the `journalled: true` literal (grep for it)

- [ ] **Step 1: Find both sites**

```sh
grep -rn "NotHere" crates/norte-gui/src/
grep -rn "journalled" crates/norte-gui/src/
```

- [ ] **Step 2: Write the failing test**

The GUI's reference sheet has tests where the other `NotHere` commands are
asserted. Add `pane.sync-dirs` to that assertion in the same style — read the
neighbouring test and match it, rather than inventing a new harness.

- [ ] **Step 3: Run it and watch it fail**

```sh
just gui-ci
```

Expected: FAIL on the new assertion.

- [ ] **Step 4: Make both edits**

`pane.sync-dirs` into the `NotHere` set, and `journalled: true` →
`Backend::is_journalled()`.

- [ ] **Step 5: Run the GUI gate**

```sh
just gui-ci
```

Expected: PASS.

- [ ] **Step 6: Commit**

```sh
git add crates/norte-gui/src/
git commit -m "fix(gui): the reference sheet stops claiming a sync command the GUI lacks"
```

---

## Task 5: Close the branch

- [ ] **Step 1: Dispatch the reviewers**

Per `CLAUDE.md`'s review workflow, the agent that did the work dispatches them
before this commit. For this branch, by surface:

- `encoding-auditor` — tasks 1–3. Every row and every step prints a filename
  that came off a filesystem the user may not control, and the masking is the
  defence.
- `rust-reviewer` — the whole diff.

No `protocol-guardian`: nothing here touches `norte-proto` or a JSON-RPC
handler. If something did, the phase has left the spec and should stop.

Give them the commit range, and the two questions actually open: whether the
exit codes can ever say "same" for a comparison that did not complete, and
whether the confirmation can be reached before the whole plan has been printed.

Apply BLOCKER and MAJOR findings in ONE pass. Say which MINORs were skipped.

- [ ] **Step 2: One gate run**

```sh
just ci
```

Expected: green. Note that `just ci` does **not** cover `norte-gui` beyond a
`cargo check` — Task 4's real gate was `just gui-ci` and it ran in that task.

- [ ] **Step 3: Update the issues**

#162 stays OPEN — this is its CLI half; the MCP half is phase B. Comment on it
with the SHAs and what is left. #161 also stays OPEN — Task 4 was its
independent piece, the GUI surface is phase C. Comment likewise.

- [ ] **Step 4: Merge**

Follow `superpowers:finishing-a-development-branch`.
