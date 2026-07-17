//! Config en capas (ADR 0007): defaults → sistema → usuario → proyecto →
//! flags. Escalares: último-gana por campo; `keymap.toml`: las capas se
//! ACUMULAN y se pliegan en el keymap efectivo (ADR 0006). Config rota =
//! error con archivo y campo; en hot-reload, se conserva lo vigente.
//!
//! Exención deliberada de la regla 2: la config del PROPIO frontend se lee
//! con `std::fs` (leerla vía providers sería circular — la config decide
//! cómo arranca el TUI). Desde contexto async, usar [`load_async`].

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::keymap::{KeymapFile, parse_keymap};

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
    let mut sources = Vec::new();
    for dir in &layers.dirs {
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
            if let Some(m) = parsed.daemon.mode {
                daemon_mode = Some(m);
            }
            if let Some(sock) = parsed.daemon.socket {
                daemon_socket = Some(sock);
            }
            sources.push(norte);
        }
        let keymap = dir.join("keymap.toml");
        if let Some(raw) = read_optional(&keymap)? {
            let parsed = parse_keymap(&raw).map_err(|e| ConfigError::Toml {
                path: keymap.clone(),
                message: e.to_string(),
            })?;
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
