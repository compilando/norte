//! Approval router for the daemon (M3-3b Task 4): resolves a policy `Ask`
//! with a human round-trip over the wire. The engine's gate calls
//! [`ApprovalResolver::request`]; this router registers the pending entry,
//! broadcasts `policy.approval_required` to subscribed frontends and
//! suspends the call until the matching `policy.decide` (or the TTL, which
//! denies).
//!
//! Construction order (the plan's "chicken-egg"): the resolver is created
//! BEFORE the engine and the daemon; the engine receives it in
//! [`Engine::with_policy`](crate::Engine::with_policy) and the daemon in
//! [`Daemon::bind_with_policy`](super::Daemon::bind_with_policy), which
//! injects the broadcaster (the outlet toward subscribers) into it. With no
//! broadcaster installed (engine without a daemon), an `Ask` is denied
//! IMMEDIATELY, fail-closed: nobody could hear the question, and letting the
//! TTL hang would only delay the inevitable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use norte_proto::methods::{PendingApproval, PolicyApprovalRequired};
use tokio::sync::oneshot;

use crate::approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver};
use crate::journal::Actor;

/// Default TTL of a pending approval: once it expires, the `Ask` is denied
/// (`not-approved`). An absent human does not leave the operation hanging
/// forever.
pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_mins(1);

/// Simultaneous pending approvals tolerated. They are already structurally
/// bounded (each pending entry suspends the SERIAL dispatch of one
/// connection, and connections have their own cap), but the belt is cheap:
/// above this, a new `Ask` is denied fail-closed instead of growing without
/// limit.
const MAX_PENDING_APPROVALS: usize = 256;

/// Outlet from the router toward the frontends: encapsulates the daemon's
/// encode + broadcast without this module knowing its `Shared` (installed by
/// `bind_with_policy` with a closure that captures a `Weak`).
type Broadcaster = Box<dyn Fn(PolicyApprovalRequired) + Send + Sync>;

/// What happened when trying to decide an approval (#279).
///
/// The three failure modes read differently to whoever is looking at the
/// screen: one says "try again", another "you were too late" and the third
/// "that approval does not belong to this daemon". Collapsing them into a
/// boolean forced the frontend to pick one phrasing and be right a third of
/// the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Decided: the suspended gate woke up with the answer.
    Applied,
    /// It was pending, but the requester is no longer listening — its TTL
    /// expired or its dispatch was cancelled. The decision had no effect.
    Expired,
    /// That id existed and is no longer pending: someone resolved it earlier
    /// — another window, its own TTL sweeping it, or the requester WITHDRAWING
    /// it with an `rpc.cancel`.
    ///
    /// The three are counted together because the advice to whoever clicked
    /// is the same — that decision is no longer theirs, refresh the list —
    /// and because telling them apart would mean remembering why each id
    /// left, which is memory spent on a nuance nobody uses.
    YaDecided,
    /// That id was never issued in this process. A stale modal from before a
    /// daemon restart lands here.
    Unknown,
}

/// An approval in flight: metadata for `policy.pending` (resync) and the
/// channel through which `policy.decide` wakes up the suspended gate.
struct PendingEntry {
    session: Option<String>,
    op: String,
    paths: Vec<String>,
    /// How many paths the decision covers (`paths` can be a prefix). Kept so
    /// the RESYNC of `policy.pending` says the same thing the notification
    /// said: a frontend that reconnects must not see a trimmed list and
    /// believe it is the whole thing.
    paths_total: u64,
    /// What the op ADDS to the question (#314): today, the mode of a
    /// `set-mode`. Kept for the same reason as `paths_total` — the resync
    /// has to say the same thing the notification said.
    detail: norte_proto::methods::ApprovalDetail,
    decide: oneshot::Sender<bool>,
}

