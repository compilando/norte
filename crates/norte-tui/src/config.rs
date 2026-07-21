//! Layered configuration (ADR 0007): defaults, system, user, project, then
//! command-line flags. Scalar fields use last-present-value wins. `keymap.toml`
//! layers accumulate into the effective keymap (ADR 0006). Invalid startup
//! configuration reports the file and field; hot reload retains the last valid
//! configuration.
//!
//! This module deliberately reads the frontend's own configuration with
//! `std::fs`; using providers would be circular because configuration selects
//! how the TUI starts. Async callers use [`load_async`].

use std::path::{Path, PathBuf};

use norte_proto::VPath;
use serde::Deserialize;

use crate::keymap::{KeymapFile, parse_keymap};
use crate::nav;

/// Default keymap preset (decision from 2026-07-10).
pub const DEFAULT_PRESET: &str = "orthodox";

/// General configuration from `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NorteToml {
    /// Keymap settings.
    #[serde(default)]
    pub keymap: KeymapSection,
    /// User-interface settings.
    #[serde(default)]
    pub ui: UiSection,
    /// Daemon settings.
    #[serde(default)]
    pub daemon: DaemonSection,
    /// Archive-provider limits (`[archive]`, #95.2).
    #[serde(default)]
    pub archive: ArchiveSection,
    /// Favourite directories shown by `Ctrl+D`.
    ///
    /// Entries accumulate across layers instead of replacing lower-layer
    /// values. The project layer is excluded while [`load`] merges the list.
    /// An absent value contributes no favourites from that layer.
    #[serde(default)]
    pub hotlist: Vec<HotlistEntry>,
}

/// One `[[hotlist]]` entry as stored in `norte.toml`.
///
/// `path` contains an unvalidated wire value such as `scheme://...`, including
/// valid remote schemes. [`load`] validates it as a [`VPath`] while merging
/// layers. One invalid entry is handled independently and does not prevent the
/// rest of `norte.toml` from loading.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HotlistEntry {
    /// Name displayed in the `Ctrl+D` popup.
    pub name: String,
    /// Path in wire form, not yet validated.
    pub path: String,
}

/// The `[archive]` section of `norte.toml` (#95.2): local anti-bomb limits
/// for browsing zip/tar/tar.gz containers. Absent values keep the compiled
/// defaults. Applied at startup on the embedded engine only — a container
/// that exceeds them fails with `LimitExceeded`, never silently truncates.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArchiveSection {
    /// Maximum indexed entries per container (default 500000).
    #[serde(default)]
    pub max_entries: Option<u64>,
    /// Decompression budget in bytes for indexing a `tar.gz` (default 64 GiB).
    #[serde(default)]
    pub max_decompressed_bytes: Option<u64>,
}

/// The `[daemon]` section of `norte.toml` (ADR 0011).
///
/// Transport mode is selected at startup and is not hot reloaded. Changing it
/// requires restarting the frontend.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DaemonSection {
    /// `embedded` for immediate startup (the default), or `daemon`.
    #[serde(default)]
    pub mode: Option<DaemonMode>,
    /// Daemon socket path. When absent, use the operating-system default.
    #[serde(default)]
    pub socket: Option<PathBuf>,
}

/// Core transport. This changes transport only, not behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    /// Core in-process (default).
    Embedded,
    /// Connect to the Unix-domain-socket daemon (Unix only; ADR 0011).
    Daemon,
}

/// The `[ui]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// Language (`es` or `en`). When absent, negotiate from the environment.
    #[serde(default)]
    pub lang: Option<String>,
    /// Theme preset name (`default`, `catppuccin-mocha`, `gruvbox-dark`,
    /// `nord`, `gruvbox-light`, or `catppuccin-latte`) or a path to a custom
    /// TOML theme (ADR 0020). When absent, use `default`.
    #[serde(default)]
    pub theme: Option<String>,
    /// Quick-search mode for `/`: `"filter"` narrows the listing (the default),
    /// while `"jump"` moves the cursor without changing the listing.
    ///
    /// [`load`] rejects other values so its diagnostic can include the source
    /// configuration path. Invalid values never silently fall back.
    #[serde(default)]
    pub quick_search: Option<String>,
}

/// The `[keymap]` section of `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Base preset (`orthodox`, `vim`, or `cua`). Inherit when absent.
    #[serde(default)]
    pub preset: Option<String>,
}

