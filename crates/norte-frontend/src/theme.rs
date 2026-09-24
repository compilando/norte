//! Frontend-shared theme resolution (ADR 0020): a `[ui].theme` spec is a
//! bundled preset NAME or a PATH to a theme TOML. Each frontend then bridges
//! the resolved [`Theme`] to its renderer (ratatui in the TUI, GPUI in the
//! GUI).

use std::path::Path;

use norte_theme::Theme;

/// Typed error from [`resolve_theme`] (#73): the caller maps each variant to
/// a Fluent key for the bar — never the OS's `Display` (localized by the OS)
/// nor the parser's raw diagnostic nor `spec` (which can come from a FOREIGN
/// repo's `./.norte` layer) unsanitized. The thiserror `Display` is only for
/// logs/stderr.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The spec's path could not be read.
    #[error("theme {spec:?}: {source}")]
    Io {
        /// The `[ui].theme` spec as is (a path).
        spec: String,
        /// The cause.
        source: std::io::Error,
    },
    /// The theme's TOML (or the embedded preset) does not validate.
    #[error("theme {spec:?}: {detail}")]
    Parse {
        /// The `[ui].theme` spec as is (a name or a path).
        spec: String,
        /// `norte-theme`'s diagnostic.
        detail: String,
    },
}

/// Is this spec an EMBEDDED preset, i.e. does resolving it never touch disk?
///
/// Both frontends use it to split the path: a preset resolves in place — it
/// is arithmetic over colors, and sending it to another thread would add a
/// frame of delay to something the reader sees change under the cursor —
/// while a PATH is read, so it goes through `spawn_blocking` (rule 2). Without
/// this question, the two surfaces chose "presets only" and a `[ui] theme`
/// naming a file silently fell over on a profile switch.
///
/// `None` counts as a preset: it is the factory one, and it does not touch
/// disk either.
///
/// ```
/// use norte_frontend::theme::is_preset;
/// assert!(is_preset(None));
/// assert!(is_preset(Some("nord")));
/// assert!(!is_preset(Some("/home/u/.config/norte/mio.toml")));
/// ```
#[must_use]
pub fn is_preset(spec: Option<&str>) -> bool {
    match spec {
        None => true,
        Some(s) => matches!(Theme::preset(s), Ok(Some(_))),
    }
}

/// The protocol's `EntryKind` → the theme's `FileKind`.
///
/// The protocol does not yet distinguish executable/fifo/socket/device — the
/// `Entry` carries no mode — so anything that is neither a directory nor a
/// symlink falls to `Regular`; the color by EXTENSION still applies on top.
/// As a result, the `executable`, `fifo`, `socket`, `block-device` and
/// `char-device` keys the presets carry in `[files.kind]` are DORMANT: no
/// frontend can select them yet.
///
/// Lives here and not in each frontend because both need it and it is the
/// same decision (ADR 0077): written twice, it silently drifts — and the day
/// `Entry` carries a mode, one frontend would take advantage of it and the
/// other would not.
///
/// ```
/// use norte_frontend::theme::file_kind_of;
/// use norte_proto::EntryKind;
/// use norte_theme::FileKind;
/// assert_eq!(file_kind_of(EntryKind::Dir), FileKind::Dir);
/// assert_eq!(file_kind_of(EntryKind::Other), FileKind::Regular);
/// ```
#[must_use]
pub fn file_kind_of(kind: norte_proto::EntryKind) -> norte_theme::FileKind {
    use norte_proto::EntryKind;
    use norte_theme::FileKind;
    match kind {
        EntryKind::Dir => FileKind::Dir,
        EntryKind::Symlink => FileKind::Symlink,
        EntryKind::File | EntryKind::Other => FileKind::Regular,
    }
}

/// A USER theme: an already-parsed `<config>/themes/<name>.toml`.
///
/// Loaded with the config — in a context that can already touch disk — and
/// travels with it, so the selectors and the wizard can offer it and preview
/// it without reading a file inside a keystroke (rule 2).
#[derive(Debug, Clone)]
pub struct UserTheme {
    /// The name it is chosen by: the file's, without `.toml`.
    pub name: String,
    /// The theme, already validated.
    pub theme: Theme,
}

/// Is `s` valid as a user theme name?
///
/// A NAME, not a path: no separators nor `..`, not starting with a dot, and
/// in an alphabet that can be written as is in `[ui] theme`. Whatever does
/// not pass is treated as a path, which is what it was before names existed.
///
/// Public for `norte theme import`, which has to refuse writing a file the
/// resolver would never look up by name.
///
/// ```
/// use norte_frontend::theme::is_theme_name;
/// assert!(is_theme_name("one-dark-pro"));
/// assert!(!is_theme_name("../outside"));
/// assert!(!is_theme_name(".oculto"));
/// ```
#[must_use]
pub fn is_theme_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `<config_dir>/themes`.
fn themes_directory(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("themes")
}