struct Inner {
    pending: Mutex<HashMap<u64, PendingEntry>>,
    next_id: AtomicU64,
    /// The FIRST id this process could have issued (#279).
    ///
    /// Needed because the sequence does not start at zero: it is seeded from
    /// the clock so that a stale modal from before a restart cannot match by
    /// collision. Without this bound, "existed" would be decided solely by
    /// `id < next_id`, and ANY small made-up number would pass as "already
    /// decided", which is exactly the wrong explanation for an id that was
    /// never issued at all.
    first_id: u64,
    broadcaster: Mutex<Option<Broadcaster>>,
}

/// The daemon's `Ask` resolver (M3-3b). Shared via `Arc` between the engine
/// (which calls it from the gate) and the daemon's `Shared` (which routes
/// `policy.decide`/`policy.pending` to it).
pub struct DaemonApprovalResolver {
    inner: Arc<Inner>,
    ttl: Duration,
}

impl Default for DaemonApprovalResolver {
    fn default() -> Self {
        Self::new(DEFAULT_APPROVAL_TTL)
    }
}

impl DaemonApprovalResolver {
    /// With an explicit per-approval TTL (tests use a short one).
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        // NON-zero start (security MINOR-1): after a daemon restart, a
        // frontend reconnecting with a stale modal must not match by
        // collision with a NEW pending entry (both sequences would otherwise
        // start at 0). The clock is enough as a best-effort separator;
        // intra-process uniqueness comes from the fetch_add.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| {
                u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(0)
            });
        Self {
            inner: Arc::new(Inner {
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(seed),
                first_id: seed,
                broadcaster: Mutex::new(None),
            }),
            ttl,
        }
    }

    /// Installs the outlet toward the frontends. Called by the daemon when
    /// binding (`bind_with_policy`); overwriting a previous one is a
    /// reasonable no-op (last daemon wins — in practice there is one).
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    pub fn set_broadcaster(&self, b: Broadcaster) {
        *self
            .inner
            .broadcaster
            .lock()
            .expect("broadcaster lock is sound") = Some(b);
    }

    /// Resolves the pending `approval_id` with `approve` and wakes up the
    /// gate.
    ///
    /// A decision CONSUMES the pending entry — a second `decide` on the same
    /// id fails (anti double-decision).
    ///
    /// **Distinguishes the three ways of failing** (#279), because they call
    /// for different answers to whoever is looking at the screen and used to
    /// be collapsed into a single `false` that the frontend could only read
    /// as "your click didn't get through" — true in one of the three cases
    /// and false in the other two.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    #[must_use]
    pub fn decide(&self, approval_id: u64, approve: bool) -> Decision {
        let entry = self
            .inner
            .pending
            .lock()
            .expect("pending approvals lock is sound")
            .remove(&approval_id);
        match entry {
            // `send` fails if the receiver died in the race: the TTL was
            // already consumed or the dispatch was cancelled. The decision
            // had NO effect and the ack must not lie (nor the M3-5 audit
            // log).
            Some(e) => {
                if e.decide.send(approve).is_ok() {
                    Decision::Applied
                } else {
                    Decision::Expired
                }
            }
            // Not pending. The id alone says so, but it takes BOTH bounds:
            // the sequence starts at a clock-derived seed, so "less than
            // next" alone would treat any small made-up number as existing.
            // Within the range this process has issued, it existed and
            // someone already resolved it; outside it, it was never issued
            // here.
            None if (self.inner.first_id..self.inner.next_id.load(Ordering::Relaxed))
                .contains(&approval_id) =>
            {
                Decision::YaDecided
            }
            None => Decision::Unknown,
        }
    }

    /// Snapshot of the pending approvals (resync of `policy.pending` for a
    /// frontend that connects after the broadcast). Stable order by
    /// `approval_id`.
    ///
    /// # Panics
    /// Only if the internal lock is poisoned.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingApproval> {
        let map = self
            .inner
            .pending
            .lock()
            .expect("pending approvals lock is sound");
        let mut out: Vec<PendingApproval> = map
            .iter()
            .map(|(&approval_id, e)| PendingApproval {
                approval_id,
                session: e.session.clone(),
                op: e.op.clone(),
                paths: e.paths.clone(),
                paths_total: e.paths_total,
                detail: e.detail.clone(),
            })
            .collect();
        out.sort_by_key(|p| p.approval_id);
        out
    }
}

