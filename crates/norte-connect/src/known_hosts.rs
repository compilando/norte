//! TOFU store for SSH host keys (ADR 0015 D): our own `known_hosts` (OpenSSH
//! format) in the config dir, overridable with `NORTE_KNOWN_HOSTS` for
//! CI/headless.
//!
//! First connection to a host → [`HostKeyStatus::Unknown`] (the core raises
//! it to `Error::HostKeyUnknown`, the frontend confirms and
//! `connection.trust_host_key` registers the key). A key that CHANGES →
//! [`HostKeyStatus::Mismatch`] (possible MITM): never accepted silently.

use std::path::{Path, PathBuf};

use russh::keys::{HashAlg, PublicKey};

use crate::error::ConnectError;

/// File name inside the config dir.
const KNOWN_HOSTS_FILE: &str = "known_hosts";
/// Env var that forces an alternate path (CI/headless, ADR 0015 D).
const KNOWN_HOSTS_ENV: &str = "NORTE_KNOWN_HOSTS";

/// Result of comparing the key a server presented against the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostKeyStatus {
    /// The key matches the one on record: connection allowed.
    Known,
    /// Host not on record (first TOFU contact): needs confirmation.
    Unknown {
        /// Algorithm of the presented key (e.g. `ssh-ed25519`).
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint of the presented key.
        fingerprint: String,
    },
    /// Host on record with a DIFFERENT key: possible MITM.
    Mismatch {
        /// Algorithm of the presented key.
        algo: String,
        /// OpenSSH `SHA256:<base64>` fingerprint of the presented key.
        fingerprint: String,
    },
}

/// OpenSSH-`known_hosts`-style host key store (compatible format: it can be
/// pre-populated by copying lines from `~/.ssh/known_hosts`).
#[derive(Debug, Clone)]
pub struct KnownHostsStore {
    path: PathBuf,
}

