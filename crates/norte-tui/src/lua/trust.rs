//! Store TOFU (trust-on-first-use) del `./.norte/init.lua` de PROYECTO
//! (ADR 0026, M4 Lua): a diferencia del `init.lua` de usuario (config propia,
//! se ejecuta sin preguntar), este fichero viene con un repo AJENO —
//! ejecutarlo a ciegas es RCE. Patrón calcado de
//! `norte-connect::known_hosts` (TOFU de host keys) y `norte-connect::secret`
//! (escritura atómica 0600): primer contacto → [`TrustDecision::Unknown`], el
//! frontend pregunta, la respuesta se persiste por (path, hash).
//!
//! Clave de identidad: (bytes EXACTOS del path, sha256 de los bytes del
//! contenido). Un fichero que CAMBIA de contenido en el mismo path no hereda
//! la confianza de la versión vieja — se re-pregunta, igual que un host que
//! cambia de clave.
//!
//! Regla 1 (nombres de archivo = bytes): el path se compara por
//! `OsStr::as_encoded_bytes()`, JAMÁS por su forma `String` — dos paths que
//! solo difieren en bytes no-UTF8 nunca colisionan, y viceversa: el campo
//! `path` que viaja en el TOML es SOLO para inspección humana del fichero,
//! el match de identidad usa `path_hex`.
//!
//! Limitación conocida (trampas recurrentes de CLAUDE.md): este store NO
//! normaliza NFC/NFD — compara bytes tal cual llegan. En macOS, si el
//! CALLER obtiene el path por dos rutas distintas (una NFC, otra NFD tras
//! pasar por HFS+/APFS), `check`/`record` los verían como paths DISTINTOS.
//! El caller (T8) debe usar SIEMPRE la misma forma (la que devuelve
//! `std::fs::canonicalize`) para check y record del mismo script.
//!
//! I/O SÍNCRONO (fichero pequeño, TOML plano): el caller (task 7 de este
//! plan) lo envuelve en `spawn_blocking` al usarlo desde contexto async
//! (regla 2 — esta regla se cumple en el CALLER, no aquí dentro).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Resultado de contrastar un script con el store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// (path, hash) coincide con una entrada aprobada: cargar sin preguntar.
    Trusted,
    /// (path, hash) coincide EXACTAMENTE con una entrada denegada: NO
    /// cargar, NO preguntar de nuevo (evita machacar al usuario con el
    /// mismo script).
    Denied,
    /// El path tiene una entrada denegada, pero con OTRO hash: el contenido
    /// cambió desde el rechazo. El caller (T8) trata esto como deny
    /// SILENCIOSO (aviso en la barra de estado), JAMÁS modal automático —
    /// si reabriéramos el modal en cada edición de un script ya rechazado,
    /// el usuario acabaría aprobando por fatiga. Para volver a preguntar
    /// hace falta una acción explícita (p. ej. borrar la entrada, fuera del
    /// alcance de T6).
    DeniedPathChanged,
    /// Sin entrada para este path, o el path estaba APROBADO pero con OTRO
    /// hash (ahí sí se re-pregunta: aprobar un script no es un cheque en
    /// blanco para cualquier futura versión).
    Unknown,
}

