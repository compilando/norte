//! Errors of `norte-connect`. Designed so that NO variant can carry secret
//! material (rule 10): the causes coming from the secret store are STATIC
//! messages (`&'static str`), never strings derived from the plaintext.

use std::path::PathBuf;

use thiserror::Error;

/// Which of the resolver's three rungs a secret came from (ADR 0015 C).
///
/// Vocabulary deliberately CLOSED: it is the only thing that tells the user
/// WHERE the gap is, and with loose `&'static str`s a swap between two call
/// sites would point at the wrong rung without any test going red (rust
/// review MINOR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretOrigin {
    /// `NORTE_SECRET_<CONN>`.
    Env,
    /// The OS keyring.
    Keyring,
    /// `secrets.age` file.
    AgeFile,
    /// What a human typed in this session (#325).
    Session,
}

impl std::fmt::Display for SecretOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Env => "environment variable",
            Self::Keyring => "keyring",
            Self::AgeFile => "secrets.age",
            Self::Session => "what was typed in this session",
        })
    }
}

/// Error resolving a connection or its secret.
///
/// Projected onto the protocol's taxonomy with `norte_proto::Error::from`:
/// the TOFU variants go 1:1; the rest degrade to a category (the detail
/// stays in the core's log).
#[derive(Debug, Error)]
pub enum ConnectError {
    /// Malformed or invalid `connections.toml` (references only, no secrets).
    #[error("connections.toml: {0}")]
    Config(String),
    /// A connection URL that could not be parsed.
    #[error("invalid connection URL: {0}")]
    InvalidUrl(String),
    /// Failed to resolve a connection's secret. Carries only the connection's
    /// NAME, never the secret (rule 10).
    #[error("could not resolve the secret for connection «{conn}»")]
    Secret {
        /// Connection name (never the secret).
        conn: String,
    },
    /// A connection's secret resolved, but is the EMPTY string (#320).
    /// Rejected instead of passed along: opendal discards an empty
    /// `secret_access_key` (`if !v.is_empty()`), never registers the static
    /// provider, and the connection would end up authenticating with the
    /// ambient chain (profile, SSO, IMDS) — an identity nobody asked for.
    /// Carries the connection's name and the ORIGIN of the gap, never the
    /// secret (rule 10).
    ///
    /// Does NOT apply to `auth = "key"`: there the secret is the key's
    /// passphrase, where empty and absent are the same thing and there is
    /// nothing to impersonate. `establish` filters that out in the core.
    #[error(
        "the secret for connection «{conn}» is set but EMPTY ({origin}): give it a real value or remove it"
    )]
    SecretEmpty {
        /// Connection name (never the secret).
        conn: String,
        /// Which rung of the resolver the empty value came from.
        origin: SecretOrigin,
    },
    /// The secret's env var exists but its bytes are NOT valid UTF-8.
    ///
    /// It used to be treated as "not there" (`env::var(..).ok()`) and
    /// resolution fell through to the keyring: a Latin-1 password vanished
    /// silently and `norte doctor` reported it as present (rust review
    /// MAJOR-4). A secret travels as a `String`, so there is nothing to
    /// preserve here: the honest thing is to say so. Carries only the
    /// connection's name (rule 10).
    #[error(
        "the secret for connection «{conn}» ({origin}) is not valid UTF-8: rewrite it, or store it in the keyring or in secrets.age"
    )]
    SecretNotUtf8 {
        /// Connection name (never the secret).
        conn: String,
        /// Which rung of the resolver it came from (today only the
        /// environment).
        origin: SecretOrigin,
    },
    /// Structural cause from the secret store (`secrets.age`). The message
    /// is STATIC by construction: it guarantees, by type, that the decrypted
    /// plaintext or the passphrase is never interpolated into an error that
    /// ends up in logs.
    #[error("secrets.age: {0}")]
    SecretStore(&'static str),
    /// I/O error (reading config / the secrets file).
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// SSH host key not on record on first contact (TOFU, ADR 0015 D). The
    /// core maps it 1:1 to the protocol's `Error::HostKeyUnknown`; the
    /// frontend shows the fingerprint and confirms with
    /// `connection.trust_host_key` before retrying.
    #[error("unknown host key for {host}:{port} ({algo} {fingerprint}); confirm before connecting")]
    HostKeyUnknown {
        /// Bare host (no port).
        host: String,
        /// Already-resolved port (22 if the URL does not carry one).
        port: u16,
        /// Algorithm of the presented key (e.g. `ssh-ed25519`).
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint of the presented key.
        fingerprint: String,
    },
    /// The host key CHANGED from the one on record: possible MITM. Never
    /// accepted silently (ADR 0015 D).
    #[error("the host key for {host}:{port} CHANGED ({algo} {fingerprint}): possible MITM")]
    HostKeyMismatch {
        /// Bare host (no port).
        host: String,
        /// Already-resolved port.
        port: u16,
        /// Algorithm of the presented key.
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint of the PRESENTED key.
        fingerprint: String,
    },
    /// The server rejected authentication. Carries only user/host, never
    /// secret material (rule 10).
    #[error("authentication rejected for {user}@{host}")]
    AuthFailed {
        /// User the attempt was made as.
        user: String,
        /// Destination host.
        host: String,
    },
    /// Client key of an unsupported algorithm. Only ed25519 (ADR 0015 E,
    /// closes #36/RUSTSEC-2023-0071): RSA is rejected unless the connection
    /// carries `allow_rsa = true` (ADR 0150).
    #[error(
        "key {} of type {algo}: only ed25519 is supported (generate one with `ssh-keygen -t ed25519`)",
        path.display()
    )]
    KeyUnsupported {
        /// Path of the rejected key.
        path: PathBuf,
        /// Detected algorithm (e.g. `ssh-rsa`).
        algo: String,
    },
    /// RSA key with a modulus below the minimum (#370), even with
    /// `allow_rsa = true`.
    ///
    /// **`allow_rsa` does not lift this, and that is the fix.** ADR 0150 buys
    /// ONE risk, named and bounded: the timing side channel of
    /// RUSTSEC-2023-0071 in the `rsa` crate's private-key operations. That
    /// risk is the same at 1024 bits as at 4096. A 1024-bit modulus is a
    /// DIFFERENT risk — classic cryptographic weakness, not a side channel —
    /// which the ADR does not mention, so whoever signed off on the opt-in
    /// did not accept it: it was riding along silently.
    ///
    /// NIST SP 800-57 retired 1024 in 2013 and RFC 8332 §3 asks for 2048 as
    /// the minimum for `rsa-sha2-*`; OpenSSH has refused to generate them
    /// since 2017.
    #[error(
        "key {} has a {bits}-bit RSA modulus and at least {minimo} are required: \
         ask the server administrator for a new key (`ssh-keygen -t ed25519`, or \
         `-t rsa -b 4096` if that server does not support anything else)",
        path.display()
    )]
    RsaTooSmall {
        /// Path of the rejected key.
        path: PathBuf,
        /// The bits it has.
        bits: usize,
        /// The bits required.
        minimo: usize,
    },
    /// RSA key allowed (`allow_rsa`), but the server only accepts `ssh-rsa`
    /// signatures with SHA-1. ADR 0150's opt-in opens up RSA, never SHA-1.
    #[error(
        "{host} only accepts RSA signatures with SHA-1 (`ssh-rsa`), which norte does not use; rsa-sha2 or an ed25519 key is required"
    )]
    RsaSha1Only {
        /// Destination host.
        host: String,
    },
    /// The client key could not be loaded (format, wrong passphrase…). The
    /// cause comes from russh and does not contain the passphrase.
    #[error("could not load key {}: {cause}", path.display())]
    KeyLoad {
        /// Path of the key.
        path: PathBuf,
        /// Cause (from russh; no secret material).
        cause: String,
    },
    /// SSH agent unavailable or with no usable identities. STATIC message:
    /// never interpolates material from the agent.
    #[error("SSH agent: {0}")]
    Agent(&'static str),
    /// The URL carries no user and the environment gives no way to guess one.
    #[error(
        "the connection does not specify a user (use user@host) and there is no $USER in the environment"
    )]
    MissingUser,
    /// SSH transport error (handshake, network, channel). russh's Display
    /// does not contain secrets.
    #[error("SSH: {0}")]
    Ssh(String),
    /// Our own `known_hosts` file could not be read/parsed.
    #[error("known_hosts: {0}")]
    KnownHosts(String),
    /// FTP transport error (control, network, protocol). The text comes
    /// SANITIZED (`redact_ftp_err`): response bodies are controlled by the
    /// server and could carry control chars or echo credentials — the login
    /// path does not even go through here (it goes to `AuthFailed`).
    #[error("FTP: {0}")]
    Ftp(String),
    /// FTPS TLS failed: the server does not offer it with `tls = "require"`,
    /// or its certificate does not validate against the roots (+ extra CA).
    /// Never degraded silently (ADR 0015 F).
    #[error("FTPS/TLS: {0}")]
    Tls(String),
    /// Object storage error (building the `Operator` or probing): network,
    /// missing bucket, config. The text only carries opendal's CATEGORY
    /// (`ErrorKind`), never the secret (rule 10, ADR 0016 K).
    #[error("s3: {0}")]
    S3(String),
}

