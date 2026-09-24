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

/// Names Win32 treats as devices no matter the extension behind them.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether `name` can be a directory INSIDE `profiles/`, and nothing else.
///
/// This is the canonical check, and `norte_frontend::layout::config` defers to
/// it for layout filenames — the rules are the same question ("can this name
/// be a single entry of a directory we own?") and had no business being
/// answered twice.
///
/// `Path::components().count() == 1` does NOT do it, and that was the check
/// before #246: on Windows `Path::new("C:")` is exactly one component — a
/// `Prefix` — and [`Path::join`] with a prefix REPLACES the whole base. The
/// same is true, on every platform, of an absolute path: `profiles.join("/etc")`
/// is `/etc`. So the name is inspected as a NAME, never as a path.
///
/// What this refuses, and why each one matters for a profile:
///
/// - empty — `profiles/` itself becomes the layer, so `profiles/norte.toml`
///   and `profiles/init.lua` load as configuration;
/// - `.` and `..` — `..` is the user's whole config directory as a second,
///   duplicated layer;
/// - anything holding `/`, `\`, `:` or NUL — traversal, a Windows drive
///   prefix, or an NTFS alternate stream. `--profile ../../../tmp/pwn` points
///   the layer at a directory nobody vetted;
/// - a trailing dot or space — Windows eats them, so the directory opened is
///   not the one named;
/// - the Win32 device names, extension or not.
///
/// It is deliberately NOT enough on its own: [`load_with_profile`] also
/// requires the name to appear byte-for-byte in [`list_profiles`], because a
/// legal name still must not be resolved by a case-folding filesystem (#245).
///
/// ```
/// use norte_config::valid_profile_name;
/// use std::ffi::OsStr;
///
/// assert!(valid_profile_name(OsStr::new("work")));
/// assert!(!valid_profile_name(OsStr::new("..")));
/// assert!(!valid_profile_name(OsStr::new("/etc/norte")));
/// assert!(!valid_profile_name(OsStr::new("")));
/// ```
#[must_use]
pub fn valid_profile_name(name: &OsStr) -> bool {
    let text = name.to_string_lossy();
    if text.is_empty() || text == "." || text == ".." {
        return false;
    }
    if text.contains(['/', '\\', ':', '\0']) {
        return false;
    }
    if text.ends_with('.') || text.ends_with(' ') {
        return false;
    }
    let root = text.split('.').next().unwrap_or(&text);
    !RESERVED.iter().any(|r| root.eq_ignore_ascii_case(r))
}

