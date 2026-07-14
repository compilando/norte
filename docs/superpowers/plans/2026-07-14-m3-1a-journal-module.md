# M3-1a — Módulo journal (sqlx + schema + hash-chain) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the journal storage layer: a `Journal` over sqlx-SQLite (WAL) that records each mutation with actor + reversal ref + a tamper-evident hash-chain, plus the `Actor` type and the `async` migration of `MutationObserver`.

**Architecture:** New `journal` module in `norte-core`. `Journal` holds a `SqlitePool` + a `tokio::Mutex<ChainState>` (next_seq, last_hash) so concurrent tasks serialize their inserts and the hash-chain stays consistent. `MutationObserver::on_mutation` becomes `async fn(&self, &Mutation, &Actor)`; `SqliteJournal` implements it; `NoopObserver` stays for journal-less tests. This PR does NOT wire `SqliteJournal` into the `Engine` (that + GC + `Trashed` reversal_ref is 1b) — but it adds the `Actor` field to `TaskCtx` so the async signature threads through.

**Tech Stack:** Rust, `sqlx` (sqlite, runtime-tokio), `sha2` (workspace), `async-trait`, `cargo nextest`.

---

## File Structure

- Create: `docs/adr/0020-journal-sqlx-sqlite.md` — ADR for the structural storage dep.
- Modify: `Cargo.toml` (workspace) + `crates/norte-core/Cargo.toml` — add `sqlx`.
- Create: `crates/norte-core/src/journal.rs` — `Journal`, `Actor`, `Reversal`, `SqliteJournal`, hash-chain.
- Modify: `crates/norte-core/src/observer.rs` — `on_mutation` async + `Actor` param; `NoopObserver` async.
- Modify: `crates/norte-core/src/scheduler.rs` — add `actor: Actor` to `TaskCtx`.
- Modify: `crates/norte-core/src/ops.rs` — `.await` + pass `&ctx.actor` at every `on_mutation` call (~15 sites).
- Modify: `crates/norte-core/src/lib.rs` — `pub mod journal;`.
- Modify: `crates/norte-core/src/engine.rs` — construct `TaskCtx` with `Actor::User` default (so it compiles).
- Reference: `crates/norte-core/src/observer.rs` (Mutation enum), `docs/superpowers/specs/2026-07-14-m3-1-journal-design.md`.

---

### Task 1: ADR 0020 — journal storage

**Files:** Create `docs/adr/0020-journal-sqlx-sqlite.md`; modify `docs/adr/README.md`.

- [ ] **Step 1: Write the ADR** (MADR, mirror 0019 style)

```markdown
# 0020 — Journal: sqlx sobre SQLite (WAL), hash-chain propia

- Estado: accepted
- Fecha: 2026-07-14
- Relacionado: spec §4/§10 (SQLite motor único, journal), regla 4, #11. Diseño:
  docs/superpowers/specs/2026-07-14-m3-1-journal-design.md.

## Decisión

- **sqlx** (feature `sqlite`, `runtime-tokio`) como capa de journal — async,
  await-eable desde el core sin actor de hilos aparte; runtime queries (sin DB en
  build). SQLite **WAL** + `synchronous=NORMAL`, un solo escritor serializado por
  un `Mutex` de la cadena. Alternativas: rusqlite (sync, exigiría actor
  bloqueante) y redb (rompe SQLite-motor-único de spec §4). sqlx encaja con el
  índice/FTS5/embeddings futuros (mismo motor).
- **on_mutation pasa a async**: el insert se await-ea antes de completar la op
  (regla 4). Consecuencia: migrar los call-sites de ops.rs.
- **hash-chain propia** (sha2, ya en el árbol): `entry_hash =
  sha256(prev_hash ‖ campos con longitud prefijada)`. Tamper-evident → audit
  (M3-5).

## Consecuencias

Dep estructural nueva (sqlx + su árbol). Positivo: un motor para journal/index/
tags. Deuda: los inserts se serializan (un escritor) — aceptable (el journal no
es el cuello de botella); si duele, batch. Licencias sqlx → revisar en `cargo
deny` (MIT/Apache).
```

