# Fase 9c — Papelera lógica en object/S3 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ObjectProvider` (S3/opendal) offer recoverable delete via logical `.norte-trash/` — opt-in per connection — by implementing `Provider::trash` on top of the pure `norte-vfs::trash` module plus the provider's already-audited copy-all→delete-all `rename()`.

**Architecture:** `ObjectProvider` gains a `logical_trash: bool` field (default off) + a monotonic `trash_counter`. When on, it declares `CapabilityFlags::TRASH` and implements `trash()` as: create the `.norte-trash/<id>/` marker dirs, write `.norte-info` (via `trash::info_encode`), then `self.rename(victim → .norte-trash/<id>/<basename>)`. That `rename` already performs **copy-all THEN delete-all** with hostile-server containment (keys reconstructed from validated suffixes) — so no new cancellation machinery is needed: interruption at any point leaves the origin intact or fully copied-to-trash, never lost. Cancellation stays drop-based (design §3; fine-grained mid-walk cancel is debt #51).

**Tech Stack:** Rust, `opendal` (services-fs test harness), `cargo nextest`.

---

## File Structure

- Modify: `crates/norte-vfs-object/src/provider.rs` — add `logical_trash`/`trash_counter` fields, `with_logical_trash` builder, capability gate, `next_counter`/`ensure_dir_idempotent` helpers, `trash()` impl.
- Create: `crates/norte-vfs-object/tests/trash.rs` — integration tests over the services-fs harness.
- Modify: `crates/norte-core/src/connect.rs:201` — pass `spec.logical_trash` to the provider.
- Reference (read only): `crates/norte-vfs/src/trash.rs`, `crates/norte-vfs-object/src/provider.rs` (`rename` at 498, `mkdir` at 449, `stat_kind` at 150, `key` at 99), `crates/norte-vfs-object/tests/contract.rs` + `tests/common/mod.rs` (harness), `docs/adr/0019-papelera-logica-remota.md`.

No proto/wire change (`TRASH` flag exists). `ConnectionSpec.logical_trash` already exists (added in 9b) — 9c only wires the object arm. No struct-literal churn (`ObjectProvider::new` is the sole constructor).

---

### Task 1: Provider fields, builder, capability gate

**Files:**
- Modify: `crates/norte-vfs-object/src/provider.rs`
- Test: `crates/norte-vfs-object/tests/trash.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-vfs-object/tests/trash.rs`:

```rust
//! Papelera lógica `.norte-trash/` de object/S3 (fase 9c, ADR 0019) contra
//! el harness `services-fs` de opendal.
mod common;

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::trash;
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

/// Provider fresco sobre un tempdir vía `services-fs`, con la papelera
/// lógica en el estado pedido.
fn fresh(logical_trash: bool) -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3").with_logical_trash(logical_trash)
}

/// La raíz del provider (`s3://norte-test/`).
fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false);
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true);
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-vfs-object trash_capability_follows_the_flag`
Expected: FAIL to compile — `with_logical_trash` does not exist.

- [ ] **Step 3: Implement fields + builder + gate**

In `crates/norte-vfs-object/src/provider.rs`:

Add the atomics import near the top (after the existing `use` lines):

```rust
use std::sync::atomic::{AtomicU64, Ordering};
```

Add the two fields to `struct ObjectProvider` (after the `server_copy: bool` field):

```rust
    /// Papelera lógica `.norte-trash/` activa (opt-in por conexión, ADR
    /// 0019). Off por defecto → no declara `TRASH` → borrado permanente.
    logical_trash: bool,
    /// Contador monótono para desempatar ids de papelera del mismo ms.
    trash_counter: AtomicU64,
```

In `ObjectProvider::new`, initialise them in the returned struct literal (alongside `op`/`scheme`/`key_budget`/`server_copy`):

```rust
            logical_trash: false,
            trash_counter: AtomicU64::new(0),
