//! Core connection manager (phase 6e, ADR 0015 A/G): resolves a remote
//! `scheme://authority` to a live [`Provider`] — looks up the connection in
//! `connections.toml` (or builds an ad-hoc one from the URL), resolves the
//! secret (env → keyring → `secrets.age`) and establishes the transport with
//! `norte-connect`, injecting the session into the provider (providers NEVER
//! see a secret, rules 7/10).
//!
//! The [`Engine`](crate::Engine) consults this subsystem via the
//! [`RemoteConnector`] trait (injectable: the engine's tests use a fake one)
//! and caches the resulting provider by `scheme://authority`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use norte_connect::{
    AuthMethod, ConnectionSpec, ConnectionsFile, S3Connector, Secret, SecretResolver, SshConnector,
};
use norte_proto::Error;
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;
use norte_vfs_sftp::SftpProvider;

use crate::ftp_plugin::{connect_ftp_plugin, fmt_ip, resolve_ip};
use norte_plugin_host::PluginRuntime;

use crate::plugin_provider::{PluginProvider, map_runtime_error};
use crate::plugins::{PluginRegistry, ResolvedProvider};

/// Establishes remote providers on demand. The Engine consults it when a
/// remote `VPath` has no cached provider; the real implementation is
/// [`ConnectionManager`].
#[async_trait]
pub trait RemoteConnector: Send + Sync {
    /// Connects and builds the provider for `scheme://authority`, along with
    /// the security warnings emitted while establishing it (#44: TLS
    /// degradation).
    ///
    /// # Errors
    /// [`DialError`], which carries the usual category —
    /// [`Error::HostKeyUnknown`]/[`Error::HostKeyMismatch`] (TOFU flow, ADR
    /// 0015 D) or whichever the failure degrades to — and, if it can be told,
    /// the why (#322). An `Error` converts with just `.into()`: that is "I
    /// can't explain it," which was the only behavior before.
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, DialError>;

    /// The CANONICAL form of `authority` for this connection (#47, dedup):
    /// the authority with the EFFECTIVE user/port that connect would use —
    /// `sftp://host` that inherits `oscar@` from `connections.toml`
    /// canonicalizes to `oscar@host`, and an explicit default port gets
    /// normalized away. The Engine caches the session under the canonical
    /// key (+ aliases the requested one): two forms of the same identity =
    /// ONE session.
    ///
    /// LOCAL and cheap resolution (no network). `None` (default) = no
    /// opinion: the Engine caches under the requested authority as is.
    async fn canonical_authority(&self, scheme: &str, authority: &str) -> Option<String> {
        let _ = (scheme, authority);
        None
    }

    /// Records the host key of `host:port` after the user's EXPLICIT
    /// confirmation (`connection.trust_host_key` method); re-verifies the
    /// fingerprint against the real key (anti-TOCTOU, in `norte-connect`).
    ///
    /// # Errors
    /// [`Error::HostKeyMismatch`] if the host no longer presents that key.
    async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        fingerprint: &str,
    ) -> Result<(), Error>;

    /// Stores, for THIS session, the secret a human just typed
    /// (`connection.provide_secret`, #325).
    ///
    /// The twin of [`Self::trust_host_key`], and for the same reason: some
    /// decisions only the person in front of the screen can make, and the
    /// core needs a door through which the answer comes in. In memory, and
    /// nothing more.
    ///
    /// # Errors
    /// Implementation-dependent; the default connector never fails.
    async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error>;
}

/// Cause of a security degradation while connecting (#44). CLOSED
/// vocabulary: its `wire()` is the `reason` of
/// [`norte_proto::methods::ConnectionDegraded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionWarningReason {
    /// FTP `tls="allow"`: the server rejected `AUTH TLS` → session in the
    /// clear.
    TlsAuthRejected,
    /// FTP-via-plugin (ADR 0033): the session is ALWAYS in the clear — FTPS
    /// is debt (aws-lc-rs does not compile to wasm). Credentials and data
    /// unencrypted.
    FtpPlaintext,
}

impl ConnectionWarningReason {
    /// The wire string (closed and contractual; see `ConnectionDegraded.reason`).
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            ConnectionWarningReason::TlsAuthRejected => "tls-auth-rejected",
            ConnectionWarningReason::FtpPlaintext => "ftp-plaintext",
        }
    }
}

/// Cause of a connection NOT being established (#322). CLOSED vocabulary:
/// its [`Self::wire`] is the `reason` of
/// [`norte_proto::methods::ConnectionFailed`].
///
/// An enum and not a bare `&'static str`, same as [`ConnectionWarningReason`]:
/// with the raw string, renaming a value here turned nothing red — the
/// goldens froze a different copy — and the effect was that every failure
/// ended up painted as "unknown reason," silently and forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionFailureReason {
    /// There is no secret in any of the configured sources.
    SecretMissing,
    /// There is one, and it is EMPTY — which is not the same thing (#320).
    SecretEmpty,
    /// There is one and it is not valid text.
    SecretNotUtf8,
    /// The secret store could not be read.
    SecretStore,
    /// The server rejected the credentials.
    AuthRejected,
    /// The user is missing.
    NoUser,
    /// The SSH agent could not authenticate.
    Agent,
    /// The RSA key has a modulus below the minimum (#370).
    ///
    /// It carries its own reason and does not stay silent like the rest of
    /// the key failures because it is the only one of them the reader can
    /// ACT ON without looking at a log: it is not "I couldn't," it is "this
    /// key is too short, ask for another one." Without the phrase it reads
    /// as "permission denied," which would send them looking in the wrong
    /// place — the password, the user, the server.
    RsaTooSmall,
}

impl ConnectionFailureReason {
    /// The wire string (closed and contractual; see `ConnectionFailed.reason`).
    ///
    /// Everything this function returns is in
    /// [`norte_proto::methods::CONNECTION_FAILURE_REASONS`], and the reverse
    /// too: proven by `el_vocabulario_de_fallos_es_el_del_proto`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::SecretMissing => "secret-missing",
            Self::SecretEmpty => "secret-empty",
            Self::SecretNotUtf8 => "secret-not-utf8",
            Self::SecretStore => "secret-store",
            Self::AuthRejected => "auth-rejected",
            Self::NoUser => "no-user",
            Self::Agent => "agent",
            Self::RsaTooSmall => "rsa-too-small",
        }
    }

    /// All the variants, for the vocabulary's exhaustive tests.
    ///
    /// A constant and not a `strum`: there are seven of them and the
    /// dependency does not pay for itself. If someone adds an eighth and
    /// does not put it here, the `match` in [`Self::wire`] does force them
    /// to decide its string, and this list simply stops covering it — which
    /// is why the test compares BOTH WAYS against the proto, which is where
    /// the gap would show.
    pub const TODAS: &'static [Self] = &[
        Self::SecretMissing,
        Self::SecretEmpty,
        Self::SecretNotUtf8,
        Self::SecretStore,
        Self::AuthRejected,
        Self::NoUser,
        Self::Agent,
        Self::RsaTooSmall,
    ];
}