/// One profile's directory, or `None` when the name cannot be one.
///
/// The name is joined as BYTES (rule 1): it is a directory name, and passing it
/// through `String` is how #245 and #246 sent a layout name to the wrong file
/// twice. It is also checked with [`valid_profile_name`] FIRST — joining an
/// unvetted name is how a configuration layer ends up pointing outside the
/// directory it was supposed to live in.
#[must_use]
pub fn profile_dir_from(get: &impl Fn(&str) -> Option<OsString>, name: &OsStr) -> Option<PathBuf> {
    if !valid_profile_name(name) {
        return None;
    }
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
    let dir = name
        .filter(|n| valid_profile_name(n))
        .and_then(|n| profiles_dir_on(windows, get).map(|d| d.join(n)));
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

/// Whether `dir` is a profile that `dir`'s parent LISTS under exactly these
/// bytes.
///
/// Not `dir.is_dir()`, which was the first version of this and was wrong twice
/// over. It follows symlinks while [`list_profiles`] does not, so a symlinked
/// profile loaded and never appeared in the picker; and it lets the operating
/// system resolve the name, so on a case-folding filesystem asking for `WORK`
/// opens `work` — #245, exactly, and `norte_frontend::layout::config::load`
/// learned it first for the same reason.
fn is_in_the_listing(dir: &Path, name: &OsStr) -> bool {
    let Some(parent) = dir.parent() else {
        return false;
    };
    list_profiles(parent).is_ok_and(|v| v.iter().any(|n| n == name))
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
    /// The name cannot be a directory inside `profiles/`.
    #[error("\"{}\" cannot be a profile name", name.to_string_lossy())]
    BadName {
        /// The name that was asked for.
        name: OsString,
    },
    /// There is nowhere to put a profile layer: no user config directory
    /// resolved, so `profiles/` has no parent.
    #[error("no user configuration directory to hang a profile on")]
    NoHome,
    /// The profile directory is not there.
    #[error("profile \"{}\" is not in {}", name.to_string_lossy(), dir.display())]
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
///
/// Generic over what "the configuration" means, because D7 is a rule about a
/// LAYER and a layer is more than its `norte.toml`: a frontend also reads
/// `keymap.toml` and `openers.toml` from the same directory, and both are
/// fatal outside the project layer. Answering the three-way question over
/// `norte.toml` alone declared a profile with a typo'd shortcut healthy and
/// let it blow up afterwards (#305).
#[derive(Debug)]
pub struct Loaded<T> {
    /// The resulting configuration.
    pub config: T,
    /// The profile that ended up active. `None` = started with NO profile
    /// layer.
    pub active: Option<OsString>,
    /// Why the profile that was asked for could not be used. `None` = it was.
    pub degraded: Option<String>,
}

/// [`Loaded`] over the scalars this crate merges. What
/// [`load_with_profile`] returns.
pub type ProfileLoad = Loaded<crate::load::CommonConfig>;

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
    load_with(layers_for, name, source, &crate::load::load)
}

/// [`load_with_profile`], but over whatever a caller means by "loading these
/// layers".
///
/// The rule of D7 lives HERE and in exactly one place. A frontend's layer is
/// its `norte.toml` plus its `keymap.toml` plus its `openers.toml`, and the
/// last two are fatal for any layer that is not the project's — so a frontend
/// that went through [`load_with_profile`] got "this profile is fine" for a
/// profile that was about to abort the program. Passing the loader in, instead
/// of duplicating the three-way answer upstairs, is what keeps the two from
/// drifting (#305).
///
/// `load` must fail with [`crate::schema::ConfigError`], which both
/// [`crate::load::load`] and `norte_frontend::config::load` already do.
///
/// # Errors
///
/// The same as [`load_with_profile`]: [`ProfileError`] when the profile cannot
/// be used and the source is not [`ProfileSource::Sticky`], or when any other
/// layer fails to load.
pub fn load_with<T>(
    layers_for: &impl Fn(Option<&OsStr>) -> Layers,
    name: Option<&OsStr>,
    source: ProfileSource,
    load: &impl Fn(&Layers) -> Result<T, crate::schema::ConfigError>,
) -> Result<Loaded<T>, ProfileError> {
    let Some(name) = name else {
        return Ok(Loaded {
            config: load(&layers_for(None))?,
            active: None,
            degraded: None,
        });
    };

    let layers = layers_for(Some(name));
    let problem: Option<ProfileError> = if valid_profile_name(name) {
        match layers
            .dirs
            .iter()
            .find(|(_, k)| *k == Layer::Profile)
            .map(|(d, _)| d.clone())
        {
            // A name was asked for and no profile layer came back: the
            // resolver had nowhere to hang it (no user config directory). This
            // is a REFUSAL and not a quiet "no profile", or `--profile work`
            // would start as something else without a word — the outcome D7
            // declares fatal for an explicit request.
            None => Some(ProfileError::NoHome),
            Some(dir) if !is_in_the_listing(&dir, name) => Some(ProfileError::NotFound {
                name: name.to_owned(),
                dir,
            }),
            Some(_) => match load(&layers) {
                Ok(config) => {
                    return Ok(Loaded {
                        config,
                        active: Some(name.to_owned()),
                        degraded: None,
                    });
                }
                Err(e) => Some(ProfileError::Config(e)),
            },
        }
    } else {
        Some(ProfileError::BadName {
            name: name.to_owned(),
        })
    };

    let Some(problem) = problem else {
        return Ok(Loaded {
            config: load(&layers)?,
            active: None,
            degraded: None,
        });
    };

    // Before blaming the profile, load without it. If THAT fails too, the real
    // fault is in a layer the reader owns outright, and reporting the profile
    // would send them to fix the wrong file.
    let without_profile = load(&layers_for(None))?;
    match source {
        ProfileSource::Sticky => Ok(Loaded {
            config: without_profile,
            active: None,
            degraded: Some(problem.to_string()),
        }),
        ProfileSource::Explicit | ProfileSource::Switch => Err(problem),
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
        // `metadata` and not `entry.file_type()`: the second is `lstat`, so a
        // symlink to a directory would not be listed — and keeping a profile
        // in a dotfiles repository and symlinking it here is the obvious way
        // to carry one between machines, which is a thing the design asks for.
        // A DECISION, not an accident: whoever can plant a symlink in this
        // directory can already rewrite the `norte.toml` next to it, so
        // following one grants nothing new.
        //
        // It also has to agree with `is_in_the_listing`, which asks this
        // same question for `load_with_profile`. Two answers to one question
        // is how a profile loads by name and never appears in the picker.
        if std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()) {
            out.push(entry.file_name());
        }
    }
    out.sort();
    Ok(out)
}

