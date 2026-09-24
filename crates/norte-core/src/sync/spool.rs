//! The **spool**: the approved plan, retained in a file (ADR 0049).
//!
//! `sync.apply` carries nothing more than a `plan_hash` —there's no second
//! parameter another intent could arrive through—, so what runs is what was
//! approved by the wire's SHAPE and not by a check someone could forget. For
//! that to work the daemon has to RETAIN the plan, and this is where it
//! retains it.
//!
//! # One file per plan, and its name is the key
//! `<state_dir>/sync-spools/<conn_id>-<plan_hash>.jsonl`. The connection id
//! goes **in the name**, so "nobody applies a plan they didn't produce" is a
//! property of the LOOKUP: [`Spool::open`] composes the name with the
//! `conn_id` of whoever is asking, and one connection cannot name another's
//! file even knowing its hash. Neither `conn_id` (a decimal `u64`) nor
//! `plan_hash` ([`PlanHash`] validates 64 lowercase hex on deserializing)
//! can contain a path separator: the name is safe by construction, not by
//! sanitizing.
//!
//! # The name isn't enough, and here's what goes with it
//! A name isn't a secret: whoever can LIST the directory reads it, and
//! whoever can WRITE to it can forge a file under that name. `plan_hash`
//! doesn't help alone either, because [`PlanHasher`] carries no key: whoever
//! writes any plan can compute its digest and use it as the name. With
//! nothing more, a file dropped into the directory would be an approved
//! plan nobody approved — with the whole approval dialog skipped.
//!
//! So the name comes with two things:
//!
//! 1. **An IN-MEMORY record of what this process emitted.**
//!    [`SpoolWriter::finish`] notes the `(conn_id, plan_hash)` in the
//!    [`Spool`], and [`Spool::open`] requires it before touching disk. A
//!    file this daemon didn't write doesn't open even if it's there, no
//!    matter what it's called — and neither does a plan from a PREVIOUS
//!    startup, which is what fully closes off `conn_id` recycling (they
//!    start from zero on every startup).
//! 2. **The digest gets RECOMPUTED on open.** The file's summary states a
//!    hash, but that's the file talking about itself; [`Spool::open`]
//!    re-hashes the steps with the header's seed and compares. Editing a
//!    `kind` or a `rel` of an already-approved plan stops working.
//!
//! And that same in-memory record is what makes the plan **single-use**:
//! `open` TAKES it away. Two `sync.apply`s of the same hash at once would
//! run the plan twice against the same destination, with two different
//! `batch_id`s and an undo that no longer describes any state it went
//! through.
//!
//! What this does NOT defend against: whoever can write to the directory
//! runs with the daemon's uid, and with that uid can rewrite `policy.toml`.
//! The spool's integrity is the state directory's, not one gram more.
//!
//! # What's inside, and what isn't
//! One JSON line per record:
//!
//! | line | record |
//! | --- | --- |
//! | first | [`SpoolHeader`]: the two roots, the mode and the compare options it was planned with |
//! | middle | a [`SyncStep`] each, in plan order |
//! | last | [`SpoolSummary`]: the hash, the counters, the blockers and `executable` |
//!
//! **Never content.** Paths, sizes and verdicts: exactly what the human saw
//! in the approval dialog. A file that authorizes writes cannot also be the
//! data.
//!
//! Writing and reading are STREAMING —the write buffer has a cap and
//! reading goes in chunks—, so a half-million-step plan costs the same in
//! memory as a three-step one.
//!
//! # Four ways to die
//! 1. **Applied** — [`Spool::remove`], which the `sync.apply` Task calls on
//!    finishing, in any state. **No caller yet: task 9 wires it up.** The
//!    right to apply, on the other hand, is consumed in [`Spool::open`], so
//!    a plan cannot be executed twice even if the file is still there.
//! 2. **TTL** — [`SYNC_PLAN_TTL_MS`] against the mtime, checked on every
//!    [`Spool::open`], which also DELETES the expired one as it finds it.
//! 3. **Connection closed** — [`Spool::drop_connection`], from the
//!    connection's teardown in the daemon (task 8).
//! 4. **Daemon startup** — [`Spool::sweep`], because a violent shutdown
//!    leaves files behind and nobody else is going to collect them. Wired
//!    up in `norte-cli`, alongside the journal.
//!
//! And a fifth that isn't a death but a non-birth: a plan that never
//! reaches [`SpoolWriter::finish`] doesn't exist. It's written under a
//! `.part` name and only the final `rename` gives it the name it can be
//! opened by, so a cancelled plan or a daemon that dies halfway **leave
//! nothing that looks approvable**. The terminator record isn't redundant
//! for that reason: `rename` is atomic with respect to the *directory*, but
//! doesn't promise the data is on disk after a power cut, and a file
//! truncated under the right name is detected because its last line isn't
//! the terminator.
//!
//! A [`SpoolReader`] SURVIVES all four: it keeps the descriptor open, so on
//! unix a delete underneath it doesn't cut off its reading, and the TTL
//! isn't checked again mid-execution. That's deliberate — what protects the
//! destination while applying is the per-step revalidation `stat` (ADR
//! 0049), not the TTL, and aborting halfway would leave a journal batch
//! open for nothing.
//!
//! # Permissions
//! The directory is created `0o700` and each file `0o600` **at birth**
//! (unix), with `mode` in the creation call itself. `chmod`ing afterward
//! leaves a window where the plan is world-readable, and that window is the
//! entire bug.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use futures::stream::FusedStream;
use norte_proto::methods::{
    PlanHash, SYNC_MAX_BLOCKERS_REPORTED, SYNC_PLAN_TTL_MS, SyncBlocker, SyncCompareOptions,
    SyncCounts, SyncStep,
};
use norte_sync::{DestWitness, PlanHasher, PlanItem, SyncOptions};
use serde::{Deserialize, Serialize};

/// The daemon state's subdirectory where spools live. Sibling of
/// `journal.db` and `policy.toml`.
pub const SPOOL_DIR_NAME: &str = "sync-spools";

/// The file's FORMAT version. Not a compatibility promise: a spool is
/// written and read by the same binary within the TTL window, so a
/// different number means "this file is from another norte" and the plan
/// is declared stale ([`SpoolError::Malformed`]), not migrated.
///
/// It's at 2 since a step's record became a [`SpoolStep`] and not a bare
/// [`SyncStep`] (the destination witness the executor revalidates). A file
/// of the previous shape would already fail to deserialize —
/// `deny_unknown_fields` and a new mandatory field—, so the number isn't
/// what protects it: it's what makes the failure tell the truth in the log.
pub const SPOOL_FORMAT: u32 = 2;

/// Cap on ONE record. The terminator is the big one: up to
/// [`SYNC_MAX_BLOCKERS_REPORTED`] blockers with their `rel`. Exists so a
/// corrupt file (or one from another program) cannot demand unbounded
/// memory.
const SPOOL_MAX_RECORD: usize = 8 << 20;

/// How much accumulates in memory before going down to disk. One `write`
/// per step would be half a million `spawn_blocking`s.
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// How much ONE `spawn_blocking` reads on a read, for the same reason.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// What happened to the retained plan.
///
/// [`SpoolError::NotFound`], [`SpoolError::Expired`] and
/// [`SpoolError::Malformed`] are the SAME answer facing the client —
/// `Error::PlanStale`, see [`SpoolError::is_stale`]— because all three say
/// "there's no live plan with that hash". Only [`SpoolError::Io`] is a
/// daemon failure.
///
/// `Malformed`'s text is for the daemon's LOG and not the wire: it stays
/// here and `PlanStale` carries nothing. Today it only brings serde
/// offsets, but it's one refactor away from bringing a path fragment.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpoolError {
    /// There's no plan retained with that hash for that connection.
    /// Includes the "another connection produced it" case: the filename
    /// doesn't match.
    #[error("no plan retained with that hash for this connection")]
    NotFound,
    /// There was one and it passed [`SYNC_PLAN_TTL_MS`]. It's already deleted.
    #[error("the retained plan expired")]
    Expired,
    /// The file is there and cannot be read as a plan: truncated, from
    /// another format version, tampered with (the recomputed digest
    /// doesn't match), or written by a binary that didn't know a field
    /// that's mandatory today.
    ///
    /// It's DELIBERATE that this isn't recoverable. [`SyncCounts`]'s new
    /// counters carry no `serde(default)`: a spool from an old binary fails
    /// to deserialize instead of reading a silent zero, because a dialog
    /// that approved "340 unmeasured files" and an execution that believes
    /// there are none aren't the same act.
    #[error("the spool cannot be read as a plan: {0}")]
    Malformed(String),
    /// The plan's stream never reached its end —cancellation, or a planner
    /// failure— and therefore there's no plan to retain. The `.part` is
    /// already deleted.
    #[error("the plan was interrupted before finishing")]
    Interrupted,
    /// Real I/O over the spool.
    #[error("I/O over the spool: {0}")]
    Io(#[from] io::Error),
}

impl SpoolError {
    /// Is it one of the ones that mean "no live plan"?
    ///
    /// Whoever serves `sync.apply` translates `true` to
    /// [`Error::PlanStale`](norte_proto::Error::PlanStale) and `false` to an
    /// internal failure. An unreadable file is a stale plan, **not a dead
    /// Task**: the client can plan again, which is exactly what the answer
    /// is telling it.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(
            self,
            SpoolError::NotFound | SpoolError::Expired | SpoolError::Malformed(_)
        )
    }
}

/// The first line: what it was planned with.
///
/// The executor needs it whole. `sync.apply` carries no roots —it carries
/// the hash and nothing else—, so if they weren't here they wouldn't be
/// anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolHeader {
    /// [`SPOOL_FORMAT`] when it was written.
    pub format: u32,
    /// The connection that planned it. Redundant with the filename, and
    /// that's the point: [`Spool::open`] checks that they match, so a file
    /// renamed by hand doesn't open.
    pub conn_id: u64,
    /// The roots, the mode, `on_unknown`, the source side and the
    /// destination's two capability booleans.
    pub options: SyncOptions,
    /// What criteria it was compared with. They do **not** live in
    /// [`SyncOptions`] and still enter the `plan_hash`: a plan made with
    /// `hash` on isn't the same as one made with size only even if the
    /// steps come out equal, because something else was approved.
    pub compare: SyncCompareOptions,
}

/// The last line: what the plan added up to.
///
/// It's what [`SpoolWriter::finish`] returns and what [`Spool::open`] reads
/// without walking the steps, so the executor can refuse a non-executable
/// plan **before** the first step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolSummary {
    /// The fingerprint of what was approved, and half the file's key.
    pub plan_hash: PlanHash,
    /// The whole plan's counters.
    pub counts: SyncCounts,
    /// The blockers, trimmed to [`SYNC_MAX_BLOCKERS_REPORTED`]. It's the
    /// explanation; `executable` is what decides.
    pub blockers: Vec<SyncBlocker>,
    /// How many there really were. No cap:
    /// [`SyncBlockerKind::TypeMismatchDir`](norte_proto::methods::SyncBlockerKind::TypeMismatchDir)'s
    /// grows with the tree.
    pub blockers_total: u64,
    /// `true` when the plan can be executed as-is, i.e. when there was NO
    /// blocker at all. Computed by [`SpoolWriter::finish`] and nobody else:
    /// it's `sync.plan_done`'s normative field, and whoever derives it on
    /// their own will sooner or later derive it differently.
    pub executable: bool,
}

/// A record from the file. ADJACENTLY tagged (`{"r":…,"v":…}`) so the tag
/// can never collide with a field of the type it wraps.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "r", content = "v", deny_unknown_fields)]
enum Record {
    #[serde(rename = "head")]
    Head(SpoolHeader),
    #[serde(rename = "step")]
    Step(SpoolStep),
    #[serde(rename = "end")]
    End(SpoolSummary),
}

