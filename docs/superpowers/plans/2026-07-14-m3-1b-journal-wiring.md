# M3-1b — Journal wiring en el engine — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enchufar `SqliteJournal` como observer real del engine, capturar la `reversal_ref` de las papelerizaciones lógicas, exponer el GC de staging `.norte-partial` como mecanismo del core, y probar la costura engine↔journal de extremo a extremo.

**Architecture:** El engine YA tiene la costura `MutationObserver` (`Engine::with_observer`) y `TaskCtx.actor` (M3-1a). 1b (a) añade una API pública de LECTURA del journal (`entries()`) para poder aseverar entradas desde tests de integración y futuros undo/audit; (b) cambia `Provider::trash` para que devuelva el destino recuperable (`Option<VPath>`) y lo propaga por `Mutation::Trashed { dest }` hasta la `reversal_ref`; (c) promueve `gc_partials` al trait `Provider` (default no-op) + `Engine::gc_partials` que despacha al provider — SIN barrido automático al arranque (decisión de scope: no hay raíz gestionada fiable hasta que el journal registre staging in-flight); (d) cablea `SqliteJournal::open` en los binarios como observer default.

**Tech Stack:** Rust, `sqlx` (SQLite WAL, ya en el árbol), `async_trait`, `tokio`, `nextest`, `norte-testkit` (`MemProvider`).

**Reviewers obligatorios antes de commits sustantivos (CLAUDE.md):** `rust-reviewer` siempre; `encoding-auditor` en la Task 3 (toca `VPath`/bytes de path en 5 providers). NO toca `norte-proto` ni handlers JSON-RPC → `protocol-guardian` no aplica.

---

## File Structure

- **`crates/norte-core/src/journal.rs`** — añade `JournalEntry` (struct público de lectura) + `Journal::entries()`; mapea `Mutation::Trashed { dest }` → `reversal_ref` en `SqliteJournal::on_mutation`; añade `SqliteJournal::open` + `default_db_path()`.
- **`crates/norte-core/src/lib.rs`** — reexporta `JournalEntry` (y ya reexporta `Actor`, `SqliteJournal`, `Journal`).
- **`crates/norte-core/src/observer.rs`** — `Mutation::Trashed` gana `dest: Option<&'a VPath>`.
- **`crates/norte-core/src/ops.rs`** — `delete_task` captura el `Option<VPath>` de `trash()` y lo pasa a `Mutation::Trashed`.
- **`crates/norte-core/src/engine.rs`** — `Engine::gc_partials(dir, older_than)` (despacho puntual, sin Task, sin journal).
- **`crates/norte-vfs/src/provider.rs`** — `Provider::trash -> Result<Option<VPath>, Error>`; nuevo `Provider::gc_partials` default no-op.
- **`crates/norte-vfs-local/src/provider.rs`** — `trash` devuelve `Ok(None)` (papelera NATIVA del OS, sin ruta estable); reubica el `gc_partials` inherente al `impl Provider`.
- **`crates/norte-vfs-sftp/src/provider.rs`** — `trash` devuelve `Ok(Some(paths.payload))`.
- **`crates/norte-vfs-object/src/provider.rs`** — `trash` devuelve `Ok(Some(paths.payload))`.
- **`crates/norte-testkit/src/mem.rs`** — `trash` devuelve `Ok(None)` (semántica "vanish": el subárbol desaparece, sin destino recuperable).
- **`crates/norte-vfs-object/tests/trash.rs`** — asevera que el `Some(dest)` devuelto apunta a `.norte-trash`.
- **`crates/norte-core/tests/engine_journal.rs`** (nuevo) — integración engine↔journal.
- **`crates/norte-cli/src/main.rs`**, **`crates/norte-tui/src/main.rs`** — construyen el engine con el journal como observer default.

---

## Task 1: API de lectura del journal (`entries()` + `JournalEntry`)

Necesaria para que los tests de integración (Task 4) y el futuro undo/audit puedan aseverar el contenido de las entradas (hoy solo hay `count()`/`verify_chain()`). Los tests de integración viven en OTRO crate → `pool` (`pub(crate)`) no basta: hace falta API pública.

**Files:**
- Modify: `crates/norte-core/src/journal.rs` (tras `verify_chain`, ~línea 352)
- Modify: `crates/norte-core/src/lib.rs:18` (reexport)
- Test: `crates/norte-core/src/journal.rs` (mod `tests`, ~línea 632)

- [ ] **Step 1: Test rojo — `entries()` vuelca campos en orden de `seq`**

Añade al final del `mod tests` de `journal.rs`:

