//! [`Backend`]: the SAME surface for the embedded core and the daemon (phase
//! 3 M2). Rule 7 is what makes this possible: frontends only change
//! transport, never logic.
//!
//! - Embedded: passthrough to [`Engine`] (the default for instant startup).
//! - Remote (unix only, ADR 0011): JSON-RPC against the daemon, with a
//!   notification pump (`task.progress` → a `watch` per task), FOREIGN tasks
//!   (queued by OTHER frontends) delivered over a channel, resync via
//!   `task.list`, and reconnection with a warning.

use std::sync::Arc;

use norte_proto::{Error, TaskId, TaskProgress, TaskState};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::Engine;
use crate::scheduler::TaskHandle;

mod archive;
mod connect;
mod events;
mod fs;
mod host;
mod index;
mod journal;
mod plugins;
mod rename;
mod session;
mod sync;

/// Projects an `IndexHit` from the core (norte-index) to the protocol type (M4).
fn index_hit_to_proto(h: norte_index::IndexHit) -> norte_proto::methods::IndexHit {
    norte_proto::methods::IndexHit {
        path: h.path,
        kind: h.kind,
        size: h.size,
        mtime_ms: h.mtime_ms,
    }
}

/// Projects a [`crate::volumes::VolumeKind`] to the protocol type (0.37.0,
/// #131). The two types do NOT share a definition (`norte-proto` cannot
/// depend on `norte-core`, the dependency runs the other way): an exhaustive
/// `match` here is what keeps the mapping honest — a new kind in the core
/// breaks this function's compilation instead of silently degrading.
///
/// `pub(crate)`: the `host.volumes` handler in `daemon::server` reuses this
/// same function (with [`volume_to_proto`]) instead of duplicating the
/// `match` — unlike `index_hit_to_proto`, whose mapping is so trivial (four
/// fields with no branches) that duplicating it in the daemon risks nothing;
/// here there IS a `match` over variants, and two copies are two places to
/// forget when one is added.
pub(crate) fn volume_kind_to_proto(
    k: crate::volumes::VolumeKind,
) -> norte_proto::methods::VolumeKind {
    match k {
        crate::volumes::VolumeKind::Fixed => norte_proto::methods::VolumeKind::Fixed,
        crate::volumes::VolumeKind::Removable => norte_proto::methods::VolumeKind::Removable,
        crate::volumes::VolumeKind::Network => norte_proto::methods::VolumeKind::Network,
        crate::volumes::VolumeKind::Pseudo => norte_proto::methods::VolumeKind::Pseudo,
        crate::volumes::VolumeKind::Unknown => norte_proto::methods::VolumeKind::Unknown,
    }
}

/// Projects a [`crate::volumes::Volume`] to the protocol type (0.37.0, #131).
/// `pub(crate)`: see the rustdoc of [`volume_kind_to_proto`].
pub(crate) fn volume_to_proto(v: crate::volumes::Volume) -> norte_proto::methods::Volume {
    norte_proto::methods::Volume {
        mount: v.mount,
        label: v.label,
        fs_type: v.fs_type,
        kind: volume_kind_to_proto(v.kind),
        total_bytes: v.total_bytes,
        free_bytes: v.free_bytes,
        read_only: v.read_only,
    }
}

/// Timeout for AI calls: the provider (remote model) legitimately takes much
/// longer than an fs.*. Bounds BOTH arms of [`Backend::ai_rename_plan`]
/// (embedded and remote — cancel-on-drop on the remote one too).
const AI_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

/// The stream type returned by [`Backend::list_stream`] (ADR 0017),
/// re-exported so frontends can name it without depending on `norte-vfs`.
pub use norte_vfs::EntryStream;

/// A task in flight, whether from the embedded scheduler or the daemon.
///
/// NOT `Clone` on purpose: [`Self::join`] consumes the handle, and two owners
/// of the wait are two places that believe they will see the outcome. What
/// CAN be shared out is OBSERVING it — see [`Self::observer`] (#173).
pub struct TaskRef {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: TaskCanceller,
    pauser: TaskPauser,
}

