# M4-IA-2: Semantic Index Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Semantic search over the FTS5 index: an `index.embed` task generates per-file embeddings via the configured AI provider, and `index.search_semantic` answers queries by brute-force cosine scan — wire bump 0.33.0, TUI + GUI UX.

**Architecture:** Additive `embeddings` table in the existing `norte-index` SQLite DB (vectors as f32-LE BLOBs, invalidated by content hash + model id). A new cancellable `TaskKind::Embed` task reads bounded 32 KiB prefixes through providers (never direct FS), filters `denied_prefixes` BEFORE reading, and batches 16 texts per `AiProvider::embed` call (the trait method already exists; ollama + openai-compat implement it). Search is a plain request: one query-embed call + cosine scan in Rust, k ≤ 100. Full AI gate (enabled / local_only / denied_prefixes) on both; daemon restricts both endpoints to `Actor::User` (same fail-closed rule as `ai.rename_plan`).

**Tech Stack:** sqlx SQLite (existing pool in `norte-index`), `sha2` (already a norte-core dep), `norte_ai::AiProvider::embed`, tokio tasks + `CancellationToken`, proto 0.33.0.

**Spec:** `docs/superpowers/specs/2026-08-03-m4-ia-design.md` (§IA-2).

**Decisions locked here** (not in the spec, flag to reviewers):
- "Requires prior `index.build`" → `Error::NotFound` (zero `files` rows for the root). No new proto error variant; frontends map it to "run index.build first". Guardian confirms in Task 4.
- Size cap for candidates: skip files with `size > 8 MiB` (`EMBED_MAX_FILE_SIZE`); prefix read is 32 KiB regardless.
- `SemanticHit.score` is `f64` on the wire → the struct derives `PartialEq` but **not** `Eq` (unlike sibling structs).
- `IndexSearchSemanticParams.root` is `Option<VPath>` (spec says `root?`): `None` scans all roots. Needed in practice: `root_id` is keyed by the EXACT build root, and a pane's cwd rarely equals it — the TUI passes `None`.
- No `IndexEmbedResult` struct: the wire returns `FsTaskResult { task_id }` like `index.build`; progress counters tell the rest (YAGNI — `IndexBuildResult` exists but is not sent either).
- `FakeEmbed` lives in `norte-ai` behind a new `testutil` cargo feature (spec: "a norte-ai test utility"; testkit must not depend on norte-ai).
- Foreign keys: sqlite enforces `ON DELETE CASCADE` only with `foreign_keys(true)` on the connection — added to both `Index::open` paths.

---

### Task 1: `norte-index` — embeddings storage

**Files:**
- Modify: `crates/norte-index/src/lib.rs` (schema in `migrate()` ~line 157, new API after `query` ~line 330, tests at bottom)

- [ ] **Step 1: Write failing tests** — append to the `#[cfg(test)] mod tests` at the bottom of `crates/norte-index/src/lib.rs`:

```rust
#[tokio::test]
async fn embedding_upsert_and_fetch_roundtrip() {
    let idx = Index::open_memory().await.unwrap();
    let root = VPath::parse("file:///r").unwrap();
    let cancel = CancellationToken::new();
    idx.build(&root, [entry("file:///r/a.txt")], &cancel).await.unwrap();
    let cands = idx.files_for_embed(&root).await.unwrap();
    assert_eq!(cands.len(), 1);
    let id = cands[0].file_id;
    idx.upsert_embedding(id, "m1", &[1.0, 0.0], b"hash-a").await.unwrap();
    let hashes = idx.embedding_hashes(&root, "m1").await.unwrap();
    assert_eq!(hashes.get(&id).map(Vec::as_slice), Some(&b"hash-a"[..]));
    let vecs = idx.embeddings_for_root(Some(&root), "m1").await.unwrap();
    assert_eq!(vecs, vec![(cands[0].path.clone(), vec![1.0, 0.0])]);
    // upsert reemplaza
    idx.upsert_embedding(id, "m1", &[0.0, 1.0], b"hash-b").await.unwrap();
    let vecs = idx.embeddings_for_root(Some(&root), "m1").await.unwrap();
    assert_eq!(vecs[0].1, vec![0.0, 1.0]);
}

#[tokio::test]
async fn embedding_model_filter_and_all_roots() {
    let idx = Index::open_memory().await.unwrap();
    let root = VPath::parse("file:///r").unwrap();
    let cancel = CancellationToken::new();
    idx.build(&root, [entry("file:///r/a.txt")], &cancel).await.unwrap();
    let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
    idx.upsert_embedding(id, "old-model", &[1.0], b"h").await.unwrap();
    // modelo distinto ⇒ invisible (vector stale cuenta como ausente)
    assert!(idx.embedding_hashes(&root, "new-model").await.unwrap().is_empty());
    assert!(idx.embeddings_for_root(Some(&root), "new-model").await.unwrap().is_empty());
    // root None ⇒ escanea todos los roots
    assert_eq!(idx.embeddings_for_root(None, "old-model").await.unwrap().len(), 1);
}

#[tokio::test]
async fn rebuild_sweep_cascades_embedding_delete() {
    let idx = Index::open_memory().await.unwrap();
    let root = VPath::parse("file:///r").unwrap();
    let cancel = CancellationToken::new();
    idx.build(&root, [entry("file:///r/a.txt")], &cancel).await.unwrap();
    let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
    idx.upsert_embedding(id, "m", &[1.0], b"h").await.unwrap();
    // rebuild sin el fichero ⇒ sweep borra la fila y CASCADE su embedding
    idx.build(&root, std::iter::empty(), &cancel).await.unwrap();
    assert!(idx.embeddings_for_root(Some(&root), "m").await.unwrap().is_empty());
}

#[test]
fn decode_vec_rejects_ragged_blob() {
    assert_eq!(decode_vec(&encode_vec(&[1.5, -2.0])), Some(vec![1.5, -2.0]));
    assert_eq!(decode_vec(&[0u8; 5]), None);
}

#[tokio::test]
async fn files_for_embed_only_kind_file() {
    let idx = Index::open_memory().await.unwrap();
    let root = VPath::parse("file:///r").unwrap();
    let cancel = CancellationToken::new();
    let dir = IndexEntry { path: VPath::parse("file:///r/sub").unwrap(),
        kind: EntryKind::Dir, size: None, mtime_ms: None };
    idx.build(&root, [entry("file:///r/a.txt"), dir], &cancel).await.unwrap();
    assert_eq!(idx.files_for_embed(&root).await.unwrap().len(), 1);
}
```

Reuse (or add, if absent) the test helper `fn entry(p: &str) -> IndexEntry` building `IndexEntry { path: VPath::parse(p).unwrap(), kind: EntryKind::File, size: Some(4), mtime_ms: None }` — the existing test module has an equivalent; match its name.

- [ ] **Step 2: Run tests, verify failure**

Run: `cargo nextest run -p norte-index`
Expected: FAIL — `files_for_embed`, `upsert_embedding`, `encode_vec` not found.

- [ ] **Step 3: Implement.** In `migrate()` (after the trigger statements), append:

```rust
// Embeddings semánticos (M4-IA-2, ADR 0031 A3). Aditivo: un DB viejo gana
// la tabla en el siguiente open. Invalidación por (text_hash, model): un
// vector de otro modelo cuenta como ausente. El borrado de `files` (sweep
// del build) arrastra el embedding vía ON DELETE CASCADE — requiere
// foreign_keys(true) en la conexión (se activa en open/open_memory).
sqlx::query(
    "CREATE TABLE IF NOT EXISTS embeddings (
        file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        model     TEXT NOT NULL,
        dim       INTEGER NOT NULL,
        vec       BLOB NOT NULL,
        text_hash BLOB NOT NULL
    )",
)
.execute(&self.pool)
.await?;
```

In `Index::open` and `Index::open_memory`, add `.foreign_keys(true)` to the `SqliteConnectOptions` builder chain.

New public API (after `query`), with rustdoc + doctest-free examples are fine here (crate has `#![warn(missing_docs)]` — document every item):

```rust
/// Fila de `files` candidata a embedding (`kind = file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedCandidate {
    /// Rowid de `files` (clave del embedding).
    pub file_id: i64,
    /// Path completo (bytes exactos, wire encoding).
    pub path: VPath,
    /// Tamaño si el build lo conocía.
    pub size: Option<u64>,
}

