//! Backend de SESIÓN persistente: UN `RemoteBackend` vivo toda la sesión, en
//! un hilo con runtime tokio propio. Reemplaza el reconnect-per-cd del spike
//! (GUI-a deuda T4): la conexión se establece una vez y todos los listados
//! (y en tasks posteriores las mutaciones) van por ella — condición NECESARIA
//! para seguir el progreso de una task (`task.progress` llega por la BOMBA del
//! `RemoteBackend`, que exige conexión viva).
//!
//! # Puente GPUI ↔ tokio
//! GPUI (executor propio) manda [`SessionCmd`] por un `mpsc` sin bloquear; el
//! hilo tokio ejecuta y devuelve [`SessionEvent`] por otro `mpsc` que la GUI
//! drena en UN `cx.spawn` (los `mpsc` de tokio son awaitables en cualquier
//! executor: solo registran un waker). Un daemon caído en `connect` emite
//! [`SessionEvent::ConnectFailed`], jamás panic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use norte_core::backend::{Backend, TaskCanceller, TaskRef, remote::RemoteBackend};
use norte_proto::methods::ClientInfo;
use norte_proto::{Entry, Error, TaskId, TaskProgress, TaskState, VPath};
use tokio::sync::mpsc;

use crate::modal::{PendingOp, TransferKind};

/// Config resuelta del entorno (socket + dir inicial). Idéntica a la del spike
/// (movida de `backend_task.rs`).
pub struct LoadConfig {
    /// Socket UDS del daemon.
    pub socket: PathBuf,
    /// Directorio inicial de ambos panes.
    pub dir: VPath,
}

impl LoadConfig {
    /// Resuelve desde el entorno: `NORTE_SOCKET` o el default del daemon;
    /// `NORTE_DIR` (wire) o el `cwd`.
    ///
    /// # Errors
    /// `NORTE_DIR` no parsea, o el `cwd` no se puede leer/convertir.
    pub fn from_env() -> anyhow::Result<Self> {
        let socket = match std::env::var_os("NORTE_SOCKET") {
            Some(s) => PathBuf::from(s),
            None => norte_core::daemon::default_socket_path(None),
        };
        let dir = match std::env::var("NORTE_DIR") {
            Ok(wire) => VPath::parse(&wire)
                .map_err(|e| anyhow::anyhow!("NORTE_DIR no es un VPath válido: {e}"))?,
            Err(_) => {
                let cwd = std::env::current_dir()?;
                norte_vfs_local::vpath_from_native(&cwd)
                    .map_err(|e| anyhow::anyhow!("cwd → VPath: {e}"))?
            }
        };
        Ok(Self { socket, dir })
    }
}

/// Comando de la GUI hacia el hilo de sesión.
pub enum SessionCmd {
    /// Lista `dir` en `pane`; `generation` es el guard anti-stale de GUI-a.
    List {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd (ver `main::generation_is_current`).
        generation: u64,
        /// Directorio a listar.
        dir: VPath,
    },
    /// Lanza una operación mutante (copy/move/delete) como task.
    Submit(PendingOp),
    /// Cancela una task en curso (`task.cancel`, fire-and-forget).
    // allow(dead_code): aún sin constructor — `Action::CancelTask` es no-op
    // hasta la task de la franja de tasks (GUI-b, posterior a esta), que
    // sabrá qué `TaskId` está seleccionado y mandará este comando.
    #[allow(dead_code)]
    Cancel(TaskId),
}

/// Evento del hilo de sesión hacia la GUI.
pub enum SessionEvent {
    /// Resultado de un `List` (etiquetado con pane/generación/dir).
    Listed {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado.
        dir: VPath,
        /// Entradas o error aplanado a String (ya renderizable).
        outcome: Result<Vec<Entry>, String>,
    },
    /// La conexión inicial con el daemon falló (mensaje ya renderizable).
    ConnectFailed(String),
    /// La op fue aceptada; su task corre con este id (para mapear progreso →
    /// operación en la GUI).
    Submitted {
        /// Id de la task recién creada.
        task_id: TaskId,
        /// La operación que la originó (para read-after-write y reintento).
        op: PendingOp,
    },
    /// La op fue RECHAZADA antes de crear task (path inválido, unsupported…).
    SubmitFailed {
        /// La operación rechazada.
        op: PendingOp,
        /// El error tipado (la GUI decide: banner o modal de conflicto).
        error: Error,
    },
    /// Snapshot de progreso de una task (incl. estado terminal).
    Task(TaskProgress),
}

