//! Operation hooks (H1, ADR 0100): a `hook` plugin OBSERVES the entries the
//! journal already recorded, and the only thing it can return is a sentence
//! for the human.
//!
//! The source is the journal and not the handlers: [`crate::journal::Journal::
//! record_entry`] offers each committed row to a [`HookSender`], so every
//! mutation — from any frontend, from the CLI, from an agent, from a batch,
//! from an undo — arrives through the same place (ADR 0077). The dispatcher
//! ([`spawn_dispatcher`]) lives OUTSIDE the critical path: a bounded queue, a
//! per-batch drain, a per-batch rediscovery of the registry (so a
//! just-granted approval counts on the next one), and one instance per
//! plugin that is REUSED across batches while its `.wasm` does not change.
//! Three failures in a row turn off that plugin's hooks until it is disabled
//! and re-enabled, and it is said over the same channel as the sentences.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use norte_plugin_host::{HOOK_EVENTS, HookInstance, PluginRuntime, hook_iface};
use norte_proto::methods::PluginNotice;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::plugins::{LocationMint, LocationSession, PluginRegistry, guest_reason};
use crate::policy::{OpSet, Scope, ScopeRegistry};

/// How many events fit in the queue between the journal and the dispatcher.
/// Above that, [`HookSender::offer`] drops the NEWEST one and counts it: the
/// mutation already happened and will not be held back by a slow observer.
/// What was dropped is TOLD to the guest on the next call (`dropped`).
pub const HOOK_QUEUE: usize = 1024;

/// How many events are handed to a guest in one call at most. It is also
/// what caps the cost of a drain: a batch of ten thousand renames arrives in
/// forty calls, not in one or in ten thousand.
pub const HOOK_DRAIN_MAX: usize = 256;

/// Failures IN A ROW — did not instantiate, panicked, went over budget,
/// refused — after which a plugin's hooks turn off. A success in between
/// resets the counter to zero; disabling the plugin in the manager rearms
/// it.
pub const HOOK_FUSE_FAILURES: u32 = 3;

/// How many notices a plugin can drop at once, and at what rate the quota
/// refills: one per second. A hook is one sentence per thing that happened,
/// not a channel; and without a cap, one sentence per batch would step on
/// the status bar's single transient-message slot — including the notice
/// that its own hooks turned off.
pub const HOOK_NOTICE_BURST: u32 = 4;

/// A journal entry exactly as it comes out of `record_entry`: wire bytes,
/// uninterpreted. What the guest receives is built in the dispatcher
/// (`to_wire_events`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// The assigned `seq`.
    pub seq: i64,
    /// UTC milliseconds of the record.
    pub ts_ms: i64,
    /// `"created" | "removed" | "trashed" | "renamed" | "mode_changed"`.
    pub op: String,
    /// `"user" | "agent" | "plugin"`. The actor's id does NOT travel (ADR
    /// 0100).
    pub actor_kind: String,
    /// Affected path (`to_wire` bytes).
    pub path: Vec<u8>,
    /// The name that WAS there in a `renamed` (the journal keeps the new one
    /// in `path`) / the new mode of a `mode_changed`.
    pub path_to: Option<Vec<u8>>,
    /// Batch, if it was part of one.
    pub batch_id: Option<i64>,
}

/// The journal's end: offers events without ever waiting.
#[derive(Debug, Clone)]
pub struct HookSender {
    tx: mpsc::Sender<HookEvent>,
    /// Dropped since startup (to look at) and since the last batch (to tell
    /// the guest); the dispatcher empties the second.
    dropped: Arc<AtomicU64>,
    dropped_since: Arc<AtomicU64>,
}

impl HookSender {
    /// Queues `ev` if it fits. Never blocks nor fails: with the queue full
    /// the event is dropped and counted ([`Self::dropped`]); with the
    /// dispatcher dead, it is dropped silently — there is nobody left to
    /// tell.
    pub fn offer(&self, ev: HookEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(ev) {
            self.dropped_since.fetch_add(1, Ordering::Relaxed);
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // One trace per power of two: the first says it is happening,
            // the following ones say how much, and none of them turns a
            // full queue into a full log.
            if n.is_power_of_two() {
                tracing::warn!(dropped = n, "hooks: queue full, events dropped");
            }
        }
    }

