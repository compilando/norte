//! Profiles (spec 2026-08-26): turning a profile NAME into a configuration
//! layer, or into a stated reason why not.
//!
//! Kept out of [`crate::dirs`] because that module answers "where does
//! configuration live" for every norte process; this one is a policy on top of
//! that answer, and only the frontends ask it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::dirs::{Layer, Layers, standard_layers_on, user_config_dir_on};

/// The directory holding every profile: `<user config dir>/profiles`.
///
/// Test seam behind [`profiles_dir_from`], with the target platform selected
/// explicitly. Not general API.
#[doc(hidden)]
#[must_use]
pub fn profiles_dir_on(windows: bool, get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    user_config_dir_on(windows, get).map(|d| d.join("profiles"))
}

/// The directory holding every profile, from an injectable environment.
///
/// `None` only when the user config dir itself cannot be resolved — there is
/// nowhere to hang a profile, and inventing a cwd-relative path would let an
/// attacker-influenced working directory decide which configuration loads.
///
/// # Example
///
/// ```
/// use norte_config::profiles_dir_from;
/// use std::ffi::OsString;
/// use std::path::PathBuf;
///
/// let get = |k: &str| -> Option<OsString> {
///     match k {
///         "NORTE_CONFIG_DIR" => Some(OsString::from("/custom")),
///         _ => None,
///     }
/// };
/// assert_eq!(
///     profiles_dir_from(&get),
///     Some(PathBuf::from("/custom/profiles"))
/// );
/// ```
#[must_use]
pub fn profiles_dir_from(get: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    profiles_dir_on(cfg!(windows), get)
}

/// One profile's directory. The name is joined as BYTES (rule 1): it is a
/// directory name, and passing it through `String` is how #245 and #246 sent a
/// layout name to the wrong file twice.
#[must_use]
pub fn profile_dir_from(get: &impl Fn(&str) -> Option<OsString>, name: &OsStr) -> Option<PathBuf> {
    profiles_dir_from(get).map(|d| d.join(name))
}

/// Splices `name`'s profile directory into `layers` right after the `User`
/// entry, or returns them unchanged when there is no `User` layer to hang it
/// from.
fn splice(mut layers: Layers, profile_dir: Option<PathBuf>) -> Layers {
    let Some(dir) = profile_dir else {
        return layers;
    };
    let Some(at) = layers.dirs.iter().position(|(_, k)| *k == Layer::User) else {
        // No user layer at all (no HOME, no NORTE_CONFIG_DIR): a profile has
        // nowhere to live, and manufacturing a path under the cwd is exactly
        // what `user_config_dir` refuses to do.
        return layers;
    };
    layers.dirs.insert(at + 1, (dir, Layer::Profile));
    layers
}

/// Test seam behind [`standard_layers_with_profile`], with the target platform
/// selected explicitly. Not general API.
#[doc(hidden)]
#[must_use]
pub fn standard_layers_with_profile_on(
    windows: bool,
    get: &impl Fn(&str) -> Option<OsString>,
    name: Option<&OsStr>,
) -> Layers {
    let base = standard_layers_on(windows, get);
    let dir = name.and_then(|n| profiles_dir_on(windows, get).map(|d| d.join(n)));
    splice(base, dir)
}

/// The standard layers (ADR 0007/0035) with `name`'s profile spliced in after
/// `User` and before `Project` (spec 2026-08-26, D1).
///
/// `None` gives exactly [`crate::dirs::standard_layers`].
#[must_use]
pub fn standard_layers_with_profile(name: Option<&OsStr>) -> Layers {
    standard_layers_with_profile_on(cfg!(windows), &|k| std::env::var_os(k), name)
}

/// Test seam behind [`standard_layers_no_project_with_profile`]. Not general
/// API.
#[doc(hidden)]
#[must_use]
pub fn standard_layers_no_project_on(
    windows: bool,
    get: &impl Fn(&str) -> Option<OsString>,
    name: Option<&OsStr>,
) -> Layers {
    let mut l = standard_layers_with_profile_on(windows, get, name);
    l.dirs
        .retain(|(_, kind)| *kind != Layer::Project && *kind != Layer::Profile);
    l
}

