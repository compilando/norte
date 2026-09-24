//! The REMOTE backend: the typed methods a frontend actually uses.
//!
//! Underneath there is a [`crate::rpc::Client`] and, above, the same surface
//! the core's embedded backend presents to a frontend. What lives here and
//! not in the `Client` is everything it takes for talking to a daemon to
//! feel like calling a function: reconnection with resync, a task registry,
//! the routing of search/compare/sync batches, and the fan-out of
//! connection events and approvals.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use futures::StreamExt as _;
use norte_proto::methods::{
    self, ClientInfo, CompareRowsBatch, ConnectionDegraded, ConnectionFailed, FsCapabilitiesParams,
    FsCapabilitiesResult, FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams,
    FsReadParams, FsReadResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    PluginNotice, PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult,
    PolicyPendingResult, SearchHits, TaskCancelParams, TaskCancelResult, TaskListParams,
    TaskListResult,
};
use norte_proto::{
    ByteRange, Capabilities, DeleteMode, Entry, Error, TaskId, TaskKind, TaskProgress, TaskState,
    VPath,
};
use tokio::sync::{mpsc, watch};

pub mod calls;
mod paging;
mod routes;

use calls::{CALL_TIMEOUT, CancelOnAbandon, to_taxonomy};
use paging::{LIST_PAGE, PageState, page_step};
use routes::{BatchRoutes, OnFull, register_route, route_batch, schedule_route_removal};

use crate::rpc::{Client, ClientError};
use crate::task::{RemoteTask, RemoteTaskCanceller};
use crate::types::{
    AI_CALL_TIMEOUT, ConnEvent, EntryStream, SyncPlanEvent, Transfer, TransferOptions,
};

/// Reconnection backoff (walked through and staying on the last one).
const RECONNECT_BACKOFF_MS: &[u64] = &[250, 500, 1000, 2000, 5000];

/// How long a start-up permit is worth after a `daemon.going_away`.
///
/// It has to cover how long the old daemon takes to LEAVE THE PROCESS — not
/// to answer — because until then it holds `journal.db`'s exclusive lock and
/// the replacement aborts on opening it. Thirty seconds are plenty for that,
/// and still little for what the rule protects: that a daemon the user
/// stopped does not resurrect later.
const HANDOVER_SPAWN_WINDOW: Duration = Duration::from_secs(30);

/// Is the start-up permit still alive?
///
/// A separate, pure function so its expiry can be tested without spending
/// thirty seconds of wall clock.
fn spawn_allowed(until: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    until.is_some_and(|d| now < d)
}

// `derive(Default)` would require `T: Default`, which a batch has no reason
// to be: the empty map does not depend on the batch's type.
impl<T> Default for BatchRoutes<T> {
    fn default() -> Self {
        Self {
            routes: HashMap::new(),
            pending: HashMap::new(),
            terminated: std::collections::HashSet::new(),
        }
    }
}

impl<T> BatchRoutes<T> {
    /// Pending batches held in total (across all `task_id`s).
    fn pending_len(&self) -> usize {
        self.pending.values().map(Vec::len).sum()
    }

    /// Remembers that `id` reached its terminal (bounded: a backstop clears
    /// everything if it grows unbounded, never unlimited memory against a
    /// hostile daemon). Returns `true` only on the NEW insertion: the caller
    /// schedules the removal exactly once, without duplicating it on
    /// repeated terminals (the pump + a `task.list` resync). The set ONLY
    /// accumulates terminals of this feed's KIND (see `route`), so the 256
    /// cap is realistic (it would take 256 concurrent feeds for `clear` to
    /// erase a live mark).
    fn mark_terminated(&mut self, id: u64) -> bool {
        if self.terminated.len() >= 256 {
            self.terminated.clear();
        }
        self.terminated.insert(id)
    }

    /// Drops everything (reconnection): the `rx`s in flight close.
    fn clear(&mut self) {
        self.routes.clear();
        self.pending.clear();
        self.terminated.clear();
    }
}

struct Inner {
    socket: PathBuf,
    /// Auto-start command (`argv[0]` + args); `None` = connect only.
    spawn_cmd: Option<Vec<std::ffi::OsString>>,
    client_info: ClientInfo,
    /// Agent session declared in the handshake, or `None` for a human
    /// connection. Lives HERE and not only in `connect` because
    /// `establish` also runs on every RECONNECTION: an agent backend that
    /// reconnected without it would come back as `Actor::User` — the actor
    /// would whiten itself out silently on the daemon's first disconnect.
    agent_session: Option<String>,
    /// The daemon said a HANDOFF was coming (`daemon.going_away` with
    /// `reconnect: true`), so the next reconnection may start it.
    ///
    /// Exists because a handoff and a stop are the SAME closed connection
    /// seen from here, and the correct response is the opposite one:
    /// without this signal, reconnecting would always resurrect a daemon
    /// the user just stopped, and never reconnecting would leave the
    /// session dead after an update.
    ///
    /// **Expires by TIME, not by attempts**, and that difference is the one
    /// between the handoff working and not.
    ///
    /// The first version spent it on the first reconnection attempt, which
    /// arrives at 250ms. By then the old daemon has STILL not left the
    /// process, and with it holds `journal.db`'s exclusive lock — the first
    /// thing the replacement opens, and it aborts if it cannot. So the
    /// handoff died on that lock, the permit went with the failed attempt,
    /// and no later attempt ever started anything again: a session dead
    /// forever after a routine update, which is exactly the failure this
    /// feature exists to avoid. This phase's security review found it.
    ///
    /// What the no-resurrection rule wants is that the license does not
    /// survive "until next week", and that is a TIME bound. Within the
    /// window it starts as many times as needed; once past, "the daemon is
    /// not there" goes back to meaning what it always meant.
    handover_until: Mutex<Option<std::time::Instant>>,
    /// The protocol version the peer declared in the LAST handshake (#294).
    /// `None` = it has never connected yet.
    ///
    /// Exists because a client could not know what it was talking to, and
    /// that leaves undefended the degradations that are only safe by
    /// accident: it sends `expected_digest` (#282), a 0.52 daemon ignores
    /// it as ADR 0004 mandates, grants without checking, and nothing tells
    /// it. An `InitializeResult` that gets thrown away is a response
    /// already paid for.
    ///
    /// Rewritten on EVERY reconnection, because the daemon on the other end
    /// can be a different one: a handoff (`daemon.going_away { reconnect:
    /// true }`) is exactly the case of a different version on the same
    /// socket.
    peer_version: Mutex<Option<String>>,
    client: tokio::sync::RwLock<Option<Arc<Client>>>,
    watches: Mutex<HashMap<u64, watch::Sender<TaskProgress>>>,
    /// Outcomes seen with NO watch receiver (the terminal broadcast can get
    /// ahead of the response of the method that created the task):
    /// `own_task` consults them so as not to wait on a progress that
    /// already passed.
    finished: Mutex<std::collections::VecDeque<TaskProgress>>,
    foreign_tx: mpsc::UnboundedSender<RemoteTask>,
    events_tx: mpsc::UnboundedSender<ConnEvent>,
    /// Policy approvals toward the frontend (M3-3b T5): the pump's
    /// `policy.approval_required` notifications + the `policy.pending`
    /// resync on (re)connect.
    approvals_tx: mpsc::UnboundedSender<PolicyApprovalRequired>,
    /// `connection.degraded` warnings (#44) toward the frontend: every
    /// notification from the pump is relayed here (same pattern as
    /// `approvals_tx`).
    degraded_tx: mpsc::UnboundedSender<ConnectionDegraded>,
    /// `connection.failed` failures (#322) toward the frontend: why it
    /// could NOT connect. Same single-consumer channel as `degraded`.
    failed_tx: mpsc::UnboundedSender<ConnectionFailed>,
    /// `plugin.notice` warnings (0.69.0, ADR 0100) toward the frontend: a
    /// hook's sentence, or that the daemon turned off a plugin's hooks.
    /// Same single-consumer channel as `failed`.
    notices_tx: mpsc::UnboundedSender<PluginNotice>,
    /// `approval_id`s already delivered to the frontend: the daemon's
    /// delivery is at-least-once (broadcast + resync can overlap; every
    /// reconnection re-lists pending ones) and a duplicate SECURITY prompt
    /// is confusing (rust-reviewer MAJOR-1). Bounded best-effort dedup.
    seen_approvals: Mutex<std::collections::HashSet<u64>>,
    /// Routing of `search.hits` batches by `task_id` (live search). Shared
    /// by ALL clones via `Inner`: it is routed (not a one-shot `take_*`
    /// channel), so a search launched by any clone receives its hits via
    /// the single pump.
    search_routes: Mutex<BatchRoutes<SearchHits>>,
    /// The same for an `fs.compare`'s `compare.rows` (0.39.0). A SEPARATE
    /// map and not a shared one: the `task_id`s of two different feeds do
    /// not collide, but a single map would force a sum-type batch in the
    /// channel and the frontend would have to filter out what it did not
    /// ask for.
    compare_routes: Mutex<BatchRoutes<CompareRowsBatch>>,
    /// And the same for `sync.plan` (0.40.0), with one difference: the
    /// plan's TWO events — `sync.steps` and the `sync.plan_done` that
    /// closes it — travel over ONE channel, same as in the daemon, because
    /// the order between them is normative. With two maps that order would
    /// depend on how the runtime wakes two receivers; with one it is the
    /// queue.
    sync_routes: Mutex<BatchRoutes<SyncPlanEvent>>,
    /// The OPAQUE identity of the directories this client has LISTED (#295,
    /// ADR 0073), to be able to say, on copying, "the destination was
    /// THAT one".
    ///
    /// Lives in `Inner` and not per instance: whoever lists is the pane and
    /// whoever copies can be another clone of the same backend. And it is
    /// saved HERE, not in the frontend, so any client — the TUI, the
    /// window, the CLI — gets the check without writing a line.
    ///
    /// Bounded and best-effort: these are directories a human has open, i.e.
    /// units. Losing one costs that copy's check, never the copy.
    anchors: Mutex<AnchorCache>,
}

/// Retained directory anchors, with a cap and arrival order (#295).
///
/// A bare map would grow with every directory visited in a long session.
/// The cap is human-scale: the open panes, their recent history and little
/// more.
#[derive(Debug, Default)]
struct AnchorCache {
    by_dir: HashMap<VPath, norte_proto::DirAnchor>,
    order: std::collections::VecDeque<VPath>,
}

/// How many directories are remembered at once.
const ANCHORS_MAX: usize = 64;

impl AnchorCache {
    /// Remembers (or refreshes) `dir`'s anchor.
    ///
    /// `None` DELETES whatever there was, and that is deliberate: a listing
    /// that no longer brings an anchor — because the destination stopped
    /// being able to give one, or because it reconnected against a 0.53
    /// daemon — must not leave the old one alive. A copy that sent an old
    /// anchor would reject itself for no reason.
    ///
    /// Eviction is **LRU, not FIFO** (#301): refreshing moves the directory
    /// to the back of the queue. With FIFO, the directory a human has open
    /// got evicted as soon as `ANCHORS_MAX` DIFFERENT directories passed
    /// through the connection — the side tree expanded, a search — no
    /// matter that it was being relisted every second, and the next copy's
    /// check disappeared without anyone saying so.
    fn remember(&mut self, dir: &VPath, anchor: Option<norte_proto::DirAnchor>) {
        let Some(anchor) = anchor else {
            self.by_dir.remove(dir);
            self.order.retain(|d| d != dir);
            return;
        };
        if self.by_dir.insert(dir.clone(), anchor).is_some() {
            self.order.retain(|d| d != dir);
        }
        self.order.push_back(dir.clone());
        while self.order.len() > ANCHORS_MAX {
            if let Some(old) = self.order.pop_front() {
                self.by_dir.remove(&old);
            }
        }
    }

    fn get(&self, dir: &VPath) -> Option<norte_proto::DirAnchor> {
        self.by_dir.get(dir).cloned()
    }
}

impl Inner {
    /// Delivers an approval to the frontend exactly ONCE per `approval_id`
    /// (the source is at-least-once: broadcast + resync on every
    /// reconnection). The set is pruned entirely at the cap — best-effort
    /// dedup (human scale), never unbounded memory.
    fn push_approval(&self, req: PolicyApprovalRequired) {
        let mut seen = self
            .seen_approvals
            .lock()
            .expect("seen_approvals lock is sound");
        if seen.len() >= 1024 {
            seen.clear();
        }
        if seen.insert(req.approval_id) {
            let _ = self.approvals_tx.send(req);
        }
    }

    /// Queues a `connection.degraded` warning (#44) toward the frontend.
    fn push_degraded(&self, d: ConnectionDegraded) {
        let _ = self.degraded_tx.send(d);
    }

    /// Queues a `connection.failed` failure (#322) toward the frontend.
    fn push_failed(&self, f: ConnectionFailed) {
        let _ = self.failed_tx.send(f);
    }

    /// Queues a `plugin.notice` warning (0.69.0, ADR 0100) toward the
    /// frontend.
    fn push_notice(&self, n: PluginNotice) {
        let _ = self.notices_tx.send(n);
    }

    /// panicking would turn another thread's failure into the listing's
    /// death.
    fn remember_anchor(&self, dir: &VPath, anchor: Option<norte_proto::DirAnchor>) {
        if let Ok(mut cache) = self.anchors.lock() {
            cache.remember(dir, anchor);
        }
    }

    /// `dir`'s retained anchor, if this client listed it.
    fn anchor_for(&self, dir: &VPath) -> Option<norte_proto::DirAnchor> {
        self.anchors.lock().ok()?.get(dir)
    }
}

/// (Auto-reconnecting) connection to the daemon. Clonable: every clone
/// shares the connection and watches (`inner`), but the one-shot channels
/// below are PER INSTANCE — see the manual `impl Clone`.
pub struct RemoteBackend {
    inner: Arc<Inner>,
    /// FOREIGN tasks channel. `Some` only in the instance that has not
    /// taken it yet; a clone is born with `None` (it cannot steal it from
    /// the owner).
    foreign_rx: Mutex<Option<mpsc::UnboundedReceiver<RemoteTask>>>,
    /// Connection events channel. Same invariant as `foreign_rx`.
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnEvent>>>,
    /// Policy approvals channel. Same invariant as `foreign_rx`.
    approvals_rx: Mutex<Option<mpsc::UnboundedReceiver<PolicyApprovalRequired>>>,
    /// `connection.degraded` warnings channel (#44). Same invariant as
    /// `foreign_rx`.
    degraded_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnectionDegraded>>>,
    /// `connection.failed` failures channel (#322). Same invariant as
    /// `degraded_rx`: a single consumer takes it.
    failed_rx: Mutex<Option<mpsc::UnboundedReceiver<ConnectionFailed>>>,
    /// `plugin.notice` warnings channel (0.69.0). Same invariant as
    /// `failed_rx`.
    notices_rx: Mutex<Option<mpsc::UnboundedReceiver<PluginNotice>>>,
}

impl Clone for RemoteBackend {
    /// STRUCTURAL clone (not derived): shares `inner` (connection, watches,
    /// the `_tx`s) via `Arc`, but the receivers are born `None`.
    /// They used to live inside `Inner` (shared) and a clone could
    /// `take_*` and steal them from the real owner (e.g. the TUI) — a
    /// policy `ask` would expire to `deny` silently with nobody seeing it
    /// (rust-reviewer MAJOR over e408373). The INTERNAL uses that clone
    /// `RemoteBackend` (`TaskCanceller::Remote`, `list_stream`'s
    /// `PageState`, etc.) never call `take_*`, so `None` is also the
    /// correct value for them.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(None),
            failed_rx: Mutex::new(None),
            notices_rx: Mutex::new(None),
        }
    }
}

impl RemoteBackend {
    /// Connects (starting the daemon if `spawn_cmd` allows it), negotiates
    /// `initialize` and resyncs with `task.list`.
    ///
    /// # Errors
    /// Taxonomy: `ProviderUnavailable` if no daemon is reachable;
    /// `Internal` against an incompatible server.
    pub async fn connect(
        socket: PathBuf,
        spawn_cmd: Option<Vec<std::ffi::OsString>>,
        client_info: ClientInfo,
    ) -> Result<Self, Error> {
        Self::connect_inner(socket, spawn_cmd, client_info, None)
            .await
            .map_err(crate::remote::calls::to_taxonomy)
    }

    /// Like [`Self::connect`], but returning the client's RAW error.
    ///
    /// Exists for what the wire's taxonomy cannot carry: when the daemon
    /// starts and DIES — a journal that cannot be migrated, another user's
    /// socket — the only sentence saying what to do is the one it writes
    /// to `stderr`, and `Error::ProviderUnavailable` has nowhere to put it.
    ///
    /// A frontend that STARTS the daemon should use this one: it is the
    /// only one that can show that sentence, because it is the one that
    /// spawned it. For everything else, [`Self::connect`] and its
    /// taxonomy.
    ///
    /// # Errors
    /// [`ClientError::SpawnFailed`] if the daemon started and died;
    /// [`ClientError::SpawnTimeout`] if it is still alive and does not
    /// accept; whatever the handshake gives otherwise.
    pub async fn connect_detailed(
        socket: PathBuf,
        spawn_cmd: Option<Vec<std::ffi::OsString>>,
        client_info: ClientInfo,
    ) -> Result<Self, crate::ClientError> {
        Self::connect_inner(socket, spawn_cmd, client_info, None).await
    }

