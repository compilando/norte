//! `connections.toml`: SOLO referencias (regla 10), nunca secretos (ADR 0015 B).
//!
//! ```toml
//! [connections.trabajo]
//! url = "sftp://oscar@sftp.example.com:22"
//! auth = "key"
//! key = "~/.ssh/id_ed25519"
//!
//! [connections.backup]
//! url = "ftp://backup@ftp.example.com:21"
//! auth = "password"
//! tls = "require"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ConnectError;

/// Fichero `connections.toml` parseado.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionsFile {
    /// Conexiones por nombre.
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionSpec>,
}

/// Una conexión remota: referencia, jamás el secreto (ADR 0015).
///
/// `deny_unknown_fields`: un campo inesperado (p. ej. un `password = "…"`
/// inline que el usuario intente meter aquí) es un ERROR ruidoso, no se ignora
/// en silencio — los secretos van al keyring/env/age, jamás a config plano.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSpec {
    /// `scheme://[user@]host[:port]`.
    pub url: String,
    /// Método de auth. Default: `agent` (SSH agent / anónimo).
    #[serde(default)]
    pub auth: AuthMethod,
    /// Ruta a la clave privada (para `auth = "key"`). NUNCA el secreto en sí:
    /// la passphrase de la clave se resuelve por el `SecretResolver`.
    pub key: Option<PathBuf>,
    /// Política TLS para FTP. Default: `require` (FTPS).
    #[serde(default)]
    pub tls: TlsMode,
}

/// Cómo autenticarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    /// SSH agent (sftp) o anónimo (ftp).
    #[default]
    Agent,
    /// Clave privada (`key = …`), passphrase por el resolver.
    Key,
    /// Contraseña por el resolver.
    Password,
}

/// Política TLS de FTP (ADR 0014/0015 F).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// FTPS obligatorio (AUTH TLS). Default seguro.
    #[default]
    Require,
    /// Intenta TLS; si el servidor RECHAZA `AUTH TLS`, cae a plano con aviso.
    /// OJO: no protege ante un atacante ACTIVO (puede suprimir el AUTH y
    /// recibir las credenciales en claro). Un fallo de handshake/validación
    /// con el AUTH ya aceptado NO degrada (fail-closed: posible MITM).
    Allow,
    /// FTP plano (inseguro): opt-in EXPLÍCITO.
    Plain,
}

/// Endpoint extraído de una `url` (`scheme://[user@]host[:port]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `sftp` | `ftp`.
    pub scheme: String,
    /// Usuario, si la URL lo lleva.
    pub user: Option<String>,
    /// Host (sin `[]` de IPv6).
    pub host: String,
    /// Puerto, si la URL lo lleva.
    pub port: Option<u16>,
}

impl ConnectionSpec {
    /// Parsea el `scheme://[user@]host[:port]` de la `url`.
    ///
    /// # Errors
    /// Si la URL no tiene la forma esperada.
    pub fn endpoint(&self) -> Result<Endpoint, ConnectError> {
        parse_endpoint(&self.url)
    }
}

/// Parser mínimo de `scheme://[user@]host[:port]` (sin path). Evita una dep de
/// URL completa: solo estos schemes remotos.
fn parse_endpoint(url: &str) -> Result<Endpoint, ConnectError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| ConnectError::InvalidUrl(url.to_string()))?;
    // Solo authority: descarta cualquier `/path` accidental.
    let authority = rest.split('/').next().unwrap_or(rest);
    let (user, hostport) = match authority.split_once('@') {
        // `@` sin usuario (`sftp://@host`) es una URL malformada, no un host.
        Some(("", _)) => return Err(ConnectError::InvalidUrl(url.to_string())),
        // `user:pass@host` NO se admite (regla 10: el secreto iría a config/
        // logs). Mensaje ESTÁTICO, y este check va ANTES que el del scheme:
        // ningún error posterior puede ecoar una URL con password (p. ej. el
        // typo `ftps://u:pass@h` moriría por scheme ecoando el secreto).
        Some((u, _)) if u.contains(':') => {
            return Err(ConnectError::InvalidUrl(
                "la URL no debe llevar password inline (user:pass@…); el secreto va por el \
                 keyring/env/secrets.age"
                    .to_string(),
            ));
        }
        Some((u, hp)) => (Some(u.to_string()), hp),
        None => (None, authority),
    };
    if scheme.is_empty() || (scheme != "sftp" && scheme != "ftp") {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    }
    // IPv6 SIEMPRE entre `[...]`; fuera de corchetes un `:` residual en el host
    // sería un IPv6 sin corchetes (ambiguo) → inválido.
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let (h, tail) = rest
            .split_once(']')
            .ok_or_else(|| ConnectError::InvalidUrl(url.to_string()))?;
        (h.to_string(), parse_port(tail.strip_prefix(':'), url)?)
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        if h.contains(':') {
            return Err(ConnectError::InvalidUrl(url.to_string()));
        }
        (h.to_string(), parse_port(Some(p), url)?)
    } else if hostport.contains(':') {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    } else {
        (hostport.to_string(), None)
    };
    if host.is_empty() || !is_valid_host(&host) {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    }
    Ok(Endpoint {
        scheme: scheme.to_string(),
        user,
        host,
        port,
    })
}

/// Charset de hostname/IP (incl. IPv6 con zona: `:`/`%`). Excluye TODO lo que
/// tiene significado en el formato `known_hosts` (`,` lista de hosts, espacio
/// y nueva línea separadores, `#` comentario, `|` hash) y los caracteres de
/// control: un host hostil no puede envenenar entradas ajenas vía `learn`.
fn is_valid_host(host: &str) -> bool {
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '%'))
}