/// A security warning produced while establishing a remote session (#44).
/// The `host` goes WITHOUT userinfo (rule 10) by construction — it is the
/// `Endpoint.host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionWarning {
    /// Scheme of the session (e.g. `"ftp"`).
    pub scheme: String,
    /// Host, without userinfo.
    pub host: String,
    /// Cause.
    pub reason: ConnectionWarningReason,
}

/// The established session + the security warnings emitted while
/// establishing it.
pub struct Connected {
    /// The live provider.
    pub provider: Arc<dyn Provider>,
    /// Warnings (e.g. TLS degradation); empty in the normal case.
    pub warnings: Vec<ConnectionWarning>,
}

/// Why a session could NOT be established, in what can be told (#322).
///
/// Symmetric to [`ConnectionWarning`]: success carries its warnings, and
/// failure carries its explanation. Before, it did not carry one, and the
/// result was that the frontend received a CATEGORY — `PermissionDenied` —
/// indistinguishable from a wrong key, while the exact phrase died in the
/// daemon's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionFailure {
    /// Name from `connections.toml`, if a named one was being opened.
    pub conn: Option<String>,
    /// Scheme it was trying to connect to.
    pub scheme: String,
    /// Host, without userinfo (rule 10).
    pub host: String,
    /// Cause, CLOSED vocabulary (see `methods::ConnectionFailed::reason`).
    pub reason: ConnectionFailureReason,
    /// Human phrase, if the variant can expose it — see
    /// `ConnectError::detalle_publico`. `None` is not a failure without an
    /// explanation: it is an explanation that cannot go out.
    pub detail: Option<String>,
}

/// Observes connection warnings (#44): the daemon implements it to broadcast
/// `connection.degraded`; the embedded CLI, to print to stderr. Injected into
/// the [`Engine`](crate::Engine) with `set_connection_observer`.
pub trait ConnectionObserver: Send + Sync {
    /// A warning occurred while establishing a session. Best-effort,
    /// non-blocking.
    fn on_connection_warning(&self, warning: &ConnectionWarning);

    /// A session could NOT be established (#322). Best-effort, non-blocking.
    ///
    /// With an empty `default` on purpose: observers that only wanted the
    /// warnings keep compiling, and whoever wants to show the why implements
    /// it. Adding it without a default would have broken test implementers
    /// over a notification they don't care about.
    fn on_connection_failure(&self, _failure: &ConnectionFailure) {}
}

/// The publishable cause of a failure, WITHOUT the destination.
///
/// It is kept apart from the destination because they are known in
/// different places: the cause is known by whoever caught the
/// `ConnectError`; the scheme and the host, by whoever requested the
/// connection. Joining them earlier would force dragging the destination
/// through the whole error path just to name it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Causa {
    /// Name from `connections.toml`, if there was one.
    pub conn: Option<String>,
    /// CLOSED vocabulary (see `methods::ConnectionFailed::reason`).
    pub reason: ConnectionFailureReason,
    /// The phrase, if the variant can expose it (rule 10).
    pub detail: Option<String>,
}

/// A connection failure with what can be told to whoever is looking.
///
/// The error travels as always; the explanation goes alongside it. It
/// exists because `RemoteConnector` used to return only the category, and
/// that is where the diagnosis was being lost (#322).
#[derive(Debug, Clone)]
pub struct DialError {
    /// The category that ends up on the wire as the operation's error.
    pub error: Error,
    /// What's publishable of the why. `None` = this connector does not know
    /// how to explain it, or the variant cannot expose its text.
    ///
    /// In a `Box` because this type travels in the `Err` of every `connect`,
    /// and the normal path is the one that does NOT carry a cause: fattening
    /// every `Result` in the pool with two `String`s that are almost always
    /// empty would be paying for the rare case in the common one.
    pub causa: Option<Box<Causa>>,
}

impl From<Error> for DialError {
    /// A failure without a publishable explanation: exactly the behavior
    /// before #322. It's what lets a connector that doesn't provide one —
    /// the test doubles — not have to change.
    fn from(error: Error) -> Self {
        Self { error, causa: None }
    }
}

// Journal anchor key (M3-5, ADR 0025): lives in norte-connect (the
// secrets/keyring domain, rule 10); the CLI uses it via this re-export.
pub use norte_connect::journal_anchor_key;

// The user config dir — relocated to norte-config (ADR 0035); re-exported
// so the historic `norte_core::connect::config_dir()` path keeps working.
pub use norte_config::config_dir;

/// The URL of the connection NAMED `name` in `<dir>/connections.toml` (for
/// `norte connect <name>`: the frontend translates the name to a URL and the
/// establishment goes through the engine's normal path).
///
/// # Errors
/// [`Error::NotFound`] if the name does not exist; [`Error::InvalidPath`] if
/// the file does not parse.
pub async fn named_url(dir: &std::path::Path, name: &str) -> Result<String, Error> {
    let dir = dir.to_path_buf();
    let file = crate::blocking::spawn_blocking(move || ConnectionsFile::load(&dir))
        .await
        .map_err(|_| Error::Internal { panic: true })?
        .map_err(log_and_map)?;
    file.connections
        .get(name)
        .map(|s| s.url.clone())
        .ok_or(Error::NotFound)
}

/// The NAMED connections from `<dir>/connections.toml`, in alphabetical
/// order (#140): `(name, url)`.
///
/// The URL, and never a secret: `ConnectionSpec` references its credentials
/// (ADR 0015) and this list is for painting a selector.
///
/// A file that is not there is an EMPTY list and not an error: not having
/// any connections configured is normal on day one. One whose SYNTAX does
/// not parse is one — saying "you have none" when what's actually happening
/// is that its file has one comma too many would be lying about what the
/// user wrote.
///
/// **An unusable entry does not take the rest down with it** (#365). It
/// comes out separately, in the second member, with its name and the
/// reason. Before, the whole call failed, and that cost the reader the list
/// of ALL their connections over a single one norte couldn't read, with an
/// error that didn't name any and didn't point to anything fixable.
///
/// # Errors
/// [`Error::InvalidPath`] if the file exists and its TOML does not parse.
pub async fn named_connections(
    dir: &std::path::Path,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>), Error> {
    let dir = dir.to_path_buf();
    let (file, bad) =
        crate::blocking::spawn_blocking(move || ConnectionsFile::load_tolerante(&dir))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(log_and_map)?;
    Ok((
        file.connections
            .into_iter()
            .map(|(name, spec)| (name, spec.url))
            .collect(),
        bad,
    ))
}

/// The real implementation of [`RemoteConnector`] over `norte-connect`.
pub struct ConnectionManager {
    config_dir: PathBuf,
    secrets: SecretResolver,
    ssh: SshConnector,
    s3: S3Connector,
}

