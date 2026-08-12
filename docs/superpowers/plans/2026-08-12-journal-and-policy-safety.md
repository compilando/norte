# Journal and Policy Safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three holes where hard rule 4 ("send every mutation through
the journal") and hard rule 9 (policy gate) are broken in the wild rather than
in theory — #160, #166 and #167.

**Architecture:** Three independent changes on the same theme, ordered cheapest
first. (1) `Engine` records whether a policy was ever installed, and
`Daemon::bind_with_policy` warns when it was not, so "AllowAll by omission" is
visible in the operator's log. (2) The two places that trash a node and *then*
write its journal row compensate on a journal failure — `restore_from` puts the
victim back, so a broken journal leaves the tree untouched instead of leaving a
file moved and unrecorded. (3) The embedded backend opens the real
`SqliteJournal` in the state directory, so the TUI and CLI without a daemon are
journalled and undoable like the daemon is; SQLite's `locking_mode=EXCLUSIVE`
already makes the single-writer rule enforceable, so a second embedded process
(or a running daemon) is detected at open time and falls back to today's
unjournalled engine with a loud warning.

**Tech Stack:** Rust, tokio, `sqlx`/SQLite (journal, unchanged schema),
`tracing`, `nextest`, `norte-testkit`'s `MemProvider`.

**Issues closed:** #160, #166, #167. **Decisions taken:** #167 → option 1 of the
issue (give the embedded engine a journal), #166 → option 1 (start-up `warn!`;
the type-level change stays out, it is a breaking embedding-API change and wants
its own ADR).

**Not in this plan:** #164 (`RESOLVE_BENEATH`). It needs a provider-surface
design — the `Provider` trait is path-based and has no notion of the caller's
root, so "open beneath this root" is a new capability, not a syscall swap. It
gets its own spec.

**Also not in this plan:** the `created`-after-`trashed` half of an `Overwrite`
pair (recorded as a deliberately-unfixed MAJOR in task 9 of
`docs/superpowers/plans/2026-08-11-directory-sync.md`). Compensating it means
deleting the copy that just landed *and* restoring from the trash, i.e. two more
unjournalled mutations on the journal-is-broken path. #160 stays open for that
half; Task 2 adds the rustdoc pointer.

## Progress

