//! Errores de `norte-connect`. Diseñados para que NINGUNA variante pueda
//! contener material secreto (regla 10): las causas del store de secretos son
//! mensajes ESTÁTICOS (`&'static str`), nunca strings derivados del plaintext.

use std::path::PathBuf;

use thiserror::Error;

/// Error al resolver una conexión o su secreto.
///
/// Se proyecta a la taxonomía del protocolo con `norte_proto::Error::from`:
/// las variantes TOFU van 1:1; el resto degrada a categoría (el detalle se
/// queda en el log del core).
#[derive(Debug, Error)]
pub enum ConnectError {
    /// `connections.toml` mal formado o inválido (referencias, sin secretos).
    #[error("connections.toml: {0}")]
    Config(String),
    /// Una URL de conexión no parseable.
    #[error("URL de conexión inválida: {0}")]
    InvalidUrl(String),
    /// Fallo resolviendo el secreto de una conexión. Solo lleva el NOMBRE de
    /// la conexión, jamás el secreto (regla 10).
    #[error("no se pudo resolver el secreto de la conexión «{conn}»")]
    Secret {
        /// Nombre de la conexión (nunca el secreto).
        conn: String,
    },
    /// Causa estructural del store de secretos (`secrets.age`). El mensaje es
    /// ESTÁTICO por construcción: garantiza por tipo que jamás se interpola el
    /// plaintext descifrado ni la passphrase en un error que acabe en logs.
    #[error("secrets.age: {0}")]
    SecretStore(&'static str),
    /// Error de I/O (lectura de config / fichero de secretos).
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Host key SSH sin registrar en el primer contacto (TOFU, ADR 0015 D).
    /// El core lo mapea 1:1 a `Error::HostKeyUnknown` del protocolo; el
    /// frontend muestra el fingerprint y confirma con
    /// `connection.trust_host_key` antes de reintentar.
    #[error(
        "host key desconocida para {host}:{port} ({algo} {fingerprint}); confirma antes de conectar"
    )]
    HostKeyUnknown {
        /// Host desnudo (sin puerto).
        host: String,
        /// Puerto ya resuelto (22 si la URL no lo lleva).
        port: u16,
        /// Algoritmo de la clave presentada (p. ej. `ssh-ed25519`).
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` de la clave presentada.
        fingerprint: String,
    },
    /// La host key CAMBIÓ respecto a la registrada: posible MITM. Jamás se
    /// acepta en silencio (ADR 0015 D).
    #[error("la host key de {host}:{port} CAMBIÓ ({algo} {fingerprint}): posible MITM")]
    HostKeyMismatch {
        /// Host desnudo (sin puerto).
        host: String,
        /// Puerto ya resuelto.
        port: u16,
        /// Algoritmo de la clave presentada.
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` de la clave PRESENTADA.
        fingerprint: String,
    },
    /// El servidor rechazó la autenticación. Solo lleva user/host, jamás
    /// material secreto (regla 10).
    #[error("autenticación rechazada para {user}@{host}")]
    AuthFailed {
        /// Usuario con el que se intentó.
        user: String,
        /// Host de destino.
        host: String,
    },
    /// Clave de cliente de un algoritmo no admitido. Solo ed25519 (ADR 0015 E,
    /// cierra #36/RUSTSEC-2023-0071): RSA se rechaza SIEMPRE.
    #[error(
        "clave {} de tipo {algo}: solo se admite ed25519 (genera una con `ssh-keygen -t ed25519`)",
        path.display()
    )]
    KeyUnsupported {
        /// Ruta de la clave rechazada.
        path: PathBuf,
        /// Algoritmo detectado (p. ej. `ssh-rsa`).
        algo: String,
    },
    /// La clave de cliente no se pudo cargar (formato, passphrase incorrecta…).
    /// La causa viene de russh y no contiene la passphrase.
    #[error("no se pudo cargar la clave {}: {cause}", path.display())]
    KeyLoad {
        /// Ruta de la clave.
        path: PathBuf,
        /// Causa (de russh; sin material secreto).
        cause: String,
    },
    /// Agente SSH no disponible o sin identidades utilizables. Mensaje
    /// ESTÁTICO: nunca interpola material del agente.
    #[error("agente SSH: {0}")]
    Agent(&'static str),
    /// La URL no lleva usuario y el entorno no permite deducirlo.
    #[error("la conexión no especifica usuario (usa user@host) y no hay $USER en el entorno")]
    MissingUser,
    /// Error del transporte SSH (handshake, red, canal). El Display de russh
    /// no contiene secretos.
    #[error("SSH: {0}")]
    Ssh(String),
    /// El fichero `known_hosts` propio no se pudo leer/parsear.
    #[error("known_hosts: {0}")]
    KnownHosts(String),
    /// Error del transporte FTP (control, red, protocolo). El texto viene
    /// SANEADO (`redact_ftp_err`): los bodies de respuesta los controla el
    /// servidor y podrían llevar control chars o ecoar credenciales — el
    /// camino de login ni siquiera pasa por aquí (va a `AuthFailed`).
    #[error("FTP: {0}")]
    Ftp(String),
    /// TLS de FTPS falló: el servidor no lo ofrece con `tls = "require"`, o
    /// su certificado no valida contra las raíces (+ CA extra). Jamás se
    /// degrada en silencio (ADR 0015 F).
    #[error("FTPS/TLS: {0}")]
    Tls(String),
}

// EXCEPCIÓN consciente a "los tipos de russh no cruzan la frontera" (ADR
// 0015 A): `russh::client::Handler` exige `type Error: From<russh::Error>`,
// así que este impl es superficie pública forzada. El contenido sí queda
// contenido: se degrada a String (Display de russh, sin material secreto).
impl From<russh::Error> for ConnectError {
    fn from(e: russh::Error) -> Self {
        Self::Ssh(e.to_string())
    }
}

// Proyección a la taxonomía del protocolo (spec §17.7): el core la usa para
// que el fallo de conexión viaje por el wire. Las variantes TOFU van 1:1
// (portan host/port/algo/fingerprint para el flujo `connection.trust_host_key`,
// ADR 0015 D); el resto degrada a la categoría más cercana — el detalle queda
// en el log del core (el Display de ConnectError), no en el wire.
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
            // Credenciales rechazadas o irresolubles / material de clave
            // inutilizable: el usuario no puede autenticarse.
            ConnectError::AuthFailed { .. }
            | ConnectError::Secret { .. }
            | ConnectError::KeyUnsupported { .. }
            | ConnectError::KeyLoad { .. } => Self::PermissionDenied,
            // La URL/config de la conexión no es válida.
            ConnectError::InvalidUrl(_) | ConnectError::MissingUser | ConnectError::Config(_) => {
                Self::InvalidPath
            }
            // Transporte: la red puede reintentarse; una validación TLS o un
            // known_hosts/secret-store rotos NO (reintentar no los arregla).
            ConnectError::Ssh(_) | ConnectError::Ftp(_) => {
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

    /// Las variantes TOFU cruzan 1:1 al protocolo (mismo fingerprint y
    /// puerto RESUELTO): es lo que permite el mapeo error→trust en el
    /// frontend sin ambigüedad (ADR 0015 D).
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
            panic!("esperaba HostKeyUnknown, fue {p:?}");
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
}
