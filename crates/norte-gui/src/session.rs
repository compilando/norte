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

/// Config resuelta del entorno (socket + dir inicial).
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
    Cancel(TaskId),
    /// Abre el visor de `path`: intenta un previewer de plugin, si no lee bytes.
    OpenViewer {
        /// Archivo a abrir en el visor.
        path: VPath,
        /// Generación del open (guard anti-stale, como `List`): un
        /// `ViewerOpened`/`ViewerFailed` con una generación vieja se
        /// descarta en `apply_event` (un F3 tardío no reabre por sorpresa
        /// encima de lo que el usuario esté haciendo).
        generation: u64,
    },
    /// Decora `paths` (G3b, ADR 0037): pide `plugin.decorate` para la
    /// página VISIBLE que acaba de listar `pane` — mismo guard anti-stale
    /// (`generation`/`dir`) que `List`/`OpenViewer`, así el `apply_event` de
    /// la GUI descarta una respuesta tardía de un cd ya superado.
    Decorate {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado (para el guard anti-stale al aplicar).
        dir: VPath,
        /// Rutas visibles a decorar, en el orden del listado.
        paths: Vec<VPath>,
    },
}

/// Contenido del viewer que cruza a la GUI (GUI-d T3).
pub enum ViewerContent {
    /// Salida de un previewer de plugin (texto + nombre).
    Plugin {
        /// Nombre legible del plugin previewer (para el indicador «via …»).
        plugin_name: String,
        /// Salida del plugin (texto), sin sanear todavía — la sanea el
        /// `Viewer` core al construirse (`with_plugin_preview`).
        output: String,
    },
    /// Salida CON ESTILO de un previewer de plugin (G3a, ADR 0037): el
    /// gemelo estructurado de [`Self::Plugin`] — líneas de spans (`role`
    /// SIN VALIDAR aún, se valida al construir el `Viewer` core,
    /// `with_plugin_preview_styled`) en vez de una cadena plana.
    PluginStyled {
        /// Nombre legible del plugin previewer (para el indicador «via …»).
        plugin_name: String,
        /// Líneas de spans, sin sanear/validar todavía — lo hace el
        /// `Viewer` core al construirse.
        lines: Vec<Vec<norte_proto::methods::SpanWire>>,
    },
    /// Bytes crudos (posiblemente truncados al presupuesto).
    Raw {
        /// Los bytes leídos (acotados a `FS_READ_MAX_CHUNK`).
        bytes: Vec<u8>,
        /// `true` si se alcanzó el tope de lectura: puede haber más archivo.
        truncated: bool,
    },
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
        /// Entradas + omitidas del contenedor (#93/#96) o error aplanado a
        /// String (ya renderizable).
        outcome: Result<(Vec<Entry>, Option<u64>), String>,
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
    /// El visor de `path` está listo con su contenido.
    ViewerOpened {
        /// Archivo mostrado.
        path: VPath,
        /// Preview de plugin o bytes crudos.
        content: ViewerContent,
        /// Imagen YA decodificada en el hilo de sesión (#92: el decode caro
        /// jamás corre en el hilo de UI). `None` = el contenido no es una
        /// imagen reconocida (o vino de un previewer plugin).
        image: Option<ImageDecode>,
        /// Generación del `OpenViewer` que lo pidió (guard anti-stale).
        generation: u64,
    },
    /// No se pudo abrir el visor de `path` (error ya renderizable).
    ViewerFailed {
        /// Archivo que se intentó abrir.
        path: VPath,
        /// Mensaje ya renderizable.
        error: String,
        /// Generación del `OpenViewer` que lo pidió (guard anti-stale).
        generation: u64,
    },
    /// Resultado de un `Decorate` (G3b, ADR 0037): el mapa YA aplanado y
    /// SANEADO (`norte_frontend::merge_decorations`/`sanitize_decoration` —
    /// el mismo criterio que la TUI, un solo lugar de saneado compartido
    /// por ambos frontends). Un daemon sin decoradores consentidos (o un
    /// `MethodNotFound` de un daemon viejo, ya absorbido por
    /// `Backend::plugin_decorate`) da un mapa vacío, nunca un error — el
    /// listado se pinta igual, sin badges.
    Decorated {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió (guard anti-stale).
        generation: u64,
        /// Directorio listado (guard anti-stale: debe casar el `dir`
        /// vigente del pane al aplicar).
        dir: VPath,
        /// Decoraciones ya saneadas, por ruta.
        decorations: HashMap<VPath, norte_frontend::Decoration>,
    },
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
                            let outcome = backend
                                .list_with_skipped(&dir)
                                .await
                                .map_err(|e| format!("{e}"));
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
                    SessionCmd::OpenViewer { path, generation } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(
                            async move { open_viewer(&backend, path, generation, &tx).await },
                        );
                    }
                    SessionCmd::Decorate {
                        pane,
                        generation,
                        dir,
                        paths,
                    } => {
                        if paths.is_empty() {
                            continue;
                        }
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let plugins = backend.plugin_decorate(&paths).await.unwrap_or_default();
                            let decorations = norte_frontend::merge_decorations(&paths, &plugins);
                            let _ = tx.send(SessionEvent::Decorated {
                                pane,
                                generation,
                                dir,
                                decorations,
                            });
                        });
                    }
                }
            }
        });
    });
}

