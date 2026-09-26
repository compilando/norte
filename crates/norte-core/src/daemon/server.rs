//! Daemon server (ADR 0011): UDS + `SO_PEERCRED`, dispatch of
//! `initialize`/`fs.*`/`task.*`/`daemon.shutdown`, broadcast of
//! `task.progress` and shutdown on idleness or request.

use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures::StreamExt;
use norte_proto::wire::{
    FrameDecoder, MessageKind, Notification, Request, RequestId, Response, RpcError, classify,
    codes, encode_frame,
};
use norte_proto::{TaskId, methods};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::DaemonError;
use super::approvals::DaemonApprovalResolver;
use super::transport;
use crate::Engine;
use crate::engine::TransferOptions;
use crate::journal::Actor;
use crate::policy::{OpSet, Scope, ScopeRegistry};
use crate::scheduler::TaskHandle;

/// Minimum interval between progress notifications of ONE task (≤30 Hz).
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(33);
/// How often the server evaluates the idleness condition.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// How long shutdown waits for connections to finish writing what they
/// already had queued (the response to `daemon.shutdown` itself, the
/// `daemon.going_away`). A client that does not read no longer holds up
/// shutdown.
const CONNECTION_DRAIN: Duration = Duration::from_secs(2);
/// Frames pending to be written per connection. A client that does not drain
/// its side of the socket hits this and gets CUT OFF: never unbounded memory
/// for a slow or hostile peer (security-reviewer finding M1).
const OUTBOX_FRAMES: usize = 1024;
/// Consecutive parse errors tolerated before cutting the connection (a peer
/// that only emits garbage does not deserve infinite responses).
const MAX_PARSE_ERRORS: u32 = 16;
/// Simultaneous connections per daemon (same uid; anti-exhaustion).
const MAX_CONNECTIONS: usize = 256;
/// Simultaneous live tasks queued via the daemon.
const MAX_LIVE_TASKS: usize = 512;
/// Sub-cap of live AGENT tasks — all sessions together (#70): a greedy agent
/// does not exhaust the global quota; the human ALWAYS keeps headroom
/// (`MAX_LIVE_TASKS - MAX_LIVE_TASKS_AGENTS`). By CLASS, not by session:
/// isolating agent-from-agent is m4 debt (policy.rs).
const MAX_LIVE_TASKS_AGENTS: usize = 384;
/// Sync plans RETAINED at once by ONE connection (ADR 0049).
///
/// Not a concurrency cap — [`MAX_LIVE_TASKS`] already handles that — but an
/// accumulation one: an approved plan outlives its Task for
/// `SYNC_PLAN_TTL_MS`, and is a file with the relative listing of two whole
/// trees. Without a cap, planning in a loop while varying `include` (each
/// selection gives a different digest, i.e. a different file) fills up the
/// state directory, the same one `journal.db` lives in; running out of disk
/// there is not a nuisance, it is rule 4 ceasing to be satisfiable.
///
/// Generous on purpose: a frontend has one plan per pane and at most one
/// previous comparison it is still looking at.
const MAX_RETAINED_SYNC_PLANS: usize = 16;
/// Parsed frames QUEUED between the reader and a connection's dispatch (#64).
/// Small on purpose: dispatch is serial and clients are request/response —
/// the inbox's value is keeping the READER alive during a suspended dispatch
/// (Ask) so it can observe the peer's death.
const INBOX_FRAMES: usize = 16;
/// Cap of DEFERRED frames during an in-flight dispatch (#72): frames that
/// arrive while a request is being processed (typically a suspended Ask) and
/// are not its `rpc.cancel` are buffered up to this cap. Once reached, the
/// inner-select's read arm is disabled and the reader goes back to applying
/// backpressure on the socket (as before #72): total memory stays bounded at
/// `INBOX_FRAMES + MAX_DEFERRED_FRAMES` instead of growing without limit
/// while the peer pipelines during its own Ask (security-reviewer MAJOR). A
/// normal request/response client never comes close (0-1 deferred); only a
/// semi-hostile peer under an unapproved Ask reaches it.
const MAX_DEFERRED_FRAMES: usize = 16;
/// TERMINAL snapshots retained for `task.list` resync (a frontend that
/// reconnects sees the outcome of what it missed).
const RECENT_TERMINAL: usize = 64;
/// Sub-cap of `recent` slots for AGENT terminals (#70): a burst of trivial
/// agent tasks evicts its own oldest entries first, never the human's
/// outcomes (which keeps ≥ `RECENT_TERMINAL - RECENT_TERMINAL_AGENTS`
/// slots).
const RECENT_TERMINAL_AGENTS: usize = 32;
/// LIVE paginated listings retained per connection (ADR 0017): each one is a
/// lazy, undrained `EntryStream`. Opening the (N+1)th evicts the oldest
/// (LRU). A TUI browses 1-2 at a time; the cap bounds the cost (including a
/// parked blocking thread from the local producer per undrained listing).
const MAX_OPEN_LISTINGS: usize = 8;
/// GLOBAL cap of listings retained across the whole daemon (rust-reviewer
/// M1): 256 connections × 8 = 2048 `vfs-local` producers could end up
/// parked in `blocking_send` > the default blocking pool (512). With this
/// cap (well below the pool), exceeding it makes a NEW listing drain
/// ENTIRELY inline (frees the thread instantly) instead of being retained:
/// under pressure from the SAME uid, pagination degrades to full-listing,
/// never to exhausting the pool. The cost is one slow call, never
/// corruption or truncation.
///
/// Tuning INVARIANT (#53 M1): this cap must stay ≤ half the runtime's
/// blocking pool (tokio default: 512) — each listing retained from
/// vfs-local can park ONE pool thread in `blocking_send`, and the rest of
/// the daemon (sqlx, plugins, local fs) needs its own margin. If this is
/// ever raised, or the binary sets `max_blocking_threads`, review both
/// together.
const GLOBAL_MAX_LISTINGS: usize = 256;
/// PENDING scope requests retained across the WHOLE daemon (M3-3b): GLOBAL
/// cap. Below it, each connection has its own sub-cap
/// [`MAX_PENDING_SCOPE_PER_CONN`] so that a single session cannot exhaust
/// the others' channel (same pattern as the listings: per-connection +
/// global).
const MAX_PENDING_SCOPE: usize = 256;
/// Pending scope requests per CONNECTION: bounds what a single session can
/// hold of the global cap. Cleared when the connection dies.
const MAX_PENDING_SCOPE_PER_CONN: usize = 16;
/// Cap on the TTL of a granted scope (24h): a request with a huge `ttl_ms`
/// does not grant near-perpetual access by accident.
const MAX_SCOPE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// How often the server sweeps a LIVE-but-silent connection's expired
/// retained listings (in addition to the lazy sweep on every `fs.list`).
const LISTING_SWEEP: Duration = Duration::from_secs(30);
/// Log lines `log.tail` delivers AT MOST in one round (#328, ADR 0092).
///
/// A thousand, i.e. half the default ring
/// ([`norte_config::logring::RING_DEFAULT`]). The trimming is the server's —
/// the client's `max` is a request, same as in `fs.list` — and nothing is
/// lost: what does not fit is still there after `next`, and the next round
/// picks it up.
///
/// Half and not the whole ring because the normal case is an open pane
/// polling every ~300ms, which brings in units of lines; the number is only
/// ever touched by someone catching up after a while without looking, and
/// that person is fine with two rounds. Whole, the two thousand lines are a
/// megabyte-sized frame — each carries two `String`s and the message runs up
/// to 2 KiB — built on the reactor.
///
/// **Deliberately not published in the protocol** (ADR 0092): the client
/// sizes with `capacity`, which is the upper bound of what can arrive, and
/// iterates over `next` until the response comes back empty. Freezing this
/// number on the wire would promise forever a number chosen before the
/// first real response.
const LOG_TAIL_MAX_LINES: usize = 1000;
/// Cap on the `ai.rename_plan` prompt (security review M4-IA): INPUT tokens
/// are the provider's cost; the 16 MiB frame is not a limit.
const MAX_AI_INSTRUCTION_BYTES: usize = 4 * 1024;
/// Cap on the `index.search_semantic` query (same belt as the
/// `ai.rename_plan` instruction).
const MAX_AI_QUERY_BYTES: usize = MAX_AI_INSTRUCTION_BYTES;

/// Daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Socket path; `None` = [`super::default_socket_path`].
    pub socket_path: Option<PathBuf>,
    /// Shutdown after this much time with NO clients AND no tasks. `None` =
    /// never.
    pub idle_timeout: Option<Duration>,
    /// TTL of a paginated listing retained without a `next` (ADR 0017): past
    /// this time it is discarded even if the connection is still alive.
    /// Configurable to test expiry without long waits.
    pub listing_ttl: Duration,
    /// **The config root of THIS daemon**, not only the plugins' one.
    /// `None` = the real [`crate::connect::config_dir`] (the user's layer);
    /// `Some(dir)` = that directory — for tests, ALWAYS a tempdir, never the
    /// real `~/.config`.
    ///
    /// The plugin catalogue hangs off it (`<dir>/plugins/<id>/plugin.toml`
    /// and `<dir>/plugins-state.toml`, M4-P3), and so does the
    /// `connections.toml` that serves `connection.list` (#365).
    ///
    /// **The name fell short and is historical**: it was born with the
    /// plugins and today governs more than that. It stays as is instead of
    /// being renamed because the rename touches some twenty literals across
    /// four crates' tests, and a short, documented name misleads less than
    /// an exact one bought with a diff nobody is going to read in full.
    ///
    /// `connections.toml` hanging off this is what makes the daemon suite
    /// HERMETIC (#365). Before, `connection.list` read whoever ran the tests'
    /// `~/.config/norte`: it was enough for that machine to have a connection
    /// norte could not read for the tests to turn red, and the test's color
    /// stopped being about the code. It's the same class of problem as a
    /// test that takes a lock in the real `HOME`.
    pub plugins_dir: Option<PathBuf>,
    /// Where the UI session lives (L2): `<state_dir>/session.json` and its
    /// lock. `None` = **nothing is persisted** — neither the lock is taken
    /// nor the writer started — which is what a test wants and what must
    /// never happen to the real `state_dir` by accident. The binary passes
    /// [`norte_config::dirs::state_dir`] explicitly.
    pub state_dir: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            idle_timeout: Some(Duration::from_mins(5)),
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
            state_dir: None,
        }
    }
}

/// State shared between connections.
struct Shared {
    engine: Arc<Engine>,
    /// Live tasks queued BY the daemon (for `task.cancel` and graceful
    /// shutdown), tagged with the actor that queued them: the
    /// `task.list`/`task.cancel` gate for agent connections decides by owner
    /// (#66).
    tasks: Mutex<HashMap<u64, RegisteredTask>>,
    /// Notification outlets for each ALREADY initialized client, by
    /// connection id (the connection removes ITS OWN on death — without this
    /// the writer task would never finish: the broadcast would keep holding
    /// its sender). Bounded: a subscriber that does not drain loses the
    /// subscription, never accumulates.
    subscribers: Mutex<HashMap<u64, Subscriber>>,
    /// Recent outcomes (terminal snapshots) for `task.list`, with the owning
    /// actor: an agent's resync does not see other actors' terminals either
    /// (#66).
    recent: Mutex<std::collections::VecDeque<(norte_proto::TaskProgress, Actor)>>,
    /// Connection id counter.
    next_conn: AtomicUsize,
    /// Live authenticated connections.
    connections: AtomicUsize,
    /// Global shutdown (graceful: stops accepting first).
    shutdown: CancellationToken,
    /// `true` = the requested shutdown wants to cancel the tasks first.
    hard_shutdown: CancellationToken,
    /// The daemon's own identity — uid (derived from the socket itself) or
    /// user SID: the ONLY admitted peer.
    owner: transport::Owner,
    /// This daemon's config root, from [`DaemonConfig::plugins_dir`]. `None`
    /// = the user's real config. This is where the `connections.toml` that
    /// serves `connection.list` comes from (#365).
    connections_dir: Option<PathBuf>,
    /// TTL of a retained paginated listing (ADR 0017); from [`DaemonConfig`].
    listing_ttl: Duration,
    /// Paginated listings retained across the WHOLE daemon (M1): global cap
    /// [`GLOBAL_MAX_LISTINGS`] to protect the blocking pool.
    open_listings: Arc<AtomicUsize>,
    /// Registry of scopes granted per agent session (M3-3b). It is the SAME
    /// instance (`Arc`-backed) the engine's `ScopedPolicy` consults in the
    /// gate: granting here opens the border there. Empty if there is no
    /// policy.
    scopes: ScopeRegistry,
    /// Scope requests pending human grant, by `request_id`. Capped at
    /// [`MAX_PENDING_SCOPE`].
    pending_scope: Mutex<HashMap<u64, PendingScope>>,
    /// Monotonic scope `request_id` counter.
    next_scope_req: AtomicU64,
    /// `Ask` approval router (M3-3b Task 4): the SAME object (`Arc`) the
    /// engine uses as `ApprovalResolver` — a `policy.decide` here wakes up
    /// the suspended gate there. With [`Daemon::bind`]/
    /// [`Daemon::bind_with_scopes`] it is an orphaned router (the engine does
    /// not know it): `policy.pending` answers empty and `policy.decide` finds
    /// no ids — harmless.
    approvals: Arc<DaemonApprovalResolver>,
    /// Registry of discovered plugins + approved/enabled state (M4-P3).
    /// Discovery is synchronous I/O done ONCE at bind time (inside the
    /// `spawn_blocking`, rule 2); mutations (`plugin.set_approval`/
    /// `set_enabled`) re-read and persist `plugins-state.toml` under the
    /// lock. The std `Mutex` is enough: dispatch is serial and the sections
    /// are short.
    plugins: Mutex<crate::plugins::PluginRegistry>,
    /// Serializes each WRITE of `plugins-state.toml` with the snapshot it
    /// comes from (ADR 0104). The `Mutex` above does not cross an `.await`,
    /// and `persist_state` merges the snapshot onto the file: two governance
    /// changes in flight from two human connections — window and terminal —
    /// could write in the reverse order of their snapshots, and with an
    /// `uninstall` in between that resurrected on disk a consent, anchored
    /// to an already-deleted manifest, that one installed later with the
    /// same id would inherit. Taken BEFORE mutating memory and released
    /// AFTER persisting.
    plugins_state_io: tokio::sync::Mutex<()>,
    /// Connections that OWN at least one live directed feed (`compare.rows`,
    /// `search.hits`, `sync.steps`), with how many (#155). A connection on
    /// this list is NOT evicted from the subscriber map because its outbox
    /// fills up: it loses the FRAME, never the subscription — see
    /// [`Shared::broadcast_where`].
    directed_feeds: Mutex<HashMap<u64, u32>>,
    /// The daemon's UI session (L2): ONE document, with its revision and its
    /// owning connection. `Arc` because the disk flush watches it from
    /// another task. The core stores it and does not read it (ADR 0058).
    ui_session: Arc<crate::ui_session::SessionStore>,
    /// Whether what gets written into that session will REACH disk: there is
    /// a `state_dir`, this core holds the lock, and the file that was there
    /// is not from a newer version.
    ///
    /// This is what `session.get` answers as `owner`, not just "you kept
    /// it": to the client the two things are the same question — "are my
    /// writes being saved?" — and answering yes when there is no writer
    /// promises a screen that is lost entirely and silently.
    ///
    /// **Atomic and not a `bool`, and that is #237.** It used to be computed
    /// ONCE at bind time, so a daemon that started while another core held
    /// the lock answered `owner: false` for the rest of its life — also
    /// hours after the other one had gone and the file had been free ever
    /// since. [`session_writer`] retries it, since it is the one with
    /// somewhere to run, and turns it on from there. `Arc` for the same
    /// reason as `session_flush`: the writer does NOT hold onto `Shared`,
    /// which would keep it alive.
    session_persists: Arc<AtomicBool>,
    /// Wakes up the session writer outside its tick: the last connection to
    /// leave should not leave a second of screen unflushed. `Arc` because the
    /// writer does NOT hold onto `Shared` (which would keep it alive).
    session_flush: Arc<tokio::sync::Notify>,
    /// Shared WASM runtime for running plugin commands (M4-P4). Built ONCE
    /// at bind time (starts an epoch "ticker" thread) and reused across
    /// `plugin.run_command` calls. `Arc` because `PluginRuntime` is
    /// `Send+Sync` but NOT `Clone` (it owns the ticker's `JoinHandle`): the
    /// handler clones the `Arc` and runs the (heavy, synchronous)
    /// instantiation+execution in a `spawn_blocking`, never on the reactor
    /// with a lock held (rule 2).
    plugin_runtime: Arc<norte_plugin_host::PluginRuntime>,
    /// Column instances LIVE across pages (#224). It hangs off here for the
    /// same reason as the runtime: it is process state, and the instance
    /// that served page 1 is the one that already has the project's index
    /// parsed when page 2 arrives. The embedded backend has its own, and it
    /// is the SAME type — one with a pool and another without it would be
    /// the #165/#201/#181 asymmetry all over again.
    column_pool: Arc<crate::plugins::ColumnPool>,
    /// THIS process's log ring, if anyone mounted one (#328, ADR 0092). It is
    /// what `log.tail` and `log.level` serve.
    ///
    /// Not in [`DaemonConfig`] because it is not configuration: it is the
    /// LIVE object the `tracing` layer writes into, and only whoever
    /// installed the subscriber knows about that — the binary, with
    /// [`Daemon::with_log_ring`], between the bind and the `run`. The same
    /// arrangement as `scopes` and `approvals`, which also arrive from
    /// outside the config for being shared objects rather than values.
    ///
    /// Empty is the daemon with NO log to show: the mount was not done, or
    /// failed because there already was a subscriber. Then both methods
    /// answer [`norte_proto::Error::Unsupported`] and never an empty list —
    /// an empty log and an absent log cannot be read the same way, which is
    /// exactly the confusion #326 started to fix.
    log_ring: std::sync::OnceLock<norte_config::logring::LogRing>,
}

/// A scope request registered by an agent, waiting for a human to grant it
/// with `policy.grant_scope`. Stores what was asked verbatim; the session
/// was already validated against the connection's actor when it was
/// registered.
struct PendingScope {
    session: String,
    roots: Vec<norte_proto::VPath>,
    ops: Vec<String>,
    ttl_ms: u64,
}

/// A broadcast outlet: the outbox's sender and the connection's ACTOR
/// (M3-3b Task 4, security MAJOR-1): `policy.*` notifications cross sessions
/// (other sessions' paths and ops) and only go to humans — the same
/// criterion as the `policy.pending` gate. `task.progress` is routed by
/// owner (#66): always to humans, an agent only gets its own tasks'.
struct Subscriber {
    tx: mpsc::Sender<Arc<[u8]>>,
    /// The connection's actor (fixed server-side by ITS `initialize`):
    /// decides which broadcasts it receives — `policy.*` only humans;
    /// another actor's `task.progress` never reaches an agent (#66, the same
    /// leak as `task.list`: `current` carries other actors' paths).
    actor: Actor,
}

/// A live task registered in the daemon: the handle + WHO queued it. The
/// owner governs visibility (`task.list`, progress broadcast) and
/// cancellability (`task.cancel`) against agent connections (#66).
struct RegisteredTask {
    handle: TaskHandle,
    owner: Actor,
}

/// A connection's right not to be evicted while its directed feed is alive
/// (#155). It is counted, not flagged: a connection can have a search and a
/// comparison at the same time, and the first one to finish must not take
/// away the other's right.
struct DirectedFeed {
    shared: Arc<Shared>,
    conn_id: u64,
}

impl Drop for DirectedFeed {
    fn drop(&mut self) {
        let mut feeds = self
            .shared
            .directed_feeds
            .lock()
            .expect("directed feeds lock is sound");
        if let Some(n) = feeds.get_mut(&self.conn_id) {
            *n -= 1;
            if *n == 0 {
                feeds.remove(&self.conn_id);
            }
        }
    }
}

/// Pushes a terminal onto the `recent` ring while respecting the per-class
/// caps (#70): an agent terminal first evicts the OLDEST of its own class if
/// agents already occupy [`RECENT_TERMINAL_AGENTS`] slots; the global cap
/// [`RECENT_TERMINAL`] can only eat into the human's entries when it is the
/// human itself overflowing (agents never go past their sub-cap).
fn push_recent(
    recent: &mut std::collections::VecDeque<(norte_proto::TaskProgress, Actor)>,
    snapshot: norte_proto::TaskProgress,
    owner: &Actor,
) {
    if !matches!(owner, Actor::User) {
        let agents = recent
            .iter()
            .filter(|(_, o)| !matches!(o, Actor::User))
            .count();
        if agents >= RECENT_TERMINAL_AGENTS
            && let Some(pos) = recent.iter().position(|(_, o)| !matches!(o, Actor::User))
        {
            recent.remove(pos);
        }
    }
    recent.push_back((snapshot, owner.clone()));
    while recent.len() > RECENT_TERMINAL {
        recent.pop_front();
    }
}

/// THE visibility/scope criterion over tasks (#66), the single one for
/// `task.list`, `task.cancel` and the progress broadcast: a human observes
/// everything; any other actor, only its own (actor equality — two
/// connections of the same agent session share a view, consistent with
/// `ScopeRegistry`). A future `Plugin` owner is fail-closed: only the human
/// sees it.
fn may_observe(viewer: &Actor, owner: &Actor) -> bool {
    matches!(viewer, Actor::User) || viewer == owner
}

/// Where hook notices go (ADR 0100): to human connections, via
/// `plugin.notice`. `Weak` for the same reason as the connection observer.
struct DaemonHookSink {
    shared: Weak<Shared>,
}

impl crate::hooks::HookNoticeSink for DaemonHookSink {
    fn notice(&self, n: methods::PluginNotice) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        // If serialization failed (it cannot: a flat struct), better NOT to
        // emit than to emit a notif with a corrupt shape.
        let Ok(params) = serde_json::to_value(&n) else {
            return;
        };
        let notif = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::PLUGIN_NOTICE.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&notif) {
            shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
        }
    }

    /// Daemon dead, dispatcher dead.
    fn is_closed(&self) -> bool {
        self.shared.strong_count() == 0
    }
}

/// The daemon's connection-warning observer (#44): encodes each warning as
/// `connection.degraded` and broadcasts it ONLY to humans (like `policy.*`).
/// `Weak` breaks the Shared→engine→observer→Shared cycle.
struct DaemonConnectionObserver {
    shared: std::sync::Weak<Shared>,
    /// The observer that was already in the engine's slot, if any. The slot
    /// holds ONE: whoever installs forwards, does not overwrite.
    previo: Option<Arc<dyn crate::connect::ConnectionObserver>>,
}

impl crate::connect::ConnectionObserver for DaemonConnectionObserver {
    fn on_connection_warning(&self, w: &crate::connect::ConnectionWarning) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let notif = norte_proto::methods::ConnectionDegraded {
            scheme: w.scheme.clone(),
            host: w.host.clone(),
            reason: w.reason.wire().to_owned(),
            detail: None,
        };
        // If serialization failed (it cannot: a flat struct), better NOT to
        // emit than to emit a notif with a corrupt shape.
        let Ok(params) = serde_json::to_value(&notif) else {
            return;
        };
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::CONNECTION_DEGRADED.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            // Humans only: a degraded session is security info for the user,
            // not for the agent (same criterion as policy.*).
            shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
        }
        if let Some(p) = &self.previo {
            p.on_connection_warning(w);
        }
    }

    /// #322: why the connection could NOT be made, for whoever is watching.
    ///
    /// Same path and same criterion as the degradation warning: humans only.
    /// An agent connection does not read sentences — it decides by category,
    /// and the category already reaches it in its operation's error.
    fn on_connection_failure(&self, f: &crate::connect::ConnectionFailure) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let notif = norte_proto::methods::ConnectionFailed {
            conn: f.conn.clone(),
            scheme: f.scheme.clone(),
            host: f.host.clone(),
            reason: f.reason.wire().to_owned(),
            detail: f.detail.clone(),
        };
        let Ok(params) = serde_json::to_value(&notif) else {
            return;
        };
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::CONNECTION_FAILED.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
        }
        if let Some(p) = &self.previo {
            p.on_connection_failure(f);
        }
    }
}

impl Shared {
    fn idle(&self) -> bool {
        self.connections.load(Ordering::SeqCst) == 0
            && self.tasks.lock().expect("tasks lock is sound").is_empty()
    }

    /// Broadcasts to ALL connections. Today only `daemon.going_away`: this
    /// daemon leaving affects an agent the same as a human, and both have to
    /// decide the same thing (come back or not).
    fn broadcast_all(&self, frame: &Arc<[u8]>) {
        self.broadcast_where(frame, |_| true);
    }

    /// Broadcasts ONLY to human (non-agent) connections: `policy.*` notifs.
    fn broadcast_humans(&self, frame: &Arc<[u8]>) {
        self.broadcast_where(frame, |s| matches!(s.actor, Actor::User));
    }

    /// Broadcasts ONE task's progress: always to humans; an agent connection
    /// only if the task is ITS OWN (same actor). Same criterion as the
    /// `task.list` filter (#66).
    fn broadcast_task_progress(&self, frame: &Arc<[u8]>, owner: &Actor) {
        self.broadcast_where(frame, |s| may_observe(&s.actor, owner));
    }

    /// Sends a frame to ONE specific connection (the hits of an `fs.search`
    /// belong to whoever launched it — never broadcast, security T4). No-op
    /// if the connection died or is no longer subscribed.
    ///
    /// A FULL outbox loses the frame and NOT the subscription (#155): the
    /// eviction used to be irreversible — the entry is only inserted in
    /// `initialize` — and took down with it the terminal `task.progress`,
    /// which is exactly the signal the client uses to detect it is missing
    /// rows. The backlog is still bounded by the channel, which is what
    /// really bounded it; what gets lost is frames, and the contract already
    /// knows how to say that. A CLOSED outbox does remove the entry: there
    /// is nobody there to protect.
    ///
    /// Returns `false` if the frame was NOT delivered. Used so a directed
    /// feed's producer stops: continuing to compare two trees for an hour for
    /// an owner who is not reading is wasted work and a scheduler permit held
    /// onto for nothing.
    fn send_to_conn(&self, conn_id: u64, frame: &Arc<[u8]>) -> bool {
        send_to_conn_impl(&self.subscribers, conn_id, frame)
    }

    /// Marks `conn_id` as the owner of a live directed feed until the guard
    /// is dropped (#155).
    fn feed_guard(self: &Arc<Self>, conn_id: u64) -> DirectedFeed {
        *self
            .directed_feeds
            .lock()
            .expect("directed feeds lock is sound")
            .entry(conn_id)
            .or_insert(0) += 1;
        DirectedFeed {
            shared: Arc::clone(self),
            conn_id,
        }
    }

    fn broadcast_where(&self, frame: &Arc<[u8]>, wants: impl Fn(&Subscriber) -> bool) {
        // Who is NOT evicted for a full outbox (#155): the owner of a live
        // directed feed. Read BEFORE taking the subscribers lock — nesting
        // the two on every broadcast frame is a locking order there is no
        // need to invent.
        let feeds: Vec<u64> = self
            .directed_feeds
            .lock()
            .expect("directed feeds lock is sound")
            .keys()
            .copied()
            .collect();
        broadcast_impl(&self.subscribers, &feeds, frame, wants);
    }
}