/// Entrada persistida: una decisión por path (la más reciente sustituye a
/// cualquier anterior del mismo path — `record` es upsert-by-path).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Path en forma "lossy" — SOLO para que un humano pueda inspeccionar
    /// el TOML a ojo. NUNCA se usa para el match de identidad (regla 1):
    /// eso es `path_hex`.
    path: String,
    /// Bytes EXACTOS del path (`OsStr::as_encoded_bytes()`) en hex — la
    /// clave real de identidad, sin pérdida ni asunción de UTF-8.
    path_hex: String,
    /// sha256 en hex de los bytes de contenido evaluados.
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
    /// `lua-trust.toml` válido (incluye bytes no-UTF8: `read_to_string`
    /// los rechaza y el error se propaga, no se trata como "no existe").
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
    ///
    /// Requisito del CALLER (no lo puede imponer este tipo, que solo ve
    /// bytes de path+contenido): antes de invocar `check` hay que verificar
    /// que `.norte` es un directorio REAL y que `init.lua` es un fichero
    /// REGULAR (`symlink_metadata`, o abrir con `O_NOFOLLOW`) — sin esa
    /// comprobación, un symlink que apunte a un proyecto ya confiado
    /// ejecutaría, en un contexto distinto (potencialmente hostil), el
    /// contenido que el usuario aprobó para OTRO sitio. Se implementa en T8.
    #[must_use]
    pub fn check(&self, script_path: &Path, content: &[u8]) -> TrustDecision {
        let path_hex = path_hex(script_path);
        let hash = hash_hex(content);
        match self.entries.iter().find(|e| e.path_hex == path_hex) {
            Some(e) if e.hash == hash && e.allow => TrustDecision::Trusted,
            Some(e) if e.hash == hash => TrustDecision::Denied,
            Some(e) if !e.allow => TrustDecision::DeniedPathChanged,
            _ => TrustDecision::Unknown,
        }
    }

    /// Registra la decisión del usuario para `(script_path, content)`.
    /// Sustituye cualquier entrada previa del MISMO `script_path` (una viva
    /// por path — la entrada vieja, si tenía otro hash, deja de aplicar).
    ///
    /// Escritura ATÓMICA (fichero temporal en el MISMO directorio + rename)
    /// con permisos 0600 en Unix AL CREAR (calcado de `write_secret_file` en
    /// `crates/norte-connect/src/secret.rs`). Crea el directorio padre con
    /// 0700 si falta (calcado de `Journal::open`,
    /// `crates/norte-core/src/journal.rs:283-286`).
    ///
    /// Si la persistencia falla, la mutación en memoria se REVIERTE — el
    /// estado en RAM nunca diverge del disco (si no, un `check` posterior
    /// mentiría que algo quedó registrado cuando en realidad no sobrevive a
    /// un reinicio).
    ///
    /// # Errors
    ///
    /// Si falla la creación del directorio padre, la escritura del fichero
    /// temporal o el rename atómico final.
    pub fn record(
        &mut self,
        script_path: &Path,
        content: &[u8],
        allow: bool,
    ) -> std::io::Result<()> {
        let previous = self.entries.clone();
        let path_hex = path_hex(script_path);
        let hash = hash_hex(content);
        let date_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.entries.retain(|e| e.path_hex != path_hex);
        self.entries.push(Entry {
            path: script_path.to_string_lossy().into_owned(),
            path_hex,
            hash,
            allow,
            date_epoch,
        });

        if let Err(e) = self.persist() {
            self.entries = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Vuelca `self.entries` a disco de forma atómica. Separado de
    /// `record` para que el rollback en caso de error sea un simple
    /// `self.entries = previous` en el caller (arriba).
    fn persist(&self) -> std::io::Result<()> {
        ensure_parent_dir_0700(&self.path)?;
        let serialized = toml::to_string(&FileFormat {
            entries: self.entries.clone(),
        })
        .map_err(|e| std::io::Error::other(format!("serializar lua-trust.toml: {e}")))?;
        write_atomic_0600(&self.path, serialized.as_bytes())
    }
}

/// Bytes exactos de `path` (sin asumir UTF-8 — regla 1) en hex.
fn path_hex(path: &Path) -> String {
    bytes_to_hex(path.as_os_str().as_encoded_bytes())
}

/// sha256 en hex minúscula de `content`.
fn hash_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    bytes_to_hex(hasher.finalize().as_slice())
}

/// hex minúscula de una tira de bytes cualquiera.
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Crea el directorio padre de `path` con 0700 en Unix si falta (calcado de
/// `Journal::open`, `crates/norte-core/src/journal.rs:283-286`). No-op si
/// `path` no tiene padre o el padre ya existe.
fn ensure_parent_dir_0700(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)
}