impl TaskRef {
    /// ONLY for frontend tests (#85): a SYNTHETIC `TaskRef` backed by a
    /// `watch` from the test itself — lets a frontend's async/stateful logic
    /// (terminal synthesis on connection death, canceller deregistration,
    /// read-after-write) be tested without an engine or a daemon. The
    /// canceller is a loose token (cancel = a no-op observable via
    /// `token.is_cancelled()` if the test keeps its clone). Not a stable
    /// API: `doc(hidden)`, can change without a bump.
    #[doc(hidden)]
    #[must_use]
    pub fn synthetic_for_tests(id: TaskId, rx: watch::Receiver<TaskProgress>) -> Self {
        Self {
            id,
            rx,
            canceller: TaskCanceller::Embedded(CancellationToken::new()),
            pauser: TaskPauser::Embedded(crate::scheduler::PauseGate::default()),
        }
    }

    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Live snapshots (same contract as the scheduler's `watch`: the terminal
    /// state is always published except on lost connection).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Clonable handle to cancel from another task (CLI's Ctrl-C).
    #[must_use]
    pub fn canceller(&self) -> TaskCanceller {
        self.canceller.clone()
    }

    /// A CLONABLE view of this task: id, progress and cancellation, without
    /// the wait (#173).
    ///
    /// Exists for a frontend's task board, which needs to PAINT progress and
    /// REQUEST cancellation, not own the task. It used to have to keep the
    /// whole [`TaskRef`], so whoever launched it was left without it — and
    /// that is why a sync APPLYING, which can only be stopped from its own
    /// panel, did not show up on the board: the program's most destructive
    /// operation was the only invisible one.
    ///
    /// Two places being able to cancel breaks nothing: [`TaskCanceller`] was
    /// already clonable, and a cancellation is idempotent and cooperative.
    /// What still has a single owner is the WAIT.
    #[must_use]
    pub fn observer(&self) -> TaskObserver {
        TaskObserver {
            id: self.id,
            rx: self.rx.clone(),
            canceller: self.canceller.clone(),
            pauser: self.pauser.clone(),
        }
    }

    /// Cooperative cancellation request.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    /// Waits for the terminal state. If the connection to the daemon dies
    /// with no known outcome, returns `Failed{ProviderUnavailable}` — the
    /// most honest thing that can be said from the outside.
    pub async fn join(mut self) -> TaskState {
        loop {
            let state = self.rx.borrow().state.clone();
            if state.is_terminal() {
                return state;
            }
            if self.rx.changed().await.is_err() {
                let last = self.rx.borrow().state.clone();
                if last.is_terminal() {
                    return last;
                }
                return TaskState::Failed {
                    error: Error::ProviderUnavailable { retryable: true },
                };
            }
        }
    }

    fn from_handle(handle: &TaskHandle) -> Self {
        Self {
            id: handle.id(),
            rx: handle.progress(),
            canceller: TaskCanceller::Embedded(handle.cancel_token()),
            pauser: TaskPauser::Embedded(handle.pause_gate()),
        }
    }
}

/// Clonable view of a live task: what is needed to PAINT it and STOP it,
/// without owning it ([`TaskRef::observer`], #173).
///
/// ```
/// use norte_core::backend::TaskRef;
/// use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
///
/// let (_tx, rx) = tokio::sync::watch::channel(TaskProgress {
///     task_id: TaskId::new(7),
///     kind: TaskKind::Sync,
///     state: TaskState::Running,
///     bytes_done: 0,
///     bytes_total: None,
///     entries_done: 0,
///     entries_total: None,
///     unreadable: None,
///     unvisited: None,
///     current: None,
/// });
/// let task = TaskRef::synthetic_for_tests(TaskId::new(7), rx);
/// let observer = task.observer();
/// // Two observers of the SAME task, and the task is still owned by whoever launched it.
/// assert_eq!(observer.clone().id(), task.id());
/// assert!(!observer.progress().borrow().state.is_terminal());
/// ```
#[derive(Clone)]
pub struct TaskObserver {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: TaskCanceller,
    pauser: TaskPauser,
}

impl TaskObserver {
    /// Id of the observed task.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Live snapshots (the same `watch` as [`TaskRef::progress`]).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Requests cooperative cancellation. Idempotent, and a no-op on an
    /// already-terminated task.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    /// Pauses (`true`) or resumes (`false`) the task (ADR 0147). Cooperative:
    /// the `Paused` state arrives through the progress channel when it
    /// actually stops.
    ///
    /// # Errors
    /// `Unsupported` if the task belongs to a daemon that does not know how
    /// to pause.
    pub async fn set_paused(&self, paused: bool) -> Result<(), Error> {
        self.pauser.set_paused(paused).await
    }

