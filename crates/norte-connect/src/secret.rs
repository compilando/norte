//! Secret resolution (ADR 0015 C): `NORTE_SECRET_<CONN>` (env) → OS keyring →
//! encrypted `secrets.age` file. The secret is wrapped in [`Secret`] (wiped
//! from memory when dropped, spec §265) and is NEVER logged or printed
//! (rule 10). The plaintext intermediates (decrypted map, passphrase) are
//! kept zeroized end to end.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use zeroize::{Zeroize, Zeroizing};

use crate::error::{ConnectError, SecretOrigin};

/// Service under which secrets are stored in the OS keyring.
const KEYRING_SERVICE: &str = "norte";
/// Encrypted secrets file inside the config dir.
const SECRETS_FILE: &str = "secrets.age";
/// Optional file with the `secrets.age` passphrase (must be 0600).
const SECRETS_KEY_FILE: &str = "secrets.key";

/// A secret (password/passphrase) that is wiped from memory when dropped and
/// NEVER printed. Its content only comes out through [`Secret::expose`].
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wraps a secret.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// The plaintext content. Use it as late and as briefly as possible;
    /// never log it (rule 10).
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the content (rule 10).
        f.write_str("Secret(***)")
    }
}

/// conn→decrypted-secret map from `secrets.age`. Zeroizes ALL its values when
/// dropped (spec §265): the plaintext leaves no residue on the heap.
#[derive(Default)]
struct SecretMap(BTreeMap<String, String>);

impl Drop for SecretMap {
    fn drop(&mut self) {
        for v in self.0.values_mut() {
            v.zeroize();
        }
    }
}

/// Resolves a connection's secret in the order env → keyring → `age` → what
/// the human typed in THIS session.
#[derive(Debug, Clone)]
pub struct SecretResolver {
    config_dir: PathBuf,
    /// What a human answered, per connection, for this session (#325).
    ///
    /// In memory and NEVER on disk: it dies with the process. It goes ahead
    /// of the three file-based rungs because if it was already asked once,
    /// asking again on every navigation would be unacceptable — and behind
    /// nothing, because what the human just typed is more recent than
    /// anything already stored.
    ///
    /// `Mutex` and not `RwLock`: touched once per connection, not on any hot
    /// path.
    session: Arc<Mutex<BTreeMap<String, Secret>>>,
}

