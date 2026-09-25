//! `norte-index`: `SQLite` FTS5 name/metadata index (spec §9, ADR 0034).
//!
//! The path/name are the AUTHORITY in their `VPath::to_wire()` form
//! (percent-encoded, ASCII, lossless — recovers the exact bytes via
//! `VPath::parse`, rule 1); a lossy UTF-8 view (`display_lossy`/
//! `from_utf8_lossy`) feeds FTS5 for matching. Read-only index after
//! `build`; single-writer (owner = daemon), like the journal (ADR 0020).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use norte_proto::{EntryKind, VPath};
use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use tokio_util::sync::CancellationToken;

/// Index error.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// `SQLite` failure (open, migrate, query).
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
    /// I/O failure while pre-creating the index file with 0600 permissions.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// An attempt was made to save a dimension-0 embedding (#122).
    ///
    /// This is not a storage error but the fault of whoever brought it: a
    /// `dim = 0` row cannot score against anything —`cosine` rejects it— so
    /// it lives in the database taking up space and making the file look
    /// embedded when it is not, and the next `index.embed` does not retry it
    /// because the hash matches. It is rejected on write, which is the only
    /// place where a record is kept that the provider lied.
    #[error("empty embedding: the provider returned a dimension-0 vector")]
    EmptyVector,
}

impl IndexError {
    /// `true` if it is TRANSIENT (lock busy: `SQLITE_BUSY`/`SQLITE_LOCKED`) —
    /// the caller can retry. Corruption/other errors are not retryable.
    /// Lets the engine map it to `Error::Io { retryable }` faithfully (rust
    /// review MAJOR).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        let Self::Sqlite(e) = self else {
            return false; // Io (pre-create) is not a lock transient.
        };
        e.as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            // Primary codes SQLITE_BUSY=5, SQLITE_LOCKED=6.
            .is_some_and(|c| c == "5" || c == "6")
    }
}

/// An entry to index (projection of `norte_vfs::Entry`). `path` full under
/// the root, in raw bytes via `VPath` (rule 1).
#[derive(Debug, Clone)]
pub struct IndexEntry {
    /// Full path under the root.
    pub path: VPath,
    /// Entry type.
    pub kind: EntryKind,
    /// Size (`None` for dirs).
    pub size: Option<u64>,
    /// mtime in ms since epoch (`None` if unknown).
    pub mtime_ms: Option<i64>,
}

/// Summary of an [`Index::build`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildReport {
    /// Entries inserted or updated.
    pub indexed: u64,
    /// Rows swept (paths that no longer exist); 0 if cancelled.
    pub removed: u64,
}

/// A result of [`Index::query`]: the path (exact bytes reconstructed) +
/// metadata.
#[derive(Debug, Clone)]
pub struct IndexHit {
    /// Full path (exact bytes, via `VPath`).
    pub path: VPath,
    /// Type.
    pub kind: EntryKind,
    /// Size (`None` for dirs).
    pub size: Option<u64>,
    /// mtime ms (`None` if unknown).
    pub mtime_ms: Option<i64>,
}

/// The index: a `SQLite` connection (WAL) with `files` + `files_fts`.
#[derive(Debug, Clone)]
pub struct Index {
    pool: SqlitePool,
}

impl Index {
    /// Opens/creates the index at `path` (WAL, idempotent schema).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] if it cannot be opened or migrated.
    pub async fn open(path: &Path) -> Result<Self, IndexError> {
        // Pre-creates the file 0600 BEFORE connecting (security review
        // MEDIUM): the index stores the user's NAMES/PATHS (sensitive).
        // Without this SQLite would create it with the umask (typically
        // 0644). Same pattern as the journal; the -wal/-shm sidecars
        // inherit it. No EXCLUSIVE lock on purpose: WAL gives concurrent
        // reads and the `SQLITE_BUSY` from a simultaneous write is reported
        // retryable (see `is_retryable`) — this way the embedded CLI can
        // read even while the daemon owns the file.
        #[cfg(unix)]
        {
            // tokio::fs::{DirBuilder,OpenOptions} expose `.mode()` inherently
            // on unix (without std's ext traits).
            if let Some(parent) = path.parent() {
                let mut builder = tokio::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                let _ = builder.create(parent).await;
            }
            tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .await?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            // PIN, not a change: sqlx 0.8 already emits
            // `PRAGMA foreign_keys = ON` by default, but `embeddings`'s
            // CASCADE DEPENDS on it (SQLite only applies it per connection),
            // so it is set explicitly in case the dependency's default
            // changes.
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        let idx = Self { pool };
        idx.migrate().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Belt-and-suspenders in case the file pre-existed with other
            // permissions.
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(idx)
    }

    /// In-memory index (tests). Pool of ONE connection: each `:memory:`
    /// `SQLite` connection would have its own DB, so a single one is
    /// shared.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] if the migration fails.
    pub async fn open_memory() -> Result<Self, IndexError> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let idx = Self { pool };
        idx.migrate().await?;
        Ok(idx)
    }

