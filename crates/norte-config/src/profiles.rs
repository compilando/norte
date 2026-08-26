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

/// Who asked for this profile. It decides what happens when it does not load
/// (spec 2026-08-26, D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    /// The human named it in this invocation (`--profile`).
    Explicit,
    /// It came from the previous session.
    Sticky,
    /// A switch, with the program already running.
    Switch,
}

/// Why a profile could not be used.
///
/// Its own type rather than a new [`crate::schema::ConfigError`] variant: a
/// missing profile is neither an I/O error on a file that exists nor invalid
/// TOML, and adding a variant to a public enum breaks every exhaustive match
/// on it.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The profile directory is not there.
    #[error("el perfil «{}» no está en {}", name.to_string_lossy(), dir.display())]
    NotFound {
        /// The name that was asked for.
        name: OsString,
        /// Where it was looked for.
        dir: PathBuf,
    },
    /// A layer did not load. Which layer is in the message.
    #[error(transparent)]
    Config(#[from] crate::schema::ConfigError),
}

/// The result of a load that had a profile in it, and what it had to give up.
#[derive(Debug)]
pub struct ProfileLoad {
    /// The resulting configuration.
    pub config: crate::load::CommonConfig,
    /// The profile that ended up active. `None` = started with NO profile
    /// layer.
    pub active: Option<OsString>,
    /// Why the profile that was asked for could not be used. `None` = it was.
    pub degraded: Option<String>,
}

