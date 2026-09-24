//! SSH/SFTP connection establishment (ADR 0015 A/D/E): TOFU host key
//! verification against the [`KnownHostsStore`], auth by password / ed25519
//! key (RSA with `allow_rsa`, ADR 0150) / agent, and opening the sftp
//! subsystem. Returns the
//! [`SftpSession`] that `SftpProvider::new` accepts (session injection,
//! ADR 0013): the provider never sees a secret.
//!
//! russh's auth types do NOT cross this crate's boundary: the core consumes
//! [`SshConnector`] with its own types (`ConnectionSpec`, `Secret`,
//! `ConnectError`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use russh::client::AuthResult;
use russh::keys::agent::AgentIdentity;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh_sftp::client::SftpSession;
use zeroize::Zeroizing;

use crate::error::ConnectError;
use crate::known_hosts::{HostKeyStatus, KnownHostsStore, algo, fingerprint};
use crate::secret::Secret;
use crate::spec::{AuthMethod, ConnectionSpec};

/// Default SSH port when the URL does not carry one.
const SSH_PORT: u16 = 22;

/// SSH connector: wraps the TOFU store and the agent socket.
///
/// The fields are public so the core (or a test) can inject explicit paths;
/// [`SshConnector::new`] resolves the environment's defaults.
#[derive(Debug, Clone)]
pub struct SshConnector {
    /// Host key store (TOFU, ADR 0015 D).
    pub known_hosts: KnownHostsStore,
    /// SSH agent socket (`SSH_AUTH_SOCK`); `None` = no agent.
    pub agent_socket: Option<PathBuf>,
}

/// Auth material already validated, BEFORE touching the network: rejecting
/// an RSA key (ADR 0015 E) is local and clear, not a connection error.
enum PreparedAuth {
    Password,
    // Box: a PrivateKey weighs >400 bytes and will end up in an Arc anyway.
    Key(Box<PrivateKey>),
    Agent,
}

impl SshConnector {
    /// Connector with environment defaults: `known_hosts` in the config dir
    /// (or override `NORTE_KNOWN_HOSTS`) and the agent from `SSH_AUTH_SOCK`.
    #[must_use]
    pub fn new(config_dir: &Path) -> Self {
        Self {
            known_hosts: KnownHostsStore::new(config_dir),
            agent_socket: std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
        }
    }

