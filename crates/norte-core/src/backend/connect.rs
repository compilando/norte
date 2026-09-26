//! [`Backend`](super::Backend)'s connection area: closing a remote session,
//! trusting a host key (TOFU) and delivering a requested secret.

use norte_proto::Error;

use super::Backend;

impl Backend {
    /// Closes `path`'s remote session (#140). `false` = there wasn't one.
    ///
    /// # Errors
    ///
    /// Whatever the transport returns. An N-1 daemon with no such method
    /// answers `METHOD_NOT_FOUND` → [`Error::Unsupported`].
    pub async fn close_connection(&self, path: &norte_proto::VPath) -> Result<bool, Error> {
        match self {
            Self::Embedded(engine) => Ok(engine.close_connection(path)),
            Self::Remote(r) => r.close_connection(path).await,
        }
    }

    /// Registers `host:port`'s host key after the user's EXPLICIT
    /// confirmation (TOFU flow: an `Error::HostKeyUnknown` brought the
    /// fingerprint, the frontend showed it and the user accepted — ADR 0015
    /// D). The core re-verifies the fingerprint against the host's real key
    /// before registering it (anti-TOCTOU).
    ///
    /// # Errors
    /// Protocol taxonomy ([`Error::HostKeyMismatch`] if the host no longer
    /// presents that key).
    /// `algo` travels informationally on the wire (the identity being
    /// confirmed is the fingerprint); pass it exactly as it arrived in the
    /// `HostKeyUnknown`.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        algo: &str,
        fingerprint: &str,
    ) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => {
                let _ = algo; // the engine confirms by fingerprint
                engine.trust_host_key(host, port, fingerprint).await
            }
            Self::Remote(r) => r.trust_host_key(host, port, algo, fingerprint).await,
        }
    }

    /// Delivers to the core the secret for `conn` the human just typed,
    /// after an [`Error::SecretNeeded`] (#325). Lives in memory, in the
    /// process holding the engine, and only until that process stops: it is
    /// never persisted anywhere.
    ///
    /// # Errors
    /// Protocol taxonomy; [`Error::Unsupported`] if there's no connector.
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => engine.provide_secret(conn, secret).await,
            Self::Remote(r) => r.provide_secret(conn, secret).await,
        }
    }
}