impl SecretResolver {
    /// Resolver anchored at the config dir (where `secrets.age` lives).
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            session: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Stores for THIS session what a human just typed (#325).
    ///
    /// In memory and nothing more: there is no write-to-disk path from here,
    /// on purpose. Saving a secret for real is a separate decision —and
    /// today, on Linux, there is not even anywhere to do it: the keyring is
    /// an opt-in feature nobody turns on and `secrets.age` has no write path
    /// from the interface.
    pub fn remember_for_session(&self, conn: &str, secret: Secret) {
        let mut s = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        s.insert(conn.to_string(), secret);
    }

    /// Forgets what was remembered for `conn`, if there was anything. Returns
    /// whether there was.
    ///
    /// **Without this, a mistyped password is permanent.** The session rung
    /// goes AHEAD of the other three, so a wrong value not only fails: it
    /// covers up the environment variable the user would try to fix it with,
    /// and it stops the dialog from ever coming back (the core only asks
    /// when it finds NOTHING). And since the resolver lives in the daemon,
    /// not even closing the interface clears it. Whoever sees the server
    /// reject a session credential has to call this.
    pub fn forget_session(&self, conn: &str) -> bool {
        let mut s = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        s.remove(conn).is_some()
    }

    /// What was remembered in this session for `conn`, if anything.
    fn remembered_this_session(&self, conn: &str) -> Option<Secret> {
        let s = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        s.get(conn).cloned()
    }

    /// Resolves connection `conn`'s secret (with `keyring_account` typically
    /// the URL). `None` = there is no secret (e.g. agent auth).
    ///
    /// Order (ADR 0015 C): `NORTE_SECRET_<CONN>` → OS keyring →
    /// `secrets.age`. The first one that hits wins.
    ///
    /// A rung that hits the EMPTY string is not passed along as a secret and
    /// does NOT fall through to the next one either (#320): it is a
    /// configuration failure (`NORTE_SECRET_X=""`), and looking in the next
    /// rung would cover it up the same way passing it along used to — which
    /// is what happened before #320.
    ///
    /// The empty value that arrives here from `auth = "key"` is NOT a
    /// failure (there the secret is the passphrase of a key that may not be
    /// encrypted); `establish` filters that out in the core, which is the
    /// one that knows the auth method.
    ///
    /// # Errors
    /// [`ConnectError::SecretEmpty`] if the resolved secret is the empty
    /// string; [`ConnectError::SecretNotUtf8`] if the env var exists with
    /// bytes that do not decode; or if `secrets.age` exists but cannot be
    /// decrypted/parsed. The keyring being unavailable (headless) is NOT an
    /// error (it falls through to the next one).
    pub async fn resolve(
        &self,
        conn: &str,
        keyring_account: &str,
    ) -> Result<Option<Secret>, ConnectError> {
        Ok(self
            .resolve_with_origin(conn, keyring_account)
            .await?
            .map(|(s, _)| s))
    }

    /// Like [`Self::resolve`], but also says WHICH rung the secret came
    /// from.
    ///
    /// The origin is not diagnostic: it is what allows UNDOING a wrong
    /// answer (#325). A secret from [`SecretOrigin::Session`] was typed by a
    /// human a moment ago and could be wrong; the other three were
    /// deliberately put by someone in a place that can be edited. Only the
    /// first one gets forgotten automatically when the server rejects it —
    /// see [`Self::forget_session`].
    ///
    /// # Errors
    /// Same as [`Self::resolve`].
    #[tracing::instrument(level = "debug", skip_all, fields(conn = %conn))]
    pub async fn resolve_with_origin(
        &self,
        conn: &str,
        keyring_account: &str,
    ) -> Result<Option<(Secret, SecretOrigin)>, ConnectError> {
        // 0. What the human typed in this session (#325). Ahead of
        //    everything: if they already answered once, they are not asked
        //    again just from navigating.
        if let Some(s) = self.remembered_this_session(conn) {
            return non_empty(s, conn, SecretOrigin::Session)
                .map(|s| Some((s, SecretOrigin::Session)));
        }
        // 1. Env var (explicit override for CI/corporate).
        if let Some(s) = env_secret(conn)? {
            return non_empty(s, conn, SecretOrigin::Env).map(|s| Some((s, SecretOrigin::Env)));
        }
        // 2. OS keyring (blocking → spawn_blocking). Unavailable (headless) =
        //    falls through to the file, not an error.
        let account = keyring_account.to_string();
        let from_keyring = tokio::task::spawn_blocking(move || keyring_lookup(&account))
            .await
            .map_err(|_| ConnectError::SecretStore("internal resolver error"))?;
        if let Some(s) = from_keyring {
            return non_empty(s, conn, SecretOrigin::Keyring)
                .map(|s| Some((s, SecretOrigin::Keyring)));
        }
        // 3. Encrypted `secrets.age` file (persistent headless).
        let dir = self.config_dir.clone();
        let conn_owned = conn.to_string();
        let from_age = tokio::task::spawn_blocking(move || age_lookup(&dir, &conn_owned))
            .await
            .map_err(|_| ConnectError::SecretStore("internal resolver error"))??;
        if let Some(s) = from_age {
            return non_empty(s, conn, SecretOrigin::AgeFile)
                .map(|s| Some((s, SecretOrigin::AgeFile)));
        }
        Ok(None)
    }

    /// Saves `secret` for `conn` in `secrets.age` (encrypted
    /// read-modify-write). For the phase-6 UX (`norte connect --save`).
    ///
    /// An empty secret is rejected HERE in addition to in [`Self::resolve`]:
    /// without this guard, a prompt answered blank would write an entry that
    /// poisons the connection forever —the file is encrypted and is not
    /// edited by hand— and the failure would show up on every later
    /// connection, far from the mistake. The place where the mistake is made
    /// is the one that must stop it.
    ///
    /// # Errors
    /// [`ConnectError::SecretEmpty`] if `secret` is the empty string; if
    /// there is no store passphrase; or if encryption/writing fails.
    #[tracing::instrument(level = "debug", skip_all, fields(conn = %conn))]
    pub async fn store_in_age(&self, conn: &str, secret: &Secret) -> Result<(), ConnectError> {
        if secret.expose().is_empty() {
            return Err(ConnectError::SecretEmpty {
                conn: conn.to_string(),
                origin: SecretOrigin::AgeFile,
            });
        }
        let dir = self.config_dir.clone();
        let conn = conn.to_string();
        // The value stays zeroized on the write path too.
        let value = Zeroizing::new(secret.expose().to_string());
        tokio::task::spawn_blocking(move || age_store(&dir, &conn, value.as_str()))
            .await
            .map_err(|_| ConnectError::SecretStore("internal resolver error"))?
    }
}

/// The name of the env var that resolves connection `conn`'s secret.
///
/// Convention: `NORTE_SECRET_<CONN>`, with `<CONN>` = `conn` in UPPERCASE and
/// every non-alphanumeric byte replaced by `_` (so a connection name with
/// `-`/`.`/spaces is still a valid env var in any POSIX shell). Public so
/// `norte doctor` (H2) can name the missing variable without duplicating the
/// rule — [`SecretResolver::resolve`] itself uses it as the first resolution
/// rung (`env_secret`).
///
/// ```
/// assert_eq!(norte_connect::env_key("mi-server.1"), "NORTE_SECRET_MI_SERVER_1");
/// ```
#[must_use]
pub fn env_key(conn: &str) -> String {
    let tail: String = conn
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("NORTE_SECRET_{tail}")
}

/// The env var's secret, if it exists.
///
/// `var_os` and not `var`: with `var(..).ok()` bytes that do not decode were
/// indistinguishable from "the variable is not there", resolution fell
/// through to the keyring and `norte doctor` —which reads with `var_os`—
/// reported it as present. Two readers with different policies on the same
/// bytes is #320's lie all over again (rust review MAJOR-4). A secret
/// travels as a `String`, so there is nothing to preserve: it is said and it
/// stops.
fn env_secret(conn: &str) -> Result<Option<Secret>, ConnectError> {
    match std::env::var_os(env_key(conn)) {
        None => Ok(None),
        Some(raw) => raw
            .into_string()
            .map(|v| Some(Secret::new(v)))
            .map_err(|_| ConnectError::SecretNotUtf8 {
                conn: conn.to_string(),
                origin: SecretOrigin::Env,
            }),
    }
}

/// Lets `secret` through, or fails if it is the EMPTY string (#320).
///
/// An empty secret is not a secret: opendal silently discards an empty
/// `secret_access_key` (`if !v.is_empty()`), so the connection degrades to
/// the environment's credential chain without saying so. `origin` names the
/// rung where the gap showed up — it is the only thing that tells the user
/// where to look.
///
/// Only the STRICT empty string is rejected: a secret made of spaces DOES
/// travel to the server and dies with an actionable credential rejection, so
/// trimming it before comparing would only add false positives.
///
/// Takes and returns a [`Secret`] (not a `String`) on purpose: the plaintext
/// never exists again outside the wrapper that zeroizes it when dropped.
fn non_empty(secret: Secret, conn: &str, origin: SecretOrigin) -> Result<Secret, ConnectError> {
    if secret.expose().is_empty() {
        return Err(ConnectError::SecretEmpty {
            conn: conn.to_string(),
            origin,
        });
    }
    Ok(secret)
}

/// Looks up `account` in the OS keyring. Any failure (including "no
/// backend", typical in headless/CI) is treated as "not found": it is
/// best-effort, the env/age fallbacks are the reliable ones.
fn keyring_lookup(account: &str) -> Option<Secret> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account).ok()?;
    match entry.get_password() {
        Ok(p) => Some(Secret::new(p)),
        Err(e) => {
            // get_password's error does not contain the password.
            tracing::debug!(error = %e, "keyring unavailable or no entry; trying the file");
            None
        }
    }
}