/// RAII guard for the pending entry: if the [`request`] future is cancelled
/// (daemon shutdown cuts the dispatch) or the TTL expires, the entry leaves
/// the map when the guard is dropped — never an orphaned pending entry that
/// `policy.pending` would show forever. After a `decide`, the remove is a
/// benign no-op.
///
/// The requesting connection's DEATH DOES cancel this future (#64, solved):
/// reading the socket lives in its own task, and an EOF/reset cancels
/// `peer_gone`, dropping the suspended dispatch → this guard removes the
/// pending entry. (Residual, bounded blind spot: if the peer left more than
/// `INBOX_FRAMES` frames in flight, the reader stays blocked sending to the
/// inbox and does not observe the EOF until the Ask resolves by TTL — norte's
/// clients are request/response, so this does not apply.)
///
/// [`request`]: ApprovalResolver::request
struct PendingGuard {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.inner
            .pending
            .lock()
            .expect("pending approvals lock is sound")
            .remove(&self.id);
    }
}

#[async_trait]
impl ApprovalResolver for DaemonApprovalResolver {
    /// Suspends until `policy.decide` or TTL. The suspension holds ONLY the
    /// (serial) dispatch of the connection that requested the op — not the
    /// scheduler's worker pool nor other connections.
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
        // The display session comes from the ACTOR fixed server-side at the
        // handshake, never from anything the requester declares here.
        let session = match &req.actor {
            Actor::User => None,
            Actor::Agent { session } => Some(session.clone()),
            // M4: the plugin id travels in `session` as a display
            // identifier — if the plugin model needs to distinguish it on
            // the wire, that will be a new proto field, not overloading
            // this one.
            Actor::Plugin { id } => Some(id.clone()),
        };
        let op = req.op.kind().to_owned();
        // #314: what the op adds to the question. For all but one, nothing:
        // the op and the paths ARE the decision. Not `set-mode`, because two
        // requests with the same paths and different modes mean opposite
        // things.
        let detail = match &req.op {
            // #315: and with the SCOPE, not just the mode. A recursive
            // change on a root used to be asked as "set-mode on 1 path" while
            // what was actually being approved was a hundred thousand nodes:
            // the same hole the mode came to close in 0.61, one size bigger.
            crate::policy::PolicyOp::SetMode {
                mode,
                recursive,
                dir_mode,
            } => norte_proto::methods::ApprovalDetail {
                mode: Some(*mode),
                recursive: *recursive,
                dir_mode: *dir_mode,
            },
            _ => norte_proto::methods::ApprovalDetail::default(),
        };
        let (tx, rx) = oneshot::channel();
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut pending = self
                .inner
                .pending
                .lock()
                .expect("pending approvals lock is sound");
            if pending.len() >= MAX_PENDING_APPROVALS {
                tracing::warn!("pending approvals at capacity; Ask denied fail-closed");
                return ApprovalOutcome::Denied;
            }
            pending.insert(
                id,
                PendingEntry {
                    session: session.clone(),
                    op: op.clone(),
                    paths: req.paths.clone(),
                    paths_total: req.paths_total,
                    detail: detail.clone(),
                    decide: tx,
                },
            );
        }
        let _guard = PendingGuard {
            inner: Arc::clone(&self.inner),
            id,
        };
        // Broadcast AFTER registering: a concurrent `policy.pending` sees it
        // through one of the two paths, never through neither.
        {
            let broadcaster = self
                .inner
                .broadcaster
                .lock()
                .expect("broadcaster lock is sound");
            let Some(broadcast) = broadcaster.as_ref() else {
                tracing::warn!("Ask with no broadcaster installed: denied fail-closed");
                return ApprovalOutcome::Denied;
            };
            broadcast(PolicyApprovalRequired {
                approval_id: id,
                session,
                op,
                paths: req.paths,
                paths_total: req.paths_total,
                ttl_ms: u64::try_from(self.ttl.as_millis()).unwrap_or(u64::MAX),
                detail,
            });
        }
        match tokio::time::timeout(self.ttl, rx).await {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            // The sender only dies without sending if the whole resolver is
            // being torn down: denying is the only honest thing to do.
            Ok(Ok(false) | Err(_)) => ApprovalOutcome::Denied,
            Err(_elapsed) => ApprovalOutcome::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyOp;

    fn ask(resolver: &Arc<DaemonApprovalResolver>) -> tokio::task::JoinHandle<ApprovalOutcome> {
        let r = Arc::clone(resolver);
        tokio::spawn(async move {
            r.request(ApprovalRequest {
                actor: Actor::Agent {
                    session: "s1".into(),
                },
                op: PolicyOp::Copy,
                paths: vec!["mem:///a".into()],
                paths_total: 1,
            })
            .await
        })
    }

    /// Waits (with a cap) until there are exactly `n` pending entries
    /// registered.
    async fn wait_pending(resolver: &DaemonApprovalResolver, n: usize) {
        for _ in 0..200 {
            if resolver.pending().len() == n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("never reached {n} pending entries");
    }

    #[tokio::test]
    async fn no_broadcaster_denies_immediately_fail_closed() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        let out = ask(&r).await.expect("join");
        assert_eq!(out, ApprovalOutcome::Denied);
        assert!(
            r.pending().is_empty(),
            "the guard cleaned up the pending entry"
        );
    }

    #[tokio::test]
    async fn decide_approves_and_consumes_the_pending_entry() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        let id = r.pending()[0].approval_id;
        assert_eq!(r.decide(id, true), Decision::Applied, "it existed");
        assert_eq!(task.await.expect("join"), ApprovalOutcome::Approved);
        // A decision consumes the id — and what a second attempt is told is
        // "someone already decided it", not "it doesn't exist" (#279): with
        // two windows open that is exactly what happened.
        assert_eq!(r.decide(id, true), Decision::YaDecided);
        assert!(r.pending().is_empty());
    }

    /// An id this process never issued is distinguished from one already
    /// decided (#279): the former is a stale modal from before a restart,
    /// and the advice to the user is not the same.
    #[tokio::test]
    async fn an_id_never_issued_is_unknown() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        let id = r.pending()[0].approval_id;
        assert_eq!(r.decide(id.saturating_add(1000), true), Decision::Unknown);
        // And the real one is still pending: asking about another does not
        // touch it.
        assert_eq!(r.decide(id, false), Decision::Applied);
        let _ = task.await;
    }

    #[tokio::test]
    async fn decide_denies() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        let id = r.pending()[0].approval_id;
        assert_eq!(r.decide(id, false), Decision::Applied);
        assert_eq!(task.await.expect("join"), ApprovalOutcome::Denied);
    }

    #[tokio::test]
    async fn expired_ttl_is_timed_out_and_cleans_up() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_millis(50)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        assert_eq!(task.await.expect("join"), ApprovalOutcome::TimedOut);
        assert!(r.pending().is_empty(), "the TTL leaves no orphans");
    }

    #[tokio::test]
    async fn cancelling_the_future_cleans_up_the_pending_entry() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        // The future is cancelled (in the real daemon: shutdown cuts the
        // dispatch — the peer's death does NOT arrive here, see the guard's
        // rustdoc and issue #64). The guard must remove the pending entry
        // from the map.
        task.abort();
        let _ = task.await;
        wait_pending(&r, 0).await;
    }

    #[tokio::test]
    async fn the_notification_carries_what_was_registered() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PolicyApprovalRequired>();
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_millis(50)));
        r.set_broadcaster(Box::new(move |n| {
            let _ = tx.send(n);
        }));
        let task = ask(&r);
        let notif = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the broadcast went out")
            .expect("channel alive");
        assert_eq!(notif.session.as_deref(), Some("s1"));
        assert_eq!(notif.op, "copy");
        assert_eq!(notif.paths, vec!["mem:///a".to_string()]);
        assert_eq!(notif.ttl_ms, 50);
        let _ = task.await;
    }
}