/// What a profile SAVES from the current screen (#306, ADR 0079).
///
/// One type and not six arguments: these are six things that go together or
/// not at all, and half are `Option`.
#[derive(Debug, Default)]
pub struct ProfileSnapshot {
    /// The title to display (`[profile] title`), if the reader set one.
    pub title: Option<String>,
    /// The live layout, already in TOML — serialized by whoever holds it
    /// (`norte_frontend::layout::config::to_toml`): this crate does not know
    /// the slot tree and must not.
    pub layout_toml: Option<String>,
    /// The `[ui]` scalars that DIFFER from what the user layer already says.
    /// Only those: copying the ones that match would add noise nobody could
    /// later tell was deliberate.
    pub ui: Vec<(String, toml_edit::Value)>,
    /// `[profile.start]`: where each slot opens the first time, by slot id as
    /// text (TOML has no numeric keys).
    pub start: Vec<(String, String)>,
    /// The `keymap.toml` to copy as is, if the starting profile had one.
    ///
    /// Copied BYTE for BYTE and not rewritten: it is the reader's file, with
    /// their comments, and "save as" produces a profile that behaves like the
    /// one you had — if you changed shortcuts, the new one carries them.
    pub keymap: Option<Vec<u8>>,
}

/// The name of the layout file [`save_profile`] writes.
///
/// Fixed, and that is why the caller does not choose it: the profile's
/// `norte.toml` points to it with `[ui] layout`, and two names for the same
/// thing is a pair that can come apart.
pub const PROFILE_LAYOUT_NAME: &str = "workspace";

