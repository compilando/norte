# Fase 10b — E2E del criterio de salida M2 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the M2 exit criterion "sin sorpresas" end-to-end at the Engine level: read from a local zip and copy its content through S3 (object-fs) and back, byte-exact, with hostile names preserved and clean cancellation.

**Architecture:** One integration test in `norte-core/tests/` builds an `Engine` with a `MemProvider` (holds the zip container + acts as the "local" POSIX-like target) and an `ObjectProvider` over opendal `services-fs` (the S3 stand-in, registered under scheme `s3`). The zip is seeded with `ZipSmith` (hostile UTF-8 names + a larger binary). The archive scheme `zip+mem` is composed on demand by the Engine (fase 8f). Resume mechanics are already covered by `engine_resume.rs`; object resume is deferred (ADR 0016), so 10b covers the cross-provider chain + cancel-clean, not resume.

**Tech Stack:** Rust, `norte-core::Engine`, `norte-testkit` (`MemProvider`, `ZipSmith`), `opendal` (services-fs, new dev-dep), `cargo nextest`.

---

## File Structure

- Modify: `crates/norte-core/Cargo.toml` — add `opendal` (features `services-fs`) + `tempfile` (already present) to `[dev-dependencies]`.
- Create: `crates/norte-core/tests/e2e_exit_criterion.rs` — the scenario (setup helper inline; no shared `tests/common` needed for a single remote).
- Reference (read only): `crates/norte-vfs-object/tests/common/mod.rs:61-74` (`fs_operator` idiom), `crates/norte-core/tests/engine_archive.rs` (zip-over-Mem + `zip+mem` paths), `crates/norte-core/tests/engine_resume.rs` (cancel/join idiom), `crates/norte-vfs-object/src/provider.rs` (`ObjectProvider::{new,root}`).

No production code. No wire change. sftp is NOT in this E2E (covered by its own suite + nightly OpenSSH) — documented in the spec.

---

### Task 1: opendal dev-dep + object-fs setup helper

**Files:**
- Modify: `crates/norte-core/Cargo.toml`
- Create: `crates/norte-core/tests/e2e_exit_criterion.rs`

- [ ] **Step 1: Add the dev-dep**

In `crates/norte-core/Cargo.toml`, under `[dev-dependencies]`, add:

```toml
# E2E del criterio de salida M2 (fase 10b): object-fs como remoto S3 stand-in.
opendal = { workspace = true, features = ["services-fs"] }
```

`tempfile` is already a dev-dep. `norte-vfs-object` is already a regular dep (exposes `ObjectProvider`).

- [ ] **Step 2: Write the setup helper + a smoke test that fails first**

Create `crates/norte-core/tests/e2e_exit_criterion.rs`:

```rust
//! Fase 10b — E2E del criterio de salida M2 ("sin sorpresas"): leer desde un
//! zip local y mover su contenido por S3 (object-fs) y de vuelta, byte-exacto,
//! con nombres hostiles preservados y cancelación limpia.
//!
//! Remoto = object-fs (S3 stand-in). sftp queda en su suite + nightly. El
//! resume está cubierto en engine_resume.rs; object no reanuda (ADR 0016).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::Engine;
use norte_proto::{Authority, TaskState, VPath};
use norte_testkit::{MemProvider, ZipSmith};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Nombre hostil UTF-8 (S3 y sftp son UTF-8-only): unicode + emoji + espacio.
const HOSTILE: &[u8] = "año 名前 😀.txt".as_bytes();

/// Engine con: MemProvider (contiene `a.zip` + hace de "local"), ObjectProvider
/// (object-fs sobre tempdir, scheme `s3`). Devuelve el engine y guarda vivos los
/// tempdirs (el SO limpia /tmp; se filtran con `mem::forget`).
async fn setup() -> Engine {
    let engine = Engine::new();

    // Zip con nombres hostiles + un binario "grande" (256 KiB).
    let big = vec![0xABu8; 256 * 1024];
    let zip = ZipSmith::new()
        .file(HOSTILE, b"contenido hostil")
        .file(b"grande.bin", &big)
        .build();
    let mem = Arc::new(MemProvider::new());
    let mut sink = mem.write(&vp("mem:///a.zip")).await.expect("write zip");
    sink.write(Bytes::copy_from_slice(&zip)).await.expect("chunk");
    sink.commit().await.expect("commit");
    engine.register_provider(mem as Arc<dyn Provider>);

    // object-fs sobre tempdir (idiom de vfs-object/tests/common::fs_operator).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    opendal::install_default();
    let op = opendal::Operator::new(
        opendal::services::Fs::default()
            .root(root.to_str().expect("utf8"))
            .atomic_write_dir(atomic.to_str().expect("utf8")),
    )
    .expect("operator fs"); // forma conocida-buena (vfs-object/tests/common:74)
    std::mem::forget(dir);
    engine.register_provider(Arc::new(ObjectProvider::new(op, "s3")) as Arc<dyn Provider>);

    engine
}

/// Drena `read` a bytes.
async fn read_all(engine: &Engine, p: &VPath) -> Vec<u8> {
    let mut s = engine.read(p, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.expect("chunk"));
    }
    out
}

/// La raíz S3 de test (`s3://norte-test/`).
fn s3_root() -> VPath {
    ObjectProvider::root("s3", Authority::new("norte-test").expect("authority"))
}