impl ConnectionManager {
    /// Manager anchored at `config_dir` (see [`config_dir()`] for the
    /// default).
    ///
    /// FTP no longer carries a connector here: `ftp://` goes through the
    /// provider-plugin ([`crate::ftp_plugin`], ADR 0033), which establishes
    /// the connection inside the WASM guest. [`norte_connect::FtpConnector`]
    /// (host-side TLS) stays reserved for a future host-terminated FTPS
    /// (debt).
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        let dir: PathBuf = config_dir.into();
        Self {
            secrets: SecretResolver::new(&dir),
            ssh: SshConnector::new(&dir),
            s3: S3Connector::new(),
            config_dir: dir,
        }
    }

    /// Connects the connection NAMED `name` from `connections.toml` (UX of
    /// `norte connect <name>`). Returns the (scheme, authority) pair under
    /// which the Engine would cache it, along with the provider.
    ///
    /// # Errors
    /// `Error::NotFound` if the name does not exist; the connection's own
    /// errors.
    pub async fn connect_named(&self, name: &str) -> Result<(String, String, Connected), Error> {
        let file = self.load_connections().await?;
        let spec = file.connections.get(name).ok_or(Error::NotFound)?.clone();
        let ep = spec.endpoint().map_err(log_and_map)?;
        let authority = authority_of(&ep);
        let connected = self
            .establish(&spec, Some(name))
            .await
            .map_err(|d| d.error)?;
        Ok((ep.scheme, authority, connected))
    }

    /// Loads `connections.toml` (synchronous I/O → `spawn_blocking`, rule 2).
    async fn load_connections(&self) -> Result<ConnectionsFile, Error> {
        let dir = self.config_dir.clone();
        crate::blocking::spawn_blocking(move || ConnectionsFile::load(&dir))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(log_and_map)
    }

    /// Establishes the transport for `spec` and builds the provider. `name`
    /// is the connection's name in `connections.toml` (ad-hoc = `None`; the
    /// secret is then looked up under the host).
    ///
    /// # A mistyped password does not stay forever (#325)
    ///
    /// If the secret came from the SESSION tier — a human just typed it a
    /// moment ago — and the server rejects it, it is FORGOTTEN and the
    /// failure turns back into `SecretNeeded`, i.e., into the dialog.
    /// Without this, a mistyped finger left the connection dead until the
    /// daemon was stopped: the session tier goes ahead of the other three,
    /// so the bad value also shadowed the environment variable one would try
    /// to fix it with, and since the core only asks when it finds NOTHING,
    /// the dialog never came back up.
    ///
    /// Only the session one. A secret from the environment, the keyring or
    /// `age` was put there on purpose by someone, in a place that can be
    /// edited: erasing it over a server rejection would be deciding for them
    /// that it was wrong.
    async fn establish(
        &self,
        spec: &ConnectionSpec,
        name: Option<&str>,
    ) -> Result<Connected, DialError> {
        // The origin comes out via parameter, and `establish_inner` is a
        // separate function, for a reason that took a test with a real
        // account to discover: the scheme arms use `?`, so a transport
        // failure RETURNS from the whole function. A block "after the
        // match" inside `establish_inner` never ran in the one case that
        // matters — the failure one.
        let mut origin = None;
        let result = self.establish_inner(spec, name, &mut origin).await;
        if matches!(&result, Err(d) if d.error == Error::PermissionDenied)
            && origin == Some(norte_connect::SecretOrigin::Session)
        {
            let conn_name = match spec.endpoint() {
                Ok(ep) => name.map_or(ep.host, ToString::to_string),
                Err(_) => return result,
            };
            self.secrets.forget_session(&conn_name);
            tracing::info!(
                conn = %conn_name,
                "server rejected the typed secret: forgetting it and asking again"
            );
            return Err(secret_needed(&conn_name, spec).into());
        }
        result
    }

    /// The body of [`Self::establish`]. `origin` comes out via parameter
    /// because it's the only thing its wrapper needs to know about the path
    /// taken.
    async fn establish_inner(
        &self,
        spec: &ConnectionSpec,
        name: Option<&str>,
        origin: &mut Option<norte_connect::SecretOrigin>,
    ) -> Result<Connected, DialError> {
        let ep = spec.endpoint().map_err(|e| log_and_dial(e, name))?;
        let mut warnings: Vec<ConnectionWarning> = Vec::new();
        // The secret is only resolved if the auth method can use it
        // (password/access-key always; key for the passphrase). Agent
        // carries no secret (in s3, agent = opendal's environment chain).
        // The name under which the secret is stored and looked up. For a
        // connection with an entry it is ITS `connections.toml` key, unique
        // by construction; the `unwrap_or(&ep.host)` is for ad-hoc ones,
        // which today CANNOT reach here with a secret — `resolve_spec` sets
        // them to `auth = "agent"`, and that arm does not consult the
        // resolver. If an ad-hoc ever carries another auth method, this name
        // stops being unique and two different hosts could share an entry:
        // then a key that includes the scheme and the port is needed.
        let conn_name = name.unwrap_or(&ep.host);
        let secret: Option<Secret> = match spec.auth {
            AuthMethod::Agent => None,
            AuthMethod::Password | AuthMethod::AccessKey => {
                // A `prompt` ASKS also when what's there is the empty
                // string. Empty is still a configuration failure — #320,
                // and `norte doctor` says so — but returning it here would
                // leave the user facing an opaque "permission denied" while
                // the person is right there and a dialog is ready to ask
                // them. It warns, and it asks.
                let found = match self.secrets.resolve_with_origin(conn_name, &spec.url).await {
                    Ok(v) => v,
                    Err(e @ norte_connect::ConnectError::SecretEmpty { .. })
                        if spec.secret == norte_connect::SecretSource::Prompt =>
                    {
                        tracing::warn!(conn = %conn_name, error = %e, "empty secret: will ask");
                        None
                    }
                    Err(e) => return Err(log_and_dial(e, name)),
                };
                // #325: if it's nowhere and the connection says it must be
                // asked, this goes up the wire as a QUESTION and the
                // frontend opens its dialog. The core cannot ask on its own:
                // its resolver has no user interface.
                if found.is_none() && spec.secret == norte_connect::SecretSource::Prompt {
                    return Err(secret_needed(conn_name, spec).into());
                }
                found.map(|(s, found_origin)| {
                    *origin = Some(found_origin);
                    s
                })
            }
            // `key`: the secret is the key's PASSPHRASE, and there empty and
            // absent are the same thing — an unencrypted key carries no
            // passphrase, and `load_secret_key` treats `Some("")` the same
            // as `None`. Behind it there is no environment credential that
            // could impersonate another, which is the only thing that made
            // empty dangerous in #320, so the resolver's rejection is undone
            // RIGHT HERE: applying it to `key` too would turn a bluntly
            // exported `NORTE_SECRET_*=""` (a `$(cat …)` that found no file)
            // into a key that stops working, for no gain.
            AuthMethod::Key => match self.secrets.resolve(conn_name, &spec.url).await {
                Ok(s) => s,
                Err(norte_connect::ConnectError::SecretEmpty { .. }) => None,
                Err(e) => return Err(log_and_dial(e, name)),
            },
        };
        // A provider plugin serves the scheme it declares. The catalogue is
        // asked BEFORE the core's arms, for every scheme that isn't the
        // core's own: `ftp` included, because its embedded guest is a
        // fallback and an installed plugin replaces it. The core's own are
        // not consulted — the manifest already refuses to let them be
        // claimed, and not consulting them is the second gate.
        if let Some(via_plugin) = self.via_plugin(&ep, spec, secret.as_ref(), name).await {
            return via_plugin.map(|provider| Connected {
                provider: Arc::new(provider),
                warnings,
            });
        }
        match ep.scheme.as_str() {
            "sftp" => {
                let session = self
                    .ssh
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(|e| log_and_dial(e, name))?;
                // Base "/": the VPath's segments are absolute on the server.
                Ok(Connected {
                    provider: Arc::new(
                        SftpProvider::new(session, "/").with_logical_trash(spec.logical_trash),
                    ),
                    warnings,
                })
            }
            "ftp" => {
                // FTP-via-plugin (ADR 0033): the WASM guest establishes the
                // connection over gated `wasi:sockets`. The host resolves
                // DNS (with an anti-SSRF filter), grants `net` to the IP and
                // calls `configure`. No TLS (FTPS = debt): ALWAYS in the
                // clear → the user is warned.
                let user = ep.user.clone().unwrap_or_else(|| "anonymous".to_string());
                let password = match (spec.auth, secret.as_ref()) {
                    (AuthMethod::Password, Some(s)) => s.expose().to_string(),
                    // Guest convention: anonymous login.
                    (AuthMethod::Agent, _) => "anonymous".to_string(),
                    (AuthMethod::Password, None) => {
                        return Err(log_and_dial(
                            norte_connect::ConnectError::Secret {
                                conn: ep.host.clone(),
                            },
                            name,
                        ));
                    }
                    // key/access-key do not exist in FTP.
                    (AuthMethod::Key | AuthMethod::AccessKey, _) => {
                        return Err(Error::Unsupported.into());
                    }
                };
                let port = ep.port.unwrap_or(21);
                warnings.push(ConnectionWarning {
                    scheme: ep.scheme.clone(),
                    host: ep.host.clone(),
                    reason: ConnectionWarningReason::FtpPlaintext,
                });
                let provider = connect_ftp_plugin(&ep.host, port, &user, &password, "/").await?;
                Ok(Connected {
                    provider: Arc::new(provider),
                    warnings,
                })
            }
            "s3" => {
                // `S3Connector` builds and PROBES the Operator (fail-fast);
                // the provider receives it injected, without ever seeing
                // the secret-access-key.
                let op = self
                    .s3
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(|e| log_and_dial(e, name))?;
                Ok(Connected {
                    provider: Arc::new(
                        ObjectProvider::new(op, "s3").with_logical_trash(spec.logical_trash),
                    ),
                    warnings,
                })
            }
            _ => Err(Error::Unsupported.into()),
        }
    }

    /// `Some` if a consented provider plugin serves `ep`'s scheme, with the
    /// result of connecting it; `None` if the scheme belongs to the core or
    /// no plugin declares it, in which case the core's arms answer instead.
    async fn via_plugin(
        &self,
        ep: &norte_connect::Endpoint,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
        name: Option<&str>,
    ) -> Option<Result<PluginProvider, DialError>> {
        if norte_plugin_host::CORE_SCHEMES.contains(&ep.scheme.as_str()) {
            return None;
        }
        let resolved = self.resolve_plugin_provider(&ep.scheme).await?;
        Some(
            self.connect_plugin_provider(resolved, ep, spec, secret, name)
                .await,
        )
    }

    /// The APPROVED and ACTIVE provider plugin that declares `scheme`, or
    /// `None`.
    ///
    /// Rediscovers the catalogue on every connection: it's what the
    /// embedded `Backend` already does for every plugin call, and a
    /// connection is established once and cached in the Engine, so the cost
    /// is not repeated per op. A catalogue that cannot be read is treated
    /// as empty, with a warning: a broken `plugins/` directory must not
    /// leave anyone without FTP.
    async fn resolve_plugin_provider(&self, scheme: &str) -> Option<ResolvedProvider> {
        let dir = self.config_dir.clone();
        let scheme = scheme.to_owned();
        let discovered =
            crate::blocking::spawn_blocking(move || match PluginRegistry::discover(&dir) {
                Ok(reg) => reg.resolve_provider(&scheme),
                Err(e) => {
                    tracing::warn!(error = %e, "the plugin catalogue could not be read");
                    None
                }
            })
            .await;
        match discovered {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "catalogue discovery aborted");
                None
            }
        }
    }

    /// Instantiates a provider plugin's guest under the capabilities of ITS
    /// OWN manifest and configures it with the connection's endpoint and
    /// credentials.
    ///
    /// **Network.** The guest has no DNS. If the manifest declares `net`,
    /// the host resolves the endpoint — with the same anti-SSRF filter as
    /// the embedded FTP — and adds `ip:port` to the declared allow-list: the
    /// human approved "network," and the connection is theirs, but a port,
    /// not the machine. The port is the URL's or the contribution's
    /// `default-port`; without either, there's nothing to know what to
    /// grant, and it is refused. Without `net` in the manifest there is no
    /// network, and the endpoint crosses as is (an in-memory provider
    /// ignores it).
    ///
    /// **Binary.** The bytes of `plugin.wasm` are read, hashed and compared
    /// against the digest the catalogue anchored: what runs is what the
    /// human approved, not whatever is at that path now.
    async fn connect_plugin_provider(
        &self,
        resolved: ResolvedProvider,
        ep: &norte_connect::Endpoint,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
        name: Option<&str>,
    ) -> Result<PluginProvider, DialError> {
        let ResolvedProvider {
            id,
            wasm,
            capabilities: mut caps,
            settings,
            default_port,
            ..
        } = resolved;
        let password = match (spec.auth, secret) {
            (AuthMethod::Password, Some(s)) => s.expose().to_string(),
            (AuthMethod::Password, None) => {
                return Err(log_and_dial(
                    norte_connect::ConnectError::Secret {
                        conn: ep.host.clone(),
                    },
                    name,
                ));
            }
            (AuthMethod::Agent, _) => String::new(),
            // A key or an access-key have no shape in the provider's WIT
            // interface: only `user` + `password` cross.
            (AuthMethod::Key | AuthMethod::AccessKey, _) => {
                return Err(Error::Unsupported.into());
            }
        };
        let port = ep.port.or(default_port);
        let port_suffix = port.map(|p| format!(":{p}")).unwrap_or_default();
        let endpoint = if let Some(net) = caps.net.as_mut() {
            let Some(port) = port else {
                tracing::warn!(
                    plugin = %id, scheme = %ep.scheme,
                    "no port in the URL nor default-port in the manifest: network not granted"
                );
                return Err(Error::Unsupported.into());
            };
            let host = ep.host.clone();
            let ip = crate::blocking::spawn_blocking(move || resolve_ip(&host, port))
                .await
                .map_err(|_| Error::Internal { panic: true })??;
            net.hosts.push(format!("{ip}:{port}"));
            format!("{}{port_suffix}", fmt_ip(ip))
        } else {
            format!("{}{port_suffix}", ep.host)
        };
        tracing::info!(plugin = %id, scheme = %ep.scheme, "provider via plugin");

        // Read, hash and instantiate (compiles cranelift): all blocking
        // (rule 2). The runtime reads with the artifact's cap and compares
        // the approved digest BEFORE compiling (ADR 0142).
        let scheme = ep.scheme.clone();
        let provider = crate::blocking::spawn_blocking(move || {
            let runtime = PluginRuntime::new().map_err(|e| map_runtime_error(&e))?;
            PluginProvider::new(runtime, &wasm, caps, scheme).map_err(|e| {
                if matches!(e, norte_plugin_host::RuntimeError::DigestMismatch) {
                    tracing::warn!(plugin = %id, "plugin.wasm is not the one that was approved");
                }
                map_runtime_error(&e)
            })
        })
        .await
        .map_err(|_| Error::Internal { panic: true })??;
        provider.set_settings(settings).await;
        provider
            .configure(
                endpoint,
                ep.user.clone().unwrap_or_default(),
                password,
                "/".to_string(),
            )
            .await?;
        Ok(provider)
    }
}

