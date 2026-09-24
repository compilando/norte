//! Transactional journal (M3-1, ADR 0023): every mutation → one entry with an
//! actor, a reversal reference, and a hash chain over `SQLite` (WAL).
//!
//! **Scope of the integrity guarantee (important).** The hash chain (keyless
//! SHA-256, fixed genesis) detects corruption and NAIVE edits — ones that do
//! not recompute the chain. It is NOT tamper-evidence against an attacker with
//! write access to the DB: a full rewrite, a TAIL truncation and a rollback
//! all pass [`Journal::verify_chain`]. The **HMAC anchors** (M3-5, ADR 0025,
//! module [`crate::audit`]) bound that window: fabricating history ALSO
//! requires the keyring key and re-anchoring. Still not covered: an attacker
//! with keyring access, mutations between the last anchor and the attack,
//! destruction of the anchor file (an external copy is recommended).
//!
//! **Format (ADR 0046).** A journal created from this version onward declares
//! its format in an entry INSIDE the chain, at the reserved `seq` 0: that way
//! an older binary can say "I don't know how to read this" instead of
//! accusing of tampering a file nobody touched (#127). `seq` 0 is metadata,
//! not history: [`Journal::entries`], [`Journal::revertible_for`],
//! [`Journal::count`] and [`Journal::head`] only see mutations (`seq >= 1`).

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

/// Migration for the batch column (batch rename, §17). Deliberately kept
/// outside `SCHEMA`: `CREATE TABLE IF NOT EXISTS` does NOT alter a table that
/// already exists, so a DB written before this version would end up without
/// the column. Idempotency comes from asking the catalog FIRST
/// ([`has_batch_id_column`]), not from swallowing the `ALTER`'s error: the
/// "duplicate column name" message is nobody's contract, and swallowing an
/// error by its text also swallows the one that shouldn't be swallowed.
const MIGRATE_BATCH_ID: &str = "ALTER TABLE journal ADD COLUMN batch_id INTEGER";

/// Migration for the column that points at the entry an undo COMPENSATES
/// (M3-2). This column was added to `SCHEMA` WITHOUT its migration: an older
/// journal opened fine and then blew up on EVERY write with "table journal has
/// no column named `undoes_seq`". Seen live when journaling the embedded
/// engine (#167), where that failure left a `norte cp` in "internal error"
/// without copying anything.
///
/// Unlike [`MIGRATE_BATCH_ID`], this ONLY applies to an EMPTY table: the
/// column arrived together with its presence byte in the hash's preimage, so
/// older rows do not verify under today's `chain_hash`. See the call site.
const MIGRATE_UNDOES_SEQ: &str = "ALTER TABLE journal ADD COLUMN undoes_seq INTEGER";

/// How long `SQLite` waits on someone else's lock before giving up, unless
/// whoever opens it says otherwise ([`Journal::open_with_busy_timeout`]).
///
/// It is `sqlx`'s own default, written here so it is a named fact and not an
/// implicit property of a dependency.
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

/// Read columns, in two fixed variants. WITHOUT `format!`: in the file that
/// holds the evidence of tampering, "SQL is not built from strings here" has
/// to be checkable at a glance. The `NULL` variant is for a pre-migration DB
/// opened READ-ONLY, which cannot be altered.
const SELECT_VERIFY: &str = "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash, batch_id FROM journal ORDER BY seq ASC";
const SELECT_VERIFY_NO_BATCH: &str = "SELECT seq, ts_ms, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, prev_hash, entry_hash, NULL FROM journal ORDER BY seq ASC";
/// `seq >= 1` on every MUTATION reader: row 0 is the format marker
/// ([`FORMAT_SEQ`]), which is part of the chain but not part of the history.
/// Letting it out here would put a non-mutation row in the audit export, in
/// the undo's LIFO stack and in `count()`.
const SELECT_ENTRIES: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, batch_id FROM journal WHERE seq >= 1 ORDER BY seq ASC";
const SELECT_ENTRIES_NO_BATCH: &str = "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, NULL FROM journal WHERE seq >= 1 ORDER BY seq ASC";
/// An entry is UNDONE if it has a LIVE compensation: an entry with its `seq`
/// in `undoes_seq` that nobody has, in turn, compensated.
///
/// The naive condition — "some compensation exists" — was correct as long as
/// a compensation was never itself undone. The undo of a BATCH (§17) broke
/// that: it runs through the same executor as the forward action, so if a
/// step of the undo fails, the executor UNDOES the undo steps it had already
/// applied and journals that reversal as a compensation of the compensation.
/// The tree ends up as it was — the batch is still applied — but under the
/// naive condition its entries stayed covered by compensations that no longer
/// count, and the batch became UNUNDOABLE forever, silently.
///
/// The chain this code can produce is `O ← C ← D` and nothing deeper: a
/// compensation is born with a non-null `undoes_seq`, so it is never
/// revertible by itself and nobody compensates it again except by undoing its
/// own undo batch. One level of nesting covers exactly that.
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

/// A backward PAGE for the timeline (phase 7).
///
/// `?1` is the EXCLUSIVE upper bound (`NULL` = from the newest) and `?2` the
/// actor class (`NULL` = all). The `IS NULL OR` on each one is what lets a
/// single statement cover all four combinations, instead of building SQL by
/// concatenation — which is how something from outside ends up inside a
/// query.
///
/// Compensations (`undoes_seq IS NOT NULL`) DO come out: they are mutations
/// that happened, and a timeline that hid them would show a past that did not
/// happen — "I undid this" is an event just as real as the one it undid.
/// Column 12 of the two that follow: whether the entry is ALREADY undone,
/// i.e. whether it has a LIVE compensation.
///
/// It is the SAME condition [`SELECT_REVERTIBLE`] uses to discard it, and
/// that is why it is written once and pasted into both: if they diverged, the
/// timeline would promise to undo entries the undo is going to skip — which
/// is exactly the number a confirmation cannot get wrong.
///
/// The client CANNOT compute it: the compensation may be outside the page it
/// has in front of it.
const COL_UNDONE: &str = "(seq IN ( \
       SELECT c.undoes_seq FROM journal c \
       WHERE c.undoes_seq IS NOT NULL \
         AND c.seq NOT IN (SELECT d.undoes_seq FROM journal d WHERE d.undoes_seq IS NOT NULL) \
     ))";

/// The page, with [`COL_UNDONE`] pasted behind the usual twelve.
fn select_page(with_batch: bool) -> String {
    let batch = if with_batch { "batch_id" } else { "NULL" };
    format!(
        "SELECT seq, ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal, reversal_ref, undoes_seq, {batch}, {COL_UNDONE} \
         FROM journal \
         WHERE seq >= 1 AND (?1 IS NULL OR seq < ?1) AND (?2 IS NULL OR actor_kind = ?2) \
         ORDER BY seq DESC LIMIT ?3"
    )
}

/// [`SELECT_REVERTIBLE`] bounded to what comes AFTER a `seq` (phase 7).
///
/// Same body, two more conditions. Duplicated instead of composed because the
/// "live compensation" condition is the delicate part of this query — its
/// rationale is on [`SELECT_REVERTIBLE`] — and an SQL builder that pastes it
/// together in pieces is how it one day stops being there.
///
/// **A BATCH goes in whole or not at all** (`batch_id NOT IN (…seq <= cutoff)`),
/// and this is the condition that actually matters here. `revertible_for`
/// did not need it: it brought back ALL of the actor's entries, so a
/// `batch_id` always arrived complete at `undo_units`. Cutting by `seq` stops
/// that being true, and `revert_batch` — which reverts "all or nothing" —
/// would receive half a unit believing it whole: its own `debug_assert` only
/// checks that the piece is internally coherent, and a piece is. The result
/// would be an `fs.rename_batch` with half the names returned and the other
/// half not, with compensations written for the half that moved.
///
/// And it is NOT solved by including the whole batch: that would undo entries
/// before the cutoff, i.e. the row the human pointed at to KEEP. It is
/// excluded, which is the safe direction — undoing too little gets asked for
/// again; undoing too much does not. A batch's seqs may also not be
/// contiguous (two concurrent tasks interleave, as `alloc_batch` says), so
/// this cannot be left to the cutoff "happening to fall between batches".
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
// The CEILING (`?4`, 0.80.0) is the exact mirror of the cutoff: nothing
// above it, and no batch with an entry above it — checked against the WHOLE
// journal, same as the cutoff, and not against what is already selected.
// Reverting the counted half of a batch that kept growing is reverting half
// a unit believing it whole. `NULL` = no ceiling, which is 0.79's behavior.
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

/// Journal errors.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// Error from the sqlx layer (open/query/insert).
    #[error("sqlite: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// The journal on disk is corrupt (e.g. an `entry_hash` that is not 32
    /// bytes long): it does NOT panic, it fails safe.
    #[error("corrupt journal: {0}")]
    Corrupt(&'static str),
    /// I/O error while preparing the journal's location (e.g. creating the
    /// config dir).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<JournalError> for ProtoError {
    fn from(_: JournalError) -> Self {
        // The detail goes through `tracing`; it is exposed as internal to the
        // wire/task.
        ProtoError::Internal { panic: false }
    }
}

/// Who originated the mutation (spec §10). Today always `User`; agents set it
/// via scopes/MCP (M3-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A local human frontend.
    User,
    /// An agent over MCP, with its session id.
    Agent {
        /// The agent's session id.
        session: String,
    },
    /// A plugin, with its declared id.
    Plugin {
        /// The plugin's id.
        id: String,
    },
}

impl Actor {
    /// `(kind, id)` for persisting: `("user", None)`, `("agent", Some(sess))`…
    #[must_use]
    pub fn parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Actor::User => ("user", None),
            Actor::Agent { session } => ("agent", Some(session.as_str())),
            Actor::Plugin { id } => ("plugin", Some(id.as_str())),
        }
    }
}

/// How to revert the entry (M3-2 is what EXECUTES it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversal {
    /// Delete the created node.
    Delete,
    /// Rename back (destination → source).
    RenameBack,
    /// Restore from the trash.
    RestoreTrash,
    /// No way back (permanent delete).
    Irreversible,
    /// Give back the POSIX permissions it had (#314).
    ///
    /// The PREVIOUS mode travels in `reversal_ref`, which for this op is not a
    /// path but the number in ASCII decimal. It is the only column that
    /// exists for "what the reversal needs", and adding another one to the
    /// schema for twelve bits would be worse than saying here what is inside.
    SetModeBack,
}

impl Reversal {
    /// Persisted label.
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

