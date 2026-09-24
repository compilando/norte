//! Task scheduler (spec §4, §10): every long operation is a Task with a
//! cooperative `CancellationToken` and coalesced progress. Priority queue
//! (`BinaryHeap`) + per-provider `Semaphore`; a panic inside a task is
//! supervised → `Failed{Internal{panic}}` and the process keeps going (spec
//! §17.7).

use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use norte_proto::{Error, TaskId, TaskKind, TaskProgress, TaskState};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::progress::ProgressReporter;

/// A task's priority; at equal priority, FIFO by arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    /// Background work (indexing, hashes).
    Low,
    /// Normal user operations.
    #[default]
    Normal,
    /// Urgent interactive (the user is watching).
    High,
}

/// A task's PAUSE gate (ADR 0147): closed = paused.
///
/// Cooperative, like cancellation: the task only stops at its
/// [`TaskCtx::checkpoint`]s, which sit in the per-chunk loop of a streaming
/// copy and between entries when copying, moving and deleting. A
/// server-to-server copy or one done via `copy_file_range` has no chunks, so
/// it stops when the file in progress finishes, not before.
///
/// Clonable: shared by the body, the [`TaskHandle`] and whoever pauses it
/// from outside. Pausing or resuming twice is not an error.
///
/// ```
/// use norte_core::PauseGate;
/// let g = PauseGate::default();
/// assert!(!g.is_paused());
/// g.pause();
/// assert!(g.clone().is_paused(), "clones share the gate");
/// g.resume();
/// assert!(!g.is_paused());
/// ```
#[derive(Clone)]
pub struct PauseGate(Arc<watch::Sender<bool>>);

impl Default for PauseGate {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl PauseGate {
    /// Closes the gate: the task will stop at its next checkpoint.
    pub fn pause(&self) {
        self.0.send_replace(true);
    }

    /// Opens the gate: a paused task continues.
    pub fn resume(&self) {
        self.0.send_replace(false);
    }

    /// Whether it's closed.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        *self.0.borrow()
    }
}

/// How a task enters: in parallel —up to four per scheme— or into the
/// QUEUE, where they go one at a time (ADR 0149).
///
/// The queue is ONE, global, not one per device: it's Total Commander's and
/// Krusader's, and it's the one that can be explained. Four copies to the
/// same mechanical disk are slower than four in sequence, and that's the
/// entire reason it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    /// Up to four per scheme, as always.
    #[default]
    Paralelo,
    /// One at a time, in arrival order.
    Cola,
}

/// The key for the serial queue. Not a scheme, and it can't be one: if it
/// were, a provider by that name would share a slot with it.
const QUEUE: &str = "\u{0}queue";

/// The context a task's body receives: cooperative cancellation (check it in
/// the inner loop, hard rule 3), pause, and the progress emitter.
pub struct TaskCtx {
    /// Cancellation token; the body must observe it frequently.
    pub cancel: CancellationToken,
    /// The pause gate; the body honors it via [`Self::checkpoint`].
    pub pause: PauseGate,
    /// Coalesced progress emitter.
    pub progress: Arc<ProgressReporter>,
    /// The source of this task's mutations (default `User`; agents set it
    /// via MCP in M3-4). Consumed by the journal.
    pub actor: crate::journal::Actor,
}

impl TaskCtx {
    /// Checkpoint: `Err(Cancelled)` if the task was cancelled, and if it is
    /// PAUSED it waits here —publishing `Paused`, and `Running` when it
    /// continues— until it's resumed or cancelled.
    ///
    /// Cancelling a paused task works: the wait also listens to the token,
    /// and returns with `Cancelled` so the body can clean up as with any
    /// other cancellation.
    ///
    /// # Errors
    /// [`Error::Cancelled`] if cancelled before or during the pause.
    pub async fn checkpoint(&self) -> Result<(), Error> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !self.pause.is_paused() {
            return Ok(());
        }
        self.progress.update(|p| p.state = TaskState::Paused);
        let mut rx = self.pause.0.subscribe();
        tokio::select! {
            () = self.cancel.cancelled() => {
                // While the body cleans up it looks the same as any other
                // cancellation —running until the outcome—, not "paused".
                self.progress.update(|p| p.state = TaskState::Running);
                return Err(Error::Cancelled);
            }
            // `wait_for` only fails if the sender drops, and this same
            // context holds the sender: it cannot drop while this waits.
            _ = rx.wait_for(|paused| !*paused) => {}
        }
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.progress.update(|p| p.state = TaskState::Running);
        Ok(())
    }
}

