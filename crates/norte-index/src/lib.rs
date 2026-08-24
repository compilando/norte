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
    /// Fallo de I/O al pre-crear el fichero del índice con permisos 0600.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl IndexError {
    /// `true` si es TRANSITORIO (lock ocupado: `SQLITE_BUSY`/`SQLITE_LOCKED`) —
    /// el caller puede reintentar. La corrupción/otros no son retryables. Sirve
    /// para que el engine mapee a `Error::Io { retryable }` con fidelidad (rust
    /// review MAJOR).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        let Self::Sqlite(e) = self else {
            return false; // Io (pre-create) no es transitorio de lock.
        };
        e.as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            // Códigos primarios SQLITE_BUSY=5, SQLITE_LOCKED=6.
            .is_some_and(|c| c == "5" || c == "6")
    }
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
        // Pre-crea el fichero 0600 ANTES de conectar (security review MEDIUM): el
        // índice guarda NOMBRES/PATHS del usuario (sensibles). Sin esto SQLite lo
        // crearía con el umask (típicamente 0644). Mismo patrón que el journal;
        // los sidecars -wal/-shm heredan. Sin lock EXCLUSIVO a propósito: el WAL
        // da lectura concurrente y el `SQLITE_BUSY` de una escritura simultánea se
        // reporta retryable (ver `is_retryable`) — así el CLI embebido puede leer
        // aunque el daemon posea el fichero.
        #[cfg(unix)]
        {
            // tokio::fs::{DirBuilder,OpenOptions} exponen `.mode()` inherente en
            // unix (sin los traits ext de std).
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
            // PIN, no cambio: sqlx 0.8 ya emite `PRAGMA foreign_keys = ON` por
            // defecto, pero el CASCADE de `embeddings` DEPENDE de ello (SQLite
            // solo lo aplica por conexión), así que se fija explícito por si el
            // default de la dependencia cambia.
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        let idx = Self { pool };
        idx.migrate().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Cinturón si el fichero preexistía con otros permisos.
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(idx)
    }

    /// Índice en memoria (tests). Pool de UNA conexión: cada conexión `SQLite`
    /// `:memory:` tendría su propia BD, así que se comparte una sola.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] si la migración falla.
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
            // Las columnas *_display son FUNCIÓN de `path` (la clave del
            // conflicto), así que NO cambian para una fila dada → el UPDATE solo
            // toca kind/size/mtime y NO hay trigger AFTER UPDATE. INVARIANTE
            // (rust review MINOR): jamás añadir `name_display`/`path_display` a
            // este DO UPDATE sin un trigger AFTER UPDATE, o la FTS se desincroniza.
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

    /// Filas de `files` con `kind = file` bajo `root`: el UNIVERSO de
    /// `index.embed` (los dirs/symlinks/other no se embeben). Vacío ⇒ sin
    /// build previo de ese root.
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
                // Un path corrupto (imposible: lo escribió `build`) se salta
                // — pero JAMÁS en silencio (encoding audit M4-IA-2 S3): un
                // salto mudo aquí deja un fichero sin embedding y sin rastro
                // de por qué, y el scheduler lo reintentaría en cada pasada.
                // Se registra el rowid y la LONGITUD en bytes del TEXT
                // guardado; nunca el path (bytes de usuario, spec §6 — un
                // nombre hostil no se vuelca crudo a un log).
                let file_id: i64 = r.get("id");
                let raw: String = r.get("path");
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "fila de `files` con path ilegible: se salta para embedding"
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

    /// Borra los embeddings de los ficheros de `root` cuyo `file_id` esté en
    /// `file_ids`, y dice CUÁNTOS borró (#122).
    ///
    /// Existe para que una denegación pueda aplicarse hacia ATRÁS. El filtro
    /// de `denied_prefixes` decide qué se lee, o sea que protege lo que
    /// todavía no se ha embebido; un fichero que se embebió ANTES de que el
    /// usuario lo denegara deja su vector guardado para siempre, y un vector
    /// es invertible a una aproximación del texto. Sin esto, la única forma de
    /// honrar una denegación nueva era borrar `index.db` entero.
    ///
    /// Toma `file_ids` y no rutas a propósito: quién cae bajo un prefijo lo
    /// decide `norte-core` con su `policy::is_under` —que sabe de plegado y de
    /// fronteras de segmento—, y reimplementar aquí una comparación de rutas
    /// en SQL sería una segunda respuesta a la misma pregunta.
    ///
    /// # Errors
    /// [`IndexError::Sqlite`].
    pub async fn forget_embeddings(&self, file_ids: &[i64]) -> Result<u64, IndexError> {
        if file_ids.is_empty() {
            return Ok(0);
        }
        // De uno en uno y no con un `IN (...)` construido a mano: `sqlx` no
        // liga listas, y componer el SQL con los ids sería concatenar valores
        // dentro de una sentencia. Son unidades o decenas, y esto corre una
        // vez por task de embed.
        let mut borrados = 0u64;
        for id in file_ids {
            let r = sqlx::query("DELETE FROM embeddings WHERE file_id = ?1")
                .bind(id)
                .execute(&self.pool)
                .await?;
            borrados += r.rows_affected();
        }
        Ok(borrados)
    }

    /// Los `file_id` y rutas de TODOS los ficheros de `root` que tienen
    /// embedding guardado, sea del modelo que sea (#122).
    ///
    /// «Sea del modelo que sea» es deliberado: lo que se purga es un dato del
    /// usuario, y un vector de un modelo viejo lo sigue siendo. Filtrar por
    /// modelo dejaría atrás justo las filas rancias que nadie vuelve a mirar y
    /// que nada recoge.
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
                // Un path ilegible no se puede comparar contra un prefijo
                // denegado, así que NO se puede afirmar que esté permitido. Se
                // deja fuera de la purga y se DICE: un salto mudo aquí es un
                // vector que sobrevive a una denegación sin que nada lo cuente.
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "embedding con path ilegible: no se puede decidir si está denegado"
                    );
                    return None;
                };
                Some((file_id, path))
            })
            .collect())
    }

    /// `text_hash` por `file_id` de los embeddings de `root` calculados con
    /// `model`. Un embedding de un modelo DISTINTO no aparece (stale = ausente):
    /// el caller lo tratará como pendiente de re-embeber. Una fila con BLOB
    /// incoherente (`length(vec) != dim * 4`) TAMPOCO aparece: como
    /// [`Self::embeddings_for_root`] la salta en búsqueda, reportar su hash la
    /// dejaría "al día" para el scheduler pero invisible — ausente aquí ⇒ se
    /// re-embebe y se repara sola.
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

    /// Inserta o reemplaza el embedding de `file_id` (un vector por fichero:
    /// re-embeber con otro modelo o hash SUSTITUYE al anterior).
    ///
    /// # Errors
    /// [`IndexError::Sqlite`] (p. ej. `file_id` inexistente viola la FK).
    pub async fn upsert_embedding(
        &self,
        file_id: i64,
        model: &str,
        vec: &[f32],
        text_hash: &[u8],
    ) -> Result<(), IndexError> {
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

    /// Los embeddings de `model` como `(path, vector)`: de un `root` concreto, o
    /// de TODOS los roots si `root` es `None`. Las filas con BLOB corrupto o
    /// `dim` incoherente se SALTAN (jamás rompen la búsqueda).
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
                // Path ilegible ⇒ se salta, pero con RASTRO (encoding audit
                // M4-IA-2 S3): sin el warn, un embedding húerfano desaparece
                // de toda búsqueda semántica sin que nada lo diga. Se
                // registra el `file_id` y la longitud del TEXT, jamás los
                // bytes del path (spec §6: no se vuelcan crudos a un log).
                let file_id: i64 = r.get("file_id");
                let raw: String = r.get("path");
                let Ok(path) = VPath::parse(&raw) else {
                    tracing::warn!(
                        file_id,
                        path_len = raw.len(),
                        "embedding con path ilegible: se salta en la búsqueda"
                    );
                    return None;
                };
                let v = decode_vec(&r.get::<Vec<u8>, _>("vec"))?;
                // Coherencia dim⟷blob: una fila corrupta se salta, no panica.
                (i64::try_from(v.len()) == Ok(r.get::<i64, _>("dim"))).then_some((path, v))
            })
            .collect())
    }
}

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
    if !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Id estable del root desde su forma canónica (`scheme://authority` + base).