/// The user's themes from `<config_dir>/themes/*.toml`, by name.
///
/// Does not fail: whatever does not work is skipped, because a list missing
/// one broken file is better than a selector that does not open. Skipped: a
/// file that does not parse, a name that is not a theme name, a name that is
/// not UTF-8 (it has to be writable in `[ui] theme`, which is text), and a
/// name that already belongs to a preset — the resolver puts presets first,
/// so that file would never be read, and listing it would offer a choice
/// that does not exist.
///
/// SYNC: reads disk. Called by the config load, which already runs where it
/// can.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// assert!(norte_frontend::theme::load_user_themes(dir.path()).is_empty());
/// ```
#[must_use]
pub fn load_user_themes(config_dir: &Path) -> Vec<UserTheme> {
    let Ok(entries) = std::fs::read_dir(themes_directory(config_dir)) else {
        return Vec::new();
    };
    let mut themes: Vec<UserTheme> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "toml") {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_owned();
            if !is_theme_name(&name) || matches!(Theme::preset(&name), Ok(Some(_))) {
                return None;
            }
            let raw = std::fs::read_to_string(&path).ok()?;
            let theme = Theme::from_toml(&raw).ok()?;
            Some(UserTheme { name, theme })
        })
        .collect();
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

/// The theme names offered: the embedded presets and, after them, the
/// user's.
///
/// ONE list for every surface — both selectors, both wizards, both settings
/// screens — which each used to build their own from `preset_names()`: six
/// places writing "which themes exist" is how a list silently drifts (ADR
/// 0077).
///
/// ```
/// let names = norte_frontend::theme::theme_names(&[]);
/// assert!(names.iter().any(|n| n == "nord"));
/// ```
#[must_use]
pub fn theme_names(user: &[UserTheme]) -> Vec<String> {
    norte_theme::preset_names()
        .into_iter()
        .map(String::from)
        .chain(user.iter().map(|t| t.name.clone()))
        .collect()
}

/// The theme named `name` WITHOUT touching disk: an embedded preset or an
/// already-loaded user one. This is what a live preview uses.
///
/// ```
/// let t = norte_frontend::theme::theme_by_name("nord", &[]).expect("preset");
/// assert_eq!(t.name.as_deref(), Some("nord"));
/// ```
#[must_use]
pub fn theme_by_name(name: &str, user: &[UserTheme]) -> Option<Theme> {
    if let Ok(Some(theme)) = Theme::preset(name) {
        return Some(theme);
    }
    user.iter()
        .find(|t| t.name == name)
        .map(|t| t.theme.clone())
}

/// Resolves the `[ui].theme` spec: an embedded preset name, the name of a
/// theme in `<config>/themes/`, or a path to a `.toml` theme file, in that
/// order. `None` = the default preset. SYNC (startup): wrap in
/// `spawn_blocking` from async contexts.
///
/// # Errors
/// [`ResolveError`] if the path cannot be read or the TOML does not
/// validate; the caller decides to degrade to the default and warn.
pub fn resolve_theme(spec: Option<&str>) -> Result<Theme, ResolveError> {
    resolve_theme_in(spec, norte_config::user_config_dir().as_deref())
}

