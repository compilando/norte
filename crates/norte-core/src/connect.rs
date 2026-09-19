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

/// Establece providers remotos bajo demanda. El Engine lo consulta cuando un
/// `VPath` remoto no tiene provider cacheado; la implementación real es
/// [`ConnectionManager`].
#[async_trait]
pub trait RemoteConnector: Send + Sync {
    /// Conecta y construye el provider para `scheme://authority`, junto a los
    /// avisos de seguridad emitidos al establecerlo (#44: degradación TLS).
    ///
    /// # Errors
    /// [`DialError`], que lleva la categoría de siempre —
    /// [`Error::HostKeyUnknown`]/[`Error::HostKeyMismatch`] (flujo TOFU, ADR
    /// 0015 D) o aquella a la que degrade el fallo — y, si se puede contar, el
    /// porqué (#322). Un `Error` se convierte solo con `.into()`: eso es «no
    /// sé explicarlo», que era el comportamiento único antes.
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, DialError>;

    /// La forma CANÓNICA de `authority` para esta conexión (#47, dedup): la
    /// authority con el usuario/puerto EFECTIVOS que usaría el connect —
    /// `sftp://host` que hereda `oscar@` de `connections.toml` canonicaliza a
    /// `oscar@host`, y un puerto default explícito se normaliza fuera. El
    /// Engine cachea la sesión bajo la clave canónica (+ alias la pedida):
    /// dos formas de la misma identidad = UNA sesión.
    ///
    /// Resolución LOCAL y barata (sin red). `None` (default) = sin opinión:
    /// el Engine cachea bajo la authority pedida tal cual.
    async fn canonical_authority(&self, scheme: &str, authority: &str) -> Option<String> {
        let _ = (scheme, authority);
        None
    }

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

    /// Guarda para ESTA sesión el secreto que un humano acaba de teclear
    /// (`connection.provide_secret`, #325).
    ///
    /// El gemelo de [`Self::trust_host_key`], y por el mismo motivo: hay
    /// decisiones que solo puede tomar quien está delante, y el core necesita
    /// una puerta por la que reciban la respuesta. En memoria y nada más.
    ///
    /// # Errors
    /// Implementación dependiente; el conector por defecto no falla.
    async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error>;
}

/// Causa de una degradación de seguridad al conectar (#44). Vocabulario CERRADO:
/// su `wire()` es el `reason` de [`norte_proto::methods::ConnectionDegraded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionWarningReason {
    /// FTP `tls="allow"`: el servidor rechazó `AUTH TLS` → sesión en claro.
    TlsAuthRejected,
    /// FTP-por-plugin (ADR 0033): la sesión es SIEMPRE en claro — FTPS es deuda
    /// (aws-lc-rs no compila a wasm). Credenciales y datos sin cifrar.
    FtpPlaintext,
}

impl ConnectionWarningReason {
    /// El string de wire (cerrado y contractual; ver `ConnectionDegraded.reason`).
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            ConnectionWarningReason::TlsAuthRejected => "tls-auth-rejected",
            ConnectionWarningReason::FtpPlaintext => "ftp-plaintext",
        }
    }
}

/// Causa de que una conexión NO se estableciera (#322). Vocabulario CERRADO:
/// su [`Self::wire`] es el `reason` de [`norte_proto::methods::ConnectionFailed`].
///
/// Un enum y no un `&'static str` suelto, igual que
/// [`ConnectionWarningReason`]: con la cadena a pelo, renombrar un valor aquí
/// no ponía nada rojo —los goldens congelaban una copia distinta— y el efecto
/// era que todos los fallos pasaban a pintarse como «motivo desconocido», en
/// silencio y para siempre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionFailureReason {
    /// No hay secreto en ninguna de las fuentes configuradas.
    SecretMissing,
    /// Lo hay, y está VACÍO — que no es lo mismo (#320).
    SecretEmpty,
    /// Lo hay y no es texto válido.
    SecretNotUtf8,
    /// El almacén de secretos no se pudo leer.
    SecretStore,
    /// El servidor rechazó las credenciales.
    AuthRejected,
    /// Falta el usuario.
    NoUser,
    /// El agente SSH no pudo autenticar.
    Agent,
}

impl ConnectionFailureReason {
    /// El string de wire (cerrado y contractual; ver `ConnectionFailed.reason`).
    ///
    /// Todo lo que devuelva esta función está en
    /// [`norte_proto::methods::CONNECTION_FAILURE_REASONS`], y lo contrario
    /// también: lo prueba `el_vocabulario_de_fallos_es_el_del_proto`.
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
        }
    }

    /// Todas las variantes, para las pruebas exhaustivas del vocabulario.
    ///
    /// Una constante y no un `strum`: son siete y la dependencia no se paga
    /// sola. Si alguien añade una octava y no la mete aquí, el `match` de
    /// [`Self::wire`] sí le obliga a decidir su cadena, y esta lista solo
    /// deja de cubrirla — por eso la prueba compara EN LOS DOS SENTIDOS contra
    /// el proto, que es donde el hueco se vería.
    pub const TODAS: &'static [Self] = &[
        Self::SecretMissing,
        Self::SecretEmpty,
        Self::SecretNotUtf8,
        Self::SecretStore,
        Self::AuthRejected,
        Self::NoUser,
        Self::Agent,
    ];
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