/// Testable core of [`Shared::broadcast_where`]: sends `frame` to every
/// subscriber that `wants` accepts and REMOVES the ones whose receiver died.
///
/// A FULL outbox evicts — a slow client's backlog never grows without limit
/// (M1) — UNLESS the connection is in `feeds`, i.e. it owns a live directed
/// feed (#155): for that one, eviction would also take away its own task's
/// terminal `task.progress`, which is the signal it uses to check whether all
/// its rows arrived. It loses the frame and stays subscribed.
fn broadcast_impl(
    subs: &Mutex<HashMap<u64, Subscriber>>,
    feeds: &[u64],
    frame: &Arc<[u8]>,
    wants: impl Fn(&Subscriber) -> bool,
) {
    let mut subs = subs.lock().expect("subscribers lock is sound");
    subs.retain(|conn, s| {
        // A subscriber excluded from THIS notif keeps its subscription.
        if !wants(s) {
            return true;
        }
        match s.tx.try_send(Arc::clone(frame)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if feeds.contains(conn) {
                    tracing::warn!(
                        conn,
                        "owner of an undrained directed feed: the frame is lost, not the subscription"
                    );
                    return true;
                }
                tracing::warn!(conn, "undrained subscriber: evicted from the broadcast");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    });
}

/// Testable core of [`Shared::send_to_conn`]: sends `frame` to connection
/// `conn_id` in `subs` (if it exists) and REMOVES the entry only if its
/// receiver DIED. A full outbox loses the frame and keeps the subscription
/// (#155).
///
/// `true` = delivered.
fn send_to_conn_impl(
    subs: &Mutex<HashMap<u64, Subscriber>>,
    conn_id: u64,
    frame: &Arc<[u8]>,
) -> bool {
    let mut subs = subs.lock().expect("subscribers lock is sound");
    let (delivered, remove) = match subs.get(&conn_id) {
        None => return false,
        Some(s) => match s.tx.try_send(Arc::clone(frame)) {
            Ok(()) => (true, false),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    conn = conn_id,
                    "owner of an undrained directed feed: the frame is lost, not the subscription"
                );
                (false, false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => (false, true),
        },
    };
    if remove {
        subs.remove(&conn_id);
    }
    delivered
}

/// The daemon bound to its socket, ready for [`Daemon::run`].
///
/// `Debug` is deliberately shallow (the socket path): internal state is not
/// API.
pub struct Daemon {
    listener: transport::Listener,
    socket_path: PathBuf,
    shared: Arc<Shared>,
    idle_timeout: Option<Duration>,
    /// The session writer, if there is anywhere to write it.
    ///
    /// **The right to write — the lock — lives INSIDE the task** since #237:
    /// it is the task that takes it late if another core held it at startup,
    /// so keeping it here too would mean keeping it in two places. It is
    /// awaited to finish during an orderly shutdown, and on finishing it
    /// releases the lock: a handoff's successor has to find the file already
    /// written and the lock already free. If the daemon leaves by any other
    /// path, its `Drop` aborts it (see [`SessionWriter`]).
    session_writer: Option<SessionWriter>,
}

/// The session writer, which STOPS if its daemon dies without shutting down.
///
/// **`abort` does not cancel an in-flight `spawn_blocking`.** If it falls
/// right while `flush_session` is waiting on its write, that write finishes,
/// but the writer's state — and with it the lock — is already dropped: the
/// write can land after a successor has taken the lock. This predates #237
/// and that version had it worse (it released `session_lock` BEFORE
/// aborting); it is not reached from the binary, which always waits for
/// `run()` to the end, only from a `Daemon` dropped in a test or in an
/// embedder.
///
/// Dropping a tokio `JoinHandle` DETACHES the task, it does not stop it.
/// Without this wrapper, a daemon that leaves by a path other than orderly
/// shutdown — a failing `accept`, a dropped bind, an abandoning test —
/// releases its `session_lock` and leaves the task alive: a process writing
/// the file WITHOUT the right to write it, which is exactly the second
/// writer all of this exists to prevent.
struct SessionWriter(Option<tokio::task::JoinHandle<()>>);

impl SessionWriter {
    /// The handle, to AWAIT during orderly shutdown. What remains no longer
    /// aborts anything.
    fn take(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        self.0.take()
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Daemon {
    /// Creates (or verifies) the socket directory, binds and authenticates
    /// the environment: dir owner/mode, never root, never two daemons.
    ///
    /// # Errors
    /// [`DaemonError`]: unsafe dir, root, socket occupied by a live daemon,
    /// or I/O.
    #[tracing::instrument(skip(engine, cfg))]
    pub async fn bind(engine: Arc<Engine>, cfg: DaemonConfig) -> Result<Self, DaemonError> {
        Self::bind_with_scopes(engine, ScopeRegistry::new(), cfg).await
    }

    /// Like [`Self::bind`] but shares `scopes` with the engine's
    /// `ScopedPolicy` (M3-3b): the caller builds
    /// `engine.with_policy(ScopedPolicy::new(scopes.clone(), cfg), …)` and
    /// passes the SAME registry here so that `policy.grant_scope` opens the
    /// border the engine's gate consults. Without policy, pass it an empty
    /// registry (or use [`Self::bind`]).
    ///
    /// # Errors
    /// [`DaemonError`]: unsafe dir, root, socket occupied by a live daemon,
    /// or I/O.
    #[tracing::instrument(skip(engine, scopes, cfg))]
    pub async fn bind_with_scopes(
        engine: Arc<Engine>,
        scopes: ScopeRegistry,
        cfg: DaemonConfig,
    ) -> Result<Self, DaemonError> {
        Self::bind_with_policy(
            engine,
            scopes,
            Arc::new(DaemonApprovalResolver::default()),
            cfg,
        )
        .await
    }

    /// Like [`Self::bind_with_scopes`] but also shares the `Ask` approval
    /// router (M3-3b Task 4). Construction order: the caller creates
    /// `approvals` FIRST, builds the engine with
    /// `with_policy(ScopedPolicy::new(scopes.clone(), cfg), approvals.clone())`
    /// and passes the SAME `Arc` here; this bind installs its outlet toward
    /// the subscribers (broadcast of `policy.approval_required`) and routes
    /// `policy.decide`/`policy.pending` to it.
    ///
    /// # Errors
    /// [`DaemonError`]: unsafe dir, root, socket occupied by a live daemon,
    /// or I/O.
    ///
    /// # Policy
    /// An engine on which nobody called
    /// [`Engine::with_policy`](crate::Engine::with_policy) gates with
    /// `AllowAll`: this bind WARNS about it via `warn!` and continues (#166).
    /// It is not rejected because "permissive on purpose" is a legitimate
    /// configuration; what it must not be is indistinguishable from an
    /// oversight.
    // Strictly linear startup sequence (socket → plugins → session → `Shared`
    // → routers), one step per block — same criterion as
    // `dispatch`/`dispatch_fs_task` in this same file: splitting it into
    // sub-functions would not reduce the bind's real complexity, only hide it
    // behind indirection and more parameters crossing the boundary. It
    // crossed 100 lines when 0.65.0 mounted the log ring (`log_ring`) into
    // `Shared` (#328, ADR 0092).
    #[expect(
        clippy::too_many_lines,
        reason = "daemon startup: mounts each piece of `Shared` once, in order"
    )]
    #[tracing::instrument(skip(engine, scopes, approvals, cfg))]
    pub async fn bind_with_policy(
        engine: Arc<Engine>,
        scopes: ScopeRegistry,
        approvals: Arc<DaemonApprovalResolver>,
        cfg: DaemonConfig,
    ) -> Result<Self, DaemonError> {
        // #166: the engine's gate is `AllowAll` while nobody installs a
        // policy, and a daemon over that engine gates NOTHING — not even an
        // agent. None of our binaries reaches here that way (`daemon run`
        // installs `ScopedPolicy`), but an embedder or a harness can, and the
        // gap does not have a single log line today. `sync.apply` is what
        // changes the consequences: one call, one hash, and a `Mirror`
        // rewrites and deletes.
        if !engine.has_explicit_policy() {
            tracing::warn!(
                "daemon mounted on an engine WITHOUT policy: every operation from \
                 every actor passes (AllowAll by default). Install a policy with \
                 Engine::with_policy before bind (#166)."
            );
        }
        // Resolving the default path can touch the FS (uid probe of the
        // /tmp fallback), and discovering the plugin catalogue reads the
        // config dir: ALL synchronous I/O inside the spawn_blocking (rule 2).
        // The plugin root is also needed by the hook dispatcher (ADR 0100),
        // which rediscovers the registry per batch from this same place.
        let plugins_root = cfg
            .plugins_dir
            .clone()
            .unwrap_or_else(crate::connect::config_dir);
        let (bound, owner, socket_path, plugins, plugin_runtime) =
            crate::blocking::spawn_blocking({
                let requested = cfg.socket_path;
                let plugins_dir = cfg.plugins_dir.clone();
                move || -> Result<
                (
                    transport::Bound,
                    transport::Owner,
                    PathBuf,
                    crate::plugins::PluginRegistry,
                    Arc<norte_plugin_host::PluginRuntime>,
                ),
                DaemonError,
            > {
                let (bound, owner, socket_path) = transport::bind(requested)?;
                // Plugin catalogue + shared WASM runtime (M4-P3/P4):
                // synchronous I/O/CPU inside the spawn_blocking (rule 2).
                let (plugins, plugin_runtime) = discover_plugins(plugins_dir)?;
                Ok((bound, owner, socket_path, plugins, plugin_runtime))
            }
            })
            .await
            // A panic from the closure is NOT a problem with the dir: an
            // honest category.
            .map_err(|e| DaemonError::Io(std::io::Error::other(e)))??;
        let listener = transport::Listener::new(bound)?;

        // The UI session (L2): the lock and the load are synchronous I/O, so
        // they go to a blocking pool (rule 2). Without `state_dir` nothing is
        // persisted, which is what a test wants; with it, whoever fails to
        // get the lock starts WITH the screen and without a writer — clones
        // it and runs detached.
        let (session_lock, session, writable) = open_session(cfg.state_dir.clone()).await?;
        // Persisting is THREE things at once: there is somewhere to, the
        // right is held, and what is on disk is not from a newer binary.
        // What is computed here is the STARTUP, not the whole lifetime
        // (#237): whoever is only missing the lock gets it retried by the
        // writer.
        // `Release`/`Acquire` and not `Relaxed`: the writer ADOPTS the
        // document (under the store's mutex) and only afterward turns on
        // this flag, and the `session.get` handler reads the flag and only
        // afterward the document. With `Relaxed` nothing ties those two
        // pairs together, so a client could receive `owner: true` with the
        // revision from BEFORE the adoption, write against it and get a
        // `Conflict` that had no reason to exist. Fixed for free: on x86
        // these are the same instructions.
        let session_persists = Arc::new(AtomicBool::new(
            session_lock.is_some() && cfg.state_dir.is_some() && writable,
        ));
        let session_flush = Arc::new(tokio::sync::Notify::new());

        tracing::info!(socket = %socket_path.display(), owner = %owner, "daemon bound");
        let shared = Arc::new(Shared {
            engine,
            tasks: Mutex::new(HashMap::new()),
            recent: Mutex::new(std::collections::VecDeque::new()),
            subscribers: Mutex::new(HashMap::new()),
            next_conn: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            shutdown: CancellationToken::new(),
            hard_shutdown: CancellationToken::new(),
            owner,
            connections_dir: cfg.plugins_dir.clone(),
            listing_ttl: cfg.listing_ttl,
            open_listings: Arc::new(AtomicUsize::new(0)),
            scopes,
            pending_scope: Mutex::new(HashMap::new()),
            next_scope_req: AtomicU64::new(0),
            approvals: Arc::clone(&approvals),
            plugins: Mutex::new(plugins),
            plugins_state_io: tokio::sync::Mutex::new(()),
            plugin_runtime,
            column_pool: Arc::new(crate::plugins::ColumnPool::default()),
            log_ring: std::sync::OnceLock::new(),
            directed_feeds: Mutex::new(HashMap::new()),
            ui_session: Arc::new(crate::ui_session::SessionStore::new(session)),
            session_persists: Arc::clone(&session_persists),
            session_flush: Arc::clone(&session_flush),
        });
        // The approval router's outlet toward the subscribers. `Weak` breaks
        // the Shared → approvals → closure → Shared cycle: with the daemon
        // dead, a late Ask does not broadcast to anyone (and will expire by
        // TTL).
        let weak = Arc::downgrade(&shared);
        approvals.set_broadcaster(Box::new(move |notif| {
            let Some(shared) = weak.upgrade() else { return };
            // If serialization failed (it cannot: a flat struct), better NOT
            // to emit than to emit a notif with a corrupt shape.
            let Ok(params) = serde_json::to_value(&notif) else {
                return;
            };
            let n = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::POLICY_APPROVAL_REQUIRED.into(),
                params: Some(params),
            };
            if let Ok(frame) = encode_frame(&n) {
                // Humans ONLY (security MAJOR-1): the notif crosses sessions
                // — same criterion as `policy.pending`'s User-only gate.
                shared.broadcast_humans(&Arc::from(frame.into_boxed_slice()));
            }
        }));
        // #44: connection warnings (TLS degradation) → `connection.degraded`
        // ONLY to humans, and #322: failures → `connection.failed`. `Weak`
        // breaks the Shared → engine → observer → Shared cycle.
        //
        // CHAINS instead of overwriting, even though the engine that arrives
        // here today comes freshly built: the slot holds one, and an
        // installer that overwrites it leaves the previous one SILENTLY mute.
        // That is the failure chaining exists to make impossible again.
        shared.engine.chain_connection_observer(|previo| {
            Arc::new(DaemonConnectionObserver {
                shared: Arc::downgrade(&shared),
                previo,
            })
        });
        // ADR 0100: the hooks. The dispatcher lives as long as the daemon;
        // its notices — a plugin's sentence, or "I turned off its hooks" —
        // go ONLY to humans via `plugin.notice`, with the same `Weak` that
        // breaks the cycle above. The event source is the engine's journal,
        // so a mutation from an agent over MCP fires the same as one from
        // the human.
        // Ends with the daemon's shutdown (rule 3): the same signal that
        // stops everything else.
        let (hooks_tx, _hooks_task) = crate::hooks::spawn_dispatcher(
            plugins_root,
            Arc::clone(&shared.plugin_runtime),
            Arc::new(DaemonHookSink {
                shared: Arc::downgrade(&shared),
            }),
            shared.shutdown.clone(),
            Some(crate::hooks::SidecarWriter {
                engine: Arc::downgrade(&shared.engine),
                scopes: Some(shared.scopes.clone()),
                // The rules are applied by the engine's gate; not needed
                // here.
                policy: None,
            }),
        );
        if shared.engine.claim_hooks_slot() {
            shared.engine.enable_hooks(hooks_tx).await;
        }
        // The writer exists whenever there is SOMEWHERE to write, and it is
        // the one that decides whether it really writes: it starts with the
        // lock if the bind got it, and without it, it retries (#237). What
        // still holds is the gate: a detached core — or one that found a
        // session from a newer binary — has the screen and does NOT write
        // it. Without that, "not read" ended up being "overwritten a second
        // later", which is exactly the opposite of what it promises.
        let session_writer = cfg.state_dir.map(|dir| {
            SessionWriter(Some(crate::blocking::spawn(session_writer(
                Arc::clone(&shared.ui_session),
                dir,
                session_flush,
                shared.shutdown.clone(),
                session_persists,
                WriteState::initial(session_lock, writable),
            ))))
        });
        Ok(Self {
            listener,
            socket_path,
            shared,
            idle_timeout: cfg.idle_timeout,
            session_writer,
        })
    }

    /// Mounts the log ring this daemon will serve via `log.tail` (#328, ADR
    /// 0092).
    ///
    /// Called between the bind and [`Self::run`], by whoever installed the
    /// subscriber: the ring has to be THE SAME one the `tracing` layer writes
    /// into, and only the binary that mounted the log
    /// (`norte_config::logging::init_with_ring`) knows that. That is why it
    /// does not travel in [`DaemonConfig`], which holds values, but here,
    /// with `scopes` and `approvals`, which are shared objects.
    ///
    /// A daemon that gets none mounted answers
    /// [`norte_proto::Error::Unsupported`] to `log.tail` and `log.level`, and
    /// never an empty list: the frontend degrades to its local ring WHILE
    /// SAYING why.
    ///
    /// A second call does not change the already-mounted ring — there is one
    /// per process, and changing it live would leave the client with a
    /// cursor counting lines from somewhere else.
    #[must_use]
    pub fn with_log_ring(self, ring: norte_config::logring::LogRing) -> Self {
        let _ = self.shared.log_ring.set(ring);
        self
    }

    /// Where the socket ended up (for clients and logs).
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Token that shuts the daemon down from outside (the binary's SIGTERM).
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// Serves until shutdown (request, external token or idleness). On exit,
    /// the socket has been removed from the FS and the tasks have finished
    /// (graceful) or been cancelled (hard).
    ///
    /// # Errors
    /// Only unrecoverable listener I/O.
    ///
    /// # Panics
    /// Never: the internal locks do not get poisoned (nobody panics while
    /// holding them).
    #[tracing::instrument(skip(self), fields(socket = %self.socket_path.display()))]
    pub async fn run(mut self) -> Result<(), DaemonError> {
        let shared = Arc::clone(&self.shared);
        let mut idle_since = tokio::time::Instant::now();
        // A failing `accept` does NOT exit via `?`: exiting that way would
        // skip the orderly shutdown below, and what is left behind is a
        // DETACHED session writer — the `JoinHandle` is dropped without
        // aborting, i.e. the task keeps going — writing the file with the
        // lock already released. The error is stored and returned AFTER
        // shutting down.
        let mut failure: Option<std::io::Error> = None;
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let stream = match accepted {
                        Ok(v) => v,
                        Err(e) => { failure = Some(e); break }
                    };
                    // The idleness keepalive does NOT count unauthenticated
                    // connections (serve resets it after auth); the
                    // anti-exhaustion cap cuts here, before spending
                    // anything.
                    if shared.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                        tracing::warn!("connection limit reached; rejected");
                        drop(stream);
                    } else {
                        spawn_connection(stream, Arc::clone(&shared));
                    }
                }
                () = shared.shutdown.cancelled() => break,
                () = tokio::time::sleep(IDLE_POLL) => {
                    if !shared.idle() {
                        idle_since = tokio::time::Instant::now();
                    } else if let Some(t) = self.idle_timeout
                        && idle_since.elapsed() >= t
                    {
                        tracing::info!("shutdown due to idleness");
                        break;
                    }
                }
            }
        }

        // The UI session is flushed and the lock released HERE, before
        // removing the socket path — which is what grants a handoff's
        // successor permission to start. The other way around, the successor
        // would find the lock still held and run detached: a handoff would
        // leave the new daemon unable to save anything, exactly the opposite
        // of what a handoff promises. The token is also cancelled here
        // because the idleness path leaves the loop without going through
        // `daemon.shutdown`.
        // CLOSE the session before the final flush, not after: between the
        // flush and closing the connections a `put` fits, and sealing under
        // the same lock as the mutation is the only thing that prevents
        // answering `Ok(revision)` over a file nobody is going to write
        // anymore.
        //
        // And the seal goes before the `cancel`, not after (revised after
        // #237): `cancel` ARMS the writer's shutdown branch, which does its
        // final flush and finishes — on a multithreaded runtime that can
        // happen before the next line executes, and a `put` landing in that
        // window would receive `Ok(revision)` for a body nobody writes
        // anymore. Which is exactly what the paragraph above says does not
        // happen.
        shared.ui_session.seal();
        shared.shutdown.cancel();
        // Waiting for the writer means waiting for the final flush AND for it
        // to release the lock: both live inside the task since #237.
        if let Some(mut writer) = self.session_writer.take()
            && let Some(handle) = writer.take()
        {
            let _ = handle.await;
        }

        // Shutdown phase: no new clients (the listener dies with the drop);
        // hard = cancel tasks; graceful = wait for them — and if hard arrives
        // DURING the wait (second signal), they get cancelled right away.
        drop(self.listener);
        // **The path is removed HERE, before draining, and the order has
        // mattered since handoff exists (roadmap item 10).**
        //
        // With the listener dead and the file still in place, a client that
        // reconnects gets `ECONNREFUSED`, and — only after a handoff, because
        // only then does it have permission — the replacement starts up. The
        // replacement sees a stale path, deletes it, and binds its own. If
        // this daemon finished draining first, its `remove_file` would delete
        // the REPLACEMENT's socket: it would end up listening on a nameless
        // inode, and since the startup permission is single-use, nobody would
        // ever bring it back up.
        //
        // Deleting earlier shrinks the window from "however long draining
        // takes" to microseconds, and what this daemon deletes is always its
        // own. Nobody loses anything: with the listener already dead, the
        // path only served to give `ECONNREFUSED` instead of `NotFound`.
        let socket_path = self.socket_path.clone();
        let socket_for_delete = socket_path.clone();
        // Rule 2: not a single synchronous unlink on the runtime.
        let _ =
            crate::blocking::spawn_blocking(move || transport::release(socket_for_delete)).await;
        let mut hard_done = false;
        loop {
            if shared.hard_shutdown.is_cancelled() && !hard_done {
                hard_done = true;
                for task in shared.tasks.lock().expect("tasks lock is sound").values() {
                    task.handle.cancel();
                }
            }
            if shared.tasks.lock().expect("tasks lock is sound").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // And the CONNECTIONS, before returning: whoever calls `run()` is
        // usually a `main` that returns as soon as this returns, and dropping
        // the runtime kills every live task — including the one writing the
        // response to this very `daemon.shutdown`. `norte daemon stop` used
        // to fail this way, now and then, with "connection closed with the
        // request in flight" on a daemon that had in fact stopped. Each
        // connection exits on its own upon seeing the `shutdown`; this only
        // waits for it to finish writing. With a deadline: a client that does
        // not read can have a full socket, and it no longer holds up
        // shutdown.
        let deadline = tokio::time::Instant::now() + CONNECTION_DRAIN;
        while shared.connections.load(Ordering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    open = shared.connections.load(Ordering::SeqCst),
                    "shutdown without waiting for connections that never finish writing"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = socket_path;
        tracing::info!("daemon shut down");
        match failure {
            Some(e) => Err(DaemonError::Io(e)),
            None => Ok(()),
        }
    }

    /// Token that CANCELS live tasks in addition to shutting down (the
    /// binary's second signal, or `stop --hard`).
    #[must_use]
    pub fn hard_shutdown_token(&self) -> CancellationToken {
        self.shared.hard_shutdown.clone()
    }
}

/// Takes the UI session's lock and loads it (L2).
///
/// Without `state_dir` nothing is persisted: no lock, no file. With it, the
/// lock decides who WRITES — whoever fails to get it starts up the same, with
/// the same screen, and never writes it — and the load never fails: its bad
/// outcomes give an empty session and a notice.
///
/// All the I/O goes to a blocking pool (rule 2).
async fn open_session(
    state_dir: Option<PathBuf>,
) -> Result<
    (
        Option<crate::ui_session::disk::SessionLock>,
        norte_proto::methods::Session,
        bool,
    ),
    DaemonError,
> {
    let Some(dir) = state_dir else {
        return Ok((None, norte_proto::methods::Session::default(), false));
    };
    crate::blocking::spawn_blocking(move || {
        // A lock that cannot even be attempted (permissions, full disk) does
        // NOT prevent startup: it leaves the core detached, the same
        // degradation that already exists for the second core.
        let lock = crate::ui_session::disk::lock(&dir).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not take the UI session lock");
            None
        });
        let r = crate::ui_session::disk::load_or_default(&dir);
        (lock, r.session, r.writable)
    })
    .await
    .map_err(|e| DaemonError::Io(std::io::Error::other(e)))
}

/// The single writer of the UI session: coalesces changes and flushes them.
///
/// One tick per second, and each one **only if something is dirty**.
/// Coalescing is the whole point: the cursor moves on every arrow key and
/// this is a file, not a database. `session_flush` moves it forward when the
/// last connection leaves, and the token ends it — with a final flush, which
/// is what handoff needs and the whole reason this exists.
///
/// A write failure is a `warn!`, the dirty flag is set AGAIN and the next
/// tick retries: losing a session is a bad afternoon, and taking down the
/// daemon over it is worse. (`mark_dirty` is what makes a real retry happen;
/// without it the flag was already clean and the warning was all that
/// happened.)
async fn session_writer(
    store: Arc<crate::ui_session::SessionStore>,
    dir: PathBuf,
    flush: Arc<tokio::sync::Notify>,
    stop: CancellationToken,
    persiste: Arc<AtomicBool>,
    initial: WriteState,
) {
    let mut state = initial;
    if matches!(state, WriteState::Surrendered) {
        // The bind held the lock and the file was from a newer binary: it
        // was released while building the state and is not retried. The task
        // ENDS here instead of spinning a once-a-second timer for the whole
        // life of the daemon to do nothing with it.
        persiste.store(false, Ordering::Release);
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                // The retry goes on the TICK and only there: it is the only
                // one of the three wake-ups that happens no matter what. With
                // shutdown already requested it is not attempted: taking the
                // lock right then costs an acquisition and a flush at the
                // worst possible moment — a handoff, where the successor is
                // polling that very lock — and `select!` can pick this branch
                // with the token already cancelled.
                if !stop.is_cancelled() {
                    state = state.retry(&dir, &store, &persiste).await;
                }
                if state.writes() {
                    flush_session(&store, &dir).await;
                }
            }
            () = flush.notified() => {
                if state.writes() {
                    flush_session(&store, &dir).await;
                }
            }
            () = stop.cancelled() => {
                // The last one, and so the one that matters: this is where
                // the screen survives a handoff.
                if state.writes() {
                    flush_session(&store, &dir).await;
                }
                break;
            }
        }
    }
    // Explicit: the lock is released when the task ends, and orderly shutdown
    // WAITS for it exactly for this — the handoff's successor has to find the
    // file written and the lock free.
    drop(state);
}

/// The right to write the session, as seen by the writer (#237).
///
/// Three states and not an `Option<SessionLock>`, because "I don't have it
/// yet" and "I'm never going to have it" are different decisions: the first
/// is retried every tick and the second is never retried.
enum WriteState {
    /// With the lock: this instance is the one that writes. The lock is
    /// never READ — it is worth having for its `Drop`, which releases it,
    /// hence the name.
    Owned {
        /// The right, alive for as long as the state lasts.
        _lock: crate::ui_session::disk::SessionLock,
    },
    /// Without the lock, and retrying it. `warned` so the warning goes out
    /// once and not once a second; `ticks` counts the attempts to space them
    /// out ([`WriteState::should_retry`]).
    Detached { warned: bool, ticks: u32 },
    /// On disk there is a session from a NEWER binary. It is not overwritten
    /// and not retried for the rest of the process's life.
    ///
    /// **And that is not free**: if the future file is deleted or later
    /// replaced by a readable one — a version rollback, a manual `rm` — this
    /// daemon keeps answering `owner: false` and every one of its windows
    /// keeps saying "not being saved" until it restarts. Accepted because it
    /// is the same thing the embedded arm's `surrendered` state does, and
    /// because a newer binary running is that state's normal situation.
    Surrendered,
}

impl WriteState {
    /// The state the writer starts with, from what the bind managed to get.
    ///
    /// With the lock taken but a file from the future, it is released HERE:
    /// keeping it would leave the file hostage to a core that cannot write
    /// it, which is what the daemon used to do before #237.
    fn initial(lock: Option<crate::ui_session::disk::SessionLock>, writable: bool) -> Self {
        match (lock, writable) {
            (Some(lock), true) => Self::Owned { _lock: lock },
            (Some(lock), false) => {
                drop(lock);
                Self::Surrendered
            }
            (None, _) => Self::Detached {
                warned: false,
                ticks: 0,
            },
        }
    }

    /// Does this process write?
    fn writes(&self) -> bool {
        matches!(self, Self::Owned { .. })
    }

    /// How many ticks the lock is tried once a second before spacing out.
    ///
    /// The case that matters is a HANDOFF: the old daemon leaves seconds
    /// after the new one starts, and there a second of latency is the
    /// difference between saving the screen and losing it. Past that minute,
    /// what is there is someone else's core that can last for hours, and
    /// still polling every second is one `mkdir`+`open`+`flock` per second
    /// forever.
    const BURST: u32 = 60;
    /// Cadence after the burst: the same as the embedded arm.
    const SPACING: u32 = 30;

    /// Is this tick due for an attempt?
    fn should_retry(ticks: u32) -> bool {
        ticks < Self::BURST || ticks.is_multiple_of(Self::SPACING)
    }

    /// Retries the lock if it is not held yet (#237).
    ///
    /// Every second during the first minute and every thirty afterward
    /// ([`Self::should_retry`]). The warning goes out only once.
    ///
    /// **The file is read ONLY once the lock is already in hand.** Reading it
    /// before knowing whether it was obtained — which is what the first
    /// version did — is a `read` and a parse of up to a meg per second whose
    /// result is thrown away, and worse: `load_or_default` WARNS about a
    /// corrupt or future file, so a permanently detached daemon used to write
    /// that `warn!` once a second forever, burying everything else in the
    /// log. The embedded arm never did it that way.
    ///
    /// On getting it late, the file is re-read, exactly like the embedded
    /// arm: if a newer binary wrote it, the just-taken lock is released and
    /// abandoned; if another core of this version wrote it, its document is
    /// the current one and is adopted WHOLE — body included — or this
    /// instance would answer its own screen with the other one's number and
    /// what the other saved would disappear without anything noticing.
    async fn retry(
        self,
        dir: &Path,
        store: &Arc<crate::ui_session::SessionStore>,
        persiste: &Arc<AtomicBool>,
    ) -> Self {
        let Self::Detached { warned, ticks } = self else {
            return self;
        };
        let next = ticks.saturating_add(1);
        if !Self::should_retry(ticks) {
            return Self::Detached {
                warned,
                ticks: next,
            };
        }
        let d = dir.to_path_buf();
        // Rule 2: the lock and the read are disk I/O.
        let attempt = crate::blocking::spawn_blocking(move || {
            let Some(lock) = crate::ui_session::disk::lock(&d)? else {
                return Ok::<_, std::io::Error>(None);
            };
            // With the lock in place, and not before.
            let recovered = crate::ui_session::disk::load_or_default(&d);
            Ok(Some((lock, recovered)))
        })
        .await;
        let taken = match attempt {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                if !warned {
                    tracing::warn!(error = %e, "could not take the UI session lock");
                }
                return Self::Detached {
                    warned: true,
                    ticks: next,
                };
            }
            Err(e) => {
                if !warned {
                    tracing::warn!(error = %e, "the UI session lock attempt crashed");
                }
                return Self::Detached {
                    warned: true,
                    ticks: next,
                };
            }
        };
        let Some((lock, recovered)) = taken else {
            return Self::Detached {
                warned,
                ticks: next,
            };
        };
        if !recovered.writable {
            drop(lock);
            persiste.store(false, Ordering::Release);
            tracing::warn!("the UI session on disk is from a newer binary: not writing it");
            return Self::Surrendered;
        }
        store.adopt_from_disk(recovered.session);
        persiste.store(true, Ordering::Release);
        tracing::info!("the UI session became free: this daemon is saving it again");
        Self::Owned { _lock: lock }
    }
}

/// Flushes the session if there is anything to flush. Nothing dirty = not
/// even an `open`, which is what makes it cheap to wake up every second.
async fn flush_session(store: &Arc<crate::ui_session::SessionStore>, dir: &Path) {
    let Some(session) = store.take_dirty() else {
        return;
    };
    let dir = dir.to_path_buf();
    // Rule 2: the write is disk I/O and goes to a blocking pool.
    let written =
        crate::blocking::spawn_blocking(move || crate::ui_session::disk::write(&dir, &session))
            .await;
    match written {
        Ok(Ok(())) => {}
        // `take_dirty` already took the dirty flag away, so a failure
        // WITHOUT marking it again is never retried: the next tick would see
        // nothing to do and the screen would be lost to a one-second
        // `ENOSPC`.
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "could not write the UI session; retrying");
            store.mark_dirty();
        }
        Err(e) => {
            tracing::warn!(error = %e, "the UI session flush crashed; retrying");
            store.mark_dirty();
        }
    }
}

/// Discovers the plugin catalogue and builds the shared WASM runtime
/// (M4-P3/P4). ALL synchronous I/O/CPU: the caller invokes it inside the
/// bind's `spawn_blocking` (rule 2).
///
/// A corrupt `plugins-state.toml` does NOT prevent the daemon from starting
/// (it would leave the user without any other operation over a broken state
/// file): it degrades FAIL-CLOSED to an EMPTY registry (nothing approved or
/// enabled) with a warning, and the user can re-approve. Corruption NEVER
/// "opens up" a plugin that was not consented to. An absent catalogue is
/// already "empty" with no error. A failure creating the runtime, on the
/// other hand, DOES abort the bind: without a runtime, no plugin runs
/// (fail-closed).
fn discover_plugins(
    plugins_dir: Option<PathBuf>,
) -> Result<
    (
        crate::plugins::PluginRegistry,
        Arc<norte_plugin_host::PluginRuntime>,
    ),
    DaemonError,
> {
    let plugins_root = plugins_dir.unwrap_or_else(crate::connect::config_dir);
    let plugins = crate::plugins::PluginRegistry::discover(&plugins_root).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "corrupt plugins-state: starting with an empty catalogue");
        crate::plugins::PluginRegistry::empty(&plugins_root)
    });
    let runtime = norte_plugin_host::PluginRuntime::new()
        .map(Arc::new)
        .map_err(|e| {
            DaemonError::Io(std::io::Error::other(format!(
                "could not create the plugin runtime: {e}"
            )))
        })?;
    Ok((plugins, runtime))
}

/// Prepares the dir, binds the UDS socket and hardens it (0600, no-root),
/// re-verifying the dir's identity after the bind (#34.2). `requested` `None`
/// = default fallback (with an actionable message about `/tmp`, #34.1).
/// Synchronous: called inside the bind's `spawn_blocking` (rule 2).
#[cfg(unix)]
pub(super) fn bind_socket(
    requested: Option<PathBuf>,
) -> Result<(std::os::unix::net::UnixListener, u32, PathBuf), DaemonError> {
    let defaulted = requested.is_none();
    let socket_path = requested.unwrap_or_else(|| super::default_socket_path(None));
    let dir = socket_path
        .parent()
        .ok_or(DaemonError::InsecureDir {
            reason: "the socket needs a parent directory",
        })?
        .to_path_buf();
    // #34.1: over the /tmp fallback, an unsafe dir (squat) exits with an
    // ACTIONABLE message instead of the opaque InsecureDir.
    let dir_id = prepare_socket_dir(&dir).map_err(|e| match e {
        DaemonError::InsecureDir { reason } if is_default_tmp_fallback(&socket_path, defaulted) => {
            DaemonError::UnusableDefaultDir {
                path: dir.clone(),
                reason,
            }
        }
        other => other,
    })?;
    // Is there a LIVE daemon? A connect gives it away; an orphaned socket
    // (previous crash) gives ECONNREFUSED and is removed.
    match std::os::unix::net::UnixStream::connect(&socket_path) {
        Ok(_) => return Err(DaemonError::AlreadyRunning),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => match std::fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        },
    }
    let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            return Err(DaemonError::AlreadyRunning);
        }
        Err(e) => return Err(e.into()),
    };
    // #34.2 (TOCTOU): detects the COMMON swap (rm+recreate changes the inode)
    // of the dir between prepare and bind; if it changed, the freshly created
    // socket lives in someone else's dir — it is removed and aborted. NOT a
    // total barrier (inode reuse, later swap): the channel's integrity is
    // guaranteed by the bilateral peer-cred, see `DirIdentity`. The
    // bind→this-check window is not exploitable (the accept loop does not
    // start until this helper returns Ok).
    if let Err(e) = dir_id.verify_unchanged(&dir) {
        let _ = std::fs::remove_file(&socket_path);
        return Err(e);
    }
    // Our euid = the owner of the socket we JUST created (no unsafe, rule 5).
    // Only the same uid will be able to speak.
    let md = std::fs::metadata(&socket_path)?;
    let uid = md.uid();
    if uid == 0 {
        let _ = std::fs::remove_file(&socket_path);
        return Err(DaemonError::Root);
    }
    // The socket itself does not give anything away either: 0600.
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, uid, socket_path))
}

/// A directory's identity: `(dev, ino)`. Captured when validating the
/// socket's dir and re-verified after the bind — a common REPLACEMENT of the
/// dir under the same path during the prepare→bind window (TOCTOU with no
/// sticky bit on /tmp, #34.2) changes the inode and is detected.
///
/// SCOPE (defense in depth, not a total barrier): (a) checking by PATH is
/// intrinsically racy — every `of` re-stats; (b) inode reuse (ext4/tmpfs
/// recycle a freed ino instantly) can give an identical `(dev, ino)` after an
/// rm+recreate; (c) it is one-shot: it does not cover a LATER swap during the
/// socket's life. The REAL guarantee against an impostor daemon in a
/// squatted dir is the BILATERAL peer-cred (server: `peer_allowed`; client:
/// `Client::authenticated` rejects a socket whose owner is not its uid).
/// Full closure would require anchoring to an fd (openat/fstatat), which in
/// std requires `unsafe`/a dep — out of scope (rule 5).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl DirIdentity {
    /// Identity of the dir at `path` WITHOUT following the last component's
    /// symlinks.
    fn of(path: &Path) -> Result<Self, DaemonError> {
        let md = std::fs::symlink_metadata(path)?;
        Ok(Self {
            dev: md.dev(),
            ino: md.ino(),
        })
    }

    /// `Ok` if `path` is still the SAME FS object (dev+ino) as this identity;
    /// `InsecureDir` if it was replaced (or disappeared).
    fn verify_unchanged(self, path: &Path) -> Result<(), DaemonError> {
        let now = Self::of(path).map_err(|_| DaemonError::InsecureDir {
            reason: "the socket's dir disappeared during the bind",
        })?;
        if now == self {
            Ok(())
        } else {
            Err(DaemonError::InsecureDir {
                reason: "the socket's dir was replaced during the bind",
            })
        }
    }
}

