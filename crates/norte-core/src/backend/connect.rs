//! El área de conexión de [`Backend`](super::Backend): cerrar una sesión
//! remota, confiar una host key (TOFU) y entregar un secreto pedido.

use norte_proto::Error;

use super::Backend;

impl Backend {
    /// Cierra la sesión remota de `path` (#140). `false` = no había ninguna.
    ///
    /// # Errors
    ///
    /// Lo que devuelva el transporte. Un daemon N-1 sin el método contesta
    /// `METHOD_NOT_FOUND` → [`Error::Unsupported`].
    pub async fn close_connection(&self, path: &norte_proto::VPath) -> Result<bool, Error> {
        match self {
            Self::Embedded(engine) => Ok(engine.close_connection(path)),
            #[cfg(unix)]
            Self::Remote(r) => r.close_connection(path).await,
        }
    }

    /// Registra la host key de `host:port` tras la confirmación EXPLÍCITA
    /// del usuario (flujo TOFU: un `Error::HostKeyUnknown` trajo el
    /// fingerprint, el frontend lo mostró y el usuario aceptó — ADR 0015 D).
    /// El core re-verifica el fingerprint contra la clave real del host
    /// antes de registrar (anti-TOCTOU).
    ///
    /// # Errors
    /// Taxonomía del protocolo ([`Error::HostKeyMismatch`] si el host ya no
    /// presenta esa clave).
    /// `algo` viaja informativo en el wire (la identidad que se confirma es
    /// el fingerprint); pásalo tal cual llegó en el `HostKeyUnknown`.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        algo: &str,
        fingerprint: &str,
    ) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => {
                let _ = algo; // el engine confirma por fingerprint
                engine.trust_host_key(host, port, fingerprint).await
            }
            #[cfg(unix)]
            Self::Remote(r) => r.trust_host_key(host, port, algo, fingerprint).await,
        }
    }

    /// Entrega al core el secreto de `conn` que el humano acaba de teclear,
    /// tras un [`Error::SecretNeeded`] (#325). Vive en memoria, en el proceso
    /// que tiene el engine, y hasta que ese proceso pare: no se persiste en
    /// ningún sitio.
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::Unsupported`] si no hay conector.
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        match self {
            Self::Embedded(engine) => engine.provide_secret(conn, secret).await,
            #[cfg(unix)]
            Self::Remote(r) => r.provide_secret(conn, secret).await,
        }
    }
}
