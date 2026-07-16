//! Manifiesto `plugin.toml` (ADR 0022 D3): identidad + contribuciones por
//! interfaz (estilo `contributes` de `VSCode`) + capabilities.

use serde::Deserialize;

use crate::capability::Capabilities;

/// Categoría PRIMARIA del plugin = la interfaz WIT por la que se ordena en el
/// gestor (spec §7.1). Un plugin puede contribuir a varias, pero declara una
/// principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Genera previews para mimetypes.
    Previewer,
    /// Provider VFS de terceros.
    Provider,
    /// Comando invocable desde palette/keybinding.
    Command,
    /// Columnas custom en el listado.
    Columns,
    /// Hooks before/after de operaciones.
    Hook,
}

impl Category {
    /// Nombre estable (para agrupar en la UI y trazas).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Previewer => "previewer",
            Category::Provider => "provider",
            Category::Command => "command",
            Category::Columns => "columns",
            Category::Hook => "hook",
        }
    }
}

/// Un previewer declarado: los mimetypes que sabe pintar.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewerContrib {
    /// Patrones de mimetype (`text/*`, `application/json`).
    pub mimetypes: Vec<String>,
}

/// Un comando declarado.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContrib {
    /// Id estable (namespaced por el plugin al registrarlo).
    pub id: String,
    /// Título para la palette.
    pub title: String,
}

/// Una columna declarada.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnContrib {
    /// Id estable.
    pub id: String,
    /// Cabecera visible.
    pub header: String,
}

/// Un provider declarado: el scheme que sirve (`webdav`, …).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContrib {
    /// Scheme del VFS (sin `://`).
    pub scheme: String,
}

/// Un hook declarado: el evento al que engancha.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookContrib {
    /// Evento (`before-copy`, `after-copy`, `after-delete`, …).
    pub on: String,
}

/// Lo que el plugin APORTA, por interfaz. Todo opcional: un plugin de una sola
/// interfaz solo rellena la suya.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    /// Previewers.
    #[serde(default)]
    pub previewer: Vec<PreviewerContrib>,
    /// Comandos.
    #[serde(default)]
    pub command: Vec<CommandContrib>,
    /// Columnas.
    #[serde(default)]
    pub columns: Vec<ColumnContrib>,
    /// Providers.
    #[serde(default)]
    pub provider: Vec<ProviderContrib>,
    /// Hooks.
    #[serde(default)]
    pub hook: Vec<HookContrib>,
}

/// Bloque `[plugin]` del manifiesto.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginSection {
    id: String,
    name: String,
    publisher: String,
    version: String,
    category: Category,
}

/// Forma cruda del TOML (antes de validar).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    plugin: PluginSection,
    #[serde(default)]
    contributions: Contributions,
    #[serde(default)]
    capabilities: Capabilities,
}

/// El manifiesto ya validado de un plugin.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Id reverse-DNS único (`org.norte.syntax-preview`).
    pub id: String,
    /// Nombre legible.
    pub name: String,
    /// Publicador.
    pub publisher: String,
    /// Versión (`SemVer`, sin validar aquí).
    pub version: String,
    /// Categoría primaria (ordena el gestor).
    pub category: Category,
    /// Contribuciones por interfaz.
    pub contributions: Contributions,
    /// Capabilities declaradas.
    pub capabilities: Capabilities,
}

/// Error al cargar un manifiesto.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// El TOML no parsea o tiene claves desconocidas.
    #[error("plugin.toml inválido: {0}")]
    Toml(#[from] toml::de::Error),
    /// `plugin.id` vacío o no reverse-DNS (sin `.`).
    #[error("plugin.id inválido: se espera reverse-DNS (p. ej. `org.foo.bar`)")]
    Id,
    /// `exec` distinto de `none`: PROHIBIDO (spec §7.1, invariante dura).
    #[error("capability `exec` prohibida: debe ser `none` (o ausente)")]
    ExecForbidden,
}

/// `true` si `id` es un identificador reverse-DNS válido: uno o más segmentos
/// `[A-Za-z0-9-]+` separados por puntos, con al menos un punto, ningún segmento
/// vacío (ni punto inicial/final), longitud total `1..=128`.
fn is_valid_plugin_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let mut segments = 0_usize;
    for segment in id.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        segments += 1;
    }
    // Al menos un punto ⇒ al menos dos segmentos.
    segments >= 2
}

impl Manifest {
    /// Parsea y VALIDA un `plugin.toml`.
    ///
    /// # Errors
    /// [`ManifestError`] si el TOML no parsea, el `id` no es reverse-DNS, o se
    /// declara `exec` distinto de `none` (prohibido sin excepción).
    pub fn from_toml(src: &str) -> Result<Self, ManifestError> {
        let raw: ManifestRaw = toml::from_str(src)?;
        // Invariante dura: exec SIEMPRE none.
        if raw
            .capabilities
            .exec
            .as_deref()
            .is_some_and(|e| e != "none")
        {
            return Err(ManifestError::ExecForbidden);
        }
        // id reverse-DNS REAL: uno o más segmentos `[A-Za-z0-9-]+` separados por
        // puntos, con al menos un punto, sin segmento vacío (ni punto inicial/
        // final), longitud total 1..=128. Endurecido más allá de "contiene un
        // punto" porque el id crudo del manifiesto termina en logs y en el modal
        // de aprobación (T5): un id con saltos de línea, comillas o espacios
        // permitiría inyección en el log o spoofing del diálogo de consentimiento.
        if !is_valid_plugin_id(&raw.plugin.id) {
            return Err(ManifestError::Id);
        }
        Ok(Self {
            id: raw.plugin.id,
            name: raw.plugin.name,
            publisher: raw.plugin.publisher,
            version: raw.plugin.version,
            category: raw.plugin.category,
            contributions: raw.contributions,
            capabilities: raw.capabilities,
        })
    }
}
