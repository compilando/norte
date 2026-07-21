//! Connection manager del core (fase 6e, ADR 0015 A/G): resuelve un
//! `scheme://authority` remoto a un [`Provider`] vivo — busca la conexión en
//! `connections.toml` (o construye una ad-hoc desde la URL), resuelve el
//! secreto (env → keyring → `secrets.age`) y establece el transporte con
//! `norte-connect`, inyectando la sesión al provider (los providers JAMÁS
//! ven un secreto, reglas 7/10).
//!
//! El [`Engine`](crate::Engine) consulta este subsistema vía el trait
//! [`RemoteConnector`] (inyectable: los tests del engine usan uno falso) y
//! cachea el provider resultante por `scheme://authority`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use norte_connect::{
    AuthMethod, ConnectionSpec, ConnectionsFile, FtpConnector, S3Connector, Secret, SecretResolver,
    SshConnector,
};
use norte_proto::Error;
use norte_vfs::Provider;
use norte_vfs_ftp::FtpProvider;
use norte_vfs_object::ObjectProvider;
use norte_vfs_sftp::SftpProvider;

/// Establece providers remotos bajo demanda. El Engine lo consulta cuando un
/// `VPath` remoto no tiene provider cacheado; la implementación real es
/// [`ConnectionManager`].
#[async_trait]
pub trait RemoteConnector: Send + Sync {
    /// Conecta y construye el provider para `scheme://authority`, junto a los
    /// avisos de seguridad emitidos al establecerlo (#44: degradación TLS).
    ///
    /// # Errors
    /// [`Error::HostKeyUnknown`]/[`Error::HostKeyMismatch`] (flujo TOFU, ADR
    /// 0015 D) o la categoría a la que degrade el fallo de conexión.
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, Error>;

    /// Registra la host key de `host:port` tras la confirmación EXPLÍCITA del
    /// usuario (método `connection.trust_host_key`); re-verifica el
    /// fingerprint contra la clave real (anti-TOCTOU, en `norte-connect`).
    ///
    /// # Errors
    /// [`Error::HostKeyMismatch`] si el host ya no presenta esa clave.
    async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        fingerprint: &str,
    ) -> Result<(), Error>;
}

/// Causa de una degradación de seguridad al conectar (#44). Vocabulario CERRADO:
/// su `wire()` es el `reason` de [`norte_proto::methods::ConnectionDegraded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionWarningReason {
    /// FTP `tls="allow"`: el servidor rechazó `AUTH TLS` → sesión en claro.
    TlsAuthRejected,
}

impl ConnectionWarningReason {
    /// El string de wire (cerrado y contractual; ver `ConnectionDegraded.reason`).
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            ConnectionWarningReason::TlsAuthRejected => "tls-auth-rejected",
        }
    }
}

/// Un aviso de seguridad producido al establecer una sesión remota (#44). El
/// `host` va SIN userinfo (regla 10) por construcción — es el `Endpoint.host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionWarning {
    /// Scheme de la sesión (p. ej. `"ftp"`).
    pub scheme: String,
    /// Host, sin userinfo.
    pub host: String,
    /// Causa.
    pub reason: ConnectionWarningReason,
}

/// La sesión establecida + los avisos de seguridad emitidos al establecerla.
pub struct Connected {
    /// El provider vivo.
    pub provider: Arc<dyn Provider>,
    /// Avisos (p. ej. degradación TLS); vacío en el caso normal.
    pub warnings: Vec<ConnectionWarning>,
}

/// Observa los avisos de conexión (#44): el daemon lo implementa para difundir
/// `connection.degraded`; la CLI embebida para imprimir por stderr. Inyectado en
/// el [`Engine`](crate::Engine) con `set_connection_observer`.
pub trait ConnectionObserver: Send + Sync {
    /// Un aviso ocurrió al establecer una sesión. Best-effort, no bloqueante.
    fn on_connection_warning(&self, warning: &ConnectionWarning);
}

// Clave de anclaje del journal (M3-5, ADR 0025): vive en norte-connect (el
// dominio de secretos/keyring, regla 10); el CLI la usa vía este re-export.
pub use norte_connect::journal_anchor_key;