    /// Moves the task up or down the serial queue (ADR 0149), if it has not
    /// started yet.
    ///
    /// # Errors
    /// `Unsupported` with the EMBEDDED scheduler: the queue is reordered by
    /// the daemon, which is the one holding it.
    pub async fn mover_en_cola(&self, up: bool) -> Result<(), Error> {
        self.pauser.mover_en_cola(up).await
    }

    /// A clonable pause handle, to request it from another task without
    /// carrying the whole observer along.
    #[must_use]
    pub fn pauser(&self) -> TaskPauser {
        self.pauser.clone()
    }
}

/// Clonable pause for a task (ADR 0147), from the embedded scheduler or the
/// daemon. Separate from [`TaskCanceller`] so as not to change its shape,
/// which other surfaces (MCP) that do not pause also use.
#[derive(Clone)]
pub enum TaskPauser {
    /// The embedded scheduler's gate.
    Embedded(crate::scheduler::PauseGate),
    /// `task.pause`/`task.resume` against the daemon.
    Remote(norte_client::RemoteTaskCanceller),
}

impl TaskPauser {
    /// Pauses (`true`) or resumes (`false`).
    ///
    /// # Errors
    /// `Unsupported` against a daemon that does not know how to pause.
    pub async fn set_paused(&self, paused: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(g) => {
                if paused {
                    g.pause();
                } else {
                    g.resume();
                }
                Ok(())
            }
            Self::Remote(c) => c.set_paused(paused).await,
        }
    }

    /// Moves up or down the serial queue (ADR 0149).
    ///
    /// # Errors
    /// `Unsupported` with the embedded scheduler.
    pub async fn mover_en_cola(&self, up: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            Self::Remote(c) => c.mover_en_cola(up).await,
        }
    }
}

/// Clonable cancellation for a [`TaskRef`].
#[derive(Clone)]
pub enum TaskCanceller {
    /// Token from the embedded scheduler.
    Embedded(CancellationToken),
    /// `task.cancel` against the daemon (fire-and-forget: the real
    /// confirmation arrives via `task.progress`, per the method's contract).
    /// The handle is built by the SDK ([`norte_client::RemoteTaskCanceller`],
    /// ADR 0066).
    Remote(norte_client::RemoteTaskCanceller),
}

impl From<norte_client::RemoteTask> for TaskRef {
    /// A daemon task seen as a [`TaskRef`]: the core does not distinguish
    /// where it came from, and that is why `Backend` can return the same
    /// thing from the embedded engine and from the SDK (ADR 0066).
    fn from(t: norte_client::RemoteTask) -> Self {
        let id = t.id();
        let rx = t.progress();
        let canceller = TaskCanceller::Remote(t.canceller());
        let pauser = TaskPauser::Remote(t.canceller());
        Self {
            id,
            rx,
            canceller,
            pauser,
        }
    }
}

impl From<crate::engine::TransferOptions> for norte_client::TransferOptions {
    /// The ENGINE's options, as a remote client puts them in the params.
    /// Field by field and deliberately without `..Default::default()`: a new
    /// field on either struct must break here and not get lost along the way.
    fn from(o: crate::engine::TransferOptions) -> Self {
        let crate::engine::TransferOptions {
            on_collision,
            symlinks,
            resume,
            verify,
            queued,
        } = o;
        Self {
            on_collision,
            symlinks,
            resume,
            verify,
            queued,
        }
    }
}

impl TaskCanceller {
    /// Fires the cooperative cancellation.
    pub fn cancel(&self) {
        match self {
            Self::Embedded(token) => token.cancel(),
            Self::Remote(canceller) => canceller.cancel(),
        }
    }
}

/// A connection event from the remote backend (for the message bar).
///
/// Defined by the SDK ([`norte_client::ConnEvent`], ADR 0066) and
/// re-exported here: it is produced by reconnection, which lives there, and
/// consumed by a frontend, which names it `norte_core::backend`.
pub use norte_client::ConnEvent;