/// Verifies (creating it if missing) that the socket's dir is OURS and 0700:
/// never a symlink, never another uid's, never accessible to others. Returns
/// the validated [`DirIdentity`] to re-check after the bind (#34.2).
#[cfg(unix)]
fn prepare_socket_dir(dir: &Path) -> Result<DirIdentity, DaemonError> {
    match std::fs::create_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let md = std::fs::symlink_metadata(dir)?;
    if md.file_type().is_symlink() {
        return Err(DaemonError::InsecureDir {
            reason: "the socket's dir is a symlink",
        });
    }
    if !md.is_dir() {
        return Err(DaemonError::InsecureDir {
            reason: "the socket's path is not a directory",
        });
    }
    // Harden the mode BEFORE comparing: if the dir is ours, this leaves it at
    // 0700; if it is someone else's, it will fail or the owner check will
    // give it away.
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| {
            DaemonError::InsecureDir {
                reason: "could not harden the mode to 0700",
            }
        })?;
    }
    let md = std::fs::symlink_metadata(dir)?;
    if md.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::InsecureDir {
            reason: "the socket's dir is accessible to other users",
        });
    }
    // Owner: compared against the REAL euid further on (the created
    // socket's); here it is enough to reject dirs we cannot own.
    let probe = dir.join(format!(".norte-owner-probe-{}", std::process::id()));
    let owned = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.uid() == md.uid());
    let _ = std::fs::remove_file(&probe);
    if !owned {
        return Err(DaemonError::InsecureDir {
            reason: "the socket's dir belongs to another user",
        });
    }
    Ok(DirIdentity {
        dev: md.dev(),
        ino: md.ino(),
    })
}

/// `true` if `path` is the default fallback under `/tmp/norte-<uid>/…` (only
/// when the path was taken by default (`defaulted=true`), with no
/// `--socket`). The fallback is squattable (#34.1): an actionable message
/// asks to set `XDG_RUNTIME_DIR` or pass `--socket`, instead of the opaque
/// `InsecureDir`.
#[cfg(unix)]
fn is_default_tmp_fallback(path: &Path, defaulted: bool) -> bool {
    defaulted
        && path.parent().is_some_and(|dir| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("norte-"))
                && dir.parent() == Some(Path::new("/tmp"))
        })
}

/// Is this an admissible agent session id? Closed charset
/// `[A-Za-z0-9._-]`, 1..=64: the id travels to the journal, tracing and
/// approval modals of ALL frontends — a closed charset at the boundary is
/// worth more than trusting every consumer to mask it (which they must do
/// too, defense in depth). `.`/`..` are rejected up front (security M3-4
/// MINOR-4): if a session ever derives a file (M3-5 audit export), it will
/// never be traversal.
fn valid_agent_session(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Is this peer admitted? Only the SAME uid (spec §17.6). Root does NOT get
/// in: a user daemon is not a surface for privileged processes.
#[cfg(unix)]
pub(super) fn peer_allowed(peer_uid: u32, daemon_uid: u32) -> bool {
    peer_uid == daemon_uid
}

fn spawn_connection(stream: transport::Stream, shared: Arc<Shared>) {
    // ROOT: each `rpc` of this connection is a root (ADR 0127). Inheriting,
    // they would all hang off `run`, the span of the daemon's whole life.
    crate::blocking::spawn_root(async move {
        // Auth BEFORE reading a single byte (ADR 0011).
        if let Err(reason) = transport::admit(&stream, &shared.owner) {
            tracing::warn!(%reason, "connection rejected");
            return;
        }
        shared.connections.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = serve_connection(stream, &shared).await {
            tracing::debug!(error = %e, "connection ended with an error");
        }
        // With the last connection gone there is nobody left to serve, and
        // what it just left set should not wait for the next tick.
        if shared.connections.fetch_sub(1, Ordering::SeqCst) == 1 {
            shared.session_flush.notify_one();
        }
    });
}

/// A LIVE paginated listing retained by the daemon between pages (ADR 0017):
/// the lazy, undrained `EntryStream`, the path that opened it (to validate
/// that a `cursor` matches THIS listing) and when it was last used
/// (TTL/LRU). Dropping the struct = dropping the stream = cooperative
/// cancellation down to the provider's producer (rule 3).
struct OpenListing {
    path: norte_proto::VPath,
    stream: norte_vfs::EntryStream,
    last_used: std::time::Instant,
    /// Skipped from the container's index (#93), captured when OPENING the
    /// listing: every page repeats it (the client can latch onto any of
    /// them; the total is per-container, not per-page).
    skipped: Option<u64>,
    /// The anchor of the listed directory (#295), captured on OPEN for the
    /// same reason as `skipped`: a page is not a different directory, and
    /// the client can latch onto any of them.
    dir_anchor: Option<norte_proto::DirAnchor>,
    /// Attrs request RESOLVED when opening the listing (#108 block 2): the
    /// stream was born with it, so continuations reuse it for the emission
    /// belt (a continuation's `attrs` are ignored).
    attrs: norte_vfs::AttrRequest,
    /// Decrements the GLOBAL counter when the listing is released
    /// (remove/evict/sweep/connection death): RAII accounting, no scattered
    /// decrements (rust-reviewer M1).
    _guard: ListingGuard,
}

/// RAII guard of the global counter of retained listings.
struct ListingGuard {
    global: Arc<AtomicUsize>,
}

impl Drop for ListingGuard {
    fn drop(&mut self) {
        self.global.fetch_sub(1, Ordering::SeqCst);
    }
}

/// PER-CONNECTION state: the handshake's `initialized` plus the retained
/// paginated listings. Local to [`serve_connection`] — dropped on ALL exit
/// paths, so the streams die with the connection. Dispatch is serial
/// (awaited inline), with no concurrency: `&mut` is enough.
struct ConnState {
    initialized: bool,
    /// The actor under which EVERY mutation of this connection is journaled
    /// and policy-evaluated (M3-3b). `User` by default (human frontend, no
    /// sandbox); becomes `Agent { session }` if the `initialize` carries
    /// `agent_session`. Set ONLY by the server at the handshake: a client can
    /// never declare itself `User` any other way.
    actor: crate::journal::Actor,
    listings: HashMap<u64, OpenListing>,
    next_listing_id: u64,
    /// Scope `request_id`s THIS connection left pending (M3-3b): when the
    /// connection dies they are removed from `Shared`'s global map — an
    /// ungranted request does not outlive its requester (without this, an
    /// agent that asks and leaves pins a slot forever). Capped at
    /// [`MAX_PENDING_SCOPE_PER_CONN`]: one session does not monopolize the
    /// global channel.
    pending_scope_ids: Vec<u64>,
}

impl ConnState {
    fn new() -> Self {
        Self {
            initialized: false,
            actor: crate::journal::Actor::User,
            listings: HashMap::new(),
            next_listing_id: 0,
            pending_scope_ids: Vec::new(),
        }
    }

    /// Discards listings not continued for more than `ttl` (lazy sweep).
    fn sweep_expired(&mut self, ttl: Duration) {
        let now = std::time::Instant::now();
        self.listings
            .retain(|_, l| now.duration_since(l.last_used) < ttl);
    }

    /// Evicts the least-recently-used (LRU) listing if the map is at the cap:
    /// opening the (N+1)th must not grow without limit.
    fn evict_if_full(&mut self) {
        if self.listings.len() < MAX_OPEN_LISTINGS {
            return;
        }
        if let Some((&victim, _)) = self.listings.iter().min_by_key(|(_, l)| l.last_used) {
            self.listings.remove(&victim);
        }
    }
}

/// Result of draining one page of an `EntryStream`.
enum Drained {
    /// The stream has MORE: retained for the next page.
    More,
    /// The stream ran out: no `next_cursor`.
    Done,
}

/// Drains up to `cap` entries (or all if `cap` is `None`) into `out`. An
/// `Err` from the stream propagates (the listing is discarded above).
async fn drain_page(
    stream: &mut norte_vfs::EntryStream,
    cap: Option<usize>,
    out: &mut Vec<norte_proto::Entry>,
) -> Result<Drained, norte_proto::Error> {
    loop {
        if let Some(c) = cap
            && out.len() >= c
        {
            return Ok(Drained::More);
        }
        match stream.next().await {
            Some(item) => out.push(item?),
            None => return Ok(Drained::Done),
        }
    }
}

/// Validates the wire's attrs request (#108 block 2, ADR 0039 §4): a
/// malformed id or more than `ATTRS_MAX_REQUEST` (the deserializer
/// materializes 16+1 as a witness) = `-32602`. Asking for a VALID but unknown
/// id is NOT an error (it comes back absent — a client with a stale
/// catalogue degrades).
fn validate_attr_request(ids: &[String]) -> Result<(), RpcError> {
    if ids.len() > norte_proto::ATTRS_MAX_REQUEST {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!(
                "attrs: at most {} ids per call",
                norte_proto::ATTRS_MAX_REQUEST
            ),
        ));
    }
    if let Some(bad) = ids.iter().find(|id| !norte_proto::is_valid_attr_id(id)) {
        // `escape_debug`: the invalid id is hostile input — never raw in an
        // error message (controls, RTL, invisibles).
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("attrs: malformed id \"{}\"", bad.escape_debug()),
        ));
    }
    Ok(())
}

/// Validates `p.attrs` and crosses it with `path`'s provider catalogue:
/// returns the request that really travels to the provider. A valid id that
/// is NOT advertised falls out here — the daemon only forwards ids the
/// provider advertises, never invents cells (ADR 0039 §1).
///
/// Two provider resolutions (catalogue here, `list_with`/`stat_with` in the
/// handler): if the connection gets remapped between the two, the filtered
/// request may not match the new catalogue — it degrades to ABSENCE, which
/// is contract-legal. Do not "fix" this with a single lookup under a lock.
async fn resolve_attr_request(
    ids: &[String],
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<norte_vfs::AttrRequest, RpcError> {
    validate_attr_request(ids)?;
    if ids.is_empty() {
        return Ok(norte_vfs::AttrRequest::default());
    }
    let advertised = shared
        .engine
        .attr_catalog(path)
        .await
        .map_err(RpcError::from)?;
    Ok(norte_vfs::AttrRequest::sanitized(
        ids.iter()
            .filter(|id| advertised.iter().any(|a| &a.id == *id))
            .cloned(),
    ))
}

/// Emission belt (ADR 0039 §5): delegates to `AttrRequest`'s shared belt —
/// the same filter the embedded backend applies, so no path (wire or
/// in-process) emits unrequested ids, over-cap values, or `Unknown`.
fn enforce_attr_caps(entry: &mut norte_proto::Entry, allowed: &norte_vfs::AttrRequest) {
    allowed.retain_conforming(entry);
}

/// The `fs.stat` handler (#108 block 2): validates the attrs request,
/// crosses it with what is advertised, materializes and applies the
/// emission belt. The `read_gate` (#80) is applied by the dispatch's arm.
async fn handle_fs_stat(
    p: methods::FsStatParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let request = resolve_attr_request(&p.attrs, &p.path, shared).await?;
    let mut entry = shared
        .engine
        .stat_with(
            &p.path,
            &norte_vfs::ListOptions {
                attrs: request.clone(),
            },
        )
        .await
        .map_err(RpcError::from)?;
    enforce_attr_caps(&mut entry, &request);
    to_value(&methods::FsStatResult { entry })
}

/// The `fs.list` handler with cursor pagination (ADR 0017). ADR 0004 clause:
/// with no `cursor` AND no `limit`, drains the WHOLE listing with
/// `next_cursor: null` (a 0.7 client receives exactly what it used to).
///
/// Attrs (#108 block 2): the request is validated and resolved ON OPEN; a
/// cursor continuation IGNORES `p.attrs` (the retained stream was born with
/// its options — resending them changes nothing, though the validation does
/// run).
#[tracing::instrument(level = "debug", skip_all, fields(path = %p.path.display_lossy(), paged = p.cursor.is_some()))]
async fn handle_fs_list(
    p: methods::FsListParams,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // The read gate (#80) is applied by the dispatch's ARM, outside this
    // instrumented span (it does not leak the path into the trace on a
    // denial).

    // Lazy sweep before touching the map (in addition to the periodic one).
    conn.sweep_expired(shared.listing_ttl);

    // `limit == 0` would be an empty page in a loop: a params error.
    if p.limit == Some(0) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "limit must be >= 1",
        ));
    }
    let cap = p.limit.map(|l| l.min(methods::FS_LIST_MAX_PAGE) as usize);
    let now = std::time::Instant::now();
    let mut entries = Vec::new();

    // Attrs validation ALWAYS runs (even with a cursor: a malformed id is
    // -32602 even if the continuation does not use it).
    validate_attr_request(&p.attrs)?;

    // Continuation: the cursor is the opaque id of a retained listing.
    if let Some(cur) = &p.cursor {
        return continue_listing(cur, &p.path, cap, now, conn, entries).await;
    }

    // NEW listing (no cursor): only advertised ids travel to the provider.
    let request = resolve_attr_request(&p.attrs, &p.path, shared).await?;
    let opt = norte_vfs::ListOptions {
        attrs: request.clone(),
    };
    let mut stream = shared
        .engine
        .list_with(&p.path, &opt)
        .await
        .map_err(RpcError::from)?;
    // Skipped from the container (#93), captured ONCE on open (the archive
    // index is already warm after the `list`). An error here does NOT bring
    // down a listing that already opened: it degrades to `None` (= unknown,
    // as before) — but WITH a trace (the point of #93 is not to silence
    // incomplete listings).
    let skipped = shared
        .engine
        .list_skipped(&p.path)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "list_skipped failed; skipped = unknown");
            None
        });
    // The directory's anchor (#295), also captured ONCE on open. A failure
    // here does NOT bring down the listing: it degrades to `None`, which is
    // what a provider that cannot give identity says, and then the client
    // sends no anchor and the write behaves as in 0.53. Going the other way
    // — refusing to list because it cannot be anchored — would leave a whole
    // bucket unlisted over a check that destination cannot give.
    let dir_anchor = shared.engine.dir_anchor(&p.path).await.unwrap_or_else(|e| {
        tracing::debug!(error = %e, "could not anchor the listed directory (#295)");
        None
    });
    match drain_page(&mut stream, cap, &mut entries).await {
        Ok(Drained::Done) => {
            for e in &mut entries {
                enforce_attr_caps(e, &request);
            }
            to_value(&methods::FsListResult {
                entries,
                next_cursor: None,
                skipped,
                dir_anchor,
            })
        }
        Ok(Drained::More) => {
            // GLOBAL pressure (M1): above the cap, nothing is retained — the
            // rest is drained INLINE and returned complete (frees the
            // producer's blocking thread instantly). Degrades to a
            // full-listing, never exhausts the pool nor truncates.
            if shared.open_listings.load(Ordering::SeqCst) >= GLOBAL_MAX_LISTINGS {
                match drain_page(&mut stream, None, &mut entries).await {
                    Ok(_) => {
                        for e in &mut entries {
                            enforce_attr_caps(e, &request);
                        }
                        return to_value(&methods::FsListResult {
                            entries,
                            next_cursor: None,
                            skipped,
                            dir_anchor,
                        });
                    }
                    Err(e) => return Err(RpcError::from(e)),
                }
            }
            conn.evict_if_full();
            shared.open_listings.fetch_add(1, Ordering::SeqCst);
            let id = conn.next_listing_id;
            conn.next_listing_id += 1;
            for e in &mut entries {
                enforce_attr_caps(e, &request);
            }
            conn.listings.insert(
                id,
                OpenListing {
                    path: p.path,
                    stream,
                    last_used: now,
                    skipped,
                    dir_anchor: dir_anchor.clone(),
                    attrs: request,
                    _guard: ListingGuard {
                        global: Arc::clone(&shared.open_listings),
                    },
                },
            );
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
                skipped,
                dir_anchor,
            })
        }
        Err(e) => Err(RpcError::from(e)),
    }
}

/// Continuation of a retained listing (the `cursor` arm of
/// [`handle_fs_list`], split out by size): validates cursor↔path, drains the
/// page and decides whether to retain (More) or close (Done/error). The
/// listing's open-time `skipped` is repeated on every page (#93).
async fn continue_listing(
    cursor: &str,
    path: &norte_proto::VPath,
    cap: Option<usize>,
    now: std::time::Instant,
    conn: &mut ConnState,
    mut entries: Vec<norte_proto::Entry>,
) -> Result<serde_json::Value, RpcError> {
    // Non-numeric or unknown cursor = expired (the client restarts).
    let id: u64 = cursor.parse().map_err(|_| cursor_expired())?;
    let (drained, skipped, dir_anchor) = {
        let listing = conn.listings.get_mut(&id).ok_or_else(cursor_expired)?;
        if listing.path != *path {
            return Err(RpcError::protocol(
                codes::INVALID_PARAMS,
                "cursor does not belong to this path",
            ));
        }
        let skipped = listing.skipped;
        // The anchor from OPEN, repeated on every page (#295): asking it
        // again here would answer for the directory as it is NOW, which is
        // exactly what the anchor exists to avoid confusing with the one from
        // back then.
        let dir_anchor = listing.dir_anchor.clone();
        let drained = drain_page(&mut listing.stream, cap, &mut entries).await;
        // Emission belt with the OPEN-time request (#108 block 2).
        let allowed = listing.attrs.clone();
        for e in &mut entries {
            enforce_attr_caps(e, &allowed);
        }
        (drained, skipped, dir_anchor)
    };
    match drained {
        Ok(Drained::More) => {
            // Kept under the SAME id (the client reuses the cursor).
            if let Some(l) = conn.listings.get_mut(&id) {
                l.last_used = now;
            }
            to_value(&methods::FsListResult {
                entries,
                next_cursor: Some(id.to_string()),
                skipped,
                dir_anchor,
            })
        }
        Ok(Drained::Done) => {
            conn.listings.remove(&id);
            to_value(&methods::FsListResult {
                entries,
                next_cursor: None,
                skipped,
                dir_anchor,
            })
        }
        Err(e) => {
            conn.listings.remove(&id);
            Err(RpcError::from(e))
        }
    }
}

/// `RpcError` for an invalid/expired pagination cursor (ADR 0017): carries the
/// [`Error::CursorExpired`](norte_proto::Error::CursorExpired) taxonomy in
/// `data`, so the client distinguishes it and restarts the listing.
fn cursor_expired() -> RpcError {
    RpcError::from(norte_proto::Error::CursorExpired)
}

async fn serve_connection(stream: transport::Stream, shared: &Arc<Shared>) -> std::io::Result<()> {
    let (reader, mut writer) = transport::split(stream);

    // Every write (responses AND broadcasts) goes out through a single
    // BOUNDED channel: never two interleaved frames and never unbounded
    // memory for a client that does not read (security-reviewer M1).
    let (tx, mut rx) = mpsc::channel::<Arc<[u8]>>(OUTBOX_FRAMES);
    let writer_task = crate::blocking::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    let conn_id = shared.next_conn.fetch_add(1, Ordering::SeqCst) as u64;
    // #64: READING lives in its own task feeding a bounded inbox — during a
    // SUSPENDED dispatch (a policy Ask) the socket keeps being read, and an
    // EOF/reset from the peer cancels `peer_gone`: the in-flight dispatch is
    // dropped and its RAII guards clean up (the pending approval stops being
    // a zombie until the TTL). Dispatch stays SERIAL (frame order = execution
    // order); only WHO reads changes.
    let peer_gone = CancellationToken::new();
    let (inbox_tx, mut inbox_rx) = mpsc::channel::<serde_json::Value>(INBOX_FRAMES);
    let reader_task = crate::blocking::spawn(read_frames(
        reader,
        tx.clone(),
        inbox_tx,
        peer_gone.clone(),
        shared.shutdown.clone(),
    ));

    let mut conn = ConnState::new();
    // #72: cancellation tokens of cancellable in-flight requests.
    let inflight_cancel: InflightCancel = Arc::default();
    // Reaping expired paginated listings on a live-but-silent connection (in
    // addition to the lazy sweep on every fs.list). NOTE: between dispatches
    // — a suspended dispatch keeps holding up the tick (mitigated by the next
    // fs.list's lazy sweep, M3-3b's MINOR-3 note).
    let mut sweep = tokio::time::interval(LISTING_SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // #72: frames read from the inbox WHILE a dispatch is in flight that are
    // NOT an `rpc.cancel` of the in-flight request are buffered here and
    // processed AFTER the outcome — dispatch stays SERIAL (frame order =
    // execution order). An MCP agent does not pipeline, so in practice the
    // only frame during an Ask is the cancel or the EOF.
    let mut pending_frames: std::collections::VecDeque<serde_json::Value> =
        std::collections::VecDeque::new();
    let result: std::io::Result<()> = loop {
        // Next frame: first the local buffer, then the inbox (with
        // shutdown/sweep handled ONLY between dispatches, as before #72).
        let value = if let Some(v) = pending_frames.pop_front() {
            v
        } else {
            tokio::select! {
                msg = inbox_rx.recv() => match msg {
                    None => break Ok(()),
                    Some(v) => v,
                },
                () = shared.shutdown.cancelled() => break Ok(()),
                _ = sweep.tick() => {
                    conn.sweep_expired(shared.listing_ttl);
                    continue;
                }
            }
        };
        // Dispatches while watching the inbox in parallel (#72 + #64).
        let dispatch = handle_value(value, conn_id, &tx, &mut conn, shared, &inflight_cancel);
        tokio::pin!(dispatch);
        let peer_died = loop {
            tokio::select! {
                biased;
                // A dispatch that completes, completes (including the
                // Cancelled response of a withdrawal): it wins over the
                // peer's death.
                () = &mut dispatch => break false,
                // A SUSPENDED dispatch dies with its requester (#64).
                () = peer_gone.cancelled() => break true,
                // Frames arriving while this dispatch is still in flight —
                // ONLY while the deferred buffer is not at the cap: at the
                // cap this arm is disabled and the reader goes back to
                // applying backpressure on the socket (anti-unbounded-memory
                // cap, security-reviewer MAJOR #72). A cancel left behind a
                // flood does not get read (the Ask will expire by TTL) —
                // acceptable: pipelining under an unapproved Ask is the
                // semi-hostile case.
                msg = inbox_rx.recv(), if pending_frames.len() < MAX_DEFERRED_FRAMES => match msg {
                    // The reader finished (EOF/shutdown): DROPS the in-flight
                    // dispatch (its guards clean up) and closes via the
                    // common path. In practice `peer_gone` (biased, above)
                    // wins first; this arm is defensive — never wait for a
                    // suspended dispatch here (it would hang until the TTL).
                    None => break true,
                    Some(frame) => {
                        if let Some(id) = rpc_cancel_id(&frame) {
                            // rpc.cancel: fires that request's token if it is
                            // in flight. Does NOT break the connection (unlike
                            // peer_gone). Unknown/already-resolved id = no-op.
                            if let Some(tok) = inflight_cancel
                                .lock()
                                .expect("inflight_cancel lock is sound")
                                .get(&id)
                            {
                                tok.cancel();
                            }
                        } else {
                            // Any other frame: processed AFTER the outcome
                            // (serial dispatch).
                            pending_frames.push_back(frame);
                        }
                    }
                }
            }
        };
        if peer_died {
            break Ok(());
        }
    };

    // Common cleanup for ALL exit paths. Remove OUR subscription before
    // waiting for the writer: it is the sender's other owner — without this,
    // deadlock (the writer drains until EVERYONE dies).
    shared
        .subscribers
        .lock()
        .expect("subscribers lock is sound")
        .remove(&conn_id);
    // The scope requests THIS connection left pending die with it: an
    // ungranted request must not outlive its requester (anti-leak of the
    // global channel, M3-3b). `remove` of an id already granted is a benign
    // no-op.
    if !conn.pending_scope_ids.is_empty() {
        let mut pending = shared
            .pending_scope
            .lock()
            .expect("pending_scope lock is sound");
        for id in &conn.pending_scope_ids {
            pending.remove(id);
        }
    }
    // The owner of the UI session RELEASES it on leaving: without this, a
    // client that dies leaves the screen hostage and the next terminal runs
    // detached forever. Releasing someone else's is a no-op (evicts nobody).
    shared.ui_session.release(conn_id);
    drop_sync_plans(shared, conn_id).await;
    drop(tx);
    // Closing the inbox ends the reader if it is still alive (its `send`
    // fails).
    drop(inbox_rx);
    let _ = writer_task.await;
    // A READ error (e.g. ECONNRESET) propagates as before.
    match reader_task.await {
        Ok(read_result) => result.and(read_result),
        Err(_) => result,
    }
}

/// A connection's READ loop (#64): decodes frames and queues them for
/// dispatch. Stays alive even while the dispatch is suspended; on ANY exit
/// (EOF, reset, hostile framing, shutdown) the `DropGuard` cancels
/// `peer_gone` and the in-flight dispatch is aborted. Note: a peer doing a
/// half-close (shutdown of the write side while still awaiting the response)
/// is treated as dead — no norte client does this.
async fn read_frames(
    mut reader: transport::Reader,
    tx: mpsc::Sender<Arc<[u8]>>,
    inbox: mpsc::Sender<serde_json::Value>,
    peer_gone: CancellationToken,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let _gone = peer_gone.drop_guard();
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut parse_errors = 0u32;
    loop {
        let n = tokio::select! {
            r = reader.read(&mut buf) => r?,
            () = shutdown.cancelled() => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        if decoder.push(&buf[..n]).is_err() {
            let resp = Response::err(
                None,
                RpcError::protocol(codes::PARSE_ERROR, "frame too large"),
            );
            send(&tx, &resp);
            return Ok(());
        }
        while let Some(frame) = decoder.next_frame() {
            // Broken JSON = -32700; valid JSON that is not an envelope =
            // -32600 (protocol-guardian M2).
            let value: serde_json::Value = match serde_json::from_slice(&frame) {
                Ok(v) => v,
                Err(e) => {
                    parse_errors += 1;
                    if parse_errors > MAX_PARSE_ERRORS {
                        return Ok(());
                    }
                    let resp = Response::err(
                        None,
                        RpcError::protocol(codes::PARSE_ERROR, format!("invalid JSON: {e}")),
                    );
                    send(&tx, &resp);
                    continue;
                }
            };
            parse_errors = 0;
            // Backpressure: with a full inbox the reader waits here (the same
            // throttling inline dispatch used to give); detecting EOF during
            // an Ask requires the peer to have no more than INBOX_FRAMES
            // frames in flight — norte's clients are request/response.
            if inbox.send(value).await.is_err() {
                // The dispatch died (shutdown): nothing to queue.
                return Ok(());
            }
        }
    }
}

/// Cancellation tokens of cancellable in-flight requests (#72), by JSON-RPC
/// id. Lives OUTSIDE `ConnState` (which `handle_value` takes as `&mut`): the
/// inner loop of `serve_connection` consults it IN PARALLEL to an in-flight
/// dispatch to fire an `rpc.cancel`'s token. `Arc<Mutex<…>>` because there
/// are two concurrent owners: the loop (fires) and `handle_value`
/// (registers/removes).
type InflightCancel = Arc<Mutex<HashMap<RequestId, CancellationToken>>>;

/// RAII guard of the in-flight token (#72): removes the id from the map on
/// ANY dispatch exit (normal response, cancel withdrawal, shutdown, or drop
/// from the peer's death) — never an orphaned token that a late `rpc.cancel`
/// would fire onto an already-resolved request.
struct InflightCancelGuard {
    map: InflightCancel,
    id: RequestId,
}

impl Drop for InflightCancelGuard {
    fn drop(&mut self) {
        self.map
            .lock()
            .expect("inflight_cancel lock is sound")
            .remove(&self.id);
    }
}

/// Extracts the `id` of an already-parsed `rpc.cancel` frame (#72), or `None`
/// if the frame is not a well-formed `rpc.cancel`. Applied to frames read
/// from the inbox WHILE a dispatch is in flight — structural classification
/// over the `Value`, without deserializing the whole envelope. `rpc.cancel`
/// is specified ONLY as a notification (no envelope id); a Request-shaped
/// frame with that method would still be consumed here (benign: never
/// answered), but no client emits it that way.
fn rpc_cancel_id(value: &serde_json::Value) -> Option<RequestId> {
    if value.get("method").and_then(serde_json::Value::as_str) != Some(methods::RPC_CANCEL) {
        return None;
    }
    let id = value.get("params")?.get("id")?;
    serde_json::from_value::<RequestId>(id.clone()).ok()
}

/// Handles ONE already-parsed JSON message from the connection.
/// Classification is structural so that an illegally-typed id NEVER dies
/// silently as a notification (guardian M3).
async fn handle_value(
    value: serde_json::Value,
    conn_id: u64,
    tx: &mpsc::Sender<Arc<[u8]>>,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
    inflight_cancel: &InflightCancel,
) {
    match classify(&value) {
        MessageKind::Request => {
            let req: Request = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    let resp = Response::err(
                        None,
                        RpcError::protocol(
                            codes::INVALID_REQUEST,
                            format!("invalid request envelope: {e}"),
                        ),
                    );
                    send(tx, &resp);
                    return;
                }
            };
            let id = req.id.clone();
            let was_initialized = conn.initialized;
            // #72: fs.copy/move/delete can be suspended in a policy Ask. A
            // token is registered by its id so that an `rpc.cancel` (read by
            // serve_connection's loop in parallel) withdraws the Ask by
            // dropping this dispatch — the gate dies PRE-effect (its
            // PendingGuard cleans up policy.pending) and NEVER approves
            // (fail-closed by construction: dropping a future cannot return
            // Approved).
            let cancelable = matches!(
                req.method.as_str(),
                methods::FS_COPY
                    | methods::FS_MOVE
                    | methods::FS_DELETE
                    | methods::FS_MKDIR
                    | methods::FS_CREATE
                    // #314: gates the WHOLE batch before touching anything, so
                    // it can be left suspended in an Ask just like a mkdir.
                    | methods::FS_SET_MODE
                    | methods::AI_RENAME_PLAN
                    | methods::INDEX_SEARCH_SEMANTIC
                    // 0.36.0: `fs.rename_batch` gates the WHOLE batch before
                    // reserving anything, so it can be left suspended in an
                    // Ask exactly like an fs.move; and `fs.rename_batch_plan`
                    // lists and plans a directory inside the dispatch, like
                    // `ai.rename_plan`. Withdrawing either is safe by
                    // construction: the gate dies PRE-effect, and there is no
                    // `.await` between the engine's submit and the register
                    // (#64).
                    //
                    // Withdrawing withdraws the RESPONSE, not the work: the
                    // planner already runs in `spawn_blocking` and dropping
                    // its `JoinHandle` does not stop it — it finishes on its
                    // own, bounded by `RENAME_BATCH_MAX_LISTING` and the pair
                    // cap. The client stops waiting; the CPU already spent
                    // does not come back.
                    | methods::FS_RENAME_BATCH
                    | methods::FS_RENAME_BATCH_PLAN
                    // 0.40.0: `sync.apply` gates BOTH roots of the plan before
                    // writing a single byte, so it suspends in an `ask` just
                    // like an fs.copy — and its wait is the most expensive of
                    // all, because the client has nothing to do in the
                    // meantime. Withdrawing it is safe: the gate dies
                    // PRE-effect, and the right to apply the plan — which
                    // `Spool::open` already charged for — is returned by
                    // `ApplyClaim`'s `Drop` in the engine, which exists
                    // exactly for this path.
                    | methods::SYNC_APPLY
                    // #248: pure READS, for a different reason. Here there is
                    // no effect to leave half-done — a read writes nothing, so
                    // dropping its dispatch cannot leave a trace — what gets
                    // freed is the CONNECTION. `serve_connection` dispatches
                    // serially, so an `fs.list` the client had abandoned (the
                    // startup's five-second budget, #235) kept running
                    // against a hung provider and EVERY following request
                    // waited behind it, dying one by one at its 30s
                    // `CALL_TIMEOUT`. The client already sends the
                    // `rpc.cancel` (`call_timed_guarded`'s guard); without
                    // this arm nobody was listening for it.
                    | methods::FS_LIST
                    | methods::FS_STAT
                    | methods::FS_READ
                    | methods::FS_CAPABILITIES
            );
            let response = if cancelable {
                let cancel = CancellationToken::new();
                inflight_cancel
                    .lock()
                    .expect("inflight_cancel lock is sound")
                    .insert(id.clone(), cancel.clone());
                let _cancel_guard = InflightCancelGuard {
                    map: Arc::clone(inflight_cancel),
                    id: id.clone(),
                };
                tokio::select! {
                    biased;
                    // An op that COMPLETES (approved, or rejected by policy)
                    // wins over a simultaneous cancel: what is already
                    // resolved is not withdrawn.
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
                    // The agent withdrew the request suspended in the Ask:
                    // clean state guaranteed (pre-effect gate), no policy
                    // leak.
                    () = cancel.cancelled() => Err(RpcError::from(norte_proto::Error::Cancelled)),
                }
            } else {
                // Dispatch respects shutdown (rule 3): a giant fs.list does
                // not hold up shutdown.
                tokio::select! {
                    r = dispatch(req, conn_id, conn, shared) => r,
                    () = shared.shutdown.cancelled() => Err(RpcError::protocol(
                        codes::INTERNAL_ERROR,
                        "daemon shutting down",
                    )),
                }
            };
            // The broadcast subscription is born WITH the handshake
            // (security-reviewer note): before initialize nobody receives
            // anyone else's progress.
            if !was_initialized && conn.initialized {
                shared
                    .subscribers
                    .lock()
                    .expect("subscribers lock is sound")
                    .insert(
                        conn_id,
                        Subscriber {
                            tx: tx.clone(),
                            // The actor was fixed server-side by THIS initialize.
                            actor: conn.actor.clone(),
                        },
                    );
            }
            send(tx, &Response::from_outcome(id, response));
        }
        // Client notifications (none defined yet; JSON-RPC forbids answering
        // them) and spurious responses: ignored.
        MessageKind::Notification | MessageKind::Response => {}
        MessageKind::Invalid => {
            let resp = Response::err(
                None,
                RpcError::protocol(codes::INVALID_REQUEST, "not a JSON-RPC message"),
            );
            send(tx, &resp);
        }
    }
}

