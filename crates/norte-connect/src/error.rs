//! Errores de `norte-connect`. Diseñados para que NINGUNA variante pueda
//! contener material secreto (regla 10): las causas del store de secretos son
//! mensajes ESTÁTICOS (`&'static str`), nunca strings derivados del plaintext.

use std::path::PathBuf;

use thiserror::Error;

/// De cuál de los tres escalones del resolver salió un secreto (ADR 0015 C).
///
/// Vocabulario CERRADO a propósito: es lo único que le dice al usuario DÓNDE
/// está el hueco, y con `&'static str` sueltos un intercambio entre dos sitios
/// de llamada apuntaría al escalón equivocado sin que ningún test se pusiera
/// rojo (revisión rust MINOR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretOrigin {
    /// `NORTE_SECRET_<CONN>`.
    Env,
    /// Keyring del OS.
    Keyring,
    /// Fichero `secrets.age`.
    AgeFile,
    /// Lo que un humano tecleó en esta sesión (#325).
    Session,
}

impl std::fmt::Display for SecretOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Env => "variable de entorno",
            Self::Keyring => "keyring",
            Self::AgeFile => "secrets.age",
            Self::Session => "lo tecleado en esta sesión",
        })
    }
}

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
    /// El secreto de una conexión se resolvió, pero es la cadena VACÍA (#320).
    /// Se rechaza en vez de pasarlo: opendal descarta un `secret_access_key`
    /// vacío (`if !v.is_empty()`), no registra el proveedor estático y la
    /// conexión acabaría autenticando con la cadena ambiente (perfil, SSO,
    /// IMDS) — una identidad que nadie pidió. Lleva el nombre de la conexión y
    /// el ORIGEN del hueco, jamás el secreto (regla 10).
    ///
    /// NO aplica a `auth = "key"`: ahí el secreto es la passphrase de la clave,
    /// donde vacío y ausente son lo mismo y no hay nada que suplantar. Lo
    /// filtra `establish` en el core.
    #[error(
        "el secreto de la conexión «{conn}» está definido pero VACÍO ({origin}): dale un valor real o quítalo"
    )]
    SecretEmpty {
        /// Nombre de la conexión (nunca el secreto).
        conn: String,
        /// En qué escalón del resolver apareció el vacío.
        origin: SecretOrigin,
    },
    /// La env var del secreto existe pero sus bytes NO son UTF-8 válido.
    ///
    /// Antes se trataba como «no está» (`env::var(..).ok()`) y la resolución
    /// seguía al keyring: una contraseña en Latin-1 desaparecía en silencio y
    /// `norte doctor` la daba por presente (revisión rust MAJOR-4). Un secreto
    /// viaja como `String`, así que aquí no hay nada que preservar: lo honesto
    /// es decirlo. Lleva solo el nombre de la conexión (regla 10).
    #[error(
        "el secreto de la conexión «{conn}» ({origin}) no es UTF-8 válido: reescríbelo, o guárdalo en el keyring o en secrets.age"
    )]
    SecretNotUtf8 {
        /// Nombre de la conexión (nunca el secreto).
        conn: String,
        /// En qué escalón del resolver apareció (hoy solo el entorno).
        origin: SecretOrigin,
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
    /// Error de object storage (construcción del `Operator` o sondeo): red,
    /// bucket ausente, config. El texto lleva solo la CATEGORÍA de opendal
    /// (`ErrorKind`), jamás el secreto (regla 10, ADR 0016 K).
    #[error("s3: {0}")]
    S3(String),
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
            | ConnectError::SecretEmpty { .. }
            | ConnectError::SecretNotUtf8 { .. }
            | ConnectError::KeyUnsupported { .. }
            | ConnectError::KeyLoad { .. } => Self::PermissionDenied,
            // NOTA (#325): `Error::SecretNeeded` no se produce aquí. Es una
            // PREGUNTA, no un fallo, y necesita el ENDPOINT además del nombre
            // —un diálogo de contraseña que no dice a quién se la va a dar no
            // es contestable—; el endpoint lo conoce `establish`, en el core,
            // no este resolutor. Se construye allí (`connect::secret_needed`).
            // La URL/config de la conexión no es válida.
            ConnectError::InvalidUrl(_) | ConnectError::MissingUser | ConnectError::Config(_) => {
                Self::InvalidPath
            }
            // Transporte: la red puede reintentarse; una validación TLS o un
            // known_hosts/secret-store rotos NO (reintentar no los arregla).
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