    /// Connects over SSH per `spec`, verifies the host key against the store
    /// (strict TOFU) and opens the sftp subsystem.
    ///
    /// `secret` is the password (`auth = "password"`) or the key's
    /// passphrase (`auth = "key"`, `None` if the key is not encrypted);
    /// resolved upstream by the `SecretResolver`. Never logged (rule 10).
    ///
    /// # Errors
    /// - [`ConnectError::HostKeyUnknown`]: first contact; confirm with
    ///   [`SshConnector::trust_host_key`] and retry.
    /// - [`ConnectError::HostKeyMismatch`]: the registered key changed
    ///   (possible MITM); never connects.
    /// - [`ConnectError::KeyUnsupported`]: non-ed25519 client key
    ///   (ADR 0015 E), rejected BEFORE touching the network.
    /// - [`ConnectError::AuthFailed`]: the server rejected the credentials.
    // No raw URL fields: the span opens BEFORE the parser can reject an
    // inline `user:pass@host` (rule 10). The redacted endpoint (host:port)
    // is recorded after parsing.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<SftpSession, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "sftp" {
            // Scheme only: the whole URL is not echoed in an error.
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (the SSH connector only accepts sftp://)",
                ep.scheme
            )));
        }
        let user = resolve_user(ep.user.as_deref())?;
        let port = ep.port.unwrap_or(SSH_PORT);
        tracing::debug!(host = %ep.host, port, "establishing sftp connection");

        // Auth prerequisites BEFORE dialing: rejecting an RSA key
        // (ADR 0015 E) or a missing agent are local, clear errors, not
        // network errors.
        let prepared = match spec.auth {
            AuthMethod::AccessKey => {
                return Err(ConnectError::Config(
                    "auth = \"access-key\" is from s3, not SSH; use \"key\", \"password\" or \"agent\""
                        .to_string(),
                ));
            }
            AuthMethod::Password => PreparedAuth::Password,
            AuthMethod::Agent => {
                if self.agent_socket.is_none() {
                    return Err(ConnectError::Agent("unavailable (SSH_AUTH_SOCK not set)"));
                }
                PreparedAuth::Agent
            }
            AuthMethod::Key => {
                let path = spec.key.as_deref().ok_or_else(|| {
                    ConnectError::Config(format!(
                        "the connection to {} has auth = \"key\" without `key = ...`",
                        ep.host
                    ))
                })?;
                PreparedAuth::Key(Box::new(
                    load_client_key(path, secret, spec.allow_rsa).await?,
                ))
            }
        };

        let handler = TofuHandler {
            host: ep.host.clone(),
            port,
            store: self.known_hosts.clone(),
        };
        let config = Arc::new(russh::client::Config::default());
        let mut handle = russh::client::connect(config, (ep.host.as_str(), port), handler).await?;

        let authed = match prepared {
            PreparedAuth::Password => {
                // Redacted identifier (host, not the URL) per rule 10.
                let s = secret.ok_or_else(|| ConnectError::Secret {
                    conn: ep.host.clone(),
                })?;
                // russh copies the password into its own String; freed with
                // the session (russh API limitation, does not end up in
                // logs).
                handle
                    .authenticate_password(user.clone(), s.expose())
                    .await?
            }
            PreparedAuth::Key(key) => {
                // Outside RSA the hash does not apply (None). With RSA —
                // which only reaches here with `allow_rsa`, ADR 0150— rsa-sha2
                // is negotiated.
                let hash_alg = if key.algorithm().is_rsa() {
                    let hash = rsa_hash(&handle, &ep.host).await?;
                    tracing::warn!(
                        host = %ep.host,
                        "authenticating with an RSA key (allow_rsa, ADR 0150): \
                         signing through the RUSTSEC-2023-0071 path"
                    );
                    Some(hash)
                } else {
                    None
                };
                handle
                    .authenticate_publickey(
                        user.clone(),
                        PrivateKeyWithHashAlg::new(Arc::new(*key), hash_alg),
                    )
                    .await?
            }
            PreparedAuth::Agent => self.authenticate_via_agent(&mut handle, &user).await?,
        };
        if !authed.success() {
            return Err(ConnectError::AuthFailed {
                user,
                host: ep.host,
            });
        }

        let channel = handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| ConnectError::Ssh(format!("sftp handshake: {e}")))
    }

    /// Registers `host:port`'s host key in the store AFTER verifying that
    /// its real fingerprint matches `expected_fingerprint` (the one
    /// `HostKeyUnknown` showed and the user confirmed). Anti-TOCTOU
    /// re-verification: if the host now presents ANOTHER key, it is a
    /// [`ConnectError::HostKeyMismatch`] and nothing gets registered.
    ///
    /// # Errors
    /// If the host does not respond, or the presented fingerprint does not
    /// match.
    #[tracing::instrument(level = "debug", skip_all, fields(host = %host, port))]
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: u16,
        expected_fingerprint: &str,
    ) -> Result<(), ConnectError> {
        let (tx, rx) = mpsc::channel();
        let config = Arc::new(russh::client::Config::default());
        let dial = russh::client::connect(config, (host, port), CaptureHandler { tx }).await;
        let key = match (rx.try_recv(), dial) {
            // The handler captures the key and REJECTS the handshake: the
            // dial ends in an "expected" error that carries no information
            // here anymore.
            (Ok(key), _) => key,
            // No key presented: the real error is the network/handshake one.
            (Err(_), Err(e)) => return Err(e),
            (Err(_), Ok(_)) => {
                return Err(ConnectError::Ssh(format!(
                    "{host}:{port} did not present a host key"
                )));
            }
        };
        let presented = fingerprint(&key);
        if presented != expected_fingerprint {
            return Err(ConnectError::HostKeyMismatch {
                host: host.to_string(),
                port,
                algo: algo(&key),
                fingerprint: presented,
            });
        }
        let store = self.known_hosts.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || {
            // russh's learn only APPENDS: if the host already has ANOTHER
            // key on record, "trusting on top" would leave two conflicting
            // entries and the check stuck in a perpetual Mismatch despite a
            // successful trust. Fail-closed: let the user remove the old
            // entry.
            match store.check(&host, port, &key)? {
                HostKeyStatus::Known => Ok(()), // idempotent
                HostKeyStatus::Unknown { .. } => store.learn(&host, port, &key),
                // The category TRAVELS as HostKeyMismatch (it is the
                // rotation/MITM scenario the taxonomy has it for); the
                // actionable guidance stays in the core's log.
                HostKeyStatus::Mismatch { algo, fingerprint } => {
                    tracing::warn!(
                        host = %host,
                        port,
                        "trust rejected: {host}:{port} already has another key on record \
                         (rotation?); remove the old entry from known_hosts before trusting \
                         the new one"
                    );
                    Err(ConnectError::HostKeyMismatch {
                        host: host.clone(),
                        port,
                        algo,
                        fingerprint,
                    })
                }
            }
        })
        .await
        .map_err(|_| ConnectError::KnownHosts("registration interrupted".into()))?
    }

    /// Authenticates by trying the agent's ed25519 identities (ADR 0015 E:
    /// also via agent, only ed25519).
    async fn authenticate_via_agent(
        &self,
        handle: &mut russh::client::Handle<TofuHandler>,
        user: &str,
    ) -> Result<AuthResult, ConnectError> {
        let Some(sock) = self.agent_socket.as_deref() else {
            return Err(ConnectError::Agent("unavailable (SSH_AUTH_SOCK not set)"));
        };
        #[cfg(not(unix))]
        {
            let _ = (sock, handle, user);
            Err(ConnectError::Agent(
                "only a unix-socket agent is supported for now",
            ))
        }
        #[cfg(unix)]
        {
            let mut agent = russh::keys::agent::client::AgentClient::connect_uds(sock)
                .await
                .map_err(|_| ConnectError::Agent("could not connect to the agent's socket"))?;
            let identities = agent.request_identities().await.map_err(|_| {
                ConnectError::Agent("the agent did not respond when listing identities")
            })?;
            let mut last = None;
            for id in identities {
                let AgentIdentity::PublicKey { key, .. } = id else {
                    continue;
                };
                if key.algorithm() != Algorithm::Ed25519 {
                    continue;
                }
                let res = handle
                    .authenticate_publickey_with(user, key, None, &mut agent)
                    .await
                    .map_err(|e| ConnectError::Ssh(e.to_string()))?;
                if res.success() {
                    return Ok(res);
                }
                last = Some(res);
            }
            // The caller turns a failed AuthResult into AuthFailed.
            last.ok_or(ConnectError::Agent("the agent has no ed25519 identities"))
        }
    }
}