/// Error de carga de config. Siempre con el ARCHIVO en el diagnóstico.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// No se pudo leer un archivo que existe.
    #[error("no se pudo leer {}: {source}", path.display())]
    Io {
        /// El archivo.
        path: PathBuf,
        /// La causa.
        source: std::io::Error,
    },
    /// TOML inválido o con claves desconocidas.
    #[error("{}: {message}", path.display())]
    Toml {
        /// El archivo.
        path: PathBuf,
        /// Diagnóstico del parser (incluye campo y posición).
        message: String,
    },
}

/// Diagnóstico COMPACTO de un error de `toml`: posición + mensaje semántico.
/// El `Display` multilínea del crate cita ENTERA la línea del fichero —
/// contenido potencialmente hostil/kilométrico que además desplazaría lo
/// accionable («unknown field …», que va al final) fuera del tope de la
/// barra (#73).
pub(crate) fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// Clase de una capa de config (ADR 0007), en precedencia ASCENDENTE. Se
/// lleva POR DIR en [`Layers`] (deuda #75) en vez de inferirse por la
/// POSICIÓN en `dirs`: en Windows sin `%ProgramData%` la capa `System` está
/// ausente, así que `%APPDATA%` (`User`) caería en el índice 0 y la
/// inferencia posicional la etiquetaría como `System`. `Layer` reexporta
/// por `crate::lua` para el `init.lua` (config es dueña del concepto de
/// capa; el scripting solo lo consume).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// `/etc/norte` (o `%ProgramData%\norte`).
    System,
    /// `$XDG_CONFIG_HOME/norte` (`~/.config/norte`; `%APPDATA%\norte`).
    User,
    /// `./.norte` — SOLO tras trust (ADR 0026).
    Project,
}

/// Los directorios de capas, en precedencia ASCENDENTE, cada uno con su
/// [`Layer`] (deuda #75: el kind viaja POR DIR, no se infiere por posición).
#[derive(Debug, Clone)]
pub struct Layers {
    /// sistema → usuario → proyecto (los ausentes simplemente no aportan),
    /// cada dir etiquetado con su clase de capa.
    pub dirs: Vec<(PathBuf, Layer)>,
}

/// Capas estándar (ADR 0007): `/etc/norte` (`%ProgramData%\norte`),
/// `$XDG_CONFIG_HOME/norte` (`~/.config/norte`; `%APPDATA%\norte`) y
/// `./.norte`.
#[must_use]
pub fn standard_layers() -> Layers {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(pd) = std::env::var_os("ProgramData") {
            dirs.push((PathBuf::from(pd).join("norte"), Layer::System));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push((PathBuf::from(appdata).join("norte"), Layer::User));
        }
    } else {
        dirs.push((PathBuf::from("/etc/norte"), Layer::System));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            dirs.push((PathBuf::from(xdg).join("norte"), Layer::User));
        } else if let Some(home) = std::env::var_os("HOME") {
            dirs.push((PathBuf::from(home).join(".config/norte"), Layer::User));
        }
    }
    dirs.push((PathBuf::from(".norte"), Layer::Project));
    Layers { dirs }
}

/// El directorio de config del USUARIO (donde se persiste una preferencia como
/// el tema): `$XDG_CONFIG_HOME/norte` (`~/.config/norte`) o `%APPDATA%\norte`.
/// `None` si el entorno no lo define (CI sin HOME): el caller avisa.
#[must_use]
pub fn user_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("norte"))
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(xdg).join("norte"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/norte"))
    }
}

/// Fija `[ui].theme = name` en el `norte.toml` del usuario, PRESERVANDO
/// comentarios y formato (`toml_edit`). Crea el fichero/directorio si no
/// existen. Devuelve la ruta escrita.
///
/// # Errors
/// [`std::io::Error`] si no hay dir de usuario, el TOML existente no parsea, o
/// falla el I/O.
pub fn persist_ui_theme(name: &str) -> std::io::Result<PathBuf> {
    let dir = user_config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sin directorio de config de usuario",
        )
    })?;
    persist_ui_theme_to(&dir, name)
}

