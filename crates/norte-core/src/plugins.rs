//! Registro de plugins del daemon (M4-P3): descubre el catálogo local
//! ([`norte_plugin_host::Catalog`]), le fusiona el estado aprobado/activado que
//! persiste el usuario, y lo expone por protocolo ([`norte_proto::methods`]).
//!
//! El [`PluginEntry`](norte_plugin_host::PluginEntry) del catálogo nace SIEMPRE
//! `approved = false` / `enabled = false` (el descubridor no conoce el estado
//! del usuario): la verdad del estado vive en `plugins-state.toml` y este
//! registro es quien la fusiona.
//!
//! ## Formato de `plugins-state.toml`
//!
//! El id de un plugin es reverse-DNS (`org.norte.demo`) — CON PUNTOS. Escrito a
//! pelo como cabecera (`[org.norte.demo]`) TOML lo leería como tablas anidadas
//! (`org` → `norte` → `demo`), NO como una clave literal. Por eso el estado va
//! bajo una tabla `[plugins]` con la clave ENTRECOMILLADA:
//!
//! ```toml
//! [plugins]
//! "org.norte.demo" = { approved = true, enabled = false }
//! ```
//!
//! `toml_edit` entrecomilla la clave con puntos al re-emitir, así que el
//! round-trip descubrir → persistir → descubrir conserva el id intacto.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use norte_plugin_host::Catalog;
use norte_proto::methods::{PluginInfo, PluginListResult, PluginLoadError};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

/// Estado que el usuario fija sobre un plugin descubierto. Ausente = ambos
/// `false` (descubierto pero sin aprobar ni activar).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PluginState {
    /// Un humano aprobó las capabilities declaradas.
    pub approved: bool,
    /// Un humano lo tiene activado.
    pub enabled: bool,
}

/// Fallo al ejecutar un comando de plugin. Los tres primeros variantes son el
/// veredicto fail-closed del consentimiento (desconocido / sin aprobar /
/// desactivado); los dos últimos, fallos de artefacto o de runtime.
#[derive(Debug, thiserror::Error)]
pub enum PluginRunError {
    /// No hay ningún plugin descubierto con ese id.
    #[error("plugin desconocido: {0}")]
    Unknown(String),
    /// El plugin existe pero un humano no ha aprobado sus capabilities.
    #[error("plugin sin aprobar: {0}")]
    NotApproved(String),
    /// El plugin está aprobado pero desactivado.
    #[error("plugin desactivado: {0}")]
    Disabled(String),
    /// El plugin no tiene `plugin.wasm` en su directorio. Lleva el ID (no la
    /// ruta absoluta: revelaría el home del usuario a un agente que llame a
    /// `plugin.run_command` — coherente con la redacción de `list()`,
    /// security-reviewer M4-P4).
    #[error("el plugin {0} no tiene binario (plugin.wasm)")]
    NoBinary(String),
    /// El runtime WASM falló al compilar, instanciar o ejecutar el componente.
    #[error("runtime: {0}")]
    Runtime(#[from] norte_plugin_host::RuntimeError),
}

/// Tope de bytes que el core lee de un archivo al PREVISUALIZAR (1 MiB,
/// anti-DoS): el handler del daemon lee como mucho esto y se lo pasa al guest.
/// El guest de M4-P2 tiene ADEMÁS su propio límite; este es la primera barrera,
/// en el lado del host, para no cargar un archivo enorme en memoria solo porque
/// alguien pidió su preview.
pub(crate) const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Adivina el mimetype por EXTENSIÓN (heurística ligera, sin dep de sniffing).
/// Un archivo sin extensión reconocible → `application/octet-stream` (ningún
/// previewer `text/*` lo tomará). NO lee el contenido. `pub(crate)` para el
/// handler del daemon.
pub(crate) fn guess_mimetype(path: &norte_proto::VPath) -> &'static str {
    let ext = path
        .file_name()
        .map(norte_proto::Segment::as_bytes)
        .and_then(|n| std::str::from_utf8(n).ok())
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()));
    match ext.as_deref() {
        Some("txt" | "md" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        _ => "application/octet-stream",
    }
}

/// ¿El glob `pat` (`text/*` o exacto `application/json`) casa `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}

