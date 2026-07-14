//! Journal transaccional (M3-1, ADR 0020): toda mutación → una entrada con
//! actor, referencia de reversa y hash-chain sobre `SQLite` (WAL).
//!
//! **Alcance de la integridad (importante).** El hash-chain (SHA-256 SIN clave,
//! genesis fijo) detecta corrupción y ediciones INGENUAS —las que no recomputan
//! la cadena—. NO es tamper-evidence frente a un atacante con acceso de
//! escritura a la DB: reescritura total, truncación de COLA y rollback pasan
//! [`Journal::verify_chain`]. La evidencia criptográfica real (firma/anclaje del
//! head) es audit **M3-5** (issue #63). Hasta entonces, «detección de corrupción
//! y ediciones ingenuas», no tamper-evidence.

use std::str::FromStr;

use norte_proto::Error as ProtoError;
use sha2::{Digest, Sha256};
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
    prev_hash    BLOB    NOT NULL,
    entry_hash   BLOB    NOT NULL
);";

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

fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Campo opcional con BYTE DE PRESENCIA (0/1) → `None` y `Some(vacío)` NUNCA
/// colisionan (sin él, ambos serían `len=0` — hallazgo security B1).
fn feed_opt(h: &mut Sha256, o: Option<&[u8]>) {
    match o {
        None => h.update([0u8]),
        Some(b) => {
            h.update([1u8]);
            feed(h, b);
        }
    }
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
    h.finalize().into()
}

/// Estado de la cadena. `seq` y `last_hash` se avanzan JUNTOS bajo el `Mutex`
/// de `record`, así que el orden de `seq` == orden de encadenado por
/// construcción (evita el falso «manipulado» bajo concurrencia — security M1).
struct ChainState {
    last_seq: i64,
    last_hash: [u8; 32],
}

/// El journal transaccional sobre `SQLite` (WAL).
pub struct Journal {
    pub(crate) pool: SqlitePool,
    chain: Mutex<ChainState>,
}

impl Journal {
    /// Abre (o crea) el journal en `path` con WAL + `synchronous=NORMAL`. El
    /// fichero se restringe a modo `0600` en unix (metadatos de auditoría —
    /// security M2).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al abrir/crear; [`JournalError::Corrupt`] si el
    /// último `entry_hash` no mide 32 bytes.
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let this = Self::from_options(opts).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: el fichero ya existe tras conectar. Los sidecars
            // -wal/-shm los crea SQLite con perms derivados del principal.
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

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, JournalError> {
        // Pool de 1 conexión: un solo escritor (in-memory exige max=1 para no
        // perder la DB entre conexiones).
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
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
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState {
                last_seq,
                last_hash,
            }),
        })
    }

    /// Registra una mutación. El `seq` se asigna monótono DENTRO del lock de la
    /// cadena (junto al encadenado) → orden de `seq` == orden de hash. Devuelve
    /// el `seq` asignado. Si el insert falla, ni `seq` ni `last_hash` avanzan
    /// (sin huecos ni cadena rota).
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

    /// Recorre la cadena recomputando cada hash; `false` si hay una rotura de
    /// encadenado o una edición ingenua. NO detecta reescritura completa,
    /// truncación de cola ni rollback (keyless — ver módulo, issue #63).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn verify_chain(&self) -> Result<bool, JournalError> {
        let rows = sqlx::query(
            "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, prev_hash, entry_hash \
             FROM journal ORDER BY seq ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut prev = [0u8; 32];
        for row in rows {
            let actor_kind: String = row.get(2);
            let actor_id: Option<String> = row.get(3);
            let op: String = row.get(4);
            let path: Vec<u8> = row.get(5);
            let path_to: Option<Vec<u8>> = row.get(6);
            let reversal: String = row.get(7);
            let reversal_ref: Option<Vec<u8>> = row.get(8);
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
    async fn corrupt_path_for_test(&self, seq: i64, path: &[u8]) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET path = ? WHERE seq = ?")
            .bind(path)
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
/// lock. El wiring en el engine y la `reversal_ref` de `Trashed` llegan en
/// M3-1b.
pub struct SqliteJournal {
    journal: Journal,
}

impl SqliteJournal {
    /// Envuelve un journal ya abierto.
    #[must_use]
    pub fn new(journal: Journal) -> Self {
        Self { journal }
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
        let (op, path, path_to, reversal): (&str, Vec<u8>, Option<Vec<u8>>, Reversal) =
            match mutation {
                Mutation::Created(p) => {
                    ("created", p.to_wire().into_bytes(), None, Reversal::Delete)
                }
                Mutation::Removed(p) => (
                    "removed",
                    p.to_wire().into_bytes(),
                    None,
                    Reversal::Irreversible,
                ),
                // reversal_ref (ruta de papelera) llega en M3-1b; aquí queda None.
                Mutation::Trashed(p) => (
                    "trashed",
                    p.to_wire().into_bytes(),
                    None,
                    Reversal::RestoreTrash,
                ),
                Mutation::Renamed { from, to } => (
                    "renamed",
                    to.to_wire().into_bytes(),
                    Some(from.to_wire().into_bytes()),
                    Reversal::RenameBack,
                ),
            };
        // El error se PROPAGA (regla 4): la op no se considera completa si su
        // entrada de journal no quedó durable. El detalle va por tracing.
        self.journal
            .record(op, &path, path_to.as_deref(), reversal, None, actor)
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
        assert!(j.verify_chain().await.expect("verify"), "cadena íntegra");
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
        assert!(!j.verify_chain().await.expect("verify"), "se detecta");
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
        assert!(j.verify_chain().await.expect("verify"));
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
            obs.journal.verify_chain().await.expect("verify"),
            "sin falso-manipulado bajo concurrencia (security M1)"
        );
    }
}
