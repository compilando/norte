//! Building the object storage `Operator` (ADR 0016 B, phase 7d): the
//! region/endpoint/credentials live HERE; `ObjectProvider::new` receives the
//! already-configured `Operator` and never sees the secret-access-key
//! (rule 10).
//!
//! No TOFU: S3 goes over TLS/WebPKI, it never emits a `HostKeyUnknown`.

use opendal::Operator;

use crate::error::ConnectError;
use crate::secret::Secret;
use crate::spec::{AddressingStyle, AuthMethod, ConnectionSpec};

/// S3/object storage connector. No state of its own (unlike SSH, which
/// retains `known_hosts`): every `connect` builds a new `Operator`.
#[derive(Debug, Clone, Default)]
pub struct S3Connector {}

impl S3Connector {
    /// S3 connector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds and PROBES an `Operator` for `spec`. The probe (a limit-1
    /// `list`) gives the fail-fast the engine's connect timeout expects: bad
    /// credentials/bucket/network fail here, not on the first operation.
    ///
    /// `secret` = the secret-access-key (only with `auth = "access-key"`);
    /// with `auth = "agent"` opendal's ambient chain is used
    /// (`AWS_*`/profile/IMDS). `auth = "key"`/`"password"` do not apply to
    /// s3.
    ///
    /// # Errors
    /// - [`ConnectError::Config`]: auth/region incoherent with s3.
    /// - [`ConnectError::Secret`]: `access-key` without the secret-access-key.
    /// - projects the probe's error (403 → permission, missing bucket →
    ///   `NotFound`, network → transport).
    // No raw URL/credential fields in the span (rule 10).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn connect(
        &self,
        spec: &ConnectionSpec,
        secret: Option<&Secret>,
    ) -> Result<Operator, ConnectError> {
        let ep = spec.endpoint()?;
        if ep.scheme != "s3" {
            return Err(ConnectError::InvalidUrl(format!(
                "scheme {}:// (the S3 connector only accepts s3://)",
                ep.scheme
            )));
        }
        let bucket = &ep.host; // s3://bucket's authority IS the bucket
        // opendal with default-features=false does NOT auto-register the
        // HTTP transport or the service; idempotent.
        opendal::install_default();

        let mut builder = opendal::services::S3::default().bucket(bucket);

        // An EMPTY field is not a field that was set (#320, security review
        // MINOR-3). Every opendal setter silently discards the empty string
        // (`if !v.is_empty()`), and the result is not a failure but a
        // DIFFERENT destination from the one the user wrote: with
        // `endpoint = ""` our code takes the "own endpoint" branch —
        // path-style, without the `http://` warning— and opendal then falls
        // through to the AWS endpoint, so the bucket, the access-key-id and
        // the signature end up at Amazon while the user believes they are
        // talking to their MinIO. With `region = ""` the signing region comes
        // from the environment's `AWS_REGION`, and THAT fallback is not
        // gated by `disable_config_load`. They are rejected before touching
        // anything.
        for (field, value) in [("region", &spec.region), ("endpoint", &spec.endpoint)] {
            if value.as_deref().is_some_and(str::is_empty) {
                return Err(ConnectError::Config(format!(
                    "`{field}` is present and EMPTY in the s3 connection: give it a value or remove the key"
                )));
            }
        }

        // Region: mandatory against AWS; with a custom endpoint (MinIO)
        // us-east-1 is assumed if missing (convention; the server ignores
        // it).
        match (&spec.region, &spec.endpoint) {
            (Some(r), _) => builder = builder.region(r),
            (None, Some(_)) => builder = builder.region("us-east-1"),
            (None, None) => {
                return Err(ConnectError::Config(
                    "an s3 connection without `endpoint` (AWS) requires `region`".to_string(),
                ));
            }
        }
        if let Some(endpoint) = &spec.endpoint {
            // ADR 0015/0016 promises that http (no TLS) is a VISIBLE opt-in:
            // it is warned about (the endpoint is not secret — it is
            // loggable). In the clear travel the data, the access_key_id and,
            // with `agent`+IMDS, the X-Amz-Security-Token (a replayable
            // bearer) — MITM-able.
            if endpoint.starts_with("http://") {
                tracing::warn!(
                    endpoint = %endpoint,
                    "s3 connection over unencrypted HTTP (insecure): data and credentials in the clear"
                );
            }
            builder = builder.endpoint(endpoint);
        }

        // Addressing: virtual-host by default without an endpoint (AWS),
        // path-style with a custom endpoint (MinIO); overridable.
        let virtual_host = match spec.addressing {
            Some(AddressingStyle::VirtualHost) => true,
            Some(AddressingStyle::Path) => false,
            None => spec.endpoint.is_none(),
        };
        if virtual_host {
            builder = builder.enable_virtual_host_style();
        }

        // Credentials.
        match spec.auth {
            AuthMethod::AccessKey => {
                // Empty is as invalid as absent, and in BOTH halves: the
                // static provider is gated on `(access_key_id,
                // secret_access_key)` and opendal discards the empty string
                // in every setter, so an `access_key_id = ""` reproduces all
                // of #320 even if the secret is fine. Checked HERE, in the
                // layer that holds the danger, and not only in the resolver:
                // this connector is public API and the resolver is not its
                // only possible caller. See ADR 0015 (2026-08-31 amendment)
                // and #321.
                let key_id = spec
                    .access_key_id
                    .as_deref()
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| {
                        ConnectError::Config(
                            "auth = \"access-key\" requires a NON-EMPTY `access_key_id` in \
                             connections.toml"
                                .to_string(),
                        )
                    })?;
                let sk = secret.filter(|s| !s.expose().is_empty()).ok_or_else(|| {
                    ConnectError::Secret {
                        conn: bucket.clone(),
                    }
                })?;
                builder = builder
                    .access_key_id(key_id)
                    .secret_access_key(sk.expose())
                    // Determinism, and NOT via these two flags: in opendal
                    // 0.58 they only turn off env, profile and IMDS — SSO,
                    // web-identity, process and ECS remain in the chain
                    // (#321). What actually guarantees it is that the static
                    // provider comes in first and wins; the chain is only
                    // reached if there are no explicit credentials, which is
                    // exactly what the guards above prevent.
                    .disable_config_load()
                    .disable_ec2_metadata();
            }
            // `agent` on s3 = opendal's ambient chain (AWS_*/profile/IMDS):
            // the CI/instance-with-role case. The credentials builder is not
            // touched.
            AuthMethod::Agent => {}
            AuthMethod::Key | AuthMethod::Password => {
                return Err(ConnectError::Config(
                    "s3 uses auth = \"access-key\" (or \"agent\" for the ambient chain), not \
                     \"key\"/\"password\""
                        .to_string(),
                ));
            }
        }

        let op = Operator::new(builder).map_err(|e| map_opendal(&e))?;
        // Fail-fast probe: `list` (NOT `lister`, which is lazy and would not
        // touch the network) of 1 entry does a real ListObjectsV2 — it
        // validates credentials + bucket + connectivity without depending on
        // a specific key.
        op.list_with("")
            .limit(1)
            .await
            .map_err(|e| map_opendal(&e))?;
        Ok(op)
    }
}

/// Projects an opendal error (construction or probing) onto `ConnectError`.
/// Degrades to a CATEGORY (`ErrorKind`), without re-emitting opendal's
/// message (which could carry the endpoint) or ever the secret (it stays in
/// the builder).
fn map_opendal(e: &opendal::Error) -> ConnectError {
    use opendal::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => ConnectError::AuthFailed {
            user: "(s3)".to_string(),
            host: "(s3)".to_string(),
        },
        ErrorKind::ConfigInvalid => {
            ConnectError::Config("invalid s3 configuration (endpoint/region/bucket)".to_string())
        }
        // Missing/inaccessible bucket: retrying does NOT fix it — projected
        // as invalid config (non-retryable), not as transport.
        ErrorKind::NotFound => ConnectError::Config("s3 bucket not found or no access".to_string()),
        // Network, service, rate-limit: the provider "is not responding"
        // (retryable).
        other => ConnectError::S3(format!("{other}")),
    }
}
