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
//!
//! **Formato (ADR 0046).** Un journal creado desde esta versión declara su
//! formato en una entrada DENTRO de la cadena, en el `seq` 0 reservado: así un
//! binario más viejo puede decir «no sé leer esto» en vez de acusar de
//! manipulación a un fichero que nadie tocó (#127). El `seq` 0 es metadato, no
//! historia: [`Journal::entries`], [`Journal::revertible_for`],
//! [`Journal::count`] y [`Journal::head`] solo ven mutaciones (`seq >= 1`).

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

/// Migración de la columna de lote (batch rename, §17). Va fuera de `SCHEMA` a
/// propósito: `CREATE TABLE IF NOT EXISTS` NO altera una tabla que ya existe,
/// así que una DB escrita antes de esta versión se quedaría sin columna. La
/// idempotencia la da preguntar al catálogo ANTES ([`has_batch_id_column`]), no
/// tragarse el error del `ALTER`: el mensaje de «duplicate column name» no es
/// contrato de nadie, y comerse un error por su texto es comerse también el que
/// no toca.
const MIGRATE_BATCH_ID: &str = "ALTER TABLE journal ADD COLUMN batch_id INTEGER";

/// Migración de la columna que apunta a la entrada que un undo COMPENSA
/// (M3-2). Esta columna se añadió al `SCHEMA` SIN su migración: un journal de
/// antes se abría bien y reventaba en CADA escritura con «table journal has no
/// column named `undoes_seq`». Se vio en vivo al journalizar el motor embebido
/// (#167), donde ese fallo dejaba un `norte cp` en «internal error» sin copiar
/// nada.
///
/// A diferencia de [`MIGRATE_BATCH_ID`], esto SOLO se aplica a una tabla VACÍA:
/// la columna llegó junto con su byte de presencia en el preimagen del hash, así
/// que las filas de antes no verifican bajo el `chain_hash` de hoy. Ver el sitio
/// donde se ejecuta.
const MIGRATE_UNDOES_SEQ: &str = "ALTER TABLE journal ADD COLUMN undoes_seq INTEGER";

/// Lo que `SQLite` espera a un lock ajeno antes de rendirse, salvo que quien
/// abre diga otra cosa ([`Journal::open_with_busy_timeout`]).
///
/// Es el de `sqlx` por omisión, escrito aquí para que sea un hecho con nombre y
/// no una propiedad implícita de una dependencia.
pub const DEFAULT_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The one INSERT of this module, shared by [`Journal::record_entry`] and by
/// the format marker below: a row that the chain covers is written in exactly
/// one place, so «what gets hashed» and «what gets stored» cannot drift.
const INSERT_ENTRY: &str = "INSERT INTO journal (seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id, prev_hash, entry_hash) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

/// The journal format this binary writes, and the highest it knows how to
/// verify (ADR 0046). Bump it only together with a change to what the chain
/// hash covers, or to the meaning of a column.
///
/// **Bumping it is not, by itself, enough.** A journal's declared format is
/// fixed when the file is created and can never be rewritten — re-declaring it
/// changes its digest and breaks every link after it. So a build of format N
/// that opens a journal declaring M < N must either keep hashing that file with
/// M's rules, or append a marker at the point of change declaring "from here
/// on, format N". Appending N-shaped entries onto an M-declaring journal
/// produces exactly the false accusation of #127 — delivered by the fix — for
/// anyone who later opens it with a build of format M. See ADR 0046 §5.
pub const JOURNAL_FORMAT: u32 = 1;

/// `seq` reserved for the format marker. Mutations start at 1
/// ([`Journal::record_entry`] assigns `last_seq + 1` from an initial 0), so
/// row 0 is journal METADATA and never a mutation: that is the discriminator,
/// and it is the reason every mutation reader below filters `seq >= 1`.
const FORMAT_SEQ: i64 = 0;
/// `op` of the marker row. Named, so a raw `sqlite3` dump explains itself.
const FORMAT_OP: &str = "journal_format";
/// `actor_kind` of the marker row. Deliberately outside [`Actor::parts`]'s
/// vocabulary (`user`/`agent`/`plugin`): no actor can claim it, and
/// `revertible_for` cannot return it even if the `seq` filter were dropped.
const FORMAT_ACTOR_KIND: &str = "system";

/// Columnas de lectura, en dos variantes fijas. SIN `format!`: en el fichero
/// que sostiene la evidencia de manipulación, «aquí no se construye SQL con
/// strings» tiene que poder comprobarse de un vistazo. La variante `NULL` es
/// para una DB pre-migración abierta en SOLO-LECTURA, que no se puede alterar.
const SELECT_VERIFY: &str = "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash, batch_id FROM journal ORDER BY seq ASC";
const SELECT_VERIFY_NO_BATCH: &str = "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash, NULL FROM journal ORDER BY seq ASC";
/// `seq >= 1` on every MUTATION reader: row 0 is the format marker
/// ([`FORMAT_SEQ`]), which is part of the chain but not part of the history.
/// Letting it out here would put a non-mutation row in the audit export, in
/// the undo's LIFO stack and in `count()`.
const SELECT_ENTRIES: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id FROM journal WHERE seq >= 1 ORDER BY seq ASC";
const SELECT_ENTRIES_NO_BATCH: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, NULL FROM journal WHERE seq >= 1 ORDER BY seq ASC";
/// Una entrada está DESHECHA si tiene una compensación VIVA: una entrada con
/// su `seq` en `undoes_seq` que a su vez nadie haya compensado.
///
/// La condición ingenua —«existe alguna compensación»— era correcta mientras
/// una compensación jamás se desandaba. El undo de un LOTE (§17) rompió eso:
/// se ejecuta por el mismo ejecutor que la ida, así que si un paso del undo
/// falla, el ejecutor DESANDA los pasos de undo que ya había aplicado y
/// journaliza esa vuelta como compensación de la compensación. El árbol queda
/// como estaba —el lote sigue aplicado—, pero con la condición ingenua sus
/// entradas quedaban tapadas por unas compensaciones que ya no valen, y el
/// lote se volvía INDESHACIBLE para siempre, en silencio.
///
/// La cadena que este código puede producir es `O ← C ← D` y nada más hondo:
/// una compensación nace con `undoes_seq` no nulo, así que jamás es revertible
/// por sí misma y nadie la vuelve a compensar salvo el desandado de su propio
/// lote de undo. Un nivel de anidamiento cubre exactamente eso.
const SELECT_REVERTIBLE: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id \
     FROM journal \
     WHERE seq >= 1 AND undoes_seq IS NULL AND actor_kind = ? AND actor_id IS ? \
       AND seq NOT IN ( \
         SELECT c.undoes_seq FROM journal c \
         WHERE c.undoes_seq IS NOT NULL \
           AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
       ) \
     ORDER BY seq DESC";
const SELECT_REVERTIBLE_NO_BATCH: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, NULL \
     FROM journal \
     WHERE seq >= 1 AND undoes_seq IS NULL AND actor_kind = ? AND actor_id IS ? \
       AND seq NOT IN ( \
         SELECT c.undoes_seq FROM journal c \
         WHERE c.undoes_seq IS NOT NULL \
           AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
       ) \
     ORDER BY seq DESC";

/// Una PÁGINA hacia atrás para la línea de tiempo (fase 7).
///
/// `?1` es la cota superior EXCLUSIVA (`NULL` = desde la más nueva) y `?2`
/// la clase de actor (`NULL` = todas). El `IS NULL OR` de cada una es lo que
/// permite una sola sentencia para las cuatro combinaciones, en vez de
/// construir SQL por concatenación — que es como se acaba metiendo en una
/// consulta algo que vino de fuera.
///
/// Las compensaciones (`undoes_seq IS NOT NULL`) SÍ salen: son mutaciones
/// que ocurrieron, y una línea de tiempo que las escondiera enseñaría un
/// pasado que no pasó — «deshice esto» es un suceso tan real como lo que
/// deshizo.
/// La columna 12 de las dos que siguen: si la entrada YA está deshecha, o
/// sea si tiene una compensación VIVA.
///
/// Es la MISMA condición que usa [`SELECT_REVERTIBLE`] para descartarla, y
/// por eso se escribe una vez y se pega en las dos: si divergieran, la línea
/// de tiempo prometería deshacer entradas que el undo se va a saltar — que
/// es exactamente el número que una confirmación no puede tener mal.
///
/// El cliente NO puede calcularlo: la compensación puede estar fuera de la
/// página que tiene delante.
const COL_UNDONE: &str = "(seq IN ( \
       SELECT c.undoes_seq FROM journal c \
       WHERE c.undoes_seq IS NOT NULL \
         AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
     ))";

/// La página, con [`COL_UNDONE`] pegado detrás de las doce de siempre.
fn select_page(con_batch: bool) -> String {
    let batch = if con_batch { "batch_id" } else { "NULL" };
    format!(
        "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, {batch}, {COL_UNDONE} \
         FROM journal \
         WHERE seq >= 1 AND (?1 IS NULL OR seq < ?1) AND (?2 IS NULL OR actor_kind = ?2) \
         ORDER BY seq DESC LIMIT ?3"
    )
}

/// [`SELECT_REVERTIBLE`] acotado a lo POSTERIOR a un `seq` (fase 7).
///
/// Mismo cuerpo, dos condiciones más. Se duplica en vez de componerse porque
/// la condición de «compensada viva» es la parte delicada de esta consulta
/// —su porqué está sobre [`SELECT_REVERTIBLE`]— y un constructor de SQL que
/// la pegue por trozos es la forma de que un día deje de estar.
///
/// **Un LOTE entra entero o no entra** (`batch_id NOT IN (…seq <= corte)`),
/// y ésta es la condición que de verdad importa aquí. `revertible_for` no la
/// necesitaba: traía TODAS las entradas del actor, así que un `batch_id`
/// siempre llegaba completo a `undo_units`. Al cortar por `seq` deja de ser
/// cierto, y `revert_batch` —que revierte «entero o nada»— recibiría media
/// unidad creyéndola entera: su propio `debug_assert` sólo comprueba que el
/// trozo sea internamente coherente, y un trozo lo es. El resultado sería un
/// `fs.rename_batch` con la mitad de los nombres devueltos y la otra mitad
/// no, con compensaciones escritas para la mitad que se movió.
///
/// Y NO se resuelve metiendo el lote entero: eso desharía entradas anteriores
/// al corte, o sea la fila que el humano señaló para conservar. Se excluye,
/// que es la dirección segura — deshacer de menos se vuelve a pedir; deshacer
/// de más, no. Los seqs de un lote pueden además no ser contiguos (dos tareas
/// concurrentes se intercalan, como dice `alloc_batch`), así que esto no se
/// puede dejar en manos de que el corte «caiga entre lotes».
const SELECT_REVERTIBLE_AFTER: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id \
     FROM journal \
     WHERE seq >= 1 AND seq > ?3 AND undoes_seq IS NULL AND actor_kind = ?1 AND actor_id IS ?2 \
       AND seq NOT IN ( \
         SELECT c.undoes_seq FROM journal c \
         WHERE c.undoes_seq IS NOT NULL \
           AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
       ) \
       AND (batch_id IS NULL OR batch_id NOT IN ( \
         SELECT b.batch_id FROM journal b WHERE b.batch_id IS NOT NULL AND b.seq <= ?3 \
       )) \
       AND (?4 IS NULL OR seq <= ?4) \
       AND (?4 IS NULL OR batch_id IS NULL OR batch_id NOT IN ( \
         SELECT b.batch_id FROM journal b WHERE b.batch_id IS NOT NULL AND b.seq > ?4 \
       )) \
     ORDER BY seq DESC";
// El TECHO (`?4`, 0.80.0) es el espejo exacto del corte: nada por encima, y
// ningún lote con una entrada por encima — mirado contra el journal ENTERO,
// igual que el corte, y no contra lo ya seleccionado. Revertir la mitad
// contada de un lote que seguía creciendo es revertir media unidad
// creyéndola entera. `NULL` = sin techo, que es lo de 0.79.
const SELECT_REVERTIBLE_AFTER_NO_BATCH: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, NULL \
     FROM journal \
     WHERE seq >= 1 AND seq > ?3 AND undoes_seq IS NULL AND actor_kind = ?1 AND actor_id IS ?2 \
       AND seq NOT IN ( \
         SELECT c.undoes_seq FROM journal c \
         WHERE c.undoes_seq IS NOT NULL \
           AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
       ) \
       AND (?4 IS NULL OR seq <= ?4) \
     ORDER BY seq DESC";

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
    /// Devolver los permisos POSIX que tenía (#314).
    ///
    /// El modo ANTERIOR viaja en `reversal_ref`, que para esta op no es una
    /// ruta sino el número en ASCII decimal. Es la única columna que existe
    /// para «lo que la reversa necesita», y añadir otra al esquema por doce
    /// bits sería peor que decir aquí lo que hay dentro.
    SetModeBack,
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
            Reversal::SetModeBack => "set_mode_back",
        }
    }

    /// La reversa que nombra esa etiqueta, o `None` si este binario no la
    /// conoce.
    ///
    /// `None` NO es «irreversible»: es «no sé qué es esto», y quien pregunte
    /// tiene que decidir qué hacer con esa diferencia. La línea de tiempo la
    /// cuenta como SIN vuelta, porque en un journal manipulado —o escrito
    /// por una versión que no es ésta— afirmar que algo se puede deshacer es
    /// la mentira que cuesta cara.
    #[must_use]
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "delete" => Some(Reversal::Delete),
            "rename_back" => Some(Reversal::RenameBack),
            "restore_trash" => Some(Reversal::RestoreTrash),
            "irreversible" => Some(Reversal::Irreversible),
            "set_mode_back" => Some(Reversal::SetModeBack),
            _ => None,
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
    /// Lote al que pertenece la entrada (`fs.rename_batch`). Es la ETIQUETA que
    /// permitirá deshacer n entradas como una sola unidad; el consumidor (el
    /// ejecutor de lotes y el undo por grupo) llega después. `None` para una
    /// mutación suelta — y para toda entrada escrita antes de que existieran
    /// los lotes.
    pub batch_id: Option<i64>,
}

/// Una entrada tal y como la sirve [`Journal::page`]: la entrada, más si YA
/// está deshecha.
///
/// «Deshecha» no es un campo de la tabla: es que exista una compensación
/// suya VIVA, y eso se calcula con una subconsulta (`COL_UNDONE`). Viaja
/// pegada a la entrada porque el cliente no puede deducirlo — la
/// compensación puede estar fuera de su página — y sin ello una línea de
/// tiempo cuenta como deshacible lo que el undo se va a saltar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    /// La entrada.
    pub entry: JournalEntry,
    /// Si tiene una compensación viva.
    pub undone: bool,
}