/// Registro de plugins: catálogo descubierto + estado persistido fusionado.
#[derive(Debug)]
pub struct PluginRegistry {
    config_dir: PathBuf,
    state: BTreeMap<String, PluginState>,
    catalog: Catalog,
}

impl PluginRegistry {
    /// Nombre del fichero de estado dentro de `config_dir`.
    const STATE_FILE: &'static str = "plugins-state.toml";

    /// Descubre el catálogo en `config_dir/plugins/<id>/plugin.toml` y le fusiona
    /// el estado de `config_dir/plugins-state.toml`.
    ///
    /// Un `config_dir/plugins` inexistente = catálogo vacío (no es error). Un
    /// `plugins-state.toml` ausente = estado vacío.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidData`] si `plugins-state.toml` existe pero no es
    /// TOML válido; cualquier otro error de I/O al leerlo se propaga tal cual.
    pub fn discover(config_dir: &Path) -> io::Result<Self> {
        let catalog = Catalog::load_dir(&config_dir.join("plugins"));
        let state = Self::read_state(&config_dir.join(Self::STATE_FILE))?;
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            state,
            catalog,
        })
    }

    /// Un registro VACÍO anclado en `config_dir`, sin tocar el FS: catálogo sin
    /// plugins y estado sin fusionar. Lo usa el daemon como degradación si el
    /// descubrimiento falla (p. ej. `plugins-state.toml` corrupto): un fichero
    /// de estado roto no debe impedir arrancar. Persistir sobre él re-crea el
    /// estado desde cero bajo `config_dir`.
    #[must_use]
    pub fn empty(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            state: BTreeMap::new(),
            catalog: Catalog::default(),
        }
    }

    /// El catálogo descubierto fusionado con el estado persistido, en la forma
    /// del protocolo.
    #[must_use]
    pub fn list(&self) -> PluginListResult {
        let plugins = self
            .catalog
            .plugins
            .iter()
            .map(|e| {
                let st = self.state.get(&e.manifest.id).copied().unwrap_or_default();
                PluginInfo {
                    id: e.manifest.id.clone(),
                    name: e.manifest.name.clone(),
                    publisher: e.manifest.publisher.clone(),
                    version: e.manifest.version.clone(),
                    category: e.manifest.category.as_str().to_string(),
                    capabilities: e
                        .manifest
                        .capabilities
                        .badges()
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    approved: st.approved,
                    enabled: st.enabled,
                }
            })
            .collect();
        let errors = self
            .catalog
            .errors
            .iter()
            .map(|e| PluginLoadError {
                // Solo el NOMBRE del directorio del plugin, nunca la ruta
                // absoluta: revelaría el home del usuario (`~/.config/norte/...`)
                // a un agente que llame a `plugin.list`. El basename basta para
                // que un humano identifique el plugin roto.
                dir: e
                    .dir
                    .file_name()
                    .map_or_else(|| e.dir.to_string_lossy(), |n| n.to_string_lossy())
                    .into_owned(),
                reason: e.error.to_string(),
            })
            .collect();
        PluginListResult { plugins, errors }
    }

    /// El directorio de configuración donde vive `plugins-state.toml`. Lo usa el
    /// daemon para persistir FUERA del lock (regla 2): captura el dir bajo el
    /// lock y escribe en `spawn_blocking`.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Copia del estado aprobado/activado, para persistir fuera del lock (el
    /// daemon lo mueve a `spawn_blocking` junto a [`Self::config_dir`], regla 2).
    #[must_use]
    pub fn state_snapshot(&self) -> BTreeMap<String, PluginState> {
        self.state.clone()
    }

    /// Muta EN MEMORIA el estado `approved` de un plugin descubierto, SIN I/O.
    ///
    /// Devuelve `true` si el plugin existe en el catálogo (y se aplicó), o
    /// `false` si el id es desconocido — en cuyo caso no se toca nada (no se
    /// ensucia el estado con plugins fantasma). La persistencia es
    /// responsabilidad del llamante (daemon: `persist_state` en
    /// `spawn_blocking`; embebido: [`Self::set_approval`]).
    pub fn set_approval_in_memory(&mut self, id: &str, approved: bool) -> bool {
        if !self.is_known(id) {
            return false;
        }
        self.state.entry(id.to_string()).or_default().approved = approved;
        true
    }

    /// Muta EN MEMORIA el estado `enabled`. Semántica idéntica a
    /// [`Self::set_approval_in_memory`].
    pub fn set_enabled_in_memory(&mut self, id: &str, enabled: bool) -> bool {
        if !self.is_known(id) {
            return false;
        }
        self.state.entry(id.to_string()).or_default().enabled = enabled;
        true
    }

    /// Fija el estado `approved` de un plugin descubierto y lo persiste, todo en
    /// el MISMO hilo. Es la API para el uso EMBEBIDO, que ya corre dentro de un
    /// `spawn_blocking` (backend del frontend). El daemon NO usa esto: separa la
    /// mutación ([`Self::set_approval_in_memory`]) de la persistencia
    /// (`persist_state`) para no bloquear el reactor (regla 2).
    ///
    /// Devuelve `Ok(true)` si el plugin existe (y se aplicó+persistió), o
    /// `Ok(false)` si el id es desconocido — sin persistir nada.
    ///
    /// # Errors
    /// Errores de I/O al re-leer o escribir `plugins-state.toml`, o
    /// [`io::ErrorKind::InvalidData`] si el fichero existente es TOML corrupto.
    pub fn set_approval(&mut self, id: &str, approved: bool) -> io::Result<bool> {
        if !self.set_approval_in_memory(id, approved) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Fija el estado `enabled` de un plugin descubierto y lo persiste (uso
    /// EMBEBIDO). Semántica de retorno idéntica a [`Self::set_approval`].
    ///
    /// # Errors
    /// Igual que [`Self::set_approval`].
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> io::Result<bool> {
        if !self.set_enabled_in_memory(id, enabled) {
            return Ok(false);
        }
        persist_state(&self.config_dir, &self.state)?;
        Ok(true)
    }

    /// Valida el consentimiento (fail-closed) y RESUELVE el `.wasm` +
    /// capabilities de un plugin, SIN ejecutarlo. Es BARATO (lectura del
    /// catálogo/estado en memoria + un `is_file`): pensado para correr bajo el
    /// `Mutex<PluginRegistry>` del daemon, que después ejecuta lo PESADO
    /// (`PluginRuntime::instantiate` + `run_command`, que compila el componente
    /// WASM) FUERA del lock, en un `spawn_blocking` (regla 2). El `.wasm` es
    /// `<dir>/plugin.wasm` por convención (ADR 0022 D6); las capabilities son
    /// las DEL MANIFIESTO (el sandbox de M4-P2 las hace cumplir).
    ///
    /// # Errors
    /// [`PluginRunError`] `Unknown`/`NotApproved`/`Disabled`/`NoBinary` según el
    /// veredicto de consentimiento; nunca `Runtime` (no ejecuta nada).
    pub fn resolve_runnable(
        &self,
        id: &str,
    ) -> Result<(PathBuf, norte_plugin_host::Capabilities), PluginRunError> {
        let entry = self
            .catalog
            .plugins
            .iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginRunError::Unknown(id.to_string()))?;
        let st = self.state.get(id).copied().unwrap_or_default();
        if !st.approved {
            return Err(PluginRunError::NotApproved(id.to_string()));
        }
        if !st.enabled {
            return Err(PluginRunError::Disabled(id.to_string()));
        }
        let wasm = entry.dir.join("plugin.wasm");
        if !wasm.is_file() {
            return Err(PluginRunError::NoBinary(id.to_string()));
        }
        Ok((wasm, entry.manifest.capabilities.clone()))
    }

    /// Resuelve el PRIMER previewer APROBADO y ACTIVADO cuyo mimetype declarado
    /// case `mime`, devolviendo `(id, name, wasm_path, capabilities)`; `None` si
    /// ninguno aplica. Fail-closed: un previewer no consentido jamás se elige.
    /// Barato: el caller lee los bytes del archivo y ejecuta fuera del lock.
    #[must_use]
    pub fn resolve_previewer(
        &self,
        mime: &str,
    ) -> Option<(
        String,
        String,
        std::path::PathBuf,
        norte_plugin_host::Capabilities,
    )> {
        self.catalog.plugins.iter().find_map(|e| {
            let st = self.state.get(&e.manifest.id).copied().unwrap_or_default();
            if !st.approved || !st.enabled {
                return None;
            }
            let handles = e
                .manifest
                .contributions
                .previewer
                .iter()
                .flat_map(|c| c.mimetypes.iter())
                .any(|pat| mimetype_matches(pat, mime));
            if !handles {
                return None;
            }
            let wasm = e.dir.join("plugin.wasm");
            wasm.is_file().then(|| {
                (
                    e.manifest.id.clone(),
                    e.manifest.name.clone(),
                    wasm,
                    e.manifest.capabilities.clone(),
                )
            })
        })
    }

    /// Ejecuta un comando de un plugin APROBADO y ACTIVADO (fail-closed: un
    /// plugin no consentido JAMÁS se ejecuta). Delega la validación en
    /// [`Self::resolve_runnable`] y ejecuta a continuación. SÍNCRONO (compila e
    /// instancia el componente): el caller lo corre en `spawn_blocking` (regla
    /// 2). El daemon prefiere separar resolución (bajo lock) y ejecución (fuera
    /// del lock) llamando a [`Self::resolve_runnable`] directamente.
    ///
    /// # Errors
    /// [`PluginRunError`] si el plugin no existe, no está aprobado, está
    /// desactivado, no tiene binario, o el runtime falla.
    pub fn run_command(
        &self,
        runtime: &norte_plugin_host::PluginRuntime,
        id: &str,
        command: &str,
        arg: &str,
    ) -> Result<String, PluginRunError> {
        let (wasm, caps) = self.resolve_runnable(id)?;
        let mut inst = runtime.instantiate(&wasm, caps)?;
        Ok(inst.run_command(command, arg)?)
    }

    /// `true` si `id` corresponde a un plugin realmente descubierto.
    fn is_known(&self, id: &str) -> bool {
        self.catalog.plugins.iter().any(|e| e.manifest.id == id)
    }

    /// Lee el estado persistido. Ausente = vacío; corrupto = `InvalidData`.
    fn read_state(path: &Path) -> io::Result<BTreeMap<String, PluginState>> {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
        };
        let doc = src
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut map = BTreeMap::new();
        if let Some(plugins) = doc.get("plugins").and_then(Item::as_table_like) {
            for (key, item) in plugins.iter() {
                let Some(tbl) = item.as_table_like() else {
                    continue;
                };
                let flag = |name: &str| tbl.get(name).and_then(Item::as_bool).unwrap_or(false);
                map.insert(
                    key.to_string(),
                    PluginState {
                        approved: flag("approved"),
                        enabled: flag("enabled"),
                    },
                );
            }
        }
        Ok(map)
    }
}

