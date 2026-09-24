//! Task progress: [`TaskProgress`] snapshots published over a `watch`
//! channel, coalesced to ≤30 Hz (spec §4). The terminal state is ALWAYS
//! published — a frontend never gets stuck waiting for an ending that never
//! arrives.

use std::sync::Mutex;
use std::time::Duration;

use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
use tokio::sync::watch;
use tokio::time::Instant;

/// Minimum interval between non-terminal publications (~30 Hz).
pub(crate) const COALESCE_INTERVAL: Duration = Duration::from_millis(33);

/// Progress emitter for ONE task. Coalesced: updates mutate the internal
/// snapshot, but are only published if the coalescing interval (~33 ms) has
/// passed since the last publication, or if the state changed/is terminal.
pub struct ProgressReporter {
    tx: watch::Sender<TaskProgress>,
    inner: Mutex<ReporterState>,
}

struct ReporterState {
    current: TaskProgress,
    last_published: Option<Instant>,
}

impl ProgressReporter {
    /// Creates the reporter and its associated receiver, with an initial
    /// `Pending` snapshot.
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

    /// Mutates the snapshot and publishes if it's due (coalesced). A state
    /// change (including any terminal one) ALWAYS publishes.
    ///
    /// # Panics
    /// Never in practice: only if another thread panicked with the internal
    /// lock held (poisoning), which would already be a core bug.
    pub fn update(&self, f: impl FnOnce(&mut TaskProgress)) {
        // Invariant: nobody panics with the lock held.
        let mut st = self.inner.lock().expect("progress lock sound");
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

    /// Current snapshot (even if not published).
    ///
    /// # Panics
    /// Never in practice (see [`Self::update`]).
    #[must_use]
    pub fn snapshot(&self) -> TaskProgress {
        self.inner
            .lock()
            .expect("progress lock sound")
            .current
            .clone()
    }
}
