//! Establecimiento de conexión FTP/FTPS (ADR 0015 A/F, issue #38): la
//! política TLS (`require`/`allow`/`plain`) y el login viven AQUÍ; el
//! provider recibe el stream ya negociado ([`AsyncRustlsFtpStream`], el tipo
//! que `FtpProvider::new` acepta) y jamás ve un secreto.
//!
//! FTP plano NUNCA es silencioso: `plain` es opt-in con aviso, `allow` avisa
//! al degradar, y `require` (el default) falla cerrado sin TLS.

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

/// Puerto FTP por defecto cuando la URL no lo lleva.
const FTP_PORT: u16 = 21;

/// Conector FTP/FTPS.
#[derive(Debug, Clone, Default)]
pub struct FtpConnector {
    /// CA extra en PEM (cert corporativo/self-signed) que se AÑADE a las
    /// raíces de Mozilla al validar el cert del servidor FTPS. Nunca
    /// deshabilita la validación.
    pub extra_root_ca: Option<PathBuf>,
}

impl FtpConnector {
    /// Conector con las raíces por defecto (Mozilla, compiladas).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Conecta por FTP según `spec`, negocia TLS según `spec.tls` (ADR 0015
    /// F) y hace login. `secret` es la password (`auth = "password"`); con
    /// `auth = "agent"` el login es anónimo (convención guest).
    ///
    /// # Errors
    /// - [`ConnectError::Tls`]: `require` sin TLS del servidor o cert que no
    ///   valida (jamás se degrada).
    /// - [`ConnectError::AuthFailed`]: el servidor rechazó las credenciales.
    /// - [`ConnectError::Config`]: `auth = "key"` (no existe en FTP).
    // Sin campos de la URL cruda en el span (regla 10, mismo criterio que ssh).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<AsyncRustlsFtpStream, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "ftp" {
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (el conector FTP solo acepta ftp://)",
                ep.scheme
            )));
        }
        let port = ep.port.unwrap_or(FTP_PORT);
        tracing::debug!(host = %ep.host, port, tls = ?spec.tls, "estableciendo conexión ftp");

        // Credenciales ANTES de tocar la red: errores locales y claros.
        let user = ep.user.clone().unwrap_or_else(|| "anonymous".to_string());
        let pass: Zeroizing<String> = match spec.auth {
            AuthMethod::Key => {
                return Err(ConnectError::Config(
                    "auth = \"key\" no existe en FTP; usa \"password\" o \"agent\" (anónimo)"
                        .to_string(),
                ));
            }
            AuthMethod::Password => {
                let s = secret.ok_or_else(|| ConnectError::Secret {
                    conn: ep.host.clone(),
                })?;
                Zeroizing::new(s.expose().to_string())
            }
            // Convención guest: login anónimo, password de cortesía.
            AuthMethod::Agent => Zeroizing::new("anonymous".to_string()),
        };

        // La config TLS (lee la CA extra del disco) se valida ANTES de tocar
        // la red: un typo en `extra_root_ca` es error LOCAL de config — con
        // `allow` jamás debe acabar degradando a claro por un PEM roto.
        let tls: Option<TlsConnector> = match spec.tls {
            TlsMode::Plain => None,
            TlsMode::Require | TlsMode::Allow => {
                let extra = self.extra_root_ca.clone();
                let config = tokio::task::spawn_blocking(move || tls_config(extra.as_deref()))
                    .await
                    .map_err(|_| ConnectError::Tls("configuración TLS interrumpida".into()))??;
                Some(TlsConnector::from(Arc::new(config)))
            }
        };

        let stream = dial(&ep.host, port).await?;
        let mut stream = match (spec.tls, tls) {
            (TlsMode::Require, Some(tls)) => match secure(stream, tls, &ep.host).await {
                Ok(s) => s,
                Err(SecureError::AuthRejected) => {
                    return Err(ConnectError::Tls(
                        "el servidor rechazó AUTH TLS y tls = \"require\" exige FTPS".into(),
                    ));
                }
                Err(SecureError::Tls(e)) => return Err(e),
            },
            (TlsMode::Allow, Some(tls)) => match secure(stream, tls, &ep.host).await {
                Ok(s) => s,
                // Degradación LEGÍTIMA: el servidor rechazó el comando AUTH
                // (no ofrece TLS). Nunca silenciosa (ADR 0015 F).
                Err(SecureError::AuthRejected) => {
                    tracing::warn!(
                        host = %ep.host,
                        "el servidor no ofrece FTPS; DEGRADANDO a FTP en claro (tls = \"allow\") \
                         — sin protección ante un atacante activo"
                    );
                    // into_secure consumió la conexión: rediscar en plano.
                    dial(&ep.host, port).await?
                }
                // Handshake/validación que FALLA con el AUTH ya aceptado:
                // señal de MITM activo — fail-closed, jamás se degrada
                // entregando las credenciales al interceptor.
                Err(SecureError::Tls(e)) => return Err(e),
            },
            (TlsMode::Plain, _) => {
                tracing::warn!(
                    host = %ep.host,
                    "FTP EN CLARO (tls = \"plain\"): credenciales y datos sin cifrar"
                );
                stream
            }
            // tls es Some(...) exactamente para Require/Allow (match de arriba).
            (TlsMode::Require | TlsMode::Allow, None) => unreachable!("tls construido arriba"),
        };

        // suppaftp copia la password a su propio String (límite de la API);
        // el lado norte viaja zeroizado y jamás se loguea (regla 10).
        match stream.login(user.as_str(), pass.as_str()).await {
            Ok(()) => Ok(stream),
            // CUALQUIER respuesta inesperada al login va a AuthFailed sin
            // interpolar el body: el servidor controla ese texto y puede
            // ecoar la password que acaba de recibir (regla 10 — el sink a
            // proteger son los LOGS locales, el servidor ya vio el secreto).
            Err(suppaftp::FtpError::UnexpectedResponse(_)) => Err(ConnectError::AuthFailed {
                user,
                host: ep.host,
            }),
            Err(e) => Err(ConnectError::Ftp(redact_ftp_err(&e))),
        }
    }
}

