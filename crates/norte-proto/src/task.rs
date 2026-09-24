//! Task types: every long-running operation is a Task with an id, a state and
//! progress (CLAUDE.md hard rule 3). The `task.progress` notification travels
//! coalesced (≤30 Hz) — the coalescing is the emitter's, not the type's.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, VPath};

/// Identifier of a Task, unique per core process.
///
/// Wire: a transparent JSON number.
///
/// ```
/// use norte_proto::TaskId;
/// let id = TaskId::new(42);
/// assert_eq!(serde_json::to_string(&id).unwrap(), "42");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(u64);

impl TaskId {
    /// Builds it from the scheduler's counter.
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// The numeric value.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Class of operation a Task runs (M0: the three VFS mutations).
///
/// `#[non_exhaustive]` (#126): before this, every new variant broke the Rust
/// API for whoever matched exhaustively outside this crate — that is why
/// `Mkdir`, `Embed` and `RenameBatch` touched both frontends in their own
/// commit. It is a property ONLY of the Rust API: invisible in JSON, it does
/// not move the wire, does not touch `#[serde(other)]`, and does not require a
/// protocol version bump. The cost is symmetric with the benefit: an external
/// `match` now needs a `_` arm, so the compiler stops pointing out where a new
/// kind needs a label — every `_` must do the same thing
/// [`TaskKind::Unknown`]'s arm already does in that same match, not invent new
/// behavior.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Copy (possibly recursive, possibly cross-provider).
    Copy,
    /// Move (atomic rename or copy+delete).
    Move,
    /// Delete (post-order recursive).
    Delete,
    /// Session undo: undoes previous mutations LIFO (M3-2).
    ///
    /// NOTE (compat): this variant entered in 0.10.0. A 0.9.x (N-1) client
    /// does NOT know it and its `TaskKind` parse FAILS on receiving it — the
    /// `serde(other)` below protects THIS proto (0.10+) against 0.11+ kinds,
    /// not retroactively against 0.9. In M3-2 the undo is not exposed over
    /// RPC (it never reaches clients), so the break is latent; M3-4 must gate
    /// the emission of new kinds by negotiated version (or assume silent
    /// discard on broadcast and protect `task.list`'s resync).
    Undo,
    /// Live search by name and/or content under a subtree (`fs.search`, M4
    /// live search). Pure read (rule 4 does not apply): no journal.
    ///
    /// NOTE (compat): this variant entered in 0.18.0. Unlike the 0.9→0.10
    /// edge of [`TaskKind::Undo`] (where the `serde(other)` below did NOT yet
    /// exist), a 0.17.x (N-1) client ALREADY has that fallback (since 0.10) —
    /// receiving it degrades it to [`TaskKind::Unknown`] without failing the
    /// parse. No emission gating needed for this edge.
    Search,
    /// Creating a directory (`fs.mkdir`, #104). Mutation: goes through the
    /// journal as `Created` with its undo (rule 4). Entered in 0.31.0; an N-1
    /// client (0.30.x) degrades it to [`TaskKind::Unknown`] via
    /// `serde(other)`, same case as `Search`/`Index`.
    Mkdir,
    /// Creating an EMPTY file (`fs.create`, #290). The same as
    /// [`TaskKind::Mkdir`] with the other node class: mutation, journal
    /// `Created` with its undo. Entered in 0.57.0; an N-1 client (0.56.x)
    /// degrades it to [`TaskKind::Unknown`] via `serde(other)`.
    Create,
    /// Building/updating the search index of a subtree (`index.build`, M4).
    /// Entered in 0.25.0; an N-1 client (0.24.x) degrades it to
    /// [`TaskKind::Unknown`] via `serde(other)`.
    Index,
    /// `index.embed` (0.33.0): generating embeddings for the semantic index.
    /// An N-1 client (0.32.x) degrades it to [`TaskKind::Unknown`] via its
    /// `serde(other)`.
    Embed,
    /// A batch of renames within ONE directory executed as ONE transaction
    /// with ONE undoable journal unit (`fs.rename_batch`, 0.36.0). Progress is
    /// `i/n` STEPS, not bytes. An N-1 client (0.35.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below, same as
    /// `Search`/`Index`/`Embed`.
    RenameBatch,
    /// Comparing TWO directory trees
    /// (`fs.compare`/[`FS_COMPARE`](crate::methods::FS_COMPARE), 0.39.0, ADR
    /// 0048). Pure read (rule 4 does not apply): no journal, no undo, writes
    /// not a byte. Progress counts PAIRS emitted, not bytes: with the hash
    /// rung off the comparison reads no content at all, so a byte bar would
    /// forever paint zero — same case as [`TaskKind::RenameBatch`].
    ///
    /// Entered WITH the method, in its own bump, and not after: the `task_id`
    /// of a [`COMPARE_ROWS`](crate::methods::COMPARE_ROWS) batch correlates
    /// with a Task the client has to be able to classify in `task.list`. An
    /// N-1 client (0.38.x) degrades it to [`TaskKind::Unknown`] via the
    /// `serde(other)` below, same as `Search`/`Index`/`Embed`/`RenameBatch`.
    Compare,
    /// How much space a directory tree takes up
    /// (`fs.dir_size`/[`FS_DIR_SIZE`](crate::methods::FS_DIR_SIZE), 0.49.0,
    /// #139). Pure read (rule 4 does not apply): no journal, no undo, not a
    /// byte written.
    ///
    /// This one's progress DOES count bytes, unlike [`TaskKind::Compare`]:
    /// the bytes are exactly what is being asked. What it does not carry are
    /// totals — `bytes_total` and `entries_total` stay `None` until the end —
    /// because the total IS the result, and a bar toward a made-up number is
    /// worse than no bar at all.
    ///
    /// Entered WITH the method. An N-1 client (0.48.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below, same as
    /// `Search`/`Index`/`Embed`/`RenameBatch`/`Compare`.
    DirSize,
    /// The content digest of a batch of files
    /// (`fs.checksum`/[`FS_CHECKSUM`](crate::methods::FS_CHECKSUM), 0.59.0,
    /// #311). Pure read (rule 4 does not apply): no journal, no undo, not a
    /// byte written.
    ///
    /// Progress counts bytes and entries, and here there ARE totals from the
    /// start: how many paths were requested is known. What does not fit in
    /// progress are the digests, and that is why the method has a report
    /// ([`FS_CHECKSUM_REPORT`](crate::methods::FS_CHECKSUM_REPORT)).
    ///
    /// Entered WITH the method. An N-1 client (0.58.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below, same as
    /// `Search`/`Index`/`Embed`/`RenameBatch`/`Compare`/`DirSize`.
    Checksum,
    /// What EACH CHILD of a directory takes up (`fs.dir_usage`, 0.75.0, phase
    /// 4 of the 2026-09-15 program). Pure read, like `DirSize`: no journal
    /// and no undo.
    ///
    /// Separate from [`TaskKind::DirSize`] and not a parameter of it: that
    /// one answers ONE number about a selection — "does this fit at the
    /// destination?" — and its total travels in the progress; this one
    /// answers a LIST, which does not fit there and is collected with
    /// `fs.dir_usage_report`. Two different questions, two classes the reader
    /// tells apart in `task.list`.
    ///
    /// Entered WITH the method. An N-1 client (0.74.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below, same as
    /// `Search`/`Index`/`Embed`/`RenameBatch`/`Compare`/`DirSize`/`Checksum`.
    DirUsage,
    /// Changing POSIX permissions of a batch of paths
    /// (`fs.set_mode`/[`FS_SET_MODE`](crate::methods::FS_SET_MODE), 0.60.0,
    /// #314). MUTATES: journal with a reverse and a policy gate (rule 4).
    ///
    /// Progress counts ENTRIES, not bytes: a `chmod` moves none, and a byte
    /// bar here would stay at zero forever. The total is known from the
    /// start, since it is the paths that were sent.
    ///
    /// Entered WITH the method, and an N-1 client (0.59.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below.
    SetMode,
    /// Building an archive
    /// (`archive.pack`/[`ARCHIVE_PACK`](crate::methods::ARCHIVE_PACK), 0.50.0,
    /// #132). MUTATES: journal as ONE creation, and undoing it is deleting the
    /// archive.
    ///
    /// Entered WITH the method, and the four of this bump degrade the same
    /// way: an N-1 client (0.49.x) turns them into [`TaskKind::Unknown`] via
    /// the `serde(other)` below, like `Search`/`Index`/`Embed`/`RenameBatch`/
    /// `Compare`/`DirSize` before them.
    ///
    /// Progress counts bytes READ from the source and entries packed; the
    /// bytes written cannot be known ahead of time — the compressor
    /// decides — and promising a total that will be wrong is worse than not
    /// giving one.
    Pack,
    /// Testing an archive
    /// (`archive.test`/[`ARCHIVE_TEST`](crate::methods::ARCHIVE_TEST), 0.50.0,
    /// #132). Pure read: no journal.
    TestArchive,
    /// Splitting a file into chunks
    /// (`file.split`/[`FILE_SPLIT`](crate::methods::FILE_SPLIT), 0.50.0,
    /// #132). MUTATES: one creation per chunk.
    Split,
    /// Joining chunks back together
    /// (`file.combine`/[`FILE_COMBINE`](crate::methods::FILE_COMBINE), 0.50.0,
    /// #132). MUTATES: one creation.
    Combine,
    /// Planning a one-way synchronization
    /// (`sync.plan`/[`SYNC_PLAN`](crate::methods::SYNC_PLAN), 0.40.0, ADR
    /// 0049). Pure read (rule 4 does not apply): planning writes not a byte —
    /// what writes is [`TaskKind::Sync`]. Progress counts STEPS emitted, not
    /// bytes, for the same reason as [`TaskKind::Compare`]: it is the
    /// comparison underneath with a decision per row, and with the hash rung
    /// off it reads no content at all.
    ///
    /// Entered WITH the method, in its own bump, and for the same reason as
    /// `Compare`: the `task_id` of a [`SYNC_STEPS`](crate::methods::SYNC_STEPS)
    /// batch correlates with a Task the client has to be able to classify in
    /// `task.list`. An N-1 client (0.39.x) degrades it to
    /// [`TaskKind::Unknown`] via the `serde(other)` below.
    SyncPlan,
    /// Executing an APPROVED synchronization plan
    /// (`sync.apply`/[`SYNC_APPLY`](crate::methods::SYNC_APPLY), 0.40.0, ADR
    /// 0049): copies, overwrites and deletes as ONE undoable journal unit
    /// (rule 4). Unlike [`TaskKind::SyncPlan`] its progress does have bytes to
    /// count. An N-1 client (0.39.x) degrades it to [`TaskKind::Unknown`].
    Sync,
    /// Unknown class: an N+1 daemon (0.11+) sent a kind THIS proto does not
    /// know → accepted as generic instead of failing the parse
    /// (forward-compat since 0.10, like [`TaskState::Unknown`]). Does not
    /// cover the 0.9→0.10 backward edge (see `Undo`); DOES cover 0.17→0.18
    /// (see `Search`, already born inside this fallback's window).
    ///
    /// `TaskKind::Index` (0.25.0, `index.build`) is the same case as `Search`:
    /// a 0.24.x client degrades it here without failing.
    #[serde(other)]
    Unknown,
}

