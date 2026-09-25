//! Config-directory resolution (ADR 0035). The single resolver every
//! norte process uses.

use std::ffi::OsString;
use std::path::PathBuf;

/// Class of a config layer (ADR 0007), in ASCENDING precedence. Carried PER
/// DIR in [`Layers`] (debt #75) instead of inferred from POSITION in `dirs`:
/// on Windows without `%ProgramData%` the `System` layer is absent, so
/// `%APPDATA%` (`User`) would fall at index 0 and positional inference would
/// tag it as `System`. `Layer` is re-exported from `norte_tui::lua` for
/// `init.lua` (config owns the layer concept; scripting only consumes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// `/etc/norte` (o `%ProgramData%\norte`).
    System,
    /// `$XDG_CONFIG_HOME/norte` (`~/.config/norte`; `%APPDATA%\norte`).
    User,
    /// `<config>/profiles/<name>` — the layer the reader picks by name
    /// (spec 2026-08-26, D1). Above `User` because picking a profile is meant
    /// to override what the user's own `norte.toml` says; below `Project` so
    /// ADR 0026 and #260 are untouched.
    Profile,
    /// `./.norte` — ONLY after trust (ADR 0026).
    Project,
}

/// The layer directories, in ASCENDING precedence, each with its [`Layer`]
/// (debt #75: the kind travels PER DIR, it is not inferred from position).
#[derive(Debug, Clone)]
pub struct Layers {
    /// system → user → project (the absent ones simply contribute nothing),
    /// each dir tagged with its layer class.
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

/// Test seam behind [`state_dir`]: same resolution, with the target platform
/// selected explicitly. Not general API — call [`state_dir`] instead.
#[doc(hidden)]
#[must_use]
pub fn state_dir_on(windows: bool, get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // An EMPTY variable is absent, across all three. Without this filter,
    // `HOME=""` returned `.local/state/norte` RELATIVE to the cwd — and a
    // file manager's cwd is the directory you launched it from, often a
    // repository. Since `state_dir` also decides where `lua-trust.toml`
    // lives, that let a hostile repo bring its own trust store pre-approving
    // its `.norte/init.lua`, and the TOFU prompt never appeared. Same danger
    // ADR 0035 C1 documents for the config directory, same fix.
    if windows {
        return get("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(|d| PathBuf::from(d).join("norte").join("state"));
    }
    if let Some(xdg) = get("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("norte"));
    }
    get("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".local/state/norte"))
}

/// The user's STATE directory: `$XDG_STATE_HOME/norte`,
/// `~/.local/state/norte`, or `%LOCALAPPDATA%\norte\state` on Windows.
///
/// **State, not configuration, and the difference matters**: this is where
/// the Lua trust store, the embedded journal and the local log live — things
/// belonging to THIS machine that must not travel with the user's dotfiles.
/// That is why `XDG_STATE_HOME` and not `XDG_CONFIG_HOME`, which is what
/// [`user_config_dir`] resolves.
///
/// `None` when the environment defines nothing (a bare CI, a service without
/// `HOME`): the caller DEGRADES with a warning, never invents a path relative
/// to the cwd. For the trust store that means fail-closed (with no store the
/// project script does not run); for the log, there is no file and stderr is
/// what is left.
///
/// ```
/// // On a machine with `HOME`, it resolves; with nothing defined, `None`.
/// let d = norte_config::dirs::state_dir();
/// assert!(d.is_none() || d.expect("present").ends_with("norte"));
/// ```
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    state_dir_on(cfg!(windows), &|k| std::env::var_os(k)).or_else(|| {
        // Parity with `user_config_dir`: with no environment variables, the
        // OS is asked (getpwuid_r on unix, the profile on Windows). Without
        // this, a daemon with no `HOME` — a systemd unit, cron, a container —
        // would end up with no state directory, which is fail-closed for the
        // trust store but also leaves the log with nowhere to go.
        #[allow(deprecated)]
        std::env::home_dir().map(|h| h.join(".local/state/norte"))
    })
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

    /// The enum's order IS the precedence (ADR 0007): system → user →
    /// profile → project. A `Profile` placed elsewhere compiles just as well
    /// and leaves the layer taking precedence where it should not, so it is
    /// pinned here.
    #[test]
    fn profile_sits_between_user_and_project() {
        assert!(Layer::System < Layer::User);
        assert!(Layer::User < Layer::Profile);
        assert!(Layer::Profile < Layer::Project);
    }

    /// Precedence of the STATE directory, with the environment injected — this
    /// way the Windows branch is pinned by a suite that only runs on Linux,
    /// same as `user_config_dir_on` does.
    #[test]
    fn state_dir_follows_its_precedence() {
        // XDG beats HOME.
        assert_eq!(
            state_dir_on(false, &env(&[("XDG_STATE_HOME", "/x"), ("HOME", "/h")])),
            Some(PathBuf::from("/x/norte"))
        );
        // Empty is ABSENT, not a path to the root — same rule as config with
        // its `XDG_CONFIG_HOME`.
        assert_eq!(
            state_dir_on(false, &env(&[("XDG_STATE_HOME", ""), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/state/norte"))
        );
        // Windows does not look at XDG.
        assert_eq!(
            state_dir_on(
                true,
                &env(&[("XDG_STATE_HOME", "/x"), ("LOCALAPPDATA", "C:/s")])
            ),
            Some(PathBuf::from("C:/s").join("norte").join("state"))
        );
        // And a bare environment invents nothing: the caller degrades with a
        // warning.
        assert_eq!(state_dir_on(false, &env(&[])), None);
        // An empty `HOME` is ABSENT, not a path relative to the cwd: that is
        // also where `lua-trust.toml` ends up, and a trust store inside the
        // working tree would be written by whatever repository you are
        // looking at.
        assert_eq!(state_dir_on(false, &env(&[("HOME", "")])), None);
        assert_eq!(state_dir_on(true, &env(&[("LOCALAPPDATA", "")])), None);
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
    fn no_environment_is_none() {
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
    fn standard_layers_with_override_is_hermetic() {
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
    fn standard_layers_no_project_excludes_project() {
        let l = standard_layers_no_project();
        assert!(
            !l.dirs.iter().any(|(_, kind)| *kind == Layer::Project),
            "no Project entry: {:?}",
            l.dirs
        );
    }

    #[cfg(unix)]
    #[test]
    fn standard_layers_without_override_includes_system() {
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
    fn norte_config_dir_empty_counts_as_unset() {
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
    fn windows_xdg_beats_appdata() {
        let e = env(&[
            ("XDG_CONFIG_HOME", "/xdg"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
            ("HOME", "/home/u"),
        ]);
        let d = user_config_dir_on(true, &e);
        assert_eq!(d, Some(PathBuf::from("/xdg").join("norte")));
    }

    #[test]
    fn windows_appdata_beats_home_without_xdg() {
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
    fn windows_without_appdata_falls_back_to_userprofile() {
        let e = env(&[("USERPROFILE", r"C:\Users\u")]);
        let d = user_config_dir_on(true, &e);
        assert_eq!(
            d,
            Some(PathBuf::from(r"C:\Users\u").join(".config").join("norte"))
        );
    }

    #[test]
    fn windows_standard_layers_with_programdata_and_appdata() {
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