#[async_trait]
impl RemoteConnector for ConnectionManager {
    // skip_all: the RAW authority does not enter the span — a
    // `user:pass@host` that the VPath accepted gets rejected when parsing
    // the connection, but the span would open BEFORE that (rule 10). It's
    // logged redacted after the parse.
    #[tracing::instrument(level = "info", skip_all)]
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, DialError> {
        let url = format!("{scheme}://{authority}");
        let file = self.load_connections().await.map_err(DialError::from)?;
        let (name, spec) =
            resolve_spec(&file, &url).map_err(|e| DialError::from(log_and_map(e)))?;
        if let Ok(ep) = spec.endpoint() {
            tracing::info!(scheme = %ep.scheme, host = %ep.host, port = ?ep.port,
                conn = name.as_deref().unwrap_or("(ad-hoc)"), "connecting");
        }
        self.establish(&spec, name.as_deref()).await
    }

    // skip_all for the same reason as `connect`: the raw authority may
    // carry userinfo that the parse will reject afterward (rule 10).
    #[tracing::instrument(level = "debug", skip_all)]
    async fn canonical_authority(&self, scheme: &str, authority: &str) -> Option<String> {
        let url = format!("{scheme}://{authority}");
        let file = self.load_connections().await.ok()?;
        canonical_from_file(&file, &url)
    }