/// `secrets.age`'s passphrase: `NORTE_SECRETS_KEY` (env) or
/// `<dir>/secrets.key`. The content is kept zeroized; on Unix a warning is
/// issued if the file is readable by group/others (the passphrase would be
/// exposed).
fn age_passphrase(dir: &Path) -> Option<Zeroizing<String>> {
    if let Ok(p) = std::env::var("NORTE_SECRETS_KEY") {
        return Some(Zeroizing::new(p));
    }
    let path = dir.join(SECRETS_KEY_FILE);
    // Reads the WHOLE file into a zeroized buffer (no residual String).
    let raw = Zeroizing::new(std::fs::read_to_string(&path).ok()?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&path)
            && md.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                "secrets.key is readable by group/others: use 0600 permissions (the passphrase is exposed)"
            );
        }
    }
    Some(Zeroizing::new(
        raw.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

/// Decrypts `secrets.age` and returns `conn`'s secret (if present).
fn age_lookup(dir: &Path, conn: &str) -> Result<Option<Secret>, ConnectError> {
    let path = dir.join(SECRETS_FILE);
    let ciphertext = match std::fs::read(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConnectError::Io(e)),
    };
    let map = age_decrypt(dir, &ciphertext)?;
    // The copy enters `Secret` (zeroizes); `map` zeroizes the rest when dropped.
    Ok(map.0.get(conn).map(|v| Secret::new(v.clone())))
}

/// Adds/updates `conn`→`value` in `secrets.age` (read-modify-write).
fn age_store(dir: &Path, conn: &str, value: &str) -> Result<(), ConnectError> {
    let path = dir.join(SECRETS_FILE);
    let mut map = match std::fs::read(&path) {
        Ok(c) => age_decrypt(dir, &c)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SecretMap::default(),
        Err(e) => return Err(ConnectError::Io(e)),
    };
    map.0.insert(conn.to_string(), value.to_string());
    let plaintext = Zeroizing::new(
        toml::to_string(&map.0).map_err(|_| ConnectError::SecretStore("serialize"))?,
    );
    let ciphertext = age_encrypt(dir, plaintext.as_bytes())?;
    write_secret_file(&path, &ciphertext)?;
    Ok(())
}

/// Writes `data` to `path` ATOMICALLY (tmp + rename) and with 0600
/// permissions on Unix (`secrets.age` must not be world-readable — ADR
/// 0015 C/6).
fn write_secret_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("age.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// conn→decrypted-secret map of a `secrets.age`. Values are zeroized when
/// the [`SecretMap`] is dropped.
fn age_decrypt(dir: &Path, ciphertext: &[u8]) -> Result<SecretMap, ConnectError> {
    let passphrase =
        age_passphrase(dir).ok_or(ConnectError::SecretStore("no passphrase for secrets.age"))?;
    let decryptor = age::Decryptor::new(ciphertext)
        .map_err(|_| ConnectError::SecretStore("secrets.age unreadable"))?;
    let identity =
        age::scrypt::Identity::new(age::secrecy::SecretString::from(passphrase.to_string()));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| ConnectError::SecretStore("wrong passphrase"))?;
    let mut plaintext = Zeroizing::new(String::new());
    reader
        .read_to_string(&mut plaintext)
        .map_err(|_| ConnectError::SecretStore("decryption"))?;
    // NEVER interpolate toml's error: it would carry the plaintext (the
    // secrets) into the message and from there into the logs (rule 10).
    // Static message.
    let map: BTreeMap<String, String> =
        toml::from_str(&plaintext).map_err(|_| ConnectError::SecretStore("invalid TOML"))?;
    Ok(SecretMap(map))
}

/// Encrypts `plaintext` with the store's passphrase.
fn age_encrypt(dir: &Path, plaintext: &[u8]) -> Result<Vec<u8>, ConnectError> {
    let passphrase =
        age_passphrase(dir).ok_or(ConnectError::SecretStore("no passphrase for secrets.age"))?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.to_string(),
    ));
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|_| ConnectError::SecretStore("encryption"))?;
    writer
        .write_all(plaintext)
        .map_err(|_| ConnectError::SecretStore("encryption"))?;
    writer
        .finish()
        .map_err(|_| ConnectError::SecretStore("encryption"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_sanitiza() {
        assert_eq!(env_key("trabajo"), "NORTE_SECRET_TRABAJO");
        assert_eq!(env_key("mi-server.1"), "NORTE_SECRET_MI_SERVER_1");
    }

    /// #325: what was remembered in the session beats EVERYTHING, the env var
    /// included. It goes first because it is the most recent —the human just
    /// typed it— and because otherwise, a variable stuck at a stale value
    /// would leave the dialog with no effect: it would be asked on every
    /// navigation and the answer would never be used.
    #[tokio::test]
    async fn lo_recordado_en_la_sesion_gana_a_la_env_var() {
        let dir = tempfile::tempdir().expect("tmp");
        let r = SecretResolver::new(dir.path());

        // Nothing remembered and no source: no secret.
        assert!(
            r.resolve("sesion-test", "sftp://h")
                .await
                .expect("resolve")
                .is_none(),
            "no sources means no secret"
        );

        r.remember_for_session("sesion-test", Secret::new("typed".into()));
        let s = r
            .resolve("sesion-test", "sftp://h")
            .await
            .expect("resolve")
            .expect("the session one");
        assert_eq!(s.expose(), "typed");
    }

    /// #325 + #320: remembering the EMPTY string does not pass it along as a
    /// secret, nor does it fall through to the next rung. The dialog does
    /// not produce it (Enter with the field empty delivers nothing), but the
    /// resolver is public and cannot rely on that: an empty value getting
    /// through would reproduce exactly the ambient-credential leak that #320
    /// closed.
    #[tokio::test]
    async fn recordar_vacio_es_error_y_no_pasa_como_secreto() {
        let dir = tempfile::tempdir().expect("tmp");
        let r = SecretResolver::new(dir.path());
        r.remember_for_session("vacio-test", Secret::new(String::new()));
        let err = r
            .resolve("vacio-test", "sftp://h")
            .await
            .expect_err("a remembered empty value is an error");
        assert!(
            matches!(
                err,
                ConnectError::SecretEmpty {
                    origin: SecretOrigin::Session,
                    ..
                }
            ),
            "and it says which rung it came from: {err:?}"
        );
    }

    #[test]
    fn secret_debug_no_filtra() {
        let s = Secret::new("hunter2".into());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
    }

    /// Round-trip of the `age` store: save and resolve with the passphrase
    /// from the `secrets.key` file (without touching the global env, which
    /// is unsafe in 2024).
    #[tokio::test]
    async fn age_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "test-passphrase");
        let r = SecretResolver::new(dir.path());
        // Unique name → neither the keyring nor an env var intercept it.
        let conn = "conn-age-roundtrip-xyz";
        assert!(r.resolve(conn, "sftp://x@y:22").await.unwrap().is_none());
        r.store_in_age(conn, &Secret::new("s3cr3t".into()))
            .await
            .unwrap();
        let got = r.resolve(conn, "sftp://x@y:22").await.unwrap();
        assert_eq!(got.expect("present").expose(), "s3cr3t");
        // The file on disk is ENCRYPTED (does not contain the secret in the
        // clear).
        let raw = std::fs::read(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(
            !raw.windows(6).any(|w| w == b"s3cr3t"),
            "plaintext secret on disk"
        );
    }

    /// `secrets.age` is written 0600 (not world-readable).
    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_age_es_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "pass");
        let r = SecretResolver::new(dir.path());
        r.store_in_age("c", &Secret::new("v".into())).await.unwrap();
        let md = std::fs::metadata(dir.path().join(SECRETS_FILE)).unwrap();
        assert_eq!(md.permissions().mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn age_sin_passphrase_es_error_si_existe_el_fichero() {
        let dir = tempfile::tempdir().unwrap();
        // File present but no passphrase (neither env nor secrets.key).
        std::fs::write(dir.path().join(SECRETS_FILE), b"whatever").unwrap();
        let r = SecretResolver::new(dir.path());
        assert!(
            r.resolve("conn-sin-pass-xyz", "sftp://x@y:22")
                .await
                .is_err()
        );
    }

    /// #320: a secret that resolves to the EMPTY string is an ERROR, not a
    /// secret. Without this the empty value reaches `s3.rs` intact, opendal
    /// DISCARDS it (`secret_access_key`: `if !v.is_empty()`), the
    /// `StaticCredentialProvider` does not get registered and the connection
    /// ends up authenticating with the ambient chain (profile, SSO, IMDS) —
    /// an identity nobody asked for, silently.
    ///
    /// Seeded through the `age` store because it is the only rung a test can
    /// plant without touching the global environment (unsafe in the 2024
    /// edition), and through `age_store` rather than `store_in_age` because
    /// the public path also rejects the empty value: the fixture goes in
    /// underneath, on purpose.
    #[tokio::test]
    async fn secreto_vacio_es_error_y_no_pasa_como_secreto() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "test-passphrase");
        let r = SecretResolver::new(dir.path());
        let conn = "conn-vacia-xyz";
        age_store(dir.path(), conn, "").expect("seed the fixture");
        let e = r
            .resolve(conn, "s3://un-bucket")
            .await
            .expect_err("an empty secret cannot resolve as valid");
        assert!(
            matches!(
                &e,
                ConnectError::SecretEmpty { conn: c, origin: SecretOrigin::AgeFile } if c == conn
            ),
            "unexpected error (name or origin): {e:?}"
        );
    }

    /// The WRITE path rejects the same thing the read path does: without
    /// this you could persist, into an encrypted file —not editable by
    /// hand—, an entry that makes the connection fail forever, and the error
    /// would show up far from where the mistake was made.
    #[tokio::test]
    async fn store_in_age_rechaza_el_vacio() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "test-passphrase");
        let r = SecretResolver::new(dir.path());
        let e = r
            .store_in_age("c", &Secret::new(String::new()))
            .await
            .expect_err("saving an empty secret cannot succeed");
        assert!(matches!(e, ConnectError::SecretEmpty { .. }), "{e:?}");
        assert!(
            !dir.path().join(SECRETS_FILE).exists(),
            "the rejection must not leave a written file"
        );
    }

    /// The empty-value error names the ORIGIN (env/keyring/age): it is the
    /// only thing that says WHERE the gap is. And a secret made only of
    /// spaces is NOT rejected: that one does reach the server and dies with
    /// an actionable 403, so treating it as empty would only add a false
    /// positive.
    #[test]
    fn secreto_vacio_nombra_el_origen_y_los_espacios_pasan() {
        for (origin, expected) in [
            (SecretOrigin::Env, "environment variable"),
            (SecretOrigin::Keyring, "keyring"),
            (SecretOrigin::AgeFile, "secrets.age"),
        ] {
            let e = non_empty(Secret::new(String::new()), "demo", origin)
                .expect_err("empty string = error");
            let msg = e.to_string();
            assert!(msg.contains("demo"), "missing the connection's name: {msg}");
            assert!(
                msg.contains(expected),
                "origin {origin:?} said wrong: {msg}"
            );
        }
        assert_eq!(
            non_empty(Secret::new(" ".into()), "demo", SecretOrigin::Env)
                .expect("spaces are not empty")
                .expose(),
            " "
        );
    }

    /// Writes `secrets.key` with 0600 on Unix (avoids the permissions
    /// warning).
    fn write_key_file(dir: &Path, pass: &str) {
        let path = dir.join(SECRETS_KEY_FILE);
        std::fs::write(&path, pass).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

/// Keyring entry for the journal's anchor key (M3-5, ADR 0025).
const ANCHOR_KEY_ACCOUNT: &str = "journal-anchor";

/// HMAC key for the journal's anchors: reads it from the keyring and, if it
/// does not exist, generates 32 bytes from the OS and saves them
/// (get-or-create, hex). UNLIKE connection secrets, here the keyring is NOT
/// best-effort: without it there are no anchors (the key never touches plain
/// disk — rule 10). Overridable with env `NORTE_ANCHOR_KEY` (64 hex chars)
/// for headless/CI, same env → keyring order as connection secrets —
/// **WATCH OUT**: in env mode the guarantee against same-uid is ZERO (the
/// threat model's attacker reads `/proc/<pid>/environ`, and passing it
/// inline leaves it in the shell's history); use it only where the keyring
/// does not exist and the environment is controlled. Get-or-create race: the
/// first two CONCURRENT anchorings can generate different keys
/// (last-writer wins and the other ends up `BadMac`); after `set_password`
/// it RE-READS and returns what was persisted, which bounds it to the
/// keyring's own window. The error messages are STATIC (same criterion as
/// [`crate::ConnectError::SecretStore`]); the detail goes through
/// `tracing::debug` (the keyring's error does not contain the key).
///
/// # Errors
/// Keyring unavailable/no backend, unreadable entry, or OS entropy.
pub fn journal_anchor_key() -> Result<[u8; 32], crate::ConnectError> {
    use crate::ConnectError::SecretStore;
    // Env first (same order as connection secrets, ADR 0015 C): essential in
    // headless/CI where the keyring has no backend (`linux-keyring` is an
    // opt-in feature). 64 hex chars.
    if let Ok(hexed) = std::env::var("NORTE_ANCHOR_KEY") {
        let hexed = zeroize::Zeroizing::new(hexed);
        return decode_anchor_key(&hexed)
            .ok_or(SecretStore("NORTE_ANCHOR_KEY invalid (64 hex chars)"));
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, ANCHOR_KEY_ACCOUNT).map_err(|e| {
        tracing::debug!(error = %e, "keyring: could not open the anchor entry");
        SecretStore("keyring unavailable for the anchor key")
    })?;
    match entry.get_password() {
        Ok(hexed) => {
            decode_anchor_key(&hexed).ok_or(SecretStore("anchor key corrupted in the keyring"))
        }
        Err(keyring::Error::NoEntry) => {
            let mut key = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(key.as_mut()).map_err(|e| {
                tracing::debug!(error = %e, "getrandom failed");
                SecretStore("no OS entropy for the anchor key")
            })?;
            let hexed = zeroize::Zeroizing::new(key.iter().fold(String::new(), |mut acc, b| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            }));
            entry.set_password(&hexed).map_err(|e| {
                tracing::debug!(error = %e, "keyring: could not save the anchor key");
                SecretStore("keyring unavailable to save the anchor key")
            })?;
            // RE-READ: if another process won the get-or-create race, return
            // the PERSISTED key, not the local loser.
            let persisted = zeroize::Zeroizing::new(entry.get_password().map_err(|e| {
                tracing::debug!(error = %e, "keyring: re-read after saving failed");
                SecretStore("keyring unavailable for the anchor key")
            })?);
            decode_anchor_key(&persisted).ok_or(SecretStore("anchor key corrupted in the keyring"))
        }
        Err(e) => {
            tracing::debug!(error = %e, "keyring: could not read the anchor key");
            Err(SecretStore("keyring unavailable for the anchor key"))
        }
    }
}

/// Decodes the 64-char hex key; `None` if the length or hex is wrong.
fn decode_anchor_key(hexed: &str) -> Option<[u8; 32]> {
    let bytes = hexed.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        let hi = char::from(chunk[0]).to_digit(16)?;
        let lo = char::from(chunk[1]).to_digit(16)?;
        key[i] = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(key)
}

#[cfg(test)]
mod anchor_key_tests {
    use super::decode_anchor_key;

    #[test]
    fn decode_round_trip_y_rechazos() {
        let key = [0xABu8; 32];
        let hexed: String = key.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        assert_eq!(decode_anchor_key(&hexed), Some(key));
        assert_eq!(decode_anchor_key("short"), None);
        assert_eq!(decode_anchor_key(&"zz".repeat(32)), None);
    }
}
