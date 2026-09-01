//! Gestión de conexiones remotas y secretos (ADR 0015).
//!
//! Lee `connections.toml` (solo referencias), resuelve secretos (env → keyring
//! → fichero `age`) y establece el transporte: SSH con TOFU de host key
//! ([`SshConnector`]) y FTP/FTPS con política TLS ([`FtpConnector`]). La
//! sesión resultante se INYECTA al provider (ADR 0013/0014): los providers
//! jamás ven un secreto, y los secretos jamás se loguean (regla 10).
#![forbid(unsafe_code)]

mod error;
mod ftp;
mod known_hosts;
mod s3;
mod secret;
mod spec;
mod ssh;

pub use error::{ConnectError, SecretOrigin};
pub use ftp::{FtpConnectOutcome, FtpConnector};
pub use known_hosts::KnownHostsStore;
pub use s3::S3Connector;
// Re-exports: los tipos que `SftpProvider::new`/`FtpProvider::new`/
// `ObjectProvider::new` aceptan (inyección de sesión, ADR 0013/0014/0016); así
// el core no necesita deps directas de russh-sftp/suppaftp/opendal.
pub use opendal::Operator;
pub use russh_sftp::client::SftpSession;
pub use secret::{Secret, SecretResolver, env_key, journal_anchor_key};
pub use spec::{
    AddressingStyle, AuthMethod, ConnectionSpec, ConnectionsFile, Endpoint, SecretSource, TlsMode,
};
pub use ssh::SshConnector;
pub use suppaftp::tokio::AsyncRustlsFtpStream as FtpStream;
