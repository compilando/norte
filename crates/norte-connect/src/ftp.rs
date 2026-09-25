//! FTP/FTPS connection establishment (ADR 0015 A/F, issue #38): the TLS
//! policy (`require`/`allow`/`plain`) and the login live HERE; the provider
//! receives the already-negotiated stream ([`AsyncRustlsFtpStream`], the type
//! `FtpProvider::new` accepts) and never sees a secret.
//!
//! Plain FTP is NEVER silent: `plain` is opt-in with a warning, `allow` warns
//! when it degrades, and `require` (the default) fails closed without TLS.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use zeroize::Zeroizing;

use crate::error::ConnectError;
use crate::secret::Secret;
use crate::spec::{AuthMethod, ConnectionSpec, TlsMode};

/// Default FTP port when the URL does not carry one.
const FTP_PORT: u16 = 21;

/// Result of [`FtpConnector::connect`] (#44): the stream + whether the
/// session DEGRADED to plain text (only possible with `tls="allow"` and the
/// server rejecting `AUTH TLS`). The caller (core) surfaces the degradation
/// to the user.
pub struct FtpConnectOutcome {
    /// The already-logged-in control stream.
    pub stream: AsyncRustlsFtpStream,
    /// `true` if `tls="allow"` fell back to plain because `AUTH TLS` was
    /// rejected.
    pub tls_degraded: bool,
}

/// FTP/FTPS connector.
#[derive(Debug, Clone, Default)]
pub struct FtpConnector {
    /// Extra CA in PEM (corporate/self-signed cert) that is ADDED to the
    /// Mozilla roots when validating the FTPS server's cert. Never disables
    /// validation.
    pub extra_root_ca: Option<PathBuf>,
}

impl FtpConnector {
    /// Connector with the default roots (Mozilla, compiled in).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects over FTP per `spec`, negotiates TLS per `spec.tls` (ADR 0015
    /// F) and logs in. `secret` is the password (`auth = "password"`); with
    /// `auth = "agent"` the login is anonymous (guest convention).
    ///
    /// # Errors
    /// - [`ConnectError::Tls`]: `require` without server TLS, or a cert that
    ///   does not validate (never degrades).
    /// - [`ConnectError::AuthFailed`]: the server rejected the credentials.
    /// - [`ConnectError::Config`]: `auth = "key"` (does not exist in FTP).
    // No raw URL fields in the span (rule 10, same criterion as ssh).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<FtpConnectOutcome, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "ftp" {
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (the FTP connector only accepts ftp://)",
                ep.scheme
            )));
        }
        let port = ep.port.unwrap_or(FTP_PORT);
        tracing::debug!(host = %ep.host, port, tls = ?spec.tls, "establishing ftp connection");

        // Credentials BEFORE touching the network: local, clear errors.
        let user = ep.user.clone().unwrap_or_else(|| "anonymous".to_string());
        let pass: Zeroizing<String> = match spec.auth {
            AuthMethod::Key => {
                return Err(ConnectError::Config(
                    "auth = \"key\" does not exist in FTP; use \"password\" or \"agent\" (anonymous)"
                        .to_string(),
                ));
            }
            AuthMethod::AccessKey => {
                return Err(ConnectError::Config(
                    "auth = \"access-key\" is from s3, not FTP; use \"password\" or \"agent\""
                        .to_string(),
                ));
            }
            AuthMethod::Password => {
                let s = secret.ok_or_else(|| ConnectError::Secret {
                    conn: ep.host.clone(),
                })?;
                Zeroizing::new(s.expose().to_string())
            }
            // Guest convention: anonymous login, courtesy password.
            AuthMethod::Agent => Zeroizing::new("anonymous".to_string()),
        };

        // The TLS config (reads the extra CA from disk) is validated BEFORE
        // touching the network: a typo in `extra_root_ca` is a LOCAL config
        // error — with `allow` it must never end up degrading to plain
        // because of a broken PEM.
        let tls: Option<TlsConnector> = match spec.tls {
            TlsMode::Plain => None,
            TlsMode::Require | TlsMode::Allow => {
                let extra = self.extra_root_ca.clone();
                let config = tokio::task::spawn_blocking(move || tls_config(extra.as_deref()))
                    .await
                    .map_err(|_| ConnectError::Tls("TLS configuration interrupted".into()))??;
                Some(TlsConnector::from(Arc::new(config)))
            }
        };

        let stream = dial(&ep.host, port).await?;
        let mut tls_degraded = false;
        let mut stream = match (spec.tls, tls) {
            (TlsMode::Require, Some(tls)) => match secure(stream, tls, &ep.host).await {
                Ok(s) => s,
                Err(SecureError::AuthRejected) => {
                    return Err(ConnectError::Tls(
                        "the server rejected AUTH TLS and tls = \"require\" demands FTPS".into(),
                    ));
                }
                Err(SecureError::Tls(e)) => return Err(e),
            },
            (TlsMode::Allow, Some(tls)) => match secure(stream, tls, &ep.host).await {
                Ok(s) => s,
                // LEGITIMATE degradation: the server rejected the AUTH
                // command (it does not offer TLS). Never silent (ADR 0015 F).
                Err(SecureError::AuthRejected) => {
                    tracing::warn!(
                        host = %ep.host,
                        "the server does not offer FTPS; DEGRADING to plain FTP (tls = \"allow\") \
                         — no protection against an active attacker"
                    );
                    tls_degraded = true;
                    // into_secure consumed the connection: redial in plain.
                    dial(&ep.host, port).await?
                }
                // Handshake/validation that FAILS with AUTH already
                // accepted: signal of an active MITM — fail-closed, never
                // degrades and hands the credentials to the interceptor.
                Err(SecureError::Tls(e)) => return Err(e),
            },
            (TlsMode::Plain, _) => {
                tracing::warn!(
                    host = %ep.host,
                    "FTP IN PLAIN TEXT (tls = \"plain\"): credentials and data unencrypted"
                );
                stream
            }
            // tls is Some(...) exactly for Require/Allow (match above).
            (TlsMode::Require | TlsMode::Allow, None) => unreachable!("tls built above"),
        };

        // suppaftp copies the password into its own String (API limitation);
        // norte's side travels zeroized and is never logged (rule 10).
        match stream.login(user.as_str(), pass.as_str()).await {
            Ok(()) => Ok(FtpConnectOutcome {
                stream,
                tls_degraded,
            }),
            // ANY unexpected response to login goes to AuthFailed without
            // interpolating the body: the server controls that text and it
            // could echo the password it just received (rule 10 — the sink
            // to protect is the LOCAL logs, the server has already seen the
            // secret).
            Err(suppaftp::FtpError::UnexpectedResponse(_)) => Err(ConnectError::AuthFailed {
                user,
                host: ep.host,
            }),
            Err(e) => Err(ConnectError::Ftp(redact_ftp_err(&e))),
        }
    }
}

