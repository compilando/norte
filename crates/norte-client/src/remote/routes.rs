//! The routing of a feed's BATCHES: who receives what, and what happens when
//! the receiver does not drain.
//!
//! An `fs.search`, an `fs.compare` and a `sync.plan` deliver their result in
//! batches by notification, and notifications arrive over a single
//! connection. Here is the route table — task id → channel — with the three
//! things that keep it from losing anything: batches arriving BEFORE their
//! route exists are held (bounded), a route's removal waits a grace period
//! in case the final batch comes in behind the terminal progress, and a
//! consumer that does not drain cuts itself off, not the connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use super::Inner;

/// Buffer of a remote feed's batch channel (`search.hits` from an
/// `fs.search`, `compare.rows` from an `fs.compare`). Absorbs the burst of
/// batches the daemon already coalesced (`SEARCH_HITS_MAX_BATCH` /
/// `COMPARE_ROWS_MAX_BATCH` per batch) while the frontend drains; generous
/// so `try_send` does not discard from backpressure in the normal case.
pub(super) const BATCH_BUF: usize = 64;

/// Cap on batches held with NO route (a startup race: a batch can get ahead
/// of the route's registration). Bounds the memory against a daemon
/// emitting batches for `task_id`s this process never registered.
///
/// PER FEED, not global: each `BatchRoutes` carries its own `pending`, and
/// there are two, so what is held in the worst case is double this number.
pub(super) const BATCH_PENDING_CAP: usize = 64;

/// Grace period after a feed's terminal before removing its route. In the
/// daemon, the batch pump and the progress one are INDEPENDENT tasks
/// writing to the same sink: a `search.hits`/`compare.rows` can arrive after
/// the terminal `task.progress`. The grace period lets those late batches
/// still be routed; once it passes, the sender is dropped and `rx` closes
/// (paired with the embedded arm). Removing it cold at the terminal would
/// lose the late batch.
pub(super) const BATCH_ROUTE_GRACE: Duration = Duration::from_millis(500);

/// Routing of ONE live feed's batches (the `search.hits` of an `fs.search`,
/// the `compare.rows` of an `fs.compare`) by `task_id`. Everything behind
/// ONE Mutex so registering the route (draining what is pending + inserting)
/// is ATOMIC against the pump — no window where a batch is lost between the
/// drain and the insert.
///
/// Generic over the batch type and not duplicated per feed: both have the
/// same lifecycle (route, startup race, grace after the terminal) and two
/// copies of the same subtle reasoning drift apart.
pub(super) struct BatchRoutes<T> {
    /// `task_id` → sender of the `rx` the method that launched it returned.
    pub(super) routes: HashMap<u64, mpsc::Sender<T>>,
    /// Batches that arrived BEFORE their route was registered (a startup
    /// race): registration drains them in order. Bounded to
    /// [`BATCH_PENDING_CAP`] batches in total.
    pub(super) pending: HashMap<u64, Vec<T>>,
    /// `task_id`s whose TERMINAL `task.progress` has already been seen. The
    /// terminal can get AHEAD of the route's registration (the frame leaves
    /// the daemon before `fs.search`'s response, and on the client the pump
    /// and `search` run in parallel): without this, the route's removal
    /// would be missed and `rx` would never close. Mirror of `own_task`'s
    /// `finished` ring. Bounded; an entry is cleaned up when its route is
    /// removed.
    pub(super) terminated: std::collections::HashSet<u64>,
}

// TODO(translation): review — this doc comment appears to have been split
// around `enum OnFull` below: it reads as one sentence continuing after the
// enum ("...estos lotes son un feed de" here, "UI, no dato autoritativo)."
// on `route_batch`'s doc further down). Translated in place, split intact,
// not restructured.
/// Routes ONE batch of a live feed (`search.hits`, `compare.rows`) to its
/// Task by `task_id`. If the route exists, sends; `Closed` (the frontend
/// dropped its `rx`) removes the route; `Full` discards the batch with a
/// warning (backpressure: the frontend is falling behind — these batches are
/// a feed of
/// What to do with a batch that does not fit in the consumer's buffer.
///
/// The difference is not stylistic: it depends on what the batches are for.
#[derive(Clone, Copy)]
pub(super) enum OnFull {
    /// Discard the batch and continue. A search's hits and a comparison's
    /// rows are PAINT: losing a batch impoverishes a list nobody is going to
    /// use for writing, and closing the whole feed would punish more than it
    /// protects.
    DropBatch,
    /// Close the feed. A sync plan's steps are NOT paint: they are the
    /// operations `plan_hash` is going to execute, and among them are
    /// `DeleteTree` and `Overwrite`. A batch silently dropped with the
    /// closing delivered behind it would leave a human approving a hash that
    /// covers steps they never saw — which is exactly what this design
    /// exists to prevent. Closing the feed means `sync.plan_done` never
    /// arrives, and without it there is no hash to approve anything with: it
    /// fails on the safe side.
    ///
    /// (The EMBEDDED arm does not have this problem: it uses `send().await`,
    /// i.e. real backpressure, and loses no step.)
    CloseFeed,
}