    /// Like [`Self::connect`], but declaring `agent_session`: the
    /// connection ends up bound to the agent actor the daemon governs
    /// (`Actor::Agent { session }`), and therefore to the agent gate —
    /// reads and mutations require a live scope from that session.
    ///
    /// Exists for the MCP bridge, which needs an arm able to drain
    /// notifications (`fs.compare` and `sync.plan` deliver through there)
    /// without stopping being the same actor as its tools connection.
    /// Policy scopes are stored PER SESSION (`ScopeRegistry::grant(session,
    /// …)`), not per connection, so granted permissions hold equally in
    /// both. What is NOT shared is the `conn_id`: a plan held for this
    /// connection is not redeemable from the other one.
    ///
    /// No `spawn_cmd` on purpose: an agent does not start daemons. If none
    /// is listening, this fails.
    ///
    /// # Errors
    /// Those of [`Self::connect`], plus a session rejected by the daemon
    /// (charset `[A-Za-z0-9._-]`, 1..=64).
    pub async fn connect_as_agent(
        socket: PathBuf,
        client_info: ClientInfo,
        agent_session: String,
    ) -> Result<Self, Error> {
        Self::connect_inner(socket, None, client_info, Some(agent_session))
            .await
            .map_err(crate::remote::calls::to_taxonomy)
    }

    /// The shared body of [`Self::connect`] and [`Self::connect_as_agent`]:
    /// one single handshake, one single pump.
    ///
    /// Returns the RAW error and lets each caller translate: what a daemon
    /// says while dying does not fit the taxonomy, and translating here
    /// would lose it for everyone (see [`Self::connect_detailed`]).
    async fn connect_inner(
        socket: PathBuf,
        spawn_cmd: Option<Vec<std::ffi::OsString>>,
        client_info: ClientInfo,
        agent_session: Option<String>,
    ) -> Result<Self, crate::ClientError> {
        let (foreign_tx, foreign_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
        let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();
        let (failed_tx, failed_rx) = mpsc::unbounded_channel();
        let (notices_tx, notices_rx) = mpsc::unbounded_channel();
        let backend = Self {
            inner: Arc::new(Inner {
                socket,
                spawn_cmd,
                client_info,
                agent_session,
                handover_until: Mutex::new(None),
                peer_version: Mutex::new(None),
                client: tokio::sync::RwLock::new(None),
                watches: Mutex::new(HashMap::new()),
                finished: Mutex::new(std::collections::VecDeque::new()),
                foreign_tx,
                events_tx,
                approvals_tx,
                degraded_tx,
                failed_tx,
                notices_tx,
                seen_approvals: Mutex::new(std::collections::HashSet::new()),
                search_routes: Mutex::new(BatchRoutes::default()),
                compare_routes: Mutex::new(BatchRoutes::default()),
                sync_routes: Mutex::new(BatchRoutes::default()),
                anchors: Mutex::new(AnchorCache::default()),
            }),
            foreign_rx: Mutex::new(Some(foreign_rx)),
            events_rx: Mutex::new(Some(events_rx)),
            approvals_rx: Mutex::new(Some(approvals_rx)),
            degraded_rx: Mutex::new(Some(degraded_rx)),
            failed_rx: Mutex::new(Some(failed_rx)),
            notices_rx: Mutex::new(Some(notices_rx)),
        };
        // The 1st connection DOES start the daemon (spawn); reconnections
        // do NOT (rust-reviewer M3: reconnecting must never resurrect a
        // daemon the user just stopped).
        let notifications = backend.establish(true).await?;
        // ONE pump task for the backend's whole life. Holds a Weak (not an
        // Arc): when the last external `RemoteBackend` is dropped, `Inner`
        // is freed and the pump exits on its own — no Arc cycle, no eternal
        // reconnection (rust-reviewer M2).
        let weak = Arc::downgrade(&backend.inner);
        tokio::spawn(async move { pump_loop(weak, notifications).await });
        Ok(backend)
    }

    /// A new connection: connect(+spawn if `spawn`) → initialize → resync →
    /// reconciliation. Returns the notification receiver.
    async fn establish(
        &self,
        spawn: bool,
    ) -> Result<mpsc::UnboundedReceiver<norte_proto::wire::Notification>, ClientError> {
        let mut client = match (&self.inner.spawn_cmd, spawn) {
            (Some(argv), true) => {
                let argv = argv.clone();
                Client::connect_or_spawn(&self.inner.socket, move || {
                    let mut cmd = std::process::Command::new(&argv[0]);
                    cmd.args(&argv[1..]);
                    cmd
                })
                .await?
            }
            _ => Client::connect(&self.inner.socket).await?,
        };
        // The actor is re-declared on EVERY connection: the daemon sets it
        // in the handshake and does not remember it from the previous one.
        let hello = match self.inner.agent_session.clone() {
            Some(session) => {
                client
                    .initialize_as_agent(self.inner.client_info.clone(), session)
                    .await?
            }
            None => client.initialize(self.inner.client_info.clone()).await?,
        };
        // The peer's version is RETAINED (#294): without it, a client
        // cannot know that the check it just asked for was not done.
        // Rewritten on every reconnection because there may be a different
        // daemon on the other side — which is exactly what a handoff
        // means.
        *self
            .inner
            .peer_version
            .lock()
            .expect("peer_version lock is sound") = Some(hello.protocol_version.clone());
        let notifications = client.take_notifications();
        let client = Arc::new(client);
        *self.inner.client.write().await = Some(Arc::clone(&client));

        // From HERE on, a failure has to leave the slot empty (#181).
        // Publishing the client before the resync is correct — the resync
        // is done WITH it — but if the resync fails and the client is left
        // set, the caller talks over a connection whose notification
        // receiver just died with this frame: nothing gets routed,
        // `task.progress` included, and a `TaskRef::join()` — which has no
        // deadline — waits forever for a terminal that can no longer
        // arrive. With the slot set to `None`, the next attempt starts
        // clean and the caller gets an error instead of silence.
        let resync = self.resync(&client).await;
        if let Err(e) = resync {
            *self.inner.client.write().await = None;
            return Err(e);
        }
        Ok(notifications)
    }

    /// The resync of a freshly made connection: the tasks that were already
    /// running, orphan reconciliation and pending approvals.
    ///
    /// Separated from [`Self::establish`] so its failure has ONE exit path
    /// and not several `?`s scattered around, each one a chance to forget
    /// there is published state to clean up (#181).
    async fn resync(&self, client: &Arc<Client>) -> Result<(), ClientError> {
        // Resync: the state of the tasks that were already running (or
        // finished while we were away — the server retains recent
        // outcomes).
        let list: TaskListResult = client.call(methods::TASK_LIST, &TaskListParams {}).await?;
        let mut live: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for snapshot in list.tasks {
            live.insert(snapshot.task_id.get());
            self.route(snapshot);
        }
        // Reconciliation (B1): one of OUR tasks in flight that the daemon
        // (possibly restarted and empty) no longer knows about would never
        // receive its terminal — its `join()` would hang. Resolved as
        // `Failed`.
        self.fail_orphans(&live);
        // Resync of pending policy approvals (M3-3b T5): an Ask broadcast
        // BEFORE this connection is not lost. Best-effort: an N-1 daemon
        // (with no `policy.pending`) answers METHOD_NOT_FOUND and nothing
        // happens; the TTL of a pending one nobody saw denies it on its
        // own.
        //
        // An AGENT connection does not ask for it: `policy.pending` is
        // human-only and the daemon answers it with INVALID_REQUEST, which
        // is not METHOD_NOT_FOUND and would fall into the `warn!` below —
        // one warning per connection AND PER RECONNECTION, forever, on the
        // channel where the real warnings need to be readable. It would
        // also make no sense: whoever approves is the human, never the
        // agent.
        if self.inner.agent_session.is_some() {
            return Ok(());
        }
        match client
            .call::<_, PolicyPendingResult>(methods::POLICY_PENDING, &serde_json::json!({}))
            .await
        {
            Ok(listed) => {
                for p in listed.pending {
                    self.inner.push_approval(PolicyApprovalRequired {
                        approval_id: p.approval_id,
                        session: p.session,
                        op: p.op,
                        paths: p.paths,
                        paths_total: p.paths_total,
                        // The remaining TTL does not travel in
                        // `policy.pending`: 0 = unknown (documented in
                        // proto).
                        ttl_ms: 0,
                        // #314: the detail DOES travel in the resync, and
                        // that is why it is in both shapes — a reconstructed
                        // pending entry showing less than the notification
                        // that announced it would leave the human deciding
                        // with less.
                        detail: p.detail,
                    });
                }
            }
            // The behavior is the same in both cases, but a real failure
            // must not be silently confused with an N-1 daemon (review m4).
            Err(ClientError::Rpc(ref rpc))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                tracing::debug!("daemon with no policy.pending (N-1): resync skipped");
            }
            Err(e) => {
                tracing::warn!(error = %e, "policy.pending resync failed");
            }
        }
        Ok(())
    }

    /// Marks `Failed{ProviderUnavailable}` on every task with a live watch
    /// the daemon no longer knows about (neither alive nor just-finished):
    /// its outcome was lost with the disconnect (rust-reviewer B1).
    fn fail_orphans(&self, live: &std::collections::HashSet<u64>) {
        let mut watches = self.inner.watches.lock().expect("watches lock is sound");
        let orphans: Vec<u64> = watches
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in orphans {
            if let Some(sender) = watches.remove(&id) {
                let mut last = sender.borrow().clone();
                last.state = TaskState::Failed {
                    error: Error::ProviderUnavailable { retryable: false },
                };
                let _ = sender.send(last);
            }
        }
    }

    /// Routes ONE snapshot: to its task's watch, creating it (and
    /// announcing it as a foreign task) if it is the first time.
    fn route(&self, snapshot: TaskProgress) {
        let id = snapshot.task_id;
        // Terminal search: schedules its route's removal AFTER the grace
        // period (lets stray `search.hits` through; see
        // `BATCH_ROUTE_GRACE`). FILTER BY KIND (mandatory): `terminated` is
        // only marked for SEARCH terminals. If everything were marked, other
        // copy/move/delete/list terminals would fill the set and
        // `clear(256)` could erase a live search's mark right between its
        // terminal and its route's registration → a hung route (sender
        // leak). The mark is ALWAYS taken under the lock: if the terminal
        // got ahead of the route's registration (no route yet), `search`
        // will see the mark and schedule the removal itself. That way
        // neither order leaves it hanging. Scheduled ONLY on the new
        // insertion (`mark_terminated` returns `true`) and with a live
        // route: a duplicate terminal (pump + resync) does not schedule it
        // again. `search_routes`'s lock is independent of `watches`'; it is
        // taken and dropped here, with no nesting. Future improvement: a
        // STRUCTURAL close (a "search done" sentinel after draining the
        // hits) would avoid the time-based grace period.
        //
        // `fs.compare` (0.39.0) has its own map and the same treatment: its
        // kind is `Compare` and its batches are `compare.rows`.
        if snapshot.state.is_terminal() {
            match snapshot.kind {
                TaskKind::Search => {
                    let schedule = {
                        let mut sr = self
                            .inner
                            .search_routes
                            .lock()
                            .expect("search_routes lock is sound");
                        let newly = sr.mark_terminated(id.get());
                        newly && sr.routes.contains_key(&id.get())
                    };
                    if schedule {
                        schedule_route_removal(&self.inner, id.get(), |i| &i.search_routes);
                    }
                }
                TaskKind::Compare => {
                    let schedule = {
                        let mut cr = self
                            .inner
                            .compare_routes
                            .lock()
                            .expect("compare_routes lock is sound");
                        let newly = cr.mark_terminated(id.get());
                        newly && cr.routes.contains_key(&id.get())
                    };
                    if schedule {
                        schedule_route_removal(&self.inner, id.get(), |i| &i.compare_routes);
                    }
                }
                TaskKind::SyncPlan => {
                    let schedule = {
                        let mut sr = self
                            .inner
                            .sync_routes
                            .lock()
                            .expect("sync_routes lock is sound");
                        let newly = sr.mark_terminated(id.get());
                        newly && sr.routes.contains_key(&id.get())
                    };
                    if schedule {
                        schedule_route_removal(&self.inner, id.get(), |i| &i.sync_routes);
                    }
                }
                _ => {}
            }
        }
        let mut watches = self.inner.watches.lock().expect("watches lock is sound");
        if let Some(sender) = watches.get(&id.get()) {
            let terminal = snapshot.state.is_terminal();
            let _ = sender.send(snapshot.clone());
            if terminal {
                watches.remove(&id.get());
                self.remember_finished(snapshot);
            }
            return;
        }
        // A new task not requested by this process: if it already arrived
        // terminal it is not announced as foreign, but it IS remembered —
        // the broadcast terminal can get ahead of fs.copy's response and
        // own_task needs it. The watches lock is HELD during registration
        // (watches→finished order everywhere): that way own_task cannot
        // slip in between the two and lose the outcome.
        if snapshot.state.is_terminal() {
            self.remember_finished(snapshot);
            drop(watches);
            return;
        }
        let (sender, rx) = watch::channel(snapshot);
        watches.insert(id.get(), sender);
        drop(watches);
        let _ = self.inner.foreign_tx.send(RemoteTask::new(
            id,
            rx,
            RemoteTaskCanceller::new(self.clone(), id),
        ));
    }

    async fn client(&self) -> Result<Arc<Client>, Error> {
        self.inner
            .client
            .read()
            .await
            .clone()
            .ok_or(Error::ProviderUnavailable { retryable: true })
    }

    /// A request with a time CAP (rust-reviewer M4): a daemon that is
    /// alive-but-stuck never freezes the frontend — at [`CALL_TIMEOUT`] the
    /// operation fails `ProviderUnavailable`.
    async fn call_timed<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let client = self.client().await?;
        match tokio::time::timeout(CALL_TIMEOUT, client.call(method, params)).await {
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// Like [`Self::call_timed`] but CANCEL-ON-DROP (#74, #72's pattern): if
    /// this future is dropped (or the timeout expires) with the request
    /// still IN FLIGHT, it sends `rpc.cancel {id}` best-effort — the
    /// daemon's dispatch dies PRE-effect and the Task is not born orphaned
    /// with no canceller (the window where the Lua driver abandons the run
    /// with the submit in flight). An id whose dispatch has ALREADY
    /// finished is a no-op on the daemon. Used by mutations
    /// (fs.copy/move/delete/mkdir), `index.build` and — via
    /// [`Self::call_timed_guarded_with`] — `ai.rename_plan`. The daemon
    /// wraps all those methods EXCEPT `index.build` in its cancellation arm
    /// (#72): for that one the guard is best-effort (`rpc.cancel` finds no
    /// dispatch to cut).
    ///
    /// **READS also use it** (`fs.list`, `fs.stat`, `fs.read`,
    /// `fs.capabilities`) since #248, and there it protects not a half-done
    /// effect — a read leaves none — but the CONNECTION: `serve_connection`
    /// dispatches serially, so an abandoned read — the session's five-second
    /// budget (#235), a dropped future — left every following request
    /// waiting behind it, each one dying in its own 30s [`CALL_TIMEOUT`].
    /// The TUI started up, showed itself, and was useless without saying so.
    async fn call_timed_guarded<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.call_timed_guarded_with(CALL_TIMEOUT, method, params)
            .await
    }

    /// Like [`Self::call_timed_guarded`] with an EXPLICIT timeout: the AI
    /// call uses [`AI_CALL_TIMEOUT`] (a remote model legitimately takes
    /// longer than an fs.*'s [`CALL_TIMEOUT`]).
    async fn call_timed_guarded_with<P, R>(
        &self,
        timeout: Duration,
        method: &str,
        params: &P,
    ) -> Result<R, Error>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let client = self.client().await?;
        let mut guard = CancelOnAbandon {
            client: std::sync::Arc::clone(&client),
            id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            armed: true,
        };
        let id_cell = std::sync::Arc::clone(&guard.id);
        let res = match tokio::time::timeout(
            timeout,
            client.call_tracked(method, params, |id| {
                // The Client's counter starts at 1: `0` = "no id yet".
                id_cell.store(id, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .await
        {
            Ok(res) => res.map_err(to_taxonomy),
            // Timeout: the guard stays ARMED — the return drops it and the
            // rpc.cancel travels (before, the dispatch kept running
            // server-side with nobody listening).
            Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
        };
        // Response received (RPC ok or error): there is nothing left to
        // cancel — disarm so as not to cancel a reusable id.
        guard.armed = false;
        res
    }

    /// Remote listing as a lazy stream: an EAGER first page (error parity)
    /// plus `try_unfold` over the `next_cursor`. No new deps. The container's
    /// `skipped` (#93) travels on every page — the first one's is enough
    /// (an N-1 daemon does not send it: `None` = unknown).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn list_stream(
        &self,
        dir: &VPath,
        attrs: Vec<String>,
    ) -> Result<(EntryStream, Option<u64>), Error> {
        // Synchronous first page: a `NotFound`/`TypeMismatch` comes out in
        // the Result, not as the stream's first item (parity with the
        // embedded one).
        let first: FsListResult = self
            .call_timed_guarded(
                methods::FS_LIST,
                &FsListParams {
                    path: dir.clone(),
                    limit: Some(LIST_PAGE),
                    cursor: None,
                    attrs: attrs.clone(),
                },
            )
            .await?;
        let skipped = first.skipped;
        // The anchor of the directory just listed (#295): retained here so
        // a `transfer` toward it returns it on its own. It is the listing
        // the human is looking at, which is exactly what the anchor says.
        self.inner.remember_anchor(dir, first.dir_anchor.clone());
        let done = first.next_cursor.is_none();
        let state = PageState {
            backend: self.clone(),
            dir: dir.clone(),
            buffer: first.entries.into(),
            cursor: first.next_cursor,
            done,
            attrs,
        };
        Ok((
            futures::stream::try_unfold(state, page_step).boxed(),
            skipped,
        ))
    }

    /// The location's capabilities, without the attribute catalog.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn capabilities(&self, path: &VPath) -> Result<Capabilities, Error> {
        Ok(self.capabilities_full(path).await?.capabilities)
    }

    /// The attribute catalog the location knows how to report (#117).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn attr_catalog(&self, path: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        Ok(self.capabilities_full(path).await?.attrs)
    }