/// Como [`persist_ui_theme`] pero en un `dir` explícito (sin depender del
/// entorno — la base testeable).
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O.
pub fn persist_ui_theme_to(dir: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    // Una tabla `[ui]` recién creada sería IMPLÍCITA (se emitiría como
    // `ui.theme = …` en vez de bajo `[ui]`): se crea EXPLÍCITA para que el
    // fichero nuevo tenga una sección legible; la ya existente se respeta.
    let ui = doc.as_table_mut().entry("ui").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    ui["theme"] = toml_edit::value(name);
    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}

/// Añade (o reemplaza si `name` ya existe) una entrada `[[hotlist]]` en el
/// `norte.toml` de `dir`, PRESERVANDO comentarios y formato (mismo patrón
/// `toml_edit` que [`persist_ui_theme_to`]). `wire_path` se guarda TAL
/// CUAL — la validación a [`VPath`] ocurre al releer (`load`), no aquí:
/// persistir no debe rechazar un path que el propio `norte` todavía no
/// sabe interpretar (p.ej. un scheme nuevo de un provider futuro).
///
/// BLOQUEANTE: hace I/O de FS síncrono. El caller (T5) DEBE envolverla en
/// `tokio::task::spawn_blocking` — el runtime jamás se bloquea (regla 2),
/// mismo patrón que `persist_ui_theme` en `main.rs`.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea, `hotlist` existe
/// pero no es un array de tablas, o falla el I/O.
pub fn persist_hotlist_add(dir: &Path, name: &str, wire_path: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let arr = doc
        .as_table_mut()
        .entry("hotlist")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "`hotlist` no es un array de tablas"))?;
    // add REEMPLAZA si el name ya existe (spec: "add reemplaza si name
    // existe") — mismo comportamiento que renombrar/actualizar el favorito
    // sin dejar una entrada vieja huérfana.
    if let Some(existing) = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
    {
        existing["path"] = toml_edit::value(wire_path);
    } else {
        let mut t = toml_edit::Table::new();
        t["name"] = toml_edit::value(name);
        t["path"] = toml_edit::value(wire_path);
        arr.push(t);
    }
    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}

/// Retira la entrada `[[hotlist]]` de nombre `name` del `norte.toml` de
/// `dir`, PRESERVANDO comentarios y formato. `name` inexistente (o
/// `norte.toml`/`hotlist` inexistentes) es NO-OP documentado: no hay nada
/// que borrar, no es un error — y crucialmente NO reescribe el fichero
/// (review MINOR-1: escribir sin cambios toca el mtime → el watcher de
/// `config::watch` lo confunde con una edición real y dispara un
/// hot-reload fantasma).
///
/// BLOQUEANTE: hace I/O de FS síncrono. El caller (T5) DEBE envolverla en
/// `tokio::task::spawn_blocking` — el runtime jamás se bloquea (regla 2),
/// mismo patrón que `persist_ui_theme` en `main.rs`.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O.
pub fn persist_hotlist_remove(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?,
        // No-op documentado: nada que borrar.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(path),
        Err(e) => return Err(e),
    };
    // Solo se escribe si `retain` REALMENTE quitó algo — comparar
    // longitudes antes/después en vez de escribir incondicionalmente.
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("hotlist")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
    {
        let before = arr.len();
        arr.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        if arr.len() != before {
            std::fs::write(&path, doc.to_string())?;
        }
    }
    Ok(path)
}

/// Una entrada de hotlist YA fusionada y validada por [`load`]. `target` es
/// `Err` si el `path` de `norte.toml` no parsea como [`VPath`] — la entrada
/// se CONSERVA (se muestra con badge de error en el popup, T5) en vez de
/// tumbar la carga entera: la hotlist es data del usuario, no config
/// estructural (spec 2026-07-18, decisión 3). La clave de error es
/// ESTABLE (`"err-invalid-path"`, no el mensaje crudo del parser de
/// `VPath`): el popup la traduce vía Fluent y un path hostil (bidi,
/// kilométrico) jamás llega intacto a la barra (misma cautela que #73).
#[derive(Debug, Clone)]
pub struct HotlistItem {
    /// Nombre mostrado.
    pub name: String,
    /// Destino ya parseado, o la clave de error estable.
    pub target: Result<VPath, String>,
}

/// Clave de error ESTABLE para un `path` de hotlist que no parsea como
/// [`VPath`] (ver doc de [`HotlistItem`]).
const ERR_INVALID_PATH: &str = "err-invalid-path";