fn parse_port(p: Option<&str>, url: &str) -> Result<Option<u16>, ConnectError> {
    match p {
        None | Some("") => Ok(None),
        // El puerto 0 no es un puerto de destino válido.
        Some(p) => match p.parse::<u16>() {
            Ok(0) | Err(_) => Err(ConnectError::InvalidUrl(url.to_string())),
            Ok(n) => Ok(Some(n)),
        },
    }
}

impl ConnectionsFile {
    /// Carga `<dir>/connections.toml`. Si no existe, devuelve vacío (no es un
    /// error: las conexiones son opcionales).
    ///
    /// SÍNCRONO a propósito: es carga de config de BOOTSTRAP (una vez al
    /// arranque, antes de entrar al runtime, o desde `spawn_blocking` si se
    /// llama en contexto async). No es una ruta caliente; no hace I/O de red.
    ///
    /// # Errors
    /// Si el fichero existe pero es TOML inválido o no legible.
    pub fn load(dir: &Path) -> Result<Self, ConnectError> {
        let path = dir.join("connections.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConnectError::Io(e)),
        };
        toml::from_str(&text).map_err(|e| ConnectError::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connections_toml() {
        let toml = r#"
            [connections.trabajo]
            url = "sftp://oscar@sftp.example.com:22"
            auth = "key"
            key = "/home/oscar/.ssh/id_ed25519"

            [connections.backup]
            url = "ftp://backup@ftp.example.com"
            auth = "password"
            tls = "plain"
        "#;
        let f: ConnectionsFile = toml::from_str(toml).unwrap();
        let t = &f.connections["trabajo"];
        assert_eq!(t.auth, AuthMethod::Key);
        assert_eq!(t.tls, TlsMode::Require); // default
        assert!(t.key.is_some());
        let ep = t.endpoint().unwrap();
        assert_eq!(ep.scheme, "sftp");
        assert_eq!(ep.user.as_deref(), Some("oscar"));
        assert_eq!(ep.host, "sftp.example.com");
        assert_eq!(ep.port, Some(22));
        assert_eq!(f.connections["backup"].tls, TlsMode::Plain);
    }

    #[test]
    fn endpoint_variantes() {
        let ep = parse_endpoint("sftp://host").unwrap();
        assert_eq!(ep.host, "host");
        assert_eq!(ep.user, None);
        assert_eq!(ep.port, None);
        let ep = parse_endpoint("ftp://u@[::1]:2121").unwrap();
        assert_eq!(ep.host, "::1");
        assert_eq!(ep.user.as_deref(), Some("u"));
        assert_eq!(ep.port, Some(2121));
    }

    #[test]
    fn endpoint_invalido() {
        assert!(parse_endpoint("sin-scheme").is_err());
        assert!(parse_endpoint("http://host").is_err()); // scheme no remoto
        assert!(parse_endpoint("sftp://host:noport").is_err());
        assert!(parse_endpoint("sftp://").is_err()); // host vacío
        assert!(parse_endpoint("sftp://@host").is_err()); // usuario vacío
        assert!(parse_endpoint("sftp://host:0").is_err()); // puerto 0
        assert!(parse_endpoint("sftp://::1").is_err()); // IPv6 sin corchetes
    }

    /// Un password inline en la URL (`user:pass@host`) se rechaza SIN ecoar
    /// la URL: si se aceptara (o se ecoara en el error), el password acabaría
    /// en connections.toml, en logs o en mensajes de error (regla 10).
    #[test]
    fn password_inline_en_url_rechazado_sin_eco() {
        let err = parse_endpoint("sftp://u:hunter2@h").unwrap_err();
        assert!(
            !format!("{err}").contains("hunter2"),
            "el error no debe ecoar el password"
        );
    }

    /// El typo `ftps://` (scheme inválido) con password inline TAMPOCO ecoa
    /// la URL: el check del userinfo va ANTES que el del scheme — si no, el
    /// error de scheme llevaría el password a los logs (regla 10).
    #[test]
    fn scheme_invalido_con_password_inline_no_eco() {
        for url in ["ftps://u:hunter2@h", "http://u:hunter2@h"] {
            let err = parse_endpoint(url).unwrap_err();
            assert!(
                !format!("{err}").contains("hunter2"),
                "{url}: el error ecoa el password"
            );
        }
    }

    /// El host no admite caracteres con significado en `known_hosts` (`,` lista
    /// de hosts, espacio/nueva-línea separadores, `#` comentario, `|` hash) ni
    /// de control: si se colaran, un `learn` podría envenenar entradas ajenas.
    #[test]
    fn host_con_caracteres_de_formato_rechazado() {
        for url in [
            "sftp://banco.com,evil.com",
            "sftp://a b",
            "sftp://a\nb",
            "sftp://a#b",
            "sftp://a|b",
            "sftp://a\tb",
        ] {
            assert!(parse_endpoint(url).is_err(), "{url:?} debería ser inválida");
        }
    }

    #[test]
    fn deny_unknown_rechaza_secreto_inline() {
        // Un `password` inline (regla 10) debe ser ERROR, no ignorarse.
        let toml = r#"
            [connections.x]
            url = "sftp://h"
            password = "no-va-aqui"
        "#;
        assert!(toml::from_str::<ConnectionsFile>(toml).is_err());
    }

    #[test]
    fn load_ausente_es_vacio() {
        let dir = tempfile::tempdir().unwrap();
        let f = ConnectionsFile::load(dir.path()).unwrap();
        assert!(f.connections.is_empty());
    }
}