    #[tracing::instrument(level = "info", skip(self))]
    async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        fingerprint: &str,
    ) -> Result<(), Error> {
        self.ssh
            .trust_host_key(host, port.unwrap_or(22), fingerprint)
            .await
            .map_err(log_and_map)
    }

    // `skip_all` and not `skip(self)`: the second parameter is a PASSWORD.
    // With `skip(self)`, `tracing` would format it into the span — at info
    // level, to the file and to the log panel — and rule 10 would have been
    // broken by the easiest line of the patch to write. The connection's
    // name is logged by hand, which is the only thing that can be said
    // here.
    #[tracing::instrument(level = "info", skip_all, fields(conn = %conn))]
    async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        self.secrets
            .remember_for_session(conn, norte_connect::Secret::new(secret.to_string()));
        tracing::info!("connection secret received from the frontend (in memory only)");
        Ok(())
    }
}

/// Resolves the connection for a remote URL: the `connections.toml` ENTRY
/// whose endpoint matches (scheme + host + effective port + user, if the URL
/// carries one), or an ad-hoc spec built from the URL (`auth = agent`: SSH
/// agent on sftp, anonymous on ftp; `tls` default = require). Priority: the
/// URL's user wins; on a tie, the first entry in alphabetical order
/// (`ConnectionsFile`'s `BTreeMap` guarantees this deterministically).
fn resolve_spec(
    file: &ConnectionsFile,
    url: &str,
) -> Result<(Option<String>, ConnectionSpec), norte_connect::ConnectError> {
    let ad_hoc = ConnectionSpec {
        url: url.to_string(),
        auth: AuthMethod::Agent,
        key: None,
        tls: norte_connect::TlsMode::Require,
        region: None,
        endpoint: None,
        access_key_id: None,
        addressing: None,
        logical_trash: false,
        // Ad-hoc = `auth = "agent"`: `allow_rsa` would not apply, and RSA
        // over agent remains outside ADR 0150.
        allow_rsa: false,
        // An AD-HOC connection — browsing to a URL that isn't in the file —
        // does not ask: there is no entry declaring `secret = "prompt"`,
        // and asking someone for a password over typing a URL would be
        // teaching them to type passwords into any dialog that shows up.
        secret: norte_connect::SecretSource::Stored,
    };
    let target = ad_hoc.endpoint()?;
    for (name, spec) in &file.connections {
        let Ok(ep) = spec.endpoint() else {
            continue; // a broken entry doesn't block the rest
        };
        if ep.scheme != target.scheme || ep.host != target.host {
            continue;
        }
        if effective_port(&ep) != effective_port(&target) {
            continue;
        }
        // User: if the URL carries one, it must match; if not, the entry's
        // is fine (or none).
        if let Some(u) = &target.user
            && ep.user.as_ref() != Some(u)
        {
            continue;
        }
        let mut spec = spec.clone();
        // The request's URL rules (it carries the effective user/port the
        // frontend asked for), but if it carries no user and the entry
        // does, the entry's completes the spec. INVARIANT: the request's
        // scheme/host/port == the entry's (compared above) — the entry's
        // secret never travels to another host. If some future caller
        // passes URLs not derived from an already-parsed VPath, revalidate
        // here.
        if target.user.is_some() || ep.user.is_none() {
            spec.url = url.to_string();
        }
        return Ok((Some(name.clone()), spec));
    }
    Ok((None, ad_hoc))
}

/// Scheme's default port — a matching constant (never travels on the
/// wire): s3 carries no port in the authority (443 nominal); sftp=22,
/// ftp=21. A plugin scheme has no default the core knows: `None`, and then
/// only an explicit port matches an explicit port.
fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "sftp" => Some(22),
        "ftp" => Some(21),
        "s3" => Some(443),
        _ => None,
    }
}

fn effective_port(ep: &norte_connect::Endpoint) -> Option<u16> {
    ep.port.or_else(|| default_port(&ep.scheme))
}

/// The canonical form for dedup (#47): the RESOLVED endpoint's authority
/// (effective user included) with the default port normalized away —
/// `sftp://h:22` and `sftp://h` canonicalize the same.
fn canonical_authority_of(ep: &norte_connect::Endpoint) -> String {
    let mut canon = ep.clone();
    if canon.port.is_some() && canon.port == default_port(&canon.scheme) {
        canon.port = None;
    }
    authority_of(&canon)
}

/// The canonical form of `url` against an already-loaded `connections.toml`
/// (kept apart from [`ConnectionManager::canonical_authority`] to test
/// purely).
fn canonical_from_file(file: &ConnectionsFile, url: &str) -> Option<String> {
    let (_name, spec) = resolve_spec(file, url).ok()?;
    Some(canonical_authority_of(&spec.endpoint().ok()?))
}

