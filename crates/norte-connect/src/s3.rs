//! Construcción del `Operator` de object storage (ADR 0016 B, fase 7d): la
//! región/endpoint/credenciales viven AQUÍ; `ObjectProvider::new` recibe el
//! `Operator` ya configurado y jamás ve el secret-access-key (regla 10).
//!
//! Sin TOFU: S3 va por TLS/WebPKI, nunca emite un `HostKeyUnknown`.

use opendal::Operator;

use crate::error::ConnectError;
use crate::secret::Secret;
use crate::spec::{AddressingStyle, AuthMethod, ConnectionSpec};

/// Conector S3/object storage. Sin estado propio (a diferencia de SSH, que
/// retiene `known_hosts`): cada `connect` construye un `Operator` nuevo.
#[derive(Debug, Clone, Default)]
pub struct S3Connector {}

impl S3Connector {
    /// Conector S3.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construye y SONDEA un `Operator` para `spec`. El sondeo (un `list`
    /// limit-1) da el fail-fast que el timeout de connect del engine espera:
    /// credenciales/bucket/red malos fallan aquí, no en la primera operación.
    ///
    /// `secret` = el secret-access-key (solo con `auth = "access-key"`); con
    /// `auth = "agent"` se usa la cadena ambiente de opendal (`AWS_*`/perfil/
    /// IMDS). `auth = "key"`/`"password"` no aplican a s3.
    ///
    /// # Errors
    /// - [`ConnectError::Config`]: auth/region incoherentes con s3.
    /// - [`ConnectError::Secret`]: `access-key` sin el secret-access-key.
    /// - proyecta el error del sondeo (403 → permiso, bucket ausente →
    ///   `NotFound`, red → transporte).
    // Sin campos crudos de la URL/credenciales en el span (regla 10).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<Operator, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "s3" {
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (el conector S3 solo acepta s3://)",
                ep.scheme
            )));
        }
        let bucket = &ep.host; // la authority de s3://bucket ES el bucket
        // opendal con default-features=false NO auto-registra el transporte
        // HTTP ni el servicio; idempotente.
        opendal::install_default();

        let mut builder = opendal::services::S3::default().bucket(bucket);

        // Región: obligatoria contra AWS; con endpoint custom (MinIO) se asume
        // us-east-1 si falta (convención; el servidor la ignora).
        match (&spec.region, &spec.endpoint) {
            (Some(r), _) => builder = builder.region(r),
            (None, Some(_)) => builder = builder.region("us-east-1"),
            (None, None) => {
                return Err(ConnectError::Config(
                    "la conexión s3 sin `endpoint` (AWS) exige `region`".to_string(),
                ));
            }
        }
        if let Some(endpoint) = &spec.endpoint {
            // El ADR 0015/0016 promete que http (sin TLS) es opt-in VISIBLE:
            // se avisa (el endpoint no es secreto — es logueable). En claro
            // viajan los datos, el access_key_id y, con `agent`+IMDS, el
            // X-Amz-Security-Token (bearer replayable) — MITM-able.
            if endpoint.starts_with("http://") {
                tracing::warn!(
                    endpoint = %endpoint,
                    "conexión s3 por HTTP sin cifrar (inseguro): datos y credenciales en claro"
                );
            }
            builder = builder.endpoint(endpoint);
        }

        // Direccionamiento: virtual-host por defecto sin endpoint (AWS),
        // path-style con endpoint custom (MinIO); overridable.
        let virtual_host = match spec.addressing {
            Some(AddressingStyle::VirtualHost) => true,
            Some(AddressingStyle::Path) => false,
            None => spec.endpoint.is_none(),
        };
        if virtual_host {
            builder = builder.enable_virtual_host_style();
        }

        // Credenciales.
        match spec.auth {
            AuthMethod::AccessKey => {
                let key_id = spec.access_key_id.as_deref().ok_or_else(|| {
                    ConnectError::Config(
                        "auth = \"access-key\" exige `access_key_id` en connections.toml"
                            .to_string(),
                    )
                })?;
                let sk = secret.ok_or_else(|| ConnectError::Secret {
                    conn: bucket.clone(),
                })?;
                builder = builder
                    .access_key_id(key_id)
                    .secret_access_key(sk.expose())
                    // Determinismo: con credenciales explícitas NO se mezcla la
                    // cadena ambiente (ni perfil ni IMDS podrían suplantarlas).
                    .disable_config_load()
                    .disable_ec2_metadata();
            }
            // `agent` en s3 = cadena ambiente de opendal (AWS_*/perfil/IMDS):
            // el caso CI/instancia con rol. No se toca el builder de creds.
            AuthMethod::Agent => {}
            AuthMethod::Key | AuthMethod::Password => {
                return Err(ConnectError::Config(
                    "s3 usa auth = \"access-key\" (o \"agent\" para la cadena ambiente), no \
                     \"key\"/\"password\""
                        .to_string(),
                ));
            }
        }

        let op = Operator::new(builder).map_err(|e| map_opendal(&e))?;
        // Sondeo fail-fast: `list` (NO `lister`, que es perezoso y no tocaría
        // la red) de 1 entrada hace un ListObjectsV2 real — valida
        // credenciales + bucket + conectividad sin depender de una key concreta.
        op.list_with("")
            .limit(1)
            .await
            .map_err(|e| map_opendal(&e))?;
        Ok(op)
    }
}

/// Proyecta un error de opendal (construcción o sondeo) a `ConnectError`. Se
/// degrada a CATEGORÍA (`ErrorKind`), sin re-emitir el mensaje de opendal (que
/// podría llevar el endpoint) ni jamás el secreto (queda en el builder).
fn map_opendal(e: &opendal::Error) -> ConnectError {
    use opendal::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => ConnectError::AuthFailed {
            user: "(s3)".to_string(),
            host: "(s3)".to_string(),
        },
        ErrorKind::ConfigInvalid => {
            ConnectError::Config("configuración s3 inválida (endpoint/region/bucket)".to_string())
        }
        // Bucket inexistente/inaccesible: reintentar NO lo arregla — se
        // proyecta como config inválida (no-retryable), no como transporte.
        ErrorKind::NotFound => {
            ConnectError::Config("bucket s3 no encontrado o sin acceso".to_string())
        }
        // Red, servicio, rate-limit: el provider "no responde" (retryable).
        other => ConnectError::S3(format!("{other}")),
    }
}