impl KnownHostsStore {
    /// Store at `<config_dir>/known_hosts`, unless `NORTE_KNOWN_HOSTS` points
    /// at another path (override for CI/headless).
    #[must_use]
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: resolve_path(config_dir, std::env::var_os(KNOWN_HOSTS_ENV)),
        }
    }

    /// Store at an explicit path (tests, or already-resolved paths).
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Compares the key `key` presented by `host:port` against the store.
    ///
    /// SYNCHRONOUS (file I/O): in an async context it goes through
    /// `spawn_blocking`.
    pub(crate) fn check(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
    ) -> Result<HostKeyStatus, ConnectError> {
        // russh swallows ANY open error and returns "no entries": an
        // existing but unreadable file would silently degrade the pinning to
        // TOFU. Fail-closed: exists + cannot read = error.
        match std::fs::File::open(&self.path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ConnectError::KnownHosts(format!(
                    "exists but cannot be read: {e}"
                )));
            }
        }
        match russh::keys::check_known_hosts_path(host, port, key, &self.path) {
            Ok(true) => Ok(HostKeyStatus::Known),
            // NOTE: russh returns `false` for BOTH "host never seen" AND
            // "host pinned with a key of ANOTHER algorithm" (it compares
            // algo+key). Without disambiguating, a MITM forcing an algorithm
            // downgrade (ed25519 pinned → presents rsa/ecdsa) would show up
            // as a benign first contact instead of a possible MITM (ADR
            // 0015 D).
            Ok(false) => {
                let pinned = russh::keys::known_hosts::known_host_keys_path(host, port, &self.path)
                    .map_err(|e| ConnectError::KnownHosts(e.to_string()))?;
                if pinned.is_empty() {
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

    /// Registers `key` as the host key for `host:port` (after explicit user
    /// confirmation — the `connection.trust_host_key` flow).
    ///
    /// SYNCHRONOUS (file I/O): in an async context it goes through
    /// `spawn_blocking`.
    pub(crate) fn learn(&self, host: &str, port: u16, key: &PublicKey) -> Result<(), ConnectError> {
        russh::keys::known_hosts::learn_known_hosts_path(host, port, key, &self.path)
            .map_err(|e| ConnectError::KnownHosts(e.to_string()))
    }
}

/// Effective path of the store: the env override wins over the config dir.
fn resolve_path(config_dir: &Path, env_override: Option<std::ffi::OsString>) -> PathBuf {
    env_override.map_or_else(|| config_dir.join(KNOWN_HOSTS_FILE), PathBuf::from)
}

/// OpenSSH `SHA256:<base64>` fingerprint — the SAME string that travels in
/// `Error::HostKeyUnknown` and that `connection.trust_host_key` returns.
pub(crate) fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Name of the key's algorithm (e.g. `ssh-ed25519`).
pub(crate) fn algo(key: &PublicKey) -> String {
    key.algorithm().to_string()
}

#[cfg(test)]
mod tests {
    use russh::keys::{Algorithm, PrivateKey};

    use super::*;

    fn key() -> PublicKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .expect("generate test ed25519")
            .public_key()
            .clone()
    }

    #[test]
    fn primer_contacto_es_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let k = key();
        let st = store.check("example.com", 22, &k).unwrap();
        let HostKeyStatus::Unknown { algo, fingerprint } = st else {
            panic!("expected Unknown, got {st:?}");
        };
        assert_eq!(algo, "ssh-ed25519");
        assert!(
            fingerprint.starts_with("SHA256:"),
            "OpenSSH format, got {fingerprint}"
        );
    }

    #[test]
    fn learn_registra_y_check_reconoce() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let k = key();
        store.learn("example.com", 2222, &k).unwrap();
        assert_eq!(
            store.check("example.com", 2222, &k).unwrap(),
            HostKeyStatus::Known
        );
        // Different port = different identity: still first contact.
        assert!(matches!(
            store.check("example.com", 22, &k).unwrap(),
            HostKeyStatus::Unknown { .. }
        ));
        // Different host, same thing.
        assert!(matches!(
            store.check("otro.example.com", 2222, &k).unwrap(),
            HostKeyStatus::Unknown { .. }
        ));
    }

    #[test]
    fn clave_cambiada_es_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let registered = key();
        let impostor = key();
        store.learn("example.com", 22, &registered).unwrap();
        let st = store.check("example.com", 22, &impostor).unwrap();
        let HostKeyStatus::Mismatch { fingerprint, .. } = st else {
            panic!("expected Mismatch, got {st:?}");
        };
        // The reported fingerprint is the PRESENTED (suspicious) key's,
        // which is what the frontend must show.
        assert_eq!(fingerprint, super::fingerprint(&impostor));
    }

    /// CROSS-ALGORITHM key change: a MITM forcing the negotiation to another
    /// algorithm (e.g. from ed25519 to ecdsa/rsa) must NOT be seen as a
    /// benign first contact — the host ALREADY has a pinned key: it is a
    /// Mismatch (ADR 0015 D; russh alone, without disambiguating, would
    /// return "not found").
    #[test]
    fn clave_de_otro_algoritmo_es_mismatch_no_unknown() {
        use russh::keys::EcdsaCurve;
        let dir = tempfile::tempdir().unwrap();
        let store = KnownHostsStore::at(dir.path().join("kh"));
        let pinned = key(); // ed25519
        let presented = PrivateKey::random(
            &mut rand::rng(),
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        )
        .expect("generate test ecdsa")
        .public_key()
        .clone();
        store.learn("example.com", 22, &pinned).unwrap();
        let st = store.check("example.com", 22, &presented).unwrap();
        let HostKeyStatus::Mismatch { fingerprint, .. } = st else {
            panic!("expected Mismatch (algorithm downgrade), got {st:?}");
        };
        assert_eq!(fingerprint, super::fingerprint(&presented));
    }

    /// A `known_hosts` that EXISTS but cannot be read is a fail-closed error,
    /// not a silent TOFU (russh alone would swallow the open error and every
    /// host would become "first contact" again).
    #[cfg(unix)]
    #[test]
    fn fichero_ilegible_es_error_no_tofu() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        let store = KnownHostsStore::at(&path);
        store.learn("example.com", 22, &key()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            store.check("example.com", 22, &key()).is_err(),
            "unreadable must be an error, not Unknown"
        );
    }

    #[test]
    fn fichero_corrupto_es_error_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kh");
        std::fs::write(&path, "example.com ssh-ed25519 not-base64!!\n").unwrap();
        let store = KnownHostsStore::at(&path);
        assert!(store.check("example.com", 22, &key()).is_err());
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