/// A task's body: a factory that receives its [`TaskCtx`] and returns the
/// future to run. `Ok(())` → `Completed`; `Err(Cancelled)` → `Cancelled`;
/// any other `Err` → `Failed{error}`.
pub type TaskBody = Box<dyn FnOnce(TaskCtx) -> BoxFuture<'static, Result<(), Error>> + Send>;

/// Handle to a live task: watch progress, cancel, wait for the end.
pub struct TaskHandle {
    id: TaskId,
    progress: watch::Receiver<TaskProgress>,
    cancel: CancellationToken,
    pause: PauseGate,
}

impl TaskHandle {
    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Receiver for progress snapshots (always holds the latest).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.progress.clone()
    }

    /// Requests cooperative cancellation (the task decides when to stop
    /// cleanly).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Clones the cancellation token (to cancel from another task, e.g. a
    /// signal handler).
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// The task's pause gate (ADR 0147).
    #[must_use]
    pub fn pause_gate(&self) -> PauseGate {
        self.pause.clone()
    }

    /// Waits for the terminal state and returns it.
    pub async fn join(mut self) -> TaskState {
        loop {
            let state = self.progress.borrow().state.clone();
            if state.is_terminal() {
                return state;
            }
            if self.progress.changed().await.is_err() {
                // Sender dropped without a terminal state: only possible if
                // the scheduler died; report it as an internal panic.
                return TaskState::Failed {
                    error: Error::Internal { panic: true },
                };
            }
        }
    }
}

struct QueuedJob {
    id: TaskId,
    priority: Priority,
    seq: u64,
    body: TaskBody,
    ctx: TaskCtx,
    /// `task{task_id, kind, provider}`, a child of whatever was open at
    /// `submit` time — the daemon's `rpc` request (ADR 0127). Travels with
    /// the JOB and not with the runner, because the runner pops whichever
    /// one priority dictates off the heap, which need not be the one it
    /// pushed.
    span: tracing::Span,
}

impl PartialEq for QueuedJob {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl Eq for QueuedJob {}
impl PartialOrd for QueuedJob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueuedJob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap: higher priority first; ties broken by lower seq first.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct ProviderQueue {
    sem: Arc<Semaphore>,
    heap: Mutex<BinaryHeap<QueuedJob>>,
}

/// The core's task scheduler. Cheap to clone via an internal `Arc`.
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    next_id: AtomicU64,
    /// Max concurrency per provider (spec §4: small N, ≤4).
    per_provider_permits: usize,
    queues: Mutex<HashMap<String, Arc<ProviderQueue>>>,
}

/// NON-zero origin for the task id sequence (#278).
///
/// A daemon handover —`daemon.going_away { reconnect: true }`— is an
/// EXPECTED event: without this, the new process hands out the same ids as
/// the old one and a frontend with a report in flight matches, by
/// collision, the wrong row. `daemon/approvals.rs` already seeded itself
/// this way, with the same argument written down; tasks did not.
///
/// The difference from `approvals.rs` is the CEILING, and it's why this
/// isn't a copy of that function: a `task_id` travels to the renderer
/// inside a `TaskView`, i.e. JSON that JavaScript reads, where the largest
/// exact integer is 2^53. The nanosecond seed `approvals` uses is fine
/// there because an approval id never crosses that bridge; here it would
/// exceed 2^53 and two different ids would collapse into the same
/// `Number`, which is worse than the collision it fixes.
///
/// So: milliseconds truncated to 40 bits: up to ~1.1e12, leaving ~9e15 of
/// headroom below 2^53 for the `fetch_add`. The truncation wraps around
/// every ~34 years, and uniqueness WITHIN the process is still given by the
/// counter, not the clock. Best-effort separation, like `approvals`'s: two
/// startups in the same millisecond collide, and that doesn't happen.
fn semilla_de_ids() -> u64 {
    /// 40 bits: ~1.1e12 ms, three orders of magnitude below 2^53.
    const MASCARA: u64 = (1 << 40) - 1;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    // The usual 1 as a floor: a clock at zero must not send the sequence
    // back to an origin that was already handed out.
    (millis & MASCARA).max(1)
}

