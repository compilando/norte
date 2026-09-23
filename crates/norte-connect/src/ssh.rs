//! Establecimiento de conexión SSH/SFTP (ADR 0015 A/D/E): verificación de
//! host key TOFU contra el [`KnownHostsStore`], auth por password / clave
//! ed25519 (RSA con `allow_rsa`, ADR 0150) / agente, y apertura del
//! subsistema sftp. Devuelve la
//! [`SftpSession`] que `SftpProvider::new` acepta (inyección de sesión,
//! ADR 0013): el provider jamás ve un secreto.
//!
//! Los tipos de auth de russh NO cruzan la frontera de este crate: el core
//! consume [`SshConnector`] con tipos propios (`ConnectionSpec`, `Secret`,
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

/// Puerto SSH por defecto cuando la URL no lo lleva.
const SSH_PORT: u16 = 22;

/// Conector SSH: encapsula el store TOFU y el socket del agente.
///
/// Los campos son públicos para que el core (o un test) inyecte rutas
/// explícitas; [`SshConnector::new`] resuelve los defaults del entorno.
#[derive(Debug, Clone)]
pub struct SshConnector {
    /// Store de host keys (TOFU, ADR 0015 D).
    pub known_hosts: KnownHostsStore,
    /// Socket del agente SSH (`SSH_AUTH_SOCK`); `None` = sin agente.
    pub agent_socket: Option<PathBuf>,
}

/// Material de auth ya validado, ANTES de tocar la red: el rechazo de una
/// clave RSA (ADR 0015 E) es local y claro, no un error de conexión.
enum PreparedAuth {
    Password,
    // Box: una PrivateKey pesa >400 bytes y acabará en un Arc igualmente.
    Key(Box<PrivateKey>),
    Agent,
}

impl SshConnector {
    /// Conector con defaults del entorno: `known_hosts` en el dir de config
    /// (u override `NORTE_KNOWN_HOSTS`) y agente de `SSH_AUTH_SOCK`.
    #[must_use]
    pub fn new(config_dir: &Path) -> Self {
        Self {
            known_hosts: KnownHostsStore::new(config_dir),
            agent_socket: std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
        }
    }