```rust
    #[tokio::test]
    async fn entries_returns_fields_in_seq_order() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record("created", b"file:///a", None, Reversal::Delete, None, &Actor::User)
            .await
            .expect("r1");
        j.record(
            "renamed",
            b"file:///b",
            Some(b"file:///a"),
            Reversal::RenameBack,
            None,
            &Actor::User,
        )
        .await
        .expect("r2");

        let es = j.entries().await.expect("entries");
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].seq, 1);
        assert_eq!(es[0].op, "created");
        assert_eq!(es[0].path, b"file:///a");
        assert_eq!(es[0].reversal, "delete");
        assert_eq!(es[1].op, "renamed");
        assert_eq!(es[1].path, b"file:///b");
        assert_eq!(es[1].path_to.as_deref(), Some(&b"file:///a"[..]));
        assert_eq!(es[1].reversal, "rename_back");
    }
```

- [ ] **Step 2: Verifica que falla (no compila: `entries`/`JournalEntry` no existen)**

Run: `cargo nextest run -p norte-core journal::tests::entries_returns_fields_in_seq_order`
Expected: FAIL de compilación — `no method named entries` / `JournalEntry` no definido.

- [ ] **Step 3: Implementa `JournalEntry` + `Journal::entries()`**

En `journal.rs`, tras el cierre de `verify_chain` (antes de `corrupt_path_for_test`), añade dentro del `impl Journal`:

```rust
    /// Vuelca todas las entradas en orden de `seq`. Materializa en memoria:
    /// pensado para journals de tamaño de sesión (la paginación es deuda si
    /// crece — mismo criterio que el listado, #27). Base de lectura para el
    /// undo (M3-2) y el audit export (M3-5).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let rows = sqlx::query(
            "SELECT seq, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref \
             FROM journal ORDER BY seq ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| JournalEntry {
                seq: row.get(0),
                actor_kind: row.get(1),
                actor_id: row.get(2),
                op: row.get(3),
                path: row.get(4),
                path_to: row.get(5),
                reversal: row.get(6),
                reversal_ref: row.get(7),
            })
            .collect())
    }
```

Y, a nivel de módulo (junto a `Reversal`, antes de `pub(crate) struct Record`), el struct público:

```rust
/// Una entrada del journal materializada para LECTURA (undo M3-2, audit M3-5,
/// tests de integración). Los `path`/`path_to`/`reversal_ref` son BYTES crudos
/// de `VPath::to_wire` (regla 1): reconstruye con `VPath::from_wire` al
/// consumir, jamás asumas UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Secuencia monótona asignada al registrar.
    pub seq: i64,
    /// Origen: `"user" | "agent" | "plugin"`.
    pub actor_kind: String,
    /// Id de sesión del agente / id del plugin, si aplica.
    pub actor_id: Option<String>,
    /// Operación: `"created" | "removed" | "trashed" | "renamed"`.
    pub op: String,
    /// Path afectado (bytes `to_wire`).
    pub path: Vec<u8>,
    /// Destino de un `renamed` (bytes `to_wire`).
    pub path_to: Option<Vec<u8>>,
    /// Etiqueta de reversa persistida (`Reversal::as_str`).
    pub reversal: String,
    /// Referencia para revertir (p. ej. ruta de papelera de un `trashed`),
    /// bytes `to_wire`.
    pub reversal_ref: Option<Vec<u8>>,
}
```

- [ ] **Step 4: Reexporta `JournalEntry`**

En `crates/norte-core/src/lib.rs`, amplía el reexport del journal (junto a `Actor`, `Journal`, `SqliteJournal`):

```rust
pub use journal::{Actor, Journal, JournalEntry, SqliteJournal};
```
(Ajusta la línea existente para incluir `JournalEntry`; conserva los demás nombres ya exportados.)

- [ ] **Step 5: Verde**

Run: `cargo nextest run -p norte-core journal::tests::entries_returns_fields_in_seq_order`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core/src/journal.rs crates/norte-core/src/lib.rs
git commit -m "feat(core): Journal::entries + JournalEntry (lectura para undo/audit/tests) (M3-1b)"
```

---

## Task 2: GC de staging como mecanismo del core (`Provider::gc_partials` + `Engine::gc_partials`)

Decisión de scope (aprobada): mecanismo en el core, SIN barrido automático al arranque. `gc_partials` es single-dir/no-recursivo y el `LocalProvider` real va rooteado en `/` → no hay raíz gestionada que barrer al arranque hasta que el journal registre destinos in-flight (increment posterior). Aquí se expone el mecanismo polimórfico y el despacho del engine.

**Files:**
- Modify: `crates/norte-vfs/src/provider.rs` (trait `Provider`, junto a `trash`, ~línea 143)
- Modify: `crates/norte-vfs-local/src/provider.rs` (reubica `gc_partials` inherente ~144-174 al `impl Provider for LocalProvider`)
- Modify: `crates/norte-core/src/engine.rs` (nuevo método, tras `capabilities`, ~línea 355)
- Test: `crates/norte-core/tests/engine_gc.rs` (nuevo)

- [ ] **Step 1: Test rojo — despacho no-op y despacho a local**

Crea `crates/norte-core/tests/engine_gc.rs`:

```rust
//! `Engine::gc_partials` (#11, ADR 0012): despacha el barrido de staging
//! `.norte-partial` al provider que sirve el path. Puntual, sin Task, sin
//! journal. Sin provider de staging local → no-op.