impl Scheduler {
    /// A scheduler with `permits` concurrent tasks per provider (capped at 4).
    #[must_use]
    pub fn new(permits: usize) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                next_id: AtomicU64::new(semilla_de_ids()),
                per_provider_permits: permits.clamp(1, 4),
                queues: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Enqueues a task for `provider_key` and returns its handle. The body
    /// runs when its turn comes (priority + semaphore).
    ///
    /// `provider_key` is the SCHEME (`file`, `sftp`…), never a key carrying
    /// authority: it goes as-is into the log's `task` span (ADR 0127), and
    /// an authority can carry `user:pass@`.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of an internal lock (another
    /// thread panicked while holding it), which would already be a core bug.
    #[must_use]
    pub fn submit(
        &self,
        provider_key: &str,
        kind: TaskKind,
        priority: Priority,
        actor: crate::journal::Actor,
        body: TaskBody,
    ) -> TaskHandle {
        self.submit_en(Lane::Paralelo, provider_key, kind, priority, actor, body)
    }

    /// Like [`Self::submit`], choosing how it enters (ADR 0149).
    ///
    /// # Panics
    /// Never in practice: only from poisoning of an internal lock.
    pub fn submit_en(
        &self,
        lane: Lane,
        provider_key: &str,
        kind: TaskKind,
        priority: Priority,
        actor: crate::journal::Actor,
        body: TaskBody,
    ) -> TaskHandle {
        let seq = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let id = TaskId::new(seq);
        // Ids and types, never paths or parameters (ADR 0127). `provider`
        // is the SCHEME every caller passes, not `Engine::provider_key`
        // (which carries the authority, and with it a possible
        // `user:pass@`). `actor` is what correlates an agent's work the
        // most: its session is an id, just like the task's.
        let span = tracing::info_span!(
            "task",
            task_id = %id,
            kind = ?kind,
            provider = provider_key,
            actor = ?actor
        );
        let (reporter, rx) = ProgressReporter::new(id, kind);
        let reporter = Arc::new(reporter);
        let cancel = CancellationToken::new();
        let pause = PauseGate::default();

        let ctx = TaskCtx {
            cancel: cancel.clone(),
            pause: pause.clone(),
            progress: Arc::clone(&reporter),
            // The real actor is set by the caller (M3-3): `User` on the
            // human path, `Agent{session}` on the agentic one. Feeds the
            // journal.
            actor,
        };
        let queue = match lane {
            Lane::Paralelo => self.queue_for(provider_key),
            Lane::Cola => self.queue_for(QUEUE),
        };
        {
            // Invariant: nobody panics with the lock held.
            let mut heap = queue.heap.lock().expect("heap lock sound");
            heap.push(QueuedJob {
                id,
                priority,
                seq,
                body,
                ctx,
                span,
            });
        }

        // One runner per submit; WHICH job runs is decided by the heap
        // (priority).
        //
        // ROOT (ADR 0127): the runner must NOT inherit this `submit`'s
        // span, because it doesn't necessarily run the job this `submit`
        // pushed. Each job carries its own span and is instrumented with
        // it below.
        let runner_queue = Arc::clone(&queue);
        crate::blocking::spawn_raiz(async move {
            let _permit = runner_queue
                .sem
                .acquire()
                .await
                .expect("semaphore never closes");
            let job = {
                let mut heap = runner_queue.heap.lock().expect("heap lock sound");
                heap.pop()
            };
            let Some(job) = job else {
                // Impossible: every runner corresponds to a push. Defensive.
                return;
            };
            // `tokio::spawn` inherits nobody's span: the JOB provides its own.
            let span = job.span.clone();
            run_job(job).instrument(span).await;
        });

        TaskHandle {
            id,
            progress: rx,
            cancel,
            pause,
        }
    }