/// UI, not authoritative data). With no route yet (a startup race), it holds
/// it in bounded `pending` for the method that launched it to drain on
/// registering; an unknown `task_id` with `pending` full = discarded with a
/// trace (a daemon should not emit batches for Tasks we did not launch).
///
/// `feed` is only the trace label. `on_full` decides what happens when the
/// consumer does not drain, which is where the feeds STOP looking alike.
pub(super) fn route_batch<T>(
    routes: &Mutex<BatchRoutes<T>>,
    id: u64,
    batch: T,
    feed: &'static str,
    on_full: OnFull,
) {
    let mut sr = routes.lock().expect("batch routes lock is sound");
    if let Some(tx) = sr.routes.get(&id) {
        match tx.try_send(batch) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The frontend dropped its Receiver: the route no longer serves.
                sr.routes.remove(&id);
            }
            Err(mpsc::error::TrySendError::Full(_)) => match on_full {
                OnFull::DropBatch => tracing::warn!(
                    task_id = id,
                    feed,
                    "client buffer full, batch discarded (backpressure)"
                ),
                OnFull::CloseFeed => {
                    tracing::warn!(
                        task_id = id,
                        feed,
                        "client buffer full: CLOSING the feed instead of \
                         discarding the batch"
                    );
                    // Dropping the sender closes the frontend's `rx`.
                    // Whatever comes after — including `sync.plan_done` — is
                    // no longer delivered, so the client is left with no
                    // `plan_hash` and cannot approve a plan it saw
                    // incomplete.
                    sr.routes.remove(&id);
                    sr.pending.remove(&id);
                }
            },
        }
    } else if sr.pending_len() < BATCH_PENDING_CAP {
        sr.pending.entry(id).or_default().push(batch);
    } else {
        tracing::debug!(
            task_id = id,
            feed,
            "batch with no route and pending full: discarded"
        );
    }
}

/// Registers a freshly launched feed's route and returns its `rx`.
///
/// ANTI-RACE order: the route is registered BEFORE more batches can arrive.
/// The pump (another task) may have already routed batches that got ahead
/// of the method's response — the frame can leave the daemon before
/// `fs.search`/`fs.compare`'s response, and on the client the pump and the
/// call run in parallel: those batches stayed in `pending`. Registration
/// (draining `pending` + inserting the route) is ATOMIC under the lock, so
/// not a single batch is lost between the two steps. Same pattern
/// `own_task` uses to close the race of an early terminal via the
/// `finished` ring.
///
/// If the TERMINAL got ahead of registration, `route` could not schedule the
/// removal (there was no route yet): it is scheduled here.
pub(super) fn register_route<T: Send + 'static>(
    inner: &Arc<Inner>,
    id: u64,
    feed: &'static str,
    sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel::<T>(BATCH_BUF);
    let (already_terminal, discarded) = {
        let mut sr = sel(inner).lock().expect("batch routes lock is sound");
        // Drains the batches that got ahead of registration (in order). The
        // buffer is sized to absorb the startup; if it still filled up, a UI
        // batch is lost (honest). The log goes AFTER dropping the guard:
        // this lock is also taken by the pump (a hot path) and must not
        // wait on a `tracing::warn!`.
        let mut discarded = 0usize;
        if let Some(early) = sr.pending.remove(&id) {
            for batch in early {
                if tx.try_send(batch).is_err() {
                    discarded += 1;
                }
            }
        }
        sr.routes.insert(id, tx);
        (sr.terminated.contains(&id), discarded)
    };
    if discarded > 0 {
        tracing::warn!(
            task_id = id,
            discarded,
            feed,
            "startup batches discarded (client buffer full)"
        );
    }
    if already_terminal {
        schedule_route_removal(inner, id, sel);
    }
    rx
}

/// Schedules a terminal feed's route removal after [`BATCH_ROUTE_GRACE`].
/// Holds a [`Weak`] (does not keep `Inner` alive): if the backend already
/// died, there is nothing to clean up. On removing the sender, the
/// frontend's `rx` closes (end of the batch stream).
///
/// `sel` picks the feed's map inside `Inner` — a function pointer, so the
/// grace task captures nothing but the `Weak` and the id.
pub(super) fn schedule_route_removal<T: Send + 'static>(
    inner: &Arc<Inner>,
    id: u64,
    sel: fn(&Inner) -> &Mutex<BatchRoutes<T>>,
) {
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        tokio::time::sleep(BATCH_ROUTE_GRACE).await;
        if let Some(inner) = weak.upgrade() {
            let mut sr = sel(&inner).lock().expect("batch routes lock is sound");
            sr.routes.remove(&id);
            sr.pending.remove(&id);
            sr.terminated.remove(&id);
        }
    });
}
