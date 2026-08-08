//! Journal transaccional (M3-1, ADR 0023): toda mutación → una entrada con
//! actor, referencia de reversa y hash-chain sobre `SQLite` (WAL).
//!
//! **Alcance de la integridad (importante).** El hash-chain (SHA-256 SIN clave,
//! genesis fijo) detecta corrupción y ediciones INGENUAS —las que no recomputan
//! la cadena—. NO es tamper-evidence frente a un atacante con acceso de
//! escritura a la DB: reescritura total, truncación de COLA y rollback pasan
//! [`Journal::verify_chain`]. Las **anclas HMAC** (M3-5, ADR 0025, módulo
//! [`crate::audit`]) acotan esa ventana: fabricar historia exige ADEMÁS la
//! clave del keyring y re-anclar. Sigue SIN cubrir: atacante con acceso al
//! keyring, mutaciones entre el último ancla y el ataque, destrucción del
//! fichero de anclas (copia externa recomendada).

use std::str::FromStr;

use norte_proto::Error as ProtoError;
use sha2::{Digest, Sha256};

use crate::hashing::{feed, feed_opt};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
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
    undoes_seq   INTEGER,
    prev_hash    BLOB    NOT NULL,
    entry_hash   BLOB    NOT NULL
);";

/// Migración IDEMPOTENTE de la columna de lote (batch rename, §17). Va fuera de
/// `SCHEMA` a propósito: `CREATE TABLE IF NOT EXISTS` NO altera una tabla que ya
/// existe, así que una DB escrita antes de esta versión se quedaría sin columna.
/// Un `ALTER TABLE` sobre una DB ya migrada responde «duplicate column name» y
/// eso es un no-op, no un fallo.
const MIGRATE_BATCH_ID: &str = "ALTER TABLE journal ADD COLUMN batch_id INTEGER";

/// Errores del journal.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// Error de la capa sqlx (abrir/consultar/insertar).
    #[error("sqlite: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// El journal en disco está corrupto (p. ej. un `entry_hash` que no mide 32
    /// bytes): NO se panica, se falla en seguro.
    #[error("journal corrupto: {0}")]
    Corrupt(&'static str),
    /// Error de I/O al preparar la ubicación del journal (p. ej. crear el dir
    /// de config).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<JournalError> for ProtoError {
    fn from(_: JournalError) -> Self {
        // El detalle va por `tracing`; al wire/task se expone como interno.
        ProtoError::Internal { panic: false }
    }
}

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

/// Una entrada del journal materializada para LECTURA (undo M3-2, audit M3-5,
/// tests de integración). Los `path`/`path_to`/`reversal_ref` son BYTES crudos
/// de [`norte_proto::VPath::to_wire`] (regla 1): reconstruye con
/// `VPath::from_wire` al consumir, jamás asumas UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Secuencia monótona asignada al registrar.
    pub seq: i64,
    /// Milisegundos UTC del registro (reloj del daemon al journalizar).
    pub ts_ms: i64,
    /// Hash de la entrada en la cadena (32 bytes) — el audit lo cruza con
    /// las anclas (ADR 0025).
    pub entry_hash: Vec<u8>,
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
    /// Etiqueta de reversa persistida ([`Reversal::as_str`]).
    pub reversal: String,
    /// Referencia para revertir (p. ej. ruta de papelera de un `trashed`),
    /// bytes `to_wire`.
    pub reversal_ref: Option<Vec<u8>>,
    /// Si esta entrada COMPENSA un undo, el `seq` original que deshace; `None`
    /// si es una mutación normal.
    pub undoes_seq: Option<i64>,
    /// Lote al que pertenece la entrada (`fs.rename_batch`): las entradas que
    /// comparten `batch_id` son UNA unidad deshacible. `None` para una mutación
    /// suelta — y para toda entrada escrita antes de que existieran los lotes.
    pub batch_id: Option<i64>,
}

/// Materializa un `JournalEntry` desde una fila con el orden de columnas
/// `seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to,
/// reversal, reversal_ref, undoes_seq, batch_id` (compartido por `entries` y
/// `revertible_for`).
fn row_to_entry(row: &sqlx::sqlite::SqliteRow) -> JournalEntry {
    JournalEntry {
        seq: row.get(0),
        ts_ms: row.get(1),
        entry_hash: row.get(2),
        actor_kind: row.get(3),
        actor_id: row.get(4),
        op: row.get(5),
        path: row.get(6),
        path_to: row.get(7),
        reversal: row.get(8),
        reversal_ref: row.get(9),
        undoes_seq: row.get(10),
        batch_id: row.get(11),
    }
}

/// ¿Existe ya la columna `batch_id`? Se pregunta al catálogo en vez de asumir:
/// el handle de SOLO-LECTURA (audit) no puede migrar una DB antigua y aun así
/// tiene que leerla. Una tabla ausente responde CERO filas → `false`, y la
/// primera query real fallará con su error propio, sin enmascarar nada.
async fn has_batch_id_column(pool: &SqlitePool) -> Result<bool, JournalError> {
    let rows = sqlx::query("PRAGMA table_info(journal)")
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().any(|r| {
        let name: String = r.get(1);
        name == "batch_id"
    }))
}

/// Veredicto de [`Journal::verify_chain`] (B2 de #63): si la cadena se
/// rompió, DÓNDE — el audit lo cita en vez de un booleano mudo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainStatus {
    /// Cadena íntegra.
    Intact {
        /// Entradas verificadas.
        entries: u64,
    },
    /// Primera entrada cuyo encadenado o hash no casa.
    Broken {
        /// `seq` de la primera rotura.
        first_bad_seq: i64,
    },
}

impl ChainStatus {
    /// `true` si la cadena está íntegra.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        matches!(self, ChainStatus::Intact { .. })
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
    pub undoes_seq: Option<i64>,
    pub batch_id: Option<i64>,
}