    /// Full `fs.capabilities`: capabilities PLUS the attribute catalog, in
    /// one single trip (the two accessors above come from here).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn capabilities_full(&self, path: &VPath) -> Result<FsCapabilitiesResult, Error> {
        self.call_timed_guarded(
            methods::FS_CAPABILITIES,
            &FsCapabilitiesParams { path: path.clone() },
        )
        .await
    }

    /// An entry's `fs.stat`, asking for the given attributes.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn stat(&self, path: &VPath, attrs: Vec<String>) -> Result<Entry, Error> {
        let r: FsStatResult = self
            .call_timed_guarded(
                methods::FS_STAT,
                &FsStatParams {
                    path: path.clone(),
                    attrs,
                },
            )
            .await?;
        Ok(r.entry)
    }

    /// `connection.trust_host_key`: trusts the host key TOFU just showed
    /// (#45, ADR 0015).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        algo: &str,
        fingerprint: &str,
    ) -> Result<(), Error> {
        let r: methods::ConnectionTrustHostKeyResult = self
            .call_timed(
                methods::CONNECTION_TRUST_HOST_KEY,
                &methods::ConnectionTrustHostKeyParams {
                    host: host.to_string(),
                    port,
                    algo: algo.to_string(),
                    fingerprint: fingerprint.to_string(),
                },
            )
            .await?;
        // `trusted: false` is reserved in 0.7.0 for a policy rejection from
        // a future core: treating it as success would leave the user in a
        // loop of "trusted" retries that never register anything.
        if r.trusted {
            Ok(())
        } else {
            Err(Error::PolicyDenied {
                rule: "connection.trust_host_key".to_string(),
            })
        }
    }

    /// `connection.provide_secret`: delivers the secret the human typed
    /// after an [`Error::SecretNeeded`] (#325, ADR 0015).
    ///
    /// The secret crosses the socket IN THE CLEAR — the socket is a unix
    /// domain one, with 0600 permissions and owned by the user; see ADR
    /// 0015 — and the daemon keeps it only in memory, until it stops.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        let r: methods::ConnectionProvideSecretResult = self
            .call_timed(
                methods::CONNECTION_PROVIDE_SECRET,
                &methods::ConnectionProvideSecretParams {
                    conn: conn.to_string(),
                    secret: secret.to_string(),
                },
            )
            .await?;
        // Same reasoning as `trusted` above: a `stored: false` is the
        // reservation for a core that refuses, and treating it as success
        // would leave the user retrying a navigation that will never have
        // the secret.
        if r.stored {
            Ok(())
        } else {
            Err(Error::PolicyDenied {
                rule: "connection.provide_secret".to_string(),
            })
        }
    }

    /// `fs.read` of a byte range.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn read(&self, path: &VPath, range: Option<ByteRange>) -> Result<Vec<u8>, Error> {
        let mut offset = range.as_ref().map_or(0, |r| r.offset);
        let mut remaining = range.as_ref().and_then(|r| r.len);
        let mut out: Vec<u8> = Vec::new();
        loop {
            let want = remaining.map_or(methods::FS_READ_MAX_CHUNK, |r| {
                r.min(methods::FS_READ_MAX_CHUNK)
            });
            if want == 0 {
                return Ok(out);
            }
            let r: FsReadResult = self
                .call_timed_guarded(
                    methods::FS_READ,
                    &FsReadParams {
                        path: path.clone(),
                        range: Some(ByteRange {
                            offset,
                            len: Some(want),
                        }),
                    },
                )
                .await?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(r.content_b64.as_bytes())
                .map_err(|_| Error::Internal { panic: false })?;
            // Defensive guard (rust/protocol reviewers m1): a server
            // returning 0 bytes with eof=false would loop forever.
            if bytes.is_empty() && !r.eof {
                return Err(Error::Internal { panic: false });
            }
            offset += bytes.len() as u64;
            if let Some(rem) = &mut remaining {
                *rem = rem.saturating_sub(bytes.len() as u64);
            }
            out.extend_from_slice(&bytes);
            if r.eof {
                return Ok(out);
            }
        }
    }

    /// Copy or move, per `what`: a daemon Task in both cases.
    ///
    /// The verb is a [`Transfer`] and not the method name (#270): with a
    /// string, anything that was not exactly `fs.copy` fell into the `else`
    /// and turned into a move, which also deletes the source.
    ///
    /// # Errors
    /// Whatever the daemon responds on ENQUEUING (the outcome arrives via
    /// progress).
    pub async fn transfer(
        &self,
        what: Transfer,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<RemoteTask, Error> {
        // The DESTINATION directory's anchor, if this client listed it
        // (#295). Comes out on its own: no frontend has to remember, and
        // whoever did not list the destination — a `norte cp` with a
        // hand-typed path — sends `None` and gets 0.53's behavior.
        let dest_anchor = to.parent().and_then(|dir| self.inner.anchor_for(&dir));
        let result: FsTaskResult = match what {
            Transfer::Copy => {
                self.call_timed_guarded(
                    methods::FS_COPY,
                    &FsCopyParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
                        resume: opts.resume,
                        verify: opts.verify,
                        dest_anchor,
                        // Into the queue if asked (ADR 0149).
                        queued: opts.queued,
                    },
                )
                .await?
            }
            Transfer::Move => {
                self.call_timed_guarded(
                    methods::FS_MOVE,
                    &FsMoveParams {
                        from: from.clone(),
                        to: to.clone(),
                        on_collision: opts.on_collision,
                        symlinks: opts.symlinks,
                        resume: opts.resume,
                        verify: opts.verify,
                        dest_anchor,
                        // Into the queue if asked (ADR 0149).
                        queued: opts.queued,
                    },
                )
                .await?
            }
        };
        let kind = match what {
            Transfer::Copy => TaskKind::Copy,
            Transfer::Move => TaskKind::Move,
        };
        Ok(self.own_task(result.task_id, kind))
    }

    /// `index.build`: indexes a root as a Task.
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn index_build(&self, root: &VPath) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::INDEX_BUILD,
                &methods::IndexBuildParams { root: root.clone() },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Index))
    }

    /// `index.query`: search by name against the index.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn index_query(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
    ) -> Result<Vec<methods::IndexHit>, Error> {
        let r: methods::IndexQueryResult = self
            .call_timed(
                methods::INDEX_QUERY,
                &methods::IndexQueryParams {
                    root: root.clone(),
                    text: text.to_string(),
                    limit,
                },
            )
            .await?;
        Ok(r.hits)
    }

    /// `index.embed`: computes a root's embeddings as a Task (M4-IA-2).
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn index_embed(&self, root: &VPath) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::INDEX_EMBED,
                &methods::IndexEmbedParams { root: root.clone() },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Embed))
    }

    /// `index.search_semantic` (0.33.0, M4-IA-2): a direct response with
    /// AI's LONG timeout and cancel-on-drop (the daemon has it in its
    /// cancellation arm #72 — abandoning the wait cuts the dispatch).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn index_search_semantic(
        &self,
        root: Option<&VPath>,
        query: &str,
        k: u32,
    ) -> Result<Vec<methods::SemanticHit>, Error> {
        let r: methods::IndexSearchSemanticResult = self
            .call_timed_guarded_with(
                AI_CALL_TIMEOUT,
                methods::INDEX_SEARCH_SEMANTIC,
                &methods::IndexSearchSemanticParams {
                    root: root.cloned(),
                    query: query.to_owned(),
                    k,
                },
            )
            .await?;
        Ok(r.hits)
    }

    /// `ai.rename_plan` (0.32.0, M4-IA): a direct response with AI's LONG
    /// timeout and cancel-on-drop (the daemon has it in its cancellation arm
    /// #72 — abandoning the wait cuts the dispatch).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<methods::AiRenamePlanResult, Error> {
        self.call_timed_guarded_with(
            AI_CALL_TIMEOUT,
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams {
                dir: dir.clone(),
                instruction: instruction.to_string(),
                // The MARKED names, if any (#121): empty is the whole
                // directory, which is what this method used to do.
                names: names.to_vec(),
            },
        )
        .await
    }

    /// `ai.organize_plan` (0.77.0, phase 8): a REVIEWABLE plan to reorganize
    /// a directory. Mutates nothing.
    ///
    /// # Errors
    /// Whatever the daemon responds: with no provider, `Unsupported`; the AI
    /// gate refuses with its reason.
    pub async fn ai_organize_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<methods::AiOrganizePlanResult, Error> {
        self.call_timed_guarded_with(
            AI_CALL_TIMEOUT,
            methods::AI_ORGANIZE_PLAN,
            &methods::AiOrganizePlanParams {
                dir: dir.clone(),
                instruction: instruction.to_string(),
                names: names.to_vec(),
            },
        )
        .await
    }

    /// `fs.organize` (0.77.0, phase 8): applies the plan the human approved
    /// — creates the folders and moves, all as ONE undoable batch.
    ///
    /// # Errors
    /// `PlanStale` if the token is not the reviewed plan's; whatever the
    /// daemon responds on enqueuing.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[methods::OrganizeMove],
        plan_hash: &methods::PlanHash,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::FS_ORGANIZE,
                &methods::FsOrganizeParams {
                    dir: dir.clone(),
                    moves: moves.to_vec(),
                    plan_hash: plan_hash.clone(),
                },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::RenameBatch))
    }

    /// `fs.delete` of a batch: trash or permanent per `mode`.
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn delete(&self, path: &VPath, mode: DeleteMode) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::FS_DELETE,
                &FsDeleteParams {
                    path: path.clone(),
                    mode,
                },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Delete))
    }

    /// `fs.rename_batch_plan` (0.36.0, ADR 0042): a DIRECT response, no
    /// Task. The daemon has it in its cancellation arm (#72), so abandoning
    /// the wait cuts the dispatch instead of leaving it planning a huge
    /// directory.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[methods::RenamePair],
    ) -> Result<methods::FsRenameBatchPlanResult, Error> {
        self.call_timed_guarded(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: dir.clone(),
                pairs: pairs.to_vec(),
            },
        )
        .await
    }

    /// `fs.rename_batch` (0.36.0, ADR 0042): ONE Task for the whole batch.
    /// The SAME intent that produced the `plan_hash` is sent; the order
    /// never crosses the wire.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[methods::RenamePair],
        plan_hash: &methods::PlanHash,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::FS_RENAME_BATCH,
                &methods::FsRenameBatchParams {
                    dir: dir.clone(),
                    pairs: pairs.to_vec(),
                    plan_hash: plan_hash.clone(),
                },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::RenameBatch))
    }

    /// Like [`Self::call_timed`], translating `METHOD_NOT_FOUND` into
    /// [`Error::Unsupported`] (#247).
    ///
    /// This is the "degrade AND say so" primitive: against an older daemon,
    /// "your daemon does not know this" and a real failure are not the same
    /// thing, and a `session.get` that returns a generic error leaves the
    /// frontend saying the session failed when what happened is that there
    /// is none.
    async fn call_no_method_is_unsupported<P, R>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, Error>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let client = self.client().await?;
        match tokio::time::timeout(CALL_TIMEOUT, client.call::<_, R>(method, params)).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `fs.rename_batch_report` (0.36.0): the batch's report. An N-1 daemon
    /// with no such method answers `METHOD_NOT_FOUND` → `Unsupported`, so
    /// the caller can tell it apart from a REAL failure — same rule as
    /// `policy.undo_report` (#71): the report is the ONLY signal that a
    /// batch left the directory half-done, and it does not degrade
    /// silently.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Result<methods::FsRenameBatchReportResult, Error> {
        let client = self.client().await?;
        let params = methods::FsRenameBatchReportParams { task_id };
        let call = client.call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &params,
        );
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `fs.create`: an EMPTY file, as a Task (#290).
    ///
    /// Fails if the destination exists. Creating is a claim about a free
    /// name, and a method that silently truncates is data loss with an
    /// innocent-sounding name.
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn create_file(&self, path: &VPath) -> Result<RemoteTask, Error> {
        // The anchor of the directory it is created in, if this SDK listed
        // it (#295). Comes out on its own, as in `copy`/`move`: a frontend
        // gets the check without writing a line.
        let dest_anchor = path.parent().and_then(|dir| self.inner.anchor_for(&dir));
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::FS_CREATE,
                &norte_proto::methods::FsCreateParams {
                    path: path.clone(),
                    dest_anchor,
                },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Create))
    }

    /// `fs.set_mode`: a batch's POSIX permissions, as a Task (#314).
    ///
    /// Mutates, so the daemon logs it in the journal with its reverse — the
    /// previous mode — and passes it through policy. A location with no
    /// POSIX permissions answers `Unsupported` without changing anything.
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(methods::FS_SET_MODE, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::SetMode))
    }

    /// `fs.mkdir`, as a Task (the mutation goes through journal and policy
    /// just the same).
    ///
    /// # Errors
    /// Whatever the daemon responds on enqueuing.
    pub async fn mkdir(&self, path: &VPath) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_timed_guarded(
                methods::FS_MKDIR,
                &norte_proto::methods::FsMkdirParams { path: path.clone() },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Mkdir))
    }

    /// `fs.search` (live search T5): launches the Task and returns the `rx`
    /// the pump routes THIS `task_id`'s `search.hits` batches through.
    ///
    /// ANTI-RACE order: the route is registered BEFORE more hits can arrive.
    /// The pump (another task) may have already routed batches that got
    /// ahead of this response — the `search.hits` frame can leave the daemon
    /// before `fs.search`'s response, and on the client the pump and this
    /// call run in parallel: those batches stayed in `pending`.
    /// Registration (draining `pending` + inserting the route) is ATOMIC
    /// under `search_routes`'s lock, so not a single batch is lost between
    /// the two steps. Same pattern `own_task` uses to close the race of an
    /// early terminal via the `finished` ring.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn search(
        &self,
        params: FsSearchParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<SearchHits>), Error> {
        // 0.81.0's filters are not sent to a daemon that does not know them,
        // by the same rule as `undo_after`'s ceiling and with an
        // aggravation: an ignored filter does not stop filtering silently,
        // it returns the SUPERSET. Whoever asked for "under a meg, not
        // going into node_modules" would receive the whole tree, and an
        // over-broad search reads exactly like a plain one.
        if let Some(filter) = first_filter_since_0_81(&params)
            && !self.peer_honors_filters()
        {
            tracing::warn!(
                peer = self.peer_protocol_version().as_deref().unwrap_or("?"),
                filter,
                "the daemon predates 0.81 and cannot filter the search: \
                 refusing instead of returning more than was asked for"
            );
            return Err(Error::Unsupported);
        }
        let result: FsTaskResult = self.call_timed(methods::FS_SEARCH, &params).await?;
        let id = result.task_id;
        let rx = register_route(&self.inner, id.get(), methods::SEARCH_HITS, |i| {
            &i.search_routes
        });
        Ok((self.own_task(id, TaskKind::Search), rx))
    }

    /// `fs.compare` (0.39.0, ADR 0048): launches the Task and returns the
    /// `rx` the pump routes THIS `task_id`'s `compare.rows` batches through.
    /// Same lifecycle as [`Self::search`], including the startup race.
    ///
    /// An N-1 daemon (0.38.x) does NOT have the method and answers
    /// `METHOD_NOT_FOUND`: translated to [`Error::Unsupported`] so the
    /// frontend can tell "your daemon is older" apart from a real failure
    /// (same rule as `undo_report` and the `policy.pending` resync).
    /// `version_compatible` accepts N-1, so this combination is not
    /// hypothetical.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn compare(
        &self,
        params: methods::FsCompareParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<CompareRowsBatch>), Error> {
        let client = self.client().await?;
        let call = client.call::<_, FsTaskResult>(methods::FS_COMPARE, &params);
        let result: FsTaskResult = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                return Err(Error::Unsupported);
            }
            Ok(res) => res.map_err(to_taxonomy)?,
            Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
        };
        let id = result.task_id;
        let rx = register_route(&self.inner, id.get(), methods::COMPARE_ROWS, |i| {
            &i.compare_routes
        });
        Ok((self.own_task(id, TaskKind::Compare), rx))
    }

    /// `connection.close` (0.49.0, #140): drops that path's session.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn close_connection(&self, path: &norte_proto::VPath) -> Result<bool, Error> {
        let client = self.client().await?;
        let params = methods::ConnectionCloseParams { path: path.clone() };
        let call =
            client.call::<_, methods::ConnectionCloseResult>(methods::CONNECTION_CLOSE, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map(|r| r.closed).map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `fs.dir_size` (0.49.0, #139): launches the Task and returns its
    /// reference. No channel: what needs listening to is the progress,
    /// which already arrives via the usual subscription.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn dir_size(&self, params: methods::FsDirSizeParams) -> Result<RemoteTask, Error> {
        let client = self.client().await?;
        let call = client.call::<_, FsTaskResult>(methods::FS_DIR_SIZE, &params);
        let result: FsTaskResult = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                return Err(Error::Unsupported);
            }
            Ok(res) => res.map_err(to_taxonomy)?,
            Err(_) => return Err(Error::ProviderUnavailable { retryable: true }),
        };
        Ok(self.own_task(result.task_id, TaskKind::DirSize))
    }

    /// `fs.checksum` (0.59.0, #311): launches the checksum Task and returns
    /// its reference. The digests are collected with
    /// [`Self::checksum_report`], which is the only way: they do not fit in
    /// a Task's outcome.
    ///
    /// # Errors
    /// Whatever the daemon responds; [`Error::Unsupported`] against a 0.58
    /// one, which does not know the method.
    pub async fn checksum(&self, params: methods::FsChecksumParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FS_CHECKSUM, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Checksum))
    }

    /// `fs.checksum_report` (0.59.0, #311): the digests computed so far.
    ///
    /// # Errors
    /// Whatever the daemon responds; [`Error::Unsupported`] against a 0.58
    /// one.
    pub async fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Result<methods::FsChecksumReportResult, Error> {
        self.call_maybe_unknown(
            methods::FS_CHECKSUM_REPORT,
            &methods::FsChecksumReportParams { task_id },
        )
        .await
    }

    /// `fs.dir_usage` (0.75.0, phase 4): launches the Task that measures a
    /// directory child by child and returns its reference. The map is
    /// collected with [`Self::dir_usage_report`], which is the only way: a
    /// list of children does not fit in a Task's outcome.
    ///
    /// # Errors
    /// Whatever the daemon responds. [`Error::Unsupported`] says TWO things,
    /// not one: that the daemon does not know the method (a 0.74 one, which
    /// answers `METHOD_NOT_FOUND`) or that it knows the method and does not
    /// serve that `depth` yet. Whoever called tells them apart, by the
    /// `depth` it asked for: with `depth: 1` — what paints a map — only the
    /// first fits.
    pub async fn dir_usage(&self, params: methods::FsDirUsageParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FS_DIR_USAGE, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::DirUsage))
    }

    /// `fs.dir_usage_report` (0.75.0, phase 4): the map measured so far.
    ///
    /// # Errors
    /// Whatever the daemon responds; [`Error::Unsupported`] against a 0.74
    /// one, which does not know the method.
    pub async fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Result<methods::FsDirUsageReportResult, Error> {
        self.call_maybe_unknown(
            methods::FS_DIR_USAGE_REPORT,
            &methods::FsDirUsageReportParams { task_id },
        )
        .await
    }

    /// `archive.pack` (0.50.0, #132).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn pack(&self, params: methods::ArchivePackParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::ARCHIVE_PACK, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Pack))
    }

    /// `archive.test` (0.50.0, #132).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn test_archive(
        &self,
        params: methods::ArchiveTestParams,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::ARCHIVE_TEST, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::TestArchive))
    }

    /// `archive.test_report` (0.50.0, #132): the report, once the Task has
    /// already finished (or before, partial).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn archive_test_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<methods::ArchiveTestResult, Error> {
        self.call_maybe_unknown(
            methods::ARCHIVE_TEST_REPORT,
            &methods::ArchiveTestReportParams { task_id },
        )
        .await
    }

    /// `archive.pack_report` (0.58.0, #250): what that packaging saved that
    /// MEANS something else on another system — an `a\b.txt` that on
    /// Windows is a `b.txt` inside an `a` folder, a `CON` that will not
    /// extract there.
    ///
    /// The report is ready before the archive: it is computed over the
    /// entry list before writing the first byte. Asking for it when the
    /// Task finishes is the natural thing, but a report asked for halfway
    /// through is already final.
    ///
    /// **An empty report is a claim**, and only about the classes `checked`
    /// declares. This SDK always talks to an N or N+1 daemon — the
    /// handshake rejects anything else — so a daemon that does not know the
    /// method is not a reachable case from here; the `Unsupported` that
    /// still propagates is the defensive degradation, not the compatibility
    /// story.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<methods::ArchivePackReportResult, Error> {
        self.call_maybe_unknown(
            methods::ARCHIVE_PACK_REPORT,
            &methods::ArchivePackReportParams { task_id },
        )
        .await
    }

    /// `file.split` (0.50.0, #132).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn split_file(&self, params: methods::FileSplitParams) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FILE_SPLIT, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Split))
    }

    /// `file.combine` (0.50.0, #132).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn combine_files(
        &self,
        params: methods::FileCombineParams,
    ) -> Result<RemoteTask, Error> {
        let result: FsTaskResult = self
            .call_maybe_unknown(methods::FILE_COMBINE, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Combine))
    }

    /// `sync.plan` (0.40.0, ADR 0049): launches the Task and returns the
    /// `rx` the pump routes THIS `task_id`'s plan's TWO events
    /// (`sync.steps` and the `sync.plan_done` that closes it) through. Same
    /// lifecycle as [`Self::compare`], including the startup race.
    ///
    /// An N-1 daemon with no such method answers `METHOD_NOT_FOUND` →
    /// [`Error::Unsupported`], so the frontend can tell "your daemon is
    /// older" apart from a real failure.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn sync_plan(
        &self,
        params: methods::SyncPlanParams,
    ) -> Result<(RemoteTask, mpsc::Receiver<SyncPlanEvent>), Error> {
        let result: FsTaskResult = self.call_maybe_unknown(methods::SYNC_PLAN, &params).await?;
        let id = result.task_id;
        let rx = register_route(&self.inner, id.get(), methods::SYNC_STEPS, |i| {
            &i.sync_routes
        });
        Ok((self.own_task(id, TaskKind::SyncPlan), rx))
    }

    /// `sync.apply` (0.40.0, ADR 0049): executes the plan `plan_hash` names.
    /// Carries no paths — the two roots come from the server-side retained
    /// plan, bound to THIS connection.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn sync_apply(&self, plan_hash: &methods::PlanHash) -> Result<RemoteTask, Error> {
        let params = methods::SyncApplyParams {
            plan_hash: plan_hash.clone(),
        };
        // CANCEL-ON-DROP (#74), and here it is not an extra precaution:
        // `sync.apply`'s gate can stay suspended on a policy `ask` for
        // longer than [`CALL_TIMEOUT`] lasts, and without the guard the
        // dispatch would stay alive server-side — the human would approve a
        // minute later and the tree would get rewritten for a client that
        // had already given up and had received "the daemon is not
        // answering". With it, abandoning sends the `rpc.cancel` that
        // withdraws the gate PRE-effect.
        //
        // In exchange, the translation of `METHOD_NOT_FOUND` is lost, and it
        // does not matter: reaching here requires a `plan_hash`, which can
        // only have come from a `sync.plan` of the SAME daemon.
        let result: FsTaskResult = self
            .call_timed_guarded(methods::SYNC_APPLY, &params)
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Sync))
    }

    /// `sync.report` (0.40.0, ADR 0049): the report of an already-launched
    /// apply. `NotFound` if that id is not an apply this daemon retains —
    /// and that is also the answer for an id from ANOTHER connection, on
    /// purpose.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn sync_report(&self, task_id: TaskId) -> Result<methods::SyncReportResult, Error> {
        let params = methods::SyncReportParams { task_id };
        self.call_maybe_unknown(methods::SYNC_REPORT, &params).await
    }

    /// A call whose `METHOD_NOT_FOUND` means "your daemon is older" and is
    /// delivered as [`Error::Unsupported`], not as the `Internal` that
    /// `to_taxonomy` would turn a `-32601` into. This is the pattern
    /// `compare` and `undo_report` already wrote by hand.
    async fn call_maybe_unknown<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R, Error> {
        let client = self.client().await?;
        let call = client.call::<_, R>(method, params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `policy.undo_session` (M3-4): a human undoes an agent's session.
    /// Runs as an undo Task with progress/cancel like the others.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn undo_session(&self, session: &str) -> Result<RemoteTask, Error> {
        let result: methods::PolicyUndoSessionResult = self
            .call_timed(
                methods::POLICY_UNDO_SESSION,
                &methods::PolicyUndoSessionParams {
                    session: session.to_owned(),
                },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Undo))
    }

    /// `journal.list` (0.76.0, phase 7): a page of the timeline, newest to
    /// oldest.
    ///
    /// ONLY for a human connection: against an agent one the daemon answers
    /// `PolicyDenied`, which is what has to be shown — a forbidden journal
    /// cannot be read as an empty one.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<methods::JournalListResult, Error> {
        // `call_maybe_unknown` and not `call_timed`: a daemon that does not
        // know the method answers `METHOD_NOT_FOUND`, and that is "this
        // daemon keeps no timeline", which gets said, not a generic
        // failure. The handshake should make this unreachable — a 0.76
        // client never gets to talk to a 0.74 daemon — but the pattern is
        // the one `undo_report` already uses twelve lines below, and it is
        // free.
        self.call_maybe_unknown(
            methods::JOURNAL_LIST,
            &methods::JournalListParams {
                before_seq,
                limit,
                actor_kind: actor_kind.map(ToOwned::to_owned),
            },
        )
        .await
    }

    /// `journal.undo_after` (0.76.0, phase 7): undoes the human's work after
    /// `seq`. The marked entry stays.
    ///
    /// Runs as an undo Task, with the same progress, the same cancellation
    /// and the same report (`policy.undo_report`) as undoing a whole
    /// session: it is the same undo with a different selection rule.
    ///
    /// `upto_seq` (0.80.0) is the ceiling: the newest thing the human saw
    /// counted. A 0.79 daemon does not know it and would IGNORE it (ADR
    /// 0004), so undoing with a ceiling against it is refused with
    /// `Unsupported`, instead of undoing with no ceiling what the question
    /// did not count (#294).
    ///
    /// # Errors
    /// `Unsupported` with a ceiling against a pre-0.80 daemon; whatever the
    /// daemon responds.
    pub async fn undo_after(&self, seq: i64, upto_seq: Option<i64>) -> Result<RemoteTask, Error> {
        // Same rule as `plugins_set_approval`'s anchor (#294): sending a
        // guarantee the peer does not know how to apply is believing in a
        // guarantee that was never applied — and here what is lost is the
        // ceiling of an operation that REVERTS work. The question promised
        // N; an old daemon would undo N plus whatever was done after it.
        if upto_seq.is_some() && !self.peer_honors_the_ceiling() {
            tracing::warn!(
                peer = self.peer_protocol_version().as_deref().unwrap_or("?"),
                "the daemon predates 0.80 and cannot put a ceiling on an undo: \
                 refusing instead of undoing more than was counted"
            );
            return Err(Error::Unsupported);
        }
        let result: methods::PolicyUndoSessionResult = self
            .call_maybe_unknown(
                methods::JOURNAL_UNDO_AFTER,
                &methods::JournalUndoAfterParams { seq, upto_seq },
            )
            .await?;
        Ok(self.own_task(result.task_id, TaskKind::Undo))
    }

    /// `policy.undo_report` (#71): the undo Task's report. An N-1 daemon
    /// with no such method answers `METHOD_NOT_FOUND` → `Unsupported`, so
    /// the caller can tell it apart from a REAL failure (the report is the
    /// only signal that a Completed undo got blocked or skipped — it does
    /// not degrade silently; same rule as the `policy.pending` resync).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Result<methods::PolicyUndoReportResult, Error> {
        let client = self.client().await?;
        let params = methods::PolicyUndoReportParams { task_id };
        let call =
            client.call::<_, methods::PolicyUndoReportResult>(methods::POLICY_UNDO_REPORT, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// Remembers an outcome (bounded ring).
    fn remember_finished(&self, snapshot: TaskProgress) {
        let mut finished = self.inner.finished.lock().expect("finished lock is sound");
        finished.push_back(snapshot);
        while finished.len() > 128 {
            finished.pop_front();
        }
    }

    /// The `TaskRef` of a task REQUESTED by this process: hooks up (or
    /// creates) its watch. If the pump arrived first (broadcast) and
    /// already announced it as foreign, the frontend dedupes by id
    /// (documented contract); if its TERMINAL already arrived, the watch is
    /// born resolved.
    fn own_task(&self, id: TaskId, kind: TaskKind) -> RemoteTask {
        // Lock order watches→finished (the same as route): with watches
        // held, an outcome is either already in finished or will reach the
        // watch created below — no window.
        let mut watches = self.inner.watches.lock().expect("watches lock is sound");
        if let Some(done) = self
            .inner
            .finished
            .lock()
            .expect("finished lock is sound")
            .iter()
            .find(|p| p.task_id == id)
            .cloned()
        {
            drop(watches);
            let (_sender, rx) = watch::channel(done);
            return RemoteTask::new(id, rx, RemoteTaskCanceller::new(self.clone(), id));
        }
        let rx = if let Some(sender) = watches.get(&id.get()) {
            sender.subscribe()
        } else {
            let initial = TaskProgress {
                task_id: id,
                kind,
                state: TaskState::Pending,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 0,
                entries_total: None,
                current: None,
                unreadable: None,
                unvisited: None,
            };
            let (sender, rx) = watch::channel(initial);
            watches.insert(id.get(), sender);
            rx
        };
        RemoteTask::new(id, rx, RemoteTaskCanceller::new(self.clone(), id))
    }

    /// `task.cancel` fire-and-forget (the confirmation arrives via
    /// `task.progress`, the method's contract).
    pub(crate) fn spawn_cancel(&self, id: TaskId) {
        let backend = self.clone();
        tokio::spawn(async move {
            if let Ok(client) = backend.client().await {
                let _ = client
                    .call::<_, TaskCancelResult>(
                        methods::TASK_CANCEL,
                        &TaskCancelParams { task_id: id },
                    )
                    .await;
            }
        });
    }

    /// `task.pause` / `task.resume` (0.82.0, ADR 0147). NOT fire-and-forget
    /// like cancel: a 0.81 daemon cannot pause, answers `METHOD_NOT_FOUND`,
    /// and that comes back as `Unsupported` so the frontend can SAY so
    /// instead of painting a pause that never happened.
    ///
    /// # Errors
    /// `Unsupported` against a daemon with no such method; whatever the
    /// daemon responds otherwise.
    pub(crate) async fn set_paused(&self, id: TaskId, paused: bool) -> Result<(), Error> {
        let client = self.client().await?;
        let method = if paused {
            methods::TASK_PAUSE
        } else {
            methods::TASK_RESUME
        };
        let params = methods::TaskPauseParams { task_id: id };
        let call = client.call::<_, methods::TaskPauseResult>(method, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map(|_| ()).map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `task.move` (0.83.0, ADR 0149): moves a task that has not started yet
    /// up or down in the serial queue.
    ///
    /// # Errors
    /// `Unsupported` against a daemon with no such method; whatever the
    /// daemon responds otherwise.
    pub(crate) async fn mover_en_cola(&self, id: TaskId, up: bool) -> Result<(), Error> {
        let client = self.client().await?;
        let params = methods::TaskMoveParams { task_id: id, up };
        let call = client.call::<_, methods::TaskMoveResult>(methods::TASK_MOVE, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Err(ClientError::Rpc(ref rpc)))
                if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
            {
                Err(Error::Unsupported)
            }
            Ok(res) => res.map(|_| ()).map_err(to_taxonomy),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// The channel for FOREIGN tasks (the ones another client launched and
    /// this one observes). Belongs to the FIRST owner: a clone of the
    /// backend must not call it.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_foreign_tasks(&self) -> Option<mpsc::UnboundedReceiver<RemoteTask>> {
        self.foreign_rx
            .lock()
            .expect("foreign_rx lock is sound")
            .take()
    }

    /// The connection events channel (lost / restored). From the first
    /// owner, like the rest of the `take_*`s.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_conn_events(&self) -> Option<mpsc::UnboundedReceiver<ConnEvent>> {
        self.events_rx
            .lock()
            .expect("events_rx lock is sound")
            .take()
    }

    /// The pending policy approvals channel. From the first owner.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_approvals(&self) -> Option<mpsc::UnboundedReceiver<PolicyApprovalRequired>> {
        self.approvals_rx
            .lock()
            .expect("approvals_rx lock is sound")
            .take()
    }

    /// Takes the `connection.degraded` warnings receiver (#44). Just one
    /// (the first owner), like the other `take_*`s.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_degraded(&self) -> Option<mpsc::UnboundedReceiver<ConnectionDegraded>> {
        self.degraded_rx
            .lock()
            .expect("degraded_rx lock is sound")
            .take()
    }

    /// Takes the `connection.failed` failures receiver (#322): WHY it could
    /// not connect. Just one, like the other `take_*`s.
    ///
    /// Without this, the frontend receives the error's category —
    /// `PermissionDenied`, which does not distinguish an empty secret from a
    /// wrong key — and the exact sentence stays in the daemon's log.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_failed(&self) -> Option<mpsc::UnboundedReceiver<ConnectionFailed>> {
        self.failed_rx
            .lock()
            .expect("failed_rx lock is sound")
            .take()
    }

    /// Takes the `plugin.notice` warnings receiver (0.69.0, ADR 0100): what
    /// a `hook` plugin wanted to tell the human about an already-logged
    /// mutation, or that the daemon turned off a plugin's hooks. Just one,
    /// like the other `take_*`s.
    ///
    /// # Panics
    /// If the internal state is poisoned by a previous panic.
    pub fn take_plugin_notices(&self) -> Option<mpsc::UnboundedReceiver<PluginNotice>> {
        self.notices_rx
            .lock()
            .expect("notices_rx lock is sound")
            .take()
    }

    /// `policy.decide` against the daemon (M3-3b T5).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn policy_decide(&self, approval_id: u64, approve: bool) -> Result<(), Error> {
        let _: PolicyDecideResult = self
            .call_timed(
                methods::POLICY_DECIDE,
                &PolicyDecideParams {
                    approval_id,
                    approve,
                },
            )
            .await?;
        Ok(())
    }

    /// `plugin.list` against the daemon (M4-P3).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugins_list(&self) -> Result<methods::PluginListResult, Error> {
        self.call_timed(methods::PLUGIN_LIST, &methods::PluginListParams {})
            .await
    }

    /// `host.volumes` against the daemon (0.37.0, #131). The per-actor gate
    /// lives server-side (design §C): an agent connection sees
    /// [`Error::PolicyDenied`] here, not a transport failure.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn volumes(&self, include_pseudo: bool) -> Result<Vec<methods::Volume>, Error> {
        let result: methods::HostVolumesResult = self
            .call_timed(
                methods::HOST_VOLUMES,
                &methods::HostVolumesParams { include_pseudo },
            )
            .await?;
        Ok(result.volumes)
    }

    /// `connection.list` (0.56.0, #264): the named connections the DAEMON
    /// has configured.
    ///
    /// Asked for instead of reading `connections.toml` because reading it
    /// would force pulling the whole network stack — russh, opendal,
    /// suppaftp, age, keyring — into a binary that only wants to paint a
    /// list of names.
    ///
    /// What comes back does NOT connect: it is where one COULD go. Going is
    /// navigating to that URL, and that already establishes the session the
    /// usual way, with its TOFU and its policy.
    ///
    /// Returns the WHOLE result and not just the good ones: since 0.84.0 it
    /// also carries the entries the daemon could not read (#365), and
    /// dropping them here would leave the client unable to say why a
    /// connection the reader knows they wrote is missing.
    ///
    /// # Errors
    /// Taxonomy: `PolicyDenied` if the connection is an agent's;
    /// `InvalidPath` if the daemon's file exists and its TOML does not
    /// parse.
    pub async fn connections(&self) -> Result<methods::ConnectionListResult, Error> {
        self.call_timed(methods::CONNECTION_LIST, &serde_json::json!({}))
            .await
    }

    /// `session.get` against the daemon (L2): the screen and whether THIS
    /// connection owns it.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn session_get(&self) -> Result<(methods::Session, bool), Error> {
        let r: methods::SessionGetResult = self
            .call_no_method_is_unsupported(methods::SESSION_GET, &serde_json::json!({}))
            .await?;
        Ok((r.session, r.owner))
    }

    /// `session.put` against the daemon (L2): returns the NEW revision.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, Error> {
        let r: methods::SessionPutResult = self
            .call_no_method_is_unsupported(
                methods::SESSION_PUT,
                &methods::SessionPutParams {
                    version,
                    revision,
                    body,
                },
            )
            .await?;
        Ok(r.revision)
    }

    /// `session.release` against the daemon (0.78.0, phase 9): this
    /// connection gives up owning the UI session.
    ///
    /// Returns whether it WAS the owner. `false` is not an error: it is
    /// "it wasn't you", and whoever hands off needs it so as not to send the
    /// other frontend to claim a session that is still occupied.
    ///
    /// A 0.77 daemon answers `Unsupported` — a translated `MethodNotFound`
    /// — and there the honest degradation is releasing nothing.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn session_release(&self) -> Result<bool, Error> {
        let r: methods::SessionReleaseResult = self
            .call_no_method_is_unsupported(methods::SESSION_RELEASE, &serde_json::json!({}))
            .await?;
        Ok(r.released)
    }

    /// `log.tail` against the daemon (L2): what its log ring has after
    /// `cursor` (0.65.0, #328, ADR 0092).
    ///
    /// `cursor: None` asks for "whatever there is" — this is NOT the same as
    /// `Some(0)`, see [`methods::LogTailParams::cursor`]: against a ring
    /// that has already wrapped around, a `0` would report a false `lost`
    /// on the first poll. This SDK translates nothing: it passes the
    /// `Option` as is.
    ///
    /// The really reachable case is not an OLDER daemon — a 0.65 client
    /// never completes `initialize` against a 0.64 one, see
    /// [`methods::LOG_TAIL`] — but one of the SAME version compiled without
    /// the `logging` feature, which has no ring to serve and answers
    /// `Error::Unsupported` (`-32000`).
    ///
    /// This SDK ALSO folds a `METHOD_NOT_FOUND` into [`Error::Unsupported`]
    /// (`call_no_method_is_unsupported`), and that stays: it is defense
    /// against a peer that is not this daemon, not the description of the
    /// case that happens. Whoever reads this to write a degradation has ONE
    /// branch to program, and it is [`Error::Unsupported`].
    ///
    /// # Errors
    /// Whatever the daemon responds; [`Error::Unsupported`] if it has no
    /// ring to serve.
    pub async fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> Result<methods::LogTailResult, Error> {
        self.call_no_method_is_unsupported(
            methods::LOG_TAIL,
            &methods::LogTailParams { cursor, max },
        )
        .await
    }

    /// `log.level` against the daemon (L2): raises the level its ring is
    /// keeping and returns the one that really ended up set (0.65.0, #328,
    /// ADR 0092).
    ///
    /// The ring never LOWERS its level (see [`methods::LOG_LEVEL`]), so
    /// asking for one less verbose than the current one is not an error:
    /// the daemon answers with the one it already had set, and this SDK
    /// delivers it as is — there is nothing to translate nor to
    /// pre-validate against [`methods::LOG_LEVELS`] on the client.
    ///
    /// # Errors
    /// Whatever the daemon responds; [`Error::Unsupported`] against one with
    /// no ring to serve (see [`Self::log_tail`]). A `level` outside the
    /// vocabulary is `INVALID_PARAMS`, not `Unsupported` — they are two
    /// different questions and the daemon distinguishes them on purpose.
    pub async fn log_level(&self, level: &str) -> Result<String, Error> {
        let r: methods::LogLevelResult = self
            .call_no_method_is_unsupported(
                methods::LOG_LEVEL,
                &methods::LogLevelParams {
                    level: level.to_owned(),
                },
            )
            .await?;
        Ok(r.level)
    }

    /// The protocol version the peer declared in the last handshake (#294),
    /// or `None` if it has never connected yet.
    ///
    /// What is answered with it is "was the check I asked for really done?".
    /// An optional field an old peer ignores (ADR 0004) is a silent
    /// degradation, and without this the client could not even detect it.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of the internal lock, which
    /// would require another thread to have panicked while holding it — and
    /// the only thing done under it is reading and writing an
    /// `Option<String>`.
    #[must_use]
    pub fn peer_protocol_version(&self) -> Option<String> {
        self.inner
            .peer_version
            .lock()
            .expect("peer_version lock is sound")
            .clone()
    }

    // TODO(translation): review — this doc comment appears to have been
    // split between two functions: the paragraphs about `expected_digest`
    // belong to `peer_checks_the_anchor` below (which carries no doc of its
    // own), while only the last paragraph ("Does the peer know how to cap
    // `journal.undo_after`...") documents `peer_honors_the_ceiling`. Translated
    // in place, split intact, not restructured.
    /// Does the peer understand `expected_digest` in `plugin.set_approval`
    /// (#282)?
    ///
    /// The comparison is done by [`methods::version_at_least`], which is the
    /// canonical function and **fails CLOSED**: a version that does not
    /// parse answers `false`, "not knowing what the other one speaks, it is
    /// assumed nothing". This had its own parser for half an hour and it
    /// failed OPEN, which is the wrong direction — the string is chosen by
    /// the PEER, i.e. exactly the part being classified, and a
    /// `"norte-0.52"` would have skipped the whole check.
    ///
    /// With no version retained — not connected yet — it answers `true` and
    /// whoever decides is the daemon: there is no claim to make there, and
    /// the handshake goes before any call.
    /// Does the peer know how to cap `journal.undo_after` (`upto_seq`,
    /// 0.80.0)? Fails CLOSED like [`Self::peer_checks_the_anchor`], and for
    /// the same reason.
    fn peer_honors_the_ceiling(&self) -> bool {
        ceiling_honored(self.peer_protocol_version().as_deref())
    }

    /// Does the peer know how to filter a search (0.81.0)? Fails CLOSED the
    /// same way.
    fn peer_honors_filters(&self) -> bool {
        let Some(v) = self.peer_protocol_version() else {
            return true;
        };
        methods::version_at_least(&v, 0, 81)
    }

    fn peer_checks_the_anchor(&self) -> bool {
        let Some(v) = self.peer_protocol_version() else {
            return true;
        };
        // `expected_digest` arrived in 0.53.0.
        methods::version_at_least(&v, 0, 53)
    }

    /// `plugin.set_approval` against the daemon (M4-P3).
    ///
    /// `expected_digest` is the anchor the human READ (#282): the daemon
    /// refuses if it no longer matches its own, so that what is granted is
    /// what was shown. `None` keeps 0.52's behavior — the check is what is
    /// lost, not the correctness.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugins_set_approval(
        &self,
        id: &str,
        approved: bool,
        expected_digest: Option<&str>,
    ) -> Result<(), Error> {
        // Granting while asking for a check the peer does not know how to do
        // is believing in a guarantee that was never applied (#294): the old
        // daemon ignores the field as ADR 0004 mandates and grants whatever
        // it has. It is refused, and whoever wants to grant anyway can send
        // `None` — which is explicitly saying "without checking".
        if approved && expected_digest.is_some() && !self.peer_checks_the_anchor() {
            // It IS SAID, with the version inside: the whole point of #294
            // is making a silent degradation audible, and a mute
            // `Unsupported` reads exactly like "this daemon does not do
            // plugins".
            tracing::warn!(
                peer = self.peer_protocol_version().as_deref().unwrap_or("?"),
                "the daemon predates 0.53 and cannot check the approval's anchor: \
                 refusing instead of granting without checking (#294)"
            );
            return Err(Error::Unsupported);
        }
        let _: methods::PluginSetApprovalResult = self
            .call_timed(
                methods::PLUGIN_SET_APPROVAL,
                &methods::PluginSetApprovalParams {
                    id: id.to_owned(),
                    approved,
                    expected_digest: expected_digest.map(ToOwned::to_owned),
                },
            )
            .await?;
        Ok(())
    }

    /// `plugin.set_enabled` against the daemon (M4-P3).
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugins_set_enabled(&self, id: &str, enabled: bool) -> Result<(), Error> {
        let _: methods::PluginSetEnabledResult = self
            .call_timed(
                methods::PLUGIN_SET_ENABLED,
                &methods::PluginSetEnabledParams {
                    id: id.to_owned(),
                    enabled,
                },
            )
            .await?;
        Ok(())
    }

    /// `plugin.uninstall` against the daemon (0.71.0, ADR 0104): deletes the
    /// plugin, withdraws its consent and forgets it in the daemon's
    /// registry.
    ///
    /// # Errors
    /// Whatever the daemon responds: `INVALID_PARAMS` if it is not an id or
    /// is not installed; `INVALID_REQUEST` from an agent connection.
    pub async fn plugins_uninstall(
        &self,
        id: &str,
    ) -> Result<methods::PluginUninstallResult, Error> {
        self.call_timed(
            methods::PLUGIN_UNINSTALL,
            &methods::PluginUninstallParams { id: id.to_owned() },
        )
        .await
    }

    /// `plugin.run_command` against the daemon (M4-P4): returns the
    /// command's output. The daemon already redacts runtime failures to
    /// `Internal`.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_run_command(
        &self,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, Error> {
        let r: methods::PluginRunCommandResult = self
            .call_timed(
                methods::PLUGIN_RUN_COMMAND,
                &methods::PluginRunCommandParams {
                    id: id.to_owned(),
                    command: command.to_owned(),
                    arg: arg.to_owned(),
                },
            )
            .await?;
        Ok(r.output)
    }

    /// `plugin.preview` against the daemon (M4-P5): returns the result as is
    /// (the preview, or `None`). The daemon already redacts runtime
    /// failures to `INTERNAL_ERROR` and resolves the previewer fail-closed.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_preview(
        &self,
        path: &VPath,
    ) -> Result<methods::PluginPreviewResult, Error> {
        self.call_timed(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams { path: path.clone() },
        )
        .await
    }

    /// `plugin.preview_styled` against the daemon (G3a, ADR 0037): the
    /// styled twin of [`Self::plugin_preview`]. `MethodNotFound` (-32601) is
    /// the REAL trigger within the SAME 0.27 window (a 0.27 daemon with this
    /// handler not wired up yet — the ADR distinguishes this from
    /// `VERSION_MISMATCH`, which does not even let the call be attempted):
    /// translated to `Ok(None)`, exactly the same as "no previewer
    /// applies" — the caller falls back to [`Self::plugin_preview`] (plain).
    /// `call_timed` is NOT used here (same reason as `undo_report` above):
    /// `call_timed`/`to_taxonomy` only look at `rpc.data`, which for a
    /// `METHOD_NOT_FOUND` from the dispatch's `_` branch is `None` — the
    /// -32601 code would be lost. Any OTHER failure (I/O, timeout, a real
    /// runtime failure redacted by the daemon…) propagates as is.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_preview_styled(
        &self,
        path: &VPath,
        columns: Option<u32>,
    ) -> Result<Option<methods::PluginPreviewStyled>, Error> {
        let client = self.client().await?;
        let params = methods::PluginPreviewStyledParams {
            path: path.clone(),
            columns,
        };
        let call = client
            .call::<_, methods::PluginPreviewStyledResult>(methods::PLUGIN_PREVIEW_STYLED, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(res) => map_styled_preview_result(res),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `plugin.thumbnail` against the daemon (ADR 0107): `MethodNotFound` —
    /// a 0.72 daemon that does not have it — falls back to NO thumbnail,
    /// which is what there was. Any other error propagates.
    ///
    /// # Errors
    /// The wire's, translated into the taxonomy; never `MethodNotFound`.
    pub async fn plugin_thumbnail(
        &self,
        path: &VPath,
        max_edge: u32,
    ) -> Result<Option<methods::PluginThumbnail>, Error> {
        let client = self.client().await?;
        let params = methods::PluginThumbnailParams {
            path: path.clone(),
            max_edge,
        };
        let call =
            client.call::<_, methods::PluginThumbnailResult>(methods::PLUGIN_THUMBNAIL, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(res) => map_thumbnail_result(res),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `plugin.panel_render` against the daemon (0.74.0, phase 3):
    /// `MethodNotFound` — a 0.73 daemon that does not have it — falls back to
    /// NO frame, which leaves the slot with the last thing it painted. Any
    /// other error propagates.
    ///
    /// # Errors
    /// The wire's, translated into the taxonomy; never `MethodNotFound`.
    pub async fn plugin_panel_render(
        &self,
        params: methods::PluginPanelRenderParams,
    ) -> Result<Option<methods::PanelFrame>, Error> {
        let client = self.client().await?;
        let call = client
            .call::<_, methods::PluginPanelRenderResult>(methods::PLUGIN_PANEL_RENDER, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(res) => map_panel_result(res),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `plugin.decorate` against the daemon (G3b, ADR 0037): `MethodNotFound`
    /// (the same TWO triggers documented in the ADR — a 0.27 daemon that has
    /// not wired up the handler yet, or a client that decides not to call
    /// it) falls back to NO decorations (`Ok(vec![])`) — the listing paints
    /// just the same, with no badges. Any OTHER error propagates. An empty
    /// `paths` does not call the wire (nothing to decorate).
    ///
    /// `kinds` is each path's class, POSITIONAL with `paths` (0.72.0, ADR
    /// 0105): building the two lists from different iterators — one
    /// filtered, the other not — gives wrong icons with no error at all.
    /// Empty is legal and means "I don't know": everything is treated as
    /// `other`, and directories go with no icon; shorter than `paths`
    /// degrades whatever is missing the same way, and the daemon notes it;
    /// longer is `INVALID_PARAMS`.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_decorate(
        &self,
        paths: &[VPath],
        kinds: &[norte_proto::EntryKind],
    ) -> Result<Vec<methods::PluginDecorations>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let client = self.client().await?;
        let params = methods::PluginDecorateParams {
            paths: paths.to_vec(),
            kinds: kinds.to_vec(),
        };
        let call =
            client.call::<_, methods::PluginDecorateResult>(methods::PLUGIN_DECORATE, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(res) => map_decorate_result(res),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `plugin.rename_plan` (0.67.0, C3, ADR 0095): the plan a `renamer`
    /// plugin proposes. A 0.66 daemon does not negotiate with this client,
    /// so a version-caused `MethodNotFound` never arrives here.
    ///
    /// A guest refusal is not an error: an empty `entries` and `refused`
    /// with the sentence (0.68.0, #332).
    ///
    /// # Errors
    /// The daemon's: `NotFound` if the renamer is not consented to; `Io` if
    /// the guest does not run.
    pub async fn plugin_rename_plan(
        &self,
        plugin_id: &str,
        renamer_id: &str,
        dir: &VPath,
        names: &[String],
    ) -> Result<methods::AiRenamePlanResult, Error> {
        self.call_timed_guarded_with(
            AI_CALL_TIMEOUT,
            methods::PLUGIN_RENAME_PLAN,
            &methods::PluginRenamePlanParams {
                plugin_id: plugin_id.to_owned(),
                renamer_id: renamer_id.to_owned(),
                dir: dir.clone(),
                names: names.to_vec(),
            },
        )
        .await
    }

    /// `plugin.organize_plan` (0.77.0, phase 8): the plan of an `organizer`
    /// kind plugin. Same contract as the renamer's — it proposes, does not
    /// mutate — and the same response type as `ai.organize_plan`, because
    /// what makes the operation safe is not where the names came from.
    ///
    /// # Errors
    /// `NotFound` if that plugin does not declare that organizer, is not
    /// approved or is disabled; `InvalidPath` if it proposed writing outside
    /// the directory.
    pub async fn plugin_organize_plan(
        &self,
        plugin_id: &str,
        organizer_id: &str,
        dir: &VPath,
        names: &[String],
    ) -> Result<methods::AiOrganizePlanResult, Error> {
        self.call_timed_guarded_with(
            AI_CALL_TIMEOUT,
            methods::PLUGIN_ORGANIZE_PLAN,
            &methods::PluginOrganizePlanParams {
                plugin_id: plugin_id.to_owned(),
                organizer_id: organizer_id.to_owned(),
                dir: dir.clone(),
                names: names.to_vec(),
            },
        )
        .await
    }

    /// `plugin.column_values` against the daemon (G3b, ADR 0037): same
    /// fallback rule as [`Self::plugin_decorate`], but the "no data" shape
    /// is a vector of `None` the size of `paths` (an empty cell per entry),
    /// not an empty vector — the caller ALWAYS expects one cell per path
    /// (positional contract), even when the column does not apply at all.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_column_values(
        &self,
        plugin_id: &str,
        column_id: &str,
        paths: &[VPath],
    ) -> Result<Vec<Option<String>>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let client = self.client().await?;
        let params = methods::PluginColumnValuesParams {
            column_id: column_id.to_owned(),
            paths: paths.to_vec(),
            plugin_id: Some(plugin_id.to_owned()),
        };
        let call = client
            .call::<_, methods::PluginColumnValuesResult>(methods::PLUGIN_COLUMN_VALUES, &params);
        match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(res) => map_column_values_result(res, paths.len()),
            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
        }
    }

    /// `plugin.get_config` against the daemon (0.28.0, G3c). No special
    /// fallback: an N-1 daemon (0.27, with no such handler) answers
    /// `MethodNotFound`, which `call_timed`/`to_taxonomy` degrade to a
    /// generic error — the caller (TUI/GUI) treats "could not read the
    /// config" as "hide this plugin's settings section", never as a crash.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_get_config(
        &self,
        id: &str,
    ) -> Result<methods::PluginGetConfigResult, Error> {
        self.call_timed(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams { id: id.to_owned() },
        )
        .await
    }

    /// `plugin.help` against the daemon (H3e, 0.34.0). No special fallback:
    /// any error — including a peer that does not implement the method and
    /// answers `MethodNotFound` — is degraded by `call_timed`/`to_taxonomy`
    /// into a taxonomy error, and the frontend treats it as "this plugin has
    /// no page" and keeps painting the help, never as a failure. The help is
    /// cosmetic.
    ///
    /// The markdown is NOT masked (see the core's `Backend::plugin_help`):
    /// it is parsed before painting it, never dumped raw to a terminal or a
    /// log.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_help(&self, id: &str) -> Result<methods::PluginHelpResult, Error> {
        self.call_timed(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams { id: id.to_owned() },
        )
        .await
    }

    /// `plugin.set_config` against the daemon (0.28.0, G3c). Same fallback
    /// rule as [`Self::plugin_get_config`] — NO special fallback, an error
    /// (including an N-1 daemon with no such handler, or a value the daemon
    /// rejects) propagates as is.
    ///
    /// # Errors
    /// Whatever the daemon responds.
    pub async fn plugin_set_config(&self, id: &str, key: &str, value: &str) -> Result<(), Error> {
        let _: methods::PluginSetConfigResult = self
            .call_timed(
                methods::PLUGIN_SET_CONFIG,
                &methods::PluginSetConfigParams {
                    id: id.to_owned(),
                    key: key.to_owned(),
                    value: value.to_owned(),
                },
            )
            .await?;
        Ok(())
    }
}