Append to `docs/adr/README.md`:
```markdown
| [0020](0020-journal-sqlx-sqlite.md) | Journal: sqlx sobre SQLite (WAL), hash-chain propia | accepted |
```

- [ ] **Step 2: Commit**

```bash
git add docs/adr/0020-journal-sqlx-sqlite.md docs/adr/README.md
git commit -m "docs(adr): 0020 journal sqlx sobre SQLite WAL (M3-1a)"
```

---

### Task 2: sqlx dep + `journal` module skeleton (Actor, Reversal)

**Files:** `Cargo.toml`, `crates/norte-core/Cargo.toml`, `crates/norte-core/src/journal.rs`, `crates/norte-core/src/lib.rs`.

- [ ] **Step 1: Add sqlx to the workspace**

In root `Cargo.toml` `[workspace.dependencies]`, after the `sha2` line:
```toml
sqlx = { version = "0.8", default-features = false, features = ["sqlite", "runtime-tokio"] }
```

In `crates/norte-core/Cargo.toml` `[dependencies]`, add:
```toml
sqlx.workspace = true
sha2.workspace = true
```
(`sha2` may already be an indirect dep via vfs-local, but core needs it directly for the hash-chain.)

- [ ] **Step 2: Create the module with the pure types + a failing hash test**

Create `crates/norte-core/src/journal.rs`:

```rust
//! Journal transaccional (M3-1, ADR 0020): toda mutación → una entrada con
//! actor, referencia de reversa y hash-chain tamper-evident sobre SQLite (WAL).

use sha2::{Digest, Sha256};

/// Quién originó la mutación (spec §10). Hoy siempre `User`; los agentes lo
/// fijan vía scopes/MCP (M3-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// Un frontend humano local.
    User,
    /// Un agente por MCP, con su id de sesión.
    Agent {
        /// Id de la sesión del agente.
        session: String,
    },
    /// Un plugin, con su id declarado.
    Plugin {
        /// Id del plugin.
        id: String,
    },
}

impl Actor {
    /// `(kind, id)` para persistir: `("user", None)`, `("agent", Some(sess))`…
    #[must_use]
    pub fn parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Actor::User => ("user", None),
            Actor::Agent { session } => ("agent", Some(session.as_str())),
            Actor::Plugin { id } => ("plugin", Some(id.as_str())),
        }
    }
}

/// Cómo revertir la entrada (lo EJECUTA M3-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversal {
    /// Borrar el nodo creado.
    Delete,
    /// Renombrar de vuelta (destino → origen).
    RenameBack,
    /// Restaurar desde la papelera.
    RestoreTrash,
    /// No hay vuelta atrás (borrado permanente).
    Irreversible,
}

impl Reversal {
    /// Etiqueta persistida.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Reversal::Delete => "delete",
            Reversal::RenameBack => "rename_back",
            Reversal::RestoreTrash => "restore_trash",
            Reversal::Irreversible => "irreversible",
        }
    }
}

/// Los campos de una entrada, en el orden canónico del hash.
pub(crate) struct Record<'a> {
    pub seq: i64,
    pub ts_ms: i64,
    pub actor_kind: &'a str,
    pub actor_id: Option<&'a str>,
    pub op: &'a str,
    pub path: &'a [u8],
    pub path_to: Option<&'a [u8]>,
    pub reversal: &'a str,
    pub reversal_ref: Option<&'a [u8]>,
}

/// `entry_hash = sha256(prev_hash ‖ campos con LONGITUD PREFIJADA)`. La longitud
/// prefijada evita colisiones de concatenación (`ab‖c` vs `a‖bc`).
#[must_use]
pub(crate) fn chain_hash(prev: &[u8; 32], r: &Record<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev);
    let mut field = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(&r.seq.to_le_bytes());
    field(&r.ts_ms.to_le_bytes());
    field(r.actor_kind.as_bytes());
    field(r.actor_id.unwrap_or("").as_bytes());
    field(r.op.as_bytes());
    field(r.path);
    field(r.path_to.unwrap_or(&[]));
    field(r.reversal.as_bytes());
    field(r.reversal_ref.unwrap_or(&[]));
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: i64) -> Record<'static> {
        Record {
            seq,
            ts_ms: 1_726_000_000_000,
            actor_kind: "user",
            actor_id: None,
            op: "created",
            path: b"file:///a",
            path_to: None,
            reversal: "delete",
            reversal_ref: None,
        }
    }

    #[test]
    fn chain_hash_is_deterministic_and_prev_sensitive() {
        let zero = [0u8; 32];
        let h1 = chain_hash(&zero, &rec(1));
        assert_eq!(h1, chain_hash(&zero, &rec(1)), "determinista");
        // Distinto prev → distinto hash (encadenado).
        assert_ne!(h1, chain_hash(&h1, &rec(1)));
        // Distinto seq → distinto hash.
        assert_ne!(h1, chain_hash(&zero, &rec(2)));
    }

    #[test]
    fn length_prefix_prevents_concatenation_collision() {
        // Dos records que sin longitud-prefijada colisionarían (`ab`+`c` vs
        // `a`+`bc`) deben dar hashes distintos.
        let zero = [0u8; 32];
        let mut a = rec(1);
        a.actor_id = Some("ab");
        a.op = "c";
        let mut b = rec(1);
        b.actor_id = Some("a");
        b.op = "bc";
        assert_ne!(chain_hash(&zero, &a), chain_hash(&zero, &b));
    }
}
```