```

Add the builder as a public method in the `impl ObjectProvider` block (right after `new`):

```rust
    /// Activa/desactiva la papelera lógica `.norte-trash/` (ADR 0019).
    /// Sin ella el provider no declara `TRASH` y `trash()` da `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }
```

Change `capabilities()` to gate the flag. Replace its body (lines ~250-268) with:

```rust
    fn capabilities(&self) -> Capabilities {
        // Honestas (ADR 0016 H): keys UTF-8 byte-exactas → case-sensitive y
        // case-preserving. SERVER_COPY = CopyObject solo si el backend lo
        // anuncia. NO declara APPEND/RANDOM_WRITE/SYMLINKS/RENAME_ATOMIC.
        // TRASH solo si la conexión activó la papelera lógica `.norte-trash/`
        // (ADR 0019): la relocalización reusa el rename copy-all→delete-all.
        let mut flags = CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::CASE_PRESERVING;
        if self.server_copy {
            flags |= CapabilityFlags::SERVER_COPY;
        }
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            max_path: u32::try_from(self.key_budget).ok(),
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-vfs-object trash_capability_follows_the_flag`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-object/src/provider.rs crates/norte-vfs-object/tests/trash.rs
git commit -m "feat(vfs-object): logical_trash field + capability gate (fase 9c)"
```

---

### Task 2: `trash()` — markers + write info + reuse rename

**Files:**
- Modify: `crates/norte-vfs-object/src/provider.rs`
- Test: `crates/norte-vfs-object/tests/trash.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-vfs-object/tests/trash.rs`:

```rust
/// Lista los nombres (bytes) de los hijos de un dir (drena el `EntryStream`).
async fn child_names(p: &ObjectProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(entry) = stream.try_next().await.expect("entry") {
        names.push(
            entry
                .path
                .file_name()
                .expect("hijo con nombre")
                .as_bytes()
                .to_vec(),
        );
    }
    names
}

/// El único `<id>` bajo `.norte-trash/`.
async fn sole_entry(p: &ObjectProvider) -> VPath {
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "una entrada de papelera");
    trash_dir.join(Segment::new(ids[0].clone()).unwrap())
}

#[tokio::test]
async fn trash_moves_file_and_writes_info() {
    let p = fresh(true);
    let victim = root().join(Segment::new(b"victim.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"contenido").await;

    p.trash(&victim).await.expect("trash");

    // Origen desaparece.
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    let entry = sole_entry(&p).await;
    // Payload preserva el contenido.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(
        common::read_all(&p, &payload).await.expect("read payload"),
        b"contenido"
    );
    // `.norte-info` decodifica a la ruta original, anclado a la conexión.
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = common::read_all(&p, &info_path).await.expect("read info");
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_moves_directory_tree() {
    // El rename copy-all→delete-all se lleva el ÁRBOL entero.
    let p = fresh(true);
    let dir = root().join(Segment::new(b"proj".to_vec()).unwrap());
    p.mkdir(&dir).await.expect("mkdir proj");
    let sub = dir.join(Segment::new(b"sub".to_vec()).unwrap());
    p.mkdir(&sub).await.expect("mkdir sub");
    let deep = sub.join(Segment::new(b"b.txt".to_vec()).unwrap());
    common::write_all(&p, &deep, b"hondo").await;

    p.trash(&dir).await.expect("trash");
    assert!(matches!(p.stat(&dir).await, Err(norte_proto::Error::NotFound)));

    let entry = sole_entry(&p).await;
    let moved_deep = entry
        .join(Segment::new(b"proj".to_vec()).unwrap())
        .join(Segment::new(b"sub".to_vec()).unwrap())
        .join(Segment::new(b"b.txt".to_vec()).unwrap());
    assert_eq!(
        common::read_all(&p, &moved_deep).await.expect("read deep"),
        b"hondo"
    );
}

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    let p = fresh(false);
    let victim = root().join(Segment::new(b"x.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"y").await;

    assert!(matches!(
        p.trash(&victim).await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&victim).await.is_ok());
}

#[tokio::test]
async fn trash_refuses_to_trash_itself() {
    let p = fresh(true);
    let victim = root().join(Segment::new(b"v.txt".to_vec()).unwrap());
    common::write_all(&p, &victim, b"a").await;
    p.trash(&victim).await.expect("trash");

    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    assert!(matches!(
        p.trash(&trash_dir).await,
        Err(norte_proto::Error::Unsupported)
    ));
    assert!(p.stat(&trash_dir).await.is_ok());
}

#[tokio::test]
async fn trash_preserves_hostile_basename() {
    // S3 keys son UTF-8-only (como sftp): el nombre hostil es UTF-8 retorcido.
    let p = fresh(true);
    let hostile = "año 名前 😀.txt".as_bytes().to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());
    common::write_all(&p, &victim, b"z").await;

    p.trash(&victim).await.expect("trash");
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));
    let entry = sole_entry(&p).await;
    let names = child_names(&p, &entry).await;
    assert!(
        names.iter().any(|n| n == &hostile),
        "basename hostil preservado"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-vfs-object -E 'test(/trash_moves|trash_without|trash_refuses|trash_preserves/)'`
Expected: FAIL — default `trash()` returns `Unsupported` even with the flag on, so the move tests fail.

- [ ] **Step 3: Implement helpers + `trash()`**

In `crates/norte-vfs-object/src/provider.rs`, add `trash` to the `norte_vfs` import:

```rust
use norte_vfs::{trash, ByteSink, ByteStream, EntryStream, Provider};
```

Add the two helpers to an `impl ObjectProvider` block (the inherent one that already holds `key`/`stat_kind`, near those methods):

```rust
    /// Siguiente valor del contador monótono de ids de papelera.
    fn next_counter(&self) -> u64 {
        self.trash_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Crea el marker `dir` tolerando que ya exista (idempotente): útil para
    /// `.norte-trash/` bajo concurrencia entre sesiones.
    async fn ensure_dir_idempotent(&self, dir: &VPath) -> Result<(), Error> {
        match self.mkdir(dir).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => Ok(()),
            Err(e) => match self.stat_kind(&self.key(dir)?).await? {
                Some((EntryKind::Dir, _)) => Ok(()),
                _ => Err(e),
            },
        }
    }
```

Add the `trash` method to the `#[async_trait] impl Provider for ObjectProvider` block (place it near `rename`):

```rust
    async fn trash(&self, p: &VPath) -> Result<(), Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // Víctima ausente = `NotFound` limpio, sin entrada de papelera huérfana.
        let _ = self.stat(p).await?;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));

        // Primer plan: valida `p` (rechaza papelerizar la propia papelera,
        // ADR 0019) y da la raíz `.norte-trash`.
        let first = trash::plan(p, &trash::trash_id(now_ms, self.next_counter()))?;
        let trash_root = first.dir.parent().ok_or(Error::Unsupported)?;
        self.ensure_dir_idempotent(&trash_root).await?;

        // `.norte-trash/<id>/` fresco; `<id>` solo único POR SESIÓN → reintenta
        // con id nuevo si otra sesión colisionó en el mismo ms.
        let mut paths = first;
        let mut attempts = 0u32;
        loop {
            match self.mkdir(&paths.dir).await {
                Ok(()) => break,
                Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                }) if attempts < 8 => {
                    attempts += 1;
                    paths = trash::plan(p, &trash::trash_id(now_ms, self.next_counter()))?;
                }
                Err(e) => return Err(e),
            }
        }

        // `.norte-info` ANTES de mover: si el rename falla, el origen queda
        // intacto o recuperable (copiado a la papelera), nunca un payload sin
        // metadatos.
        let info = trash::info_encode(p, now_ms);
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Mueve el árbol reutilizando el rename AUDITADO: copy-all →
        // delete-all, keys reconstruidas desde sufijos validados (contención
        // de servidor hostil), sin pérdida ante interrupción (ADR 0019/0016).
        self.rename(p, &paths.payload).await
    }
```

Confirm `EntryKind`, `ConflictKind`, `Bytes` are already imported (they are — used by `stat_kind`/`mkdir`/the sink).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-vfs-object -E 'test(/trash_/)'`
Expected: PASS (all trash tests + the capability toggle).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-object/src/provider.rs crates/norte-vfs-object/tests/trash.rs
git commit -m "feat(vfs-object): trash() — .norte-trash/ markers+info+reuse rename (fase 9c)"
```

---

### Task 3: Wire `logical_trash` → object provider in the core manager

**Files:**
- Modify: `crates/norte-core/src/connect.rs`

- [ ] **Step 1: Change the s3 arm**

In `crates/norte-core/src/connect.rs`, change the s3 arm (line ~201) from:

```rust
                Ok(Arc::new(ObjectProvider::new(op, "s3")))
```

to:

```rust
                Ok(Arc::new(
                    ObjectProvider::new(op, "s3").with_logical_trash(spec.logical_trash),
                ))
```

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo build -p norte-core`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/norte-core/src/connect.rs
git commit -m "feat(core): ConnectionSpec.logical_trash → ObjectProvider (fase 9c)"
```

---

### Task 4: Gate — fmt, clippy, full CI

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff or applied.

- [ ] **Step 2: Clippy**

Run: `cargo clippy -p norte-vfs-object -p norte-core --all-targets -- -D warnings`
Expected: PASS. Watch for the same `map_or` pedantic lint seen in 9b (already using `map_or` here) and any needless-borrow on the `format!`/`key` calls.

- [ ] **Step 3: Full local CI**

Run: `just ci`
Expected: green (fmt + clippy + nextest + deny + coverage + doc). Release gate (GitHub CI off, billing).

- [ ] **Step 4: Commit if CI changed files**

```bash
git add -A
git commit -m "style: cargo fmt fase 9c" # only if fmt changed files
```

---

## Self-Review Notes

- **Spec coverage:** capability gate + opt-in (Task 1, 3) ← spec «opt-in por conexión default OFF»; `trash()` markers+info+rename (Task 2) ← spec «object/S3 → copy-all→delete-all» (delivered by reusing the audited `rename`); hostile bytes + self-ref + dir tree (Task 2) ← spec testing + 9b audit guards.
- **Cancellation:** no token; the copy-all→delete-all ordering that gives the no-data-loss guarantee lives inside the existing `rename` (`provider.rs:525` "copy-all LUEGO delete-all"). Fine-grained mid-walk cancel = debt #51, consistent with the design.
- **No new dep / no proto change.** `ConnectionSpec.logical_trash` reused from 9b.
- **Ordering safety:** info written before rename → interruption never yields a payload without metadata; a mid-rename failure leaves origin fully copied-to-trash + info present (recoverable), matching ADR 0016's accepted S3 non-atomicity.
- **Self-reference:** handled by the shared `trash::plan` guard added in the 9b audit — no object-specific code needed; `trash_refuses_to_trash_itself` locks it.
- **Type consistency:** `with_logical_trash`, `logical_trash`, `trash_counter`, `next_counter`, `ensure_dir_idempotent`, `trash::{trash_id,plan,info_encode,info_decode,TRASH_DIR,INFO_NAME}` match 9a/9b usage. `info_decode(bytes, &root())` two-arg.
- **Verified against codebase:** `rename` (498) does copy-all→delete-all with validated-suffix reconstruction; `mkdir` (449) returns `Conflict::Exists` when present; `stat_kind` (150) → `Option<(EntryKind, Metadata)>`; harness `common::{fs_operator, write_all, read_all}` + `ObjectProvider::root("s3", Authority)`; `list`/`read` are streams (drained via `TryStreamExt`).
- **Debt after 9c (closes the provider work of fase 9):** rename-failure fault-injection test (orphan-info) + `corpus::utf8_hostile_names()` helper (shared 9b/9c) + `poisoned_trash_info` corpus for M3 restore. Then fase 10 (M2 quality + exit criterion).