// TODO(translation): review — this doc comment appears to document three
// different functions in a row (`map_styled_preview_result`,
// `map_thumbnail_result`, and the `map_panel_result` it is actually attached
// to). Translated in place, not restructured.
/// Translates `plugin.preview_styled`'s raw `Result` (G3a, ADR 0037) into
/// [`RemoteBackend::plugin_preview_styled`]'s contract. Extracted out of that
/// function ONLY so it can be tested with no socket (building a
/// [`ClientError::Rpc`] by hand): `METHOD_NOT_FOUND` (-32601) → `Ok(None)`
/// (same destination as "no previewer applies" — the caller falls back to
/// the plain preview); any OTHER error goes through the normal taxonomy
/// (`to_taxonomy`, which DOES look at `rpc.data` for `APP_ERROR`s).
/// Translates `plugin.thumbnail`'s raw `Result` (ADR 0107) into
/// [`RemoteBackend::plugin_thumbnail`]'s contract: `METHOD_NOT_FOUND` →
/// `Ok(None)`.
/// Translates `plugin.panel_render`'s raw `Result` (0.74.0, phase 3) into
/// [`RemoteBackend::plugin_panel_render`]'s contract: `METHOD_NOT_FOUND` →
/// `Ok(None)`, the same destination as "no plugin paints this panel".
///
/// Extracted out of the function for the same reason as its sibling: this
/// way the N-1 compatibility promise can be tested by building a
/// [`ClientError::Rpc`] by hand, with no socket or old daemon to bring up.
fn map_panel_result(
    res: Result<methods::PluginPanelRenderResult, ClientError>,
) -> Result<Option<methods::PanelFrame>, Error> {
    match res {
        Ok(r) => Ok(r.frame),
        Err(ClientError::Rpc(ref rpc))
            if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
        {
            Ok(None)
        }
        Err(e) => Err(to_taxonomy(e)),
    }
}

