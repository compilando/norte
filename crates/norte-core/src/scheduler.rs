//! Scheduler de tasks (spec §4, §10): toda operación larga es una Task con
//! `CancellationToken` cooperativo y progreso coalescido. Cola por prioridad
//! (`BinaryHeap`) + `Semaphore` por provider; un panic dentro de una task se
//! supervisa → `Failed{Internal{panic}}` y el proceso sigue (spec §17.7).

use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use norte_proto::{Error, TaskId, TaskKind, TaskProgress, TaskState};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

use crate::progress::ProgressReporter;

/// Prioridad de una task; a igual prioridad, FIFO por orden de entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Priority {
    /// Trabajo de fondo (indexado, hashes).
    Low,
    /// Operaciones normales de usuario.
    #[default]
    Normal,
    /// Interactivo urgente (el usuario está mirando).
    High,
}

/// Contexto que recibe el cuerpo de una task: cancelación cooperativa
/// (chequéala en el inner loop, regla dura 3) y emisor de progreso.
pub struct TaskCtx {
    /// Token de cancelación; el cuerpo debe observarlo con frecuencia.
    pub cancel: CancellationToken,
    /// Emisor de progreso coalescido.
    pub progress: Arc<ProgressReporter>,
    /// Origen de las mutaciones de esta task (default `User`; los agentes lo
    /// fijan vía MCP en M3-4). Lo consume el journal.
    pub actor: crate::journal::Actor,
}

/// El cuerpo de una task: una factoría que recibe su [`TaskCtx`] y devuelve
/// el future a ejecutar. `Ok(())` → `Completed`; `Err(Cancelled)` →
/// `Cancelled`; otro `Err` → `Failed{error}`.
pub type TaskBody = Box<dyn FnOnce(TaskCtx) -> BoxFuture<'static, Result<(), Error>> + Send>;

/// Handle de una task viva: observar progreso, cancelar, esperar el final.
pub struct TaskHandle {
    id: TaskId,
    progress: watch::Receiver<TaskProgress>,
    cancel: CancellationToken,
}

impl TaskHandle {
    /// Id de la task.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Receptor de snapshots de progreso (siempre contiene el último).
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.progress.clone()
    }

    /// Pide cancelación cooperativa (la task decide cuándo parar limpio).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Clona el token de cancelación (para cancelar desde otra task, p. ej.
    /// un manejador de señales).
    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Espera el estado terminal y lo devuelve.
    pub async fn join(mut self) -> TaskState {
        loop {
            let state = self.progress.borrow().state.clone();
            if state.is_terminal() {
                return state;
            }
            if self.progress.changed().await.is_err() {
                // Emisor caído sin terminal: solo posible si el scheduler
                // murió; repórtalo como panic interno.
                return TaskState::Failed {
                    error: Error::Internal { panic: true },
                };
            }
        }
    }
}

struct QueuedJob {
    priority: Priority,
    seq: u64,
    body: TaskBody,
    ctx: TaskCtx,
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
        // Max-heap: prioridad mayor primero; a igualdad, seq menor primero.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct ProviderQueue {
    sem: Arc<Semaphore>,
    heap: Mutex<BinaryHeap<QueuedJob>>,
}

/// Scheduler de tasks del core. Barato de clonar vía `Arc` interno.
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    next_id: AtomicU64,
    /// Concurrencia máxima por provider (spec §4: N pequeño, ≤4).
    per_provider_permits: usize,
    queues: Mutex<HashMap<String, Arc<ProviderQueue>>>,
}

impl Scheduler {
    /// Scheduler con `permits` tasks concurrentes por provider (cap a 4).
    #[must_use]
    pub fn new(permits: usize) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                next_id: AtomicU64::new(1),
                per_provider_permits: permits.clamp(1, 4),
                queues: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Encola una task para `provider_key` (normalmente el scheme) y devuelve
    /// su handle. El cuerpo corre cuando le toque (prioridad + semáforo).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento de un lock interno
    /// (otro hilo panicó con él tomado), que ya sería un bug del core.
    #[tracing::instrument(skip(self, body), fields(provider = provider_key))]
    pub fn submit(
        &self,
        provider_key: &str,
        kind: TaskKind,
        priority: Priority,
        actor: crate::journal::Actor,
        body: TaskBody,
    ) -> TaskHandle {
        let seq = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let id = TaskId::new(seq);
        let (reporter, rx) = ProgressReporter::new(id, kind);
        let reporter = Arc::new(reporter);
        let cancel = CancellationToken::new();

        let ctx = TaskCtx {
            cancel: cancel.clone(),
            progress: Arc::clone(&reporter),
            // El actor real lo fija el llamante (M3-3): `User` en el camino
            // humano, `Agent{session}` en el agéntico. Alimenta el journal.
            actor,
        };
        let queue = self.queue_for(provider_key);
        {
            // Invariante: nadie panica con el lock tomado.
            let mut heap = queue.heap.lock().expect("heap lock sano");
            heap.push(QueuedJob {
                priority,
                seq,
                body,
                ctx,
            });
        }

        // Un runner por submit; CUÁL job corre lo decide el heap (prioridad).
        let runner_queue = Arc::clone(&queue);
        tokio::spawn(async move {
            let _permit = runner_queue
                .sem
                .acquire()
                .await
                .expect("semáforo jamás se cierra");
            let job = {
                let mut heap = runner_queue.heap.lock().expect("heap lock sano");
                heap.pop()
            };
            let Some(job) = job else {
                // Imposible: cada runner corresponde a un push. Defensivo.
                return;
            };
            run_job(job).await;
        });

        TaskHandle {
            id,
            progress: rx,
            cancel,
        }
    }

    fn queue_for(&self, provider_key: &str) -> Arc<ProviderQueue> {
        let mut queues = self.inner.queues.lock().expect("queues lock sano");
        Arc::clone(queues.entry(provider_key.to_owned()).or_insert_with(|| {
            Arc::new(ProviderQueue {
                sem: Arc::new(Semaphore::new(self.inner.per_provider_permits)),
                heap: Mutex::new(BinaryHeap::new()),
            })
        }))
    }
}

/// Ejecuta un job supervisado: panic → `Failed{Internal{panic}}`, jamás tumba
/// el proceso; el estado terminal SIEMPRE se publica.
#[tracing::instrument(skip(job), fields(task_id = %job.ctx.progress.snapshot().task_id))]
async fn run_job(job: QueuedJob) {
    let QueuedJob { body, ctx, .. } = job;
    let progress = Arc::clone(&ctx.progress);
    progress.update(|p| p.state = TaskState::Running);

    let fut = std::panic::AssertUnwindSafe(body(ctx));
    let outcome = fut.catch_unwind().await;

    let final_state = match outcome {
        Ok(Ok(())) => TaskState::Completed,
        Ok(Err(Error::Cancelled)) => TaskState::Cancelled,
        Ok(Err(error)) => TaskState::Failed { error },
        Err(_panic) => {
            tracing::error!("panic capturado en task supervisada");
            TaskState::Failed {
                error: Error::Internal { panic: true },
            }
        }
    };
    progress.update(|p| p.state = final_state);
}
