//! Una task del daemon, vista desde el cliente.
//!
//! El core tiene su propia `TaskRef` que envuelve esto o al scheduler
//! embebido; lo que vive aquí es solo la forma REMOTA, y conserva la
//! distinción que ya tenía: el dueño espera, los observadores se clonan, y
//! cancelar es idempotente.

use norte_proto::{TaskId, TaskProgress};
use tokio::sync::watch;

/// Cancelación clonable de una task remota: `task.cancel` contra el daemon,
/// fire-and-forget (la confirmación real llega por `task.progress`, que es el
/// contrato del método).
#[derive(Clone)]
pub struct RemoteTaskCanceller {
    backend: crate::remote::RemoteBackend,
    id: TaskId,
}

impl RemoteTaskCanceller {
    /// Construye el asa. La usa el propio backend al registrar la task.
    #[must_use]
    pub(crate) fn new(backend: crate::remote::RemoteBackend, id: TaskId) -> Self {
        Self { backend, id }
    }

    /// Pide la cancelación cooperativa.
    pub fn cancel(&self) {
        self.backend.spawn_cancel(self.id);
    }

    /// Pausa (`true`) o reanuda (`false`) la task (0.82.0, ADR 0147). Vive
    /// en la misma asa que cancelar porque es el mismo control sobre la
    /// misma task; el estado `paused` llega por `task.progress`.
    ///
    /// # Errors
    /// `Unsupported` contra un daemon que no sabe pausar (0.81 o anterior).
    pub async fn set_paused(&self, paused: bool) -> Result<(), norte_proto::Error> {
        self.backend.set_paused(self.id, paused).await
    }
}

/// Una task en marcha en el daemon.
///
/// NO es `Clone` a propósito: la ESPERA tiene un solo dueño. Lo que se
/// reparte es el progreso (un `watch`) y la cancelación (un
/// [`RemoteTaskCanceller`]).
pub struct RemoteTask {
    id: TaskId,
    rx: watch::Receiver<TaskProgress>,
    canceller: RemoteTaskCanceller,
}

impl RemoteTask {
    /// Arma la task remota con su canal de progreso y su cancelación.
    #[must_use]
    pub(crate) fn new(
        id: TaskId,
        rx: watch::Receiver<TaskProgress>,
        canceller: RemoteTaskCanceller,
    ) -> Self {
        Self { id, rx, canceller }
    }

    /// Id de la task.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Snapshots vivos del progreso.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TaskProgress> {
        self.rx.clone()
    }

    /// Asa de cancelación, clonable.
    #[must_use]
    pub fn canceller(&self) -> RemoteTaskCanceller {
        self.canceller.clone()
    }
}
