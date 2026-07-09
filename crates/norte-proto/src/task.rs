//! Tipos de Task: toda operación larga es una Task con id, estado y progreso
//! (regla dura 3 de `CLAUDE.md`). La notificación `task.progress` viaja
//! coalescida (≤30 Hz) — el coalescido es del emisor, no del tipo.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, VPath};

/// Identificador de una Task, único por proceso core.
///
/// Wire: número JSON transparente.
///
/// ```
/// use norte_proto::TaskId;
/// let id = TaskId::new(42);
/// assert_eq!(serde_json::to_string(&id).unwrap(), "42");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(u64);

impl TaskId {
    /// Construye desde el contador del scheduler.
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// El valor numérico.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Clase de operación que ejecuta una Task (M0: las tres mutaciones del VFS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Copia (posiblemente recursiva, posiblemente cross-provider).
    Copy,
    /// Movimiento (rename atómico o copy+delete).
    Move,
    /// Borrado (recursivo post-order).
    Delete,
}

/// Estado del ciclo de vida de una Task.
///
/// Wire: objeto tagged `{"kind": "...", …}` — mismo convenio que [`Error`].
/// Tolerancia N/N-1 (ADR 0004): un estado desconocido deserializa a
/// [`TaskState::Unknown`], que se trata como NO terminal (conservador:
/// el cliente sigue escuchando `task.progress` hasta un estado que entienda).
///
/// ```
/// use norte_proto::TaskState;
/// let s: TaskState = serde_json::from_str(r#"{"kind": "running"}"#).unwrap();
/// assert_eq!(s, TaskState::Running);
/// assert!(!s.is_terminal());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskState {
    /// Encolada, aún sin hueco en el scheduler.
    Pending,
    /// Ejecutándose.
    Running,
    /// Pausada (spec §11: `task.pause`). M0 no la emite; queda reservada ya
    /// para que estrenarla no rompa a clientes N-1.
    Paused,
    /// Terminó bien.
    Completed,
    /// Cancelada limpiamente (destino limpio o `.norte-partial`, spec §5).
    Cancelled,
    /// Falló; el error dice por qué (un panic capturado llega como
    /// [`Error::Internal`] con `panic: true`).
    Failed {
        /// Causa del fallo.
        error: Error,
    },
    /// Estado de un protocolo más nuevo (fallback de deserialización).
    /// El core JAMÁS lo emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl TaskState {
    /// `true` si la Task ya no va a cambiar de estado. [`TaskState::Unknown`]
    /// cuenta como no-terminal: ante un estado que no entiende, el cliente
    /// sigue escuchando.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed { .. }
        )
    }
}

/// Snapshot de progreso de una Task (payload de la notificación `task.progress`).
///
/// Totales `None` = aún desconocidos (walk en curso), jamás un 0 fingido.
/// El emisor coalesce; el último snapshot de una Task siempre lleva estado
/// terminal y totales finales.
///
/// ```
/// use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
/// let p = TaskProgress {
///     task_id: TaskId::new(7),
///     kind: TaskKind::Copy,
///     state: TaskState::Running,
///     bytes_done: 512,
///     bytes_total: Some(4096),
///     entries_done: 0,
///     entries_total: Some(2),
///     current: None,
/// };
/// let json = serde_json::to_string(&p).unwrap();
/// assert_eq!(serde_json::from_str::<TaskProgress>(&json).unwrap(), p);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgress {
    /// Task a la que pertenece el snapshot.
    pub task_id: TaskId,
    /// Clase de operación (un frontend pinta "copiando…" sin estado propio).
    pub kind: TaskKind,
    /// Estado en el momento del snapshot.
    pub state: TaskState,
    /// Bytes ya procesados.
    pub bytes_done: u64,
    /// Bytes totales estimados; `None` mientras el walk no termina.
    #[serde(default)]
    pub bytes_total: Option<u64>,
    /// Entradas (archivos/dirs) ya procesadas.
    pub entries_done: u64,
    /// Entradas totales estimadas; `None` mientras el walk no termina.
    #[serde(default)]
    pub entries_total: Option<u64>,
    /// Entrada en curso (para pintar "copiando X…"); puede faltar.
    #[serde(default)]
    pub current: Option<VPath>,
}