impl PageEntry {
    /// Esta entrada en su forma de WIRE, para la línea de tiempo
    /// ([`norte_proto::methods::JOURNAL_LIST`], fase 7).
    ///
    /// Vive aquí y no en el daemon porque los dos backends la necesitan: el
    /// embebido contesta la misma lista sin socket de por medio, y dos
    /// conversiones para la misma pregunta divergen en el primer campo que
    /// alguien añada.
    ///
    /// Las rutas salen SANEADAS (`mask_terminal_hazards`) y con
    /// [`norte_proto::methods::JournalRow::hostile`] puesta si el texto dejó
    /// de decir lo que decían los bytes. El nombre de un fichero lo elige
    /// quien lo crea —incluido un agente dentro de su recinto—, y ésta es la
    /// pantalla donde un humano decide qué revertir: un override bidi o una
    /// secuencia de escape aquí repintan esa decisión. Es el mismo trato que
    /// `fs.search` le da a la línea que devuelve, y por el mismo motivo.
    ///
    /// Que los bytes no sean texto no tira la fila: se enseña con
    /// reemplazos. Una mutación que no se ve es indistinguible de una que no
    /// ocurrió.
    #[must_use]
    pub fn to_wire_row(&self) -> norte_proto::methods::JournalRow {
        let e = &self.entry;
        let (path, path_hostil) = texto_de_ruta(&e.path);
        let (path_to, to_hostil) = match &e.path_to {
            Some(b) => {
                let (t, h) = texto_de_ruta(b);
                (Some(t), h)
            }
            None => (None, false),
        };
        norte_proto::methods::JournalRow {
            seq: e.seq,
            ts_ms: e.ts_ms,
            actor_kind: e.actor_kind.clone(),
            actor_id: e.actor_id.clone(),
            op: e.op.clone(),
            path,
            path_to,
            hostile: path_hostil || to_hostil,
            // Un token de `reversal` que este daemon no conoce cuenta como
            // SIN vuelta: en un journal manipulado, afirmar que algo se
            // puede deshacer es la mentira cara.
            reversible: Reversal::from_str_opt(&e.reversal)
                .is_some_and(|r| r != Reversal::Irreversible),
            undoes_seq: e.undoes_seq,
            undone: self.undone,
            batch_id: e.batch_id,
        }
    }
}

/// Una página en su forma de wire, con el CURSOR ya calculado.
///
/// Vive aquí, junto a [`PageEntry::to_wire_row`], y por la misma razón: los
/// dos bordes que sirven `journal.list` —el daemon y el backend embebido—
/// tenían esta misma expresión de seis fichas copiada, y ninguna de las dos
/// copias podía ponerse roja sola.
///
/// **El cursor sólo se ofrece si la página vino LLENA.** Con una a medias ya
/// no queda nada más viejo, y ofrecerlo haría que el cliente pidiera otra
/// vuelta para recibir cero filas, indefinidamente. Y es el `seq` de la
/// última servida, nunca `seq - 1`: los `seq` no son densos, y esa resta es
/// justo la aritmética que se rompe el día que dejen de serlo.
#[must_use]
pub fn page_to_wire(entries: &[PageEntry], limit: u32) -> norte_proto::methods::JournalListResult {
    let lleno = entries.len() == limit as usize;
    norte_proto::methods::JournalListResult {
        rows: entries.iter().map(PageEntry::to_wire_row).collect(),
        // El `flatten` no es adorno: con `limit` cero —que ningún borde deja
        // pasar, pero que nadie de aquí abajo puede garantizar— una página
        // vacía contaría como «llena», y esto la salva de anunciar un cursor
        // que no existe.
        next_before_seq: lleno.then(|| entries.last().map(|e| e.entry.seq)).flatten(),
    }
}

/// El texto pintable de unos bytes de ruta, y si dejó de decir lo que ellos
/// decían (por no ser texto, o por llevar algo que un terminal ejecutaría).
fn texto_de_ruta(bytes: &[u8]) -> (String, bool) {
    let crudo = String::from_utf8_lossy(bytes);
    let saneado = norte_encoding::mask_terminal_hazards(&crudo);
    let hostil = matches!(crudo, std::borrow::Cow::Owned(_)) || saneado != crudo;
    (saneado, hostil)
}

/// Materializa un `JournalEntry` desde una fila con el orden de columnas
/// `seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to,
/// reversal, reversal_ref, undoes_seq, batch_id` (compartido por `entries` y
/// `revertible_for`).
///
/// `try_get` en todas: el tipado de `SQLite` es DINÁMICO, así que un blob
/// no-UTF-8 metido en una columna TEXT hace panicar a `get` — y un panic aquí
/// es un `norte audit export` que no exporta nada en vez de un error que se
/// pueda leer y contar.
fn row_to_entry(row: &sqlx::sqlite::SqliteRow) -> Result<JournalEntry, JournalError> {
    Ok(JournalEntry {
        seq: row.try_get(0)?,
        ts_ms: row.try_get(1)?,
        entry_hash: row.try_get(2)?,
        actor_kind: row.try_get(3)?,
        actor_id: row.try_get(4)?,
        op: row.try_get(5)?,
        path: row.try_get(6)?,
        path_to: row.try_get(7)?,
        reversal: row.try_get(8)?,
        reversal_ref: row.try_get(9)?,
        undoes_seq: row.try_get(10)?,
        batch_id: row.try_get(11)?,
    })
}

/// ¿Existe ya la columna `batch_id`? Se pregunta al catálogo en vez de asumir:
/// el handle de SOLO-LECTURA (audit) no puede migrar una DB antigua y aun así
/// tiene que leerla. Una tabla ausente responde CERO filas → `false`, y la
/// primera query real fallará con su error propio, sin enmascarar nada.
async fn has_batch_id_column(pool: &SqlitePool) -> Result<bool, JournalError> {
    has_column(pool, "batch_id").await
}

/// Si la tabla `journal` tiene la columna `nombre`, preguntándoselo al catálogo.
///
/// Ver [`has_batch_id_column`] para por qué se pregunta en vez de tragarse el
/// error del `ALTER`.
async fn has_column(pool: &SqlitePool, nombre: &str) -> Result<bool, JournalError> {
    Ok(columnas(pool).await?.iter().any(|c| c == nombre))
}

/// Las columnas de la tabla `journal`, según el catálogo. VACÍO si la tabla no
/// existe —lo que aquí no es un error: el audit abre el fichero que le señalen y
/// la primera query real fallará con su error propio, sin enmascarar nada.
async fn columnas(pool: &SqlitePool) -> Result<Vec<String>, JournalError> {
    let rows = sqlx::query("PRAGMA table_info(journal)")
        .fetch_all(pool)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        // `try_get`: la fila la produce un fichero que el operador señala (el
        // audit abre lo que le den), así que su forma no se da por hecha.
        out.push(r.try_get::<String, _>(1)?);
    }
    Ok(out)
}

/// What format the journal on disk declares (ADR 0046).
///
/// The declaration is the row at the reserved `seq 0`, written when the
/// journal is created and covered by the hash chain like any other row. Its
/// ABSENCE is
/// information, not a fault: every journal created before the marker existed
/// is [`JournalFormat::Unmarked`] and verifies exactly as it always did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JournalFormat {
    /// No marker row: a journal created before the marker existed. It is not
    /// stamped retroactively — inserting a row ahead of `seq 1` would change
    /// what the second entry chains onto and break a chain nobody touched.
    Unmarked,
    /// The journal declares this format version.
    Version(u32),
    /// There IS a row at the reserved `seq`, and this binary cannot read its
    /// version. Treated exactly like a version from the future: something
    /// wrote metadata with rules this build does not know.
    Unreadable,
}

impl JournalFormat {
    /// `true` when this binary cannot claim to understand the journal: a
    /// declared version outside `1..=`[`JOURNAL_FORMAT`], or a marker it cannot
    /// read. Version 0 counts as unknown — no format was ever numbered 0, so a
    /// journal claiming it was written by something this build cannot name.
    /// [`JournalFormat::Unmarked`] is NOT unknown: an unmarked journal predates
    /// the marker and is verified with today's rules.
    #[must_use]
    pub fn is_unknown(self) -> bool {
        match self {
            JournalFormat::Unmarked => false,
            JournalFormat::Version(v) => !(1..=JOURNAL_FORMAT).contains(&v),
            JournalFormat::Unreadable => true,
        }
    }
}

/// Veredicto de [`Journal::verify_chain`] (B2 de #63): si la cadena se
/// rompió, DÓNDE — el audit lo cita en vez de un booleano mudo.
///
/// `#[non_exhaustive]`: a verdict enum grows (this variant is the proof), and
/// a caller that stops compiling is cheaper than one that silently treats a
/// new verdict as the old one. The wildcard arm a caller must write is the
/// fail-closed answer — "did not certify" — for every verdict yet to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// The journal declares a format this binary does not know (ADR 0046), so
    /// this build **cannot verify it** — neither to clear it nor to accuse it.
    ///
    /// This is NOT a clean bill of health and NOT an accusation. It is the
    /// verdict for the case where an accusation would be a lie: an older
    /// binary meeting entries hashed over a field set that did not exist when
    /// it was built recomputes different digests on an untouched file, and
    /// `Broken` there teaches a user to ignore the one signal the journal
    /// exists to give (#127).
    ///
    /// It does not exculpate anyone either, and it is NOT proof that a journal
    /// is merely newer: a tampered journal whose marker was also re-declared
    /// lands here instead of in [`ChainStatus::Broken`], and no anchor rules
    /// that out (see [`Journal::verify_chain`] — such an edit leaves every
    /// stored digest at `seq >= 1` untouched, so the anchors still verify).
    /// What survives is the refusal: [`ChainStatus::is_intact`] is `false`, the
    /// journal is not certified, and callers must fail closed.
    UnknownFormat {
        /// What the journal says it is.
        declared: JournalFormat,
        /// What this binary knows ([`JOURNAL_FORMAT`]).
        known: u32,
        /// First `seq` whose hash did not recompute under this binary's rules,
        /// if any. `None` means everything recomputed and the refusal is about
        /// the declaration alone.
        first_unverifiable_seq: Option<i64>,
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
    //
    // ESTO NO ES UN PATRÓN REUTILIZABLE. Funciona porque `batch_id` es el
    // ÚLTIMO campo: el mensaje de una entrada sin lote es un prefijo estricto
    // del de una con lote, así que no hay ambigüedad. Un SEGUNDO campo opcional
    // añadido con el mismo truco la crearía al instante — `(batch=Some(x),
    // otro=None)` y `(batch=None, otro=Some(x))` producirían la MISMA cola
    // `01 ‖ len ‖ x` y por tanto el mismo `entry_hash`, que es un agujero en la
    // cadena, no una optimización. Campo nuevo ⇒ `feed_opt` (presencia
    // siempre), y si por compatibilidad hiciera falta repetir el truco, versiona
    // antes el formato de la cadena — con un marcador DENTRO de lo que la cadena
    // y las anclas autentican, nunca en la cabecera del fichero.
    if let Some(b) = r.batch_id {
        h.update([1u8]);
        feed(&mut h, &b.to_le_bytes());
    }
    h.finalize().into()
}

/// The marker row as a [`Record`], in ONE place: the writer and the verifier
/// build the same preimage or the marker is not verifiable at all.
///
/// **This preimage is frozen forever, and that is what makes the whole scheme
/// work.** Every field it feeds existed in format 1, and `batch_id: None`
/// feeds nothing, so a binary that predates the marker — including one that
/// predates `batch_id` — recomputes this row's hash byte for byte and reports
/// it intact instead of accusing it. A future format that adds a hashed field
/// must keep it out of THIS row's preimage; otherwise the binary that needs to
/// read the version in order not to accuse would first have to know the format
/// the version is there to announce.
fn format_record(ts_ms: i64, version: &[u8]) -> Record<'_> {
    Record {
        seq: FORMAT_SEQ,
        ts_ms,
        actor_kind: FORMAT_ACTOR_KIND,
        actor_id: None,
        op: FORMAT_OP,
        // The version travels in `path` as ASCII decimal. Reusing a column
        // beats adding one: a new column would have to be migrated onto
        // journals that already exist and fed to the chain for every row.
        path: version,
        path_to: None,
        // There is no undoing a format declaration. The literal is deliberate:
        // it must NOT be "tidied up" into `Reversal::Irreversible.as_str()`,
        // because this preimage is frozen and that enum is not.
        reversal: "irreversible",
        reversal_ref: None,
        undoes_seq: None,
        batch_id: None,
    }
}

/// Reads the declared version out of a marker row's `op` and `path`.
///
/// Anything it cannot read is [`JournalFormat::Unreadable`], never a default:
/// guessing "probably 1" for a row written by something else is the exact
/// failure this marker exists to prevent, with the blame reversed.
///
/// The decimal must be CANONICAL. `u32::from_str` would also accept `+1` and
/// `0001`, which would give one version several byte strings and therefore
/// several valid digests — a version has exactly one preimage or the frozen
/// vector below means nothing.
fn parse_format(op: &str, path: &[u8]) -> JournalFormat {
    if op != FORMAT_OP {
        return JournalFormat::Unreadable;
    }
    let Ok(s) = std::str::from_utf8(path) else {
        return JournalFormat::Unreadable;
    };
    match s.parse::<u32>() {
        Ok(v) if v.to_string() == s => JournalFormat::Version(v),
        _ => JournalFormat::Unreadable,
    }
}

/// The hashed CONTENT of a row, owned, as [`Journal::verify_chain`] decodes it.
/// Owning it keeps the fallible decoding in one place: a column that does not
/// decode makes the whole struct absent, and an absent struct is a row that
/// cannot be recomputed — which is a verdict, not an error.
struct VerifiedRow {
    ts_ms: i64,
    actor_kind: String,
    actor_id: Option<String>,
    op: String,
    path: Vec<u8>,
    path_to: Option<Vec<u8>>,
    reversal: String,
    reversal_ref: Option<Vec<u8>>,
    undoes_seq: Option<i64>,
    batch_id: Option<i64>,
}

impl VerifiedRow {
    /// The [`Record`] this row hashes as, borrowing from `self`.
    fn record(&self, seq: i64) -> Record<'_> {
        Record {
            seq,
            ts_ms: self.ts_ms,
            actor_kind: &self.actor_kind,
            actor_id: self.actor_id.as_deref(),
            op: &self.op,
            path: &self.path,
            path_to: self.path_to.as_deref(),
            reversal: &self.reversal,
            reversal_ref: self.reversal_ref.as_deref(),
            undoes_seq: self.undoes_seq,
            batch_id: self.batch_id,
        }
    }

    /// Is this row shaped like the format marker in every field the marker does
    /// not get to choose? Only the version (`path`) is the row's own.
    ///
    /// Comparing beats trusting: the digest alone cannot speak for a column the
    /// verifier substitutes before hashing, and `op` is read afterwards to
    /// decide whether the journal is readable at all.
    fn is_canonical_marker(&self) -> bool {
        let canonical = format_record(self.ts_ms, &self.path);
        self.op == canonical.op
            && self.actor_kind == canonical.actor_kind
            && self.reversal == canonical.reversal
            && self.actor_id.is_none()
            && self.path_to.is_none()
            && self.reversal_ref.is_none()
            && self.undoes_seq.is_none()
            && self.batch_id.is_none()
    }
}