/// Codifica un vector como BLOB f32 little-endian (`dim * 4` bytes).
#[must_use]
pub fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decodifica un BLOB f32-LE. `None` si la longitud no es múltiplo de 4.
#[must_use]
pub fn decode_vec(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

impl Index {
    /// Filas `kind = file` de un root — el universo de `index.embed`.
    /// Vacío ⇒ no hubo `index.build` previo (o el root no tiene ficheros).
    pub async fn files_for_embed(&self, root: &VPath) -> Result<Vec<EmbedCandidate>, IndexError> {
        let rid = root_id(root);
        let rows: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
            "SELECT id, path, size FROM files WHERE root_id = ?1 AND kind = 0",
        )
        .bind(rid)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, path, size)| {
                let path = VPath::parse(&path).ok()?;
                Some(EmbedCandidate {
                    file_id: id,
                    path,
                    size: size.and_then(|s| u64::try_from(s).ok()),
                })
            })
            .collect())
    }

    /// `file_id → text_hash` de los embeddings vigentes de un root para un
    /// modelo. Un modelo distinto no aparece (stale = ausente).
    pub async fn embedding_hashes(
        &self,
        root: &VPath,
        model: &str,
    ) -> Result<std::collections::HashMap<i64, Vec<u8>>, IndexError> {
        let rid = root_id(root);
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT e.file_id, e.text_hash FROM embeddings e
             JOIN files f ON f.id = e.file_id
             WHERE f.root_id = ?1 AND e.model = ?2",
        )
        .bind(rid)
        .bind(model)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Inserta o reemplaza el embedding de un fichero.
    pub async fn upsert_embedding(
        &self,
        file_id: i64,
        model: &str,
        vec: &[f32],
        text_hash: &[u8],
    ) -> Result<(), IndexError> {
        let dim = i64::try_from(vec.len()).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO embeddings (file_id, model, dim, vec, text_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(file_id) DO UPDATE SET
                 model = excluded.model, dim = excluded.dim,
                 vec = excluded.vec, text_hash = excluded.text_hash",
        )
        .bind(file_id)
        .bind(model)
        .bind(dim)
        .bind(encode_vec(vec))
        .bind(text_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Vectores de un root (o de todos, `root = None`) para un modelo.
    /// Filas con BLOB corrupto o `dim` incoherente se saltan, no rompen.
    pub async fn embeddings_for_root(
        &self,
        root: Option<&VPath>,
        model: &str,
    ) -> Result<Vec<(VPath, Vec<f32>)>, IndexError> {
        let rows: Vec<(String, i64, Vec<u8>)> = match root {
            Some(r) => {
                sqlx::query_as(
                    "SELECT f.path, e.dim, e.vec FROM embeddings e
                     JOIN files f ON f.id = e.file_id
                     WHERE f.root_id = ?1 AND e.model = ?2",
                )
                .bind(root_id(r))
                .bind(model)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT f.path, e.dim, e.vec FROM embeddings e
                     JOIN files f ON f.id = e.file_id
                     WHERE e.model = ?1",
                )
                .bind(model)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows
            .into_iter()
            .filter_map(|(path, dim, blob)| {
                let path = VPath::parse(&path).ok()?;
                let v = decode_vec(&blob)?;
                (i64::try_from(v.len()) == Ok(dim)).then_some((path, v))
            })
            .collect())
    }
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p norte-index && cargo clippy -p norte-index --all-targets -- -D warnings`
Expected: PASS, no warnings. (Coverage gate crate — keep tests meaningful.)

- [ ] **Step 5: Commit**

```bash
git add crates/norte-index
git commit -m "feat(index): embeddings table + vector storage API (M4-IA-2)"
```

---

### Task 2: config — `[ai] embed_provider` + `AiOp::Embed`

**Files:**
- Modify: `crates/norte-config/src/schema.rs:300-344` (`AiSection`)
- Modify: `crates/norte-config/src/load.rs:653-665` (`AiSettings`), `load.rs:842-873` (`merge_ai_layer`)
- Modify: `crates/norte-core/src/ai.rs` (`AiConfig`, `AiOp`)

- [ ] **Step 1: Write failing tests.**

In the `#[cfg(test)]` module of `crates/norte-config/src/load.rs`, next to the existing `rename_provider` merge test (grep `rename_provider` inside the test module and mirror its structure/fixture style):

```rust
#[test]
fn ai_embed_provider_last_layer_wins() {
    // misma forma que el test de rename_provider: dos capas, la última
    // presente gana; ausente en ambas ⇒ None.
    let base = r#"[ai]
embed_provider = "ollama-local"
"#;
    let over = r#"[ai]
embed_provider = "otro"
"#;
    let merged = merge_two_layers_for_test(base, over); // usa el helper del test vecino
    assert_eq!(merged.ai.embed_provider.as_deref(), Some("otro"));
}
```

(If the neighbouring test builds layers differently — e.g. via `load()` over tempdirs — copy that exact mechanism instead of a `merge_two_layers_for_test` helper; the assertion is what matters.)

In `crates/norte-core/src/ai.rs` tests module:

```rust
#[test]
fn embed_provider_config_named_else_single_else_none() {
    let mut cfg = AiConfig::default();
    assert!(cfg.embed_provider_config().is_none());
    cfg.providers.push(AiProviderConfig {
        name: "solo".into(), kind: "ollama".into(),
        model: "nomic-embed-text".into(), base_url: None,
    });
    // un único proveedor sin nombre explícito ⇒ ese
    assert_eq!(cfg.embed_provider_config().unwrap().name, "solo");
    cfg.providers.push(AiProviderConfig {
        name: "b".into(), kind: "ollama".into(), model: "x".into(), base_url: None,
    });
    // dos y sin nombre ⇒ None (ambiguo)
    assert!(cfg.embed_provider_config().is_none());
    cfg.embed_provider = Some("b".into());
    assert_eq!(cfg.embed_provider_config().unwrap().name, "b");
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-config -p norte-core ai_embed embed_provider_config`
Expected: FAIL — field/method missing (compile error).

- [ ] **Step 3: Implement.**

`schema.rs` `AiSection` — after `rename_provider`, mirroring its serde attrs and doc style:

```rust
/// Nombre del proveedor para embeddings (`index.embed` /
/// `index.search_semantic`). Ausente en todas las capas ⇒ sin embeddings
/// (los métodos degradan a `Unsupported`). Última capa presente gana.
#[serde(default)]
pub embed_provider: Option<String>,
```

`load.rs` `AiSettings` — add `pub embed_provider: Option<String>,` (with rustdoc mirroring `rename_provider`). In `merge_ai_layer`, next to the `rename_provider` merge line, add the identical pattern:

```rust
if let Some(v) = section.embed_provider {
    settings.embed_provider = Some(v);
}
```

`norte-core/src/ai.rs`:
- `AiConfig` gains `pub embed_provider: Option<String>,` (rustdoc: same contract as `rename_provider`); `from_settings` copies it.
- After `rename_provider_config` (ai.rs:78), add the symmetric selector:

```rust
/// Proveedor de embeddings: el nombrado en `embed_provider`, o el único
/// configurado si solo hay uno, o `None` (misma regla que
/// [`Self::rename_provider_config`]).
#[must_use]
pub fn embed_provider_config(&self) -> Option<&AiProviderConfig> {
    match &self.embed_provider {
        Some(name) => self.providers.iter().find(|p| &p.name == name),
        None if self.providers.len() == 1 => self.providers.first(),
        None => None,
    }
}
```

(If `rename_provider_config`'s body differs from this, copy ITS body and only swap the field — one rule, two names.)

- `AiOp` (ai.rs:197) gains a variant:

```rust
/// `index.embed` / `index.search_semantic` — prefijos de contenido o la
/// query salen hacia el proveedor.
Embed,
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-config -p norte-core && cargo clippy -p norte-config -p norte-core --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config crates/norte-core
git commit -m "feat(config,core): [ai] embed_provider + AiOp::Embed (M4-IA-2)"
```

---

### Task 3: `norte-ai` — `FakeEmbed` test utility (feature `testutil`)

**Files:**
- Modify: `crates/norte-ai/Cargo.toml` (add `[features]`)
- Create: `crates/norte-ai/src/fake.rs`
- Modify: `crates/norte-ai/src/lib.rs` (module + re-export)
- Modify: `crates/norte-core/Cargo.toml` (dev-dep feature)

- [ ] **Step 1: Write the failing test** — bottom of the new `crates/norte-ai/src/fake.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::AiProvider;

    #[tokio::test]
    async fn deterministic_and_records_inputs() {
        let f = FakeEmbed::new(8);
        let a = f.embed(&["hola".into(), "mundo".into()]).await.unwrap();
        let b = f.embed(&["hola".into()]).await.unwrap();
        assert_eq!(a[0], b[0]); // mismo texto ⇒ mismo vector
        assert_ne!(a[0], a[1]); // texto distinto ⇒ vector distinto
        assert_eq!(a[0].len(), 8);
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], vec!["hola".to_string(), "mundo".to_string()]);
    }

    #[tokio::test]
    async fn rate_limit_budget_then_ok() {
        let f = FakeEmbed::new(4).with_rate_limited(1);
        assert!(matches!(
            f.embed(&["x".into()]).await,
            Err(crate::AiError::RateLimited { .. })
        ));
        assert!(f.embed(&["x".into()]).await.is_ok());
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-ai --features testutil`
Expected: FAIL — module missing (add the feature first so the command parses: see Step 3; a missing feature is also an acceptable failure mode here).

- [ ] **Step 3: Implement.**

`crates/norte-ai/Cargo.toml` — after `[dependencies]`:

```toml
[features]
# Utilidades de test compartidas (FakeEmbed). Solo dev-deps de otros crates.
testutil = []
```

`crates/norte-ai/src/lib.rs`:

```rust
#[cfg(feature = "testutil")]
pub mod fake;
```

`crates/norte-ai/src/fake.rs`:

```rust
//! Proveedor de embeddings determinista para tests (feature `testutil`).
//!
//! El vector deriva de un hash del texto: mismo texto ⇒ mismo vector,
//! textos distintos ⇒ vectores no correlacionados. Registra cada batch
//! recibido (`calls`) para poder afirmar QUÉ salió hacia el proveedor
//! (p. ej. que un path bajo `denied_prefixes` jamás aparece).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::{AiCaps, AiError, AiProvider, ChatRequest, ChatStream, ModelInfo};

/// Proveedor fake: `embed` determinista, `chat` no soportado.
pub struct FakeEmbed {
    dim: usize,
    /// `is_local()` que anuncia (default `true`).
    pub local: bool,
    /// Batches recibidos, en orden.
    pub calls: Mutex<Vec<Vec<String>>>,
    /// Latencia artificial por llamada (ventana para tests de cancelación).
    pub delay: Option<Duration>,
    rate_limited_budget: AtomicU32,
}

impl FakeEmbed {
    /// Fake de dimensión `dim`, local, sin fallos.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            local: true,
            calls: Mutex::new(Vec::new()),
            delay: None,
            rate_limited_budget: AtomicU32::new(0),
        }
    }

    /// Las próximas `n` llamadas a `embed` devuelven `RateLimited`.
    #[must_use]
    pub fn with_rate_limited(self, n: u32) -> Self {
        self.rate_limited_budget.store(n, Ordering::SeqCst);
        self
    }

    /// Latencia artificial antes de responder.
    #[must_use]
    pub fn with_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    fn vec_for(&self, text: &str) -> Vec<f32> {
        // FNV-1a como semilla + xorshift64: determinista y sin deps.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut state = h | 1;
        let mut v: Vec<f32> = (0..self.dim)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // [-1, 1)
                ((state >> 11) as f32 / (1u64 << 53) as f32).mul_add(2.0, -1.0)
            })
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait]
impl AiProvider for FakeEmbed {
    fn id(&self) -> &str {
        "fake-embed"
    }

    fn capabilities(&self) -> AiCaps {
        AiCaps::EMBEDDINGS
    }

    fn is_local(&self) -> bool {
        self.local
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, AiError> {
        Err(AiError::Unsupported)
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        self.calls
            .lock()
            .expect("lock de test")
            .push(inputs.to_vec());
        let budget = &self.rate_limited_budget;
        if budget
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(AiError::RateLimited { retry_after: Some(0) });
        }
        Ok(inputs.iter().map(|t| self.vec_for(t)).collect())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
}
```

`crates/norte-core/Cargo.toml` — in `[dev-dependencies]`:

```toml
norte-ai = { workspace = true, features = ["testutil"] }
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-ai --features testutil && cargo clippy -p norte-ai --all-targets --features testutil -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-ai crates/norte-core/Cargo.toml
git commit -m "feat(ai): FakeEmbed deterministic test provider (feature testutil)"
```

---

### Task 4: proto 0.33.0 — `index.embed`, `index.search_semantic`, `TaskKind::Embed`

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (changelog block ~:273, `PROTOCOL_VERSION` :277, consts near :421-430, structs near :854-932)
- Modify: `crates/norte-proto/src/task.rs:46-94` (`TaskKind`)
- Modify: `crates/norte-proto/tests/golden_types.rs` (`check_methods_index` :476, `golden_task_progress` :363, count assert :440, name consts :1741)
- Modify: `crates/norte-proto/tests/golden/types/methods.json`, `.../task_progress.json`
- Modify: `docs/schema/proto.schema.json` (regenerated), `crates/norte-proto/tests/schema.rs:57-61` (register types)
- Modify: `crates/norte-tui/src/ui.rs:1005-1017`, `crates/norte-gui/src/main.rs:4252-4261` (new match arm — enum is exhaustive), `crates/norte-i18n/i18n/{en,es}.ftl` (gui-task-kind-embed)

- [ ] **Step 1: Write failing goldens.** In `golden_types.rs`, extend `check_methods_index` (or add `check_methods_semantic` beside it, same `check_one` pattern):

```rust
check_one(&fixtures, "index_embed_params", &methods::IndexEmbedParams {
    root: vpath("file:///home/user"),
});
check_one(&fixtures, "index_search_semantic_params", &methods::IndexSearchSemanticParams {
    root: Some(vpath("file:///home/user")),
    query: "informe anual".into(),
    k: 20,
});
check_one(&fixtures, "index_search_semantic_params_no_root", &methods::IndexSearchSemanticParams {
    root: None,
    query: "informe".into(),
    k: 20,
});
check_one(&fixtures, "semantic_hit", &methods::SemanticHit {
    path: vpath("file:///home/user/informe-a%FF%FE.txt"),
    score: 0.87,
});
check_one(&fixtures, "index_search_semantic_result", &methods::IndexSearchSemanticResult {
    hits: vec![methods::SemanticHit { path: vpath("file:///home/user/a.txt"), score: 0.5 }],
});
```

(Use the fixture-building helpers the file already uses — grep `fn vpath` / how `check_methods_index` constructs `VPath`.) Add matching keys to `methods.json` (score serializes as a plain JSON number; use values exact in binary like `0.5` and `0.87` — if `0.87` round-trips lossily under serde_json, switch both sides to `0.875`):

```json
"index_embed_params": {"root": "file:///home/user"},
"index_search_semantic_params": {"k": 20, "query": "informe anual", "root": "file:///home/user"},
"index_search_semantic_params_no_root": {"k": 20, "query": "informe"},
"semantic_hit": {"path": "file:///home/user/informe-a%FF%FE.txt", "score": 0.87},
"index_search_semantic_result": {"hits": [{"path": "file:///home/user/a.txt", "score": 0.5}]}
```

Bump the count assert at golden_types.rs:440 from `101` to `106`. In `golden_task_progress`, add a Rust case + `task_progress.json` key `"running_embed"` mirroring `"running_mkdir"` (golden_types.rs:381-394) with `"kind": "embed"`. Add to the method-name consts assert (:1741): `assert_eq!(methods::INDEX_EMBED, "index.embed");` and `assert_eq!(methods::INDEX_SEARCH_SEMANTIC, "index.search_semantic");`.

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-proto`
Expected: FAIL — types/consts missing.

- [ ] **Step 3: Implement proto.**

`task.rs` — before the `Unknown` variant:

```rust
/// `index.embed` (0.33.0): generación de embeddings del índice semántico.
/// Un cliente N-1 (0.32.x) la degrada a [`TaskKind::Unknown`] por su
/// `serde(other)`.
Embed,
```

`methods.rs` — changelog doc block gains:

```rust
/// 0.33.0 (M4-IA-2, ADR 0031 A3): métodos nuevos `index.embed` (Task de
/// embeddings — [`IndexEmbedParams`] → [`FsTaskResult`]) y
/// `index.search_semantic` (request directa cancelable con `rpc.cancel` —
/// [`IndexSearchSemanticParams`] → [`IndexSearchSemanticResult`]) más la
/// variante `TaskKind::Embed`. Ventana N=0.33.x / N-1=0.32.x: un cliente
/// 0.32 jamás llama a los métodos nuevos y degrada el kind nuevo a
/// `TaskKind::Unknown` por su `serde(other)` — nada que gatear en emisión.
```

`PROTOCOL_VERSION` → `"0.33.0"`. Consts next to `INDEX_QUERY` (:424):

```rust
/// `index.embed` — Task ([`TaskKind::Embed`], 0.33.0): genera embeddings de
/// los ficheros ya indexados de un root vía el proveedor de IA configurado
/// (`[ai] embed_provider`). Requiere `index.build` previo (`NotFound` si el
/// root no tiene filas). Gate de IA completo; SOLO conexión humana (una de
/// agente recibe `PolicyDenied`): prefijos de contenido salen del proceso.
pub const INDEX_EMBED: &str = "index.embed";
/// `index.search_semantic` — request directa (0.33.0), cancelable con
/// `rpc.cancel`: un embed de la query + barrido coseno en el core. `k` se
/// recorta a [`INDEX_SEMANTIC_MAX_K`]. SOLO conexión humana, como
/// [`INDEX_EMBED`] — la query sale hacia el proveedor.
pub const INDEX_SEARCH_SEMANTIC: &str = "index.search_semantic";
/// Tope de `k` en [`INDEX_SEARCH_SEMANTIC`]. Pedir más no es error: se
/// recorta (mismo patrón que [`FS_LIST_MAX_PAGE`]).
pub const INDEX_SEMANTIC_MAX_K: u32 = 100;
```

Structs next to the index family (~:854-900), matching sibling derive/attr style — **note `SemanticHit`/`IndexSearchSemanticResult` derive `PartialEq` but NOT `Eq`** (`score: f64`):

```rust
/// Params de [`INDEX_EMBED`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEmbedParams {
    /// Root YA indexado con `index.build` (misma clave exacta).
    pub root: VPath,
}

/// Params de [`INDEX_SEARCH_SEMANTIC`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSearchSemanticParams {
    /// Root a consultar; ausente ⇒ todos los roots del índice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<VPath>,
    /// Consulta en lenguaje natural (sale hacia el proveedor de IA).
    pub query: String,
    /// Máximo de hits; el server recorta a [`INDEX_SEMANTIC_MAX_K`].
    pub k: u32,
}

/// Un hit semántico: path + similitud coseno.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticHit {
    /// Path del fichero (wire encoding).
    pub path: VPath,
    /// Similitud coseno en `[-1, 1]` (mayor = más afín).
    pub score: f64,
}

