//! Config-directory resolution (ADR 0035). The single resolver every
//! norte process uses.

use std::ffi::OsString;
use std::path::PathBuf;

/// Clase de una capa de config (ADR 0007), en precedencia ASCENDENTE. Se
/// lleva POR DIR en [`Layers`] (deuda #75) en vez de inferirse por la
/// POSICIÓN en `dirs`: en Windows sin `%ProgramData%` la capa `System` está
/// ausente, así que `%APPDATA%` (`User`) caería en el índice 0 y la
/// inferencia posicional la etiquetaría como `System`. `Layer` se reexporta
/// desde `norte_tui::lua` para el `init.lua` (config es dueña del concepto
/// de capa; el scripting solo lo consume).
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
    if windows {
        if let Some(d) = get("APPDATA") {
            return Some(PathBuf::from(d).join("norte"));
        }
        // No `%APPDATA%` (unusual, but seen in constrained/service
        // environments): the pre-migration core resolver fell back to
        // `home_dir()`, which on Windows resolves via `%USERPROFILE%`. Keep
        // that same fallback here before the generic `$HOME` below (which a
        // stock Windows shell rarely sets).
        if let Some(up) = get("USERPROFILE") {
            return Some(PathBuf::from(up).join(".config").join("norte"));
        }
    }
    get("HOME").map(|h| PathBuf::from(h).join(".config").join("norte"))
}

/// The user config dir, resolved from an injectable environment (tests pass
/// a closure; production wrappers pass [`std::env::var_os`]). Precedence
/// (ADR 0035): `NORTE_CONFIG_DIR` (non-empty) → `XDG_CONFIG_HOME/norte`
/// (non-empty) → `%APPDATA%\norte` (Windows) → `%USERPROFILE%\.config\norte`
/// (Windows, no `%APPDATA%`) → `$HOME/.config/norte`. An empty
/// `NORTE_CONFIG_DIR` counts as unset, same as an empty `XDG_CONFIG_HOME`.
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

/// The user config dir from the process environment. Falls back to the
/// OS-reported home directory ([`std::env::home_dir`]) when none of the
/// environment variables resolve; `None` only when even the OS cannot name
/// a home (CI without HOME and no passwd entry): the caller warns.
#[must_use]
pub fn user_config_dir() -> Option<PathBuf> {
    user_config_dir_from(&|k| std::env::var_os(k)).or_else(|| {
        // Parity with the pre-ADR-0035 resolver: `std::env::home_dir()`
        // consults the OS (getpwuid_r on unix, the user profile on
        // Windows) when the env vars above are all absent. Without this, a
        // HOME-less daemon (systemd unit, cron, container) would silently
        // anchor connections.toml/known_hosts/secrets.age/policy.toml/
        // journal.db in a cwd-relative directory — an attacker-influenced
        // cwd must never decide where secrets live (security review, ADR
        // 0035 C1). `home_dir()` was deprecated 1.29–1.84 over an
        // inconsistent Windows implementation and un-deprecated in 1.85
        // once that was fixed; the workspace MSRV (1.94) postdates that.
        #[allow(deprecated)]
        std::env::home_dir().map(|h| h.join(".config").join("norte"))
    })
}

/// Infallible variant for core paths (`connections.toml`, `journal.db`, …):
/// falls back to `./.config/norte` only when [`user_config_dir`] returns
/// `None` (no env var AND the OS cannot name a home).
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

/// Standard layers WITHOUT the project layer (ADR 0035, C1 review): for
/// value-only core consumers (`[archive]`, `[ai]`) where every project-layer
/// value is carved out anyway by [`load`](crate::load::load) — parsing
/// `./.norte/norte.toml` there would give a foreign repo a startup-abort
/// lever over the daemon (a hostile or merely broken project `norte.toml`,
/// combined with `deny_unknown_fields`, aborts `norte daemon run`) and no
/// other effect.
#[must_use]
pub fn standard_layers_no_project() -> Layers {
    let mut l = standard_layers();
    l.dirs.retain(|(_, kind)| *kind != Layer::Project);
    l
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

    // Security review item 1 (C1): the PRODUCTION wrapper `user_config_dir()`
    // falls back to `std::env::home_dir()` when the whole environment is
    // silent (see its rustdoc). Not pinned by a test here: this crate
    // `#![forbid(unsafe_code)]` (CLAUDE.md rule 5), and mutating process env
    // to exercise that branch needs `unsafe` (edition 2024) — the injectable
    // `_from`/`_on` seam this module already tests deliberately stays
    // env-only (`getpwuid_r`/the Windows profile API are not injectable), so
    // there is no unsafe-free way to drive the fallback branch from a test
    // in this crate.

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

    /// Item 2 (both reviewers, C1): `standard_layers_no_project()` drops the
    /// `(./.norte, Project)` entry that `standard_layers()` always appends —
    /// value-only core consumers (`[archive]`, `[ai]`) must never let a
    /// foreign repo's `norte.toml` reach `deny_unknown_fields` and abort
    /// `norte daemon run`.
    #[test]
    fn standard_layers_no_project_excluye_proyecto() {
        let l = standard_layers_no_project();
        assert!(
            !l.dirs.iter().any(|(_, kind)| *kind == Layer::Project),
            "no Project entry: {:?}",
            l.dirs
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

    /// MINOR-4 fix: without `%APPDATA%` (unusual but seen in constrained
    /// environments), fall back to `%USERPROFILE%\.config\norte` — the
    /// pre-migration core resolver used `home_dir()`, which resolves via
    /// `%USERPROFILE%` on Windows; losing that fallback would be a
    /// regression for those environments.
    #[test]
    fn windows_sin_appdata_cae_a_userprofile() {
        let e = env(&[("USERPROFILE", r"C:\Users\u")]);
        let d = user_config_dir_on(true, &e);
        assert_eq!(
            d,
            Some(PathBuf::from(r"C:\Users\u").join(".config").join("norte"))
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