Add to `crates/norte-core/src/lib.rs` (near the other `pub mod`):
```rust
pub mod journal;
```

- [ ] **Step 3: Run the hash tests**

Run: `cargo nextest run -p norte-core journal::tests`
Expected: PASS (2 tests). If sqlx fails to resolve, re-check the workspace dep version/features.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/norte-core/Cargo.toml crates/norte-core/src/journal.rs crates/norte-core/src/lib.rs Cargo.lock
git commit -m "feat(core): journal module — Actor, Reversal, hash-chain (M3-1a)"
```

---

### Task 3: `Journal` — open (WAL), insert, verify

**Files:** `crates/norte-core/src/journal.rs`.

- [ ] **Step 1: Write the failing DB tests**

Add to the `tests` module in `journal.rs`:

```rust
    #[tokio::test]
    async fn open_insert_and_verify_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record("created", b"file:///a", None, Reversal::Delete, None, &Actor::User, 1)
            .await
            .expect("insert 1");
        j.record(
            "trashed",
            "file:///\u{00e9}".as_bytes(),
            None,
            Reversal::RestoreTrash,
            Some(b"file:///.norte-trash/1-0"),
            &Actor::Agent { session: "s1".into() },
            2,
        )
        .await
        .expect("insert 2");

        assert_eq!(j.count().await.expect("count"), 2);
        assert!(j.verify_chain().await.expect("verify"), "cadena íntegra");
    }

    #[tokio::test]
    async fn tampering_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record("created", b"file:///a", None, Reversal::Delete, None, &Actor::User, 1)
            .await
            .expect("insert");
        // Manipular una entrada (cambiar el path directamente en la fila).
        j.corrupt_path_for_test(1, b"file:///HACKED").await.expect("corrupt");
        assert!(!j.verify_chain().await.expect("verify"), "la manipulación se detecta");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -p norte-core journal::tests::open_insert`
Expected: FAIL to compile — `Journal` not defined.

- [ ] **Step 3: Implement `Journal`**

Add above the `tests` module in `journal.rs`:

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use tokio::sync::Mutex;

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS journal (
    seq          INTEGER PRIMARY KEY,
    ts_ms        INTEGER NOT NULL,
    actor_kind   TEXT    NOT NULL,
    actor_id     TEXT,
    op           TEXT    NOT NULL,
    path         BLOB    NOT NULL,
    path_to      BLOB,
    reversal     TEXT    NOT NULL,
    reversal_ref BLOB,
    prev_hash    BLOB    NOT NULL,
    entry_hash   BLOB    NOT NULL
);";

/// Estado de la cadena (serializa inserts para hash-chain consistente).
struct ChainState {
    last_hash: [u8; 32],
}

/// El journal transaccional sobre SQLite (WAL).
pub struct Journal {
    pool: SqlitePool,
    chain: Mutex<ChainState>,
}

impl Journal {
    /// Abre (o crea) el journal en `path` con WAL + `synchronous=NORMAL`.
    ///
    /// # Errors
    /// Errores de sqlx al abrir/crear el schema.
    pub async fn open(path: &std::path::Path) -> Result<Self, sqlx::Error> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        Self::from_options(opts).await
    }

    /// Journal efímero en memoria (tests).
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn open_in_memory() -> Result<Self, sqlx::Error> {
        Self::from_options(SqliteConnectOptions::from_str("sqlite::memory:")?).await
    }

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, sqlx::Error> {
        // Pool de 1 conexión: un solo escritor (in-memory exige max=1 para no
        // perder la DB entre conexiones).
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        let last_hash = sqlx::query("SELECT entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&pool)
            .await?
            .map(|row| {
                let v: Vec<u8> = row.get(0);
                let mut h = [0u8; 32];
                h.copy_from_slice(&v);
                h
            })
            .unwrap_or([0u8; 32]);
        Ok(Self { pool, chain: Mutex::new(ChainState { last_hash }) })
    }

    /// Registra una mutación. `seq` lo asigna el llamante (monótono); el hash
    /// encadena con la última entrada. Serializado por el `Mutex` de la cadena.
    ///
    /// # Errors
    /// Errores de sqlx al insertar.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        op: &str,
        path: &[u8],
        path_to: Option<&[u8]>,
        reversal: Reversal,
        reversal_ref: Option<&[u8]>,
        actor: &Actor,
        seq: i64,
    ) -> Result<(), sqlx::Error> {
        let (actor_kind, actor_id) = actor.parts();
        // ts_ms sin `Date::now` prohibido: SystemTime está permitido en runtime.
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

        let mut chain = self.chain.lock().await;
        let prev = chain.last_hash;
        let rec = Record {
            seq,
            ts_ms,
            actor_kind,
            actor_id,
            op,
            path,
            path_to,
            reversal: reversal.as_str(),
            reversal_ref,
        };
        let entry_hash = chain_hash(&prev, &rec);

        sqlx::query(
            "INSERT INTO journal (seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, prev_hash, entry_hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(seq)
        .bind(ts_ms)
        .bind(actor_kind)
        .bind(actor_id)
        .bind(op)
        .bind(path)
        .bind(path_to)
        .bind(reversal.as_str())
        .bind(reversal_ref)
        .bind(&prev[..])
        .bind(&entry_hash[..])
        .execute(&self.pool)
        .await?;

        chain.last_hash = entry_hash;
        Ok(())
    }

    /// Número de entradas.
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn count(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) FROM journal").fetch_one(&self.pool).await?;
        Ok(row.get(0))
    }

    /// Recorre la cadena recomputando cada hash; `false` si alguna entrada fue
    /// manipulada (base del audit, M3-5).
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn verify_chain(&self) -> Result<bool, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, prev_hash, entry_hash \
             FROM journal ORDER BY seq ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut prev = [0u8; 32];
        for row in rows {
            let path: Vec<u8> = row.get(5);
            let path_to: Option<Vec<u8>> = row.get(6);
            let reversal_ref: Option<Vec<u8>> = row.get(8);
            let actor_id: Option<String> = row.get(3);
            let op: String = row.get(4);
            let actor_kind: String = row.get(2);
            let reversal: String = row.get(7);
            let stored_prev: Vec<u8> = row.get(9);
            let stored_hash: Vec<u8> = row.get(10);
            if stored_prev != prev {
                return Ok(false); // rotura de encadenado
            }
            let rec = Record {
                seq: row.get(0),
                ts_ms: row.get(1),
                actor_kind: &actor_kind,
                actor_id: actor_id.as_deref(),
                op: &op,
                path: &path,
                path_to: path_to.as_deref(),
                reversal: &reversal,
                reversal_ref: reversal_ref.as_deref(),
            };
            let computed = chain_hash(&prev, &rec);
            if computed[..] != stored_hash[..] {
                return Ok(false);
            }
            prev = computed;
        }
        Ok(true)
    }

    /// SOLO TESTS: corrompe el `path` de una entrada sin recomputar su hash.
    #[cfg(test)]
    pub async fn corrupt_path_for_test(&self, seq: i64, path: &[u8]) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE journal SET path = ? WHERE seq = ?")
            .bind(path)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo nextest run -p norte-core journal::tests`