    /// A (sender, receiver) pair with no dispatcher, to look at what the
    /// journal offers.
    #[cfg(test)]
    pub(crate) fn for_test(capacity: usize) -> (Self, mpsc::Receiver<HookEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
                dropped_since: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// How many events were dropped for a full queue since startup.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Where notices go: the daemon broadcasts them to humans via
/// `plugin.notice`; an embedded frontend pushes them to its channel.
pub trait HookNoticeSink: Send + Sync {
    /// A notice, already masked and capped.
    fn notice(&self, n: PluginNotice);

    /// `true` when there is nobody left on the other side: the dispatcher
    /// ends on the next batch. This is what ties the embedded dispatcher's
    /// life to that of the frontend that took the channel.
    fn is_closed(&self) -> bool {
        false
    }
}

/// Where a sidecar writes (ADR 0101): the engine, as the `plugin` actor, and
/// the scope registry that grants the event's directory for the duration of
/// the write. `Weak` because the engine holds the journal, which holds the
/// dispatcher's end: an `Arc` here would be a cycle. Without a registry
/// (embedded mode, no policy) the gate is `AllowAll` and what caps it is
/// what the dispatcher already checked: the manifest name, the event's
/// parent, neither protected nor a ceiling.
#[derive(Clone)]
pub struct SidecarWriter {
    /// The engine that writes.
    pub engine: std::sync::Weak<crate::Engine>,
    /// The daemon's scope registry, if there is one.
    pub scopes: Option<ScopeRegistry>,
    /// The `policy.toml` rules when the engine carries NO gate (embedded
    /// mode): evaluated here for the `plugin` actor, so that the rule
    /// `actor = "plugin", action = "deny"` holds in the TUI the same as in
    /// the daemon. `None` = no file, and with no file an approved plugin
    /// writes (its rule is the manifest, ADR 0101).
    pub policy: Option<Arc<crate::PolicyConfig>>,
}

/// How long the transient scope granted to a plugin for ONE write lives if
/// something prevented revoking it: the gate is evaluated when queuing, so
/// the door closes with `revoke_all` as soon as it returns, and this is the
/// safety net.
const SIDECAR_SCOPE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// A sidecar already validated by the dispatcher, pending the engine writing
/// it: the guest asked for `name` alongside the `seq` event; this is where
/// it goes.
#[derive(Debug)]
struct PendingWrite {
    plugin_id: String,
    parent: norte_proto::VPath,
    path: norte_proto::VPath,
    content: Vec<u8>,
    on_exists: crate::ops::OnExists,
}

/// The per-plugin fuse: counts failures IN A ROW and turns off on reaching
/// [`HOOK_FUSE_FAILURES`]. Pure, clockless, so it can be tested.
#[derive(Debug, Default)]
pub(crate) struct Fuse {
    failures: HashMap<String, u32>,
    disabled: HashSet<String>,
}

impl Fuse {
    /// Are `id`'s hooks turned off?
    pub(crate) fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// A call that went well: the counter goes back to zero.
    pub(crate) fn record_ok(&mut self, id: &str) {
        self.failures.remove(id);
    }

    /// A call that failed. Returns `true` the time it TURNS OFF the
    /// plugin's hooks (and only that time), to warn once and not on every
    /// batch.
    pub(crate) fn record_failure(&mut self, id: &str) -> bool {
        if self.disabled.contains(id) {
            return false;
        }
        let n = self.failures.entry(id.to_owned()).or_insert(0);
        *n += 1;
        if *n >= HOOK_FUSE_FAILURES {
            self.disabled.insert(id.to_owned());
            self.failures.remove(id);
            return true;
        }
        false
    }

    /// Rearms the plugins that are NO LONGER consented: disabling one in the
    /// manager (or withdrawing its approval) is what the shutdown notice
    /// asks of the reader, and it has to be true. The one that gets
    /// re-enabled starts with the counter at zero.
    pub(crate) fn rearm_missing(&mut self, present: &HashSet<&str>) {
        self.disabled.retain(|id| present.contains(id.as_str()));
        self.failures.retain(|id, _| present.contains(id.as_str()));
    }
}

/// A per-plugin notice quota: [`HOOK_NOTICE_BURST`] at once and one per
/// second after that. Pure over an instant it is given, so it can be tested.
#[derive(Debug)]
pub(crate) struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(now: Instant) -> Self {
        Self {
            tokens: f64::from(HOOK_NOTICE_BURST),
            last: now,
        }
    }

    /// Is there quota for a notice now? Consumes one if there is.
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed).min(f64::from(HOOK_NOTICE_BURST));
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// An instance alive across batches, with what is needed to know whether it
/// still serves: the `.wasm` that was instantiated and its footprint on
/// disk.
struct Live {
    inst: HookInstance,
    wasm: norte_plugin_host::WasmArtifact,
    stamp: Option<(std::time::SystemTime, u64)>,
}

/// What the dispatcher keeps across batches. Under ONE lock, and shared with
/// the blocking task via `Arc`: if a batch dies with a panic, what was there
/// — the disabled ones, above all — is still there; a host failure does not
/// re-enable a plugin that turned off from failing.
#[derive(Default)]
struct State {
    fuse: Fuse,
    live: HashMap<String, Live>,
    /// When each plugin was first seen consented (ms UTC): an event before
    /// that was recorded before the human approved it, and is not delivered
    /// to it.
    first_seen: HashMap<String, i64>,
    buckets: HashMap<String, Bucket>,
    /// Who has already been told the policy denied them an effect: once per
    /// plugin and process; after that, to the log.
    denied_told: HashSet<String>,
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX)
}