/// `entry_hash = sha256(prev_hash ‖ campos con longitud prefijada y presencia)`.
pub(crate) fn chain_hash(prev: &[u8; 32], r: &Record<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev);
    feed(&mut h, &r.seq.to_le_bytes());
    feed(&mut h, &r.ts_ms.to_le_bytes());
    feed(&mut h, r.actor_kind.as_bytes());
    feed_opt(&mut h, r.actor_id.map(str::as_bytes));
    feed(&mut h, r.op.as_bytes());
    feed(&mut h, r.path);
    feed_opt(&mut h, r.path_to);
    feed(&mut h, r.reversal.as_bytes());
    feed_opt(&mut h, r.reversal_ref);
    match r.undoes_seq {
        None => h.update([0u8]),
        Some(s) => {
            h.update([1u8]);
            feed(&mut h, &s.to_le_bytes());
        }
    }
    // AL FINAL y SOLO si hay lote. `None` no alimenta NADA —ni siquiera el byte
    // de presencia que usan los demás `Option`— porque una entrada escrita antes
    // de que existiera `batch_id` tiene que hashear EXACTAMENTE igual que
    // entonces: si no, `verify_chain` gritaría «manipulado» sobre una DB que
    // solo se migró. `Some` sí alimenta presencia + id con longitud prefijada,
    // así que ni quitar un lote ni inventarlo sobrevive a la verificación.
    if let Some(b) = r.batch_id {
        h.update([1u8]);
        feed(&mut h, &b.to_le_bytes());
    }
    h.finalize().into()
}

/// Estado de la cadena. `seq` y `last_hash` se avanzan JUNTOS bajo el `Mutex`
/// de `record`, así que el orden de `seq` == orden de encadenado por
/// construcción (evita el falso «manipulado» bajo concurrencia — security M1).
struct ChainState {
    last_seq: i64,
    last_hash: [u8; 32],
    /// Último id de lote entregado. Vive AQUÍ, bajo el mismo lock que `seq`,
    /// para que dos tareas de batch concurrentes no puedan compartir id.
    batch_counter: i64,
}

/// El journal transaccional sobre `SQLite` (WAL).
pub struct Journal {
    pub(crate) pool: SqlitePool,
    chain: Mutex<ChainState>,
    /// ¿Tiene la tabla la columna `batch_id`? Siempre `true` tras un [`Journal::open`]
    /// (migra), puede ser `false` en [`Journal::open_read_only`] sobre una DB
    /// pre-migración, que no se puede alterar y aun así hay que poder auditar.
    has_batch_id: bool,
}

/// Todo lo que necesita UNA entrada del journal. Una struct en vez de ocho
/// argumentos posicionales: la llamada se lee, y añadir un campo más adelante no
/// vuelve a barajarlos. Los `path*` son BYTES de [`norte_proto::VPath::to_wire`]
/// (regla 1).
#[derive(Debug, Clone, Copy)]
pub struct NewEntry<'a> {
    /// Operación: `"created" | "removed" | "trashed" | "renamed"`.
    pub op: &'a str,
    /// Path afectado (bytes `to_wire`).
    pub path: &'a [u8],
    /// Destino de un `renamed` (bytes `to_wire`).
    pub path_to: Option<&'a [u8]>,
    /// Cómo revertir.
    pub reversal: Reversal,
    /// Referencia necesaria para revertir (p. ej. el destino en la papelera
    /// lógica), bytes `to_wire`.
    pub reversal_ref: Option<&'a [u8]>,
    /// Quién la causó.
    pub actor: &'a Actor,
    /// El `seq` que esta entrada COMPENSA, si es un undo.
    pub undoes_seq: Option<i64>,
    /// El lote al que pertenece, si formó parte de uno ([`Journal::alloc_batch`]).
    /// Las entradas que comparten lote son UNA unidad deshacible.
    pub batch_id: Option<i64>,
}

