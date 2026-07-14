//! Journal transaccional (M3-1, ADR 0020): toda mutación → una entrada con
//! actor, referencia de reversa y hash-chain tamper-evident sobre SQLite (WAL).

use std::str::FromStr;

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

/// Estado de la cadena (serializa inserts para hash-chain consistente).
struct ChainState {
    last_hash: [u8; 32],
}

/// El journal transaccional sobre SQLite (WAL).
pub struct Journal {
    pub(crate) pool: SqlitePool,
    chain: Mutex<ChainState>,
}

impl Journal {
    /// Abre (o crea) el journal en `path` con WAL + `synchronous=NORMAL`.
    ///
    /// # Errors
    /// Errores de sqlx al abrir/crear el schema.
    pub async fn open(path: &std::path::Path) -> Result<Self, sqlx::Error> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        Self::from_options(opts).await
    }

    /// Journal efímero en memoria (tests).
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn open_in_memory() -> Result<Self, sqlx::Error> {
        Self::from_options(SqliteConnectOptions::from_str("sqlite::memory:")?).await
    }

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, sqlx::Error> {
        // Pool de 1 conexión: un solo escritor (in-memory exige max=1 para no
        // perder la DB entre conexiones).
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        let last_hash = sqlx::query("SELECT entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&pool)
            .await?
            .map(|row| {
                let v: Vec<u8> = row.get(0);
                let mut h = [0u8; 32];
                h.copy_from_slice(&v);
                h
            })
            .unwrap_or([0u8; 32]);
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState { last_hash }),
        })
    }

    /// Registra una mutación. `seq` lo asigna el llamante (monótono); el hash
    /// encadena con la última entrada. Serializado por el `Mutex` de la cadena.
    ///
    /// # Errors
    /// Errores de sqlx al insertar.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        op: &str,
        path: &[u8],
        path_to: Option<&[u8]>,
        reversal: Reversal,
        reversal_ref: Option<&[u8]>,
        actor: &Actor,
        seq: i64,
    ) -> Result<(), sqlx::Error> {
        let (actor_kind, actor_id) = actor.parts();
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));

        let mut chain = self.chain.lock().await;
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

        chain.last_hash = entry_hash;
        Ok(())
    }

    /// Número de entradas.
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn count(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) FROM journal")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get(0))
    }

    /// Recorre la cadena recomputando cada hash; `false` si alguna entrada fue
    /// manipulada (base del audit, M3-5).
    ///
    /// # Errors
    /// Errores de sqlx.
    pub async fn verify_chain(&self) -> Result<bool, sqlx::Error> {
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
    pub async fn corrupt_path_for_test(&self, seq: i64, path: &[u8]) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE journal SET path = ? WHERE seq = ?")
            .bind(path)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
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

/// `entry_hash = sha256(prev_hash ‖ campos con LONGITUD PREFIJADA)`. La longitud
/// prefijada evita colisiones de concatenación (`ab‖c` vs `a‖bc`).
pub(crate) fn chain_hash(prev: &[u8; 32], r: &Record<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev);
    let mut field = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(&r.seq.to_le_bytes());
    field(&r.ts_ms.to_le_bytes());
    field(r.actor_kind.as_bytes());
    field(r.actor_id.unwrap_or("").as_bytes());
    field(r.op.as_bytes());
    field(r.path);
    field(r.path_to.unwrap_or(&[]));
    field(r.reversal.as_bytes());
    field(r.reversal_ref.unwrap_or(&[]));
    h.finalize().into()
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
        // Distinto prev → distinto hash (encadenado).
        assert_ne!(h1, chain_hash(&h1, &rec(1)));
        // Distinto seq → distinto hash.
        assert_ne!(h1, chain_hash(&zero, &rec(2)));
    }

    #[test]
    fn length_prefix_prevents_concatenation_collision() {
        // Dos records que sin longitud-prefijada colisionarían (`ab`+`c` vs
        // `a`+`bc`) deben dar hashes distintos.
        let zero = [0u8; 32];
        let mut a = rec(1);
        a.actor_id = Some("ab");
        a.op = "c";
        let mut b = rec(1);
        b.actor_id = Some("a");
        b.op = "bc";
        assert_ne!(chain_hash(&zero, &a), chain_hash(&zero, &b));
    }

    #[tokio::test]
    async fn open_insert_and_verify_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "created",
            b"file:///a",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
            1,
        )
        .await
        .expect("insert 1");
        j.record(
            "trashed",
            "file:///\u{00e9}".as_bytes(),
            None,
            Reversal::RestoreTrash,
            Some(b"file:///.norte-trash/1-0"),
            &Actor::Agent {
                session: "s1".into(),
            },
            2,
        )
        .await
        .expect("insert 2");

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
            1,
        )
        .await
        .expect("insert");
        // Manipular una entrada (cambiar el path directamente en la fila).
        j.corrupt_path_for_test(1, b"file:///HACKED")
            .await
            .expect("corrupt");
        assert!(
            !j.verify_chain().await.expect("verify"),
            "la manipulación se detecta"
        );
    }
}