/// Por qué NO se pudo establecer una sesión, en lo que se puede contar (#322).
///
/// Simétrico de [`ConnectionWarning`]: el éxito lleva sus avisos, y el fallo
/// lleva su explicación. Antes no la llevaba, y el resultado era que el
/// frontend recibía una CATEGORÍA —`PermissionDenied`— indistinguible de una
/// clave equivocada, mientras la frase exacta moría en el log del daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionFailure {
    /// Nombre de `connections.toml`, si se abría una con nombre.
    pub conn: Option<String>,
    /// Scheme al que se intentaba conectar.
    pub scheme: String,
    /// Host, sin userinfo (regla 10).
    pub host: String,
    /// Causa, vocabulario CERRADO (ver `methods::ConnectionFailed::reason`).
    pub reason: ConnectionFailureReason,
    /// Frase humana, si la variante puede exponerla — ver
    /// `ConnectError::detalle_publico`. `None` no es un fallo sin explicación:
    /// es una explicación que no puede salir.
    pub detail: Option<String>,
}

/// Observa los avisos de conexión (#44): el daemon lo implementa para difundir
/// `connection.degraded`; la CLI embebida para imprimir por stderr. Inyectado en
/// el [`Engine`](crate::Engine) con `set_connection_observer`.
pub trait ConnectionObserver: Send + Sync {
    /// Un aviso ocurrió al establecer una sesión. Best-effort, no bloqueante.
    fn on_connection_warning(&self, warning: &ConnectionWarning);

    /// Una sesión NO se pudo establecer (#322). Best-effort, no bloqueante.
    ///
    /// Con `default` vacío a propósito: los observadores que solo querían los
    /// avisos siguen compilando, y quien quiera enseñar el porqué lo
    /// implementa. Añadirlo sin default habría roto a los implementadores de
    /// test por una notificación que no les interesa.
    fn on_connection_failure(&self, _failure: &ConnectionFailure) {}
}

/// La causa publicable de un fallo, SIN el destino.
///
/// Se separa del destino porque se conocen en sitios distintos: la causa la
/// sabe quien atrapó el `ConnectError`; el scheme y el host, quien pidió la
/// conexión. Juntarlas antes obligaría a arrastrar el destino por todo el
/// camino de error solo para volver a nombrarlo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Causa {
    /// Nombre de `connections.toml`, si lo había.
    pub conn: Option<String>,
    /// Vocabulario CERRADO (ver `methods::ConnectionFailed::reason`).
    pub reason: ConnectionFailureReason,
    /// La frase, si la variante puede exponerla (regla 10).
    pub detail: Option<String>,
}

/// Un fallo de conexión con lo que se le puede contar a quien mira.
///
/// El error viaja como siempre; la explicación va al lado. Existe porque el
/// `RemoteConnector` devolvía solo la categoría, y ahí es donde se perdía el
/// diagnóstico (#322).
#[derive(Debug, Clone)]
pub struct DialError {
    /// La categoría que acaba en el wire como error de la operación.
    pub error: Error,
    /// Lo publicable del porqué. `None` = este connector no lo sabe explicar,
    /// o la variante no puede exponer su texto.
    ///
    /// En un `Box` porque este tipo viaja en el `Err` de cada `connect`, y el
    /// camino normal es el que NO lleva causa: engordar todos los `Result` del
    /// pool con dos `String` que casi siempre están vacías es pagar el caso
    /// raro en el caso común.
    pub causa: Option<Box<Causa>>,
}

impl From<Error> for DialError {
    /// Un fallo sin explicación publicable: exactamente el comportamiento
    /// anterior a #322. Es lo que hace que un connector que no la aporte —los
    /// dobles de test— no tenga que cambiar.
    fn from(error: Error) -> Self {
        Self { error, causa: None }
    }
}

// Clave de anclaje del journal (M3-5, ADR 0025): vive en norte-connect (el
// dominio de secretos/keyring, regla 10); el CLI la usa vía este re-export.
pub use norte_connect::journal_anchor_key;

// The user config dir — relocated to norte-config (ADR 0035); re-exported
// so the historic `norte_core::connect::config_dir()` path keeps working.
pub use norte_config::config_dir;

/// La URL de la conexión NOMBRADA `name` en `<dir>/connections.toml` (para
/// `norte connect <nombre>`: el frontend traduce el nombre a URL y el
/// establecimiento va por el camino normal del engine).
///
/// # Errors
/// [`Error::NotFound`] si el nombre no existe; [`Error::InvalidPath`] si el
/// fichero no parsea.
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