/// Connection-warning observer (#44) that forwards them over a channel: the
/// route for `Backend::Embedded` so that an IN-PROCESS CLI/TUI can surface
/// degradation just like daemon mode does (where the observer broadcasts
/// over the wire). Maps the core's `ConnectionWarning` to the wire's
/// `ConnectionDegraded`.
struct ChannelConnectionObserver {
    tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionDegraded>,
    /// The observer that was already in the slot, if there was one.
    ///
    /// The engine's slot holds ONE, and the two channels are taken
    /// separately, so the second one to install itself has to keep calling
    /// the first. Without this, `take_failed` after `take_degraded` left the
    /// degradation channel mute — and silently mute, which is the worst kind.
    previo: Option<Arc<dyn crate::connect::ConnectionObserver>>,
}

impl crate::connect::ConnectionObserver for ChannelConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let _ = self.tx.send(norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        });
        if let Some(p) = &self.previo {
            p.on_connection_warning(w);
        }
    }

    fn on_connection_failure(&self, f: &crate::connect::ConnectionFailure) {
        if let Some(p) = &self.previo {
            p.on_connection_failure(f);
        }
    }
}

/// Twin of the one above for failures (#322): a connection that did NOT
/// open.
///
/// Two observers and not one with two channels because the two `take_*` are
/// independent: a frontend may want the security warning and not the
/// diagnostic, or the other way around, and forcing both at once would turn
/// one into the other's condition.
struct ChannelFailureObserver {
    tx: mpsc::UnboundedSender<norte_proto::methods::ConnectionFailed>,
    previo: Option<Arc<dyn crate::connect::ConnectionObserver>>,
}

impl crate::connect::ConnectionObserver for ChannelFailureObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        if let Some(p) = &self.previo {
            p.on_connection_warning(w);
        }
    }

    fn on_connection_failure(&self, f: &crate::connect::ConnectionFailure) {
        let _ = self.tx.send(norte_proto::methods::ConnectionFailed {
            conn: f.conn.clone(),
            scheme: f.scheme.clone(),
            host: f.host.clone(),
            reason: f.reason.wire().to_owned(),
            detail: f.detail.clone(),
        });
        if let Some(p) = &self.previo {
            p.on_connection_failure(f);
        }
    }
}

/// Where the hook notices go in `Backend::Embedded` (ADR 0100): a channel the
/// frontend drains, just like daemon mode broadcasts them over the wire.
/// Without this the embedded arm would run the hooks and swallow their
/// messages.
struct ChannelHookSink {
    tx: mpsc::UnboundedSender<norte_proto::methods::PluginNotice>,
}

impl crate::hooks::HookNoticeSink for ChannelHookSink {
    fn notice(&self, n: norte_proto::methods::PluginNotice) {
        let _ = self.tx.send(n);
    }