/// Takes away the sync PLANS a closing connection left retained (ADR 0049,
/// third of the spool's four deaths).
///
/// An approvable plan belongs to the connection that produced it, so without
/// it nobody can apply it; and what would remain on disk is a relative
/// listing of two whole trees under the state directory.
///
/// The RIGHT to apply it is forgotten in memory even if the deletion fails,
/// so a failure here leaves garbage on disk and not a live plan: it is
/// logged and does not break closing the connection.
async fn drop_sync_plans(shared: &Arc<Shared>, conn_id: u64) {
    let Some(spool) = shared.engine.spool() else {
        return;
    };
    match spool.drop_connection(conn_id).await {
        Ok(report) if report.is_clean() => {}
        Ok(report) => tracing::warn!(
            conn = conn_id,
            failed = report.failed,
            "sync plans that could not be deleted on connection close"
        ),
        Err(e) => tracing::warn!(conn = conn_id, error = %e, "sync plan sweep"),
    }
}

/// Queues a frame on the connection's outbox. `try_send`: if the client does
/// not drain (full outbox), the frame is lost and the connection will die on
/// its next read — never unbounded accumulation.
fn send<T: serde::Serialize>(tx: &mpsc::Sender<Arc<[u8]>>, msg: &T) {
    if let Ok(frame) = encode_frame(msg) {
        let _ = tx.try_send(Arc::from(frame.into_boxed_slice()));
    }
}

/// Sugar: builds a dispatch's Response.
trait FromOutcome {
    fn from_outcome(id: RequestId, out: Result<serde_json::Value, RpcError>) -> Response;
}

impl FromOutcome for Response {
    fn from_outcome(id: RequestId, out: Result<serde_json::Value, RpcError>) -> Response {
        match out {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(Some(id), e),
        }
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<serde_json::Value>,
) -> Result<T, RpcError> {
    serde_json::from_value(params.unwrap_or(serde_json::Value::Null))
        .map_err(|e| RpcError::protocol(codes::INVALID_PARAMS, format!("invalid params: {e}")))
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, RpcError> {
    serde_json::to_value(v)
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("serialization: {e}")))
}

// The dispatch table grows one arm per new method (G3b added two): it is a
// flat list, not nested logic — splitting it into sub-functions by block of
// methods would not reduce its real complexity, only hide it behind
// indirection. Same criterion as other large dispatchers in the tree (see
// `dispatch_fs_task`).
#[expect(
    clippy::too_many_lines,
    reason = "flat method→handler table; see `dispatch_fs_task`"
)]
// ADR 0127: everything registered while serving a request — and every task it
// opens, because `Scheduler::submit` hangs its `task` off here — carries the
// connection, the JSON-RPC `id` the client already knows, and the method.
// Never the params: routes go there. The method and the id are chosen by the
// PEER, so they go through `peer_field`: bounded and escaped, so a `\n`
// cannot fabricate a line in the text log nor a 16 MiB id repeat on every
// line of its tasks.
#[tracing::instrument(
    name = "rpc",
    skip_all,
    fields(
        conn_id = conn_id,
        method = %peer_field(&req.method),
        req_id = %id_for_log(&req.id),
    )
)]
async fn dispatch(
    req: Request,
    conn_id: u64,
    conn: &mut ConnState,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !conn.initialized && req.method != methods::INITIALIZE {
        return Err(RpcError::protocol(
            codes::NOT_INITIALIZED,
            "initialize required first",
        ));
    }
    match req.method.as_str() {
        methods::INITIALIZE => {
            // Repeating it is a protocol error (like LSP): renegotiating
            // mid-session means nothing.
            if conn.initialized {
                return Err(RpcError::protocol(
                    codes::INVALID_REQUEST,
                    "already initialized",
                ));
            }
            let p: methods::InitializeParams = parse_params(req.params)?;
            if !methods::version_compatible(methods::PROTOCOL_VERSION, &p.protocol_version) {
                // OUR OWN code (protocol-guardian B1): the upgrade signal is
                // distinguished by code, never by message.
                return Err(RpcError::protocol(
                    codes::VERSION_MISMATCH,
                    format!(
                        "incompatible protocol version: server {}, client {} (N and N-1 accepted)",
                        methods::PROTOCOL_VERSION,
                        p.protocol_version
                    ),
                ));
            }
            if !p.encodings.is_empty() && !p.encodings.iter().any(|e| e == "json") {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    "only supported encoding: json",
                ));
            }
            // The server binds the actor to the connection (M3 debt from
            // 3a): a connection with `agent_session` IS an agent session and
            // gets sandboxed; without it, it is `User` (human). It cannot be
            // declared the other way around. The id is VALIDATED fail-closed
            // (encoding-auditor H1 from M3-3b): it goes to the journal, logs
            // and approval UIs — never an injection vector of
            // controls/bidi chosen by the agent.
            if let Some(session) = p.agent_session {
                if !valid_agent_session(&session) {
                    return Err(RpcError::protocol(
                        codes::INVALID_PARAMS,
                        "agent_session must be 1..=64 chars of [A-Za-z0-9._-]",
                    ));
                }
                conn.actor = crate::journal::Actor::Agent { session };
            }
            conn.initialized = true;
            to_value(&methods::InitializeResult {
                server_info: methods::ServerInfo {
                    name: "norte-core".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
            })
        }
        methods::DAEMON_SHUTDOWN => handle_daemon_shutdown(&conn.actor, req.params, shared),
        methods::SESSION_GET => handle_session_get(&conn.actor, conn_id, shared),
        methods::SESSION_PUT => {
            let p: methods::SessionPutParams = parse_params(req.params)?;
            handle_session_put(&conn.actor, conn_id, p, shared)
        }
        // No params, like `session.get`: who releases is said by the
        // CONNECTION, and an id that traveled would be an id anyone could
        // send.
        methods::SESSION_RELEASE => handle_session_release(&conn.actor, conn_id, shared),
        // fs.list lives HERE (not in dispatch_fs_task): it needs ConnState to
        // retain the paginated stream between pages (ADR 0017).
        methods::FS_LIST => {
            let p: methods::FsListParams = parse_params(req.params)?;
            // Read gate (#80) HERE, OUTSIDE `handle_fs_list`'s instrumented
            // span (which carries `path` in its fields): a denial must not
            // leak the path into the trace. The START is enough — a cursor
            // continuation is for the SAME already-validated path.
            read_gate(&conn.actor, &p.path, shared)?;
            handle_fs_list(p, conn, shared).await
        }
        // policy.* with a human round-trip (M3-3b): scope request/grant. They
        // live HERE because they tie the operation to the connection's ACTOR
        // (server-side).
        methods::POLICY_REQUEST_SCOPE => {
            let p: methods::RequestScopeParams = parse_params(req.params)?;
            handle_request_scope(conn, p, shared)
        }
        methods::POLICY_GRANT_SCOPE => {
            let p: methods::GrantScopeParams = parse_params(req.params)?;
            handle_grant_scope(&conn.actor, &p, shared)
        }
        methods::POLICY_DECIDE => {
            let p: methods::PolicyDecideParams = parse_params(req.params)?;
            handle_policy_decide(&conn.actor, &p, shared)
        }
        methods::POLICY_PENDING => {
            // No params defined: null/absence are accepted (ADR 0004) and any
            // object is deliberately IGNORED — adding params in a future
            // version (filters) must not break old servers.
            handle_policy_pending(&conn.actor, shared)
        }
        methods::POLICY_UNDO_SESSION => {
            let p: methods::PolicyUndoSessionParams = parse_params(req.params)?;
            handle_policy_undo_session(&conn.actor, p, shared).await
        }
        methods::POLICY_UNDO_REPORT => {
            let p: methods::PolicyUndoReportParams = parse_params(req.params)?;
            handle_policy_undo_report(&conn.actor, &p, shared)
        }
        // The timeline (phase 7). Both are User-ONLY, and the gate goes
        // BEFORE parsing, as in `log.tail` and `host.volumes` and for the
        // same reason: if parsing happens first, an agent can distinguish
        // "forbidden" (good params) from "bad params" by probing shapes, and
        // that turns the denial into an oracle about the protocol itself.
        // The handler re-checks it on its own — it is the gate, not
        // decoration, and the tests call it directly.
        methods::JOURNAL_LIST => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::JournalListParams = parse_params(req.params)?;
            handle_journal_list(&conn.actor, &p, shared).await
        }
        methods::JOURNAL_UNDO_AFTER => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::JournalUndoAfterParams = parse_params(req.params)?;
            handle_journal_undo_after(&conn.actor, &p, shared).await
        }
        // host.volumes (0.37.0, #131): enumerating the HOST's volumes, ONLY
        // for a User connection (design §C of
        // `2026-08-10-volumes-design.md`) — the mount table names the human's
        // disks, servers and removable media, and an agent under scope has no
        // use for it at all. The gate goes BEFORE parsing (same criterion as
        // `index.embed`/`index.search_semantic`/`ai.rename_plan`, review V2's
        // MAJOR): an agent sees `PolicyDenied` regardless of its params'
        // validity, and never an `INVALID_PARAMS` that would let it
        // distinguish "forbidden" from "bad params" by fuzzing the one
        // field. No params defined beyond the bool with a default:
        // null/absent is accepted (ADR 0004), same pattern as
        // `task.list`/`plugin.list`.
        methods::HOST_VOLUMES => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::HostVolumesParams = parse_params(
                req.params
                    .filter(|v| !v.is_null())
                    .or_else(|| Some(serde_json::json!({}))),
            )?;
            handle_host_volumes(p).await
        }
        // `connection.list` (0.56.0, #264): the names from
        // `connections.toml`, so a frontend can offer a selector without
        // reading that file itself — reading it would cost the whole
        // network stack.
        //
        // The gate goes BEFORE parsing, same criterion as `host.volumes` and
        // for the same reason: the list names the human's servers, and an
        // agent cannot distinguish "forbidden" from "bad params" by fuzzing
        // anything. No params defined: null or absent is accepted (ADR
        // 0004).
        methods::CONNECTION_LIST => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            handle_connection_list(shared).await
        }
        // `log.tail` / `log.level` (0.65.0, #328, ADR 0092): THIS process's
        // log, which a frontend on another one has no other way to see — the
        // window starts its own daemon (#300), so its ring has the bridge's
        // lines and not those of the provider that failed.
        //
        // The gate goes BEFORE parsing, same criterion as `connection.list`
        // and `host.volumes` and for the same twofold reason: the ring
        // carries paths, connection names and activity from OTHER
        // sessions — an existence oracle outside an agent's confinement, the
        // same leak `read_gate_all` documents for `plugin.decorate` — and an
        // agent cannot distinguish "forbidden" from "bad params" by fuzzing
        // the shape of its own request.
        methods::LOG_TAIL => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::LogTailParams = parse_params(req.params)?;
            handle_log_tail(&p, shared)
        }
        // Raising the level is more of the same: an agent would raise the
        // verbosity of a job it is not part of. And what applies it is the
        // daemon — `LogRing::raise_to`, with its allowlist — never the
        // client.
        methods::LOG_LEVEL => {
            if !matches!(conn.actor, Actor::User) {
                return Err(RpcError::from(norte_proto::Error::PolicyDenied {
                    rule: "not-approved".into(),
                }));
            }
            let p: methods::LogLevelParams = parse_params(req.params)?;
            handle_log_level(&p, shared)
        }
        // plugin.* (M4-P3): listing the catalogue (any connection) and
        // approving/enabling (humans ONLY — it is consenting to
        // capabilities, a security act).
        methods::PLUGIN_LIST => handle_plugin_list(req.params, shared),
        methods::PLUGIN_SET_APPROVAL => {
            let p: methods::PluginSetApprovalParams = parse_params(req.params)?;
            handle_plugin_set_approval(&conn.actor, &p, shared).await
        }
        methods::PLUGIN_SET_ENABLED => {
            let p: methods::PluginSetEnabledParams = parse_params(req.params)?;
            handle_plugin_set_enabled(&conn.actor, &p, shared).await
        }
        methods::PLUGIN_UNINSTALL => {
            let p: methods::PluginUninstallParams = parse_params(req.params)?;
            handle_plugin_uninstall(&conn.actor, &p, shared).await
        }
        // plugin.run_command (M4-P4): OPEN (running consents to nothing).
        methods::PLUGIN_RUN_COMMAND => handle_plugin_run_command(req.params, shared).await,
        // plugin.preview (M4-P5): OPEN (previewing consents to nothing).
        methods::PLUGIN_PREVIEW => handle_plugin_preview(req.params, &conn.actor, shared).await,
        // plugin.preview_styled (G3a, ADR 0037): styled twin, same openness
        // criterion as its plain twin.
        methods::PLUGIN_PREVIEW_STYLED => {
            handle_plugin_preview_styled(req.params, &conn.actor, shared).await
        }
        // plugin.thumbnail (ADR 0107): OPEN, with the same read gate as its
        // twins — a thumbnail READS the file.
        methods::PLUGIN_THUMBNAIL => handle_plugin_thumbnail(req.params, &conn.actor, shared).await,
        // plugin.panel_render (0.74.0, phase 3): OPEN like its twins. The
        // read gate goes over the DIRECTORY the panel accompanies: what the
        // guest actually reads also goes through `norte:location`, with its
        // consented prefix and its budget.
        methods::PLUGIN_PANEL_RENDER => {
            handle_plugin_panel_render(req.params, &conn.actor, shared).await
        }
        // plugin.decorate / plugin.column_values (G3b, ADR 0037): OPEN like
        // the rest of `plugin.preview*`, with the same read gate (#80)
        // extended to the whole batch (`read_gate_all`).
        methods::PLUGIN_DECORATE => handle_plugin_decorate(req.params, &conn.actor, shared).await,
        methods::PLUGIN_COLUMN_VALUES => {
            handle_plugin_column_values(req.params, &conn.actor, shared).await
        }
        // plugin.rename_plan (C3, ADR 0095): a plan, not a mutation — open
        // with the read gate over `dir`, like `column_values`.
        methods::PLUGIN_RENAME_PLAN => {
            handle_plugin_rename_plan(req.params, &conn.actor, shared).await
        }
        // The same arrangement for the organizer (phase 8): open like its
        // sibling — proposing mutates nothing — with the directory's read
        // gate inside the handler.
        methods::PLUGIN_ORGANIZE_PLAN => {
            handle_plugin_organize_plan(req.params, &conn.actor, shared).await
        }
        // plugin.get_config (G3c): OPEN, same criterion as plugin.list.
        // plugin.set_config (G3c): humans ONLY, same criterion as
        // plugin.set_approval/set_enabled — plugin settings are user data, an
        // agent does not edit them on its own.
        methods::PLUGIN_GET_CONFIG => handle_plugin_get_config(req.params, shared).await,
        // plugin.help (H3e): OPEN, same criterion as plugin.list.
        methods::PLUGIN_HELP => handle_plugin_help(req.params, shared).await,
        methods::PLUGIN_SET_CONFIG => {
            let p: methods::PluginSetConfigParams = parse_params(req.params)?;
            handle_plugin_set_config(&conn.actor, &p, shared).await
        }
        _ => dispatch_fs_task(req, conn_id, conn.actor.clone(), shared).await,
    }
}

/// `daemon.shutdown` — shutting the daemon down is a human act (#66): without
/// this gate, the hard shutdown cancels EVERY task (bypassing
/// `task.cancel`'s gate) and takes down the human's session.
fn handle_daemon_shutdown(
    actor: &Actor,
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may shut down the daemon",
        ));
    }
    // The canonical emitter writes `params: null` (ADR 0004) and this method
    // is all-optional: null and absence = defaults (protocol-guardian M1; the
    // request_null_params golden pins it).
    let p: methods::DaemonShutdownParams = parse_params(
        params
            .filter(|v| !v.is_null())
            .or_else(|| Some(serde_json::json!({}))),
    )?;
    // A HANDOVER with live tasks is refused HERE, the only moment there is
    // still someone to answer: this method's response goes out immediately,
    // so a refusal decided after waiting would have no recipient — and by
    // then the listener would have already stopped accepting, so "refusing"
    // would mean accepting again.
    //
    // Waiting and NOT cancelling is what sets this axis apart from
    // `graceful`: a copy dying mid-tree is exactly the mess the journal then
    // has to clean up. Whoever genuinely wants to cancel them already has
    // `graceful: false`, and a second door is not opened onto the same room.
    // Count and SHUT DOWN under the same lock: between releasing it and
    // cancelling, another connection could register a task, and then the
    // handover would start with a live task anyway — exactly what is being
    // refused. The lock is `std`'s and everything inside is synchronous.
    let live = shared.tasks.lock().expect("tasks lock is sound");
    if p.mode == methods::ShutdownMode::Handover && !live.is_empty() {
        let how_many = live.len();
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            format!(
                "a handover needs an idle daemon: {how_many} task(s) still running. \
                 Wait for them; a handover never cancels a task, not even with \
                 graceful:false — use a plain stop for that"
            ),
        ));
    }
    // The notice goes BEFORE stopping accepting, or it reaches nobody:
    // `shutdown` cuts the accept loop and the connections go right after.
    //
    // To ALL connections, not only the human ones: an agent's session dies
    // with this daemon just like a human's, and it needs to know whether to
    // come back. This is information about the TRANSPORT, not about
    // governance.
    let notice = methods::DaemonGoingAway {
        reconnect: p.mode == methods::ShutdownMode::Handover,
    };
    if let Ok(params) = serde_json::to_value(notice) {
        let n = Notification {
            jsonrpc: norte_proto::wire::JsonRpcVersion,
            method: methods::DAEMON_GOING_AWAY.into(),
            params: Some(params),
        };
        if let Ok(frame) = encode_frame(&n) {
            shared.broadcast_all(&Arc::from(frame.into_boxed_slice()));
        }
    }
    if !p.graceful {
        shared.hard_shutdown.cancel();
    }
    shared.shutdown.cancel();
    drop(live);
    to_value(&methods::DaemonShutdownResult {})
}

/// `policy.request_scope` (M3-3b): an AGENT asks for a scope for ITSELF. The
/// request stays pending; it grants nothing until a human grants it with
/// `policy.grant_scope`. Returns the `request_id`. The pending entry is
/// removed from the global map when the connection that created it dies (it
/// does not outlive its owner).
#[tracing::instrument(skip_all, fields(ops = ?p.ops, ttl_ms = p.ttl_ms))]
fn handle_request_scope(
    conn: &mut ConnState,
    p: methods::RequestScopeParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // Only an agent requests scope, and ONLY for its own session: the
    // identity is fixed by the connection (T2), never by the message body. A
    // `User` does not need scope (it is not sandboxed), so requesting it is a
    // protocol error.
    let Actor::Agent { session } = &conn.actor else {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only an agent session may request a scope",
        ));
    };
    if p.session != *session {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "session must match the connection's agent session",
        ));
    }
    // Per-connection sub-cap: a single session does not exhaust the others'
    // global channel (an ungranted pending entry occupies a slot until grant
    // or disconnection).
    if conn.pending_scope_ids.len() >= MAX_PENDING_SCOPE_PER_CONN {
        return Err(RpcError::protocol(
            codes::OVERLOADED,
            "too many pending scope requests on this connection",
        ));
    }
    let session = session.clone();
    let mut pending = shared
        .pending_scope
        .lock()
        .expect("pending_scope lock is sound");
    if pending.len() >= MAX_PENDING_SCOPE {
        return Err(RpcError::protocol(
            codes::OVERLOADED,
            "too many pending scope requests",
        ));
    }
    let request_id = shared.next_scope_req.fetch_add(1, Ordering::SeqCst);
    pending.insert(
        request_id,
        PendingScope {
            session,
            roots: p.roots,
            ops: p.ops,
            ttl_ms: p.ttl_ms,
        },
    );
    drop(pending);
    conn.pending_scope_ids.push(request_id);
    to_value(&methods::RequestScopeResult { request_id })
}

/// `policy.grant_scope` (M3-3b): a human grants a pending request.
///
/// "Human" = any connection that did NOT declare `agent_session` (actor
/// `User`). Under the UDS same-uid threat model (§14) this is not a strong
/// barrier: a process of the same uid can open a 2nd connection without
/// `agent_session` and self-grant scope — but that `User` connection can
/// ALREADY run the mutations directly (`User` = allow-all), so the grant does
/// not give it extra power. The policy is a guardrail for agents that
/// COOPERATE (via norte-mcp, M3-4), not a sandbox. Materializes the `Scope`
/// with its TTL and opens the border the engine's gate consults (the same
/// `ScopeRegistry`).
#[tracing::instrument(skip_all, fields(request_id = p.request_id))]
fn handle_grant_scope(
    actor: &Actor,
    p: &methods::GrantScopeParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may grant a scope",
        ));
    }
    // Consumes the request (one grant per id; re-granting is INVALID_PARAMS).
    let req = shared
        .pending_scope
        .lock()
        .expect("pending_scope lock is sound")
        .remove(&p.request_id);
    let Some(req) = req else {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown or already-granted request_id",
        ));
    };
    let scope = Scope {
        roots: req.roots,
        ops: OpSet::from_names(&req.ops),
        expires_at: Some(scope_deadline(req.ttl_ms)),
    };
    // Security effect (future M3-5 audit material): traces the grant with the
    // session and the ops, never the raw paths (rule 10).
    tracing::info!(session = %req.session, ops = ?req.ops, "scope granted to the agent session");
    shared.scopes.grant(&req.session, scope);
    to_value(&methods::GrantScopeResult {})
}

/// `session.get` (L2, 0.48.0) — the screen the client left, and whether THIS
/// connection is the one that can write it.
///
/// Claiming ownership is part of READING, not a separate method: whoever
/// starts up reads, and whoever reads is the natural candidate to write. The
/// first human connection keeps it; the following ones receive the same copy
/// and run detached — opening a second terminal gives what the reader
/// expected, and there are never two writers over one state.
///
/// Humans ONLY, same criterion as `daemon.shutdown` and `policy.pending`: an
/// agent session has no screen to save.
fn handle_session_get(
    actor: &Actor,
    conn_id: u64,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection has a UI session",
        ));
    }
    // Owner AND has somewhere to write: a daemon with no `state_dir`, no
    // lock, or that found on disk a session from a newer binary, accepts a
    // `put` in memory and persists nothing. Saying `owner: true` there would
    // send the client to write a screen every second that is lost on exit,
    // with not a single warning — and the embedded arm already answered
    // correctly, so it was also the SAME question with two answers.
    let owner = shared.ui_session.claim(conn_id) && shared.session_persists.load(Ordering::Acquire);
    to_value(&methods::SessionGetResult {
        session: shared.ui_session.get(),
        owner,
    })
}

/// `session.release` (0.78.0, phase 9) — the owning connection gives up the
/// UI session without disconnecting.
///
/// This is what makes handoff between frontends possible: flush the screen,
/// release it, and have the other one claim it in its `session.get`. Until
/// 0.78, releasing only happened on DISCONNECT, so the one leaving had to die
/// before the one arriving could claim it — and if the one arriving never
/// started, the screen went down with the dead one.
///
/// **Releasing someone else's does nothing, and it SAYS SO.** `released:
/// false` is "it wasn't you", and whoever is handing off needs that: without
/// the distinction it would send the other frontend off to claim a session
/// that still has an owner, and the window would open empty with nobody able
/// to explain why.
///
/// The body is NOT touched: what is released is ownership. The document
/// stays where it was with its revision, which is exactly what the other one
/// is going to read.
fn handle_session_release(
    actor: &Actor,
    conn_id: u64,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection has a UI session",
        ));
    }
    let was_owner = shared.ui_session.owner() == Some(conn_id);
    if was_owner {
        shared.ui_session.release(conn_id);
    }
    to_value(&methods::SessionReleaseResult {
        released: was_owner,
    })
}

/// `session.put` (L2, 0.48.0) — replaces the whole session.
///
/// Four denials, and each says something different to the client:
/// [`norte_proto::Error::Cancelled`] if the daemon is shutting down (there is
/// nowhere left to write; against the handoff's successor the same write is
/// valid), [`norte_proto::Error::PermissionDenied`] if it is not the owner
/// (re-reading fixes nothing: this connection never writes),
/// [`norte_proto::Error::Conflict`] with
/// [`norte_proto::ConflictKind::StaleRevision`] if it carries a past revision
/// (re-reading DOES fix it) and [`norte_proto::Error::LimitExceeded`] with
/// [`norte_proto::Error::LIMIT_SESSION_BODY`] if the body is over the cap
/// (re-reading does not; dropping history and retrying does). In all three
/// cases what is stored stays exactly as it was.
/// Why a `session.put` does not reach the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPutVeto {
    /// An agent session has no screen to save.
    NotHuman,
    /// Another connection is the owner.
    NotTheOwner,
    /// This CORE does not write: no `state_dir`, no lock, or with a session
    /// from a newer binary on disk.
    NoWriter,
}

impl From<SessionPutVeto> for RpcError {
    fn from(v: SessionPutVeto) -> Self {
        match v {
            SessionPutVeto::NotHuman => Self::protocol(
                codes::INVALID_REQUEST,
                "only a human (non-agent) connection has a UI session",
            ),
            // The SAME taxonomy as "you're not the owner", on purpose: the
            // client does not care which of the two it is missing, and both
            // are fixed the same way — ask again. The distinction lives in
            // the log, not the wire.
            SessionPutVeto::NotTheOwner | SessionPutVeto::NoWriter => {
                Self::from(norte_proto::Error::PermissionDenied)
            }
        }
    }
}

/// Who may write the session, in the order it is asked.
///
/// Pure and kept apart from the handler because the two interesting
/// conditions are RACES — shutdown and owner handoff — and a race is not
/// tested by triggering it: it is tested by deciding over the same four
/// pieces of data. Same thing done with the CLI's SIGINT gate (#212).
///
/// The order is not cosmetic. Human first, because an agent should not even
/// find out whether there is an owner. And ownership before the revision and
/// the cap, because giving the other two answers to whoever is not in charge
/// would give false advice ("re-read", "trim") about a write that is never
/// going to be accepted.
///
/// Shutdown is NOT here (#233): checking it against the token, outside the
/// lock protecting the mutation, left open the window it meant to close.
/// [`crate::ui_session::SessionStore::seal`] closes it, under the same lock
/// as the `put`.
///
/// **`persiste` is the third one, added by #237's review.** The embedded arm
/// already refused a `put` from a detached process — "it doesn't write, not
/// even in memory" —; the daemon used to accept it and answer a new
/// revision, and that stopped being harmless the moment its writer could take
/// the lock LATE: a body accepted while detached, with its revision already
/// ahead of disk's, survived `adopt_from_disk` (which backs off when the
/// local one is ahead, leaving the dirty flag set) and got published over the
/// other core's screen in the same tick. And since the revision only goes up,
/// nothing could detect it afterward.
fn session_put_veto(
    is_human: bool,
    owner: Option<u64>,
    conn_id: u64,
    persists: bool,
) -> Option<SessionPutVeto> {
    if !is_human {
        return Some(SessionPutVeto::NotHuman);
    }
    if owner != Some(conn_id) {
        return Some(SessionPutVeto::NotTheOwner);
    }
    if !persists {
        return Some(SessionPutVeto::NoWriter);
    }
    None
}

fn handle_session_put(
    actor: &Actor,
    conn_id: u64,
    p: methods::SessionPutParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if let Some(veto) = session_put_veto(
        matches!(actor, Actor::User),
        shared.ui_session.owner(),
        conn_id,
        shared.session_persists.load(Ordering::Acquire),
    ) {
        return Err(veto.into());
    }
    match shared.ui_session.put(p.version, p.revision, p.body) {
        Ok(revision) => to_value(&methods::SessionPutResult { revision }),
        Err(crate::ui_session::PutError::Conflict { current }) => {
            // With a TAXONOMY in `data` (#182): without it the client reads
            // "internal error" and does not know that re-reading fixes it.
            // The current revision does NOT travel in the error — it is
            // fetched with `session.get`, the same trip it has to make
            // anyway to know what body it was writing against.
            tracing::debug!(current, "session.put with a stale revision");
            Err(RpcError::from(norte_proto::Error::Conflict {
                conflict: norte_proto::ConflictKind::StaleRevision,
            }))
        }
        // The session already closed: the client was in fact sending and its
        // write arrived late, so `Cancelled` — against a handoff's successor,
        // the same write is valid.
        Err(crate::ui_session::PutError::Sealed) => {
            tracing::debug!("session.put after the session's closing");
            Err(RpcError::from(norte_proto::Error::Cancelled))
        }
        Err(crate::ui_session::PutError::TooLarge { bytes }) => {
            tracing::warn!(bytes, "session.put over the cap");
            Err(RpcError::from(norte_proto::Error::LimitExceeded {
                limit: norte_proto::Error::LIMIT_SESSION_BODY.to_owned(),
            }))
        }
        // `Unsupported` and not `InvalidPath`: what is missing is not a
        // well-formed parameter, it is a core able to read that schema —
        // "your daemon is older", exactly what that error says across the
        // rest of the wire (#247).
        Err(crate::ui_session::PutError::UnknownSchema { version, known }) => {
            tracing::warn!(version, known, "session.put of an unknown schema");
            Err(RpcError::from(norte_proto::Error::Unsupported))
        }
    }
}

/// `policy.decide` (M3-3b Task 4): a human approves/denies a pending entry.
///
/// Only a NON-agent connection decides (same criterion and same threat model
/// as [`handle_grant_scope`]): an agent never approves its own op — the
/// `Ask`'s suspension would be theater. A decision consumes the id;
/// repeating it (or an expired/unknown id) is `INVALID_PARAMS`.
#[tracing::instrument(skip_all, fields(approval_id = p.approval_id, approve = p.approve))]
fn handle_policy_decide(
    actor: &Actor,
    p: &methods::PolicyDecideParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may decide an approval",
        ));
    }
    // The three failure modes travel DIFFERENTLY (#279): "your click didn't
    // get through", "you were too late" and "that approval does not belong
    // to this daemon" call for different answers to whoever is looking at
    // the screen, and used to be collapsed into an `INVALID_PARAMS` with the
    // reason inside an English `message` no frontend can classify.
    use crate::daemon::approvals::Decision;
    let reason = match shared.approvals.decide(p.approval_id, p.approve) {
        Decision::Applied => {
            // Security effect (M3-5 audit material): who decided what.
            tracing::info!("policy approval decided by the human");
            return to_value(&methods::PolicyDecideResult {});
        }
        Decision::Expired => "expired",
        Decision::YaDecided => "already-decided",
        Decision::Unknown => "unknown",
    };
    Err(RpcError::from(norte_proto::Error::ApprovalGone {
        reason: reason.to_owned(),
    }))
}

/// `policy.pending` (M3-3b Task 4): resync of pending approvals for a
/// frontend that connects AFTER the broadcast. Humans only: the list crosses
/// sessions (other sessions' paths) and an agent does not decide, so it does
/// not list either.
fn handle_policy_pending(
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may list pending approvals",
        ));
    }
    to_value(&methods::PolicyPendingResult {
        pending: shared.approvals.pending(),
    })
}

/// `policy.undo_session` (M3-4): a HUMAN undoes an agent's whole session.
/// Target = `Agent{session}` (selects the journal entries), executor = `User`
/// (passes the gate and signs the compensations): the undo does not depend
/// on the agent's scope still being alive. User connections only — an agent
/// does not undo others (its own session, as a tool = ADR 0024 debt).
///
/// The span does NOT log the raw wire session (security MINOR-2): before
/// passing `valid_agent_session` it can carry `\n`/ANSI and fabricate fake
/// log lines — right in M3-5 audit material. It is logged VALIDATED.
#[tracing::instrument(skip_all)]
async fn handle_policy_undo_session(
    actor: &Actor,
    p: methods::PolicyUndoSessionParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may undo an agent session",
        ));
    }
    if !valid_agent_session(&p.session) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "session must be 1..=64 chars of [A-Za-z0-9._-]",
        ));
    }
    let target = Actor::Agent { session: p.session };
    let (handle, _report) = shared
        .engine
        .undo_session_for(&target, Actor::User)
        .await
        .map_err(RpcError::from)?;
    // Security effect (M3-5 audit material): who undid whom. The session
    // already passed the charset validation — safe to log.
    if let Actor::Agent { session } = &target {
        tracing::info!(session = %session, "agent session undo requested by the human");
    }
    // The undo task's owner is the human EXECUTOR (only User reaches here).
    // The report for `policy.undo_report` (#71) was already retained by the
    // engine when it launched the Task. Honesty NOTE: if this answers
    // OVERLOADED, the Task ALREADY runs since the submit and the cancellation
    // is cooperative — real reverts can land, and the client does not
    // receive the id to ask for its report with (the journal does record the
    // compensations). That is why the report is RELEASED in that case: an id
    // nobody received has nobody to serve it to.
    let task_id = register_task_undo(shared, handle)?;
    to_value(&methods::PolicyUndoSessionResult { task_id })
}

