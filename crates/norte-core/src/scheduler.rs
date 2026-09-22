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
use tracing::Instrument as _;

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

/// La puerta de PAUSA de una task (ADR 0147): cerrada = pausada.
///
/// Cooperativa, como la cancelación: la task solo se para en sus
/// [`TaskCtx::checkpoint`], que están en el bucle por chunk de una copia en
/// streaming y entre entrada y entrada de copiar, mover y borrar. Una copia
/// servidor-a-servidor o por `copy_file_range` no tiene chunks, así que se
/// para al acabar el fichero en curso, no antes.
///
/// Clonable: la comparten el cuerpo, el [`TaskHandle`] y quien la pause
/// desde fuera. Pausar o reanudar dos veces no es un error.
///
/// ```
/// use norte_core::PauseGate;
/// let g = PauseGate::default();
/// assert!(!g.is_paused());
/// g.pause();
/// assert!(g.clone().is_paused(), "los clones comparten la puerta");
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
    /// Cierra la puerta: la task se parará en su próximo punto de control.
    pub fn pause(&self) {
        self.0.send_replace(true);
    }

    /// Abre la puerta: la task pausada sigue.
    pub fn resume(&self) {
        self.0.send_replace(false);
    }

    /// Si está cerrada.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        *self.0.borrow()
    }
}

/// Contexto que recibe el cuerpo de una task: cancelación cooperativa
/// (chequéala en el inner loop, regla dura 3), pausa y emisor de progreso.
pub struct TaskCtx {
    /// Token de cancelación; el cuerpo debe observarlo con frecuencia.
    pub cancel: CancellationToken,
    /// La puerta de pausa; el cuerpo la respeta con [`Self::checkpoint`].
    pub pause: PauseGate,
    /// Emisor de progreso coalescido.
    pub progress: Arc<ProgressReporter>,
    /// Origen de las mutaciones de esta task (default `User`; los agentes lo
    /// fijan vía MCP en M3-4). Lo consume el journal.
    pub actor: crate::journal::Actor,
}

impl TaskCtx {
    /// Punto de control: `Err(Cancelled)` si la task se canceló, y si está
    /// PAUSADA espera aquí —publicando `Paused`, y `Running` al seguir—
    /// hasta que se reanude o se cancele.
    ///
    /// Cancelar una task pausada funciona: la espera escucha también al
    /// token, y vuelve con `Cancelled` para que el cuerpo limpie como en
    /// cualquier otra cancelación.
    ///
    /// # Errors
    /// [`Error::Cancelled`] si se canceló antes o durante la pausa.
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
                // Mientras el cuerpo limpia se ve lo mismo que en cualquier
                // cancelación —en marcha hasta el desenlace—, no «pausada».
                self.progress.update(|p| p.state = TaskState::Running);
                return Err(Error::Cancelled);
            }
            // `wait_for` solo falla si el emisor cae, y el emisor lo tiene
            // este mismo contexto: no puede caer mientras se espera.
            _ = rx.wait_for(|pausada| !*pausada) => {}
        }
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.progress.update(|p| p.state = TaskState::Running);
        Ok(())
    }
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
    pause: PauseGate,
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

    /// La puerta de pausa de la task (ADR 0147).
    #[must_use]
    pub fn pause_gate(&self) -> PauseGate {
        self.pause.clone()
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
    /// `task{task_id, kind, provider}`, hijo de lo que estuviera abierto al
    /// hacer `submit` — la petición `rpc` en el daemon (ADR 0127). Viaja con
    /// el JOB y no con el runner, porque el runner saca del heap el que la
    /// prioridad diga, que no tiene por qué ser el que él empujó.
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

/// Origen NO-cero de la secuencia de ids de task (#278).
///
/// Un relevo del daemon —`daemon.going_away { reconnect: true }`— es un evento
/// PREVISTO: sin esto, el proceso nuevo reparte los mismos ids que el viejo y
/// un frontend con un informe en vuelo acierta por colisión sobre la fila
/// equivocada. `daemon/approvals.rs` ya se sembraba así, con el mismo
/// argumento escrito; las tasks no.
///
/// La diferencia con `approvals.rs` es el TECHO, y es la razón de que esto no
/// sea una copia de aquella función: un `task_id` viaja al renderer dentro de
/// `TaskView`, o sea JSON que lee JavaScript, donde el último entero exacto es
/// 2^53. La semilla en nanosegundos de `approvals` vale allí porque un id de
/// aprobación NO cruza ese puente; aquí pasaría de 2^53 y dos ids distintos
/// colapsarían en el mismo `Number`, que es peor que la colisión que arregla.
///
/// Así que milisegundos truncados a 40 bits: hasta ~1,1e12, lo que deja ~9e15
/// de recorrido por debajo de 2^53 para el `fetch_add`. El truncado da la
/// vuelta cada ~34 años, y la unicidad DENTRO del proceso la sigue dando el
/// contador, no el reloj. Separación best-effort, como la de `approvals`: dos
/// arranques en el mismo milisegundo colisionan, y eso no pasa.
fn semilla_de_ids() -> u64 {
    /// 40 bits: ~1,1e12 ms, tres órdenes de magnitud por debajo de 2^53.
    const MASCARA: u64 = (1 << 40) - 1;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    // El 1 de siempre como suelo: un reloj a cero no debe devolver la
    // secuencia a un origen que ya se repartió.
    (millis & MASCARA).max(1)
}

impl Scheduler {
    /// Scheduler con `permits` tasks concurrentes por provider (cap a 4).
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

    /// Encola una task para `provider_key` y devuelve su handle. El cuerpo
    /// corre cuando le toque (prioridad + semáforo).
    ///
    /// `provider_key` es el SCHEME (`file`, `sftp`…), nunca una clave con
    /// autoridad: va tal cual al span `task` del log (ADR 0127), y una
    /// autoridad puede llevar `user:pass@`.
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento de un lock interno
    /// (otro hilo panicó con él tomado), que ya sería un bug del core.
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
        // Ids y tipos, nunca rutas ni parámetros (ADR 0127). `provider` es el
        // SCHEME que pasan todos los llamadores, no `Engine::provider_key`
        // (que lleva la autoridad, y con ella un posible `user:pass@`).
        // `actor` es lo que más correlaciona el trabajo de un agente: su
        // sesión es un id, como el de la tarea.
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
                span,
            });
        }

        // Un runner por submit; CUÁL job corre lo decide el heap (prioridad).
        //
        // RAÍZ (ADR 0127): el runner NO debe heredar el span de este
        // `submit`, porque no corre necesariamente el job que este `submit`
        // empujó. Cada job trae su propio span y se instrumenta con él abajo.
        let runner_queue = Arc::clone(&queue);
        crate::blocking::spawn_raiz(async move {
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
            // `tokio::spawn` no hereda el span de nadie: se lo pone el JOB.
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
async fn run_job(job: QueuedJob) {
    let QueuedJob { body, ctx, .. } = job;
    let progress = Arc::clone(&ctx.progress);
    progress.update(|p| p.state = TaskState::Running);
    // Una task pausada ANTES de empezar no empieza: espera aquí, y un cuerpo
    // sin puntos de control propios también respeta la pausa al arrancar.
    // Si la cancelan mientras espera, el cuerpo corre IGUAL: es él quien
    // sabe limpiar y contar lo que no hizo (un informe marcado incompleto),
    // y lo hace al ver el token, como con cualquier otra cancelación.
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
            tracing::error!("panic capturado en task supervisada");
            TaskState::Failed {
                error: Error::Internal { panic: true },
            }
        }
    };
    progress.update(|p| p.state = final_state);
}