/// Intenta un previewer de plugin; si ninguno aplica, lee bytes acotados. El
/// `truncated` se estima con el tope de lectura alcanzado (más bytes de los
/// leídos podrían quedar pendientes). `generation` viaja intacta a los
/// eventos (guard anti-stale de `apply_event`, como `List`).
async fn open_viewer(
    backend: &Backend,
    path: VPath,
    generation: u64,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    // G3a (ADR 0037): intenta el preview CON ESTILO primero — `Ok(Some(_))`
    // = un previewer aplicó; `Ok(None)` (ninguno aplica, un guest cayó, o
    // los topes del wire se violaron — todo degrada igual, ver
    // `Backend::plugin_preview_styled`) o `Err` (fallo de red) caen al
    // preview PLANO clásico, que a su vez cae a los bytes crudos.
    if let Ok(Some(p)) = backend.plugin_preview_styled(&path).await {
        let _ = tx.send(SessionEvent::ViewerOpened {
            path,
            content: ViewerContent::PluginStyled {
                plugin_name: p.plugin_name,
                lines: p.lines,
            },
            image: None,
            generation,
        });
        return;
    }
    // `Err` (preview falló) o `Ok` sin previewer aplicable: cae a la vista
    // cruda igual, sin distinguir el motivo aquí.
    if let Ok(res) = backend.plugin_preview(&path).await
        && let Some(p) = res.preview
    {
        let _ = tx.send(SessionEvent::ViewerOpened {
            path,
            content: ViewerContent::Plugin {
                plugin_name: p.plugin_name,
                output: p.output,
            },
            image: None,
            generation,
        });
        return;
    }
    let budget = norte_proto::methods::FS_READ_MAX_CHUNK;
    match backend
        .read(
            &path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(budget),
            }),
        )
        .await
    {
        Ok(bytes) => {
            let truncated = bytes.len() as u64 >= budget; // leímos el tope: puede haber más.
            // #92: decode de imagen AQUÍ (hilo de sesión), no en el handler
            // de UI — una imagen cara (JPEG progresivo cerca del tope) ya no
            // congela el frame. Solo bytes que el viewer pintará como imagen.
            let image = norte_frontend::viewer::image_format(&bytes)
                .is_some()
                .then(|| decode_image(&bytes));
            let _ = tx.send(SessionEvent::ViewerOpened {
                path,
                content: ViewerContent::Raw { bytes, truncated },
                image,
                generation,
            });
        }
        Err(e) => {
            let _ = tx.send(SessionEvent::ViewerFailed {
                path,
                error: format!("{e}"),
                generation,
            });
        }
    }
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

/// Presupuesto de píxeles del preview de imagen (#92, movido del hilo de UI):
/// `into_rgba8` alloca ancho×alto×4 y puede coexistir con el buffer del
/// decoder — pico real ≈ 32 MP × 4 × 2 ≈ 256 MiB (una sola imagen a la vez).
const MAX_IMAGE_PIXELS: u64 = 32_000_000;

/// Frame BGRA crudo decodificado en el hilo de sesión (#92): la UI solo lo
/// ENVUELVE en su tipo de render (O(1)), jamás decodifica.
pub struct DecodedImage {
    /// Píxeles BGRA8 (ancho×alto×4 bytes).
    pub bgra: Vec<u8>,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
}

/// Resultado del decode de sesión (#92): espejo transportable de la
/// distinción Ready/Unreadable del preview de la UI.
pub enum ImageDecode {
    /// Decodificada y lista para envolver en el render.
    Ready(DecodedImage),
    /// El decode falló (truncada/corrupta/excede el presupuesto): la UI
    /// pinta el aviso i18n, sin reintentar.
    Unreadable,
}

