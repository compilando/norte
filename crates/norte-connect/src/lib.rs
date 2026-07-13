//! Gestión de conexiones remotas y secretos (ADR 0015).
//!
//! Lee `connections.toml` (solo referencias), resuelve secretos (env → keyring
//! → fichero `age`) y —en fases posteriores— establece el transporte SSH/FTP e
//! inyecta la sesión al provider. Los secretos jamás se loguean (regla 10).
#![forbid(unsafe_code)]

mod error;
mod known_hosts;
mod secret;
mod spec;
mod ssh;

pub use error::ConnectError;
pub use known_hosts::KnownHostsStore;
// Re-export: es el tipo que `SftpProvider::new` acepta (inyección de sesión,
// ADR 0013); así el core no necesita una dep directa de russh-sftp.
pub use russh_sftp::client::SftpSession;
pub use secret::{Secret, SecretResolver};
pub use spec::{AuthMethod, ConnectionSpec, ConnectionsFile, Endpoint, TlsMode};
pub use ssh::SshConnector;
