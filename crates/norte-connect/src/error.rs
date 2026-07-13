//! Errores de `norte-connect`. Diseñados para que NINGUNA variante pueda
//! contener material secreto (regla 10): las causas del store de secretos son
//! mensajes ESTÁTICOS (`&'static str`), nunca strings derivados del plaintext.

use thiserror::Error;

/// Error al resolver una conexión o su secreto.
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
}