    /// The frontend dropped the receiver: the dispatcher ends with it.
    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Sink for lazy-journal notices (#177) that forwards them over a channel:
/// the route for an embedded TUI to paint, IN THE SESSION, that its
/// mutations are not being recorded.
///
/// Twin of [`ChannelConnectionObserver`] and for the same reason: the notice
/// is born inside the core, mid-mutation, and the frontend has no way to ask
/// anyone about it afterwards.
struct ChannelJournalSink {
    tx: mpsc::UnboundedSender<crate::embedded::JournalStatus>,
}

impl crate::embedded::JournalWarningSink for ChannelJournalSink {
    fn on_no_journal(&self, why: &crate::embedded::NoJournal) {
        let _ = self
            .tx
            .send(crate::embedded::JournalStatus::Lost(why.clone()));
    }

    fn on_journal_recovered(&self) {
        let _ = self.tx.send(crate::embedded::JournalStatus::Recovered);
    }

    fn on_journal_squatted(&self) {
        let _ = self.tx.send(crate::embedded::JournalStatus::Squatted);
    }
}

/// The EMBEDDED arm's "connection", for whatever carries it by key: today
/// only the sync-plan spool, which ties each approved plan to the connection
/// that produced it (ADR 0049).
///
/// In-process there is exactly ONE, and that is why it is a constant and not
/// a counter: planning and applying have to match, and two different ids
/// would make a `sync_apply` from this same `Backend` answer `PlanStale` to
/// its own plan.
///
/// `u64::MAX` and deliberately not `0`: the daemon's `conn_id`s come from a
/// counter that starts at zero, so this value cannot coincide with any of
/// them in a process that has both at once — the spool would not end up
/// confusing a socket client's plan with an embedded `Backend`'s. (And the
/// per-connection sweep compares the `"<id>-"` prefix of the file name,
/// which does not collide either.)
///
/// **Corollary, for whoever exposes these methods:** `plan_hash` is NOT a
/// secret —it is a deterministic digest of the roots, the options and the
/// steps, computable by anyone who can read the two trees—, so the only
/// thing tying a plan to whoever requested it is this `conn_id`, and here it
/// is a constant. Everything that reaches this arm shares the same
/// connection and acts as `Actor::User`, i.e. with no policy gate.
/// `sync_plan`/`sync_apply` must not be wired to a scripting environment or
/// to the plugin host without their own actor: it would be a whole-tree
/// write with no gate.
const EMBEDDED_CONN_ID: u64 = u64::MAX;

/// The REMOTE backend has lived in the SDK since ADR 0066 and is re-exported
/// here so that its usual consumers (MCP, e2e tests) keep naming it where
/// they always named it.
pub mod remote {
    pub use norte_client::remote::*;
}

/// The core behind a single surface (rule 7).
pub enum Backend {
    /// In-process core: instant startup, no daemon.
    Embedded(Arc<Engine>),
    /// Against the UDS daemon (ADR 0011).
    Remote(norte_client::RemoteBackend),
}

impl Clone for Backend {
    /// CHEAP clone: shares the engine/connection (internal Arc in both
    /// variants). NOTE: the one-shot channels (`take_foreign_tasks`,
    /// `take_conn_events`, `take_approvals`, `take_degraded`,
    /// `take_journal_warnings`) belong to the FIRST owner — a clone (e.g.
    /// for Lua scripting, tasks 4-5) must not call them.
    fn clone(&self) -> Self {
        match self {
            Self::Embedded(e) => Self::Embedded(Arc::clone(e)),
            Self::Remote(r) => Self::Remote(r.clone()),
        }
    }
}

impl Backend {
    /// Does this backend record its mutations in a journal, and can they
    /// therefore be undone?
    ///
    /// Today it is exactly "it talks to the daemon", and it answers for what
    /// this type can PROMISE, not for what a given process happens to have
    /// set up. The embedded arm has carried the state directory's journal
    /// since #167 (`norte_core::embedded`) but opens it LAZILY and over an
    /// exclusive lock another process may hold; and the spool that
    /// [`Self::sync_plan`] needs is not installed by this type but by
    /// whoever builds the engine —`norte-cli` does it, and only for `norte
    /// sync`—, so neither the journal nor the spool are guaranteed by
    /// construction. A `true` here would be a promise this value cannot
    /// keep.
    ///
    /// So what it says is: **"there is a daemon behind it"**, which is the
    /// only configuration where both things are guaranteed in advance. The
    /// LIVE question —"does THIS session actually get recorded?", the one
    /// that has to be answered to a human before they say yes— is
    /// [`Self::ensure_journal`], which opens the journal to answer; this one
    /// opens nothing.
    ///
    /// It is a question about the TRANSPORT and not about the engine because
    /// there is no way to ask the engine from the outside: `Engine` does not
    /// publish whether it has a journal, and a frontend that inferred it from
    /// the first `Unsupported` would have found out after already showing a
    /// plan.
    ///
    /// A frontend uses it to DIM before the reader presses the key
    /// (`norte_frontend::availability::Facts::journalled`), not to skip any
    /// check: the core still decides.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// assert!(!Backend::Embedded(Arc::new(Engine::new())).is_journalled());
    /// ```
    #[must_use]
    pub fn is_journalled(&self) -> bool {
        match self {
            Self::Embedded(_) => false,
            Self::Remote(_) => true,
        }
    }

    /// Is there a daemon on the other end, or is the core in THIS process?
    ///
    /// The bare transport question, which differs from [`Self::is_journalled`]:
    /// that one says what can be PROMISED (journal and spool) and this one
    /// says whether a second process exists to talk about.
    ///
    /// Exists because there are surfaces that are not "can or can't" but
    /// "is there another place or isn't there", and the first one is the log
    /// panel (#328). With the embedded core there is a single ring —this
    /// process's, which is the one the panel already reads—, so asking the
    /// backend for the daemon's log rightly answers [`Error::Unsupported`],
    /// and a frontend that treated that answer as a fact about a daemon
    /// would end up saying "this daemon does not serve its log" where there
    /// is none. The right answer there is not another sentence: it is **not
    /// asking**, and not mentioning anyone.
    ///
    /// Decided ONCE and it does not change: the `Backend` does not switch
    /// arms during the process's lifetime.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// assert!(!Backend::Embedded(Arc::new(Engine::new())).is_remote());
    /// ```
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        match self {
            Self::Embedded(_) => false,
            Self::Remote(_) => true,
        }
    }

    /// Opens the journal now (if needed) and says whether THIS session ends
    /// up recorded — the question a frontend asks right before mutating and
    /// needs to ANSWER to the human before the yes, not after (`norte ai
    /// rename`, and as of this task `norte sync`).
    ///
    /// - Embedded: delegates to [`Engine::ensure_journal`], which takes the
    ///   lazy lock HERE (not on the first mutation) and keeps it until it is
    ///   released for being idle ([`Self::release_journal_if_idle`], which
    ///   the TUI calls on its tick — #179).
    /// - Remote: always `true`. The daemon OWNS the journal and refuses to
    ///   start without one (see `norte daemon run`'s startup); there is no
    ///   round trip to make to find out, and a remote connection with no
    ///   journal is not a state this process can observe or remedy — only
    ///   the daemon's operator can.
    ///
    /// Unlike [`Self::is_journalled`] (which on the embedded arm says
    /// `false` on purpose, see its rustdoc), this one DOES open the journal
    /// when it can: it is the call of someone about to mutate, not of
    /// someone who just wants to dim a key.
    ///
    /// A freshly built engine has nowhere to get it from, and then the
    /// honest answer is `false` — not an error:
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(!rt.block_on(backend.ensure_journal()));
    /// ```
    pub async fn ensure_journal(&self) -> bool {
        match self {
            Self::Embedded(engine) => engine.ensure_journal().await,
            Self::Remote(_) => true,
        }
    }

    /// Releases the journal if it has gone unused for `idle` (#179).
    ///
    /// An `ntc` that copied a file at 09:00 would keep `journal.db` open
    /// until it quit, so `norte daemon run` and `norte audit` could not open
    /// it all day. The window reopens by itself on the next mutation, and the
    /// reopen RE-READS the chain — which is what makes releasing it safe.
    ///
    /// Remote: `true` without doing anything. The journal belongs to the
    /// DAEMON, which refuses to start without one; releasing it from here is
    /// not useless, it is that it is not this process's to release.
    ///
    /// # This is NOT cancel-safe. In the BODY of a `select!` branch, never in
    /// its condition (see
    /// [`LazyJournal::release`](crate::embedded::LazyJournal::release)).
    pub async fn release_journal_if_idle(&self, idle: std::time::Duration) -> bool {
        match self {
            Self::Embedded(engine) => engine.release_journal_if_idle(idle).await,
            Self::Remote(_) => true,
        }
    }

    /// The same, saying WHY not — see [`Engine::journal_obstacle`].
    ///
    /// `None` = this session records, or there is no window to lose (the
    /// daemon on the other end of a socket, or an `Engine::new()`).
    ///
    /// Exists because of what #178 split in two: with `Busy` the operation
    /// would happen without being recorded —and the remedy is `--daemon`,
    /// talking to whoever holds the file— and with `Failed` it will not
    /// happen at all, and there `--daemon` is no remedy at all because the
    /// daemon refuses to start with that same file. A `bool` sends half the
    /// users into the wrong wall.
    pub async fn journal_obstacle(&self) -> Option<crate::embedded::NoJournal> {
        match self {
            Self::Embedded(engine) => engine.journal_obstacle().await,
            Self::Remote(_) => None,
        }
    }

    /// Releases the sync plans THIS backend is holding, the way the daemon
    /// does when a connection drops on it.
    ///
    /// The right to apply a plan lives in an IN-MEMORY registry that dies
    /// with the process, so what this takes away is not an applicable plan
    /// but its file: a listing with the relative paths of the two trees,
    /// readable by anyone who can read the state directory. The daemon
    /// releases it in two places —a sweep on startup and
    /// `Spool::drop_connection` when each connection closes— and an embedded
    /// process has neither of the two: **it is the caller's job to call this
    /// on the way out**, through every path, including the one that applied
    /// nothing.
    ///
    /// It does not sweep the whole directory, and that is not an oversight:
    /// an embedded process shares the state directory with a daemon that may
    /// be alive, and it does not hold the journal lock with which to prove
    /// that it is not. It takes only its own and nothing more.
    ///
    /// With no spool installed (everyone except `norte sync`) and against
    /// the daemon it is a no-op: there the spool's owner is the daemon, and
    /// tearing down the connection is already its job.
    ///
    /// Returns nothing and does not fail: a file that refuses to be deleted
    /// ends up in `tracing::warn!` and in the command's exit code, which
    /// belongs to the sync and not to the cleanup (same criterion as the
    /// daemon's startup sweep, see [`crate::sync::Spool::sweep`]).
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// // With no spool installed there is nothing to release, and saying so costs nothing.
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// rt.block_on(backend.drop_retained_plans());
    /// ```
    pub async fn drop_retained_plans(&self) {
        match self {
            Self::Embedded(engine) => {
                let Some(spool) = engine.spool() else { return };
                match spool.drop_connection(EMBEDDED_CONN_ID).await {
                    Ok(report) if report.is_clean() => {}
                    Ok(report) => {
                        tracing::warn!(failed = report.failed, "sync spools were left undeleted");
                    }
                    Err(e) => tracing::warn!(error = %e, "could not release the spool"),
                }
            }
            Self::Remote(_) => {}
        }
    }
}

/// The remote backend (unix only, like the daemon — ADR 0011).
#[cfg(unix)]
#[cfg(test)]
mod observer_tests {
    use super::*;