/// FNV-1a de 64 bits sobre `to_wire()`; estable entre procesos Y entre
/// endianness (bytes little-endian, no `to_ne_bytes` — así una `.db` copiada a
/// otra máquina conserva el id, encoding review). Solo es una clave de SCOPING
/// (`WHERE root_id = ?`), jamás autoridad: una colisión (64 bits, ínfima)
/// mezclaría a lo sumo dos roots, sin corromper bytes. Deuda (root table
/// interna) en ADR 0034 si el confused-deputy importa.
fn root_id(root: &VPath) -> i64 {
    let s = root.to_wire();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    i64::from_le_bytes(h.to_le_bytes())
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
    async fn build_cancel_persists_partial_superset_and_skips_sweep() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        // Primer build: a, b.
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"a.txt"), entry(&root, b"b.txt")],
            &tok,
        )
        .await
        .unwrap();
        // Segundo build que INSERTA c y luego se cancela ANTES de d: el token se
        // dispara al tirar del 2º item (i==1), así c ya se procesó pero el sweep
        // se salta. Resultado = SUPERSET (a, b viejos + c nuevo), removed=0. Esto
        // prueba el camino "coherente superset", no solo el skip-sweep (rust MINOR).
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
        assert_eq!(r.removed, 0, "cancelado NO barre (b/d no se podan)");
        let names: Vec<String> = sqlx::query_scalar("SELECT path FROM files ORDER BY path")
            .fetch_all(&idx.pool)
            .await
            .unwrap();
        // a, b (viejos) + c (parcial nuevo); d nunca se insertó.
        assert_eq!(names.len(), 3, "superset a+b+c, fue {names:?}");
        assert!(
            names.iter().any(|p| p.ends_with("c.txt")),
            "c parcial persistió"
        );
        assert!(
            names.iter().all(|p| !p.ends_with("d.txt")),
            "d no se insertó"
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
        // Re-embed del mismo fichero: el upsert reemplaza vector y hash.
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
        // Modelo distinto ⇒ el embedding viejo cuenta como AUSENTE.
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
        // Sin filtro de root: aparece el del modelo viejo.
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
        // Rebuild sin el fichero: el sweep borra la fila de `files` y el
        // ON DELETE CASCADE arrastra su embedding (pin de foreign_keys=ON).
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
        // Corrompe el BLOB a mano (length != dim * 4): la fila debe leerse como
        // AUSENTE en embedding_hashes — si devolviera el hash, el scheduler la
        // creería al día y jamás se repararía (review MAJOR-1).
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
        // Rebuild con el fichero AÚN presente: el upsert de `build` debe
        // conservar el rowid (ON CONFLICT DO UPDATE, jamás INSERT OR REPLACE)
        // o el CASCADE barrería TODOS los embeddings en cada rebuild.
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        assert_eq!(
            idx.embeddings_for_root(Some(&root), "m")
                .await
                .unwrap()
                .len(),
            1,
            "el rebuild preserva el embedding (rowid estable)"
        );
    }

    #[tokio::test]
    async fn embedding_hostile_path_roundtrips_byte_exact() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let hostile = b"informe-a\xff\xfe.txt"; // no-UTF8
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
            "el nombre no-UTF8 vuelve BYTE-EXACTO por la ruta de embeddings"
        );
    }

    #[tokio::test]
    async fn upsert_embedding_nonexistent_file_id_errors() {
        let idx = Index::open_memory().await.unwrap();
        // FK: un file_id que no existe en `files` se RECHAZA, no se inserta.
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

    /// **Un vector guardado se puede OLVIDAR** (#122): sin esto, la única
    /// forma de honrar una denegación nueva era borrar `index.db` entero.
    #[tokio::test]
    async fn un_embedding_se_puede_olvidar() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(
            &root,
            vec![entry(&root, b"publico.txt"), entry(&root, b"secreto.txt")],
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

        let secreto = idx
            .embedded_files(&root)
            .await
            .unwrap()
            .into_iter()
            .find(|(_, p)| p.display_lossy().ends_with("secreto.txt"))
            .expect("está");
        assert_eq!(idx.forget_embeddings(&[secreto.0]).await.unwrap(), 1);

        let quedan = idx.embedded_files(&root).await.unwrap();
        assert_eq!(quedan.len(), 1, "solo se fue el denegado");
        assert!(quedan[0].1.display_lossy().ends_with("publico.txt"));
        // Y desaparece de la BÚSQUEDA, que es lo que de verdad importa: un
        // vector que sigue puntuando es el texto del fichero contestando.
        assert_eq!(
            idx.embeddings_for_root(Some(&root), "m")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Olvidar una lista vacía no borra nada. Es el caso de cada task de embed
    /// sin nada denegado, o sea el común.
    #[tokio::test]
    async fn olvidar_nada_no_borra_nada() {
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

    /// **`embedded_files` no filtra por modelo, y es deliberado**: un vector de
    /// un modelo viejo sigue siendo un dato del usuario. Filtrar dejaría atrás
    /// justo las filas rancias que nadie vuelve a mirar y que nada recoge.
    #[tokio::test]
    async fn un_vector_de_otro_modelo_tambien_se_ve_para_purgar() {
        let idx = Index::open_memory().await.unwrap();
        let root = root();
        let tok = CancellationToken::new();
        idx.build(&root, vec![entry(&root, b"a.txt")], &tok)
            .await
            .unwrap();
        let c = idx.files_for_embed(&root).await.unwrap();
        idx.upsert_embedding(c[0].file_id, "modelo-viejo", &[1.0], b"h")
            .await
            .unwrap();

        assert!(
            idx.embeddings_for_root(Some(&root), "modelo-nuevo")
                .await
                .unwrap()
                .is_empty(),
            "la búsqueda con el modelo nuevo ya no lo ve…"
        );
        assert_eq!(
            idx.embedded_files(&root).await.unwrap().len(),
            1,
            "…pero la purga sí, que es el punto"
        );
    }
}