/// ONE retained step: what travels over the wire, plus what only the
/// executor needs.
///
/// The second field is why this type exists instead of storing the bare
/// [`SyncStep`]. The executor has to revalidate the destination BEFORE
/// destroying it —up to ten minutes separate approval from application—
/// and there's nothing in `SyncStep` to do it with: its `size` is the bytes
/// the step MOVES, i.e. the source's, and no field describes the
/// destination's prior state. Without this, the revalidation `stat` would
/// have nothing to compare against and would be decorative.
///
/// It's in the spool and not on the wire because nobody on the other side
/// needs it, and because publishing it would mean sending the client a
/// second description of the destination tree with its sizes and dates.
///
/// # `plan_hash` does NOT cover this field
/// The digest summarizes the PLAN —what a human approved— and the witness
/// is where that conclusion came from, not the conclusion; putting it
/// inside would make two identical plans over an untouched tree differ
/// because a date moved. The consequence has to be known: [`Spool::open`]
/// recomputes the digest over the steps and checks the counters, so it
/// authenticates the STEP and not the whole record. What closes the gap
/// isn't the digest but the executor: it refuses any destructive step that
/// arrives with no witness, so deleting it doesn't disable revalidation, it
/// turns it into a conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolStep {
    /// The approved step.
    pub step: SyncStep,
    /// What the comparison saw on the destination, for the two classes
    /// that are going to destroy it. Absent for the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<DestWitness>,
}

/// A daemon's spool directory, and the record of what it emitted.
///
/// Built with the state directory —the same one `journal.db` uses, i.e.
/// [`crate::connect::config_dir`]— and hangs [`SPOOL_DIR_NAME`] off it.
///
/// # One per daemon, cloned, never built twice
/// The record of emitted plans lives BEHIND an [`Arc`], so a clone shares
/// the same one. That's what makes two handles of the same daemon see each
/// other, and —just as important— what makes two PROCESSES not see each
/// other: a fresh `Spool::new` is born with an empty record and cannot open
/// anything it didn't write itself.
///
/// Hence the rule, and it isn't negotiable: **the daemon builds ONE `Spool`
/// and clones it**. A second `Spool::new` over the same directory isn't
/// another handle of the same spool, it's a spool that doesn't recognize a
/// single plan — and if someone uses it to plan, its plans will be the only
/// ones it can apply.
///
/// The side effect is a good one: two processes sharing the state directory
/// cannot apply each other's plans, not even with the same `conn_id`. That
/// matters more now than before: since #167 the embedded engine DOES open
/// the journal when the lock is free, so "a single process over this state
/// directory" stopped being the only thing separating two spools.
#[derive(Debug, Clone)]
pub struct Spool {
    dir: PathBuf,
    /// Everything this PROCESS knows about its plans, under a single lock:
    /// without it, "is the connection alive?" and "is the plan noted?"
    /// would be two decisions with a gap in between, which is exactly where
    /// the races [`Registry`] exists to close would fit.
    reg: Arc<Mutex<Registry>>,
}

/// What this process knows about its own plans. All together and under a lock.
#[derive(Debug, Default)]
struct Registry {
    /// The `(conn_id, plan_hash)`s THIS process emitted and nobody has
    /// applied yet. It's both the proof of emission and the single-use
    /// right.
    issued: HashSet<(u64, PlanHash)>,
    /// The ones someone is applying RIGHT NOW: [`Spool::open`] took the
    /// right and [`Spool::remove`] hasn't happened yet.
    ///
    /// Exists because re-planning the same tree with the same options gives
    /// the SAME hash, and without this [`SpoolWriter::finish`] would mint
    /// again a right an `open` had just consumed: two executions of the
    /// same plan against the same destination, with two journal batches and
    /// an undo that no longer describes any state it went through.
    applying: HashSet<(u64, PlanHash)>,
    /// How many plans each connection has IN FLIGHT (open writers). It's
    /// what bounds `dead`: a dead connection is only remembered while some
    /// of its plans are still being written.
    planning: HashMap<u64, usize>,
    /// Connections that closed and for which, therefore, a plan can no
    /// longer be retained.
    ///
    /// Without this, a plan that finishes after its connection's teardown
    /// renames its file and notes itself as emitted AFTER the one death
    /// that applied to it —"connection closed"— has already happened: a
    /// retained plan is left that nobody can apply and nobody is going to
    /// collect. And the case needs no race at all to happen: a plan that
    /// emits NOT ONE step (two identical trees) never touches the channel,
    /// so it never finds out its owner left.
    ///
    /// **It's noted even if there was no plan halfway through**, and that
    /// was the fix: before, the tombstone was only set if the connection
    /// already had an open writer, so a `sync.plan` that opened its own an
    /// instant AFTER the teardown found out nothing and retained its plan
    /// forever. It showed up as an intermittent red —the order of the two
    /// things isn't fixed— and an intermittent red here is a bug, not
    /// noise.
    ///
    /// What bounds it is no longer "having a plan in flight" but
    /// [`DEAD_CAP`]: on reaching it, the ones with no open writer are
    /// swept, since those can no longer serve any purpose. See
    /// [`Spool::forget_issued`].
    dead: HashSet<u64>,
}

/// How many dead connections are remembered before sweeping the useless ones.
///
/// A tombstone is only good while that connection's plan `finish` can still
/// arrive, and that's a planning task's lifespan: milliseconds. The number
/// is generous on purpose —remembering too many breaks nothing and
/// remembering too few does— and what it buys is that the set doesn't grow
/// with the connection counter of a daemon that's been up for days.
const DEAD_CAP: usize = 1024;