/// Decodes a row's content columns from `SELECT_VERIFY`'s column order.
fn decode_verified_row(row: &sqlx::sqlite::SqliteRow) -> Result<VerifiedRow, sqlx::Error> {
    Ok(VerifiedRow {
        ts_ms: row.try_get(1)?,
        actor_kind: row.try_get(2)?,
        actor_id: row.try_get(3)?,
        op: row.try_get(4)?,
        path: row.try_get(5)?,
        path_to: row.try_get(6)?,
        reversal: row.try_get(7)?,
        reversal_ref: row.try_get(8)?,
        undoes_seq: row.try_get(9)?,
        batch_id: row.try_get(12)?,
    })
}

/// UTC milliseconds, saturating: a clock before the epoch or past `i64` gives
/// a bad timestamp, never a panic in the code that writes the evidence.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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
    /// **No se CLONA fuera de este tipo**, y de eso depende
    /// [`crate::embedded::LazyJournal::release`]: un `SqlitePool` clonado
    /// sobrevive al `Arc::try_unwrap` que decide que nadie sostiene el journal,
    /// y mantiene el fichero abierto después de que este proceso se haya
    /// declarado no-dueño. `pub(crate)` no lo impide; esta línea sí lo dice.
    /// Todos los usos del árbol son préstamos (`&self.pool`).
    pub(crate) pool: SqlitePool,
    chain: Mutex<ChainState>,
    /// ¿Tiene la tabla la columna `batch_id`? Siempre `true` tras un [`Journal::open`]
    /// (migra), puede ser `false` en [`Journal::open_read_only`] sobre una DB
    /// pre-migración, que no se puede alterar y aun así hay que poder auditar.
    has_batch_id: bool,
    /// A quién se le ofrece cada fila comprometida (ADR 0100): los hooks. Un
    /// `RwLock` std porque se lee en cada `record_entry` y se escribe una vez
    /// al arrancar; `None` hasta que [`Journal::set_hook_sender`] lo ponga.
    hooks: std::sync::RwLock<Option<crate::hooks::HookSender>>,
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
    /// El lote al que pertenece, si formó parte de uno ([`Journal::alloc_batch`]):
    /// la etiqueta que agrupa n entradas para deshacerlas juntas. Las
    /// compensaciones de un undo de grupo se registran con el MISMO lote, para
    /// que el grupo siga siendo legible después.
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
        Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT).await
    }

    /// Como [`Journal::open`], pero con un plazo propio para el caso «el lock lo
    /// tiene otro».
    ///
    /// `busy_timeout` es lo que `SQLite` espera antes de rendirse con `database
    /// is locked`. El de [`DEFAULT_BUSY_TIMEOUT`] es el que quiere el daemon:
    /// arranca una vez y prefiere aguantar un checkpoint ajeno a morir.
    ///
    /// Un proceso EMBEBIDO quiere lo contrario y por eso existe esta puerta
    /// (#167): quien tiene el lock lo tiene para toda su vida —otro TUI, o el
    /// daemon—, así que esperar cinco segundos no lo consigue, solo convierte
    /// cada `norte cp` en cinco segundos de nada antes de seguir sin registro.
    ///
    /// OJO: el plazo es de la CONEXIÓN, no del `open`. Rige también cada
    /// sentencia posterior sobre ese handle — hoy da igual (una sola conexión,
    /// dueña exclusiva, sin nadie con quien competir), y dejaría de darlo si
    /// alguna vez se permitiera reconectar bajo un lock ajeno.
    ///
    /// # Errors
    /// Las mismas que [`Journal::open`] — y con un plazo corto, `database is
    /// locked` deja de ser el caso raro: es LA respuesta esperada cuando el
    /// journal ya tiene dueño.
    pub async fn open_with_busy_timeout(
        path: &std::path::Path,
        busy_timeout: std::time::Duration,
    ) -> Result<Self, JournalError> {
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
        let opts = opts.busy_timeout(busy_timeout);
        let this = Self::from_options(opts).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Cinturón por si el fichero preexistía con otros permisos. Los
            // sidecars -wal/-shm heredan del principal. Por `tokio::fs` (regla
            // 2): es un `chmod` corto, pero un `std::fs` en un contexto async
            // no deja de serlo por ser barato.
            let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
        }
        Ok(this)
    }

    /// Cierra el journal y ESPERA a que el fichero quede libre.
    ///
    /// Soltar el valor no basta y por eso esto existe: `sqlx` cierra la
    /// conexión de `SQLite` en su hilo trabajador, así que un `drop` devuelve
    /// antes de que el lock exclusivo del fichero se haya soltado y el
    /// siguiente en abrir se lleva un `database is locked` que nadie sostiene.
    /// Lo nota [`crate::embedded::LazyJournal::release`], que suelta para que
    /// OTRO proceso pueda abrir acto seguido.
    ///
    /// Consume el journal: reabrir es [`Self::open`], y tiene que serlo — es
    /// ahí donde `last_seq`/`last_hash` se releen del fichero.
    pub async fn close(self) {
        self.pool.close().await;
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
        //
        // Lo que sí se ataja es el journal anterior a `undoes_seq`: TODAS las
        // consultas de aquí nombran esa columna y este handle no puede `ALTER`
        // (es solo-lectura por diseño), así que saldría un «no such column»
        // crudo envuelto en `cli-audit-open-failed`. La tabla AUSENTE no entra
        // por aquí —eso es «no es un journal», y lo cuenta la query real—.
        let cols = columnas(&pool).await?;
        if !cols.is_empty() && !cols.iter().any(|c| c == "undoes_seq") {
            return Err(JournalError::Corrupt(
                "journal anterior a undoes_seq: este binario no sabe leerlo (sus filas \
                 se hashearon sobre un preimagen sin esa columna). Léelo con un \
                 binario de su época",
            ));
        }
        let has_batch_id = has_batch_id_column(&pool).await?;
        // El contador arranca del máximo escrito igual que en escritura: este
        // handle no puede insertar nada (SQLite lo rechaza), pero un
        // `alloc_batch` que devolviera ids ya usados sería una respuesta
        // MENTIROSA, y aquí no se miente por no poder equivocarse.
        let batch_counter: i64 = if has_batch_id {
            sqlx::query("SELECT COALESCE(MAX(batch_id), 0) FROM journal")
                .fetch_one(&pool)
                .await?
                .try_get(0)?
        } else {
            0
        };
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState {
                last_seq: 0,
                last_hash: [0u8; 32],
                batch_counter,
            }),
            has_batch_id,
            hooks: std::sync::RwLock::new(None),
        })
    }

    /// Elige entre la consulta CON columna de lote y la que la sustituye por
    /// `NULL` (DB pre-migración abierta en solo-lectura, que no se puede
    /// alterar). Dos constantes, ninguna construida.
    fn pick(&self, with: &'static str, without: &'static str) -> &'static str {
        if self.has_batch_id { with } else { without }
    }

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, JournalError> {
        // Pool de 1 conexión: un solo escritor (in-memory exige max=1 para no
        // perder la DB entre conexiones).
        //
        // Y esa conexión NO se recicla, que es lo que sostiene todo lo demás.
        // El lock exclusivo del fichero es una propiedad de LA CONEXIÓN VIVA, no
        // del proceso: los defaults de `sqlx` (`min_connections=0`,
        // `idle_timeout=10min`, `max_lifetime=30min`) levantan un barrendero que
        // la cierra estando ociosa, y cerrarla SUELTA el lock. Con eso, un
        // proceso que se cree dueño (un TUI que lleva once minutos navegando)
        // deja entrar a otro, y su `ChainState` en memoria —`last_seq`,
        // `last_hash`— se queda viejo: la siguiente mutación choca contra la PK
        // de `seq` y falla, y como `last_seq` solo avanza al acertar, falla
        // TODAS las siguientes. Un efecto ya aplicado sin su fila, en bucle.
        // (Y en `sqlite::memory:`, cerrar la única conexión BORRA la DB.)
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        // Migración: se pregunta al catálogo y solo entonces se altera. Todo
        // error se PROPAGA (falla en seguro: sin la columna no se puede
        // journalizar un lote, y escribir sin ella perdería el agrupamiento en
        // silencio, que es justo lo que la cadena tiene que impedir). La
        // idempotencia sale del catálogo, NO de mirarle el texto al error:
        // «duplicate column name» no es contrato de nadie, y comerse un error
        // por su mensaje es comerse también el que no toca.
        //
        // El marcador de formato (#127, ADR 0046) es lo que le queda a un
        // binario FUTURO para no confundir «no sé leer esto» con «te lo han
        // manipulado». Se escribe abajo, sobre un journal sin filas.
        if !has_batch_id_column(&pool).await? {
            sqlx::query(MIGRATE_BATCH_ID).execute(&pool).await?;
            if !has_batch_id_column(&pool).await? {
                return Err(JournalError::Corrupt(
                    "la columna batch_id sigue ausente tras migrar",
                ));
            }
        }
        // Y la de `undoes_seq`, que es MÁS vieja. Va antes del marcador de
        // formato de abajo porque ese marcador es una fila, o sea un INSERT que
        // nombra la columna.
        //
        // Pero SOLO si la tabla está vacía, y esta es la diferencia con
        // `batch_id`: aquella columna se añadió sin tocar el preimagen del hash
        // (`None` no alimenta nada, ver `chain_hash`), y esta llegó JUNTO con su
        // byte de presencia. Una fila escrita antes se hasheó sobre un preimagen
        // que terminaba en `reversal_ref`; recalcularla hoy da otro digest. Es
        // decir: migrar una DB CON historia la deja escribible y `verify_chain`
        // la declara rota en su primera fila —una acusación FALSA de
        // manipulación sobre un fichero que nadie tocó, que es exactamente lo
        // que el marcador de formato (ADR 0046) existe para no producir— y sin
        // arreglo posible, porque esas filas ya no se pueden rehashear.
        //
        // Así que se rehúsa, y se dice qué hacer. El embebido lo verá como
        // `NoJournal::Failed` y seguirá sin registro (#167); el daemon no
        // arrancará, que para un journal ilegible es lo correcto.
        if !has_column(&pool, "undoes_seq").await? {
            let filas: i64 = sqlx::query("SELECT COUNT(*) FROM journal")
                .fetch_one(&pool)
                .await?
                .try_get(0)?;
            if filas != 0 {
                return Err(JournalError::Corrupt(
                    "journal anterior a undoes_seq y CON historia: migrarlo haría que \
                     verify_chain lo declarase roto en su primera fila (esas filas se \
                     hashearon sobre un preimagen sin esa columna). Expórtalo con un \
                     binario de su época, archívalo y deja que se cree uno nuevo",
                ));
            }
            sqlx::query(MIGRATE_UNDOES_SEQ).execute(&pool).await?;
            if !has_column(&pool, "undoes_seq").await? {
                return Err(JournalError::Corrupt(
                    "la columna undoes_seq sigue ausente tras migrar",
                ));
            }
        }
        Self::stamp_format_if_new(&pool).await?;
        Self::warn_if_format_unknown(&pool).await?;
        // SIN filtro de `seq` A PROPÓSITO (y no es un descuido que «unificar»
        // con las lecturas de mutaciones): la primera mutación de un journal
        // recién creado tiene que encadenar con el MARCADOR del `seq` 0. Si
        // esta consulta lo saltara, nacería encadenada al hash cero y la cadena
        // estaría rota desde la entrada 1.
        let (last_seq, last_hash) =
            sqlx::query("SELECT seq, entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&pool)
                .await?
                .map_or(Ok((0i64, [0u8; 32])), |row| {
                    // `try_get`: un blob hostil aquí haría panicar el ARRANQUE
                    // del dueño del journal. Falla con error tipado, que es lo
                    // que la regla 6 pide y lo que ADR 0046 §5 supone al
                    // razonar sobre disponibilidad.
                    let seq: i64 = row.try_get(0)?;
                    let v: Vec<u8> = row.try_get(1)?;
                    if v.len() != 32 {
                        return Err(JournalError::Corrupt("entry_hash no mide 32 bytes"));
                    }
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&v);
                    Ok((seq, h))
                })?;
        // El contador de lotes arranca del MÁXIMO ya escrito: reabrir jamás
        // reutiliza un id que alguna entrada lleva puesto.
        // `try_get`: un blob hostil en la columna haría panicar a `get`, y en
        // este fichero un panic es un veredicto que nunca se emite.
        let batch_counter: i64 = sqlx::query("SELECT COALESCE(MAX(batch_id), 0) FROM journal")
            .fetch_one(&pool)
            .await?
            .try_get(0)?;
        Ok(Self {
            pool,
            chain: Mutex::new(ChainState {
                last_seq,
                last_hash,
                batch_counter,
            }),
            has_batch_id: true,
            hooks: std::sync::RwLock::new(None),
        })
    }

    /// Writes the format marker (ADR 0046) — but only on a journal that has no
    /// rows at all.
    ///
    /// The condition is the whole design. A journal that already holds entries
    /// cannot be stamped: the marker lives at `seq 0`, ahead of the first
    /// mutation, and inserting it there would leave `seq 1` chained onto the
    /// zero hash while a row now precedes it — `verify_chain` would report
    /// `Broken` on a file nobody touched, which is precisely the accusation
    /// this marker exists to avoid. So journals written before this change
    /// stay [`JournalFormat::Unmarked`] for life, and only journals created
    /// from here on declare anything. That is what "fixable only forward"
    /// means in code.
    async fn stamp_format_if_new(pool: &SqlitePool) -> Result<(), JournalError> {
        let rows: i64 = sqlx::query("SELECT COUNT(*) FROM journal")
            .fetch_one(pool)
            .await?
            .try_get(0)?;
        if rows != 0 {
            return Ok(());
        }
        let ts_ms = now_ms();
        let version = JOURNAL_FORMAT.to_string();
        let rec = format_record(ts_ms, version.as_bytes());
        let prev = [0u8; 32];
        let entry_hash = chain_hash(&prev, &rec);
        sqlx::query(INSERT_ENTRY)
            .bind(rec.seq)
            .bind(rec.ts_ms)
            .bind(rec.actor_kind)
            .bind(rec.actor_id)
            .bind(rec.op)
            .bind(rec.path)
            .bind(rec.path_to)
            .bind(rec.reversal)
            .bind(rec.reversal_ref)
            .bind(rec.undoes_seq)
            .bind(rec.batch_id)
            .bind(&prev[..])
            .bind(&entry_hash[..])
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Says so, loudly, when the journal declares a format this build does not
    /// know — and then opens it anyway.
    ///
    /// Opening it is the lesser evil, and the choice is deliberate. Appending
    /// this build's entries onto a journal written by a newer one is genuinely
    /// bad: the newer build will later verify with rules these rows were not
    /// written under and report a break on entries nobody touched. But
    /// REFUSING to open would hand anyone who can write one column — the
    /// declared version — the power to stop the daemon from starting, which
    /// converts the marker into an availability weapon and is a worse trade
    /// than a loud log. The real fix is that a format bump must not append to
    /// a journal it cannot verify; see [`JOURNAL_FORMAT`] and ADR 0046 §5.
    ///
    /// It reads the marker WITHOUT verifying its hash — cheap, and enough for a
    /// log line. That is also why it must never be promoted into a gate: an
    /// unverified declaration is exactly what one column write can change.
    async fn warn_if_format_unknown(pool: &SqlitePool) -> Result<(), JournalError> {
        let row = sqlx::query("SELECT op, path FROM journal WHERE seq = 0")
            .fetch_optional(pool)
            .await?;
        let Some(row) = row else { return Ok(()) };
        let op: String = row.try_get(0)?;
        let path: Vec<u8> = row.try_get(1)?;
        let declared = parse_format(&op, &path);
        if declared.is_unknown() {
            tracing::warn!(
                ?declared,
                known = JOURNAL_FORMAT,
                "el journal declara un formato que este binario no conoce: sus entradas \
                 nuevas se escriben con las reglas de este formato y una versión más \
                 nueva las verá como rotas (ADR 0046)"
            );
        }
        Ok(())
    }

    /// What format this journal declares (ADR 0046). Reads the marker row;
    /// [`JournalFormat::Unmarked`] when there is none.
    ///
    /// It reports the DECLARATION, not a verdict: the version is only worth
    /// believing once the marker's own hash has been checked, which is what
    /// [`Journal::verify_chain`] does.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn format(&self) -> Result<JournalFormat, JournalError> {
        let row = sqlx::query("SELECT op, path FROM journal WHERE seq = 0")
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(JournalFormat::Unmarked);
        };
        // `try_get`: this row comes from a file an operator points us at.
        let op: String = row.try_get(0)?;
        let path: Vec<u8> = row.try_get(1)?;
        Ok(parse_format(&op, &path))
    }

    /// Registra una mutación NORMAL (no compensa ningún undo). El `seq` se
    /// asigna monótono DENTRO del lock de la cadena (junto al encadenado) →
    /// orden de `seq` == orden de hash. Devuelve el `seq` asignado. Si el insert
    /// falla, ni `seq` ni `last_hash` avanzan (sin huecos ni cadena rota).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] al insertar.
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
    #[expect(
        clippy::too_many_arguments,
        reason = "todos los campos de una entrada del journal, en el orden de la tabla"
    )]
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
        let ts_ms = now_ms();

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

        sqlx::query(INSERT_ENTRY)
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
        drop(chain);
        // Y DESPUÉS de durable, a los hooks (ADR 0100): lo que un hook ve es
        // exactamente lo que el journal registró. `offer` no espera nunca.
        let sender = self
            .hooks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(tx) = sender {
            tx.offer(crate::hooks::HookEvent {
                seq,
                ts_ms,
                op: op.to_owned(),
                actor_kind: actor_kind.to_owned(),
                path: path.to_vec(),
                path_to: path_to.map(<[u8]>::to_vec),
                batch_id,
            });
        }
        Ok(seq)
    }

    /// Instala el extremo al que se le ofrece cada fila comprometida (ADR
    /// 0100). El segundo en instalarse pisa al primero: hay un despachador
    /// por proceso, y es del arranque.
    pub fn set_hook_sender(&self, tx: crate::hooks::HookSender) {
        *self
            .hooks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    }

    /// Número de MUTACIONES (el marcador de formato del `seq 0` no lo es).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn count(&self) -> Result<i64, JournalError> {
        let row = sqlx::query("SELECT COUNT(*) FROM journal WHERE seq >= 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get(0))
    }

    /// Walks the chain, recomputing every hash. `Broken` names the FIRST entry
    /// whose link or digest does not hold (#63 B2: the audit cites it).
    /// Keyless: it does NOT detect a full rewrite, a tail truncation or a
    /// rollback — that cover comes from the HMAC anchors (ADR 0025).
    ///
    /// # The format marker, and why the order of these checks is the point
    ///
    /// Row `seq 0`, when present, declares the journal's format (ADR 0046).
    /// The walk **verifies that row's own hash before it believes a word of
    /// it**, and only then decides what a later mismatch means:
    ///
    /// - marker absent, or declaring a version this binary knows → today's
    ///   behaviour exactly: [`ChainStatus::Intact`] or [`ChainStatus::Broken`];
    /// - marker intact and declaring a version above [`JOURNAL_FORMAT`] (or one
    ///   this binary cannot read) → [`ChainStatus::UnknownFormat`], because an
    ///   older build cannot recompute hashes over a field set that did not
    ///   exist when it was compiled, and calling that "tampering" is a false
    ///   accusation (#127);
    /// - marker itself altered → [`ChainStatus::Broken`] at `seq 0`. Its
    ///   digest is recomputed from the CANONICAL marker record, not from the
    ///   row's own columns, so a marker with a doctored `actor_kind` or
    ///   `reversal` does not verify however carefully its hash was refreshed.
    ///
    /// # What the walk keeps checking when it cannot recompute
    ///
    /// A digest that does not recompute stops nothing: the walk carries the
    /// STORED hash forward and keeps checking every link
    /// (`prev_hash[i] == entry_hash[i-1]`). The link needs no preimage, so it
    /// is the one property this binary can assert about a journal it cannot
    /// read, and a link that does not hold is [`ChainStatus::Broken`] even
    /// under an unknown format. That is what keeps an insertion, a deletion or
    /// a reordering visible in a journal from the future.
    ///
    /// # What remains, stated rather than hidden
    ///
    /// This is keyless (ADR 0023): an attacker who can write the file can
    /// recompute the whole chain, and can therefore produce a self-consistent
    /// journal declaring any format at all — as they could already produce one
    /// declaring format 1 with the history of their choice. What the marker
    /// adds is cheaper: re-declaring it and relinking `seq 1` (three column
    /// writes, no key) turns a [`ChainStatus::Broken`] verdict into
    /// [`ChainStatus::UnknownFormat`], trading a located accusation for a
    /// refusal. **The HMAC anchors do not close that one**: such an edit
    /// changes no stored digest at `seq >= 1`, so every anchor still verifies.
    /// The alarm survives — the audit exits with failure either way — but the
    /// blame does not. Head anchors close the consistent rewrite, which is the
    /// strictly larger attack; the inconsistent re-declaration is closed by
    /// anchoring the MARKER itself ([`Journal::marker_hash`], #146), which is a
    /// separate line in a separate file for exactly that reason.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`], including a column whose stored type this build
    /// cannot decode — a verdict is never produced by panicking.
    pub async fn verify_chain(&self) -> Result<ChainStatus, JournalError> {
        let rows = sqlx::query(self.pick(SELECT_VERIFY, SELECT_VERIFY_NO_BATCH))
            .fetch_all(&self.pool)
            .await?;
        // The digest the NEXT row must carry in `prev_hash`. It is the STORED
        // hash of the row before it, never the recomputed one: they are equal
        // whenever a row verifies, and when one does not, the stored value is
        // what lets the link check survive the row it could not recompute.
        let mut prev = [0u8; 32];
        let mut verified: u64 = 0;
        let mut declared = JournalFormat::Unmarked;
        let mut first_bad: Option<i64> = None;
        for row in rows {
            // The row's SKELETON — `seq` and the two digests — must decode:
            // without them a row cannot be placed in the chain at all, and that
            // is a failure of the database, not of an entry.
            let seq: i64 = row.try_get(0)?;
            let stored_prev: Vec<u8> = row.try_get(10)?;
            let stored_hash: Vec<u8> = row.try_get(11)?;
            // Its CONTENT is different: `SQLite`'s typing is dynamic, so a blob
            // written into a TEXT column makes the decode fail (and `get`
            // PANIC). A column that does not decode is EVIDENCE — the row
            // cannot recompute — and it is treated as such below, because
            // answering `Err` there would let one column write replace a
            // located accusation with a shrug.
            let content = decode_verified_row(&row);
            // Two format-independent checks, and they run first. Below the
            // reserved metadata `seq` nothing legitimate exists — a row there
            // would be chained and certified while being invisible to every
            // mutation reader — and a `prev_hash` that is not the previous
            // `entry_hash` is a break in the chain itself, whatever any version
            // puts in its preimage.
            if seq < FORMAT_SEQ || stored_prev != prev {
                // The first anomaly is the one worth citing — except under an
                // unknown format, where a digest that did not recompute is
                // expected and only the link is evidence.
                let first_bad_seq = if declared.is_unknown() {
                    seq
                } else {
                    first_bad.unwrap_or(seq)
                };
                return Ok(ChainStatus::Broken { first_bad_seq });
            }
            let stored: [u8; 32] = match <[u8; 32]>::try_from(&stored_hash[..]) {
                Ok(h) => h,
                // A hash of the wrong length breaks this row and every link
                // after it: there is nothing to carry forward.
                Err(_) => {
                    return Ok(ChainStatus::Broken {
                        first_bad_seq: first_bad.unwrap_or(seq),
                    });
                }
            };
            if seq == FORMAT_SEQ {
                // The marker's shape is COMPARED against the canonical one and
                // only then hashed. Hashing a substituted canonical record
                // would leave the row's real `op` outside the digest while
                // `parse_format` still read it — one column write, no rehash,
                // and a pristine journal starts reporting an unreadable format.
                let Ok(c) = &content else {
                    return Ok(ChainStatus::Broken { first_bad_seq: seq });
                };
                if !c.is_canonical_marker() || chain_hash(&prev, &c.record(seq)) != stored {
                    return Ok(ChainStatus::Broken { first_bad_seq: seq });
                }
                declared = parse_format(&c.op, &c.path);
                prev = stored;
                continue;
            }
            match &content {
                Ok(c) if chain_hash(&prev, &c.record(seq)) == stored => verified += 1,
                // Both a digest that does not match and a row that does not
                // decode mean the same thing here: this build cannot vouch for
                // this entry.
                _ => {
                    if first_bad.is_none() {
                        first_bad = Some(seq);
                    }
                }
            }
            prev = stored;
        }
        Ok(match (first_bad, declared.is_unknown()) {
            (None, false) => ChainStatus::Intact { entries: verified },
            (Some(first_bad_seq), false) => ChainStatus::Broken { first_bad_seq },
            // Everything recomputed, and the file still says it was written by
            // rules this build does not know. "Intact" would be a claim about
            // a format nobody here can read, so it is not made.
            (first_unverifiable_seq, true) => ChainStatus::UnknownFormat {
                declared,
                known: JOURNAL_FORMAT,
                first_unverifiable_seq,
            },
        })
    }

    /// Head de la cadena: `(seq, entry_hash)` de la última MUTACIÓN (`None`
    /// sin ninguna). Es lo que un ancla HMAC firma (ADR 0025).
    ///
    /// **Este `head` NO devuelve el marcador de formato (`seq 0`, ADR 0046), y
    /// sigue sin devolverlo a propósito**: el `seq` 0 tampoco sale por
    /// [`Journal::entries`], así que un ancla del HEAD que apuntara ahí la
    /// leería el audit como «el seq anclado ya no existe» — una acusación falsa
    /// de truncación.
    ///
    /// La cobertura que un ancla del head da al marcador es TRANSITIVA y con
    /// una salvedad: cada mutación encadena con el hash del marcador, así que
    /// re-declarar el formato y RECOMPUTAR la cola cambia todos los hashes
    /// almacenados y ningún ancla previa casa. Pero eso vale para quien pueda
    /// recomputar la cadena, y un verificador que ya ha dicho
    /// [`ChainStatus::UnknownFormat`] es justo el que no puede: para él el
    /// ancla del head no cubre el marcador. Una re-declaración que NO recomputa
    /// la cola (tres escrituras) no mueve ningún hash almacenado y las anclas
    /// del head siguen casando — ver [`Journal::verify_chain`].
    ///
    /// **Lo que sí lo cubre es un ancla PROPIA del marcador** (#146), con su
    /// digest tomado de [`Journal::marker_hash`] y en su propio fichero. Esa es
    /// la razón de que este método no haya tenido que cambiar: el ancla del
    /// marcador no pasa por aquí.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] si el hash
    /// almacenado no mide 32 bytes.
    pub async fn head(&self) -> Result<Option<(i64, [u8; 32])>, JournalError> {
        let row = sqlx::query(
            "SELECT seq, entry_hash FROM journal WHERE seq >= 1 ORDER BY seq DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let seq: i64 = row.try_get(0)?;
        let blob: Vec<u8> = row.try_get(1)?;
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("entry_hash del head no mide 32 bytes"))?;
        Ok(Some((seq, hash)))
    }

    /// Hash de la MUTACIÓN `seq` (`None` si no existe). El audit lo contrasta
    /// con cada ancla DESPUÉS de un [`Journal::verify_chain`] `Intact` (ADR
    /// 0025): con la cadena verificada, el hash almacenado ES el recomputado.
    ///
    /// El marcador de formato (`seq` 0) queda fuera, como en
    /// [`Journal::head`] y [`Journal::entries`], y una respuesta aquí para un
    /// `seq` que el resto del audit dice que no existe sería una incoherencia
    /// esperando a que alguien la use.
    ///
    /// **Su digest se pide por [`Journal::marker_hash`]**, que es la puerta
    /// estrecha que #146 abrió para anclarlo — no relajando esta. Las dos
    /// coexisten porque sirven a mecanismos distintos: las anclas del HEAD
    /// contrastan contra este filtro, y las del MARCADOR contra aquella, en su
    /// propio fichero.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] si el blob no mide
    /// 32 bytes.
    pub async fn entry_hash_at(&self, seq: i64) -> Result<Option<[u8; 32]>, JournalError> {
        let row = sqlx::query("SELECT entry_hash FROM journal WHERE seq = ? AND seq >= 1")
            .bind(seq)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let blob: Vec<u8> = row.try_get(0)?;
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("entry_hash no mide 32 bytes"))?;
        Ok(Some(hash))
    }

    /// Digest del MARCADOR DE FORMATO (`seq 0`, ADR 0046), o `None` si este
    /// journal no lo lleva (se creó antes de que el marcador existiera).
    ///
    /// Accesor propio, y no una relajación del filtro de
    /// [`Journal::entry_hash_at`]: ese filtro está puesto a mano porque una
    /// respuesta ahí contradiría a [`Journal::head`] y a [`Journal::entries`],
    /// que no devuelven el `seq` 0 — y un ancla no puede apuntar a un `seq` que
    /// el resto del audit dice que no existe. Lo que hace falta es lo
    /// contrario: UNA puerta, estrecha y con nombre, para lo único que sí
    /// quiere el marcador.
    ///
    /// # Por qué existe (#146)
    /// ADR 0046 concedía un agujero: re-declarar el formato cuesta TRES
    /// escrituras de columna y ninguna clave —poner la versión, refrescar el
    /// digest del marcador (que es keyless y públicamente computable), reencadenar
    /// el `seq 1`— y convierte un veredicto `Broken { first_bad_seq: k }` en
    /// `UnknownFormat`, con una versión de `u32::MAX` para que ningún binario
    /// futuro diga otra cosa. La alarma sobrevive; la CULPA no. Y las anclas de
    /// ADR 0025 no lo cierran, al contrario de lo que parece: la edición no
    /// mueve ningún `entry_hash` de `seq >= 1`, así que todas siguen casando.
    ///
    /// Con esto, `norte audit anchor` firma también el marcador y una
    /// re-declaración incoherente sale como
    /// [`AnchorVerdict::HashMismatch`](crate::audit::AnchorVerdict::HashMismatch)
    /// **en el `seq` 0** — localizada, y con una clave detrás.
    ///
    /// # Es el digest ALMACENADO, no uno recomputado
    /// Como [`Journal::entry_hash_at`], y con la misma condición para que
    /// signifique algo: contrástalo DESPUÉS de un [`Journal::verify_chain`] que
    /// haya validado el marcador — `Intact` o `UnknownFormat`, los dos brazos
    /// que solo se alcanzan si la fila del `seq` 0 es canónica y su hash
    /// recomputa. Bajo `Broken` este valor es lo que ponga el fichero.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] si el blob no mide
    /// 32 bytes.
    pub async fn marker_hash(&self) -> Result<Option<[u8; 32]>, JournalError> {
        let row = sqlx::query("SELECT entry_hash FROM journal WHERE seq = ?")
            .bind(FORMAT_SEQ)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let blob: Vec<u8> = row.try_get(0)?;
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("el entry_hash del marcador no mide 32 bytes"))?;
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
        let rows = sqlx::query(self.pick(SELECT_ENTRIES, SELECT_ENTRIES_NO_BATCH))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_entry).collect()
    }

    /// Una PÁGINA de entradas hacia atrás, de la más nueva a la más vieja
    /// (fase 7): las anteriores a `before_seq` —`None` = desde la última—,
    /// como mucho `limit`, y sólo las de `actor_kind` si se da uno.
    ///
    /// A diferencia de [`Self::entries`], que trae el journal ENTERO y es
    /// para auditar, esto es lo que lee una pantalla: acotado por
    /// construcción, porque un journal de meses no cabe en la memoria de
    /// nadie.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn page(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<Vec<PageEntry>, JournalError> {
        let sql = select_page(self.has_batch_id);
        let rows = sqlx::query(&sql)
            .bind(before_seq)
            .bind(actor_kind)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(PageEntry {
                    entry: row_to_entry(r)?,
                    undone: r.try_get::<bool, _>(12)?,
                })
            })
            .collect()
    }

    /// Como [`Self::revertible_for`], pero sólo lo POSTERIOR a `after_seq`
    /// (fase 7, base de [`crate::Engine::undo_after`]).
    ///
    /// La entrada `after_seq` NO entra: es el punto al que se quiere volver,
    /// no la primera víctima. `upto_seq` es el techo (0.80.0): nada por
    /// encima, ni ningún lote con una entrada por encima. `None` = sin techo.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn revertible_for_after(
        &self,
        actor: &Actor,
        after_seq: i64,
        upto_seq: Option<i64>,
    ) -> Result<Vec<JournalEntry>, JournalError> {
        let (actor_kind, actor_id) = actor.parts();
        let rows =
            sqlx::query(self.pick(SELECT_REVERTIBLE_AFTER, SELECT_REVERTIBLE_AFTER_NO_BATCH))
                .bind(actor_kind)
                .bind(actor_id)
                .bind(after_seq)
                .bind(upto_seq)
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(row_to_entry).collect()
    }

    /// Las entradas REVERTIBLES de la sesión `actor`, en orden LIFO (`seq`
    /// DESC): mutaciones normales (`undoes_seq IS NULL`) de ese actor cuya
    /// compensación no exista o haya sido a su vez desandada. Base de
    /// [`crate::Engine::undo_session`] (M3-2).
    ///
    /// «Compensada» significa compensada VIVA, no «compensada alguna vez»: un
    /// undo de lote que falla a mitad desanda sus propias compensaciones, y el
    /// lote tiene que volver a ser deshacible. El porqué, con la forma exacta
    /// de la cadena, está sobre la constante `SELECT_REVERTIBLE`.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn revertible_for(&self, actor: &Actor) -> Result<Vec<JournalEntry>, JournalError> {
        let (actor_kind, actor_id) = actor.parts();
        let rows = sqlx::query(self.pick(SELECT_REVERTIBLE, SELECT_REVERTIBLE_NO_BATCH))
            .bind(actor_kind)
            .bind(actor_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_entry).collect()
    }

    /// Cuáles de `seqs` están deshechas AHORA: tienen una compensación viva,
    /// con la misma condición (`COL_UNDONE`) que usa la selección de lo
    /// revertible.
    ///
    /// Es la re-comprobación de un undo JUSTO ANTES de ejecutar una unidad
    /// (#358): lo que se eligió al pedir el undo pudo deshacerlo, entretanto,
    /// otro undo — un doble clic, dos frontends, un reintento tras timeout.
    ///
    /// Pregunta por trozos: una unidad de sincronización puede tener medio
    /// millón de entradas, y `SQLite` pone techo a los parámetros de una
    /// consulta.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn undone_among(&self, seqs: &[i64]) -> Result<Vec<i64>, JournalError> {
        const TROZO: usize = 500;
        let mut deshechas = Vec::new();
        for trozo in seqs.chunks(TROZO) {
            let huecos = vec!["?"; trozo.len()].join(",");
            let sql = format!("SELECT seq FROM journal WHERE seq IN ({huecos}) AND {COL_UNDONE}");
            let mut q = sqlx::query_scalar::<_, i64>(&sql);
            for s in trozo {
                q = q.bind(s);
            }
            deshechas.extend(q.fetch_all(&self.pool).await?);
        }
        Ok(deshechas)
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

    /// TESTS ONLY: rewrites the version the format marker declares.
    ///
    /// With `rehash` it also recomputes the marker's hash, which is what a
    /// GENUINE writer of that format would have left behind; without it, the
    /// row is simply altered, which is what an attacker leaves behind.
    #[cfg(test)]
    async fn redeclare_format_for_test(
        &self,
        version: &[u8],
        rehash: bool,
    ) -> Result<(), JournalError> {
        let ts_ms: i64 = sqlx::query("SELECT ts_ms FROM journal WHERE seq = 0")
            .fetch_one(&self.pool)
            .await?
            .try_get(0)?;
        let hash = chain_hash(&[0u8; 32], &format_record(ts_ms, version));
        if rehash {
            sqlx::query("UPDATE journal SET path = ?, entry_hash = ? WHERE seq = 0")
                .bind(version)
                .bind(&hash[..])
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query("UPDATE journal SET path = ? WHERE seq = 0")
                .bind(version)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// TESTS ONLY: forges a marker row with a non-canonical `actor_kind`, and
    /// gives it a hash that is self-consistent OVER THE ROW'S OWN FIELDS — what
    /// an attacker who read `chain_hash` would produce.
    #[cfg(test)]
    async fn forge_marker_shape_for_test(&self, actor_kind: &str) -> Result<(), JournalError> {
        let ts_ms: i64 = sqlx::query("SELECT ts_ms FROM journal WHERE seq = 0")
            .fetch_one(&self.pool)
            .await?
            .try_get(0)?;
        let mut rec = format_record(ts_ms, b"1");
        rec.actor_kind = actor_kind;
        let hash = chain_hash(&[0u8; 32], &rec);
        sqlx::query("UPDATE journal SET actor_kind = ?, entry_hash = ? WHERE seq = 0")
            .bind(actor_kind)
            .bind(&hash[..])
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// TESTS ONLY: relinks `seq 1` onto whatever the marker now hashes to, the
    /// way a writer of the newer format would have chained it.
    #[cfg(test)]
    async fn relink_first_entry_for_test(&self) -> Result<(), JournalError> {
        let head0: Vec<u8> = sqlx::query("SELECT entry_hash FROM journal WHERE seq = 0")
            .fetch_one(&self.pool)
            .await?
            .try_get(0)?;
        sqlx::query("UPDATE journal SET prev_hash = ? WHERE seq = 1")
            .bind(head0)
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

    /// Ver [`Journal::set_hook_sender`].
    pub fn set_hook_sender(&self, tx: crate::hooks::HookSender) {
        self.journal.set_hook_sender(tx);
    }

    /// Cierra el journal subyacente y espera a que el fichero quede libre
    /// (ver [`Journal::close`]).
    pub async fn close(self) {
        self.journal.close().await;
    }

    /// Abre (o crea) el journal en `path` y lo envuelve como observer, listo
    /// para [`crate::Engine::with_observer`]. Crea el directorio contenedor si
    /// falta.
    ///
    /// UN SOLO ESCRITOR (spec §4): el hash-chain asume un único proceso dueño.
    /// Varios procesos escribiendo el MISMO fichero forkearían la cadena y
    /// colisionarían en `seq`, y por eso no es una convención: [`Journal::open`]
    /// toma el lock EXCLUSIVO de `SQLite`, así que el segundo en llegar falla al
    /// abrir en vez de compartir.
    ///
    /// Quién es ese dueño ya no es siempre el daemon: desde #167 un proceso
    /// embebido (TUI, o un `norte cp` sin daemon) abre este mismo fichero — ver
    /// [`crate::embedded::LazyJournal`], que es quien decide qué hacer cuando
    /// el lock ya lo tiene otro, que desde #177 no lo abre hasta la primera
    /// mutación (así, una sesión que solo navega no se lo quita a nadie) y que
    /// desde #179 lo reintenta y sabe soltarlo.
    ///
    /// # Errors
    /// [`JournalError::Io`] si no puede crear el directorio contenedor;
    /// [`JournalError`] al abrir/crear la DB (ver [`Journal::open`]).
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT).await
    }

    /// Como [`SqliteJournal::open`], con el plazo de espera del lock de
    /// [`Journal::open_with_busy_timeout`].
    ///
    /// # Errors
    /// Las mismas que [`SqliteJournal::open`].
    pub async fn open_with_busy_timeout(
        path: &std::path::Path,
        busy_timeout: std::time::Duration,
    ) -> Result<Self, JournalError> {
        // El dir de config puede no existir en el primer arranque; SQLite crea
        // el FICHERO (create_if_missing) pero no su directorio padre.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(Self::new(
            Journal::open_with_busy_timeout(path, busy_timeout).await?,
        ))
    }
}