// Conscious EXCEPTION to "russh's types do not cross the boundary" (ADR
// 0015 A): `russh::client::Handler` requires `type Error: From<russh::Error>`,
// so this impl is forced public surface. The content IS kept contained: it
// degrades to a String (russh's Display, no secret material).
impl From<russh::Error> for ConnectError {
    fn from(e: russh::Error) -> Self {
        Self::Ssh(e.to_string())
    }
}

impl ConnectError {
    /// The sentence that CAN cross the wire and end up on a screen, if there
    /// is one (#322).
    ///
    /// A connection failure used to reach the frontend as a category and
    /// nothing else: `permission denied`, indistinguishable from a wrong key,
    /// a mistyped passphrase or a bucket without permissions. The exact
    /// diagnosis — «the secret for «myconn» is set but EMPTY» — was written
    /// to the daemon's log and discarded. Worse: with the embedded CLI it WAS
    /// visible, because `tracing` goes out through the process's own stderr,
    /// so the same failure was diagnosable or not depending on the
    /// TRANSPORT.
    ///
    /// # Why this is an allowlist, and why it is short
    ///
    /// This sends text to someone's screen and, over the wire, to any
    /// client. Rule 10 does not distinguish between "a secret" and
    /// "something that contains a secret", so only the variants whose
    /// message is composed of fields WE set get through. Left out:
    ///
    /// - `Config`: wraps `toml`'s error, which echoes the offending line —
    ///   and that line can be the secret's. `doctor` already avoids this for
    ///   that reason.
    /// - `InvalidUrl`: a URL can carry `user:password@host`.
    /// - `Io`, `KeyLoad`, `KeyUnsupported`: carry PATHS, and `path.display()`
    ///   is a silently lossy conversion (rule 1).
    /// - `Ssh`, `Ftp`, `Tls`, `S3`, `KnownHosts`: free-form text from a
    ///   third-party library. The one from `s3` can carry the signed URL.
    ///
    /// The TOFU variants are not here because they do not need it: they
    /// travel 1:1 as typed variants with host, port, algorithm and
    /// fingerprint.
    ///
    /// The `match` is exhaustive on purpose: a new variant will not compile
    /// until someone decides whether its text may go out.
    #[must_use]
    #[expect(
        clippy::match_same_arms,
        reason = "`AuthFailed` stays silent for a different reason than the rest \
                  —its sentence interpolates the USER, not third-party text— and \
                  that comment is what needs re-reading when adding a variant"
    )]
    pub fn detalle_publico(&self) -> Option<String> {
        match self {
            Self::Secret { .. }
            | Self::SecretEmpty { .. }
            | Self::SecretNotUtf8 { .. }
            | Self::SecretStore(_)
            | Self::MissingUser
            | Self::Agent(_) => Some(self.to_string()),

            // `AuthFailed` DOES have a publishable reason —and it publishes
            // it, through its closed `reason`— but its Display is
            // "authentication rejected for {user}@{host}", i.e. the USER. The
            // core burns an `rsplit('@')` two files further away precisely so
            // the userinfo does not leak into `host`; returning it here would
            // undo that through the same notification. And nothing is lost:
            // the translated reason sentence already says everything this
            // one added.
            Self::AuthFailed { .. } => None,

            Self::Config(_)
            | Self::InvalidUrl(_)
            | Self::Io(_)
            | Self::KeyLoad { .. }
            | Self::KeyUnsupported { .. }
            // Carries a PATH, like its two neighbors above. The bits WOULD
            // cross fine, but the sentence that makes the failure actionable
            // is the one that says WHICH key, and without it it is not worth
            // crossing the wire.
            | Self::RsaTooSmall { .. }
            | Self::RsaSha1Only { .. }
            | Self::Ssh(_)
            | Self::KnownHosts(_)
            | Self::Ftp(_)
            | Self::Tls(_)
            | Self::S3(_)
            | Self::HostKeyUnknown { .. }
            | Self::HostKeyMismatch { .. } => None,
        }
    }
}