    async fn migrate(&self) -> Result<(), IndexError> {
        // Authority table: `path` = VPath::to_wire() (lossless, recoverable);
        // the *_display columns (lossy UTF-8) feed FTS5.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS files (
                 id INTEGER PRIMARY KEY,
                 root_id INTEGER NOT NULL,
                 path TEXT NOT NULL,
                 name_display TEXT NOT NULL,
                 path_display TEXT NOT NULL,
                 kind INTEGER NOT NULL,
                 size INTEGER,
                 mtime_ms INTEGER,
                 last_seen_build INTEGER NOT NULL,
                 UNIQUE(root_id, path)
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
                 name_display, path_display, content='files', content_rowid='id'
             )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
                 INSERT INTO files_fts(rowid, name_display, path_display)
                 VALUES (new.id, new.name_display, new.path_display);
             END",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
                 INSERT INTO files_fts(files_fts, rowid, name_display, path_display)
                 VALUES ('delete', old.id, old.name_display, old.path_display);
             END",
        )
        .execute(&self.pool)
        .await?;
        // Semantic embeddings (M4-IA-2, ADR 0031 A3). Additive: an old DB
        // gains the table on the next open. Invalidation by
        // (text_hash, model): a vector from another model counts as absent.
        // Deleting from `files` (build's sweep) drags its embedding along
        // via ON DELETE CASCADE — requires foreign_keys(true) on the
        // connection (enabled in open/open_memory).
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
        Ok(())
    }

    /// (Re)indexes `root` with `entries`: upsert by `(root_id, path)`, then
    /// sweeps that root's rows NOT seen in this build. Cancelable: if the
    /// token fires, both the rest of the entries and the SWEEP are skipped —
    /// the rows already inserted persist (a coherent superset, never a wrong
    /// prune).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn build(
        &self,
        root: &VPath,
        entries: impl IntoIterator<Item = IndexEntry>,
        cancel: &CancellationToken,
    ) -> Result<BuildReport, IndexError> {
        let rid = root_id(root);
        let prev: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(last_seen_build), 0) FROM files WHERE root_id = ?",
        )
        .bind(rid)
        .fetch_one(&self.pool)
        .await?;
        let build_id = prev + 1;
        let mut indexed = 0u64;
        let mut cancelled = false;
        // Commit in batches: bounds the WAL and persists what was done if
        // cancelled.
        let mut tx = self.pool.begin().await?;
        let mut in_batch = 0u32;
        for e in entries {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let path_wire = e.path.to_wire();
            let name_display = e
                .path
                .file_name()
                .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
                .unwrap_or_default();
            let path_display = e.path.display_lossy();
            // The *_display columns are a FUNCTION of `path` (the conflict
            // key), so they do NOT change for a given row → the UPDATE only
            // touches kind/size/mtime and there is NO AFTER UPDATE trigger.
            // INVARIANT (rust review MINOR): never add `name_display`/
            // `path_display` to this DO UPDATE without an AFTER UPDATE
            // trigger, or the FTS goes out of sync.
            sqlx::query(
                "INSERT INTO files
                     (root_id, path, name_display, path_display, kind, size, mtime_ms, last_seen_build)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(root_id, path) DO UPDATE SET
                     kind = excluded.kind, size = excluded.size,
                     mtime_ms = excluded.mtime_ms, last_seen_build = excluded.last_seen_build",
            )
            .bind(rid)
            .bind(&path_wire)
            .bind(&name_display)
            .bind(&path_display)
            .bind(kind_to_i64(e.kind))
            .bind(e.size.map(|v| i64::try_from(v).unwrap_or(i64::MAX)))
            .bind(e.mtime_ms)
            .bind(build_id)
            .execute(&mut *tx)
            .await?;
            indexed += 1;
            in_batch += 1;
            if in_batch >= 512 {
                tx.commit().await?;
                tx = self.pool.begin().await?;
                in_batch = 0;
            }
        }
        tx.commit().await?;
        let removed = if cancelled {
            0
        } else {
            sqlx::query("DELETE FROM files WHERE root_id = ? AND last_seen_build != ?")
                .bind(rid)
                .bind(build_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
        };
        Ok(BuildReport { indexed, removed })
    }

    /// Searches `root`'s index for `text` (FTS5 MATCH, prefix-AND of the
    /// terms), ranked by bm25, up to `limit` hits. The user's `text` is
    /// SANITIZED into a valid FTS5 query (not passed raw — avoids syntax
    /// errors from `*`/`"`/`:`). An empty query after sanitizing → no hits.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn query(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
    ) -> Result<Vec<IndexHit>, IndexError> {
        let rid = root_id(root);
        let Some(fts) = sanitize_fts_query(text) else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT f.path AS path, f.kind AS kind, f.size AS size, f.mtime_ms AS mtime_ms
             FROM files_fts fts JOIN files f ON f.id = fts.rowid
             WHERE fts.files_fts MATCH ?1 AND f.root_id = ?2
             ORDER BY bm25(fts.files_fts) LIMIT ?3",
        )
        .bind(&fts)
        .bind(rid)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        let mut hits = Vec::with_capacity(rows.len());
        for r in rows {
            let path_wire: String = r.get("path");
            // Reconstructs the VPath from the wire form (lossless). A
            // corrupt value (impossible: `build` wrote it) is skipped, no
            // panic.
            let Ok(path) = VPath::parse(&path_wire) else {
                continue;
            };
            hits.push(IndexHit {
                path,
                kind: kind_from_i64(r.get::<i64, _>("kind")),
                // A negative `size` is an IMPOSSIBLE row (nothing writes it
                // that way), and that is why it matters that both reads
                // treat it the same: until #122, `query` returned it as
                // `Some(0)` —an empty file, which is an assertion— and
                // `files_for_embed` as `None` —"unknown", which is the
                // truth—. Now both say `None`.
                size: r
                    .get::<Option<i64>, _>("size")
                    .and_then(|v| u64::try_from(v).ok()),
                mtime_ms: r.get("mtime_ms"),
            });
        }
        Ok(hits)
    }

    /// `files` rows with `kind = file` under `root`: the UNIVERSE of
    /// `index.embed` (dirs/symlinks/other are not embedded). Empty ⇒ no
    /// prior build of that root.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn files_for_embed(&self, root: &VPath) -> Result<Vec<EmbedCandidate>, IndexError> {
        let rid = root_id(root);
        let rows = sqlx::query("SELECT id, path, size FROM files WHERE root_id = ?1 AND kind = ?2")
            .bind(rid)
            .bind(kind_to_i64(EntryKind::File))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                // A corrupt path (impossible: `build` wrote it) is skipped
                // — but NEVER silently (encoding audit M4-IA-2 S3): a mute
                // skip here leaves a file without an embedding and without
                // a trace of why, and the scheduler would retry it on every
                // pass. The rowid and the byte LENGTH of the stored TEXT are
                // logged; never the path (user bytes, spec §6 — a hostile
                // name is not dumped raw to a log).
                let file_id: i64 = r.get("id");
                let raw: String = r.get("path");
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "`files` row with unreadable path: skipped for embedding"
                    );
                    return None;
                };
                Some(EmbedCandidate {
                    file_id: r.get("id"),
                    path,
                    size: r
                        .get::<Option<i64>, _>("size")
                        .and_then(|s| u64::try_from(s).ok()),
                })
            })
            .collect())
    }

    /// Is there ANYTHING to embed under `root`? (#122)
    ///
    /// This is the question `index.embed`'s pre-check asks, and the only
    /// one it asks: it wanted to know if the universe is empty, and for
    /// that it materialized the ENTIRE list of candidates —with its
    /// `VPath::parse` per row— just to look at its `is_empty()` and throw it
    /// away. On a large tree that is the full sweep done twice per embed,
    /// one of them for nothing.
    ///
    /// The same predicate as [`Self::files_for_embed`], on purpose: if the
    /// two diverged, the pre-check would say "there is work" over a list
    /// that comes out empty and the Task would fail on start, which is
    /// exactly what the pre-check exists to avoid. A row with a corrupt path
    /// DOES count here and not there, and that asymmetry is the good one:
    /// it leaves the Task without work, not without a universe, and
    /// `files_for_embed` already logs why it was skipped.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn has_files_for_embed(&self, root: &VPath) -> Result<bool, IndexError> {
        let rid = root_id(root);
        let there_is: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM files WHERE root_id = ?1 AND kind = ?2)",
        )
        .bind(rid)
        .bind(kind_to_i64(EntryKind::File))
        .fetch_one(&self.pool)
        .await?;
        Ok(there_is != 0)
    }

    /// Deletes the embeddings of `root`'s files whose `file_id` is in
    /// `file_ids`, and says HOW MANY it deleted (#122).
    ///
    /// Exists so that a denial can be applied BACKWARDS. The
    /// `denied_prefixes` filter decides what gets read, i.e. it protects
    /// what has not been embedded yet; a file that was embedded BEFORE the
    /// user denied it keeps its vector saved forever, and a vector is
    /// invertible to an approximation of the text. Without this, the only
    /// way to honor a new denial was to delete the whole `index.db`.
    ///
    /// Takes `file_ids` and not paths on purpose: who falls under a prefix
    /// is decided by `norte-core` with its `policy::is_under` —which knows
    /// about folding and segment boundaries—, and reimplementing a path
    /// comparison here in SQL would be a second answer to the same
    /// question.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn forget_embeddings(&self, file_ids: &[i64]) -> Result<u64, IndexError> {
        if file_ids.is_empty() {
            return Ok(0);
        }
        // One at a time and not with a hand-built `IN (...)`: `sqlx` does
        // not bind lists, and composing the SQL with the ids would mean
        // concatenating values inside a statement. These are units or tens,
        // and this runs once per embed task.
        let mut removed = 0u64;
        for id in file_ids {
            let r = sqlx::query("DELETE FROM embeddings WHERE file_id = ?1")
                .bind(id)
                .execute(&self.pool)
                .await?;
            removed += r.rows_affected();
        }
        Ok(removed)
    }

    /// The `file_id`s and paths of ALL of `root`'s files that have a saved
    /// embedding, whatever the model (#122).
    ///
    /// "Whatever the model" is deliberate: what is being purged is user
    /// data, and a vector from an old model still is. Filtering by model
    /// would leave behind exactly the stale rows that nobody looks at again
    /// and that nothing collects.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn embedded_files(&self, root: &VPath) -> Result<Vec<(i64, VPath)>, IndexError> {
        let rows = sqlx::query(
            "SELECT e.file_id AS file_id, f.path AS path
             FROM embeddings e
             JOIN files f ON f.id = e.file_id
             WHERE f.root_id = ?1",
        )
        .bind(root_id(root))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let file_id: i64 = r.get("file_id");
                let raw: String = r.get("path");
                // An unreadable path cannot be compared against a denied
                // prefix, so it cannot be asserted that it is allowed. It is
                // left out of the purge and it is SAID: a mute skip here is
                // a vector that survives a denial without anything counting
                // it.
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "embedding with unreadable path: cannot decide whether it is denied"
                    );
                    return None;
                };
                Some((file_id, path))
            })
            .collect())
    }

    /// `text_hash` per `file_id` of `root`'s embeddings computed with
    /// `model`. An embedding from a DIFFERENT model does not appear (stale =
    /// absent): the caller will treat it as pending re-embedding. A row with
    /// an inconsistent BLOB (`length(vec) != dim * 4`) does NOT appear
    /// either: like [`Self::embeddings_for_root`] skips it in search,
    /// reporting its hash would leave it "up to date" for the scheduler but
    /// invisible — absent here ⇒ it gets re-embedded and repairs itself.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn embedding_hashes(
        &self,
        root: &VPath,
        model: &str,
    ) -> Result<std::collections::HashMap<i64, Vec<u8>>, IndexError> {
        let rid = root_id(root);
        let rows = sqlx::query(
            "SELECT e.file_id, e.text_hash FROM embeddings e
             JOIN files f ON f.id = e.file_id
             WHERE f.root_id = ?1 AND e.model = ?2
               AND length(e.vec) = e.dim * 4",
        )
        .bind(rid)
        .bind(model)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<i64, _>("file_id"), r.get::<Vec<u8>, _>("text_hash")))
            .collect())
    }

    /// Inserts or replaces `file_id`'s embedding (one vector per file:
    /// re-embedding with another model or hash REPLACES the previous one).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] (e.g. a nonexistent `file_id` violates the
    /// FK); [`IndexError::EmptyVector`] if `vec` is empty.
    pub async fn upsert_embedding(
        &self,
        file_id: i64,
        model: &str,
        vec: &[f32],
        text_hash: &[u8],
    ) -> Result<(), IndexError> {
        if vec.is_empty() {
            return Err(IndexError::EmptyVector);
        }
        sqlx::query(
            "INSERT INTO embeddings (file_id, model, dim, vec, text_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(file_id) DO UPDATE SET
                 model = excluded.model, dim = excluded.dim,
                 vec = excluded.vec, text_hash = excluded.text_hash",
        )
        .bind(file_id)
        .bind(model)
        .bind(i64::try_from(vec.len()).unwrap_or(i64::MAX))
        .bind(encode_vec(vec))
        .bind(text_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `model`'s embeddings as `(path, vector)`: from a specific `root`, or
    /// from ALL roots if `root` is `None`. Rows with a corrupt BLOB or an
    /// inconsistent `dim` are SKIPPED (they never break the search).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn embeddings_for_root(
        &self,
        root: Option<&VPath>,
        model: &str,
    ) -> Result<Vec<(VPath, Vec<f32>)>, IndexError> {
        let rows = if let Some(root) = root {
            sqlx::query(
                "SELECT e.file_id AS file_id, f.path AS path, e.dim AS dim, e.vec AS vec
                 FROM embeddings e
                 JOIN files f ON f.id = e.file_id
                 WHERE f.root_id = ?1 AND e.model = ?2",
            )
            .bind(root_id(root))
            .bind(model)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT e.file_id AS file_id, f.path AS path, e.dim AS dim, e.vec AS vec
                 FROM embeddings e
                 JOIN files f ON f.id = e.file_id
                 WHERE e.model = ?1",
            )
            .bind(model)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                // Unreadable path ⇒ skipped, but with a TRACE (encoding
                // audit M4-IA-2 S3): without the warn, an orphaned
                // embedding disappears from every semantic search without
                // anything saying so. The `file_id` and the TEXT's length
                // are logged, never the path's bytes (spec §6: not dumped
                // raw to a log).
                let file_id: i64 = r.get("file_id");
                let raw: String = r.get("path");
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "embedding with unreadable path: skipped in search"
                    );
                    return None;
                };
                let v = decode_vec(&r.get::<Vec<u8>, _>("vec"))?;
                // dim⟷blob consistency: a corrupt row is skipped, no panic.
                (i64::try_from(v.len()) == Ok(r.get::<i64, _>("dim"))).then_some((path, v))
            })
            .collect())
    }
}