/// Las conexiones NOMBRADAS de `<dir>/connections.toml`, en orden alfabético
/// (#140): `(nombre, url)`.
///
/// La URL, y jamás un secreto: `ConnectionSpec` referencia sus credenciales
/// (ADR 0015) y esta lista es para pintar un selector.
///
/// Un fichero que no está es una lista VACÍA y no un error: no tener
/// conexiones configuradas es lo normal el primer día. Uno que no parsea sí lo
/// es — decir «no tienes ninguna» cuando lo que pasa es que su fichero tiene
/// una coma de más sería mentir sobre lo que el usuario escribió.
///
/// # Errors
/// [`Error::InvalidPath`] si el fichero existe y no parsea.
pub async fn named_connections(dir: &std::path::Path) -> Result<Vec<(String, String)>, Error> {
    let dir = dir.to_path_buf();
    let file = crate::blocking::spawn_blocking(move || ConnectionsFile::load(&dir))
        .await
        .map_err(|_| Error::Internal { panic: true })?
        .map_err(log_and_map)?;
    Ok(file
        .connections
        .into_iter()
        .map(|(nombre, spec)| (nombre, spec.url))
        .collect())
}

/// La implementación real de [`RemoteConnector`] sobre `norte-connect`.
pub struct ConnectionManager {
    config_dir: PathBuf,
    secrets: SecretResolver,
    ssh: SshConnector,
    s3: S3Connector,
}

