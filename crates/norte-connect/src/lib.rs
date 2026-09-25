//! Management of remote connections and secrets (ADR 0015).
//!
//! Reads `connections.toml` (references only), resolves secrets (env → keyring
//! → `age` file) and sets up the transport: SSH with host key TOFU
//! ([`SshConnector`]) and FTP/FTPS with a TLS policy ([`FtpConnector`]). The
//! resulting session is INJECTED into the provider (ADR 0013/0014): providers
//! never see a secret, and secrets are never logged (rule 10).
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
// Re-exports: the types that `SftpProvider::new`/`FtpProvider::new`/
// `ObjectProvider::new` accept (session injection, ADR 0013/0014/0016); this
// way the core does not need direct deps on russh-sftp/suppaftp/opendal.
pub use opendal::Operator;
pub use russh_sftp::client::SftpSession;
pub use secret::{Secret, SecretResolver, env_key, journal_anchor_key};
pub use spec::{
    AddressingStyle, AuthMethod, ConnectionSpec, ConnectionsFile, Endpoint, SecretSource, TlsMode,
};
pub use ssh::SshConnector;
pub use suppaftp::tokio::AsyncRustlsFtpStream as FtpStream;
