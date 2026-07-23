//! `norte-index`: índice `SQLite` FTS5 de nombres/metadata (spec §9, ADR 0034).
//!
//! Los path/nombre son la AUTORIDAD en su forma `VPath::to_wire()` (percent-
//! encoded, ASCII, lossless — recupera los bytes exactos vía `VPath::parse`,
//! regla 1); una vista UTF-8 lossy (`display_lossy`/`from_utf8_lossy`) alimenta
//! FTS5 para el matching. Índice de solo-lectura tras `build`; single-writer
//! (dueño = daemon), como el journal (ADR 0020).

#![warn(missing_docs)]

use std::path::Path;

use norte_proto::{EntryKind, VPath};
use sqlx::Row;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use tokio_util::sync::CancellationToken;

/// Error del índice.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// Fallo de `SQLite` (abrir, migrar, consultar).
    #[error("sqlite: {0}")]
    Sqlite(#[from] sqlx::Error),
}

/// Una entrada a indexar (proyección de `norte_vfs::Entry`). `path` completo bajo
/// el root, en bytes crudos vía `VPath` (regla 1).
#[derive(Debug, Clone)]
pub struct IndexEntry {
    /// Path completo bajo el root.
    pub path: VPath,
    /// Tipo de entrada.
    pub kind: EntryKind,
    /// Tamaño (`None` para dirs).
    pub size: Option<u64>,
    /// mtime en ms desde epoch (`None` si desconocido).
    pub mtime_ms: Option<i64>,
}

/// Resumen de un [`Index::build`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildReport {
    /// Entradas insertadas o actualizadas.
    pub indexed: u64,
    /// Filas barridas (paths que ya no existen); 0 si se canceló.
    pub removed: u64,
}

/// Un resultado de [`Index::query`]: el path (bytes exactos reconstruidos) +
/// metadata.
#[derive(Debug, Clone)]
pub struct IndexHit {
    /// Path completo (bytes exactos, vía `VPath`).
    pub path: VPath,
    /// Tipo.
    pub kind: EntryKind,
    /// Tamaño (`None` para dirs).
    pub size: Option<u64>,
    /// mtime ms (`None` si desconocido).
    pub mtime_ms: Option<i64>,
}

/// El índice: una conexión `SQLite` (WAL) con `files` + `files_fts`.
#[derive(Debug, Clone)]
pub struct Index {
    pool: SqlitePool,
}

impl Index {
    /// Abre/crea el índice en `path` (WAL, schema idempotente).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] si no se puede abrir o migrar.
    pub async fn open(path: &Path) -> Result<Self, IndexError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        let idx = Self { pool };
        idx.migrate().await?;
        Ok(idx)
    }

    /// Índice en memoria (tests). Pool de UNA conexión: cada conexión `SQLite`
    /// `:memory:` tendría su propia BD, así que se comparte una sola.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] si la migración falla.
    pub async fn open_memory() -> Result<Self, IndexError> {
        let opts = SqliteConnectOptions::new().in_memory(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let idx = Self { pool };
        idx.migrate().await?;
        Ok(idx)
    }

    async fn migrate(&self) -> Result<(), IndexError> {
        // Tabla de autoridad: `path` = VPath::to_wire() (lossless, recuperable);
        // las columnas *_display (lossy UTF-8) alimentan FTS5.
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
        Ok(())
    }

    /// (Re)indexa `root` con `entries`: upsert por `(root_id, path)`, luego barre
    /// las filas de ese root NO vistas en este build. Cancelable: si el token se
    /// dispara, se saltan tanto el resto de entradas como el BARRIDO — las filas
    /// ya insertadas persisten (superset coherente, jamás una poda equivocada).
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
        // Commit por lotes: acota el WAL y deja persistido lo hecho si se cancela.
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

    /// Busca en el índice de `root` por `text` (FTS5 MATCH, prefijo-AND de los
    /// términos), rankeado por bm25, hasta `limit` hits. El `text` del usuario se
    /// SANEA a una query FTS5 válida (no se pasa crudo — evita errores de sintaxis
    /// por `*`/`"`/`:`). Query vacía tras sanear → sin hits.
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
            // Reconstruye el VPath desde la forma wire (lossless). Un valor
            // corrupto (imposible: lo escribió `build`) se salta, no panica.
            let Ok(path) = VPath::parse(&path_wire) else {
                continue;
            };
            hits.push(IndexHit {
                path,
                kind: kind_from_i64(r.get::<i64, _>("kind")),
                size: r
                    .get::<Option<i64>, _>("size")
                    .map(|v| u64::try_from(v).unwrap_or(0)),
                mtime_ms: r.get("mtime_ms"),
            });
        }
        Ok(hits)
    }
}

/// Id estable del root desde su forma canónica (`scheme://authority` + base).
/// FNV-1a de 64 bits sobre `to_wire()`; estable entre procesos.
fn root_id(root: &VPath) -> i64 {
    let s = root.to_wire();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    i64::from_ne_bytes(h.to_ne_bytes())
}

/// Discriminante estable de `EntryKind` para la columna `kind`.
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

/// Convierte el texto libre del usuario en una query FTS5 SEGURA: separa por
/// whitespace, deja solo caracteres seguros de cada token, y emite cada token no
/// vacío como prefijo citado (`"tok"*`), unidos por espacio (AND). `None` si no
/// queda ningún token (query vacía).
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
        // Reindex: alpha se queda, beta desaparece, gamma nuevo → 1 removed.
        let r2 = idx
            .build(
                &root,
                vec![entry(&root, b"alpha.txt"), entry(&root, b"gamma.txt")],
                &tok,
            )
            .await
            .unwrap();
        assert_eq!(r2.removed, 1, "beta barrido");
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
        let hostile = b"informe-a\xff\xfe.txt"; // no-UTF8
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"informe-anual.txt"), entry(&root, hostile)],
            &tok,
        )
        .await
        .unwrap();
        let hits = idx.query(&root, "informe", 10).await.unwrap();
        assert_eq!(hits.len(), 2, "prefijo 'informe' casa ambos");
        let got: Vec<Vec<u8>> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().as_bytes().to_vec())
            .collect();
        assert!(
            got.iter().any(|n| n.as_slice() == hostile),
            "el nombre no-UTF8 vuelve BYTE-EXACTO desde la autoridad"
        );
    }

    #[tokio::test]
    async fn build_cancel_persists_partial_and_skips_sweep() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        // Primer build: 2 entradas.
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"a.txt"), entry(&root, b"b.txt")],
            &tok,
        )
        .await
        .unwrap();
        // Segundo build CANCELADO de entrada: no barre nada (a/b persisten).
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let r = idx
            .build(&root, vec![entry(&root, b"c.txt")], &cancelled)
            .await
            .unwrap();
        assert_eq!(r.removed, 0, "cancelado no barre");
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM files")
            .fetch_one(&idx.pool)
            .await
            .unwrap();
        assert_eq!(n, 2, "a y b siguen (no se podó)");
    }

    #[test]
    fn sanitize_strips_specials_and_empty_is_none() {
        assert_eq!(sanitize_fts_query("  "), None);
        assert_eq!(
            sanitize_fts_query("a*b \"c\""),
            Some("\"ab\"* \"c\"*".to_owned())
        );
    }
}