fn map_thumbnail_result(
    res: Result<methods::PluginThumbnailResult, ClientError>,
) -> Result<Option<methods::PluginThumbnail>, Error> {
    match res {
        Ok(r) => Ok(r.thumbnail),
        Err(ClientError::Rpc(ref rpc))
            if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
        {
            Ok(None)
        }
        Err(e) => Err(to_taxonomy(e)),
    }
}

fn map_styled_preview_result(
    res: Result<methods::PluginPreviewStyledResult, ClientError>,
) -> Result<Option<methods::PluginPreviewStyled>, Error> {
    match res {
        Ok(r) => Ok(r.preview),
        Err(ClientError::Rpc(ref rpc))
            if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
        {
            Ok(None)
        }
        Err(e) => Err(to_taxonomy(e)),
    }
}

/// Translates `plugin.decorate`'s raw `Result` (G3b, ADR 0037) into
/// [`RemoteBackend::plugin_decorate`]'s contract: `METHOD_NOT_FOUND` →
/// `Ok(vec![])` (no decorations, same destination as "no decorator
/// consented to"); any OTHER error goes through the normal taxonomy.
fn map_decorate_result(
    res: Result<methods::PluginDecorateResult, ClientError>,
) -> Result<Vec<methods::PluginDecorations>, Error> {
    match res {
        Ok(r) => Ok(r.plugins),
        Err(ClientError::Rpc(ref rpc))
            if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(to_taxonomy(e)),
    }
}

