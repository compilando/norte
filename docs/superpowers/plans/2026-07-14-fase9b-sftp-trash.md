# Fase 9b — Papelera lógica en sftp — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `SftpProvider` offer recoverable delete via logical `.norte-trash/` — opt-in per connection — by implementing `Provider::trash` on top of the pure `norte-vfs::trash` module (create dir + write `.norte-info` + rename the tree).

**Architecture:** `SftpProvider` gains a `logical_trash: bool` field (default off) plus a monotonic `trash_counter`. When on, it declares `CapabilityFlags::TRASH` and implements `trash()` as: mkdir `.norte-trash/<id>/`, write `.norte-info` (via `trash::info_encode`), then `rename(victim → .norte-trash/<id>/<basename>)` (one server-side move, `entries_total = 1` per ADR 0009). The connection config (`ConnectionSpec.logical_trash`) flows to the provider in the core connect manager via a `with_logical_trash` builder. Cancellation stays drop-based (no token; sftp trash is a fast 3-op sequence).

**Tech Stack:** Rust, `russh-sftp`, in-process sftp test server, `cargo nextest`.

---

## File Structure

- Modify: `crates/norte-vfs-sftp/src/provider.rs` — add `logical_trash`/`trash_counter` fields, `with_logical_trash` builder, capability gate, `trash()` impl.
- Create: `crates/norte-vfs-sftp/tests/trash.rs` — integration tests over the in-process server (move, info roundtrip, Unsupported-when-off, hostile basename, caps toggle).
- Modify: `crates/norte-connect/src/spec.rs` — add `logical_trash: bool` field to `ConnectionSpec`.
- Modify: `crates/norte-core/src/connect.rs:173` — pass `spec.logical_trash` to the provider.
- Reference (read only): `crates/norte-vfs/src/trash.rs` (`trash_id`, `plan`, `info_encode`, `info_decode`), `crates/norte-vfs-sftp/tests/contract.rs` + `tests/common/mod.rs` (harness pattern), `docs/adr/0019-papelera-logica-remota.md`.

No proto/wire change: `CapabilityFlags::TRASH` already exists (ADR 0009); sftp merely declares it conditionally.

---

### Task 1: Provider fields, builder, capability gate

**Files:**
- Modify: `crates/norte-vfs-sftp/src/provider.rs`
- Test: `crates/norte-vfs-sftp/tests/trash.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-vfs-sftp/tests/trash.rs`. It mirrors the harness in `tests/contract.rs` (in-process server on a tempdir, base `/`). First test asserts the capability toggles with the flag:

```rust
//! Papelera lógica `.norte-trash/` de sftp (fase 9b, ADR 0019) contra el
//! servidor sftp in-process.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{Authority, CapabilityFlags, Segment, VPath};
use norte_vfs::trash;
use norte_vfs::Provider;
use norte_vfs_sftp::SftpProvider;

/// Provider fresco sobre tempdir + servidor in-process, con la papelera
/// lógica en el estado pedido.
async fn fresh(logical_trash: bool) -> SftpProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    let session = common::connect(&base, common::Mode::Honest).await;
    std::mem::forget(dir);
    SftpProvider::new(session, "/").with_logical_trash(logical_trash)
}

/// La raíz remota del cliente (`/`), con authority de test.
fn root() -> VPath {
    SftpProvider::root(Authority::new("test:22").expect("authority"))
}

#[tokio::test]
async fn trash_capability_follows_the_flag() {
    let off = fresh(false).await;
    assert!(!off.capabilities().flags.contains(CapabilityFlags::TRASH));

    let on = fresh(true).await;
    assert!(on.capabilities().flags.contains(CapabilityFlags::TRASH));
}
```

Note: if `tests/common` is not auto-visible, copy the `mod common;` declaration exactly as `tests/contract.rs` uses it (same directory-level `common` module).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-vfs-sftp trash_capability_follows_the_flag`
Expected: FAIL to compile — `with_logical_trash` does not exist.

- [ ] **Step 3: Implement fields + builder + gate**

In `crates/norte-vfs-sftp/src/provider.rs`:

Add to the imports at the top of the file (near the other `std`/`bytes` uses):

```rust
use std::sync::atomic::{AtomicU64, Ordering};
```

Add the two fields to `struct SftpProvider` (after the existing `base: String` field):

```rust
    /// Papelera lógica `.norte-trash/` activa (opt-in por conexión, ADR
    /// 0019). Off por defecto → no declara `TRASH` → borrado permanente.
    logical_trash: bool,
    /// Contador monótono para desempatar ids de papelera del mismo ms.
    trash_counter: AtomicU64,
