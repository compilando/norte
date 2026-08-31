//! Resolución de secretos (ADR 0015 C): `NORTE_SECRET_<CONN>` (env) → keyring
//! del OS → fichero `secrets.age` cifrado. El secreto se envuelve en
//! [`Secret`] (se borra de memoria al soltarse, spec §265) y JAMÁS se loguea
//! ni se imprime (regla 10). Los intermedios en claro (mapa descifrado,
//! passphrase) se mantienen zeroizados de punta a punta.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use zeroize::{Zeroize, Zeroizing};

use crate::error::{ConnectError, SecretOrigin};

/// Servicio bajo el que se guardan los secretos en el keyring del OS.
const KEYRING_SERVICE: &str = "norte";
/// Fichero de secretos cifrados dentro del dir de config.
const SECRETS_FILE: &str = "secrets.age";
/// Fichero opcional con la passphrase del `secrets.age` (debe ser 0600).
const SECRETS_KEY_FILE: &str = "secrets.key";

/// Un secreto (contraseña/passphrase) que se borra de memoria al soltarse y
/// NUNCA se imprime. Su contenido solo sale por [`Secret::expose`].
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Envuelve un secreto.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// El contenido en claro. Úsalo lo más tarde y brevemente posible; jamás
    /// lo loguees (regla 10).
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nunca el contenido (regla 10).
        f.write_str("Secret(***)")
    }
}

/// Mapa conn→secreto descifrado del `secrets.age`. Zeroiza TODOS sus valores
/// al soltarse (spec §265): el plaintext no queda residual en el heap.
#[derive(Default)]
struct SecretMap(BTreeMap<String, String>);

impl Drop for SecretMap {
    fn drop(&mut self) {
        for v in self.0.values_mut() {
            v.zeroize();
        }
    }
}

/// Resuelve el secreto de una conexión por el orden env → keyring → `age`.
#[derive(Debug, Clone)]
pub struct SecretResolver {
    config_dir: PathBuf,
}

impl SecretResolver {
    /// Resolver anclado en el dir de config (donde vive `secrets.age`).
    #[must_use]
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    /// Resuelve el secreto de la conexión `conn` (con `keyring_account`
    /// típicamente la URL). `None` = no hay secreto (p. ej. auth por agente).
    ///
    /// Orden (ADR 0015 C): `NORTE_SECRET_<CONN>` → keyring del OS →
    /// `secrets.age`. El primero que acierte gana.
    ///
    /// Un escalón que acierta con la cadena VACÍA no se pasa como secreto y
    /// TAMPOCO se cae al siguiente (#320): es un fallo de configuración
    /// (`NORTE_SECRET_X=""`), y buscar en el siguiente escalón lo taparía igual
    /// que lo tapaba pasarlo — que es lo que se hacía antes de #320.
    ///
    /// El vacío que llega aquí desde `auth = "key"` NO es un fallo (ahí el
    /// secreto es la passphrase de una clave que puede no estar cifrada); lo
    /// filtra `establish` en el core, que es quien sabe el método de auth.
    ///
    /// # Errors
    /// [`ConnectError::SecretEmpty`] si el secreto resuelto es la cadena
    /// vacía; [`ConnectError::SecretNotUtf8`] si la env var existe con bytes
    /// que no decodifican; o si el `secrets.age` existe pero no se puede
    /// descifrar/parsear. El keyring no disponible (headless) NO es error (se
    /// cae al siguiente).
    #[tracing::instrument(level = "debug", skip_all, fields(conn = %conn))]
    pub async fn resolve(
        &self,
        conn: &str,
        keyring_account: &str,
    ) -> Result<Option<Secret>, ConnectError> {
        // 1. Env var (override explícito para CI/corporativo).
        if let Some(s) = env_secret(conn)? {
            return non_empty(s, conn, SecretOrigin::Env).map(Some);
        }
        // 2. Keyring del OS (bloqueante → spawn_blocking). No disponible
        //    (headless) = se cae al fichero, no es error.
        let account = keyring_account.to_string();
        let from_keyring = tokio::task::spawn_blocking(move || keyring_lookup(&account))
            .await
            .map_err(|_| ConnectError::SecretStore("error interno del resolver"))?;
        if let Some(s) = from_keyring {
            return non_empty(s, conn, SecretOrigin::Keyring).map(Some);
        }
        // 3. Fichero `secrets.age` cifrado (headless persistente).
        let dir = self.config_dir.clone();
        let conn_owned = conn.to_string();
        let from_age = tokio::task::spawn_blocking(move || age_lookup(&dir, &conn_owned))
            .await
            .map_err(|_| ConnectError::SecretStore("error interno del resolver"))??;
        if let Some(s) = from_age {
            return non_empty(s, conn, SecretOrigin::AgeFile).map(Some);
        }
        Ok(None)
    }