use std::sync::Arc;
use std::time::Duration;

use norte_core::Engine;
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

#[tokio::test]
async fn gc_partials_noop_on_provider_without_local_staging() {
    let engine = Engine::new();
    engine.register_provider(Arc::new(MemProvider::new()) as Arc<dyn Provider>);
    let n = engine
        .gc_partials(&MemProvider::root(), Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(n, 0, "provider sin staging local: no-op");
}

#[tokio::test]
async fn gc_partials_dispatches_to_local_and_sweeps() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Staging huérfano en su FORMA estable exacta (prefijo + 32 hex).
    std::fs::write(
        dir.path().join(".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c"),
        b"x",
    )
    .expect("plant partial");
    // Un archivo real del usuario con el prefijo NO debe barrerse.
    std::fs::write(dir.path().join(".norte-partial.backup"), b"keep").expect("plant backup");

    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())) as Arc<dyn Provider>);

    // older_than = ZERO → cualquier antigüedad (>=0) califica: determinista.
    let n = engine
        .gc_partials(&LocalProvider::root(), Duration::ZERO)
        .await
        .expect("gc");
    assert_eq!(n, 1, "barre solo el staging con forma exacta");
    assert!(
        dir.path().join(".norte-partial.backup").exists(),
        "el backup del usuario se respeta"
    );
}
```

Asegura las dev-deps en `crates/norte-core/Cargo.toml` (`[dev-dependencies]`): `tempfile`, `norte-vfs-local`, `norte-testkit`, `norte-vfs` — añade solo las que falten (comprueba con `rg` antes de editar).

- [ ] **Step 2: Verifica que falla (compilación: `Engine::gc_partials` no existe)**

Run: `cargo nextest run -p norte-core --test engine_gc`
Expected: FAIL de compilación — `no method named gc_partials on Engine`.

- [ ] **Step 3: Añade `gc_partials` al trait `Provider` (default no-op)**

En `crates/norte-vfs/src/provider.rs`, tras el método `trash` del trait:

```rust
    /// GC de staging `.norte-partial` huérfano (ADR 0012, #11) en el directorio
    /// `dir`: borra los parciales cuya antigüedad supera `older_than`. Los
    /// reconoce por su FORMA exacta, no por prefijo suelto — un archivo real
    /// `.norte-partial.backup` JAMÁS se toca. Devuelve cuántos borró.
    ///
    /// Default no-op (`Ok(0)`): solo los providers con staging LOCAL lo
    /// implementan. NO es una mutación de usuario → no pasa por el journal.
    ///
    /// # Errors
    /// [`Error`] si `dir` no se puede listar; los fallos de borrado
    /// individuales se cuentan como no-borrados, sin abortar el barrido.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let _ = (dir, older_than);
        Ok(0)
    }
```

- [ ] **Step 4: Reubica el `gc_partials` de `LocalProvider` al `impl Provider`**

En `crates/norte-vfs-local/src/provider.rs`: MUEVE el método `pub async fn gc_partials(...)` (cuerpo intacto, ~líneas 144-174) desde el bloque `impl LocalProvider` al bloque `impl Provider for LocalProvider` (junto a `trash`, ~línea 839). Quita el `pub` (los métodos de trait no lo llevan). El cuerpo NO cambia.

Si tras mover, los tests unitarios de `provider.rs` que llamaban `p.gc_partials(...)` sobre el tipo concreto fallan por resolución, añade `use norte_vfs::Provider;` en el `mod tests` correspondiente (varios ya lo importan).

- [ ] **Step 5: Añade `Engine::gc_partials`**

En `crates/norte-core/src/engine.rs`, tras `capabilities` (dentro de `impl Engine`):

```rust
    /// Barre el staging `.norte-partial` huérfano (crashes previos, ADR 0012 /
    /// #11) bajo `dir`, delegando en el provider que lo sirve. Operación
    /// PUNTUAL (no una Task) y NO registrada en el journal (no es una mutación
    /// de usuario). Los providers sin staging local devuelven 0.
    ///
    /// No hay barrido automático al arranque: `gc_partials` es single-dir y no
    /// existe una raíz gestionada fiable hasta que el journal registre el
    /// staging in-flight (deuda, futuro increment de M3).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider; los del provider.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        self.provider_for(dir)
            .await?
            .gc_partials(dir, older_than)
            .await
    }