/// [`register_task_id`] for an undo Task, which also releases its retained
/// report if registration fails (OVERLOADED): that id reaches nobody.
fn register_task_undo(shared: &Arc<Shared>, handle: TaskHandle) -> Result<TaskId, RpcError> {
    let id = handle.id();
    register_task_id(shared, handle, Actor::User).inspect_err(|_| {
        shared.engine.forget_undo_report(id);
    })
}

/// `journal.list` (0.76.0, phase 7): a page of the timeline.
///
/// User-ONLY, and the gate goes BEFORE parsing for the same reason as in
/// `host.volumes` and `log.tail`: an agent sees `PolicyDenied` regardless of
/// its params' shape, and cannot distinguish "forbidden" from "bad params"
/// by fuzzing the fields. What is behind it is more sensitive than the log:
/// the journal names EVERYTHING touched on this machine, including what is
/// outside the agent's confinement and what other sessions did.
///
/// The `limit` is capped here, like `fs.list` with its page: asking for more
/// is not an error and loses nothing, because what does not fit is still
/// there behind the cursor.
async fn handle_journal_list(
    actor: &Actor,
    p: &methods::JournalListParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: "not-approved".into(),
        }));
    }
    let limit = p.limit.clamp(1, methods::JOURNAL_LIST_MAX_PAGE);
    let entries = shared
        .engine
        .journal_page(p.before_seq, limit, p.actor_kind.as_deref())
        .await
        .map_err(RpcError::from)?;
    // The cursor comes from the LAST `seq` served, and only if the page was
    // full: with a half-full page there is nothing older left, and offering
    // a cursor there would make a client ask for another round only to get
    // zero rows in a loop. If it is full, the next one asks for "before this
    // one", exactly `before_seq`'s (strict) contract.
    to_value(&crate::journal::page_to_wire(&entries, limit))
}

/// `journal.undo_after` (0.76.0, phase 7): undoes the HUMAN's work after a
/// `seq`.
///
/// User-ONLY, like the `policy.undo_session` it shares a report with: this
/// reverts work, and how far it goes is decided by whoever did it. An agent
/// session that could request it would erase the trace of its own.
async fn handle_journal_undo_after(
    actor: &Actor,
    p: &methods::JournalUndoAfterParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: "not-approved".into(),
        }));
    }
    let (handle, _report) = shared
        .engine
        .undo_after(p.seq, p.upto_seq)
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_undo(shared, handle)?;
    // Audit material, like an agent session's undo: how far, who, and with
    // which Task. Without the `task_id` the line cannot be cross-referenced
    // with the report nor with the compensations that appear later in the
    // journal itself.
    //
    // Goes AFTER the registration on purpose: `register_task_id` can still
    // answer OVERLOADED, and then there is no Task to name. The honesty
    // `handle_policy_undo_session` documents in its own spot applies here
    // too — the Task already runs since the submit, so real reverts can
    // happen whose report gets lost; what this avoids is pointing at an id
    // that never came to exist.
    tracing::info!(
        after_seq = p.seq,
        // The ceiling also decides what got undone (0.80.0): without it in
        // the line, cross-referencing it with the report does not explain
        // why something was left out.
        upto_seq = ?p.upto_seq,
        task_id = task_id.get(),
        "undo up to a point requested by the human"
    );
    // The report was retained by the engine, in the SAME ring as
    // `policy.undo_session`'s: it is the same undo and is read through the
    // same `policy.undo_report`.
    to_value(&methods::PolicyUndoSessionResult { task_id })
}

/// `policy.undo_report` (#71): an undo Task's report — User-ONLY, like the
/// `policy.undo_session` that generates it (carries a journal `seq` and a
/// block reason). Snapshot: final once the Task is terminal.
fn handle_policy_undo_report(
    actor: &Actor,
    p: &methods::PolicyUndoReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may read an undo report",
        ));
    }
    // `NotFound` from the taxonomy (0.79.0), the SAME one as its twin
    // `fs.rename_batch_report` and as the embedded arm, for both situations
    // — it was never an undo, or it was evicted from the ring. It used to be
    // a bare `INVALID_PARAMS`, which the client read as `Internal`.
    //
    // The stored owner is not looked at: unlike its twin, only the human
    // reaches here (the barrier above), and every undo is executed by the
    // human, so a `may_observe` could not deny anything — it would be dead
    // code that looks like a check.
    let (_owner, snapshot) = shared
        .engine
        .undo_report(p.task_id)
        .ok_or_else(|| RpcError::from(norte_proto::Error::NotFound))?;
    to_value(&crate::undo::report_to_proto(snapshot))
}

/// `host.volumes` (0.37.0, #131): enumeration of the HOST's volumes. The
/// actor gate (`User` ONLY — design §C of `2026-08-10-volumes-design.md`)
/// lives in the `dispatch` arm that calls this function, BEFORE parsing the
/// params (see that arm's comment): this function only ever runs for an
/// already-authorized connection, so it does not re-check the actor. There
/// is no `Provider`/engine to consult — [`crate::volumes::enumerate`] is a
/// free HOST function (design §A).
async fn handle_host_volumes(p: methods::HostVolumesParams) -> Result<serde_json::Value, RpcError> {
    let volumes = crate::volumes::enumerate(p.include_pseudo)
        .await
        .map_err(|_| RpcError::from(norte_proto::Error::Io { retryable: false }))?;
    to_value(&methods::HostVolumesResult {
        volumes: volumes
            .into_iter()
            .map(crate::backend::volume_to_proto)
            .collect(),
    })
}

/// `connection.list` (0.56.0, #264): the daemon's named connections.
///
/// Reads the DAEMON's `connections.toml`, which is what makes the method
/// useful: the frontend does not have it and should not. A missing file is
/// an empty list — having no connections is normal on day one — and one
/// whose SYNTAX does not parse is an error, because saying "you have none"
/// when there is an extra comma would lie about what the user wrote.
///
/// An ENTRY that cannot be understood no longer aborts the call (#365): it
/// comes out in `unusable` with its reason, and the rest get listed. Before,
/// a single bad entry cost the whole list, with an error that named no
/// connection.
///
/// Never a secret: what comes out is the `(name, url)` pair as written, and
/// credentials are REFERENCED (ADR 0015).
async fn handle_connection_list(shared: &Arc<Shared>) -> Result<serde_json::Value, RpcError> {
    let dir = shared
        .connections_dir
        .clone()
        .unwrap_or_else(crate::connect::config_dir);
    let (connections, unusable) = crate::connect::named_connections(&dir)
        .await
        .map_err(RpcError::from)?;
    to_value(&methods::ConnectionListResult {
        connections: connections
            .into_iter()
            .map(|(name, url)| methods::ConnectionEntry { name, url })
            .collect(),
        unusable: unusable
            .into_iter()
            .map(|(name, reason)| methods::ConnectionProblem { name, reason })
            .collect(),
    })
}

/// `log.tail` (0.65.0, #328, ADR 0092): what THIS process's log ring has
/// after a cursor.
///
/// The actor gate (`User` ONLY) lives in the `dispatch` arm that calls here,
/// BEFORE parsing the params: only an already-authorized connection reaches
/// this function, so it does not re-check the actor.
///
/// Three of the ADR's decisions are applied in these few lines:
///
/// - **No ring, `Unsupported`.** Never an empty list: a log that does not
///   exist and one with nothing to report read the same on screen, and the
///   panel needs to be able to say "this daemon does not serve it" so it can
///   degrade to its own WHILE SAYING why.
/// - **`cursor: null` is not `0`.** A zero asserts having seen line number
///   zero, so against a ring that already wrapped around it would answer a
///   huge, false `lost` — nobody lost what they never expected. The ring
///   starts from the oldest it retains in both cases; what changes is that
///   with no cursor, no gap is asserted.
/// - **`max` is capped here** at [`LOG_TAIL_MAX_LINES`], like `fs.list` with
///   `FS_LIST_MAX_PAGE`: asking for more is not an error and loses nothing,
///   because what does not fit is still there after `next`. Asking for ZERO
///   is an error (`INVALID_PARAMS`, same criterion as `FsListParams::limit`):
///   a panel polling with `max: 0` would receive an empty list every round
///   with the cursor stuck, and on screen that reads as "nothing is
///   happening" instead of the programming error it is.
#[tracing::instrument(skip(shared))]
fn handle_log_tail(
    p: &methods::LogTailParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // The ring FIRST: to a daemon that does not serve a log, the honest
    // answer is "I don't have one", also when the params are bad — what the
    // client has to do next is the same in both cases.
    let Some(ring) = shared.log_ring.get() else {
        return Err(RpcError::from(norte_proto::Error::Unsupported));
    };
    if p.max == 0 {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "log.tail: `max` must be at least 1 (a zero page would poll forever)",
        ));
    }
    let max = usize::try_from(p.max)
        .unwrap_or(usize::MAX)
        .min(LOG_TAIL_MAX_LINES);
    // With no cursor, it is asked from the start of what the ring RETAINS,
    // which is what `since(0, …)` delivers; what does not hold from that
    // zero is its `lost`, which would count as lost everything that had
    // already fallen off before this reader existed.
    let tail = ring.since(p.cursor.unwrap_or(0), max);
    to_value(&methods::LogTailResult {
        lines: tail
            .lines
            .into_iter()
            .map(|l| methods::LogLine {
                epoch_ms: l.epoch_ms,
                level: l.level.wire().to_owned(),
                target: l.target,
                message: l.message,
            })
            .collect(),
        next: tail.next,
        lost: if p.cursor.is_some() { tail.lost } else { 0 },
        // The level travels WITH the lines it applies to: it is global to the
        // daemon and another client could have raised it a second ago, so in
        // a separate method it would be born stale.
        level: ring.level().wire().to_owned(),
        capacity: u32::try_from(ring.capacity()).unwrap_or(u32::MAX),
    })
}

/// `log.level` (0.65.0, #328, ADR 0092): raises THIS process's ring level and
/// answers with the one that resulted.
///
/// **The security cap is not touched here, and that is the whole design.**
/// Who decides what is kept is
/// [`norte_config::logring::LogRing::raise_to`], with its allowlist: only
/// norte's own targets go above INFO, and `suppaftp` — which emits `PASS
/// <password>` at TRACE (#43, rule 10) — stays down no matter who asks for
/// what. The client ASKS for a level; it does not compute it, does not apply
/// it and does not know the list. A second copy of that defense on the other
/// side of the wire would drift from this one the moment either one changed.
///
/// Since the ring never lowers, the level answered back can be MORE verbose
/// than what was asked, and that is not a bug: it is why the result carries
/// the level instead of a `bool`.
///
/// **This method's two denials are different and answered differently**: no
/// ring is `Unsupported` ("this daemon does not serve a log", and the panel
/// degrades to its own while saying so), and a level outside the vocabulary
/// is `INVALID_PARAMS` ("what you sent is not a level"), same criterion as
/// [`handle_log_tail`]'s `max: 0`. Collapsing them into one would leave a
/// client unable to distinguish a daemon with no log from its own typo,
/// which is the same empty-vs-absent confusion the rest of this change
/// avoids.
///
/// The actor gate lives in the `dispatch` arm, same as in [`handle_log_tail`].
///
/// The span does not carry the `level` that came off the wire until AFTER
/// validating it: it is an arbitrary-length `String` chosen by the client,
/// and formatting it before the rejection would mean logging whatever a peer
/// wants for the simple fact of having sent it.
#[tracing::instrument(skip(shared, p), fields(level = tracing::field::Empty))]
fn handle_log_level(
    p: &methods::LogLevelParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let Some(ring) = shared.log_ring.get() else {
        return Err(RpcError::from(norte_proto::Error::Unsupported));
    };
    // A level outside the vocabulary is NOT degraded to a default one:
    // accepting what is not understood and setting something else would
    // leave the reader believing it asked for something nobody did.
    let Some(level) = norte_config::logline::LogLevel::from_wire(&p.level) else {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "log.level: `level` must be one of error|warn|info|debug|trace",
        ));
    };
    // Already validated: here `level` is one of the five, and what gets
    // logged is that enum's canonical string, not whatever the peer sent.
    tracing::Span::current().record("level", level.wire());
    ring.raise_to(level);
    to_value(&methods::LogLevelResult {
        level: ring.level().wire().to_owned(),
    })
}

/// `plugin.list` (M4-P3): the discovered catalogue + its state, for ANY
/// connection (listing consents to nothing). No params defined (reserved
/// empty object): null/absence are accepted as defaults (ADR 0004), same as
/// `daemon.shutdown`; any object is ignored — a future extension does not
/// break old clients.
fn handle_plugin_list(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let _p: methods::PluginListParams = parse_params(
        params
            .filter(|v| !v.is_null())
            .or_else(|| Some(serde_json::json!({}))),
    )?;
    let list = shared.plugins.lock().expect("plugins lock is sound").list();
    to_value(&list)
}

/// `plugin.set_approval` (M4-P3): a HUMAN approves (or revokes) the
/// capabilities a plugin declared. Approving is a SECURITY act (consenting
/// to a plugin exercising its capabilities), so ONLY a non-agent connection
/// does it — the same criterion as `policy.grant_scope`/`policy.decide`: an
/// agent never consents on the human's behalf. An unknown id is
/// `INVALID_PARAMS` (state is not dirtied with phantom plugins); a
/// persistence failure, `INTERNAL_ERROR`.
// `skip_all` WITHOUT `id = %p.id`: the id comes raw off the wire and must
// NOT go to the log before being validated against the catalogue (log
// injection / spoofing). It is logged (info) ONLY after confirming it is a
// known plugin.
#[tracing::instrument(skip_all, fields(approved = p.approved))]
async fn handle_plugin_set_approval(
    actor: &Actor,
    p: &methods::PluginSetApprovalParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may approve a plugin",
        ));
    }
    // Mutates IN MEMORY under the lock and captures the snapshot + the dir;
    // the lock is released when the block closes, BEFORE any `.await` (rule
    // 2: no blocking I/O on the reactor, nor holding a std::Mutex across an
    // await). And the snapshot goes to disk exclusively with its siblings'
    // (ADR 0104, `Shared::plugins_state_io`): taken before mutating and
    // released on exit, already persisted.
    let _write = shared.plugins_state_io.lock().await;
    let (applied, stale, snapshot, dir) = {
        let mut reg = shared.plugins.lock().expect("plugins lock is sound");
        // What gets GRANTED has to be what the human READ (#282). This
        // daemon discovers the catalogue once at startup, so today the
        // window is closed by accident; the check makes it real, and the
        // embedded `Backend` — which rediscovers on every call — genuinely
        // needs it.
        //
        // Only when APPROVING: revoking grants nothing, and refusing a
        // revocation over a stale anchor would keep alive the permission
        // someone is taking away.
        //
        // An UNKNOWN id is not a stale anchor: `manifest_digest` returns
        // `None` for both cases, and answering "the manifest changed" to
        // whoever named a plugin that does not exist is the wrong diagnosis
        // for a badly-written client's most common mistake. It is asked
        // first whether it is known at all.
        let known = reg.manifest_digest(&p.id).is_some();
        let stale = p.approved
            && known
            && p.expected_digest
                .as_ref()
                .is_some_and(|expected| reg.manifest_digest(&p.id).as_ref() != Some(expected));
        let applied = !stale && reg.set_approval_in_memory(&p.id, p.approved);
        (
            applied,
            stale,
            reg.state_snapshot(),
            reg.config_dir().to_path_buf(),
        )
    };
    if stale {
        // NOT `INVALID_PARAMS`: that is the code for "that plugin does not
        // exist" three lines below, and a client receiving the same for
        // both cannot distinguish "re-read it and approve" from "that id
        // isn't there". It is the same shape as `session.put` with a stale
        // revision, and uses the same variant: `ConflictKind::StaleRevision`.
        return Err(RpcError::from(norte_proto::Error::Conflict {
            conflict: norte_proto::ConflictKind::StaleRevision,
        }));
    }
    if !applied {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    }
    crate::blocking::spawn_blocking(move || crate::plugins::persist_state(&dir, &snapshot))
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "persist task panicked"))?
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("persist: {e}")))?;
    // Security effect (M4 audit material): plugin ALREADY validated as
    // known, so its id is safe for the log.
    tracing::info!(id = %p.id, "plugin (dis)approved by the human");
    to_value(&methods::PluginSetApprovalResult {})
}

/// `plugin.set_enabled` (M4-P3): a HUMAN enables/disables an already-approved
/// plugin. Same barrier and same return semantics as
/// [`handle_plugin_set_approval`] (unknown id = `INVALID_PARAMS`).
// `skip_all` without `id = %p.id`: identical reasoning to
// [`handle_plugin_set_approval`] — the raw wire id does not go to the log
// without validation.
#[tracing::instrument(skip_all, fields(enabled = p.enabled))]
async fn handle_plugin_set_enabled(
    actor: &Actor,
    p: &methods::PluginSetEnabledParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may enable a plugin",
        ));
    }
    // Same rule-2 pattern as set_approval: mutate under the lock, persist
    // outside it, and the write exclusively (`Shared::plugins_state_io`).
    let _write = shared.plugins_state_io.lock().await;
    let (applied, snapshot, dir) = {
        let mut reg = shared.plugins.lock().expect("plugins lock is sound");
        let applied = reg.set_enabled_in_memory(&p.id, p.enabled);
        (
            applied,
            reg.state_snapshot(),
            reg.config_dir().to_path_buf(),
        )
    };
    if !applied {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    }
    crate::blocking::spawn_blocking(move || crate::plugins::persist_state(&dir, &snapshot))
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "persist task panicked"))?
        .map_err(|e| RpcError::protocol(codes::INTERNAL_ERROR, format!("persist: {e}")))?;
    tracing::info!(id = %p.id, "plugin (dis)enabled by the human");
    to_value(&methods::PluginSetEnabledResult {})
}

/// `plugin.uninstall` (0.71.0, ADR 0104): a HUMAN uninstalls a plugin. Same
/// barrier as [`handle_plugin_set_approval`]: withdrawing a consent is as
/// much the human's as giving it, and deleting files from their
/// configuration even more so.
///
/// The work is done by [`crate::plugins::uninstall`], the same one the CLI
/// uses: it validates the id BEFORE turning it into a path, deletes
/// `plugins/<id>/` and leaves the state entry disabled and unapproved. What
/// the CLI could not do is what follows: forgetting it also in the IN-MEMORY
/// registry, which until now kept listing — and decorating with — what was
/// deleted until a restart.
///
/// An id that is not an id, or that is not installed, is `INVALID_PARAMS`,
/// like its siblings' unknown id; an I/O failure is `INTERNAL_ERROR`. The
/// deletion runs in `spawn_blocking` (rule 2) and WITHOUT the registry's
/// lock: the lock is taken afterward, only to forget.
// `skip_all` without `id = %p.id`: the raw wire id does not go to the log
// without validation — identical reasoning to
// [`handle_plugin_set_approval`]. Logged (info) ONLY after the deletion, once
// `uninstall` has already validated it as an id.
#[tracing::instrument(skip_all)]
async fn handle_plugin_uninstall(
    actor: &Actor,
    p: &methods::PluginUninstallParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may uninstall a plugin",
        ));
    }
    // The state write, exclusive with its siblings': see
    // `Shared::plugins_state_io`. Taken before the deletion and released
    // after forgetting in memory, so no in-flight `set_*` can persist a
    // snapshot where this plugin is still approved ON TOP of what
    // `uninstall` just wrote.
    let _write = shared.plugins_state_io.lock().await;
    // `expect`: nobody panics under the registry's lock — only maps and
    // vectors get mutated — so it cannot end up poisoned. Holds for the
    // fourteen uses in this file.
    let dir = shared
        .plugins
        .lock()
        .expect("plugins lock is sound")
        .config_dir()
        .to_path_buf();
    let id = p.id.clone();
    let report = crate::blocking::spawn_blocking(move || crate::plugins::uninstall(&dir, &id))
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "uninstall task panicked"))?
        .map_err(|e| {
            use crate::plugins::UninstallError as U;
            match e {
                U::InvalidId => RpcError::protocol(codes::INVALID_PARAMS, "not a plugin id"),
                U::NotInstalled(_) => {
                    RpcError::protocol(codes::INVALID_PARAMS, "plugin is not installed")
                }
                // No I/O error text in the RESPONSE: it quotes a path under
                // the user's home, and a client reads this too. To the log,
                // yes: `log.tail` is humans-only.
                U::Io(io) => {
                    tracing::warn!(error = %io, "uninstall: I/O failure");
                    RpcError::protocol(codes::INTERNAL_ERROR, "uninstall: io error")
                }
            }
        })?;
    shared
        .plugins
        .lock()
        .expect("plugins lock is sound")
        .forget_in_memory(&report.id);
    tracing::info!(
        id = %report.id,
        was_approved = report.was_approved,
        "plugin uninstalled by the human"
    );
    to_value(&methods::PluginUninstallResult {
        was_approved: report.was_approved,
    })
}

/// `plugin.run_command` (M4-P4): runs a command of an ALREADY approved and
/// enabled plugin. OPEN to any connection (consents to nothing: the human
/// already approved+enabled, and the empty WASI sandbox contains the guest).
///
/// Rule 2 (critical): EXECUTION (`instantiate` compiles the WASM component +
/// `run_command`) is synchronous and heavy. Split into two:
/// 1. RESOLVE (cheap) under the registry's lock: validates consent and
///    resolves `.wasm` + capabilities. The `MutexGuard` is released when the
///    block closes, BEFORE the `.await`.
/// 2. RUN (heavy) OUTSIDE the lock, in a `spawn_blocking`, with a clone of the
///    shared `Arc<PluginRuntime>`.
///
/// Redaction toward the client (security-reviewer M4-P4): a runtime failure
/// (`PluginRunError::Runtime`) can carry the `.wasm` path or internal
/// wasmtime details; its raw `Display` is NEVER returned to the client — a
/// generic `INTERNAL_ERROR` is answered and the detail goes ONLY to the local
/// log.
// `skip_all` without `id`: the id comes raw off the wire; it does not go to
// the log except after resolving (same criterion as set_approval/set_enabled).
#[tracing::instrument(skip_all)]
async fn handle_plugin_run_command(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginRunCommandParams = parse_params(params)?;
    // 1) Resolve under the lock (cheap). The guard does NOT cross the `.await`.
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_runnable(&p.id)
    };
    let (wasm, caps, settings) = resolved.map_err(|e| run_error_to_rpc(&e))?;

    // 2) Run outside the lock, in spawn_blocking (rule 2). The runtime is
    // `Send+Sync` but not `Clone`: the `Arc` is cloned.
    let runtime = Arc::clone(&shared.plugin_runtime);
    let command = p.command.clone();
    let arg = p.arg.clone();
    let output = crate::blocking::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        // P2 Task 4a: delivers `[config]` ALREADY resolved (Task 2) to the
        // guest, same criterion as `PluginRegistry::run_command` (embedded
        // use).
        inst.set_settings(settings);
        inst.run_command(&command, &arg)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "plugin task panicked"))?
    .map_err(|e| {
        // Redaction: the detail (possible .wasm path / wasmtime internals)
        // goes ONLY to the local log; to the client, a generic message.
        tracing::warn!(id = %p.id, error = %e, "the plugin runtime failed");
        RpcError::protocol(codes::INTERNAL_ERROR, "plugin runtime failed")
    })?;
    to_value(&methods::PluginRunCommandResult { output })
}

/// Maps the consent verdict from [`crate::plugins::resolve_runnable`] (never
/// `Runtime`, which is handled separately with redaction) to an `RpcError`.
/// These variants' messages do NOT reveal paths (they carry the id, not the
/// path; consistent with `list()`'s redaction), so they are safe to
/// propagate.
fn run_error_to_rpc(e: &crate::plugins::PluginRunError) -> RpcError {
    use crate::plugins::PluginRunError as E;
    match e {
        // Nonexistent id or no binary: PARAMETER error (the client asked for
        // something that does not exist/is not runnable).
        E::Unknown(_) | E::NotRunnable(_) | E::NoBinary(_) => {
            RpcError::protocol(codes::INVALID_PARAMS, e.to_string())
        }
        // Not approved / disabled: the request is not valid in this state
        // (the human has not consented). INVALID_REQUEST with a clear
        // message.
        E::NotApproved(_) | E::Disabled(_) => {
            RpcError::protocol(codes::INVALID_REQUEST, e.to_string())
        }
        // Should not reach here (resolve_runnable does not execute), but in
        // case the type evolves: redacted, never the raw Display.
        E::Runtime(_) => RpcError::protocol(codes::INTERNAL_ERROR, "plugin runtime failed"),
    }
}

/// `plugin.preview` (M4-P5): renders file `p.path` with the FIRST consented
/// previewer whose mimetype (guessed by extension) matches, or returns
/// `preview: None` if none applies. OPEN like `run_command` (previewing
/// consents to nothing). Three phases, separated so as NOT to cross the
/// `MutexGuard` over an `.await` (rule 2):
///
/// 1. RESOLVE (cheap) under the registry's lock: guesses the mimetype and
///    picks the previewer. The guard is released when the block closes,
///    BEFORE any await. None → `preview: None` (NOT an error: the frontend
///    falls back to the raw view).
/// 2. READ the file's bytes CAPPED at [`PREVIEW_MAX_BYTES`](crate::plugins::PREVIEW_MAX_BYTES)
///    via the engine (async, outside the lock). An unreadable file
///    (`NotFound`…) is an HONEST error that propagates — not an empty
///    preview.
/// 3. RUN (heavy, compiles WASM) OUTSIDE the lock, in a `spawn_blocking`,
///    with a clone of the shared `Arc<PluginRuntime>`.
///
/// Redaction toward the client (security-reviewer M4-P4/P5): a runtime
/// failure can carry the `.wasm` path or wasmtime details; its raw `Display`
/// is NEVER returned — a generic `INTERNAL_ERROR` + the detail ONLY to the
/// local log.
///
/// OPEN (not User-only) KNOWINGLY and COUPLED to `fs.read`: this handler
/// reads the file with the daemon's authority, just like `fs.read`, which is
/// TODAY open to agents. Since the preview returns a lossy transformation of
/// the first MiB, it is strictly WEAKER a read than raw `fs.read` (no
/// escalation; security-reviewer M4-P5). INVARIANT (met in #80):
/// `plugin.preview` gates with the SAME [`read_gate`] as `fs.read` — an agent
/// only previews under its scope; otherwise it would be a read bypass.
// `skip_all`: `p.path` goes into the redacted fields of the lower layers, not
// this handler's span (same criterion as run_command).
#[tracing::instrument(skip_all)]
async fn handle_plugin_preview(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginPreviewParams = parse_params(params)?;
    // Read gate (#80): preview READS the file with the daemon's authority;
    // without this gate a scopeless agent would exfiltrate content by
    // sidestepping fs.read.
    read_gate(actor, &p.path, shared)?;
    // The mimetype is `&'static str` (extension heuristic, does not read bytes).
    let mime = crate::plugins::guess_mimetype(&p.path);
    // 1) Resolve under the lock (cheap). The guard does NOT cross the `.await`.
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_previewer(mime)
    };
    let Some((id, name, wasm, caps, settings)) = resolved else {
        // No consented previewer matches the mimetype: NOT an error. The
        // frontend falls back to the raw view. The bytes are not even read.
        return to_value(&methods::PluginPreviewResult { preview: None });
    };

    // 2) Read the CAPPED bytes via the engine (async, outside the lock). An
    // unreadable file (NotFound, permissions…) is an honest error that
    // propagates, not a silently empty preview. `len` caps it at the
    // provider; truncated in case some provider delivers more.
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(crate::plugins::PREVIEW_MAX_BYTES),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));

    // 2.5) §6.2 (#29): decodes to TEXT like the EMBEDDED path
    // (`Backend::plugin_preview`) — the guest must never assume UTF-8 over
    // raw bytes. `lossy` (#101) flags when decoding produced `�`. (Embedded↔
    // daemon behavior parity, rule 7 — this handler used to pass the raw
    // bytes to the guest.)
    let (content, lossy) = crate::plugins::decode_for_preview(bytes);

    // 3) Run outside the lock, in spawn_blocking (rule 2). The runtime is
    // `Send+Sync` but not `Clone`: the `Arc` is cloned. `mime` is `&'static`
    // → moved as-is into the closure.
    let runtime = Arc::clone(&shared.plugin_runtime);
    let output = crate::blocking::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        // P2 Task 4a: delivers `[config]` ALREADY resolved (Task 2) to the
        // previewer, just as `handle_plugin_run_command` already does for
        // commands.
        inst.set_settings(settings);
        inst.render_preview(mime, &content)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "preview task panicked"))?
    .map_err(|e| {
        // Redaction: the detail (possible .wasm path / wasmtime internals)
        // goes ONLY to the local log; to the client, a generic message.
        tracing::warn!(plugin = %id, error = %e, "preview runtime failed");
        RpcError::protocol(codes::INTERNAL_ERROR, "preview runtime failed")
    })?;
    to_value(&methods::PluginPreviewResult {
        preview: Some(methods::PluginPreview {
            plugin_id: id,
            plugin_name: name,
            output,
            lossy,
        }),
    })
}

/// `plugin.preview_styled` (G3a, ADR 0037): STYLED twin of
/// [`handle_plugin_preview`]. Same three steps and the same read gate (#80) —
/// OPEN like its plain twin, previewing consents to nothing.
///
/// Differs from the plain twin in step 3 (execution) and in its failure
/// redaction: a RUNTIME failure in `render-styled` (trap, guest logic error,
/// or the ADR 0037 table's caps exceeded via
/// `RuntimeError::StyledPreviewTooLarge`) answers `preview: None`, NEVER
/// `INTERNAL_ERROR` — the styled preview is an ENRICHMENT over the plain one
/// (ADR 0037: "a client re-validates and falls back to `plugin.preview` if
/// [the caps] are violated"; here the SERVER already gets ahead with the
/// same criterion so as not to force a client to distinguish "there is no
/// preview" from "the preview failed" when the observable outcome — falling
/// back to plain — is identical). The failure's detail goes ONLY to the
/// local log (same redaction as the plain twin).
///
/// Each [`methods::SpanWire`]'s `role` travels UNVALIDATED: `norte-core`
/// (headless) does not depend on `norte-theme` — see the rustdoc of
/// [`crate::plugins::to_wire_lines`] and of `Backend::plugin_preview_styled`
/// for the full reasoning behind that boundary.
/// `plugin.thumbnail` (ADR 0107): the same path as `plugin.preview` — read
/// gate, resolve under the lock, read CAPPED outside it, run the guest in
/// `spawn_blocking` — and one difference: a failing guest is NOT a method
/// error. A thumbnail is cosmetic: "there isn't one" is the honest answer,
/// and the viewer keeps what it had. Only an unreadable file propagates.
#[tracing::instrument(skip_all)]
async fn handle_plugin_thumbnail(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginThumbnailParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let mime = crate::plugins::guess_mimetype(&p.path);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_thumbnailer(mime)
    };
    let Some((id, name, wasm, caps, settings)) = resolved else {
        return to_value(&methods::PluginThumbnailResult { thumbnail: None });
    };

    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(crate::plugins::THUMBNAIL_MAX_BYTES),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= crate::plugins::THUMBNAIL_MAX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(crate::plugins::THUMBNAIL_MAX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));

    let runtime = Arc::clone(&shared.plugin_runtime);
    let max_edge = p.max_edge;
    let outcome = crate::blocking::spawn_blocking(move || {
        let mut inst = runtime.instantiate_thumbnail(&wasm, caps)?;
        inst.set_settings(settings);
        inst.render_thumbnail(mime, &bytes, max_edge)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "thumbnail task panicked"))?;
    let thumb = match outcome {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(plugin = %id, error = %e, "thumbnail runtime failed");
            return to_value(&methods::PluginThumbnailResult { thumbnail: None });
        }
    };
    to_value(&methods::PluginThumbnailResult {
        thumbnail: Some(methods::PluginThumbnail {
            plugin_id: id,
            plugin_name: name,
            mimetype: thumb.mimetype.to_owned(),
            bytes: thumb.bytes,
            width: thumb.width,
            height: thumb.height,
        }),
    })
}