/// Starts the dispatcher and returns the end that gets installed on the
/// journal ([`crate::Engine::enable_hooks`]) and the task, for whoever wants
/// to wait on it. `config_dir` is where `plugins/` and `plugins-state.toml`
/// live; the registry is rediscovered on every batch so that a just-granted
/// approval counts without restarting anything.
///
/// Ends with `cancel` (the daemon shutting down), when `sink` says there is
/// nobody left on the other side, or when the last [`HookSender`] dies. A
/// guest call in progress is not interrupted — it is bounded by the epoch
/// budget — but is not waited on: cancelling returns right away.
#[must_use]
pub fn spawn_dispatcher(
    config_dir: PathBuf,
    runtime: Arc<PluginRuntime>,
    sink: Arc<dyn HookNoticeSink>,
    cancel: CancellationToken,
    writer: Option<SidecarWriter>,
) -> (HookSender, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<HookEvent>(HOOK_QUEUE);
    let sender = HookSender {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
        dropped_since: Arc::new(AtomicU64::new(0)),
    };
    let dropped_since = Arc::clone(&sender.dropped_since);
    let task = crate::blocking::spawn(async move {
        let state = Arc::new(Mutex::new(State::default()));
        // Plugins ALREADY consented at startup receive everything that
        // arrives: their approval predates this process. Ones approved
        // afterward start at the batch that first sees them, and what was
        // recorded before that batch is not delivered to them (it could
        // predate the approval, and there is no way to know).
        {
            let dir = config_dir.clone();
            let st = Arc::clone(&state);
            let seeded = crate::blocking::spawn_blocking(move || {
                let ids = consented_hook_ids(&dir);
                let mut guard = st.lock().unwrap_or_else(PoisonError::into_inner);
                for id in ids {
                    guard.first_seen.insert(id, i64::MIN);
                }
            })
            .await;
            if let Err(e) = seeded {
                tracing::warn!(error = %e, "hooks: could not read the registry at startup");
            }
        }
        loop {
            let first = tokio::select! {
                () = cancel.cancelled() => break,
                got = rx.recv() => match got {
                    Some(ev) => ev,
                    None => break,
                },
            };
            if sink.is_closed() {
                break;
            }
            let mut batch = vec![first];
            while batch.len() < HOOK_DRAIN_MAX {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            // Two concurrent `record_entry` calls can offer out of order:
            // the `seq` is assigned under the chain's lock and the offer
            // happens after releasing it. The WIT promises `seq` order, and
            // it is honored here.
            batch.sort_unstable_by_key(|e| e.seq);
            let dropped = dropped_since.swap(0, Ordering::Relaxed);
            // Everything that follows is synchronous I/O and CPU —
            // discovering the registry, opening directories, running wasm —
            // so it goes in `spawn_blocking` (rule 2). The state travels via
            // `Arc`: a panic in the batch does not lose it.
            let dir = config_dir.clone();
            let rt = Arc::clone(&runtime);
            let st = Arc::clone(&state);
            let work = crate::blocking::spawn_blocking(move || {
                let mut guard = st.lock().unwrap_or_else(PoisonError::into_inner);
                dispatch_batch(&dir, &rt, &mut guard, &batch, dropped)
            });
            let out = tokio::select! {
                () = cancel.cancelled() => break,
                out = work => out,
            };
            match out {
                Ok((notices, pending)) => {
                    for n in notices {
                        sink.notice(n);
                    }
                    // The writes go AFTER the sentences and on the async
                    // side: each one is an engine Task that goes through the
                    // gate and the journal as the `plugin` actor.
                    // Sequential on purpose: the e2e counts on batch N+1 not
                    // draining until N's writes finished.
                    for w in pending {
                        let denied = tokio::select! {
                            () = cancel.cancelled() => return,
                            d = apply_write(writer.as_ref(), w) => d,
                        };
                        if let Some(id) = denied {
                            let first = state
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .denied_told
                                .insert(id.clone());
                            if first {
                                sink.notice(PluginNotice {
                                    plugin_id: id,
                                    kind: KIND_EFFECT_DENIED.to_owned(),
                                    text: None,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "hooks: a batch's dispatcher died");
                }
            }
        }
    });
    (sender, task)
}

/// Writes ONE sidecar via the engine. Returns `Some(plugin_id)` if the
/// human's policy denied it — the only outcome that is told to the human;
/// the rest goes to the log. A `Conflict` with `refuse` is what the guest
/// asked for.
async fn apply_write(writer: Option<&SidecarWriter>, w: PendingWrite) -> Option<String> {
    let Some(writer) = writer else {
        tracing::debug!(plugin = %w.plugin_id, "sidecar: no writer, discarded");
        return None;
    };
    let engine = writer.engine.upgrade()?;
    let actor = crate::journal::Actor::Plugin {
        id: w.plugin_id.clone(),
    };
    // With no gate on the engine (embedded), the human's rules are checked
    // here: a `deny` or an `ask` on the `plugin` actor is a no; with no
    // rule, the approved manifest is the rule.
    if writer.scopes.is_none()
        && let Some(policy) = &writer.policy
    {
        use crate::policy::{Decision, DenyReason, PolicyOp};
        let ops: &[PolicyOp] = if w.on_exists == crate::ops::OnExists::Replace {
            &[
                PolicyOp::Create,
                PolicyOp::Delete {
                    mode: norte_proto::DeleteMode::Trash,
                },
            ]
        } else {
            &[PolicyOp::Create]
        };
        for op in ops {
            match policy.decide(&actor, *op, &[&w.path]) {
                Decision::Allow | Decision::Deny(DenyReason::NoRule) => {}
                Decision::Deny(reason) => {
                    tracing::info!(plugin = %w.plugin_id, ?reason, "sidecar: denied by policy (embedded)");
                    return Some(w.plugin_id);
                }
                Decision::Ask => {
                    tracing::info!(plugin = %w.plugin_id, "sidecar: the policy asks a plugin for confirmation: denied");
                    return Some(w.plugin_id);
                }
            }
        }
    }
    // The transient scope: the event's directory, create and delete, under
    // the plugin's key (`plugin:<id>`, never an agent's). Revoked as soon as
    // it returns; the TTL is the safety net. Without it, the daemon's gate
    // denies `OutOfScope` — a plugin has no session to request scopes with.
    let key = crate::policy::scope_key(&actor).map(std::borrow::Cow::into_owned);
    if let (Some(scopes), Some(key)) = (&writer.scopes, &key) {
        scopes.grant(
            key,
            Scope {
                roots: vec![w.parent.clone()],
                ops: OpSet::of(&["create", "delete"]),
                expires_at: Some(Instant::now() + SIDECAR_SCOPE_TTL),
            },
        );
    }
    let queued = engine
        .write_file_as(&w.path, w.content, w.on_exists, actor)
        .await;
    if let (Some(scopes), Some(key)) = (&writer.scopes, &key) {
        scopes.revoke_all(key);
    }
    match queued {
        Ok(handle) => {
            match handle.join().await {
                norte_proto::TaskState::Completed => {}
                norte_proto::TaskState::Failed {
                    error: norte_proto::Error::Conflict { .. },
                } => {
                    tracing::debug!(plugin = %w.plugin_id, "sidecar: already exists and the guest asked not to touch it");
                }
                other => {
                    tracing::warn!(plugin = %w.plugin_id, ?other, "sidecar: the write did not finish cleanly");
                }
            }
            None
        }
        Err(norte_proto::Error::PolicyDenied { rule }) => {
            tracing::info!(plugin = %w.plugin_id, %rule, "sidecar: denied by policy");
            Some(w.plugin_id)
        }
        Err(e) => {
            tracing::warn!(plugin = %w.plugin_id, error = %e, "sidecar: the engine did not accept it");
            None
        }
    }
}

/// The manifest's event name for a journal op, or `None` for an op this
/// binary does not know how to name (a newer journal).
pub(crate) fn event_name_for(op: &str) -> Option<&'static str> {
    let wanted = format!("after-{}", op.replace('_', "-"));
    HOOK_EVENTS.iter().copied().find(|e| *e == wanted)
}

fn wire_op(op: &str) -> Option<hook_iface::Op> {
    Some(match op {
        "created" => hook_iface::Op::Created,
        "removed" => hook_iface::Op::Removed,
        "trashed" => hook_iface::Op::Trashed,
        "renamed" => hook_iface::Op::Renamed,
        "mode_changed" => hook_iface::Op::ModeChanged,
        _ => return None,
    })
}

fn wire_actor(kind: &str) -> Option<hook_iface::ActorKind> {
    Some(match kind {
        "user" => hook_iface::ActorKind::User,
        "agent" => hook_iface::ActorKind::Agent,
        "plugin" => hook_iface::ActorKind::Plugin,
        _ => return None,
    })
}

/// The wire form WITHOUT userinfo: `sftp://ana@host/x` → `sftp://host/x`. A
/// hook with no network capability has no reason to learn which user the
/// human logs into each machine with; the host and the path already say
/// what changed.
fn without_userinfo(wire: &str) -> String {
    let Some((scheme, rest)) = wire.split_once("://") else {
        return wire.to_owned();
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if rest.contains('/') {
        format!("{scheme}://{host}/{path}")
    } else {
        format!("{scheme}://{host}")
    }
}

/// A batch's events that `ons` asks for, in the guest's shape, minus the
/// ones that fall under a protected root and minus the ones before
/// `since_ms`. With `mint`, one location session per distinct PARENT
/// directory, that lives as long as the call does (returned so the caller
/// holds them); a parent that is `$HOME` or the system root is not opened —
/// a hook looks at the result of a mutation, not the whole disk.
fn to_wire_events(
    batch: &[HookEvent],
    ons: &[String],
    since_ms: i64,
    protected: &[norte_proto::VPath],
    mint: Option<&Arc<LocationMint>>,
) -> (Vec<hook_iface::Event>, Vec<LocationSession>) {
    let mut sessions: Vec<LocationSession> = Vec::new();
    // `None` cached too: a parent that does not open is not retried for
    // each of its two hundred children.
    let mut by_parent: BTreeMap<String, Option<usize>> = BTreeMap::new();
    let mut out = Vec::new();
    for ev in batch {
        if ev.ts_ms < since_ms {
            continue;
        }
        // What a plugin writes — a sidecar — does not come back as an event
        // to any hook: a hook that listened on `after-created` and wrote a
        // sidecar would call itself forever (ADR 0101).
        if ev.actor_kind == "plugin" {
            continue;
        }
        let Some(name) = event_name_for(&ev.op) else {
            continue;
        };
        if !ons.iter().any(|o| o == name) {
            continue;
        }
        let (Some(op), Some(actor)) = (wire_op(&ev.op), wire_actor(&ev.actor_kind)) else {
            continue;
        };
        // The journal's path is `VPath::to_wire`, i.e. text by
        // construction; if it were not, it is a row this binary did not
        // write and it is not passed to anyone — and it is said.
        let Ok(path) = String::from_utf8(ev.path.clone()) else {
            tracing::warn!(seq = ev.seq, "hooks: row with non-UTF-8 path, skipped");
            continue;
        };
        let vpath = norte_proto::VPath::parse(&path).ok();
        if let Some(v) = &vpath
            && protected
                .iter()
                .any(|root| crate::policy::is_under(root, v))
        {
            // Under the daemon's state there is nothing a plugin should
            // see, not even the name.
            continue;
        }
        let path_to = ev
            .path_to
            .as_ref()
            .and_then(|b| String::from_utf8(b.clone()).ok())
            .map(|s| without_userinfo(&s));
        let leaf = vpath
            .as_ref()
            .and_then(|v| v.file_name().map(|s| s.as_bytes().to_vec()))
            .unwrap_or_default();
        let location = mint.and_then(|m| {
            let parent = vpath.as_ref()?.parent()?;
            if m.is_ceiling(&parent) {
                return None;
            }
            let key = parent.to_wire();
            let idx = if let Some(i) = by_parent.get(&key) {
                (*i)?
            } else {
                // No marker and no going up: whoever runs is a plugin, and
                // what it sees is the mutation's directory and nothing else.
                let minted = m.mint_for(&parent, None, false).map(|s| {
                    sessions.push(s);
                    sessions.len() - 1
                });
                by_parent.insert(key, minted);
                minted?
            };
            let r = sessions[idx].as_ref();
            Some(hook_iface::LocationRef {
                token: r.token,
                prefix: r.prefix,
            })
        });
        out.push(hook_iface::Event {
            seq: u64::try_from(ev.seq).unwrap_or_default(),
            ts_ms: ev.ts_ms,
            op,
            actor,
            path: without_userinfo(&path),
            path_to,
            name: leaf,
            batch: ev.batch_id.and_then(|b| u64::try_from(b).ok()),
            location,
        });
    }
    (out, sessions)
}

/// The ids of the hooks currently consented. BLOCKING.
fn consented_hook_ids(config_dir: &std::path::Path) -> Vec<String> {
    PluginRegistry::discover(config_dir)
        .map(|reg| reg.resolve_hooks().into_iter().map(|(r, _)| r.0).collect())
        .unwrap_or_default()
}

fn stamp_of(wasm: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(wasm).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// One batch against every consented hook. BLOCKING.
#[tracing::instrument(skip_all, fields(batch_len = batch.len(), dropped))]
fn dispatch_batch(
    config_dir: &std::path::Path,
    runtime: &PluginRuntime,
    state: &mut State,
    batch: &[HookEvent],
    dropped: u64,
) -> (Vec<PluginNotice>, Vec<PendingWrite>) {
    let mut notices = Vec::new();
    let mut writes = Vec::new();
    let reg = match PluginRegistry::discover(config_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "hooks: could not read the plugin registry");
            return (notices, writes);
        }
    };
    let hooks = reg.resolve_hooks();
    let present: HashSet<&str> = hooks.iter().map(|(r, _)| r.0.as_str()).collect();
    state.fuse.rearm_missing(&present);
    state.live.retain(|id, _| present.contains(id.as_str()));
    state
        .first_seen
        .retain(|id, _| present.contains(id.as_str()));
    let protected = crate::policy::protected_roots();
    let now = Instant::now();
    let now_ms = now_ms();
    // One minter per batch: mints tokens for hooks with `location` and knows
    // the ceiling (`$HOME`, `/`) that is neither read nor written.
    let mint = LocationMint::new(norte_vfs_local::Bounds::default());
    for (resolved, ons) in hooks {
        let (id, _name, wasm, caps, settings) = resolved;
        if state.fuse.is_disabled(&id) {
            continue;
        }
        let since = *state.first_seen.entry(id.clone()).or_insert(now_ms);
        let location_mint = caps.location.granted().then_some(&mint);
        let (events, _sessions) = to_wire_events(batch, &ons, since, &protected, location_mint);
        if events.is_empty() {
            continue;
        }
        let host: Option<Arc<dyn norte_plugin_host::LocationHost>> =
            location_mint.map(|m| Arc::clone(m) as Arc<dyn norte_plugin_host::LocationHost>);
        let sidecar_names = caps.fs_write.sidecar_names().to_vec();
        let outcome = ensure_live(state, runtime, &id, &wasm, caps).and_then(|live| {
            live.inst.set_location(host);
            live.inst.set_settings(settings);
            let out = live.inst.on_events(&events, dropped);
            // The token dies with `_sessions` at the end of the iteration;
            // the instance is left without a resolver until the next batch.
            live.inst.set_location(None);
            out.map_err(|e| e.to_string())
        });
        match outcome {
            Ok(Ok(effects)) => {
                let spoken = speak(
                    state,
                    &id,
                    effects,
                    now,
                    &events,
                    &sidecar_names,
                    &protected,
                    &mint,
                );
                notices.extend(spoken.notices);
                writes.extend(spoken.writes);
                // A malformed effect — a name outside the manifest, a `seq`
                // that is not in the call — is the guest's fault, even if
                // the rest of the call was valid.
                if spoken.malformed {
                    tracing::warn!(plugin = %id, "hook: malformed effect");
                    if state.fuse.record_failure(&id) {
                        notices.push(disabled_notice(&id));
                    }
                } else {
                    state.fuse.record_ok(&id);
                }
            }
            Ok(Err(sentence)) => {
                tracing::warn!(plugin = %id, reason = %guest_reason(&sentence), "hook: the guest refused");
                state.live.remove(&id);
                if state.fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
            Err(e) => {
                tracing::warn!(plugin = %id, error = %e, "hook: failed to run");
                state.live.remove(&id);
                if state.fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
        }
    }
    (notices, writes)
}

/// The live instance for `id`, reused while the `.wasm` is the same
/// unchanged file: compiling one component per batch is what would turn
/// "outside the critical path" into "a pool thread busy for the whole
/// batch". Instantiates if needed; `Err` if it could not.
fn ensure_live<'s>(
    state: &'s mut State,
    runtime: &PluginRuntime,
    id: &str,
    wasm: &norte_plugin_host::WasmArtifact,
    caps: norte_plugin_host::Capabilities,
) -> Result<&'s mut Live, String> {
    let stamp = stamp_of(wasm.path());
    // The WHOLE artifact, footprint included (ADR 0142): a live instance
    // does not serve a binary that was re-approved with different bytes.
    let reuse = state
        .live
        .get(id)
        .is_some_and(|l| l.wasm == *wasm && l.stamp == stamp && l.stamp.is_some());
    if !reuse {
        state.live.remove(id);
        let inst = runtime
            .instantiate_hook_with_location(wasm, caps, None)
            .map_err(|e| e.to_string())?;
        state.live.insert(
            id.to_owned(),
            Live {
                inst,
                wasm: wasm.clone(),
                stamp,
            },
        );
    }
    state
        .live
        .get_mut(id)
        .ok_or_else(|| "instance lost".to_owned())
}

/// What comes out of a call's effects.
#[derive(Default)]
struct Spoken {
    notices: Vec<PluginNotice>,
    writes: Vec<PendingWrite>,
    /// Some effect was invalid: counts against the fuse.
    malformed: bool,
}

/// A call's effects turned into notices and writes: ONE sentence per plugin
/// and batch, within the plugin's quota (the rest is counted, not shown);
/// and one sidecar for each `write-sidecar` whose name is in the manifest
/// and whose `seq` is an event of THIS call with an openable parent.
#[expect(
    clippy::too_many_arguments,
    reason = "the contexts of a guest call; a struct would hide them"
)]
fn speak(
    state: &mut State,
    id: &str,
    effects: Vec<hook_iface::Effect>,
    now: Instant,
    events: &[hook_iface::Event],
    sidecar_names: &[String],
    protected: &[norte_proto::VPath],
    ceiling: &LocationMint,
) -> Spoken {
    let mut out = Spoken::default();
    let mut dropped_effects = 0u32;
    for eff in effects {
        match eff {
            hook_iface::Effect::Notify(text) => {
                let bucket = state
                    .buckets
                    .entry(id.to_owned())
                    .or_insert_with(|| Bucket::new(now));
                if !out.notices.is_empty() || !bucket.take(now) {
                    dropped_effects += 1;
                    continue;
                }
                out.notices.push(PluginNotice {
                    plugin_id: id.to_owned(),
                    kind: KIND_NOTIFY.to_owned(),
                    text: Some(guest_reason(&text)),
                });
            }
            hook_iface::Effect::WriteSidecar(sc) => {
                match sidecar_target(&sc, events, sidecar_names, protected, ceiling) {
                    Ok((parent, path)) => out.writes.push(PendingWrite {
                        plugin_id: id.to_owned(),
                        parent,
                        path,
                        content: sc.content,
                        on_exists: match sc.if_exists {
                            hook_iface::OnExists::Refuse => crate::ops::OnExists::Refuse,
                            hook_iface::OnExists::Replace => crate::ops::OnExists::Replace,
                        },
                    }),
                    // Guest's fault: it counts. Environment's: it is
                    // dropped and said in the log.
                    Err(SidecarFault::Guest) => out.malformed = true,
                    Err(SidecarFault::Environment) => {
                        tracing::debug!(plugin = %id, "sidecar: nowhere to write it");
                    }
                }
            }
        }
    }
    if dropped_effects > 0 {
        tracing::debug!(plugin = %id, dropped_effects, "hook: notices out of quota");
    }
    out
}

/// Why a sidecar has nowhere to go: the guest's fault — counts against the
/// fuse — or the environment's — does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarFault {
    /// Name outside the manifest, or `seq` that is not from this call.
    Guest,
    /// An event with no writable parent: remote, protected, home, the root.
    Environment,
}

/// Where a sidecar goes: `(parent, parent/name)`. The name is validated
/// again here even though the manifest already did: it is the only place
/// between the guest and the disk.
fn sidecar_target(
    sc: &hook_iface::Sidecar,
    events: &[hook_iface::Event],
    sidecar_names: &[String],
    protected: &[norte_proto::VPath],
    ceiling: &LocationMint,
) -> Result<(norte_proto::VPath, norte_proto::VPath), SidecarFault> {
    let Some(name) = sidecar_names
        .iter()
        .find(|n| n.as_bytes() == sc.name.as_slice())
    else {
        return Err(SidecarFault::Guest);
    };
    if !norte_plugin_host::is_valid_sidecar_name(name) {
        return Err(SidecarFault::Guest);
    }
    let ev = events
        .iter()
        .find(|e| e.seq == sc.seq)
        .ok_or(SidecarFault::Guest)?;
    let vpath = norte_proto::VPath::parse(&ev.path).map_err(|_| SidecarFault::Environment)?;
    let parent = vpath.parent().ok_or(SidecarFault::Environment)?;
    if parent.scheme() != "file"
        || parent.authority().is_some()
        || protected
            .iter()
            .any(|root| crate::policy::is_under(root, &parent))
        || ceiling.is_ceiling(&parent)
    {
        return Err(SidecarFault::Environment);
    }
    let segment = norte_proto::Segment::new(sc.name.clone()).map_err(|_| SidecarFault::Guest)?;
    let path = parent.join(segment);
    Ok((parent, path))
}

/// The two notice classes, the SAME strings the proto declares in
/// `PLUGIN_NOTICE_KINDS`; a test pins it.
const KIND_NOTIFY: &str = "notify";
const KIND_HOOKS_DISABLED: &str = "hooks-disabled";
const KIND_EFFECT_DENIED: &str = "effect-denied";

fn disabled_notice(id: &str) -> PluginNotice {
    tracing::warn!(
        plugin = %id,
        failures = HOOK_FUSE_FAILURES,
        "hooks turned off: disabling and re-enabling the plugin rearms them"
    );
    PluginNotice {
        plugin_id: id.to_owned(),
        kind: KIND_HOOKS_DISABLED.to_owned(),
        text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, op: &str, path: &str) -> HookEvent {
        HookEvent {
            seq,
            ts_ms: 1_726_000_000_000,
            op: op.to_owned(),
            actor_kind: "user".to_owned(),
            path: path.as_bytes().to_vec(),
            path_to: None,
            batch_id: Some(7),
        }
    }

    #[test]
    fn the_classes_emitted_are_the_ones_the_proto_declares() {
        use norte_proto::methods::PLUGIN_NOTICE_KINDS;
        assert!(PLUGIN_NOTICE_KINDS.contains(&KIND_NOTIFY));
        assert!(PLUGIN_NOTICE_KINDS.contains(&KIND_HOOKS_DISABLED));
        assert!(PLUGIN_NOTICE_KINDS.contains(&KIND_EFFECT_DENIED));
        assert_eq!(
            PLUGIN_NOTICE_KINDS.len(),
            3,
            "a new class arrives with its emitter"
        );
    }

    #[test]
    fn the_fuse_turns_off_at_the_third_failure_in_a_row_and_warns_once() {
        let mut f = Fuse::default();
        assert!(!f.record_failure("a"));
        assert!(!f.record_failure("a"));
        f.record_ok("a");
        assert!(!f.record_failure("a"), "success reset the counter to zero");
        assert!(!f.record_failure("a"));
        assert!(f.record_failure("a"), "the third in a row turns off");
        assert!(f.is_disabled("a"));
        assert!(!f.record_failure("a"), "disabled does not warn again");
        assert!(!f.is_disabled("b"), "each plugin carries its own fuse");
        // Disabling the plugin (it stops being present) rearms it; on
        // returning, it starts at zero.
        f.rearm_missing(&HashSet::from(["b"]));
        assert!(!f.is_disabled("a"));
        assert!(!f.record_failure("a"), "counter at zero after rearming");
    }

    #[test]
    fn the_notice_quota_is_a_burst_and_one_per_second() {
        let t0 = Instant::now();
        let mut b = Bucket::new(t0);
        for _ in 0..HOOK_NOTICE_BURST {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0), "the burst ran out");
        assert!(
            b.take(t0 + std::time::Duration::from_secs(1)),
            "one second, one more"
        );
        assert!(!b.take(t0 + std::time::Duration::from_millis(1100)));
    }

    #[test]
    fn the_event_name_comes_from_the_journal_op() {
        assert_eq!(event_name_for("created"), Some("after-created"));
        assert_eq!(event_name_for("mode_changed"), Some("after-mode-changed"));
        assert_eq!(event_name_for("teleported"), None);
        for e in HOOK_EVENTS {
            assert!(e.starts_with("after-"), "{e}: only after-* exist");
        }
    }

    #[test]
    fn the_userinfo_does_not_travel_to_the_guest() {
        assert_eq!(without_userinfo("sftp://ana@host/a/b"), "sftp://host/a/b");
        assert_eq!(without_userinfo("sftp://ana@host"), "sftp://host");
        assert_eq!(without_userinfo("file:///a/b"), "file:///a/b");
        assert_eq!(without_userinfo("ftp://host/x"), "ftp://host/x");
    }

    #[test]
    fn events_are_filtered_by_what_the_manifest_asks_for() {
        let batch = vec![
            ev(1, "created", "file:///a/b.txt"),
            ev(2, "renamed", "file:///a/c.txt"),
            ev(3, "vanished", "file:///a/d.txt"),
        ];
        let (out, sessions) = to_wire_events(&batch, &["after-renamed".to_owned()], 0, &[], None);
        assert!(sessions.is_empty(), "with no location nothing is minted");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 2);
        assert_eq!(out[0].path, "file:///a/c.txt");
        assert_eq!(out[0].name, b"c.txt".to_vec());
        assert_eq!(out[0].batch, Some(7));
        assert!(out[0].location.is_none());
        assert!(matches!(out[0].op, hook_iface::Op::Renamed));
    }

    #[test]
    fn what_predates_approval_and_what_is_protected_is_not_delivered() {
        let mut old = ev(1, "created", "file:///a/old.txt");
        old.ts_ms = 1;
        let batch = vec![
            old,
            ev(2, "created", "file:///cfg/norte/journal.db"),
            ev(3, "created", "file:///a/new.txt"),
        ];
        let protected_path = norte_proto::VPath::parse("file:///cfg/norte").expect("vpath");
        let (out, _) = to_wire_events(
            &batch,
            &["after-created".to_owned()],
            1_000,
            std::slice::from_ref(&protected_path),
            None,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 3);
    }

    #[test]
    fn one_location_session_per_parent_directory_and_none_at_the_ceiling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let root = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
        let a = format!("{}/a.txt", root.to_wire());
        let b = format!("{}/b.txt", root.to_wire());
        let c = format!("{}/sub/c.txt", root.to_wire());
        let at_home = format!("{}/x.txt", root.to_wire());
        let batch = vec![
            ev(1, "created", &a),
            ev(2, "created", &b),
            ev(3, "created", &c),
            ev(4, "created", "file:///top.txt"),
        ];
        // With home at `sub`: a/b's parent opens; c's is home and top.txt's
        // is the system root — both ceilings.
        let mint = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(sub.clone()),
        );
        let (out, sessions) =
            to_wire_events(&batch, &["after-created".to_owned()], 0, &[], Some(&mint));
        assert_eq!(out.len(), 4, "the event travels even with no token");
        assert_eq!(sessions.len(), 1, "a single open parent");
        let t = |i: usize| out[i].location.as_ref().map(|l| l.token.clone());
        assert_eq!(t(0), t(1), "the same parent shares a token");
        assert!(t(0).is_some());
        assert!(t(2).is_none(), "home does not open");
        assert!(t(3).is_none(), "neither does the system root");
        // And with home at the test's directory, a.txt does not either.
        let mint = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(dir.path().to_path_buf()),
        );
        let (out, sessions) = to_wire_events(
            &[ev(1, "created", &at_home)],
            &["after-created".to_owned()],
            0,
            &[],
            Some(&mint),
        );
        assert_eq!(out.len(), 1);
        assert!(sessions.is_empty());
    }

    #[test]
    fn a_sidecar_goes_next_to_its_event_and_only_with_a_manifest_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
        let at = |name: &str| format!("{}/{name}", root.to_wire());
        let ev = |seq: u64, path: String| hook_iface::Event {
            seq,
            ts_ms: 0,
            op: hook_iface::Op::Renamed,
            actor: hook_iface::ActorKind::User,
            path,
            path_to: None,
            name: b"x.txt".to_vec(),
            batch: None,
            location: None,
        };
        let events = vec![ev(9, at("x.txt")), ev(10, "file:///top.txt".to_owned())];
        let names = vec![".norte-renames.log".to_owned()];
        let sc = |seq: u64, name: &str| hook_iface::Sidecar {
            seq,
            name: name.as_bytes().to_vec(),
            content: b"x".to_vec(),
            if_exists: hook_iface::OnExists::Replace,
        };
        let mint = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(dir.path().join("elsewhere")),
        );
        let ok = sidecar_target(&sc(9, ".norte-renames.log"), &events, &names, &[], &mint)
            .expect("valid");
        assert_eq!(ok.0.to_wire(), root.to_wire());
        assert_eq!(ok.1.to_wire(), at(".norte-renames.log"));
        // Guest's fault: name outside the manifest, foreign `seq`.
        assert_eq!(
            sidecar_target(&sc(9, "other.log"), &events, &names, &[], &mint),
            Err(SidecarFault::Guest)
        );
        assert_eq!(
            sidecar_target(&sc(8, ".norte-renames.log"), &events, &names, &[], &mint),
            Err(SidecarFault::Guest)
        );
        // Environment's: the system root is a ceiling; a protected root is
        // not written to; neither is home.
        assert_eq!(
            sidecar_target(&sc(10, ".norte-renames.log"), &events, &names, &[], &mint),
            Err(SidecarFault::Environment)
        );
        assert_eq!(
            sidecar_target(
                &sc(9, ".norte-renames.log"),
                &events,
                &names,
                std::slice::from_ref(&root),
                &mint
            ),
            Err(SidecarFault::Environment)
        );
        let home = LocationMint::with_protected_and_home(
            vec![],
            norte_vfs_local::Bounds::default(),
            Some(dir.path().to_path_buf()),
        );
        assert_eq!(
            sidecar_target(&sc(9, ".norte-renames.log"), &events, &names, &[], &home),
            Err(SidecarFault::Environment)
        );
    }

    /// What a plugin writes does not come back as any hook's event: without
    /// this, a hook on `after-created` that wrote a sidecar would call
    /// itself.
    #[test]
    fn a_plugins_rows_are_not_events() {
        let mut own = ev(1, "created", "file:///a/.norte-renames.log");
        own.actor_kind = "plugin".to_owned();
        let batch = vec![own, ev(2, "created", "file:///a/b.txt")];
        let (out, _) = to_wire_events(&batch, &["after-created".to_owned()], 0, &[], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 2);
    }

    struct Nobody;
    impl HookNoticeSink for Nobody {
        fn notice(&self, _n: PluginNotice) {}
    }

    /// Hard rule 3: the dispatcher is a long task, and cancelling ends it
    /// even if an event never arrives.
    #[tokio::test]
    async fn cancel_ends_the_dispatcher() {
        let cfg = tempfile::tempdir().expect("tempdir");
        let cancel = CancellationToken::new();
        let (tx, task) = spawn_dispatcher(
            cfg.path().to_path_buf(),
            Arc::new(PluginRuntime::new().expect("runtime")),
            Arc::new(Nobody),
            cancel.clone(),
            None,
        );
        tx.offer(ev(1, "created", "file:///a"));
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("ends on cancel")
            .expect("no panic");
        // The end is still harmless with the dispatcher dead.
        tx.offer(ev(2, "created", "file:///b"));
    }

    /// And a sink that no longer has anyone behind it ends the dispatcher
    /// on the next batch: it is the embedded one's lifetime, tied to its
    /// frontend.
    #[tokio::test]
    async fn a_closed_sink_ends_the_dispatcher() {
        struct Closed;
        impl HookNoticeSink for Closed {
            fn notice(&self, _n: PluginNotice) {}
            fn is_closed(&self) -> bool {
                true
            }
        }
        let cfg = tempfile::tempdir().expect("tempdir");
        let (tx, task) = spawn_dispatcher(
            cfg.path().to_path_buf(),
            Arc::new(PluginRuntime::new().expect("runtime")),
            Arc::new(Closed),
            CancellationToken::new(),
            None,
        );
        tx.offer(ev(1, "created", "file:///a"));
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("ends on seeing the sink closed")
            .expect("no panic");
    }
}