/// Loads with a profile in the layers, answering a broken one according to who
/// asked for it (spec 2026-08-26, D7).
///
/// `layers_for` is a closure and not a [`Layers`] value because the degrade
/// path has to rebuild the layers WITHOUT the profile, and rebuilding is the
/// only honest way: filtering a vector would leave a `Layers` no resolver ever
/// produced.
///
/// The three answers:
///
/// - [`ProfileSource::Explicit`] — fatal. The reader named that profile;
///   starting as something else answers a different question.
/// - [`ProfileSource::Sticky`] — start with no profile layer and say so.
///   Nobody asked for it this run, and aborting would trap the reader outside
///   the program with no way to pick another.
/// - [`ProfileSource::Switch`] — refused. The caller keeps the configuration
///   it already had; a half-applied profile is not a state this design admits.
///
/// A failure in a layer that is NOT the profile's is fatal for all three: a
/// broken user `norte.toml` is the reader's own, and hiding it behind the
/// degrade path is exactly what [`crate::load::load`] refuses to do.
///
/// # Errors
///
/// [`ProfileError`] when the profile cannot be used and the source is not
/// [`ProfileSource::Sticky`], or when any other layer fails to load.
pub fn load_with_profile(
    layers_for: &impl Fn(Option<&OsStr>) -> Layers,
    name: Option<&OsStr>,
    source: ProfileSource,
) -> Result<ProfileLoad, ProfileError> {
    let Some(name) = name else {
        return Ok(ProfileLoad {
            config: crate::load::load(&layers_for(None))?,
            active: None,
            degraded: None,
        });
    };

    let layers = layers_for(Some(name));
    let problema: Option<ProfileError> = match layers
        .dirs
        .iter()
        .find(|(_, k)| *k == Layer::Profile)
        .map(|(d, _)| d.clone())
    {
        Some(dir) if !dir.is_dir() => Some(ProfileError::NotFound {
            name: name.to_owned(),
            dir,
        }),
        // No profile layer in these layers at all: nothing was asked of the
        // resolver that it could refuse, so this is the plain load.
        None => None,
        Some(_) => match crate::load::load(&layers) {
            Ok(config) => {
                return Ok(ProfileLoad {
                    config,
                    active: Some(name.to_owned()),
                    degraded: None,
                });
            }
            Err(e) => Some(ProfileError::Config(e)),
        },
    };

    let Some(problema) = problema else {
        return Ok(ProfileLoad {
            config: crate::load::load(&layers)?,
            active: None,
            degraded: None,
        });
    };

    // Before blaming the profile, load without it. If THAT fails too, the real
    // fault is in a layer the reader owns outright, and reporting the profile
    // would send them to fix the wrong file.
    let sin_perfil = crate::load::load(&layers_for(None))?;
    match source {
        ProfileSource::Sticky => Ok(ProfileLoad {
            config: sin_perfil,
            active: None,
            degraded: Some(problema.to_string()),
        }),
        ProfileSource::Explicit | ProfileSource::Switch => Err(problema),
    }
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

    /// Un árbol con un perfil `name` cuyo `norte.toml` NO parsea (una errata
    /// realista: una clave desconocida, que `deny_unknown_fields` hace fatal).
    fn arbol_con_perfil_roto(
        name: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        arbol(name, "[ui]\nthem = \"nord\"\n")
    }

    /// Un árbol con un perfil `name` sano.
    fn arbol_con_perfil_sano(
        name: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        arbol(name, "[ui]\ntheme = \"nord\"\n")
    }

    /// Un árbol SIN perfiles: solo la capa de usuario.
    fn arbol_sin_perfiles() -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        let usuario = tempfile::tempdir().expect("tempdir");
        let raiz = usuario.path().to_path_buf();
        let f = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(raiz.clone(), Layer::User)],
                Some(n) => vec![
                    (raiz.clone(), Layer::User),
                    (raiz.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        (f, vec![usuario])
    }

    fn arbol(
        name: &str,
        contenido: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        let usuario = tempfile::tempdir().expect("tempdir");
        let dir = usuario.path().join("profiles").join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("norte.toml"), contenido).expect("write");
        let raiz = usuario.path().to_path_buf();
        let f = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(raiz.clone(), Layer::User)],
                Some(n) => vec![
                    (raiz.clone(), Layer::User),
                    (raiz.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        // Los guards VUELVEN: un `TempDir` que cae borra el árbol, y un test
        // que los pierde acaba probando el camino de «el directorio no está»
        // sin enterarse.
        (f, vec![usuario])
    }

    /// `--profile` roto ABORTA: el lector pidió ese perfil por su nombre, y
    /// arrancar como otra cosa sería contestar otra pregunta.
    #[test]
    fn explicito_y_roto_es_fatal() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Explicit)
            .expect_err("tiene que abortar");
        assert!(
            format!("{err}").contains("norte.toml"),
            "y decir qué fichero: {err}"
        );
    }

    /// El PEGAJOSO roto arranca sin capa de perfil y lo dice. Abortar dejaría
    /// al lector fuera del programa, sin manera de elegir otro.
    #[test]
    fn pegajoso_y_roto_arranca_sin_perfil_y_lo_dice() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("arranca igual");
        assert_eq!(r.active, None, "sin capa de perfil");
        assert!(r.degraded.is_some(), "y no en silencio");
    }

    /// Cambiar EN CALIENTE a uno roto se RECHAZA: el llamante se queda con la
    /// configuración que ya tenía. Un perfil a medio aplicar no es un estado
    /// que este diseño admita.
    #[test]
    fn cambiar_a_uno_roto_se_rechaza() {
        let (dirs, _guards) = arbol_con_perfil_roto("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Switch)
            .expect_err("el cambio se rechaza");
        assert!(format!("{err}").contains("norte.toml"), "{err}");
    }

    /// Un perfil que NO EXISTE sigue la misma regla: es la misma pregunta
    /// («¿puedo usar el que pediste?») con la misma respuesta por procedencia.
    #[test]
    fn un_perfil_que_no_existe_sigue_la_misma_regla() {
        let (dirs, _guards) = arbol_sin_perfiles();
        assert!(
            load_with_profile(&dirs, Some(OsStr::new("fantasma")), ProfileSource::Explicit)
                .is_err()
        );
        let r = load_with_profile(&dirs, Some(OsStr::new("fantasma")), ProfileSource::Sticky)
            .expect("arranca");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// Y el camino feliz sigue siendo el camino feliz.
    #[test]
    fn un_perfil_sano_queda_activo_y_sin_degradar() {
        let (dirs, _guards) = arbol_con_perfil_sano("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("carga");
        assert_eq!(r.active.as_deref(), Some(OsStr::new("work")));
        assert!(r.degraded.is_none());
        assert_eq!(r.config.ui_theme.as_deref(), Some("nord"));
    }

    /// Un `norte.toml` DEL USUARIO roto es fatal para las tres procedencias.
    /// Degradar aquí escondería el error de la capa que ADR 0035 declara
    /// suya: «arrancar ignorándolas en silencio sería peor que no arrancar».
    #[test]
    fn una_capa_de_usuario_rota_es_fatal_incluso_degradando() {
        let usuario = tempfile::tempdir().expect("tempdir");
        std::fs::write(usuario.path().join("norte.toml"), "[ui]\nthem = 1\n").expect("write");
        let raiz = usuario.path().to_path_buf();
        let dirs = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(raiz.clone(), Layer::User)],
                Some(n) => vec![
                    (raiz.clone(), Layer::User),
                    (raiz.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        assert!(
            load_with_profile(&dirs, Some(OsStr::new("fantasma")), ProfileSource::Sticky).is_err(),
            "la capa del usuario no se degrada por el camino del perfil"
        );
    }

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