impl Spool {
    /// Anchors the spool under `state_dir`. Doesn't touch disk: the
    /// directory is created on the first [`Spool::create`].
    ///
    /// See the type's note: this is called ONCE per daemon and the handle
    /// is cloned. Calling it twice creates two emission records that don't
    /// see each other.
    #[must_use]
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            dir: state_dir.as_ref().join(SPOOL_DIR_NAME),
            reg: Arc::new(Mutex::new(Registry::default())),
        }
    }

    /// The registry, or `None` if the lock is poisoned. A broken lock is
    /// treated as "I know nothing": fail-closed in every use below.
    fn reg(&self) -> Option<std::sync::MutexGuard<'_, Registry>> {
        self.reg.lock().ok()
    }

    /// Notes a plan as emitted, unless it no longer applies. Called by
    /// [`SpoolWriter::finish`] AFTER the rename and under the same lock that
    /// checks the two reasons not to, which is what makes it atomic against
    /// a simultaneous connection close or `open`.
    ///
    /// `false` = it wasn't noted, and the just-renamed file has to be removed.
    fn record_issued(&self, conn_id: u64, hash: &PlanHash) -> bool {
        let Some(mut reg) = self.reg() else {
            return false;
        };
        if reg.dead.contains(&conn_id) || reg.applying.contains(&(conn_id, hash.clone())) {
            return false;
        }
        reg.issued.insert((conn_id, hash.clone()));
        true
    }

    /// TAKES the right to apply `hash` and moves it to "applying". `false`
    /// if it wasn't there: either this process never emitted it, or someone
    /// already applied it.
    fn claim_issued(&self, conn_id: u64, hash: &PlanHash) -> bool {
        let Some(mut reg) = self.reg() else {
            return false;
        };
        let key = (conn_id, hash.clone());
        if !reg.issued.remove(&key) {
            return false;
        }
        reg.applying.insert(key);
        true
    }

    /// Forgets a connection's plans (or all of them, with `None`).
    ///
    /// With `Some`, it also marks the connection as DEAD: none of its
    /// plans, whether halfway or not yet started, can end up as an
    /// approvable plan.
    fn forget_issued(&self, conn_id: Option<u64>) {
        let Some(mut reg) = self.reg() else {
            return;
        };
        let Some(id) = conn_id else {
            reg.issued.clear();
            reg.applying.clear();
            reg.dead.clear();
            return;
        };
        reg.issued.retain(|(c, _)| *c != id);
        reg.applying.retain(|(c, _)| *c != id);
        // Before noting it, sweep whatever can no longer serve any purpose:
        // a tombstone with no open writer is only good while the `finish`
        // of a plan that started right as the teardown happened can still
        // arrive, and that lasts as long as a task does. Without this sweep
        // the set would grow with the connection counter of a daemon that's
        // been up for days.
        if reg.dead.len() >= DEAD_CAP {
            let alive: Vec<u64> = reg.planning.keys().copied().collect();
            reg.dead.retain(|c| alive.contains(c));
        }
        reg.dead.insert(id);
    }

    /// Releases the "applying" mark WITHOUT touching disk and with no
    /// `await`: abandons `conn_id`'s plan `hash`.
    ///
    /// This is the emergency exit for a `sync.apply` whose dispatch gets
    /// DROPPED before it gets to create the Task — today, an `rpc.cancel`
    /// while the policy gate is suspended on an `ask`. That path cannot
    /// call [`Spool::remove`] (it's `async`, and a `Drop` cannot wait), and
    /// if it didn't release the mark the hash would stay "applying" for the
    /// rest of the connection's life: neither applicable nor re-plannable.
    ///
    /// Same as [`Spool::open`]'s error exits, it releases from `applying`
    /// and does **not** return it to `issued`: the plan doesn't become
    /// applicable again —nobody knows if the gate got to approve it— but
    /// the same tree becomes re-plannable again, which is what the user
    /// needs. The file stays for the TTL, for the startup sweep, or for the
    /// connection's close.
    pub(crate) fn abandon(&self, conn_id: u64, hash: &PlanHash) {
        self.release_applying(conn_id, hash);
    }

    /// Releases the "applying" mark. Called by [`Spool::remove`], which is
    /// what the `sync.apply` Task invokes on finishing in any state.
    fn release_applying(&self, conn_id: u64, hash: &PlanHash) {
        if let Some(mut reg) = self.reg() {
            reg.applying.remove(&(conn_id, hash.clone()));
        }
    }

    /// Was this plan's connection closed while it was being written?
    fn is_dead(&self, conn_id: u64) -> bool {
        self.reg().is_some_and(|reg| reg.dead.contains(&conn_id))
    }

    /// Notes an open writer for `conn_id`.
    fn open_writer(&self, conn_id: u64) {
        if let Some(mut reg) = self.reg() {
            *reg.planning.entry(conn_id).or_insert(0) += 1;
        }
    }

    /// Closes a writer. With a connection's last one, its tombstone is also
    /// forgotten: `dead` doesn't grow with the connection counter, only
    /// with the ones that have a plan halfway through right when they fall.
    fn close_writer(&self, conn_id: u64) {
        let Some(mut reg) = self.reg() else {
            return;
        };
        if let Some(n) = reg.planning.get_mut(&conn_id) {
            *n -= 1;
            if *n == 0 {
                reg.planning.remove(&conn_id);
                reg.dead.remove(&conn_id);
            }
        }
    }

    /// How many RETAINED plans (issued and unapplied) a connection has.
    ///
    /// Queried by the daemon before accepting another `sync.plan`: a
    /// retained plan is a file on disk with the listing of two trees, and
    /// nothing but the connection's close collects it while it's alive.
    #[must_use]
    pub fn retained_for(&self, conn_id: u64) -> usize {
        self.reg().map_or(0, |reg| {
            reg.issued.iter().filter(|(c, _)| *c == conn_id).count()
        })
    }

    /// The directory, for whoever wants to log it.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Opens a NEW plan for `conn_id`.
    ///
    /// The file is born under a `.part` name because `plan_hash` —the other
    /// half of its name— doesn't exist yet: it's computed in streaming
    /// fashion over the plan's elements and isn't known until the stream
    /// ends. Only [`SpoolWriter::finish`] gives it its real name.
    ///
    /// # The TTL is charged HERE, and nowhere else is it charged
    /// [`SYNC_PLAN_TTL_MS`] is checked in [`Spool::open`], but a plan nobody
    /// opens never gets opened: without this, "the TTL" wouldn't be one of
    /// the four deaths but a check that only runs when it's no longer
    /// needed. So every new plan first sweeps the expired `.jsonl`s, which
    /// bounds what's retained to what was planned in the last ten minutes
    /// with no need for a timer thread.
    ///
    /// A `.part` is NOT touched for its age: a plan over a network tree
    /// legitimately takes hours, and its file carries its original mtime.
    /// Orphaned `.part`s are handled by the writer's `Drop` and the startup
    /// sweep.
    ///
    /// # Errors
    /// [`SpoolError::Io`] if the state directory cannot be created or the
    /// file cannot be opened.
    #[tracing::instrument(skip_all, fields(conn_id = conn_id))]
    pub async fn create(
        &self,
        conn_id: u64,
        options: &SyncOptions,
        compare: &SyncCompareOptions,
    ) -> Result<SpoolWriter, SpoolError> {
        let header = SpoolHeader {
            format: SPOOL_FORMAT,
            conn_id,
            options: options.clone(),
            compare: compare.clone(),
        };
        let hasher = PlanHasher::new(options, compare);
        let mut line = encode(&Record::Head(header))?;
        let dir = self.dir.clone();
        // Rule 2: `std::fs` is blocking, and this is an async context.
        let (file, part) = crate::blocking::spawn_blocking(move || {
            ensure_dir(&dir)?;
            reap_expired(&dir);
            let (mut file, part) = create_part(&dir, conn_id)?;
            file.write_all(&line)?;
            line.clear();
            Ok::<_, SpoolError>((file, part))
        })
        .await
        .map_err(joined)??;
        self.open_writer(conn_id);
        Ok(SpoolWriter {
            spool: self.clone(),
            part,
            conn_id,
            file: Some(file),
            buf: Vec::with_capacity(WRITE_BUFFER_BYTES),
            hasher: Some(hasher),
            counts: SyncCounts::default(),
            blockers: Vec::new(),
            blockers_total: 0,
            finished: false,
        })
    }

    /// Opens —and CONSUMES— plan `hash` of connection `conn_id`.
    ///
    /// Checks, in this order:
    ///
    /// 1. That this process emitted that plan for that connection and
    ///    nobody has taken it already. This is first on purpose: a plan we
    ///    didn't emit doesn't even deserve to have its `stat` looked at.
    ///    **The right is consumed here**, so two `sync.apply`s of the same
    ///    hash cannot run at once — they would run the plan twice against
    ///    the same destination, with two journal batches and an undo that
    ///    no longer describes any real state. The second one receives
    ///    [`SpoolError::NotFound`], i.e. `PlanStale`, which is the true
    ///    answer.
    /// 2. That the file exists ([`SpoolError::NotFound`]).
    /// 3. That it hasn't expired; if it has, it's DELETED and
    ///    [`SpoolError::Expired`] is returned. The TTL is measured over the
    ///    already-open descriptor's `fstat`, not the path: another file
    ///    could fit between looking at the path and opening it. And the
    ///    delete checks the path still names THAT inode, because
    ///    re-planning the same tree produces the same hash and deleting by
    ///    name would take down the just-approved plan.
    /// 4. That the last line is the terminator and that the header and the
    ///    terminator agree on the same connection and the same hash as the
    ///    name.
    /// 5. That the digest **recomputed** over the steps matches the name.
    ///    Costs one more sequential read of the file, which is negligible
    ///    next to executing the plan, and it's what turns "what's approved
    ///    is what runs" into a property instead of a declaration by the
    ///    file itself.
    ///
    /// A plan with blockers cannot be recomputed —the stored list is
    /// trimmed to [`SYNC_MAX_BLOCKERS_REPORTED`] and the digest covers all
    /// of them—, but it can't be executed either: the invariant
    /// `executable == (blockers_total == 0)` is required, so every
    /// EXECUTABLE plan goes through point 5's check.
    ///
    /// # Errors
    /// See [`SpoolError`]. The first three variants mean the same thing
    /// facing the client ([`SpoolError::is_stale`]).
    #[tracing::instrument(skip_all, fields(conn_id = conn_id, plan_hash = hash.as_str()))]
    pub async fn open(&self, conn_id: u64, hash: &PlanHash) -> Result<SpoolReader, SpoolError> {
        if !self.claim_issued(conn_id, hash) {
            return Err(SpoolError::NotFound);
        }
        let path = self.dir.join(file_name(conn_id, hash));
        let want = hash.clone();
        let opened = crate::blocking::spawn_blocking(move || open_blocking(&path, conn_id, &want))
            .await
            .map_err(joined);
        // The right was CHARGED above, and from here on there are four ways
        // to fail (expired, no longer there, tampered with, I/O). If it
        // isn't returned, that hash stays "applying" forever: re-planning
        // the same tree with the same options gives the SAME digest,
        // `finish` finds it busy and deletes the plan it just wrote — the
        // user can neither apply nor re-plan, and all they see is an
        // internal error.
        //
        // Released from `applying` and NOT returned to `issued`: an
        // expired or tampered-with plan doesn't become applicable again,
        // only re-plannable again.
        let opened = match opened {
            Ok(Ok(opened)) => opened,
            Ok(Err(e)) => {
                self.release_applying(conn_id, hash);
                if let SpoolError::Malformed(why) = &e {
                    // A file we wrote ourselves minutes ago that no longer
                    // lets itself be read is the sign that someone touched
                    // it. The client will only see `PlanStale`.
                    tracing::warn!(conn = conn_id, why, "unreadable spool");
                }
                return Err(e);
            }
            Err(e) => {
                self.release_applying(conn_id, hash);
                return Err(e);
            }
        };
        let (file, header, summary) = opened;
        Ok(SpoolReader {
            file,
            header,
            summary,
        })
    }

    /// Deletes plan `hash` of `conn_id`, whether it exists or not. This is
    /// what the `sync.apply` Task calls on finishing, in any state (task 9).
    ///
    /// **It has to be called**, and not just for disk hygiene: as long as
    /// it isn't, the plan keeps counting as "applying" and re-planning that
    /// same tree with the same options —which gives the same digest— is
    /// refused. That's the safe side of the trade-off (rather than minting
    /// the right to write the same destination twice), but it's a
    /// user-visible failure.
    ///
    /// # Errors
    /// [`SpoolError::Io`] only if the delete fails for something other than
    /// "wasn't there".
    #[tracing::instrument(skip_all, fields(conn_id = conn_id, plan_hash = hash.as_str()))]
    pub async fn remove(&self, conn_id: u64, hash: &PlanHash) -> Result<(), SpoolError> {
        self.claim_issued(conn_id, hash);
        self.release_applying(conn_id, hash);
        let path = self.dir.join(file_name(conn_id, hash));
        crate::blocking::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SpoolError::Io(e)),
        })
        .await
        .map_err(joined)?
    }

    /// Deletes ALL of a connection's plans, finished or halfway. Called
    /// when the connection drops: nobody can apply a plan with no owner.
    /// Called by the connection's teardown in the daemon.
    ///
    /// # Errors
    /// [`SpoolError::Io`] if the directory cannot be listed. A file that
    /// cannot be deleted does NOT abort the sweep, but comes out in
    /// [`SweepReport::failed`]: the right to apply it has already been
    /// forgotten in memory either way, so what's left is garbage on disk
    /// and not a live plan.
    #[tracing::instrument(skip_all, fields(conn_id = conn_id))]
    pub async fn drop_connection(&self, conn_id: u64) -> Result<SweepReport, SpoolError> {
        self.forget_issued(Some(conn_id));
        let dir = self.dir.clone();
        let prefix = format!("{conn_id}-");
        crate::blocking::spawn_blocking(move || {
            remove_matching(&dir, |name| name.starts_with(prefix.as_bytes()))
        })
        .await
        .map_err(joined)?
    }

    /// Sweeps the WHOLE directory. Called on daemon startup, alongside the
    /// journal.
    ///
    /// Takes every spool, not just the expired ones, and that's the
    /// important part: at startup there's no live connection at all, so
    /// **every spool that exists belongs to a dead connection** and nobody
    /// can apply it. Also, `conn_id`s start over from zero on every
    /// startup, so leaving a fresh one would leave a file authorizing
    /// writes under an id the daemon is about to hand out again.
    ///
    /// **That safety does NOT depend on this**, and it's worth being clear
    /// about: what prevents applying a plan from a previous startup is that
    /// the emitted-plans record lives in memory and is born empty, so a
    /// sweep that fails leaves garbage on disk —and the paths of two trees,
    /// readable by whoever can read the state directory— but not an
    /// applicable plan. That's why [`SweepReport::failed`] warns and
    /// doesn't abort startup: a daemon that refuses to start over a file
    /// that won't delete is a worse failure than the one it avoids.
    ///
    /// With a single daemon per state directory —which the journal already
    /// enforces with its exclusive lock over `journal.db` (ADR 0024)— it
    /// also never takes down a live connection's spool.
    ///
    /// # Errors
    /// [`SpoolError::Io`] if the directory exists and cannot be listed.
    /// Not existing isn't an error: that's normal on first startup.
    #[tracing::instrument(skip_all)]
    pub async fn sweep(&self) -> Result<SweepReport, SpoolError> {
        self.forget_issued(None);
        let dir = self.dir.clone();
        crate::blocking::spawn_blocking(move || remove_matching(&dir, |_| true))
            .await
            .map_err(joined)?
    }
}

/// What a sweep took, and what resisted it.
///
/// `failed` exists because a bare delete counter LIES: an `Ok(2)` with
/// three files still on disk is indistinguishable from a clean sweep, and
/// the place where that happens —a `remove_file` that fails— is exactly
/// where nobody looks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Files deleted.
    pub removed: usize,
    /// Files that were there and refused to be deleted.
    pub failed: usize,
}

impl SweepReport {
    /// Did it take everything it found?
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed == 0
    }
}

/// Writes a plan while it's being planned.
///
/// Swallows [`PlanItem`], which is what `norte_sync::plan` produces, and
/// does three things with each one **at once**: hashes it, counts it, and
/// (if it's a step) writes it. Being the same place isn't convenience: the
/// `plan_hash` has to summarize EXACTLY the elements the human sees, i.e.
/// the ones after the request's `include`, and with a single funnel there's
/// no way to hash one sequence and show another.
///
/// Whoever uses this must push here the SAME elements it sends the client
/// over `sync.steps`, in the same order.
///
/// # Only a stream that FINISHED gets closed, and that's why it has to be stated
/// [`SpoolWriter::finish`] requires a [`PlanOutcome`]. It isn't ceremony:
/// the partial digest of a plan cut in half is indistinguishable from a
/// shorter complete plan's, so closing a cancelled one produces a
/// perfectly valid `plan_hash` for a plan that claims to sync a tree that
/// was only a third walked. The human approves "412 files", 412 get
/// copied, and the 400,000 that were missing never get copied with nothing
/// saying so.
///
/// The loop that destroys it is this one, and it's the one that exits on
/// its own:
///
/// ```ignore
/// while let Some(Ok(item)) = items.next().await { w.push(&item).await?; }
/// let s = w.finish(PlanOutcome::Ended).await?;   // A LIE if there was an Err
/// ```
///
/// `Some(Err(_))` exits through the same place as `None`. Only the `None`
/// arm can pass [`PlanOutcome::Ended`]; the error one passes
/// [`PlanOutcome::Interrupted`], which deletes the `.part` and returns no
/// hash at all.
#[derive(Debug)]
pub struct SpoolWriter {
    /// The spool that created it: `finish` notes the plan there as emitted,
    /// which is what later lets it be opened.
    spool: Spool,
    part: PathBuf,
    conn_id: u64,
    /// `None` only while a `spawn_blocking` has borrowed it, and after
    /// [`SpoolWriter::finish`].
    file: Option<std::fs::File>,
    buf: Vec<u8>,
    /// `Option` for the same reason as `file`: `SpoolWriter` implements
    /// `Drop`, so a field cannot be taken out of it without leaving
    /// something in its place.
    hasher: Option<PlanHasher>,
    counts: SyncCounts,
    blockers: Vec<SyncBlocker>,
    blockers_total: u64,
    finished: bool,
}

/// How the plan's stream ended. Required by [`SpoolWriter::finish`] so
/// nobody can close a half-written plan without having written it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOutcome {
    /// The stream returned `None`: the plan is whole. **Only from that arm.**
    Ended,
    /// The stream was cut off — cancellation, or a
    /// [`norte_sync::SyncError`]. The spool is deleted and there's no hash.
    Interrupted,
}