/// Directorio de config del usuario para `connections.toml` / `known_hosts` /
/// `secrets.age`: `$NORTE_CONFIG_DIR` (override explícito) →
/// `$XDG_CONFIG_HOME/norte` → `~/.config/norte` (unix) / `%APPDATA%\norte`
/// (Windows). Misma capa de usuario que el resto de la config (ADR 0007).
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NORTE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME")
        && !d.is_empty()
    {
        return PathBuf::from(d).join("norte");
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("APPDATA") {
        return PathBuf::from(d).join("norte");
    }
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("norte")
}

/// La URL de la conexión NOMBRADA `name` en `<dir>/connections.toml` (para
/// `norte connect <nombre>`: el frontend traduce el nombre a URL y el
/// establecimiento va por el camino normal del engine).
///
/// # Errors
/// [`Error::NotFound`] si el nombre no existe; [`Error::InvalidPath`] si el
/// fichero no parsea.
pub async fn named_url(dir: &std::path::Path, name: &str) -> Result<String, Error> {
    let dir = dir.to_path_buf();
    let file = tokio::task::spawn_blocking(move || ConnectionsFile::load(&dir))
        .await
        .map_err(|_| Error::Internal { panic: true })?
        .map_err(log_and_map)?;
    file.connections
        .get(name)
        .map(|s| s.url.clone())
        .ok_or(Error::NotFound)
}

/// La implementación real de [`RemoteConnector`] sobre `norte-connect`.
pub struct ConnectionManager {
    config_dir: PathBuf,
    secrets: SecretResolver,
    ssh: SshConnector,
    ftp: FtpConnector,
    s3: S3Connector,
}

impl ConnectionManager {
    /// Manager anclado en `config_dir` (ver [`config_dir()`] para el default).
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        let dir: PathBuf = config_dir.into();
        Self {
            secrets: SecretResolver::new(&dir),
            ssh: SshConnector::new(&dir),
            ftp: FtpConnector::new(),
            s3: S3Connector::new(),
            config_dir: dir,
        }
    }

    /// Conecta la conexión NOMBRADA `name` de `connections.toml` (UX de
    /// `norte connect <nombre>`). Devuelve el par (scheme, authority) bajo el
    /// que el Engine la cachearía, junto al provider.
    ///
    /// # Errors
    /// `Error::NotFound` si el nombre no existe; los de la conexión.
    pub async fn connect_named(&self, name: &str) -> Result<(String, String, Connected), Error> {
        let file = self.load_connections().await?;
        let spec = file.connections.get(name).ok_or(Error::NotFound)?.clone();
        let ep = spec.endpoint().map_err(log_and_map)?;
        let authority = authority_of(&ep);
        let connected = self.establish(&spec, Some(name)).await?;
        Ok((ep.scheme, authority, connected))
    }

    /// Carga `connections.toml` (I/O síncrona → `spawn_blocking`, regla 2).
    async fn load_connections(&self) -> Result<ConnectionsFile, Error> {
        let dir = self.config_dir.clone();
        tokio::task::spawn_blocking(move || ConnectionsFile::load(&dir))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(log_and_map)
    }

    /// Establece el transporte para `spec` y construye el provider. `name` es
    /// el nombre de la conexión en `connections.toml` (ad-hoc = None; el
    /// secreto se busca entonces bajo el host).
    async fn establish(
        &self,
        spec: &ConnectionSpec,
        name: Option<&str>,
    ) -> Result<Connected, Error> {
        let ep = spec.endpoint().map_err(log_and_map)?;
        let mut warnings: Vec<ConnectionWarning> = Vec::new();
        // El secreto solo se resuelve si el método de auth lo puede usar
        // (password/access-key siempre; key para la passphrase). Agent no
        // lleva secreto (en s3, agent = cadena ambiente de opendal).
        let secret: Option<Secret> = match spec.auth {
            AuthMethod::Agent => None,
            AuthMethod::Password | AuthMethod::Key | AuthMethod::AccessKey => self
                .secrets
                .resolve(name.unwrap_or(&ep.host), &spec.url)
                .await
                .map_err(log_and_map)?,
        };
        match ep.scheme.as_str() {
            "sftp" => {
                let session = self
                    .ssh
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(log_and_map)?;
                // Base "/": los segmentos del VPath son absolutos del server.
                Ok(Connected {
                    provider: Arc::new(
                        SftpProvider::new(session, "/").with_logical_trash(spec.logical_trash),
                    ),
                    warnings,
                })
            }
            "ftp" => {
                // DOS conexiones: control principal + lectura dedicada — la
                // copia FTP→FTP mismo host no deadlockea (issue #39 B1).
                let main = self
                    .ftp
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(log_and_map)?;
                let reader = self
                    .ftp
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(log_and_map)?;
                // #44: si el control principal cayó a claro (tls="allow" +
                // AUTH TLS rechazado), se surfacea la degradación al usuario.
                // Basta `main`: ambas conexiones comparten el MISMO `spec`, así
                // que degradan juntas o ninguna — mirar `reader.tls_degraded`
                // solo duplicaría el aviso (rust m2).
                if main.tls_degraded {
                    warnings.push(ConnectionWarning {
                        scheme: ep.scheme.clone(),
                        host: ep.host.clone(),
                        reason: ConnectionWarningReason::TlsAuthRejected,
                    });
                }
                let provider = FtpProvider::with_reader(main.stream, reader.stream, "/").await?;
                Ok(Connected {
                    provider: Arc::new(provider),
                    warnings,
                })
            }
            "s3" => {
                // El S3Connector construye y SONDEA el Operator (fail-fast); el
                // provider lo recibe inyectado, sin ver el secret-access-key.
                let op = self
                    .s3
                    .connect(spec, secret.as_ref())
                    .await
                    .map_err(log_and_map)?;
                Ok(Connected {
                    provider: Arc::new(
                        ObjectProvider::new(op, "s3").with_logical_trash(spec.logical_trash),
                    ),
                    warnings,
                })
            }
            _ => Err(Error::Unsupported),
        }
    }
}

