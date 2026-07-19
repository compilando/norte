//! norte-gui — SPIKE M5 (hito 1), Task 4: carga del listado real del daemon.
//!
//! Regla 7: el frontend SOLO habla `norte-proto` (vía el `RemoteBackend` de
//! `norte-core`); aquí es read-only (`fs.list`). El daemon debe estar YA
//! corriendo (`spawn_cmd = None`): el spike no lo autoarranca — coherente con
//! «va siempre por daemon» y más simple.
//!
//! # GPUI + tokio (el puente que este step descubre)
//!
//! GPUI trae su propio executor (no tokio); el `RemoteBackend` es tokio puro
//! (spawnea tasks, usa `tokio::sync`). No se pueden mezclar: la conexión+listado
//! corren en un **runtime tokio propio en un hilo aparte** ([`spawn_load`]), y el
//! resultado cruza al hilo de render por un `tokio::sync::oneshot`. El receptor
//! se `.await`-ea DENTRO de un `cx.spawn` de GPUI (ver `main.rs`): un oneshot de
//! tokio es awaitable en cualquier executor (solo registra un waker, no toca el
//! runtime), así que el render nunca se bloquea esperando al daemon.

use std::path::PathBuf;

use norte_core::backend::Backend;
use norte_proto::methods::ClientInfo;
use norte_proto::{Entry, VPath};

/// Resultado del listado que cruza al hilo de GPUI. El error se aplana a
/// `String` (ya renderizable) porque solo se muestra: el detalle tipado vive en
/// el log del hilo de carga, no en la UI.
pub type ListOutcome = Result<Vec<Entry>, String>;

/// Resultado de un listado ETIQUETADO con el pane destino (0|1) y el `VPath`
/// que se pidió. El índice deja que el hilo de render aplique el listado al
/// pane correcto; el `VPath` deja anclar el resultado al directorio que se
/// listó (un cd posterior no debe verse pisado por un resultado viejo).
pub struct PaneListOutcome {
    /// Pane destino (0|1).
    pub pane: usize,
    /// Directorio que se listó (el que estaba en `begin_loading`).
    pub dir: VPath,
    /// Entradas o error aplanado.
    pub outcome: ListOutcome,
}

/// Config resuelta de entorno: a qué socket y qué dir listar.
pub struct LoadConfig {
    /// Socket UDS del daemon.
    pub socket: PathBuf,
    /// Directorio a listar (wire `VPath`).
    pub dir: VPath,
}

impl LoadConfig {
    /// Resuelve la config desde el entorno:
    /// - Socket: `NORTE_SOCKET` si está; si no, el default del daemon
    ///   (`$XDG_RUNTIME_DIR/norte/daemon.sock`), el MISMO que usa `norte daemon run`.
    /// - Dir: `NORTE_DIR` (wire, p. ej. `file:///home/oscar`) si está; si no, el
    ///   `cwd` convertido a `VPath` `file://` igual que la TUI.
    ///
    /// # Errors
    /// Si `NORTE_DIR` no parsea como `VPath`, o el `cwd` no se puede leer/convertir.
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

/// Conecta al daemon (sin autoarrancarlo) y lista `dir`. Reusable: cada cd la
/// invoca de nuevo (conexión fresca por listado — el spike no comparte el
/// `RemoteBackend` entre cds porque su bomba interna vive atada al runtime tokio
/// que la creó, y GPUI corre en otro executor; reconectar es simple y correcto,
/// compartir es optimización posterior). Async; corre sobre un runtime tokio
/// (ver [`spawn_list`]).
async fn connect_and_list(
    socket: &std::path::Path,
    dir: &VPath,
) -> Result<Vec<Entry>, norte_proto::Error> {
    let remote = norte_core::backend::remote::RemoteBackend::connect(
        socket.to_path_buf(),
        None, // exige daemon ya corriendo
        ClientInfo {
            name: "norte-gui".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await?;
    // `Backend::list` drena el stream paginado a un Vec completo.
    Backend::Remote(remote).list(dir).await
}

/// Lanza un hilo con su propio runtime tokio que conecta al `socket`, lista
/// `dir` y envía el resultado (etiquetado con `pane` y `dir`) por `tx`. No
/// bloquea al llamante (el hilo de render de GPUI).
///
/// El runtime muere al terminar `block_on` (la bomba interna del `RemoteBackend`
/// se aborta con él): el listado ya se drenó, no queda nada vivo. Si el daemon
/// no está, `connect` devuelve `ProviderUnavailable` → mensaje de error, jamás
/// panic.
pub fn spawn_list(
    socket: PathBuf,
    dir: VPath,
    pane: usize,
    tx: tokio::sync::oneshot::Sender<PaneListOutcome>,
) {
    std::thread::spawn(move || {
        let outcome = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt
                .block_on(connect_and_list(&socket, &dir))
                .map_err(|e| format!("{e}")),
            Err(e) => Err(format!("no se pudo crear el runtime tokio: {e}")),
        };
        if std::env::var_os("NORTE_GUI_DEBUG").is_some() {
            match &outcome {
                Ok(entries) => eprintln!(
                    "[norte-gui] pane {pane}: listado recibido del daemon: {} entradas de {dir}",
                    entries.len(),
                ),
                Err(e) => eprintln!("[norte-gui] pane {pane}: carga de {dir} falló: {e}"),
            }
        }
        // El receptor pudo soltarse (ventana cerrada antes de tiempo): ignora.
        let _ = tx.send(PaneListOutcome { pane, dir, outcome });
    });
}