impl SpoolWriter {
    /// Swallows one plan element.
    ///
    /// A step gets hashed, counted and written. A blocker gets hashed and
    /// counted —all of them, no cap, because the hash has to distinguish
    /// two plans that differ in blocker 257— but only the first
    /// [`SYNC_MAX_BLOCKERS_REPORTED`] are stored so they can be named.
    ///
    /// # Errors
    /// [`SpoolError::Io`] if the write fails. An error here leaves the plan
    /// unfinished, which is the same as never having made it.
    pub async fn push(&mut self, item: &PlanItem) -> Result<(), SpoolError> {
        let Some(hasher) = self.hasher.as_mut() else {
            return Err(SpoolError::Malformed(
                "the spool is already closed".to_owned(),
            ));
        };
        hasher.item(item);
        match item {
            PlanItem::Step { step, dest } => {
                self.counts.add(step);
                let line = encode(&Record::Step(SpoolStep {
                    step: step.clone(),
                    dest: *dest,
                }))?;
                self.buf.extend_from_slice(&line);
                if self.buf.len() >= WRITE_BUFFER_BYTES {
                    self.flush().await?;
                }
            }
            PlanItem::Blocker(blocker) => {
                self.blockers_total = self.blockers_total.saturating_add(1);
                if self.blockers.len() < SYNC_MAX_BLOCKERS_REPORTED {
                    self.blockers.push(blocker.clone());
                }
            }
        }
        Ok(())
    }

    /// Closes the plan: writes the terminator, gives the file its final
    /// name (`<conn_id>-<plan_hash>.jsonl`) and notes it as emitted.
    ///
    /// Until that `rename` the plan has no name to be looked up by, and
    /// until that note there's no right to apply it: the two together are
    /// what makes a cancelled plan —or a daemon dying halfway— leave
    /// nothing approvable.
    ///
    /// `outcome` isn't decoration: see the type's note. With
    /// [`PlanOutcome::Interrupted`] this deletes the `.part` and returns
    /// [`SpoolError::Interrupted`] with no terminator written at all.
    ///
    /// If the future is dropped DURING the `rename`, the blocking task can
    /// still complete it and leave a spool under the right name whose hash
    /// the caller never learned. Nobody can apply it —it never got noted as
    /// emitted— and the sweep takes it.
    ///
    /// # Two other ways to NOT close, besides `outcome`
    /// Both return [`SpoolError::Interrupted`], and both are things that
    /// happened while the plan was being written that whoever writes it
    /// cannot see:
    ///
    /// - **Its connection closed.** The teardown took that connection's
    ///   plans, so one that got noted AFTER would be left retained with no
    ///   owner and nobody to collect it. No race is needed to get here: a
    ///   plan over two identical trees emits not a single step, never
    ///   touches the channel and therefore never finds out its owner left.
    /// - **An `open` took THAT hash's right.** Re-planning the same tree
    ///   with the same options gives the same digest; minting the right
    ///   again while someone is applying it would authorize a second
    ///   execution of the same plan against the same destination, with two
    ///   journal batches.
    ///
    /// The real check is the one AFTER the rename, done under the same lock
    /// as the decision to note the plan as emitted; the one before only
    /// saves the work.
    ///
    /// # Errors
    /// [`SpoolError::Interrupted`] if `outcome` says so, if the connection
    /// died or if the plan is being applied, and [`SpoolError::Io`] if the
    /// write or the `rename` fail.
    #[tracing::instrument(skip_all, fields(conn_id = self.conn_id, ?outcome))]
    pub async fn finish(mut self, outcome: PlanOutcome) -> Result<SpoolSummary, SpoolError> {
        if outcome == PlanOutcome::Interrupted || self.spool.is_dead(self.conn_id) {
            self.abandon_inner().await;
            return Err(SpoolError::Interrupted);
        }
        let Some(hasher) = self.hasher.take() else {
            return Err(SpoolError::Malformed(
                "the spool is already closed".to_owned(),
            ));
        };
        let summary = SpoolSummary {
            plan_hash: hasher.finish(),
            counts: self.counts,
            blockers: std::mem::take(&mut self.blockers),
            blockers_total: self.blockers_total,
            executable: self.blockers_total == 0,
        };
        let line = encode(&Record::End(summary.clone()))?;
        self.buf.extend_from_slice(&line);
        self.flush().await?;

        let file = self.file.take();
        let part = self.part.clone();
        let target = self
            .spool
            .dir
            .join(file_name(self.conn_id, &summary.plan_hash));
        let landed = target.clone();
        crate::blocking::spawn_blocking(move || {
            // Close BEFORE the rename: on Windows an open file doesn't
            // rename, and on unix it costs nothing.
            drop(file);
            std::fs::rename(&part, &target)
        })
        .await
        .map_err(joined)??;
        self.finished = true;
        // AFTER the rename: noting a plan whose file never got its name
        // would promise an `open` that later finds nothing. And under the
        // lock, which is what decides whether it still applies — see the
        // note above about the other two ways of not closing.
        if !self.spool.record_issued(self.conn_id, &summary.plan_hash) {
            crate::blocking::spawn_blocking(move || {
                let _ = std::fs::remove_file(&landed);
            })
            .await
            .map_err(joined)?;
            return Err(SpoolError::Interrupted);
        }
        Ok(summary)
    }

    /// Throws away the half-written plan, returning nothing. It's
    /// `finish(PlanOutcome::Interrupted)` with no error, for whoever
    /// already knows there won't be a plan.
    ///
    /// Doesn't fail: a delete that can't be done is picked up by the
    /// startup sweep, and the file doesn't have the name it would be
    /// looked up by anyway.
    pub async fn abandon(mut self) {
        self.abandon_inner().await;
    }

    /// [`SpoolWriter::abandon`]'s body, by `&mut` so
    /// [`SpoolWriter::finish`] can use it on the interrupted path.
    async fn abandon_inner(&mut self) {
        let file = self.file.take();
        let part = self.part.clone();
        self.hasher = None;
        self.finished = true;
        let _ = crate::blocking::spawn_blocking(move || {
            drop(file);
            let _ = std::fs::remove_file(&part);
        })
        .await;
    }

    /// Flushes the buffer to disk. The `File` travels to the blocking
    /// thread and comes back.
    async fn flush(&mut self) -> Result<(), SpoolError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut file = self.file.take().ok_or_else(|| {
            // `Malformed` and not `Io`: means this writer can no longer
            // produce a plan, which facing the client is a stale plan
            // ([`SpoolError::is_stale`]) and not a daemon failure.
            SpoolError::Malformed("the spool is already closed".to_owned())
        })?;
        let chunk = std::mem::take(&mut self.buf);
        let (file, mut chunk, res) = crate::blocking::spawn_blocking(move || {
            let res = file.write_all(&chunk);
            (file, chunk, res)
        })
        .await
        .map_err(joined)?;
        self.file = Some(file);
        // Capacity is recovered: the buffer is reused for the whole plan.
        chunk.clear();
        self.buf = chunk;
        res.map_err(SpoolError::Io)
    }
}

impl Drop for SpoolWriter {
    fn drop(&mut self) {
        // ALWAYS, however it closed: this is the counter that bounds a dead
        // connection's tombstone to the ones that really have a plan
        // halfway through.
        self.spool.close_writer(self.conn_id);
        if self.finished {
            return;
        }
        // An unfinished plan cannot be opened —the `.part` doesn't have the
        // name it would be looked up by and was never noted as emitted—, so
        // this is hygiene and not a guarantee: the startup sweep is what
        // gives that.
        //
        // A SYNCHRONOUS `unlink`, knowingly against rule 2. The alternative
        // was `Handle::spawn_blocking`, which panics when the runtime is
        // already shutting down (and `try_current` still returns `Ok` in
        // that window, because the drop happens inside the context): a
        // panic in a `Drop` during unwinding aborts the process. Trading an
        // abort for an `unlink` that cannot block appreciably is the right
        // trade.
        let part = std::mem::take(&mut self.part);
        drop(self.file.take());
        let _ = std::fs::remove_file(&part);
    }
}

/// A retained, already validated plan: the header and summary are read and
/// the steps are requested in streaming.
#[derive(Debug)]
pub struct SpoolReader {
    file: std::fs::File,
    header: SpoolHeader,
    summary: SpoolSummary,
}

impl SpoolReader {
    /// What it was planned with. The executor gets the two roots from here.
    #[must_use]
    pub fn header(&self) -> &SpoolHeader {
        &self.header
    }

    /// What it added up to. Read without walking a single step, so a
    /// non-executable plan is refused before starting.
    #[must_use]
    pub fn summary(&self) -> &SpoolSummary {
        &self.summary
    }

    /// The steps, in plan order.
    ///
    /// The order is the walk's (pre-order), so a `CreateDir` precedes every
    /// copy inside it: **nothing gets sorted**, it runs as it comes.
    ///
    /// The stream is FUSED: asking it for one more element after the end
    /// returns `None` instead of panicking, so a `select!` with a progress
    /// tick on top is legal.
    ///
    /// Ends at the terminator record, and requires NOTHING after it: if
    /// there were a second terminator, [`Spool::open`] would have validated
    /// the last one and this would execute up to the first — the approved
    /// summary and the executed plan would be two different things. If the
    /// file ends before that —someone truncated it after opening it— a
    /// [`SpoolError::Malformed`] comes out, not a half plan.
    ///
    /// The error can arrive HALFWAY, with steps already executed: whoever
    /// consumes it needs its journal batch closed and undoable at that
    /// point, not only at the cancellation one.
    #[must_use]
    pub fn steps(self) -> impl FusedStream<Item = Result<SpoolStep, SpoolError>> {
        struct State {
            reader: Option<BufReader<std::fs::File>>,
            queue: VecDeque<SpoolStep>,
        }
        let state = State {
            reader: Some(BufReader::new(self.file)),
            queue: VecDeque::new(),
        };
        futures::stream::try_unfold(state, |mut state| async move {
            loop {
                if let Some(step) = state.queue.pop_front() {
                    return Ok(Some((step, state)));
                }
                let Some(mut reader) = state.reader.take() else {
                    return Ok(None);
                };
                let (reader, batch, ended) = crate::blocking::spawn_blocking(move || {
                    let mut batch = Vec::new();
                    let mut read = 0usize;
                    let mut line = Vec::new();
                    let ended = loop {
                        if read >= READ_CHUNK_BYTES {
                            break false;
                        }
                        let n = read_capped_line(&mut reader, &mut line)?;
                        if n == 0 {
                            return Err(SpoolError::Malformed(
                                "the spool ends with no terminator".to_owned(),
                            ));
                        }
                        read += n;
                        match decode(&line)? {
                            Record::Step(step) => batch.push(validated_step(step)?),
                            Record::End(_) => {
                                // Nothing after the terminator. With two,
                                // `open` validates the last one and this
                                // would execute up to the first: two plans
                                // in one file.
                                if read_capped_line(&mut reader, &mut line)? != 0 {
                                    return Err(SpoolError::Malformed(
                                        "there are records after the terminator".to_owned(),
                                    ));
                                }
                                break true;
                            }
                            Record::Head(_) => {
                                return Err(SpoolError::Malformed(
                                    "a header in the middle of the spool".to_owned(),
                                ));
                            }
                        }
                    };
                    Ok::<_, SpoolError>((reader, batch, ended))
                })
                .await
                .map_err(joined)??;
                state.queue = batch.into();
                if !ended {
                    state.reader = Some(reader);
                }
            }
        })
        .fuse()
    }
}

// ------------------------------------------------------------------ blocking