    /// Moves a task that HAS NOT STARTED YET up or down within the serial
    /// queue (ADR 0149). `false` if it was already running, if it's not in
    /// the queue, or if it was already at the end it's being moved toward.
    ///
    /// Reorders by the order KEY, not by its slot in the heap: the `seq` is
    /// swapped with its neighbor, which is what decides who goes out first
    /// at equal priority.
    ///
    /// # Panics
    /// Never in practice: only from poisoning of an internal lock.
    #[must_use]
    pub fn mover_en_cola(&self, id: TaskId, up: bool) -> bool {
        let queue = self.queue_for(QUEUE);
        let mut heap = queue.heap.lock().expect("heap lock sound");
        let mut jobs = std::mem::take(&mut *heap).into_vec();
        // By exit order: priority first, and at equal priority the lowest
        // `seq`.
        jobs.sort_by_key(|j| (std::cmp::Reverse(j.priority), j.seq));
        let Some(pos) = jobs.iter().position(|j| j.id == id) else {
            *heap = jobs.into_iter().collect();
            return false;
        };
        let other = if up {
            pos.checked_sub(1)
        } else {
            (pos + 1 < jobs.len()).then_some(pos + 1)
        };
        let moved = if let Some(other) = other {
            let (a, b) = (jobs[pos].seq, jobs[other].seq);
            jobs[pos].seq = b;
            jobs[other].seq = a;
            true
        } else {
            false
        };
        *heap = jobs.into_iter().collect();
        moved
    }

    fn queue_for(&self, provider_key: &str) -> Arc<ProviderQueue> {
        let mut queues = self.inner.queues.lock().expect("queues lock sound");
        // The serial queue has ONE slot (ADR 0149); everything else gets
        // the usual per-scheme permits.
        let permits = if provider_key == QUEUE {
            1
        } else {
            self.inner.per_provider_permits
        };
        Arc::clone(queues.entry(provider_key.to_owned()).or_insert_with(|| {
            Arc::new(ProviderQueue {
                sem: Arc::new(Semaphore::new(permits)),
                heap: Mutex::new(BinaryHeap::new()),
            })
        }))
    }
}

/// Runs a supervised job: panic → `Failed{Internal{panic}}`, never brings
/// down the process; the terminal state is ALWAYS published.
async fn run_job(job: QueuedJob) {
    let QueuedJob { body, ctx, .. } = job;
    let progress = Arc::clone(&ctx.progress);
    progress.update(|p| p.state = TaskState::Running);
    // A task paused BEFORE it starts does not start: it waits here, and a
    // body with no checkpoints of its own also honors the pause at
    // startup. If it's cancelled while waiting, the body runs ANYWAY: it's
    // the one that knows how to clean up and account for what it didn't do
    // (a report marked incomplete), and it does so on seeing the token,
    // like with any other cancellation.
    if ctx.pause.is_paused() {
        let _ = ctx.checkpoint().await;
    }

    let fut = std::panic::AssertUnwindSafe(body(ctx));
    let outcome = fut.catch_unwind().await;

    let final_state = match outcome {
        Ok(Ok(())) => TaskState::Completed,
        Ok(Err(Error::Cancelled)) => TaskState::Cancelled,
        Ok(Err(error)) => TaskState::Failed { error },
        Err(_panic) => {
            tracing::error!("panic caught in a supervised task");
            TaskState::Failed {
                error: Error::Internal { panic: true },
            }
        }
    };
    progress.update(|p| p.state = final_state);
}
