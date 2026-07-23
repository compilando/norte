//! Config-directory resolution (ADR 0035). The single resolver every
//! norte process uses.

use std::ffi::OsString;
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

/// Test seam behind [`user_config_dir_from`]: same resolution, but with the
/// target platform selected explicitly instead of baked in via
/// `cfg!(windows)`. This lets the Windows branch be pinned by a test suite
/// that only ever runs on Linux CI. Not general API — call
/// [`user_config_dir_from`] instead.
#[doc(hidden)]
#[must_use]
pub fn user_config_dir_on(
    windows: bool,
    get: &impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if let Some(d) = get("NORTE_CONFIG_DIR")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = get("XDG_CONFIG_HOME")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d).join("norte"));
    }
    if windows && let Some(d) = get("APPDATA") {
        return Some(PathBuf::from(d).join("norte"));
    }
    get("HOME").map(|h| PathBuf::from(h).join(".config").join("norte"))
}

/// The user config dir, resolved from an injectable environment (tests pass
/// a closure; production wrappers pass [`std::env::var_os`]). Precedence
/// (ADR 0035): `NORTE_CONFIG_DIR` (non-empty) → `XDG_CONFIG_HOME/norte`
/// (non-empty) → `%APPDATA%\norte` (Windows) → `$HOME/.config/norte`. An
/// empty `NORTE_CONFIG_DIR` counts as unset, same as an empty
/// `XDG_CONFIG_HOME`.
///
/// # Example
///
/// ```
/// use norte_config::user_config_dir_from;
/// use std::ffi::OsString;
/// use std::path::PathBuf;
///
/// let get = |k: &str| -> Option<OsString> {
///     match k {
///         "NORTE_CONFIG_DIR" => Some(OsString::from("/custom")),
///         _ => None,
///     }
/// };
/// assert_eq!(user_config_dir_from(&get), Some(PathBuf::from("/custom")));
/// ```
#[must_use]
pub fn user_config_dir_from(get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    user_config_dir_on(cfg!(windows), get)
}

/// The user config dir from the process environment; `None` when the
/// environment defines nothing (CI without HOME): the caller warns.
#[must_use]
pub fn user_config_dir() -> Option<PathBuf> {
    user_config_dir_from(&|k| std::env::var_os(k))
}

/// Infallible variant for core paths (`connections.toml`, `journal.db`, …):
/// falls back to `./.config/norte`.
#[must_use]
pub fn config_dir() -> PathBuf {
    user_config_dir().unwrap_or_else(|| PathBuf::from(".").join(".config").join("norte"))
}

/// Test seam behind [`standard_layers_from`]: same layering, but with the
/// target platform selected explicitly. Not general API — call
/// [`standard_layers_from`] instead.
#[doc(hidden)]
#[must_use]
pub fn standard_layers_on(windows: bool, get: &impl Fn(&str) -> Option<OsString>) -> Layers {
    let mut dirs = Vec::new();
    if let Some(over) = get("NORTE_CONFIG_DIR")
        && !over.is_empty()
    {
        dirs.push((PathBuf::from(over), Layer::User));
        dirs.push((PathBuf::from(".norte"), Layer::Project));
        return Layers { dirs };
    }
    if windows {
        if let Some(pd) = get("ProgramData") {
            dirs.push((PathBuf::from(pd).join("norte"), Layer::System));
        }
    } else {
        dirs.push((PathBuf::from("/etc/norte"), Layer::System));
    }
    if let Some(user) = user_config_dir_on(windows, get) {
        dirs.push((user, Layer::User));
    }
    dirs.push((PathBuf::from(".norte"), Layer::Project));
    Layers { dirs }
}

/// Standard layers (ADR 0007/0035) from an injectable environment.
/// A non-empty `NORTE_CONFIG_DIR` makes this hermetic: only that dir (User)
/// + `./.norte`, no system layer.
#[must_use]
pub fn standard_layers_from(get: &impl Fn(&str) -> Option<OsString>) -> Layers {
    standard_layers_on(cfg!(windows), get)
}