/// Result de [`INDEX_SEARCH_SEMANTIC`], mejor primero.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexSearchSemanticResult {
    /// Hits ordenados por score descendente.
    pub hits: Vec<SemanticHit>,
}
```

Register the three schema types in `schema.rs:57-61` alongside the index family; regen: `NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema`.

Frontend label arms (exhaustive matches break the workspace otherwise):
- `crates/norte-tui/src/ui.rs` after the `Mkdir` arm: `norte_proto::TaskKind::Embed => "embed",`
- `crates/norte-gui/src/main.rs:4259` region: `TaskKind::Embed => "gui-task-kind-embed",`
- `crates/norte-i18n/i18n/en.ftl` (next to `gui-task-kind-index`): `gui-task-kind-embed = embed` ; `es.ftl`: `gui-task-kind-embed = embed`

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-proto -p norte-i18n && cargo test -p norte-proto --features schema --test schema && cargo clippy --workspace --all-targets -- -D warnings && (cd crates/norte-gui && cargo check --locked)`
Expected: PASS.

- [ ] **Step 5: protocol-guardian review** — dispatch the `protocol-guardian` agent over the proto diff (wire freeze: new goldens, N-1 window, `Option<VPath>` root, `f64` score, `NotFound` as the "no prior build" error). Apply findings.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-proto crates/norte-tui crates/norte-gui crates/norte-i18n docs/schema
git commit -m "feat(proto): index.embed + index.search_semantic + TaskKind::Embed (0.33.0)"
```

---

### Task 5: engine — `index_embed_as` task

**Files:**
- Create: `crates/norte-core/src/index_embed.rs`
- Modify: `crates/norte-core/src/lib.rs` (register module next to `index_build`)
- Modify: `crates/norte-core/src/engine.rs` (field `ai_embed` next to :78, setter next to :428, `index_embed_as` next to `index_build_as` :634, factor `ai_denied_to_error` out of :475-487)
- Test: `crates/norte-core/tests/index_embed.rs`

- [ ] **Step 1: Write failing tests** — `crates/norte-core/tests/index_embed.rs`. Harness mirrors `crates/norte-core/tests/index_e2e.rs` `setup()` (open_memory + `with_index` + MemProvider) plus AI plumbing:

```rust
use std::sync::Arc;
use std::time::Duration;