    fn progress_channel(id: u64) -> (watch::Sender<TaskProgress>, watch::Receiver<TaskProgress>) {
        let (tx, rx) = watch::channel(TaskProgress {
            task_id: TaskId::new(id),
            kind: norte_proto::TaskKind::Sync,
            state: TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        // The sender is returned so the test keeps it alive: a `watch` with
        // no sender is not what this test observes.
        (tx, rx)
    }

    /// #173: the observer cancels the SAME task, not an inert copy. This is
    /// the property that lets a board stop a sync that is being applied
    /// without taking the handle away from its panel.
    #[test]
    fn the_observer_cancels_the_same_task() {
        let (_tx, rx) = progress_channel(7);
        let task = TaskRef::synthetic_for_tests(TaskId::new(7), rx);
        let TaskCanceller::Embedded(token) = task.canceller() else {
            panic!("a synthetic TaskRef cancels with an embedded token");
        };
        assert!(!token.is_cancelled());
        let observer = task.observer();
        // And cloned: the board clones its row when reordering it.
        observer.clone().cancel();
        assert!(
            token.is_cancelled(),
            "the cancellation reaches the real task"
        );
        assert_eq!(observer.id(), task.id());
    }

    /// And observing does not consume: whoever launched the task keeps all
    /// of it — including the WAIT, which is the only thing that still has a
    /// single owner.
    #[tokio::test]
    async fn observing_does_not_take_the_task_away_from_whoever_launched_it() {
        let (tx, rx) = watch::channel(TaskProgress {
            task_id: TaskId::new(9),
            kind: norte_proto::TaskKind::Sync,
            state: TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        });
        let task = TaskRef::synthetic_for_tests(TaskId::new(9), rx);
        let observer = task.observer();
        tx.send_modify(|p| p.state = TaskState::Completed);
        assert_eq!(
            observer.progress().borrow().state,
            TaskState::Completed,
            "the observer sees the same channel"
        );
        assert!(matches!(task.join().await, TaskState::Completed));
    }
}