/// Valida `entry.path` a [`VPath`] y lo fusiona en `items`: si ya hay una
/// entrada con el mismo `name`, la reemplaza — sea de una capa ANTERIOR
/// (la capa posterior gana, igual que el resto de la config), sea de un
/// `[[hotlist]]` PREVIO dentro de la MISMA capa (TOML no impide repetir
/// `name` en un array de tablas; `load` llama a esta función una vez por
/// entrada, en orden de aparición, así que la ÚLTIMA gana también
/// intra-capa). Conserva la posición original para que el orden del popup
/// no salte al editar solo el `path` de un favorito ya existente. Si no
/// existía, se añade al final. Las claves (`name`) comparan byte-exactas
/// SIN normalizar (la identidad jamás se normaliza); twins NFC/NFD conviven
/// como filas distintas — decisión consciente.
fn merge_hotlist_entry(items: &mut Vec<HotlistItem>, entry: HotlistEntry) {
    let target = VPath::parse(&entry.path).map_err(|_| ERR_INVALID_PATH.to_owned());
    if let Some(existing) = items.iter_mut().find(|it| it.name == entry.name) {
        existing.target = target;
    } else {
        items.push(HotlistItem {
            name: entry.name,
            target,
        });
    }
}

/// La config ya fusionada.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// Preset de keymap efectivo (último-gana; default compilado).
    pub preset: String,
    /// Idioma de `[ui] lang` (último-gana; None = entorno).
    pub ui_lang: Option<String>,
    /// Tema de `[ui] theme` (último-gana; None = preset default).
    pub ui_theme: Option<String>,
    /// `[daemon] mode` (último-gana; None = embedded). Solo arranque.
    pub daemon_mode: Option<DaemonMode>,
    /// `[daemon] socket` (último-gana; None = default del OS).
    pub daemon_socket: Option<PathBuf>,
    /// Capas de `keymap.toml` presentes, en precedencia ascendente.
    pub keymap_layers: Vec<KeymapFile>,
    /// Modo del quick search (`[ui] quick_search`; default `Filter`).
    pub quick_search_mode: nav::Mode,
    /// Hotlist fusionada de TODAS las capas menos la de proyecto (ver
    /// [`load`]). Nombre duplicado entre capas: la posterior gana.
    pub hotlist: Vec<HotlistItem>,
    /// `[archive] max_entries` (último-gana; None = default compilado).
    pub archive_max_entries: Option<u64>,
    /// `[archive] max_decompressed_bytes` (último-gana; None = default).
    pub archive_max_decompressed_bytes: Option<u64>,
    /// Archivos que participaron (para el watcher y los diagnósticos).
    pub sources: Vec<PathBuf>,
}