```

- [ ] **Step 6: Verde (targeted + crates tocados)**

Run: `cargo nextest run -p norte-core --test engine_gc`
Expected: PASS (ambos tests).
Run: `cargo nextest run -p norte-vfs-local`
Expected: PASS (la reubicación no rompió el GC ni sus tests).

- [ ] **Step 7: Revisión + commit**

Dispara `rust-reviewer` sobre el diff. Aplica bloqueos. Luego:

```bash
git add crates/norte-vfs/src/provider.rs crates/norte-vfs-local/src/provider.rs \
        crates/norte-core/src/engine.rs crates/norte-core/tests/engine_gc.rs \
        crates/norte-core/Cargo.toml
git commit -m "feat(core): Provider::gc_partials trait + Engine::gc_partials (mecanismo #11, sin auto-sweep) (M3-1b)"
```

---

## Task 3: `reversal_ref` de `Trashed` — `trash()` devuelve el destino recuperable

`Provider::trash` pasa de `Result<(), Error>` a `Result<Option<VPath>, Error>`: `Some(dest)` = destino recuperable de una papelera LÓGICA (`.norte-trash/<id>/payload` en sftp/object — la `reversal_ref` que el undo M3-2 usará); `None` = papelera NATIVA del OS (local) o "vanish" (testkit), donde no hay ruta estable (el handle se resuelve en M3-2). El destino se propaga por `Mutation::Trashed { dest }` hasta la `reversal_ref` del journal.

Las llamadas de test existentes (`sftp/tests`, `object/tests`, `contract.rs`, `contract_ro.rs`) usan `.expect("trash")` (desenvuelven el `Result`, ignoran el `Option`) o casan `Err(Unsupported)` → **compilan sin cambios**. Solo los impls y `ops.rs` tocan el valor.

**Files:**
- Modify: `crates/norte-vfs/src/provider.rs:140` (firma del default `trash`)
- Modify: `crates/norte-vfs-local/src/provider.rs:839` (`Ok(None)`)
- Modify: `crates/norte-vfs-sftp/src/provider.rs:512-513` (`Ok(Some(paths.payload))`)
- Modify: `crates/norte-vfs-object/src/provider.rs:694` (`Ok(Some(paths.payload))`)
- Modify: `crates/norte-testkit/src/mem.rs:773-798` (`Ok(None)`)
- Modify: `crates/norte-core/src/observer.rs:19-26` (`Mutation::Trashed { path, dest }`)
- Modify: `crates/norte-core/src/ops.rs:1205-1208` (captura + propaga)
- Modify: `crates/norte-core/src/journal.rs:414-419` (mapea `dest` → `reversal_ref`)
- Test: `crates/norte-core/src/journal.rs` (mod `tests`) — unit del mapeo `Some(dest)`
- Test: `crates/norte-vfs-object/tests/trash.rs` — asevera `Some(dest)` en `.norte-trash`

- [ ] **Step 1: Test rojo A — mapeo `on_mutation(Trashed{dest:Some}) → reversal_ref`**

Añade al `mod tests` de `journal.rs`:

```rust
    #[tokio::test]
    async fn trashed_with_dest_records_reversal_ref_byte_exact() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let obs = SqliteJournal::new(Journal::open_in_memory().await.expect("open"));
        // Nombre hostil (no-UTF8 en el segmento): el dest debe round-trip byte-exacto.
        let victim = VPath::parse("file:///wíctim").expect("vpath");
        let dest = VPath::parse("file:///.norte-trash/17-3/payload").expect("dest");

        obs.on_mutation(
            &Mutation::Trashed {
                path: &victim,
                dest: Some(&dest),
            },
            &Actor::User,
        )
        .await
        .expect("on_mutation");

        let es = obs.journal.entries().await.expect("entries");
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].op, "trashed");
        assert_eq!(es[0].reversal, "restore_trash");
        assert_eq!(
            es[0].reversal_ref.as_deref(),
            Some(dest.to_wire().as_bytes()),
            "la reversal_ref es el dest en bytes to_wire (regla 1)"
        );
    }

    #[tokio::test]
    async fn trashed_without_dest_has_no_reversal_ref() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let obs = SqliteJournal::new(Journal::open_in_memory().await.expect("open"));
        let victim = VPath::parse("file:///v").expect("vpath");
        obs.on_mutation(
            &Mutation::Trashed { path: &victim, dest: None },
            &Actor::User,
        )
        .await
        .expect("on_mutation");
        let es = obs.journal.entries().await.expect("entries");
        assert_eq!(es[0].reversal, "restore_trash");
        assert_eq!(es[0].reversal_ref, None, "papelera nativa: sin ruta estable");
    }
