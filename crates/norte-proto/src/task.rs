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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
///
// TODO(#126): es el ÚNICO enum de wire sin `#[non_exhaustive]`, así que cada
// variante nueva rompe la API de Rust para quien haga match exhaustivo (por eso
// `Mkdir`, `Embed` y `RenameBatch` tocaron los dos frontends en su propio
// commit). El wire está cubierto por el `serde(other)` de abajo; lo que falta es
// decidir si ese match exhaustivo es un coste o una función — hoy es lo que
// obliga a etiquetar un kind nuevo en vez de pintarlo como «task».
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Copia (posiblemente recursiva, posiblemente cross-provider).
    Copy,
    /// Movimiento (rename atómico o copy+delete).
    Move,
    /// Borrado (recursivo post-order).
    Delete,
    /// Undo de sesión: deshace mutaciones previas en LIFO (M3-2).
    ///
    /// OJO (compat): esta variante entra en 0.10.0. Un cliente 0.9.x (N-1) NO la
    /// conoce y su parse de `TaskKind` FALLA al recibirla — el `serde(other)` de
    /// abajo protege a ESTE proto (0.10+) frente a kinds de 0.11+, no
    /// retroactivamente al 0.9. En M3-2 el undo no se expone por RPC (no llega a
    /// clientes), así que la rotura es latente; M3-4 debe gatear la emisión de
    /// kinds nuevos por versión negociada (o asumir descarte silencioso en
    /// broadcast y proteger el resync de `task.list`).
    Undo,
    /// Búsqueda viva por nombre y/o contenido bajo un subtree (`fs.search`,
    /// M4 live search). Lectura pura (regla 4 no aplica): sin journal.
    ///
    /// OJO (compat): esta variante entra en 0.18.0. A diferencia del borde
    /// 0.9→0.10 de [`TaskKind::Undo`] (donde el `serde(other)` de abajo
    /// TODAVÍA no existía), un cliente 0.17.x (N-1) YA tiene ese fallback
    /// (desde 0.10) — recibirla la degrada a [`TaskKind::Unknown`] sin
    /// fallar el parse. Sin gating de emisión necesario para este borde.
    Search,
    /// Creación de un directorio (`fs.mkdir`, #104). Mutación: pasa por el
    /// journal como `Created` con su undo (regla 4). Entra en 0.31.0; un
    /// cliente N-1 (0.30.x) la degrada a [`TaskKind::Unknown`] vía el
    /// `serde(other)`, mismo caso que `Search`/`Index`.
    Mkdir,
    /// Construcción/actualización del índice de búsqueda de un subtree
    /// (`index.build`, M4). Entra en 0.25.0; un cliente N-1 (0.24.x) la degrada a
    /// [`TaskKind::Unknown`] vía el `serde(other)`.
    Index,
    /// `index.embed` (0.33.0): generación de embeddings del índice semántico.
    /// Un cliente N-1 (0.32.x) la degrada a [`TaskKind::Unknown`] por su
    /// `serde(other)`.
    Embed,
    /// Un lote de renames dentro de UN directorio ejecutado como UNA
    /// transacción con UNA unidad deshacible del journal (`fs.rename_batch`,
    /// 0.36.0). El progreso es `i/n` PASOS, no bytes. Un cliente N-1 (0.35.x)
    /// la degrada a [`TaskKind::Unknown`] por el `serde(other)` de abajo, igual
    /// que `Search`/`Index`/`Embed`.
    RenameBatch,
    /// Clase desconocida: un daemon N+1 (0.11+) envió un kind que ESTE proto no
    /// conoce → se acepta como genérica en vez de fallar el parse (forward-compat
    /// desde 0.10, como [`TaskState::Unknown`]). No cubre el borde hacia atrás
    /// 0.9→0.10 (ver `Undo`); SÍ cubre 0.17→0.18 (ver `Search`, ya nacida
    /// dentro de la ventana de este fallback).
    ///
    /// `TaskKind::Index` (0.25.0, `index.build`) es el mismo caso que `Search`:
    /// un cliente 0.24.x lo degrada aquí sin fallar.
    #[serde(other)]
    Unknown,
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
