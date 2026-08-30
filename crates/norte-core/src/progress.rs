//! Progreso de tasks: snapshots [`TaskProgress`] publicados por un canal
//! `watch`, coalescidos a ≤30 Hz (spec §4). El estado terminal SIEMPRE se
//! publica — un frontend jamás se queda esperando un final que no llega.

use std::sync::Mutex;
use std::time::Duration;

use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
use tokio::sync::watch;
use tokio::time::Instant;

/// Intervalo mínimo entre publicaciones no-terminales (~30 Hz).
pub(crate) const COALESCE_INTERVAL: Duration = Duration::from_millis(33);

/// Emisor de progreso de UNA task. Coalescido: las actualizaciones mutan el
/// snapshot interno, pero solo se publican si pasó el intervalo de coalescido
/// (~33 ms) desde la última publicación o si el estado cambió/es terminal.
pub struct ProgressReporter {
    tx: watch::Sender<TaskProgress>,
    inner: Mutex<ReporterState>,
}

struct ReporterState {
    current: TaskProgress,
    last_published: Option<Instant>,
}

impl ProgressReporter {
    /// Crea el reporter y el receptor asociado, con snapshot inicial `Pending`.
    pub(crate) fn new(task_id: TaskId, kind: TaskKind) -> (Self, watch::Receiver<TaskProgress>) {
        let initial = TaskProgress {
            task_id,
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
        let (tx, rx) = watch::channel(initial.clone());
        (
            Self {
                tx,
                inner: Mutex::new(ReporterState {
                    current: initial,
                    last_published: None,
                }),
            },
            rx,
        )
    }

    /// Muta el snapshot y publica si toca (coalescido). El cambio de estado
    /// (incluido cualquier terminal) publica SIEMPRE.
    ///
    /// # Panics
    /// Nunca en la práctica: solo si otro hilo panicó con el lock interno
    /// tomado (envenenamiento), lo que ya sería un bug del core.
    pub fn update(&self, f: impl FnOnce(&mut TaskProgress)) {
        // Invariante: nadie panica con el lock tomado.
        let mut st = self.inner.lock().expect("progress lock sano");
        let before_state = st.current.state.clone();
        f(&mut st.current);
        let state_changed = st.current.state != before_state;
        let now = Instant::now();
        let due = st
            .last_published
            .is_none_or(|last| now.duration_since(last) >= COALESCE_INTERVAL);
        if state_changed || st.current.state.is_terminal() || due {
            st.last_published = Some(now);
            let _ = self.tx.send(st.current.clone());
        }
    }

    /// Snapshot actual (aunque no esté publicado).
    ///
    /// # Panics
    /// Nunca en la práctica (ver [`Self::update`]).
    #[must_use]
    pub fn snapshot(&self) -> TaskProgress {
        self.inner
            .lock()
            .expect("progress lock sano")
            .current
            .clone()
    }
}