/// State of a Task's lifecycle.
///
/// Wire: a tagged object `{"kind": "...", …}` — the same convention as
/// [`Error`]. N/N-1 tolerance (ADR 0004): an unknown state deserializes to
/// [`TaskState::Unknown`], treated as NOT terminal (conservative: the client
/// keeps listening to `task.progress` until a state it understands arrives).
///
/// ```
/// use norte_proto::TaskState;
/// let s: TaskState = serde_json::from_str(r#"{"kind": "running"}"#).unwrap();
/// assert_eq!(s, TaskState::Running);
/// assert!(!s.is_terminal());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskState {
    /// Queued, no slot in the scheduler yet.
    Pending,
    /// Running.
    Running,
    /// Paused (spec §11: `task.pause`). M0 does not emit it; already reserved
    /// so introducing it later does not break N-1 clients.
    Paused,
    /// Finished successfully.
    Completed,
    /// Cleanly cancelled (clean destination or `.norte-partial`, spec §5).
    Cancelled,
    /// Failed; the error says why (a caught panic arrives as
    /// [`Error::Internal`] with `panic: true`).
    Failed {
        /// Cause of the failure.
        error: Error,
    },
    /// State of a newer protocol (deserialization fallback). The core NEVER
    /// emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl TaskState {
    /// `true` if the Task will not change state anymore.
    /// [`TaskState::Unknown`] counts as non-terminal: faced with a state it
    /// does not understand, the client keeps listening.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed { .. }
        )
    }
}