    /// Conecta por SSH según `spec`, verifica la host key contra el store
    /// (TOFU estricto) y abre el subsistema sftp.
    ///
    /// `secret` es la password (`auth = "password"`) o la passphrase de la
    /// clave (`auth = "key"`, `None` si la clave no está cifrada); lo resuelve
    /// el `SecretResolver` aguas arriba. Jamás se loguea (regla 10).
    ///
    /// # Errors
    /// - [`ConnectError::HostKeyUnknown`]: primer contacto; confirmar con
    ///   [`SshConnector::trust_host_key`] y reintentar.
    /// - [`ConnectError::HostKeyMismatch`]: la clave registrada cambió
    ///   (posible MITM); jamás se conecta.
    /// - [`ConnectError::KeyUnsupported`]: clave de cliente no-ed25519
    ///   (ADR 0015 E), rechazada ANTES de tocar la red.
    /// - [`ConnectError::AuthFailed`]: el servidor rechazó las credenciales.
    // Sin campos de la URL cruda: el span se abre ANTES de que el parser
    // pueda rechazar un `user:pass@host` inline (regla 10). El endpoint
    // redactado (host:port) se registra tras parsear.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<SftpSession, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "sftp" {
            // Solo el scheme: no se ecoa la URL entera en un error.
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (el conector SSH solo acepta sftp://)",
                ep.scheme
            )));
        }
        let user = resolve_user(ep.user.as_deref())?;
        let port = ep.port.unwrap_or(SSH_PORT);
        tracing::debug!(host = %ep.host, port, "estableciendo conexión sftp");

        // Prerrequisitos de auth ANTES de dial: el rechazo de una clave RSA
        // (ADR 0015 E) o la falta de agente son errores locales y claros,
        // no errores de red.
        let prepared = match spec.auth {
            AuthMethod::AccessKey => {
                return Err(ConnectError::Config(
                    "auth = \"access-key\" es de s3, no de SSH; usa \"key\", \"password\" o \"agent\""
                        .to_string(),
                ));
            }
            AuthMethod::Password => PreparedAuth::Password,
            AuthMethod::Agent => {
                if self.agent_socket.is_none() {
                    return Err(ConnectError::Agent(
                        "no disponible (SSH_AUTH_SOCK sin definir)",
                    ));
                }
                PreparedAuth::Agent
            }
            AuthMethod::Key => {
                let path = spec.key.as_deref().ok_or_else(|| {
                    ConnectError::Config(format!(
                        "la conexión a {} lleva auth = \"key\" sin `key = ...`",
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
                // Identificador redactado (host, no la URL) por regla 10.
                let s = secret.ok_or_else(|| ConnectError::Secret {
                    conn: ep.host.clone(),
                })?;
                // russh copia la password a su propio String; se libera con la
                // sesión (límite de la API de russh, no queda en logs).
                handle
                    .authenticate_password(user.clone(), s.expose())
                    .await?
            }
            PreparedAuth::Key(key) => {
                // Fuera de RSA el hash no aplica (None). Con RSA —que solo
                // llega aquí con `allow_rsa`, ADR 0150— se negocia rsa-sha2.
                let hash_alg = if key.algorithm().is_rsa() {
                    let hash = rsa_hash(&handle, &ep.host).await?;
                    tracing::warn!(
                        host = %ep.host,
                        "autenticando con clave RSA (allow_rsa, ADR 0150): \
                         firma por el camino de RUSTSEC-2023-0071"
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
            .map_err(|e| ConnectError::Ssh(format!("handshake sftp: {e}")))
    }

    /// Registra la host key de `host:port` en el store TRAS verificar que su
    /// fingerprint real coincide con `expected_fingerprint` (el que mostró
    /// `HostKeyUnknown` y confirmó el usuario). Re-verificación anti-TOCTOU:
    /// si el host presenta ahora OTRA clave, es
    /// [`ConnectError::HostKeyMismatch`] y no se registra nada.
    ///
    /// # Errors
    /// Si el host no responde, o el fingerprint presentado no coincide.
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
            // El handler captura la clave y RECHAZA el handshake: el dial
            // termina en error "esperado" que aquí ya no informa de nada.
            (Ok(key), _) => key,
            // Sin clave presentada: el error real es el de red/handshake.
            (Err(_), Err(e)) => return Err(e),
            (Err(_), Ok(_)) => {
                return Err(ConnectError::Ssh(format!(
                    "{host}:{port} no presentó host key"
                )));
            }
        };
        let presentada = fingerprint(&key);
        if presentada != expected_fingerprint {
            return Err(ConnectError::HostKeyMismatch {
                host: host.to_string(),
                port,
                algo: algo(&key),
                fingerprint: presentada,
            });
        }
        let store = self.known_hosts.clone();
        let host = host.to_string();
        tokio::task::spawn_blocking(move || {
            // El learn de russh solo APPENDEA: si el host ya tiene OTRA clave
            // registrada, "confiar por encima" dejaría dos entradas en
            // conflicto y el check en Mismatch perpetuo pese a un trust con
            // éxito. Fail-closed: que el usuario retire la entrada antigua.
            match store.check(&host, port, &key)? {
                HostKeyStatus::Known => Ok(()), // idempotente
                HostKeyStatus::Unknown { .. } => store.learn(&host, port, &key),
                // La categoría VIAJA como HostKeyMismatch (es el escenario
                // rotación/MITM para el que existe en la taxonomía); la guía
                // accionable queda en el log del core.
                HostKeyStatus::Mismatch { algo, fingerprint } => {
                    tracing::warn!(
                        host = %host,
                        port,
                        "trust rechazado: {host}:{port} ya tiene otra clave registrada \
                         (¿rotación?); elimina la entrada antigua del known_hosts antes \
                         de confiar la nueva"
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
        .map_err(|_| ConnectError::KnownHosts("registro interrumpido".into()))?
    }

    /// Autentica probando las identidades ed25519 del agente (ADR 0015 E:
    /// también vía agente, solo ed25519).
    async fn authenticate_via_agent(
        &self,
        handle: &mut russh::client::Handle<TofuHandler>,
        user: &str,
    ) -> Result<AuthResult, ConnectError> {
        let Some(sock) = self.agent_socket.as_deref() else {
            return Err(ConnectError::Agent(
                "no disponible (SSH_AUTH_SOCK sin definir)",
            ));
        };
        #[cfg(not(unix))]
        {
            let _ = (sock, handle, user);
            Err(ConnectError::Agent(
                "solo se soporta agente por socket unix por ahora",
            ))
        }
        #[cfg(unix)]
        {
            let mut agent = russh::keys::agent::client::AgentClient::connect_uds(sock)
                .await
                .map_err(|_| ConnectError::Agent("no se pudo conectar al socket del agente"))?;
            let identities = agent
                .request_identities()
                .await
                .map_err(|_| ConnectError::Agent("el agente no respondió al listar identidades"))?;
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
            // El caller convierte un AuthResult fallido en AuthFailed.
            last.ok_or(ConnectError::Agent(
                "el agente no tiene identidades ed25519",
            ))
        }
    }
}

/// Handler TOFU real (sustituye al `Ok(true)` de los tests de fase 5): la
/// clave del servidor se verifica ESTRICTA contra el store; desconocida o
/// cambiada abortan el handshake con el error tipado correspondiente.
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
        // I/O de fichero fuera del reactor (regla 2).
        let status = tokio::task::spawn_blocking(move || store.check(&host, port, &key))
            .await
            .map_err(|_| ConnectError::KnownHosts("verificación interrumpida".into()))??;
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

/// Handler de `trust_host_key`: captura la clave presentada y RECHAZA el
/// handshake (solo queríamos verla; jamás se auténtica ni se envía nada).
struct CaptureHandler {
    tx: mpsc::Sender<PublicKey>,
}

impl russh::client::Handler for CaptureHandler {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // Si el receptor murió, el connect ya se abandonó: nada que hacer.
        let _ = self.tx.send(server_public_key.clone());
        Ok(false)
    }
}

/// El hash de firma RSA, de lo que el servidor anuncia en `server-sig-algs`.
///
/// rsa-sha2-512 o -256 si los anuncia. Si solo anuncia `ssh-rsa` (SHA-1), es
/// un error: el opt-in de la ADR 0150 abre RSA, nunca SHA-1.
///
/// El `None` de russh junta tres casos: el servidor no manda la extensión
/// (RFC 8308 es opcional), la manda sin ningún algoritmo RSA, o llega después
/// del segundo que russh espera. En los tres se intenta rsa-sha2-256 —lo que
/// acepta cualquier servidor de la última década— y, si no lo acepta, falla
/// cerrado como `AuthFailed`. Nunca cae a SHA-1: con `Some(hash)` russh firma
/// y anuncia exactamente ese hash.
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
                "server-sig-algs sin rsa-sha2 (o sin extensión): se intenta rsa-sha2-256"
            );
            Ok(HashAlg::Sha256)
        }
    }
}

/// Carga la clave privada de cliente (con `~` expandido) y aplica la política
/// de algoritmos: ed25519 siempre (ADR 0015 E); RSA solo con `allow_rsa`
/// (ADR 0150), porque firmar con él es el camino de RUSTSEC-2023-0071.
async fn load_client_key(
    path: &Path,
    passphrase: Option<&Secret>,
    allow_rsa: bool,
) -> Result<PrivateKey, ConnectError> {
    // La passphrase viaja zeroizada hasta el descifrado de russh.
    let pass = passphrase.map(|s| Zeroizing::new(s.expose().to_string()));
    let for_task = path.to_path_buf();
    // expand_tilde también dentro: sin $HOME, home_dir() cae a getpwuid_r
    // (NSS puede tocar disco/red) — bloqueante, fuera del reactor (regla 2).
    let (expanded, loaded) = tokio::task::spawn_blocking(move || {
        let expanded = expand_tilde(&for_task);
        let loaded = russh::keys::load_secret_key(&expanded, pass.as_deref().map(String::as_str));
        (expanded, loaded)
    })
    .await
    .map_err(|_| ConnectError::KeyLoad {
        path: path.to_path_buf(),
        cause: "carga interrumpida".into(),
    })?;
    let key = loaded.map_err(|e| ConnectError::KeyLoad {
        // El error de russh (formato/passphrase) no contiene la passphrase.
        path: expanded.clone(),
        cause: e.to_string(),
    })?;
    match key.algorithm() {
        Algorithm::Ed25519 => Ok(key),
        Algorithm::Rsa { .. } if allow_rsa => Ok(key),
        other => Err(ConnectError::KeyUnsupported {
            path: expanded,
            algo: other.to_string(),
        }),
    }
}

/// Expande un `~` inicial al home del usuario (`key = "~/.ssh/id_ed25519"`
/// en `connections.toml`). Opera a nivel de componentes de `Path`, sin asumir
/// UTF-8 en el resto de la ruta (regla 1).
fn expand_tilde(path: &Path) -> PathBuf {
    match (std::env::home_dir(), path.strip_prefix("~")) {
        (Some(home), Ok(rest)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Usuario efectivo: el de la URL, o el del entorno (como OpenSSH).
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
    fn expand_tilde_solo_prefijo() {
        let home = std::env::home_dir().expect("home en el entorno de test");
        assert_eq!(
            expand_tilde(Path::new("~/.ssh/id_ed25519")),
            home.join(".ssh/id_ed25519")
        );
        // Sin `~` inicial: intacta (un `~` interior NO se expande).
        assert_eq!(
            expand_tilde(Path::new("/abs/~/x")),
            PathBuf::from("/abs/~/x")
        );
        assert_eq!(expand_tilde(Path::new("rel/x")), PathBuf::from("rel/x"));
    }

    #[test]
    fn resolve_user_explicito_gana() {
        assert_eq!(resolve_user(Some("oscar")).unwrap(), "oscar");
    }
}