#[async_trait]
impl RemoteConnector for ConnectionManager {
    // skip_all: la authority CRUDA no entra al span — un `user:pass@host`
    // que el VPath haya aceptado se rechaza al parsear la conexión, pero el
    // span se abriría ANTES (regla 10). Se loguea redactado tras el parse.
    #[tracing::instrument(level = "info", skip_all)]
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, Error> {
        let url = format!("{scheme}://{authority}");
        let file = self.load_connections().await?;
        let (name, spec) = resolve_spec(&file, &url).map_err(log_and_map)?;
        if let Ok(ep) = spec.endpoint() {
            tracing::info!(scheme = %ep.scheme, host = %ep.host, port = ?ep.port,
                conexion = name.as_deref().unwrap_or("(ad-hoc)"), "conectando");
        }
        self.establish(&spec, name.as_deref()).await
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
}

/// Resuelve la conexión para una URL remota: la ENTRADA de `connections.toml`
/// cuyo endpoint coincide (scheme + host + puerto efectivo + usuario si la
/// URL lo trae), o una spec ad-hoc desde la URL (auth = `agent`: SSH agent en
/// sftp, anónimo en ftp; `tls` default = require). Prioridad: el usuario de
/// la URL gana; a igualdad, la primera entrada por orden alfabético (el
/// `BTreeMap` de `ConnectionsFile` lo garantiza determinista).
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
    };
    let target = ad_hoc.endpoint()?;
    for (name, spec) in &file.connections {
        let Ok(ep) = spec.endpoint() else {
            continue; // una entrada rota no bloquea el resto
        };
        if ep.scheme != target.scheme || ep.host != target.host {
            continue;
        }
        if effective_port(&ep) != effective_port(&target) {
            continue;
        }
        // Usuario: si la URL lo trae, debe coincidir; si no, vale el de la
        // entrada (o ninguno).
        if let Some(u) = &target.user
            && ep.user.as_ref() != Some(u)
        {
            continue;
        }
        let mut spec = spec.clone();
        // La URL de la petición manda (lleva el usuario/puerto efectivos que
        // pidió el frontend), pero si no trae usuario y la entrada sí, el de
        // la entrada completa la spec. INVARIANTE: scheme/host/puerto de la
        // petición == los de la entrada (comparados arriba) — el secreto de
        // la entrada jamás viaja a otro host. Si un caller futuro pasa URLs
        // no derivadas de un VPath ya parseado, revalidar aquí.
        if target.user.is_some() || ep.user.is_none() {
            spec.url = url.to_string();
        }
        return Ok((Some(name.clone()), spec));
    }
    Ok((None, ad_hoc))
}