Expected: PASS (hash + DB tests). Note the hostile path (`file:///é`) round-trips through the BLOB byte-exact.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/journal.rs
git commit -m "feat(core): Journal open(WAL)/record/verify_chain sobre sqlx (M3-1a)"
```

---

### Task 4: `on_mutation` async + `Actor` en `TaskCtx` + `SqliteJournal`

**Files:** `crates/norte-core/src/observer.rs`, `crates/norte-core/src/scheduler.rs`, `crates/norte-core/src/engine.rs`, `crates/norte-core/src/ops.rs`, `crates/norte-core/src/journal.rs`.

- [ ] **Step 1: Make the observer async + take `Actor`**

In `crates/norte-core/src/observer.rs`, add `use async_trait::async_trait;` and `use crate::journal::Actor;`, then change the trait + Noop:

```rust
#[async_trait]
pub trait MutationObserver: Send + Sync {
    /// Notifica una mutación ya aplicada. Async: el journal await-ea el insert
    /// antes de que la op se considere completa (regla 4).
    async fn on_mutation(&self, mutation: &Mutation<'_>, actor: &Actor);
}

pub(crate) struct NoopObserver;

#[async_trait]
impl MutationObserver for NoopObserver {
    async fn on_mutation(&self, _mutation: &Mutation<'_>, _actor: &Actor) {}
}
```

- [ ] **Step 2: Add `actor` to `TaskCtx`**

In `crates/norte-core/src/scheduler.rs`, add to `struct TaskCtx`:
```rust
    /// Origen de las mutaciones de esta task (default `User`; los agentes lo
    /// fijan vía MCP en M3-4).
    pub actor: crate::journal::Actor,
```
Find every construction of `TaskCtx { cancel, progress }` in `scheduler.rs`/`engine.rs` and add `actor: crate::journal::Actor::User`.

- [ ] **Step 3: Migrate the ~15 `on_mutation` call sites in `ops.rs`**

Every `observer.on_mutation(&Mutation::X(..))` becomes
`observer.on_mutation(&Mutation::X(..), &ctx.actor).await`. The sites (from `grep -n "on_mutation" crates/norte-core/src/ops.rs`): 456, 471, 548, 795, 874, 988, 1003, 1014, 1033, 1101, 1142, 1152, 1182, 1199, 1209. Each has `observer` and `ctx` in scope (they are threaded together). Mechanical transformation — apply to all. Example:

```rust
// antes
observer.on_mutation(&Mutation::Trashed(&path));
// después
observer.on_mutation(&Mutation::Trashed(&path), &ctx.actor).await;
```

- [ ] **Step 4: Implement `SqliteJournal` observer in `journal.rs`**

Add to `journal.rs` (bridges `Journal` to the observer; owns the monotonic seq):

```rust
use crate::observer::{Mutation, MutationObserver};
use async_trait::async_trait;
use std::sync::atomic::{AtomicI64, Ordering};

/// El [`Journal`] como [`MutationObserver`]: mapea cada `Mutation` a una entrada
/// y asigna el `seq` monótono.
pub struct SqliteJournal {
    journal: Journal,
    next_seq: AtomicI64,
}

impl SqliteJournal {
    /// Envuelve un journal ya abierto; el `seq` continúa tras las entradas
    /// existentes.
    ///
    /// # Errors
    /// Errores de sqlx al leer el último `seq`.
    pub async fn new(journal: Journal) -> Result<Self, sqlx::Error> {
        let last = sqlx::query("SELECT COALESCE(MAX(seq), 0) FROM journal")
            .fetch_one(&journal.pool)
            .await?;
        let max: i64 = last.get(0);
        Ok(Self { journal, next_seq: AtomicI64::new(max + 1) })
    }
}

#[async_trait]
impl MutationObserver for SqliteJournal {
    async fn on_mutation(&self, mutation: &Mutation<'_>, actor: &Actor) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let (op, path, path_to, reversal, reversal_ref): (&str, Vec<u8>, Option<Vec<u8>>, Reversal, Option<Vec<u8>>) =
            match mutation {
                Mutation::Created(p) => ("created", p.to_wire().into_bytes(), None, Reversal::Delete, None),
                Mutation::Removed(p) => ("removed", p.to_wire().into_bytes(), None, Reversal::Irreversible, None),
                Mutation::Trashed(p) => {
                    // reversal_ref (ruta de papelera) llega en 1b; aquí queda None.
                    ("trashed", p.to_wire().into_bytes(), None, Reversal::RestoreTrash, None)
                }
                Mutation::Renamed { from, to } => (
                    "renamed",
                    to.to_wire().into_bytes(),
                    Some(from.to_wire().into_bytes()),
                    Reversal::RenameBack,
                    None,
                ),
            };
        // Un fallo de journal tras una mutación aplicada NO puede tragarse
        // (regla 4): se registra el error a nivel tracing; 1b decide propagarlo
        // como fallo de la task.
        if let Err(e) = self
            .journal
            .record(op, &path, path_to.as_deref(), reversal, reversal_ref.as_deref(), actor, seq)
            .await
        {
            tracing::error!(error = %e, seq, "fallo al escribir el journal");
        }
    }
}
```
Make `Journal.pool` visible to `SqliteJournal` (same module → already accessible).

- [ ] **Step 5: Build + run the whole core**

Run: `cargo nextest run -p norte-core`
Expected: PASS. The async-observer migration must not break existing engine/ops tests (they use `NoopObserver`, now async — still a no-op).

- [ ] **Step 6: Commit**

```bash
git add crates/norte-core/src/observer.rs crates/norte-core/src/scheduler.rs crates/norte-core/src/engine.rs crates/norte-core/src/ops.rs crates/norte-core/src/journal.rs
git commit -m "feat(core): on_mutation async + Actor en TaskCtx + SqliteJournal (M3-1a)"
```

---

### Task 5: Gate — fmt, clippy, deny, full CI

- [ ] **Step 1: fmt + clippy**

Run: `cargo fmt --all && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: clean. Watch pedantic lints on the new module (`must_use`, doc backticks, `too_many_arguments` already allowed on `record`).

- [ ] **Step 2: deny (new dep licenses)**

Run: `cargo deny check licenses`
Expected: OK. If sqlx pulls a flagged license, add a justified exception to `deny.toml` with an issue (as done for prior deps).

- [ ] **Step 3: Full CI**

Run: `just ci`
Expected: green, coverage ≥ 85% (the new `journal` module is well-covered by its unit tests).

- [ ] **Step 4: Commit if fmt changed files**

```bash
git add -A && git commit -m "style: cargo fmt M3-1a"  # only if needed
```

---

## Self-Review Notes

- **Spec coverage (M3-1a):** sqlx SQLite WAL + module (Task 2-3) ← spec «storage»; hash-chain + verify (Task 3) ← spec «hash-chain»; `Actor` + async `on_mutation` (Task 4) ← spec «actor» + «durabilidad»; ADR (Task 1) ← «sqlx dep estructural». GC + engine-wiring + `Trashed` reversal_ref are explicitly 1b (spec decomposition).
- **Rule discipline:** rule 4 (journal awaited before op completes) delivered by async observer; rule 6 (`SystemTime` map_or, no unwrap outside tests); rule 8 (sqlx justified in ADR + deny check). Hostile paths stored as BLOB via `to_wire()` (rule 1).
- **Type consistency:** `Actor::{User,Agent,Plugin}`, `Reversal::{Delete,RenameBack,RestoreTrash,Irreversible}`, `Record`, `chain_hash`, `Journal::{open,open_in_memory,record,count,verify_chain}`, `SqliteJournal::new` + `on_mutation(&Mutation, &Actor)` used consistently across tasks.
- **Open risk at execution:** the exact sqlx 0.8 `SqliteConnectOptions`/`SqlitePoolOptions` builder names + `Row::get` typing — if a signature differs, consult sqlx 0.8 docs; the shapes here match sqlx 0.8. `Journal.pool` is `pub(crate)`-visible within the module for `SqliteJournal::new` (same file → private field access is fine).
- **Follow-ups:** 1b (wire `SqliteJournal` into `Engine` as default observer, capture `Trashed` reversal_ref — likely extend `Mutation::Trashed`, startup GC of `.norte-partial`), then M3-2 undo.