/// A step read from disk, checked.
///
/// On the WIRE, `SyncStepKind` degrades to `Unknown` and a malformed step
/// doesn't kill a batch of 256 (ADR 0049) — there, forward compatibility is
/// worth more. In a file THIS binary wrote minutes ago there's no
/// compatibility to defend: a class we don't recognize or a shape that
/// doesn't hold up can only be corruption or tampering, and a step like
/// that would be about to authorize a write.
///
/// `shape_is_consistent` is free here and is rule 4 checked wherever
/// possible: an `Overwrite` that claims to undo by deleting would make the
/// journal note a false reversal.
fn validated_step(record: SpoolStep) -> Result<SpoolStep, SpoolError> {
    if record.step.kind == norte_proto::methods::SyncStepKind::Unknown {
        return Err(SpoolError::Malformed(
            "an unknown-class step in a spool we wrote ourselves".to_owned(),
        ));
    }
    if !record.step.shape_is_consistent() {
        return Err(SpoolError::Malformed(
            "a step whose class, reversal and reason don't agree".to_owned(),
        ));
    }
    Ok(record)
}

/// Creates the directory with owner-only permissions and nothing else.
fn ensure_dir(dir: &Path) -> Result<(), SpoolError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        // `recursive` + `mode` for the state directory too, which is what
        // `Journal::open` does with its own: creating it with the umask
        // would leave it at 0755 depending on who gets there first.
        builder.mode(0o700);
    }
    if let Some(parent) = dir.parent() {
        let mut parents = std::fs::DirBuilder::new();
        parents.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            parents.mode(0o700);
        }
        parents.create(parent)?;
    }
    match builder.create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    // It already existed. Make sure it's a real directory and not a link
    // elsewhere, and that its permissions are still what we say they are: a
    // spool in a world-readable directory is a world-readable plan.
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(SpoolError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "the spools directory is not a directory",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// This process's plan counter. With the pid in front, it's enough that two
/// in-flight plans of the same connection don't fight over the name.
static NEXT_PART: AtomicU64 = AtomicU64::new(0);

/// Opens the `.part`, `0o600` at birth.
fn create_part(dir: &Path, conn_id: u64) -> Result<(std::fs::File, PathBuf), SpoolError> {
    let pid = std::process::id();
    for _ in 0..8 {
        let seq = NEXT_PART.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{conn_id}-{pid}-{seq}.part"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // `mode` in the CALL, not a `chmod` afterward: between creating
            // and changing permissions there's a window where the plan is
            // readable by anyone, and that window is the entire bug.
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(file) => return Ok((file, path)),
            // `create_new` is also the guarantee that we never write inside
            // a file that was already there: retried under another name.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(SpoolError::Io(io::Error::new(
        ErrorKind::AlreadyExists,
        "no free name for the spool",
    )))
}

/// The name a plan is looked up by. Neither `conn_id` nor the hash can
/// carry a path separator, so there's nothing to sanitize.
fn file_name(conn_id: u64, hash: &PlanHash) -> String {
    format!("{conn_id}-{}.jsonl", hash.as_str())
}

fn open_blocking(
    path: &Path,
    conn_id: u64,
    want: &PlanHash,
) -> Result<(std::fs::File, SpoolHeader, SpoolSummary), SpoolError> {
    let mut file = match std::fs::OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(SpoolError::NotFound),
        Err(e) => return Err(e.into()),
    };
    // `fstat` of the open descriptor, not `stat` of the path: another file
    // could fit between looking at the path and opening it.
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(SpoolError::NotFound);
    }
    if expired(&meta) {
        drop(file);
        remove_if_same_inode(path, &meta);
        return Err(SpoolError::Expired);
    }

    // The LAST line first: with no terminator the file isn't a plan, and
    // this way the whole thing isn't walked to find that out.
    let tail = read_last_line(&mut file, meta.len())?;
    let Record::End(summary) = decode(&tail)? else {
        return Err(SpoolError::Malformed(
            "the spool's last line is not its terminator".to_owned(),
        ));
    };

    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let head_len = read_capped_line(&mut reader, &mut line)?;
    if head_len == 0 {
        return Err(SpoolError::Malformed("empty spool".to_owned()));
    }
    let Record::Head(header) = decode(&line)? else {
        return Err(SpoolError::Malformed(
            "the spool's first line is not its header".to_owned(),
        ));
    };

    // The filename states a connection and a hash; the CONTENT has to state
    // the same ones. A file renamed by hand doesn't open.
    if header.format != SPOOL_FORMAT {
        return Err(SpoolError::Malformed(format!(
            "spool format {} (this binary writes {SPOOL_FORMAT})",
            header.format
        )));
    }
    if header.conn_id != conn_id || &summary.plan_hash != want {
        return Err(SpoolError::Malformed(
            "the spool does not state its name's connection and hash".to_owned(),
        ));
    }
    // `SyncPlanDone`'s invariant, checked here because it decides whether
    // the next point can be done: a plan with no blockers is executable and
    // has its digest recomputed; one with blockers is neither. Without
    // this, a file could declare itself executable AND carry blockers, and
    // slip through the gap with nobody recomputing anything for it.
    if summary.executable != (summary.blockers_total == 0) {
        return Err(SpoolError::Malformed(
            "`executable` does not agree with the number of blockers".to_owned(),
        ));
    }

    let mut file = reader.into_inner();
    if summary.executable {
        verify_digest(&mut file, head_len, &header, &summary.plan_hash, &summary)?;
    }
    file.seek(SeekFrom::Start(head_len as u64))?;
    Ok((file, header, summary))
}

/// Recomputes `plan_hash` and the COUNTERS over the file's steps, and
/// compares them against what the file says about itself.
///
/// The stored summary states a hash, but that's the file talking about
/// itself: [`PlanHasher`] carries no key, so whoever can write to the
/// directory can write any plan AND its digest. What makes the name worth
/// something is [`Spool`]'s in-memory record; what makes the CONTENT worth
/// something is this. Without it, nobody detects editing a `kind` of a plan
/// the human already approved.
///
/// Costs one more sequential read. Next to executing the plan —one
/// provider operation per step— it's noise.
///
/// Only called over plans with no blockers: the stored list is trimmed to
/// [`SYNC_MAX_BLOCKERS_REPORTED`] and the digest covers ALL of them, so one
/// with blockers cannot be recomputed. It cannot be executed either.
fn verify_digest(
    file: &mut std::fs::File,
    head_len: usize,
    header: &SpoolHeader,
    want: &PlanHash,
    summary: &SpoolSummary,
) -> Result<(), SpoolError> {
    file.seek(SeekFrom::Start(head_len as u64))?;
    let mut reader = BufReader::new(file);
    let mut hasher = PlanHasher::new(&header.options, &header.compare);
    let mut counts = SyncCounts::default();
    let mut line = Vec::new();
    loop {
        if read_capped_line(&mut reader, &mut line)? == 0 {
            return Err(SpoolError::Malformed(
                "the spool ends with no terminator".to_owned(),
            ));
        }
        match decode(&line)? {
            Record::Step(record) => {
                let record = validated_step(record)?;
                // The counters are REBUILT, not trusted. They're what the
                // executor looks at to decide which policy gates to ask for
                // (`Mkdir` if the plan creates directories, `Delete` if it
                // overwrites or deletes), and they live in the terminator,
                // which the digest does NOT cover: without this, a file
                // with the steps intact and `overwrite: 0` would pass
                // verification and run with nobody asking about the
                // delete.
                counts.add(&record.step);
                hasher.step(&record.step);
            }
            Record::End(_) => {
                // Nothing after the terminator, and it's checked HERE and
                // not only in `steps()`: with two IDENTICAL terminators the
                // digest matches —`read_last_line` read the second and this
                // stopped at the first—, so without this the file would
                // open and blow up mid-execution instead of before
                // starting.
                if read_capped_line(&mut reader, &mut line)? != 0 {
                    return Err(SpoolError::Malformed(
                        "there are records after the terminator".to_owned(),
                    ));
                }
                break;
            }
            Record::Head(_) => {
                return Err(SpoolError::Malformed(
                    "a header in the middle of the spool".to_owned(),
                ));
            }
        }
    }
    if &hasher.finish() != want {
        return Err(SpoolError::Malformed(
            "the recomputed digest is not the one in the name: the spool was tampered with"
                .to_owned(),
        ));
    }
    if counts != summary.counts {
        return Err(SpoolError::Malformed(
            "the terminator's counters are not the steps'".to_owned(),
        ));
    }
    Ok(())
}

/// Deletes `path` only if it still names the inode that was looked at.
///
/// Re-planning the same tree with the same options produces the SAME hash,
/// and `finish` renames over it. Without this check, a late-arriving `open`
/// with the old file's descriptor would delete by name the just-approved
/// plan, and the human would get "your plan expired" over one from seconds
/// ago.
fn remove_if_same_inode(path: &Path, opened: &std::fs::Metadata) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(now) if now.dev() == opened.dev() && now.ino() == opened.ino() => {}
            // No longer there, or already another file: in both cases, not ours.
            _ => return,
        }
    }
    let _ = std::fs::remove_file(path);
}

/// Did it pass the TTL? A mtime in the FUTURE counts as fresh: a clock
/// running backward isn't a reason to throw out a plan someone is looking
/// at, and the ceiling is set just the same by the startup sweep.
fn expired(meta: &std::fs::Metadata) -> bool {
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > Duration::from_millis(SYNC_PLAN_TTL_MS))
}

/// The last complete line, reading backward in windows.
///
/// The ceiling is [`SPOOL_MAX_RECORD`] **plus two**: to find a line of N
/// content bytes you need to see its own line ending and the previous
/// one's, so a ceiling of exactly N would reject a terminator that
/// `read_capped_line` DOES accept — and `encode` DOES write.
fn read_last_line(file: &mut std::fs::File, len: u64) -> Result<Vec<u8>, SpoolError> {
    if len == 0 {
        return Err(SpoolError::Malformed("empty spool".to_owned()));
    }
    let ceiling = SPOOL_MAX_RECORD as u64 + 2;
    let mut window: u64 = 8 * 1024;
    loop {
        let start = len.saturating_sub(window);
        let take = usize::try_from(len - start)
            .map_err(|_| SpoolError::Malformed("unmanageable spool".to_owned()))?;
        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; take];
        file.read_exact(&mut buf).map_err(truncated)?;
        let body = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if let Some(pos) = body.iter().rposition(|b| *b == b'\n') {
            return Ok(body[pos + 1..].to_vec());
        }
        if start == 0 {
            return Ok(body.to_vec());
        }
        if window >= ceiling {
            return Err(SpoolError::Malformed(
                "the spool's last record exceeds the cap".to_owned(),
            ));
        }
        window = (window * 2).min(ceiling);
    }
}

/// A file that shrinks under our feet is a broken spool —i.e. a stale
/// plan—, not a daemon I/O failure: `is_stale` tells the two apart and the
/// client deserves the first answer.
fn truncated(e: io::Error) -> SpoolError {
    if e.kind() == ErrorKind::UnexpectedEof {
        SpoolError::Malformed("the spool was truncated while being read".to_owned())
    } else {
        SpoolError::Io(e)
    }
}

/// Reads ONE capped line. `Ok(0)` is end of file.
fn read_capped_line(reader: &mut impl BufRead, out: &mut Vec<u8>) -> Result<usize, SpoolError> {
    out.clear();
    let n = reader
        .by_ref()
        .take(SPOOL_MAX_RECORD as u64 + 1)
        .read_until(b'\n', out)
        .map_err(truncated)?;
    if n == 0 {
        return Ok(0);
    }
    if out.last() != Some(&b'\n') {
        return Err(SpoolError::Malformed(
            "a record with no line ending: truncated spool, or the record exceeds the cap"
                .to_owned(),
        ));
    }
    out.pop();
    Ok(n)
}

/// Deletes the directory's files matching `pred`, over the name's BYTES
/// (rule 1: a filename isn't a `String`).
///
/// A delete that fails does NOT abort the sweep and is NOT silent: it goes
/// to [`SweepReport::failed`], because a bare delete counter doesn't tell
/// "there was nothing" apart from "couldn't with anything".
fn remove_matching(dir: &Path, pred: impl Fn(&[u8]) -> bool) -> Result<SweepReport, SpoolError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // Not existing is normal on first startup.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(SweepReport::default()),
        Err(e) => return Err(e.into()),
    };
    let mut report = SweepReport::default();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !pred(name_bytes(&name)) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                report.failed += 1;
                tracing::warn!(path = %entry.path().display(), error = %e,
                    "could not delete a spool");
            }
        }
    }
    Ok(report)
}