/// Writes `profiles/<name>/` with what is on screen (#306).
///
/// Returns the profile's directory.
///
/// **Does not check whether it already exists**: the caller asks beforehand,
/// because the answer to "is there already one with that name?" belongs to
/// the human, not to a writer. What it does do is leave alone what it does
/// not write: a profile that already had other files keeps them.
///
/// The `norte.toml` is composed with the same `persist_*` family as the rest
/// — lock, tmp+rename, comments preserved — so saving over a hand-written
/// profile does not run over whatever was there.
///
/// # Errors
/// Whatever fails while creating the directories or writing any of the three
/// files. A name [`valid_profile_name`] rejects is
/// [`std::io::ErrorKind::InvalidInput`]: a profile name ends up being a
/// directory, and composing the path with an invalid one is what this guard
/// exists to prevent.
pub fn save_profile(
    profiles_dir: &Path,
    name: &OsStr,
    snap: &ProfileSnapshot,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    if !valid_profile_name(name) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "that name cannot be a profile directory",
        ));
    }
    let dir = profiles_dir.join(name);
    std::fs::create_dir_all(&dir)?;
    if let Some(toml) = &snap.layout_toml {
        let layouts = dir.join("layouts");
        std::fs::create_dir_all(&layouts)?;
        std::fs::write(
            layouts.join(format!("{PROFILE_LAYOUT_NAME}.toml")),
            toml.as_bytes(),
        )?;
        crate::load::persist_set(
            &dir,
            "ui",
            "layout",
            toml_edit::Value::from(PROFILE_LAYOUT_NAME),
        )?;
    }
    for (key, value) in &snap.ui {
        crate::load::persist_set(&dir, "ui", key, value.clone())?;
    }
    if let Some(title) = &snap.title {
        crate::load::persist_set(
            &dir,
            "profile",
            "title",
            toml_edit::Value::from(title.as_str()),
        )?;
    }
    for (slot, destination) in &snap.start {
        crate::load::persist_set(
            &dir,
            "profile.start",
            slot,
            toml_edit::Value::from(destination.as_str()),
        )?;
    }
    if let Some(bytes) = &snap.keymap {
        std::fs::write(dir.join("keymap.toml"), bytes)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests_save {
    use super::*;

    /// **Saving a profile writes all three pieces** (#306): the layout with
    /// its `[ui] layout` pointing at it, where each slot opens, and the
    /// keymap carried over from the starting profile.
    #[test]
    fn saving_a_profile_writes_the_three_pieces() {
        let root = tempfile::tempdir().expect("tempdir");
        let snap = ProfileSnapshot {
            title: Some("Photos".to_owned()),
            layout_toml: Some("kind = \"slot\"\n".to_owned()),
            ui: vec![("theme".to_owned(), toml_edit::Value::from("nord"))],
            start: vec![("1".to_owned(), "file:///photos".to_owned())],
            keymap: Some(b"# my keys\n".to_vec()),
        };
        let dir = save_profile(root.path(), OsStr::new("photos"), &snap).expect("saves");

        let toml = std::fs::read_to_string(dir.join("norte.toml")).expect("norte.toml");
        assert!(toml.contains("layout = \"workspace\""), "{toml}");
        assert!(toml.contains("theme = \"nord\""), "{toml}");
        assert!(toml.contains("title = \"Photos\""), "{toml}");
        assert!(toml.contains("file:///photos"), "{toml}");
        assert_eq!(
            std::fs::read_to_string(dir.join("layouts/workspace.toml")).expect("layout"),
            "kind = \"slot\"\n"
        );
        assert_eq!(
            std::fs::read(dir.join("keymap.toml")).expect("keymap"),
            b"# my keys\n"
        );
    }

    /// A name that cannot be a directory is refused BEFORE touching disk:
    /// composing the path with `..` is what this guard exists to prevent.
    #[test]
    fn a_name_that_is_not_a_directory_writes_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        for bad in ["..", "", "a/b", "."] {
            let e = save_profile(root.path(), OsStr::new(bad), &ProfileSnapshot::default())
                .expect_err("not valid");
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{bad}");
        }
        assert_eq!(
            std::fs::read_dir(root.path()).expect("read_dir").count(),
            0,
            "and not even a directory was created"
        );
    }

    /// Saving OVER one that already exists rewrites its pieces and leaves the
    /// rest of its files as they were: a profile belongs to the reader, not
    /// to this writer.
    #[test]
    fn saving_over_keeps_what_it_does_not_write() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("photos");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("openers.toml"), b"# mine\n").expect("openers");

        let snap = ProfileSnapshot {
            layout_toml: Some("kind = \"slot\"\n".to_owned()),
            ..ProfileSnapshot::default()
        };
        save_profile(root.path(), OsStr::new("photos"), &snap).expect("saves");

        assert_eq!(
            std::fs::read(dir.join("openers.toml")).expect("openers"),
            b"# mine\n",
            "what it does not write, it does not touch"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    /// A tree with a profile `name` whose `norte.toml` does NOT parse (a
    /// realistic typo: an unknown key, which `deny_unknown_fields` makes
    /// fatal).
    fn tree_with_broken_profile(
        name: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        tree(name, "[ui]\nthem = \"nord\"\n")
    }

    /// A tree with a healthy profile `name`.
    fn tree_with_healthy_profile(
        name: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        tree(name, "[ui]\ntheme = \"nord\"\n")
    }

    /// A tree with NO profiles: only the user layer.
    fn tree_without_profiles() -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        let user = tempfile::tempdir().expect("tempdir");
        let root = user.path().to_path_buf();
        let f = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(root.clone(), Layer::User)],
                Some(n) => vec![
                    (root.clone(), Layer::User),
                    (root.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        (f, vec![user])
    }

    fn tree(
        name: &str,
        content: &str,
    ) -> (
        impl Fn(Option<&OsStr>) -> Layers + use<>,
        Vec<tempfile::TempDir>,
    ) {
        let user = tempfile::tempdir().expect("tempdir");
        let dir = user.path().join("profiles").join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("norte.toml"), content).expect("write");
        let root = user.path().to_path_buf();
        let f = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(root.clone(), Layer::User)],
                Some(n) => vec![
                    (root.clone(), Layer::User),
                    (root.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        // The guards COME BACK: a `TempDir` that is dropped deletes the tree,
        // and a test that loses them ends up testing the "the directory is
        // not there" path without realizing it.
        (f, vec![user])
    }

    /// A profile name must not be able to point the LAYER anywhere on disk.
    /// `Path::join` with an absolute path — or with a drive prefix on
    /// Windows — replaces the WHOLE base, and `..` climbs up.
    #[test]
    fn a_name_cannot_escape_the_profiles_directory() {
        for bad in [
            "",
            ".",
            "..",
            "../../../tmp/pwn",
            "/etc/norte",
            "C:",
            "notes:secret",
            "work/../..",
            "work.",
            "work ",
            "CON",
            "con.toml",
        ] {
            assert!(
                !valid_profile_name(OsStr::new(bad)),
                "\"{bad}\" cannot be a profile name"
            );
        }
        for good in ["work", "photos", "my profile", "work.2"] {
            assert!(valid_profile_name(OsStr::new(good)), "\"{good}\" is valid");
        }
    }

    /// And the gate is not just the validator: `profile_dir_from` and the
    /// layer resolver REFUSE to build the path, instead of building it and
    /// trusting someone to look.
    #[test]
    fn a_hostile_name_produces_neither_a_path_nor_a_layer() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        assert_eq!(profile_dir_from(&e, OsStr::new("..")), None);
        assert_eq!(profile_dir_from(&e, OsStr::new("/etc/norte")), None);

        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("../../etc")));
        assert!(
            l.dirs.iter().all(|(_, k)| *k != Layer::Profile),
            "a layer pointing outside slipped through: {:?}",
            l.dirs
        );
    }

    /// The name has to be in the LISTING, byte for byte. Letting the
    /// filesystem resolve it opens `work` when `WORK` was asked for on macOS
    /// and Windows — #245, exactly, and what D4 promised and was missing.
    #[test]
    fn the_name_is_compared_against_the_listing_byte_for_byte() {
        let (dirs, _guards) = tree_with_healthy_profile("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("WORK")), ProfileSource::Explicit)
            .expect_err("there is no profile with that name");
        assert!(matches!(err, ProfileError::NotFound { .. }), "{err:?}");
    }

    /// A hostile name follows the same three-answer rule as a broken profile:
    /// fatal if the human named it, degraded if it came from the session.
    #[test]
    fn a_hostile_name_follows_the_three_answer_rule() {
        let (dirs, _guards) = tree_with_healthy_profile("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("..")), ProfileSource::Explicit)
            .expect_err("aborts");
        assert!(matches!(err, ProfileError::BadName { .. }), "{err:?}");

        let r = load_with_profile(&dirs, Some(OsStr::new("..")), ProfileSource::Sticky)
            .expect("starts with no profile");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// A broken `--profile` ABORTS: the reader asked for that profile by
    /// name, and starting as something else would answer a different
    /// question.
    #[test]
    fn explicit_and_broken_is_fatal() {
        let (dirs, _guards) = tree_with_broken_profile("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Explicit)
            .expect_err("has to abort");
        assert!(
            format!("{err}").contains("norte.toml"),
            "and say which file: {err}"
        );
    }

    /// A broken STICKY one starts with no profile layer and says so.
    /// Aborting would leave the reader outside the program, with no way to
    /// choose another.
    #[test]
    fn sticky_and_broken_starts_with_no_profile_and_says_so() {
        let (dirs, _guards) = tree_with_broken_profile("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("starts anyway");
        assert_eq!(r.active, None, "no profile layer");
        assert!(r.degraded.is_some(), "and not silently");
    }

    /// Switching LIVE to a broken one is REFUSED: the caller keeps the
    /// configuration it already had. A half-applied profile is not a state
    /// this design admits.
    #[test]
    fn switching_to_a_broken_one_is_refused() {
        let (dirs, _guards) = tree_with_broken_profile("work");
        let err = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Switch)
            .expect_err("the switch is refused");
        assert!(format!("{err}").contains("norte.toml"), "{err}");
    }

    /// A profile that does NOT EXIST follows the same rule: it is the same
    /// question ("can I use the one you asked for?") with the same answer by
    /// source.
    #[test]
    fn a_profile_that_does_not_exist_follows_the_same_rule() {
        let (dirs, _guards) = tree_without_profiles();
        assert!(
            load_with_profile(&dirs, Some(OsStr::new("ghost")), ProfileSource::Explicit).is_err()
        );
        let r = load_with_profile(&dirs, Some(OsStr::new("ghost")), ProfileSource::Sticky)
            .expect("starts");
        assert_eq!(r.active, None);
        assert!(r.degraded.is_some());
    }

    /// And the happy path is still the happy path.
    #[test]
    fn a_healthy_profile_ends_up_active_and_not_degraded() {
        let (dirs, _guards) = tree_with_healthy_profile("work");
        let r = load_with_profile(&dirs, Some(OsStr::new("work")), ProfileSource::Sticky)
            .expect("loads");
        assert_eq!(r.active.as_deref(), Some(OsStr::new("work")));
        assert!(r.degraded.is_none());
        assert_eq!(r.config.ui_theme.as_deref(), Some("nord"));
    }

    /// A broken USER `norte.toml` is fatal for all three sources. Degrading
    /// here would hide the error in the layer ADR 0035 declares its own:
    /// "starting while silently ignoring it would be worse than not
    /// starting".
    #[test]
    fn a_broken_user_layer_is_fatal_even_while_degrading() {
        let user = tempfile::tempdir().expect("tempdir");
        std::fs::write(user.path().join("norte.toml"), "[ui]\nthem = 1\n").expect("write");
        let root = user.path().to_path_buf();
        let dirs = move |n: Option<&OsStr>| Layers {
            dirs: match n {
                None => vec![(root.clone(), Layer::User)],
                Some(n) => vec![
                    (root.clone(), Layer::User),
                    (root.join("profiles").join(n), Layer::Profile),
                ],
            },
        };
        assert!(
            load_with_profile(&dirs, Some(OsStr::new("ghost")), ProfileSource::Sticky).is_err(),
            "the user layer does not degrade through the profile path"
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
    fn the_profiles_dir_hangs_off_the_user_dir() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        assert_eq!(
            profiles_dir_on(false, &e),
            Some(PathBuf::from("/custom").join("profiles"))
        );
    }

    /// Under `NORTE_CONFIG_DIR` the resolver is hermetic (only that layer and
    /// `./.norte`), and the profile has to stay INSIDE that hermeticity or
    /// the tests would stop isolating what they claim to isolate.
    #[test]
    fn with_norte_config_dir_the_profile_stays_inside() {
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
    fn with_no_profile_the_layers_are_the_usual_ones() {
        let e = env(&[("NORTE_CONFIG_DIR", "/custom")]);
        let with = standard_layers_with_profile_on(false, &e, None);
        let without = crate::dirs::standard_layers_on(false, &e);
        assert_eq!(with.dirs, without.dirs);
    }

    /// The profile goes AFTER user and BEFORE project, with all three layers
    /// present (the hermetic case above has no `System`).
    #[test]
    fn the_profile_is_spliced_between_user_and_project() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("photos")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![Layer::System, Layer::User, Layer::Profile, Layer::Project]
        );
    }

    /// A consumer of core values (`[archive]`, `[ai]`) does not see the
    /// profile layer: it cannot set anything of its own (D2) and has no
    /// reason to know which profile a frontend chose.
    #[test]
    fn no_project_does_not_bring_a_profile_either() {
        let e = env(&[("HOME", "/home/u")]);
        let l = standard_layers_no_project_on(false, &e, Some(OsStr::new("work")));
        let kinds: Vec<Layer> = l.dirs.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, vec![Layer::System, Layer::User]);
    }

    /// With no USER layer there is nowhere to hang a profile, and instead of
    /// inventing a path the layers are returned as is: asking for a profile
    /// that cannot exist cannot manufacture a directory under the cwd.
    #[test]
    fn with_no_user_layer_the_profile_is_not_invented() {
        let e = env(&[]);
        let l = standard_layers_with_profile_on(false, &e, Some(OsStr::new("work")));
        assert!(l.dirs.iter().all(|(_, k)| *k != Layer::Profile));
    }
}