```

- [ ] **Step 2: Verifica que falla (compilación: `Mutation::Trashed` sin campos con nombre)**

Run: `cargo nextest run -p norte-core journal::tests::trashed_with_dest_records_reversal_ref_byte_exact`
Expected: FAIL de compilación — `Mutation::Trashed` es tuple-variant, no tiene `path`/`dest`.

- [ ] **Step 3: `Mutation::Trashed` gana `dest`**

En `crates/norte-core/src/observer.rs`, reemplaza la variante:

```rust
    /// Nodo movido a la papelera (RECUPERABLE — el undo de M3 lo restaura;
    /// régimen distinto a `Removed`, ADR 0009).
    Trashed {
        /// Path original (víctima).
        path: &'a VPath,
        /// Destino recuperable en una papelera LÓGICA (`.norte-trash/<id>`,
        /// fase 9) → `reversal_ref`. `None` si es papelera NATIVA del OS o
        /// "vanish" (sin ruta estable; el handle se resuelve en el undo M3-2).
        dest: Option<&'a VPath>,
    },
```

- [ ] **Step 4: Firma del trait `trash` → `Result<Option<VPath>, Error>`**

En `crates/norte-vfs/src/provider.rs`, cambia la firma y el default (amplía el rustdoc):

```rust
    /// Mueve `p` (árbol entero si es dir) a la PAPELERA del provider —
    /// recuperable (ADR 0009). Solo con la capability `TRASH`; sin ella:
    /// [`Error::Unsupported`] (default).
    ///
    /// Devuelve `Some(dest)` con el destino recuperable cuando la papelera es
    /// LÓGICA (`.norte-trash/<id>/payload`) — el core lo persiste como
    /// `reversal_ref` para el undo. `None` si es la papelera NATIVA del OS (sin
    /// ruta estable expuesta) o una papelera "vanish" de test.
    ///
    /// Excepciones de plataforma conocidas (ADR 0009, issues #25/#26): [...]
    async fn trash(&self, p: &VPath) -> Result<Option<VPath>, Error> {
        let _ = p;
        Err(Error::Unsupported)
    }
```
(Conserva el párrafo de excepciones de plataforma existente.)

- [ ] **Step 5: Impls de `trash` devuelven el `Option`**

- `crates/norte-vfs-local/src/provider.rs` (`trash`, ~839): la papelera es nativa del OS → termina con `Ok(None)` en vez de `Ok(())`. Ajusta la firma a `-> Result<Option<VPath>, Error>`.
- `crates/norte-vfs-sftp/src/provider.rs` (~461): firma a `-> Result<Option<VPath>, Error>`; la última línea `self.rename(p, &paths.payload).await?; Ok(())` pasa a:
  ```rust
          self.rename(p, &paths.payload).await?;
          Ok(Some(paths.payload))
  ```
- `crates/norte-vfs-object/src/provider.rs` (~647): firma a `-> Result<Option<VPath>, Error>`; la última línea `self.rename(p, &paths.payload).await` pasa a:
  ```rust
          self.rename(p, &paths.payload).await?;
          Ok(Some(paths.payload))
  ```
- `crates/norte-testkit/src/mem.rs` (~773): firma a `-> Result<Option<VPath>, Error>`; el `Ok(())` final pasa a `Ok(None)` (semántica "vanish": el subárbol desaparece, sin destino recuperable — documenta en el rustdoc del método).

- [ ] **Step 6: `delete_task` captura y propaga el dest**

En `crates/norte-core/src/ops.rs`, en la rama `DeleteMode::Trash` de `delete_task` (~1205-1208):

```rust
        let dest = provider.trash(&path).await?;
        observer
            .on_mutation(
                &Mutation::Trashed {
                    path: &path,
                    dest: dest.as_ref(),
                },
                &ctx.actor,
            )
            .await?;
```

- [ ] **Step 7: `SqliteJournal::on_mutation` mapea `dest` → `reversal_ref`**

En `crates/norte-core/src/journal.rs`, reescribe la rama `Trashed` y el `record` para pasar la `reversal_ref`. Cambia la tupla del `match` para arrastrar también el `reversal_ref`:

```rust
        let (op, path, path_to, reversal, reversal_ref): (
            &str,
            Vec<u8>,
            Option<Vec<u8>>,
            Reversal,
            Option<Vec<u8>>,
        ) = match mutation {
            Mutation::Created(p) => (
                "created",
                p.to_wire().into_bytes(),
                None,
                Reversal::Delete,
                None,
            ),
            Mutation::Removed(p) => (
                "removed",
                p.to_wire().into_bytes(),
                None,
                Reversal::Irreversible,
                None,
            ),
            Mutation::Trashed { path, dest } => (
                "trashed",
                path.to_wire().into_bytes(),
                None,
                Reversal::RestoreTrash,
                dest.map(|d| d.to_wire().into_bytes()),
            ),
            Mutation::Renamed { from, to } => (
                "renamed",
                to.to_wire().into_bytes(),
                Some(from.to_wire().into_bytes()),
                Reversal::RenameBack,
                None,
            ),
        };