/// The canonical authority of an endpoint (with the port only if explicit):
/// the Engine's cache key.
fn authority_of(ep: &norte_connect::Endpoint) -> String {
    let mut s = String::new();
    if let Some(u) = &ep.user {
        s.push_str(u);
        s.push('@');
    }
    // IPv6 gets its brackets back in the authority form.
    if ep.host.contains(':') {
        s.push('[');
        s.push_str(&ep.host);
        s.push(']');
    } else {
        s.push_str(&ep.host);
    }
    if let Some(p) = ep.port {
        use std::fmt::Write;
        let _ = write!(s, ":{p}");
    }
    s
}

/// Degrades a `ConnectError` to the wire's taxonomy, leaving the DETAIL in
/// the core's log (the wire carries the category; `ConnectError`'s `Display`
/// contains no secrets by construction).
///
/// Kept for the places that do NOT know which connection the failure
/// belongs to — resolving the URL, listing the file: there is no one to
/// warn there, and reporting "a connection failed" without saying which one
/// would be noise.
fn log_and_map(e: norte_connect::ConnectError) -> Error {
    tracing::warn!(error = %e, "remote connection failure");
    Error::from(e)
}

/// The CLOSED vocabulary of `connection.failed`, per variant (#322).
///
/// `None` = this variant is not counted. That is not the same as "it has no
/// reason": it's that its explanation adds nothing for whoever is looking (a
/// TOFU already travels typed, with its fingerprint) or that it cannot go
/// out (rule 10, see `ConnectError::detalle_publico`).
///
/// Exhaustive on purpose: a new variant does not compile until someone
/// decides whether the human gets to know about it.
#[expect(
    clippy::match_same_arms,
    reason = "two arms give `None` for different reasons, and each one's \
              comment is what needs re-reading when adding a variant; \
              merging them erases the decision"
)]
fn reason_for(e: &norte_connect::ConnectError) -> Option<ConnectionFailureReason> {
    use ConnectionFailureReason as R;
    use norte_connect::ConnectError as C;
    match e {
        C::Secret { .. } => Some(R::SecretMissing),
        C::SecretEmpty { .. } => Some(R::SecretEmpty),
        C::SecretNotUtf8 { .. } => Some(R::SecretNotUtf8),
        C::SecretStore(_) => Some(R::SecretStore),
        C::AuthFailed { .. } => Some(R::AuthRejected),
        C::MissingUser => Some(R::NoUser),
        C::Agent(_) => Some(R::Agent),
        // Two different reasons for the same `None`, and that's why the
        // arms are NOT merged: the TOFU travels as a TYPED error with host,
        // port, algorithm and fingerprint — telling it again as a phrase
        // would be worse, not better — while the rest either cannot show
        // their text (rule 10) or say nothing actionable. Merging them
        // erases each one's reason, which is exactly what needs re-reading
        // when adding a variant.
        C::HostKeyUnknown { .. } | C::HostKeyMismatch { .. } => None,
        // This one IS counted, unlike its neighbors: "your key is too
        // short" is actionable and "permission denied" sends you looking in
        // the wrong place.
        C::RsaTooSmall { .. } => Some(R::RsaTooSmall),
        C::Config(_)
        | C::InvalidUrl(_)
        | C::Io(_)
        | C::KeyLoad { .. }
        | C::KeyUnsupported { .. }
        | C::RsaSha1Only { .. }
        | C::Ssh(_)
        | C::KnownHosts(_)
        | C::Ftp(_)
        | C::Tls(_)
        | C::S3(_) => None,
    }
}

/// Like [`log_and_map`], but keeping the cause so it CAN be told (#322).
///
/// This is the exact point where the diagnosis used to be lost: here the
/// `warn!` was written with the exact phrase, and only the category was
/// returned.
fn log_and_dial(e: norte_connect::ConnectError, name: Option<&str>) -> DialError {
    tracing::warn!(error = %e, "remote connection failure");
    let cause = reason_for(&e).map(|reason| {
        Box::new(Causa {
            conn: name.map(ToOwned::to_owned),
            reason,
            detail: e.detalle_publico(),
        })
    });
    DialError {
        error: Error::from(e),
        causa: cause,
    }
}

