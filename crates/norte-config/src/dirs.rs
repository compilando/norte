//! Config-directory resolution (ADR 0035). The single resolver every
//! norte process uses.

use std::path::PathBuf;

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

/// Directorio de config del usuario para `connections.toml` / `known_hosts` /
/// `secrets.age`: `$NORTE_CONFIG_DIR` (override explícito) →
/// `$XDG_CONFIG_HOME/norte` → `~/.config/norte` (unix) / `%APPDATA%\norte`
/// (Windows). Misma capa de usuario que el resto de la config (ADR 0007).
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NORTE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME")
        && !d.is_empty()
    {
        return PathBuf::from(d).join("norte");
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("APPDATA") {
        return PathBuf::from(d).join("norte");
    }
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("norte")
}