/// Standard layers from the process environment.
#[must_use]
pub fn standard_layers() -> Layers {
    standard_layers_from(&|k| std::env::var_os(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env<'a>(v: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            v.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, x)| OsString::from(x))
        }
    }

    #[test]
    fn norte_config_dir_wins_over_everything() {
        let d = user_config_dir_from(&env(&[
            ("NORTE_CONFIG_DIR", "/custom"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(d, Some(PathBuf::from("/custom")));
    }

    #[test]
    fn xdg_empty_falls_through_to_home() {
        let d = user_config_dir_from(&env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")]));
        assert_eq!(d, Some(PathBuf::from("/home/u/.config/norte")));
    }

    #[test]
    fn xdg_beats_home() {
        let d = user_config_dir_from(&env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]));
        assert_eq!(d, Some(PathBuf::from("/xdg/norte")));
    }

    #[test]
    fn sin_entorno_es_none() {
        assert_eq!(user_config_dir_from(&env(&[])), None);
    }

    /// ADR 0035 decision 2: an explicit override is HERMETIC — no system
    /// layer, only (override, User) + (./.norte, Project).
    #[test]
    fn standard_layers_con_override_es_hermetico() {
        let l = standard_layers_from(&env(&[
            ("NORTE_CONFIG_DIR", "/custom"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/custom"), Layer::User),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn standard_layers_sin_override_incluye_sistema() {
        let l = standard_layers_from(&env(&[("HOME", "/home/u")]));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/etc/norte"), Layer::System),
                (PathBuf::from("/home/u/.config/norte"), Layer::User),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }

    /// MAJOR-1 fix: an empty `NORTE_CONFIG_DIR` (e.g. inherited unset-but-
    /// exported from a parent shell) must count as unset, not as an override
    /// pointing at the empty path — otherwise `config_dir()` would resolve to
    /// `""` and hermetic layering would read the process cwd.
    #[test]
    fn norte_config_dir_vacio_cuenta_como_no_definido() {
        let e = env(&[("NORTE_CONFIG_DIR", ""), ("HOME", "/home/u")]);
        assert_eq!(
            user_config_dir_from(&e),
            Some(PathBuf::from("/home/u/.config/norte"))
        );
        #[cfg(unix)]
        {
            let l = standard_layers_from(&e);
            assert_eq!(
                l.dirs,
                vec![
                    (PathBuf::from("/etc/norte"), Layer::System),
                    (PathBuf::from("/home/u/.config/norte"), Layer::User),
                    (PathBuf::from(".norte"), Layer::Project),
                ]
            );
        }
    }

    // MAJOR-2 fix: the Windows branches are unreachable behind `cfg!(windows)`
    // on Linux-only CI, so they pin behavior through the explicit `_on` seam
    // instead of the `cfg!(windows)`-driven `_from` wrappers.

    #[test]
    fn windows_xdg_gana_a_appdata() {
        let e = env(&[
            ("XDG_CONFIG_HOME", "/xdg"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
            ("HOME", "/home/u"),
        ]);
        let d = user_config_dir_on(true, &e);
        assert_eq!(d, Some(PathBuf::from("/xdg").join("norte")));
    }

    #[test]
    fn windows_appdata_gana_a_home_sin_xdg() {
        let e = env(&[
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
            ("HOME", "/home/u"),
        ]);
        let d = user_config_dir_on(true, &e);
        assert_eq!(
            d,
            Some(PathBuf::from(r"C:\Users\u\AppData\Roaming").join("norte"))
        );
    }

    #[test]
    fn windows_standard_layers_con_programdata_y_appdata() {
        let e = env(&[
            ("ProgramData", r"C:\ProgramData"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
        ]);
        let l = standard_layers_on(true, &e);
        assert_eq!(
            l.dirs,
            vec![
                (
                    PathBuf::from(r"C:\ProgramData").join("norte"),
                    Layer::System
                ),
                (
                    PathBuf::from(r"C:\Users\u\AppData\Roaming").join("norte"),
                    Layer::User
                ),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }
}