impl Journal {
    /// Abre (o crea) el journal en `path` con WAL + `synchronous=NORMAL` y
    /// **lock exclusivo del fichero** (`locking_mode=EXCLUSIVE`): el
    /// single-writer del hash-chain (spec §4) es un MECANISMO, no una
    /// convención — un segundo proceso sobre el mismo fichero (p. ej. dos
    /// daemons con sockets distintos y el mismo dir de config) falla al abrir
    /// en vez de forkear la cadena y colisionar `seq` (MAJOR-1 del
    /// security-reviewer M3-4). El fichero se crea `0600` ANTES de conectar
    /// (sin ventana con el umask) y su dir padre `0700`.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al abrir/crear — incluida `database is locked`
    /// si OTRO proceso ya lo tiene abierto; [`JournalError::Corrupt`] si el
    /// último `entry_hash` no mide 32 bytes; I/O al pre-crear fichero/dir.
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        // Pre-creación con permisos correctos DESDE el primer byte (MINOR-1):
        // SQLite crearía el fichero con el umask (típicamente 0644) y el
        // chmod posterior dejaba una ventana legible. Con el fichero ya
        // presente, `create_if_missing` es un no-op.
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent() {
                let mut builder = tokio::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(parent).await?;
            }
            let _ = tokio::fs::OpenOptions::new()
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
            // WAL + EXCLUSIVE es válido (single-process WAL): el lock del
            // fichero se toma con el primer write — el CREATE TABLE del
            // schema en `from_options` lo fuerza YA en el open.
            .locking_mode(sqlx::sqlite::SqliteLockingMode::Exclusive);
        let this = Self::from_options(opts).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Cinturón por si el fichero preexistía con otros permisos. Los
            // sidecars -wal/-shm heredan del principal.
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(this)
    }

    /// Journal efímero en memoria (tests).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] / [`JournalError::Corrupt`].
    pub async fn open_in_memory() -> Result<Self, JournalError> {
        Self::from_options(SqliteConnectOptions::from_str("sqlite::memory:")?).await
    }

    /// Abre el journal en SOLO-LECTURA para el audit (M3-5): sin crear, sin
    /// schema, sin `locking_mode=EXCLUSIVE`. OJO: el daemon abre la DB con
    /// lock EXCLUSIVO de `SQLite` — con el daemon corriendo, este open (o la
    /// primera query) falla con `database is locked`; el audit se corre con
    /// el daemon parado. `record` sobre este handle falla (readonly), por
    /// diseño.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al abrir/consultar (incluida `database is
    /// locked` con el daemon vivo, y fichero inexistente).
    pub async fn open_read_only(path: &std::path::Path) -> Result<Self, JournalError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .read_only(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        // Sin CREATE TABLE (readonly): si el fichero no es un journal, la
        // primera query fallará con su error real — no se enmascara.
        let has_batch_id = has_batch_id_column(&pool).await?;
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState {
                last_seq: 0,
                last_hash: [0u8; 32],
                batch_counter: 0,
            }),
            has_batch_id,
        })
    }

    /// La columna de lote, o el literal `NULL` cuando la DB es pre-migración y
    /// no se puede alterar (solo-lectura). Devuelve uno de DOS literales fijos:
    /// nada de esto viene de fuera.
    fn batch_col(&self) -> &'static str {
        if self.has_batch_id {
            "batch_id"
        } else {
            "NULL"
        }
    }

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, JournalError> {
        // Pool de 1 conexión: un solo escritor (in-memory exige max=1 para no
        // perder la DB entre conexiones).
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        // Migración idempotente: una DB ya migrada responde «duplicate column
        // name» y se deja en paz. Cualquier OTRO error se propaga (falla en
        // seguro: sin columna no se puede journalizar un lote, y escribir sin
        // ella sería perder el agrupamiento en silencio).
        if let Err(e) = sqlx::query(MIGRATE_BATCH_ID).execute(&pool).await
            && !e.to_string().contains("duplicate column name")
        {
            return Err(JournalError::Sqlx(e));
        }
        let (last_seq, last_hash) =
            sqlx::query("SELECT seq, entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&pool)
                .await?
                .map_or(Ok((0i64, [0u8; 32])), |row| {
                    let seq: i64 = row.get(0);
                    let v: Vec<u8> = row.get(1);
                    if v.len() != 32 {
                        return Err(JournalError::Corrupt("entry_hash no mide 32 bytes"));
                    }
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&v);
                    Ok((seq, h))
                })?;
        // El contador de lotes arranca del MÁXIMO ya escrito: reabrir jamás
        // reutiliza un id que alguna entrada lleva puesto.
        let batch_counter: i64 = sqlx::query("SELECT COALESCE(MAX(batch_id), 0) FROM journal")
            .fetch_one(&pool)
            .await?
            .get(0);
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState {
                last_seq,
                last_hash,
                batch_counter,
            }),
            has_batch_id: true,
        })
    }

    /// Registra una mutación NORMAL (no compensa ningún undo). El `seq` se
    /// asigna monótono DENTRO del lock de la cadena (junto al encadenado) →
    /// orden de `seq` == orden de hash. Devuelve el `seq` asignado. Si el insert
    /// falla, ni `seq` ni `last_hash` avanzan (sin huecos ni cadena rota).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al insertar.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        op: &str,
        path: &[u8],
        path_to: Option<&[u8]>,
        reversal: Reversal,
        reversal_ref: Option<&[u8]>,
        actor: &Actor,
    ) -> Result<i64, JournalError> {
        self.record_undoing(op, path, path_to, reversal, reversal_ref, actor, None)
            .await
    }

    /// Como [`Self::record`] pero fija `undoes_seq` = el `seq` que esta entrada
    /// COMPENSA (undo M3-2). `None` para mutaciones normales. El `undoes_seq`
    /// entra en el hash-chain (sigue tamper-evident).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al insertar.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_undoing(
        &self,
        op: &str,
        path: &[u8],
        path_to: Option<&[u8]>,
        reversal: Reversal,
        reversal_ref: Option<&[u8]>,
        actor: &Actor,
        undoes_seq: Option<i64>,
    ) -> Result<i64, JournalError> {
        self.record_entry(&NewEntry {
            op,
            path,
            path_to,
            reversal,
            reversal_ref,
            actor,
            undoes_seq,
            batch_id: None,
        })
        .await
    }

    /// Entrega un id de lote fresco. Monótono y libre de carreras: el contador
    /// vive en el estado de la cadena, BAJO EL MISMO LOCK que asigna `seq`, así
    /// que dos tareas de batch concurrentes del mismo daemon no pueden
    /// compartirlo (un `SELECT MAX(batch_id) + 1` sí las dejaría). Al reabrir,
    /// el contador arranca del máximo escrito, así que tampoco se reutiliza
    /// entre arranques.
    ///
    /// Un id entregado y nunca usado (la tarea murió antes del primer paso) se
    /// pierde sin más: los ids son etiquetas de agrupación, no un contador
    /// auditable.
    ///
    /// # Errors
    /// Hoy no falla nunca; el `Result` se mantiene para que persistir el
    /// contador más adelante no cambie la firma.
    pub async fn alloc_batch(&self) -> Result<i64, JournalError> {
        let mut chain = self.chain.lock().await;
        chain.batch_counter += 1;
        Ok(chain.batch_counter)
    }

    /// Registra UNA entrada. Es el cuerpo real: [`Self::record`] y
    /// [`Self::record_undoing`] son envoltorios sobre esta. El `seq` se asigna
    /// DENTRO del lock de la cadena (junto al encadenado) → orden de `seq` ==
    /// orden de hash. Si el insert falla, ni `seq` ni `last_hash` avanzan.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al insertar.
    pub async fn record_entry(&self, e: &NewEntry<'_>) -> Result<i64, JournalError> {
        let NewEntry {
            op,
            path,
            path_to,
            reversal,
            reversal_ref,
            actor,
            undoes_seq,
            batch_id,
        } = *e;
        let (actor_kind, actor_id) = actor.parts();
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

        let mut chain = self.chain.lock().await;
        let seq = chain.last_seq + 1;
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
            undoes_seq,
            batch_id,
        };
        let entry_hash = chain_hash(&prev, &rec);

        sqlx::query(
            "INSERT INTO journal (seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id, prev_hash, entry_hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(undoes_seq)
        .bind(batch_id)
        .bind(&prev[..])
        .bind(&entry_hash[..])
        .execute(&self.pool)
        .await?;

        // Solo tras el insert OK: sin huecos de seq ni cadena rota si falla.
        chain.last_seq = seq;
        chain.last_hash = entry_hash;
        Ok(seq)
    }

    /// Número de entradas.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn count(&self) -> Result<i64, JournalError> {
        let row = sqlx::query("SELECT COUNT(*) FROM journal")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get(0))
    }

    /// Recorre la cadena recomputando cada hash. `Broken` señala la PRIMERA
    /// entrada cuyo encadenado o hash no casa (B2 de #63: el audit la cita).
    /// Keyless: NO detecta reescritura completa, truncación de cola ni
    /// rollback — esa cobertura la dan las anclas HMAC (ADR 0025).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn verify_chain(&self) -> Result<ChainStatus, JournalError> {
        let rows = sqlx::query(&format!(
            "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash, {} \
             FROM journal ORDER BY seq ASC",
            self.batch_col(),
        ))
        .fetch_all(&self.pool)
        .await?;
        let mut prev = [0u8; 32];
        let mut verified: u64 = 0;
        for row in rows {
            let seq: i64 = row.get(0);
            let actor_kind: String = row.get(2);
            let actor_id: Option<String> = row.get(3);
            let op: String = row.get(4);
            let path: Vec<u8> = row.get(5);
            let path_to: Option<Vec<u8>> = row.get(6);
            let reversal: String = row.get(7);
            let reversal_ref: Option<Vec<u8>> = row.get(8);
            let undoes_seq: Option<i64> = row.get(9);
            let stored_prev: Vec<u8> = row.get(10);
            let stored_hash: Vec<u8> = row.get(11);
            let batch_id: Option<i64> = row.get(12);
            if stored_prev != prev {
                return Ok(ChainStatus::Broken { first_bad_seq: seq });
            }
            let rec = Record {
                seq,
                ts_ms: row.get(1),
                actor_kind: &actor_kind,
                actor_id: actor_id.as_deref(),
                op: &op,
                path: &path,
                path_to: path_to.as_deref(),
                reversal: &reversal,
                reversal_ref: reversal_ref.as_deref(),
                undoes_seq,
                batch_id,
            };
            let computed = chain_hash(&prev, &rec);
            if computed[..] != stored_hash[..] {
                return Ok(ChainStatus::Broken { first_bad_seq: seq });
            }
            prev = computed;
            verified += 1;
        }
        Ok(ChainStatus::Intact { entries: verified })
    }

    /// Head de la cadena: `(seq, entry_hash)` de la ÚLTIMA entrada (`None`
    /// con el journal vacío). Es lo que un ancla HMAC firma (ADR 0025).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] si el hash
    /// almacenado no mide 32 bytes.
    pub async fn head(&self) -> Result<Option<(i64, [u8; 32])>, JournalError> {
        let row = sqlx::query("SELECT seq, entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let seq: i64 = row.get(0);
        let blob: Vec<u8> = row.get(1);
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("entry_hash del head no mide 32 bytes"))?;
        Ok(Some((seq, hash)))
    }

    /// Hash de la entrada `seq` (`None` si no existe). El audit lo contrasta
    /// con cada ancla DESPUÉS de un [`Journal::verify_chain`] `Intact` (ADR
    /// 0025): con la cadena verificada, el hash almacenado ES el recomputado.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] si el blob no mide
    /// 32 bytes.
    pub async fn entry_hash_at(&self, seq: i64) -> Result<Option<[u8; 32]>, JournalError> {
        let row = sqlx::query("SELECT entry_hash FROM journal WHERE seq = ?")
            .bind(seq)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let blob: Vec<u8> = row.get(0);
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("entry_hash no mide 32 bytes"))?;
        Ok(Some(hash))
    }

    /// Vuelca todas las entradas en orden de `seq`. Materializa en memoria:
    /// pensado para journals de tamaño de sesión (la paginación es deuda si
    /// crece — mismo criterio que el listado, #27). Base de lectura para el
    /// undo (M3-2) y el audit export (M3-5).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let rows = sqlx::query(&format!(
            "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, {} \
             FROM journal ORDER BY seq ASC",
            self.batch_col(),
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_entry).collect())
    }

    /// Las entradas REVERTIBLES de la sesión `actor`, en orden LIFO (`seq`
    /// DESC): mutaciones normales (`undoes_seq IS NULL`) de ese actor que nadie
    /// ha compensado todavía. Base de [`crate::Engine::undo_session`] (M3-2).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn revertible_for(&self, actor: &Actor) -> Result<Vec<JournalEntry>, JournalError> {
        let (actor_kind, actor_id) = actor.parts();
        let rows = sqlx::query(&format!(
            "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, {} \
             FROM journal \
             WHERE undoes_seq IS NULL AND actor_kind = ? AND actor_id IS ? \
               AND seq NOT IN (SELECT undoes_seq FROM journal WHERE undoes_seq IS NOT NULL) \
             ORDER BY seq DESC",
            self.batch_col(),
        ))
        .bind(actor_kind)
        .bind(actor_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_entry).collect())
    }

    /// SOLO TESTS: corrompe el `path` de una entrada sin recomputar su hash.
    #[cfg(test)]
    async fn corrupt_path_for_test(&self, seq: i64, path: &[u8]) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET path = ? WHERE seq = ?")
            .bind(path)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// SOLO TESTS: cambia el `batch_id` de una entrada sin recomputar su hash
    /// (simula a un atacante DESAGRUPANDO un lote, o inventándole uno a una
    /// mutación suelta).
    #[cfg(test)]
    async fn set_batch_for_test(&self, seq: i64, batch: Option<i64>) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET batch_id = ? WHERE seq = ?")
            .bind(batch)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// SOLO TESTS: pone un `entry_hash` de longitud inválida (simula corrupción
    /// en disco) para probar el guard de arranque.
    #[cfg(test)]
    async fn set_short_hash_for_test(&self, seq: i64) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET entry_hash = ? WHERE seq = ?")
            .bind(&b"corto"[..])
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// El [`Journal`] como [`crate::observer::MutationObserver`]: mapea cada
/// `Mutation` a una entrada. El `seq` lo asigna el propio [`Journal`] bajo su
/// lock. Una papelerización lógica arrastra su destino recuperable a la
/// `reversal_ref` (M3-1b).
pub struct SqliteJournal {
    journal: Journal,
}