```

Y la llamada a `record` pasa `reversal_ref` en vez de `None`:

```rust
        self.journal
            .record(op, &path, path_to.as_deref(), reversal, reversal_ref.as_deref(), actor)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "fallo al escribir el journal");
                ProtoError::from(e)
            })?;
```

- [ ] **Step 8: Verde de los unit del journal + crates de providers**

Run: `cargo nextest run -p norte-core journal::tests`
Expected: PASS (incluye los dos tests nuevos + los previos).
Run: `cargo nextest run -p norte-vfs-local -p norte-testkit -p norte-vfs`
Expected: PASS (los `.expect("trash")` siguen compilando).

- [ ] **Step 9: Test rojo B → verde — object devuelve `Some(dest)` en `.norte-trash`**

En `crates/norte-vfs-object/tests/trash.rs`, en el caso feliz que ya hace `provider.trash(&victim).await.expect("trash")`, captura y asevera el destino. Reemplaza esa línea por:

```rust
    let dest = provider.trash(&victim).await.expect("trash");
    let dest = dest.expect("papelera lógica devuelve destino recuperable");
    assert!(
        dest.display_lossy().contains(".norte-trash"),
        "el dest apunta a la papelera lógica: {}",
        dest.display_lossy()
    );
```
(Si el harness de este test corre solo bajo feature/nightly, replica la aserción en el caso in-memory `services-fs` que sí corre en `just ci`.)

Run: `cargo nextest run -p norte-vfs-object --test trash`
Expected: PASS.

- [ ] **Step 10: Revisión (rust + encoding) + commit**

Dispara `encoding-auditor` (toca `VPath`/bytes de path en 5 providers + journal) y `rust-reviewer`. Aplica bloqueos.

```bash
git add crates/norte-vfs/src/provider.rs crates/norte-vfs-local/src/provider.rs \
        crates/norte-vfs-sftp/src/provider.rs crates/norte-vfs-object/src/provider.rs \
        crates/norte-testkit/src/mem.rs crates/norte-core/src/observer.rs \
        crates/norte-core/src/ops.rs crates/norte-core/src/journal.rs \
        crates/norte-vfs-object/tests/trash.rs
git commit -m "feat(core): trash() devuelve dest recuperable → reversal_ref de Trashed (M3-1b)"
```

---

## Task 4: Tests de integración engine↔journal

Verifica la costura completa: el engine con `SqliteJournal` real produce las entradas esperadas, en orden, con hash-chain válida, para copy/move/delete/trash — vía `MemProvider` (in-memory, determinista, sin harness externo).

**Files:**
- Test: `crates/norte-core/tests/engine_journal.rs` (nuevo)

- [ ] **Step 1: Test rojo — copy/move/delete/trash → entradas + hash-chain**

Crea `crates/norte-core/tests/engine_journal.rs`:

```rust
//! Integración engine↔journal (M3-1b): las mutaciones del engine con un
//! `SqliteJournal` real producen las entradas esperadas, en orden y con
//! hash-chain válida. `MemProvider` in-memory → determinista, sin harness.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine, Journal, SqliteJournal};
use norte_proto::{CapabilityFlags, DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, Provider};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content)).await.expect("chunk");
    sink.commit().await.expect("commit");
}

/// Engine con journal in-memory + MemProvider con TRASH.
async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn norte_core::MutationObserver>);
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::all(), // incluye TRASH; ajusta si `all()` no existe
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

#[tokio::test]
async fn copy_records_created_with_valid_chain() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///src.txt", b"hola").await;

    let h = engine.copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt")).await.expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///dst.txt");
    assert_eq!(es[0].reversal, "delete");
    assert_eq!(es[0].actor_kind, "user");
    assert!(journal.journal().verify_chain().await.expect("verify"));
}

#[tokio::test]
async fn move_records_renamed() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"x").await;

    let h = engine.move_(&vp("mem:///a.txt"), &vp("mem:///b.txt")).await.expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "renamed");
    assert_eq!(es[0].path, b"mem:///b.txt");
    assert_eq!(es[0].path_to.as_deref(), Some(&b"mem:///a.txt"[..]));
    assert!(journal.journal().verify_chain().await.expect("verify"));
}

#[tokio::test]
async fn permanent_delete_records_removed_irreversible() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///gone.txt", b"x").await;

    let h = engine.delete(&vp("mem:///gone.txt")).await.expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.last().expect("entrada").op, "removed");
    assert_eq!(es.last().unwrap().reversal, "irreversible");
    assert!(journal.journal().verify_chain().await.expect("verify"));
}