fn s3(name: &[u8]) -> VPath {
    s3_root().join(norte_proto::Segment::new(name.to_vec()).expect("seg"))
}

#[tokio::test]
async fn reads_hostile_name_from_zip_byte_exact() {
    let engine = setup().await;
    let inside = vp("zip+mem:///a.zip/!").join(norte_proto::Segment::new(HOSTILE.to_vec()).unwrap());
    assert_eq!(read_all(&engine, &inside).await, b"contenido hostil");
}
```

- [ ] **Step 3: Run the smoke test**

Run: `cargo nextest run -p norte-core reads_hostile_name_from_zip_byte_exact`
Expected: PASS. If the `opendal::services::Fs` builder API differs (`.finish()` vs `Operator::new(...)`), match the exact idiom in `crates/norte-vfs-object/tests/common/mod.rs:69-73` — copy it verbatim (it is the known-good form).

- [ ] **Step 4: Commit**

```bash
git add crates/norte-core/Cargo.toml crates/norte-core/tests/e2e_exit_criterion.rs Cargo.lock
git commit -m "test(core): E2E 10b — setup object-fs + lee zip hostil byte-exacto"
```

---

### Task 2: zip → S3 → local, round-trip, name preservation

**Files:**
- Modify: `crates/norte-core/tests/e2e_exit_criterion.rs`

- [ ] **Step 1: Write the chain test**

Append to `crates/norte-core/tests/e2e_exit_criterion.rs`:

```rust
#[tokio::test]
async fn zip_to_s3_to_local_roundtrip_byte_exact() {
    let engine = setup().await;
    let from_zip = vp("zip+mem:///a.zip/!")
        .join(norte_proto::Segment::new(HOSTILE.to_vec()).unwrap());

    // 1. zip → S3 (nombre hostil preservado).
    let on_s3 = s3(HOSTILE);
    assert_eq!(
        engine.copy(&from_zip, &on_s3).await.expect("copy zip→s3").join().await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &on_s3).await, b"contenido hostil");

    // 2. S3 → "local" (Mem), byte-exacto.
    let local = vp("mem:///restaurado.txt");
    assert_eq!(
        engine.copy(&on_s3, &local).await.expect("copy s3→local").join().await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &local).await, b"contenido hostil");

    // 3. Round-trip S3 → S3 (otra key), byte-exacto.
    let on_s3_b = s3(b"copia.txt");
    assert_eq!(
        engine.copy(&on_s3, &on_s3_b).await.expect("copy s3→s3").join().await,
        TaskState::Completed
    );
    assert_eq!(read_all(&engine, &on_s3_b).await, b"contenido hostil");
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p norte-core zip_to_s3_to_local_roundtrip_byte_exact`
Expected: PASS. If any leg FAILS, it is a real cross-provider bug (encoding/copy) — reproduce and fix the engine/provider, do not weaken the assertion.

- [ ] **Step 3: Commit**

```bash
git add crates/norte-core/tests/e2e_exit_criterion.rs
git commit -m "test(core): E2E 10b — zip→S3→local round-trip byte-exacto + nombres"
```

---

### Task 3: cancelación limpia (sin objeto a medias)

**Files:**
- Modify: `crates/norte-core/tests/e2e_exit_criterion.rs`

- [ ] **Step 1: Write the cancel test**

Append. Cancel a copy of the 256 KiB binary zip→S3 mid-flight; assert the destination key is absent (object cancel aborts the write → no half-object), never a partial:

```rust
#[tokio::test]
async fn cancel_zip_to_s3_leaves_no_partial() {
    let engine = setup().await;
    let big_from = vp("zip+mem:///a.zip/!/grande.bin");
    let big_to = s3(b"grande.bin");

    let handle = engine.copy(&big_from, &big_to).await.expect("copy");
    handle.cancel();
    let state = handle.join().await;
    assert!(
        matches!(state, TaskState::Cancelled | TaskState::Completed),
        "cancel: o cancelada o completada antes de cancelar, nunca a medias: {state:?}"
    );

    // Si se canceló, el destino NO debe existir (sin objeto a medias); si
    // alcanzó a completar, existe entero. Nunca un objeto parcial corrupto.
    match engine.stat(&big_to).await {
        Ok(_) => {
            // Completó antes del cancel → debe estar ÍNTEGRO (256 KiB).
            assert_eq!(read_all(&engine, &big_to).await.len(), 256 * 1024);
        }
        Err(norte_proto::Error::NotFound) => {} // cancelado limpio, sin residuo
        Err(e) => panic!("estado inesperado del destino: {e:?}"),
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p norte-core cancel_zip_to_s3_leaves_no_partial`
Expected: PASS. (The copy of 256 KiB is fast; the test tolerates both "cancelled clean" and "completed whole" — what it forbids is a half-written object.)

- [ ] **Step 3: Commit**

```bash
git add crates/norte-core/tests/e2e_exit_criterion.rs
git commit -m "test(core): E2E 10b — cancel zip→S3 sin objeto a medias"
```

---

### Task 4: Gate — fmt, clippy, full CI

- [ ] **Step 1: fmt + clippy**

Run: `cargo fmt --all && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: clean. Watch `doc_markdown` on any doc comment (backtick `S3`, `ADR`, type names).

- [ ] **Step 2: Full CI + deny (new dep)**

Run: `just ci`
Expected: green. `opendal` dev-dep is already vetted (`norte-vfs-object` uses it) → `cargo deny` licenses stay OK. Coverage ≥ 85%.

- [ ] **Step 3: Commit if fmt changed files**

```bash
git add -A && git commit -m "style: cargo fmt fase 10b"  # only if needed
```

---

## Self-Review Notes

- **Spec coverage (10b):** read-from-zip byte-exact (Task 1) + zip→S3→local + round-trip + name preservation (Task 2) + cancel-clean (Task 3) ← spec §10b steps 1–4, 6. Resume (step 5) is covered by `engine_resume.rs`; object resume deferred (ADR 0016) — documented in the test header, not re-tested here.
- **sftp deviation:** approved — object-fs is the sole remote; sftp via its own suite + nightly (spec §10b amended).
- **No production code / no wire change.** New dev-dep `opendal` (already vetted via vfs-object).
- **Real-bug discipline:** every assertion is a truth about the cross-provider path; a red means an encoding/copy/cancel bug — fix the source.
- **Type consistency:** `setup()`, `read_all()`, `s3()`, `s3_root()`, `vp()`, `HOSTILE` used consistently; `ObjectProvider::{new, root}`, `Engine::{copy, read, stat, register_provider}`, `handle.join()→TaskState` match the verified APIs.
- **Open risk at execution:** the exact opendal `Fs`/`Operator` builder chain (`.finish()` vs not) — copy `vfs-object/tests/common::fs_operator` verbatim if the smoke test fails to compile.
- **Follow-ups:** 10c (framing fuzz), 10d (quality + M2 declaration).