| task | state | commit |
| --- | --- | --- |
| 1 — the policy that was never installed (#166) | done | f5ec90f |
| 2 — a trash that outlives its journal row (#160, sync) | done | 9bd0630 |
| 3 — the same shape in `fs.delete` (#160, ops) | done | df479a1 |
| 4 — the embedded engine gets a journal (#167) | done | 2db842c |
| 5 — close the branch | pending | |

---

## Task 1: The policy that was never installed (#166)

`Engine`'s default policy is `AllowAll`, and nothing distinguishes "deliberately
permissive" from "never installed". `sync.apply` is the first method whose blast
radius under that hole is a whole subtree. This task makes the condition
visible; it does not change behaviour.

**Files:**
- Modify: `crates/norte-core/src/engine.rs` (the `Engine` struct near line 60, `with_observer` near line 132, `with_journal` near line 172, `with_policy` near line 196)
- Modify: `crates/norte-core/src/daemon/server.rs` (`bind_with_policy`, near line 486)
- Test: `crates/norte-core/src/engine.rs` (its `mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` at the bottom of `crates/norte-core/src/engine.rs`:

```rust
    /// #166: un `Engine` sin `with_policy` gatea con `AllowAll`, y eso no se
    /// distingue de una policy permisiva a propósito. El daemon avisa en el
    /// arranque, y para avisar necesita poder PREGUNTARLO.
    #[test]
    fn una_policy_por_omision_se_distingue_de_una_instalada() {
        let sin = Engine::new();
        assert!(
            !sin.has_explicit_policy(),
            "un Engine recién hecho no tiene policy instalada"
        );

        let con = Engine::new().with_policy(
            Arc::new(crate::policy::AllowAll),
            Arc::new(crate::approval::DenyAll),
        );
        assert!(
            con.has_explicit_policy(),
            "AllowAll instalada A PROPÓSITO sí cuenta como policy"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-core
```

Expected: FAIL, `no method named 'has_explicit_policy' found for struct 'Engine'`.

- [ ] **Step 3: Add the flag and the accessor**

In the `Engine` struct in `crates/norte-core/src/engine.rs`, right below the
`policy` field, add:

```rust
    /// Si la `policy` de arriba la instaló ALGUIEN ([`Self::with_policy`]) o es
    /// la de por omisión.
    ///
    /// No cambia ninguna decisión: `AllowAll` gatea igual de permisivo en los
    /// dos casos. Existe porque el daemon tiene que poder AVISAR de la segunda
    /// (#166) — «policy permisiva a propósito» y «policy que nadie instaló» son
    /// la misma cosa para el gate y cosas distintas para el operador.
    policy_explicit: bool,
```

Add `policy_explicit: false,` to the struct literals in `with_observer` and in
`with_journal` (both initialise `policy: Arc::new(crate::policy::AllowAll)`).

In `with_policy`, after `self.approvals = approvals;`, add:

```rust
        self.policy_explicit = true;
```

And add the accessor, next to `with_policy`:

```rust
    /// Si alguien llamó a [`Self::with_policy`] sobre este engine.
    ///
    /// Lo consulta el daemon en el arranque: montarse sobre un engine sin
    /// policy explícita deja pasar a CUALQUIER actor, agentes incluidos, y
    /// `sync.apply` bajo ese hueco es una llamada que reescribe un subárbol
    /// (#166). No es un gate — es lo que hace falta para que el hueco salga en
    /// el log en vez de en la sorpresa.
    #[must_use]
    pub fn has_explicit_policy(&self) -> bool {
        self.policy_explicit
    }
```

- [ ] **Step 4: Run the test and watch it pass**

```sh
just t norte-core
```

Expected: PASS.

- [ ] **Step 5: Warn at bind time**

In `crates/norte-core/src/daemon/server.rs`, inside `bind_with_policy`, before
the `spawn_blocking` that resolves the socket:

```rust
        // #166: el gate del engine es `AllowAll` mientras nadie instale una
        // policy, y un daemon sobre ese engine no gatea NADA — ni siquiera a un
        // agente. Ningún binario nuestro llega aquí así (`daemon run` instala
        // `ScopedPolicy`), pero un embebedor o un harness sí puede, y el hueco
        // no tiene hoy ni una línea de log. `sync.apply` es lo que cambia las
        // consecuencias: una llamada, un hash, y un `Mirror` reescribe y borra.
        if !engine.has_explicit_policy() {
            tracing::warn!(
                "daemon montado sobre un engine SIN policy: toda operación de \
                 todo actor pasa (AllowAll por omisión). Instala una policy con \
                 Engine::with_policy antes de bind (#166)."
            );
        }
```

- [ ] **Step 6: Rustdoc on `bind_with_policy`**

Append to the rustdoc of `bind_with_policy`, after the existing `# Errors`
section:

```rust
    /// # Policy
    /// Un engine sobre el que nadie llamó a
    /// [`Engine::with_policy`](crate::Engine::with_policy) gatea con `AllowAll`:
    /// este bind lo AVISA por `warn!` y sigue (#166). No se rechaza porque
    /// «permisivo a propósito» es una configuración legítima; lo que no puede
    /// ser es indistinguible de un olvido.
```

- [ ] **Step 7: Lint and commit**

```sh
just c
git add crates/norte-core/src/engine.rs crates/norte-core/src/daemon/server.rs
git commit -m "fix(core): a daemon over a policy-less engine says so, in the log"
```

---

## Task 2: A trash that outlives its journal row (#160, sync)

Hit live under the tmux harness: `sync.apply` trashed the destination and then
failed to write the journal row, so the file had moved and nothing recorded it.
Task 11b of the sync plan made the fix possible — `trash_fdo` now returns the
exact `files/<name>` path, and `Provider::restore_from` puts it back.

**Files:**
- Modify: `crates/norte-core/src/sync/exec.rs` (`bury`, near line 626)
- Test: `crates/norte-core/src/sync/exec.rs` (its `mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/norte-core/src/sync/exec.rs`, after the existing
`Recorder`:

```rust
    /// Un recorder que falla EXACTAMENTE en `trashed`, que es el arma del #160:
    /// la papelera se llevó el fichero y la fila no llegó.
    #[derive(Default)]
    struct TrashedFalla;

    #[async_trait]
    impl StepJournal for TrashedFalla {
        async fn created(&self, _path: &VPath, _reversal: Reversal) -> Result<(), Error> {
            Ok(())
        }
        async fn trashed(&self, _path: &VPath, _dest: Option<&VPath>) -> Result<(), Error> {
            Err(Error::Io { retryable: false })
        }
        async fn removed(&self, _path: &VPath) -> Result<(), Error> {
            Ok(())
        }
    }
```

And the test itself, at the end of `mod tests`:

```rust
    /// #160: si la fila de journal NO llega DESPUÉS de haber enterrado el
    /// destino, el fichero está movido y sin registrar — regla dura 4 rota en
    /// vivo. La compensación es sacarlo de la papelera: el paso falla, la Task
    /// para, y el destino se queda con sus bytes originales.
    #[tokio::test]
    async fn un_journal_que_falla_tras_enterrar_devuelve_el_fichero_a_su_sitio() {
        let mem = Arc::new(MemProvider::new().with_logical_trash());
        mem.mkdir(&vp("mem:///s")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
        write(&mem, "mem:///d/a.txt", b"viejo").await;
        let t = targets(&mem);

        let err = super::bury(
            &t,
            &TrashedFalla,
            &vp("mem:///d/a.txt"),
            1,
            &ctx(CancellationToken::new()),
        )
        .await
        .expect_err("el journal falló");
        assert!(
            matches!(err, StepError::Fatal(Error::Io { .. })),
            "el fallo de journal para la Task: {err:?}"
        );

        assert_eq!(
            read(&mem, "mem:///d/a.txt").await,
            b"viejo",
            "el destino volvió de la papelera con sus bytes"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-core
```

Expected: FAIL — `read` panics with `read` on `mem:///d/a.txt`, because the file
is still buried in `mem:///.norte-trash/...`.

- [ ] **Step 3: Compensate inside `bury`**

Replace the error arm of `bury` in `crates/norte-core/src/sync/exec.rs` (the
`if let Err(e) = recorder.trashed(...)` block) with:

```rust
    if let Err(e) = recorder.trashed(to, buried.as_ref()).await {
        // Regla dura 4 al revés: el efecto ocurrió y su fila no. Lo único que
        // deja el árbol como estaba es DESHACERLO aquí, y desde task 11b se
        // puede: `trash()` devuelve la ruta exacta de lo enterrado y
        // `restore_from` la devuelve a su sitio.
        //
        // Solo cuando la papelera NOMBRA lo que se llevó. Con `DestTrash::Opaque`
        // (macOS, Windows) no hay a qué apuntar y la línea de log sigue siendo
        // toda la respuesta.
        let devuelto = match buried.as_ref() {
            Some(en) => targets.dest.restore_from(en, to).await,
            None => Err(Error::Unsupported),
        };
        // El `reversal_ref` es la ÚNICA pista de dónde fue a parar el fichero, y
        // acaba de no quedar en el journal. Se escribe en el log del operador
        // antes de morir: sin esto, «¿dónde está mi fichero?» no lo contesta
        // nadie. Se dice ADEMÁS si la compensación llegó — un `restore_from` que
        // también falla deja el fichero enterrado, y eso el operador lo necesita
        // saber en la misma línea.
        tracing::error!(
            error = %e,
            enterrado = %crate::engine::span_path(to),
            en = buried.as_ref().map(crate::engine::span_path),
            devuelto = devuelto.is_ok(),
            "sync.apply: se enterró el destino y su entrada de journal NO llegó",
        );
        return Err(StepError::Fatal(e));
    }
```

- [ ] **Step 4: Run the test and watch it pass**

```sh
just t norte-core
```

Expected: PASS.

- [ ] **Step 5: Say in the rustdoc what is compensated and what is not**

Replace the rustdoc line of `bury` (`/// Entierra `to` en la papelera y lo
journaliza.`) with:

```rust
/// Entierra `to` en la papelera y lo journaliza.
///
/// # La fila que no llega
/// Si el journal falla DESPUÉS del entierro, el efecto ocurrió y su registro no
/// (regla dura 4 rota en vivo — #160). Se compensa: `restore_from` devuelve lo
/// enterrado a su ruta y el paso muere con el destino intacto. Dos límites, y
/// los dos van al log:
///
/// - `restore_from` puede fallar a su vez, y entonces la línea de log es todo
///   lo que queda;
/// - con una papelera que no NOMBRA lo que se lleva (`DestTrash::Opaque`:
///   macOS, Windows) no hay adónde apuntar y no hay compensación posible.
///
/// **Lo que sigue sin compensarse** es la otra mitad del par de un `Overwrite`:
/// un `created` que falla DESPUÉS de un `trashed` que sí quedó deja un lote
/// cuyo undo BLOQUEA. Devolverlo pediría borrar la copia recién puesta Y
/// desenterrar, o sea dos mutaciones más por el camino en el que el journal ya
/// no funciona. #160 sigue abierta por esa mitad.
```

- [ ] **Step 6: Lint and commit**

```sh
just c
git add crates/norte-core/src/sync/exec.rs
git commit -m "fix(core): a journal that fails after a trash puts the file back"
```

---

## Task 3: The same shape in `fs.delete` (#160, ops)

`ops::delete_task`'s `Trash` branch has exactly the shape of Task 2: trash, then
`observer.on_mutation(Trashed)` with `?`. It is the *other* live instance, and it
is the one an ordinary F8 goes through.

**Files:**
- Modify: `crates/norte-core/src/ops.rs` (`delete_task`, the `DeleteMode::Trash` branch, near line 1509)
- Test: `crates/norte-core/src/ops.rs` (its `mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/norte-core/src/ops.rs`:

```rust
    /// #160, la misma forma que en `sync::exec::bury` y por el camino que anda
    /// un F8: la papelera se llevó el fichero y el observer del journal falló
    /// después. Se devuelve, y el borrado falla con el árbol como estaba.
    #[tokio::test]
    async fn un_observer_que_falla_tras_enterrar_devuelve_el_fichero() {
        use norte_proto::{DeleteMode, Error, TaskId, TaskKind, VPath};
        use norte_testkit::MemProvider;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;

        /// Observer que dice que no a TODO: lo que se prueba es el camino en el
        /// que la mutación ya ocurrió y su registro no llega.
        #[derive(Debug)]
        struct ObserverFalla;

        #[async_trait::async_trait]
        impl crate::MutationObserver for ObserverFalla {
            async fn on_mutation(
                &self,
                _mutation: &crate::Mutation<'_>,
                _actor: &crate::journal::Actor,
            ) -> Result<(), Error> {
                Err(Error::Io { retryable: false })
            }
        }

        let mem = Arc::new(MemProvider::new().with_logical_trash());
        let path = VPath::parse("mem:///a.txt").expect("wire");
        {
            let mut sink = mem.write(&path).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"vivo"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        // El mismo `TaskCtx` a mano que arman los tests vecinos de este módulo
        // (no hay helper compartido; no se añade uno para un solo test más).
        let (reporter, _rx) = ProgressReporter::new(TaskId::new(1), TaskKind::Delete);
        let ctx = TaskCtx {
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: Actor::User,
        };

        let err = super::delete_task(
            Arc::clone(&mem) as Arc<dyn norte_vfs::Provider>,
            path.clone(),
            DeleteMode::Trash,
            Arc::new(ObserverFalla),
            &ctx,
        )
        .await
        .expect_err("el observer falló");
        assert!(matches!(err, Error::Io { .. }), "{err:?}");

        assert!(
            mem.stat(&path).await.is_ok(),
            "el fichero volvió de la papelera a su ruta"
        );
    }
```

**Note for the implementer:** `ProgressReporter`, `TaskCtx` and `Actor` are
already in scope in `ops.rs`'s `mod tests` — the neighbouring cancellation tests
build exactly this `TaskCtx`. Check the `TaskKind` variant name for a delete
(`TaskKind::Delete`) against `norte-proto` and use whatever it actually is.

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-core
```

Expected: FAIL on the last assertion — the file is in `mem:///.norte-trash/…`,
not at `mem:///a.txt`.

- [ ] **Step 3: Compensate in `delete_task`**

In `crates/norte-core/src/ops.rs`, replace the `observer.on_mutation(...)` call
of the `Trash` branch (the one carrying `Mutation::Trashed`) with:

```rust
        if let Err(e) = observer
            .on_mutation(
                &Mutation::Trashed {
                    path: &path,
                    dest: dest.as_ref(),
                },
                &ctx.actor,
            )
            .await
        {
            // #160: el fichero ya está enterrado y su registro no llegó — regla
            // dura 4 rota por el camino de un F8 corriente. Se devuelve a su
            // ruta y el borrado falla con el árbol como estaba. Igual que en
            // `sync::exec::bury`, solo se puede cuando la papelera NOMBRA lo que
            // se lleva: con `DestTrash::Opaque` (macOS, Windows) queda la línea
            // de log.
            let devuelto = match dest.as_ref() {
                Some(en) => provider.restore_from(en, &path).await,
                None => Err(Error::Unsupported),
            };
            tracing::error!(
                error = %e,
                enterrado = %crate::engine::span_path(&path),
                en = dest.as_ref().map(crate::engine::span_path),
                devuelto = devuelto.is_ok(),
                "fs.delete: se enterró el fichero y su entrada de journal NO llegó",
            );
            return Err(e);
        }
```

- [ ] **Step 4: Run the test and watch it pass**

```sh
just t norte-core
```

Expected: PASS.

- [ ] **Step 5: Rustdoc**

Append to the rustdoc of `delete_task`:

```rust
/// # La fila que no llega
/// Un observer que falla DESPUÉS de un entierro exitoso deja el fichero movido
/// y sin registrar (#160). Se compensa con `restore_from` cuando la papelera
/// nombra lo que se llevó, y se dice en el log llegue o no. Mismo criterio que
/// [`crate::sync::exec`]: el efecto no se queda huérfano de su registro.
```

- [ ] **Step 6: Lint and commit**

```sh
just c
git add crates/norte-core/src/ops.rs
git commit -m "fix(core): fs.delete puts a trashed file back when its journal row fails"
```

---

## Task 4: The embedded engine gets a journal (#167)

`make_backend` builds `Engine::new()` — no journal — so the embedded TUI and
`norte-cli` copy, move, delete, rename and trash with nothing recording it,
while `sync.apply` is fail-closed on exactly the same rule. This task takes
option 1 of #167: the embedded process opens the real journal.

The single-writer rule (spec §4) is enforceable because `Journal::open` already
opens SQLite with `locking_mode=EXCLUSIVE`: a second embedded process, or a
running daemon, loses the race at open time and gets `database is locked`. That
case falls back to today's unjournalled engine with a `warn!` — refusing to
start would turn "a daemon is running" into "the CLI does not work".

**Files:**
- Create: `crates/norte-core/src/embedded.rs`
- Modify: `crates/norte-core/src/lib.rs` (module declaration and re-export)
- Modify: `crates/norte-cli/src/main.rs` (`make_backend`, near line 1520)
- Modify: `crates/norte-tui/src/main.rs` (`make_backend`, near line 2095)
- Test: `crates/norte-core/tests/embedded_journal.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/norte-core/tests/embedded_journal.rs`:

```rust
//! #167: el engine embebido lleva journal, y DOS a la vez no.

use norte_core::embedded::EmbeddedJournal;

/// Un directorio de estado limpio da un engine journalizado.
#[tokio::test]
async fn el_primero_se_lleva_el_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abierto = EmbeddedJournal::open_in(dir.path()).await;
    assert!(
        matches!(abierto, EmbeddedJournal::Owned(_)),
        "un directorio de estado libre da journal"
    );
}

/// El segundo proceso NO forkea la cadena: el lock exclusivo de SQLite lo
/// rechaza, y el engine sale sin journal en vez de sin arrancar.
#[tokio::test]
async fn el_segundo_se_queda_sin_journal_pero_arranca() {
    let dir = tempfile::tempdir().expect("tempdir");
    let primero = EmbeddedJournal::open_in(dir.path()).await;
    assert!(matches!(primero, EmbeddedJournal::Owned(_)));

    let segundo = EmbeddedJournal::open_in(dir.path()).await;
    assert!(
        matches!(segundo, EmbeddedJournal::Busy),
        "el segundo no puede escribir la misma cadena: {segundo:?}"
    );
}

/// Soltar al primero libera el lock: el siguiente vuelve a llevárselo.
#[tokio::test]
async fn soltarlo_devuelve_el_journal_al_siguiente() {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(EmbeddedJournal::open_in(dir.path()).await);
    let otra_vez = EmbeddedJournal::open_in(dir.path()).await;
    assert!(matches!(otra_vez, EmbeddedJournal::Owned(_)));
}
```

- [ ] **Step 2: Run it and watch it fail**

```sh
just t norte-core
```

Expected: FAIL, `unresolved import 'norte_core::embedded'`.

- [ ] **Step 3: Write `embedded.rs`**

Create `crates/norte-core/src/embedded.rs`:

```rust
//! El journal del proceso EMBEBIDO (#167).
//!
//! La regla dura 4 —«toda mutación pasa por el journal»— se afirmaba en
//! `CLAUDE.md` y se cumplía en un solo método: `sync.apply` exige journal y
//! rehúsa sin él, mientras copiar, mover, borrar, renombrar y enterrar por el
//! transporte embebido no registraban nada. Este módulo es la otra mitad: el
//! TUI y el CLI sin daemon abren EL journal del directorio de estado, el mismo
//! que abriría el daemon.
//!
//! # Un solo escritor, y quién se lo queda
//!
//! La cadena de hashes del journal asume un dueño único (spec §4, ADR 0024), y
//! quien lo impone no es una convención: [`crate::journal::Journal::open`] abre
//! SQLite con `locking_mode=EXCLUSIVE`. Un segundo proceso —otro TUI embebido, o
//! el daemon corriendo— pierde la carrera al abrir con `database is locked`.
//!
//! Ese caso NO impide arrancar. Un `norte cp` que deja de funcionar porque hay
//! un daemon vivo sería peor que la falta de registro que arregla esto: se
//! sigue sin journal, se avisa por `warn!`, y el TUI ya atenúa lo que exige
//! journal (`pane.sync-dirs`) con su razón visible.

use std::path::Path;
use std::sync::Arc;

/// Qué journal le tocó a este proceso embebido.
#[derive(Debug)]
pub enum EmbeddedJournal {
    /// Este proceso es el dueño único: las mutaciones se registran y se
    /// deshacen.
    Owned(Arc<crate::journal::SqliteJournal>),
    /// Otro proceso —un daemon, u otro embebido— tiene el lock exclusivo. Se
    /// sigue sin registro, avisando.
    Busy,
    /// El journal no se pudo abrir por una razón que no es el lock (permisos,
    /// disco, DB corrupta). Se sigue sin registro, avisando, con el motivo.
    Unavailable(String),
}

impl EmbeddedJournal {
    /// Abre `<state>/journal.db` para este proceso.
    ///
    /// Nunca falla: las tres salidas son estados, no errores. Quien llama
    /// decide qué engine construir con cada una — ver
    /// [`Self::into_engine`].
    pub async fn open_in(state_dir: &Path) -> Self {
        let path = state_dir.join("journal.db");
        match crate::journal::SqliteJournal::open(&path).await {
            Ok(j) => Self::Owned(Arc::new(j)),
            Err(e) if es_lock_ocupado(&e) => Self::Busy,
            Err(e) => Self::Unavailable(e.to_string()),
        }
    }

    /// El engine que corresponde, con el aviso ya escrito en el log.
    ///
    /// `Owned` da [`crate::Engine::with_journal`] (undo real, cadena
    /// encadenada); los otros dos dan [`crate::Engine::new`], que es lo que
    /// había antes de #167.
    #[must_use]
    pub fn into_engine(self) -> crate::Engine {
        match self {
            Self::Owned(j) => crate::Engine::with_journal(j),
            Self::Busy => {
                tracing::warn!(
                    "otro proceso tiene el journal (daemon, u otra sesión embebida): \
                     esta sesión NO registra sus mutaciones y no las puede deshacer (#167)"
                );
                crate::Engine::new()
            }
            Self::Unavailable(motivo) => {
                tracing::warn!(
                    motivo = %motivo,
                    "el journal no se pudo abrir: esta sesión NO registra sus \
                     mutaciones y no las puede deshacer (#167)"
                );
                crate::Engine::new()
            }
        }
    }
}

/// Si este error de apertura es «lo tiene otro», y no un problema de verdad.
///
/// SQLite lo dice en el TEXTO (`database is locked`), no en un código que
/// `sqlx` exponga tipado por esta ruta; se mira el texto, en minúsculas, para
/// no atarse a la capitalización de una versión.
fn es_lock_ocupado(e: &crate::journal::JournalError) -> bool {
    let texto = e.to_string().to_lowercase();
    texto.contains("database is locked") || texto.contains("database table is locked")
}
```

**Note for the implementer:** verify the `es_lock_ocupado` predicate against the
real error before trusting it — the second test in Step 1 is exactly that check,
and if it comes back `Unavailable(..)` instead of `Busy`, print the string and
match on what SQLite actually says. Do not widen the predicate to "any error".

- [ ] **Step 4: Declare the module**

In `crates/norte-core/src/lib.rs`, next to the other `pub mod` declarations:

```rust
pub mod embedded;
```

- [ ] **Step 5: Run the tests and watch them pass**

```sh
just t norte-core
```

Expected: PASS, all three.

- [ ] **Step 6: Wire the CLI**

In `crates/norte-cli/src/main.rs`, find where the `Engine` handed to
`make_backend` is built (the caller, not `make_backend` itself) and replace the
`Engine::new()` with the journalled one. The state directory is
`norte_core::connect::config_dir()` — the same one `daemon run` uses at line
1584, and that sameness is the point: undo has to see the daemon's entries and
the daemon has to see the embedded ones.

```rust
    let engine = norte_core::embedded::EmbeddedJournal::open_in(
        &norte_core::connect::config_dir(),
    )
    .await
    .into_engine();
```

Keep every `set_archive_limits` / `with_*` call that already followed the
construction, in the same order.

- [ ] **Step 7: Wire the TUI**

In `crates/norte-tui/src/main.rs`, in `make_backend`, replace the
`let engine = Engine::new();` of the `!want_daemon` branch with the same three
lines, leaving the `set_archive_limits` block that follows untouched:

```rust
        // #167: el transporte embebido registra sus mutaciones (regla dura 4).
        // Si otro proceso tiene el journal, se sigue sin él y se avisa — ver
        // `norte_core::embedded`.
        let engine = norte_core::embedded::EmbeddedJournal::open_in(
            &norte_core::connect::config_dir(),
        )
        .await
        .into_engine();
```

- [ ] **Step 8: Check both frontends build and their suites pass**

```sh
just t norte-cli
just t norte-tui
```

Expected: PASS. If a TUI test asserted that the embedded backend has no journal
(`undo_session` → `Unsupported`), that assertion is now wrong *as an assertion
about the embedded transport* — fix it to build its engine explicitly with
`Engine::new()` rather than to expect the product to be unjournalled.

- [ ] **Step 9: Correct the two documents that state the old boundary**

In `crates/norte-core/src/sync/exec.rs`, the module rustdoc section
"Regla dura 4: un plan sin journal no se aplica" says the embedded TUI has no
journal. Add after it:

```rust
//! Desde #167 el transporte embebido SÍ abre el journal del directorio de
//! estado, así que este `Unsupported` dejó de ser el caso corriente: queda para
//! el engine que de verdad no tiene journal (otro proceso con el lock, o un
//! embebedor que construyó `Engine::new()` a mano).
```

And in `docs/spec/norte-spec.md`, if §4 or the journal section states that the
embedded transport is unjournalled, correct it in one sentence. Search first:

```sh
grep -n "embebid\|embedded" docs/spec/norte-spec.md | grep -i "journal"
```

- [ ] **Step 10: Lint and commit**

```sh
just c
git add crates/norte-core/src/embedded.rs crates/norte-core/src/lib.rs \
        crates/norte-core/tests/embedded_journal.rs \
        crates/norte-core/src/sync/exec.rs \
        crates/norte-cli/src/main.rs crates/norte-tui/src/main.rs \
        docs/spec/norte-spec.md
git commit -m "feat(core): the embedded backend journals its mutations"
```

### What task 4 did differently (2db842c)

- **The CLI opens the journal only for mutating subcommands** (`muta_el_arbol`:
  Cp/Mv/Rm/Mkdir), plus `ai rename`, which never reaches `make_backend` and
  builds its own engine. `norte ls` must not take an exclusive lock away from a
  concurrent `norte mv` for an operation that writes no row.
- **`es_lock_ocupado` was rewritten to match SQLite's CODE** (`SQLITE_BUSY` /
  `SQLITE_LOCKED`), not its prose. The plan's text predicate worked — the
  contended open really does come back "database is locked" — but the
  classification would silently invert on an sqlx bump that reformats `Display`.
- **A 250 ms busy timeout** (`Journal::open_with_busy_timeout`): with sqlx's
  5 s default, a contended open cost five seconds before falling back. Measured:
  5.012s → 0.271s.
- **Two defects the plan did not foresee, both from the reviewers.** The pool
  reaped the connection that HOLDS the lock after ten idle minutes (so two
  processes could own the chain, and the loser's cached `ChainState` then
  collided with the `seq` primary key forever); and `undoes_seq` had no
  migration, which is why a `norte cp` on the dev box died with "internal
  error" — and it cannot have one, because that column arrived with its byte
  in the hash preimage, so migrating a journal WITH history would make
  `verify_chain` accuse an untouched file. Empty legacy table: migrated. With
  history: refused, with instructions.
- **Deferred with issues:** #177 (open the journal lazily on the first
  mutation, so a browsing TUI stops blocking `daemon run` and `norte audit`)
  and #178 (`Unavailable` degrades to a warning where the daemon fails closed).

---

## Task 5: Close the branch

- [ ] **Step 1: Dispatch the reviewers**

The agent that did the work dispatches them before this commit, per the review
workflow in `CLAUDE.md`. By surface:

- `security-reviewer` — Tasks 1 and 4 (policy gate, journal ownership, the
  fallback that keeps running unjournalled).
- `rust-reviewer` — the whole diff.

Give each the commit range, what the change is for, and the two questions that
are actually open: whether `es_lock_ocupado` can misclassify a real failure as
`Busy` (which would silently drop the journal), and whether the `restore_from`
compensation can itself lose data when the trash entry is stale.

Apply BLOCKER and MAJOR findings in ONE pass. Say which MINORs were skipped.

- [ ] **Step 2: One gate run**

```sh
just ci
```

Expected: green, coverage over 85%.

- [ ] **Step 3: Close the issues**

```sh
gh issue close 166 --comment "Closed by <sha>: a daemon bound over a policy-less engine warns at bind time. The type-level half (making 'no policy' unspellable) is deliberately not done — it is a breaking embedding-API change and wants its own ADR."
gh issue close 167 --comment "Closed by <sha>: option 1. The embedded backend opens the state directory's journal; SQLite's exclusive lock keeps the single-writer rule, and a process that loses it runs unjournalled with a warning."
```

#160 stays OPEN for the `created`-after-`trashed` half. Comment on it:

```sh
gh issue comment 160 --comment "The trash-then-failed-journal half is fixed in <sha> for both instances (sync's bury and ops::delete_task): restore_from puts the victim back when the trash names what it took, and the log line says whether it worked. Still open: the created-after-trashed half of an Overwrite pair, which would need two more unjournalled mutations to compensate."
```

- [ ] **Step 4: Merge**

Follow `superpowers:finishing-a-development-branch`.
