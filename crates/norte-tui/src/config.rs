//! Config en capas (ADR 0007): defaults → sistema → usuario → proyecto →
//! flags. Escalares: último-gana por campo; `keymap.toml`: las capas se
//! ACUMULAN y se pliegan en el keymap efectivo (ADR 0006). Config rota =
//! error con archivo y campo; en hot-reload, se conserva lo vigente.
//!
//! Exención deliberada de la regla 2: la config del PROPIO frontend se lee
//! con `std::fs` (leerla vía providers sería circular — la config decide
//! cómo arranca el TUI). Desde contexto async, usar [`load_async`].

use std::path::{Path, PathBuf};

use norte_proto::VPath;
use serde::Deserialize;

use crate::keymap::{KeymapFile, parse_keymap};
use crate::nav;

/// Preset por defecto (decisión 2026-07-10).
pub const DEFAULT_PRESET: &str = "orthodox";

/// `norte.toml`: la config general.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NorteToml {
    /// Sección de keymap.
    #[serde(default)]
    pub keymap: KeymapSection,
    /// Sección de UI.
    #[serde(default)]
    pub ui: UiSection,
    /// Sección del daemon (fase 3 M2).
    #[serde(default)]
    pub daemon: DaemonSection,
    /// Hotlist de directorios favoritos (`Ctrl+D`, spec 2026-07-18). Se
    /// ACUMULA entre capas (no último-gana como los escalares) salvo la
    /// capa proyecto, que queda excluida al fusionar en [`load`] — ver el
    /// comentario ahí. Ausente = sin favoritos en esta capa.
    #[serde(default)]
    pub hotlist: Vec<HotlistEntry>,
}

/// Una entrada de `[[hotlist]]` en `norte.toml`, tal cual en disco. `path`
/// es la forma wire sin validar (`scheme://…`; remotos válidos) — se
/// valida a [`VPath`] al fusionar capas en [`load`], NUNCA aquí: una
/// entrada rota se degrada por entrada (ver [`HotlistItem`]), no debe
/// tumbar el parseo de todo `norte.toml`.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HotlistEntry {
    /// Nombre mostrado en el popup (`Ctrl+D`).
    pub name: String,
    /// Path en forma wire, sin validar todavía.
    pub path: String,
}

/// `[daemon]` de `norte.toml` (ADR 0011). El modo se decide EN EL
/// ARRANQUE: no participa del hot-reload (cambiar de transporte en
/// caliente = reiniciar).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DaemonSection {
    /// `embedded` (default: arranque instantáneo) o `daemon`.
    #[serde(default)]
    pub mode: Option<DaemonMode>,
    /// Socket del daemon; ausente = el default del OS.
    #[serde(default)]
    pub socket: Option<PathBuf>,
}

/// Transporte del core (regla 7: solo cambia el transporte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    /// Core in-process (default).
    Embedded,
    /// Contra el daemon UDS (solo unix, ADR 0011).
    Daemon,
}

/// `[ui]` de `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// Idioma (`es`, `en`). Ausente = negociar del entorno.
    #[serde(default)]
    pub lang: Option<String>,
    /// Tema: nombre de preset embebido (`default`, `catppuccin-mocha`,
    /// `gruvbox-dark`, `nord`, y los claros `gruvbox-light`,
    /// `catppuccin-latte`) o ruta a un `.toml` propio (ADR 0020). Ausente =
    /// preset `default`.
    #[serde(default)]
    pub theme: Option<String>,
    /// Modo del quick search (`/`, spec 2026-07-18): `"filter"` (default,
    /// el listado se reduce) o `"jump"` (el cursor salta, el listado no
    /// cambia). Cualquier otro valor es config rota (ADR 0007: error
    /// claro, jamás degradación silenciosa) — validado en [`load`], no
    /// aquí, porque el diagnóstico necesita el `path` del archivo culpable.
    #[serde(default)]
    pub quick_search: Option<String>,
}

