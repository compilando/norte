//! Store TOFU (trust-on-first-use) del `./.norte/init.lua` de PROYECTO
//! (ADR 0026, M4 Lua): a diferencia del `init.lua` de usuario (config propia,
//! se ejecuta sin preguntar), este fichero viene con un repo AJENO —
//! ejecutarlo a ciegas es RCE. Patrón calcado de
//! `norte-connect::known_hosts` (TOFU de host keys) y `norte-connect::secret`
//! (escritura atómica 0600): primer contacto → [`TrustDecision::Unknown`], el
//! frontend pregunta, la respuesta se persiste por (path, hash).
//!
//! Clave de identidad: (path canónico, sha256 de los BYTES). Un fichero que
//! CAMBIA de contenido en el mismo path no hereda la confianza de la versión
//! vieja — se re-pregunta, igual que un host que cambia de clave.
//!
//! I/O SÍNCRONO (fichero pequeño, TOML plano): el caller (task 7 de este
//! plan) lo envuelve en `spawn_blocking` al usarlo desde contexto async
//! (regla 2 — esta regla se cumple en el CALLER, no aquí dentro).

use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Resultado de contrastar un script con el store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// (path, hash) coincide con una entrada aprobada: cargar sin preguntar.
    Trusted,
    /// (path, hash) coincide con una entrada denegada: NO cargar, NO
    /// preguntar de nuevo (evita machacar al usuario con el mismo script).
    Denied,
    /// Sin entrada para este (path, hash) exacto — primer contacto, o el
    /// contenido cambió respecto a lo registrado: preguntar.
    Unknown,
}

/// Entrada persistida: una decisión por `path` (la más reciente sustituye a
/// cualquier anterior del mismo path — `record` es upsert-by-path).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Path canónico del script (lo resuelve el CALLER — ver rustdoc de
    /// [`TrustStore::check`]).
    path: String,
    /// sha256 en hex de los bytes evaluados.
    hash: String,
    /// `true` = aprobado, `false` = denegado.
    allow: bool,
    /// Marca temporal informativa (epoch seconds; sin dep nueva de tiempo).
    date_epoch: u64,
}

/// Fichero TOML plano: lista de entradas bajo la clave `entry`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

/// Store TOFU del `init.lua` de proyecto.
///
/// Todas las operaciones son I/O SÍNCRONO (fichero pequeño, se toca en
/// arranque/reload): el caller async debe envolverlas en `spawn_blocking`
/// (regla 2 — la responsabilidad es del caller, no de este tipo).
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    entries: Vec<Entry>,
}

impl TrustStore {
    /// Abre el store en `path`. Fichero AUSENTE = store vacío (primer uso del
    /// binario en esta máquina); cualquier otro error de lectura o de
    /// parseo se propaga (fail-closed: un fichero corrupto no debe degradar
    /// en silencio a "todo desconocido" si en realidad es una manipulación —
    /// mismo criterio que `KnownHostsStore`, ver
    /// `crates/norte-connect/src/known_hosts.rs`).
    ///
    /// # Errors
    ///
    /// Si `path` existe pero no se puede leer, o su contenido no es un
    /// `lua-trust.toml` válido.
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let parsed: FileFormat = toml::from_str(&raw).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("lua-trust.toml corrupto: {e}"),
                    )
                })?;
                parsed.entries
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, entries })
    }

    /// Contrasta `script_path` (YA CANÓNICO — lo resuelve el caller con
    /// `std::fs::canonicalize`; este store no canonicaliza) y el hash de
    /// `content` contra el store.
    ///
    /// Anti-TOCTOU: el hash se calcula SOBRE LOS BYTES PASADOS, no releyendo
    /// el fichero. El caller debe leer el script UNA SOLA VEZ, llamar
    /// `check(bytes)` y, si el resultado autoriza la carga, evaluar ESOS
    /// MISMOS bytes — jamás volver a tocar disco entre el check y el eval.
    #[must_use]
    pub fn check(&self, script_path: &str, content: &[u8]) -> TrustDecision {
        let hash = hash_hex(content);
        match self
            .entries
            .iter()
            .find(|e| e.path == script_path && e.hash == hash)
        {
            Some(e) if e.allow => TrustDecision::Trusted,
            Some(_) => TrustDecision::Denied,
            None => TrustDecision::Unknown,
        }
    }

    /// Registra la decisión del usuario para `(script_path, content)`.
    /// Sustituye cualquier entrada previa del MISMO `script_path` (una viva
    /// por path — la entrada vieja, si tenía otro hash, deja de aplicar:
    /// coherente con que `check` ya la habría marcado `Unknown`).
    ///
    /// Escritura ATÓMICA (fichero temporal en el MISMO directorio + rename)
    /// con permisos 0600 en Unix AL CREAR (calcado de
    /// `write_secret_file` en `crates/norte-connect/src/secret.rs`). Crea el
    /// directorio padre si falta.
    ///
    /// # Errors
    ///
    /// Si falla la creación del directorio padre, la escritura del fichero
    /// temporal o el rename atómico final.
    pub fn record(
        &mut self,
        script_path: &str,
        content: &[u8],
        allow: bool,
    ) -> std::io::Result<()> {
        let hash = hash_hex(content);
        let date_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.entries.retain(|e| e.path != script_path);
        self.entries.push(Entry {
            path: script_path.to_string(),
            hash,
            allow,
            date_epoch,
        });

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let serialized = toml::to_string(&FileFormat {
            entries: self.entries.clone(),
        })
        .map_err(|e| std::io::Error::other(format!("serializar lua-trust.toml: {e}")))?;
        write_atomic_0600(&self.path, serialized.as_bytes())?;
        Ok(())
    }
}

/// sha256 en hex minúscula de `content`.
fn hash_hex(content: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Escribe `data` en `path` de forma atómica (tmp en el MISMO dir + rename)
/// con permisos 0600 en Unix al crear. Calcado de `write_secret_file` en
/// `crates/norte-connect/src/secret.rs`.
fn write_atomic_0600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desconocido_pregunta_aprobado_carga_denegado_persiste() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let content = b"norte.command('x', function() end)";
        assert_eq!(
            store.check("/repo/.norte/init.lua", content),
            TrustDecision::Unknown
        );
        store
            .record("/repo/.norte/init.lua", content, true)
            .unwrap();
        assert_eq!(
            store.check("/repo/.norte/init.lua", content),
            TrustDecision::Trusted
        );
        // Contenido distinto = hash distinto = re-preguntar.
        assert_eq!(
            store.check("/repo/.norte/init.lua", b"otro"),
            TrustDecision::Unknown
        );
        // Denegado persiste (no re-preguntar hasta cambiar).
        store
            .record("/repo/.norte/init.lua", b"otro", false)
            .unwrap();
        assert_eq!(
            store.check("/repo/.norte/init.lua", b"otro"),
            TrustDecision::Denied
        );
    }

    #[test]
    fn el_store_reabre_lo_persistido() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone())
            .unwrap()
            .record("/x/.norte/init.lua", b"c", true)
            .unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(
            store.check("/x/.norte/init.lua", b"c"),
            TrustDecision::Trusted
        );
    }

    #[cfg(unix)]
    #[test]
    fn el_fichero_nace_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone())
            .unwrap()
            .record("/x", b"c", true)
            .unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