    /// The reversal that label names, or `None` if this binary does not know
    /// it.
    ///
    /// `None` is NOT "irreversible": it is "I don't know what this is", and
    /// whoever asks has to decide what to do with that difference. The
    /// timeline counts it as having NO way back, because in a tampered
    /// journal — or one written by a version that is not this one —
    /// asserting that something can be undone is the lie that costs dearly.
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

/// A journal entry materialized for READING (undo M3-2, audit M3-5,
/// integration tests). `path`/`path_to`/`reversal_ref` are the raw BYTES from
/// [`norte_proto::VPath::to_wire`] (rule 1): reconstruct with
/// `VPath::from_wire` on consumption, never assume UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// Monotonic sequence assigned when recorded.
    pub seq: i64,
    /// UTC milliseconds of the recording (the daemon's clock at journaling
    /// time).
    pub ts_ms: i64,
    /// The entry's hash in the chain (32 bytes) — the audit cross-checks it
    /// against the anchors (ADR 0025).
    pub entry_hash: Vec<u8>,
    /// Origin: `"user" | "agent" | "plugin"`.
    pub actor_kind: String,
    /// Agent's session id / plugin's id, if applicable.
    pub actor_id: Option<String>,
    /// Operation: `"created" | "removed" | "trashed" | "renamed"`.
    pub op: String,
    /// Affected path (`to_wire` bytes).
    pub path: Vec<u8>,
    /// Destination of a `renamed` (`to_wire` bytes).
    pub path_to: Option<Vec<u8>>,
    /// Persisted reversal label ([`Reversal::as_str`]).
    pub reversal: String,
    /// Reference needed to revert (e.g. the trash path of a `trashed`),
    /// `to_wire` bytes.
    pub reversal_ref: Option<Vec<u8>>,
    /// If this entry COMPENSATES an undo, the original `seq` it undoes;
    /// `None` for a normal mutation.
    pub undoes_seq: Option<i64>,
    /// The batch the entry belongs to (`fs.rename_batch`). It is the LABEL
    /// that will allow undoing n entries as a single unit; the consumer (the
    /// batch executor and the group undo) comes later. `None` for a standalone
    /// mutation — and for every entry written before batches existed.
    pub batch_id: Option<i64>,
}

/// An entry as [`Journal::page`] serves it: the entry, plus whether it is
/// ALREADY undone.
///
/// "Undone" is not a column in the table: it is that a LIVE compensation of
/// it exists, and that is computed with a subquery (`COL_UNDONE`). It travels
/// attached to the entry because the client cannot deduce it — the
/// compensation may be outside its page — and without it a timeline counts as
/// undoable what the undo is going to skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    /// The entry.
    pub entry: JournalEntry,
    /// Whether it has a live compensation.
    pub undone: bool,
}

impl PageEntry {
    /// This entry in its WIRE form, for the timeline
    /// ([`norte_proto::methods::JOURNAL_LIST`], phase 7).
    ///
    /// Lives here and not in the daemon because both backends need it: the
    /// embedded one answers the same list with no socket in between, and two
    /// conversions for the same question diverge at the first field someone
    /// adds.
    ///
    /// Paths come out SANITIZED (`mask_terminal_hazards`) and with
    /// [`norte_proto::methods::JournalRow::hostile`] set if the text stopped
    /// saying what the bytes said. A file's name is chosen by whoever creates
    /// it — including an agent inside its confinement — and this is the
    /// screen where a human decides what to revert: a bidi override or an
    /// escape sequence here would repaint that decision. It is the same
    /// treatment `fs.search` gives the line it returns, and for the same
    /// reason.
    ///
    /// Bytes not being text does not drop the row: it is shown with
    /// replacements. A mutation that cannot be seen is indistinguishable from
    /// one that did not happen.
    #[must_use]
    pub fn to_wire_row(&self) -> norte_proto::methods::JournalRow {
        let e = &self.entry;
        let (path, path_hostile) = path_text(&e.path);
        let (path_to, to_hostile) = match &e.path_to {
            Some(b) => {
                let (t, h) = path_text(b);
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
            hostile: path_hostile || to_hostile,
            // A `reversal` token this daemon does not know counts as having
            // NO way back: in a tampered journal, asserting that something
            // can be undone is the costly lie.
            reversible: Reversal::from_str_opt(&e.reversal)
                .is_some_and(|r| r != Reversal::Irreversible),
            undoes_seq: e.undoes_seq,
            undone: self.undone,
            batch_id: e.batch_id,
        }
    }
}

/// A page in its wire form, with the CURSOR already computed.
///
/// Lives here, next to [`PageEntry::to_wire_row`], and for the same reason:
/// the two edges that serve `journal.list` — the daemon and the embedded
/// backend — had this same six-field expression copied, and neither copy
/// could turn red on its own.
///
/// **The cursor is only offered if the page came back FULL.** With a partial
/// one there is nothing older left, and offering it would make the client
/// ask for another round to receive zero rows, forever. And it is the `seq`
/// of the last one served, never `seq - 1`: `seq`s are not dense, and that
/// subtraction is exactly the arithmetic that breaks the day they stop being
/// so.
#[must_use]
pub fn page_to_wire(entries: &[PageEntry], limit: u32) -> norte_proto::methods::JournalListResult {
    let full = entries.len() == limit as usize;
    norte_proto::methods::JournalListResult {
        rows: entries.iter().map(PageEntry::to_wire_row).collect(),
        // The `flatten` is not decoration: with a `limit` of zero — which no
        // edge lets through, but which nothing down here can guarantee — an
        // empty page would count as "full", and this saves it from
        // announcing a cursor that does not exist.
        next_before_seq: full.then(|| entries.last().map(|e| e.entry.seq)).flatten(),
    }
}

/// The paintable text of some path bytes, and whether it stopped saying what
/// they said (for not being text, or for carrying something a terminal would
/// execute).
fn path_text(bytes: &[u8]) -> (String, bool) {
    let raw = String::from_utf8_lossy(bytes);
    let sanitized = norte_encoding::mask_terminal_hazards(&raw);
    let hostile = matches!(raw, std::borrow::Cow::Owned(_)) || sanitized != raw;
    (sanitized, hostile)
}

/// Materializes a `JournalEntry` from a row with the column order `seq,
/// ts_ms, entry_hash, actor_kind, actor_id, op, path, path_to, reversal,
/// reversal_ref, undoes_seq, batch_id` (shared by `entries` and
/// `revertible_for`).
///
/// `try_get` on every one: `SQLite`'s typing is DYNAMIC, so a non-UTF-8 blob
/// put in a TEXT column makes `get` PANIC — and a panic here is a `norte
/// audit export` that exports nothing instead of an error that can be
/// read and counted.
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

/// Does the `batch_id` column already exist? Asked of the catalog instead of
/// assumed: the READ-ONLY handle (audit) cannot migrate an old DB and still
/// has to read it. A missing table answers ZERO rows → `false`, and the first
/// real query will fail with its own error, without masking anything.
async fn has_batch_id_column(pool: &SqlitePool) -> Result<bool, JournalError> {
    has_column(pool, "batch_id").await
}

/// Whether the `journal` table has the column `name`, by asking the catalog.
///
/// See [`has_batch_id_column`] for why it is asked instead of swallowing the
/// `ALTER`'s error.
async fn has_column(pool: &SqlitePool, name: &str) -> Result<bool, JournalError> {
    Ok(columns(pool).await?.iter().any(|c| c == name))
}

/// The columns of the `journal` table, according to the catalog. EMPTY if the
/// table does not exist — which is not an error here: the audit opens
/// whatever file it is pointed at and the first real query will fail with its
/// own error, without masking anything.
async fn columns(pool: &SqlitePool) -> Result<Vec<String>, JournalError> {
    let rows = sqlx::query("PRAGMA table_info(journal)")
        .fetch_all(pool)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        // `try_get`: the row is produced by a file the operator points us at
        // (the audit opens whatever it is given), so its shape is not
        // assumed.
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

/// Verdict of [`Journal::verify_chain`] (B2 of #63): if the chain broke,
/// WHERE — the audit cites it instead of a mute boolean.
///
/// `#[non_exhaustive]`: a verdict enum grows (this variant is the proof), and
/// a caller that stops compiling is cheaper than one that silently treats a
/// new verdict as the old one. The wildcard arm a caller must write is the
/// fail-closed answer — "did not certify" — for every verdict yet to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainStatus {
    /// Chain intact.
    Intact {
        /// Entries verified.
        entries: u64,
    },
    /// First entry whose chaining or hash does not match.
    Broken {
        /// `seq` of the first break.
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
    /// `true` if the chain is intact.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        matches!(self, ChainStatus::Intact { .. })
    }
}

/// An entry's fields, in the hash's canonical order.
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

/// `entry_hash = sha256(prev_hash ‖ length- and presence-prefixed fields)`.
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
    // AT THE END and ONLY if there is a batch. `None` feeds NOTHING — not even
    // the presence byte the other `Option`s use — because an entry written
    // before `batch_id` existed has to hash EXACTLY as it did back then:
    // otherwise `verify_chain` would scream "tampered" over a DB that was only
    // migrated. `Some` does feed presence + a length-prefixed id, so neither
    // stripping a batch nor inventing one survives verification.
    //
    // THIS IS NOT A REUSABLE PATTERN. It works because `batch_id` is the LAST
    // field: the message of an entry with no batch is a strict prefix of one
    // with a batch, so there is no ambiguity. A SECOND optional field added
    // with the same trick would create one instantly — `(batch=Some(x),
    // other=None)` and `(batch=None, other=Some(x))` would produce the SAME
    // tail `01 ‖ len ‖ x` and therefore the same `entry_hash`, which is a hole
    // in the chain, not an optimization. New field ⇒ `feed_opt` (always
    // presence), and if compatibility ever required repeating the trick,
    // version the chain's format first — with a marker INSIDE what the chain
    // and the anchors authenticate, never in the file's header.
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

/// Chain state. `seq` and `last_hash` advance TOGETHER under `record`'s
/// `Mutex`, so `seq` order == chaining order by construction (avoids the
/// false "tampered" verdict under concurrency — security M1).
struct ChainState {
    last_seq: i64,
    last_hash: [u8; 32],
    /// Last batch id handed out. Lives HERE, under the same lock as `seq`, so
    /// two concurrent batch tasks cannot share an id.
    batch_counter: i64,
}

/// The transactional journal over `SQLite` (WAL).
pub struct Journal {
    /// **Never CLONED outside this type**, and
    /// [`crate::embedded::LazyJournal::release`] depends on that: a cloned
    /// `SqlitePool` survives the `Arc::try_unwrap` that decides nobody holds
    /// the journal, and keeps the file open after this process has declared
    /// itself not the owner. `pub(crate)` does not prevent it; this line says
    /// so. Every use across the tree is a borrow (`&self.pool`).
    pub(crate) pool: SqlitePool,
    chain: Mutex<ChainState>,
    /// Does the table have the `batch_id` column? Always `true` after a
    /// [`Journal::open`] (it migrates), can be `false` in
    /// [`Journal::open_read_only`] over a pre-migration DB, which cannot be
    /// altered and still has to be auditable.
    has_batch_id: bool,
    /// Who each committed row is offered to (ADR 0100): the hooks. A std
    /// `RwLock` because it is read on every `record_entry` and written once
    /// at startup; `None` until [`Journal::set_hook_sender`] sets it.
    hooks: std::sync::RwLock<Option<crate::hooks::HookSender>>,
}

/// Everything ONE journal entry needs. A struct instead of eight positional
/// arguments: the call reads clearly, and adding a field later never reshuffles
/// them. The `path*` fields are BYTES from [`norte_proto::VPath::to_wire`]
/// (rule 1).
#[derive(Debug, Clone, Copy)]
pub struct NewEntry<'a> {
    /// Operation: `"created" | "removed" | "trashed" | "renamed"`.
    pub op: &'a str,
    /// Affected path (`to_wire` bytes).
    pub path: &'a [u8],
    /// Destination of a `renamed` (`to_wire` bytes).
    pub path_to: Option<&'a [u8]>,
    /// How to revert.
    pub reversal: Reversal,
    /// Reference needed to revert (e.g. the destination in the logical
    /// trash), `to_wire` bytes.
    pub reversal_ref: Option<&'a [u8]>,
    /// Who caused it.
    pub actor: &'a Actor,
    /// The `seq` this entry COMPENSATES, if it is an undo.
    pub undoes_seq: Option<i64>,
    /// The batch it belongs to, if it was part of one
    /// ([`Journal::alloc_batch`]): the label that groups n entries to undo
    /// them together. A group undo's compensations are recorded with the SAME
    /// batch, so the group stays readable as a group afterward.
    pub batch_id: Option<i64>,
}