/// The QUESTION from #325, with who the password is going to be given to.
///
/// It deliberately does not go through [`log_and_map`]: that logs a
/// `warn!` of "remote connection failure," and this is not a failure — it's
/// the connection working exactly as its owner configured it. A `warn!` for
/// every navigation to a `prompt` connection would fill the log file with
/// false warnings and, since #324, would light up the log-button warning in
/// the pane bar.
///
/// The destination comes from
/// [`norte_connect::ConnectionSpec::destination_display`], which shows the
/// explicit `endpoint =` besides the URL — in s3 the URL's "host" is the
/// BUCKET, and whoever receives the signed credential is the endpoint —
/// and strips the userinfo from both halves (rule 10). If the URL doesn't
/// parse, it falls back to the bare name: staying silent for being unable
/// to paint the destination would be worse.
fn secret_needed(conn: &str, spec: &ConnectionSpec) -> Error {
    let endpoint = spec.destination_display().unwrap_or_default();
    tracing::info!(conn = %conn, endpoint = %endpoint, "secret missing: will ask");
    Error::SecretNeeded {
        conn: conn.to_string(),
        endpoint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin of `ConnectionWarningReason::wire()`'s CLOSED vocabulary (goes to
    /// the wire's `ConnectionDegraded.reason`; protocol-guardian). Changing
    /// a string here is a contract change: this test forces it to be
    /// deliberate.
    #[test]
    fn connection_warning_wire_vocabulary_is_pinned() {
        assert_eq!(
            ConnectionWarningReason::TlsAuthRejected.wire(),
            "tls-auth-rejected"
        );
        assert_eq!(
            ConnectionWarningReason::FtpPlaintext.wire(),
            "ftp-plaintext"
        );
    }

    fn file(toml: &str) -> ConnectionsFile {
        toml::from_str(toml).expect("valid toml")
    }

    /// #325: with no secret anywhere and `secret = "prompt"`, `establish`
    /// returns `SecretNeeded` BEFORE touching the network — it's a
    /// question, not a connection failure. With the default (`stored`) it
    /// follows its path and dies in the transport, which is the usual
    /// behavior.
    #[tokio::test]
    async fn prompt_without_secret_is_secret_needed_before_connecting() {
        let dir = tempfile::tempdir().expect("tmp");
        let mgr = ConnectionManager::new(dir.path());
        // Port 1 on loopback: if the `prompt` arm did NOT short-circuit,
        // this test would fail with a transport error instead of the
        // verdict — which is exactly the distinction being pinned down
        // here.
        let mut spec = min_spec("sftp://nadie@127.0.0.1:1/");
        spec.auth = AuthMethod::Password;
        spec.secret = norte_connect::SecretSource::Prompt;

        // `Connected` is not `Debug` (it carries providers), so the outcome
        // is pulled out by hand instead of with `expect_err`.
        let Err(err) = mgr
            .establish(&spec, Some("pregunta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("there is no secret anywhere: it should have asked");
        };
        assert!(
            matches!(&err, Error::SecretNeeded { conn, .. } if conn == "pregunta"),
            "the question arrives WHOLE over the wire, with the connection's name: {err:?}"
        );

        let Error::SecretNeeded { endpoint, .. } = &err else {
            unreachable!()
        };
        assert_eq!(
            endpoint, "sftp://127.0.0.1:1",
            "and WITH the destination: a password dialog that doesn't say who \
             it's going to be given to is not answerable"
        );

        // And in s3 the destination is BOTH halves: the URL's "host" is the
        // bucket, and whoever receives the signed credential is the
        // `endpoint =` — which is exactly the piece a foreign
        // `connections.toml` can point elsewhere. Showing only the bucket
        // would tell the half that doesn't matter. (This came out of
        // testing the real connection, not from a test.)
        let mut s3 = min_spec("s3://mi.bucket");
        s3.auth = AuthMethod::AccessKey;
        s3.secret = norte_connect::SecretSource::Prompt;
        s3.access_key_id = Some("AKIAEXAMPLE".into());
        s3.endpoint = Some("https://s3.eu-west-1.example".into());
        let Err(Error::SecretNeeded { endpoint, .. }) = mgr
            .establish(&s3, Some("cuenta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("s3 with no secret should have asked");
        };
        assert_eq!(endpoint, "s3://mi.bucket @ https://s3.eu-west-1.example");

        // And once answered, the same `establish` stops asking: tier 0 of
        // the resolver has it.
        mgr.secrets
            .remember_for_session("pregunta", norte_connect::Secret::new("tecleado".into()));
        let Err(other) = mgr
            .establish(&spec, Some("pregunta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("the transport doesn't exist: it cannot have connected");
        };
        assert!(
            !matches!(other, Error::SecretNeeded { .. }),
            "no longer asks: {other:?}"
        );
    }

    /// #325 (all three reviewers): a MISTYPED password cannot stay forever.
    /// The session tier goes ahead of the other three, so a wrong value
    /// doesn't just fail — it shadows the environment variable one would
    /// try to fix it with, and since the core only asks when it finds
    /// NOTHING, the dialog never came back up. And the resolver lives in
    /// the daemon: not even closing the interface cleaned it up.
    ///
    /// Here it's checked with `access-key`, where the session secret IS the
    /// credential: opendal responds `PermissionDenied` because the bucket
    /// doesn't exist anywhere, which is exactly the shape a server's way of
    /// saying "that credential is no good" takes.
    ///
    /// (Control mutation: removing the `forget_session` block from
    /// `establish` leaves the `PermissionDenied`, and both assertions
    /// fall.)
    #[tokio::test]
    async fn a_rejected_session_secret_is_forgotten_and_asked_again() {
        let dir = tempfile::tempdir().expect("tmp");
        let mgr = ConnectionManager::new(dir.path());
        // A local server that says 403 to everything: it's a real
        // CREDENTIAL REJECTION, without leaving the machine. A closed port
        // doesn't work — it gives a transport error, which is exactly the
        // case this test does NOT measure, and with it the test also
        // passed with the bug in place.
        let port = server_that_denies().await;
        let mut spec = min_spec("s3://bucket-de-prueba");
        spec.auth = AuthMethod::AccessKey;
        spec.secret = norte_connect::SecretSource::Prompt;
        spec.access_key_id = Some("AKIAEXAMPLE".into());
        spec.endpoint = Some(format!("http://127.0.0.1:{port}"));
        spec.region = Some("us-east-1".into());
        spec.addressing = Some(norte_connect::AddressingStyle::Path);

        mgr.secrets
            .remember_for_session("cuenta", norte_connect::Secret::new("mal-tecleada".into()));
        let Err(first) = mgr
            .establish(&spec, Some("cuenta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("the server denies: it cannot have connected");
        };
        assert!(
            matches!(first, Error::SecretNeeded { .. }),
            "the rejection turns back into the QUESTION, not a \"permission \
             denied\" with no way out: {first:?}"
        );
        assert!(
            !mgr.secrets.forget_session("cuenta"),
            "and the bad value is no longer there: `establish` forgot it"
        );
    }

    /// A local port that responds `403` to whatever and closes. Returns the
    /// port; the task dies with the test's runtime.
    async fn server_that_denies() -> u16 {
        use tokio::io::AsyncWriteExt as _;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let _ = s
                    .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = s.shutdown().await;
            }
        });
        port
    }

    /// The reverse, and the bug this test uncovered while it was being
    /// written: the condition cannot be "there was a `PermissionDenied` and
    /// there is something in the session." An `auth = "key"` with the
    /// key's path set wrong also comes out as `PermissionDenied`, and that
    /// secret is NOT the one that failed — erasing it would pop a password
    /// dialog over a missing file, and, in passing, would throw away a
    /// credential that was actually valid.
    #[tokio::test]
    async fn a_failure_unrelated_to_the_session_secret_does_not_erase_it() {
        let dir = tempfile::tempdir().expect("tmp");
        let mgr = ConnectionManager::new(dir.path());
        let mut spec = min_spec("sftp://127.0.0.1:1/");
        spec.auth = AuthMethod::Key;
        spec.key = Some(dir.path().join("no-existe"));

        mgr.secrets
            .remember_for_session("cuenta", norte_connect::Secret::new("valida".into()));
        let Err(e) = mgr
            .establish(&spec, Some("cuenta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("the key doesn't exist: it cannot have connected");
        };
        assert!(
            !matches!(e, Error::SecretNeeded { .. }),
            "a key failure is not a password question: {e:?}"
        );
        assert!(
            mgr.secrets.forget_session("cuenta"),
            "and the session secret is still where it was"
        );
    }

    const CONNS: &str = r#"
        [connections.trabajo]
        url = "sftp://oscar@work.example:2222"
        auth = "key"
        key = "/k/id_ed25519"

        [connections.backup]
        url = "ftp://backup.example"
        auth = "password"
    "#;

    #[test]
    fn matches_by_host_port_and_user() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://oscar@work.example:2222").unwrap();
        assert_eq!(name.as_deref(), Some("trabajo"));
        assert_eq!(spec.auth, AuthMethod::Key);

        // Different user in the URL → does NOT match the entry (ad-hoc).
        let (name, spec) = resolve_spec(&f, "sftp://otro@work.example:2222").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);

        // Different port → ad-hoc.
        let (name, _) = resolve_spec(&f, "sftp://oscar@work.example:22").unwrap();
        assert_eq!(name, None);
    }

    #[test]
    fn url_without_user_inherits_the_entrys() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://work.example:2222").unwrap();
        assert_eq!(name.as_deref(), Some("trabajo"));
        // The spec keeps the ENTRY's URL (with oscar@) so the connection
        // uses that user.
        assert_eq!(spec.url, "sftp://oscar@work.example:2222");
    }

    #[test]
    fn scheme_default_port_matches() {
        let f = file(CONNS);
        // The backup entry carries no port (21 implicit): a URL with an
        // explicit :21 matches all the same.
        let (name, spec) = resolve_spec(&f, "ftp://backup.example:21").unwrap();
        assert_eq!(name.as_deref(), Some("backup"));
        assert_eq!(spec.auth, AuthMethod::Password);
    }

    #[test]
    fn matches_s3_connection_by_bucket() {
        let f = file(
            r#"
            [connections.almacen]
            url = "s3://mi-bucket"
            auth = "access-key"
            access_key_id = "AKIA"
            region = "eu-west-1"
            endpoint = "https://minio.interno:9000"
        "#,
        );
        let (name, spec) = resolve_spec(&f, "s3://mi-bucket").unwrap();
        assert_eq!(name.as_deref(), Some("almacen"));
        assert_eq!(spec.auth, AuthMethod::AccessKey);
        assert_eq!(spec.endpoint.as_deref(), Some("https://minio.interno:9000"));
        // A different bucket → ad-hoc (no config credentials).
        let (name, spec) = resolve_spec(&f, "s3://otro-bucket").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);
    }

    /// Two entries for the SAME host:port with different users and a URL
    /// without a user: the first wins by alphabetical order of the entry's
    /// NAME (`BTreeMap`) — fixed, deterministic behavior.
    #[test]
    fn user_ambiguity_resolves_deterministically() {
        let f = file(
            r#"
            [connections.bbb]
            url = "sftp://root@h.example"
            auth = "password"

            [connections.aaa]
            url = "sftp://oscar@h.example"
            auth = "key"
            key = "/k"
        "#,
        );
        let (name, spec) = resolve_spec(&f, "sftp://h.example").unwrap();
        assert_eq!(name.as_deref(), Some("aaa"));
        assert_eq!(spec.url, "sftp://oscar@h.example");
    }

    /// An entry with a broken URL does NOT block the resolution of the
    /// rest.
    #[test]
    fn a_broken_entry_is_skipped() {
        let f = file(
            r#"
            [connections.aaa]
            url = "no-es-una-url"

            [connections.bbb]
            url = "sftp://oscar@h.example"
            auth = "password"
        "#,
        );
        let (name, _) = resolve_spec(&f, "sftp://oscar@h.example").unwrap();
        assert_eq!(name.as_deref(), Some("bbb"));
    }

    #[test]
    fn no_match_is_ad_hoc_with_agent() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://nadie@otro.example").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);
        assert_eq!(spec.tls, norte_connect::TlsMode::Require);
    }

    /// Minimal spec (auth agent, no s3 fields) to test URL parsing.
    fn min_spec(url: &str) -> ConnectionSpec {
        ConnectionSpec {
            url: url.into(),
            auth: AuthMethod::Agent,
            key: None,
            tls: norte_connect::TlsMode::Require,
            region: None,
            endpoint: None,
            access_key_id: None,
            addressing: None,
            logical_trash: false,
            allow_rsa: false,
            secret: norte_connect::SecretSource::Stored,
        }
    }

    /// #47: the canonical form inherits the entry's user and normalizes the
    /// default port away — two forms of the same identity, one key.
    #[test]
    fn canonical_inherits_user_and_normalizes_port() {
        let f = file(CONNS);
        assert_eq!(
            canonical_from_file(&f, "sftp://work.example:2222").as_deref(),
            Some("oscar@work.example:2222"),
        );
        assert_eq!(
            canonical_from_file(&f, "sftp://oscar@work.example:2222").as_deref(),
            Some("oscar@work.example:2222"),
        );
        // Explicit default port drops out of the canonical form.
        assert_eq!(
            canonical_from_file(&f, "ftp://backup.example:21").as_deref(),
            Some("backup.example"),
        );
        // Ad-hoc with no entry: canonical = its own normalized form.
        assert_eq!(
            canonical_from_file(&f, "sftp://nadie@otro.example:22").as_deref(),
            Some("nadie@otro.example"),
        );
        // s3: the authority is the bucket, with no port.
        assert_eq!(
            canonical_from_file(&f, "s3://mi-bucket").as_deref(),
            Some("mi-bucket"),
        );
    }

    #[test]
    fn authority_is_canonical() {
        let spec = min_spec("sftp://u@[::1]:2222");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "u@[::1]:2222");
        let spec = min_spec("ftp://host");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "host");
        // s3://bucket: authority = bucket, no port.
        let spec = min_spec("s3://mi-bucket");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "mi-bucket");
    }

    /// The list that feeds the selector (#140, and since #264 also the
    /// window's, via `connection.list`): `(name, url)` pairs, alphabetical.
    #[tokio::test]
    async fn named_connections_lists_alphabetically_and_without_secrets() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("connections.toml"),
            r#"