/// Carga y fusiona todas las capas (ADR 0007).
///
/// # Errors
/// [`ConfigError`] con el archivo culpable; una capa AUSENTE no es error.
pub fn load(layers: &Layers) -> Result<LoadedConfig, ConfigError> {
    let mut preset: Option<String> = None;
    let mut ui_lang: Option<String> = None;
    let mut ui_theme: Option<String> = None;
    let mut daemon_mode: Option<DaemonMode> = None;
    let mut daemon_socket: Option<PathBuf> = None;
    let mut keymap_layers = Vec::new();
    let mut quick_search_mode = nav::Mode::default();
    let mut hotlist: Vec<HotlistItem> = Vec::new();
    let mut archive_max_entries: Option<u64> = None;
    let mut archive_max_decompressed_bytes: Option<u64> = None;
    let mut sources = Vec::new();
    for (dir, kind) in &layers.dirs {
        let norte = dir.join("norte.toml");
        if let Some(raw) = read_optional(&norte)? {
            let parsed: NorteToml = toml::from_str(&raw).map_err(|e| ConfigError::Toml {
                path: norte.clone(),
                message: toml_diag(&raw, &e),
            })?;
            if let Some(p) = parsed.keymap.preset {
                preset = Some(p);
            }
            if let Some(l) = parsed.ui.lang {
                ui_lang = Some(l);
            }
            if let Some(th) = parsed.ui.theme {
                ui_theme = Some(th);
            }
            if let Some(qs) = parsed.ui.quick_search {
                quick_search_mode = match qs.as_str() {
                    "filter" => nav::Mode::Filter,
                    "jump" => nav::Mode::Jump,
                    // Mensaje SIN citar el valor crudo (misma cautela que
                    // `toml_diag`, #73): un TOML hostil puede meter
                    // bidi/kilométrico en cualquier string, y este es un
                    // campo de dos valores válidos — no hace falta
                    // reflejar el resto para que el diagnóstico sea claro.
                    _ => {
                        return Err(ConfigError::Toml {
                            path: norte,
                            message: "[ui] quick_search inválido: solo se admite «filter» o «jump»"
                                .to_owned(),
                        });
                    }
                };
            }
            if let Some(m) = parsed.daemon.mode {
                daemon_mode = Some(m);
            }
            if let Some(sock) = parsed.daemon.socket {
                daemon_socket = Some(sock);
            }
            // La hotlist se acumula de TODAS las capas MENOS la de
            // proyecto (deuda #75 cerrada: el kind viaja POR DIR, ya no se
            // infiere por posición). Un `./.norte/norte.toml` de un repo
            // ajeno no debe poder inyectar favoritos en la sesión del
            // usuario (spec 2026-07-18, decisión 3). Los ESCALARES de UI
            // (quick_search, theme, lang) SÍ se honran desde proyecto: son
            // config estructural de presentación (coherente con theme), no
            // data que dirija navegación como la hotlist.
            if *kind != Layer::Project {
                for entry in parsed.hotlist {
                    merge_hotlist_entry(&mut hotlist, entry);
                }
            }
            // `[archive]` (#95.2) TAMPOCO se honra desde proyecto: son
            // límites de SEGURIDAD anti-bomba — un `./.norte/norte.toml` de
            // un repo ajeno no debe poder SUBIRLOS y desarmar la protección
            // justo donde viven los contenedores hostiles (mismo criterio
            // fail-closed que la hotlist).
            if *kind != Layer::Project {
                if let Some(n) = parsed.archive.max_entries {
                    archive_max_entries = Some(n);
                }
                if let Some(b) = parsed.archive.max_decompressed_bytes {
                    archive_max_decompressed_bytes = Some(b);
                }
            }
            sources.push(norte);
        }
        let keymap = dir.join("keymap.toml");
        if let Some(raw) = read_optional(&keymap)? {
            let mut parsed = parse_keymap(&raw).map_err(|e| ConfigError::Toml {
                path: keymap.clone(),
                message: e.to_string(),
            })?;
            // La capa de PROYECTO (`./.norte`) carga SIN trust, así que se
            // marca (deuda #75 cerrada: el kind viaja POR DIR):
            // `Effective::build_for` descarta sus bindings `lua:` (un repo
            // hostil no dirige la ejecución de comandos Lua del usuario) —
            // con aviso, jamás en silencio.
            if *kind == Layer::Project {
                parsed.mark_project();
            }
            // Diagnóstico con ARCHIVO (ADR 0007): una capa de usuario no
            // admite `keymap` — eso es de presets (prepend/append aquí).
            if parsed.has_full_keymap() {
                return Err(ConfigError::Toml {
                    path: keymap,
                    message: "una capa de config no admite `keymap`: usa                               prepend_keymap/append_keymap (ADR 0006)"
                        .to_owned(),
                });
            }
            keymap_layers.push(parsed);
            sources.push(keymap);
        }
    }
    Ok(LoadedConfig {
        preset: preset.unwrap_or_else(|| DEFAULT_PRESET.to_owned()),
        ui_lang,
        ui_theme,
        daemon_mode,
        daemon_socket,
        keymap_layers,
        quick_search_mode,
        hotlist,
        archive_max_entries,
        archive_max_decompressed_bytes,
        sources,
    })
}

