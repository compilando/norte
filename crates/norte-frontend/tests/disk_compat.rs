//! The frontend's half of `tests/disk-compat/` (see
//! `crates/norte-cli/tests/disk_compat.rs` for the corpus and the other
//! half): the files `norte doctor` never opens — openers, shells, user
//! commands, themes and profiles — loaded by the real loaders.
//!
//! The themes are the sharp one: a theme that stops parsing is DROPPED from
//! the list without a word, so the check is that every file comes back.

use std::path::{Path, PathBuf};

use norte_config::{Layer, Layers, ProfileSource};

fn corpora() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/disk-compat");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("tests/disk-compat exists")
        .map(|e| e.expect("readable entry").path().join("config"))
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no corpus under {}", root.display());
    found
}

fn layers(config: &Path) -> Layers {
    Layers {
        dirs: vec![(config.to_path_buf(), Layer::User)],
    }
}

fn stems(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| p.file_stem().expect("stem").to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn every_release_config_dir_loads_whole() {
    for config in corpora() {
        let cfg = norte_frontend::config::load(&layers(&config))
            .unwrap_or_else(|e| panic!("{}: {e}", config.display()));
        let themes: Vec<String> = cfg.user_themes.iter().map(|t| t.name.clone()).collect();
        assert_eq!(
            themes,
            stems(&config.join("themes")),
            "{}: a theme was dropped",
            config.display()
        );
        for file in ["openers.toml", "terminal.toml", "commands.toml"] {
            if config.join(file).is_file() {
                assert!(
                    cfg.common.sources.contains(&config.join(file)),
                    "{}: {file} was not read",
                    config.display()
                );
            }
        }
    }
}

/// Every command name a release shipped still binds: it is in today's
/// catalogue or in `RENAMED` (ADR 0182). A user's `keymap.toml` names
/// them, so renaming one without its alias breaks that file — here, not in
/// their home.
///
/// `expected/commands.txt` is written by the freeze from the TAG's
/// catalogue source (`scripts/release/freeze-disk-compat.sh`), never from
/// this tree's: a rename made after the tag would otherwise be frozen
/// under its new name, and the old one would go unprotected.
#[test]
fn every_released_command_name_still_binds() {
    use norte_frontend::keymap::catalogue::{lookup, renamed_to};
    for config in corpora() {
        let list = config.with_file_name("expected").join("commands.txt");
        let frozen =
            std::fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e}", list.display()));
        for name in frozen.lines().filter(|l| !l.is_empty()) {
            assert!(
                lookup(name).is_some() || renamed_to(name).and_then(lookup).is_some(),
                "{}: `{name}` was shipped and no longer binds: add it to RENAMED",
                list.display()
            );
        }
    }
}

#[test]
fn every_release_profile_loads_without_degrading() {
    for config in corpora() {
        // The production splice, fed an environment that names only the
        // corpus: the profile lands where a real `--profile` puts it.
        let env = |k: &str| (k == "NORTE_CONFIG_DIR").then(|| config.clone().into_os_string());
        let with_profile = |name: Option<&std::ffi::OsStr>| {
            norte_config::profiles::standard_layers_with_profile_on(false, &env, name)
        };
        for name in std::fs::read_dir(config.join("profiles"))
            .into_iter()
            .flatten()
            .map(|e| e.expect("entry").file_name())
        {
            let loaded = norte_frontend::config::load_with_profile(
                &with_profile,
                Some(&name),
                ProfileSource::Explicit,
            )
            .unwrap_or_else(|e| panic!("{}: {name:?}: {e}", config.display()));
            assert_eq!(loaded.degraded, None, "{}: {name:?}", config.display());
            assert_eq!(loaded.active.as_deref(), Some(name.as_os_str()));
        }
    }
}