use norte_core::{Engine, TaskState};
use norte_ai::fake::FakeEmbed;
use norte_proto::{Error, VPath};
use norte_testkit::MemProvider;

fn ai_config_on() -> norte_core::ai::AiConfig {
    let mut c = norte_core::ai::AiConfig::default();
    c.enabled = true;
    c.providers.push(norte_core::ai::AiProviderConfig {
        name: "fake".into(), kind: "ollama".into(),
        model: "fake-model".into(), base_url: None,
    });
    c
}

async fn setup(fake: Arc<FakeEmbed>) -> (Arc<Engine>, Arc<MemProvider>) {
    let index = norte_core::Index::open_memory().await.unwrap();
    let engine = Engine::new().with_index(Arc::new(index));
    let engine = Arc::new(engine);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider("file", mem.clone()).await; // firma exacta: copiar de index_e2e.rs
    engine.set_ai_embed_provider(fake);
    engine.set_ai_config(ai_config_on());
    (engine, mem)
}

fn p(s: &str) -> VPath { VPath::parse(s).unwrap() }

async fn seed_and_build(engine: &Arc<Engine>, mem: &MemProvider) {
    mem.put_file(&p("file:///r/a.txt"), b"contenido alfa");   // helper real de MemProvider: copiar de index_e2e.rs
    mem.put_file(&p("file:///r/b.md"), b"contenido beta");
    mem.put_file(&p("file:///r/c.bin"), b"\x00\x01binario");
    let (h, _) = engine.index_build_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
}

#[tokio::test]
async fn embed_requires_prior_build() {
    let fake = Arc::new(FakeEmbed::new(8));
    let (engine, _mem) = setup(fake).await;
    let err = engine.index_embed_as(p("file:///nunca"), norte_core::journal::Actor::User)
        .await.err().unwrap();
    assert!(matches!(err, Error::NotFound));
}

#[tokio::test]
async fn embed_skips_non_text_and_records_only_text() {
    let fake = Arc::new(FakeEmbed::new(8));
    let (engine, mem) = setup(fake.clone()).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    let sent: Vec<String> = fake.calls.lock().unwrap().concat();
    assert_eq!(sent.len(), 2); // a.txt + b.md; c.bin fuera por extensión
    assert!(sent.iter().any(|t| t.contains("alfa")));
}

#[tokio::test]
async fn embed_skip_unchanged_hash_and_reembed_on_model_change() {
    let fake = Arc::new(FakeEmbed::new(8));
    let (engine, mem) = setup(fake.clone()).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    let after_first = fake.calls.lock().unwrap().concat().len();
    // segunda pasada sin cambios ⇒ cero inputs nuevos
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(fake.calls.lock().unwrap().concat().len(), after_first);
    // cambio de modelo ⇒ re-embed completo
    let mut cfg = ai_config_on();
    cfg.providers[0].model = "otro-modelo".into();
    engine.set_ai_config(cfg);
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(fake.calls.lock().unwrap().concat().len(), after_first * 2);
}

#[tokio::test]
async fn embed_denied_prefixes_excluded_before_read() {
    let fake = Arc::new(FakeEmbed::new(8));
    let (engine, mem) = setup(fake.clone()).await;
    mem.put_file(&p("file:///r/secreto/clave.txt"), b"SECRETO");
    seed_and_build(&engine, &mem).await;
    let mut cfg = ai_config_on();
    cfg.denied_prefixes.push(p("file:///r/secreto"));
    engine.set_ai_config(cfg);
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    let sent = fake.calls.lock().unwrap().concat().join("\n");
    assert!(!sent.contains("SECRETO")); // el contenido denegado JAMÁS llegó al proveedor
}

#[tokio::test]
async fn embed_clean_cancellation() {
    let fake = Arc::new(FakeEmbed::new(8).with_delay(Duration::from_millis(50)));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    h.cancel();
    let st = h.join().await;
    assert!(matches!(st, TaskState::Cancelled | TaskState::Completed)); // tolera perder la carrera
    // el índice sigue coherente: un embed posterior completa
}

#[tokio::test]
async fn embed_rate_limit_retries_then_fails() {
    // 2 fallos < 3 intentos ⇒ completa
    let fake = Arc::new(FakeEmbed::new(8).with_rate_limited(2));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    // presupuesto agotado ⇒ Failed{ProviderUnavailable}
    let fake = Arc::new(FakeEmbed::new(8).with_rate_limited(99));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert!(matches!(h.join().await,
        TaskState::Failed { error: Error::ProviderUnavailable { .. } }));
}

#[tokio::test]
async fn embed_gate_disabled_is_policy_denied() {
    let fake = Arc::new(FakeEmbed::new(8));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let mut cfg = ai_config_on();
    cfg.enabled = false;
    engine.set_ai_config(cfg);
    let err = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User)
        .await.err().unwrap();
    assert!(matches!(err, Error::PolicyDenied { .. }));
}
```

Adjust helper call signatures (`register_provider`, `MemProvider::put_file`, `TaskState::Failed` shape) to the real ones in `crates/norte-core/tests/index_e2e.rs` and `engine_search.rs` — copy their exact setup lines; the assertions above are the contract.

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-core --test index_embed`
Expected: FAIL — `set_ai_embed_provider` / `index_embed_as` missing.

- [ ] **Step 3: Implement.** `crates/norte-core/src/index_embed.rs`:

```rust
//! Task `index.embed` (M4-IA-2, ADR 0031 A3): embeddings de los ficheros ya
//! indexados. Filtra (denied_prefixes, heurística de texto, tamaño) ANTES de
//! leer nada; lee prefijos acotados por el provider (regla 2); sha256 del
//! prefijo decide re-embed; batches por el proveedor con reintento acotado
//! ante rate-limit. Cancelación cooperativa por fichero (regla 3).

use std::sync::Arc;

use futures::StreamExt;
use sha2::{Digest, Sha256};

use norte_proto::{Error, VPath};
use norte_vfs::Provider;

use crate::scheduler::TaskCtx;

/// Bytes de prefijo que se embeben por fichero (v1, hard-coded — spec §IA-2).
pub(crate) const EMBED_PREFIX_BYTES: u64 = 32 * 1024;
/// Tamaño de batch hacia el proveedor (v1, hard-coded).
pub(crate) const EMBED_BATCH: usize = 16;
/// Ficheros mayores se saltan (el prefijo de un binario gigante no es texto útil).
pub(crate) const EMBED_MAX_FILE_SIZE: u64 = 8 * 1024 * 1024;
/// Reintentos ante `RateLimited` antes de fallar la task (spec: jamás cuelga).
pub(crate) const EMBED_RETRY_MAX: u32 = 3;

/// Heurística de texto v1: por extensión (spec — sniffing de contenido es deuda).
const EMBED_TEXT_EXTS: &[&[u8]] = &[
    b"txt", b"md", b"rst", b"org", b"tex", b"rs", b"py", b"js", b"ts", b"tsx",
    b"jsx", b"go", b"java", b"kt", b"rb", b"php", b"pl", b"lua", b"c", b"h",
    b"cpp", b"hpp", b"cc", b"hh", b"cs", b"sh", b"bash", b"zsh", b"fish",
    b"toml", b"json", b"yaml", b"yml", b"xml", b"html", b"htm", b"css",
    b"sql", b"csv", b"ini", b"cfg", b"conf", b"log",
];

/// ¿Candidato a embedding? Decide SIN leer el contenido.
pub(crate) fn is_text_candidate(path: &VPath, size: Option<u64>) -> bool {
    if size.is_some_and(|s| s > EMBED_MAX_FILE_SIZE) {
        return false;
    }
    let name = path.file_name();
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return false;
    };
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 4 {
        return false;
    }
    let ext = ext.to_ascii_lowercase();
    EMBED_TEXT_EXTS.contains(&ext.as_slice())
}

fn index_err(e: norte_index::IndexError) -> Error {
    Error::Io { retryable: e.is_retryable() }
}

/// Lee el prefijo acotado de un fichero vía el provider (patrón de
/// `handle_plugin_preview`: tope + cinturón por si el provider sobre-entrega).
async fn read_prefix(provider: &dyn Provider, path: &VPath) -> Result<Vec<u8>, Error> {
    let range = norte_proto::ByteRange { offset: 0, len: Some(EMBED_PREFIX_BYTES) };
    let mut stream = provider.read(path, Some(range)).await?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= EMBED_PREFIX_BYTES {
            break;
        }
    }
    bytes.truncate(usize::try_from(EMBED_PREFIX_BYTES).unwrap_or(usize::MAX).min(bytes.len()));
    Ok(bytes)
}

/// Embebe un batch con reintento acotado ante rate-limit y persiste.
async fn flush_batch(
    embedder: &dyn norte_ai::AiProvider,
    index: &norte_index::Index,
    model: &str,
    batch: &mut Vec<(i64, String, [u8; 32])>,
) -> Result<(), Error> {
    if batch.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
    let mut attempt = 0u32;
    let vectors = loop {
        match embedder.embed(&texts).await {
            Ok(v) => break v,
            Err(norte_ai::AiError::RateLimited { retry_after }) if attempt + 1 < EMBED_RETRY_MAX => {
                attempt += 1;
                let secs = retry_after.unwrap_or(1).min(30);
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            }
            Err(e) => return Err(crate::engine::ai_to_proto_error(&e)),
        }
    };
    if vectors.len() != batch.len() {
        // Proveedor mentiroso: fallo típico de protocolo, jamás persistimos a ciegas.
        return Err(Error::Internal { panic: false });
    }
    for ((file_id, _, hash), vec) in batch.iter().zip(&vectors) {
        index
            .upsert_embedding(*file_id, model, vec, hash)
            .await
            .map_err(index_err)?;
    }
    batch.clear();
    Ok(())
}

/// Cuerpo de la task `index.embed`.
pub(crate) async fn embed_for_index(
    provider: Arc<dyn Provider>,
    embedder: norte_ai::SharedAiProvider,
    index: Arc<norte_index::Index>,
    root: VPath,
    model: String,
    denied: Vec<VPath>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let candidates = index.files_for_embed(&root).await.map_err(index_err)?;
    if candidates.is_empty() {
        // Sin `index.build` previo (o root vacío): accionable en el cliente.
        return Err(Error::NotFound);
    }
    // Filtros ANTES de leer: denied_prefixes + heurística + tamaño (spec §IA-2).
    let work: Vec<_> = candidates
        .into_iter()
        .filter(|c| !denied.iter().any(|d| crate::policy::is_under(d, &c.path)))
        .filter(|c| is_text_candidate(&c.path, c.size))
        .collect();
    let known = index.embedding_hashes(&root, &model).await.map_err(index_err)?;
    ctx.progress.update(|p| p.entries_total = Some(work.len() as u64));

    let mut batch: Vec<(i64, String, [u8; 32])> = Vec::new();
    for c in work {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| {
            p.entries_done += 1;
            p.current = Some(c.path.clone());
        });
        let bytes = match read_prefix(provider.as_ref(), &c.path).await {
            Ok(b) => b,
            // El fichero pudo morir entre build y embed: se salta, no aborta.
            Err(_) => continue,
        };
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        if known.get(&c.file_id).is_some_and(|h| h[..] == hash[..]) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        batch.push((c.file_id, text, hash));
        if batch.len() >= EMBED_BATCH {
            flush_batch(embedder.as_ref(), &index, &model, &mut batch).await?;
        }
    }
    flush_batch(embedder.as_ref(), &index, &model, &mut batch).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_heuristic_by_extension() {
        let p = |s: &str| VPath::parse(s).unwrap();
        assert!(is_text_candidate(&p("file:///a/x.txt"), Some(10)));
        assert!(is_text_candidate(&p("file:///a/x.RS"), None));
        assert!(!is_text_candidate(&p("file:///a/x.bin"), Some(10)));
        assert!(!is_text_candidate(&p("file:///a/sindot"), Some(10)));
        assert!(!is_text_candidate(&p("file:///a/x.txt"), Some(EMBED_MAX_FILE_SIZE + 1)));
    }
}
```

`lib.rs`: add `mod index_embed;` next to `mod index_build;`.

`engine.rs`:
- Field next to `ai_provider` (:78): `ai_embed: RwLock<Option<norte_ai::SharedAiProvider>>,` — initialize `RwLock::new(None)` in both constructors (:109-110 and :145-146 regions).
- Setter next to `set_ai_provider` (:428):

```rust
/// Instala el proveedor de embeddings (daemon/CLI al arranque; M4-IA-2).
pub fn set_ai_embed_provider(&self, provider: norte_ai::SharedAiProvider) {
    *self.ai_embed.write().expect("lock ai_embed") = Some(provider);
}
```

(Match the exact lock idiom `set_ai_provider` uses — copy its body shape.)
- Factor the `AiDenied` mapping out of `ai_rename_plan` (:475-487) into a helper both callers use:

```rust
fn ai_denied_to_error(d: crate::ai::AiDenied) -> Error {
    Error::PolicyDenied {
        rule: match d {
            crate::ai::AiDenied::Disabled => "ai-disabled".into(),
            crate::ai::AiDenied::LocalOnly => "ai-local-only".into(),
            crate::ai::AiDenied::DeniedPath => "ai-denied-path".into(),
        },
    }
}
```

(Copy the exact string/into style from :475-487 — the wire strings must not change.) Make `ai_to_proto_error` (:1104) `pub(crate)` so `index_embed.rs` can use it.
- New method next to `index_build_as` (:634):

```rust
/// Task `index.embed`: embeddings de los ficheros ya indexados de `root`.
/// `Unsupported` sin índice o sin proveedor de embeddings; `NotFound` sin
/// `index.build` previo; gate de IA completo (enabled/local_only/denied).
#[tracing::instrument(skip(self, actor), fields(root = %span_path(&root)))]
pub async fn index_embed_as(
    &self,
    root: VPath,
    actor: crate::journal::Actor,
) -> Result<TaskHandle, Error> {
    let index = self.index.clone().ok_or(Error::Unsupported)?;
    let embedder = self
        .ai_embed
        .read()
        .expect("lock ai_embed")
        .clone()
        .ok_or(Error::Unsupported)?;
    let (model, denied) = {
        let cfg = self.ai_config.read().expect("lock ai_config");
        let model = cfg
            .embed_provider_config()
            .ok_or(Error::Unsupported)?
            .model
            .clone();
        crate::ai::AiGate::new(&cfg)
            .check(crate::ai::AiOp::Embed, embedder.is_local(), &[&root])
            .map_err(ai_denied_to_error)?;
        (model, cfg.denied_prefixes.clone())
    };
    // Pre-chequeo fail-loud ANTES de encolar: sin build previo la task no
    // tiene universo — NotFound accionable en la respuesta, no en el join.
    if index.files_for_embed(&root).await.map_err(|e| Error::Io { retryable: e.is_retryable() })?.is_empty() {
        return Err(Error::NotFound);
    }
    let provider = self.provider_for(&root).await?;
    let key = root.scheme().to_owned();
    Ok(self.sched.submit(
        &key,
        TaskKind::Embed,
        Priority::Normal,
        actor,
        Box::new(move |ctx| {
            Box::pin(async move {
                crate::index_embed::embed_for_index(
                    provider, embedder, index, root, model, denied, &ctx,
                )
                .await
            })
        }),
    ))
}
```