/// Como [`load`], para contexto async (hot-reload): corre en
/// `spawn_blocking` — el runtime jamás se bloquea con el FS.
///
/// # Errors
/// Los de [`load`].
pub async fn load_async(layers: Layers) -> Result<LoadedConfig, ConfigError> {
    match tokio::task::spawn_blocking(move || load(&layers)).await {
        Ok(res) => res,
        // Un panic en load() es un bug NUESTRO: jamás enterrarlo como un
        // ConfigError con path falso (regla 6) — que reviente visible.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Lee un archivo si existe; `None` si no está (una capa ausente no es
/// error), `Err` si existe pero no se puede leer.
fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

/// Modo de vigilancia logrado (para el aviso al usuario, ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Watcher nativo (inotify/FSEvents/ReadDirectoryChangesW).
    Notify,
    /// Degradado a sondeo de mtimes cada 2 s (con aviso, jamás fallar).
    Polling,
}

/// Vigilancia viva de los dirs de config: soltar este valor la DETIENE en
/// ambos modos (el watcher nativo se cierra; el task de polling se cancela
/// vía token — regla 3).
pub struct Watch {
    /// Cómo se está vigilando.
    pub mode: WatchMode,
    _watcher: Option<notify::RecommendedWatcher>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Vigila los directorios de capas y envía `()` por `tx` en cada cambio
/// (sin debounce: eso es del consumidor). Watcher nativo si arranca — y
/// SIEMPRE un poll lento de respaldo (capas cuyo dir aún no existe, colas
/// de inotify desbordadas); si el nativo no arranca en absoluto, el poll
/// pasa a rápido y `mode` lo delata para el aviso (trampa documentada:
/// degradar, jamás fallar). El setup (stats + inotify) corre en
/// `spawn_blocking` — llamable desde async (regla 2).
pub async fn watch(layers: &Layers, tx: tokio::sync::mpsc::Sender<()>) -> Watch {
    use notify::Watcher;
    let layers2 = layers.clone();
    let tx2 = tx.clone();
    let watcher = tokio::task::spawn_blocking(move || {
        let mut watcher = notify::recommended_watcher({
            move |res: Result<notify::Event, notify::Error>| {
                // También en Err (M3 de la revisión): un error de notify
                // significa "puedes haber perdido eventos" — releer TODO es
                // exactamente la respuesta correcta. try_send: los cambios
                // se coalescen; perder uno con el canal lleno es inocuo.
                let _ = res;
                let _ = tx2.try_send(());
            }
        })
        .ok()?;
        let mut watching = false;
        for (dir, _kind) in &layers2.dirs {
            if dir.is_dir()
                && watcher
                    .watch(dir, notify::RecursiveMode::NonRecursive)
                    .is_ok()
            {
                watching = true;
            }
        }
        watching.then_some(watcher)
    })
    .await
    .ok()
    .flatten();

    let mode = if watcher.is_some() {
        WatchMode::Notify
    } else {
        WatchMode::Polling
    };
    // Poll de respaldo: rápido si es el ÚNICO mecanismo; lento como red de
    // seguridad del nativo (dirs creados en caliente, eventos perdidos).
    let period = match mode {
        WatchMode::Polling => std::time::Duration::from_secs(2),
        WatchMode::Notify => std::time::Duration::from_secs(10),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode,
        _watcher: watcher,
        cancel,
    }
}

/// Vigilancia por POLLING puro con periodo propio (el mecanismo del
/// fallback de [`watch`], expuesto para poder testear su cancelación).
///
/// # Panics
/// Si se llama fuera de un runtime tokio (hace `tokio::spawn`).
#[doc(hidden)]
#[must_use]
pub fn watch_polling(
    layers: &Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
) -> Watch {
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode: WatchMode::Polling,
        _watcher: None,
        cancel,
    }
}

/// Task de polling de mtimes+tamaños, cancelable (regla 3).
fn spawn_poll(
    layers: Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let mut last: Option<Vec<(PathBuf, std::time::SystemTime, u64)>> = None;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(period) => {}
            }
            let layers2 = layers.clone();
            let Ok(snapshot) = tokio::task::spawn_blocking(move || snapshot(&layers2)).await else {
                return;
            };
            if let Some(prev) = &last
                && *prev != snapshot
                && tx.send(()).await.is_err()
            {
                return;
            }
            last = Some(snapshot);
        }
    });
}

/// Snapshot de (mtime, tamaño) de los archivos de config presentes — el
/// tamaño caza escrituras dentro de la granularidad del mtime del FS.
fn snapshot(layers: &Layers) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let mut out = Vec::new();
    for (dir, _kind) in &layers.dirs {
        for name in ["norte.toml", "keymap.toml"] {
            let p = dir.join(name);
            if let Ok(md) = std::fs::metadata(&p)
                && let Ok(m) = md.modified()
            {
                let len = md.len();
                out.push((p, m, len));
            }
        }
    }
    out
}

#[cfg(test)]
mod toml_diag_tests {
    use super::*;