/// Re-emite `config_dir/plugins-state.toml` preservando el resto del fichero,
/// con una entrada por cada plugin con estado. La clave con puntos se
/// entrecomilla.
///
/// Es una función LIBRE (no un método) para que el daemon pueda persistir en un
/// `spawn_blocking` a partir de un snapshot del estado, sin sostener el
/// `Mutex<PluginRegistry>` a través del `.await` (regla 2).
///
/// El write es ATÓMICO: se escribe a un temporal en el MISMO directorio y luego
/// `rename` sobre el destino. Un crash a mitad no corrompe el store durable de
/// una decisión de seguridad (consentimiento de capabilities).
///
/// # Errors
/// Errores de I/O al re-leer, escribir el temporal o renombrar; o
/// [`io::ErrorKind::InvalidData`] si el fichero existente es TOML corrupto.
pub(crate) fn persist_state(
    config_dir: &Path,
    state: &BTreeMap<String, PluginState>,
) -> io::Result<()> {
    let path = config_dir.join(PluginRegistry::STATE_FILE);
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<DocumentMut>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let root = doc.as_table_mut();
    if !root.get("plugins").is_some_and(Item::is_table_like) {
        root.insert("plugins", Item::Table(Table::new()));
    }
    // `insert` con una clave con puntos guarda la clave LITERAL; toml_edit la
    // entrecomilla al render (no la interpreta como tablas anidadas).
    let plugins = root["plugins"]
        .as_table_mut()
        .expect("plugins es una tabla: se acaba de garantizar arriba");
    for (id, st) in state {
        let mut inline = InlineTable::new();
        inline.insert("approved", Value::from(st.approved));
        inline.insert("enabled", Value::from(st.enabled));
        plugins.insert(id, Item::Value(Value::InlineTable(inline)));
    }
    // Write atómico: temporal en el mismo dir (mismo filesystem → rename atómico)
    // + rename sobre el destino. El sufijo con el pid evita pisar el temporal de
    // otro proceso que persista a la vez.
    let tmp = config_dir.join(format!(
        "{}.tmp.{}",
        PluginRegistry::STATE_FILE,
        std::process::id()
    ));
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Manifiesto válido mínimo (copiado del doctest de `norte-plugin-host`).
    const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

    /// Crea `config_dir/plugins/<id>/plugin.toml` con `src`.
    fn write_plugin(config_dir: &Path, id: &str, src: &str) {
        let dir = config_dir.join("plugins").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), src).unwrap();
    }

    #[test]
    fn plugins_discover_lista_un_plugin_sin_estado() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1);
        let p = &list.plugins[0];
        assert_eq!(p.id, "org.norte.demo");
        assert_eq!(p.category, "command");
        assert!(!p.approved);
        assert!(!p.enabled);
        assert!(p.capabilities.iter().any(|c| c == "fs-read"));
        assert!(list.errors.is_empty());
    }

    #[test]
    fn plugins_set_estado_se_refleja_y_persiste() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval("org.norte.demo", true).unwrap());
        assert!(reg.set_enabled("org.norte.demo", true).unwrap());

        let p = &reg.list().plugins[0];
        assert!(p.approved);
        assert!(p.enabled);

        // Una NUEVA discover del mismo dir lo recuerda (persistió).
        let reg2 = PluginRegistry::discover(tmp.path()).unwrap();
        let p2 = &reg2.list().plugins[0];
        assert!(p2.approved, "approved debe persistir");
        assert!(p2.enabled, "enabled debe persistir");
        assert_eq!(
            p2.id, "org.norte.demo",
            "el id-con-puntos debe volver intacto"
        );
    }

    #[test]
    fn plugins_set_de_id_inexistente_no_persiste_basura() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(!reg.set_approval("org.norte.fantasma", true).unwrap());
        assert!(!reg.set_enabled("org.norte.fantasma", true).unwrap());

        // No se creó el fichero de estado (nada que persistir).
        assert!(
            !tmp.path().join("plugins-state.toml").exists(),
            "un id desconocido no debe crear plugins-state.toml"
        );
    }

    #[test]
    fn plugins_manifiesto_roto_aparece_en_errors_sin_tumbar_discover() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        write_plugin(tmp.path(), "roto", "esto no es toml [ valido =");

        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let list = reg.list();

        assert_eq!(list.plugins.len(), 1, "el válido sigue cargando");
        assert_eq!(list.errors.len(), 1, "el roto se reporta, no desaparece");
        assert!(list.errors[0].dir.contains("roto"));
    }

    #[test]
    fn plugins_state_id_con_puntos_round_trip() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);

        // Persistimos estado para un id con PUNTOS.
        {
            let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
            assert!(reg.set_approval("org.norte.demo", true).unwrap());
        }

        // El fichero debe llevar la clave ENTRECOMILLADA, no anidada.
        let raw = std::fs::read_to_string(tmp.path().join("plugins-state.toml")).unwrap();
        assert!(
            raw.contains("\"org.norte.demo\""),
            "la clave debe ir entrecomillada, no como [org.norte.demo]: {raw}"
        );

        // Y una discover fresca recupera el MISMO id con su estado.
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let st = reg.state.get("org.norte.demo").copied();
        assert_eq!(
            st,
            Some(PluginState {
                approved: true,
                enabled: false
            }),
            "el estado del id-con-puntos debe recuperarse bajo el mismo id literal"
        );
    }

    /// Manifiesto `command` mínimo, sin capabilities especiales, para los tests
    /// de ejecución fail-closed.
    const CMD_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cmd"