impl SqliteJournal {
    /// Envuelve un journal ya abierto.
    #[must_use]
    pub fn new(journal: Journal) -> Self {
        Self { journal }
    }

    /// El [`Journal`] subyacente (lectura para audit/undo/tests).
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Abre (o crea) el journal en `path` y lo envuelve como observer, listo
    /// para [`crate::Engine::with_observer`]. Crea el directorio contenedor si
    /// falta.
    ///
    /// UN SOLO ESCRITOR (spec §4): el hash-chain asume un único proceso dueño
    /// (el daemon). Varios procesos efímeros escribiendo el MISMO fichero
    /// forkean la cadena y colisionan en `seq` — el journal on-disk NO debe
    /// compartirse entre binarios embebidos concurrentes. El wiring del dueño
    /// único llega con el daemon agéntico (M3-4).
    ///
    /// # Errors
    /// [`JournalError::Io`] si no puede crear el directorio contenedor;
    /// [`JournalError`] al abrir/crear la DB (ver [`Journal::open`]).
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        // El dir de config puede no existir en el primer arranque; SQLite crea
        // el FICHERO (create_if_missing) pero no su directorio padre.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(Self::new(Journal::open(path).await?))
    }
}

#[async_trait::async_trait]
impl crate::observer::MutationObserver for SqliteJournal {
    async fn on_mutation(
        &self,
        mutation: &crate::observer::Mutation<'_>,
        actor: &Actor,
    ) -> Result<(), ProtoError> {
        use crate::observer::Mutation;
        let (op, path, path_to, reversal, reversal_ref, batch_id): (
            &str,
            Vec<u8>,
            Option<Vec<u8>>,
            Reversal,
            Option<Vec<u8>>,
            Option<i64>,
        ) = match mutation {
            Mutation::Created(p) => (
                "created",
                p.to_wire().into_bytes(),
                None,
                Reversal::Delete,
                None,
                None,
            ),
            Mutation::Removed(p) => (
                "removed",
                p.to_wire().into_bytes(),
                None,
                Reversal::Irreversible,
                None,
                None,
            ),
            // Papelera lógica → `dest` es la ruta recuperable (reversal_ref).
            // Papelera nativa/"vanish" → `dest` None (handle en el undo M3-2).
            Mutation::Trashed { path, dest } => (
                "trashed",
                path.to_wire().into_bytes(),
                None,
                Reversal::RestoreTrash,
                dest.map(|d| d.to_wire().into_bytes()),
                None,
            ),
            Mutation::Renamed { from, to, batch } => (
                "renamed",
                to.to_wire().into_bytes(),
                Some(from.to_wire().into_bytes()),
                Reversal::RenameBack,
                None,
                *batch,
            ),
        };
        // El error se PROPAGA (regla 4): la op no se considera completa si su
        // entrada de journal no quedó durable. El detalle va por tracing.
        self.journal
            .record_entry(&NewEntry {
                op,
                path: &path,
                path_to: path_to.as_deref(),
                reversal,
                reversal_ref: reversal_ref.as_deref(),
                actor,
                undoes_seq: None,
                batch_id,
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "fallo al escribir el journal");
                ProtoError::from(e)
            })?;
        Ok(())
    }
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
            undoes_seq: None,
            batch_id: None,
        }
    }

    #[test]
    fn chain_hash_is_deterministic_and_prev_sensitive() {
        let zero = [0u8; 32];
        let h1 = chain_hash(&zero, &rec(1));
        assert_eq!(h1, chain_hash(&zero, &rec(1)), "determinista");
        assert_ne!(h1, chain_hash(&h1, &rec(1)));
        assert_ne!(h1, chain_hash(&zero, &rec(2)));
    }

    /// VECTOR CONGELADO de la cadena, con TODOS los campos poblados: los dos
    /// `Option` presentes (uno de ellos vacío, para fijar el byte de
    /// presencia), `undoes_seq` presente, y rutas que NO son UTF-8.
    ///
    /// Los demás tests de `chain_hash` son relativos (`assert_ne!` entre dos
    /// digests) y seguirían verdes si el prefijo de longitud pasara de `u64` a
    /// `u32`, de little-endian a big-endian, o si el orden de los campos
    /// cambiara — y cualquiera de esas cosas invalida `verify_chain` en TODOS
    /// los journals que ya están en disco. Este es el único guardarraíl
    /// mecánico que tiene esa promesa.
    ///
    /// Si se pone rojo: NO actualices la constante. Revierte el cambio de
    /// framing, o versiona el formato de la cadena y migra los journals.
    #[test]
    fn the_chain_hash_is_frozen() {
        let r = Record {
            seq: 7,
            ts_ms: 1_726_000_000_000,
            actor_kind: "agent",
            actor_id: Some("sesion-1"),
            op: "renamed",
            path: b"file:///caf\xff",
            path_to: Some(b"file:///caf\xfe"),
            reversal: "rename",
            reversal_ref: Some(&[]),
            undoes_seq: Some(3),
            // SIN lote, como toda entrada anterior al batch rename: la
            // constante de abajo NO cambia por añadir el campo, y eso es
            // exactamente la promesa de compatibilidad.
            batch_id: None,
        };
        let got = chain_hash(&[0u8; 32], &r);
        assert_eq!(
            crate::hashing::hex_lower(&got),
            "b00a2da6db1199742aa42f4811370bf02fcc21a294d77741ae7a26ad2b794ecc",
        );
    }

    #[test]
    fn length_prefix_prevents_concatenation_collision() {
        let zero = [0u8; 32];
        let mut a = rec(1);
        a.actor_id = Some("ab");
        a.op = "c";
        let mut b = rec(1);
        b.actor_id = Some("a");
        b.op = "bc";
        assert_ne!(chain_hash(&zero, &a), chain_hash(&zero, &b));
    }

    #[test]
    fn none_and_empty_some_do_not_collide() {
        // security B1: `None` vs `Some(&[])` deben dar hashes distintos.
        let zero = [0u8; 32];
        let mut none = rec(1);
        none.reversal_ref = None;
        let mut empty = rec(1);
        empty.reversal_ref = Some(&[]);
        assert_ne!(chain_hash(&zero, &none), chain_hash(&zero, &empty));
    }

    #[tokio::test]
    async fn open_insert_and_verify_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        assert_eq!(
            j.record(
                "created",
                b"file:///a",
                None,
                Reversal::Delete,
                None,
                &Actor::User
            )
            .await
            .expect("insert 1"),
            1
        );
        assert_eq!(
            j.record(
                "trashed",
                "file:///\u{00e9}".as_bytes(),
                None,
                Reversal::RestoreTrash,
                Some(b"file:///.norte-trash/1-0"),
                &Actor::Agent {
                    session: "s1".into()
                },
            )
            .await
            .expect("insert 2"),
            2
        );
        assert_eq!(j.count().await.expect("count"), 2);
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "cadena íntegra"
        );
    }

    #[tokio::test]
    async fn tampering_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "created",
            b"file:///a",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("insert");
        j.corrupt_path_for_test(1, b"file:///HACKED")
            .await
            .expect("corrupt");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 1 },
            "se detecta Y se cita dónde (B2)"
        );
    }

    #[tokio::test]
    async fn corrupt_hash_length_fails_on_reopen_not_panics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        {
            let j = Journal::open(&path).await.expect("open");
            j.record(
                "created",
                b"file:///a",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("insert");
            j.set_short_hash_for_test(1).await.expect("corrupt");
        }
        // Reabrir lee el último entry_hash → corto → Corrupt, no panic.
        assert!(matches!(
            Journal::open(&path).await,
            Err(JournalError::Corrupt(_))
        ));
    }

    /// El single-writer del hash-chain es un MECANISMO (MAJOR-1 security
    /// M3-4): mientras un proceso tenga el journal abierto, un segundo `open`
    /// del MISMO fichero falla — jamás dos escritores forkeando la cadena
    /// (p. ej. dos daemons con sockets distintos y el mismo config dir).
    #[tokio::test]
    async fn second_open_of_live_journal_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        let vivo = Journal::open(&path).await.expect("primer open");
        assert!(
            matches!(Journal::open(&path).await, Err(JournalError::Sqlx(_))),
            "el lock exclusivo rechaza al segundo escritor"
        );
        // Soltar el primero libera el lock: reabrir vuelve a funcionar.
        drop(vivo);
        let _ = Journal::open(&path).await.expect("reopen tras drop");
    }

    #[tokio::test]
    async fn seq_resumes_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        {
            let j = Journal::open(&path).await.expect("open");
            for _ in 0..2 {
                j.record(
                    "created",
                    b"file:///a",
                    None,
                    Reversal::Delete,
                    None,
                    &Actor::User,
                )
                .await
                .expect("insert");
            }
        }
        let j = Journal::open(&path).await.expect("reopen");
        let seq = j
            .record(
                "created",
                b"file:///b",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("insert");
        assert_eq!(seq, 3, "el seq continúa tras reabrir");
        assert_eq!(j.count().await.expect("count"), 3);
        assert!(j.verify_chain().await.expect("verify").is_intact());
    }

    #[tokio::test]
    async fn concurrent_on_mutation_keeps_chain_consistent() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;
        use std::sync::Arc;

        let obs = Arc::new(SqliteJournal::new(
            Journal::open_in_memory().await.expect("open"),
        ));
        let p = VPath::parse("file:///x").expect("vpath");
        let mut handles = Vec::new();
        for _ in 0..32 {
            let obs = Arc::clone(&obs);
            let p = p.clone();
            handles.push(tokio::spawn(async move {
                obs.on_mutation(&Mutation::Created(&p), &Actor::User).await
            }));
        }
        for h in handles {
            h.await.expect("join").expect("on_mutation ok");
        }
        // seq asignado bajo el lock → cadena consistente pese a 32 concurrentes.
        assert_eq!(obs.journal.count().await.expect("count"), 32);
        assert!(
            obs.journal
                .verify_chain()
                .await
                .expect("verify")
                .is_intact(),
            "sin falso-manipulado bajo concurrencia (security M1)"
        );
    }

    #[tokio::test]
    async fn sqlite_journal_open_creates_missing_parent_dir() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let dir = tempfile::tempdir().expect("tempdir");
        // El dir contenedor NO existe todavía (primer arranque del dueño).
        let path = dir.path().join("state/journal.db");
        let j = SqliteJournal::open(&path)
            .await
            .expect("open crea el padre");
        let victim = VPath::parse("file:///a").expect("vpath");
        j.on_mutation(&Mutation::Created(&victim), &Actor::User)
            .await
            .expect("on_mutation");
        assert_eq!(j.journal().count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn trashed_with_dest_records_reversal_ref_byte_exact() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::{Scheme, Segment, VPath};

        let obs = SqliteJournal::new(Journal::open_in_memory().await.expect("open"));
        let seg = |b: &[u8]| Segment::new(b.to_vec()).expect("segment");
        let root = VPath::root(Scheme::new("file").expect("scheme"), None);
        // Basename NO-UTF8 (0xFF 0xFE): ejercita la rama percent-encoding del
        // wire, justo donde un bug lossy (regla 1) se escondería.
        let victim = root.join(seg(&[0xFF, 0xFE]));
        let dest = root
            .join(seg(b".norte-trash"))
            .join(seg(b"17-3"))
            .join(seg(&[0xFF, 0xFE]));

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
        let stored = es[0]
            .reversal_ref
            .as_deref()
            .expect("papelera lógica: hay reversal_ref");
        // Round-trip REAL (no tautológico): `VPath::parse` es la inversa de
        // `to_wire`; si el wire perdiera los bytes hostiles, reconstruiría un
        // VPath distinto y este assert fallaría.
        let roundtrip =
            VPath::parse(std::str::from_utf8(stored).expect("wire es ASCII")).expect("parse");
        assert_eq!(
            roundtrip, dest,
            "reversal_ref round-trip byte-exacto (regla 1)"
        );
    }

    #[tokio::test]
    async fn trashed_without_dest_has_no_reversal_ref() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let obs = SqliteJournal::new(Journal::open_in_memory().await.expect("open"));
        let victim = VPath::parse("file:///v").expect("vpath");
        obs.on_mutation(
            &Mutation::Trashed {
                path: &victim,
                dest: None,
            },
            &Actor::User,
        )
        .await
        .expect("on_mutation");
        let es = obs.journal.entries().await.expect("entries");
        assert_eq!(es[0].reversal, "restore_trash");
        assert_eq!(
            es[0].reversal_ref, None,
            "papelera nativa: sin ruta estable"
        );
    }

    #[tokio::test]
    async fn entries_returns_fields_in_seq_order() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "created",
            b"file:///a",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
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

    #[tokio::test]
    async fn revertible_for_excludes_compensated_and_foreign_actor() {
        let j = Journal::open_in_memory().await.expect("open");
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        // seq 1: agente crea A. seq 2: usuario crea B (otro actor).
        j.record(
            "created",
            b"file:///a",
            None,
            Reversal::Delete,
            None,
            &agent,
        )
        .await
        .expect("1");
        j.record(
            "created",
            b"file:///b",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("2");
        // seq 3: compensación de la 1 (undoes_seq=1) → la 1 deja de ser revertible.
        j.record_undoing(
            "removed",
            b"file:///a",
            None,
            Reversal::Irreversible,
            None,
            &agent,
            Some(1),
        )
        .await
        .expect("3");

        let rev = j.revertible_for(&agent).await.expect("revertible");
        assert!(
            rev.is_empty(),
            "la 1 ya está compensada; la 2 es de otro actor"
        );

        let rev_user = j
            .revertible_for(&Actor::User)
            .await
            .expect("revertible user");
        assert_eq!(rev_user.len(), 1);
        assert_eq!(rev_user[0].seq, 2);
        assert_eq!(rev_user[0].undoes_seq, None);
    }

    #[tokio::test]
    async fn revertible_for_is_lifo_and_hash_survives_undoes_seq() {
        let j = Journal::open_in_memory().await.expect("open");
        for w in [&b"file:///a"[..], b"file:///b", b"file:///c"] {
            j.record("created", w, None, Reversal::Delete, None, &Actor::User)
                .await
                .expect("rec");
        }
        let rev = j.revertible_for(&Actor::User).await.expect("rev");
        assert_eq!(
            rev.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 2, 1],
            "orden LIFO (DESC)"
        );
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "chain íntegra con undoes_seq"
        );
    }

    /// M3-5 (ADR 0025): la truncación de COLA pasa `verify_chain` (debilidad
    /// keyless PINNEADA aquí a propósito) — y el ancla HMAC la detecta.
    #[tokio::test]
    async fn ancla_detecta_truncacion_de_cola_que_la_cadena_no_ve() {
        let j = Journal::open_in_memory().await.expect("open");
        for w in [&b"file:///a"[..], b"file:///b", b"file:///c"] {
            j.record("created", w, None, Reversal::Delete, None, &Actor::User)
                .await
                .expect("rec");
        }
        let (seq, head) = j.head().await.expect("head").expect("no vacio");
        assert_eq!(seq, 3);
        assert_eq!(
            j.entry_hash_at(seq).await.expect("hash_at"),
            Some(head),
            "head() y entry_hash_at coinciden"
        );
        let key = [7u8; 32];
        let line = crate::audit::anchor_line(&key, &crate::audit::Anchor { seq, head });

        // ATAQUE: el atacante borra la ultima entrada (rollback de cola).
        sqlx::query("DELETE FROM journal WHERE seq = 3")
            .execute(&j.pool)
            .await
            .expect("delete");
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "keyless NO ve la truncacion de cola (por eso existen las anclas)"
        );
        // El ancla si: el seq anclado ya no existe.
        let at = j.entry_hash_at(seq).await.expect("hash_at");
        assert_eq!(
            crate::audit::verify_anchor_line(&key, &line, at),
            crate::audit::AnchorVerdict::MissingSeq(crate::audit::Anchor { seq, head })
        );
    }

    /// El schema del journal ANTES de que existiera `batch_id`, copiado tal
    /// cual se envió. Los tests de compatibilidad crean la DB con ESTE texto:
    /// si el `SCHEMA` de arriba cambia, ellos siguen describiendo el disco que
    /// ya existe, que es de lo que va la migración.
    const SCHEMA_BEFORE_BATCH_ID: &str = "\
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
    undoes_seq   INTEGER,
    prev_hash    BLOB    NOT NULL,
    entry_hash   BLOB    NOT NULL
);";

    fn unhex(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        assert!(b.len().is_multiple_of(2), "hex de longitud par");
        b.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).expect("ascii"), 16).expect("hex"))
            .collect()
    }

    /// Escribe en `path` una DB con el schema PRE-migración y UNA fila cuyo
    /// `entry_hash` es la constante congelada de `the_chain_hash_is_frozen` —
    /// es decir, un hash calculado por el código ANTERIOR a esta tarea, que
    /// nada de este test recomputa.
    async fn write_pre_migration_journal(path: &std::path::Path) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    // WAL como lo dejaba `Journal::open` de entonces: un
                    // journal pre-migración REAL está en WAL, y el handle de
                    // solo-lectura no podría cambiar el modo (eso es escribir).
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .expect("pool viejo");
        sqlx::query(SCHEMA_BEFORE_BATCH_ID)
            .execute(&pool)
            .await
            .expect("schema viejo");
        sqlx::query(
            "INSERT INTO journal (seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(7i64)
        .bind(1_726_000_000_000i64)
        .bind("agent")
        .bind(Some("sesion-1"))
        .bind("renamed")
        .bind(&b"file:///caf\xff"[..])
        .bind(Some(&b"file:///caf\xfe"[..]))
        .bind("rename")
        .bind(Some(&[][..]))
        .bind(Some(3i64))
        .bind(&[0u8; 32][..])
        // El MISMO hex que pinea `the_chain_hash_is_frozen`, duplicado a
        // propósito: este test no debe poder «arreglarse» tocando aquella
        // constante.
        .bind(unhex("b00a2da6db1199742aa42f4811370bf02fcc21a294d77741ae7a26ad2b794ecc"))
        .execute(&pool)
        .await
        .expect("insert de la era pre-batch");
        pool.close().await;
    }

    /// EL test de la regla del hash: un journal escrito ANTES de que existiera
    /// `batch_id` sigue verificando después de migrarlo. Su fila lleva un
    /// `entry_hash` de la era anterior; si `None` alimentara algo (aunque fuera
    /// un byte de presencia), `verify_chain` gritaría «manipulado» sobre una
    /// base de datos que nadie tocó.
    #[tokio::test]
    async fn a_pre_migration_journal_still_verifies_and_keeps_chaining() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("viejo.db");
        write_pre_migration_journal(&path).await;

        let j = Journal::open(&path).await.expect("open migra la DB");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Intact { entries: 1 },
            "la migración no puede romper una cadena ya escrita"
        );
        let es = j.entries().await.expect("entries");
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].batch_id, None, "la fila migrada no tiene lote");

        // Y la cadena SIGUE desde ahí: la entrada nueva encadena con el head
        // heredado y la cadena entera vuelve a verificar.
        let seq = j
            .record(
                "created",
                b"file:///nuevo",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record tras migrar");
        assert_eq!(seq, 8, "el seq continúa desde la fila heredada");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        assert_eq!(
            j.alloc_batch().await.expect("alloc"),
            1,
            "sin lotes previos, el contador arranca en 1"
        );
    }

    /// `open` es la vía de migración; `open_read_only` (audit, M3-5) NO puede
    /// hacer `ALTER TABLE`, así que tiene que LEER una DB pre-migración sin
    /// reventar con «no such column».
    #[tokio::test]
    async fn a_pre_migration_journal_is_readable_read_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("viejo-ro.db");
        write_pre_migration_journal(&path).await;

        let ro = Journal::open_read_only(&path).await.expect("open ro");
        let es = ro.entries().await.expect("entries");
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].batch_id, None);
        assert!(
            ro.verify_chain().await.expect("verify").is_intact(),
            "la cadena vieja verifica igual en solo-lectura"
        );
        assert_eq!(
            ro.revertible_for(&Actor::User)
                .await
                .expect("revertible")
                .len(),
            0,
            "la fila es de un agente, no del usuario"
        );
    }

    /// VECTOR CONGELADO del campo NUEVO: el mismo registro que
    /// `the_chain_hash_is_frozen` pero CON lote. Pinea la otra mitad de la
    /// regla (byte de presencia + id con longitud prefijada, al final del
    /// todo). Si se pone rojo, has cambiado el framing del `batch_id` y has
    /// invalidado la cadena de los journals que ya lo usan.
    #[test]
    fn the_batch_id_framing_is_frozen() {
        let r = Record {
            seq: 7,
            ts_ms: 1_726_000_000_000,
            actor_kind: "agent",
            actor_id: Some("sesion-1"),
            op: "renamed",
            path: b"file:///caf\xff",
            path_to: Some(b"file:///caf\xfe"),
            reversal: "rename",
            reversal_ref: Some(&[]),
            undoes_seq: Some(3),
            batch_id: Some(42),
        };
        assert_eq!(
            crate::hashing::hex_lower(&chain_hash(&[0u8; 32], &r)),
            "c0bf7ed670bbbb496d5b47defe0181f8a2b3ead1e6ce690d12aaa13a1ae8055b",
        );
    }

    /// Un id de lote es monótono y no se reutiliza, ni entre dos asignaciones
    /// sin insert de por medio ni al reabrir el journal.
    #[tokio::test]
    async fn batch_ids_are_monotonic_across_allocs_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        let usados;
        {
            let j = Journal::open(&path).await.expect("open");
            let a = j.alloc_batch().await.expect("alloc");
            let b = j.alloc_batch().await.expect("alloc");
            assert_eq!(b, a + 1, "monótono sin insert de por medio");
            j.record_entry(&NewEntry {
                op: "renamed",
                path: b"file:///b",
                path_to: Some(b"file:///a"),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &Actor::User,
                undoes_seq: None,
                batch_id: Some(b),
            })
            .await
            .expect("record");
            usados = b;
        }
        let j = Journal::open(&path).await.expect("reopen");
        assert!(
            j.alloc_batch().await.expect("alloc") > usados,
            "tras reabrir, jamás se repite un lote ya escrito"
        );
    }

    /// Dos tareas concurrentes en el mismo daemon JAMÁS comparten lote (un
    /// `MAX(batch_id) + 1` sí podría).
    #[tokio::test]
    async fn concurrent_alloc_batch_never_repeats_an_id() {
        use std::collections::HashSet;
        use std::sync::Arc;

        let j = Arc::new(Journal::open_in_memory().await.expect("open"));
        let mut handles = Vec::new();
        for _ in 0..32 {
            let j = Arc::clone(&j);
            handles.push(tokio::spawn(async move { j.alloc_batch().await }));
        }
        let mut vistos = HashSet::new();
        for h in handles {
            let id = h.await.expect("join").expect("alloc");
            assert!(vistos.insert(id), "id de lote repetido: {id}");
        }
        assert_eq!(vistos.len(), 32);
    }

    /// El lote es parte de la cadena: quitárselo a una fila rompe
    /// `verify_chain` justo ahí (un atacante no puede DESAGRUPAR un batch).
    #[tokio::test]
    async fn stripping_a_batch_id_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        let batch = j.alloc_batch().await.expect("alloc");
        let seq = j
            .record_entry(&NewEntry {
                op: "renamed",
                path: b"file:///b",
                path_to: Some(b"file:///a"),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &Actor::User,
                undoes_seq: None,
                batch_id: Some(batch),
            })
            .await
            .expect("record");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        j.set_batch_for_test(seq, None).await.expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: seq },
        );
    }

    /// Y en la otra dirección: INVENTARLE un lote a una entrada suelta también
    /// rompe la cadena. La compatibilidad con lo viejo no es un agujero.
    #[tokio::test]
    async fn inventing_a_batch_id_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        let seq = j
            .record(
                "created",
                b"file:///a",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        j.set_batch_for_test(seq, Some(1)).await.expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: seq },
        );
    }

    /// El lector expone el lote, que es lo que permite al undo consumir el
    /// grupo entero como UNA unidad.
    #[tokio::test]
    async fn entries_and_revertible_report_their_batch() {
        let j = Journal::open_in_memory().await.expect("open");
        let batch = j.alloc_batch().await.expect("alloc");
        j.record_entry(&NewEntry {
            op: "renamed",
            path: b"file:///b",
            path_to: Some(b"file:///a"),
            reversal: Reversal::RenameBack,
            reversal_ref: None,
            actor: &Actor::User,
            undoes_seq: None,
            batch_id: Some(batch),
        })
        .await
        .expect("record");
        let es = j.entries().await.expect("entries");
        assert_eq!(es[0].batch_id, Some(batch));
        let rev = j.revertible_for(&Actor::User).await.expect("revertible");
        assert_eq!(rev[0].batch_id, Some(batch));
    }

    /// Un rename suelto (el camino de `fs.move`) sigue sin lote; uno de un
    /// batch lo arrastra hasta la fila.
    #[tokio::test]
    async fn on_mutation_threads_the_batch_of_a_rename() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let obs = SqliteJournal::new(Journal::open_in_memory().await.expect("open"));
        let from = VPath::parse("file:///a").expect("vpath");
        let to = VPath::parse("file:///b").expect("vpath");
        obs.on_mutation(
            &Mutation::Renamed {
                from: &from,
                to: &to,
                batch: None,
            },
            &Actor::User,
        )
        .await
        .expect("suelto");
        let batch = obs.journal().alloc_batch().await.expect("alloc");
        obs.on_mutation(
            &Mutation::Renamed {
                from: &from,
                to: &to,
                batch: Some(batch),
            },
            &Actor::User,
        )
        .await
        .expect("en lote");
        let es = obs.journal().entries().await.expect("entries");
        assert_eq!(es[0].batch_id, None, "un rename suelto no inventa lote");
        assert_eq!(es[1].batch_id, Some(batch));
        assert!(
            obs.journal()
                .verify_chain()
                .await
                .expect("verify")
                .is_intact()
        );
    }

    /// `open_read_only` (M3-5): lee lo mismo que el handle de escritura y
    /// RECHAZA `record` (readonly por diseño). File-backed: cubre el camino
    /// WAL real del audit.
    #[tokio::test]
    async fn open_read_only_lee_y_rechaza_escrituras() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        let j = Journal::open(&path).await.expect("open rw");
        j.record(
            "created",
            b"file:///a",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("rec");
        let head_rw = j.head().await.expect("head").expect("no vacio");
        drop(j);

        let ro = Journal::open_read_only(&path).await.expect("open ro");
        assert_eq!(ro.entries().await.expect("entries").len(), 1);
        assert_eq!(ro.head().await.expect("head"), Some(head_rw));
        assert!(
            ro.verify_chain().await.expect("verify").is_intact(),
            "la cadena verifica igual en solo-lectura"
        );
        assert!(
            ro.record(
                "created",
                b"file:///b",
                None,
                Reversal::Delete,
                None,
                &Actor::User
            )
            .await
            .is_err(),
            "record sobre readonly DEBE fallar"
        );
    }
}
