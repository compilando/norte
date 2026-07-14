# Fase 10a — Honestidad de capabilities cross-provider — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Guarantee "sin sorpresas" at the capability layer — every provider's declared `CapabilityFlags` match its real behavior, and the engine gates/degrades correctly on the cross-provider path.

**Architecture:** The DRY win is putting the declared⟺behavior cross-check INSIDE the shared `provider_contract!` macro, so every provider that uses it (Mem, Local, sftp, ftp, object) gets the check for free through its own harness. `readonly_provider_contract!` already covers READ_ONLY honesty (contract_ro.rs:308-340). A separate engine-gating test in `norte-core` validates the degradation logic (Trash-without-cap → Unsupported, copy-into-READ_ONLY → Unsupported) using cheap-to-build providers (MemProvider + ArchiveProvider over Mem).

**Tech Stack:** Rust, existing `provider_contract!`/`readonly_provider_contract!` macros, `norte-testkit` MemProvider, `ZipSmith`, `cargo nextest`.

---

## File Structure

- Modify: `crates/norte-vfs/src/contract.rs` — add `contract_capabilities_are_honest` test to the `provider_contract!` macro body.
- Create: `crates/norte-core/tests/caps_gating.rs` — engine-level gating tests (Trash/READ_ONLY/SERVER_COPY degradation).
- Reference (read only): `crates/norte-vfs/src/contract_ro.rs:308-340` (READ_ONLY honesty, already done), `crates/norte-vfs/src/provider.rs` (`copy_native` default returns `None`), `crates/norte-core/tests/engine_archive.rs` (archive-over-Mem setup pattern), `crates/norte-proto/src/caps.rs` (flags).

No new deps. `provider_contract!` change runs automatically for all provider crates using the macro; verify each still passes.

---

### Task 1: `provider_contract!` — declared⟺behavior honesty

**Files:**
- Modify: `crates/norte-vfs/src/contract.rs`

- [ ] **Step 1: Add the honesty test to the macro body**

In `crates/norte-vfs/src/contract.rs`, inside the `macro_rules! provider_contract` expansion, add a new `#[tokio::test]` alongside the existing `contract_*` tests (e.g. after the write/read section, near the existing capability-gated cases). It uses the same `$factory`/`$root`/`child`/helpers already in scope:

```rust
            // ---------- honestidad de capabilities (fase 10a) ----------

            /// Lo DECLARADO se cumple: `SERVER_COPY` ⟺ `copy_native` maneja el
            /// fichero (devuelve `Some`); sin la cap, `copy_native` = `None`
            /// para que el engine caiga a streaming en vez de fallar en duro.
            #[tokio::test]
            async fn contract_capabilities_server_copy_is_honest() {
                let p = $factory;
                let root: VPath = $root;
                let flags = p.capabilities().flags;

                // Siembra un fichero origen (si el provider es READ_ONLY no
                // aplica: esa variante va por readonly_provider_contract!).
                if flags.contains(CapabilityFlags::READ_ONLY) {
                    return;
                }
                let from = child(&root, b"cap-src.txt");
                write_bytes(&p, &from, b"honesto").await;
                let to = child(&root, b"cap-dst.txt");

                let native = p.copy_native(&from, &to).await;
                if flags.contains(CapabilityFlags::SERVER_COPY) {
                    assert!(
                        matches!(native, Some(_)),
                        "declara SERVER_COPY pero copy_native devolvió None"
                    );
                } else {
                    assert!(
                        native.is_none(),
                        "NO declara SERVER_COPY pero copy_native devolvió Some"
                    );
                }
            }
```

Note: use whatever the macro's existing write-helper is named (it defines a read helper `read_bytes`-style around line 80; find the matching write helper the other `contract_*` tests use to seed a file — e.g. the one in `contract_open_resumable_fresh_starts_at_zero` at ~193). Match that exact idiom; do not invent `write_bytes` if the macro names it differently.

- [ ] **Step 2: Run for every provider using the macro**

Run: `cargo nextest run --workspace -E 'test(contract_capabilities_server_copy_is_honest)'`
Expected: PASS for Mem, Local, sftp (in-process), ftp (in-process), object (fs). If any FAILS, that provider's declared caps lie — that is a real bug; fix the provider's `capabilities()` or `copy_native`, do not weaken the test.

- [ ] **Step 3: Commit**

```bash
git add crates/norte-vfs/src/contract.rs
git commit -m "test(vfs): provider_contract cross-check SERVER_COPY⟺copy_native (fase 10a)"
```

---

### Task 2: Engine gating — Trash & READ_ONLY degradation

**Files:**
- Create: `crates/norte-core/tests/caps_gating.rs`

- [ ] **Step 1: Write the gating tests**

