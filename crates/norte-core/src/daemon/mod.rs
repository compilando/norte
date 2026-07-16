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
mod client;
mod server;

pub use approvals::DaemonApprovalResolver;
pub use client::{Client, ClientError, is_version_mismatch};
pub use server::{Daemon, DaemonConfig};

use std::path::PathBuf;

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
    /// Ya hay un daemon vivo escuchando en el socket.
    #[error("ya hay un daemon escuchando en el socket")]
    AlreadyRunning,
}

/// Path por defecto del socket: `$XDG_RUNTIME_DIR/norte/daemon.sock`
/// (el runtime dir ya es 0700 por usuario); sin `XDG_RUNTIME_DIR`,
/// `/tmp/norte-<uid>/daemon.sock` — el dir lo crea y VERIFICA el server
/// ([`Daemon::bind`]): dueño = uid del proceso, modo 0700, jamás symlink.
///
/// `uid_hint` solo se usa para el fallback de /tmp (el server lo deriva de
/// su propio socket; los clientes, del dir que encuentran).
#[must_use]
pub fn default_socket_path(uid_hint: Option<u32>) -> PathBuf {
    socket_path_from(
        std::env::var_os("XDG_RUNTIME_DIR"),
        uid_hint.unwrap_or_else(process_uid_best_effort),
    )
}

/// La lógica pura de [`default_socket_path`] (testeable sin tocar el
/// entorno global — que en edición 2024 exige `unsafe`, prohibido aquí).
fn socket_path_from(xdg: Option<std::ffi::OsString>, uid: u32) -> PathBuf {
    if let Some(runtime) = xdg.filter(|v| !v.is_empty()) {
        return PathBuf::from(runtime).join("norte").join("daemon.sock");
    }
    PathBuf::from(format!("/tmp/norte-{uid}")).join("daemon.sock")
}

/// uid del proceso SIN unsafe (regla 5): el dueño de un archivo temporal
/// recién creado por nosotros ES nuestro euid. Solo se usa para NOMBRAR el
/// dir de /tmp; la seguridad real la dan las verificaciones de
/// [`Daemon::bind`] sobre dueño y modo.
pub(crate) fn process_uid_best_effort() -> u32 {
    use std::os::unix::fs::MetadataExt;
    let probe = std::env::temp_dir().join(format!(
        ".norte-uid-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
    ));
    let uid = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata())
        .map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    // Fallback imposible en la práctica (temp_dir no escribible): 0 hará
    // que el chequeo anti-root de bind() rechace, fail-safe.
    uid.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{process_uid_best_effort, socket_path_from};

    #[test]
    fn socket_path_usa_xdg_si_esta() {
        let p = socket_path_from(Some("/run/user/4242".into()), 1000);
        assert_eq!(p, std::path::Path::new("/run/user/4242/norte/daemon.sock"));
    }

    #[test]
    fn socket_path_cae_a_tmp_sin_xdg() {
        assert_eq!(
            socket_path_from(None, 1000),
            std::path::Path::new("/tmp/norte-1000/daemon.sock")
        );
        // XDG vacío = como ausente.
        assert_eq!(
            socket_path_from(Some(String::new().into()), 7),
            std::path::Path::new("/tmp/norte-7/daemon.sock")
        );
    }

    #[test]
    fn uid_best_effort_es_nuestro_euid() {
        use std::os::unix::fs::MetadataExt;
        // El dueño de un archivo que acabamos de crear ES nuestro euid.
        let probe = std::env::temp_dir().join(format!(".norte-uid-test-{}", std::process::id()));
        let f = std::fs::File::create(&probe).expect("crear sonda");
        let expected = f.metadata().expect("metadata").uid();
        drop(f);
        let _ = std::fs::remove_file(&probe);
        assert_eq!(process_uid_best_effort(), expected);
    }
}
