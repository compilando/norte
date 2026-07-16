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
                dir: e.dir.display().to_string(),
                reason: e.error.to_string(),
            })
            .collect();
        PluginListResult { plugins, errors }
    }

    /// Fija el estado `approved` de un plugin descubierto y lo persiste.
    ///
    /// Devuelve `Ok(true)` si el plugin existe en el catálogo (y se aplicó), o
    /// `Ok(false)` si el id es desconocido — en cuyo caso NO se persiste nada
    /// (no se ensucia el fichero con estado de plugins fantasma).
    ///
    /// # Errors
    /// Errores de I/O al re-leer o escribir `plugins-state.toml`, o
    /// [`io::ErrorKind::InvalidData`] si el fichero existente es TOML corrupto.
    pub fn set_approval(&mut self, id: &str, approved: bool) -> io::Result<bool> {
        if !self.is_known(id) {
            return Ok(false);
        }
        self.state.entry(id.to_string()).or_default().approved = approved;
        self.persist()?;
        Ok(true)
    }

    /// Fija el estado `enabled` de un plugin descubierto y lo persiste.
    ///
    /// Semántica de retorno idéntica a [`Self::set_approval`].
    ///
    /// # Errors
    /// Igual que [`Self::set_approval`].
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> io::Result<bool> {
        if !self.is_known(id) {
            return Ok(false);
        }
        self.state.entry(id.to_string()).or_default().enabled = enabled;
        self.persist()?;
        Ok(true)
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

    /// Re-emite `plugins-state.toml` preservando el resto del fichero, con una
    /// entrada por cada plugin con estado. La clave con puntos se entrecomilla.
    fn persist(&self) -> io::Result<()> {
        let path = self.config_dir.join(Self::STATE_FILE);
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
        for (id, st) in &self.state {
            let mut inline = InlineTable::new();
            inline.insert("approved", Value::from(st.approved));
            inline.insert("enabled", Value::from(st.enabled));
            plugins.insert(id, Item::Value(Value::InlineTable(inline)));
        }
        std::fs::write(&path, doc.to_string())
    }
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
}