    /// Guarda `secret` para `conn` en el `secrets.age` (read-modify-write
    /// cifrado). Para la UX de fase 6 (`norte connect --save`).
    ///
    /// Un secreto vacío se rechaza AQUÍ además de en [`Self::resolve`]: sin
    /// esta guarda, un prompt contestado en blanco escribiría una entrada que
    /// envenena la conexión para siempre —el fichero está cifrado y no se edita
    /// a mano— y el fallo aparecería en cada conexión posterior, lejos del
    /// error. El sitio donde se comete la equivocación es el que debe pararla.
    ///
    /// # Errors
    /// [`ConnectError::SecretEmpty`] si `secret` es la cadena vacía; si no hay
    /// passphrase del store; o si falla el cifrado/escritura.
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
        // El valor se mantiene zeroizado también en el camino de escritura.
        let value = Zeroizing::new(secret.expose().to_string());
        tokio::task::spawn_blocking(move || age_store(&dir, &conn, value.as_str()))
            .await
            .map_err(|_| ConnectError::SecretStore("error interno del resolver"))?
    }
}

/// El nombre de la env var que resuelve el secreto de la conexión `conn`.
///
/// Convención: `NORTE_SECRET_<CONN>`, con `<CONN>` = `conn` en MAYÚSCULAS y
/// cada byte no-alfanumérico sustituido por `_` (así un nombre de conexión
/// con `-`/`.`/espacios sigue siendo una env var válida en cualquier shell
/// POSIX). Pública para que `norte doctor` (H2) pueda nombrar la variable que
/// falta sin duplicar la regla — el propio [`SecretResolver::resolve`] la usa
/// como primer escalón de resolución (`env_secret`).
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

/// El secreto de la env var, si existe.
///
/// `var_os` y no `var`: con `var(..).ok()` unos bytes que no decodifican eran
/// indistinguibles de «la variable no está», la resolución seguía al keyring y
/// `norte doctor` —que lee con `var_os`— la daba por presente. Dos lectores con
/// políticas distintas sobre los mismos bytes es la mentira de #320 otra vez
/// (revisión rust MAJOR-4). Un secreto viaja como `String`, así que no hay nada
/// que preservar: se dice y se para.
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

/// Deja pasar `secret`, o falla si es la cadena VACÍA (#320).
///
/// Un secreto vacío no es un secreto: opendal descarta un `secret_access_key`
/// vacío en silencio (`if !v.is_empty()`), con lo que la conexión degrada a la
/// cadena de credenciales del entorno sin decirlo. `origin` nombra el escalón
/// donde apareció el hueco — es lo único que le dice al usuario dónde mirar.
///
/// Solo se rechaza el vacío ESTRICTO: un secreto de espacios sí viaja al
/// servidor y muere con un rechazo de credenciales accionable, así que
/// recortarlo antes de comparar solo añadiría falsos positivos.
///
/// Toma y devuelve un [`Secret`] (no un `String`) a propósito: el plaintext no
/// vuelve a existir fuera del envoltorio que lo zeroiza al soltarse.
fn non_empty(secret: Secret, conn: &str, origin: SecretOrigin) -> Result<Secret, ConnectError> {
    if secret.expose().is_empty() {
        return Err(ConnectError::SecretEmpty {
            conn: conn.to_string(),
            origin,
        });
    }
    Ok(secret)
}

/// Busca `account` en el keyring del OS. Cualquier fallo (incluido "no hay
/// backend", típico en headless/CI) se trata como "no encontrado": es
/// best-effort, los fallbacks env/age son los fiables.
fn keyring_lookup(account: &str) -> Option<Secret> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account).ok()?;
    match entry.get_password() {
        Ok(p) => Some(Secret::new(p)),
        Err(e) => {
            // El error de get_password no contiene el password.
            tracing::debug!(error = %e, "keyring no disponible o sin entrada; se prueba el fichero");
            None
        }
    }
}