/// `plugin.panel_render` (0.74.0, phase 3): the frame a `panel` plugin paints
/// into a layout slot.
///
/// Same treatment as its cosmetic twins: OPEN, with a read gate over the
/// directory the panel accompanies, and NO FRAME on any stumble — no plugin,
/// no consent, broken/trapped/slow guest — which on the wire is `{}` and not
/// `null` (the result goes with `flatten`). A panel that does not answer
/// leaves the slot with whatever frame it last had; nothing goes down.
///
/// What the guest actually READS does not go through here: it goes through
/// `norte:location`, with its consented prefix, its call budget and its
/// audit.
#[tracing::instrument(skip_all)]
async fn handle_plugin_panel_render(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginPanelRenderParams = parse_params(params)?;
    read_gate(actor, &p.dir, shared)?;
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_panel(&p.plugin_id, &p.kind)
    };
    // The whole tuple, unopened: what is done with it — minting the
    // location, instantiating, painting — is the SAME function the embedded
    // backend uses, and opening it here would mean starting to decide
    // separately (ADR 0077).
    let Some(resolved) = resolved else {
        return to_value(&methods::PluginPanelRenderResult { frame: None });
    };
    let log_id = resolved.0.clone();

    // The state the guest saved last time. The wire already brings it
    // decoded and CAPPED (`PANEL_MAX_STATE_BYTES` at deserialization): a
    // state that does not fit invalidates the whole message before it gets
    // here, which is what stops any client from making the daemon decode and
    // copy megabytes to the guest.
    let state = p.state.clone().unwrap_or_default();
    let context = norte_plugin_host::panel_iface::PanelContext {
        cols: p.cols,
        rows: p.rows,
        lang: p.lang.clone(),
        cursor_name: p.cursor_name.clone(),
    };
    let event = crate::plugins::panel_event_to_host(&p.event);

    let runtime = Arc::clone(&shared.plugin_runtime);
    let kind = p.kind.clone();
    // The location's root is `p.dir` ITSELF, not its parent. Columns goes up
    // one level because what reaches it are files and it needs the directory
    // containing them; here the parameter already IS the directory, and
    // going up would give the guest a level above what the reader is
    // looking at — exactly the failure the columns gate documents (#239).
    //
    // And only the HUMAN climbs to look for the project root (`.git`): an
    // agent is bounded to its scope, and climbing above it is what the read
    // gate prevents.
    let dir = p.dir.clone();
    let climb = matches!(actor, Actor::User);
    let outcome = crate::blocking::spawn_blocking(move || {
        crate::plugins::render_panel_blocking(
            &runtime,
            resolved,
            &crate::plugins::PanelCall {
                dir: &dir,
                climb,
                kind: &kind,
                context: &context,
                state: &state,
                event: &event,
            },
        )
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "panel task panicked"))?;
    let (id, frame) = match outcome {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(plugin = %log_id, error = %e, "panel runtime failed");
            return to_value(&methods::PluginPanelRenderResult { frame: None });
        }
    };
    to_value(&methods::PluginPanelRenderResult {
        frame: Some(crate::plugins::panel_frame_to_wire(id, frame)),
    })
}

#[tracing::instrument(skip_all)]
async fn handle_plugin_preview_styled(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginPreviewStyledParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let mime = crate::plugins::guess_mimetype(&p.path);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_previewer(mime)
    };
    let Some((id, name, wasm, caps, settings)) = resolved else {
        return to_value(&methods::PluginPreviewStyledResult { preview: None });
    };

    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(crate::plugins::PREVIEW_MAX_BYTES),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= crate::plugins::PREVIEW_MAX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(crate::plugins::PREVIEW_MAX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));

    // §6.2 (#29): decodes to TEXT (parity with the embedded one, rule 7);
    // `lossy` (#101) to the frontend for the notice.
    let (content, lossy) = crate::plugins::decode_for_preview(bytes);

    let runtime = Arc::clone(&shared.plugin_runtime);
    let outcome = crate::blocking::spawn_blocking(move || {
        let mut inst = runtime.instantiate(&wasm, caps)?;
        inst.set_settings(settings);
        inst.render_styled_preview(
            mime,
            &content,
            crate::plugins::clamp_preview_columns(p.columns),
        )
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "styled preview task panicked"))?;

    let lines = match outcome {
        Ok(lines) => lines,
        Err(e) => {
            tracing::warn!(
                plugin = %id,
                error = %e,
                "render-styled failed: preview:None (falls back to plugin.preview)"
            );
            return to_value(&methods::PluginPreviewStyledResult { preview: None });
        }
    };
    to_value(&methods::PluginPreviewStyledResult {
        preview: Some(methods::PluginPreviewStyled {
            plugin_id: id,
            plugin_name: name,
            lines: crate::plugins::to_wire_lines(lines),
            lossy,
        }),
    })
}

/// READ gate for agents over a BATCH of paths (G3b): same criterion as
/// [`read_gate`] applied to EACH element of `paths`, cutting at the FIRST
/// denial (fail-fast, does not accumulate partially). An `Actor::User`
/// remains unsandboxed (a single cheap check would be enough, but walking
/// all of them keeps the code symmetric and does not change the result); an
/// `Actor::Agent` decorates/values columns ONLY over entries it could
/// already list under its scope — without this gate, an out-of-scope agent
/// could use `plugin.decorate`/`plugin.column_values` as an
/// existence/name oracle for paths outside its sandbox.
fn read_gate_all(
    actor: &Actor,
    paths: &[norte_proto::VPath],
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    for path in paths {
        read_gate(actor, path, shared)?;
    }
    Ok(())
}

/// `plugin.rename_plan` (C3, ADR 0095): the plan the `plugin_id` plugin's
/// `renamer_id` renamer PROPOSES for `names` in `dir`. Returns the SAME type
/// as `ai.rename_plan`: frontends review it and execute it via
/// `fs.rename_batch_plan` / `fs.rename_batch`, where the checking, the
/// policy and the journal live. This mutates nothing.
///
/// READ gate over `dir` (#80): a scopeless agent does not hand a plugin a
/// directory it cannot read, nor receive a plan that tells it what is
/// inside. Without location permission — impossible here, because the gate
/// already closed it off — the plugin would run without a token.
#[tracing::instrument(skip_all)]
async fn handle_plugin_rename_plan(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginRenamePlanParams = parse_params(params)?;
    read_gate(actor, &p.dir, shared)?;
    // The SAME cap as `ai.rename_plan`: `names` comes from outside, and the
    // guest's cap (`MAX_RENAME_PROPOSALS`) bounds what comes OUT, not what
    // goes in.
    if p.names.len() > methods::AI_RENAME_NAMES_MAX {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("names exceeds {}", methods::AI_RENAME_NAMES_MAX),
        ));
    }
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_renamer(&p.plugin_id, &p.renamer_id)
    };
    let Some(resolved) = resolved else {
        return Err(RpcError::from(norte_proto::Error::NotFound));
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let climb = matches!(actor, Actor::User);
    let (plugin_id, renamer_id, dir, names) = (p.plugin_id, p.renamer_id, p.dir, p.names);
    let output = crate::blocking::spawn_blocking(move || {
        crate::plugins::run_rename_plan(&runtime, resolved, &renamer_id, Some(&dir), climb, &names)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "rename plan task panicked"))?;
    match output {
        crate::plugins::RenamePlanOutcome::Plan(entries) => {
            to_value(&methods::AiRenamePlanResult {
                entries,
                refused: None,
            })
        }
        // Refusing is not an error (#332): an empty plan with a reason. The
        // phrase belongs to a third party: masked and capped BEFORE crossing
        // the wire.
        crate::plugins::RenamePlanOutcome::Refused(phrase) => {
            tracing::info!(plugin = %plugin_id, motivo = %phrase, "renamer: refused");
            to_value(&methods::AiRenamePlanResult {
                entries: Vec::new(),
                refused: Some(crate::plugins::guest_reason(&phrase)),
            })
        }
        crate::plugins::RenamePlanOutcome::Failed => {
            Err(RpcError::from(norte_proto::Error::Io { retryable: false }))
        }
    }
}

/// `plugin.organize_plan` (0.77.0, phase 8): the plan of an `organizer`-kind
/// plugin. Same gates as the renamer's, plus one more response.
async fn handle_plugin_organize_plan(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginOrganizePlanParams = parse_params(params)?;
    read_gate(actor, &p.dir, shared)?;
    if p.names.len() > methods::AI_RENAME_NAMES_MAX {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("names exceeds {}", methods::AI_RENAME_NAMES_MAX),
        ));
    }
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_organizer(&p.plugin_id, &p.organizer_id)
    };
    let Some(resolved) = resolved else {
        return Err(RpcError::from(norte_proto::Error::NotFound));
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let climb = matches!(actor, Actor::User);
    let (plugin_id, organizer_id, dir, names) = (p.plugin_id, p.organizer_id, p.dir, p.names);
    // The plan is tied to THIS directory, and the spawn takes its own: the
    // token has to come from the same dir passed to the plugin.
    let plan_dir = dir.clone();
    let output = crate::blocking::spawn_blocking(move || {
        crate::plugins::run_organize_plan(
            &runtime,
            resolved,
            &organizer_id,
            Some(&dir),
            climb,
            &names,
        )
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "organize plan task panicked"))?;
    match output {
        crate::plugins::OrganizePlanOutcome::Plan(moves) => {
            // The token travels WITH the plan, same as in `ai.organize_plan`:
            // a plugin's plan and a model's are approved through the same
            // path, and that includes how they get redeemed.
            let plan_hash = if moves.is_empty() {
                None
            } else {
                Some(crate::organize::plan_hash(&plan_dir, &moves).map_err(RpcError::from)?)
            };
            to_value(&methods::AiOrganizePlanResult {
                moves,
                refused: None,
                plan_hash,
            })
        }
        crate::plugins::OrganizePlanOutcome::Refused(phrase) => {
            tracing::info!(plugin = %plugin_id, motivo = %phrase, "organizer: refused");
            to_value(&methods::AiOrganizePlanResult {
                moves: Vec::new(),
                refused: Some(crate::plugins::guest_reason(&phrase)),
                plan_hash: None,
            })
        }
        // Proposing a write outside the directory is not an I/O failure and
        // is not counted as one: it is `InvalidPath`, exactly what happened,
        // and the operator has the plugin's id in the trace.
        crate::plugins::OrganizePlanOutcome::Escapes => {
            Err(RpcError::from(norte_proto::Error::InvalidPath))
        }
        crate::plugins::OrganizePlanOutcome::Failed => {
            Err(RpcError::from(norte_proto::Error::Io { retryable: false }))
        }
    }
}

/// The location minted for a columns plugin: the page's parent directory,
/// **if the actor could read it itself** (#239).
///
/// A separate function with the permission as a predicate, for the same
/// reason as `send_to_conn_impl`: the decision is testable without standing
/// up a `Shared`, and what needs pinning down is that the parent goes
/// through a gate — it used to go through none, and the handler's comment
/// claimed the opposite.
fn permitted_location(
    first: Option<&norte_proto::VPath>,
    allowed: impl Fn(&norte_proto::VPath) -> bool,
) -> Option<norte_proto::VPath> {
    let parent = first.and_then(norte_proto::VPath::parent)?;
    allowed(&parent).then_some(parent)
}

/// `plugin.decorate` (G3b, ADR 0037 decision 2): the OVERLAY of ALL APPROVED
/// and ENABLED `decorator` plugins over `params.paths` (batched,
/// POSITIONAL 1:1 — see the rustdoc of
/// [`norte_proto::methods::PluginDecorateResult`]). OPEN like its
/// `plugin.preview*` twins (decorating consents to nothing) but with the
/// SAME read gate (#80) as them, extended to the WHOLE batch
/// ([`read_gate_all`]): without it, an agent would exfiltrate the
/// existence/names of paths outside its scope by asking for decorations
/// over them.
///
/// Fail-closed PER PLUGIN (never per batch): a plugin that fails to
/// instantiate, traps, or breaks the positional contract is OMITTED from the
/// result with a warning in the local log — the rest of the page still gets
/// painted. Empty `params.paths` answers `{plugins: []}` without resolving
/// the catalogue.
#[tracing::instrument(skip_all)]
async fn handle_plugin_decorate(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginDecorateParams = parse_params(params)?;
    read_gate_all(actor, &p.paths, shared)?;
    if p.paths.is_empty() {
        return to_value(&methods::PluginDecorateResult {
            plugins: Vec::new(),
        });
    }
    // Short or empty is a 0.71 client and is fine; LONGER is a broken client,
    // and silently truncating it would hide the error forever.
    if p.kinds.len() > p.paths.len() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "kinds is longer than paths",
        ));
    }
    let expected_len = p.paths.len();
    let entries = crate::plugins::paths_to_entries(&p.paths, &p.kinds);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.resolve_decorators()
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let plugins = crate::blocking::spawn_blocking(move || {
        let mut out = Vec::new();
        for ((id, _name, wasm, caps, settings), slot) in resolved {
            let Ok(mut inst) = runtime.instantiate_decorator(&wasm, caps) else {
                tracing::warn!(plugin = %id, "decorator: failed to instantiate, omitted from the batch");
                continue;
            };
            inst.set_settings(settings);
            let Ok(raw) = inst.decorate(&entries) else {
                tracing::warn!(plugin = %id, "decorator: failed to run decorate, omitted from the batch");
                continue;
            };
            let Some(decorations) = crate::plugins::decorations_to_wire_checked(raw, expected_len)
            else {
                tracing::warn!(
                    plugin = %id,
                    "decorator: length does not match the positional contract, omitted from the batch"
                );
                continue;
            };
            out.push(methods::PluginDecorations {
                plugin_id: id,
                slot: crate::plugins::slot_to_wire(slot),
                decorations,
            });
        }
        out
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "decorate task panicked"))?;
    to_value(&methods::PluginDecorateResult { plugins })
}

/// `plugin.column_values` (G3b, ADR 0037 decision 2): values for the
/// `params.column_id` column for `params.paths`, from the SINGLE `columns`
/// plugin that declares it ([`crate::PluginRegistry::resolve_columns`],
/// first-to-match — unlike `plugin.decorate`). Same per-batch read gate and
/// same entry contract (basenames) as `handle_plugin_decorate`.
///
/// Fail-closed: if the plugin fails to instantiate, traps, or breaks the
/// positional contract, the result is a vector of `None` the size of
/// `params.paths` (an empty cell for the whole page) instead of an error.
#[tracing::instrument(skip_all)]
async fn handle_plugin_column_values(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginColumnValuesParams = parse_params(params)?;
    read_gate_all(actor, &p.paths, shared)?;
    if p.paths.is_empty() {
        return to_value(&methods::PluginColumnValuesResult { values: Vec::new() });
    }
    let expected_len = p.paths.len();
    let entries = crate::plugins::paths_to_basenames(&p.paths);
    let resolved = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        // `plugin_id` present = THAT plugin or none (#120). Absent = a 0.34
        // client: the old first-to-match is kept.
        reg.resolve_columns_of(p.plugin_id.as_deref(), &p.column_id)
    };
    let Some(resolved) = resolved else {
        return to_value(&methods::PluginColumnValuesResult {
            values: vec![None; expected_len],
        });
    };
    let runtime = Arc::clone(&shared.plugin_runtime);
    let column_id = p.column_id;
    // The LOCATION is the page's parent directory (ADR 0057), and **it goes
    // through its own read gate** (#239).
    //
    // The comment that used to be here said the parent came gated "from
    // above" because the paths were. Not true: `read_gate_all` looks at the
    // PATHS, and a path's parent within scope can be outside it. And a scope
    // root is within its own scope — a test pins this down — so an agent
    // with scope over `file:///home/u/work` would ask for columns OVER that
    // root and the plugin would receive a root confined to
    // `file:///home/u`: the whole home, one level above its sandbox, with no
    // need for the climb to a marker (which was already disabled for
    // agents). With a scope pointing at a bare file, the directory
    // containing it.
    //
    // Without permission it is NOT minted: the plugin runs with no location
    // and its column comes out blank. This is an honest degradation — denying
    // the whole call would turn the column into an oracle of which
    // directories exist outside the scope, exactly the leak this gate
    // closes.
    let location = permitted_location(p.paths.first(), |parent| {
        let allowed = read_gate(actor, parent, shared).is_ok();
        if !allowed {
            tracing::debug!("location outside scope: the plugin runs without it");
        }
        allowed
    });
    let climb = matches!(actor, Actor::User);
    let pool = Arc::clone(&shared.column_pool);
    let values = crate::blocking::spawn_blocking(move || {
        pool.column_values(
            &runtime,
            resolved,
            &column_id,
            location.as_ref(),
            // Only the human climbs to look for the project root: an agent
            // or a plugin are bounded to their scope, and climbing above it
            // is exactly what the read gate prevents.
            climb,
            &entries,
            expected_len,
        )
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "column_values task panicked"))?;
    to_value(&methods::PluginColumnValuesResult { values })
}

/// `plugin.get_config` (0.28.0, G3c, ADR 0037): `[config]` schema + EFFECTIVE
/// value of `id`, one per key. OPEN to any connection (reading a
/// schema/value consents to nothing, same criterion as `plugin.preview*`/
/// `plugin.decorate`). An unknown `id` answers `keys: []` (same lenient
/// criterion as `plugin.list` with an empty catalogue — never an error).
/// Cheap: only reads under the lock, no `spawn_blocking` (unlike
/// `plugin.decorate`/`column_values`, which instantiate WASM).
#[tracing::instrument(skip_all)]
async fn handle_plugin_get_config(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginGetConfigParams = parse_params(params)?;
    let keys = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.config_keys(&p.id).unwrap_or_default()
    };
    let keys = keys
        .into_iter()
        .map(|(key, spec, value)| crate::plugins::config_key_to_wire(key, &spec, value))
        .collect();
    to_value(&methods::PluginGetConfigResult { keys })
}

/// `plugin.help` (H3e, 0.34.0): a plugin's help page, for ANY connection —
/// same criterion as `plugin.list`/`plugin.get_config`: reading documentation
/// consents to nothing.
///
/// The `id` is a KEY against the catalogue, never a path component
/// (`PluginRegistry` resolves it against discovered plugins), so an id with
/// `../` fails the lookup instead of leaving the directory. An unknown id is
/// `INVALID_PARAMS`, the same treatment `plugin.set_approval` gives a phantom
/// plugin.
///
/// Not a single syscall under the lock: here there is a file of up to
/// [`methods::PLUGIN_HELP_MAX_BYTES`] that can live on a slow or hostile
/// mount, and both the escape guard (three syscalls) and reading inside the
/// async handler while holding the registry's `std::Mutex` would violate
/// rule 2 and queue up every other connection behind it. That is why the
/// lock only resolves the id against the catalogue — pure memory — and
/// returns an OPAQUE `HelpJob`; verifying and reading happen in
/// `spawn_blocking`, with the lock already released. The job is opaque on
/// purpose: this handler never gets to hold a path that could be re-derived
/// from the wire id.
// `skip_all` WITHOUT the id: it comes raw off the wire and must not reach the
// log before being validated against the catalogue (same criterion as
// `handle_plugin_set_approval`).
#[tracing::instrument(skip_all)]
async fn handle_plugin_help(
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::PluginHelpParams = parse_params(params)?;
    // The lock is released when the block closes, BEFORE any `.await`.
    let job = {
        let reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.help_job(&p.id)
    };
    let Some(job) = job else {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "unknown plugin id",
        ));
    };
    // `HelpJob::read` is also what CAPS the read (`max_bytes + 1`): a sparse
    // 100 GiB `help.md` cannot turn this call into a 100 GiB allocation. See
    // its rustdoc.
    let page = crate::blocking::spawn_blocking(move || job.read())
        .await
        .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "plugin help task panicked"))?;
    to_value(&page)
}

/// `plugin.set_config` (0.28.0, G3c, ADR 0037): persists ONE `[config]`
/// value, validated against the manifest's SCHEMA (the SAME validation as
/// `config.toml`, via `PluginRegistry::set_config`). Plugin settings are USER
/// DATA, not a capabilities-consent act — but it REMAINS human-only (same
/// criterion as [`handle_plugin_set_approval`]/[`handle_plugin_set_enabled`]:
/// an agent does not reconfigure a plugin on its own). An unknown id/key or
/// an invalid value are `INVALID_PARAMS`; NOTHING is persisted in that case
/// (`PluginRegistry::set_config` validates BEFORE writing).
// `skip_all` without `id`/`key`: raw off the wire, not yet validated (same
// criterion as set_approval/set_enabled — only logged after confirming).
#[tracing::instrument(skip_all)]
async fn handle_plugin_set_config(
    actor: &Actor,
    p: &methods::PluginSetConfigParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may change a plugin's settings",
        ));
    }
    let id = p.id.clone();
    let key = p.key.clone();
    let value = p.value.clone();
    // The lock is held DURING the write (unlike set_approval/set_enabled,
    // which release it before persisting): deliberate — it serializes
    // `config.toml`'s read-modify-write for THIS plugin against a concurrent
    // `set_config` on another key of the same plugin, which without it could
    // lose a write (two interleaved read-modify-writes of `config.toml`).
    // Still runs in `spawn_blocking` (rule 2: the write + the
    // re-`resolve_settings` are synchronous I/O), so the async reactor never
    // blocks — only a thread from the blocking pool holds the lock.
    let shared = Arc::clone(shared);
    crate::blocking::spawn_blocking(move || {
        let mut reg = shared.plugins.lock().expect("plugins lock is sound");
        reg.set_config(&id, &key, &value)
    })
    .await
    .map_err(|_| RpcError::protocol(codes::INTERNAL_ERROR, "set_config task panicked"))?
    .map_err(|e| RpcError::protocol(codes::INVALID_PARAMS, e.to_string()))?;
    tracing::info!(id = %p.id, key = %p.key, "plugin setting changed by the human");
    to_value(&methods::PluginSetConfigResult {})
}

/// Expiration instant of a scope from its `ttl_ms`: clamped to
/// `[1, MAX_SCOPE_TTL_MS]` (neither 0 = a useless already-expired one, nor
/// near-perpetual) and saturating addition (never panics on clock overflow).
fn scope_deadline(ttl_ms: u64) -> Instant {
    let ms = ttl_ms.clamp(1, MAX_SCOPE_TTL_MS);
    let now = Instant::now();
    now.checked_add(Duration::from_millis(ms)).unwrap_or(now)
}

/// READ gate for agents (closes #80). SOLE source of the criterion
/// `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities` and `fs.search` all
/// consult today: an `Actor::Agent` only reads under a LIVE scope of its
/// session ([`ScopeRegistry::covers_read`], root membership independent of
/// op — reading is strictly less than any mutation); a `User` (human) is not
/// sandboxed; ANY other actor (`Plugin` today, or future variants) is denied
/// WITHOUT exception (default-deny for what we do not yet know how to
/// govern; hence the lint's `allow` — naming `Plugin` would let a future
/// variant through ungated). The verdict is `PolicyDenied` with the coarse
/// category from the closed vocabulary ([`DenyReason::rule_id`]), never the
/// specific rule nor the `path` (no leak in the error nor in the M3-5 audit
/// trace).
///
/// Symmetric with M3's MUTATION gate (`Engine::gate`): the AI is a citizen,
/// not an owner (spec §1.5).
fn read_gate(
    actor: &Actor,
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    use crate::policy::{DenyReason, ScopeVerdict};
    #[expect(
        clippy::match_wildcard_for_single_variants,
        reason = "the grouped variants read better than enumerated"
    )]
    let denied: Option<DenyReason> = match actor {
        Actor::User => None,
        Actor::Agent { session } => {
            match shared.scopes.covers_read(session, path, Instant::now()) {
                ScopeVerdict::Within => None,
                ScopeVerdict::Expired => Some(DenyReason::ScopeExpired),
                ScopeVerdict::OutOfScope => Some(DenyReason::OutOfScope),
            }
        }
        _ => Some(DenyReason::OutOfScope),
    };
    if let Some(reason) = denied {
        // Traceable (M3-5 audit), like the mutation gate. Only the coarse
        // category and the actor: never the denied path.
        tracing::warn!(
            rule = reason.rule_id(),
            "read with no scope: denied (default-deny)"
        );
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        }));
    }
    Ok(())
}

/// CONTENT gate for agents (`fs.compare` with the hash rung, C6):
/// [`read_gate`] plus the requirement that the scope grant an op that
/// handles BYTES ([`ScopeRegistry::covers_content`](crate::policy::ScopeRegistry::covers_content)).
///
/// Applied IN ADDITION to the read gate, never instead of it: reading
/// decides whether the actor can look at the subtree, this one decides
/// whether it can make ONE call read both trees WHOLE. A `User` (human) is
/// not sandboxed and any actor other than `User`/`Agent` is denied, same as
/// there.
///
/// The verdict is the same `PolicyDenied` with the coarse category: whoever
/// asks for too much does not find out which gate it was missing, only that
/// it lacks scope (no leak in the error nor the trace).
fn content_gate(
    actor: &Actor,
    path: &norte_proto::VPath,
    shared: &Arc<Shared>,
) -> Result<(), RpcError> {
    use crate::policy::{DenyReason, ScopeVerdict};
    #[expect(
        clippy::match_wildcard_for_single_variants,
        reason = "the grouped variants read better than enumerated"
    )]
    let denied: Option<DenyReason> = match actor {
        Actor::User => None,
        Actor::Agent { session } => {
            match shared.scopes.covers_content(session, path, Instant::now()) {
                ScopeVerdict::Within => None,
                ScopeVerdict::Expired => Some(DenyReason::ScopeExpired),
                ScopeVerdict::OutOfScope => Some(DenyReason::OutOfScope),
            }
        }
        _ => Some(DenyReason::OutOfScope),
    };
    if let Some(reason) = denied {
        tracing::warn!(
            rule = reason.rule_id(),
            "CONTENT read with no scope: denied (default-deny)"
        );
        return Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        }));
    }
    Ok(())
}

/// `connection.close` (0.49.0, #140): releases a path's remote session.
///
/// READ gate over the path, the same criterion as for looking at it: closing
/// a connection destroys no data — the next operation reconnects — but it
/// does interrupt whoever was using it, and whoever cannot even read there
/// has no reason to be able to do that.
///
/// Humans ONLY: disconnecting is a decision for whoever is in front of the
/// screen. An agent that could close its human's session would have a free
/// denial-of-service lever, with no use for any of its own work.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_connection_close(
    actor: &Actor,
    params: Option<serde_json::Value>,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ConnectionCloseParams = parse_params(params)?;
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection closes a session",
        ));
    }
    read_gate(actor, &p.path, shared)?;
    let closed = shared.engine.close_connection(&p.path);
    to_value(&methods::ConnectionCloseResult { closed })
}

/// `fs.dir_size` (0.49.0, #139): how much what is asked for takes up, as a
/// Task.
///
/// READ gate over EACH root, before validating anything else: an actor with
/// no rights over what it asks for never finds out whether its request was
/// also malformed. Walking a tree reveals its SHAPE — how many things there
/// are and what the directories along the way are called — which is exactly
/// what a listing reveals, hence the same gate.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_dir_size(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsDirSizeParams = parse_params(params)?;
    for path in &p.paths {
        read_gate(actor, path, shared)?;
    }
    if p.paths.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.dir_size: paths must not be empty",
        ));
    }
    let handle = shared
        .engine
        .dir_size_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `dir_size_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `fs.checksum` (0.59.0, #311): the digest of a batch's content, as a Task.
///
/// READ **and CONTENT** gate over EACH path (ADR 0080).
///
/// The read one for the same reason as in `fs.dir_size`. The content one
/// because this does not read the tree's shape but each file's BYTES, and a
/// digest is a fingerprint of them: `fs.checksum` subsumes — and exceeds —
/// the oracle that `fs.compare` with the hash rung already gates through the
/// narrow door. That one answers "are they equal?" and forces PLACING the
/// candidate; this one returns the sha256, which is then compared against a
/// dictionary without placing anything. Without this, an agent denied
/// `fs.compare` with `criteria.hash` would only have to call here instead.
///
/// `policy.rs` states it as a rule: the narrow door is preferred for what is
/// NEW, because loosening it later is additive and tightening it is not.
///
/// The CAP is checked BEFORE the gates, and it leaks nothing: it is a public
/// constant and the sender knows how many paths it sent. The other way
/// around it did cost something — a batch of a million paths used to take
/// the scope registry's mutex once per path before anyone looked at the cap.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_checksum(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsChecksumParams = parse_params(params)?;
    if p.paths.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.checksum: paths must not be empty",
        ));
    }
    // `InvalidPath` and not a bare `-32602`: that one carries no category in
    // `data`, so the remote Backend would deliver it as `Internal` while the
    // embedded one says `InvalidPath` — two different answers to the same
    // event depending on which door it came through. This is `check_pairs_cap`'s
    // lesson.
    if p.paths.len() > methods::FS_CHECKSUM_MAX_PATHS {
        return Err(RpcError::from(norte_proto::Error::InvalidPath));
    }
    for path in &p.paths {
        read_gate(actor, path, shared)?;
        content_gate(actor, path, shared)?;
    }
    let handle = shared
        .engine
        .checksum_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit and this
    // register — the Task never runs OUTSIDE `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `fs.checksum_report` (0.59.0, #311): the digests that Task computed.
///
/// Same visibility as a rename batch's report, and for the same reason: a
/// single answer — `NotFound` — for all three situations (evicted, never was
/// a checksum batch, belongs to another actor), because separating the third
/// would confirm that someone else's task existed.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_fs_checksum_report(
    actor: &Actor,
    p: &methods::FsChecksumReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .checksum_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        tracing::warn!(
            actor = ?actor,
            "checksum report of another actor: denied (response = unknown id)"
        );
        return Err(unknown());
    }
    to_value(&report)
}