name = "Cmd"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

    #[test]
    fn plugins_run_command_sin_aprobar_es_not_approved() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.cmd"),
            "un plugin sin aprobar JAMÁS se ejecuta: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_aprobado_sin_activar_es_disabled() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Disabled(ref id) if id == "org.norte.cmd"),
            "aprobado pero desactivado no se ejecuta: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_activado_sin_wasm_es_no_binary() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.cmd", true));
        assert!(reg.set_enabled_in_memory("org.norte.cmd", true));
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.cmd", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(&err, PluginRunError::NoBinary(id) if id == "org.norte.cmd"),
            "sin plugin.wasm el runtime no arranca: {err:?}"
        );
    }

    #[test]
    fn plugins_run_command_id_inexistente_es_unknown() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.cmd", CMD_MANIFEST);
        let reg = PluginRegistry::discover(tmp.path()).unwrap();
        let rt = norte_plugin_host::PluginRuntime::new().unwrap();

        let err = reg
            .run_command(&rt, "org.norte.fantasma", "echo", "hola")
            .unwrap_err();
        assert!(
            matches!(err, PluginRunError::Unknown(ref id) if id == "org.norte.fantasma"),
            "un id desconocido es Unknown: {err:?}"
        );
    }

    #[test]
    fn plugins_state_corrupto_es_invalid_data() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.demo", DEMO_MANIFEST);
        std::fs::write(
            tmp.path().join("plugins-state.toml"),
            "esto no es [ toml valido =",
        )
        .unwrap();

        let err = PluginRegistry::discover(tmp.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Manifiesto de un previewer que declara `text/*`.
    const PREV_MANIFEST: &str = r#"
[plugin]
id = "org.norte.prev"
name = "Prev"
publisher = "norte"
version = "0.1.0"
category = "previewer"
[[contributions.previewer]]
mimetypes = ["text/*"]
"#;

    fn vpath(s: &str) -> norte_proto::VPath {
        norte_proto::VPath::parse(s).unwrap()
    }

    #[test]
    fn plugins_guess_mimetype_por_extension() {
        assert_eq!(guess_mimetype(&vpath("file:///a.txt")), "text/plain");
        assert_eq!(guess_mimetype(&vpath("file:///a.json")), "application/json");
        assert_eq!(
            guess_mimetype(&vpath("file:///a")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_mimetype(&vpath("file:///a.UNKNOWN")),
            "application/octet-stream"
        );
    }

    #[test]
    fn plugins_mimetype_matches_glob_y_exacto() {
        assert!(mimetype_matches("text/*", "text/plain"));
        assert!(!mimetype_matches("text/*", "application/json"));
        assert!(mimetype_matches("application/json", "application/json"));
        // No casa parcial: prefijo textual sin la barra no es glob.
        assert!(!mimetype_matches("application/json", "application/json5"));
        assert!(!mimetype_matches("text/plain", "text/plai"));
    }

    #[test]
    fn plugins_resolve_previewer_fail_closed_y_por_mimetype() {
        let tmp = TempDir::new().unwrap();
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);
        // `plugin.wasm` VACÍO: `is_file()` no valida contenido, solo presencia.
        std::fs::write(
            tmp.path()
                .join("plugins")
                .join("org.norte.prev")
                .join("plugin.wasm"),
            b"",
        )
        .unwrap();

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();

        // Descubierto pero SIN aprobar/activar → fail-closed.
        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "un previewer no consentido jamás se elige"
        );

        // Aprobado + activado → resuelve para el mimetype que casa el glob.
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        let got = reg.resolve_previewer("text/plain");
        assert!(got.is_some(), "text/plain casa text/*");
        let (id, name, wasm, _caps) = got.unwrap();
        assert_eq!(id, "org.norte.prev");
        assert_eq!(name, "Prev");
        assert!(wasm.ends_with("plugin.wasm"));

        // Un mimetype que no casa el glob declarado → None.
        assert!(
            reg.resolve_previewer("application/json").is_none(),
            "application/json no casa text/*"
        );
    }

    #[test]
    fn plugins_resolve_previewer_sin_wasm_es_none() {
        let tmp = TempDir::new().unwrap();
        // Sin escribir plugin.wasm: aunque esté consentido, no hay binario.
        write_plugin(tmp.path(), "org.norte.prev", PREV_MANIFEST);

        let mut reg = PluginRegistry::discover(tmp.path()).unwrap();
        assert!(reg.set_approval_in_memory("org.norte.prev", true));
        assert!(reg.set_enabled_in_memory("org.norte.prev", true));

        assert!(
            reg.resolve_previewer("text/plain").is_none(),
            "sin plugin.wasm no hay nada que ejecutar"
        );
    }
}