#[tokio::test]
async fn trash_records_trashed_restore() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///t.txt", b"x").await;

    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].reversal, "restore_trash");
    // MemProvider = "vanish": sin ruta recuperable.
    assert_eq!(es[0].reversal_ref, None);
    assert!(journal.journal().verify_chain().await.expect("verify"));
}
```

Nota de API: este test necesita acceder al `Journal` interno del `SqliteJournal` para leer entradas. Si `SqliteJournal` NO expone su journal públicamente, añade en `journal.rs` un accessor:

```rust
impl SqliteJournal {
    /// El journal subyacente (lectura para audit/tests).
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
}
```
(Verifica primero con `rg "pub fn journal|fn journal\(" crates/norte-core/src/journal.rs`; si el campo ya es accesible por otra vía, úsala en su lugar.)

Ajusta `CapabilityFlags::all()` si ese constructor no existe: usa el conjunto por defecto de `MemProvider::new()` si ya incluye `TRASH` (mem.rs:161), o compón las flags explícitas con `TRASH`.

- [ ] **Step 2: Verifica que falla / compila**

Run: `cargo nextest run -p norte-core --test engine_journal`
Expected: FAIL (o de compilación si falta `journal()`), luego resuelto por el accessor + flags.

- [ ] **Step 3: Añade el accessor `SqliteJournal::journal()` si hace falta**

(Ver nota del Step 1; añade y reexporta nada nuevo — `Journal` ya está exportado.)

- [ ] **Step 4: Verde**

Run: `cargo nextest run -p norte-core --test engine_journal`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/tests/engine_journal.rs crates/norte-core/src/journal.rs
git commit -m "test(core): integración engine↔journal (created/removed/renamed/trashed + chain) (M3-1b)"
```

---

## Task 5: Cablear `SqliteJournal` como observer default en los binarios

Los entrypoints reales (`norte` CLI, TUI embebido) pasan de `Engine::new()` (observer no-op) a `Engine::with_observer(SqliteJournal)` con el journal en el dir de config del usuario. Composición, no lógica de negocio (los frontends siguen sin decidir nada).

**Files:**
- Modify: `crates/norte-core/src/journal.rs` (helpers `SqliteJournal::open` + `default_db_path`)
- Modify: `crates/norte-cli/src/main.rs:182`
- Modify: `crates/norte-tui/src/main.rs:185`

- [ ] **Step 1: Helpers `SqliteJournal::open` + `default_db_path`**

En `crates/norte-core/src/journal.rs`, dentro de `impl SqliteJournal`:

```rust
    /// Abre (o crea) el journal en `path` y lo envuelve como observer.
    ///
    /// # Errors
    /// [`JournalError`] al abrir la DB (ver [`Journal::open`]).
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        Ok(Self::new(Journal::open(path).await?))
    }
```

Y a nivel de módulo (función libre pública):

```rust
/// Ruta por defecto del journal: `<config_dir>/journal.db` (respeta
/// `NORTE_CONFIG_DIR`/XDG vía [`crate::connect::config_dir`]).
#[must_use]
pub fn default_db_path() -> std::path::PathBuf {
    crate::connect::config_dir().join("journal.db")
}
```
Reexporta `default_db_path` en `lib.rs` si el módulo `journal` no es público (ajusta el `pub use journal::{...}` o expón `pub mod journal`). Verifica cómo está expuesto hoy (`rg "mod journal|pub use journal" crates/norte-core/src/lib.rs`).

- [ ] **Step 2: CLI usa el journal por defecto**

En `crates/norte-cli/src/main.rs`, reemplaza `let engine = Engine::new();` (~182) por:

```rust
    let journal = norte_core::SqliteJournal::open(&norte_core::journal::default_db_path())
        .await
        .context("abrir el journal")?;
    let engine = Engine::with_observer(std::sync::Arc::new(journal));
```
(`run` ya devuelve `anyhow::Result`; añade `use anyhow::Context as _;` si no está. Asegura `MutationObserver` no hace falta importarlo — `Arc<SqliteJournal>` coacciona a `Arc<dyn MutationObserver>` en `with_observer` vía el bound del parámetro.)

- [ ] **Step 3: TUI usa el journal por defecto**

En `crates/norte-tui/src/main.rs` (~185), aplica el mismo cambio, adaptado al tipo de `Result`/manejo de error de esa función (mira las líneas circundantes; si no es `anyhow`, propaga con el error del binario).

- [ ] **Step 4: Verde de build + tests de los binarios**