/// Escribe `data` en `path` de forma atómica (tmp en el MISMO dir + rename)
/// con permisos 0600 en Unix al crear. Calcado de `write_secret_file` en
/// `crates/norte-connect/src/secret.rs`.
fn write_atomic_0600(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // El nombre del tmp lleva el PID: dos procesos escribiendo el MISMO
    // store a la vez (dos frontends embebidos apuntando al mismo store, o
    // tests en paralelo) no deben pisarse el fichero temporal el uno al
    // otro antes del rename — un nombre fijo compartido corrompería el
    // contenido de quien pierda la carrera de `truncate`.
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
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
        let p = Path::new("/repo/.norte/init.lua");
        let content = b"norte.command('x', function() end)";
        assert_eq!(store.check(p, content), TrustDecision::Unknown);
        store.record(p, content, true).unwrap();
        assert_eq!(store.check(p, content), TrustDecision::Trusted);
        // Contenido distinto = hash distinto = re-preguntar.
        assert_eq!(store.check(p, b"otro"), TrustDecision::Unknown);
        // Denegado persiste (no re-preguntar hasta cambiar).
        store.record(p, b"otro", false).unwrap();
        assert_eq!(store.check(p, b"otro"), TrustDecision::Denied);
    }

    #[test]
    fn el_store_reabre_lo_persistido() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let script = Path::new("/x/.norte/init.lua");
        TrustStore::open(p.clone())
            .unwrap()
            .record(script, b"c", true)
            .unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(store.check(script, b"c"), TrustDecision::Trusted);
    }

    #[cfg(unix)]
    #[test]
    fn el_fichero_nace_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone())
            .unwrap()
            .record(Path::new("/x"), b"c", true)
            .unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// MEDIA-2 (review de seguridad): dos paths no-UTF8 DISTINTOS no deben
    /// colisionar en el match de identidad — si `check`/`record` degradaran
    /// a comparar por `String` (lossy), bytes inválidos se reemplazarían
    /// por `U+FFFD` y paths distintos podrían volverse indistinguibles.
    #[cfg(unix)]
    #[test]
    fn paths_no_utf8_distintos_no_colisionan() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let a = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFEa/.norte/init.lua"));
        let b = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFEb/.norte/init.lua"));
        let content = b"mismo contenido";
        store.record(a, content, true).unwrap();
        assert_eq!(store.check(a, content), TrustDecision::Trusted);
        // `b` nunca se registró: sigue Unknown pese a compartir contenido y
        // casi todos los bytes de path con `a`.
        assert_eq!(store.check(b, content), TrustDecision::Unknown);
    }

    /// MEDIA-2: un path no-UTF8 sobrevive round-trip por disco (el TOML
    /// guarda `path_hex`, no depende de que `path` sea representable).
    #[cfg(unix)]
    #[test]
    fn path_no_utf8_sobrevive_round_trip_por_disco() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let script = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFE/.norte/init.lua"));
        let content = b"c";
        TrustStore::open(p.clone())
            .unwrap()
            .record(script, content, true)
            .unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(store.check(script, content), TrustDecision::Trusted);
    }

    /// MEDIA-3: un script YA denegado que cambia de contenido es deny
    /// SILENCIOSO (`DeniedPathChanged`), NUNCA vuelve a `Unknown` — si no,
    /// cada edición de un script rechazado reabriría el modal hasta que el
    /// usuario apruebe por fatiga.
    #[test]
    fn denegado_que_cambia_de_contenido_es_deniedpathchanged_no_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let p = Path::new("/repo/.norte/init.lua");
        store.record(p, b"malo", false).unwrap();
        assert_eq!(store.check(p, b"malo"), TrustDecision::Denied);
        assert_eq!(
            store.check(p, b"cambiado"),
            TrustDecision::DeniedPathChanged
        );
    }

    /// Un aprobado que cambia de contenido sigue siendo `Unknown` (no
    /// cambia con esta review: aprobar un script no es un cheque en blanco
    /// para cualquier versión futura).
    #[test]
    fn aprobado_que_cambia_de_contenido_sigue_siendo_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let p = Path::new("/repo/.norte/init.lua");
        store.record(p, b"bueno", true).unwrap();
        assert_eq!(store.check(p, b"otra-version"), TrustDecision::Unknown);
    }

    /// BAJA-5: fichero ausente = store vacío, todo es `Unknown`.
    #[test]
    fn ausente_es_store_vacio() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("no-existe.toml");
        let store = TrustStore::open(p).unwrap();
        assert_eq!(
            store.check(Path::new("/no/importa"), b"x"),
            TrustDecision::Unknown
        );
    }

    /// BAJA-5: un fichero corrupto (TOML roto y, además, bytes no-UTF8) es
    /// ERROR, no se degrada en silencio a store vacío (fail-closed: podría
    /// ser el rastro de una manipulación, no una ausencia benigna).
    #[test]
    fn fichero_corrupto_es_err_no_store_vacio() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        std::fs::write(&p, b"no es toml valido \xFF\xFE [[[").unwrap();
        assert!(TrustStore::open(p).is_err());
    }

    /// BAJA-5 (pin anti-inyección): un `script_path` con sintaxis TOML
    /// embebida (comillas, saltos de línea, una tabla `[[entry]]` de
    /// mentira) no debe corromper el fichero ni crear una segunda entrada
    /// fantasma — como el campo se serializa vía `serde`/`toml` (no por
    /// interpolación manual de strings), el escapado es automático.
    #[test]
    fn path_hostil_con_sintaxis_toml_embebida_sobrevive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let hostile = Path::new("/tmp/\"comillas\"\n[[entry]]\npath = \"inyectado\"\n/init.lua");
        let content = b"c";
        let mut store = TrustStore::open(p.clone()).unwrap();
        store.record(hostile, content, true).unwrap();
        let reabierto = TrustStore::open(p).unwrap();
        assert_eq!(reabierto.check(hostile, content), TrustDecision::Trusted);
        assert_eq!(reabierto.entries.len(), 1);
    }

    /// Si la persistencia falla, la entrada NO debe quedar "trusted" en
    /// memoria sin respaldo en disco (si no, un reinicio del proceso vería
    /// `Unknown` para algo que un `check` anterior, en el mismo proceso,
    /// había reportado como `Trusted` — una mentira transitoria).
    #[test]
    fn write_fallido_no_deja_divergir_memoria_de_disco() {
        let dir = tempfile::tempdir().unwrap();
        // Abrimos con una ruta válida (open() no debe ver el problema)...
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        // ...y DESPUÉS rompemos el "directorio padre" convirtiéndolo en un
        // fichero: crear el dir falla, y con él debe fallar `record` entero
        // (acceso al campo privado `path`, legal desde el submódulo test).
        let bloqueador = dir.path().join("no-es-un-dir");
        std::fs::write(&bloqueador, b"soy un fichero, no un directorio").unwrap();
        store.path = bloqueador.join("subdir").join("lua-trust.toml");
        let p = Path::new("/x/init.lua");
        assert!(store.record(p, b"c", true).is_err());
        assert_eq!(store.check(p, b"c"), TrustDecision::Unknown);
    }
}