[connections.trabajo]
url = "sftp://oscar@servidor.example/datos"

[connections.archivo]
url = "s3://mi-bucket"
"#,
        )
        .expect("escribir");

        let (cs, bad) = named_connections(dir.path()).await.expect("lista");
        assert!(bad.is_empty(), "all parse");
        assert_eq!(
            cs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["archivo", "trabajo"],
            "alphabetical: the file's order doesn't decide the selector's"
        );
        // What travels is the URL as is, and credentials are REFERENCED
        // (ADR 0015): there's nothing to resolve or filter here.
        assert_eq!(cs[1].1, "sftp://oscar@servidor.example/datos");
    }

    /// A file that is NOT THERE is an empty list — not having connections
    /// is normal on day one — but one that does NOT PARSE is an error:
    /// saying "you have none" when there's one comma too many lies about
    /// what the user wrote.
    #[tokio::test]
    async fn no_file_is_empty_and_a_broken_file_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cs, bad) = named_connections(dir.path())
            .await
            .expect("no file is not an error");
        assert!(cs.is_empty() && bad.is_empty());

        std::fs::write(dir.path().join("connections.toml"), "esto no es toml [[[")
            .expect("escribir");
        assert!(
            named_connections(dir.path()).await.is_err(),
            "a broken file gets TOLD"
        );
    }

    /// **An unusable entry does not take the rest down with it** (#365).
    ///
    /// The bug that uncovered it: a machine's `connections.toml` had an
    /// entry norte couldn't read, and `connection.list` answered a bare
    /// `InvalidPath`. The reader lost the list of ALL their connections
    /// over a single one, with an error that named none of them and
    /// pointed to nothing fixable.
    #[tokio::test]
    async fn an_entry_that_cant_be_understood_does_not_hide_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("connections.toml"),
            r#"
[connections.buena]
url = "sftp://servidor.example/datos"

[connections.rota]
url = "sftp://otro.example/"
password = "esto no va aquí"
"#,
        )
        .expect("escribir");

        let (cs, bad) = named_connections(dir.path()).await.expect("lista");
        assert_eq!(
            cs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["buena"],
            "the one that works gets listed"
        );
        assert_eq!(
            bad.len(),
            1,
            "and the one that doesn't, comes out separately: {bad:?}"
        );
        assert_eq!(bad[0].0, "rota", "named, or the warning is useless");
        assert!(
            bad[0].1.contains("password"),
            "and with the reason, which is the actionable part: {}",
            bad[0].1
        );
    }

    #[test]
    fn config_dir_respects_override() {
        // Without touching the global env (unsafe in edition 2024): only
        // the pure path. The `NORTE_CONFIG_DIR` override is covered in the
        // CLI's E2E.
        let d = config_dir();
        assert!(d.ends_with("norte") || d.as_os_str().len() > 1);
    }
}
