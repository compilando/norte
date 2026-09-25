//! A daemon task, seen from the client.
//!
//! The core has its own `TaskRef` that wraps this or the embedded scheduler;
//! what lives here is only the REMOTE shape, and it keeps the distinction it
//! already had: the owner waits, observers clone, and cancelling is
//! idempotent.

use norte_proto::{TaskId, TaskProgress};
use tokio::sync::watch;

/// Clonable cancellation for a remote task: `task.cancel` against the daemon,
/// fire-and-forget (the real confirmation arrives via `task.progress`, which
/// is the method's contract).
#[derive(Clone)]
pub struct RemoteTaskCanceller {
    backend: crate::remote::RemoteBackend,
    id: TaskId,
}

impl RemoteTaskCanceller {
    /// Builds the handle. Used by the backend itself when registering the
    /// task.
    #[must_use]
    pub(crate) fn new(backend: crate::remote::RemoteBackend, id: TaskId) -> Self {
        Self { backend, id }
    }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) {
        self.backend.spawn_cancel(self.id);
    }

    /// Pauses (`true`) or resumes (`false`) the task (0.82.0, ADR 0147).
    /// Lives on the same handle as cancel because it is the same control
    /// over the same task; the `paused` state arrives via `task.progress`.
    ///
    /// # Errors
    /// `Unsupported` against a daemon that cannot pause (0.81 or earlier).
    pub async fn set_paused(&self, paused: bool) -> Result<(), norte_proto::Error> {
        self.backend.set_paused(self.id, paused).await
    }

    /// Moves the task up (`true`) or down in the serial queue, if it has not
    /// started yet (0.83.0, ADR 0149).
    ///
    /// # Errors
    /// `Unsupported` against a daemon that does not know the queue (0.82 or
    /// earlier).
    pub async fn mover_en_cola(&self, up: bool) -> Result<(), norte_proto::Error> {
        self.backend.mover_en_cola(self.id, up).await
    }
}

/// A task running in the daemon.
///
/// NOT `Clone` on purpose: the WAIT has a single owner. What gets shared is
/// the progress (a `watch`) and the cancellation (a [`RemoteTaskCanceller`]).
pub struct RemoteTask {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: RemoteTaskCanceller,
}

impl RemoteTask {
    /// Assembles the remote task with its progress channel and its
    /// cancellation.
    #[must_use]
    pub(crate) fn new(
        id: TaskId,
        rx: watch::Receiver<TaskProgress>,
        canceller: RemoteTaskCanceller,
    ) -> Self {
        Self { id, rx, canceller }
    }

    /// The task's id.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Live progress snapshots.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Clonable cancellation handle.
    #[must_use]
    pub fn canceller(&self) -> RemoteTaskCanceller {
        self.canceller.clone()
    }
}