Create `crates/norte-core/tests/caps_gating.rs`. Model the archive-over-Mem setup on `crates/norte-core/tests/engine_archive.rs` (seed a zip into a MemProvider with `ZipSmith`, register it, compose the archive scheme). The exact helper names (`build_zip`, `mem_with_zip`, the archive root VPath) must be copied from `engine_archive.rs` — read it first and reuse its idiom verbatim.

```rust
//! Gating de capabilities en el engine (fase 10a): el engine consulta caps y
//! degrada/gatea correcto — NUNCA degrada un `Trash` por su cuenta (ADR 0009),
//! y un provider READ_ONLY (archive) rechaza toda mutación limpio.

// (imports + setup helpers copiados de engine_archive.rs)

#[tokio::test]
async fn trash_without_capability_is_unsupported() {
    // Archive es READ_ONLY → no declara TRASH. Un delete con DeleteMode::Trash
    // sobre un path del archive debe fallar Unsupported, jamás degradar a
    // permanente por su cuenta (ADR 0009 B2).
    let (engine, archive_path) = engine_with_zip(&[("a.txt", b"x")]).await;
    let task = engine
        .delete_with(&archive_path.join(seg(b"a.txt")), DeleteMode::Trash)
        .await;
    // El engine acepta la Task pero termina Failed(Unsupported), o rechaza
    // upfront: ambas son "no degradó". Afirma que NO se borró/permanentizó.
    assert_task_unsupported(task).await;
}

#[tokio::test]
async fn copy_into_readonly_archive_is_unsupported() {
    // Copiar HACIA dentro de un archive (READ_ONLY) = Unsupported limpio.
    let (engine, archive_path) = engine_with_zip(&[("a.txt", b"x")]).await;
    // Un origen Mem normal → destino dentro del archive.
    let dst = archive_path.join(seg(b"nuevo.txt"));
    let task = engine.copy(&mem_src_path(), &dst).await;
    assert_task_unsupported(task).await;
}
```

The helpers `engine_with_zip`, `seg`, `assert_task_unsupported`, `mem_src_path` must be defined in the test file using the real Engine/TaskHandle API — read `engine_archive.rs` and `engine_resume.rs` for how they build an `Engine`, register providers, drive `copy`/`delete_with`, and await a `TaskHandle` to its terminal state. Reuse those exact patterns; every referenced symbol must resolve.

- [ ] **Step 2: Run to verify it fails, then implement helpers until it passes**

Run: `cargo nextest run -p norte-core -E 'test(/trash_without_capability|copy_into_readonly/)'`
Expected: after the helpers are written correctly, PASS. If `trash_without_capability_is_unsupported` FAILS with the archive actually being trashed/degraded, that is a real ADR-0009 violation in the engine — fix the engine's `DeleteMode::Trash` dispatch, do not weaken the test.

- [ ] **Step 3: Commit**

```bash
git add crates/norte-core/tests/caps_gating.rs
git commit -m "test(core): engine gatea Trash/READ_ONLY sin degradar solo (fase 10a)"
```

---

### Task 3: Gate — fmt, clippy, full CI

**Files:** none (verification only).

- [ ] **Step 1: Format + clippy**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 2: Full local CI**

Run: `just ci`
Expected: green. The new macro test runs across all provider crates; the coverage gate must stay ≥ 85%.

- [ ] **Step 3: Commit if fmt changed files**

```bash
git add -A && git commit -m "style: cargo fmt fase 10a"  # only if needed
```

---

## Self-Review Notes

- **Spec coverage (10a):** declared⟺behavior per provider (Task 1, via the shared macro → all providers) + engine gating Trash/READ_ONLY (Task 2) ← spec §10a "coherencia declarada↔real" + "gating del engine". READ_ONLY honesty already covered by `readonly_provider_contract!` (no new work). APPEND⟺resume and TRASH⟺trash already have dedicated contract cases (contract.rs open_resumable + contract_trash) — not duplicated.
- **No new deps / no wire change.**
- **Real-bug discipline:** every assertion is a truth about existing code; a failure means a provider or the engine lies about caps — fix the source, never the test.
- **Type consistency:** `contract_capabilities_server_copy_is_honest` uses the macro's in-scope `$factory`/`$root`/`child`/write-helper; `caps_gating.rs` reuses `engine_archive.rs` helper idioms verbatim.
- **Follow-ups:** 10b (E2E exit criterion), 10c (framing fuzz), 10d (quality pass + M2 declaration) — each its own plan.
- **Verified against codebase:** `copy_native` default returns `None` (provider.rs:203); `readonly_provider_contract!` asserts READ_ONLY excludes TRASH + mutations→Unsupported (contract_ro.rs:308-340); `provider_contract!` has `require_caps!` auto-skip + `$factory`/`$root` (contract.rs:92-105); norte-core deps include norte-vfs-archive/object/sftp; `engine_archive.rs` seeds zip-over-Mem.
