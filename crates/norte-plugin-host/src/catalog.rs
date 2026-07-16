//! Catálogo de plugins (ADR 0022 D5/D6): descubre los `.wasm` locales y sus
//! manifiestos, y los ordena por categoría para el gestor de extensiones.

use std::path::{Path, PathBuf};

use crate::manifest::{Category, Manifest, ManifestError};

/// Nivel de una acción invocable (ADR 0022 D5): los tres NUNCA se mezclan. El
/// catálogo cubre `Plugin`; `Script` (Lua) y `BuiltIn` (comandos nativos) los
/// aporta la UI desde sus propias fuentes y los muestra en grupos aparte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// WASM de terceros, sandbox por capabilities.
    Plugin,
    /// Lua del usuario, permisos del usuario (no sandbox).
    Script,
    /// Comando nativo del binario.
    BuiltIn,
}

/// Un plugin instalado: su manifiesto + estado local.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Manifiesto validado.
    pub manifest: Manifest,
    /// Directorio del plugin (`~/.config/norte/plugins/<id>/`).
    pub dir: PathBuf,
    /// Activado por el usuario.
    pub enabled: bool,
    /// El usuario aprobó las capabilities declaradas (si no, `⚠ sin aprobar`
    /// y el host no lo carga — ADR 0022 D4).
    pub approved: bool,
}

/// Un manifiesto que no cargó, con su causa (para avisar en el gestor en vez de
/// desaparecer en silencio).
#[derive(Debug)]
pub struct LoadError {
    /// Directorio culpable.
    pub dir: PathBuf,
    /// La causa.
    pub error: ManifestError,
}

/// El catálogo de plugins descubiertos + los que fallaron al cargar.
#[derive(Debug, Default)]
pub struct Catalog {
    /// Plugins válidos, ordenados por categoría y luego por id.
    pub plugins: Vec<PluginEntry>,
    /// Manifiestos inválidos (se muestran como error, no se ocultan).
    pub errors: Vec<LoadError>,
}

impl Catalog {
    /// Descubre plugins en `root/<id>/plugin.toml`. No es I/O async: el host lo
    /// llama una vez al arrancar (o vía `spawn_blocking` desde contexto async).
    /// Un `root` inexistente = catálogo vacío (no es error).
    #[must_use]
    pub fn load_dir(root: &Path) -> Catalog {
        let mut cat = Catalog::default();
        let Ok(entries) = std::fs::read_dir(root) else {
            return cat;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let toml_path = dir.join("plugin.toml");
            let Ok(src) = std::fs::read_to_string(&toml_path) else {
                continue; // sin plugin.toml no es un plugin (no es error)
            };
            match Manifest::from_toml(&src) {
                Ok(manifest) => cat.plugins.push(PluginEntry {
                    manifest,
                    dir,
                    enabled: false,
                    approved: false,
                }),
                Err(error) => cat.errors.push(LoadError { dir, error }),
            }
        }
        cat.plugins.sort_by(|a, b| {
            a.manifest
                .category
                .as_str()
                .cmp(b.manifest.category.as_str())
                .then_with(|| a.manifest.id.cmp(&b.manifest.id))
        });
        cat
    }

    /// Agrupa los plugins por categoría, en orden estable — la base de la vista
    /// ordenada del gestor (ADR 0022 D5). Solo categorías con algún plugin.
    #[must_use]
    pub fn by_category(&self) -> Vec<(Category, Vec<&PluginEntry>)> {
        const ORDER: [Category; 5] = [
            Category::Previewer,
            Category::Provider,
            Category::Command,
            Category::Columns,
            Category::Hook,
        ];
        ORDER
            .into_iter()
            .filter_map(|cat| {
                let group: Vec<&PluginEntry> = self
                    .plugins
                    .iter()
                    .filter(|p| p.manifest.category == cat)
                    .collect();
                (!group.is_empty()).then_some((cat, group))
            })
            .collect()
    }
}