(Match `index_build_as`'s exact `submit` closure idiom, lock idiom for `ai_config` — rename's code at :466-471 shows whether these are std or tokio locks; copy it.)

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-core --test index_embed && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: PASS (all 8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): index.embed task — bounded prefixes, hash skip, AI gate (M4-IA-2)"
```

---

### Task 6: engine — `index_search_semantic` + cosine

**Files:**
- Modify: `crates/norte-core/src/index_embed.rs` (cosine fn + unit golden)
- Modify: `crates/norte-core/src/engine.rs` (new method next to `index_query_as` :683)
- Test: `crates/norte-core/tests/index_embed.rs` (extend)

- [ ] **Step 1: Write failing tests.** In `index_embed.rs` tests module (cosine golden — known vectors, expected order):

```rust
#[test]
fn cosine_golden_order() {
    let q = [1.0f32, 0.0];
    assert!((cosine(&q, &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
    assert!(cosine(&q, &[0.0, 1.0]).unwrap().abs() < 1e-6);
    assert!((cosine(&q, &[-1.0, 0.0]).unwrap() + 1.0).abs() < 1e-6);
    let mid = cosine(&q, &[1.0, 1.0]).unwrap();
    assert!(mid > 0.0 && mid < 1.0);
    // dim mismatch y vector nulo ⇒ None (se ignora, no rompe)
    assert!(cosine(&q, &[1.0]).is_none());
    assert!(cosine(&q, &[0.0, 0.0]).is_none());
}
```

In `tests/index_embed.rs`:

```rust
#[tokio::test]
async fn semantic_search_finds_exact_content_top1() {
    let fake = Arc::new(FakeEmbed::new(16));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    // query == contenido exacto de a.txt ⇒ mismo vector determinista ⇒ cos 1.0
    let hits = engine.index_search_semantic(Some(&p("file:///r")), "contenido alfa", 10).await.unwrap();
    assert_eq!(hits[0].0, p("file:///r/a.txt"));
    assert!((hits[0].1 - 1.0).abs() < 1e-5);
    // root None también encuentra
    let hits = engine.index_search_semantic(None, "contenido beta", 10).await.unwrap();
    assert_eq!(hits[0].0, p("file:///r/b.md"));
}

#[tokio::test]
async fn semantic_search_clamps_k_and_ignores_stale_model() {
    let fake = Arc::new(FakeEmbed::new(16));
    let (engine, mem) = setup(fake).await;
    seed_and_build(&engine, &mem).await;
    let h = engine.index_embed_as(p("file:///r"), norte_core::journal::Actor::User).await.unwrap();
    assert_eq!(h.join().await, TaskState::Completed);
    // k=0 se recorta a 1 como mínimo útil; k enorme se recorta al tope
    let hits = engine.index_search_semantic(Some(&p("file:///r")), "x", 0).await.unwrap();
    assert!(hits.len() <= 1);
    // cambio de modelo ⇒ vectores stale invisibles ⇒ cero hits
    let mut cfg = ai_config_on();
    cfg.providers[0].model = "modelo-nuevo".into();
    engine.set_ai_config(cfg);
    let hits = engine.index_search_semantic(Some(&p("file:///r")), "contenido alfa", 10).await.unwrap();
    assert!(hits.is_empty());
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-core --test index_embed semantic`
Expected: FAIL — `index_search_semantic` / `cosine` missing.

- [ ] **Step 3: Implement.** In `index_embed.rs`:

```rust
/// Similitud coseno. `None` si las dimensiones difieren o un vector es nulo
/// (se ignora el hit, jamás rompe la búsqueda).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    (denom > 0.0).then(|| dot / denom)
}
```

In `engine.rs`, next to `index_query_as` (:683):

```rust
/// `index.search_semantic`: UNA llamada de embed para la query + barrido
/// coseno en Rust sobre los vectores del root (`None` ⇒ todos). Sin ANN
/// (ADR 0031: solo si un corpus real lo justifica). `k` se recorta a
/// [`norte_proto::methods::INDEX_SEMANTIC_MAX_K`].
#[tracing::instrument(skip(self, query))]
pub async fn index_search_semantic(
    &self,
    root: Option<&VPath>,
    query: &str,
    k: u32,
) -> Result<Vec<(VPath, f64)>, Error> {
    let index = self.index.clone().ok_or(Error::Unsupported)?;
    let embedder = self
        .ai_embed
        .read()
        .expect("lock ai_embed")
        .clone()
        .ok_or(Error::Unsupported)?;
    let model = {
        let cfg = self.ai_config.read().expect("lock ai_config");
        let model = cfg
            .embed_provider_config()
            .ok_or(Error::Unsupported)?
            .model
            .clone();
        let paths: Vec<&VPath> = root.into_iter().collect();
        crate::ai::AiGate::new(&cfg)
            .check(crate::ai::AiOp::Embed, embedder.is_local(), &paths)
            .map_err(ai_denied_to_error)?;
        model
    };
    let k = k.clamp(1, norte_proto::methods::INDEX_SEMANTIC_MAX_K) as usize;
    let qvec = embedder
        .embed(&[query.to_owned()])
        .await
        .map_err(|e| ai_to_proto_error(&e))?
        .into_iter()
        .next()
        .ok_or(Error::Internal { panic: false })?;
    let vectors = index
        .embeddings_for_root(root, &model)
        .await
        .map_err(|e| Error::Io { retryable: e.is_retryable() })?;
    let mut scored: Vec<(VPath, f64)> = vectors
        .into_iter()
        .filter_map(|(path, v)| {
            crate::index_embed::cosine(&qvec, &v).map(|s| (path, f64::from(s)))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}
```

(Adapt lock idiom as in Task 5.)

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-core --test index_embed && cargo clippy -p norte-core --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core
git commit -m "feat(core): index.search_semantic — cosine scan over root vectors (M4-IA-2)"
```

---

### Task 7: wire — daemon handlers, backend, CLI

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (arms in `dispatch_fs_task` ~:2852-2918, cancelable list :1570-1577, query cap const near :112)
- Modify: `crates/norte-core/src/backend.rs` (`Backend` methods near :454-519, remote impls near :2083-2129)
- Modify: `crates/norte-cli/src/main.rs` (`IndexCmd` :186, `index_cmd` :880, daemon startup :1191-1213)
- Test: `crates/norte-core/tests/daemon.rs` (extend)

- [ ] **Step 1: Write failing daemon tests** — in `crates/norte-core/tests/daemon.rs`, reusing the harness of the AI tests at :4503-4560 (`spawn_daemon_ai` etc.). Add a `spawn_daemon_embed(fake: Arc<FakeEmbed>)` helper mirroring `spawn_daemon_ai` but calling `engine.set_ai_embed_provider(fake)` + `set_ai_config` (enabled, one ollama-kind provider named `fake`, model `fake-model`) + `with_index(open_memory)` + MemProvider seeded with `file:///r/a.txt` = `b"contenido alfa"` and a completed `index.build` (drive it via the socket: `index.build` then drain the task to terminal). Tests:

```rust
#[tokio::test]
async fn agente_no_puede_embed_ni_semantic() {
    // conexión con agent_session ⇒ ambos endpoints PolicyDenied not-approved
    // (mismo criterio fail-closed que ai.rename_plan: contenido/query salen
    // del proceso). Espeja `agente_sin_scope_ve_policy_denied_humano_copia`.
}

#[tokio::test]
async fn semantic_por_el_socket_devuelve_hits() {
    // initialize humano → index.build → task terminal → index.embed → task
    // terminal → index.search_semantic {query:"contenido alfa", k:5}
    // ⇒ result.hits[0].path == "file:///r/a.txt".
}

#[tokio::test]
async fn embed_sin_build_previo_es_not_found() {
    // index.embed sobre un root jamás indexado ⇒ error NotFound en la
    // respuesta (no una task que falla después).
}

#[tokio::test]
async fn rpc_cancel_aborta_search_semantic_en_vuelo() {
    // FakeEmbed::with_delay(500ms) ⇒ lanzar search_semantic, rpc.cancel con
    // su id ⇒ respuesta Error::Cancelled. Espeja
    // `rpc_cancel_aborta_ai_rename_plan_en_vuelo` (daemon.rs:4716).
}

#[tokio::test]
async fn semantic_query_gigante_es_invalid_params() {
    // query > 4 KiB ⇒ INVALID_PARAMS (mismo cinturón que la instrucción de
    // ai.rename_plan en server.rs:2875).
}
```

Write these as real tests by copying the exact request/response plumbing of the neighbouring AI daemon tests (`send_req`, `read_response`, initialize handshake — whatever helpers that file uses; the comments above state the contract each must assert).

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-core --test daemon semantic embed`
Expected: FAIL — methods unknown (`METHOD_NOT_FOUND`) / helpers missing.

- [ ] **Step 3: Implement.**

`server.rs`:
- Const next to `MAX_AI_INSTRUCTION_BYTES` (:112):

```rust
/// Tope de la query de `index.search_semantic` (mismo cinturón que la
/// instrucción de `ai.rename_plan`).
const MAX_AI_QUERY_BYTES: usize = 4 * 1024;
```

- Add `methods::INDEX_SEARCH_SEMANTIC` to the cancelable-method list (:1570-1577) — the embed call blocks the serial dispatch like `ai.rename_plan`.
- Two arms in `dispatch_fs_task`, mirroring the structure/order of the `AI_RENAME_PLAN` arm (:2873-2900: actor check → caps → gate → engine → serialize):

```rust
methods::INDEX_EMBED => {
    let p: methods::IndexEmbedParams = parse_params(req.params)?;
    // SOLO humano: prefijos de contenido salen del proceso (fail-closed,
    // mismo criterio que ai.rename_plan).
    if !matches!(actor, crate::journal::Actor::User) {
        return Err(RpcError::from(Error::PolicyDenied { rule: "not-approved".into() }));
    }
    read_gate(&actor, &p.root, shared)?;
    let handle = shared
        .engine
        .index_embed_as(p.root, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}
methods::INDEX_SEARCH_SEMANTIC => {
    let p: methods::IndexSearchSemanticParams = parse_params(req.params)?;
    if !matches!(actor, crate::journal::Actor::User) {
        return Err(RpcError::from(Error::PolicyDenied { rule: "not-approved".into() }));
    }
    if p.query.len() > MAX_AI_QUERY_BYTES {
        return Err(RpcError::invalid_params("query demasiado larga"));
    }
    if let Some(root) = &p.root {
        read_gate(&actor, root, shared)?;
    }
    let hits = shared
        .engine
        .index_search_semantic(p.root.as_ref(), &p.query, p.k)
        .await
        .map_err(RpcError::from)?;
    to_value(&methods::IndexSearchSemanticResult {
        hits: hits
            .into_iter()
            .map(|(path, score)| methods::SemanticHit { path, score })
            .collect(),
    })
}
```

(Match the file's real error-construction helpers — `RpcError::from`, invalid-params idiom at :2875 — exactly; do not invent new ones. Keep zero `.await` between submit and `register_task_id`, invariant at :2932-2937.)

`backend.rs` — mirror `index_build`/`index_query`/`ai_rename_plan` (:454-519):

```rust
/// `index.embed` — Task de embeddings (M4-IA-2).
pub async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> { /* dos brazos:
    Embedded => engine.index_embed_as(root.clone(), Actor::User) → TaskRef como index_build;
    Remote => r.index_embed(root) */ }

/// `index.search_semantic` — request directa con timeout de IA y
/// cancel-on-drop (viaja `rpc.cancel`), como `ai_rename_plan`.
pub async fn index_search_semantic(
    &self,
    root: Option<&VPath>,
    query: &str,
    k: u32,
) -> Result<Vec<norte_proto::methods::SemanticHit>, Error> { /* Embedded =>
    tokio::time::timeout(AI_CALL_TIMEOUT, engine.index_search_semantic(root, query, k))
      → tuplas a SemanticHit; timeout ⇒ ProviderUnavailable{retryable:true};
    Remote => r.index_search_semantic(...) */ }
```

Remote impls (private module, next to :2083-2129):

```rust
async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> {
    let r: methods::FsTaskResult = self
        .call_timed_guarded(methods::INDEX_EMBED, &methods::IndexEmbedParams { root: root.clone() })
        .await?;
    Ok(self.own_task(r.task_id, norte_proto::TaskKind::Embed))
}

async fn index_search_semantic(
    &self,
    root: Option<&VPath>,
    query: &str,
    k: u32,
) -> Result<Vec<methods::SemanticHit>, Error> {
    let r: methods::IndexSearchSemanticResult = self
        .call_timed_guarded_with(
            AI_CALL_TIMEOUT,
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: root.cloned(),
                query: query.to_owned(),
                k,
            },
        )
        .await?;
    Ok(r.hits)
}
```

(Copy exact signatures/serde plumbing from the existing `index_build`/`ai_rename_plan` remote impls.)

`norte-cli/src/main.rs`:
- `IndexCmd` gains:

```rust
/// Genera embeddings del root ya indexado (requiere `[ai]` + embed_provider).
Embed { path: String },
/// Búsqueda semántica. Sin `--root`, busca en todos los roots indexados.
Semantic {
    text: String,
    #[arg(long)]
    root: Option<String>,
    #[arg(long, default_value_t = 20)]
    k: u32,
},
```

- `index_cmd` arms (mirror `Build`/`Query` at :880-915; parse `VPath` the same way; `Embed` runs the task via the same `run_task(...)` helper `Build` uses; `Semantic` prints one hit per line, path through the SAME masking helper the CLI's `ai rename` output uses — grep `norte ai rename`'s print path in main.rs:1341-1376 and reuse it — plus `{score:.2}`).
- Daemon startup (:1191-1213), after the rename-provider block and BEFORE `engine.set_ai_config(config)`:

```rust
if let Some(pcfg) = config.embed_provider_config() {
    match norte_core::ai::resolve_and_build(pcfg, norte_core::connect::config_dir()).await {
        Ok(p) => engine.set_ai_embed_provider(p),
        Err(e) => eprintln!("aviso: proveedor de embeddings no disponible: {e}"),
    }
}
```

Also add the same block to the TUI embedded backend AI setup (`crates/norte-tui/src/main.rs:693-711`) so the embedded TUI can search too.

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-core --test daemon && cargo nextest run -p norte-cli && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core crates/norte-cli crates/norte-tui
git commit -m "feat(core,cli): index.embed + index.search_semantic over the wire (M4-IA-2)"
```

---

### Task 8: TUI — semantic search UX

**Files:**
- Modify: `crates/norte-tui/src/keymap.rs:112` region (command), `crates/norte-tui/src/app.rs` (modal + methods, near the ai-rename block :1972-2061 and :2418-2443), `crates/norte-tui/src/main.rs` (run struct :99, interception :1492, harvest :1054, Esc :1596, dispatch :4297, navigate), `crates/norte-tui/src/ui.rs` (heights :1081, title/body :1224, new render fn near :1442)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `es.ftl`
- Test: `crates/norte-tui/tests/modal.rs`, `crates/norte-tui/src/ui.rs` unit tests, `crates/norte-tui/tests/render.rs`

- [ ] **Step 1: Write failing tests.**

`tests/modal.rs` (mirror the ai-rename prompt/plan tests at :290-366):

```rust
#[test]
fn semantic_query_modal_edita_y_confirma() {
    let mut app = test_app();
    app.open_semantic_search();
    assert!(matches!(app.modal, Some(Modal::SemanticQuery { .. })));
    for c in "facturas 2024".chars() { app.semantic_push(c); }
    assert_eq!(app.semantic_confirm().as_deref(), Some("facturas 2024"));
    app.semantic_submitted();
    assert!(app.modal.is_none());
}

#[test]
fn semantic_query_vacia_no_confirma() {
    let mut app = test_app();
    app.open_semantic_search();
    assert!(app.semantic_confirm().is_none());
    assert!(matches!(app.modal, Some(Modal::SemanticQuery { error: Some(_), .. })));
}

#[test]
fn semantic_hits_cursor_scroll_clamps() {
    let mut app = test_app();
    let hits: Vec<_> = (0..12).map(|i| norte_proto::methods::SemanticHit {
        path: norte_proto::VPath::parse(&format!("file:///r/f{i}.txt")).unwrap(),
        score: 1.0 - f64::from(i) * 0.05,
    }).collect();
    app.modal = Some(Modal::SemanticHits { hits, offset: 0, cursor: 0 });
    for _ in 0..99 { app.semantic_cursor(true); }
    let Some(Modal::SemanticHits { cursor, .. }) = &app.modal else { panic!() };
    assert_eq!(*cursor, 11); // clamp al final
}
```

`ui.rs` unit tests (mirror `mod ai_rename_plan_modal_tests` :2637 — corpus sweep):

```rust
#[test]
fn semantic_hits_barrido_corpus_ningun_hazard_sobrevive() {
    // cada nombre hostil del corpus como path de un hit ⇒ el texto renderizado
    // no contiene el hazard crudo y lleva el badge; espejo de
    // barrido_corpus_ningun_hazard_sobrevive_y_el_enmascarado_marca (:2658).
}

#[test]
fn semantic_hits_ventana_scroll_e_indicador() {
    // 12 hits, ventana de SEMANTIC_HIT_LIMIT ⇒ indicador "… shown/total" y el
    // cursor visible; espejo de plan_largo_ventana_indicador_y_alto (:2733).
}
```

`tests/render.rs`: one integration render test `modal_semantic_enmascara_hits_hostiles` mirroring `modal_de_plan_ai_enmascara_y_no_oculta_el_destino` (:423).

(Write the full test bodies by copying the mirrored tests and swapping the modal/type names — the corpus loop, mask assertions, and badge assertions carry over unchanged.)

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p norte-tui semantic`
Expected: FAIL — modal variants/methods missing.

- [ ] **Step 3: Implement.**

`keymap.rs` commands! macro: `"pane.semantic-search" => PaneSemanticSearch,` (palette-only; no preset chord — same as `pane.ai-rename` if that one has none; the help-translation pin test forces the ftl keys).

`app.rs` — next to the ai-rename modal variants:

```rust
/// Prompt de búsqueda semántica (M4-IA-2).
SemanticQuery { query: String, error: Option<String> },
/// Resultados semánticos: ventana + cursor; Enter navega al hit.
SemanticHits {
    hits: Vec<norte_proto::methods::SemanticHit>,
    offset: usize,
    cursor: usize,
},
```

Plus `pub const SEMANTIC_HIT_LIMIT: usize = 10;` (window size, like `MODAL_ITEM_LIMIT`). App methods mirror the ai_rename family exactly (open/push/pop/cancel/confirm/submitted/set_error — copy :1972-2061 and rename; cap input at `MARK_PATTERN_MAX_CHARS`), plus:

```rust
/// Mueve el cursor de hits (down=true baja); ajusta la ventana al cursor.
pub fn semantic_cursor(&mut self, down: bool) {
    if let Some(Modal::SemanticHits { hits, offset, cursor }) = &mut self.modal {
        if hits.is_empty() { return; }
        *cursor = if down { (*cursor + 1).min(hits.len() - 1) } else { cursor.saturating_sub(1) };
        // ventana sigue al cursor
        if *cursor < *offset { *offset = *cursor; }
        if *cursor >= *offset + SEMANTIC_HIT_LIMIT { *offset = *cursor + 1 - SEMANTIC_HIT_LIMIT; }
    }
}
```

`dialog_action`: `SemanticHits` joins the `ALLOW_CONFIRM` arm (:2482 region); `SemanticQuery` returns `None` (raw interception, like `AiRenameInstruction` :2654).

`main.rs`:
- `const SEMANTIC_K: u32 = 20;`
- `struct SemanticRun { handle: tokio::task::JoinHandle<Result<Vec<norte_proto::methods::SemanticHit>, Error>> }` next to `AiRenameRun` (:99) — the type matches `Backend::index_search_semantic`'s return (Task 7); `let mut semantic_run: Option<SemanticRun> = None;` next to :845.
- Dispatch: `Command::PaneSemanticSearch => app.open_semantic_search(),` (blocked in virtual search pane like ai-rename :4297-4303, message `msg-semantic-in-search`).
- Key interception for `Modal::SemanticQuery` mirroring :1492-1522; Enter:

```rust
let b = backend.clone();
let q = query.clone();
let handle = tokio::spawn(async move { b.index_search_semantic(None, &q, SEMANTIC_K).await });
if let Some(prev) = semantic_run.replace(SemanticRun { handle }) { prev.handle.abort(); }
app.message = Some(t("msg-semantic-running"));
app.semantic_submitted();
```

- Esc-in-BROWSE arm (:1596-1608): extend to also abort `semantic_run` (abort → drop → `rpc.cancel` remote / future drop embedded).
- Harvest select arm mirroring :1054-1108: `Ok(hits)` empty → `msg-semantic-empty`; non-empty → `Modal::SemanticHits { hits, offset: 0, cursor: 0 }` (no pending-stash needed: opening over an existing modal follows the same pending pattern as the AI plan if a modal is up — copy `pending_ai_plan` handling with a `pending_semantic: Option<Vec<SemanticHit>>`); `Err(e)` → `msg-semantic-failed` with `detail_for_bar`/`error_category`; join-abort → silence.
- `dialog.up`/`dialog.down` on `SemanticHits` route to `app.semantic_cursor` (like :3441-3446).
- Confirm arm (like :3492): take `hits[cursor].path`, close modal, navigate — reuse the `on_search_enter` mechanics (:3978): cd active pane to the hit's parent dir, re-anchor cursor at the path. Factor the two-liner if trivial, else duplicate its body.

`ui.rs`:
- Height arms mirroring :1081-1094 (`SemanticQuery` fixed like the instruction modal; `SemanticHits` dynamic like the plan modal).
- `modal_title_body` arms → new fns mirroring :1395/:1442:

```rust
fn semantic_query_modal_text(query: &str, error: Option<&str>) -> (String, String)
fn semantic_hits_modal_text(hits: &[SemanticHit], offset: usize, cursor: usize) -> (String, String)
```

`semantic_hits_modal_text` renders one hit per line: cursor marker `>` on the selected row, absolute numbering, `norte_frontend::path_display` (mask + hostile flag) → `badge_prefixed` (:1420) → `middle_ellipsis(…, 44)`, score `format!("{:.2}")` at line end; overflow indicator like the plan modal (`modal-semantic-more`), carrying the badge if a hidden hit is hostile.

i18n — `en.ftl` (es.ftl mirrors, parity test enforces):

```ftl
modal-semantic = Semantic search
modal-semantic-hint = Enter searches · Esc cancels
modal-semantic-empty-query = Type a query first
modal-semantic-hits = Semantic hits
modal-semantic-hit-line = { $n }. { $path }  { $score }
modal-semantic-more = … { $shown }/{ $total } (scroll: ↓/↑)
msg-semantic-running = Semantic search: thinking… (Esc cancels)
msg-semantic-empty = Semantic search: no hits
msg-semantic-failed = Semantic search failed: { $error }
msg-semantic-in-search = Not available in a search pane
help-cmd-pane-semantic-search = Semantic search over the index (AI)
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo nextest run -p norte-tui -p norte-i18n && cargo clippy -p norte-tui --all-targets -- -D warnings`
Expected: PASS (including `todo_comando_tiene_ayuda_traducida` and the corpus sweeps).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): semantic search — query prompt, cancelable run, hostile-safe hits (M4-IA-2)"
```

---

### Task 9: GUI — semantic search UX

**Files:**
- Modify: `crates/norte-gui/src/session.rs` (:178 cmd, :365 event, :581 spawn arm regions)
- Modify: `crates/norte-gui/src/modal.rs` (variants :100-111 region, outcome :137, on_key :244-327 region)
- Modify: `crates/norte-gui/src/keymap.rs` (:59 COMMANDS, :147-159 gui_supplement)
- Modify: `crates/norte-gui/src/main.rs` (dispatch :1400, event :1085, render :4381-4454, banner)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl` (gui-specific keys)

- [ ] **Step 1: Write failing tests** (norte-gui is outside the workspace — run inside the crate). Mirror the AI-plan modal tests at `crates/norte-gui/src/main.rs:6089-6140` and the modal state tests in `modal.rs`:

```rust
#[test]
fn semantic_prompt_edita_por_bytes_y_solicita() {
    let mut m = Modal::SemanticQuery { query: Vec::new() };
    // teclear "año" byte a byte vía on_key(key_char) y backspace en frontera
    // UTF-8 ⇒ espejo del test del prompt de ai-rename.
    // Enter ⇒ ModalOutcome::RequestSemantic { query: "año".into() }
}

#[test]
fn semantic_hits_enter_navega_y_scroll_clampa() {
    // Modal::SemanticHits con 12 hits ⇒ down×99 clampa; enter ⇒
    // ModalOutcome::NavigateTo(path del cursor).
}

#[test]
fn semantic_hits_ventana_enmascara_hostiles() {
    // corpus sweep sobre modal_lines: hazard crudo ausente + badge presente;
    // espejo de los tests :6089-6140.
}
```

(Full bodies: copy the mirrored tests, swap types.)

- [ ] **Step 2: Run, verify failure**

Run: `cd crates/norte-gui && cargo nextest run semantic`
Expected: FAIL.

- [ ] **Step 3: Implement.**

`session.rs`: `SessionCmd::SemanticSearch { query: String }`; `SessionEvent::SemanticHits { result: Result<Vec<norte_proto::methods::SemanticHit>, String> }`; spawn arm mirroring the AiRenamePlan arm (:581-592): `backend.index_search_semantic(None, &query, 20).await.map(|r| r.hits).map_err(|e| format!("{e}"))`.

`modal.rs`:
- `Modal::SemanticQuery { query: Vec<u8> }` (byte editing like `AiRenamePrompt` :100), `Modal::SemanticHits { hits: Vec<SemanticHit>, offset: usize, cursor: usize }`.
- `ModalOutcome` gains `RequestSemantic { query: String }` and `NavigateTo(VPath)`.
- `on_key` arms mirror `AiRenamePrompt` (:244-287: chars/backspace/enter→`RequestSemantic` after non-empty lossy decode/esc→Dismiss) and `AiRenamePlan` (:288-327: up/down move cursor+window, `enter` → `NavigateTo(hits[cursor].path.clone())`, esc → Dismiss).

`keymap.rs`: `COMMANDS` += `"pane.semantic-search"`; `gui_supplement()` += `{ on = ["alt+shift+s"], run = "pane.semantic-search" }` (if the reachability pin `todo_comando_gui_es_alcanzable_desde_el_preset_default` :416 reports a chord conflict, pick the first free `alt+shift+*` and note it in the commit).

`main.rs`:
- Dispatch: `"pane.semantic-search" => self.open_semantic_search(),` — opens `Modal::SemanticQuery { query: Vec::new() }`.
- Key routing: `RequestSemantic` → send `SessionCmd::SemanticSearch` + banner `t("gui-msg-semantic-running")` (mirror :2176-2187); `NavigateTo(path)` → navigate the active pane to the parent dir and select the entry (reuse the existing navigate-to-path mechanics the GUI uses after operations; grep how search/selection re-anchors).
- Event handler mirroring :1085-1127: empty → banner `msg-semantic-empty`; hits → open `Modal::SemanticHits` (or pend if a modal is up, mirror `pending_ai_plan` :204/:1178); Err → `banner_safe` + `msg-semantic-failed`.
- `modal_lines` arms mirroring :4381-4454: title `modal-semantic-hits`, one hit per line via the GUI's path masking + `hostile_badged` (:4463), cursor marker, score `{:.2}`, window indicator.
- Footer key hints for the two new modals (:3855-3864).

i18n: `gui-msg-semantic-running = Semantic search…` (en + es; note: no Esc-cancel claim — the GUI session has no cancel path, same honest wording as `gui-msg-ai-rename-running`).

- [ ] **Step 4: Run, verify pass**

Run: `just gui-ci` (runs nextest + clippy + fmt inside the crate)
Expected: PASS, including the keymap reachability pin and i18n parity (`cargo nextest run -p norte-i18n` from the workspace root).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-gui crates/norte-i18n
git commit -m "feat(gui): semantic search — prompt, hit list, navigate (M4-IA-2)"
```

---

### Task 10: E2E, reviewers, close

**Files:**
- Create: `crates/norte-core/tests/e2e_semantic.rs`
- Modify: `CHANGELOG.md` (if the repo keeps one — check root; else skip)
- Modify: memory + issue tracker on close

- [ ] **Step 1: E2E test** — `crates/norte-core/tests/e2e_semantic.rs`, the spec's exit-criterion shape in-process (mirror `e2e_m3.rs` / `index_e2e.rs` harness):

```rust
//! E2E M4-IA-2: corpus pequeño → index.build → index.embed → search_semantic
//! devuelve el fichero relevante; un nombre hostil sobrevive byte-exacto.

#[tokio::test]
async fn semantic_e2e_small_corpus() {
    // 1. MemProvider con 4 ficheros: tres .txt con contenidos distintos y uno
    //    con nombre hostil (bytes 0xFF 0xFE del corpus) y contenido único.
    // 2. Engine con open_memory + FakeEmbed(16) + AiConfig enabled.
    // 3. index_build_as → Completed; index_embed_as → Completed.
    // 4. search con la query == contenido exacto del hostil ⇒ top-1 es el
    //    path hostil, bytes EXACTOS al construido (round-trip).
    // 5. search con k=1 ⇒ exactamente 1 hit.
}
```

(Body: reuse the Task 5 harness helpers; the hostile name comes from `norte_testkit::corpus::hostile_names()` — pick the `0xFF 0xFE` fixture like index_e2e does.)

Run: `cargo nextest run -p norte-core --test e2e_semantic` → PASS. Commit:

```bash
git add crates/norte-core/tests/e2e_semantic.rs
git commit -m "test(core): E2E semantic search over hostile corpus (M4-IA-2)"
```

- [ ] **Step 2: Reviewer pass** — dispatch in parallel over the full IA-2 diff (`git diff <first-ia2-commit>^..HEAD`):
  - `security-reviewer`: embed sends file content off-machine — gate order, denied-before-read, User-only endpoints, secrets never logged, query cap.
  - `encoding-auditor`: hit rendering TUI/GUI (mask + badge + ellipsis), byte round-trip path→hit→navigate, lossy decode only for display/embed-input.
  - `rust-reviewer`: hard rules (no blocking I/O in async, typed errors, cancellation, no unwrap outside tests).
  Apply all findings in fix commits (one per reviewer or grouped, as IA-1 did).

- [ ] **Step 3: Gate.** proto/core/index touched ⇒ full gate once:

Run: `just ci` (includes check-gui) and `just gui-ci`
Expected: EXIT=0, coverage ≥ 85% (proto/vfs/core). If coverage dips: `cargo llvm-cov clean` first (stale-numbers trap), then re-run.

- [ ] **Step 4: Close.** If reviews deferred anything, file ONE issue (like IA-1's #121) listing: ANN index, chunked embeddings, content sniffing, GUI cancel path for in-flight semantic call, `index.embed` over selection. Final commit for changelog/notes if applicable:

```bash
git add -A
git commit -m "docs(changelog): semantic index over the wire — M4-IA-2 (proto 0.33.0)"
```

---

## Verification of exit criterion (manual, post-plan)

From the TUI against a daemon with `[ai] enabled = true`, `embed_provider` pointing at local Ollama (`nomic-embed-text`): `norte index build ~/docs` → `norte index embed ~/docs` → palette "Semantic search" → query → relevant files listed → Enter navigates. Nothing leaves the machine under default config (`enabled = false` ⇒ `PolicyDenied`).