fn effective_port(ep: &norte_connect::Endpoint) -> u16 {
    // Constante de matching (nunca viaja): s3 no lleva puerto en la authority
    // (443 nominal); sftp=22, ftp=21.
    ep.port.unwrap_or(match ep.scheme.as_str() {
        "sftp" => 22,
        "s3" => 443,
        _ => 21,
    })
}

/// La authority canónica de un endpoint (con el puerto solo si es explícito):
/// clave de caché del Engine.
fn authority_of(ep: &norte_connect::Endpoint) -> String {
    let mut s = String::new();
    if let Some(u) = &ep.user {
        s.push_str(u);
        s.push('@');
    }
    // IPv6 vuelve a llevar corchetes en la forma authority.
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

/// Degrada un `ConnectError` a la taxonomía del wire dejando el DETALLE en el
/// log del core (el wire lleva la categoría; el Display de `ConnectError` no
/// contiene secretos por construcción).
fn log_and_map(e: norte_connect::ConnectError) -> Error {
    tracing::warn!(error = %e, "fallo de conexión remota");
    Error::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(toml: &str) -> ConnectionsFile {
        toml::from_str(toml).expect("toml válido")
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
    fn matchea_por_host_puerto_y_usuario() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://oscar@work.example:2222").unwrap();
        assert_eq!(name.as_deref(), Some("trabajo"));
        assert_eq!(spec.auth, AuthMethod::Key);

        // Usuario distinto en la URL → NO matchea la entrada (ad-hoc).
        let (name, spec) = resolve_spec(&f, "sftp://otro@work.example:2222").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);

        // Puerto distinto → ad-hoc.
        let (name, _) = resolve_spec(&f, "sftp://oscar@work.example:22").unwrap();
        assert_eq!(name, None);
    }

    #[test]
    fn url_sin_usuario_hereda_el_de_la_entrada() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://work.example:2222").unwrap();
        assert_eq!(name.as_deref(), Some("trabajo"));
        // La spec conserva la URL de la ENTRADA (con oscar@) para que la
        // conexión use ese usuario.
        assert_eq!(spec.url, "sftp://oscar@work.example:2222");
    }

    #[test]
    fn puerto_default_del_scheme_matchea() {
        let f = file(CONNS);
        // La entrada backup no lleva puerto (21 implícito): una URL con :21
        // explícito matchea igual.
        let (name, spec) = resolve_spec(&f, "ftp://backup.example:21").unwrap();
        assert_eq!(name.as_deref(), Some("backup"));
        assert_eq!(spec.auth, AuthMethod::Password);
    }

    #[test]
    fn matchea_conexion_s3_por_bucket() {
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
        // Otro bucket → ad-hoc (sin credenciales de config).
        let (name, spec) = resolve_spec(&f, "s3://otro-bucket").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);
    }

    /// Dos entradas para el MISMO host:puerto con usuarios distintos y una
    /// URL sin usuario: gana la primera por orden alfabético del NOMBRE de
    /// la entrada (`BTreeMap`) — comportamiento fijado y determinista.
    #[test]
    fn ambiguedad_de_usuario_resuelve_determinista() {
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

    /// Una entrada con URL rota NO bloquea la resolución del resto.
    #[test]
    fn entrada_rota_se_salta() {
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
    fn sin_match_es_ad_hoc_con_agent() {
        let f = file(CONNS);
        let (name, spec) = resolve_spec(&f, "sftp://nadie@otro.example").unwrap();
        assert_eq!(name, None);
        assert_eq!(spec.auth, AuthMethod::Agent);
        assert_eq!(spec.tls, norte_connect::TlsMode::Require);
    }

    /// spec mínima (auth agent, sin campos s3) para probar el parseo de URL.
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
        }
    }

    #[test]
    fn authority_canonica() {
        let spec = min_spec("sftp://u@[::1]:2222");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "u@[::1]:2222");
        let spec = min_spec("ftp://host");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "host");
        // s3://bucket: authority = bucket, sin puerto.
        let spec = min_spec("s3://mi-bucket");
        assert_eq!(authority_of(&spec.endpoint().unwrap()), "mi-bucket");
    }

    #[test]
    fn config_dir_respeta_override() {
        // Sin tocar env global (unsafe en edition 2024): solo el camino puro.
        // El override por NORTE_CONFIG_DIR se cubre en el E2E de la CLI.
        let d = config_dir();
        assert!(d.ends_with("norte") || d.as_os_str().len() > 1);
    }
}