impl ConnectionManager {
    /// Manager anclado en `config_dir` (ver [`config_dir()`] para el default).
    ///
    /// FTP ya no lleva un conector aquí: `ftp://` va por el provider-plugin
    /// ([`crate::ftp_plugin`], ADR 0033), que establece la conexión dentro del
    /// guest WASM. El [`norte_connect::FtpConnector`] (TLS host-side) queda
    /// reservado para un futuro FTPS terminado en el host (deuda).
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
        let connected = self
            .establish(&spec, Some(name))
            .await
            .map_err(|d| d.error)?;
        Ok((ep.scheme, authority, connected))
    }

    /// Carga `connections.toml` (I/O síncrona → `spawn_blocking`, regla 2).
    async fn load_connections(&self) -> Result<ConnectionsFile, Error> {
        let dir = self.config_dir.clone();
        crate::blocking::spawn_blocking(move || ConnectionsFile::load(&dir))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(log_and_map)
    }

    /// Establece el transporte para `spec` y construye el provider. `name` es
    /// el nombre de la conexión en `connections.toml` (ad-hoc = None; el
    /// secreto se busca entonces bajo el host).
    ///
    /// # Una contraseña mal tecleada no se queda para siempre (#325)
    ///
    /// Si el secreto salió del escalón de SESIÓN —lo tecleó un humano hace un
    /// momento— y el servidor lo rechaza, se OLVIDA y el fallo se convierte de
    /// nuevo en `SecretNeeded`, o sea en el diálogo. Sin esto, un dedo torcido
    /// dejaba la conexión muerta hasta parar el daemon: el escalón de sesión va
    /// delante de los otros tres, así que el valor malo tapaba también la
    /// variable de entorno con la que se intentaría arreglar, y como el core
    /// solo pregunta cuando no encuentra NADA, el diálogo no volvía a salir.
    ///
    /// Solo el de sesión. Un secreto del entorno, del keyring o del `age` lo
    /// puso alguien a propósito en un sitio que se puede editar: borrárselo
    /// por un rechazo del servidor sería decidir por él que estaba mal.
    async fn establish(
        &self,
        spec: &ConnectionSpec,
        name: Option<&str>,
    ) -> Result<Connected, DialError> {
        // El origen sale por parámetro, y `establish_inner` es una función
        // aparte, por un motivo que costó una prueba con una cuenta de verdad
        // descubrir: los brazos de scheme usan `?`, así que un fallo del
        // transporte RETORNA de la función entera. Un bloque «después del
        // match» dentro de `establish_inner` no se ejecutaba nunca en el único
        // caso que le importa — el del fallo.
        let mut origen = None;
        let resultado = self.establish_inner(spec, name, &mut origen).await;
        if matches!(&resultado, Err(d) if d.error == Error::PermissionDenied)
            && origen == Some(norte_connect::SecretOrigin::Session)
        {
            let conn_name = match spec.endpoint() {
                Ok(ep) => name.map_or(ep.host, ToString::to_string),
                Err(_) => return resultado,
            };
            self.secrets.forget_session(&conn_name);
            tracing::info!(
                conn = %conn_name,
                "el servidor rechazó el secreto tecleado: se olvida y se vuelve a preguntar"
            );
            return Err(secret_needed(&conn_name, spec).into());
        }
        resultado
    }

    /// El cuerpo de [`Self::establish`]. `origen` sale por parámetro porque es
    /// lo único que su envoltorio necesita saber del camino recorrido.
    async fn establish_inner(
        &self,
        spec: &ConnectionSpec,
        name: Option<&str>,
        origen: &mut Option<norte_connect::SecretOrigin>,
    ) -> Result<Connected, DialError> {
        let ep = spec.endpoint().map_err(|e| log_and_dial(e, name))?;
        let mut warnings: Vec<ConnectionWarning> = Vec::new();
        // El secreto solo se resuelve si el método de auth lo puede usar
        // (password/access-key siempre; key para la passphrase). Agent no
        // lleva secreto (en s3, agent = cadena ambiente de opendal).
        // El nombre bajo el que se guarda y se busca el secreto. Para una
        // conexión con entrada es SU clave de `connections.toml`, única por
        // construcción; el `unwrap_or(&ep.host)` es para las ad-hoc, que hoy
        // NO pueden llegar aquí con secreto —`resolve_spec` las fija a
        // `auth = "agent"`, y ese brazo no consulta el resolutor—. Si alguna
        // vez una ad-hoc lleva otro método de auth, este nombre deja de ser
        // único y dos hosts distintos podrían compartir entrada: entonces hace
        // falta una clave que incluya el scheme y el puerto.
        let conn_name = name.unwrap_or(&ep.host);
        let secret: Option<Secret> = match spec.auth {
            AuthMethod::Agent => None,
            AuthMethod::Password | AuthMethod::AccessKey => {
                // Un `prompt` PREGUNTA también cuando lo que hay es la cadena
                // vacía. El vacío sigue siendo un fallo de configuración —#320,
                // y `norte doctor` lo dice— pero devolverlo aquí dejaría al
                // usuario ante un «permiso denegado» opaco teniendo la persona
                // delante y un diálogo listo para preguntarle. Se avisa y se
                // pregunta.
                let hallado = match self.secrets.resolve_with_origin(conn_name, &spec.url).await {
                    Ok(v) => v,
                    Err(e @ norte_connect::ConnectError::SecretEmpty { .. })
                        if spec.secret == norte_connect::SecretSource::Prompt =>
                    {
                        tracing::warn!(conn = %conn_name, error = %e, "secreto vacío: se preguntará");
                        None
                    }
                    Err(e) => return Err(log_and_dial(e, name)),
                };
                // #325: si no está en ninguna parte y la conexión dice que hay
                // que preguntarlo, esto sube por el cable como una PREGUNTA y
                // el frontend abre su diálogo. El core no puede preguntar por
                // su cuenta: su resolver no tiene interfaz de usuario.
                if hallado.is_none() && spec.secret == norte_connect::SecretSource::Prompt {
                    return Err(secret_needed(conn_name, spec).into());
                }
                hallado.map(|(s, origin)| {
                    *origen = Some(origin);
                    s
                })
            }
            // `key`: el secreto es la PASSPHRASE de la clave, y ahí vacío y
            // ausente son lo mismo — una clave sin cifrar no lleva passphrase, y
            // `load_secret_key` trata `Some("")` igual que `None`. Detrás no hay
            // ninguna credencial ambiente que pueda suplantar a otra, que es lo
            // único que hacía peligroso el vacío en #320, así que el rechazo del
            // resolver se deshace AQUÍ: aplicarlo también a `key` convertiría un
            // `NORTE_SECRET_*=""` exportado a lo bruto (un `$(cat …)` que no
            // encontró fichero) en una clave que deja de funcionar, sin ganar
            // nada a cambio.
            AuthMethod::Key => match self.secrets.resolve(conn_name, &spec.url).await {
                Ok(s) => s,
                Err(norte_connect::ConnectError::SecretEmpty { .. }) => None,
                Err(e) => return Err(log_and_dial(e, name)),
            },
        };
        // Un provider plugin sirve el scheme que declara. Se pregunta al
        // catálogo ANTES de los brazos del core para todo scheme que no sea
        // del core: `ftp` incluido, porque su guest embebido es un fallback y
        // un plugin instalado lo sustituye. Los del core no se consultan —el
        // manifiesto ya rechaza reclamarlos, y no consultarlos es la segunda
        // puerta.
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
                // Base "/": los segmentos del VPath son absolutos del server.
                Ok(Connected {
                    provider: Arc::new(
                        SftpProvider::new(session, "/").with_logical_trash(spec.logical_trash),
                    ),
                    warnings,
                })
            }
            "ftp" => {
                // FTP-por-plugin (ADR 0033): el guest WASM establece la conexión
                // sobre `wasi:sockets` gateado. El host resuelve DNS (con filtro
                // anti-SSRF), concede `net` a la IP y llama a `configure`. Sin
                // TLS (FTPS = deuda): SIEMPRE en claro → se avisa al usuario.
                let user = ep.user.clone().unwrap_or_else(|| "anonymous".to_string());
                let password = match (spec.auth, secret.as_ref()) {
                    (AuthMethod::Password, Some(s)) => s.expose().to_string(),
                    // Convención guest: login anónimo.
                    (AuthMethod::Agent, _) => "anonymous".to_string(),
                    (AuthMethod::Password, None) => {
                        return Err(log_and_dial(
                            norte_connect::ConnectError::Secret {
                                conn: ep.host.clone(),
                            },
                            name,
                        ));
                    }
                    // key/access-key no existen en FTP.
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
                // El S3Connector construye y SONDEA el Operator (fail-fast); el
                // provider lo recibe inyectado, sin ver el secret-access-key.
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

    /// `Some` si un provider plugin consentido sirve el scheme de `ep`, con
    /// el resultado de conectarlo; `None` si el scheme es del core o ningún
    /// plugin lo declara, y entonces contestan los brazos del core.
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

    /// El provider plugin APROBADO y ACTIVADO que declara `scheme`, o `None`.
    ///
    /// Redescubre el catálogo en cada conexión: es lo que ya hace el `Backend`
    /// embebido para cada llamada de plugin, y una conexión se establece una
    /// vez y se cachea en el Engine, así que el coste no se repite por op. Un
    /// catálogo que no se puede leer se trata como vacío, con aviso: un
    /// directorio `plugins/` roto no debe dejar sin FTP a nadie.
    async fn resolve_plugin_provider(&self, scheme: &str) -> Option<ResolvedProvider> {
        let dir = self.config_dir.clone();
        let scheme = scheme.to_owned();
        let discovered =
            crate::blocking::spawn_blocking(move || match PluginRegistry::discover(&dir) {
                Ok(reg) => reg.resolve_provider(&scheme),
                Err(e) => {
                    tracing::warn!(error = %e, "el catálogo de plugins no se pudo leer");
                    None
                }
            })
            .await;
        match discovered {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "el descubrimiento del catálogo abortó");
                None
            }
        }
    }

    /// Instancia el guest de un provider plugin bajo las capabilities de SU
    /// manifiesto y lo configura con el endpoint y las credenciales de la
    /// conexión.
    ///
    /// **Red.** El guest no tiene DNS. Si el manifiesto declara `net`, el host
    /// resuelve el endpoint —con el mismo filtro anti-SSRF que el FTP
    /// embebido— y añade `ip:puerto` a la allow-list declarada: el humano
    /// aprobó «red» y la conexión es suya, pero un puerto, no la máquina.
    /// El puerto es el de la URL o el `default-port` de la contribución;
    /// sin ninguno de los dos no se sabe a qué conceder y se rehúsa. Sin
    /// `net` en el manifiesto no hay red, y el endpoint cruza tal cual (un
    /// provider en memoria lo ignora).
    ///
    /// **Binario.** Se leen los bytes de `plugin.wasm`, se hashean y se
    /// comparan con el digest que el catálogo ancló: lo que corre es lo que
    /// el humano aprobó, no lo que haya en esa ruta ahora.
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
            wasm_digest,
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
            // Una clave o un access-key no tienen forma en la interfaz WIT
            // del provider: solo cruza `user` + `password`.
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
                    "sin puerto en la URL ni default-port en el manifiesto: no se concede red"
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
        tracing::info!(plugin = %id, scheme = %ep.scheme, "provider por plugin");

        // Leer, hashear e instanciar (compila cranelift): todo bloqueante
        // (regla 2). El digest se compara ANTES de instanciar.
        let scheme = ep.scheme.clone();
        let provider = crate::blocking::spawn_blocking(move || {
            let bytes = std::fs::read(&wasm).map_err(|_| Error::Unsupported)?;
            if norte_plugin_host::wasm_digest_of(&bytes) != wasm_digest {
                tracing::warn!(plugin = %id, "el plugin.wasm no es el que se aprobó");
                return Err(Error::PermissionDenied);
            }
            let runtime = PluginRuntime::new().map_err(|e| map_runtime_error(&e))?;
            PluginProvider::from_bytes(runtime, &bytes, caps, scheme)
                .map_err(|e| map_runtime_error(&e))
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
    // skip_all: la authority CRUDA no entra al span — un `user:pass@host`
    // que el VPath haya aceptado se rechaza al parsear la conexión, pero el
    // span se abriría ANTES (regla 10). Se loguea redactado tras el parse.
    #[tracing::instrument(level = "info", skip_all)]
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, DialError> {
        let url = format!("{scheme}://{authority}");
        let file = self.load_connections().await.map_err(DialError::from)?;
        let (name, spec) =
            resolve_spec(&file, &url).map_err(|e| DialError::from(log_and_map(e)))?;
        if let Ok(ep) = spec.endpoint() {
            tracing::info!(scheme = %ep.scheme, host = %ep.host, port = ?ep.port,
                conexion = name.as_deref().unwrap_or("(ad-hoc)"), "conectando");
        }
        self.establish(&spec, name.as_deref()).await
    }

    // skip_all por la misma razón que `connect`: la authority cruda puede
    // llevar userinfo que el parse rechazará después (regla 10).
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

    // `skip_all` y no `skip(self)`: el segundo parámetro es una CONTRASEÑA.
    // Con `skip(self)`, `tracing` la formatearía en el span —a nivel info, al
    // fichero y al panel de registro— y la regla 10 se habría roto por la
    // línea más fácil de escribir del parche. El nombre de la conexión se
    // registra a mano, que es lo único que aquí se puede decir.
    #[tracing::instrument(level = "info", skip_all, fields(conn = %conn))]
    async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        self.secrets
            .remember_for_session(conn, norte_connect::Secret::new(secret.to_string()));
        tracing::info!("secreto de conexión recibido del frontend (solo en memoria)");
        Ok(())
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
        // Una conexión AD-HOC —navegar a una URL que no está en el fichero— no
        // pregunta: no hay entrada que declare `secret = "prompt"`, y
        // preguntarle una contraseña a alguien por teclear una URL sería
        // enseñarle a teclear contraseñas en cualquier diálogo que aparezca.
        secret: norte_connect::SecretSource::Stored,
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

/// Puerto default del scheme — constante de matching (nunca viaja): s3 no
/// lleva puerto en la authority (443 nominal); sftp=22, ftp=21. Un scheme de
/// plugin no tiene default que el core conozca: `None`, y entonces solo un
/// puerto explícito casa con un puerto explícito.
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

/// La forma canónica de dedup (#47): la authority del endpoint RESUELTO
/// (usuario efectivo incluido) con el puerto default normalizado fuera —
/// `sftp://h:22` y `sftp://h` canonicalizan igual.
fn canonical_authority_of(ep: &norte_connect::Endpoint) -> String {
    let mut canon = ep.clone();
    if canon.port.is_some() && canon.port == default_port(&canon.scheme) {
        canon.port = None;
    }
    authority_of(&canon)
}

/// La canónica de `url` contra un `connections.toml` ya cargado (separado de
/// [`ConnectionManager::canonical_authority`] para testear puro).
fn canonical_from_file(file: &ConnectionsFile, url: &str) -> Option<String> {
    let (_name, spec) = resolve_spec(file, url).ok()?;
    Some(canonical_authority_of(&spec.endpoint().ok()?))
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
///
/// Se conserva para los sitios que NO saben a qué conexión pertenece el fallo
/// —resolver la URL, listar el fichero—: ahí no hay a quién avisar, y contar
/// «falló una conexión» sin decir cuál sería ruido.
fn log_and_map(e: norte_connect::ConnectError) -> Error {
    tracing::warn!(error = %e, "fallo de conexión remota");
    Error::from(e)
}

/// El vocabulario CERRADO de `connection.failed`, por variante (#322).
///
/// `None` = esta variante no se cuenta. No es lo mismo que «no tiene razón»:
/// es que su explicación no aporta nada a quien mira (un TOFU ya viaja tipado,
/// con su huella) o que no puede salir (regla 10, ver
/// `ConnectError::detalle_publico`).
///
/// Exhaustivo a propósito: una variante nueva no compila hasta que alguien
/// decida si el humano se entera de ella.
#[expect(
    clippy::match_same_arms,
    reason = "dos brazos dan `None` por motivos distintos, y el comentario de \
              cada uno es lo que hay que releer al añadir una variante; \
              fundirlos borra la decisión"
)]
fn razon_de(e: &norte_connect::ConnectError) -> Option<ConnectionFailureReason> {
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
        // Dos motivos distintos para el mismo `None`, y por eso NO se juntan
        // los brazos: el TOFU viaja como error TIPADO con host, puerto,
        // algoritmo y huella —contarlo otra vez como frase sería peor, no
        // mejor—, mientras que el resto o no puede enseñar su texto (regla 10)
        // o no dice nada accionable. Fundirlos borra la razón de cada uno, que
        // es justo lo que hay que releer al añadir una variante.
        C::HostKeyUnknown { .. } | C::HostKeyMismatch { .. } => None,
        C::Config(_)
        | C::InvalidUrl(_)
        | C::Io(_)
        | C::KeyLoad { .. }
        | C::KeyUnsupported { .. }
        | C::Ssh(_)
        | C::KnownHosts(_)
        | C::Ftp(_)
        | C::Tls(_)
        | C::S3(_) => None,
    }
}

