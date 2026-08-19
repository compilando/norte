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
/// `#[non_exhaustive]` (#126): antes de esto, cada variante nueva rompía la
/// API de Rust para quien hiciera match exhaustivo fuera de este crate — por
/// eso `Mkdir`, `Embed` y `RenameBatch` tocaron los dos frontends en su propio
/// commit. Es una propiedad SOLO de la API de Rust: invisible en JSON, no
/// mueve el wire, no toca `#[serde(other)]` ni pide bump de versión de
/// protocolo. El coste es simétrico al beneficio: un `match` externo ahora
/// necesita un brazo `_`, así que el compilador deja de señalar dónde un kind
/// nuevo necesita etiqueta — cada `_` debe hacer lo mismo que ya hace el
/// brazo de [`TaskKind::Unknown`] en ese mismo match, no inventar un
/// comportamiento nuevo.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// Comparación de DOS árboles de directorios
    /// (`fs.compare`/[`FS_COMPARE`](crate::methods::FS_COMPARE), 0.39.0, ADR
    /// 0048). Lectura pura (regla 4 no aplica): sin journal, sin undo, no
    /// escribe un byte. El progreso cuenta PAREJAS emitidas, no bytes: con el
    /// rung de hash apagado la comparación no lee contenido alguno, así que una
    /// barra de bytes pintaría cero para siempre — mismo caso que
    /// [`TaskKind::RenameBatch`].
    ///
    /// Entra CON el método, en su mismo bump, y no después: el `task_id` de un
    /// lote de [`COMPARE_ROWS`](crate::methods::COMPARE_ROWS) correlaciona con
    /// una Task que el cliente tiene que poder clasificar en `task.list`. Un
    /// cliente N-1 (0.38.x) la degrada a [`TaskKind::Unknown`] por el
    /// `serde(other)` de abajo, igual que `Search`/`Index`/`Embed`/
    /// `RenameBatch`.
    Compare,
    /// Cuánto ocupa un árbol de directorios
    /// (`fs.dir_size`/[`FS_DIR_SIZE`](crate::methods::FS_DIR_SIZE), 0.49.0,
    /// #139). Lectura pura (regla 4 no aplica): sin journal, sin undo, ni un
    /// byte escrito.
    ///
    /// El progreso de ésta SÍ cuenta bytes, al revés que
    /// [`TaskKind::Compare`]: los bytes son justo lo que se está preguntando.
    /// Lo que no lleva son totales —`bytes_total` y `entries_total` van a
    /// `None` hasta el final— porque el total es el resultado, y una barra
    /// hacia un número inventado es peor que ninguna barra.
    ///
    /// Entra CON el método. Un cliente N-1 (0.48.x) la degrada a
    /// [`TaskKind::Unknown`] por el `serde(other)` de abajo, igual que
    /// `Search`/`Index`/`Embed`/`RenameBatch`/`Compare`.
    DirSize,
    /// Fabricar un archivo
    /// (`archive.pack`/[`ARCHIVE_PACK`](crate::methods::ARCHIVE_PACK), 0.50.0,
    /// #132). MUTA: journal como UNA creación, y deshacerlo es borrar el
    /// archivo.
    ///
    /// El progreso cuenta bytes LEÍDOS del origen y entradas empaquetadas; los
    /// bytes escritos no se pueden saber por adelantado —el compresor decide—
    /// y prometer un total que va a fallar es peor que no darlo.
    Pack,
    /// Comprobar un archivo
    /// (`archive.test`/[`ARCHIVE_TEST`](crate::methods::ARCHIVE_TEST), 0.50.0,
    /// #132). Lectura pura: sin journal.
    TestArchive,
    /// Partir un fichero en trozos
    /// (`file.split`/[`FILE_SPLIT`](crate::methods::FILE_SPLIT), 0.50.0,
    /// #132). MUTA: una creación por trozo.
    Split,
    /// Juntar los trozos
    /// (`file.combine`/[`FILE_COMBINE`](crate::methods::FILE_COMBINE), 0.50.0,
    /// #132). MUTA: una creación.
    Combine,
    /// Planificación de una sincronización de un sentido
    /// (`sync.plan`/[`SYNC_PLAN`](crate::methods::SYNC_PLAN), 0.40.0, ADR
    /// 0049). Lectura pura (regla 4 no aplica): planificar no escribe un byte
    /// — lo que escribe es [`TaskKind::Sync`]. El progreso cuenta PASOS
    /// emitidos, no bytes, por el mismo motivo que
    /// [`TaskKind::Compare`]: es la comparación de debajo con una decisión por
    /// fila, y con el rung de hash apagado no se lee contenido alguno.
    ///
    /// Entra CON el método, en su mismo bump, y por la misma razón que
    /// `Compare`: el `task_id` de un lote de
    /// [`SYNC_STEPS`](crate::methods::SYNC_STEPS) correlaciona con una Task que
    /// el cliente tiene que poder clasificar en `task.list`. Un cliente N-1
    /// (0.39.x) la degrada a [`TaskKind::Unknown`] por el `serde(other)` de
    /// abajo.
    SyncPlan,
    /// Ejecución de un plan de sincronización APROBADO
    /// (`sync.apply`/[`SYNC_APPLY`](crate::methods::SYNC_APPLY), 0.40.0, ADR
    /// 0049): copias, sobrescrituras y borrados como UNA unidad deshacible del
    /// journal (regla 4). A diferencia de [`TaskKind::SyncPlan`] su progreso sí
    /// tiene bytes que contar. Un cliente N-1 (0.39.x) la degrada a
    /// [`TaskKind::Unknown`].
    Sync,
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