impl Journal {
    /// Opens (or creates) the journal at `path` with WAL + `synchronous=NORMAL`
    /// and an **exclusive file lock** (`locking_mode=EXCLUSIVE`): the hash
    /// chain's single-writer requirement (spec §4) is a MECHANISM, not a
    /// convention — a second process on the same file (e.g. two daemons with
    /// different sockets and the same config dir) fails to open instead of
    /// forking the chain and colliding on `seq` (MAJOR-1 from the
    /// security-reviewer, M3-4). The file is created `0600` BEFORE connecting
    /// (no window with the umask) and its parent dir `0700`.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] on open/create — including `database is locked`
    /// if ANOTHER process already has it open; [`JournalError::Corrupt`] if
    /// the last `entry_hash` is not 32 bytes long; I/O while pre-creating the
    /// file/dir.
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT).await
    }

    /// Like [`Journal::open`], but with its own deadline for the case "someone
    /// else has the lock".
    ///
    /// `busy_timeout` is how long `SQLite` waits before giving up with
    /// `database is locked`. [`DEFAULT_BUSY_TIMEOUT`]'s value is what the
    /// daemon wants: it starts once and would rather wait out someone else's
    /// checkpoint than die.
    ///
    /// An EMBEDDED process wants the opposite, and that is why this door
    /// exists (#167): whoever has the lock has it for its whole lifetime —
    /// another TUI, or the daemon — so waiting five seconds does not get it,
    /// it only turns every `norte cp` into five seconds of nothing before
    /// carrying on without logging.
    ///
    /// NOTE: the deadline belongs to the CONNECTION, not to `open`. It also
    /// governs every later statement on that handle — today it makes no
    /// difference (a single connection, the exclusive owner, nobody to
    /// compete with), and would stop making none if reconnecting under
    /// someone else's lock were ever allowed.
    ///
    /// # Errors
    /// The same as [`Journal::open`] — and with a short deadline, `database is
    /// locked` stops being the rare case: it is THE expected answer when the
    /// journal already has an owner.
    pub async fn open_with_busy_timeout(
        path: &std::path::Path,
        busy_timeout: std::time::Duration,
    ) -> Result<Self, JournalError> {
        // Pre-creation with the correct permissions FROM the first byte
        // (MINOR-1): SQLite would create the file with the umask (typically
        // 0644) and the later chmod left a readable window. With the file
        // already present, `create_if_missing` is a no-op.
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
            // WAL + EXCLUSIVE is valid (single-process WAL): the file's lock
            // is taken on the first write — the schema's CREATE TABLE in
            // `from_options` forces it ALREADY during open.
            .locking_mode(sqlx::sqlite::SqliteLockingMode::Exclusive);
        let opts = opts.busy_timeout(busy_timeout);
        let this = Self::from_options(opts).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Belt-and-suspenders in case the file preexisted with other
            // permissions. The -wal/-shm sidecars inherit from the main file.
            // Through `tokio::fs` (rule 2): it is a short `chmod`, but a
            // `std::fs` call in an async context does not stop being one for
            // being cheap.
            let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
        }
        Ok(this)
    }

    /// Closes the journal and WAITS for the file to become free.
    ///
    /// Dropping the value is not enough, and that is why this exists: `sqlx`
    /// closes `SQLite`'s connection on its worker thread, so a `drop` returns
    /// before the file's exclusive lock has actually been released, and the
    /// next one to open gets a `database is locked` that nobody is holding.
    /// [`crate::embedded::LazyJournal::release`] notices this, and it
    /// releases so that ANOTHER process can open right after.
    ///
    /// Consumes the journal: reopening is [`Self::open`], and it has to be —
    /// that is where `last_seq`/`last_hash` are re-read from the file.
    pub async fn close(self) {
        self.pool.close().await;
    }

    /// Ephemeral in-memory journal (tests).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] / [`JournalError::Corrupt`].
    pub async fn open_in_memory() -> Result<Self, JournalError> {
        Self::from_options(SqliteConnectOptions::from_str("sqlite::memory:")?).await
    }

    /// Opens the journal READ-ONLY for the audit (M3-5): no create, no
    /// schema, no `locking_mode=EXCLUSIVE`. NOTE: the daemon opens the DB
    /// with `SQLite`'s EXCLUSIVE lock — with the daemon running, this open
    /// (or the first query) fails with `database is locked`; the audit is run
    /// with the daemon stopped. `record` on this handle fails (readonly), by
    /// design.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] on open/query (including `database is locked`
    /// with the daemon alive, and a missing file).
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
        // No CREATE TABLE (readonly): if the file is not a journal, the first
        // query will fail with its real error — it is not masked.
        //
        // What IS caught here is a journal older than `undoes_seq`: ALL the
        // queries here name that column and this handle cannot `ALTER` (it is
        // read-only by design), so a raw "no such column" would come out
        // wrapped in `cli-audit-open-failed`. A MISSING table does not go
        // through here — that is "not a journal", and the real query reports
        // it.
        let cols = columns(&pool).await?;
        if !cols.is_empty() && !cols.iter().any(|c| c == "undoes_seq") {
            return Err(JournalError::Corrupt(
                "journal older than undoes_seq: this binary does not know how to read it \
                 (its rows were hashed over a preimage without that column). Read it with \
                 a binary of its own era",
            ));
        }
        let has_batch_id = has_batch_id_column(&pool).await?;
        // The counter starts from the highest written, same as on write: this
        // handle cannot insert anything (SQLite rejects it), but an
        // `alloc_batch` that returned already-used ids would be a LYING
        // answer, and here nothing is lied about since nothing can go wrong.
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

    /// Picks between the query WITH the batch column and the one that
    /// replaces it with `NULL` (a pre-migration DB opened read-only, which
    /// cannot be altered). Two constants, neither built.
    fn pick(&self, with: &'static str, without: &'static str) -> &'static str {
        if self.has_batch_id { with } else { without }
    }

    async fn from_options(opts: SqliteConnectOptions) -> Result<Self, JournalError> {
        // A pool of 1 connection: a single writer (in-memory requires max=1
        // so as not to lose the DB between connections).
        //
        // And that connection is NOT recycled, which is what holds up
        // everything else. The file's exclusive lock is a property of THE
        // LIVE CONNECTION, not of the process: `sqlx`'s defaults
        // (`min_connections=0`, `idle_timeout=10min`, `max_lifetime=30min`)
        // raise a reaper that closes it while idle, and closing it RELEASES
        // the lock. With that, a process that believes itself the owner (a
        // TUI that has been browsing for eleven minutes) lets another one in,
        // and its in-memory `ChainState` — `last_seq`, `last_hash` — goes
        // stale: the next mutation collides with the `seq` PK and fails, and
        // since `last_seq` only advances on success, it fails EVERY
        // subsequent one. An effect already applied without its row, in a
        // loop. (And on `sqlite::memory:`, closing the only connection ERASES
        // the DB.)
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        // Migration: the catalog is asked and only then is it altered. Every
        // error is PROPAGATED (fail safe: without the column a batch cannot
        // be journaled, and writing without it would silently lose the
        // grouping, which is exactly what the chain has to prevent).
        // Idempotency comes from the catalog, NOT from looking at the
        // error's text: "duplicate column name" is nobody's contract, and
        // swallowing an error by its message also swallows the one that
        // shouldn't be swallowed.
        //
        // The format marker (#127, ADR 0046) is what is left for a FUTURE
        // binary so it does not confuse "I don't know how to read this" with
        // "this has been tampered with". It is written below, over a journal
        // with no rows.
        if !has_batch_id_column(&pool).await? {
            sqlx::query(MIGRATE_BATCH_ID).execute(&pool).await?;
            if !has_batch_id_column(&pool).await? {
                return Err(JournalError::Corrupt(
                    "the batch_id column is still missing after migrating",
                ));
            }
        }
        // And `undoes_seq`'s, which is OLDER. It goes before the format
        // marker below because that marker is a row, i.e. an INSERT that
        // names the column.
        //
        // But ONLY if the table is empty, and this is the difference with
        // `batch_id`: that column was added without touching the hash's
        // preimage (`None` feeds nothing, see `chain_hash`), and this one
        // arrived TOGETHER with its presence byte. A row written before was
        // hashed over a preimage that ended at `reversal_ref`; recomputing it
        // today gives a different digest. In other words: migrating a DB
        // WITH history leaves it writable and `verify_chain` declares it
        // broken at its first row — a FALSE accusation of tampering on a file
        // nobody touched, which is exactly what the format marker (ADR 0046)
        // exists to avoid producing — and with no possible fix, because those
        // rows can no longer be rehashed.
        //
        // So it is refused, and it says what to do. The embedded backend will
        // see it as `NoJournal::Failed` and keep going without logging
        // (#167); the daemon will not start, which for an unreadable journal
        // is the correct behavior.
        if !has_column(&pool, "undoes_seq").await? {
            let rows: i64 = sqlx::query("SELECT COUNT(*) FROM journal")
                .fetch_one(&pool)
                .await?
                .try_get(0)?;
            if rows != 0 {
                return Err(JournalError::Corrupt(
                    "journal older than undoes_seq and WITH history: migrating it would make \
                     verify_chain declare it broken at its first row (those rows were \
                     hashed over a preimage without that column). Export it with a binary \
                     of its own era, archive it, and let a new one be created",
                ));
            }
            sqlx::query(MIGRATE_UNDOES_SEQ).execute(&pool).await?;
            if !has_column(&pool, "undoes_seq").await? {
                return Err(JournalError::Corrupt(
                    "the undoes_seq column is still missing after migrating",
                ));
            }
        }
        Self::stamp_format_if_new(&pool).await?;
        Self::warn_if_format_unknown(&pool).await?;
        // WITHOUT a `seq` filter ON PURPOSE (and it is not an oversight that
        // "unifying" with the mutation reads would fix): the first mutation
        // of a freshly created journal has to chain onto the MARKER at `seq`
        // 0. If this query skipped it, it would be born chained to the zero
        // hash and the chain would be broken starting from entry 1.
        let (last_seq, last_hash) =
            sqlx::query("SELECT seq, entry_hash FROM journal ORDER BY seq DESC LIMIT 1")
                .fetch_optional(&pool)
                .await?
                .map_or(Ok((0i64, [0u8; 32])), |row| {
                    // `try_get`: a hostile blob here would panic the STARTUP
                    // of the journal's owner. It fails with a typed error,
                    // which is what rule 6 asks for and what ADR 0046 §5
                    // assumes when reasoning about availability.
                    let seq: i64 = row.try_get(0)?;
                    let v: Vec<u8> = row.try_get(1)?;
                    if v.len() != 32 {
                        return Err(JournalError::Corrupt("entry_hash is not 32 bytes long"));
                    }
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&v);
                    Ok((seq, h))
                })?;
        // The batch counter starts from the HIGHEST already written: reopening
        // never reuses an id some entry already carries.
        // `try_get`: a hostile blob in the column would make `get` panic, and
        // in this file a panic is a verdict that is never issued.
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
                "the journal declares a format this binary does not know: its new entries \
                 are written with this format's rules and a newer version will see them \
                 as broken (ADR 0046)"
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

    /// Records a NORMAL mutation (compensates no undo). `seq` is assigned
    /// monotonically INSIDE the chain's lock (together with chaining) → `seq`
    /// order == hash order. Returns the assigned `seq`. If the insert fails,
    /// neither `seq` nor `last_hash` advance (no gaps, no broken chain).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] on insert.
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

    /// Like [`Self::record`] but sets `undoes_seq` = the `seq` this entry
    /// COMPENSATES (undo M3-2). `None` for normal mutations. `undoes_seq` goes
    /// into the hash chain (it stays tamper-evident).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] on insert.
    #[expect(
        clippy::too_many_arguments,
        reason = "every field of a journal entry, in the table's order"
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

    /// Hands out a fresh batch id. Monotonic and race-free: the counter lives
    /// in the chain state, UNDER THE SAME LOCK that assigns `seq`, so two
    /// concurrent batch tasks in the same daemon cannot share it (a `SELECT
    /// MAX(batch_id) + 1` could let them). On reopening, the counter starts
    /// from the highest written, so it is not reused across startups either.
    ///
    /// An id handed out and never used (the task died before the first step)
    /// is simply lost: ids are grouping labels, not an auditable counter.
    ///
    /// # Errors
    /// Never fails today; the `Result` is kept so that persisting the counter
    /// later does not change the signature.
    pub async fn alloc_batch(&self) -> Result<i64, JournalError> {
        let mut chain = self.chain.lock().await;
        chain.batch_counter += 1;
        Ok(chain.batch_counter)
    }

    /// Records ONE entry. This is the real body: [`Self::record`] and
    /// [`Self::record_undoing`] are wrappers over it. `seq` is assigned INSIDE
    /// the chain's lock (together with chaining) → `seq` order == hash order.
    /// If the insert fails, neither `seq` nor `last_hash` advance.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`] on insert.
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

        // Only after a successful insert: no seq gaps or broken chain on failure.
        chain.last_seq = seq;
        chain.last_hash = entry_hash;
        drop(chain);
        // And AFTER it is durable, to the hooks (ADR 0100): what a hook sees
        // is exactly what the journal recorded. `offer` never waits.
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

    /// Installs the endpoint every committed row is offered to (ADR 0100).
    /// The second one to install overwrites the first: there is one dispatcher
    /// per process, set at startup.
    pub fn set_hook_sender(&self, tx: crate::hooks::HookSender) {
        *self
            .hooks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    }

    /// Number of MUTATIONS (the format marker at `seq 0` is not one).
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

    /// Chain head: `(seq, entry_hash)` of the last MUTATION (`None` if there
    /// is none). It is what an HMAC anchor signs (ADR 0025).
    ///
    /// **This `head` does NOT return the format marker (`seq 0`, ADR 0046),
    /// and deliberately still does not**: `seq` 0 also does not come out
    /// through [`Journal::entries`], so a HEAD anchor pointing there would be
    /// read by the audit as "the anchored seq no longer exists" — a false
    /// accusation of truncation.
    ///
    /// The coverage a head anchor gives the marker is TRANSITIVE, with one
    /// caveat: every mutation chains onto the marker's hash, so re-declaring
    /// the format and RECOMPUTING the tail changes every stored hash and no
    /// prior anchor matches. But that holds for whoever can recompute the
    /// chain, and a verifier that has already said
    /// [`ChainStatus::UnknownFormat`] is exactly the one that cannot: for it,
    /// the head anchor does not cover the marker. A re-declaration that does
    /// NOT recompute the tail (three writes) moves no stored hash and the
    /// head anchors still match — see [`Journal::verify_chain`].
    ///
    /// **What DOES cover it is an anchor of the marker'S OWN** (#146), with
    /// its digest taken from [`Journal::marker_hash`] and in its own file.
    /// That is why this method has not had to change: the marker's anchor
    /// does not go through here.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] if the stored hash is
    /// not 32 bytes long.
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
            .map_err(|_| JournalError::Corrupt("head's entry_hash is not 32 bytes long"))?;
        Ok(Some((seq, hash)))
    }

    /// Hash of MUTATION `seq` (`None` if it does not exist). The audit
    /// cross-checks it against each anchor AFTER an `Intact`
    /// [`Journal::verify_chain`] (ADR 0025): with the chain verified, the
    /// stored hash IS the recomputed one.
    ///
    /// The format marker (`seq` 0) is left out, as in [`Journal::head`] and
    /// [`Journal::entries`], and an answer here for a `seq` that the rest of
    /// the audit says does not exist would be an inconsistency waiting for
    /// someone to use it.
    ///
    /// **Its digest is requested through [`Journal::marker_hash`]**, which is
    /// the narrow gate #146 opened to anchor it — not by relaxing this one.
    /// The two coexist because they serve different mechanisms: HEAD anchors
    /// are checked against this filter, and the MARKER's against that one, in
    /// its own file.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] if the blob is not 32
    /// bytes long.
    pub async fn entry_hash_at(&self, seq: i64) -> Result<Option<[u8; 32]>, JournalError> {
        let row = sqlx::query("SELECT entry_hash FROM journal WHERE seq = ? AND seq >= 1")
            .bind(seq)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let blob: Vec<u8> = row.try_get(0)?;
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("entry_hash is not 32 bytes long"))?;
        Ok(Some(hash))
    }

    /// Digest of the FORMAT MARKER (`seq 0`, ADR 0046), or `None` if this
    /// journal does not carry one (it was created before the marker existed).
    ///
    /// Its own accessor, and not a relaxation of [`Journal::entry_hash_at`]'s
    /// filter: that filter is set on purpose because an answer there would
    /// contradict [`Journal::head`] and [`Journal::entries`], which do not
    /// return `seq` 0 — and an anchor cannot point at a `seq` that the rest of
    /// the audit says does not exist. What is needed is the opposite: ONE
    /// gate, narrow and named, for the one thing the marker actually wants.
    ///
    /// # Why it exists (#146)
    /// ADR 0046 granted a hole: re-declaring the format costs THREE column
    /// writes and no key — set the version, refresh the marker's digest
    /// (which is keyless and publicly computable), rechain `seq 1` — and it
    /// turns a `Broken { first_bad_seq: k }` verdict into `UnknownFormat`,
    /// with a version of `u32::MAX` so that no future binary says otherwise.
    /// The alarm survives; the BLAME does not. And ADR 0025's anchors do not
    /// close it, contrary to how it looks: the edit moves no `entry_hash` at
    /// `seq >= 1`, so all of them still match.
    ///
    /// With this, `norte audit anchor` also signs the marker, and an
    /// inconsistent re-declaration comes out as
    /// [`AnchorVerdict::HashMismatch`](crate::audit::AnchorVerdict::HashMismatch)
    /// **at `seq` 0** — located, and with a key behind it.
    ///
    /// # It is the STORED digest, not a recomputed one
    /// Like [`Journal::entry_hash_at`], and under the same condition for it to
    /// mean anything: check it AFTER a [`Journal::verify_chain`] that has
    /// validated the marker — `Intact` or `UnknownFormat`, the two arms that
    /// are only reached if the `seq` 0 row is canonical and its hash
    /// recomputes. Under `Broken` this value is whatever the file says.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`]; [`JournalError::Corrupt`] if the blob is not 32
    /// bytes long.
    pub async fn marker_hash(&self) -> Result<Option<[u8; 32]>, JournalError> {
        let row = sqlx::query("SELECT entry_hash FROM journal WHERE seq = ?")
            .bind(FORMAT_SEQ)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else { return Ok(None) };
        let blob: Vec<u8> = row.try_get(0)?;
        let hash: [u8; 32] = blob
            .try_into()
            .map_err(|_| JournalError::Corrupt("the marker's entry_hash is not 32 bytes long"))?;
        Ok(Some(hash))
    }

    /// Dumps every entry in `seq` order. Materializes in memory: meant for
    /// session-sized journals (pagination is debt if it grows — same
    /// criterion as the listing, #27). Read base for undo (M3-2) and the
    /// audit export (M3-5).
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn entries(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let rows = sqlx::query(self.pick(SELECT_ENTRIES, SELECT_ENTRIES_NO_BATCH))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_entry).collect()
    }

    /// A backward PAGE of entries, from newest to oldest (phase 7): those
    /// before `before_seq` — `None` = from the last one — at most `limit`,
    /// and only the ones from `actor_kind` if one is given.
    ///
    /// Unlike [`Self::entries`], which brings back the WHOLE journal and is
    /// for auditing, this is what a screen reads: bounded by construction,
    /// because a journal of months does not fit in anyone's memory.
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

    /// Like [`Self::revertible_for`], but only what comes AFTER `after_seq`
    /// (phase 7, base of [`crate::Engine::undo_after`]).
    ///
    /// The `after_seq` entry does NOT go in: it is the point to return to,
    /// not the first victim. `upto_seq` is the ceiling (0.80.0): nothing
    /// above it, nor any batch with an entry above it. `None` = no ceiling.
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

    /// The REVERTIBLE entries of session `actor`, in LIFO order (`seq` DESC):
    /// normal mutations (`undoes_seq IS NULL`) from that actor whose
    /// compensation either does not exist or has itself been undone. Base of
    /// [`crate::Engine::undo_session`] (M3-2).
    ///
    /// "Compensated" means LIVE-compensated, not "compensated at some point":
    /// a batch undo that fails halfway undoes its own compensations, and the
    /// batch has to become undoable again. The why, with the chain's exact
    /// shape, is on the `SELECT_REVERTIBLE` constant.
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

    /// Which of `seqs` are undone RIGHT NOW: they have a live compensation,
    /// under the same condition (`COL_UNDONE`) the revertible selection uses.
    ///
    /// This is the re-check of an undo JUST BEFORE executing a unit (#358):
    /// what was chosen when the undo was requested may have been undone,
    /// meanwhile, by another undo — a double click, two frontends, a retry
    /// after a timeout.
    ///
    /// Asks in chunks: a sync unit can have half a million entries, and
    /// `SQLite` caps a query's parameters.
    ///
    /// # Errors
    /// [`JournalError::Sqlx`].
    pub async fn undone_among(&self, seqs: &[i64]) -> Result<Vec<i64>, JournalError> {
        const CHUNK: usize = 500;
        let mut undone = Vec::new();
        for chunk in seqs.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql =
                format!("SELECT seq FROM journal WHERE seq IN ({placeholders}) AND {COL_UNDONE}");
            let mut q = sqlx::query_scalar::<_, i64>(&sql);
            for s in chunk {
                q = q.bind(s);
            }
            undone.extend(q.fetch_all(&self.pool).await?);
        }
        Ok(undone)
    }

    /// TESTS ONLY: corrupts an entry's `path` without recomputing its hash.
    #[cfg(test)]
    async fn corrupt_path_for_test(&self, seq: i64, path: &[u8]) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET path = ? WHERE seq = ?")
            .bind(path)
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// TESTS ONLY: changes an entry's `batch_id` without recomputing its hash
    /// (simulates an attacker UNGROUPING a batch, or inventing one for a
    /// standalone mutation).
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

    /// TESTS ONLY: sets an `entry_hash` of invalid length (simulates on-disk
    /// corruption) to test the startup guard.
    #[cfg(test)]
    async fn set_short_hash_for_test(&self, seq: i64) -> Result<(), JournalError> {
        sqlx::query("UPDATE journal SET entry_hash = ? WHERE seq = ?")
            .bind(&b"short"[..])
            .bind(seq)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// [`Journal`] as a [`crate::observer::MutationObserver`]: maps every
/// `Mutation` to an entry. `seq` is assigned by [`Journal`] itself under its
/// own lock. A logical trashing carries its recoverable destination over to
/// `reversal_ref` (M3-1b).
pub struct SqliteJournal {
    journal: Journal,
}

impl SqliteJournal {
    /// Wraps an already-open journal.
    #[must_use]
    pub fn new(journal: Journal) -> Self {
        Self { journal }
    }

    /// The underlying [`Journal`] (read access for audit/undo/tests).
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// See [`Journal::set_hook_sender`].
    pub fn set_hook_sender(&self, tx: crate::hooks::HookSender) {
        self.journal.set_hook_sender(tx);
    }

    /// Closes the underlying journal and waits for the file to become free
    /// (see [`Journal::close`]).
    pub async fn close(self) {
        self.journal.close().await;
    }

    /// Opens (or creates) the journal at `path` and wraps it as an observer,
    /// ready for [`crate::Engine::with_observer`]. Creates the containing
    /// directory if it is missing.
    ///
    /// A SINGLE WRITER (spec §4): the hash chain assumes one owning process.
    /// Several processes writing the SAME file would fork the chain and
    /// collide on `seq`, and that is why it is not a convention:
    /// [`Journal::open`] takes `SQLite`'s EXCLUSIVE lock, so the second one to
    /// arrive fails to open instead of sharing.
    ///
    /// Who that owner is is no longer always the daemon: since #167 an
    /// embedded process (a TUI, or a `norte cp` with no daemon) opens this
    /// same file — see [`crate::embedded::LazyJournal`], which decides what
    /// to do when someone else already holds the lock, which since #177 it
    /// does not open until the first mutation (so a session that only
    /// browses does not take it from anyone) and which since #179 retries it
    /// and knows how to release it.
    ///
    /// # Errors
    /// [`JournalError::Io`] if it cannot create the containing directory;
    /// [`JournalError`] on opening/creating the DB (see [`Journal::open`]).
    pub async fn open(path: &std::path::Path) -> Result<Self, JournalError> {
        Self::open_with_busy_timeout(path, DEFAULT_BUSY_TIMEOUT).await
    }

    /// Like [`SqliteJournal::open`], with the lock's wait deadline from
    /// [`Journal::open_with_busy_timeout`].
    ///
    /// # Errors
    /// The same as [`SqliteJournal::open`].
    pub async fn open_with_busy_timeout(
        path: &std::path::Path,
        busy_timeout: std::time::Duration,
    ) -> Result<Self, JournalError> {
        // The config dir may not exist on first startup; SQLite creates the
        // FILE (create_if_missing) but not its parent directory.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(Self::new(
            Journal::open_with_busy_timeout(path, busy_timeout).await?,
        ))
    }
}