/// Real TOFU handler (replaces the phase-5 tests' `Ok(true)`): the server's
/// key is STRICTLY verified against the store; unknown or changed aborts
/// the handshake with the corresponding typed error.
struct TofuHandler {
    host: String,
    port: u16,
    store: KnownHostsStore,
}

impl russh::client::Handler for TofuHandler {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let store = self.store.clone();
        let host = self.host.clone();
        let port = self.port;
        let key = server_public_key.clone();
        // File I/O off the reactor (rule 2).
        let status = tokio::task::spawn_blocking(move || store.check(&host, port, &key))
            .await
            .map_err(|_| ConnectError::KnownHosts("verification interrupted".into()))??;
        match status {
            HostKeyStatus::Known => Ok(true),
            HostKeyStatus::Unknown { algo, fingerprint } => Err(ConnectError::HostKeyUnknown {
                host: self.host.clone(),
                port: self.port,
                algo,
                fingerprint,
            }),
            HostKeyStatus::Mismatch { algo, fingerprint } => Err(ConnectError::HostKeyMismatch {
                host: self.host.clone(),
                port: self.port,
                algo,
                fingerprint,
            }),
        }
    }
}

/// `trust_host_key`'s handler: captures the presented key and REJECTS the
/// handshake (we only wanted to see it; nothing is ever authenticated or
/// sent).
struct CaptureHandler {
    tx: mpsc::Sender<PublicKey>,
}

impl russh::client::Handler for CaptureHandler {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // If the receiver died, the connect was already abandoned: nothing
        // to do.
        let _ = self.tx.send(server_public_key.clone());
        Ok(false)
    }
}

/// The RSA signature hash, from what the server announces in
/// `server-sig-algs`.
///
/// rsa-sha2-512 or -256 if announced. If it only announces `ssh-rsa`
/// (SHA-1), it is an error: ADR 0150's opt-in opens up RSA, never SHA-1.
///
/// russh's `None` merges three cases: the server does not send the
/// extension (RFC 8308 is optional), sends it with no RSA algorithm at all,
/// or it arrives after the second one russh waits for. In all three,
/// rsa-sha2-256 is tried —what any server from the last decade accepts—
/// and, if it does not accept it, fails closed as `AuthFailed`. It never
/// falls back to SHA-1: with `Some(hash)` russh signs and announces exactly
/// that hash.
async fn rsa_hash(
    handle: &russh::client::Handle<TofuHandler>,
    host: &str,
) -> Result<HashAlg, ConnectError> {
    match handle.best_supported_rsa_hash().await? {
        Some(Some(hash)) => Ok(hash),
        Some(None) => Err(ConnectError::RsaSha1Only {
            host: host.to_owned(),
        }),
        None => {
            tracing::debug!(
                host,
                "server-sig-algs without rsa-sha2 (or no extension): trying rsa-sha2-256"
            );
            Ok(HashAlg::Sha256)
        }
    }
}