/// Passphrase del `secrets.age`: `NORTE_SECRETS_KEY` (env) o `<dir>/secrets.key`.
/// El contenido se mantiene zeroizado; en Unix se avisa si el fichero es
/// legible por grupo/otros (la passphrase quedaría expuesta).
fn age_passphrase(dir: &Path) -> Option<Zeroizing<String>> {
    if let Ok(p) = std::env::var("NORTE_SECRETS_KEY") {
        return Some(Zeroizing::new(p));
    }
    let path = dir.join(SECRETS_KEY_FILE);
    // Lee el fichero COMPLETO a un buffer zeroizado (sin String residual).
    let raw = Zeroizing::new(std::fs::read_to_string(&path).ok()?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&path)
            && md.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                "secrets.key es legible por grupo/otros: usa permisos 0600 (la passphrase queda expuesta)"
            );
        }
    }
    Some(Zeroizing::new(
        raw.trim_end_matches(['\r', '\n']).to_string(),
    ))
}

/// Descifra `secrets.age` y devuelve el secreto de `conn` (si está).
fn age_lookup(dir: &Path, conn: &str) -> Result<Option<Secret>, ConnectError> {
    let path = dir.join(SECRETS_FILE);
    let ciphertext = match std::fs::read(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConnectError::Io(e)),
    };
    let map = age_decrypt(dir, &ciphertext)?;
    // La copia entra a `Secret` (zeroiza); `map` zeroiza el resto al soltarse.
    Ok(map.0.get(conn).map(|v| Secret::new(v.clone())))
}

/// Añade/actualiza `conn`→`value` en el `secrets.age` (read-modify-write).
fn age_store(dir: &Path, conn: &str, value: &str) -> Result<(), ConnectError> {
    let path = dir.join(SECRETS_FILE);
    let mut map = match std::fs::read(&path) {
        Ok(c) => age_decrypt(dir, &c)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SecretMap::default(),
        Err(e) => return Err(ConnectError::Io(e)),
    };
    map.0.insert(conn.to_string(), value.to_string());
    let plaintext = Zeroizing::new(
        toml::to_string(&map.0).map_err(|_| ConnectError::SecretStore("serializar"))?,
    );
    let ciphertext = age_encrypt(dir, plaintext.as_bytes())?;
    write_secret_file(&path, &ciphertext)?;
    Ok(())
}

/// Escribe `data` en `path` de forma ATÓMICA (tmp + rename) y con permisos
/// 0600 en Unix (el `secrets.age` no debe ser world-readable — ADR 0015 C/6).
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

/// Mapa conn→secreto descifrado de un `secrets.age`. Los valores se zeroizan
/// al soltar el [`SecretMap`].
fn age_decrypt(dir: &Path, ciphertext: &[u8]) -> Result<SecretMap, ConnectError> {
    let passphrase =
        age_passphrase(dir).ok_or(ConnectError::SecretStore("sin passphrase para secrets.age"))?;
    let decryptor = age::Decryptor::new(ciphertext)
        .map_err(|_| ConnectError::SecretStore("secrets.age ilegible"))?;
    let identity =
        age::scrypt::Identity::new(age::secrecy::SecretString::from(passphrase.to_string()));
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| ConnectError::SecretStore("passphrase incorrecta"))?;
    let mut plaintext = Zeroizing::new(String::new());
    reader
        .read_to_string(&mut plaintext)
        .map_err(|_| ConnectError::SecretStore("descifrado"))?;
    // NUNCA interpolar el error de toml: llevaría el plaintext (los secretos)
    // al mensaje y de ahí a los logs (regla 10). Mensaje estático.
    let map: BTreeMap<String, String> =
        toml::from_str(&plaintext).map_err(|_| ConnectError::SecretStore("TOML inválido"))?;
    Ok(SecretMap(map))
}