/// Como [`log_and_map`], pero conservando la causa para poder CONTARLA (#322).
///
/// Es el punto exacto donde el diagnóstico se perdía: aquí se escribía el
/// `warn!` con la frase exacta y se devolvía solo la categoría.
fn log_and_dial(e: norte_connect::ConnectError, name: Option<&str>) -> DialError {
    tracing::warn!(error = %e, "fallo de conexión remota");
    let causa = razon_de(&e).map(|reason| {
        Box::new(Causa {
            conn: name.map(ToOwned::to_owned),
            reason,
            detail: e.detalle_publico(),
        })
    });
    DialError {
        error: Error::from(e),
        causa,
    }
}

/// La PREGUNTA de #325, con a quién se le va a dar la contraseña.
///
/// No pasa por [`log_and_map`] a propósito: eso registra un `warn!` de «fallo
/// de conexión remota», y esto no es un fallo — es la conexión funcionando
/// como su dueño la configuró. Un `warn!` por cada navegación a una conexión
/// `prompt` llenaría el fichero de log de avisos falsos y, desde #324,
/// encendería el aviso del botón de registro en la barra de paneles.
///
/// El destino sale de [`norte_connect::ConnectionSpec::destination_display`],
/// que enseña el `endpoint =` explícito además de la URL —en `s3` el «host»
/// de la URL es el BUCKET, y quien recibe la credencial firmada es el
/// endpoint— y quita el userinfo de las dos mitades (regla 10). Si la URL no
/// parsea se cae al nombre a secas: quedarse sin preguntar por no poder pintar
/// el destino sería peor.
fn secret_needed(conn: &str, spec: &ConnectionSpec) -> Error {
    let endpoint = spec.destination_display().unwrap_or_default();
    tracing::info!(conn = %conn, endpoint = %endpoint, "falta el secreto: se preguntará");
    Error::SecretNeeded {
        conn: conn.to_string(),
        endpoint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin del vocabulario CERRADO de `ConnectionWarningReason::wire()` (va al
    /// `ConnectionDegraded.reason` del wire; protocol-guardian). Cambiar un
    /// string aquí es un cambio de contrato: este test lo obliga a ser deliberado.
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
        toml::from_str(toml).expect("toml válido")
    }

    /// #325: sin secreto en ninguna parte y con `secret = "prompt"`,
    /// `establish` devuelve `SecretNeeded` ANTES de tocar la red — es una
    /// pregunta, no un fallo de conexión. Con el default (`stored`) sigue su
    /// camino y muere en el transporte, que es el comportamiento de siempre.
    #[tokio::test]
    async fn prompt_sin_secreto_es_secret_needed_antes_de_conectar() {
        let dir = tempfile::tempdir().expect("tmp");
        let mgr = ConnectionManager::new(dir.path());
        // Puerto 1 en loopback: si el brazo de `prompt` NO cortocircuitara,
        // este test fallaría por un error de transporte en vez de por el
        // veredicto — que es justo la distinción que se está fijando.
        let mut spec = min_spec("sftp://nadie@127.0.0.1:1/");
        spec.auth = AuthMethod::Password;
        spec.secret = norte_connect::SecretSource::Prompt;

        // `Connected` no es `Debug` (lleva providers), así que el desenlace se
        // saca a mano en vez de con `expect_err`.
        let Err(err) = mgr
            .establish(&spec, Some("pregunta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("no hay secreto en ninguna parte: debía preguntar");
        };
        assert!(
            matches!(&err, Error::SecretNeeded { conn, .. } if conn == "pregunta"),
            "la pregunta llega ENTERA por el wire, con el nombre de la conexión: {err:?}"
        );

        let Error::SecretNeeded { endpoint, .. } = &err else {
            unreachable!()
        };
        assert_eq!(
            endpoint, "sftp://127.0.0.1:1",
            "y CON el destino: un diálogo de contraseña que no dice a quién se \
             la va a dar no es contestable"
        );

        // Y en s3 el destino son las DOS mitades: el «host» de la URL es el
        // bucket, y quien recibe la credencial firmada es el `endpoint =` —
        // que es justo la pieza que un `connections.toml` ajeno puede apuntar
        // a otro sitio. Enseñar solo el bucket contaría la mitad que no
        // importa. (Salió probando la conexión de verdad, no de un test.)
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
            panic!("s3 sin secreto debía preguntar");
        };
        assert_eq!(endpoint, "s3://mi.bucket @ https://s3.eu-west-1.example");

        // Y una vez contestado, el mismo `establish` deja de preguntar: el
        // escalón 0 del resolutor lo tiene.
        mgr.secrets
            .remember_for_session("pregunta", norte_connect::Secret::new("tecleado".into()));
        let Err(otro) = mgr
            .establish(&spec, Some("pregunta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("el transporte no existe: no puede haber conectado");
        };
        assert!(
            !matches!(otro, Error::SecretNeeded { .. }),
            "ya no pregunta: {otro:?}"
        );
    }

    /// #325 (los tres revisores): una contraseña MAL TECLEADA no puede quedarse
    /// para siempre. El escalón de sesión va delante de los otros tres, así que
    /// un valor equivocado no solo falla — tapa la variable de entorno con la
    /// que se intentaría arreglar, y como el core solo pregunta cuando no
    /// encuentra NADA, el diálogo no volvía a salir. Y el resolutor vive en el
    /// daemon: ni cerrar la interfaz lo limpiaba.
    ///
    /// Aquí se comprueba con `access-key`, donde el secreto de sesión ES la
    /// credencial: opendal responde `PermissionDenied` porque el bucket no
    /// existe en ninguna parte, que es exactamente la forma que tiene un
    /// servidor de decir «esa credencial no vale».
    ///
    /// (Mutación de control: quitar el bloque de `forget_session` de
    /// `establish` deja el `PermissionDenied` y las dos aserciones se caen.)
    #[tokio::test]
    async fn un_secreto_de_sesion_rechazado_se_olvida_y_vuelve_a_preguntar() {
        let dir = tempfile::tempdir().expect("tmp");
        let mgr = ConnectionManager::new(dir.path());
        // Un servidor local que dice 403 a todo: es un RECHAZO DE CREDENCIAL
        // de verdad, sin salir de la máquina. Un puerto cerrado no vale — da
        // un error de transporte, que es justo el caso que este test NO mide,
        // y con él el test pasaba también con el fallo puesto.
        let puerto = servidor_que_deniega().await;
        let mut spec = min_spec("s3://bucket-de-prueba");
        spec.auth = AuthMethod::AccessKey;
        spec.secret = norte_connect::SecretSource::Prompt;
        spec.access_key_id = Some("AKIAEXAMPLE".into());
        spec.endpoint = Some(format!("http://127.0.0.1:{puerto}"));
        spec.region = Some("us-east-1".into());
        spec.addressing = Some(norte_connect::AddressingStyle::Path);

        mgr.secrets
            .remember_for_session("cuenta", norte_connect::Secret::new("mal-tecleada".into()));
        let Err(primero) = mgr
            .establish(&spec, Some("cuenta"))
            .await
            .map_err(|d| d.error)
        else {
            panic!("el servidor deniega: no puede haber conectado");
        };
        assert!(
            matches!(primero, Error::SecretNeeded { .. }),
            "el rechazo vuelve a ser la PREGUNTA, no un «permiso denegado» \
             del que no se sale: {primero:?}"
        );
        assert!(
            !mgr.secrets.forget_session("cuenta"),
            "y el valor malo ya no está: `establish` lo olvidó"
        );
    }

    /// Un puerto local que responde `403` a lo que sea y cierra. Devuelve el
    /// puerto; la tarea muere con el runtime del test.
    async fn servidor_que_deniega() -> u16 {
        use tokio::io::AsyncWriteExt as _;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let puerto = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let _ = s
                    .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = s.shutdown().await;
            }
        });
        puerto
    }

    /// El reverso, y el bug que este test destapó mientras se escribía: la
    /// condición no puede ser «hubo `PermissionDenied` y hay algo en la
    /// sesión». Un `auth = "key"` con la ruta de la clave mal puesta también
    /// sale `PermissionDenied`, y ese secreto NO es el que falló — borrarlo
    /// sacaría un diálogo de contraseña por un fichero que falta, y de paso
    /// tiraría una credencial que sí valía.
    #[tokio::test]
    async fn un_fallo_ajeno_al_secreto_de_sesion_no_lo_borra() {
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
            panic!("la clave no existe: no puede haber conectado");
        };
        assert!(
            !matches!(e, Error::SecretNeeded { .. }),
            "un fallo de clave no es una pregunta de contraseña: {e:?}"
        );
        assert!(
            mgr.secrets.forget_session("cuenta"),
            "y el secreto de sesión sigue donde estaba"
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
            secret: norte_connect::SecretSource::Stored,
        }
    }

    /// #47: la canónica hereda el usuario de la entrada y normaliza fuera el
    /// puerto default — dos formas de la misma identidad, una clave.
    #[test]
    fn canonica_hereda_usuario_y_normaliza_puerto() {
        let f = file(CONNS);
        assert_eq!(
            canonical_from_file(&f, "sftp://work.example:2222").as_deref(),
            Some("oscar@work.example:2222"),
        );
        assert_eq!(
            canonical_from_file(&f, "sftp://oscar@work.example:2222").as_deref(),
            Some("oscar@work.example:2222"),
        );
        // Puerto default explícito se cae de la canónica.
        assert_eq!(
            canonical_from_file(&f, "ftp://backup.example:21").as_deref(),
            Some("backup.example"),
        );
        // Ad-hoc sin entrada: canónica = la propia forma normalizada.
        assert_eq!(
            canonical_from_file(&f, "sftp://nadie@otro.example:22").as_deref(),
            Some("nadie@otro.example"),
        );
        // s3: la authority es el bucket, sin puerto.
        assert_eq!(
            canonical_from_file(&f, "s3://mi-bucket").as_deref(),
            Some("mi-bucket"),
        );
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

    /// La lista que alimenta el selector (#140, y desde #264 también el de la
    /// ventana por `connection.list`): pares `(nombre, url)`, alfabéticos.
    #[tokio::test]
    async fn named_connections_lista_alfabetico_y_sin_secretos() {
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

        let cs = named_connections(dir.path()).await.expect("lista");
        assert_eq!(
            cs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["archivo", "trabajo"],
            "alfabético: el orden del fichero no decide el del selector"
        );
        // Lo que viaja es la URL tal cual, y las credenciales se REFERENCIAN
        // (ADR 0015): no hay nada que resolver ni que filtrar aquí.
        assert_eq!(cs[1].1, "sftp://oscar@servidor.example/datos");
    }

    /// Un fichero que NO ESTÁ es una lista vacía —no tener conexiones es lo
    /// normal el primer día—, pero uno que no PARSEA es un error: decir «no
    /// tienes ninguna» cuando hay una coma de más miente sobre lo que el
    /// usuario escribió.
    #[tokio::test]
    async fn sin_fichero_es_vacio_y_un_fichero_roto_es_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            named_connections(dir.path())
                .await
                .expect("sin fichero no es error")
                .is_empty()
        );

        std::fs::write(dir.path().join("connections.toml"), "esto no es toml [[[")
            .expect("escribir");
        assert!(
            named_connections(dir.path()).await.is_err(),
            "un fichero roto se DICE"
        );
    }

    #[test]
    fn config_dir_respeta_override() {
        // Sin tocar env global (unsafe en edition 2024): solo el camino puro.
        // El override por NORTE_CONFIG_DIR se cubre en el E2E de la CLI.
        let d = config_dir();
        assert!(d.ends_with("norte") || d.as_os_str().len() > 1);
    }
}