/// The value-only core layers, which drop BOTH `Project` and `Profile`.
///
/// The profile goes for the same reason the project layer does in
/// [`crate::dirs::standard_layers_no_project`]: every value a core consumer
/// reads (`[archive]`, `[ai]`, `[daemon]`, `[log]`) is carved out of a profile
/// anyway (D2), so parsing it there would give a frontend's choice of profile a
/// say over the daemon and no other effect. The `name` argument exists so a
/// caller cannot accidentally pass one and believe it was honoured.
#[must_use]
pub fn standard_layers_no_project_with_profile(name: Option<&OsStr>) -> Layers {
    standard_layers_no_project_on(cfg!(windows), &|k| std::env::var_os(k), name)
}

/// Every profile that exists under `dir`, by directory name, sorted by bytes.
///
/// A missing directory is an empty list, not an error: it means "you have no
/// profiles yet". Anything that is not a directory is skipped, so a stray
/// `profiles/README` is not a profile.
///
/// The names are [`OsString`] and never `String`: they are directory names and
/// they end up joined into a path (rule 1).
///
/// **This reads a directory, so it blocks.** Callers on an async runtime or an
/// event loop go through `spawn_blocking` (rule 2). #244 is the precedent: a
/// layout listing done inline froze the TUI's event loop.
///
/// # Errors
///
/// Any I/O error other than "not found" while reading the directory.
pub fn list_profiles(dir: &Path) -> std::io::Result<Vec<OsString>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.file_name());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    fn env<'a>(v: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            v.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, x)| OsString::from(x))
        }
    }

    #[test]
    fn el_dir_de_perfiles_cuelga_del_dir_de_usuario() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        assert_eq!(
            profiles_dir_on(false, &e),
            Some(PathBuf::from("/custom").join("profiles"))
        );
    }

    /// Bajo `NORTE_CONFIG_DIR` el resolutor es hermético (solo esa capa y
    /// `./.norte`), y el perfil tiene que quedarse DENTRO de esa hermeticidad
    /// o los tests dejarían de aislar lo que dicen aislar.
    #[test]
    fn con_norte_config_dir_el_perfil_sigue_dentro() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("work")));
        assert_eq!(
            l.dirs,
            vec![
                (PathBuf::from("/custom"), Layer::User),
                (PathBuf::from("/custom/profiles/work"), Layer::Profile),
                (PathBuf::from(".norte"), Layer::Project),
            ]
        );
    }

    #[test]
    fn sin_perfil_las_capas_son_las_de_siempre() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        let con = standard_layers_with_profile_on(false, &e, None);
        let sin = crate::dirs::standard_layers_on(false, &e);
        assert_eq!(con.dirs, sin.dirs);
    }

    /// El perfil va DESPUÉS de usuario y ANTES de proyecto, con las tres capas
    /// presentes (el caso hermético de arriba no tiene `System`).
    #[test]
    fn el_perfil_se_intercala_entre_usuario_y_proyecto() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("photos")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![Layer::System, Layer::User, Layer::Profile, Layer::Project]
        );
    }

    /// Un consumidor de valores del core (`[archive]`, `[ai]`) no ve la capa de
    /// perfil: no puede fijar nada de lo suyo (D2) y no tiene por qué saber qué
    /// perfil eligió un frontend.
    #[test]
    fn no_project_tampoco_trae_perfil() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_no_project_on(false, &e, Some(OsStr::new("work")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, vec![Layer::System, Layer::User]);
    }

    /// Sin capa de USUARIO no hay dónde colgar un perfil, y en vez de inventar
    /// una ruta se devuelven las capas tal cual: pedir un perfil que no puede
    /// existir no puede fabricar un directorio bajo el cwd.
    #[test]
    fn sin_capa_de_usuario_el_perfil_no_se_inventa() {
        let e = env(&[]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("work")));
        assert!(l.dirs.iter().all(|(_, k)| *k != Layer::Profile));
    }
}