/// Cifra `plaintext` con la passphrase del store.
fn age_encrypt(dir: &Path, plaintext: &[u8]) -> Result<Vec<u8>, ConnectError> {
    let passphrase =
        age_passphrase(dir).ok_or(ConnectError::SecretStore("sin passphrase para secrets.age"))?;
    let encryptor = age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(
        passphrase.to_string(),
    ));
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|_| ConnectError::SecretStore("cifrado"))?;
    writer
        .write_all(plaintext)
        .map_err(|_| ConnectError::SecretStore("cifrado"))?;
    writer
        .finish()
        .map_err(|_| ConnectError::SecretStore("cifrado"))?;
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

    #[test]
    fn secret_debug_no_filtra() {
        let s = Secret::new("hunter2".into());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
    }

    /// Round-trip del store `age`: guardar y resolver con la passphrase del
    /// fichero `secrets.key` (sin tocar env global, que es unsafe en 2024).
    #[tokio::test]
    async fn age_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "passphrase-de-test");
        let r = SecretResolver::new(dir.path());
        // Nombre único → ni el keyring ni una env var lo interceptan.
        let conn = "conn-age-roundtrip-xyz";
        assert!(r.resolve(conn, "sftp://x@y:22").await.unwrap().is_none());
        r.store_in_age(conn, &Secret::new("s3cr3t".into()))
            .await
            .unwrap();
        let got = r.resolve(conn, "sftp://x@y:22").await.unwrap();
        assert_eq!(got.expect("presente").expose(), "s3cr3t");
        // El fichero en disco está CIFRADO (no contiene el secreto en claro).
        let raw = std::fs::read(dir.path().join(SECRETS_FILE)).unwrap();
        assert!(
            !raw.windows(6).any(|w| w == b"s3cr3t"),
            "secreto en claro en disco"
        );
    }

    /// El `secrets.age` se escribe 0600 (no world-readable).
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
        // Fichero presente pero sin passphrase (ni env ni secrets.key).
        std::fs::write(dir.path().join(SECRETS_FILE), b"cualquier cosa").unwrap();
        let r = SecretResolver::new(dir.path());
        assert!(
            r.resolve("conn-sin-pass-xyz", "sftp://x@y:22")
                .await
                .is_err()
        );
    }

    /// #320: un secreto que se resuelve a la cadena VACÍA es un ERROR, no un
    /// secreto. Sin esto el vacío llega intacto a `s3.rs`, opendal lo DESCARTA
    /// (`secret_access_key`: `if !v.is_empty()`), el `StaticCredentialProvider`
    /// no se registra y la conexión acaba autenticando con la cadena ambiente
    /// (perfil, SSO, IMDS) — una identidad que nadie pidió, en silencio.
    ///
    /// Se siembra por el store `age` porque es el único escalón que un test
    /// puede plantar sin tocar el entorno global (unsafe en la edición 2024),
    /// y por `age_store` y no por `store_in_age` porque el camino público
    /// también rechaza el vacío: el fixture entra por debajo, a propósito.
    #[tokio::test]
    async fn secreto_vacio_es_error_y_no_pasa_como_secreto() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "passphrase-de-test");
        let r = SecretResolver::new(dir.path());
        let conn = "conn-vacia-xyz";
        age_store(dir.path(), conn, "").expect("sembrar el fixture");
        let e = r
            .resolve(conn, "s3://un-bucket")
            .await
            .expect_err("un secreto vacío no puede resolverse como válido");
        assert!(
            matches!(
                &e,
                ConnectError::SecretEmpty { conn: c, origin: SecretOrigin::AgeFile } if c == conn
            ),
            "error inesperado (nombre u origen): {e:?}"
        );
    }

    /// El camino de ESCRITURA rechaza lo mismo que el de lectura: sin esto se
    /// puede persistir en un fichero cifrado —no editable a mano— una entrada
    /// que hace fallar la conexión para siempre, y el error aparecería lejos
    /// del sitio donde se cometió la equivocación.
    #[tokio::test]
    async fn store_in_age_rechaza_el_vacio() {
        let dir = tempfile::tempdir().unwrap();
        write_key_file(dir.path(), "passphrase-de-test");
        let r = SecretResolver::new(dir.path());
        let e = r
            .store_in_age("c", &Secret::new(String::new()))
            .await
            .expect_err("guardar un secreto vacío no puede tener éxito");
        assert!(matches!(e, ConnectError::SecretEmpty { .. }), "{e:?}");
        assert!(
            !dir.path().join(SECRETS_FILE).exists(),
            "el rechazo no debe dejar fichero escrito"
        );
    }

    /// El error del vacío nombra el ORIGEN (env/keyring/age): es lo único que
    /// dice DÓNDE está el hueco. Y un secreto de solo espacios NO se rechaza:
    /// ese sí llega al servidor y muere con un 403 accionable, así que
    /// tratarlo como vacío solo añadiría un falso positivo.
    #[test]
    fn secreto_vacio_nombra_el_origen_y_los_espacios_pasan() {
        for (origin, esperado) in [
            (SecretOrigin::Env, "variable de entorno"),
            (SecretOrigin::Keyring, "keyring"),
            (SecretOrigin::AgeFile, "secrets.age"),
        ] {
            let e = non_empty(Secret::new(String::new()), "demo", origin)
                .expect_err("cadena vacía = error");
            let msg = e.to_string();
            assert!(msg.contains("demo"), "sin el nombre de la conexión: {msg}");
            assert!(msg.contains(esperado), "origen {origin:?} mal dicho: {msg}");
        }
        assert_eq!(
            non_empty(Secret::new(" ".into()), "demo", SecretOrigin::Env)
                .expect("los espacios no son vacío")
                .expose(),
            " "
        );
    }

    /// Escribe `secrets.key` con 0600 en Unix (evita el warn de permisos).
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