/// Resultado de la negociación AUTH TLS, discriminado para la política
/// `allow`: solo el RECHAZO del comando AUTH permite degradar.
enum SecureError {
    /// El servidor rechazó `AUTH TLS` (no ofrece FTPS).
    AuthRejected,
    /// El handshake o la validación del cert fallaron (posible MITM) —
    /// fail-closed siempre.
    Tls(ConnectError),
}

/// Negocia AUTH TLS validando el cert contra Mozilla + `extra_root_ca`.
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
        // Respuesta de error del servidor al comando AUTH = no hay TLS.
        Err(suppaftp::FtpError::UnexpectedResponse(_)) => Err(SecureError::AuthRejected),
        Err(e) => Err(SecureError::Tls(ConnectError::Tls(redact_ftp_err(&e)))),
    }
}

/// Abre la conexión de control (sin TLS todavía), con el workaround NAT de
/// PASV activo: la conexión de datos va SIEMPRE a la IP del canal de control,
/// ignorando la IP que anuncie el servidor (anti PASV-hijack/SSRF, como
/// `curl --ftp-skip-pasv-ip`).
async fn dial(host: &str, port: u16) -> Result<AsyncRustlsFtpStream, ConnectError> {
    let mut stream = AsyncRustlsFtpStream::connect((host, port))
        .await
        .map_err(|e| ConnectError::Ftp(redact_ftp_err(&e)))?;
    stream.set_passive_nat_workaround(true);
    Ok(stream)
}

/// Tope de longitud de un mensaje de error que incluya texto del servidor.
const ERR_MAX: usize = 200;

/// Representación SEGURA de un error de suppaftp para logs/errores: los
/// bodies de respuesta los controla el servidor (bytes arbitrarios → escape
/// injection en terminal, log injection, tamaño sin cota). Se escapan los
/// caracteres de control y se capea la longitud. El camino de LOGIN ni
/// siquiera pasa por aquí (va a `AuthFailed` sin body).
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

/// Raíces de Mozilla + CA extra opcional (PEM). La validación NUNCA se
/// deshabilita: un self-signed entra añadiendo su CA, no apagando el check.
fn tls_config(extra_root_ca: Option<&Path>) -> Result<ClientConfig, ConnectError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = extra_root_ca {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| ConnectError::Tls(format!("CA extra {}: {e}", path.display())))?;
        let mut añadidos = 0usize;
        for cert in certs {
            let cert =
                cert.map_err(|e| ConnectError::Tls(format!("CA extra {}: {e}", path.display())))?;
            roots
                .add(cert)
                .map_err(|e| ConnectError::Tls(format!("CA extra {}: {e}", path.display())))?;
            añadidos += 1;
        }
        // Un fichero sin ningún bloque PEM produce un iterador VACÍO, no un
        // error: sin este check, una CA con typo pasaría en silencio (y con
        // `allow` acabaría degradando la conexión que debía validar).
        if añadidos == 0 {
            return Err(ConnectError::Tls(format!(
                "CA extra {}: no contiene ningún certificado PEM",
                path.display()
            )));
        }
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}
