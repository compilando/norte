//! Daemon JSON-RPC sobre UDS (ADR 0011, spec §17.6): un daemon por usuario,
//! jamás root, autenticado por `SO_PEERCRED`. Windows queda diferido con
//! issue (el modo embebido sigue siendo el camino allí).
//!
//! - [`Daemon`] (server): acepta conexiones, autentica, despacha
//!   `fs.*`/`task.*` y difunde `task.progress` a los humanos y al dueño de
//!   cada task (#66: una conexión de agente no observa tasks ajenas).
//! - [`Client`]: conexión de frontend (initialize, call, notificaciones,
//!   `connect_or_spawn`).

pub mod approvals;
mod server;

pub use approvals::DaemonApprovalResolver;
pub use server::{Daemon, DaemonConfig};

// El lado CLIENTE vive en `norte-client` desde ADR 0066: el SDK no puede
// depender del core, así que la dirección del socket y el JSON-RPC enmarcado
// viven allí y se re-exportan aquí para que los consumidores de siempre
// (CLI, MCP, tests e2e) sigan nombrándolos donde los nombraban.
pub use norte_client::{
    Client, ClientError, daemon_run_argv, default_socket_path, is_version_mismatch,
};

/// Errores del ciclo de vida del daemon (lado servidor).
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// I/O del socket o del filesystem del socket.
    #[error("i/o del daemon: {0}")]
    Io(#[from] std::io::Error),
    /// El daemon JAMÁS corre como root (spec §17.6).
    #[error("el daemon no corre como root")]
    Root,
    /// El directorio del socket no es seguro (dueño/modo/symlink).
    #[error("directorio del socket inseguro: {reason}")]
    InsecureDir {
        /// Qué comprobación falló.
        reason: &'static str,
    },
    /// El dir por defecto en `/tmp/norte-<uid>` no es utilizable (#34.1):
    /// típicamente pre-ocupado por otro usuario (`squat`, denegación de
    /// disponibilidad no de integridad: el daemon rehúsa secuestrarlo).
    /// Accionable: fijar `XDG_RUNTIME_DIR` (el camino soportado) o pasar
    /// `--socket <ruta>` a un dir propio.
    #[error(
        "el dir del socket por defecto ({path}) no es utilizable ({reason}); \
         fija XDG_RUNTIME_DIR o pasa --socket <ruta>"
    )]
    UnusableDefaultDir {
        /// El path del dir fallback que no se pudo usar.
        path: std::path::PathBuf,
        /// Qué comprobación falló.
        reason: &'static str,
    },
    /// Ya hay un daemon vivo escuchando en el socket.
    #[error("ya hay un daemon escuchando en el socket")]
    AlreadyRunning,
}