/// `[keymap]` de `norte.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Preset base (`orthodox`, `vim`, `cua`). Ausente = capa anterior.
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

/// Los directorios de capas, en precedencia ASCENDENTE.
#[derive(Debug, Clone)]
pub struct Layers {
    /// sistema → usuario → proyecto (los ausentes simplemente no aportan).
    pub dirs: Vec<PathBuf>,
}

/// Capas estándar (ADR 0007): `/etc/norte` (`%ProgramData%\norte`),
/// `$XDG_CONFIG_HOME/norte` (`~/.config/norte`; `%APPDATA%\norte`) y
/// `./.norte`.
#[must_use]
pub fn standard_layers() -> Layers {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(pd) = std::env::var_os("ProgramData") {
            dirs.push(PathBuf::from(pd).join("norte"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("norte"));
        }
    } else {
        dirs.push(PathBuf::from("/etc/norte"));
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            dirs.push(PathBuf::from(xdg).join("norte"));
        } else if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".config/norte"));
        }
    }
    dirs.push(PathBuf::from(".norte"));
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
/// que borrar, no es un error.
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
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("hotlist")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
    {
        arr.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
    }
    std::fs::write(&path, doc.to_string())?;
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
/// entrada con el mismo `name` (de una capa ANTERIOR), la reemplaza — la
/// capa posterior gana, igual que el resto de la config (última-gana),
/// pero conservando la posición original para que el orden del popup no
/// salte al editar solo el `path` de un favorito ya existente. Si no
/// existía, se añade al final.
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
    let mut sources = Vec::new();
    for (i, dir) in layers.dirs.iter().enumerate() {
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
            // proyecto (última posicional, deuda #75: `Layers` debería
            // llevar el kind por dir en vez de inferirlo por posición,
            // igual que el `mark_project()` del keymap más abajo). Un
            // `./.norte/norte.toml` de un repo ajeno no debe poder
            // inyectar favoritos en la sesión del usuario (spec
            // 2026-07-18, decisión 3).
            if i + 1 != layers.dirs.len() {
                for entry in parsed.hotlist {
                    merge_hotlist_entry(&mut hotlist, entry);
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
            // La ÚLTIMA capa es la de PROYECTO (`./.norte`, misma convención
            // posicional que las capas Lua de main.rs; deuda #75: `Layers`
            // debería llevar el kind por dir). Su keymap carga SIN trust,
            // así que se marca: `Effective::build_for` descarta sus
            // bindings `lua:` (un repo hostil no dirige la ejecución de
            // comandos Lua del usuario) — con aviso, jamás en silencio.
            if i + 1 == layers.dirs.len() {
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
        for dir in &layers2.dirs {
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
    for dir in &layers.dirs {
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
        // No debe fallar aunque "fantasma" no exista (documentado: no-op).
        persist_hotlist_remove(dir.path(), "fantasma").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("trabajo"), "{s}");
    }

    #[test]
    fn hotlist_se_carga_de_todas_las_capas_menos_proyecto() {
        // Dos dirs: "usuario" con una entrada, "proyecto" (ÚLTIMA capa) con
        // otra — la de proyecto NO debe entrar (spec: "un repo ajeno no
        // inyecta favoritos"), y la de usuario sí.
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
            dirs: vec![usuario.path().to_path_buf(), proyecto.path().to_path_buf()],
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
        // Layers de un solo dir = ese dir es la capa proyecto (posicional,
        // deuda #75) — para que la entrada "de usuario" cuente aquí, hace
        // falta una capa DESPUÉS de ella; se añade una capa proyecto vacía.
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![usuario.path().to_path_buf(), proyecto.path().to_path_buf()],
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
                sistema.path().to_path_buf(),
                usuario.path().to_path_buf(),
                proyecto.path().to_path_buf(),
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

    #[test]
    fn quick_search_valores_validos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![dir.path().to_path_buf()],
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
            dirs: vec![dir.path().to_path_buf()],
        };
        let err = load(&layers).expect_err("config rota es error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }
}