/// Decodifica bytes de imagen a BGRA8 con guardia anti-bomba (#92): primero
/// SOLO las dimensiones de la cabecera (rechazo por encima de
/// [`MAX_IMAGE_PIXELS`] antes de allocar), luego decode con
/// `image::Limits` (acota las allocaciones internas del codec) → RGBA8 →
/// BGRA in-place. Cualquier fallo → [`ImageDecode::Unreadable`]; jamás panic
/// ni OOM. GIF/WebP animados: primer frame (preview estático).
pub(crate) fn decode_image(bytes: &[u8]) -> ImageDecode {
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let Some((width, height)) = dims else {
        return ImageDecode::Unreadable;
    };
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return ImageDecode::Unreadable;
    }
    let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return ImageDecode::Unreadable;
    };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_PIXELS.saturating_mul(4));
    reader.limits(limits);
    let Ok(decoded) = reader.decode() else {
        return ImageDecode::Unreadable;
    };
    let mut rgba = decoded.into_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    // Dimensiones REALES del decode (la cabecera pudo mentir a la baja).
    let (width, height) = rgba.dimensions();
    ImageDecode::Ready(DecodedImage {
        bgra: rgba.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};

    fn snap(id: TaskId, state: TaskState) -> TaskProgress {
        TaskProgress {
            task_id: id,
            kind: TaskKind::Copy,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        }
    }

    fn id(n: u64) -> TaskId {
        TaskId::new(n)
    }

    /// #85: la conexión muere SIN desenlace (el watch se cierra) —
    /// `forward_progress` sintetiza un terminal `Failed{ProviderUnavailable}`
    /// reusando id/kind del último snapshot y DE-REGISTRA el canceller.
    #[tokio::test]
    async fn forward_progress_sintetiza_terminal_en_muerte_de_conexion() {
        let tid = id(7);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();
        cancellers.lock().unwrap().insert(tid, task.canceller());

        let fut = {
            let cancellers = Arc::clone(&cancellers);
            let ev_tx = ev_tx.clone();
            tokio::spawn(async move { forward_progress(task, &ev_tx, &cancellers).await })
        };
        // Muerte de la conexión: el emisor del watch desaparece.
        drop(watch_tx);
        fut.await.expect("join");

        // Primer evento: el snapshot vivo tal cual.
        let SessionEvent::Task(first) = ev_rx.try_recv().expect("snapshot inicial") else {
            panic!("esperaba SessionEvent::Task");
        };
        assert_eq!(first.state, TaskState::Running);
        // Segundo: el terminal SINTETIZADO (mismos id/kind, Failed honesto).
        let SessionEvent::Task(dead) = ev_rx.try_recv().expect("terminal sintetizado") else {
            panic!("esperaba SessionEvent::Task");
        };
        assert_eq!(dead.task_id, tid);
        assert_eq!(dead.kind, TaskKind::Copy);
        assert!(
            matches!(
                dead.state,
                TaskState::Failed {
                    error: norte_proto::Error::ProviderUnavailable { retryable: true }
                }
            ),
            "fue {:?}",
            dead.state
        );
        assert!(
            cancellers.lock().unwrap().is_empty(),
            "el canceller se de-registra también en el camino de muerte"
        );
    }

    /// #85: un terminal REAL publicado por el watch llega como evento y
    /// de-registra el canceller, sin terminal sintetizado de más.
    #[tokio::test]
    async fn forward_progress_terminal_real_desregistra_sin_extra() {
        let tid = id(9);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();
        cancellers.lock().unwrap().insert(tid, task.canceller());

        let fut = {
            let cancellers = Arc::clone(&cancellers);
            let ev_tx = ev_tx.clone();
            tokio::spawn(async move { forward_progress(task, &ev_tx, &cancellers).await })
        };
        watch_tx
            .send(snap(tid, TaskState::Completed))
            .expect("terminal");
        fut.await.expect("join");

        let mut states = Vec::new();
        while let Ok(SessionEvent::Task(p)) = ev_rx.try_recv() {
            states.push(p.state);
        }
        assert_eq!(
            states.last(),
            Some(&TaskState::Completed),
            "el último evento es el terminal real: {states:?}"
        );
        assert!(
            !states.iter().any(|s| matches!(s, TaskState::Failed { .. })),
            "sin terminal sintetizado de más: {states:?}"
        );
        assert!(cancellers.lock().unwrap().is_empty());
    }
}