Run: `cargo build -p norte-cli -p norte-tui`
Expected: OK.
Run: `cargo nextest run -p norte-cli -p norte-tui`
Expected: PASS. Si algún E2E de la CLI asevera el CONTENIDO de un directorio temporal, confirma que usa `NORTE_CONFIG_DIR` apuntando a su propio temp (el `journal.db` cae ahí, no contamina el árbol de trabajo). Si un E2E falla por `journal.db` inesperado, ajústalo para ignorar/aislar ese fichero.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/journal.rs crates/norte-core/src/lib.rs \
        crates/norte-cli/src/main.rs crates/norte-tui/src/main.rs
git commit -m "feat(cli,tui): journal SqliteJournal como observer default del engine (M3-1b)"
```

---

## Task 6: Cierre — docs, `just ci`, memoria

**Files:**
- Modify: `docs/spec/norte-spec.md` o `ARCHITECTURE.md` solo si describen la firma de `Provider::trash`/`Mutation` (búscalo; si no, omite).
- Modify: memoria (`proyecto-norte-estado.md`).

- [ ] **Step 1: rustdoc coherente**

Confirma que el rustdoc de `Provider::trash` (nueva semántica `Option`), `Mutation::Trashed`, `JournalEntry`, `Journal::entries`, `Engine::gc_partials` está completo (`#![warn(missing_docs)]` activo). No quedan `TODO` sin issue.

- [ ] **Step 2: `just ci` verde local (gate — CI GitHub desactivado por billing)**

Run: `just ci`
Expected: fmt + clippy `-D warnings` + nextest workspace + deny + coverage ≥85% en core/vfs/proto — todo verde. Corrige clippy (pedantic: `map_or`, backticks en rustdoc — trampas recurrentes) hasta verde.

- [ ] **Step 3: Actualiza la memoria del proyecto**

Edita `/home/oscar/.claude/projects/-home-oscar-work-wot-projects-high-norte/memory/proyecto-norte-estado.md`: marca **M3-1b COMPLETA** con el resumen (entries()/JournalEntry, trash()→Option<VPath>+reversal_ref, Provider::gc_partials+Engine::gc_partials sin auto-sweep, wiring cli/tui, integración engine↔journal) y fija **SIGUIENTE = M3-2 (undo)**. Nota deuda: GC dirigido por journal/raíz gestionada diferido; `reversal_ref` de trash NATIVO local (handle) se resuelve en M3-2.

- [ ] **Step 4: Commit de cierre**

```bash
git add docs
git commit -m "docs(core): cierre M3-1b — wiring journal + reversal_ref + gc_partials (M3-1b)"
```

---

## Self-Review (hecho al escribir el plan)

- **Cobertura del spec (design 2026-07-14-m3-1-journal-design.md, decomposición 1b líneas 144-146):**
  - "wiring en el engine (`Actor::User` default en `TaskCtx`)" → `TaskCtx.actor` ya en M3-1a; wiring del observer default = Task 5. ✅
  - "captura de `reversal_ref` para `Trashed` (posible extensión de `Mutation`)" → Task 3 (extiende `Mutation::Trashed` + `trash()→Option<VPath>`). ✅
  - "GC al arranque" → reinterpretado (decisión aprobada): mecanismo del core sin auto-sweep (Task 2), con deuda documentada. ✅ (desviación consciente vs "al arranque" literal, justificada por single-dir/`/`-root)
  - "tests de integración engine↔journal" → Task 4. ✅
  - API de lectura (`entries`) = habilitador necesario de Task 4, no estaba explícito pero el design testing §128 exige aseverar "las entradas esperadas". ✅
- **Placeholders:** ninguno — todo paso con código/comando concretos.
- **Consistencia de tipos:** `trash -> Result<Option<VPath>, Error>` uniforme en trait + 5 impls + `ops.rs` + tests. `Mutation::Trashed { path, dest }` uniforme en `observer.rs` + `ops.rs` + `journal.rs`. `JournalEntry`/`entries()` usados igual en Task 1 y Task 4. `SqliteJournal::journal()` accessor usado en Task 4 (verificar existencia antes de añadir). ✅

## Riesgos / verificaciones durante ejecución (no asumir)

1. `MemProvider::with_flags(CapabilityFlags::all())` — confirmar que `all()` existe y que `MemProvider::new()` ya trae `TRASH` (mem.rs:161); si no, componer flags explícitas.
2. `SqliteJournal.journal` es privado — Task 4 añade `journal()` accessor; confirmar antes de duplicar.
3. Reubicar `gc_partials` en local puede exigir `use norte_vfs::Provider;` en su `mod tests`.
4. Wiring de binarios: vigilar E2E de la CLI que asevere contenidos de dir temporal (aislar `journal.db` vía `NORTE_CONFIG_DIR`).
5. `default_db_path`/`journal` module visibility en `lib.rs` — confirmar cómo se reexporta hoy.