/// [`resolve_theme`] against an explicit config directory, for tests and for
/// callers that already know it.
///
/// The order is fixed and load-bearing: an embedded preset first, so a stale
/// `themes/nord.toml` cannot change what `nord` means; then
/// `<config_dir>/themes/<name>.toml` when the spec is a plain name; then the
/// spec as a path.
///
/// # Errors
/// As [`resolve_theme`].
pub fn resolve_theme_in(
    spec: Option<&str>,
    config_dir: Option<&Path>,
) -> Result<Theme, ResolveError> {
    let Some(spec) = spec else {
        return Ok(Theme::preset_default());
    };
    let parse = |e: norte_theme::ThemeError| ResolveError::Parse {
        spec: spec.to_owned(),
        detail: e.to_string(),
    };
    if let Some(theme) = Theme::preset(spec).map_err(parse)? {
        return Ok(theme);
    }
    if let Some(dir) = config_dir
        && is_theme_name(spec)
    {
        let candidate = themes_directory(dir).join(format!("{spec}.toml"));
        match std::fs::read_to_string(&candidate) {
            Ok(raw) => return Theme::from_toml(&raw).map_err(parse),
            // No theme with that name: it may be a relative path.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ResolveError::Io {
                    spec: spec.to_owned(),
                    source: e,
                });
            }
        }
    }
    let path = Path::new(spec);
    let raw = std::fs::read_to_string(path).map_err(|e| ResolveError::Io {
        spec: spec.to_owned(),
        source: e,
    })?;
    Theme::from_toml(&raw).map_err(parse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_the_default_preset() {
        let t = resolve_theme(None).expect("default");
        assert_eq!(t.name.as_deref(), Theme::preset_default().name.as_deref());
    }

    #[test]
    fn a_preset_name_resolves() {
        let t = resolve_theme(Some("nord")).expect("embedded preset");
        assert_eq!(t.name.as_deref(), Some("nord"));
    }

    #[test]
    fn a_file_path_resolves_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mio.toml");
        std::fs::write(&p, "name = \"mio\"\n").unwrap();
        let t = resolve_theme(Some(p.to_str().unwrap())).expect("file");
        assert_eq!(t.name.as_deref(), Some("mio"));
        let missing = dir.path().join("no-existe.toml");
        assert!(matches!(
            resolve_theme(Some(missing.to_str().unwrap())),
            Err(ResolveError::Io { .. })
        ));
    }

    fn with_theme(dir: &Path, name: &str, toml: &str) {
        let themes = dir.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join(format!("{name}.toml")), toml).unwrap();
    }

    /// A name that is not a preset is looked up in
    /// `<config>/themes/<name>.toml` BEFORE being treated as a path.
    #[test]
    fn a_user_name_resolves_against_the_themes_directory() {
        let dir = tempfile::tempdir().unwrap();
        with_theme(dir.path(), "mio", "name = \"mio\"\n");
        let t = resolve_theme_in(Some("mio"), Some(dir.path())).expect("resolves");
        assert_eq!(t.name.as_deref(), Some("mio"));
    }

    /// And an EMBEDDED preset cannot be shadowed by a file.
    #[test]
    fn a_file_cannot_shadow_a_preset() {
        let dir = tempfile::tempdir().unwrap();
        with_theme(dir.path(), "nord", "name = \"impostor\"\n");
        let t = resolve_theme_in(Some("nord"), Some(dir.path())).expect("resolves");
        assert_eq!(t.name.as_deref(), Some("nord"), "the embedded preset wins");
        assert!(
            load_user_themes(dir.path()).is_empty(),
            "and it is not listed: it would be a choice that does not exist"
        );
    }

    /// The list: presets first, the user's after and by name, without the
    /// broken ones, the hidden ones, nor the ones that are not a name.
    #[test]
    fn user_themes_are_listed_after_the_presets() {
        let dir = tempfile::tempdir().unwrap();
        with_theme(dir.path(), "zeta", "name = \"zeta\"\n");
        with_theme(dir.path(), "mio", "name = \"mio\"\n");
        with_theme(dir.path(), "roto", "this is not toml {{{");
        with_theme(dir.path(), ".oculto", "name = \"oculto\"\n");
        std::fs::write(dir.path().join("themes/nota.txt"), "not a theme").unwrap();
        let user = load_user_themes(dir.path());
        let names: Vec<&str> = user.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["mio", "zeta"]);

        let all = theme_names(&user);
        assert!(all.iter().any(|n| n == "vscode-dark"));
        assert_eq!(&all[all.len() - 2..], ["mio", "zeta"]);
    }

    /// With no themes directory there is nothing, and it is not an error.
    #[test]
    fn with_no_themes_directory_the_list_is_the_presets_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_user_themes(dir.path()).is_empty());
        assert_eq!(theme_names(&[]).len(), norte_theme::preset_names().len());
    }

    /// The preview does not touch disk: a preset or an already-loaded theme,
    /// or nothing.
    #[test]
    fn theme_by_name_finds_presets_and_loaded_themes() {
        let user = vec![UserTheme {
            name: "mio".to_owned(),
            theme: Theme::preset_default(),
        }];
        assert!(theme_by_name("mio", &user).is_some());
        assert_eq!(
            theme_by_name("nord", &user).and_then(|t| t.name),
            Some("nord".to_owned())
        );
        assert!(theme_by_name("otro", &user).is_none());
    }

    /// A spec with a separator is not a name: it is not looked up in
    /// `themes/`, it is treated as a path.
    #[test]
    fn a_spec_with_a_separator_is_not_looked_up_as_a_user_theme() {
        let dir = tempfile::tempdir().unwrap();
        with_theme(dir.path(), "mio", "name = \"mio\"\n");
        assert!(matches!(
            resolve_theme_in(Some("themes/../mio"), Some(dir.path())),
            Err(ResolveError::Io { .. })
        ));
    }
}