/// Takes the CLOSED plans that passed [`SYNC_PLAN_TTL_MS`].
///
/// Called by [`Spool::create`], and that's the only clock the TTL has:
/// [`Spool::open`]'s check only reaches plans someone opens, and a plan
/// nobody opens is exactly the one that's excess. Without this, a client
/// that plans in a loop varying `include` —each selection gives a different
/// digest, i.e. a different file— fills up the state directory, which is
/// where `journal.db` lives.
///
/// **Only `.jsonl`.** A `.part` is a plan IN PROGRESS and can legitimately
/// take hours over a network tree; orphaned ones are handled by the
/// writer's `Drop` and the startup sweep.
///
/// Returns nothing and doesn't fail upward: it's opportunistic maintenance,
/// and a file resisting isn't a reason to stop letting planning happen. The
/// in-memory record is still what decides what's applicable.
fn reap_expired(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !name_bytes(&entry.file_name()).ends_with(b".jsonl") {
            continue;
        }
        if entry.metadata().is_ok_and(|m| expired(&m)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A filename's bytes. On unix they're the real ones; elsewhere, whatever
/// can be had — spool names are ASCII by construction (digits, `-` and
/// lowercase hex), so none get lost.
fn name_bytes(name: &std::ffi::OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes()
    }
    #[cfg(not(unix))]
    {
        name.to_str().unwrap_or("").as_bytes()
    }
}

/// A serialized record, with its line ending.
///
/// The cap is checked HERE and not only on reading. A terminator with 256
/// very long `rel` blockers can exceed [`SPOOL_MAX_RECORD`], and if it gets
/// written, `finish` returns a hash and an `executable: true` for a plan no
/// later `open` will ever be able to read again: the client would approve a
/// plan that answers "stale" forever. Failing to write it puts the error
/// where it can be seen.
fn encode(record: &Record) -> Result<Vec<u8>, SpoolError> {
    let mut line = serde_json::to_vec(record)
        .map_err(|e| SpoolError::Malformed(format!("could not serialize the record: {e}")))?;
    if line.len() > SPOOL_MAX_RECORD {
        return Err(SpoolError::Malformed(format!(
            "a {}-byte record exceeds the {SPOOL_MAX_RECORD}-byte cap",
            line.len()
        )));
    }
    line.push(b'\n');
    Ok(line)
}

fn decode(line: &[u8]) -> Result<Record, SpoolError> {
    serde_json::from_slice(line).map_err(|e| SpoolError::Malformed(e.to_string()))
}

/// A `spawn_blocking` that never returns is a runtime failure, not the plan's.
fn joined(e: tokio::task::JoinError) -> SpoolError {
    SpoolError::Io(io::Error::other(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt as _;
    use norte_proto::VPath;
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, OnUnknown, RelPath, Side, StepReversal,
        SyncBlockerKind, SyncMode, SyncReason, SyncStepKind,
    };

    // ------------------------------------------------------------ fixtures

    fn opts() -> SyncOptions {
        SyncOptions {
            source_root: VPath::parse("mem:///source").expect("path"),
            dest_root: VPath::parse("mem:///dest").expect("path"),
            mode: SyncMode::Update,
            on_unknown: OnUnknown::Copy,
            source_side: Side::Left,
            dest_has_trash: true,
            dest_trash_restorable: true,
            dest_writable: true,
        }
    }

    fn compare_opts() -> SyncCompareOptions {
        SyncCompareOptions::default()
    }

    fn rel(wire: &str) -> RelPath {
        RelPath::parse_wire(wire).expect("rel")
    }

    fn copy_step(id: u64, name: &str, size: u64) -> SyncStep {
        SyncStep {
            id,
            kind: SyncStepKind::Copy,
            rel: rel(name),
            dest_rel: None,
            size: Some(size),
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            reversal: Some(StepReversal::Delete),
            reason: None,
        }
    }

    /// A tabletop plan with the four shapes the executor has to
    /// distinguish: a copy, a directory, an irreversible overwrite and a
    /// skip. With a non-UTF-8 name inside, which is what rule 1 asks for.
    fn steps_fixture() -> Vec<SyncStep> {
        vec![
            SyncStep {
                id: 1,
                kind: SyncStepKind::CreateDir,
                rel: rel("sub"),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: Some(StepReversal::Delete),
                reason: None,
            },
            copy_step(2, "sub/informe%FF%FE.dat", 12),
            SyncStep {
                id: 3,
                kind: SyncStepKind::Overwrite,
                rel: rel("NOTAS/a.txt"),
                dest_rel: Some(rel("notas/a.txt")),
                size: Some(40),
                criterion: CompareCriterion::Mtime,
                confidence: CompareConfidence::Probable,
                reversal: Some(StepReversal::Irreversible),
                reason: Some(SyncReason::NoTrashOnTarget),
            },
            SyncStep {
                id: 4,
                kind: SyncStepKind::Skip,
                rel: rel("ilegible"),
                dest_rel: None,
                size: None,
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Unknown,
                reversal: None,
                reason: Some(SyncReason::Unreadable),
            },
        ]
    }

    /// Writes a whole plan and returns its hash.
    async fn write_plan(spool: &Spool, conn_id: u64, steps: &[SyncStep]) -> PlanHash {
        let mut w = spool
            .create(conn_id, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps {
            w.push(&PlanItem::Step {
                step: s.clone(),
                dest: None,
            })
            .await
            .expect("push");
        }
        w.finish(PlanOutcome::Ended)
            .await
            .expect("finish")
            .plan_hash
    }

    fn spool_files(spool: &Spool) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(spool.dir()) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd.map(|e| e.expect("entry").path()).collect();
        v.sort();
        v
    }

    /// Ages a file's mtime. `std::fs::FileTimes` instead of a new dependency.
    fn age(path: &Path, ms: u64) {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open to age it");
        let when = SystemTime::now() - Duration::from_millis(ms);
        f.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("set_times");
    }

    // --------------------------------------------------------------- tests

    #[tokio::test]
    async fn a_written_plan_reads_back_step_for_step() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;

        let reader = spool.open(1, &hash).await.expect("open");
        assert_eq!(reader.header().options, opts());
        assert_eq!(reader.header().compare, compare_opts());
        assert_eq!(reader.summary().counts.copy, 1);
        assert_eq!(reader.summary().counts.overwrite, 1);
        assert_eq!(reader.summary().counts.create_dir, 1);
        assert_eq!(reader.summary().counts.skip, 1);
        assert_eq!(reader.summary().counts.irreversible, 1);
        assert!(reader.summary().executable);

        let read: Vec<SyncStep> = reader
            .steps()
            .map_ok(|record| record.step)
            .try_collect()
            .await
            .expect("steps");
        assert_eq!(
            read,
            steps_fixture(),
            "byte for byte, hostile name included"
        );
    }

    #[tokio::test]
    async fn another_connection_cannot_open_it() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        assert!(
            matches!(spool.open(2, &hash).await, Err(SpoolError::NotFound)),
            "nobody applies a plan they didn't produce, not even knowing its hash"
        );
        assert!(spool.open(1, &hash).await.is_ok(), "the owner does");
    }

    #[tokio::test]
    async fn a_spool_renamed_into_another_connection_still_does_not_open() {
        // Two barriers, and the memory one trips first: this process never
        // emitted any plan for connection 2, so the disk isn't even looked
        // at.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        std::fs::rename(
            spool.dir().join(file_name(1, &hash)),
            spool.dir().join(file_name(2, &hash)),
        )
        .expect("rename");
        let e = spool.open(2, &hash).await.expect_err("refused");
        assert!(matches!(e, SpoolError::NotFound));
        assert!(e.is_stale());
    }

    #[test]
    fn the_second_barrier_is_the_file_itself_saying_another_connection() {
        // And if the memory one weren't there: the header carries `conn_id`
        // and the terminator the hash, and `open` compares them against the
        // NAME's.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let hash = rt.block_on(write_plan(&spool, 1, &steps_fixture()));
        std::fs::rename(
            spool.dir().join(file_name(1, &hash)),
            spool.dir().join(file_name(2, &hash)),
        )
        .expect("rename");
        let e =
            open_blocking(&spool.dir().join(file_name(2, &hash)), 2, &hash).expect_err("refused");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
    }

    #[tokio::test]
    async fn a_plan_this_process_did_not_issue_cannot_be_opened() {
        // The forgery: `PlanHasher` carries no key, so whoever can write to
        // the directory can write a plan AND compute its digest. What
        // prevents it is that this process never emitted it.
        let dir = tempfile::tempdir().expect("tmp");
        let writer = Spool::new(dir.path());
        let hash = write_plan(&writer, 1, &steps_fixture()).await;
        assert!(
            writer.open(1, &hash).await.is_ok(),
            "whoever emitted it, yes"
        );

        // Another process (another `Spool`) over the SAME directory: the
        // file is there, with its correct name and its correct digest.
        let stranger = Spool::new(dir.path());
        assert!(
            spool_files(&stranger).len() == 1,
            "the file is still on disk"
        );
        assert!(matches!(
            stranger.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_plan_can_only_be_applied_once() {
        // Two `sync.apply`s of the same hash would run the plan twice
        // against the same destination, with two journal batches.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        assert!(spool.open(1, &hash).await.is_ok());
        assert!(
            matches!(spool.open(1, &hash).await, Err(SpoolError::NotFound)),
            "the right to apply is consumed on opening"
        );
    }

    #[tokio::test]
    async fn a_tampered_step_is_caught_by_the_recomputed_digest() {
        // The summary states a hash, but that's the file talking about itself.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        // A copy becomes an overwrite: same file size, summary intact,
        // something completely different over the destination.
        let touched = text.replacen("\"kind\":\"copy\"", "\"kind\":\"overwrite\"", 1);
        assert_ne!(touched, text, "there was a step to touch");
        std::fs::write(&path, touched).expect("write");

        let e = spool.open(1, &hash).await.expect_err("refused");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale());
    }

    #[tokio::test]
    async fn a_step_of_an_unknown_kind_on_disk_is_refused_not_degraded() {
        // On the wire, an unknown class degrades so as not to kill a batch
        // of 256. In a file we wrote ourselves minutes ago there's no
        // compatibility to defend: it can only be corruption.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        std::fs::write(
            &path,
            text.replacen("\"kind\":\"copy\"", "\"kind\":\"teleport\"", 1),
        )
        .expect("write");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn records_after_the_terminator_are_refused() {
        // With two terminators, `open` validates the LAST one and
        // `steps()` would stop at the first: the approved summary and the
        // executed plan would be two different things.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let mut text = std::fs::read_to_string(&path).expect("read");
        let terminator = text.lines().last().expect("terminator").to_owned();
        text.push_str(&terminator);
        text.push('\n');
        std::fs::write(&path, text).expect("write");

        // And it's refused on OPENING, not mid-execution: with two identical
        // terminators the digest would match, so the check cannot be the
        // digest one alone.
        let e = spool.open(1, &hash).await.expect_err("refused");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale());
    }

    #[tokio::test]
    async fn a_second_different_terminator_cannot_swap_the_summary() {
        // The case with teeth: `open` validates the LAST terminator and
        // `steps()` would stop at the first. The recomputed digest catches
        // it because it only covers the steps before the first one.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let other = write_plan(&spool, 2, &[copy_step(9, "other.txt", 1)]).await;
        let path = spool.dir().join(file_name(1, &hash));

        let mine = std::fs::read_to_string(&path).expect("read");
        let stranger =
            std::fs::read_to_string(spool.dir().join(file_name(2, &other))).expect("read");
        let head: Vec<&str> = stranger.lines().collect();
        let mut stitched: Vec<&str> = mine.lines().collect();
        stitched.push(head.last().expect("stranger's terminator"));
        std::fs::write(&path, stitched.join("\n") + "\n").expect("write");

        // The name is still the approved plan's, but the last line no
        // longer is: not even the name's hash matches the new terminator.
        let e = spool.open(1, &hash).await.expect_err("refused");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
    }

    #[tokio::test]
    async fn a_spool_of_another_format_version_is_stale() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        // Patched against `SPOOL_FORMAT` and not a literal number: the
        // format goes up every time a step's record changes shape, and
        // this test is about ANY other version, not specifically the next
        // one.
        let mine = format!("\"format\":{SPOOL_FORMAT}");
        let other = format!("\"format\":{}", SPOOL_FORMAT + 1);
        std::fs::write(&path, text.replacen(&mine, &other, 1)).expect("write");
        let e = spool.open(1, &hash).await.expect_err("refused");
        assert!(matches!(e, SpoolError::Malformed(_)), "{e:?}");
        assert!(e.is_stale(), "another format version is a stale plan");
    }

    #[tokio::test]
    async fn a_header_in_the_middle_is_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        let header = text.lines().next().expect("header").to_owned();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.insert(2, &header);
        std::fs::write(&path, lines.join("\n") + "\n").expect("write");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn an_interrupted_plan_leaves_nothing_and_yields_no_hash() {
        // The loop that lets a `Some(Err(_))` through would close a plan
        // that's only a third done with a perfectly valid hash. That's why
        // `finish` requires stating how the stream ended.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step {
                step: s,
                dest: None,
            })
            .await
            .expect("push");
        }
        assert!(matches!(
            w.finish(PlanOutcome::Interrupted).await,
            Err(SpoolError::Interrupted)
        ));
        assert!(spool_files(&spool).is_empty(), "not even the .part");
    }

    #[tokio::test]
    async fn dropping_a_writer_inside_a_runtime_removes_its_part() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        {
            let mut w = spool
                .create(1, &opts(), &compare_opts())
                .await
                .expect("create");
            w.push(&PlanItem::Step {
                step: copy_step(1, "a.txt", 1),
                dest: None,
            })
            .await
            .expect("push");
        }
        assert!(spool_files(&spool).is_empty(), "Drop takes it");
    }

    #[test]
    fn dropping_a_writer_outside_a_runtime_also_removes_its_part() {
        // `Drop` deletes SYNCHRONOUSLY on purpose: `Handle::spawn_blocking`
        // panics if the runtime is shutting down, and a panic in a `Drop`
        // during unwinding aborts the process.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let w = rt
            .block_on(spool.create(1, &opts(), &compare_opts()))
            .expect("create");
        assert_eq!(spool_files(&spool).len(), 1, "the .part is there");
        drop(rt); // the runtime leaves BEFORE the writer
        drop(w);
        assert!(spool_files(&spool).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_sweep_that_cannot_delete_says_so_instead_of_counting_zero() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        write_plan(&spool, 1, &steps_fixture()).await;
        // A directory with no write permission: the file is there and
        // won't unlink.
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let report = spool.sweep().await.expect("sweep");
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o700))
            .expect("rechmod");
        assert_eq!(
            (report.removed, report.failed),
            (0, 1),
            "a bare delete counter would have said 0 and looked clean"
        );
        assert!(!report.is_clean());
    }

    // ------------------------------------------------------ the backward read

    /// Writes `lines` to a temp file and returns its last line according to
    /// [`read_last_line`].
    fn last_line_of(lines: &[String]) -> Result<Vec<u8>, SpoolError> {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("f");
        let mut body = String::new();
        for l in lines {
            body.push_str(l);
            body.push('\n');
        }
        std::fs::write(&path, &body).expect("write");
        let mut f = std::fs::File::open(&path).expect("open");
        let len = f.metadata().expect("meta").len();
        read_last_line(&mut f, len)
    }

    #[test]
    fn the_backwards_read_finds_the_last_line_whatever_its_size() {
        // The window starts at 8 KiB and doubles. These cases force it to
        // double once, twice, and not at all.
        for size in [1usize, 4 * 1024, 12 * 1024, 20 * 1024] {
            let last = "z".repeat(size);
            let read =
                last_line_of(&["a".to_owned(), "bb".to_owned(), last.clone()]).expect("last line");
            assert_eq!(read, last.as_bytes(), "with a last line of {size} B");
        }
    }

    #[test]
    fn the_backwards_read_handles_a_single_line_and_a_boundary() {
        // A single-line file: there's no earlier newline to find.
        let only = "only".to_owned();
        assert_eq!(
            last_line_of(std::slice::from_ref(&only)).expect("line"),
            only.as_bytes()
        );
        // And a last line that starts RIGHT at the first window's edge: 8
        // KiB of prior content + its newline.
        let prior = "p".repeat(8 * 1024 - 1);
        let last = "u".repeat(16);
        assert_eq!(
            last_line_of(&[prior, last.clone()]).expect("line"),
            last.as_bytes()
        );
    }

    #[test]
    fn a_last_record_over_the_cap_is_malformed_not_an_oom() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("f");
        let mut body = vec![b'a', b'\n'];
        body.extend(std::iter::repeat_n(b'z', SPOOL_MAX_RECORD + 8));
        body.push(b'\n');
        std::fs::write(&path, &body).expect("write");
        let mut f = std::fs::File::open(&path).expect("open");
        let len = f.metadata().expect("meta").len();
        assert!(matches!(
            read_last_line(&mut f, len),
            Err(SpoolError::Malformed(_))
        ));
    }

    #[test]
    fn a_record_the_reader_could_never_accept_fails_when_it_is_written() {
        // A terminator with huge `rel` blockers exceeds the cap. If it were
        // written, `finish` would give a hash for a plan no `open` could
        // ever read again: stale forever, with no recovery.
        let huge: Vec<SyncBlocker> = (0..SYNC_MAX_BLOCKERS_REPORTED)
            .map(|i| SyncBlocker {
                rel: rel(&format!("{}{i}", "x".repeat(60_000))),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Right),
            })
            .collect();
        let fat = Record::End(SpoolSummary {
            plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hash"),
            counts: SyncCounts::default(),
            blockers: huge,
            blockers_total: SYNC_MAX_BLOCKERS_REPORTED as u64,
            executable: false,
        });
        assert!(matches!(encode(&fat), Err(SpoolError::Malformed(_))));
    }

    #[tokio::test]
    async fn an_unfinished_spool_cannot_be_opened() {
        // A daemon that dies halfway leaves nothing that looks approvable.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hasher = PlanHasher::new(&opts(), &compare_opts());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let step = copy_step(1, "a.txt", 1);
        w.push(&PlanItem::Step {
            step: step.clone(),
            dest: None,
        })
        .await
        .expect("push");
        std::mem::forget(w); // like a `kill -9`: neither `finish` nor `Drop`.

        // The hash that plan WOULD HAVE had: it doesn't open even with it.
        let mut h = hasher;
        h.step(&step);
        let hash = h.finish();
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn abandoning_a_writer_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        w.push(&PlanItem::Step {
            step: copy_step(1, "a.txt", 1),
            dest: None,
        })
        .await
        .expect("push");
        w.abandon().await;
        assert!(spool_files(&spool).is_empty(), "not even the .part");
    }

    #[tokio::test]
    async fn a_plan_past_its_ttl_is_gone() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        age(&spool.dir().join(file_name(1, &hash)), SYNC_PLAN_TTL_MS + 1);

        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::Expired)
        ));
        assert!(
            spool_files(&spool).is_empty(),
            "open deletes the expired one as it finds it"
        );
    }

    #[tokio::test]
    async fn a_plan_just_inside_its_ttl_still_opens() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        age(&spool.dir().join(file_name(1, &hash)), SYNC_PLAN_TTL_MS / 2);
        assert!(spool.open(1, &hash).await.is_ok());
    }

    #[tokio::test]
    async fn closing_a_connection_drops_its_plans_and_only_its_plans() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let a = write_plan(&spool, 1, &steps_fixture()).await;
        let b = write_plan(&spool, 2, &[copy_step(1, "other.txt", 3)]).await;
        // And a `.part` from the same connection, which is also theirs.
        let _open = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");

        let report = spool.drop_connection(1).await.expect("drop");
        assert_eq!((report.removed, report.failed), (2, 0));
        assert!(matches!(spool.open(1, &a).await, Err(SpoolError::NotFound)));
        assert!(spool.open(2, &b).await.is_ok());
    }

    #[tokio::test]
    async fn dropping_connection_1_does_not_touch_connection_12() {
        // `"1-"` is not a prefix of `"12-…"`, and it's worth keeping it that way.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let one = write_plan(&spool, 1, &steps_fixture()).await;
        let twelve = write_plan(&spool, 12, &steps_fixture()).await;
        assert_eq!(spool.drop_connection(1).await.expect("drop").removed, 1);
        assert!(matches!(
            spool.open(1, &one).await,
            Err(SpoolError::NotFound)
        ));
        assert!(spool.open(12, &twelve).await.is_ok());
    }

    #[tokio::test]
    async fn the_startup_sweep_collects_everything_a_crash_left_behind() {
        // Fresh or expired makes no difference: at startup there's no live
        // connection at all, so EVERY spool that exists belongs to a dead
        // connection — and `conn_id`s start over from zero, so leaving a
        // fresh one would leave it under the name of an id the daemon is
        // about to hand out.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let old = write_plan(&spool, 1, &steps_fixture()).await;
        let fresh = write_plan(&spool, 2, &steps_fixture()).await;
        age(&spool.dir().join(file_name(1, &old)), SYNC_PLAN_TTL_MS + 1);

        let report = spool.sweep().await.expect("sweep");
        assert_eq!((report.removed, report.failed), (2, 0));
        assert!(report.is_clean());
        assert!(matches!(
            spool.open(2, &fresh).await,
            Err(SpoolError::NotFound)
        ));
        assert!(spool_files(&spool).is_empty());
    }

    #[tokio::test]
    async fn sweeping_a_directory_that_was_never_created_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        assert_eq!(spool.sweep().await.expect("sweep"), SweepReport::default());
        assert_eq!(
            spool.drop_connection(7).await.expect("drop"),
            SweepReport::default()
        );
    }

    #[tokio::test]
    async fn removing_an_applied_plan_is_idempotent() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        spool.remove(1, &hash).await.expect("remove");
        spool.remove(1, &hash).await.expect("remove again");
        assert!(matches!(
            spool.open(1, &hash).await,
            Err(SpoolError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_plan_bigger_than_the_write_buffer_round_trips() {
        // Crosses the 64 KiB write flush and the 64 KiB read chunk several
        // times: this is where a half-million-step plan lives.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let steps: Vec<SyncStep> = (0..4_000)
            .map(|i| copy_step(i, &format!("dir{}/f{i}.bin", i % 7), i))
            .collect();
        let hash = write_plan(&spool, 1, &steps).await;
        let reader = spool.open(1, &hash).await.expect("open");
        assert_eq!(reader.summary().counts.copy, 4_000);
        let read: Vec<SyncStep> = reader
            .steps()
            .map_ok(|record| record.step)
            .try_collect()
            .await
            .expect("steps");
        assert_eq!(read, steps);
    }

    #[tokio::test]
    async fn the_steps_stream_is_fused() {
        use futures::StreamExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &[copy_step(1, "a", 1)]).await;
        let mut s = Box::pin(spool.open(1, &hash).await.expect("open").steps());
        assert!(s.next().await.is_some());
        assert!(s.next().await.is_none());
        assert!(s.next().await.is_none(), "a select! with a tick is legal");
    }

    #[tokio::test]
    async fn blockers_make_the_plan_not_executable_and_the_list_is_capped() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let total = SYNC_MAX_BLOCKERS_REPORTED + 10;
        for i in 0..total {
            w.push(&PlanItem::Blocker(SyncBlocker {
                rel: rel(&format!("x{i}")),
                kind: SyncBlockerKind::TypeMismatchDir,
                side: Some(Side::Right),
            }))
            .await
            .expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");
        assert!(!summary.executable);
        assert_eq!(summary.blockers.len(), SYNC_MAX_BLOCKERS_REPORTED);
        assert_eq!(summary.blockers_total, total as u64);

        let reader = spool.open(1, &summary.plan_hash).await.expect("open");
        assert_eq!(reader.summary(), &summary, "what's read is what was closed");
        assert!(
            reader
                .steps()
                .try_collect::<Vec<_>>()
                .await
                .expect("steps")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_stored_hash_is_the_hash_of_the_items_that_were_spooled() {
        // There's only one funnel: one sequence cannot be hashed and
        // another one stored. Checked here against the bare hasher.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let items: Vec<PlanItem> = steps_fixture()
            .into_iter()
            .map(|step| PlanItem::Step { step, dest: None })
            .collect();

        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for i in &items {
            w.push(i).await.expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");

        let mut h = PlanHasher::new(&opts(), &compare_opts());
        for i in &items {
            h.item(i);
        }
        assert_eq!(summary.plan_hash, h.finish());
    }

    #[tokio::test]
    async fn a_plan_hashed_with_other_compare_options_is_another_plan() {
        // What task 6 established: `hash` on isn't the same as size only
        // even if the steps come out equal. That's why the compare options
        // go into the header and the hash's seed.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let a = write_plan(&spool, 1, &steps_fixture()).await;

        let mut other = compare_opts();
        other.mtime_tolerance_ms = 5_000;
        let mut w = spool.create(1, &opts(), &other).await.expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step {
                step: s,
                dest: None,
            })
            .await
            .expect("push");
        }
        let b = w
            .finish(PlanOutcome::Ended)
            .await
            .expect("finish")
            .plan_hash;
        assert_ne!(a, b, "same steps, another question, another plan");
    }

    #[tokio::test]
    async fn a_spool_from_a_binary_that_did_not_know_a_counter_is_stale_not_fatal() {
        // The new counters deliberately carry NO `serde(default)`: a silent
        // zero would turn "340 unmeasured files" into "none". The correct
        // answer is "stale plan", not a dead Task.
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        let mutilated = text.replace(",\"unmeasured_steps\":0", "");
        assert_ne!(mutilated, text, "the counter was where it's believed to be");
        std::fs::write(&path, mutilated).expect("write");

        let e = spool.open(1, &hash).await.expect_err("doesn't read");
        assert!(matches!(e, SpoolError::Malformed(_)));
        assert!(e.is_stale(), "answers PlanStale, not an internal failure");
    }

    #[tokio::test]
    async fn a_spool_whose_terminator_was_cut_off_is_not_approvable() {
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &hash));
        let text = std::fs::read_to_string(&path).expect("read");
        let mut no_ending = String::new();
        for l in text.lines().take(text.lines().count() - 1) {
            no_ending.push_str(l);
            no_ending.push('\n');
        }
        std::fs::write(&path, no_ending).expect("write");

        let e = spool.open(1, &hash).await.expect_err("doesn't read");
        assert!(e.is_stale());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_spool_directory_and_its_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;

        let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(spool.dir()), 0o700);
        assert_eq!(mode(&spool.dir().join(file_name(1, &hash))), 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_spool_directory_that_was_left_world_readable_is_tightened() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        std::fs::create_dir_all(spool.dir()).expect("mkdir");
        std::fs::set_permissions(spool.dir(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");

        write_plan(&spool, 1, &steps_fixture()).await;
        assert_eq!(
            std::fs::metadata(spool.dir())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "it's tightened BEFORE the first file inside is created"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_spool_directory_that_is_a_symlink_is_refused() {
        let dir = tempfile::tempdir().expect("tmp");
        let other = tempfile::tempdir().expect("tmp2");
        let spool = Spool::new(dir.path());
        std::os::unix::fs::symlink(other.path(), spool.dir()).expect("symlink");
        assert!(spool.create(1, &opts(), &compare_opts()).await.is_err());
    }

    /// The one test that cannot be a tautology: it runs a REAL comparison
    /// through the planner and the spool, and searches the file's bytes for
    /// the content of the compared files.
    #[tokio::test]
    async fn a_spool_holds_no_content_only_paths_and_verdicts() {
        use futures::StreamExt as _;
        use norte_compare::{CompareOptions, compare};
        use norte_vfs::Provider as _;
        use tokio_util::sync::CancellationToken;

        const SECRET: &[u8] = b"safe-combination-4815162342";

        async fn seed(mem: &norte_testkit::MemProvider, name: &[u8], content: &[u8]) {
            let at = norte_testkit::MemProvider::root()
                .join(norte_proto::Segment::new(name).expect("segment"));
            let mut sink = mem.write(&at).await.expect("write");
            sink.write(bytes::Bytes::copy_from_slice(content))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }

        let source = norte_testkit::MemProvider::new();
        let dest = norte_testkit::MemProvider::new();
        seed(&source, b"secrets.txt", SECRET).await;
        seed(&dest, b"secrets.txt", b"something else entirely").await;
        seed(&source, b"only-here.txt", SECRET).await;

        let root = norte_testkit::MemProvider::root();
        let sides =
            norte_compare::Sides::from_capabilities(source.capabilities(), dest.capabilities());
        let rows = compare(
            &source,
            &root,
            &dest,
            &root,
            CompareOptions::cheap(),
            sides,
            Vec::new(),
            CancellationToken::new(),
        );
        let plan_opts = SyncOptions {
            source_root: root.clone(),
            dest_root: root,
            ..opts()
        };

        let dir = tempfile::tempdir().expect("tmp");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &plan_opts, &compare_opts())
            .await
            .expect("create");
        let mut items = Box::pin(norte_sync::plan(rows, plan_opts, CancellationToken::new()));
        while let Some(item) = items.next().await {
            w.push(&item.expect("item")).await.expect("push");
        }
        let summary = w.finish(PlanOutcome::Ended).await.expect("finish");
        assert!(
            summary.counts.copy + summary.counts.overwrite >= 2,
            "there was a plan"
        );

        let bytes = std::fs::read(spool.dir().join(file_name(1, &summary.plan_hash)))
            .expect("read the spool");
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET),
            "a file that authorizes writes cannot also be the data"
        );
        // And the other half, the one that keeps this from being a tautology:
        // what SHOULD be there, is — i.e. the search above would have found
        // the secret had it been there.
        assert!(
            bytes
                .windows(b"only-here.txt".len())
                .any(|w| w == b"only-here.txt"),
            "the paths DO travel: the search above works"
        );
    }

    // ------------------------------------- what does NOT let itself close (task 8)

    #[tokio::test]
    async fn a_plan_whose_connection_closed_never_becomes_approvable() {
        // The case does NOT need any race: a plan over two identical trees
        // does not emit a single step, so it never touches its channel and
        // never finds out its owner is gone. If `finish` closed it anyway, a
        // plan would stay retained after the only death that was ever coming
        // for it.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let w = spool
            .create(7, &opts(), &compare_opts())
            .await
            .expect("create");

        spool.drop_connection(7).await.expect("drop");

        let e = w
            .finish(PlanOutcome::Ended)
            .await
            .expect_err("its owner is gone");
        assert!(matches!(e, SpoolError::Interrupted), "was {e:?}");
        assert!(
            spool_files(&spool).is_empty(),
            "nothing can be left, neither `.part` nor `.jsonl`"
        );
    }

    #[tokio::test]
    async fn a_dead_connections_tombstone_does_not_outlive_its_plans() {
        // The moment that connection's last writer closes, the mark goes
        // away: no `finish` of its can arrive anymore, so there is nothing
        // left to remember. That is what keeps the set small in the normal
        // case, and `DEAD_CAP` is the floor for when it isn't.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let w = spool
            .create(7, &opts(), &compare_opts())
            .await
            .expect("create");
        spool.drop_connection(7).await.expect("drop");
        assert!(spool.is_dead(7));
        w.abandon().await;
        assert!(
            !spool.is_dead(7),
            "with no plans in flight there is nothing to mark"
        );
    }

    /// **A plan that STARTS after the unmount is not retained either.**
    ///
    /// Before, the tombstone was only set if the connection already had an
    /// open writer. A `sync.plan` that opened its own an instant after the
    /// unmount never found out anything, finished quite calmly, and left
    /// behind a retained plan for a connection that no longer exists: nobody
    /// can apply it and nobody is going to collect it.
    ///
    /// It surfaced as an intermittent red in
    /// `engine_sync_plan::a_zero_step_plan_whose_owner_left_is_not_retained`,
    /// because the order of the two things is not fixed — and here an
    /// intermittent red is a bug, not noise. This test pins the bad order.
    #[tokio::test]
    async fn a_plan_that_starts_after_the_unmount_is_not_retained() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        // The unmount goes FIRST, with no plan in flight.
        spool.drop_connection(9).await.expect("drop");
        // And the plan starts afterwards, as if the task had lost the race
        // by a hair.
        let w = spool
            .create(9, &opts(), &compare_opts())
            .await
            .expect("create");
        // It is DENIED, just like one that was half-done when its owner
        // died: a plan with no owner isn't a half-finished plan, it's a plan
        // nobody can apply.
        let e = w
            .finish(PlanOutcome::Ended)
            .await
            .expect_err("its owner is gone");
        assert!(matches!(e, SpoolError::Interrupted), "was {e:?}");
        assert_eq!(spool.retained_for(9), 0, "and nothing of its is retained");
        assert!(
            spool_files(&spool).is_empty(),
            "and no file of its is left on disk"
        );
    }

    #[tokio::test]
    async fn replanning_what_is_being_applied_does_not_re_mint_the_claim() {
        // Replanning the same tree with the same options gives the SAME
        // hash. Without this, `finish` would point at a claim an `open` had
        // just consumed: two runs of the same plan against the same
        // destination, with two journal batches and an undo that no longer
        // describes any state that was ever actually passed through.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let hash = write_plan(&spool, 1, &steps_fixture()).await;
        let _reader = spool.open(1, &hash).await.expect("it's being applied");

        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        for s in steps_fixture() {
            w.push(&PlanItem::Step {
                step: s,
                dest: None,
            })
            .await
            .expect("push");
        }
        let e = w
            .finish(PlanOutcome::Ended)
            .await
            .expect_err("that plan is being applied");
        assert!(matches!(e, SpoolError::Interrupted), "was {e:?}");
        assert!(!spool.claim_issued(1, &hash), "the claim did not come back");

        // And the moment the application finishes and calls `remove`,
        // planning can resume as normal.
        spool.remove(1, &hash).await.expect("remove");
        let another = write_plan(&spool, 1, &steps_fixture()).await;
        assert_eq!(another, hash);
        assert!(spool.open(1, &another).await.is_ok());
    }

    #[tokio::test]
    async fn planning_sweeps_expired_plans() {
        // The TTL used to be checked only inside `open`, and a plan nobody
        // opens never gets opened: without this sweep, "ten minutes" wasn't
        // one of the four deaths but a check that ran when it no longer
        // mattered.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let old = write_plan(&spool, 1, &steps_fixture()).await;
        let path = spool.dir().join(file_name(1, &old));
        age(&path, SYNC_PLAN_TTL_MS + 60_000);

        // A NEW plan from another connection: sweeps in passing.
        let mut w = spool
            .create(2, &opts(), &compare_opts())
            .await
            .expect("create");
        assert!(!path.exists(), "the expired one left while planning");
        w.push(&PlanItem::Step {
            step: copy_step(1, "a.txt", 1),
            dest: None,
        })
        .await
        .expect("push");
        w.finish(PlanOutcome::Ended).await.expect("finish");
    }

    #[tokio::test]
    async fn a_plan_in_progress_is_not_swept_by_its_own_age() {
        // A `.part` is a plan IN PROGRESS: over a network tree it can
        // legitimately take hours, and its mtime is that of when it started.
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::new(dir.path());
        let mut w = spool
            .create(1, &opts(), &compare_opts())
            .await
            .expect("create");
        let part = spool_files(&spool)
            .first()
            .cloned()
            .expect("there is a .part");
        age(&part, SYNC_PLAN_TTL_MS + 60_000);

        let w2 = spool
            .create(2, &opts(), &compare_opts())
            .await
            .expect("create");
        assert!(part.exists(), "a plan in progress is not an expired plan");
        w2.abandon().await;
        w.push(&PlanItem::Step {
            step: copy_step(1, "a.txt", 1),
            dest: None,
        })
        .await
        .expect("push");
        w.finish(PlanOutcome::Ended).await.expect("finish");
    }
}