/// Progress snapshot of a Task (payload of the `task.progress` notification).
///
/// Totals `None` = still unknown (walk in progress), never a fabricated 0.
/// The emitter coalesces; a Task's last snapshot always carries a terminal
/// state and final totals.
///
/// ```
/// use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
/// let p = TaskProgress {
///     task_id: TaskId::new(7),
///     kind: TaskKind::Copy,
///     state: TaskState::Running,
///     bytes_done: 512,
///     bytes_total: Some(4096),
///     entries_done: 0,
///     entries_total: Some(2),
///     current: None,
///     // `None` = this task does not count unreadables; `Some(0)` would be
///     // "it counts them and there were none". See the field.
///     unreadable: None,
///     unvisited: None,
/// };
/// let json = serde_json::to_string(&p).unwrap();
/// assert_eq!(serde_json::from_str::<TaskProgress>(&json).unwrap(), p);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgress {
    /// Task the snapshot belongs to.
    pub task_id: TaskId,
    /// Class of operation (a frontend paints "copying…" with no state of its
    /// own).
    pub kind: TaskKind,
    /// State at the moment of the snapshot.
    pub state: TaskState,
    /// Bytes already processed.
    pub bytes_done: u64,
    /// Estimated total bytes; `None` while the walk has not finished.
    #[serde(default)]
    pub bytes_total: Option<u64>,
    /// Entries (files/dirs) already processed.
    pub entries_done: u64,
    /// Estimated total entries; `None` while the walk has not finished.
    #[serde(default)]
    pub entries_total: Option<u64>,
    /// Entry in progress (to paint "copying X…"); may be missing.
    #[serde(default)]
    pub current: Option<VPath>,
    /// Subtrees or entries the task could NOT read (0.53.0, #251).
    ///
    /// `None` = **whoever emits this does not count it**, and `Some(0)` = it
    /// counts it and there were none. The distinction is not cosmetic and is
    /// the same rule [`crate::methods::Volume::total_bytes`] states for its
    /// case: a zero standing in for unknown reads as an answer, and here the
    /// answer it would fabricate is the dangerous one. A 0.52 daemon does not
    /// emit the field; if this were `u64`, a 0.53 client would read `0` and
    /// paint a "safe" total that is not.
    ///
    /// What makes it necessary is `fs.dir_size`: it counted unreadable
    /// subtrees in a LOCAL counter, emitted a `tracing::info!` and nothing
    /// else, so a tree half of which gave `EACCES` reported `Completed` with
    /// a confident, wrong total — and the method exists to answer "does this
    /// fit at the destination?", where a silently small number is the
    /// dangerous direction.
    ///
    /// With this, a client paints "at least X" instead of "X". It is the twin
    /// of the `confidence` `fs.compare` gives each row, and for the same
    /// reason: a count without it cannot say it is a lower bound.
    ///
    /// Omitted when it is `None`, which is the value for every task that does
    /// not count unreadables — i.e. almost all of them, and a `task.progress`
    /// travels many times per second and per task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<u64>,
    /// Nodes the task did NOT get to visit because the walk hit its cap
    /// (0.62.0, #315).
    ///
    /// Separate from [`Self::unreadable`] and not added to it, even though
    /// both mean "this did not happen": an unreadable is a permission or a
    /// file that moved — things the reader can fix — and this is norte saying
    /// the tree is bigger than it is going to walk in one go. Mixing them
    /// made a recursive `set_mode` over a huge tree say "40,000 could not be
    /// changed (a link, or not yours)", which is not what happened, and
    /// `unreadable` has carried its own contract since 0.53: a counter that
    /// meant two things depending on the task would be unreadable to anyone.
    ///
    /// Omitted when it is `None`, which is the value for every task that does
    /// not walk trees with a cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unvisited: Option<u64>,
}