/// Result of the AUTH TLS negotiation, discriminated for the `allow` policy:
/// only the REJECTION of the AUTH command allows degrading.
enum SecureError {
    /// The server rejected `AUTH TLS` (it does not offer FTPS).
    AuthRejected,
    /// The handshake or the cert validation failed (possible MITM) — always
    /// fail-closed.
    Tls(ConnectError),
}

/// Negotiates AUTH TLS, validating the cert against Mozilla + `extra_root_ca`.
async fn secure(
    stream: AsyncRustlsFtpStream,
    tls: TlsConnector,
    host: &str,
) -> Result<AsyncRustlsFtpStream, SecureError> {
    match stream
        .into_secure(AsyncRustlsConnector::from(tls), host)
        .await
    {
        Ok(s) => Ok(s),
        // An error response from the server to the AUTH command = no TLS.
        Err(suppaftp::FtpError::UnexpectedResponse(_)) => Err(SecureError::AuthRejected),
        Err(e) => Err(SecureError::Tls(ConnectError::Tls(redact_ftp_err(&e)))),
    }
}

/// Opens the control connection (no TLS yet), with the PASV NAT workaround
/// active: the data connection ALWAYS goes to the control channel's IP,
/// ignoring whatever IP the server announces (anti PASV-hijack/SSRF, like
/// `curl --ftp-skip-pasv-ip`).
async fn dial(host: &str, port: u16) -> Result<AsyncRustlsFtpStream, ConnectError> {
    let mut stream = AsyncRustlsFtpStream::connect((host, port))
        .await
        .map_err(|e| ConnectError::Ftp(redact_ftp_err(&e)))?;
    stream.set_passive_nat_workaround(true);
    Ok(stream)
}

/// Length cap for an error message that includes server text.
const ERR_MAX: usize = 200;

/// SAFE representation of a suppaftp error for logs/errors: response bodies
/// are controlled by the server (arbitrary bytes → terminal escape
/// injection, log injection, unbounded size). Control characters are
/// escaped and the length is capped. The LOGIN path does not even go through
/// here (it goes to `AuthFailed` without a body).
fn redact_ftp_err(e: &suppaftp::FtpError) -> String {
    let raw = e.to_string();
    let mut out: String = raw
        .chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .take(ERR_MAX)
        .collect();
    if raw.chars().count() > ERR_MAX {
        out.push('…');
    }
    out
}

/// Mozilla roots + optional extra CA (PEM). Validation is NEVER disabled: a
/// self-signed cert gets in by adding its CA, not by turning off the check.
fn tls_config(extra_root_ca: Option<&Path>) -> Result<ClientConfig, ConnectError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = extra_root_ca {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| ConnectError::Tls(format!("extra CA {}: {e}", path.display())))?;
        let mut added = 0usize;
        for cert in certs {
            let cert =
                cert.map_err(|e| ConnectError::Tls(format!("extra CA {}: {e}", path.display())))?;
            roots
                .add(cert)
                .map_err(|e| ConnectError::Tls(format!("extra CA {}: {e}", path.display())))?;
            added += 1;
        }
        // A file with no PEM block produces an EMPTY iterator, not an error:
        // without this check, a CA with a typo would pass silently (and with
        // `allow` would end up degrading the connection it was meant to
        // validate).
        if added == 0 {
            return Err(ConnectError::Tls(format!(
                "extra CA {}: does not contain any PEM certificate",
                path.display()
            )));
        }
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}