/// Cómo se escribe la identidad de un nodo en `reversal_ref` (ADR 0152).
///
/// Texto y no bytes crudos porque el volcado de una fila con `sqlite3` durante
/// una investigación enseña `12:34567` y no cuatro bytes opacos. No sale por
/// el wire ni por la exportación de auditoría, así que nadie más lo lee.
///
/// Vive aquí, en pareja con [`huella_a_nodo`], para que quien escribe y quien
/// compara no puedan divergir: el día que esto cambie de forma, cambia en un
/// sitio y el parser de al lado lo acompaña.
pub(crate) fn huella_de_nodo(n: &norte_vfs::NodeId) -> String {
    format!("{}:{}", n.volume, n.index)
}

/// La inversa de [`huella_de_nodo`]. `None` = esos bytes no son una huella.
///
/// La comparación del deshacer va por [`norte_vfs::NodeId`] y no por bytes
/// justamente por este `None`. Hoy nada puede dejar otra cosa en el
/// `reversal_ref` de un `delete` —los otros escritores ponen `None`, y el
/// `restore_trash`, que sí guarda una ruta ahí, se atiende en otro brazo—,
/// pero comparar cadenas hace que el día que algo la deje, esa entrada no
/// coincida NUNCA y se bloquee para siempre. Parseando, un valor que no es
/// una huella cae en «no hay nada que comparar» y el deshacer se comporta
/// como antes de ADR 0152, que es la dirección en la que esto tiene que
/// fallar.
pub(crate) fn huella_a_nodo(bytes: &[u8]) -> Option<norte_vfs::NodeId> {
    let texto = std::str::from_utf8(bytes).ok()?;
    let (vol, idx) = texto.split_once(':')?;
    Some(norte_vfs::NodeId {
        volume: vol.parse().ok()?,
        index: idx.parse().ok()?,
    })
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
            // `reversal_ref` lleva aquí la IDENTIDAD de lo creado, no una ruta
            // (#369, ADR 0152). La columna es libre y cada reversa le da su
            // sentido: `restore_trash` guarda el destino recuperable, y
            // `delete` guarda qué nodo era el suyo para no borrar otro.
            Mutation::Created { path, node } => (
                "created",
                path.to_wire().into_bytes(),
                None,
                Reversal::Delete,
                node.map(|n| huella_de_nodo(&n).into_bytes()),
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
            // #314: la reversa ES el modo anterior, y va en `reversal_ref` en
            // ASCII decimal. Sin él no hay vuelta atrás que prometer, y la
            // entrada lo dice —`Irreversible` con su motivo— en vez de ofrecer
            // un undo que pondría un modo que nadie tuvo. El modo NUEVO va en
            // `path_to` para que el diario se pueda leer sin adivinar qué se
            // puso.
            Mutation::ModeChanged {
                path,
                from,
                to,
                batch,
            } => (
                "mode_changed",
                path.to_wire().into_bytes(),
                Some(to.to_string().into_bytes()),
                if from.is_some() {
                    Reversal::SetModeBack
                } else {
                    Reversal::Irreversible
                },
                from.map(|m| m.to_string().into_bytes()),
                // El lote de un recursivo (#315): n entradas que fueron UNA
                // acción del humano.
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

    /// ADR 0100: cada fila comprometida se le ofrece al extremo de los hooks,
    /// con lo que la fila dice — y solo tras el insert, con su `seq`.
    #[tokio::test]
    async fn cada_fila_comprometida_se_ofrece_a_los_hooks() {
        let j = Journal::open_in_memory().await.expect("open");
        let (tx, mut rx) = crate::hooks::HookSender::for_test(2);
        j.set_hook_sender(tx.clone());
        let actor = Actor::Agent {
            session: "s-1".into(),
        };
        let seq = j
            .record_entry(&NewEntry {
                op: "renamed",
                path: b"file:///a/nuevo",
                path_to: Some(b"file:///a/viejo"),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &actor,
                undoes_seq: None,
                batch_id: Some(3),
            })
            .await
            .expect("record");
        let ev = rx.try_recv().expect("un evento por fila");
        assert_eq!(ev.seq, seq);
        assert_eq!(ev.op, "renamed");
        assert_eq!(ev.actor_kind, "agent", "la clase sí; la sesión no viaja");
        assert_eq!(ev.path, b"file:///a/nuevo".to_vec());
        assert_eq!(ev.path_to, Some(b"file:///a/viejo".to_vec()));
        assert_eq!(ev.batch_id, Some(3));

        // Cola llena: la fila se escribe igual y el evento se cuenta como
        // descartado. Un observador lento jamás frena una mutación.
        for _ in 0..3 {
            j.record(
                "created",
                b"file:///a/x",
                None,
                Reversal::Delete,
                None,
                &actor,
            )
            .await
            .expect("record");
        }
        assert_eq!(j.count().await.expect("count"), 4);
        assert_eq!(tx.dropped(), 1, "dos cupieron, el tercero se descartó");
    }

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
                obs.on_mutation(&Mutation::creado(&p), &Actor::User).await
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
        j.on_mutation(&Mutation::creado(&victim), &Actor::User)
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

    /// Una compensación DESANDADA no tapa a su original: la mutación vuelve a
    /// ser revertible.
    ///
    /// Es la forma que produce el undo de un lote cuando falla a mitad: el
    /// ejecutor desanda los pasos de undo que ya había aplicado y journaliza
    /// esa vuelta como compensación de la compensación (`O ← C ← D`). El árbol
    /// queda con el lote aplicado, así que decir «ya está deshecho» lo dejaría
    /// indeshacible para siempre y en silencio.
    #[tokio::test]
    async fn a_compensation_that_was_itself_undone_reopens_its_entry() {
        let j = Journal::open_in_memory().await.expect("open");
        // seq 1: la mutación original.
        j.record(
            "renamed",
            b"file:///x",
            Some(b"file:///a"),
            Reversal::RenameBack,
            None,
            &Actor::User,
        )
        .await
        .expect("1");
        // seq 2: su compensación → la 1 deja de ser revertible.
        let comp = j
            .record_undoing(
                "renamed",
                b"file:///a",
                Some(b"file:///x"),
                Reversal::RenameBack,
                None,
                &Actor::User,
                Some(1),
            )
            .await
            .expect("2");
        assert!(
            j.revertible_for(&Actor::User)
                .await
                .expect("revertible")
                .is_empty(),
        );
        // seq 3: la compensación se DESANDA (el undo del lote se cayó y el
        // ejecutor la devolvió) → la 1 vuelve a estar pendiente.
        j.record_undoing(
            "renamed",
            b"file:///x",
            Some(b"file:///a"),
            Reversal::RenameBack,
            None,
            &Actor::User,
            Some(comp),
        )
        .await
        .expect("3");
        let rev = j.revertible_for(&Actor::User).await.expect("revertible");
        assert_eq!(
            rev.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1],
            "la compensación ya no vale, así que la 1 sigue por deshacer",
        );
        // Y la re-comprobación del undo (#358) lee la MISMA condición: la 1 no
        // está deshecha. Si divergieran, un undo en marcha se saltaría lo que
        // la selección acaba de ofrecer, o al revés.
        assert!(
            j.undone_among(&[1]).await.expect("undone").is_empty(),
            "desandada la compensación, la 1 no cuenta como deshecha"
        );
    }

    /// `undone_among` dice cuáles de las pedidas tienen una compensación VIVA,
    /// y pregunta por trozos: con más seqs que el trozo, sigue contestando
    /// entero.
    #[tokio::test]
    async fn undone_among_devuelve_las_compensadas_vivas_y_cruza_los_trozos() {
        let j = Journal::open_in_memory().await.expect("open");
        for i in 0..3 {
            j.record(
                "created",
                format!("file:///f{i}").as_bytes(),
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("mutación");
        }
        // seq 4 compensa la 2.
        j.record_undoing(
            "removed",
            b"file:///f1",
            None,
            Reversal::Irreversible,
            None,
            &Actor::User,
            Some(2),
        )
        .await
        .expect("compensación");
        assert_eq!(j.undone_among(&[1, 2, 3]).await.expect("undone"), vec![2]);
        // Más de un trozo (500), con la compensada al final: la que importa
        // no se pierde en la costura.
        let mut muchas: Vec<i64> = (10_000..10_600).collect();
        muchas.push(2);
        assert_eq!(j.undone_among(&muchas).await.expect("undone"), vec![2]);
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

    /// El schema de ANTES de que existiera `undoes_seq` (la columna que apunta a
    /// la entrada que un undo compensa, M3-2). Es más viejo que
    /// [`SCHEMA_BEFORE_BATCH_ID`], y hay ficheros así en disco.
    const SCHEMA_BEFORE_UNDOES_SEQ: &str = "\
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

    /// Un journal anterior a `undoes_seq` se MIGRA al abrir, como el de
    /// `batch_id`.
    ///
    /// Sin esto, `open` tiene éxito —`CREATE TABLE IF NOT EXISTS` no altera una
    /// tabla que ya existe— y es cada ESCRITURA la que revienta con «table
    /// journal has no column named `undoes_seq`». Encontrado en vivo (#167): un
    /// `norte cp` embebido contra un journal de esa era abortaba con «internal
    /// error» y no copiaba nada. `batch_id` ya tenía su migración; esta columna
    /// se añadió sin la suya.
    #[tokio::test]
    async fn un_journal_anterior_a_undoes_seq_se_migra_y_acepta_escrituras() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prehistorico.db");
        {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(&path)
                        .create_if_missing(true)
                        .journal_mode(SqliteJournalMode::Wal),
                )
                .await
                .expect("pool viejo");
            sqlx::query(SCHEMA_BEFORE_UNDOES_SEQ)
                .execute(&pool)
                .await
                .expect("schema prehistórico");
            pool.close().await;
        }

        let j = Journal::open(&path).await.expect("open migra la DB");
        j.record(
            "created",
            b"file:///nuevo",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("escribir en una DB migrada");
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "migrar no rompe la cadena"
        );
    }

    /// El lock exclusivo es de LA CONEXIÓN, así que el pool no puede reciclarla.
    ///
    /// Los defaults de `sqlx` —`min_connections=0`, `idle_timeout=10min`,
    /// `max_lifetime=30min`— levantan un barrendero que cierra la conexión
    /// ociosa, y con ella se va el lock: el proceso se sigue creyendo dueño, otro
    /// entra, y el `ChainState` en memoria de éste choca contra la PK de `seq`
    /// en su siguiente mutación… y en todas las demás. Se pinea por las opciones
    /// y no por el reloj: esperar diez minutos en la suite no es un test.
    #[tokio::test]
    async fn el_pool_no_recicla_la_conexion_que_sostiene_el_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::open(&dir.path().join("j.db")).await.expect("open");
        let opts = j.pool.options();
        assert_eq!(opts.get_max_connections(), 1, "un solo escritor");
        assert_eq!(
            opts.get_min_connections(),
            1,
            "a cero, el barrendero puede dejar el pool vacío y soltar el lock"
        );
        assert_eq!(opts.get_idle_timeout(), None, "ocioso sigue siendo dueño");
        assert_eq!(
            opts.get_max_lifetime(),
            None,
            "reciclar la conexión es reciclar el lock"
        );
    }

    /// Un journal anterior a `undoes_seq` CON historia NO se migra: se rehúsa.
    ///
    /// Migrarlo lo dejaría escribible y `verify_chain` lo declararía roto en su
    /// primera fila, porque esas filas se hashearon sobre un preimagen que no
    /// llevaba la columna (`01e3cf8` añadió las dos cosas a la vez). Una
    /// acusación FALSA de manipulación sobre un fichero que nadie tocó, y sin
    /// arreglo: esas filas ya no se pueden rehashear.
    #[tokio::test]
    async fn un_journal_anterior_a_undoes_seq_con_filas_se_rehusa() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("con-historia.db");
        {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(&path)
                        .create_if_missing(true)
                        .journal_mode(SqliteJournalMode::Wal),
                )
                .await
                .expect("pool viejo");
            sqlx::query(SCHEMA_BEFORE_UNDOES_SEQ)
                .execute(&pool)
                .await
                .expect("schema prehistórico");
            sqlx::query(
                "INSERT INTO journal (seq, ts_ms, actor_kind, actor_id, op, path, path_to, \
                 reversal, reversal_ref, prev_hash, entry_hash) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(1i64)
            .bind(1_700_000_000_000i64)
            .bind("user")
            .bind(Option::<String>::None)
            .bind("created")
            .bind(&b"file:///viejo"[..])
            .bind(Option::<Vec<u8>>::None)
            .bind("delete")
            .bind(Option::<Vec<u8>>::None)
            .bind(&[0u8; 32][..])
            .bind(&[7u8; 32][..])
            .execute(&pool)
            .await
            .expect("fila de la era pre-undoes_seq");
            pool.close().await;
        }

        let Err(err) = Journal::open(&path).await else {
            panic!("una DB pre-undoes_seq CON filas no se puede migrar")
        };
        assert!(
            matches!(err, JournalError::Corrupt(m) if m.contains("undoes_seq")),
            "y se dice por qué: {err}"
        );

        // Y el audit tampoco lo lee a ciegas: dice qué es, en vez de soltar un
        // «no such column» crudo.
        let Err(ro) = Journal::open_read_only(&path).await else {
            panic!("el audit tampoco puede leerla")
        };
        assert!(
            matches!(ro, JournalError::Corrupt(m) if m.contains("undoes_seq")),
            "{ro}"
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

    /// El handle de solo-lectura (audit) lee un lote REAL de la DB y no
    /// entrega ids ya usados: su contador arranca del máximo escrito.
    #[tokio::test]
    async fn read_only_reads_a_real_batch_and_does_not_reuse_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lote.db");
        let batch;
        {
            let j = Journal::open(&path).await.expect("open rw");
            batch = j.alloc_batch().await.expect("alloc");
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
        }
        let ro = Journal::open_read_only(&path).await.expect("open ro");
        assert_eq!(
            ro.entries().await.expect("entries")[0].batch_id,
            Some(batch)
        );
        assert!(
            ro.verify_chain().await.expect("verify").is_intact(),
            "la cadena con lote verifica igual en solo-lectura"
        );
        assert!(
            ro.alloc_batch().await.expect("alloc") > batch,
            "jamás un id que ya está en disco"
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
        // Un lote NO cruza la frontera de actor: quien deshaga por grupo tiene
        // que seguir filtrando por actor, jamás solo por `batch_id`.
        assert!(
            j.revertible_for(&Actor::Agent {
                session: "s1".into()
            })
            .await
            .expect("revertible agente")
            .is_empty(),
            "el lote es del usuario, no del agente"
        );
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

    // ---------------------------------------------------------------------
    // The format marker (#127, ADR 0046).
    // ---------------------------------------------------------------------

    /// FROZEN VECTOR of the marker's preimage. Every claim in ADR 0046 rests
    /// on this digest never moving: it is what a binary built before the
    /// marker existed computes for that row, which is why writing the marker
    /// does not turn a downgrade into an accusation, and it is what a binary
    /// built after any future format bump must still compute in order to read
    /// the version that tells it to stop accusing.
    ///
    /// If this goes red you have changed the marker's preimage. Do not update
    /// the constant: every journal already on disk declares its format through
    /// this exact byte string, and moving it makes them all unreadable at
    /// exactly the moment they need to be readable.
    #[test]
    fn the_format_marker_preimage_is_frozen() {
        let r = format_record(1_726_000_000_000, b"1");
        assert_eq!(r.seq, 0, "the marker sits at the reserved seq");
        assert_eq!(
            r.batch_id, None,
            "and feeds nothing a pre-batch build lacks"
        );
        assert_eq!(
            crate::hashing::hex_lower(&chain_hash(&[0u8; 32], &r)),
            "9901556ad24053ecc2fb19321100309093df214f3ef38404109504c2daec0ff8",
        );
    }

    /// A journal created now says so, and says it inside the chain.
    #[tokio::test]
    async fn a_new_journal_declares_its_format() {
        let j = Journal::open_in_memory().await.expect("open");
        assert_eq!(j.format().await.expect("format"), JournalFormat::Version(1));
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Intact { entries: 0 },
            "the marker is chained, but it is not history"
        );
    }

    /// The marker is METADATA: it must not show up as a mutation anywhere, or
    /// the audit export gains a row that never happened and the undo gains a
    /// step it cannot take.
    #[tokio::test]
    async fn the_marker_is_not_a_mutation() {
        let j = Journal::open_in_memory().await.expect("open");
        assert_eq!(j.count().await.expect("count"), 0);
        assert!(j.entries().await.expect("entries").is_empty());
        assert!(
            j.revertible_for(&Actor::User)
                .await
                .expect("revertible")
                .is_empty()
        );
        assert_eq!(j.head().await.expect("head"), None, "nothing to anchor yet");

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
        assert_eq!(seq, 1, "mutations still start at 1");
        assert_eq!(j.count().await.expect("count"), 1);
        assert_eq!(j.entries().await.expect("entries").len(), 1);
        assert_eq!(
            j.head().await.expect("head").map(|(s, _)| s),
            Some(1),
            "the head an anchor signs is the last MUTATION"
        );
    }

    /// Reopening does not stamp a second marker, and the chain still verifies.
    #[tokio::test]
    async fn reopening_does_not_stamp_a_second_marker() {
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
            .expect("record");
        }
        let j = Journal::open(&path).await.expect("reopen");
        let markers: i64 = sqlx::query("SELECT COUNT(*) FROM journal WHERE seq = 0")
            .fetch_one(&j.pool)
            .await
            .expect("count markers")
            .get(0);
        assert_eq!(markers, 1);
        assert_eq!(j.format().await.expect("format"), JournalFormat::Version(1));
        assert!(j.verify_chain().await.expect("verify").is_intact());
    }

    /// Walks the chain the way a binary built BEFORE the marker existed does:
    /// every row is an ordinary entry, `seq 0` included, and nothing is known
    /// about formats. This is the released code's algorithm, kept here as the
    /// only way to test the claim it makes.
    ///
    /// It also covers a build older than `batch_id` — the marker's `batch_id`
    /// is `None`, which feeds nothing, so both eras compute the same digest —
    /// but only because `the_batch_id_framing_is_frozen` pins that. And it
    /// reuses today's `chain_hash`, so if a future format changes it, this
    /// helper changes with it and quietly stops simulating anything: what keeps
    /// the claim honest then is `the_format_marker_preimage_is_frozen`.
    async fn verifies_like_a_binary_without_the_marker(j: &Journal) -> bool {
        let rows = sqlx::query(SELECT_VERIFY)
            .fetch_all(&j.pool)
            .await
            .expect("rows");
        let mut prev = [0u8; 32];
        for row in rows {
            let actor_kind: String = row.get(2);
            let actor_id: Option<String> = row.get(3);
            let op: String = row.get(4);
            let path: Vec<u8> = row.get(5);
            let path_to: Option<Vec<u8>> = row.get(6);
            let reversal: String = row.get(7);
            let reversal_ref: Option<Vec<u8>> = row.get(8);
            let stored_prev: Vec<u8> = row.get(10);
            let stored_hash: Vec<u8> = row.get(11);
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
                undoes_seq: row.get(9),
                batch_id: row.get(12),
            };
            let computed = chain_hash(&prev, &rec);
            if stored_prev != prev || computed[..] != stored_hash[..] {
                return false;
            }
            prev = computed;
        }
        true
    }

    /// Adding the marker must not create the very problem it is here to fix:
    /// an older binary, which knows nothing about `seq 0`, hashes it as an
    /// ordinary row and finds it intact.
    #[tokio::test]
    async fn an_older_binary_reads_the_marker_as_an_ordinary_intact_entry() {
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
        .expect("record");
        assert!(
            verifies_like_a_binary_without_the_marker(&j).await,
            "the marker is hashed with fields that existed before it did",
        );
    }

    /// A journal written by a NEWER format must not read as tampering. The
    /// difference matters more than it looks: `Broken` is an accusation, and
    /// making it at a file nobody touched teaches a user to ignore the one
    /// signal the journal exists to give.
    ///
    /// What is staged, precisely, because the difference matters to whoever
    /// reads this next: a marker declaring version 2 and hashed with the frozen
    /// preimage (the one thing every version shares), and `seq 1` relinked onto
    /// it so the chain is well formed. That leaves `seq 1`'s stored digest
    /// stale, which is the SAME observable a real format-2 preimage would
    /// produce — a digest that does not recompute here — without this test
    /// having to invent a format-2 hash function.
    #[tokio::test]
    async fn a_newer_format_is_reported_as_a_newer_format_not_as_tampering() {
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
        .expect("record");
        j.redeclare_format_for_test(b"2", true)
            .await
            .expect("declare 2");
        j.relink_first_entry_for_test().await.expect("relink");

        let status = j.verify_chain().await.expect("verify");
        assert_eq!(
            status,
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Version(2),
                known: JOURNAL_FORMAT,
                first_unverifiable_seq: Some(1),
            },
            "not Broken: this build cannot recompute what format 2 hashes",
        );
        assert!(!status.is_intact(), "and it is not a clean bill of health");
        // Unreadable is not unopenable: the entries are still there to export.
        assert_eq!(j.entries().await.expect("entries").len(), 1);
    }

    /// **#146: anclar el marcador cierra el agujero que ADR 0046 concedía.**
    ///
    /// El ataque son tres escrituras de columna y ninguna clave: poner la
    /// versión, refrescar el digest del marcador (keyless y públicamente
    /// computable) y reencadenar el `seq 1`. Convierte un
    /// `Broken { first_bad_seq: k }` en `UnknownFormat`, y con `u32::MAX` de
    /// versión ningún binario futuro dirá otra cosa: la alarma sobrevive, la
    /// CULPA no.
    ///
    /// Las anclas del HEAD no lo cazan, y esta es la mitad del test que importa
    /// — se comprueba explícitamente abajo: la edición no mueve ningún
    /// `entry_hash` de `seq >= 1`, así que el ancla del head SIGUE casando. Es
    /// el hueco entre los dos mecanismos: `verify_chain` caza ediciones
    /// incoherentes, las anclas cazan las coherentes, y esta era una
    /// incoherente a la que le habían desviado el veredicto.
    #[tokio::test]
    async fn una_redeclaracion_del_formato_rompe_el_ancla_del_marcador() {
        use crate::audit::{Anchor, AnchorVerdict, anchor_line, verify_anchor_line};

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
        .expect("record");

        // `norte audit anchor`: el marcador Y el head.
        let key = b"clave de anclado del test";
        let marcador = j
            .marker_hash()
            .await
            .expect("marker_hash")
            .expect("un journal nuevo lleva marcador");
        let ancla_marcador = anchor_line(
            key,
            &Anchor {
                seq: 0,
                head: marcador,
            },
        );
        let (seq, head) = j.head().await.expect("head").expect("hay una mutación");
        let ancla_head = anchor_line(key, &Anchor { seq, head });

        // ANTES del ataque: el ancla del marcador CASA. Es la mitad que un
        // regresor silencioso rompería —basta con dejar de sembrar el `seq` 0
        // en el snapshot del audit— y a partir de ahí TODO journal anclado
        // empezaría a decir «truncación» sobre un fichero intacto.
        assert!(
            matches!(
                verify_anchor_line(key, &ancla_marcador, j.marker_hash().await.expect("marker")),
                AnchorVerdict::Ok(_)
            ),
            "un journal sin tocar no se acusa a sí mismo",
        );

        // El ataque, entero.
        j.redeclare_format_for_test(&u32::MAX.to_string().into_bytes(), true)
            .await
            .expect("re-declarar");
        j.relink_first_entry_for_test().await.expect("relink");

        // El veredicto de la cadena, desviado: ya no acusa a nadie.
        assert!(
            matches!(
                j.verify_chain().await.expect("verify"),
                ChainStatus::UnknownFormat { .. }
            ),
            "el desvío del veredicto es la premisa del ataque",
        );

        // Y el ancla del HEAD sigue casando, que es justo lo que hacía que
        // «las anclas ya lo cubren» sonara verdad y no lo fuera.
        assert!(
            matches!(
                verify_anchor_line(
                    key,
                    &ancla_head,
                    j.entry_hash_at(seq).await.expect("hash del head"),
                ),
                AnchorVerdict::Ok(_)
            ),
            "la edición no mueve ningún entry_hash de seq >= 1",
        );

        // La del marcador, no. Localizada en el `seq` 0 y con una clave detrás.
        let ahora = j.marker_hash().await.expect("marker_hash");
        assert_eq!(
            verify_anchor_line(key, &ancla_marcador, ahora),
            AnchorVerdict::HashMismatch(Anchor {
                seq: 0,
                head: marcador,
            }),
            "una re-declaración incoherente es HashMismatch en el seq 0",
        );
    }

    /// **El agujero que ESTO no cierra, escrito en código y no solo en prosa.**
    ///
    /// Un journal ANTERIOR a ADR 0046 no tiene marcador —y por diseño no lo
    /// gana nunca (§6)—, así que cuando se ancló no había nada del `seq` 0 que
    /// firmar. El ataque ahí no es re-declarar sino INYECTAR: meter la fila del
    /// `seq` 0 y reencadenar el `seq` 1. El veredicto se desvía igual, y no hay
    /// ancla previa que lo contradiga.
    ///
    /// Lo que sí queda es una señal, y el audit la dice: hay marcador y nadie
    /// lo ancla.
    #[tokio::test]
    async fn un_journal_sin_marcador_no_tiene_ancla_que_lo_defienda() {
        let j = Journal::open_in_memory().await.expect("open");
        // Se le quita el marcador, que es como nacieron los journals de antes
        // de ADR 0046.
        sqlx::query("DELETE FROM journal WHERE seq = 0")
            .execute(&j.pool)
            .await
            .expect("borrar el marcador");
        assert_eq!(
            j.marker_hash().await.expect("marker_hash"),
            None,
            "sin marcador no hay digest que anclar, y el audit no escribe línea"
        );
    }

    /// Y el marcador anclado NO se cuela en la historia: `entry_hash_at` sigue
    /// filtrando `seq >= 1`, porque una respuesta ahí contradiría a `head` y a
    /// `entries`, y un ancla no puede apuntar a un `seq` que el resto del audit
    /// dice que no existe.
    #[tokio::test]
    async fn el_marcador_tiene_puerta_propia_y_no_relaja_la_de_las_mutaciones() {
        let j = Journal::open_in_memory().await.expect("open");
        assert!(
            j.marker_hash().await.expect("marker_hash").is_some(),
            "su puerta contesta"
        );
        assert_eq!(
            j.entry_hash_at(0).await.expect("entry_hash_at"),
            None,
            "y la de las mutaciones sigue sin contestar por el seq 0"
        );
        assert_eq!(j.head().await.expect("head"), None, "ni head");
        assert!(j.entries().await.expect("entries").is_empty(), "ni entries");
    }

    /// Same refusal when the marker is present but its version is not a number
    /// this build understands. Here everything recomputes, and the answer is
    /// still not `Intact`: certifying a format nobody here can read would be a
    /// claim about rules this binary does not have.
    #[tokio::test]
    async fn an_unreadable_marker_is_refused_rather_than_guessed() {
        let j = Journal::open_in_memory().await.expect("open");
        j.redeclare_format_for_test(b"2.0-beta", true)
            .await
            .expect("declare");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Unreadable,
                known: JOURNAL_FORMAT,
                first_unverifiable_seq: None,
            },
        );
        assert_eq!(j.format().await.expect("format"), JournalFormat::Unreadable);
    }

    /// The format entry is INSIDE the chain, so altering it breaks the chain
    /// like any other entry — which is the whole reason it is not a pragma.
    #[tokio::test]
    async fn tampering_with_the_format_entry_breaks_the_chain() {
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
        .expect("record");
        // Four bytes in the SQLite header would have been invisible. Four bytes
        // here are not.
        j.redeclare_format_for_test(b"999", false)
            .await
            .expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 0 },
            "the marker's own hash is checked before its version is believed",
        );
    }

    /// And re-hashing the marker does not launder the tampering into a shrug:
    /// the entry behind it no longer links, and a broken link is an accusation
    /// this binary is entitled to make about any format.
    #[tokio::test]
    async fn redeclaring_the_format_does_not_launder_a_broken_link() {
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
        .expect("record");
        j.redeclare_format_for_test(b"999", true)
            .await
            .expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 1 },
            "the link is format-independent, so the break is still reported",
        );
    }

    /// A journal declaring a format this build DOES know is verified exactly as
    /// before — the marker buys nobody an exemption.
    #[tokio::test]
    async fn a_known_format_still_reports_tampering_as_tampering() {
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
        j.corrupt_path_for_test(seq, b"file:///HACKED")
            .await
            .expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: seq },
        );
    }

    /// Under an unknown format the walk keeps checking the LINKS, which need no
    /// preimage — so a deletion in a journal from the future is still named,
    /// and named where it happened rather than at the first row this build
    /// could not recompute.
    #[tokio::test]
    async fn a_deletion_is_still_reported_in_a_journal_from_the_future() {
        let j = Journal::open_in_memory().await.expect("open");
        for w in [&b"file:///a"[..], b"file:///b", b"file:///c"] {
            j.record("created", w, None, Reversal::Delete, None, &Actor::User)
                .await
                .expect("record");
        }
        j.redeclare_format_for_test(b"2", true)
            .await
            .expect("declare 2");
        j.relink_first_entry_for_test().await.expect("relink");
        // The chain now reads as "written by format 2" from seq 1 on. An
        // attacker removes the middle entry.
        sqlx::query("DELETE FROM journal WHERE seq = 2")
            .execute(&j.pool)
            .await
            .expect("delete");

        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 3 },
            "seq 3 links to a digest no row carries, and that is format-independent",
        );
    }

    /// The marker's SHAPE is verified, not just its version: only the version
    /// is the row's to choose. A row at `seq 0` wearing a different
    /// `actor_kind` — the one row the mutation readers never show — does not
    /// pass just because its hash was refreshed over its own fields.
    #[tokio::test]
    async fn a_marker_with_a_forged_shape_does_not_verify() {
        let j = Journal::open_in_memory().await.expect("open");
        j.forge_marker_shape_for_test("user").await.expect("forge");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 0 },
        );
    }

    /// `seq 0` is the floor. Anything below it would be chained and certified
    /// while being invisible to `entries`, `count` and the audit export.
    #[tokio::test]
    async fn a_row_below_the_reserved_seq_breaks_the_chain() {
        let j = Journal::open_in_memory().await.expect("open");
        let rec = Record {
            seq: -1,
            ..format_record(1, b"1")
        };
        let hash = chain_hash(&[0u8; 32], &rec);
        sqlx::query(INSERT_ENTRY)
            .bind(rec.seq)
            .bind(rec.ts_ms)
            .bind(rec.actor_kind)
            .bind(rec.actor_id)
            .bind(rec.op)
            .bind(rec.path)
            .bind(rec.path_to)
            .bind(rec.reversal)
            .bind(rec.reversal_ref)
            .bind(rec.undoes_seq)
            .bind(rec.batch_id)
            .bind(&[0u8; 32][..])
            .bind(&hash[..])
            .execute(&j.pool)
            .await
            .expect("insert");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: -1 },
        );
        assert!(j.entries().await.expect("entries").is_empty());
    }

    /// A version is one byte string or the frozen vector means nothing: `+1`
    /// and `0001` are not version 1, they are markers this build will not read.
    /// Nor is `0` a version anyone ever wrote.
    #[test]
    fn a_version_has_exactly_one_spelling() {
        assert_eq!(parse_format(FORMAT_OP, b"1"), JournalFormat::Version(1));
        for odd in [&b"+1"[..], b"0001", b" 1", b"1 ", b""] {
            assert_eq!(
                parse_format(FORMAT_OP, odd),
                JournalFormat::Unreadable,
                "{odd:?} is not a canonical version",
            );
        }
        assert_eq!(
            parse_format("created", b"1"),
            JournalFormat::Unreadable,
            "a row at seq 0 that is not a marker is not read as one",
        );
        assert!(
            JournalFormat::Version(0).is_unknown(),
            "no format was ever numbered 0",
        );
        assert!(!JournalFormat::Unmarked.is_unknown());
    }

    /// ONE column write, no rehash, no key: flip the marker's `op`. The digest
    /// is untouched, so a verifier that hashed a substituted canonical record
    /// would still call the marker good — and then read that same `op` and
    /// declare the journal unreadable. A pristine journal would report
    /// "upgrade norte". The shape is COMPARED, so it reports the truth.
    #[tokio::test]
    async fn flipping_the_markers_op_is_a_break_not_an_unknown_format() {
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
        .expect("record");
        sqlx::query("UPDATE journal SET op = 'created' WHERE seq = 0")
            .execute(&j.pool)
            .await
            .expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Broken { first_bad_seq: 0 },
        );
    }

    /// A tampered column that does not even decode must still produce a
    /// VERDICT: `SQLite` types are dynamic, and neither a panic nor a bare
    /// error is the statement the audit exists to make. A row that cannot be
    /// decoded is a row that cannot be recomputed, which is a break.
    #[tokio::test]
    async fn a_type_confused_column_does_not_panic_the_verdict() {
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
        .expect("record");
        // A non-UTF-8 BLOB in a TEXT column: TEXT affinity converts numbers,
        // never blobs, so this is what actually reaches the decoder — and
        // `row.get::<String>` would PANIC on it.
        sqlx::query("UPDATE journal SET op = X'FFFE' WHERE seq = 1")
            .execute(&j.pool)
            .await
            .expect("tamper");
        assert_eq!(
            j.verify_chain().await.expect("a verdict, not an error"),
            ChainStatus::Broken { first_bad_seq: 1 },
            "an undecodable row is evidence, and it is reported where it is",
        );
    }

    /// A pre-migration journal has no marker and stays valid: its absence is
    /// information, not a fault. It is never stamped either — a row inserted
    /// ahead of `seq 1` would break the chain of a file nobody touched.
    #[tokio::test]
    async fn a_pre_migration_journal_is_unmarked_and_is_not_stamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("viejo.db");
        write_pre_migration_journal(&path).await;

        let j = Journal::open(&path).await.expect("open");
        assert_eq!(j.format().await.expect("format"), JournalFormat::Unmarked);
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Intact { entries: 1 },
        );
        let markers: i64 = sqlx::query("SELECT COUNT(*) FROM journal WHERE seq = 0")
            .fetch_one(&j.pool)
            .await
            .expect("count")
            .get(0);
        assert_eq!(markers, 0, "history that exists is never re-stamped");
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

    /// Escribe `n` mutaciones del humano y devuelve sus `seq`.
    async fn del_humano(j: &Journal, n: usize) -> Vec<i64> {
        let mut seqs = Vec::new();
        for i in 0..n {
            let seq = j
                .record(
                    "created",
                    format!("file:///a/{i}").as_bytes(),
                    None,
                    Reversal::Delete,
                    None,
                    &Actor::User,
                )
                .await
                .expect("record");
            seqs.push(seq);
        }
        seqs
    }

    /// La página va de la más NUEVA hacia atrás, respeta el tope, y el
    /// `before_seq` es ESTRICTO: la entrada señalada no vuelve a salir, que
    /// es lo que hace que paginar termine en vez de repetir una fila para
    /// siempre.
    #[tokio::test]
    async fn la_pagina_va_hacia_atras_y_before_seq_es_estricto() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = del_humano(&j, 5).await;

        let primera = j.page(None, 2, None).await.expect("page");
        assert_eq!(
            primera.iter().map(|e| e.entry.seq).collect::<Vec<_>>(),
            vec![seqs[4], seqs[3]],
            "las dos más nuevas, en ese orden"
        );

        let segunda = j.page(Some(seqs[3]), 2, None).await.expect("page");
        assert_eq!(
            segunda.iter().map(|e| e.entry.seq).collect::<Vec<_>>(),
            vec![seqs[2], seqs[1]],
            "sigue por debajo de la última servida, sin repetirla"
        );
    }

    /// **Paginar hasta el final devuelve cada entrada UNA vez y termina**
    /// (hallazgo de la revisión de protocolo: la regla del cursor estaba
    /// escrita dos veces y probada cero).
    ///
    /// Es el contrato entero en un test: sin repetir, sin saltarse ninguna,
    /// en orden descendente, y con `next_before_seq` a `None` exactamente
    /// cuando ya no queda nada más viejo.
    #[tokio::test]
    async fn paginar_hasta_el_final_no_repite_ni_se_salta_nada() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = del_humano(&j, 5).await;

        let mut vistas = Vec::new();
        let mut cursor = None;
        let mut vueltas = 0;
        loop {
            vueltas += 1;
            assert!(vueltas < 10, "el bucle de paginación no termina");
            let pagina = j.page(cursor, 2, None).await.expect("page");
            let wire = page_to_wire(&pagina, 2);
            vistas.extend(wire.rows.iter().map(|r| r.seq));
            match wire.next_before_seq {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        let mut esperadas = seqs.clone();
        esperadas.reverse();
        assert_eq!(vistas, esperadas, "todas, una vez, de la nueva a la vieja");
        assert_eq!(vueltas, 3, "2 + 2 + 1, y la de 1 ya no ofrece cursor");
    }

    /// Una página que viene LLENA justo al agotar el journal ofrece cursor, y
    /// la vuelta siguiente contesta vacío y sin cursor. Es el único caso en
    /// que el cliente da una vuelta de más, y es correcto: el servidor no
    /// puede saber que no queda nada sin mirar.
    #[tokio::test]
    async fn una_pagina_justa_ofrece_cursor_y_la_siguiente_cierra() {
        let j = Journal::open_in_memory().await.expect("open");
        del_humano(&j, 2).await;

        let primera = page_to_wire(&j.page(None, 2, None).await.expect("page"), 2);
        let cursor = primera.next_before_seq.expect("la página vino llena");

        let segunda = page_to_wire(&j.page(Some(cursor), 2, None).await.expect("page"), 2);
        assert!(segunda.rows.is_empty());
        assert_eq!(segunda.next_before_seq, None, "y ahí se cierra");
    }

    /// Filtrar por clase de actor deja fuera a las demás — y no filtrar las
    /// trae todas.
    #[tokio::test]
    async fn la_pagina_filtra_por_clase_de_actor() {
        let j = Journal::open_in_memory().await.expect("open");
        del_humano(&j, 2).await;
        j.record(
            "created",
            b"file:///a/agente",
            None,
            Reversal::Delete,
            None,
            &Actor::Agent {
                session: "s-1".into(),
            },
        )
        .await
        .expect("record");

        let todas = j.page(None, 50, None).await.expect("page");
        assert_eq!(todas.len(), 3);

        let solo_humano = j.page(None, 50, Some("user")).await.expect("page");
        assert_eq!(solo_humano.len(), 2);
        assert!(solo_humano.iter().all(|e| e.entry.actor_kind == "user"));

        let solo_agente = j.page(None, 50, Some("agent")).await.expect("page");
        assert_eq!(solo_agente.len(), 1);
    }

    /// `revertible_for_after` deja FUERA la entrada señalada y todo lo
    /// anterior. Señalar una fila es decir «vuelve a este estado», así que
    /// esa fila es lo que se conserva, no la primera víctima — y equivocarse
    /// aquí deshace una mutación que el humano quería mantener.
    #[tokio::test]
    async fn revertible_after_no_toca_la_entrada_senalada() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = del_humano(&j, 4).await;

        let desde_la_segunda = j
            .revertible_for_after(&Actor::User, seqs[1], None)
            .await
            .expect("revertible");

        assert_eq!(
            desde_la_segunda.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![seqs[3], seqs[2]],
            "LIFO, y sin la señalada ni lo de antes"
        );
    }

    /// **Un LOTE partido por el corte se queda FUERA entero** (BLOCKER de la
    /// revisión de seguridad).
    ///
    /// `revertible_for` traía todas las entradas del actor, así que un
    /// `batch_id` llegaba siempre completo a `undo_units`. Cortar por `seq`
    /// rompe eso: `revert_batch` —que revierte «entero o nada»— recibiría
    /// media unidad creyéndola entera, porque su `debug_assert` sólo
    /// comprueba que el trozo sea internamente coherente, y un trozo lo es.
    /// El resultado sería un `fs.rename_batch` con la mitad de los nombres
    /// devueltos y la otra mitad no.
    ///
    /// Se excluye entero, y no se incluye entero, porque incluirlo desharía
    /// la entrada que el humano señaló para CONSERVAR.
    #[tokio::test]
    async fn un_lote_partido_por_el_corte_se_queda_fuera_entero() {
        let j = Journal::open_in_memory().await.expect("open");
        let lote = j.alloc_batch().await.expect("batch");
        let mut seqs = Vec::new();
        for i in 0..3 {
            let seq = j
                .record_entry(&NewEntry {
                    op: "renamed",
                    path: format!("file:///a/{i}").as_bytes(),
                    path_to: Some(format!("file:///a/viejo{i}").as_bytes()),
                    reversal: Reversal::RenameBack,
                    reversal_ref: None,
                    actor: &Actor::User,
                    undoes_seq: None,
                    batch_id: Some(lote),
                })
                .await
                .expect("record");
            seqs.push(seq);
        }
        let suelta = j
            .record(
                "created",
                b"file:///a/suelta",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record");

        // Corte EN MEDIO del lote: la de en medio.
        let elegidas = j
            .revertible_for_after(&Actor::User, seqs[1], None)
            .await
            .expect("revertible");

        assert_eq!(
            elegidas.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![suelta],
            "sólo lo de fuera del lote: el lote no se parte"
        );
    }

    /// Y un lote ENTERAMENTE posterior al corte sí entra entero.
    #[tokio::test]
    async fn un_lote_entero_posterior_al_corte_entra() {
        let j = Journal::open_in_memory().await.expect("open");
        let corte = j
            .record(
                "created",
                b"file:///a/base",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record");
        let lote = j.alloc_batch().await.expect("batch");
        for i in 0..2 {
            j.record_entry(&NewEntry {
                op: "renamed",
                path: format!("file:///a/{i}").as_bytes(),
                path_to: Some(format!("file:///a/viejo{i}").as_bytes()),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &Actor::User,
                undoes_seq: None,
                batch_id: Some(lote),
            })
            .await
            .expect("record");
        }

        let elegidas = j
            .revertible_for_after(&Actor::User, corte, None)
            .await
            .expect("revertible");

        assert_eq!(elegidas.len(), 2, "el lote entero: {elegidas:?}");
        assert!(elegidas.iter().all(|e| e.batch_id == Some(lote)));
    }

    /// El TECHO (0.80.0) es el espejo del corte: lo más nuevo se queda, y un
    /// LOTE con una entrada por encima se queda ENTERO — aunque la de arriba
    /// no sea revertible, porque la regla mira el journal entero y no la
    /// selección. Un techo por debajo del corte no selecciona nada.
    #[tokio::test]
    async fn el_techo_deja_fuera_lo_nuevo_y_el_lote_que_parte() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = del_humano(&j, 1).await;
        let corte = seqs[0];
        let lote = j.alloc_batch().await.expect("batch");
        let mut del_lote = Vec::new();
        for i in 0..2 {
            del_lote.push(
                j.record_entry(&NewEntry {
                    op: "renamed",
                    path: format!("file:///a/{i}").as_bytes(),
                    path_to: Some(format!("file:///a/viejo{i}").as_bytes()),
                    reversal: Reversal::RenameBack,
                    reversal_ref: None,
                    actor: &Actor::User,
                    undoes_seq: None,
                    batch_id: Some(lote),
                })
                .await
                .expect("record"),
            );
        }
        let suelta = j
            .record(
                "created",
                b"file:///a/suelta",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record");

        let seqs_de = |v: Vec<JournalEntry>| v.into_iter().map(|e| e.seq).collect::<Vec<_>>();
        // Techo justo en la primera del lote: lo parte, así que fuera entero.
        let partido = j
            .revertible_for_after(&Actor::User, corte, Some(del_lote[0]))
            .await
            .expect("revertible");
        assert!(partido.is_empty(), "el lote partido no entra: {partido:?}");
        // Techo en la última del lote: entra entero, y la suelta de después no.
        let entero = j
            .revertible_for_after(&Actor::User, corte, Some(del_lote[1]))
            .await
            .expect("revertible");
        assert_eq!(seqs_de(entero), vec![del_lote[1], del_lote[0]]);
        // Sin techo, todo; con un techo por debajo del corte, nada.
        let todo = j
            .revertible_for_after(&Actor::User, corte, None)
            .await
            .expect("revertible");
        assert_eq!(seqs_de(todo), vec![suelta, del_lote[1], del_lote[0]]);
        let nada = j
            .revertible_for_after(&Actor::User, corte, Some(corte - 1))
            .await
            .expect("revertible");
        assert!(nada.is_empty());
    }

    /// Y sólo mira al actor que se le pide: lo que hizo un agente no entra en
    /// el «deshaz lo mío» de un humano, aunque sea posterior.
    #[tokio::test]
    async fn revertible_after_no_se_lleva_lo_de_otro_actor() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = del_humano(&j, 1).await;
        j.record(
            "created",
            b"file:///a/agente",
            None,
            Reversal::Delete,
            None,
            &Actor::Agent {
                session: "s-1".into(),
            },
        )
        .await
        .expect("record");

        let del_humano = j
            .revertible_for_after(&Actor::User, seqs[0], None)
            .await
            .expect("revertible");

        assert!(
            del_humano.is_empty(),
            "lo del agente es suyo: {del_humano:?}"
        );
    }

    /// La fila de wire dice `reversible` por lo que la entrada DECLARÓ, y
    /// una ruta ilegible se enseña con reemplazos en vez de perderse: una
    /// mutación que no se ve es indistinguible de una que no ocurrió.
    #[tokio::test]
    async fn la_fila_de_wire_no_pierde_una_entrada_ilegible() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "deleted",
            b"file:///a/\xff\xfe",
            None,
            Reversal::Irreversible,
            None,
            &Actor::User,
        )
        .await
        .expect("record");

        let filas: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert_eq!(
            filas.len(),
            1,
            "la entrada sale aunque su ruta no sea texto"
        );
        assert!(!filas[0].reversible, "un Irreversible lo dice");
        assert!(filas[0].path.contains('\u{FFFD}'));
        assert!(filas[0].hostile, "y la fila lo DICE, no lo deja adivinar");
    }

    /// **Una ruta con algo que un terminal ejecutaría sale SANEADA, y la fila
    /// lo dice** (hallazgo de la revisión de seguridad).
    ///
    /// El nombre de un fichero lo elige quien lo crea —incluido un agente
    /// dentro de su recinto— y ésta es la pantalla donde un humano decide
    /// qué revertir: un override bidi o una secuencia de escape aquí
    /// repintan esa decisión. Mismo trato que le da `fs.search` a la línea
    /// que devuelve.
    #[tokio::test]
    async fn una_ruta_con_trampa_de_terminal_sale_saneada() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "renamed",
            "file:///a/\u{202E}gpj.exe".as_bytes(),
            Some("file:///a/\u{1b}[2Jborrado".as_bytes()),
            Reversal::RenameBack,
            None,
            &Actor::User,
        )
        .await
        .expect("record");

        let filas: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert!(
            !filas[0].path.contains('\u{202E}'),
            "el override RTL no sale crudo: {}",
            filas[0].path
        );
        let destino = filas[0].path_to.as_deref().expect("hay destino");
        assert!(
            !destino.contains('\u{1b}'),
            "ni un ESC en el destino: {destino}"
        );
        assert!(filas[0].hostile, "y se marca como pintado distinto");
    }

    /// Un `reversal` que este binario no conoce cuenta como SIN vuelta: en
    /// un journal manipulado, afirmar que algo se puede deshacer es la
    /// mentira que cuesta cara.
    #[tokio::test]
    async fn un_reversal_desconocido_no_promete_vuelta() {
        let j = Journal::open_in_memory().await.expect("open");
        j.record(
            "created",
            b"file:///a/x",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("record");
        sqlx::query("UPDATE journal SET reversal = 'lo_que_sea' WHERE seq = 1")
            .execute(&j.pool)
            .await
            .expect("tocar la fila");

        let filas: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert!(!filas[0].reversible);
    }

    /// Una entrada YA deshecha lo dice, y su compensación también: son las
    /// dos cosas que el undo no va a volver a tocar, y sin ellas una línea
    /// de tiempo promete el doble de lo que va a pasar.
    #[tokio::test]
    async fn la_pagina_dice_lo_que_ya_esta_deshecho() {
        let j = Journal::open_in_memory().await.expect("open");
        let seq = j
            .record(
                "created",
                b"file:///a/x",
                None,
                Reversal::Delete,
                None,
                &Actor::User,
            )
            .await
            .expect("record");
        j.record_undoing(
            "deleted",
            b"file:///a/x",
            None,
            Reversal::Irreversible,
            None,
            &Actor::User,
            Some(seq),
        )
        .await
        .expect("compensación");

        let filas: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        let compensacion = &filas[0];
        let original = &filas[1];
        assert_eq!(compensacion.undoes_seq, Some(seq), "es la compensación");
        assert!(original.undone, "y la de abajo ya está deshecha");
    }
}