/// `fs.dir_usage` (0.75.0, phase 4): what a directory is made of, as a Task.
///
/// READ gate over the root, for the same reason as in `fs.dir_size`: without
/// it, an out-of-scope agent would enumerate someone else's tree through this
/// call's errors.
///
/// **No CONTENT gate**, and that is the difference with `fs.checksum`: that
/// one reads each file's BYTES and returns a fingerprint of them; this one
/// only measures the tree's SHAPE — names and sizes, the same thing a
/// listing already returns — and does not open a single file. Demanding the
/// narrow door here would close the map off to whoever can already list the
/// whole directory.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_dir_usage(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsDirUsageParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let handle = shared
        .engine
        .dir_usage_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `dir_usage_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `fs.dir_usage_report` (0.75.0, phase 4): the map that Task measured.
///
/// Same visibility as the checksum report, and for the same reason: a single
/// answer — `NotFound` — for all three situations (evicted, never was a map,
/// belongs to another actor), because separating the third would confirm
/// someone else's task existed.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_fs_dir_usage_report(
    actor: &Actor,
    p: &methods::FsDirUsageReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .dir_usage_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        tracing::warn!(
            actor = ?actor,
            "disk map of another actor: denied (response = unknown id)"
        );
        return Err(unknown());
    }
    to_value(&report)
}

/// `archive.pack` (0.50.0, #132): builds an archive, as a Task.
///
/// The engine does the MUTATION gate (same op as a copy: the sources are read
/// and the destination is written). Here goes the READ one over the sources,
/// for the same reason as in `fs.dir_size`: without it, an out-of-scope agent
/// would enumerate someone else's tree through this call's errors.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_archive_pack(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchivePackParams = parse_params(params)?;
    if p.sources.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "archive.pack: sources must not be empty",
        ));
    }
    for path in &p.sources {
        read_gate(actor, path, shared)?;
    }
    let handle = shared
        .engine
        .pack_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the submit and this register.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `archive.pack_report` (0.58.0, #250): what that packing saved that does
/// not survive leaving here.
///
/// Exact twin of `archive.test_report`, visibility included: only whoever
/// launched the Task sees it, and another actor's id is answered the same as
/// one that does not exist.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_archive_pack_report(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchivePackReportParams = parse_params(params)?;
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .archive_pack_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        tracing::warn!(actor = ?actor, "archive.pack_report of another actor");
        return Err(unknown());
    }
    to_value(&report)
}

/// `archive.test` (0.50.0, #132): verifies an archive, as a Task.
///
/// Mutates nothing, so only a READ gate. The report is collected afterward
/// with [`methods::ARCHIVE_TEST_REPORT`], same as a rename batch's: a Task
/// returns no value, and "which entry is corrupt" does not fit in a
/// `Failed`.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_archive_test(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchiveTestParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let (handle, _report) = shared
        .engine
        .test_archive_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `archive.test_report` (0.50.0, #132): the report of a test already
/// launched.
///
/// Exact twin of `fs.rename_batch_report`, VISIBILITY included: only whoever
/// launched the Task sees it, and another actor's id is answered the same as
/// one that does not exist — saying "it exists but isn't yours" would
/// already be telling something.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
fn handle_archive_test_report(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::ArchiveTestReportParams = parse_params(params)?;
    // A single answer for all three situations — evicted from the ring,
    // never was a test, belongs to another actor — and the third is the
    // reason: separating it would confirm someone else's task existed.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .archive_test_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Audit material, like its twins: asking about other actors' reports
        // leaves a trace even if the answer says nothing.
        tracing::warn!(actor = ?actor, "archive.test_report of another actor");
        return Err(unknown());
    }
    to_value(&report)
}

/// `file.split` (0.50.0, #132): splits a file, as a Task.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_file_split(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FileSplitParams = parse_params(params)?;
    read_gate(actor, &p.path, shared)?;
    let handle = shared
        .engine
        .split_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `file.combine` (0.50.0, #132): joins the chunks back, as a Task.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_file_combine(
    params: Option<serde_json::Value>,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FileCombineParams = parse_params(params)?;
    read_gate(actor, &p.first, shared)?;
    // And the DIRECTORY, because the other chunks are derived by convention
    // and the request does not name them: a scope over the bare `.001` file
    // does not cover its siblings.
    if let Some(dir) = p.first.parent() {
        read_gate(actor, &dir, shared)?;
    }
    let handle = shared
        .engine
        .combine_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `fs.compare` (0.39.0, ADR 0048): compares two trees as a cancellable Task.
/// ROWS arrive via `compare.rows` ONLY to the `conn_id` connection that
/// launched it (directed send, never broadcast — same criterion as
/// `search.hits`).
///
/// Mutates NOTHING: no journal, no undo, not a byte is written. **Hard rule 4
/// does not apply** — stated here so a later review does not ask for a
/// journal entry that would mean nothing.
///
/// GATE. Comparing READS two whole trees, so there are two doors:
/// - [`read_gate`] over **BOTH** roots (#80). One alone is not enough: the
///   tree not under scope would get listed all the same, and its names would
///   travel in the rows.
/// - [`content_gate`] over both **when `criteria.hash` is on**: that rung
///   passes every byte of every paired file through a sha256, which is more
///   than what a listing reveals (see the tension noted in
///   `covers_content`).
///
/// `INVALID_PARAMS` WITHOUT creating a Task, in the same shape as
/// `fs.search`'s invalid criteria:
/// - Two EQUAL roots: comparing something against itself for an hour is not
///   a request, it is the caller's typo.
/// - `follow_symlinks: true`: the engine accepts the field and IGNORES it,
///   and silently serving a different walk than the one requested is worse
///   than not offering it.
///
/// (Both are a bare `-32602`, so a remote `Backend` delivers them as
/// `Internal` while the embedded one says `InvalidPath`/`Unsupported`. Same
/// asymmetry `fs.search`'s criteria already have, and the contract published
/// in `methods::FS_COMPARE` is the code, not the taxonomy.)
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_compare(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsCompareParams = parse_params(params)?;

    // Gate BEFORE validating params: an actor with no rights over the roots
    // never finds out whether its request was also malformed.
    read_gate(actor, &p.left, shared)?;
    read_gate(actor, &p.right, shared)?;
    if p.criteria.hash {
        content_gate(actor, &p.left, shared)?;
        content_gate(actor, &p.right, shared)?;
    }

    if p.left == p.right {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.compare: left and right resolve to the same root",
        ));
    }
    if p.follow_symlinks {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "fs.compare: follow_symlinks is not supported (link targets are compared as bytes)",
        ));
    }
    // `descend_orphans` is NOT validated here: it is a `DescendSide`, which
    // has no `serde(other)`, so an `"lft"` dies in `parse_params` above
    // (`-32602`) and `Side::Unknown` is not even representable. An `if` in
    // this handler would have left out the EMBEDDED arm, which calls the
    // engine without going through here.

    let (handle, mut rx) = shared
        .engine
        .compare_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `compare_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks` (with task.list/cancel and counting against the caps).
    // Whoever adds an await here breaks that guarantee.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // ROW pump: drains the walk's channel and routes each batch as
    // `compare.rows` ONLY to the owner. Dies on its own when the walk closes
    // `tx` (terminal, cancel, or the receiver — the owner itself — gone).
    //
    // And the other way around: as soon as a batch is NOT delivered — the
    // owner left, or was not draining its outbox and the daemon evicted it —
    // the pump STOPS. Dropping `rx`, the walk sees `ReceiverGone` and ends.
    // Without this, a three-hour comparison would keep reading two trees
    // (and hashing them) for nobody, holding its scheduler permit against the
    // rest of that scheme's work. `fs.compare` is the case that calls for
    // it: unlike `fs.search`, it has no `max_hits` to bound it.
    //
    // And what no longer happens (#155): stopping it does not cost it the
    // subscription, so the terminal `task.progress` — the only signal it has
    // to know it is missing rows — still reaches it.
    let shared_pump = Arc::clone(shared);
    // #155: while this pump is alive, its owner is not evicted from the
    // subscribers map for a full outbox — it would lose the terminal
    // `task.progress` it uses to compare received rows against
    // `entries_done`, the only way it has to know the comparison reached it
    // whole.
    let feed = shared_pump.feed_guard(conn_id);
    crate::blocking::spawn(async move {
        let _feed = feed;
        while let Some(rows) = rx.recv().await {
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::COMPARE_ROWS.into(),
                params: serde_json::to_value(&rows).ok(),
            };
            let Ok(frame) = encode_frame(&notif) else {
                continue;
            };
            if !shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice())) {
                tracing::debug!(
                    conn = conn_id,
                    "compare.rows with no owner: stopping the walk"
                );
                break;
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.plan` (0.40.0, ADR 0049): plans a ONE-way sync (`source` → `dest`)
/// as a cancellable Task. STEPS arrive via `sync.steps` ONLY to the `conn_id`
/// connection that launched it, and the plan is CLOSED by a `sync.plan_done`
/// through the same path.
///
/// Mutates NOTHING: underneath it is `fs.compare` with a decision per row.
/// What it does do is RETAIN the plan in a spool tied to this connection,
/// which is what lets `sync.apply` carry nothing more than a hash.
///
/// GATE. Planning READS two whole trees, so it is the same two doors as
/// [`handle_fs_compare`], and for the same reasons:
/// - [`read_gate`] over **BOTH** roots (#80).
/// - [`content_gate`] over both **when `compare.criteria.hash` is on**.
///
/// And they go **before** validating the params: an actor with no rights
/// over the roots never finds out whether its request was also malformed.
///
/// `INVALID_PARAMS` WITHOUT creating a Task, in the same shape as
/// `fs.compare`, for the two `compare` fields that in `sync.plan` **are not
/// the caller's** (`follow_symlinks` and `descend_orphans` — the planner
/// fixes the latter to the SOURCE side) and for an `include` above
/// [`methods::SYNC_MAX_INCLUDE`], which is refused instead of trimmed. The
/// engine also rejects them, with its own taxonomy, because the EMBEDDED arm
/// does not go through here.
///
/// Overlapping roots are NOT checked here: they are
/// [`Error::OverlappingRoots`](norte_proto::Error::OverlappingRoots), a wire
/// category, and the engine produces it in one single place for both paths.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_sync_plan(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::SyncPlanParams = parse_params(params)?;

    // Gate BEFORE validating params (see the doc's note).
    read_gate(actor, &p.source, shared)?;
    read_gate(actor, &p.dest, shared)?;
    if p.compare.criteria.hash {
        content_gate(actor, &p.source, shared)?;
        content_gate(actor, &p.dest, shared)?;
    }

    if p.compare.follow_symlinks {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "sync.plan: compare.follow_symlinks is not supported (link targets are compared as bytes)",
        ));
    }
    if p.compare.descend_orphans.is_some() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "sync.plan: compare.descend_orphans is set by the planner, not by the caller",
        ));
    }
    // The constant is the contract: the cap is named, never written out.
    let max_include = methods::SYNC_MAX_INCLUDE;
    if p.include
        .as_ref()
        .is_some_and(|inc| inc.len() > max_include)
    {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("sync.plan: include has more than {max_include} paths"),
        ));
    }

    // Cap of plans RETAINED per connection. An approved plan is a file with
    // two trees' relative listing, and while its connection is alive the
    // only thing that reclaims it is the TTL. Without this cap, a client
    // planning in a loop while varying `include` — each selection gives a
    // different digest, i.e. a different file — fills up the state
    // directory, which is where `journal.db` lives. `OVERLOADED` and not
    // `-32602`: the request is valid, the moment is not (same criterion and
    // same code as the live-tasks cap).
    if let Some(spool) = shared.engine.spool()
        && spool.retained_for(conn_id) >= MAX_RETAINED_SYNC_PLANS
    {
        // With a TAXONOMY in `data` and not just the phrase (#182): a
        // rejection with no taxonomy reaches the client as `Internal {
        // panic: false }` — "internal error" — because `to_taxonomy` has
        // nothing else to return, and to an agent that says "try again",
        // which is what filled this very cap in the first place.
        // `LimitExceeded` says what happened: the plan is valid, what ran
        // out is the budget.
        return Err(RpcError::from(norte_proto::Error::LimitExceeded {
            limit: norte_proto::Error::LIMIT_RETAINED_SYNC_PLANS.to_owned(),
        }));
    }

    let (handle, mut rx) = shared
        .engine
        .sync_plan_as(p, conn_id, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `sync_plan_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks`. Whoever adds an await here breaks that guarantee.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // STEPS pump: drains the plan's channel and routes each event ONLY to
    // the owner. A single channel carries the batches and the close, so the
    // order "`sync.steps`* then a `sync.plan_done`" does not depend on this
    // pump: it is the queue.
    //
    // As soon as an event is NOT delivered — the owner left, or was not
    // draining its outbox and the daemon evicted it — the pump STOPS.
    // Dropping `rx`, the Task sees its receiver disappeared, closes the
    // spool as INTERRUPTED (leaves no approvable plan) and ends. Without
    // this, a three-hour plan would keep walking two trees for nobody,
    // holding onto its scheduler permit.
    //
    // What no longer happens (#155): this feed's owner is not evicted from
    // the subscribers map while it lasts, so it keeps its terminal
    // `task.progress`. Here the failure used to be the least serious of the
    // three — a client with no `sync.plan_done` has no `plan_hash` and
    // cannot apply anything — but it is the same mechanism, and fixing it in
    // two of three pumps would leave it half-done.
    let shared_pump = Arc::clone(shared);
    let feed = shared_pump.feed_guard(conn_id);
    crate::blocking::spawn(async move {
        let _feed = feed;
        while let Some(event) = rx.recv().await {
            let (method, params) = match event {
                crate::sync::SyncPlanEvent::Steps(batch) => {
                    (methods::SYNC_STEPS, serde_json::to_value(&batch))
                }
                crate::sync::SyncPlanEvent::Done(done) => {
                    (methods::SYNC_PLAN_DONE, serde_json::to_value(&done))
                }
            };
            // Unlike `fs.compare`'s pump, a serialization failure does NOT
            // send the notification with `params: null`: here a lost batch
            // feeds an approval, and a frame no client can parse is worse
            // than none. It is unreachable with these types (flat structs),
            // and that is precisely why it is cheap to have here.
            let Ok(params) = params else {
                tracing::error!(method, "could not serialize a sync.plan event");
                break;
            };
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: method.into(),
                params: Some(params),
            };
            let Ok(frame) = encode_frame(&notif) else {
                break;
            };
            if !shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice())) {
                tracing::debug!(
                    conn = conn_id,
                    method,
                    "sync with no owner: stopping the plan"
                );
                // And its retained plans go too. This is the only observer of
                // delivery, and it arrives AFTER the connection's teardown: a
                // plan that closed between the teardown's sweep and this
                // point would stay retained forever — the `sync.plan_done`
                // fits in the channel's buffer, so the Task counts it as
                // delivered.
                drop_sync_plans(&shared_pump, conn_id).await;
                break;
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.apply` (0.40.0, ADR 0049): runs a RETAINED plan as a cancellable
/// Task. The only parameter is `plan_hash`, so by the SHAPE of the request
/// nothing can be run except what a human approved.
///
/// # Why there is NO gate here
/// Not because it is not needed, but because this handler does not know what
/// to ask it about: the two roots live in the SPOOL and `sync.apply` does not
/// carry them. The gate runs inside [`Engine::sync_apply_as`], over the paths
/// read from the file and at the moment of applying — which is also the
/// correct thing to do, because up to `SYNC_PLAN_TTL_MS` passes between
/// planning and applying, and a scope can expire within that window. A
/// `read_gate` here over something that is not the roots would be theater.
///
/// # The rejections belong to the engine, and this does not duplicate them
/// `Unsupported` (no spool or no journal), `PlanStale` (the hash does not
/// name a live plan of THIS connection — it does not exist, expired, was
/// tampered with, belongs to another connection, or is already being
/// applied), `PlanNotExecutable` (the plan carried blocks) and `PolicyDenied`
/// all come from `sync_apply_as`, because the `Backend`'s EMBEDDED arm calls
/// the engine without going through here. This handler delivers them with
/// their taxonomy intact.
///
/// A MALFORMED `plan_hash` never reaches the engine: `PlanHash` rejects it at
/// deserialization, i.e. `-32602`. And that matters — "this is not a hash"
/// and "the world moved" are different facts, and answering the second to
/// whoever sent the first lies to it about the state of the world.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_sync_apply(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::SyncApplyParams = parse_params(params)?;
    // The live-tasks cap is checked BEFORE opening the plan, not only in
    // `register_task_id`. Opening it takes away the RIGHT to apply it (it is
    // single-use) and the Task's body SPENDS it on any terminal state, so a
    // later `OVERLOADED` destroys the approved plan: the client is left with
    // no `task_id`, every retry of that hash answers `PlanStale` and the only
    // way out is walking both whole trees again. No other method has a
    // parameter this expensive to rebuild.
    //
    // It is TOCTOU — two simultaneous `sync.apply` can both pass and the
    // second die in `register_task_id` — and it is still worth it: it moves
    // the normal case from "plan destroyed" to "plan intact, try again",
    // which is what the error's message says. Same criterion as
    // `handle_sync_plan`'s retained-plans cap pre-check.
    if let Some(err) = tasks_at_capacity(shared, actor) {
        return Err(err);
    }
    let (handle, _report) = shared
        .engine
        .sync_apply_as(&p.plan_hash, conn_id, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // The report is NOT retained here: it lives in the engine's ring
    // (`Engine::sync_report`), which is what `sync.report` serves it from —
    // a single ring for the socket and for the embedded `Backend`, same as
    // `fs.rename_batch_report`'s.
    //
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `sync_apply_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks`.
    let task_id = register_task_id(shared, handle, actor.clone())?;
    to_value(&methods::FsTaskResult { task_id })
}

/// `sync.report` (0.40.0, ADR 0049): what applying a plan did — how many
/// steps ran, how many failed and why, and under which journal batch it all
/// ended up.
///
/// Twin of [`handle_rename_batch_report`], owner check included: seen by
/// whoever could see the Task ([`may_observe`]) — its owner, or any human
/// connection. For everyone else the answer is the SAME as an unknown id,
/// because the report carries relative paths of two trees it has no
/// business seeing, and distinguishing "not yours" from "does not exist"
/// would already leak that it existed.
///
/// An id EVICTED from the ring ([`SYNC_REPORTS_MAX`](crate::SYNC_REPORTS_MAX))
/// answers the same as one that was never an application, with the same
/// trade-off its twin documents.
fn handle_sync_report(
    actor: &Actor,
    p: &methods::SyncReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // `NotFound` from the taxonomy, a single answer for all THREE situations
    // — evicted from the ring, never was an application, belongs to another
    // actor — and the third is the reason: separating it would confirm
    // someone else's task existed.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared.engine.sync_report(p.task_id).ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Audit material (M3-5), same as `task.cancel`'s denied branch:
        // whoever asks about other actors' reports leaves a trace even if the
        // answer tells it nothing. Neither the id nor the owner.
        tracing::warn!(
            actor = ?actor,
            "sync report of another actor: denied (response = unknown id)"
        );
        return Err(unknown());
    }
    to_value(&report)
}

/// `fs.search` (0.18.0, live search): recursive name/content search under
/// `root` as a cancellable Task. HITS arrive via `search.hits` ONLY to the
/// `conn_id` connection that launched it (directed send, never broadcast —
/// they are its own).
///
/// READ GATE FOR AGENTS (security, liveSearch T4): `fs.search` amplifies
/// reading — a single call over `/` would exfiltrate previews of the WHOLE
/// tree. That is why an `Actor::Agent` only searches if `root` falls under a
/// LIVE scope of its session ([`ScopeRegistry::covers_read`]); outside it,
/// it is `PolicyDenied` with the coarse category from the closed vocabulary
/// ([`DenyReason::rule_id`]), never the specific rule. A `User` (human) is
/// not sandboxed: symmetric with `fs.list`/`fs.read`, which TODAY remain
/// open to agents — #80 debt (M3 only gated mutations; search is the first
/// bounded read).
///
/// Invalid criteria (glob/regex that fails to compile, zero criteria,
/// mutually exclusive axes) → `INVALID_PARAMS` WITHOUT creating a Task, with
/// the compiler's diagnostic (it is the requester's own input, not a leak).
/// Peer death mid-way: the Task is already registered and is governed by
/// `task.cancel` like a copy (#64 bounds the dropped-dispatch window); the
/// hits pump dies on its own when the walker closes the channel.
#[tracing::instrument(skip_all, fields(actor = ?actor))]
async fn handle_fs_search(
    params: Option<serde_json::Value>,
    conn_id: u64,
    actor: &Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    let p: methods::FsSearchParams = parse_params(params)?;

    // Read gate (default-deny for out-of-scope agents): the same
    // [`read_gate`] as `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities` (#80).
    read_gate(actor, &p.root, shared)?;

    // Criteria validation BEFORE the Task: compiles the matchers to recover
    // the sanitized diagnostic and answer INVALID_PARAMS without creating a
    // Task (`search_as` recompiles them — cheap — and would return an opaque
    // error). The detail is the glob/regex compiler's message: the
    // requester's own input.
    if let Err(e) = crate::search::SearchMatchers::compile(&p) {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            format!("invalid search criteria: {e}"),
        ));
    }

    let (handle, mut rx) = shared
        .engine
        .search_as(p, actor.clone())
        .await
        .map_err(RpcError::from)?;
    // INVARIANT (#64): ZERO `.await` between the engine's submit (inside
    // `search_as`) and this register — the Task never runs OUTSIDE
    // `shared.tasks` (with task.list/cancel and counting against the caps).
    // Whoever adds an await here breaks that guarantee.
    let task_id = register_task_id(shared, handle, actor.clone())?;

    // HITS pump: drains the walker's channel and routes each batch as
    // `search.hits` ONLY to the owner. Dies on its own when the walker closes
    // `tx` (terminal, cancel, or the receiver — the owner itself — gone).
    let shared_pump = Arc::clone(shared);
    // #155: same as `compare.rows` — the owner of a live directed feed loses
    // frames, never the subscription.
    let feed = shared_pump.feed_guard(conn_id);

    crate::blocking::spawn(async move {
        let _feed = feed;
        while let Some(hits) = rx.recv().await {
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::SEARCH_HITS.into(),
                params: serde_json::to_value(&hits).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                // The outcome is ignored ON PURPOSE: `fs.compare`'s pump does
                // stop when the owner disappears, but changing that here
                // would change a live search's behavior, which is not what
                // this change came to touch. `fs.search` is also bounded by
                // `max_hits`.
                let _delivered =
                    shared_pump.send_to_conn(conn_id, &Arc::from(frame.into_boxed_slice()));
            }
        }
    });

    to_value(&methods::FsTaskResult { task_id })
}

/// The pair cap of a rename batch (0.36.0), applied AT THE BOUNDARY.
///
/// The constant is the contract ([`methods::FS_RENAME_BATCH_MAX_PAIRS`]) and
/// the daemon is where it is enforced: this is where input from a peer that
/// could be an agent arrives. It REJECTS, does not trim — trimming would run
/// a plan different from the one requested, and for that plan it would
/// return verdicts for a batch nobody sent — and it rejects BEFORE touching
/// the engine, which plans in linear time over the pairs AND over the
/// listing. The engine re-checks it on its own (it is public embedded API);
/// this check is the boundary's, not an idle duplicate.
///
/// The verdict is [`Error::InvalidPath`] from the TAXONOMY, the same one
/// `plan_for` returns when the engine's twin check fires. A bare `-32602`
/// carries no category in `data`, so the remote `Backend` would deliver it as
/// `Internal` while the embedded one says `InvalidPath`: two different
/// answers to the same event depending on which door it came through.
fn check_pairs_cap(
    pairs: &[methods::RenamePair],
) -> Result<Vec<crate::rename::PairBytes>, RpcError> {
    let max = methods::FS_RENAME_BATCH_MAX_PAIRS;
    if pairs.len() > max {
        tracing::debug!(pairs = pairs.len(), max, "rename batch above the cap");
        return Err(RpcError::from(norte_proto::Error::InvalidPath));
    }
    Ok(crate::rename::pairs_from_wire(pairs))
}

/// `fs.rename_batch_report` (0.36.0): the report of an already-launched
/// batch.
///
/// Seen by whoever could see the Task ([`may_observe`]): its owner, or any
/// human connection. For everyone else the answer is the SAME as an unknown
/// id — the report carries paths from another actor's directory, and
/// distinguishing "not yours" from "does not exist" would already leak that
/// it existed (same criterion as `task.cancel`).
///
/// An id EVICTED from the ring answers the same as one that was never a
/// batch, and that IS a trade-off: there is precedent in
/// [`Error::CursorExpired`](norte_proto::Error::CursorExpired) (ADR 0017)
/// for "your handle aged out of a bounded server ring". No category is
/// minted because today no client would retry differently — a terminal
/// task's report is asked for once, right after — and because separating the
/// two things only makes sense if the third is also separated, which is
/// exactly the one that cannot be. Additive the day a client demonstrates
/// the case (ADR 0042 §8).
fn handle_rename_batch_report(
    actor: &Actor,
    p: &methods::FsRenameBatchReportParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // `NotFound` from the taxonomy, exactly what the `Backend`'s embedded arm
    // answers for the same case. A single answer for all THREE situations —
    // evicted from the ring, never was a batch, belongs to another actor —
    // and the third is the reason: separating it would confirm someone
    // else's task existed.
    let unknown = || RpcError::from(norte_proto::Error::NotFound);
    let (owner, report) = shared
        .engine
        .rename_batch_report(p.task_id)
        .ok_or_else(unknown)?;
    if !may_observe(actor, &owner) {
        // Audit material (M3-5), same as `task.cancel`'s denied branch:
        // whoever asks about other actors' reports leaves a trace even if
        // the answer tells it nothing. Neither the id nor the owner: the
        // trace counts that a probe happened, not what was on the other
        // side.
        tracing::warn!(
            actor = ?actor,
            "rename batch report of another actor: denied (response = unknown id)"
        );
        return Err(unknown());
    }
    to_value(&crate::rename::report_to_proto(&report))
}

/// Methods only a HUMAN may request: an agent receives `PolicyDenied` with
/// the `not-approved` rule, same as if its scope did not reach. The three
/// that use it (`ai.rename_plan`, `index.embed`, `index.search_semantic`)
/// spend model or build an index: it is not a path permission, it is a who
/// permission.
fn human_only(actor: &Actor) -> Result<(), RpcError> {
    if matches!(actor, Actor::User) {
        Ok(())
    } else {
        Err(RpcError::from(norte_proto::Error::PolicyDenied {
            rule: "not-approved".into(),
        }))
    }
}

/// The `fs.*`/`task.*` families of the dispatch (split out by size). `actor`
/// comes from the connection (M3-3b): mutations are journaled and evaluated
/// under it.
// Flat list of arms, one method per arm — same criterion as `dispatch`:
// splitting it would not reduce the real complexity, only hide it.
#[expect(
    clippy::too_many_lines,
    reason = "splitting it would not reduce the real complexity, only hide it"
)]
async fn dispatch_fs_task(
    req: Request,
    conn_id: u64,
    actor: crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        // fs.search (0.18.0): HITS belong to whoever launched it → needs
        // conn_id for the directed send (never broadcast).
        methods::FS_SEARCH => handle_fs_search(req.params, conn_id, &actor, shared).await,
        // fs.compare (0.39.0): ROWS belong to whoever launched it → conn_id,
        // same as `fs.search` (directed send, never broadcast).
        methods::FS_COMPARE => handle_fs_compare(req.params, conn_id, &actor, shared).await,
        // fs.dir_size (0.49.0, #139): no conn_id — routes nothing, the total
        // travels in the progress everybody already listens to.
        methods::FS_DIR_SIZE => handle_fs_dir_size(req.params, &actor, shared).await,
        // fs.checksum (0.59.0, #311): the content's digest, as a Task, and
        // its report — digests do not fit in a Task's outcome.
        methods::FS_CHECKSUM => handle_fs_checksum(req.params, &actor, shared).await,
        methods::FS_CHECKSUM_REPORT => {
            let p: methods::FsChecksumReportParams = parse_params(req.params)?;
            handle_fs_checksum_report(&actor, &p, shared)
        }
        // fs.dir_usage (0.75.0, phase 4): no conn_id — routes nothing; the
        // map is collected with its report, which does not fit in progress.
        methods::FS_DIR_USAGE => handle_fs_dir_usage(req.params, &actor, shared).await,
        methods::FS_DIR_USAGE_REPORT => {
            let p: methods::FsDirUsageReportParams = parse_params(req.params)?;
            handle_fs_dir_usage_report(&actor, &p, shared)
        }
        // 0.50.0 (#132): writing archives. None of them write INSIDE a
        // container — the archive provider stays `READ_ONLY` (ADR 0018).
        methods::ARCHIVE_PACK => handle_archive_pack(req.params, &actor, shared).await,
        methods::ARCHIVE_TEST => handle_archive_test(req.params, &actor, shared).await,
        methods::ARCHIVE_TEST_REPORT => handle_archive_test_report(req.params, &actor, shared),
        methods::ARCHIVE_PACK_REPORT => handle_archive_pack_report(req.params, &actor, shared),
        methods::FILE_SPLIT => handle_file_split(req.params, &actor, shared).await,
        methods::FILE_COMBINE => handle_file_combine(req.params, &actor, shared).await,
        // connection.close (0.49.0, #140): human, with a read gate.
        methods::CONNECTION_CLOSE => handle_connection_close(&actor, req.params, shared),
        // sync.plan (0.40.0): the STEPS belong to whoever launched it, and
        // the plan is RETAINED in this connection's name → conn_id twice
        // over.
        methods::SYNC_PLAN => handle_sync_plan(req.params, conn_id, &actor, shared).await,
        // sync.apply (0.40.0): the RETAINED plan is opened by `(conn_id,
        // hash)`, so conn_id is not for routing anything — it is half the
        // key.
        methods::SYNC_APPLY => handle_sync_apply(req.params, conn_id, &actor, shared).await,
        // sync.report (0.40.0): what a `Failed` cannot tell — which steps
        // were left unapplied and under which batch the ones that succeeded
        // ended up.
        methods::SYNC_REPORT => {
            let p: methods::SyncReportParams = parse_params(req.params)?;
            handle_sync_report(&actor, &p, shared)
        }
        methods::FS_STAT => {
            let p: methods::FsStatParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80
            handle_fs_stat(p, shared).await
        }
        // index.query (0.25.0, M4): direct index read.
        methods::INDEX_QUERY => {
            let p: methods::IndexQueryParams = parse_params(req.params)?;
            read_gate(&actor, &p.root, shared)?; // #80
            let hits = shared
                .engine
                .index_query_as(&p.root, &p.text, p.limit, actor.clone())
                .await
                .map_err(RpcError::from)?;
            let hits = hits
                .into_iter()
                .map(|h| methods::IndexHit {
                    path: h.path,
                    kind: h.kind,
                    size: h.size,
                    mtime_ms: h.mtime_ms,
                })
                .collect();
            to_value(&methods::IndexQueryResult { hits })
        }
        // ai.rename_plan (0.32.0, M4-IA, ADR 0031): reviewable rename plan.
        // DIRECT response; cancellable (#72) — the provider call takes a
        // while.
        methods::AI_RENAME_PLAN => {
            // AI only for the human (M4-IA security): an agent with a read
            // scope must NOT be able to burn the provider's quota nor push
            // basenames + instruction off the machine with no trace (the read
            // path does not journal). MCP does not expose ai.* as a tool
            // either. Category from the CLOSED vocabulary of
            // [`crate::policy::DenyReason`].
            //
            // **The gate goes BEFORE parsing** (#122), like in `index.embed`
            // and for the same reason: coming after, an agent could
            // distinguish "bad params" from "instruction too long" from
            // "inside or outside my scope" BEFORE being denied — i.e. the
            // answer depended on things it controls, and that turns a
            // forbidden method into an oracle about the human's tree.
            human_only(&actor)?;
            let p: methods::AiRenamePlanParams = parse_params(req.params)?;
            if p.instruction.len() > MAX_AI_INSTRUCTION_BYTES {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("instruction exceeds {MAX_AI_INSTRUCTION_BYTES} bytes"),
                ));
            }
            // The subset has the SAME cap as a path batch (#121): `names` is
            // a list that arrives from outside, and one with no cap is a 16
            // MiB frame of short names the engine walks for every listing
            // entry.
            if p.names.len() > methods::AI_RENAME_NAMES_MAX {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("names exceeds {}", methods::AI_RENAME_NAMES_MAX),
                ));
            }
            read_gate(&actor, &p.dir, shared)?; // #80
            let plan = shared
                .engine
                .ai_rename_plan_for(&p.dir, &p.instruction, &p.names)
                .await
                .map_err(RpcError::from)?;
            // core→proto mapping shared with `Backend::Embedded`
            // (`ai_plan_to_proto`): lossy-identity by the engine's invariant.
            to_value(&crate::ai::ai_plan_to_proto(plan))
        }
        // `ai.organize_plan` (0.77.0, phase 8): the twin of the one above,
        // with the SAME gates in the same order — human first, then parsing,
        // the two caps, and the directory's read gate. Being "like the
        // other one" is not enough: each of those four exists for a
        // different reason, and skipping the first turns the method into an
        // oracle.
        methods::AI_ORGANIZE_PLAN => {
            human_only(&actor)?;
            let p: methods::AiOrganizePlanParams = parse_params(req.params)?;
            if p.instruction.len() > MAX_AI_INSTRUCTION_BYTES {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("instruction exceeds {MAX_AI_INSTRUCTION_BYTES} bytes"),
                ));
            }
            if p.names.len() > methods::AI_RENAME_NAMES_MAX {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("names exceeds {}", methods::AI_RENAME_NAMES_MAX),
                ));
            }
            read_gate(&actor, &p.dir, shared)?;
            let plan = shared
                .engine
                .ai_organize_plan_for(&p.dir, &p.instruction, &p.names)
                .await
                .map_err(RpcError::from)?;
            let plan_hash = if plan.moves.is_empty() {
                None
            } else {
                Some(crate::organize::plan_hash(&p.dir, &plan.moves).map_err(RpcError::from)?)
            };
            to_value(&methods::AiOrganizePlanResult {
                moves: plan.moves,
                refused: None,
                plan_hash,
            })
        }
        // `fs.organize` (0.77.0, phase 8): applying the plan. MUTATES, so it
        // goes by the connection's actor and the engine gates it — a single
        // gate for every path it touches, folders included. Returns a Task.
        methods::FS_ORGANIZE => {
            let p: methods::FsOrganizeParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .organize(&p.dir, &p.moves, &p.plan_hash, actor.clone())
                .await
                .map_err(RpcError::from)?;
            let task_id = register_task_id(shared, handle, actor.clone())?;
            to_value(&methods::FsTaskResult { task_id })
        }
        // index.build (0.25.0, M4): a Task. The result (indexed/removed) is
        // NOT resent over the wire yet (task complete = done); fetching a
        // report is debt analogous to `policy.undo_report`.
        methods::INDEX_BUILD => {
            let p: methods::IndexBuildParams = parse_params(req.params)?;
            // READ gate (#80): the build walks the subtree and its paths go
            // out via `task.progress.current` to the owner — an out-of-scope
            // agent would enumerate an arbitrary tree. Gated the same as
            // fs.search / index.query (M4 review's security/rust BLOCKER).
            read_gate(&actor, &p.root, shared)?;
            let (handle, _report) = shared
                .engine
                .index_build_as(p.root, actor.clone())
                .await
                .map_err(RpcError::from)?;
            let task_id = register_task_id(shared, handle, actor.clone())?;
            to_value(&methods::FsTaskResult { task_id })
        }
        // index.embed (0.33.0, M4-IA-2): embeddings Task for an already
        // indexed root. HUMAN ONLY, fail-closed like ai.rename_plan: CONTENT
        // prefixes leave the process toward the provider and the read path
        // does not journal — an agent must not burn quota nor exfiltrate
        // content with no trace. Category from the CLOSED vocabulary of
        // [`crate::policy::DenyReason`].
        methods::INDEX_EMBED => {
            // The actor gate goes BEFORE parsing ON PURPOSE (M4-IA-2 security
            // audit): an agent receives `PolicyDenied` regardless of its
            // params' validity, and never distinguishes "bad params" from
            // "forbidden" — the answer does not depend on anything it
            // controls.
            human_only(&actor)?;
            let p: methods::IndexEmbedParams = parse_params(req.params)?;
            read_gate(&actor, &p.root, shared)?; // #80
            let handle = shared
                .engine
                .index_embed_as(p.root, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // INVARIANT (#64): ZERO `.await` between the engine's submit
            // (inside `index_embed_as`) and this register.
            let task_id = register_task_id(shared, handle, actor.clone())?;
            to_value(&methods::FsTaskResult { task_id })
        }
        // index.search_semantic (0.33.0, M4-IA-2): DIRECT response,
        // cancellable with rpc.cancel (#72) — embedding the query takes as
        // long as the provider takes. HUMAN ONLY (the query LEAVES toward
        // the provider), same criterion as index.embed / ai.rename_plan.
        methods::INDEX_SEARCH_SEMANTIC => {
            // Same as `index.embed`: actor gate BEFORE parsing (M4-IA-2
            // security audit), so an agent always sees `PolicyDenied`.
            human_only(&actor)?;
            let p: methods::IndexSearchSemanticParams = parse_params(req.params)?;
            if p.query.len() > MAX_AI_QUERY_BYTES {
                return Err(RpcError::protocol(
                    codes::INVALID_PARAMS,
                    format!("query exceeds {MAX_AI_QUERY_BYTES} bytes"),
                ));
            }
            if let Some(root) = &p.root {
                read_gate(&actor, root, shared)?; // #80
            }
            let hits = shared
                .engine
                .index_search_semantic(p.root.as_ref(), &p.query, p.k)
                .await
                .map_err(RpcError::from)?;
            let hits = hits
                .into_iter()
                .map(|(path, score)| methods::SemanticHit { path, score })
                .collect();
            to_value(&methods::IndexSearchSemanticResult { hits })
        }
        methods::FS_COPY => {
            let p: methods::FsCopyParams = parse_params(req.params)?;
            let opts = TransferOptions {
                on_collision: p.on_collision,
                symlinks: p.symlinks,
                resume: p.resume,
                verify: p.verify,
                // Queued if the client asked for it (ADR 0149); an N-1
                // client does not send the field and runs in parallel, as
                // always.
                queued: p.queued,
            };
            let handle = shared
                .engine
                .copy_anchored(&p.from, &p.to, opts, actor.clone(), p.dest_anchor)
                .await
                .map_err(RpcError::from)?;
            // INVARIANT (#64): ZERO `.await` between the engine's submit and
            // this register — a dispatch dropped by the peer's death never
            // leaves a Task running OUTSIDE `shared.tasks` (with no
            // task.list/cancel, not counting against MAX_LIVE_TASKS).
            // Whoever adds an await here breaks that guarantee.
            register_task(shared, handle, actor)
        }
        methods::FS_MOVE => {
            let p: methods::FsMoveParams = parse_params(req.params)?;
            let opts = TransferOptions {
                on_collision: p.on_collision,
                symlinks: p.symlinks,
                resume: p.resume,
                verify: p.verify,
                // Queued if the client asked for it (ADR 0149); an N-1
                // client does not send the field and runs in parallel, as
                // always.
                queued: p.queued,
            };
            let handle = shared
                .engine
                .move_anchored(&p.from, &p.to, opts, actor.clone(), p.dest_anchor)
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        methods::FS_DELETE => {
            let p: methods::FsDeleteParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .delete_with_as(&p.path, p.mode, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        // fs.rename_batch_plan (0.36.0, ADR 0042): the REVIEWABLE plan of a
        // rename batch. DIRECT response: no Task, no journal, no mutation —
        // but it DOES read a directory, so it goes through `read_gate` like
        // `fs.list`/`fs.stat` (#80). Its `External`, `AbsentSource` and
        // `AmbiguousSource` verdicts say which names exist and which do not:
        // without a gate this is a name oracle — and of NFC/NFD twins, which
        // `fs.list` does not even expose — for a scopeless agent.
        methods::FS_RENAME_BATCH_PLAN => {
            let p: methods::FsRenameBatchPlanParams = parse_params(req.params)?;
            read_gate(&actor, &p.dir, shared)?; // #80
            let pairs = check_pairs_cap(&p.pairs)?;
            let plan = shared
                .engine
                .rename_batch_plan_as(&p.dir, &pairs, actor)
                .await
                .map_err(RpcError::from)?;
            to_value(&crate::rename::plan_to_proto(&plan).map_err(RpcError::from)?)
        }
        // fs.rename_batch (0.36.0, ADR 0042): ONE Task, ONE journal batch,
        // rollback if anything fails. The engine RE-PLANS and compares the
        // hash: what crosses the wire is intent (`pairs`), never an
        // ordering.
        methods::FS_RENAME_BATCH => {
            let p: methods::FsRenameBatchParams = parse_params(req.params)?;
            // #80, and here it is NOT an extra precaution. Executing BEGINS
            // by planning: `rename_batch_as` lists the directory and
            // compares the hash BEFORE reaching its mutation gate, so
            // without this gate this method is the same oracle as its
            // twin — and a better one, because `plan_hash` is deterministic
            // and computable offline: a scopeless agent sends the hash of
            // the hypothesis "X exists" and distinguishes `PolicyDenied` (it
            // existed) from `PlanStale` (it did not), an exact bit per
            // request. Having the effect gated further in does not save the
            // READ that happens before it.
            read_gate(&actor, &p.dir, shared)?;
            let pairs = check_pairs_cap(&p.pairs)?;
            let (handle, _report) = shared
                .engine
                .rename_batch_as(&p.dir, &pairs, &p.plan_hash, actor.clone())
                .await
                .map_err(RpcError::from)?;
            // The report is NOT retained here: it lives in the engine's ring
            // (`Engine::rename_batch_report`), which is what
            // `fs.rename_batch_report` serves it from — a single ring for
            // the socket and for the embedded `Backend`.
            //
            // INVARIANT (#64): ZERO `.await` between the engine's submit and
            // this register, same as fs.copy/fs.move.
            register_task(shared, handle, actor)
        }
        // fs.rename_batch_report (0.36.0): what a `Failed` cannot tell —
        // which step was left applied and under what name.
        methods::FS_RENAME_BATCH_REPORT => {
            let p: methods::FsRenameBatchReportParams = parse_params(req.params)?;
            handle_rename_batch_report(&actor, &p, shared)
        }
        // fs.mkdir (0.31.0, #104): a Task, same mold as delete.
        methods::FS_MKDIR => {
            let p: methods::FsMkdirParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .mkdir_as(&p.path, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        methods::FS_CREATE => {
            let p: methods::FsCreateParams = parse_params(req.params)?;
            let handle = shared
                .engine
                .create_file_as(&p.path, p.dest_anchor, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        // #314: changing permissions. The engine does the POLICY gate over
        // the whole list and before the first effect (`PolicyOp::SetMode`);
        // here goes the per-path READ one, for the same reason as in
        // `fs.dir_size` — without it, an out-of-scope actor would enumerate
        // someone else's tree through this call's errors.
        methods::FS_SET_MODE => {
            let p: methods::FsSetModeParams = parse_params(req.params)?;
            // The CAP before the gate loop, like in `fs.checksum`: it is a
            // public constant and the sender knows how many paths it sent, so
            // checking it first leaks nothing. The other way around did cost
            // something — a batch of a million paths used to take the scope
            // registry's mutex once per path before anyone looked at the cap.
            if p.paths.len() > methods::FS_SET_MODE_MAX_PATHS {
                return Err(RpcError::from(norte_proto::Error::InvalidPath));
            }
            for path in &p.paths {
                read_gate(&actor, path, shared)?;
            }
            let handle = shared
                .engine
                .set_mode_as(p, actor.clone())
                .await
                .map_err(RpcError::from)?;
            register_task(shared, handle, actor)
        }
        _ => dispatch_task_family(req, actor, shared).await,
    }
}

/// The rest of the dispatch: `task.*` and 0.5.0's read-only methods. `actor`
/// comes from the connection: it governs `task.list`'s visibility,
/// `task.cancel`'s scope and who may bless host keys (#66).
async fn dispatch_task_family(
    req: Request,
    actor: crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    match req.method.as_str() {
        methods::TASK_LIST => {
            let _p: methods::TaskListParams = parse_params(
                req.params
                    .filter(|v| !v.is_null())
                    .or_else(|| Some(serde_json::json!({}))),
            )?;
            // Resync of a frontend that (re)connects: LIVE tasks + recent
            // outcomes (in case its terminal was emitted while it was away);
            // what follows arrives via task.progress.
            //
            // Visibility by actor (#66): a human sees EVERYTHING; an agent
            // sees ONLY its own tasks — `current` carries other actors'
            // paths.
            let mut tasks: Vec<norte_proto::TaskProgress> = shared
                .recent
                .lock()
                .expect("recent lock is sound")
                .iter()
                .filter(|(_, owner)| may_observe(&actor, owner))
                .map(|(p, _)| p.clone())
                .collect();
            tasks.extend(
                shared
                    .tasks
                    .lock()
                    .expect("tasks lock is sound")
                    .values()
                    .filter(|t| may_observe(&actor, &t.owner))
                    .map(|t| t.handle.progress().borrow().clone()),
            );
            to_value(&methods::TaskListResult { tasks })
        }
        methods::FS_READ => {
            let p: methods::FsReadParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80
            dispatch_fs_read(p, shared).await
        }
        methods::FS_CAPABILITIES => {
            let p: methods::FsCapabilitiesParams = parse_params(req.params)?;
            read_gate(&actor, &p.path, shared)?; // #80: reveals existence/type
            let capabilities = shared
                .engine
                .capabilities(&p.path)
                .await
                .map_err(RpcError::from)?;
            // Provider catalogue (#108 block 2), ALWAYS sanitized:
            // `Engine::attr_catalog` goes through `AttrCatalog::new` — the
            // only path to the wire (ADR 0039 §4).
            let attrs = shared
                .engine
                .attr_catalog(&p.path)
                .await
                .map_err(RpcError::from)?;
            to_value(&methods::FsCapabilitiesResult {
                capabilities,
                attrs,
            })
        }
        methods::TASK_CANCEL => {
            let p: methods::TaskCancelParams = parse_params(req.params)?;
            // Cancelling something terminal or unknown is NOT an error (the
            // method's contract): the response only confirms receipt.
            if let Some(task) = shared
                .tasks
                .lock()
                .expect("tasks lock is sound")
                .get(&p.task_id.get())
            {
                // Actor gate (#66): an agent only cancels its OWN. Another
                // actor's task is treated as unknown — same ack, no leaking
                // existence. The attempt IS traced (M3-5 audit material),
                // like a scope grant.
                if may_observe(&actor, &task.owner) {
                    task.handle.cancel();
                } else {
                    tracing::warn!(
                        task_id = p.task_id.get(),
                        actor = ?actor,
                        "task.cancel on another actor's task: ignored by the actor gate"
                    );
                }
            }
            to_value(&methods::TaskCancelResult {})
        }
        methods::TASK_PAUSE => {
            let p: methods::TaskPauseParams = parse_params(req.params)?;
            pause_task(&p, true, &actor, shared);
            to_value(&methods::TaskPauseResult {})
        }
        methods::TASK_MOVE => {
            let p: methods::TaskMoveParams = parse_params(req.params)?;
            move_task_in_queue(&p, &actor, shared);
            to_value(&methods::TaskMoveResult {})
        }
        methods::TASK_RESUME => {
            let p: methods::TaskPauseParams = parse_params(req.params)?;
            pause_task(&p, false, &actor, shared);
            to_value(&methods::TaskPauseResult {})
        }
        methods::CONNECTION_TRUST_HOST_KEY => dispatch_trust_host_key(req, &actor, shared).await,
        methods::CONNECTION_PROVIDE_SECRET => dispatch_provide_secret(req, &actor, shared).await,
        other => Err(RpcError::protocol(
            codes::METHOD_NOT_FOUND,
            format!("unknown method: {other}"),
        )),
    }
}

/// `task.move` (ADR 0149): reorders what has not started yet. Same contract
/// as pausing — over another actor's task, one already running, or an
/// unknown one, the ack is the same and nothing happens.
fn move_task_in_queue(
    p: &methods::TaskMoveParams,
    actor: &crate::journal::Actor,
    shared: &Arc<Shared>,
) {
    let is_own = shared
        .tasks
        .lock()
        .expect("tasks lock is sound")
        .get(&p.task_id.get())
        .is_some_and(|t| may_observe(actor, &t.owner));
    if is_own {
        // The ack does not say whether it moved: no leaking what state it
        // was in.
        let _ = shared.engine.mover_en_cola(p.task_id, p.up);
    }
}

/// `task.pause` / `task.resume` (ADR 0147). Same contract as `task.cancel`:
/// terminal or unknown is not an error, and another actor's is treated as
/// unknown — same ack, no leaking existence; the attempt IS traced.
fn pause_task(
    p: &methods::TaskPauseParams,
    pause: bool,
    actor: &crate::journal::Actor,
    shared: &Arc<Shared>,
) {
    let tasks = shared.tasks.lock().expect("tasks lock is sound");
    let Some(task) = tasks.get(&p.task_id.get()) else {
        return;
    };
    if !may_observe(actor, &task.owner) {
        tracing::warn!(
            task_id = p.task_id.get(),
            actor = ?actor,
            pause,
            "task.pause/resume on another actor's task: ignored by the actor gate"
        );
        return;
    }
    let gate = task.handle.pause_gate();
    if pause {
        gate.pause();
    } else {
        gate.resume();
    }
}

/// `connection.trust_host_key` (#45): one of the two doors through which the
/// core asks a HUMAN something. Kept out of `dispatch_task_family`'s
/// `match`, which was going over the line cap once its twin arrived.
async fn dispatch_trust_host_key(
    req: Request,
    actor: &crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // Accepting a fingerprint under TOFU is a HUMAN trust decision, like
    // `grant_scope`/`decide`/`undo_session` (#66): an agent never blesses a
    // host's identity.
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may trust a host key",
        ));
    }
    let p: methods::ConnectionTrustHostKeyParams = parse_params(req.params)?;
    // The engine delegates to the connector, which RE-VERIFIES the
    // fingerprint against the key the host presents now (anti-TOCTOU, ADR
    // 0015 D) before registering anything. `algo` is informational: the
    // identity being confirmed is the fingerprint.
    shared
        .engine
        .trust_host_key(&p.host, p.port, &p.fingerprint)
        .await
        .map_err(RpcError::from)?;
    to_value(&methods::ConnectionTrustHostKeyResult { trusted: true })
}

/// `connection.provide_secret` (#325): the other door. The secret a human
/// just typed after a [`norte_proto::Error::SecretNeeded`].
async fn dispatch_provide_secret(
    req: Request,
    actor: &crate::journal::Actor,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    // Typing a password is a HUMAN act, for the same reason as its twin and
    // one more: an agent that could inject session credentials would be
    // choosing which identity the user acts under on the remote host.
    if !matches!(actor, Actor::User) {
        return Err(RpcError::protocol(
            codes::INVALID_REQUEST,
            "only a human (non-agent) connection may provide a connection secret",
        ));
    }
    let p: methods::ConnectionProvideSecretParams = parse_params(req.params)?;
    // An empty secret is a MALFORMED request, and it is said as such: the
    // engine also rejects it (defense in depth, and it is where the policy
    // lives), but from there the only thing that can come out is
    // `PermissionDenied`, which would tell a client with a bug that its
    // credentials are invalid instead of that its request is invalid.
    if p.secret.is_empty() {
        return Err(RpcError::protocol(
            codes::INVALID_PARAMS,
            "connection secret must not be empty",
        ));
    }
    // None of this is logged: `p` has a `Debug` that redacts, `dispatch`'s
    // span is `skip_all` and the engine also instruments with `skip_all`
    // (rule 10).
    shared
        .engine
        .provide_secret(&p.conn, &p.secret)
        .await
        .map_err(RpcError::from)?;
    to_value(&methods::ConnectionProvideSecretResult { stored: true })
}

/// `fs.read` (0.5.0): ONE chunk in base64, with a per-call cap. ONE extra
/// byte is read to know whether the file continues (honest `eof` with no
/// extra stat and no trusting the size, which can change under one's feet).
async fn dispatch_fs_read(
    p: methods::FsReadParams,
    shared: &Arc<Shared>,
) -> Result<serde_json::Value, RpcError> {
    use base64::Engine as _;
    let offset = p.range.as_ref().map_or(0, |r| r.offset);
    let want = p
        .range
        .as_ref()
        .and_then(|r| r.len)
        .unwrap_or(methods::FS_READ_MAX_CHUNK)
        .min(methods::FS_READ_MAX_CHUNK);
    let probe_range = norte_proto::ByteRange {
        offset,
        len: Some(want.saturating_add(1)),
    };
    let mut stream = shared
        .engine
        .read(&p.path, Some(probe_range))
        .await
        .map_err(RpcError::from)?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RpcError::from)?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > want {
            break;
        }
    }
    let eof = bytes.len() as u64 <= want;
    bytes.truncate(usize::try_from(want).unwrap_or(usize::MAX).min(bytes.len()));
    to_value(&methods::FsReadResult {
        content_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        eof,
    })
}

/// Registers the task and starts its progress pump; returns the standard
/// wire result `FsTaskResult`. See [`register_task_id`].
fn register_task(
    shared: &Arc<Shared>,
    handle: TaskHandle,
    owner: Actor,
) -> Result<serde_json::Value, RpcError> {
    let task_id = register_task_id(shared, handle, owner)?;
    to_value(&methods::FsTaskResult { task_id })
}

/// Registers the task and starts its progress pump: each snapshot (≤30 Hz)
/// goes out as `task.progress` to humans and to the owner (#66); the
/// terminal state is never lost to the rate-limit (it is broadcast with the
/// same owner-based routing) and deregisters the task. Returns the `TaskId`
/// (the
/// The same cap [`register_task_id`] applies, checked BEFORE creating the
/// Task. `Some(err)` = does not fit.
///
/// Exists for the only method whose LATE rejection is not recoverable
/// (`sync.apply`: rejecting after opening the plan destroys it). It is an
/// approximation — another Task can slip in between this and the register —
/// and that is why it does NOT replace `register_task_id`'s cap, which
/// remains the authority.
fn tasks_at_capacity(shared: &Arc<Shared>, owner: &Actor) -> Option<RpcError> {
    let tasks = shared.tasks.lock().expect("tasks lock sano");
    if tasks.len() >= MAX_LIVE_TASKS {
        return Some(RpcError::protocol(
            codes::OVERLOADED,
            format!("too many live tasks (max {MAX_LIVE_TASKS}); retry later"),
        ));
    }
    if !matches!(owner, Actor::User)
        && tasks
            .values()
            .filter(|t| !matches!(t.owner, Actor::User))
            .count()
            >= MAX_LIVE_TASKS_AGENTS
    {
        return Some(RpcError::protocol(
            codes::OVERLOADED,
            format!("too many live agent tasks (max {MAX_LIVE_TASKS_AGENTS}); retry later"),
        ));
    }
    None
}

/// methods with their own result wrap it themselves, M3-4).
fn register_task_id(
    shared: &Arc<Shared>,
    handle: TaskHandle,
    owner: Actor,
) -> Result<TaskId, RpcError> {
    let task_id: TaskId = handle.id();
    let mut progress = handle.progress();
    {
        let mut tasks = shared.tasks.lock().expect("tasks lock is sound");
        // Anti-exhaustion cap (security-reviewer M3). The task is ALREADY
        // queued in the scheduler: it is cancelled cooperatively before
        // rejecting — never an unregistered phantom task.
        if tasks.len() >= MAX_LIVE_TASKS {
            handle.cancel();
            return Err(RpcError::protocol(
                codes::OVERLOADED,
                format!("too many live tasks (max {MAX_LIVE_TASKS}); retry later"),
            ));
        }
        // Per-class sub-cap (#70): agent tasks (all sessions) do not exhaust
        // the global quota — the human keeps its headroom.
        if !matches!(owner, Actor::User)
            && tasks
                .values()
                .filter(|t| !matches!(t.owner, Actor::User))
                .count()
                >= MAX_LIVE_TASKS_AGENTS
        {
            handle.cancel();
            return Err(RpcError::protocol(
                codes::OVERLOADED,
                format!("too many live agent tasks (max {MAX_LIVE_TASKS_AGENTS}); retry later"),
            ));
        }
        tasks.insert(
            task_id.get(),
            RegisteredTask {
                handle,
                owner: owner.clone(),
            },
        );
    }

    let shared_pump = Arc::clone(shared);
    crate::blocking::spawn(async move {
        loop {
            let snapshot = progress.borrow_and_update().clone();
            let terminal = snapshot.state.is_terminal();
            // A terminal is remembered in `recent` BEFORE it is broadcast:
            // that way a concurrent task.list (or a subscription born after
            // the broadcast) sees it through one of the two paths, never
            // through neither (rust-reviewer M1).
            if terminal {
                let mut recent = shared_pump.recent.lock().expect("recent lock is sound");
                push_recent(&mut recent, snapshot.clone(), &owner);
            }
            let notif = Notification {
                jsonrpc: norte_proto::wire::JsonRpcVersion,
                method: methods::TASK_PROGRESS.into(),
                params: serde_json::to_value(&snapshot).ok(),
            };
            if let Ok(frame) = encode_frame(&notif) {
                shared_pump.broadcast_task_progress(&Arc::from(frame.into_boxed_slice()), &owner);
            }
            if terminal {
                break;
            }
            // Coalesced: at most one frame every PROGRESS_MIN_INTERVAL.
            tokio::time::sleep(PROGRESS_MIN_INTERVAL).await;
            if progress.changed().await.is_err() {
                // The scheduler dropped the sender: broadcast the last state.
                let last = progress.borrow().clone();
                if last.state.is_terminal() {
                    let mut recent = shared_pump.recent.lock().expect("recent lock is sound");
                    push_recent(&mut recent, last.clone(), &owner);
                }
                let notif = Notification {
                    jsonrpc: norte_proto::wire::JsonRpcVersion,
                    method: methods::TASK_PROGRESS.into(),
                    params: serde_json::to_value(&last).ok(),
                };
                if let Ok(frame) = encode_frame(&notif) {
                    shared_pump
                        .broadcast_task_progress(&Arc::from(frame.into_boxed_slice()), &owner);
                }
                break;
            }
        }
        // The outcome is already in `recent` (above, before the broadcast).
        // All that is left is to remove the task from the live map.
        shared_pump
            .tasks
            .lock()
            .expect("tasks lock is sound")
            .remove(&task_id.get());
    });

    Ok(task_id)
}

/// How much of a peer-chosen text gets copied to the log.
const PEER_FIELD_MAX: usize = 64;

/// A peer-chosen text, fit for a span field: control characters escaped
/// (`\n` comes out as `\\n`, so it does not split the text log's line) and
/// cut to [`PEER_FIELD_MAX`] characters, with `…` if it was cut. A known
/// method is short ASCII and comes out unchanged.
fn peer_field(s: &str) -> String {
    let mut out: String = s
        .chars()
        .take(PEER_FIELD_MAX)
        .flat_map(char::escape_debug)
        .collect();
    if s.chars().nth(PEER_FIELD_MAX).is_some() {
        out.push('…');
    }
    out
}

/// The JSON-RPC `id` for the log: the number as-is, and a string via
/// [`peer_field`].
fn id_for_log(id: &norte_proto::wire::RequestId) -> String {
    match id {
        norte_proto::wire::RequestId::Num(n) => n.to_string(),
        norte_proto::wire::RequestId::Str(s) => peer_field(s),
    }
}

#[cfg(test)]
mod tests {
    /// An id or a method with a newline does not split the log line, and a
    /// huge one is not copied whole (ADR 0127).
    #[test]
    fn what_the_peer_chooses_reaches_the_log_bounded_and_escaped() {
        use super::{PEER_FIELD_MAX, id_for_log, peer_field};
        use norte_proto::wire::RequestId;

        assert_eq!(peer_field("fs.copy"), "fs.copy");
        let forged = peer_field("1\n2026-09-19T00:00:00Z  INFO policy allow");
        assert!(!forged.contains('\n'), "{forged}");
        assert!(forged.starts_with("1\\n"), "{forged}");
        let long = peer_field(&"x".repeat(10_000));
        assert_eq!(long.chars().count(), PEER_FIELD_MAX + 1, "cut, with `…`");
        assert!(long.ends_with('…'));
        assert_eq!(id_for_log(&RequestId::Num(7)), "7");
        assert_eq!(id_for_log(&RequestId::Str("a\rb".to_owned())), "a\\rb");
    }

    use std::os::unix::fs::PermissionsExt;

    use super::{DirIdentity, is_default_tmp_fallback, peer_allowed, prepare_socket_dir};

    /// #239: the location given to a plugin GOES THROUGH its own gate.
    ///
    /// It was believed gated because the `paths` were, and that is not the
    /// same thing: a scope root is within its own scope — `covers_read` fixes
    /// this in no uncertain terms — so asking for columns OVER the root used
    /// to hand the plugin a root confined one level ABOVE the agent's
    /// sandbox. With a scope pointing at a file, the directory containing it.
    #[test]
    fn a_plugins_location_passes_the_read_gate() {
        use super::permitted_location;
        use crate::policy::{Scope, ScopeRegistry, ScopeVerdict};
        use norte_proto::VPath;

        let vp = |w: &str| VPath::parse(w).expect("test wire");
        let reg = ScopeRegistry::new();
        reg.grant(
            "s1",
            Scope::forever(
                vec![vp("mem:///home/u/work")],
                crate::policy::OpSet::of(&["copy"]),
            ),
        );
        let now = std::time::Instant::now();
        let allowed = |p: &VPath| reg.covers_read("s1", p, now) == ScopeVerdict::Within;

        // Inside the scope: the location is the parent, and it gets minted.
        assert_eq!(
            permitted_location(Some(&vp("mem:///home/u/work/sub/f")), allowed),
            Some(vp("mem:///home/u/work/sub")),
        );
        // The scope's ROOT: its parent is the whole home, outside the
        // sandbox. No location, and the plugin runs without it.
        assert_eq!(
            permitted_location(Some(&vp("mem:///home/u/work")), allowed),
            None,
            "the scope root's parent is OUTSIDE the scope"
        );
        // A human is not sandboxed: the predicate says yes to everything.
        assert_eq!(
            permitted_location(Some(&vp("mem:///home/u/work")), |_| true),
            Some(vp("mem:///home/u")),
        );
        // With no paths there is no location to mint.
        assert_eq!(permitted_location(None, |_| true), None);
    }

    /// `session.put`'s veto: who may write, and in what order it is asked.
    ///
    /// Shutdown is NOT here and that is the interesting half: checking it
    /// with a token, outside the lock protecting the mutation, left open the
    /// window it meant to close. `SessionStore::seal` closes it, and its test
    /// lives with the store.
    #[test]
    fn only_the_human_owner_writes_the_session() {
        use super::{SessionPutVeto, session_put_veto};

        assert_eq!(session_put_veto(true, Some(7), 7, true), None);
        // An agent does not even get to ask about the rest: it should not
        // find out whether there is an owner either.
        assert_eq!(
            session_put_veto(false, Some(7), 7, true),
            Some(SessionPutVeto::NotHuman)
        );
        assert_eq!(
            session_put_veto(true, Some(1), 7, true),
            Some(SessionPutVeto::NotTheOwner)
        );
        assert_eq!(
            session_put_veto(true, None, 7, true),
            Some(SessionPutVeto::NotTheOwner),
            "with no owner, whoever did not claim it does not write either"
        );
        // #237's review: the owner of a core that does NOT persist does not
        // write either. Accepting it in memory stopped being harmless once
        // the writer could take the lock late — the detached, accepted body
        // survived adoption and got published over the other core's screen.
        assert_eq!(
            session_put_veto(true, Some(7), 7, false),
            Some(SessionPutVeto::NoWriter),
            "with no writer, owning the store is not enough"
        );
    }

    /// `send_to_conn` is a DIRECTED send (a search's hits belong to whoever
    /// launched it): only the destination connection receives it; an unknown
    /// connection is a no-op; a connection whose receiver died is removed
    /// from the map (same eviction criterion as the broadcast — no backlog
    /// accumulation).
    #[test]
    fn send_to_conn_only_reaches_the_destination_and_evicts_the_dead() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, send_to_conn_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        let (tx1, mut rx1) = mpsc::channel::<Arc<[u8]>>(4);
        let (tx2, mut rx2) = mpsc::channel::<Arc<[u8]>>(4);
        subs.lock().expect("lock").insert(
            1u64,
            Subscriber {
                tx: tx1,
                actor: Actor::User,
            },
        );
        subs.lock().expect("lock").insert(
            2u64,
            Subscriber {
                tx: tx2,
                actor: Actor::User,
            },
        );
        let frame: Arc<[u8]> = Arc::from(vec![1u8, 2, 3].into_boxed_slice());

        // Only connection 1 receives.
        send_to_conn_impl(&subs, 1, &frame);
        assert!(rx1.try_recv().is_ok(), "the destination receives");
        assert!(rx2.try_recv().is_err(), "the other one does NOT receive");

        // Unknown connection: no-op, no panic, does not touch the map.
        send_to_conn_impl(&subs, 99, &frame);
        assert_eq!(subs.lock().expect("lock").len(), 2);

        // Dead receiver: the connection is removed from the map.
        drop(rx1);
        send_to_conn_impl(&subs, 1, &frame);
        assert!(
            !subs.lock().expect("lock").contains_key(&1),
            "the connection with a closed receiver is removed"
        );
        assert!(
            subs.lock().expect("lock").contains_key(&2),
            "the live one stays"
        );
    }

    /// #155: a FULL outbox costs the frame and NOT the subscription. Eviction
    /// used to be irreversible — the entry is only inserted in `initialize`
    /// — and took down with it the terminal `task.progress`, which is
    /// exactly what `compare.rows`'s contract says to compare against
    /// received rows to know whether they all arrived.
    #[test]
    fn a_full_outbox_costs_the_frame_not_the_subscription() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, send_to_conn_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        let (tx, _rx) = mpsc::channel::<Arc<[u8]>>(1);
        subs.lock().expect("lock").insert(
            1u64,
            Subscriber {
                tx,
                actor: Actor::User,
            },
        );
        let frame: Arc<[u8]> = Arc::from(vec![1u8].into_boxed_slice());

        assert!(send_to_conn_impl(&subs, 1, &frame), "the first one fits");
        assert!(
            !send_to_conn_impl(&subs, 1, &frame),
            "the second does not fit: not delivered"
        );
        assert!(
            subs.lock().expect("lock").contains_key(&1),
            "and still subscribed: without this it also loses its terminal"
        );
    }

    /// The broadcast DOES keep evicting whoever does not drain — a slow
    /// client's backlog cannot grow without limit (M1) — except the owner of
    /// a live directed feed (#155).
    #[test]
    fn the_broadcast_evicts_who_does_not_drain_but_not_a_feeds_owner() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::sync::mpsc;

        use super::{Subscriber, broadcast_impl};
        use crate::journal::Actor;

        let subs = Mutex::new(HashMap::new());
        // The receivers are kept alive: closing them would be the OTHER case
        // (dead outbox), and here what is being tested is the FULL one.
        let mut alive = Vec::new();
        for conn in [1u64, 2u64] {
            let (tx, rx) = mpsc::channel::<Arc<[u8]>>(1);
            alive.push(rx);
            subs.lock().expect("lock").insert(
                conn,
                Subscriber {
                    tx,
                    actor: Actor::User,
                },
            );
        }
        let frame: Arc<[u8]> = Arc::from(vec![1u8].into_boxed_slice());

        // The first frame fits in both.
        broadcast_impl(&subs, &[], &frame, |_| true);
        assert_eq!(subs.lock().expect("lock").len(), 2);

        // The second does not fit in either, but 2 owns a live feed.
        broadcast_impl(&subs, &[2], &frame, |_| true);
        let subs = subs.lock().expect("lock");
        assert!(
            !subs.contains_key(&1),
            "the one that does not drain and has no feed, out"
        );
        assert!(
            subs.contains_key(&2),
            "the owner of a live directed feed keeps the subscription"
        );
        drop(alive);
    }

    /// The admission policy is EXACTLY same-uid: not even root gets in.
    #[test]
    fn peer_allowed_only_the_same_uid() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(0, 1000), "root is NOT the daemon's user");
    }

    /// #34.2 (TOCTOU): the dir's identity is captured and a REPLACEMENT of
    /// the dir between prepare and bind (same path, different inode) is
    /// detected.
    #[test]
    fn dir_identity_detects_a_dir_replacement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sub = tmp.path().join("sock-dir");
        std::fs::create_dir(&sub).expect("mkdir");
        let id = prepare_socket_dir(&sub).expect("valid dir");
        // No changes: the identity matches.
        assert!(id.verify_unchanged(&sub).is_ok());
        // Replacement (rm + recreate) = new inode → detected.
        std::fs::remove_dir(&sub).expect("rmdir");
        std::fs::create_dir(&sub).expect("recreate");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(
            id.verify_unchanged(&sub).is_err(),
            "a dir replaced under the same path must NOT pass"
        );
    }

    /// Identity capture is stable across calls to the SAME dir.
    #[test]
    fn dir_identity_is_stable_for_the_same_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = DirIdentity::of(tmp.path()).expect("stat a");
        let b = DirIdentity::of(tmp.path()).expect("stat b");
        assert_eq!(a, b);
    }

    /// #34.1 (/tmp squat): only the `/tmp/norte-<uid>/…` fallback (empty XDG)
    /// deserves the actionable message; a real `XDG_RUNTIME_DIR` or an
    /// explicit `--socket`, no.
    #[test]
    fn detects_only_the_tmp_fallback() {
        use std::path::Path;
        assert!(is_default_tmp_fallback(
            Path::new("/tmp/norte-1000/daemon.sock"),
            true
        ));
        // Not a fallback if the path did NOT come by default (explicit
        // --socket).
        assert!(!is_default_tmp_fallback(
            Path::new("/tmp/norte-1000/daemon.sock"),
            false
        ));
        // Nor a real XDG that happened to live under /run.
        assert!(!is_default_tmp_fallback(
            Path::new("/run/user/1000/norte/daemon.sock"),
            true
        ));
    }
}
