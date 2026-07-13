//! Gestión de conexiones remotas y secretos (ADR 0015).
//!
//! Lee `connections.toml` (solo referencias), resuelve secretos (env → keyring
//! → fichero `age`) y —en fases posteriores— establece el transporte SSH/FTP e
//! inyecta la sesión al provider. Los secretos jamás se loguean (regla 10).
#![forbid(unsafe_code)]

mod error;
mod secret;
mod spec;

pub use error::ConnectError;
pub use secret::{Secret, SecretResolver};
pub use spec::{AuthMethod, ConnectionSpec, ConnectionsFile, Endpoint, TlsMode};