/// Arranca el hilo de sesión: conecta al `socket` UNA vez y sirve `cmd_rx`,
/// emitiendo por `event_tx`. Si `connect` falla, emite `ConnectFailed` y el
/// hilo termina (la GUI queda en estado de error, usable).
pub fn spawn(
    socket: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(SessionEvent::ConnectFailed(format!("runtime tokio: {e}")));
                return;
            }
        };
        rt.block_on(async move {
            let remote = match connect(&socket).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(SessionEvent::ConnectFailed(format!("{e}")));
                    return;
                }
            };
            let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> =
                Arc::new(Mutex::new(HashMap::new()));
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SessionCmd::List {
                        pane,
                        generation,
                        dir,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let outcome = backend.list(&dir).await.map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::Listed {
                                pane,
                                generation,
                                dir,
                                outcome,
                            });
                        });
                    }
                    SessionCmd::Submit(op) => {
                        submit(&remote, op, &event_tx, &cancellers).await;
                    }
                    SessionCmd::Cancel(id) => {
                        // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
                        if let Some(c) = cancellers.lock().unwrap().get(&id) {
                            c.cancel();
                        }
                    }
                }
            }
        });
    });
}

/// Conecta al daemon (sin autoarrancarlo).
async fn connect(socket: &Path) -> Result<RemoteBackend, norte_proto::Error> {
    RemoteBackend::connect(
        socket.to_path_buf(),
        None,
        ClientInfo {
            name: "norte-gui".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
}

/// Lanza una op mutante y arranca su forwarder de progreso, o emite
/// `SubmitFailed` si el daemon la rechaza de entrada.
async fn submit(
    remote: &RemoteBackend,
    op: PendingOp,
    event_tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let backend = Backend::Remote(remote.clone());
    let res = match &op {
        PendingOp::Transfer {
            kind: TransferKind::Copy,
            from,
            to,
            opts,
        } => backend.copy(from, to, *opts).await,
        PendingOp::Transfer {
            kind: TransferKind::Move,
            from,
            to,
            opts,
        } => backend.move_(from, to, *opts).await,
        PendingOp::Delete { path, mode } => backend.delete(path, *mode).await,
    };
    match res {
        Ok(task) => {
            let id = task.id();
            // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
            cancellers.lock().unwrap().insert(id, task.canceller());
            let _ = event_tx.send(SessionEvent::Submitted { task_id: id, op });
            let tx = event_tx.clone();
            let cancellers = Arc::clone(cancellers);
            tokio::spawn(async move { forward_progress(task, &tx, &cancellers).await });
        }
        Err(error) => {
            let _ = event_tx.send(SessionEvent::SubmitFailed { op, error });
        }
    }
}

/// Reenvía cada snapshot de progreso de `task` como `SessionEvent::Task` hasta
/// el terminal; de-registra el canceller al salir. Si la conexión muere sin
/// desenlace, sintetiza un terminal `Failed{ProviderUnavailable}` reusando el
/// último snapshot (mismos id/kind).
async fn forward_progress(
    task: TaskRef,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let id = task.id();
    let mut rx = task.progress();
    loop {
        let snap = rx.borrow().clone();
        let terminal = snap.state.is_terminal();
        let _ = tx.send(SessionEvent::Task(snap.clone()));
        if terminal {
            break;
        }
        if rx.changed().await.is_err() {
            let mut dead = snap;
            dead.state = TaskState::Failed {
                error: Error::ProviderUnavailable { retryable: true },
            };
            let _ = tx.send(SessionEvent::Task(dead));
            break;
        }
    }
    // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
    cancellers.lock().unwrap().remove(&id);
}