/// Loads the client's private key (with `~` expanded) and applies the
/// algorithm policy: ed25519 always (ADR 0015 E); RSA only with `allow_rsa`
/// (ADR 0150), because signing with it is the RUSTSEC-2023-0071 path. The
/// smallest RSA modulus norte will sign with, even with `allow_rsa` (#370).
///
/// 2048 because it is RFC 8332 §3's floor for `rsa-sha2-*`, the one NIST SP
/// 800-57 left after retiring 1024 in 2013, and the one OpenSSH has enforced
/// when generating since 2017. Below that it is not "weaker": it is a key
/// nobody should keep using to authenticate against anything.
///
/// **It is a separate limit from what `allow_rsa` buys, and that is why the
/// opt-in does not lift it.** ADR 0150 accepts one named risk —the
/// RUSTSEC-2023-0071 timing side channel— which is identical at 1024 bits
/// and at 4096. A short modulus is a different thing, the ADR does not
/// mention it, and whoever signed off on the opt-in did not accept it: it
/// was riding along silently.
const RSA_MIN_BITS: usize = 2048;

/// The bit length of an RSA key's modulus. `None` if it is not RSA or cannot
/// be read.
///
/// Comes from the modulus's length in BYTES, so it rounds up to seven bits.
/// For what it is used for —comparing against 2048, which is a multiple of
/// 8— that changes no verdict: a 2048-bit key gives exactly 2048 and a
/// 1024-bit one gives exactly 1024.
fn bits_del_modulo(key: &PrivateKey) -> Option<usize> {
    let rsa = key.public_key().key_data().rsa()?;
    Some(rsa.n().as_positive_bytes()?.len() * 8)
}

async fn load_client_key(
    path: &Path,
    passphrase: Option<&Secret>,
    allow_rsa: bool,
) -> Result<PrivateKey, ConnectError> {
    // The passphrase travels zeroized up to russh's decryption.
    let pass = passphrase.map(|s| Zeroizing::new(s.expose().to_string()));
    let for_task = path.to_path_buf();
    // expand_tilde also inside: without $HOME, home_dir() falls back to
    // getpwuid_r (NSS can touch disk/network) — blocking, off the reactor
    // (rule 2).
    let (expanded, loaded) = tokio::task::spawn_blocking(move || {
        let expanded = expand_tilde(&for_task);
        let loaded = russh::keys::load_secret_key(&expanded, pass.as_deref().map(String::as_str));
        (expanded, loaded)
    })
    .await
    .map_err(|_| ConnectError::KeyLoad {
        path: path.to_path_buf(),
        cause: "loading interrupted".into(),
    })?;
    let key = loaded.map_err(|e| ConnectError::KeyLoad {
        // russh's error (format/passphrase) does not contain the passphrase.
        path: expanded.clone(),
        cause: e.to_string(),
    })?;
    match key.algorithm() {
        Algorithm::Ed25519 => Ok(key),
        Algorithm::Rsa { .. } if allow_rsa => {
            // The opt-in opens up RSA, not ANY RSA (#370). See
            // `RSA_MIN_BITS`: it is the limit ADR 0150 meant to set and
            // forgot to write down.
            match bits_del_modulo(&key) {
                Some(bits) if bits < RSA_MIN_BITS => Err(ConnectError::RsaTooSmall {
                    path: expanded,
                    bits,
                    min: RSA_MIN_BITS,
                }),
                // Without being able to read the modulus nothing is
                // asserted and it is let through: the risk `allow_rsa`
                // already bought stays the same, and refusing here would
                // break a good key just for not knowing how to read it.
                _ => Ok(key),
            }
        }
        other => Err(ConnectError::KeyUnsupported {
            path: expanded,
            algo: other.to_string(),
        }),
    }
}

/// Expands a leading `~` to the user's home (`key = "~/.ssh/id_ed25519"` in
/// `connections.toml`). Operates at the `Path` component level, without
/// assuming UTF-8 in the rest of the path (rule 1).
fn expand_tilde(path: &Path) -> PathBuf {
    match (std::env::home_dir(), path.strip_prefix("~")) {
        (Some(home), Ok(rest)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Effective user: the URL's, or the environment's (like OpenSSH).
fn resolve_user(explicit: Option<&str>) -> Result<String, ConnectError> {
    if let Some(u) = explicit {
        return Ok(u.to_string());
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .map_err(|_| ConnectError::MissingUser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_solo_prefix() {
        let home = std::env::home_dir().expect("home in the test environment");
        assert_eq!(
            expand_tilde(Path::new("~/.ssh/id_ed25519")),
            home.join(".ssh/id_ed25519")
        );
        // No leading `~`: unchanged (an interior `~` is NOT expanded).
        assert_eq!(
            expand_tilde(Path::new("/abs/~/x")),
            PathBuf::from("/abs/~/x")
        );
        assert_eq!(expand_tilde(Path::new("rel/x")), PathBuf::from("rel/x"));
    }

    #[test]
    fn resolve_user_explicito_wins() {
        assert_eq!(resolve_user(Some("oscar")).unwrap(), "oscar");
    }
}