    /// #73: el diagnóstico compacto conserva posición + mensaje semántico y
    /// NO cita la línea del fichero — un TOML hostil puede meter valores
    /// kilométricos/bidi que desplazarían lo accionable fuera del tope de la
    /// barra (hallazgo MEDIA-1 del encoding-auditor).
    #[test]
    fn toml_diag_compacto_sin_citar_el_contenido() {
        let hostil = format!("v = \"{}\u{202E}\"\nbad", "x".repeat(300));
        let e = toml::from_str::<NorteToml>(&hostil).expect_err("no parsea");
        let d = toml_diag(&hostil, &e);
        assert!(!d.contains("xxx"), "no cita el contenido: {d}");
        assert!(!d.contains('\u{202E}'), "sin bidi: {d}");
        assert!(d.len() < 200, "compacto ({} bytes): {d}", d.len());
        assert!(d.contains("line "), "la posición sobrevive: {d}");
    }

    /// El span puede faltar (errores semánticos sin posición): mensaje solo.
    #[test]
    fn toml_diag_sin_span_no_panica() {
        let e = toml::from_str::<NorteToml>("keymap = 3").expect_err("no valida");
        let _ = toml_diag("keymap = 3", &e);
    }
}

/// Tests de historial+hotlist (spec 2026-07-18, navTC T2): mod nuevo junto
/// a `toml_diag_tests` (no reutilizarlo — ese mod es solo del diagnóstico
/// compacto de `#73`).
#[cfg(test)]
mod hotlist_tests {
    use super::*;