/// How a node's identity is written into `reversal_ref` (ADR 0152).
///
/// Text and not raw bytes because dumping a row with `sqlite3` during an
/// investigation shows `12:34567` and not four opaque bytes. It does not go
/// out over the wire nor through the audit export, so nobody else reads it.
///
/// Lives here, paired with [`footprint_to_node`], so that whoever writes it and
/// whoever compares it cannot diverge: the day this changes shape, it changes
/// in one place and the parser right next to it comes along.
pub(crate) fn node_footprint(n: &norte_vfs::NodeId) -> String {
    format!("{}:{}", n.volume, n.index)
}

/// The inverse of [`node_footprint`]. `None` = those bytes are not a
/// fingerprint.
///
/// Undo's comparison goes through [`norte_vfs::NodeId`] and not through bytes
/// precisely because of this `None`. Today nothing can leave anything else in
/// a `delete`'s `reversal_ref` — the other writers put `None`, and
/// `restore_trash`, which does store a path there, is handled in another
/// branch — but comparing strings would mean that the day something does
/// leave one, that entry would NEVER match and would be stuck forever. By
/// parsing, a value that is not a fingerprint falls into "nothing to
/// compare" and the undo behaves as it did before ADR 0152, which is the
/// direction this has to fail in.
pub(crate) fn footprint_to_node(bytes: &[u8]) -> Option<norte_vfs::NodeId> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (vol, idx) = text.split_once(':')?;
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
            // `reversal_ref` carries the IDENTITY of what was created here,
            // not a path (#369, ADR 0152). The column is free-form and each
            // reversal gives it its own meaning: `restore_trash` stores the
            // recoverable destination, and `delete` stores which node was its
            // own so as not to delete another one.
            Mutation::Created { path, node } => (
                "created",
                path.to_wire().into_bytes(),
                None,
                Reversal::Delete,
                node.map(|n| node_footprint(&n).into_bytes()),
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
            // Logical trash → `dest` is the recoverable path (reversal_ref).
            // Native trash/"vanish" → `dest` None (handled in the undo, M3-2).
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
            // #314: the reversal IS the previous mode, and it travels in
            // `reversal_ref` in ASCII decimal. Without it there is no way back
            // to promise, and the entry says so — `Irreversible` with its
            // reason — instead of offering an undo that would set a mode
            // nobody ever had. The NEW mode goes in `path_to` so the journal
            // can be read without guessing what was set.
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
                // The batch of a recursive op (#315): n entries that were ONE
                // human action.
                *batch,
            ),
        };
        // The error is PROPAGATED (rule 4): the op is not considered complete
        // if its journal entry did not become durable. The detail goes
        // through tracing.
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
                tracing::error!(error = %e, "failed to write the journal");
                ProtoError::from(e)
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0100: every committed row is offered to the hooks endpoint, with
    /// what the row says — and only after the insert, with its `seq`.
    #[tokio::test]
    async fn every_committed_row_is_offered_to_the_hooks() {
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
        let ev = rx.try_recv().expect("one event per row");
        assert_eq!(ev.seq, seq);
        assert_eq!(ev.op, "renamed");
        assert_eq!(
            ev.actor_kind, "agent",
            "the class does; the session does not travel"
        );
        assert_eq!(ev.path, b"file:///a/nuevo".to_vec());
        assert_eq!(ev.path_to, Some(b"file:///a/viejo".to_vec()));
        assert_eq!(ev.batch_id, Some(3));

        // Full queue: the row is written all the same and the event counts as
        // dropped. A slow observer never holds up a mutation.
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
        assert_eq!(tx.dropped(), 1, "two fit, the third was dropped");
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
        assert_eq!(h1, chain_hash(&zero, &rec(1)), "deterministic");
        assert_ne!(h1, chain_hash(&h1, &rec(1)));
        assert_ne!(h1, chain_hash(&zero, &rec(2)));
    }

    /// FROZEN VECTOR of the chain, with EVERY field populated: both `Option`s
    /// present (one of them empty, to pin the presence byte), `undoes_seq`
    /// present, and paths that are NOT UTF-8.
    ///
    /// The other `chain_hash` tests are relative (`assert_ne!` between two
    /// digests) and would stay green if the length prefix went from `u64` to
    /// `u32`, from little-endian to big-endian, or if the field order
    /// changed — and any of those things invalidates `verify_chain` on EVERY
    /// journal already on disk. This is the only mechanical guardrail that
    /// holds that promise.
    ///
    /// If this goes red: do NOT update the constant. Revert the framing
    /// change, or version the chain's format and migrate the journals.
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
            // WITHOUT a batch, like every entry before the batch rename: the
            // constant below does NOT change from adding the field, and that
            // is exactly the compatibility promise.
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
        // security B1: `None` vs `Some(&[])` must give different hashes.
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
            "intact chain"
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
            "it is detected AND it cites where (B2)"
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
        // Reopening reads the last entry_hash → short → Corrupt, not a panic.
        assert!(matches!(
            Journal::open(&path).await,
            Err(JournalError::Corrupt(_))
        ));
    }

    /// The hash chain's single-writer requirement is a MECHANISM (MAJOR-1
    /// security M3-4): while a process has the journal open, a second `open`
    /// of the SAME file fails — never two writers forking the chain (e.g. two
    /// daemons with different sockets and the same config dir).
    #[tokio::test]
    async fn second_open_of_live_journal_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        let live = Journal::open(&path).await.expect("first open");
        assert!(
            matches!(Journal::open(&path).await, Err(JournalError::Sqlx(_))),
            "the exclusive lock rejects the second writer"
        );
        // Releasing the first one frees the lock: reopening works again.
        drop(live);
        let _ = Journal::open(&path).await.expect("reopen after drop");
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
        assert_eq!(seq, 3, "the seq continues after reopening");
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
        // seq assigned under the lock → chain consistent despite 32 concurrent.
        assert_eq!(obs.journal.count().await.expect("count"), 32);
        assert!(
            obs.journal
                .verify_chain()
                .await
                .expect("verify")
                .is_intact(),
            "no false-tampered under concurrency (security M1)"
        );
    }

    #[tokio::test]
    async fn sqlite_journal_open_creates_missing_parent_dir() {
        use crate::observer::{Mutation, MutationObserver};
        use norte_proto::VPath;

        let dir = tempfile::tempdir().expect("tempdir");
        // The containing dir does NOT exist yet (owner's first startup).
        let path = dir.path().join("state/journal.db");
        let j = SqliteJournal::open(&path)
            .await
            .expect("open creates the parent");
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
        // Non-UTF-8 basename (0xFF 0xFE): exercises the wire's percent-encoding
        // branch, exactly where a lossy bug (rule 1) would hide.
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

        let entries = obs.journal.entries().await.expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].op, "trashed");
        assert_eq!(entries[0].reversal, "restore_trash");
        let stored = entries[0]
            .reversal_ref
            .as_deref()
            .expect("logical trash: there is a reversal_ref");
        // REAL round-trip (not tautological): `VPath::parse` is the inverse of
        // `to_wire`; if the wire lost the hostile bytes, it would reconstruct a
        // different VPath and this assert would fail.
        let roundtrip =
            VPath::parse(std::str::from_utf8(stored).expect("wire is ASCII")).expect("parse");
        assert_eq!(
            roundtrip, dest,
            "reversal_ref round-trips byte-exact (rule 1)"
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
        let entries = obs.journal.entries().await.expect("entries");
        assert_eq!(entries[0].reversal, "restore_trash");
        assert_eq!(
            entries[0].reversal_ref, None,
            "native trash: no stable path"
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

        let entries = j.entries().await.expect("entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[0].op, "created");
        assert_eq!(entries[0].path, b"file:///a");
        assert_eq!(entries[0].reversal, "delete");
        assert_eq!(entries[1].op, "renamed");
        assert_eq!(entries[1].path, b"file:///b");
        assert_eq!(entries[1].path_to.as_deref(), Some(&b"file:///a"[..]));
        assert_eq!(entries[1].reversal, "rename_back");
    }

    #[tokio::test]
    async fn revertible_for_excludes_compensated_and_foreign_actor() {
        let j = Journal::open_in_memory().await.expect("open");
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        // seq 1: the agent creates A. seq 2: the user creates B (another actor).
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
        // seq 3: compensation of 1 (undoes_seq=1) → 1 stops being revertible.
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
            "1 is already compensated; 2 belongs to another actor"
        );

        let rev_user = j
            .revertible_for(&Actor::User)
            .await
            .expect("revertible user");
        assert_eq!(rev_user.len(), 1);
        assert_eq!(rev_user[0].seq, 2);
        assert_eq!(rev_user[0].undoes_seq, None);
    }

    /// A compensation that was itself UNDONE does not cover its original: the
    /// mutation becomes revertible again.
    ///
    /// This is the shape produced by a batch undo that fails halfway: the
    /// executor undoes the undo steps it had already applied and journals
    /// that reversal as a compensation of the compensation (`O ← C ← D`). The
    /// tree ends up with the batch applied, so saying "already undone" would
    /// leave it undoable-never-again, silently.
    #[tokio::test]
    async fn a_compensation_that_was_itself_undone_reopens_its_entry() {
        let j = Journal::open_in_memory().await.expect("open");
        // seq 1: the original mutation.
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
        // seq 2: its compensation → 1 stops being revertible.
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
        // seq 3: the compensation gets UNDONE (the batch undo fell over and
        // the executor reverted it) → 1 is pending again.
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
            "the compensation no longer counts, so 1 is still to be undone",
        );
        // And the undo's re-check (#358) reads the SAME condition: 1 is not
        // undone. If they diverged, an undo in progress would skip what the
        // selection just offered, or the other way around.
        assert!(
            j.undone_among(&[1]).await.expect("undone").is_empty(),
            "with the compensation undone, 1 does not count as undone"
        );
    }

    /// `undone_among` says which of the requested ones have a LIVE
    /// compensation, and asks in chunks: with more seqs than the chunk size,
    /// it still answers in full.
    #[tokio::test]
    async fn undone_among_returns_the_live_compensations_and_crosses_chunks() {
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
            .expect("mutation");
        }
        // seq 4 compensates 2.
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
        .expect("compensation");
        assert_eq!(j.undone_among(&[1, 2, 3]).await.expect("undone"), vec![2]);
        // More than one chunk (500), with the compensated one at the end: the
        // one that matters is not lost at the seam.
        let mut many: Vec<i64> = (10_000..10_600).collect();
        many.push(2);
        assert_eq!(j.undone_among(&many).await.expect("undone"), vec![2]);
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
            "LIFO order (DESC)"
        );
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "chain intact with undoes_seq"
        );
    }

    /// M3-5 (ADR 0025): TAIL truncation passes `verify_chain` (a keyless
    /// weakness PINNED here on purpose) — and the HMAC anchor detects it.
    #[tokio::test]
    async fn anchor_detects_tail_truncation_the_chain_does_not_see() {
        let j = Journal::open_in_memory().await.expect("open");
        for w in [&b"file:///a"[..], b"file:///b", b"file:///c"] {
            j.record("created", w, None, Reversal::Delete, None, &Actor::User)
                .await
                .expect("rec");
        }
        let (seq, head) = j.head().await.expect("head").expect("not empty");
        assert_eq!(seq, 3);
        assert_eq!(
            j.entry_hash_at(seq).await.expect("hash_at"),
            Some(head),
            "head() and entry_hash_at agree"
        );
        let key = [7u8; 32];
        let line = crate::audit::anchor_line(&key, &crate::audit::Anchor { seq, head });

        // ATTACK: the attacker deletes the last entry (tail rollback).
        sqlx::query("DELETE FROM journal WHERE seq = 3")
            .execute(&j.pool)
            .await
            .expect("delete");
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "keyless does NOT see the tail truncation (that is why anchors exist)"
        );
        // The anchor does: the anchored seq no longer exists.
        let at = j.entry_hash_at(seq).await.expect("hash_at");
        assert_eq!(
            crate::audit::verify_anchor_line(&key, &line, at),
            crate::audit::AnchorVerdict::MissingSeq(crate::audit::Anchor { seq, head })
        );
    }

    /// The journal's schema BEFORE `batch_id` existed, copied exactly as it
    /// shipped. The compatibility tests create the DB with THIS text: if the
    /// `SCHEMA` above changes, they keep describing the disk that already
    /// exists, which is what the migration is about.
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
        assert!(b.len().is_multiple_of(2), "even-length hex");
        b.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).expect("ascii"), 16).expect("hex"))
            .collect()
    }

    /// Writes into `path` a DB with the PRE-migration schema and ONE row whose
    /// `entry_hash` is the frozen constant from `the_chain_hash_is_frozen` —
    /// i.e. a hash computed by the code BEFORE this task, which nothing in
    /// this test recomputes.
    async fn write_pre_migration_journal(path: &std::path::Path) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    // WAL as `Journal::open` left it back then: a REAL
                    // pre-migration journal is in WAL, and the read-only
                    // handle could not change the mode (that is writing).
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .expect("old pool");
        sqlx::query(SCHEMA_BEFORE_BATCH_ID)
            .execute(&pool)
            .await
            .expect("old schema");
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
        // The SAME hex `the_chain_hash_is_frozen` pins, duplicated on purpose:
        // this test must not be "fixable" by touching that constant.
        .bind(unhex("b00a2da6db1199742aa42f4811370bf02fcc21a294d77741ae7a26ad2b794ecc"))
        .execute(&pool)
        .await
        .expect("insert from the pre-batch era");
        pool.close().await;
    }

    /// THE test for the hash rule: a journal written BEFORE `batch_id` existed
    /// still verifies after migrating it. Its row carries an `entry_hash` from
    /// the earlier era; if `None` fed anything (even a mere presence byte),
    /// `verify_chain` would scream "tampered" over a database nobody touched.
    #[tokio::test]
    async fn a_pre_migration_journal_still_verifies_and_keeps_chaining() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("viejo.db");
        write_pre_migration_journal(&path).await;

        let j = Journal::open(&path).await.expect("open migrates the DB");
        assert_eq!(
            j.verify_chain().await.expect("verify"),
            ChainStatus::Intact { entries: 1 },
            "migration cannot break a chain already written"
        );
        let entries = j.entries().await.expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].batch_id, None, "the migrated row has no batch");

        // And the chain KEEPS GOING from there: the new entry chains onto the
        // inherited head and the whole chain verifies again.
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
            .expect("record after migrating");
        assert_eq!(seq, 8, "the seq continues from the inherited row");
        assert!(j.verify_chain().await.expect("verify").is_intact());
        assert_eq!(
            j.alloc_batch().await.expect("alloc"),
            1,
            "with no prior batches, the counter starts at 1"
        );
    }

    /// The schema from BEFORE `undoes_seq` existed (the column pointing at the
    /// entry an undo compensates, M3-2). It is older than
    /// [`SCHEMA_BEFORE_BATCH_ID`], and files like this exist on disk.
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

    /// A journal older than `undoes_seq` is MIGRATED on open, like the
    /// `batch_id` one.
    ///
    /// Without this, `open` succeeds — `CREATE TABLE IF NOT EXISTS` does not
    /// alter a table that already exists — and it is every WRITE that blows up
    /// with "table journal has no column named `undoes_seq`". Found live
    /// (#167): an embedded `norte cp` against a journal from that era aborted
    /// with "internal error" and copied nothing. `batch_id` already had its
    /// migration; this column was added without its own.
    #[tokio::test]
    async fn a_journal_older_than_undoes_seq_is_migrated_and_accepts_writes() {
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
                .expect("old pool");
            sqlx::query(SCHEMA_BEFORE_UNDOES_SEQ)
                .execute(&pool)
                .await
                .expect("prehistoric schema");
            pool.close().await;
        }

        let j = Journal::open(&path).await.expect("open migrates the DB");
        j.record(
            "created",
            b"file:///nuevo",
            None,
            Reversal::Delete,
            None,
            &Actor::User,
        )
        .await
        .expect("writing to a migrated DB");
        assert!(
            j.verify_chain().await.expect("verify").is_intact(),
            "migrating does not break the chain"
        );
    }

    /// The exclusive lock belongs to THE CONNECTION, so the pool must not
    /// recycle it.
    ///
    /// `sqlx`'s defaults — `min_connections=0`, `idle_timeout=10min`,
    /// `max_lifetime=30min` — raise a reaper that closes the idle connection,
    /// and the lock goes with it: the process keeps believing it is the
    /// owner, another one comes in, and this one's in-memory `ChainState`
    /// collides with the `seq` PK on its next mutation… and on every one
    /// after. Pinned via the options and not via the clock: waiting ten
    /// minutes in the suite is not a test.
    #[tokio::test]
    async fn the_pool_does_not_recycle_the_connection_holding_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let j = Journal::open(&dir.path().join("j.db")).await.expect("open");
        let opts = j.pool.options();
        assert_eq!(opts.get_max_connections(), 1, "a single writer");
        assert_eq!(
            opts.get_min_connections(),
            1,
            "at zero, the reaper could leave the pool empty and release the lock"
        );
        assert_eq!(opts.get_idle_timeout(), None, "idle is still the owner");
        assert_eq!(
            opts.get_max_lifetime(),
            None,
            "recycling the connection is recycling the lock"
        );
    }

    /// A journal older than `undoes_seq` WITH history is NOT migrated: it is
    /// refused.
    ///
    /// Migrating it would leave it writable and `verify_chain` would declare
    /// it broken at its first row, because those rows were hashed over a
    /// preimage that did not carry the column (`01e3cf8` added both things at
    /// once). A FALSE accusation of tampering on a file nobody touched, and
    /// with no fix: those rows can no longer be rehashed.
    #[tokio::test]
    async fn a_journal_older_than_undoes_seq_with_rows_is_refused() {
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
                .expect("old pool");
            sqlx::query(SCHEMA_BEFORE_UNDOES_SEQ)
                .execute(&pool)
                .await
                .expect("prehistoric schema");
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
            .expect("row from the pre-undoes_seq era");
            pool.close().await;
        }

        let Err(err) = Journal::open(&path).await else {
            panic!("a pre-undoes_seq DB WITH rows must not be migratable")
        };
        assert!(
            matches!(err, JournalError::Corrupt(m) if m.contains("undoes_seq")),
            "and it says why: {err}"
        );

        // And the audit does not read it blindly either: it says what it is,
        // instead of dropping a raw "no such column".
        let Err(ro) = Journal::open_read_only(&path).await else {
            panic!("the audit cannot read it either")
        };
        assert!(
            matches!(ro, JournalError::Corrupt(m) if m.contains("undoes_seq")),
            "{ro}"
        );
    }

    /// `open` is the migration path; `open_read_only` (audit, M3-5) CANNOT do
    /// `ALTER TABLE`, so it has to READ a pre-migration DB without blowing up
    /// with "no such column".
    #[tokio::test]
    async fn a_pre_migration_journal_is_readable_read_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("viejo-ro.db");
        write_pre_migration_journal(&path).await;

        let ro = Journal::open_read_only(&path).await.expect("open ro");
        let entries = ro.entries().await.expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].batch_id, None);
        assert!(
            ro.verify_chain().await.expect("verify").is_intact(),
            "the old chain verifies the same way read-only"
        );
        assert_eq!(
            ro.revertible_for(&Actor::User)
                .await
                .expect("revertible")
                .len(),
            0,
            "the row belongs to an agent, not the user"
        );
    }

    /// FROZEN VECTOR for the NEW field: the same record as
    /// `the_chain_hash_is_frozen` but WITH a batch. Pins the other half of the
    /// rule (presence byte + length-prefixed id, right at the end). If this
    /// goes red, you have changed `batch_id`'s framing and invalidated the
    /// chain of journals that already use it.
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

    /// A batch id is monotonic and not reused, neither between two
    /// allocations with no insert in between nor across reopening the
    /// journal.
    #[tokio::test]
    async fn batch_ids_are_monotonic_across_allocs_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("j.db");
        let used;
        {
            let j = Journal::open(&path).await.expect("open");
            let a = j.alloc_batch().await.expect("alloc");
            let b = j.alloc_batch().await.expect("alloc");
            assert_eq!(b, a + 1, "monotonic with no insert in between");
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
            used = b;
        }
        let j = Journal::open(&path).await.expect("reopen");
        assert!(
            j.alloc_batch().await.expect("alloc") > used,
            "after reopening, an already-written batch is never repeated"
        );
    }

    /// Two concurrent tasks in the same daemon NEVER share a batch (a
    /// `MAX(batch_id) + 1` could).
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
        let mut seen = HashSet::new();
        for h in handles {
            let id = h.await.expect("join").expect("alloc");
            assert!(seen.insert(id), "repeated batch id: {id}");
        }
        assert_eq!(seen.len(), 32);
    }

    /// The batch is part of the chain: stripping it from a row breaks
    /// `verify_chain` right there (an attacker cannot UNGROUP a batch).
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

    /// And in the other direction: INVENTING a batch for a standalone entry
    /// also breaks the chain. Compatibility with the old is not a loophole.
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

    /// The read-only handle (audit) reads a REAL batch from the DB and does
    /// not hand out already-used ids: its counter starts from the highest
    /// written.
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
            "the chain with a batch verifies the same way read-only"
        );
        assert!(
            ro.alloc_batch().await.expect("alloc") > batch,
            "never an id already on disk"
        );
    }

    /// The reader exposes the batch, which is what lets undo consume the
    /// whole group as ONE unit.
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
        let entries = j.entries().await.expect("entries");
        assert_eq!(entries[0].batch_id, Some(batch));
        let rev = j.revertible_for(&Actor::User).await.expect("revertible");
        assert_eq!(rev[0].batch_id, Some(batch));
        // A batch does NOT cross the actor boundary: whoever undoes by group
        // still has to filter by actor, never by `batch_id` alone.
        assert!(
            j.revertible_for(&Actor::Agent {
                session: "s1".into()
            })
            .await
            .expect("revertible agent")
            .is_empty(),
            "the batch belongs to the user, not the agent"
        );
    }

    /// A standalone rename (the `fs.move` path) still has no batch; one from a
    /// batch carries it through to the row.
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
        .expect("standalone");
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
        .expect("in a batch");
        let entries = obs.journal().entries().await.expect("entries");
        assert_eq!(
            entries[0].batch_id, None,
            "a standalone rename does not invent a batch"
        );
        assert_eq!(entries[1].batch_id, Some(batch));
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

    /// **#146: anchoring the marker closes the hole ADR 0046 granted.**
    ///
    /// The attack is three column writes and no key: set the version, refresh
    /// the marker's digest (keyless and publicly computable), and rechain
    /// `seq 1`. It turns a `Broken { first_bad_seq: k }` into `UnknownFormat`,
    /// and with a version of `u32::MAX` no future binary will say otherwise:
    /// the alarm survives, the BLAME does not.
    ///
    /// HEAD anchors do not catch it, and this is the half of the test that
    /// matters — checked explicitly below: the edit moves no `entry_hash` at
    /// `seq >= 1`, so the head anchor STILL matches. It is the gap between the
    /// two mechanisms: `verify_chain` catches inconsistent edits, anchors
    /// catch consistent ones, and this was an inconsistent one whose verdict
    /// had been diverted.
    #[tokio::test]
    async fn a_format_redeclaration_breaks_the_markers_anchor() {
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

        // `norte audit anchor`: the marker AND the head.
        let key = b"test anchor key";
        let marker = j
            .marker_hash()
            .await
            .expect("marker_hash")
            .expect("a new journal carries a marker");
        let marker_anchor = anchor_line(
            key,
            &Anchor {
                seq: 0,
                head: marker,
            },
        );
        let (seq, head) = j.head().await.expect("head").expect("there is a mutation");
        let head_anchor = anchor_line(key, &Anchor { seq, head });

        // BEFORE the attack: the marker's anchor MATCHES. This is the half a
        // silent regression would break — it takes only ceasing to seed
        // `seq` 0 into the audit's snapshot — and from then on EVERY anchored
        // journal would start saying "truncation" about an intact file.
        assert!(
            matches!(
                verify_anchor_line(key, &marker_anchor, j.marker_hash().await.expect("marker")),
                AnchorVerdict::Ok(_)
            ),
            "an untouched journal does not accuse itself",
        );

        // The attack, in full.
        j.redeclare_format_for_test(&u32::MAX.to_string().into_bytes(), true)
            .await
            .expect("re-declare");
        j.relink_first_entry_for_test().await.expect("relink");

        // The chain's verdict, diverted: it no longer accuses anyone.
        assert!(
            matches!(
                j.verify_chain().await.expect("verify"),
                ChainStatus::UnknownFormat { .. }
            ),
            "diverting the verdict is the attack's premise",
        );

        // And the HEAD anchor still matches, which is exactly what made "the
        // anchors already cover it" sound true when it was not.
        assert!(
            matches!(
                verify_anchor_line(
                    key,
                    &head_anchor,
                    j.entry_hash_at(seq).await.expect("head's hash"),
                ),
                AnchorVerdict::Ok(_)
            ),
            "the edit moves no entry_hash at seq >= 1",
        );

        // The marker's does not. Located at `seq` 0 and with a key behind it.
        let now = j.marker_hash().await.expect("marker_hash");
        assert_eq!(
            verify_anchor_line(key, &marker_anchor, now),
            AnchorVerdict::HashMismatch(Anchor {
                seq: 0,
                head: marker,
            }),
            "an inconsistent re-declaration is a HashMismatch at seq 0",
        );
    }

    /// **The hole THIS does not close, written in code and not only in prose.**
    ///
    /// A journal from BEFORE ADR 0046 has no marker — and by design never
    /// gains one (§6) — so when it was anchored there was nothing at `seq` 0
    /// to sign. The attack there is not re-declaring but INJECTING: putting in
    /// the `seq` 0 row and rechaining `seq` 1. The verdict is diverted just the
    /// same, and there is no prior anchor to contradict it.
    ///
    /// What does remain is a signal, and the audit states it: there is a
    /// marker and nobody anchors it.
    #[tokio::test]
    async fn a_journal_without_a_marker_has_no_anchor_to_defend_it() {
        let j = Journal::open_in_memory().await.expect("open");
        // The marker is removed, which is how journals from before ADR 0046
        // were born.
        sqlx::query("DELETE FROM journal WHERE seq = 0")
            .execute(&j.pool)
            .await
            .expect("delete the marker");
        assert_eq!(
            j.marker_hash().await.expect("marker_hash"),
            None,
            "with no marker there is no digest to anchor, and the audit writes no line"
        );
    }

    /// And the anchored marker does NOT sneak into history: `entry_hash_at`
    /// still filters `seq >= 1`, because an answer there would contradict
    /// `head` and `entries`, and an anchor cannot point at a `seq` that the
    /// rest of the audit says does not exist.
    #[tokio::test]
    async fn the_marker_has_its_own_gate_and_does_not_relax_the_mutations_gate() {
        let j = Journal::open_in_memory().await.expect("open");
        assert!(
            j.marker_hash().await.expect("marker_hash").is_some(),
            "its gate answers"
        );
        assert_eq!(
            j.entry_hash_at(0).await.expect("entry_hash_at"),
            None,
            "and the mutations' gate still does not answer for seq 0"
        );
        assert_eq!(j.head().await.expect("head"), None, "nor head");
        assert!(
            j.entries().await.expect("entries").is_empty(),
            "nor entries"
        );
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

    /// `open_read_only` (M3-5): reads the same as the write handle and
    /// REJECTS `record` (read-only by design). File-backed: covers the
    /// audit's real WAL path.
    #[tokio::test]
    async fn open_read_only_reads_and_rejects_writes() {
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
        let head_rw = j.head().await.expect("head").expect("not empty");
        drop(j);

        let ro = Journal::open_read_only(&path).await.expect("open ro");
        assert_eq!(ro.entries().await.expect("entries").len(), 1);
        assert_eq!(ro.head().await.expect("head"), Some(head_rw));
        assert!(
            ro.verify_chain().await.expect("verify").is_intact(),
            "the chain verifies the same way read-only"
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
            "record on a readonly handle MUST fail"
        );
    }

    /// Writes `n` mutations from the human and returns their `seq`.
    async fn human_mutations(j: &Journal, n: usize) -> Vec<i64> {
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

    /// The page goes from the NEWEST backward, respects the cap, and
    /// `before_seq` is STRICT: the marked entry does not come out again,
    /// which is what makes paging terminate instead of repeating a row
    /// forever.
    #[tokio::test]
    async fn the_page_goes_backward_and_before_seq_is_strict() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = human_mutations(&j, 5).await;

        let first = j.page(None, 2, None).await.expect("page");
        assert_eq!(
            first.iter().map(|e| e.entry.seq).collect::<Vec<_>>(),
            vec![seqs[4], seqs[3]],
            "the two newest, in that order"
        );

        let second = j.page(Some(seqs[3]), 2, None).await.expect("page");
        assert_eq!(
            second.iter().map(|e| e.entry.seq).collect::<Vec<_>>(),
            vec![seqs[2], seqs[1]],
            "continues below the last one served, without repeating it"
        );
    }

    /// **Paging to the end returns every entry ONCE and terminates** (found
    /// by the protocol review: the cursor rule was written twice and tested
    /// zero times).
    ///
    /// It is the whole contract in a test: no repeats, none skipped, in
    /// descending order, and `next_before_seq` at `None` exactly when nothing
    /// older is left.
    #[tokio::test]
    async fn paging_to_the_end_does_not_repeat_or_skip_anything() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = human_mutations(&j, 5).await;

        let mut seen = Vec::new();
        let mut cursor = None;
        let mut rounds = 0;
        loop {
            rounds += 1;
            assert!(rounds < 10, "the paging loop does not terminate");
            let page = j.page(cursor, 2, None).await.expect("page");
            let wire = page_to_wire(&page, 2);
            seen.extend(wire.rows.iter().map(|r| r.seq));
            match wire.next_before_seq {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        let mut expected = seqs.clone();
        expected.reverse();
        assert_eq!(seen, expected, "all of them, once, from newest to oldest");
        assert_eq!(
            rounds, 3,
            "2 + 2 + 1, and the one of 1 no longer offers a cursor"
        );
    }

    /// A page that comes back FULL right as it exhausts the journal offers a
    /// cursor, and the next round answers empty with no cursor. It is the
    /// only case where the client makes one extra round, and it is correct:
    /// the server cannot know nothing is left without looking.
    #[tokio::test]
    async fn an_exact_page_offers_a_cursor_and_the_next_one_closes() {
        let j = Journal::open_in_memory().await.expect("open");
        human_mutations(&j, 2).await;

        let first = page_to_wire(&j.page(None, 2, None).await.expect("page"), 2);
        let cursor = first.next_before_seq.expect("the page came back full");

        let second = page_to_wire(&j.page(Some(cursor), 2, None).await.expect("page"), 2);
        assert!(second.rows.is_empty());
        assert_eq!(second.next_before_seq, None, "and it closes there");
    }

    /// Filtering by actor class leaves the others out — and not filtering
    /// brings all of them.
    #[tokio::test]
    async fn the_page_filters_by_actor_kind() {
        let j = Journal::open_in_memory().await.expect("open");
        human_mutations(&j, 2).await;
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

        let all = j.page(None, 50, None).await.expect("page");
        assert_eq!(all.len(), 3);

        let human_only = j.page(None, 50, Some("user")).await.expect("page");
        assert_eq!(human_only.len(), 2);
        assert!(human_only.iter().all(|e| e.entry.actor_kind == "user"));

        let agent_only = j.page(None, 50, Some("agent")).await.expect("page");
        assert_eq!(agent_only.len(), 1);
    }

    /// `revertible_for_after` leaves OUT the marked entry and everything
    /// before it. Marking a row is saying "go back to this state", so that
    /// row is what is kept, not the first victim — and getting this wrong
    /// undoes a mutation the human wanted to keep.
    #[tokio::test]
    async fn revertible_after_does_not_touch_the_marked_entry() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = human_mutations(&j, 4).await;

        let from_the_second = j
            .revertible_for_after(&Actor::User, seqs[1], None)
            .await
            .expect("revertible");

        assert_eq!(
            from_the_second.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![seqs[3], seqs[2]],
            "LIFO, and without the marked one or anything before it"
        );
    }

    /// **A BATCH split by the cutoff stays OUT entirely** (BLOCKER from the
    /// security review).
    ///
    /// `revertible_for` used to bring back all of the actor's entries, so a
    /// `batch_id` always arrived complete at `undo_units`. Cutting by `seq`
    /// breaks that: `revert_batch` — which reverts "all or nothing" — would
    /// receive half a unit believing it whole, because its `debug_assert`
    /// only checks that the piece is internally coherent, and a piece is. The
    /// result would be an `fs.rename_batch` with half the names returned and
    /// the other half not.
    ///
    /// It is excluded whole, and not included whole, because including it
    /// would undo the entry the human marked to KEEP.
    #[tokio::test]
    async fn a_batch_split_by_the_cutoff_stays_out_entirely() {
        let j = Journal::open_in_memory().await.expect("open");
        let batch = j.alloc_batch().await.expect("batch");
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
                    batch_id: Some(batch),
                })
                .await
                .expect("record");
            seqs.push(seq);
        }
        let standalone = j
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

        // Cutoff IN THE MIDDLE of the batch: the middle one.
        let chosen = j
            .revertible_for_after(&Actor::User, seqs[1], None)
            .await
            .expect("revertible");

        assert_eq!(
            chosen.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![standalone],
            "only what is outside the batch: the batch is not split"
        );
    }

    /// And a batch ENTIRELY after the cutoff does go in whole.
    #[tokio::test]
    async fn a_whole_batch_after_the_cutoff_is_included() {
        let j = Journal::open_in_memory().await.expect("open");
        let cutoff = j
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
        let batch = j.alloc_batch().await.expect("batch");
        for i in 0..2 {
            j.record_entry(&NewEntry {
                op: "renamed",
                path: format!("file:///a/{i}").as_bytes(),
                path_to: Some(format!("file:///a/viejo{i}").as_bytes()),
                reversal: Reversal::RenameBack,
                reversal_ref: None,
                actor: &Actor::User,
                undoes_seq: None,
                batch_id: Some(batch),
            })
            .await
            .expect("record");
        }

        let chosen = j
            .revertible_for_after(&Actor::User, cutoff, None)
            .await
            .expect("revertible");

        assert_eq!(chosen.len(), 2, "the whole batch: {chosen:?}");
        assert!(chosen.iter().all(|e| e.batch_id == Some(batch)));
    }

    /// The CEILING (0.80.0) is the cutoff's mirror: what is newest stays out,
    /// and a BATCH with an entry above it stays out WHOLE — even if the one
    /// above is not revertible, because the rule looks at the whole journal
    /// and not the selection. A ceiling below the cutoff selects nothing.
    #[tokio::test]
    async fn the_ceiling_excludes_whats_newer_and_the_batch_it_splits() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = human_mutations(&j, 1).await;
        let cutoff = seqs[0];
        let batch = j.alloc_batch().await.expect("batch");
        let mut from_batch = Vec::new();
        for i in 0..2 {
            from_batch.push(
                j.record_entry(&NewEntry {
                    op: "renamed",
                    path: format!("file:///a/{i}").as_bytes(),
                    path_to: Some(format!("file:///a/viejo{i}").as_bytes()),
                    reversal: Reversal::RenameBack,
                    reversal_ref: None,
                    actor: &Actor::User,
                    undoes_seq: None,
                    batch_id: Some(batch),
                })
                .await
                .expect("record"),
            );
        }
        let standalone = j
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

        let seqs_of = |v: Vec<JournalEntry>| v.into_iter().map(|e| e.seq).collect::<Vec<_>>();
        // Ceiling right at the batch's first entry: it splits it, so it is
        // excluded whole.
        let split = j
            .revertible_for_after(&Actor::User, cutoff, Some(from_batch[0]))
            .await
            .expect("revertible");
        assert!(
            split.is_empty(),
            "the split batch does not go in: {split:?}"
        );
        // Ceiling at the batch's last entry: it goes in whole, and the
        // standalone one after it does not.
        let whole = j
            .revertible_for_after(&Actor::User, cutoff, Some(from_batch[1]))
            .await
            .expect("revertible");
        assert_eq!(seqs_of(whole), vec![from_batch[1], from_batch[0]]);
        // With no ceiling, everything; with a ceiling below the cutoff, nothing.
        let all = j
            .revertible_for_after(&Actor::User, cutoff, None)
            .await
            .expect("revertible");
        assert_eq!(seqs_of(all), vec![standalone, from_batch[1], from_batch[0]]);
        let none = j
            .revertible_for_after(&Actor::User, cutoff, Some(cutoff - 1))
            .await
            .expect("revertible");
        assert!(none.is_empty());
    }

    /// And it only looks at the actor it is asked about: what an agent did
    /// does not enter a human's "undo mine", even if it comes later.
    #[tokio::test]
    async fn revertible_after_does_not_take_another_actors_entries() {
        let j = Journal::open_in_memory().await.expect("open");
        let seqs = human_mutations(&j, 1).await;
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

        let human_side = j
            .revertible_for_after(&Actor::User, seqs[0], None)
            .await
            .expect("revertible");

        assert!(
            human_side.is_empty(),
            "the agent's is its own: {human_side:?}"
        );
    }

    /// The wire row states `reversible` based on what the entry DECLARED, and
    /// an unreadable path is shown with replacements instead of being lost: a
    /// mutation that cannot be seen is indistinguishable from one that did not
    /// happen.
    #[tokio::test]
    async fn the_wire_row_does_not_lose_an_unreadable_entry() {
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

        let rows: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert_eq!(
            rows.len(),
            1,
            "the entry comes out even though its path is not text"
        );
        assert!(!rows[0].reversible, "an Irreversible says so");
        assert!(rows[0].path.contains('\u{FFFD}'));
        assert!(
            rows[0].hostile,
            "and the row SAYS so, it does not leave it to guessing"
        );
    }

    /// **A path with something a terminal would execute comes out SANITIZED,
    /// and the row says so** (found by the security review).
    ///
    /// A file's name is chosen by whoever creates it — including an agent
    /// inside its confinement — and this is the screen where a human decides
    /// what to revert: a bidi override or an escape sequence here would
    /// repaint that decision. Same treatment `fs.search` gives the line it
    /// returns.
    #[tokio::test]
    async fn a_path_with_a_terminal_trap_comes_out_sanitized() {
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

        let rows: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert!(
            !rows[0].path.contains('\u{202E}'),
            "the RTL override does not come out raw: {}",
            rows[0].path
        );
        let destination = rows[0].path_to.as_deref().expect("there is a destination");
        assert!(
            !destination.contains('\u{1b}'),
            "not even an ESC in the destination: {destination}"
        );
        assert!(rows[0].hostile, "and it is marked as painted differently");
    }

    /// A `reversal` this binary does not know counts as having NO way back: in
    /// a tampered journal, asserting that something can be undone is the lie
    /// that costs dearly.
    #[tokio::test]
    async fn an_unknown_reversal_does_not_promise_a_way_back() {
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
            .expect("touch the row");

        let rows: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        assert!(!rows[0].reversible);
    }

    /// An entry that is ALREADY undone says so, and so does its compensation:
    /// they are the two things undo will never touch again, and without them
    /// a timeline promises twice what is going to happen.
    #[tokio::test]
    async fn the_page_says_what_is_already_undone() {
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
        .expect("compensation");

        let rows: Vec<_> = j
            .page(None, 10, None)
            .await
            .expect("page")
            .iter()
            .map(PageEntry::to_wire_row)
            .collect();

        let compensation = &rows[0];
        let original = &rows[1];
        assert_eq!(compensation.undoes_seq, Some(seq), "it is the compensation");
        assert!(original.undone, "and the one below is already undone");
    }
}