// Projection onto the protocol's taxonomy (spec §17.7): the core uses this
// so a connection failure can travel over the wire. The TOFU variants go 1:1
// (they carry host/port/algo/fingerprint for the
// `connection.trust_host_key` flow, ADR 0015 D); the rest degrade to the
// closest category — the detail stays in the core's log (ConnectError's
// Display), not on the wire.
impl From<ConnectError> for norte_proto::Error {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::HostKeyUnknown {
                host,
                port,
                algo,
                fingerprint,
            } => Self::HostKeyUnknown {
                host,
                port: Some(port),
                algo,
                fingerprint,
            },
            ConnectError::HostKeyMismatch {
                host,
                port,
                algo,
                fingerprint,
            } => Self::HostKeyMismatch {
                host,
                port: Some(port),
                algo,
                fingerprint,
            },
            // Credentials rejected or unresolvable / unusable key material:
            // the user cannot authenticate.
            ConnectError::AuthFailed { .. }
            | ConnectError::Secret { .. }
            | ConnectError::SecretEmpty { .. }
            | ConnectError::SecretNotUtf8 { .. }
            | ConnectError::KeyUnsupported { .. }
            | ConnectError::RsaTooSmall { .. }
            | ConnectError::RsaSha1Only { .. }
            | ConnectError::KeyLoad { .. } => Self::PermissionDenied,
            // NOTE (#325): `Error::SecretNeeded` is not produced here. It is
            // a QUESTION, not a failure, and needs the ENDPOINT in addition
            // to the name —a password dialog that does not say who it is
            // going to is not answerable—; the endpoint is known by
            // `establish`, in the core, not by this resolver. It is built
            // there (`connect::secret_needed`). The connection's URL/config
            // is not valid.
            ConnectError::InvalidUrl(_) | ConnectError::MissingUser | ConnectError::Config(_) => {
                Self::InvalidPath
            }
            // Transport: the network can be retried; a broken TLS
            // validation or known_hosts/secret-store CANNOT (retrying does
            // not fix them).
            ConnectError::Ssh(_) | ConnectError::Ftp(_) | ConnectError::S3(_) => {
                Self::ProviderUnavailable { retryable: true }
            }
            ConnectError::Tls(_)
            | ConnectError::KnownHosts(_)
            | ConnectError::Agent(_)
            | ConnectError::SecretStore(_) => Self::ProviderUnavailable { retryable: false },
            ConnectError::Io(_) => Self::Io { retryable: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TOFU variants cross 1:1 to the protocol (same fingerprint and
    /// RESOLVED port): that is what makes the error→trust mapping in the
    /// frontend unambiguous (ADR 0015 D).
    #[test]
    fn tofu_va_uno_a_uno_al_proto() {
        let e = ConnectError::HostKeyUnknown {
            host: "h".into(),
            port: 2222,
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
        };
        let p = norte_proto::Error::from(e);
        let norte_proto::Error::HostKeyUnknown {
            host,
            port,
            algo,
            fingerprint,
        } = p
        else {
            panic!("expected HostKeyUnknown, got {p:?}");
        };
        assert_eq!(host, "h");
        assert_eq!(port, Some(2222));
        assert_eq!(algo, "ssh-ed25519");
        assert_eq!(fingerprint, "SHA256:abc");
    }

    #[test]
    fn auth_degrada_a_permission_denied() {
        let e = ConnectError::AuthFailed {
            user: "u".into(),
            host: "h".into(),
        };
        assert!(matches!(
            norte_proto::Error::from(e),
            norte_proto::Error::PermissionDenied
        ));
    }

    /// #322 / rule 10: `AuthFailed` does NOT publish its sentence.
    ///
    /// Its `Display` is "authentication rejected for {user}@{host}", i.e.
    /// the USER. The core burns an `rsplit('@')` so the userinfo does not
    /// leak into the notification's `host` field; returning it here would
    /// undo that through the same notification, and its closed `reason`
    /// already says the same thing.
    #[test]
    fn el_usuario_no_sale_en_el_detalle_de_un_rechazo() {
        let e = ConnectError::AuthFailed {
            user: "alice".into(),
            host: "servidor.example".into(),
        };
        assert!(
            e.to_string().contains("alice@"),
            "the internal message is still useful in the log"
        );
        assert_eq!(
            e.detalle_publico(),
            None,
            "but it does not cross the wire: {e}"
        );
    }

    /// No publishable sentence INTERPOLATES anything shaped like userinfo.
    ///
    /// Structural, not per-variant: `@` is the shape userinfo takes, and the
    /// claim needs to stay true when someone adds variant number twenty. The
    /// fields carry sentinels so that, if a future sentence joins them with
    /// an `@`, the `@` shows up.
    ///
    /// `MissingUser` is excluded, and it is the exception that proves the
    /// rule: its sentence carries a LITERAL `user@host`, as an example of
    /// what to type, and interpolates nothing — it has no fields. What this
    /// test is after is interpolated data, not the `@` character itself.
    #[test]
    fn ninguna_frase_publicable_interpola_userinfo() {
        const USUARIO: &str = "CENTINELA-USUARIO";
        let publicables = [
            ConnectError::Secret {
                conn: USUARIO.into(),
            },
            ConnectError::SecretEmpty {
                conn: USUARIO.into(),
                origin: SecretOrigin::Env,
            },
            ConnectError::SecretNotUtf8 {
                conn: USUARIO.into(),
                origin: SecretOrigin::Env,
            },
            ConnectError::SecretStore("the store did not open"),
            ConnectError::Agent("the agent is not responding"),
        ];
        for e in &publicables {
            let d = e.detalle_publico().expect("this variant publishes");
            assert!(
                !d.contains(&format!("{USUARIO}@")) && !d.contains(&format!("@{USUARIO}")),
                "a publishable sentence interpolates something shaped like userinfo: {d}"
            );
        }
        assert_eq!(
            ConnectError::MissingUser.detalle_publico().as_deref(),
            Some(
                "the connection does not specify a user (use user@host) and there is no $USER in the environment"
            ),
            "its `user@host` is LITERAL: if someone adds fields to it, this assert will say so"
        );
    }
}