    #[test]
    fn hotlist_round_trip_preservando_comentarios() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# mi config\n[ui]\ntheme = \"nord\" # tema\n",
        )
        .unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///home/o/work").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# mi config"), "comentarios intactos: {s}");
        assert!(s.contains("[[hotlist]]"), "{s}");
        persist_hotlist_remove(dir.path(), "trabajo").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(!s.contains("trabajo"), "{s}");
    }

    #[test]
    fn hotlist_add_reemplaza_si_el_nombre_ya_existe() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///a").unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///b").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s.matches("trabajo").count(),
            1,
            "una sola entrada, no duplicada: {s}"
        );
        assert!(s.contains("file:///b"), "{s}");
        assert!(!s.contains("file:///a"), "{s}");
    }

    #[test]
    fn hotlist_remove_de_nombre_inexistente_es_no_op() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///a").unwrap();
        let path = dir.path().join("norte.toml");
        // Comentario a mano: si el no-op reescribiera el fichero, toml_edit
        // podría reformatearlo igual — la prueba fuerte no es "no falla",
        // es "el CONTENIDO no cambia ni un byte" (review MINOR-1: mtime es
        // flaky por granularidad del FS, el contenido no).
        let mut s = std::fs::read_to_string(&path).unwrap();
        s.push_str("# nota manual\n");
        std::fs::write(&path, &s).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        // No debe fallar aunque "fantasma" no exista (documentado: no-op).
        persist_hotlist_remove(dir.path(), "fantasma").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "no-op no reescribe: contenido byte-idéntico");
        assert!(after.contains("trabajo"), "{after}");
    }

    #[test]
    fn hotlist_remove_sin_seccion_hotlist_es_no_op_y_no_reescribe() {
        // `norte.toml` existe pero SIN `[[hotlist]]` en absoluto: el no-op
        // tampoco debe tocar el fichero (mismo MINOR-1).
        let dir = tempfile::tempdir().unwrap();
        let content = "# sin hotlist\n[ui]\ntheme = \"nord\"\n";
        std::fs::write(dir.path().join("norte.toml"), content).unwrap();
        persist_hotlist_remove(dir.path(), "lo-que-sea").unwrap();
        let after = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(content, after, "sin `hotlist`: contenido intacto");
    }

    #[test]
    fn hotlist_se_carga_de_todas_las_capas_menos_proyecto() {
        // Dos dirs: capa `User` con una entrada, capa `Project` con otra —
        // la de proyecto NO debe entrar (spec: "un repo ajeno no inyecta
        // favoritos"), y la de usuario sí.
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"casa\"\npath = \"file:///home/o\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[[hotlist]]\nname = \"repo-ajeno\"\npath = \"file:///tmp/x\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.hotlist.len(),
            1,
            "solo la de usuario: {:?}",
            cfg.hotlist
        );
        assert_eq!(cfg.hotlist[0].name, "casa");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///home/o").unwrap()
        );
    }

    #[test]
    fn hotlist_entrada_invalida_degrada_por_entrada() {
        // Un path que no parsea como VPath (falta scheme) no tumba la
        // carga: la entrada sobrevive con `target = Err(...)`, y las demás
        // entradas de la misma capa se cargan con normalidad.
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"rota\"\npath = \"no-es-un-path-wire\"\n\n\
             [[hotlist]]\nname = \"sana\"\npath = \"file:///ok\"\n",
        )
        .unwrap();
        // La entrada va en una capa `User` (que SÍ aporta hotlist); la capa
        // `Project` (un repo ajeno) se añade vacía para comprobar que su
        // ausencia de favoritos no altera el resultado.
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("la carga NO falla por una entrada rota");
        assert_eq!(cfg.hotlist.len(), 2);
        let rota = cfg.hotlist.iter().find(|h| h.name == "rota").unwrap();
        assert_eq!(
            rota.target.as_ref().err().map(String::as_str),
            Some(ERR_INVALID_PATH)
        );
        let sana = cfg.hotlist.iter().find(|h| h.name == "sana").unwrap();
        assert!(sana.target.is_ok());
    }

    #[test]
    fn hotlist_nombre_duplicado_entre_capas_la_capa_posterior_gana() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///viejo\"\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///nuevo\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.hotlist.len(), 1, "mismo nombre, una entrada");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///nuevo").unwrap(),
            "la capa posterior (usuario) gana sobre sistema"
        );
    }

    /// review MINOR-2: el dedup por `name` también aplica DENTRO de la
    /// MISMA capa — TOML no impide repetir `[[hotlist]] name = "..."` dos
    /// veces en el mismo array; la última aparición gana (ver rustdoc de
    /// `merge_hotlist_entry`).
    #[test]
    fn hotlist_nombre_duplicado_dentro_de_la_misma_capa_la_ultima_aparicion_gana() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///viejo\"\n\n\
             [[hotlist]]\nname = \"trabajo\"\npath = \"file:///nuevo\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.hotlist.len(), 1, "mismo nombre intra-capa, una entrada");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///nuevo").unwrap(),
            "la última aparición dentro de la capa gana"
        );
    }

    /// Pin encoding BAJA-1a: un `name` hostil (comilla, salto de línea, un
    /// `[[hotlist]]` embebido y un override bidi) sobrevive el round-trip
    /// add → load BYTE-IDÉNTICO como UNA sola entrada — `toml_edit` escapa,
    /// jamás inyecta TOML — y esa misma clave la retira con remove.
    #[test]
    fn hotlist_round_trip_name_hostil_byte_identico() {
        let usuario = tempfile::tempdir().unwrap();
        let name = "fa\"vo\n[[hotlist]]\u{202E}rito";
        persist_hotlist_add(usuario.path(), name, "file:///x").unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("el name hostil no rompe el TOML");
        assert_eq!(cfg.hotlist.len(), 1, "UNA entrada, sin inyección");
        assert_eq!(cfg.hotlist[0].name, name, "name byte-idéntico");
        persist_hotlist_remove(usuario.path(), name).unwrap();
        let cfg = load(&layers).expect("carga tras remove");
        assert!(cfg.hotlist.is_empty(), "la clave hostil retira su entrada");
    }

    /// Pin encoding BAJA-1b: el wire de un `VPath` con segmento no-UTF8
    /// (0xFF 0xFE) round-tripea add → load con `target` Ok y bytes exactos.
    #[test]
    fn hotlist_round_trip_path_no_utf8_bytes_exactos() {
        let usuario = tempfile::tempdir().unwrap();
        let vp = VPath::parse("file:///%FF%FE").unwrap();
        persist_hotlist_add(usuario.path(), "bin", &vp.to_wire()).unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        let target = cfg.hotlist[0].target.as_ref().expect("target Ok");
        assert_eq!(target, &vp);
        assert_eq!(
            target.file_name().unwrap().as_bytes(),
            &[0xFF, 0xFE],
            "los bytes crudos sobreviven el round-trip por TOML"
        );
    }

    #[test]
    fn quick_search_valores_validos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.quick_search_mode, crate::nav::Mode::Jump);
    }

    #[test]
    fn quick_search_default_es_filter() {
        let cfg = load(&Layers { dirs: vec![] }).expect("carga");
        assert_eq!(cfg.quick_search_mode, crate::nav::Mode::Filter);
    }

    #[test]
    fn quick_search_valor_invalido_es_error_de_carga() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"vuela\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("config rota es error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }
}