/// Translates `plugin.column_values`'s raw `Result` (G3b, ADR 0037) into
/// [`RemoteBackend::plugin_column_values`]'s contract: `METHOD_NOT_FOUND` →
/// `Ok(vec![None; expected_len])` (an empty cell per entry, NEVER an empty
/// vector — the caller ALWAYS expects one cell per path, positional
/// contract); any OTHER error goes through the normal taxonomy.
fn map_column_values_result(
    res: Result<methods::PluginColumnValuesResult, ClientError>,
    expected_len: usize,
) -> Result<Vec<Option<String>>, Error> {
    match res {
        Ok(r) => Ok(r.values),
        Err(ClientError::Rpc(ref rpc))
            if rpc.code == norte_proto::wire::codes::METHOD_NOT_FOUND =>
        {
            Ok(vec![None; expected_len])
        }
        Err(e) => Err(to_taxonomy(e)),
    }
}

/// Lifelong notification pump (rust-reviewer M2). Holds a [`Weak`]: in the
/// steady state (blocked in `recv().await`) it does NOT keep `Inner` alive,
/// so when the last external `RemoteBackend` is dropped, `Inner` is freed,
/// the internal `Client` closes the connection, `recv()` returns `None` and
/// the pump EXITS — no Arc cycle, no eternal reconnection.
#[expect(
    clippy::too_many_lines,
    reason = "notification→destination dispatch table + reconnection"
)]
async fn pump_loop(
    weak: Weak<Inner>,
    mut notifications: mpsc::UnboundedReceiver<norte_proto::wire::Notification>,
) {
    loop {
        // Consumption: an Arc is NEVER held across `recv().await`.
        while let Some(n) = notifications.recv().await {
            // Pending policy approval (M3-3b T5): to the frontend.
            if n.method == methods::POLICY_APPROVAL_REQUIRED {
                // Malformed = discarded WITH a trace (review m3): the agent
                // will wait out its TTL and someone must be able to know
                // why.
                let Some(params) = n.params else {
                    tracing::warn!("policy.approval_required with no params: discarded");
                    continue;
                };
                let req = match serde_json::from_value::<PolicyApprovalRequired>(params) {
                    Ok(req) => req,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed policy.approval_required");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                inner.push_approval(req);
                continue;
            }
            // Degraded session warning (#44): to the frontend. Malformed =
            // discarded WITH a trace (same treatment as the approval).
            if n.method == methods::CONNECTION_DEGRADED {
                let Some(params) = n.params else {
                    tracing::warn!("connection.degraded with no params: discarded");
                    continue;
                };
                let d = match serde_json::from_value::<ConnectionDegraded>(params) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed connection.degraded");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                inner.push_degraded(d);
                continue;
            }
            // #322: WHY it could not connect. Same treatment as above — no
            // params or malformed, discarded WITH a trace: a presentation
            // notification must not bring down the router.
            if n.method == methods::CONNECTION_FAILED {
                let Some(params) = n.params else {
                    tracing::warn!("connection.failed with no params: discarded");
                    continue;
                };
                let f = match serde_json::from_value::<ConnectionFailed>(params) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed connection.failed");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                inner.push_failed(f);
                continue;
            }
            // ADR 0100: a hook's sentence, or "I turned off its hooks". Same
            // treatment: no params or malformed, discarded WITH a trace.
            if n.method == methods::PLUGIN_NOTICE {
                let Some(params) = n.params else {
                    tracing::warn!("plugin.notice with no params: discarded");
                    continue;
                };
                let p = match serde_json::from_value::<PluginNotice>(params) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed plugin.notice");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                inner.push_notice(p);
                continue;
            }
            // The daemon is leaving (0.46.0). The only thing that needs
            // keeping is whether it is coming back, and it has to be kept
            // HERE: once the connection closes there will be no way to tell
            // a handoff apart from a stop.
            if n.method == methods::DAEMON_GOING_AWAY {
                // Malformed = discarded WITH a trace, and WITHOUT touching
                // the permit: "not understood" is not "do not come back".
                let Some(params) = n.params else {
                    tracing::warn!("daemon.going_away with no params: discarded");
                    continue;
                };
                let notice = match serde_json::from_value::<methods::DaemonGoingAway>(params) {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::warn!(error = %e, "malformed daemon.going_away");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                // An AGENT backend does not start daemons: its `spawn_cmd`
                // is `None` by construction, so it is not given a permit it
                // cannot use. Redundant today; tomorrow, if someone gave it
                // a start-up command, this is the only thing that would
                // keep a daemon notification from making the MCP bridge
                // launch processes.
                if inner.agent_session.is_some() {
                    continue;
                }
                *inner.handover_until.lock().expect("handover lock is sound") = notice
                    .reconnect
                    .then(|| std::time::Instant::now() + HANDOVER_SPAWN_WINDOW);
                tracing::info!(
                    reconnect = notice.reconnect,
                    "the daemon warns it is leaving"
                );
                // And it IS SAID upward. This warning is the only chance to
                // tell a handoff apart from a stop: as soon as the
                // connection closes, the frontend sees the same thing in
                // both cases.
                let _ = inner.events_tx.send(ConnEvent::GoingAway {
                    reconnect: notice.reconnect,
                });
                continue;
            }
            // A live search's batch of hits (live search T5): to its
            // `task_id`'s `rx`. Malformed = discarded with a trace.
            if n.method == methods::SEARCH_HITS {
                let Some(params) = n.params else {
                    tracing::debug!("search.hits with no params: discarded");
                    continue;
                };
                let hits = match serde_json::from_value::<SearchHits>(params) {
                    Ok(hits) => hits,
                    Err(e) => {
                        tracing::debug!(error = %e, "malformed search.hits: discarded");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                let id = hits.task_id.get();
                route_batch(
                    &inner.search_routes,
                    id,
                    hits,
                    methods::SEARCH_HITS,
                    OnFull::DropBatch,
                );
                continue;
            }
            // A live comparison's batch of rows (0.39.0): same treatment.
            if n.method == methods::COMPARE_ROWS {
                let Some(params) = n.params else {
                    tracing::debug!("compare.rows with no params: discarded");
                    continue;
                };
                let rows = match serde_json::from_value::<CompareRowsBatch>(params) {
                    Ok(rows) => rows,
                    Err(e) => {
                        tracing::debug!(error = %e, "malformed compare.rows: discarded");
                        continue;
                    }
                };
                let Some(inner) = weak.upgrade() else { return };
                let id = rows.task_id.get();
                route_batch(
                    &inner.compare_routes,
                    id,
                    rows,
                    methods::COMPARE_ROWS,
                    OnFull::DropBatch,
                );
                continue;
            }
            // A live plan's two events (0.40.0) go to the SAME `rx`, wrapped
            // in the same enum the embedded arm returns: the order "steps*
            // and then the close" is that channel's queue, not a race
            // between two maps. A malformed event is discarded with a
            // trace, like the other two feeds.
            if n.method == methods::SYNC_STEPS || n.method == methods::SYNC_PLAN_DONE {
                let done = n.method == methods::SYNC_PLAN_DONE;
                let Some(params) = n.params else {
                    tracing::debug!(method = %n.method, "sync event with no params: discarded");
                    continue;
                };
                let event = if done {
                    serde_json::from_value::<methods::SyncPlanDone>(params).map(SyncPlanEvent::Done)
                } else {
                    serde_json::from_value::<methods::SyncStepsBatch>(params)
                        .map(SyncPlanEvent::Steps)
                };
                let event = match event {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::debug!(error = %e, "malformed sync event: discarded");
                        continue;
                    }
                };
                let id = match &event {
                    SyncPlanEvent::Steps(b) => b.task_id.get(),
                    SyncPlanEvent::Done(d) => d.task_id.get(),
                };
                let Some(inner) = weak.upgrade() else { return };
                let feed = if done {
                    methods::SYNC_PLAN_DONE
                } else {
                    methods::SYNC_STEPS
                };
                route_batch(&inner.sync_routes, id, event, feed, OnFull::CloseFeed);
                continue;
            }
            if n.method != methods::TASK_PROGRESS {
                continue;
            }
            let Some(params) = n.params else { continue };
            let Ok(snapshot) = serde_json::from_value::<TaskProgress>(params) else {
                continue;
            };
            let Some(inner) = weak.upgrade() else { return };
            // EPHEMERAL wrapper only to reuse `route` (&self) — it never
            // calls take_*, so the three `None`s are correct.
            RemoteBackend {
                inner,
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
                failed_rx: Mutex::new(None),
                notices_rx: Mutex::new(None),
            }
            .route(snapshot);
        }
        // Dead connection. If there is no external backend left, exit.
        let Some(inner) = weak.upgrade() else { return };
        // EPHEMERAL wrapper (never calls take_*): the three `None`s are correct.
        let backend = RemoteBackend {
            inner,
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(None),
            failed_rx: Mutex::new(None),
            notices_rx: Mutex::new(None),
        };
        *backend.inner.client.write().await = None;
        // Live feeds (a search's hits, a comparison's rows) do NOT survive
        // reconnection: the daemon's pump pointed at the old (dead)
        // `conn_id`. Drops all routes → the `rx`s in flight close (the
        // frontend infers the end from the Task's terminal, reconciled by
        // the `task.list` resync).
        {
            let mut sr = backend
                .inner
                .search_routes
                .lock()
                .expect("search_routes lock is sound");
            sr.clear();
        }
        {
            let mut cr = backend
                .inner
                .compare_routes
                .lock()
                .expect("compare_routes lock is sound");
            cr.clear();
        }
        {
            let mut sr = backend
                .inner
                .sync_routes
                .lock()
                .expect("sync_routes lock is sound");
            sr.clear();
        }
        let _ = backend.inner.events_tx.send(ConnEvent::Lost);
        drop(backend);
        // Reconnection with backoff (does NOT re-start the daemon: M3). If
        // the daemon is version-incompatible, retrying is futile: it gives
        // up (the Lost warning was already sent).
        let mut attempt = 0usize;
        notifications = loop {
            let delay = RECONNECT_BACKOFF_MS[attempt.min(RECONNECT_BACKOFF_MS.len() - 1)];
            attempt += 1;
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let Some(inner) = weak.upgrade() else { return };
            // EPHEMERAL wrapper (never calls take_*): the three `None`s are correct.
            let backend = RemoteBackend {
                inner,
                foreign_rx: Mutex::new(None),
                events_rx: Mutex::new(None),
                approvals_rx: Mutex::new(None),
                degraded_rx: Mutex::new(None),
                failed_rx: Mutex::new(None),
                notices_rx: Mutex::new(None),
            };
            // Is the start-up permit still alive? By TIME, not by attempts
            // (see `Inner::handover_until`): within the window it can keep
            // insisting, which is what lets it outlast the journal lock the
            // old daemon still holds.
            let handoff = {
                let mut until = backend
                    .inner
                    .handover_until
                    .lock()
                    .expect("handover lock is sound");
                let alive = spawn_allowed(*until, std::time::Instant::now());
                if !alive {
                    // Expired: cleared so it is not looked at again.
                    *until = None;
                }
                alive
            };
            match backend.establish(handoff).await {
                Ok(rx) => {
                    let _ = backend.inner.events_tx.send(ConnEvent::Restored);
                    break rx;
                }
                // Version-incompatible daemon: retrying is futile.
                Err(e) if crate::rpc::is_version_mismatch(&e) => return,
                Err(_) => {}
            }
        };
    }
}

/// The FIRST 0.81.0 filter a search request carries, by name, or `None` if
/// it carries none.
///
/// Returns the name and not a boolean because that is what goes into the
/// warning: "your daemon is old" without saying what was refused leaves the
/// reader removing fields blindly until they get it right.
///
/// Pure, so the N-1 promise can be tested with no old daemon to bring up.
#[must_use]
fn first_filter_since_0_81(p: &FsSearchParams) -> Option<&'static str> {
    // It is DESTRUCTURED whole, and that is not style: it is the only thing
    // that keeps the eleventh filter from being forgettable here. A chain of
    // `if`s over `p.field` compiles just the same with a new field
    // unlooked-at, and the test that enumerates today's ten also passes —
    // and then that new filter would travel to a 0.81 daemon that ignores
    // it, which is exactly the superset failure one version later. This
    // way, the new field does not compile until someone decides what to do
    // with it.
    let FsSearchParams {
        root: _,
        name_glob: _,
        name_regex: _,
        content: _,
        content_regex: _,
        case_sensitive: _,
        max_hits: _,
        kinds,
        min_size,
        max_size,
        mtime_after,
        mtime_before,
        exclude_roots,
        exclude_names,
        whole_word,
        recursive,
        encoding,
    } = p;
    if !kinds.is_empty() {
        return Some("kinds");
    }
    if min_size.is_some() {
        return Some("min_size");
    }
    if max_size.is_some() {
        return Some("max_size");
    }
    if mtime_after.is_some() {
        return Some("mtime_after");
    }
    if mtime_before.is_some() {
        return Some("mtime_before");
    }
    if !exclude_roots.is_empty() {
        return Some("exclude_roots");
    }
    if !exclude_names.is_empty() {
        return Some("exclude_names");
    }
    if *whole_word {
        return Some("whole_word");
    }
    // `recursive` is the ONLY one whose default is `true`, so what is being
    // asked for — and what an old daemon would not know to honor — is the
    // `false`.
    if !*recursive {
        return Some("recursive");
    }
    if encoding.is_some() {
        return Some("encoding");
    }
    None
}

/// Does a peer with this version know how to cap `journal.undo_after`
/// (`upto_seq`, 0.80.0)?
///
/// Pure so the N-1 promise can be tested with no old daemon to bring up.
/// Fails CLOSED like the rest of these checks: a version that does not
/// parse knows nothing. With no version — not handshaken yet — the daemon
/// decides.
fn ceiling_honored(peer: Option<&str>) -> bool {
    peer.is_none_or(|v| methods::version_at_least(v, 0, 80))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vpd(wire: &str) -> VPath {
        VPath::parse(wire).expect("valid wire")
    }

    /// A 0.79 daemon would ignore `upto_seq` and undo with no ceiling: with
    /// it, the SDK refuses (`undo_after` returns `Unsupported`) instead of
    /// undoing more than the question counted. A version that does not
    /// parse, the same.
    #[test]
    fn the_ceiling_is_only_sent_to_whoever_knows_how_to_apply_it() {
        assert!(ceiling_honored(Some("0.80.0")));
        assert!(ceiling_honored(Some("0.81.3")));
        assert!(!ceiling_honored(Some("0.79.9")), "N-1 does not know");
        assert!(
            !ceiling_honored(Some("norte-0.80")),
            "does not parse: does not know"
        );
        assert!(
            ceiling_honored(None),
            "with no handshake the daemon decides"
        );
    }

    /// Every 0.81.0 filter is DETECTED, and detected by its name.
    ///
    /// This is what decides whether the search is sent or refused, so a
    /// filter this sweep missed would travel to a daemon that ignores it —
    /// and that daemon would answer with the SUPERSET, which reads exactly
    /// like a result. That is why all ten are here, one by one: a ten-way
    /// `||` passes the test with nine.
    #[test]
    fn every_0_81_filter_is_recognized_by_its_name() {
        use norte_proto::methods::FsSearchParams;
        let base = || FsSearchParams::new(vpd("file:///home"));
        // With nothing: nothing to refuse, and the usual search goes through.
        assert_eq!(first_filter_since_0_81(&base()), None);
        // And 0.18's criteria are not filters either.
        assert_eq!(
            first_filter_since_0_81(&FsSearchParams {
                name_glob: Some("*.rs".into()),
                content: Some("hello".into()),
                case_sensitive: true,
                max_hits: Some(10),
                ..base()
            }),
            None,
            "what 0.80 already knew how to do is not refused"
        );
        let cases: [(&str, FsSearchParams); 10] = [
            (
                "kinds",
                FsSearchParams {
                    kinds: vec![norte_proto::EntryKind::File],
                    ..base()
                },
            ),
            (
                "min_size",
                FsSearchParams {
                    min_size: Some(1),
                    ..base()
                },
            ),
            (
                "max_size",
                FsSearchParams {
                    max_size: Some(1),
                    ..base()
                },
            ),
            (
                "mtime_after",
                FsSearchParams {
                    mtime_after: Some(1),
                    ..base()
                },
            ),
            (
                "mtime_before",
                FsSearchParams {
                    mtime_before: Some(1),
                    ..base()
                },
            ),
            (
                "exclude_roots",
                FsSearchParams {
                    exclude_roots: vec![vpd("file:///casa/x")],
                    ..base()
                },
            ),
            (
                "exclude_names",
                FsSearchParams {
                    exclude_names: vec!["target".into()],
                    ..base()
                },
            ),
            (
                "whole_word",
                FsSearchParams {
                    whole_word: true,
                    ..base()
                },
            ),
            // The ONLY one whose default is `true`: what is being asked for
            // is the `false`.
            (
                "recursive",
                FsSearchParams {
                    recursive: false,
                    ..base()
                },
            ),
            (
                "encoding",
                FsSearchParams {
                    encoding: Some("utf-8".into()),
                    ..base()
                },
            ),
        ];
        for (name, p) in cases {
            assert_eq!(
                first_filter_since_0_81(&p),
                Some(name),
                "{name} is not recognized and would travel to a daemon that ignores it"
            );
        }
        // And `recursive: true` is NOT asking for anything: it is the default.
        assert_eq!(
            first_filter_since_0_81(&FsSearchParams {
                recursive: true,
                ..base()
            }),
            None
        );
    }

    /// A daemon that does not know `plugin.panel_render` leaves the slot
    /// with NO frame, not broken (0.74.0, phase 3).
    ///
    /// This is the N-1 compatibility promise the release note writes in
    /// words, checked here with no socket or old daemon: a 0.74 client
    /// against a 0.73 daemon receives `MethodNotFound`, and that has to
    /// mean "no plugin paints this panel" — the same destination as when
    /// there is no plugin — and not an error that brings down the screen.
    #[test]
    fn a_daemon_with_no_such_method_leaves_the_panel_with_no_frame() {
        let not_there = ClientError::Rpc(norte_proto::wire::RpcError::protocol(
            norte_proto::wire::codes::METHOD_NOT_FOUND,
            "unknown method",
        ));
        assert!(
            map_panel_result(Err(not_there))
                .expect("degrades, does not fail")
                .is_none(),
            "an N-1 daemon leaves the slot with what it already painted"
        );
    }

    /// And any OTHER error DOES propagate: silently degrading on a real
    /// failure is indistinguishable from "this panel does not exist".
    #[test]
    fn another_wire_error_is_not_confused_with_an_absent_panel() {
        let broken = ClientError::ConnectionClosed;
        assert!(map_panel_result(Err(broken)).is_err());
    }

    /// What `transfer` is going to ask: the listed directory's anchor.
    #[test]
    fn a_listed_directorys_anchor_is_remembered_and_returned() {
        let inner = test_inner();
        let dir = vpd("file:///d/sub");
        let a = norte_proto::DirAnchor::new("0123456789abcdef0123456789abcdef".to_owned());
        inner.remember_anchor(&dir, Some(a.clone()));
        assert_eq!(inner.anchor_for(&dir), Some(a));
        assert_eq!(
            inner.anchor_for(&vpd("file:///other")),
            None,
            "a directory nobody listed has no anchor to send"
        );
    }

    /// **A listing with NO anchor deletes whatever there was**, and this is
    /// what avoids the silly failure: reconnecting against a 0.53 daemon —
    /// or listing a destination that stopped knowing how to identify nodes
    /// — would leave an old anchor alive, and the next copy would reject
    /// itself with nothing having actually happened.
    #[test]
    fn a_listing_with_no_anchor_forgets_the_previous_one() {
        let inner = test_inner();
        let dir = vpd("file:///d/sub");
        inner.remember_anchor(
            &dir,
            Some(norte_proto::DirAnchor::new(
                "0123456789abcdef0123456789abcdef".to_owned(),
            )),
        );
        inner.remember_anchor(&dir, None);
        assert_eq!(inner.anchor_for(&dir), None);
    }

    /// The cap must not leave the freshly listed directory with no anchor:
    /// what gets dropped is the OLD one. A panel open in a long session
    /// visits many directories and the one that matters is always the last.
    #[test]
    fn the_cap_drops_the_old_one_and_keeps_the_freshly_listed_one() {
        let inner = test_inner();
        for i in 0..(ANCHORS_MAX + 5) {
            inner.remember_anchor(
                &vpd(&format!("file:///d{i}")),
                Some(norte_proto::DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert_eq!(
            inner.anchor_for(&vpd("file:///d0")),
            None,
            "the first one no longer fits"
        );
        let last = ANCHORS_MAX + 4;
        assert_eq!(
            inner.anchor_for(&vpd(&format!("file:///d{last}"))),
            Some(norte_proto::DirAnchor::new(format!("{last:032x}"))),
            "and the last one survives"
        );
    }

    /// And what keeps being LOOKED AT is not evicted: eviction is LRU
    /// (#301).
    ///
    /// With FIFO — what there was — the active panel's directory left as
    /// soon as `ANCHORS_MAX` different directories passed through the
    /// connection (an expanded tree, a search), even while it was being
    /// relisted every second: the next copy's check disappeared with
    /// nothing said about it.
    #[test]
    fn refreshing_saves_from_eviction() {
        let inner = test_inner();
        let panel = vpd("file:///panel");
        let anchor = norte_proto::DirAnchor::new("a".repeat(32));
        inner.remember_anchor(&panel, Some(anchor.clone()));
        for i in 0..ANCHORS_MAX {
            inner.remember_anchor(&panel, Some(anchor.clone()));
            inner.remember_anchor(
                &vpd(&format!("file:///d{i}")),
                Some(norte_proto::DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert_eq!(
            inner.anchor_for(&panel),
            Some(anchor),
            "what keeps being looked at is not evicted"
        );
    }

    /// Re-listing the SAME directory refreshes its anchor without spending a
    /// slot: if it counted as a new entry, a panel refreshing with F5 would
    /// eat up the cap by itself and evict the other panes.
    #[test]
    fn relisting_the_same_directory_spends_no_slot() {
        let inner = test_inner();
        let dir = vpd("file:///d/sub");
        for i in 0..(ANCHORS_MAX + 5) {
            inner.remember_anchor(&dir, Some(norte_proto::DirAnchor::new(format!("{i:032x}"))));
        }
        inner.remember_anchor(
            &vpd("file:///other"),
            Some(norte_proto::DirAnchor::new(format!("{:032x}", 999))),
        );
        let last = ANCHORS_MAX + 4;
        assert_eq!(
            inner.anchor_for(&dir),
            Some(norte_proto::DirAnchor::new(format!("{last:032x}"))),
            "the last one wins"
        );
        assert_eq!(
            inner.anchor_for(&vpd("file:///other")),
            Some(norte_proto::DirAnchor::new(format!("{:032x}", 999))),
            "and nobody evicted the other directory"
        );
    }

    fn test_inner() -> Arc<Inner> {
        test_inner_for(PathBuf::from("/nonexistent/test.sock"))
    }

    fn test_inner_for(socket: PathBuf) -> Arc<Inner> {
        let (foreign_tx, _fr) = mpsc::unbounded_channel();
        let (events_tx, _er) = mpsc::unbounded_channel();
        let (approvals_tx, _ar) = mpsc::unbounded_channel();
        let (degraded_tx, _dr) = mpsc::unbounded_channel();
        let (failed_tx, _ffr) = mpsc::unbounded_channel();
        let (notices_tx, _fnr) = mpsc::unbounded_channel();
        Arc::new(Inner {
            socket,
            spawn_cmd: None,
            client_info: ClientInfo {
                name: "test".into(),
                version: "0".into(),
            },
            agent_session: None,
            handover_until: Mutex::new(None),
            peer_version: Mutex::new(None),
            client: tokio::sync::RwLock::new(None),
            watches: Mutex::new(HashMap::new()),
            finished: Mutex::new(std::collections::VecDeque::new()),
            foreign_tx,
            events_tx,
            approvals_tx,
            degraded_tx,
            failed_tx,
            notices_tx,
            seen_approvals: Mutex::new(std::collections::HashSet::new()),
            search_routes: Mutex::new(BatchRoutes::default()),
            compare_routes: Mutex::new(BatchRoutes::default()),
            sync_routes: Mutex::new(BatchRoutes::default()),
            anchors: Mutex::new(AnchorCache::default()),
        })
    }

    /// A fake daemon that accepts the handshake and REFUSES `task.list` —
    /// the exact state from #181, which no real daemon knows how to set up.
    ///
    /// Returns on dropping the listener; the test keeps it alive via its
    /// `JoinHandle`.
    fn stub_that_refuses_task_list(socket: &std::path::Path) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(socket).expect("stub bind");
        tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut decoder = norte_proto::wire::FrameDecoder::new();
            let mut buf = vec![0u8; 8192];
            loop {
                let n = match tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if decoder.push(&buf[..n]).is_err() {
                    return;
                }
                while let Some(frame) = decoder.next_frame() {
                    let Ok(req) = serde_json::from_slice::<norte_proto::wire::Request>(&frame)
                    else {
                        continue;
                    };
                    let resp = if req.method == norte_proto::methods::INITIALIZE {
                        norte_proto::wire::Response::ok(
                            req.id.clone(),
                            serde_json::to_value(norte_proto::methods::InitializeResult {
                                server_info: norte_proto::methods::ServerInfo {
                                    name: "stub".into(),
                                    version: "0".into(),
                                },
                                protocol_version: norte_proto::methods::PROTOCOL_VERSION.into(),
                                encodings: vec!["json".into()],
                            })
                            .expect("json"),
                        )
                    } else {
                        // `task.list` (and anything else) is refused: this
                        // is what #181 needs to happen AFTER `establish` has
                        // published the client.
                        norte_proto::wire::Response::err(
                            Some(req.id.clone()),
                            norte_proto::wire::RpcError::protocol(
                                norte_proto::wire::codes::INTERNAL_ERROR,
                                "the stub refuses this on purpose",
                            ),
                        )
                    };
                    let Ok(bytes) = norte_proto::wire::encode_frame(&resp) else {
                        return;
                    };
                    if tokio::io::AsyncWriteExt::write_all(&mut conn, &bytes)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
    }

    /// #181: a resync that fails must NOT leave the client published.
    ///
    /// If it stays, the caller talks over a connection whose notification
    /// receiver died with `establish`'s frame: nothing gets routed,
    /// `task.progress` included, and `TaskRef::join()` — which has no
    /// deadline — waits for a terminal that can no longer arrive. Forever.
    #[tokio::test]
    async fn a_failed_resync_does_not_leave_the_client_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("stub.sock");
        let _stub = stub_that_refuses_task_list(&socket);

        let inner = test_inner_for(socket);
        let backend = RemoteBackend {
            inner: Arc::clone(&inner),
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(None),
            failed_rx: Mutex::new(None),
            notices_rx: Mutex::new(None),
        };

        let r = backend.establish(false).await;
        assert!(r.is_err(), "the stub refuses the resync");
        assert!(
            inner.client.read().await.is_none(),
            "and the slot stays EMPTY: with a client there, nobody routes and a join() hangs forever"
        );
    }

    /// A fake daemon that does the whole handshake — INITIALIZE, an empty
    /// `TASK_LIST`, `POLICY_PENDING` with `METHOD_NOT_FOUND` (which the
    /// resync tolerates, see [`RemoteBackend::resync`]) — and answers any
    /// OTHER method with whatever `responder` returns.
    ///
    /// Separate from [`stub_that_refuses_task_list`] because the
    /// `log_tail`/`log_level` tests do not want to reimplement the whole
    /// handshake to test a single response.
    fn stub_daemon_with(
        socket: &std::path::Path,
        responder: impl Fn(&str) -> norte_proto::wire::Response + Send + Sync + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(socket).expect("stub bind");
        tokio::spawn(async move {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut decoder = norte_proto::wire::FrameDecoder::new();
            let mut buf = vec![0u8; 8192];
            loop {
                let n = match tokio::io::AsyncReadExt::read(&mut conn, &mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if decoder.push(&buf[..n]).is_err() {
                    return;
                }
                while let Some(frame) = decoder.next_frame() {
                    let Ok(req) = serde_json::from_slice::<norte_proto::wire::Request>(&frame)
                    else {
                        continue;
                    };
                    let resp = if req.method == norte_proto::methods::INITIALIZE {
                        norte_proto::wire::Response::ok(
                            req.id.clone(),
                            serde_json::to_value(norte_proto::methods::InitializeResult {
                                server_info: norte_proto::methods::ServerInfo {
                                    name: "stub".into(),
                                    version: "0".into(),
                                },
                                protocol_version: norte_proto::methods::PROTOCOL_VERSION.into(),
                                encodings: vec!["json".into()],
                            })
                            .expect("json"),
                        )
                    } else if req.method == norte_proto::methods::TASK_LIST {
                        norte_proto::wire::Response::ok(
                            req.id.clone(),
                            serde_json::json!({"tasks": []}),
                        )
                    } else if req.method == norte_proto::methods::POLICY_PENDING {
                        norte_proto::wire::Response::err(
                            Some(req.id.clone()),
                            norte_proto::wire::RpcError::protocol(
                                norte_proto::wire::codes::METHOD_NOT_FOUND,
                                "el stub no monta policy.pending",
                            ),
                        )
                    } else {
                        let mut r = responder(&req.method);
                        r.id = Some(req.id.clone());
                        r
                    };
                    let Ok(bytes) = norte_proto::wire::encode_frame(&resp) else {
                        return;
                    };
                    if tokio::io::AsyncWriteExt::write_all(&mut conn, &bytes)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
    }

    /// The response [`stub_daemon_with`] gives when the requested method was
    /// not the one the test expected.
    ///
    /// Deliberately NOT an `assert_eq!` inside the closure: that lives in
    /// `stub_daemon_with`'s `tokio::spawn`ed task, whose `JoinHandle` is
    /// never awaited, so a panic there does not bring down the test — it is
    /// only noticed indirectly, as a different error in `establish()` or in
    /// the call under test. With an error code DISTINGUISHABLE from
    /// `METHOD_NOT_FOUND`, an unexpected method fails the very assertion the
    /// test already makes (`Error::Unsupported` does not come out of here,
    /// or the `expect("with ring: ok")` blows up with the real error).
    fn unexpected_method_response(method: &str) -> norte_proto::wire::Response {
        norte_proto::wire::Response::err(
            None,
            norte_proto::wire::RpcError::protocol(
                norte_proto::wire::codes::INTERNAL_ERROR,
                format!("stub: unexpected method {method}"),
            ),
        )
    }

    /// The really reachable case (see [`methods::LOG_TAIL`]'s rustdoc): a
    /// same-version daemon with NO ring — compiled without the `logging`
    /// feature — answers `METHOD_NOT_FOUND`, and the SDK delivers it as
    /// [`Error::Unsupported`] instead of a raw protocol error, which is what
    /// a panel turns into a sentence (#328).
    #[tokio::test]
    async fn log_tail_against_a_daemon_with_no_ring_degrades_to_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("stub.sock");
        let _stub = stub_daemon_with(&socket, |method| {
            if method == norte_proto::methods::LOG_TAIL {
                norte_proto::wire::Response::err(
                    None,
                    norte_proto::wire::RpcError::protocol(
                        norte_proto::wire::codes::METHOD_NOT_FOUND,
                        "no ring",
                    ),
                )
            } else {
                unexpected_method_response(method)
            }
        });

        let inner = test_inner_for(socket);
        let backend = backend_for(Arc::clone(&inner));
        backend.establish(false).await.expect("handshake");

        let err = backend
            .log_tail(None, 500)
            .await
            .expect_err("no ring: Unsupported");
        assert!(matches!(err, Error::Unsupported));
    }

    /// And against a daemon that DOES have a ring, the lines arrive parsed —
    /// not a raw `serde_json::Value` every frontend would have to
    /// reinterpret.
    #[tokio::test]
    async fn log_tail_against_a_daemon_with_a_ring_returns_the_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("stub.sock");
        let _stub = stub_daemon_with(&socket, |method| {
            if method == norte_proto::methods::LOG_TAIL {
                norte_proto::wire::Response::ok(
                    norte_proto::wire::RequestId::Num(0),
                    serde_json::to_value(methods::LogTailResult {
                        lines: vec![methods::LogLine {
                            epoch_ms: 1_756_000_000_000,
                            level: "info".into(),
                            target: "norte_core::daemon".into(),
                            message: "listening".into(),
                        }],
                        next: 1,
                        lost: 0,
                        level: "info".into(),
                        capacity: 2000,
                    })
                    .expect("json"),
                )
            } else {
                unexpected_method_response(method)
            }
        });

        let inner = test_inner_for(socket);
        let backend = backend_for(Arc::clone(&inner));
        backend.establish(false).await.expect("handshake");

        let r = backend.log_tail(None, 500).await.expect("with ring: ok");
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.lines[0].message, "listening");
        assert_eq!(r.next, 1);
    }

    /// `log.level` degrades the same way as `log.tail`: same daemon, same
    /// reason.
    #[tokio::test]
    async fn log_level_against_a_daemon_with_no_ring_degrades_to_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("stub.sock");
        let _stub = stub_daemon_with(&socket, |method| {
            if method == norte_proto::methods::LOG_LEVEL {
                norte_proto::wire::Response::err(
                    None,
                    norte_proto::wire::RpcError::protocol(
                        norte_proto::wire::codes::METHOD_NOT_FOUND,
                        "no ring",
                    ),
                )
            } else {
                unexpected_method_response(method)
            }
        });

        let inner = test_inner_for(socket);
        let backend = backend_for(Arc::clone(&inner));
        backend.establish(false).await.expect("handshake");

        let err = backend
            .log_level("debug")
            .await
            .expect_err("no ring: Unsupported");
        assert!(matches!(err, Error::Unsupported));
    }

    /// The daemon may answer a level MORE verbose than the one asked for —
    /// it never lowers it (ADR 0092) — and the SDK delivers exactly that,
    /// with no reinterpretation.
    #[tokio::test]
    async fn log_level_returns_the_level_that_really_ended_up_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("stub.sock");
        let _stub = stub_daemon_with(&socket, |method| {
            if method == norte_proto::methods::LOG_LEVEL {
                norte_proto::wire::Response::ok(
                    norte_proto::wire::RequestId::Num(0),
                    serde_json::to_value(methods::LogLevelResult {
                        level: "debug".into(),
                    })
                    .expect("json"),
                )
            } else {
                unexpected_method_response(method)
            }
        });

        let inner = test_inner_for(socket);
        let backend = backend_for(Arc::clone(&inner));
        backend.establish(false).await.expect("handshake");

        let level = backend.log_level("warn").await.expect("with ring: ok");
        assert_eq!(
            level, "debug",
            "the ring does not lower: it answers the current one"
        );
    }

    /// The start-up permit expires by TIME and not by attempts.
    ///
    /// This is the fix for a bug the security review found that nullified
    /// the whole feature: spending it on the first attempt — at 250ms — the
    /// replacement still cannot start, because the old daemon has not left
    /// the process and holds `journal.db`'s lock. The attempt failed, the
    /// permit went with it, and the session stayed dead forever after a
    /// normal update.
    #[test]
    fn the_startup_permit_expires_by_time() {
        let now = std::time::Instant::now();
        // With no notice, never: this is the rule against resurrecting a
        // stopped daemon.
        assert!(!spawn_allowed(None, now));
        // Within the window, as many times as needed — which is what lets
        // it outlast the journal's lock.
        let until = now + HANDOVER_SPAWN_WINDOW;
        assert!(spawn_allowed(Some(until), now));
        assert!(spawn_allowed(Some(until), now + Duration::from_secs(29)));
        // Once past, "the daemon is not there" goes back to its usual meaning.
        assert!(!spawn_allowed(Some(until), now + Duration::from_secs(31)));
    }

    /// The `sync.plan` feed CLOSES when the consumer does not drain, instead
    /// of dropping the batch like the other two do.
    ///
    /// This is the difference that makes the approval safe: a batch of steps
    /// silently dropped, with `sync.plan_done` delivered behind it, would
    /// leave a human approving a `plan_hash` that covers `DeleteTree` and
    /// `Overwrite` they never saw on screen. By closing the feed no closing
    /// event arrives, and with no closing event there is no hash to approve
    /// anything with.
    #[test]
    fn the_sync_feed_closes_instead_of_losing_a_batch() {
        let routes: Mutex<BatchRoutes<u32>> = Mutex::new(BatchRoutes::default());
        let (tx, mut rx) = mpsc::channel::<u32>(1);
        routes.lock().expect("lock").routes.insert(7, tx);

        route_batch(&routes, 7, 1, "sync.steps", OnFull::CloseFeed);
        route_batch(&routes, 7, 2, "sync.steps", OnFull::CloseFeed); // does not fit
        // The route was removed: the `rx` sees what did get in and then the end.
        assert!(
            !routes.lock().expect("lock").routes.contains_key(&7),
            "the feed had to close"
        );
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected));
    }

    /// And a search's or a comparison's does NOT: there a batch is paint,
    /// and closing the whole feed would punish more than it protects.
    #[test]
    fn a_search_feed_drops_the_batch_and_continues() {
        let routes: Mutex<BatchRoutes<u32>> = Mutex::new(BatchRoutes::default());
        let (tx, mut rx) = mpsc::channel::<u32>(1);
        routes.lock().expect("lock").routes.insert(7, tx);

        route_batch(&routes, 7, 1, "search.hits", OnFull::DropBatch);
        route_batch(&routes, 7, 2, "search.hits", OnFull::DropBatch); // is lost
        assert!(
            routes.lock().expect("lock").routes.contains_key(&7),
            "the feed is still alive"
        );
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    fn progress(id: u64, kind: TaskKind, state: TaskState) -> TaskProgress {
        TaskProgress {
            task_id: TaskId::new(id),
            kind,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        }
    }

    fn backend_for(inner: Arc<Inner>) -> RemoteBackend {
        RemoteBackend {
            inner,
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(None),
            failed_rx: Mutex::new(None),
            notices_rx: Mutex::new(None),
        }
    }

    /// ROOT FIX from the review: `route` marks `terminated` ONLY for search
    /// terminals. A foreign copy/move/delete/list terminal never gets in
    /// (if it did, ≥256 of them between a search's terminal and its route's
    /// registration would trigger the `clear` and lose the mark → a hung
    /// route). And a SEARCH terminal is remembered, so `search` schedules
    /// the removal even if the terminal gets ahead of it.
    #[test]
    fn route_only_counts_search_terminals() {
        let inner = test_inner();
        let backend = backend_for(Arc::clone(&inner));

        backend.route(progress(7, TaskKind::Copy, TaskState::Completed));
        backend.route(progress(8, TaskKind::Delete, TaskState::Completed));
        assert!(
            inner
                .search_routes
                .lock()
                .expect("lock")
                .terminated
                .is_empty(),
            "non-search terminals never enter `terminated`"
        );

        backend.route(progress(9, TaskKind::Search, TaskState::Completed));
        assert!(
            inner
                .search_routes
                .lock()
                .expect("lock")
                .terminated
                .contains(&9),
            "the search terminal is marked so `search` sees it"
        );
    }

    /// H2/H3: `mark_terminated` only returns `true` on the new insertion,
    /// so the removal is scheduled ONCE (a duplicate terminal — pump +
    /// resync — does not spawn the grace task again).
    #[test]
    fn mark_terminated_is_only_true_on_a_new_insertion() {
        let mut sr = BatchRoutes::<SearchHits>::default();
        assert!(sr.mark_terminated(1), "first time: freshly inserted");
        assert!(!sr.mark_terminated(1), "repeated: does not reschedule");
        assert!(sr.mark_terminated(2), "another id: freshly inserted");
    }

    /// #44: `connection.degraded`'s seam mirrors the approvals' one —
    /// `push_degraded` (what the pump's arm does) delivers to the receiver
    /// `take_degraded` takes ONCE.
    #[test]
    fn degraded_push_reaches_take_degraded() {
        let (degraded_tx, degraded_rx) = mpsc::unbounded_channel();
        let (failed_tx, _ffr) = mpsc::unbounded_channel();
        let (notices_tx, _fnr) = mpsc::unbounded_channel();
        let (foreign_tx, _fr) = mpsc::unbounded_channel();
        let (events_tx, _er) = mpsc::unbounded_channel();
        let (approvals_tx, _ar) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            socket: PathBuf::from("/nonexistent/test.sock"),
            spawn_cmd: None,
            client_info: ClientInfo {
                name: "test".into(),
                version: "0".into(),
            },
            agent_session: None,
            handover_until: Mutex::new(None),
            peer_version: Mutex::new(None),
            client: tokio::sync::RwLock::new(None),
            watches: Mutex::new(HashMap::new()),
            finished: Mutex::new(std::collections::VecDeque::new()),
            foreign_tx,
            events_tx,
            approvals_tx,
            degraded_tx,
            failed_tx,
            notices_tx,
            seen_approvals: Mutex::new(std::collections::HashSet::new()),
            search_routes: Mutex::new(BatchRoutes::default()),
            compare_routes: Mutex::new(BatchRoutes::default()),
            sync_routes: Mutex::new(BatchRoutes::default()),
            anchors: Mutex::new(AnchorCache::default()),
        });
        let backend = RemoteBackend {
            inner: Arc::clone(&inner),
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(Some(degraded_rx)),
            failed_rx: Mutex::new(None),
            notices_rx: Mutex::new(None),
        };

        inner.push_degraded(ConnectionDegraded {
            scheme: "ftp".into(),
            host: "example.test".into(),
            reason: "tls-auth-rejected".into(),
            detail: None,
        });

        let mut rx = backend.take_degraded().expect("first owner takes it");
        let got = rx.try_recv().expect("the warning reached the receiver");
        assert_eq!(got.scheme, "ftp");
        assert_eq!(got.reason, "tls-auth-rejected");
        // Just one: a second `take_*` sees `None` (like the other channels).
        assert!(
            backend.take_degraded().is_none(),
            "the receiver is one-shot, same as take_approvals"
        );
    }

    /// #322: the `connection.failed` channel is `degraded`'s with a
    /// different payload. Tested separately because they are TWO channels:
    /// mixing them up would make a connection failure arrive as a
    /// degradation and vice versa.
    #[test]
    fn failed_push_reaches_take_failed() {
        let (failed_tx, failed_rx) = mpsc::unbounded_channel();
        let (notices_tx, notices_rx) = mpsc::unbounded_channel();
        let (degraded_tx, _dr) = mpsc::unbounded_channel();
        let (foreign_tx, _fr) = mpsc::unbounded_channel();
        let (events_tx, _er) = mpsc::unbounded_channel();
        let (approvals_tx, _ar) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            socket: PathBuf::from("/nonexistent/test.sock"),
            spawn_cmd: None,
            client_info: ClientInfo {
                name: "test".into(),
                version: "0".into(),
            },
            agent_session: None,
            handover_until: Mutex::new(None),
            peer_version: Mutex::new(None),
            client: tokio::sync::RwLock::new(None),
            watches: Mutex::new(HashMap::new()),
            finished: Mutex::new(std::collections::VecDeque::new()),
            foreign_tx,
            events_tx,
            approvals_tx,
            degraded_tx,
            failed_tx,
            notices_tx,
            seen_approvals: Mutex::new(std::collections::HashSet::new()),
            search_routes: Mutex::new(BatchRoutes::default()),
            compare_routes: Mutex::new(BatchRoutes::default()),
            sync_routes: Mutex::new(BatchRoutes::default()),
            anchors: Mutex::new(AnchorCache::default()),
        });
        let backend = RemoteBackend {
            inner: Arc::clone(&inner),
            foreign_rx: Mutex::new(None),
            events_rx: Mutex::new(None),
            approvals_rx: Mutex::new(None),
            degraded_rx: Mutex::new(None),
            failed_rx: Mutex::new(Some(failed_rx)),
            notices_rx: Mutex::new(Some(notices_rx)),
        };

        inner.push_failed(ConnectionFailed {
            conn: Some("rosetta".into()),
            scheme: "s3".into(),
            host: "rosetta.example.test".into(),
            reason: "secret-empty".into(),
            detail: Some("the configured key is empty".into()),
        });

        let mut rx = backend.take_failed().expect("first owner takes it");
        let got = rx.try_recv().expect("the failure reached the receiver");
        assert_eq!(got.reason, "secret-empty");
        assert_eq!(
            got.detail.as_deref(),
            Some("the configured key is empty"),
            "the detail is the only thing this channel exists to carry"
        );
        assert!(
            backend.take_failed().is_none(),
            "the receiver is one-shot, same as take_degraded"
        );
        // And they do NOT cross: the degraded channel stays empty.
        assert!(
            backend.take_degraded().is_none(),
            "this backend did not wire up degraded; a failure must not show up there"
        );
    }

    /// G3a (ADR 0037): `plugin.preview_styled`'s `METHOD_NOT_FOUND` (-32601)
    /// translates to `Ok(None)` — a 0.27 daemon with this handler not wired
    /// up yet degrades EXACTLY like "no previewer applies"; the caller
    /// (frontend) falls back to the plain preview. Builds the `ClientError`
    /// by hand (no socket): this is the REAL trigger within the 0.27
    /// window, distinct from `VERSION_MISMATCH` (which does not even let
    /// the call happen).
    #[test]
    fn plugin_preview_styled_method_not_found_is_none() {
        let err = ClientError::Rpc(norte_proto::wire::RpcError::protocol(
            norte_proto::wire::codes::METHOD_NOT_FOUND,
            "unknown method: plugin.preview_styled",
        ));
        let got = map_styled_preview_result(Err(err)).expect("METHOD_NOT_FOUND is not an error");
        assert_eq!(
            got, None,
            "falls back to Ok(None), as if no previewer applied"
        );
    }

    /// An `Ok` with `preview: Some(..)` passes through as is.
    #[test]
    fn plugin_preview_styled_ok_passes_the_preview_through() {
        let preview = methods::PluginPreviewStyled {
            plugin_id: "org.norte.demo".into(),
            plugin_name: "Demo".into(),
            lines: vec![vec![methods::SpanWire {
                text: "hello".into(),
                role: None,
                fg: None,
                bg: None,
            }]],
            lossy: false,
        };
        let got = map_styled_preview_result(Ok(methods::PluginPreviewStyledResult {
            preview: Some(preview.clone()),
        }))
        .expect("Ok passes through");
        assert_eq!(got, Some(preview));
    }

    /// A REAL failure (not `METHOD_NOT_FOUND`) propagates via `to_taxonomy`
    /// — never silently confused with "no handler yet".
    #[test]
    fn plugin_preview_styled_another_error_propagates() {
        let err = ClientError::Rpc(norte_proto::wire::RpcError::from(Error::Internal {
            panic: false,
        }));
        let got = map_styled_preview_result(Err(err));
        assert!(
            matches!(got, Err(Error::Internal { panic: false })),
            "a real failure propagates, it is not confused with METHOD_NOT_FOUND: {got:?}"
        );
    }
}
