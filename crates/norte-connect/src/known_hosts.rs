//! Store TOFU de host keys SSH (ADR 0015 D): un `known_hosts` propio (formato
//! OpenSSH) en el dir de config, con override por `NORTE_KNOWN_HOSTS` para
//! CI/headless.
//!
//! Primera conexión a un host → [`HostKeyStatus::Unknown`] (el core lo eleva a
//! `Error::HostKeyUnknown`, el frontend confirma y `connection.trust_host_key`
//! registra la clave). Clave que CAMBIA → [`HostKeyStatus::Mismatch`] (posible
//! MITM): jamás se acepta en silencio.

use std::path::{Path, PathBuf};

use russh::keys::{HashAlg, PublicKey};

use crate::error::ConnectError;

/// Nombre del fichero dentro del dir de config.
const KNOWN_HOSTS_FILE: &str = "known_hosts";
/// Env var que fuerza una ruta alternativa (CI/headless, ADR 0015 D).
const KNOWN_HOSTS_ENV: &str = "NORTE_KNOWN_HOSTS";

/// Resultado de comparar la clave presentada por un servidor con el store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostKeyStatus {
    /// La clave coincide con la registrada: conexión permitida.
    Known,
    /// Host sin registrar (primer contacto TOFU): requiere confirmación.
    Unknown {
        /// Algoritmo de la clave presentada (p. ej. `ssh-ed25519`).
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` de la clave presentada.
        fingerprint: String,
    },
    /// Host registrado con OTRA clave: posible MITM.
    Mismatch {
        /// Algoritmo de la clave presentada.
        algo: String,
        /// Fingerprint OpenSSH `SHA256:<base64>` de la clave presentada.
        fingerprint: String,
    },
}

/// Store de host keys estilo `known_hosts` de OpenSSH (formato compatible:
/// se puede pre-poblar copiando líneas de `~/.ssh/known_hosts`).
#[derive(Debug, Clone)]
pub struct KnownHostsStore {
    path: PathBuf,
}

impl KnownHostsStore {
    /// Store en `<config_dir>/known_hosts`, salvo que `NORTE_KNOWN_HOSTS`
    /// apunte a otra ruta (override para CI/headless).
    #[must_use]
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: resolve_path(config_dir, std::env::var_os(KNOWN_HOSTS_ENV)),
        }
    }

    /// Store en una ruta explícita (tests, o rutas ya resueltas).
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Compara la clave `key` presentada por `host:port` con el store.
    ///
    /// SÍNCRONO (I/O de fichero): en contexto async va por `spawn_blocking`.
    pub(crate) fn check(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
    ) -> Result<HostKeyStatus, ConnectError> {
        // russh traga CUALQUIER error de apertura y devuelve "sin entradas":
        // un fichero existente pero ilegible degradaría el pinning a TOFU en
        // silencio. Fail-closed: existir + no poder leer = error.
        match std::fs::File::open(&self.path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ConnectError::KnownHosts(format!(
                    "existe pero no se puede leer: {e}"
                )));
            }
        }
        match russh::keys::check_known_hosts_path(host, port, key, &self.path) {
            Ok(true) => Ok(HostKeyStatus::Known),
            // OJO: russh devuelve `false` TANTO para "host nunca visto" COMO
            // para "host fijado con clave de OTRO algoritmo" (compara algo+
            // clave). Sin desambiguar, un MITM que fuerza el downgrade de
            // algoritmo (ed25519 fijada → presenta rsa/ecdsa) aparecería como
            // primer contacto benigno en vez de como posible MITM (ADR 0015 D).
            Ok(false) => {
                let fijadas =
                    russh::keys::known_hosts::known_host_keys_path(host, port, &self.path)
                        .map_err(|e| ConnectError::KnownHosts(e.to_string()))?;
                if fijadas.is_empty() {
                    Ok(HostKeyStatus::Unknown {
                        algo: algo(key),
                        fingerprint: fingerprint(key),
                    })
                } else {
                    Ok(HostKeyStatus::Mismatch {
                        algo: algo(key),
                        fingerprint: fingerprint(key),
                    })
                }
            }
            Err(russh::keys::Error::KeyChanged { .. }) => Ok(HostKeyStatus::Mismatch {
                algo: algo(key),
                fingerprint: fingerprint(key),
            }),
            Err(e) => Err(ConnectError::KnownHosts(e.to_string())),
        }
    }

    /// Registra `key` como la host key de `host:port` (tras confirmación
    /// explícita del usuario — flujo `connection.trust_host_key`).
    ///
    /// SÍNCRONO (I/O de fichero): en contexto async va por `spawn_blocking`.
    pub(crate) fn learn(&self, host: &str, port: u16, key: &PublicKey) -> Result<(), ConnectError> {
        russh::keys::known_hosts::learn_known_hosts_path(host, port, key, &self.path)
            .map_err(|e| ConnectError::KnownHosts(e.to_string()))
    }
}

/// Ruta efectiva del store: el override de env gana sobre el dir de config.
fn resolve_path(config_dir: &Path, env_override: Option<std::ffi::OsString>) -> PathBuf {
    env_override.map_or_else(|| config_dir.join(KNOWN_HOSTS_FILE), PathBuf::from)
}

/// Fingerprint OpenSSH `SHA256:<base64>` — la MISMA cadena que viaja en
/// `Error::HostKeyUnknown` y que `connection.trust_host_key` devuelve.
pub(crate) fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Nombre del algoritmo de la clave (p. ej. `ssh-ed25519`).
pub(crate) fn algo(key: &PublicKey) -> String {
    key.algorithm().to_string()
}

#[cfg(test)]
mod tests {
    use russh::keys::{Algorithm, PrivateKey};

    use super::*;

    fn clave() -> PublicKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .expect("generar ed25519 de test")
            .public_key()
            .clone()
    }

    #[test]
    fn primer_contacto_es_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let k = clave();
        let st = store.check("example.com", 22, &k).unwrap();
        let HostKeyStatus::Unknown { algo, fingerprint } = st else {
            panic!("esperaba Unknown, fue {st:?}");
        };
        assert_eq!(algo, "ssh-ed25519");
        assert!(
            fingerprint.starts_with("SHA256:"),
            "formato OpenSSH, fue {fingerprint}"
        );
    }

    #[test]
    fn learn_registra_y_check_reconoce() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let k = clave();
        store.learn("example.com", 2222, &k).unwrap();
        assert_eq!(
            store.check("example.com", 2222, &k).unwrap(),
            HostKeyStatus::Known
        );
        // Otro puerto = otra identidad: sigue siendo primer contacto.
        assert!(matches!(
            store.check("example.com", 22, &k).unwrap(),
            HostKeyStatus::Unknown { .. }
        ));
        // Otro host, ídem.
        assert!(matches!(
            store.check("otro.example.com", 2222, &k).unwrap(),
            HostKeyStatus::Unknown { .. }
        ));
    }

    #[test]
    fn clave_cambiada_es_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let registrada = clave();
        let impostora = clave();
        store.learn("example.com", 22, &registrada).unwrap();
        let st = store.check("example.com", 22, &impostora).unwrap();
        let HostKeyStatus::Mismatch { fingerprint, .. } = st else {
            panic!("esperaba Mismatch, fue {st:?}");
        };
        // El fingerprint reportado es el de la clave PRESENTADA (la sospechosa),
        // que es lo que el frontend debe enseñar.
        assert_eq!(fingerprint, super::fingerprint(&impostora));
    }

    /// Cambio de clave CROSS-ALGORITMO: un MITM que fuerza la negociación a
    /// otro algoritmo (p. ej. de ed25519 a ecdsa/rsa) NO debe verse como
    /// primer contacto benigno — el host YA tiene clave fijada: es Mismatch
    /// (ADR 0015 D; russh solo, sin desambiguar, devolvería "no encontrada").
    #[test]
    fn clave_de_otro_algoritmo_es_mismatch_no_unknown() {
        use russh::keys::EcdsaCurve;
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let fijada = clave(); // ed25519
        let presentada = PrivateKey::random(
            &mut rand::rng(),
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        )
        .expect("generar ecdsa de test")
        .public_key()
        .clone();
        store.learn("example.com", 22, &fijada).unwrap();
        let st = store.check("example.com", 22, &presentada).unwrap();
        let HostKeyStatus::Mismatch { fingerprint, .. } = st else {
            panic!("esperaba Mismatch (downgrade de algoritmo), fue {st:?}");
        };
        assert_eq!(fingerprint, super::fingerprint(&presentada));
    }

    /// Un `known_hosts` que EXISTE pero no se puede leer es error fail-closed,
    /// no un TOFU silencioso (russh solo tragaría el error de apertura y todo
    /// host volvería a ser "primer contacto").
    #[cfg(unix)]
    #[test]
    fn fichero_ilegible_es_error_no_tofu() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        let store = KnownHostsStore::at(&path);
        store.learn("example.com", 22, &clave()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            store.check("example.com", 22, &clave()).is_err(),
            "ilegible debe ser error, no Unknown"
        );
    }

    #[test]
    fn fichero_corrupto_es_error_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        std::fs::write(&path, "example.com ssh-ed25519 no-es-base64!!\n").unwrap();
        let store = KnownHostsStore::at(&path);
        assert!(store.check("example.com", 22, &clave()).is_err());
    }

    #[test]
    fn ruta_por_defecto_y_override() {
        let dir = Path::new("/cfg");
        assert_eq!(
            resolve_path(dir, None),
            Path::new("/cfg").join(KNOWN_HOSTS_FILE)
        );
        assert_eq!(
            resolve_path(dir, Some("/ci/known_hosts".into())),
            PathBuf::from("/ci/known_hosts")
        );
    }
}