/// A `files` row that is a candidate for embedding (`kind = file`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedCandidate {
    /// `files`'s rowid (the embedding's key).
    pub file_id: i64,
    /// Full path (exact bytes, wire encoding).
    pub path: VPath,
    /// Size if the build knew it.
    pub size: Option<u64>,
}

/// Encodes a vector as a little-endian f32 BLOB (`dim * 4` bytes).
#[must_use]
pub fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decodes an f32-LE BLOB. `None` if the length is not a multiple of 4.
#[must_use]
pub fn decode_vec(blob: &[u8]) -> Option<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Stable id of the root from its canonical form (`scheme://authority` +
/// base). 64-bit FNV-1a over `to_wire()`; stable across processes AND
/// across endianness (little-endian bytes, not `to_ne_bytes` — this way a
/// `.db` copied to another machine keeps its id, encoding review). It is
/// only a SCOPING key (`WHERE root_id = ?`), never an authority: a
/// collision (64 bits, minuscule) would mix at most two roots, without
/// corrupting bytes. Debt (internal root table) in ADR 0034 if the
/// confused-deputy matters.
fn root_id(root: &VPath) -> i64 {
    let s = root.to_wire();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    i64::from_le_bytes(h.to_le_bytes())
}

/// Stable discriminant of `EntryKind` for the `kind` column.
fn kind_to_i64(k: EntryKind) -> i64 {
    match k {
        EntryKind::File => 0,
        EntryKind::Dir => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other => 3,
    }
}

fn kind_from_i64(v: i64) -> EntryKind {
    match v {
        1 => EntryKind::Dir,
        2 => EntryKind::Symlink,
        3 => EntryKind::Other,
        _ => EntryKind::File,
    }
}

/// Converts the user's free text into a SAFE FTS5 query: splits on
/// whitespace, keeps only safe characters from each token, and emits every
/// non-empty token as a quoted prefix (`"tok"*`), joined by a space (AND).
/// `None` if no token remains (empty query).
fn sanitize_fts_query(text: &str) -> Option<String> {
    let mut parts = Vec::new();
    for tok in text.split_whitespace() {
        let cleaned: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if !cleaned.is_empty() {
            parts.push(format!("\"{cleaned}\"*"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Scheme, Segment};

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    fn entry(root: &VPath, name: &[u8]) -> IndexEntry {
        IndexEntry {
            path: root.join(Segment::new(name.to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(10),
            mtime_ms: Some(1),
        }
    }

    #[tokio::test]
    async fn open_memory_migrates() {
        let idx = Index::open_memory().await.expect("open");
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM files")
            .fetch_one(&idx.pool)
            .await
            .expect("count");
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn build_indexes_entries_and_reindex_sweeps() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        let r = idx
            .build(
                &root,
                vec![entry(&root, b"alpha.txt"), entry(&root, b"beta.txt")],
                &tok,
            )
            .await
            .unwrap();
        assert_eq!(r.indexed, 2);
        assert_eq!(r.removed, 0);
        // Reindex: alpha stays, beta disappears, gamma is new → 1 removed.
        let r2 = idx
            .build(
                &root,
                vec![entry(&root, b"alpha.txt"), entry(&root, b"gamma.txt")],
                &tok,
            )
            .await
            .unwrap();
        assert_eq!(r2.removed, 1, "beta swept");
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM files")
            .fetch_one(&idx.pool)
            .await
            .unwrap();
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn query_matches_and_non_utf8_roundtrips_byte_exact() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let hostile = b"report-a\xff\xfe.txt"; // non-UTF8
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"annual-report.txt"), entry(&root, hostile)],
            &tok,
        )
        .await
        .unwrap();
        let hits = idx.query(&root, "report", 10).await.unwrap();
        assert_eq!(hits.len(), 2, "the 'report' prefix matches both");
        let got: Vec<Vec<u8>> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().as_bytes().to_vec())
            .collect();
        assert!(
            got.iter().any(|n| n.as_slice() == hostile),
            "the non-UTF8 name comes back BYTE-EXACT from the authority"
        );
    }

    #[tokio::test]
    async fn build_cancel_persists_partial_superset_and_skips_sweep() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        // First build: a, b.
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"a.txt"), entry(&root, b"b.txt")],
            &tok,
        )
        .await
        .unwrap();
        // Second build that INSERTS c and then cancels BEFORE d: the token
        // fires when pulling the 2nd item (i==1), so c was already
        // processed but the sweep is skipped. Result = SUPERSET (old a, b +
        // new c), removed=0. This tests the "coherent superset" path, not
        // just the skip-sweep (rust MINOR).
        let cancelled = CancellationToken::new();
        let c2 = cancelled.clone();
        let items = vec![entry(&root, b"c.txt"), entry(&root, b"d.txt")]
            .into_iter()
            .enumerate()
            .map(move |(i, e)| {
                if i == 1 {
                    c2.cancel();
                }
                e
            });
        let r = idx.build(&root, items, &cancelled).await.unwrap();
        assert_eq!(
            r.removed, 0,
            "cancelled does NOT sweep (b/d are not pruned)"
        );
        let names: Vec<String> = sqlx::query_scalar("SELECT path FROM files ORDER BY path")
            .fetch_all(&idx.pool)
            .await
            .unwrap();
        // a, b (old) + c (new partial); d was never inserted.
        assert_eq!(names.len(), 3, "superset a+b+c, was {names:?}");
        assert!(
            names.iter().any(|p| p.ends_with("c.txt")),
            "partial c persisted"
        );
        assert!(
            names.iter().all(|p| !p.ends_with("d.txt")),
            "d was not inserted"
        );
    }

    #[test]
    fn sanitize_strips_specials_and_empty_is_none() {
        assert_eq!(sanitize_fts_query("  "), None);
        assert_eq!(
            sanitize_fts_query("a*b \"c\""),
            Some("\"ab\"* \"c\"*".to_owned())
        );
    }

    #[tokio::test]
    async fn embedding_upsert_and_fetch_roundtrip() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let cands = idx.files_for_embed(&root).await.unwrap();
        assert_eq!(cands.len(), 1);
        let id = cands[0].file_id;
        idx.upsert_embedding(id, "m1", &[1.0, 0.0], b"hash-a")
            .await
            .unwrap();
        let hashes = idx.embedding_hashes(&root, "m1").await.unwrap();
        assert_eq!(hashes.get(&id).map(Vec::as_slice), Some(&b"hash-a"[..]));
        let vecs = idx.embeddings_for_root(Some(&root), "m1").await.unwrap();
        assert_eq!(vecs, vec![(cands[0].path.clone(), vec![1.0, 0.0])]);
        // Re-embed of the same file: the upsert replaces the vector and hash.
        idx.upsert_embedding(id, "m1", &[0.0, 1.0], b"hash-b")
            .await
            .unwrap();
        let vecs = idx.embeddings_for_root(Some(&root), "m1").await.unwrap();
        assert_eq!(vecs[0].1, vec![0.0, 1.0]);
    }

    #[tokio::test]
    async fn embedding_model_filter_and_all_roots() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
        idx.upsert_embedding(id, "old-model", &[1.0], b"h")
            .await
            .unwrap();
        // Different model ⇒ the old embedding counts as ABSENT.
        assert!(
            idx.embedding_hashes(&root, "new-model")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            idx.embeddings_for_root(Some(&root), "new-model")
                .await
                .unwrap()
                .is_empty()
        );
        // Without a root filter: the old model's shows up.
        assert_eq!(
            idx.embeddings_for_root(None, "old-model")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn rebuild_sweep_cascades_embedding_delete() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
        idx.upsert_embedding(id, "m", &[1.0], b"h").await.unwrap();
        // Rebuild without the file: the sweep deletes the `files` row and
        // the ON DELETE CASCADE drags its embedding along (pin of
        // foreign_keys=ON).
        idx.build(&root, std::iter::empty::<IndexEntry>(), &tok)
            .await
            .unwrap();
        assert!(
            idx.embeddings_for_root(Some(&root), "m")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn decode_vec_rejects_ragged_blob() {
        assert_eq!(decode_vec(&encode_vec(&[1.5, -2.0])), Some(vec![1.5, -2.0]));
        assert_eq!(decode_vec(&[0u8; 5]), None);
    }

    #[tokio::test]
    async fn embedding_hashes_skips_corrupt_blob_so_it_reembeds() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
        idx.upsert_embedding(id, "m", &[1.0, 0.0], b"h")
            .await
            .unwrap();
        // Corrupts the BLOB by hand (length != dim * 4): the row must read
        // as ABSENT in embedding_hashes — if it returned the hash, the
        // scheduler would believe it up to date and it would never be
        // repaired (review MAJOR-1).
        sqlx::query("UPDATE embeddings SET vec = X'00' WHERE file_id = ?")
            .bind(id)
            .execute(&idx.pool)
            .await
            .unwrap();
        assert!(idx.embedding_hashes(&root, "m").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rebuild_with_file_present_preserves_embedding() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let id = idx.files_for_embed(&root).await.unwrap()[0].file_id;
        idx.upsert_embedding(id, "m", &[1.0], b"h").await.unwrap();
        // Rebuild with the file STILL present: `build`'s upsert must keep
        // the rowid (ON CONFLICT DO UPDATE, never INSERT OR REPLACE) or the
        // CASCADE would sweep ALL embeddings on every rebuild.
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        assert_eq!(
            idx.embeddings_for_root(Some(&root), "m")
                .await
                .unwrap()
                .len(),
            1,
            "the rebuild preserves the embedding (stable rowid)"
        );
    }

    #[tokio::test]
    async fn embedding_hostile_path_roundtrips_byte_exact() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let hostile = b"report-a\xff\xfe.txt"; // non-UTF8
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, hostile)], &tok)
            .await
            .unwrap();
        let cands = idx.files_for_embed(&root).await.unwrap();
        assert_eq!(cands.len(), 1);
        idx.upsert_embedding(cands[0].file_id, "m", &[1.0], b"h")
            .await
            .unwrap();
        let vecs = idx.embeddings_for_root(Some(&root), "m").await.unwrap();
        assert_eq!(
            vecs[0].0.file_name().unwrap().as_bytes(),
            hostile,
            "the non-UTF8 name comes back BYTE-EXACT through the embeddings path"
        );
    }

    #[tokio::test]
    async fn upsert_embedding_nonexistent_file_id_errors() {
        let idx = Index::open_memory().await.unwrap();
        // FK: a file_id that does not exist in `files` is REJECTED, not
        // inserted.
        assert!(idx.upsert_embedding(999, "m", &[1.0], b"h").await.is_err());
    }

    #[tokio::test]
    async fn files_for_embed_only_kind_file() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        let dir = IndexEntry {
            path: root.join(Segment::new(b"sub".to_vec()).unwrap()),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        };
        idx.build(&root, vec![entry(&root, b"a.txt"), dir], &tok)
            .await
            .unwrap();
        assert_eq!(idx.files_for_embed(&root).await.unwrap().len(), 1);
    }

    /// **A saved vector can be FORGOTTEN** (#122): without this, the only
    /// way to honor a new denial was to delete `index.db` entirely.
    #[tokio::test]
    async fn an_embedding_can_be_forgotten() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"public.txt"), entry(&root, b"secret.txt")],
            &tok,
        )
        .await
        .unwrap();
        for c in idx.files_for_embed(&root).await.unwrap() {
            idx.upsert_embedding(c.file_id, "m", &[1.0, 2.0], b"h")
                .await
                .unwrap();
        }
        assert_eq!(idx.embedded_files(&root).await.unwrap().len(), 2);

        let secret = idx
            .embedded_files(&root)
            .await
            .unwrap()
            .into_iter()
            .find(|(_, p)| p.display_lossy().ends_with("secret.txt"))
            .expect("is there");
        assert_eq!(idx.forget_embeddings(&[secret.0]).await.unwrap(), 1);

        let remaining = idx.embedded_files(&root).await.unwrap();
        assert_eq!(remaining.len(), 1, "only the denied one is gone");
        assert!(remaining[0].1.display_lossy().ends_with("public.txt"));
        // And it disappears from the SEARCH, which is what really matters: a
        // vector that still scores is the file's text answering.
        assert_eq!(
            idx.embeddings_for_root(Some(&root), "m")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Forgetting an empty list deletes nothing. This is the case for every
    /// embed task with nothing denied, i.e. the common one.
    #[tokio::test]
    async fn forgetting_nothing_deletes_nothing() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let c = idx.files_for_embed(&root).await.unwrap();
        idx.upsert_embedding(c[0].file_id, "m", &[1.0], b"h")
            .await
            .unwrap();
        assert_eq!(idx.forget_embeddings(&[]).await.unwrap(), 0);
        assert_eq!(idx.embedded_files(&root).await.unwrap().len(), 1);
    }

    /// **`embedded_files` does not filter by model, and it is deliberate**:
    /// a vector from an old model is still user data. Filtering would leave
    /// behind exactly the stale rows that nobody looks at again and that
    /// nothing collects.
    #[tokio::test]
    async fn a_vector_from_another_model_is_also_visible_for_purging() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let c = idx.files_for_embed(&root).await.unwrap();
        idx.upsert_embedding(c[0].file_id, "old-model", &[1.0], b"h")
            .await
            .unwrap();

        assert!(
            idx.embeddings_for_root(Some(&root), "new-model")
                .await
                .unwrap()
                .is_empty(),
            "the search with the new model no longer sees it…"
        );
        assert_eq!(
            idx.embedded_files(&root).await.unwrap().len(),
            1,
            "…but the purge does, which is the point"
        );
    }
}