```

In `SftpProvider::new`, initialise them in the returned struct literal (alongside `session`/`base`):

```rust
            logical_trash: false,
            trash_counter: AtomicU64::new(0),
```

Add the builder as a public method in the `impl SftpProvider` block (right after `new`):

```rust
    /// Activa/desactiva la papelera lógica `.norte-trash/` (ADR 0019).
    /// Sin ella el provider no declara `TRASH` y `trash()` da `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }
```

Change `capabilities()` to gate the flag. Replace the `Capabilities { flags: ... }` construction with:

```rust
    fn capabilities(&self) -> Capabilities {
        // Honestas (ADR 0013): sftp tiene symlinks y escritura en offset/
        // append (habilita el resume de ADR 0012), y se asume remoto POSIX
        // case-sensitive. NO declara rename atómico (v3 no lo garantiza) ni
        // server-copy. TRASH solo si la conexión activó la papelera lógica
        // `.norte-trash/` (ADR 0019).
        let mut flags = CapabilityFlags::SYMLINKS
            | CapabilityFlags::APPEND
            | CapabilityFlags::RANDOM_WRITE
            | CapabilityFlags::CASE_PRESERVING
            | CapabilityFlags::CASE_SENSITIVE;
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            max_path: None,
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-vfs-sftp trash_capability_follows_the_flag`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-sftp/src/provider.rs crates/norte-vfs-sftp/tests/trash.rs
git commit -m "feat(vfs-sftp): logical_trash field + capability gate (fase 9b)"
```

---

### Task 2: `trash()` — mkdir + write info + rename

**Files:**
- Modify: `crates/norte-vfs-sftp/src/provider.rs`
- Test: `crates/norte-vfs-sftp/tests/trash.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/norte-vfs-sftp/tests/trash.rs`:

```rust
/// Drena un `ByteStream` a bytes.
async fn read_all(p: &SftpProvider, path: &VPath) -> Vec<u8> {
    let mut rd = p.read(path, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// Lista los nombres (bytes) de los hijos de un dir remoto (drena el
/// `EntryStream`).
async fn child_names(p: &SftpProvider, dir: &VPath) -> Vec<Vec<u8>> {
    let mut stream = p.list(dir).await.expect("list");
    let mut names = Vec::new();
    while let Some(item) = stream.next().await {
        let entry = item.expect("entry");
        names.push(entry.path.file_name().expect("hijo con nombre").as_bytes().to_vec());
    }
    names
}

#[tokio::test]
async fn trash_moves_tree_and_writes_info() {
    let p = fresh(true).await;
    let victim = root().join(Segment::new(b"victim.txt".to_vec()).unwrap());

    // Siembra el archivo.
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"contenido")).await.expect("chunk");
    sink.commit().await.expect("commit");

    // A la papelera.
    p.trash(&victim).await.expect("trash");

    // El origen desaparece.
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    // `.norte-trash/<id>/` existe con UNA entrada.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    assert_eq!(ids.len(), 1, "una entrada de papelera");
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());

    // Contiene el payload + `.norte-info`.
    let mut names = child_names(&p, &entry).await;
    names.sort();
    let mut expected = vec![b".norte-info".to_vec(), b"victim.txt".to_vec()];
    expected.sort();
    assert_eq!(names, expected);

    // El payload conserva el contenido.
    let payload = entry.join(Segment::new(b"victim.txt".to_vec()).unwrap());
    assert_eq!(read_all(&p, &payload).await, b"contenido");

    // El `.norte-info` decodifica a la ruta original (anclado a la conexión).
    let info_path = entry.join(Segment::new(trash::INFO_NAME.to_vec()).unwrap());
    let info_bytes = read_all(&p, &info_path).await;
    let info = trash::info_decode(&info_bytes, &root()).expect("decode");
    assert_eq!(info.original, victim);
}

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    let p = fresh(false).await;
    let victim = root().join(Segment::new(b"x.txt".to_vec()).unwrap());
    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"y")).await.expect("chunk");
    sink.commit().await.expect("commit");

    assert!(matches!(
        p.trash(&victim).await,
        Err(norte_proto::Error::Unsupported)
    ));
    // El origen sigue ahí (no se degradó a permanente).
    assert!(p.stat(&victim).await.is_ok());
}
```

Note: the trait exposes only streaming `read()`/`list()` — the local `read_all`/`child_names` helpers above drain them (idiom copied from `tests/openssh.rs` / `tests/hostile.rs`). `use futures::StreamExt;` is already in the imports.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p norte-vfs-sftp trash_moves_tree_and_writes_info trash_without_capability_is_unsupported`
Expected: FAIL — default `trash()` returns `Unsupported` even with the flag on (the trait default is not overridden yet), so `trash_moves_tree_and_writes_info` fails.

- [ ] **Step 3: Implement `trash()`**

Add the `trash` method to the `#[async_trait] impl Provider for SftpProvider` block (place it near `remove`/`rename`). It uses `norte_vfs::trash`; add `use norte_vfs::trash;` to the file's imports if not present.

```rust
    async fn trash(&self, p: &VPath) -> Result<(), Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // Reloj de pared + contador de sesión → id único y ordenable.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let counter = self.trash_counter.fetch_add(1, Ordering::Relaxed);
        let id = trash::trash_id(now_ms, counter);
        let paths = trash::plan(p, &id)?;

        // `.norte-trash/` (ignora si ya existe) → `.norte-trash/<id>/` (fresco).
        let trash_root = paths.dir.parent().ok_or(Error::Unsupported)?;
        match self.mkdir(&trash_root).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => {}
            Err(e) => return Err(e),
        }
        self.mkdir(&paths.dir).await?;

        // Escribe `.norte-info` ANTES de mover: si el rename falla, el origen
        // queda intacto y solo hay un info huérfano (basura limpiable), nunca
        // un payload sin metadatos.
        let info = trash::info_encode(p, now_ms);
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Mueve el árbol entero (un rename del server — ADR 0009,
        // entries_total = 1).
        self.rename(p, &paths.payload).await?;
        Ok(())
    }
```

Confirm `ConflictKind` is already imported (it is — used in `mkdir`). Confirm `Bytes` is imported (it is — used in the sink).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-vfs-sftp trash_moves_tree_and_writes_info trash_without_capability_is_unsupported`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-vfs-sftp/src/provider.rs crates/norte-vfs-sftp/tests/trash.rs
git commit -m "feat(vfs-sftp): trash() — .norte-trash/ mkdir+info+rename (fase 9b)"
```

---

### Task 3: Hostile-basename trash test

**Files:**
- Test: `crates/norte-vfs-sftp/tests/trash.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/norte-vfs-sftp/tests/trash.rs`:

```rust
#[tokio::test]
async fn trash_preserves_hostile_basename() {
    let p = fresh(true).await;
    // Nombre no-UTF8 (0xFF 0xFE) — el servidor in-process (Linux) lo acepta.
    let hostile = b"\xff\xfe".to_vec();
    let victim = root().join(Segment::new(hostile.clone()).unwrap());

    let mut sink = p.write(&victim).await.expect("write");
    sink.write(Bytes::from_static(b"z")).await.expect("chunk");
    sink.commit().await.expect("commit");

    p.trash(&victim).await.expect("trash");
    assert!(matches!(
        p.stat(&victim).await,
        Err(norte_proto::Error::NotFound)
    ));

    // El payload dentro de la papelera conserva los bytes hostiles.
    let trash_dir = root().join(Segment::new(trash::TRASH_DIR.to_vec()).unwrap());
    let ids = child_names(&p, &trash_dir).await;
    let entry = trash_dir.join(Segment::new(ids[0].clone()).unwrap());
    let names = child_names(&p, &entry).await;
    assert!(names.iter().any(|n| n == &hostile), "basename hostil preservado");
}
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo nextest run -p norte-vfs-sftp trash_preserves_hostile_basename`
Expected: PASS immediately (the impl from Task 2 already handles this — this test locks the encoding guarantee). If it does not pass, the bug is real; fix `trash()` before proceeding.

- [ ] **Step 3: Commit**

```bash
git add crates/norte-vfs-sftp/tests/trash.rs
git commit -m "test(vfs-sftp): trash preserva basename hostil (fase 9b)"
```

---

### Task 4: Wire `ConnectionSpec.logical_trash` → provider

**Files:**
- Modify: `crates/norte-connect/src/spec.rs`
- Modify: `crates/norte-core/src/connect.rs`
- Test: `crates/norte-connect/src/spec.rs` (inline test)

- [ ] **Step 1: Write the failing test**

In `crates/norte-connect/src/spec.rs`, find the `#[cfg(test)] mod tests` block (or add one at the end of the file) and add:

```rust
    #[test]
    fn logical_trash_defaults_off_and_parses() {
        // Ausente → false (default seguro, ADR 0019).
        let f: ConnectionsFile =
            toml::from_str("[connections.a]\nurl = \"sftp://h\"\n").expect("parse");
        assert!(!f.connections["a"].logical_trash);

        // Presente → true.
        let f: ConnectionsFile = toml::from_str(
            "[connections.b]\nurl = \"sftp://h\"\nlogical_trash = true\n",
        )
        .expect("parse");
        assert!(f.connections["b"].logical_trash);
    }
```

If the test module uses a different name for the top-level struct than `ConnectionsFile`, match the name used elsewhere in the file (it is declared near line 27). Reuse whatever `toml`/import idiom existing tests in this file already use; if `toml` is not a dev-dependency of `norte-connect`, add `toml.workspace = true` under `[dev-dependencies]` in `crates/norte-connect/Cargo.toml` (it is already a workspace dep).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-connect logical_trash_defaults_off_and_parses`
Expected: FAIL to compile — `logical_trash` field does not exist.

- [ ] **Step 3: Add the field**

In `crates/norte-connect/src/spec.rs`, add to `struct ConnectionSpec` (after the `addressing` field, keeping the `#[serde(default)]` pattern):

```rust
    /// Papelera lógica `.norte-trash/` en esta conexión (ADR 0019). Off por
    /// defecto: el borrado degrada a permanente con aviso del frontend.
    #[serde(default)]
    pub logical_trash: bool,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-connect logical_trash_defaults_off_and_parses`
Expected: PASS.

- [ ] **Step 5: Pass it to the provider in the core manager**

In `crates/norte-core/src/connect.rs`, change the sftp arm (line ~173) from:

```rust
                Ok(Arc::new(SftpProvider::new(session, "/")))
```

to:

```rust
                Ok(Arc::new(
                    SftpProvider::new(session, "/").with_logical_trash(spec.logical_trash),
                ))
```

- [ ] **Step 6: Verify the workspace builds**

Run: `cargo build -p norte-core -p norte-connect`
Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-connect/src/spec.rs crates/norte-core/src/connect.rs
git commit -m "feat(connect,core): ConnectionSpec.logical_trash → SftpProvider (fase 9b)"
```

---

### Task 5: Gate — fmt, clippy, full CI

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff or applied.

- [ ] **Step 2: Clippy**

Run: `cargo clippy -p norte-vfs-sftp -p norte-connect -p norte-core --all-targets -- -D warnings`
Expected: PASS. If clippy flags the `SystemTime`/`try_from` cast or the `match` on mkdir, address per its suggestion without changing behavior.

- [ ] **Step 3: Full local CI**

Run: `just ci`
Expected: green (fmt + clippy + nextest + deny + coverage + doc). This is the release gate (GitHub CI off, billing).

- [ ] **Step 4: Commit if CI changed files**

```bash
git add -A
git commit -m "style: cargo fmt fase 9b" # only if fmt changed files
```

---

## Self-Review Notes

- **Spec coverage:** capability gate + opt-in (Task 1, 4) ← spec «opt-in por conexión default OFF» + «Capability y opt-in»; `trash()` mkdir+info+rename (Task 2) ← spec «Relocalización por provider → sftp»; hostile bytes (Task 3) ← spec testing «nombre hostil»; config field (Task 4) ← ADR 0019 `logical_trash`.
- **Cancellation:** sftp `trash()` is a fast 3-op sequence (`entries_total = 1`, ADR 0009); no dedicated cancel test — the drop-based/no-token model is a 9c/S3 concern. The engine already checks `ctx.cancel.is_cancelled()` before calling `trash()` (`ops.rs` `delete_task`).
- **No token / no new dep:** matches design §3 (revised) — `trash(&self, p)` signature unchanged.
- **Ordering safety:** info written before rename → a failed rename leaves origin intact + orphan info (cleanable), never a payload without metadata.
- **Type consistency:** `with_logical_trash(bool) -> Self`, `logical_trash: bool`, `trash_counter: AtomicU64`, `trash::{trash_id, plan, info_encode, info_decode, TRASH_DIR, INFO_NAME}` used consistently. `info_decode(bytes, &root())` matches the 9a two-arg signature.
- **Follow-up:** 9c (object/S3 copy-all→delete-all, its own plan). TUI/CLI already send `DeleteMode::Trash` gated on the `TRASH` capability (ADR 0009 wiring from M1) — no frontend change needed; verify during 9c/10.
- **Verified against the codebase:** `list()`→`EntryStream` and `read()`→`ByteStream` are streams (drained via `StreamExt::next`, helpers provided); `tests/common/mod.rs` exposes `connect(base, Mode)` + `Mode`; `ConflictKind`/`Bytes` already imported in `provider.rs`; `SftpProvider::new`/`root`/`with_logical_trash` call chain compiles at the four call sites.