/// Entrada del keyring para la clave de anclaje del journal (M3-5, ADR 0025).
const ANCHOR_KEY_ACCOUNT: &str = "journal-anchor";

/// Clave HMAC de las anclas del journal: la lee del keyring y, si no existe,
/// genera 32 bytes del OS y los guarda (get-or-create, hex). A DIFERENCIA de
/// los secretos de conexión, aquí el keyring NO es best-effort: sin él no hay
/// anclas (la clave jamás toca disco plano — regla 10). Override por env
/// `NORTE_ANCHOR_KEY` (64 chars hex) para headless/CI, mismo orden
/// env → keyring que los secretos de conexión — **OJO**: en modo env la
/// garantía frente a same-uid es CERO (el atacante del threat model lee
/// `/proc/<pid>/environ`, y pasarla inline la deja en el historial del
/// shell); úsala solo donde el keyring no exista y el entorno esté
/// controlado. Carrera get-or-create: dos primeros anclajes CONCURRENTES
/// pueden generar claves distintas (last-writer gana y el otro queda
/// `BadMac`); tras `set_password` se RE-LEE y se devuelve lo persistido,
/// que la acota a la ventana del propio keyring. Los mensajes de error
/// son ESTÁTICOS (mismo criterio que [`crate::ConnectError::SecretStore`]);
/// el detalle va por `tracing::debug` (el error del keyring no contiene la
/// clave).
///
/// # Errors
/// Keyring no disponible/sin backend, entrada ilegible, o entropía del OS.
pub fn journal_anchor_key() -> Result<[u8; 32], crate::ConnectError> {
    use crate::ConnectError::SecretStore;
    // Env primero (mismo orden que los secretos de conexión, ADR 0015 C):
    // imprescindible en headless/CI donde el keyring no tiene backend
    // (`linux-keyring` es feature opt-in). 64 chars hex.
    if let Ok(hexed) = std::env::var("NORTE_ANCHOR_KEY") {
        let hexed = zeroize::Zeroizing::new(hexed);
        return decode_anchor_key(&hexed)
            .ok_or(SecretStore("NORTE_ANCHOR_KEY inválida (64 chars hex)"));
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, ANCHOR_KEY_ACCOUNT).map_err(|e| {
        tracing::debug!(error = %e, "keyring: no se pudo abrir la entrada de anclaje");
        SecretStore("keyring no disponible para la clave de anclaje")
    })?;
    match entry.get_password() {
        Ok(hexed) => {
            decode_anchor_key(&hexed).ok_or(SecretStore("clave de anclaje corrupta en el keyring"))
        }
        Err(keyring::Error::NoEntry) => {
            let mut key = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(key.as_mut()).map_err(|e| {
                tracing::debug!(error = %e, "getrandom falló");
                SecretStore("sin entropía del OS para la clave de anclaje")
            })?;
            let hexed = zeroize::Zeroizing::new(key.iter().fold(String::new(), |mut acc, b| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            }));
            entry.set_password(&hexed).map_err(|e| {
                tracing::debug!(error = %e, "keyring: no se pudo guardar la clave de anclaje");
                SecretStore("keyring no disponible para guardar la clave de anclaje")
            })?;
            // RE-LEE: si otro proceso ganó la carrera get-or-create, se
            // devuelve la clave PERSISTIDA, no la local perdedora.
            let persisted = zeroize::Zeroizing::new(entry.get_password().map_err(|e| {
                tracing::debug!(error = %e, "keyring: re-lectura tras guardar falló");
                SecretStore("keyring no disponible para la clave de anclaje")
            })?);
            decode_anchor_key(&persisted)
                .ok_or(SecretStore("clave de anclaje corrupta en el keyring"))
        }
        Err(e) => {
            tracing::debug!(error = %e, "keyring: no se pudo leer la clave de anclaje");
            Err(SecretStore(
                "keyring no disponible para la clave de anclaje",
            ))
        }
    }
}

/// Decodifica la clave hex de 64 chars; `None` si no mide o no es hex.
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
        assert_eq!(decode_anchor_key("corto"), None);
        assert_eq!(decode_anchor_key(&"zz".repeat(32)), None);
    }
}
