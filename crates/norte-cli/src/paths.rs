//! `norte paths`: where every file norte reads or writes actually lives.
//!
//! `norte doctor` already validates those files, but it prints the path of
//! exactly one of them. Everything else — `connections.toml`, the keymap, the
//! encrypted secrets, the journal, the index — the reader has to know by
//! folklore, and the folklore is wrong the moment `NORTE_CONFIG_DIR` or
//! `XDG_CONFIG_HOME` is set. This module answers the question directly.
//!
//! It is deliberately SIDE-EFFECT-FREE, same policy as `doctor`: it stats, it
//! never creates. A path that does not exist is reported as missing, which is
//! information — half of these files are optional, and "not there" is the
//! answer to "why is norte ignoring it".

use std::path::{Path, PathBuf};

use norte_config::dirs::{Layer, Layers};

/// One path norte uses, with where it comes from and whether it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Stable machine id (`connections`, `journal`, `logs`…). Never
    /// translated: `--json` consumers key off this.
    pub id: &'static str,
    /// The resolved path.
    pub path: PathBuf,
    /// For a config-layer directory, which layer it is. `None` for everything
    /// resolved from the single config dir, and for state/runtime paths.
    pub layer: Option<Layer>,
    /// Whether it exists right now (file OR directory — the caller does not
    /// need to care which, and several of these are directories).
    pub exists: bool,
}

/// The files and directories that live under the single resolved config dir
/// (ADR 0035), in the order a reader looks for them: what they edit first,
/// then what norte keeps for itself.
///
/// The four `*.toml` are the config surfaces `doctor` validates; `secrets.age`
/// and `secrets.key` are the store from ADR 0015 C; the rest is state that
/// norte writes and a human normally does not touch.
const IN_CONFIG_DIR: &[(&str, &str)] = &[
    ("config", "norte.toml"),
    ("keymap", "keymap.toml"),
    ("connections", "connections.toml"),
    ("policy", "policy.toml"),
    ("secrets", "secrets.age"),
    ("secrets-key", "secrets.key"),
    ("profiles", "profiles"),
    ("plugins", "plugins"),
    ("plugins-state", "plugins-state.toml"),
    ("journal", "journal.db"),
    ("index", "index.db"),
    ("sync-spools", "sync-spools"),
];

/// Every path norte uses, in reading order: the config layers, then the
/// resolved config dir and what hangs off it, then state and runtime.
///
/// Everything is passed IN rather than resolved here, for the same reason
/// `doctor`'s checks take an `env` closure: the resolution rules live in
/// `norte-config` and the daemon client, and a diagnostic that re-derived them
/// could disagree with the code it is meant to explain — which is exactly the
/// class of bug #320 was.
///
/// `log_dir` is the EFFECTIVE one (`[log] dir` honoured), not the default;
/// pointing at the default while the real log is elsewhere is worse than
/// saying nothing.
#[must_use]
pub fn collect(
    layers: &Layers,
    config_dir: &Path,
    state_dir: Option<&Path>,
    log_dir: Option<&Path>,
    socket: &Path,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut push = |id, path: PathBuf, layer| {
        let exists = path.exists();
        out.push(Entry {
            id,
            path,
            layer,
            exists,
        });
    };

    // Config layers, in ASCENDING precedence — the order `Layers` already
    // carries, and the order that answers "which one wins".
    for (dir, kind) in &layers.dirs {
        push("layer", dir.clone(), Some(*kind));
    }

    push("config-dir", config_dir.to_path_buf(), None);
    for (id, name) in IN_CONFIG_DIR {
        push(id, config_dir.join(name), None);
    }

    if let Some(d) = state_dir {
        push("state-dir", d.to_path_buf(), None);
    }
    if let Some(d) = log_dir {
        push("logs", d.to_path_buf(), None);
    }
    push("daemon-socket", socket.to_path_buf(), None);

    out
}

/// The machine name of a layer, for `--json` and for the text column.
/// Deliberately not `Debug`: this is a wire-ish vocabulary and a derived
/// `Debug` would rename it the day someone renames the variant.
#[must_use]
pub fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::System => "system",
        Layer::User => "user",
        Layer::Profile => "profile",
        Layer::Project => "project",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers_from(dirs: &[(&str, Layer)]) -> Layers {
        Layers {
            dirs: dirs.iter().map(|(p, k)| (PathBuf::from(p), *k)).collect(),
        }
    }

    /// The whole point: every config surface `doctor` knows how to validate is
    /// named here with its path. A file that norte reads but this command
    /// cannot locate leaves the reader exactly where they were.
    #[test]
    fn names_every_config_surface() {
        let dir = tempfile::tempdir().unwrap();
        let entries = collect(
            &layers_from(&[("/etc/norte", Layer::System)]),
            dir.path(),
            None,
            None,
            Path::new("/run/user/1000/norte/daemon.sock"),
        );
        for id in [
            "config",
            "keymap",
            "connections",
            "policy",
            "secrets",
            "profiles",
            "plugins",
            "journal",
            "index",
        ] {
            let e = entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing path «{id}»: {entries:?}"));
            assert!(
                e.path.starts_with(dir.path()),
                "«{id}» does not hang off the config dir: {:?}",
                e.path
            );
        }
    }

    /// `exists` is a checked fact, not an assumption: it is half the
    /// answer to "why is norte ignoring my file".
    #[test]
    fn exists_distinguishes_what_is_there_from_what_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("connections.toml"), "").unwrap();
        let entries = collect(
            &layers_from(&[]),
            dir.path(),
            None,
            None,
            Path::new("/tmp/x.sock"),
        );
        let exists_of = |id: &str| entries.iter().find(|e| e.id == id).unwrap().exists;
        assert!(exists_of("connections"), "the written file must show up");
        assert!(!exists_of("policy"), "the absent one must not show up");
    }

    /// Layers come out in the SAME order and with the SAME label
    /// `norte-config` gives: precedence is the question that brings a
    /// reader with two `norte.toml`s here, and renumbering it while
    /// painting would answer wrong.
    #[test]
    fn layers_keep_their_order_and_label() {
        let dir = tempfile::tempdir().unwrap();
        let entries = collect(
            &layers_from(&[
                ("/etc/norte", Layer::System),
                ("/home/x/.config/norte", Layer::User),
                (".norte", Layer::Project),
            ]),
            dir.path(),
            None,
            None,
            Path::new("/tmp/x.sock"),
        );
        let layers: Vec<_> = entries
            .iter()
            .filter(|e| e.id == "layer")
            .map(|e| layer_name(e.layer.expect("a layer carries its class")))
            .collect();
        assert_eq!(layers, ["system", "user", "project"]);
    }

    /// An absent `state_dir`/`log_dir` does not invent a row: saying
    /// "there is none" is different from pointing at a place where there
    /// is nothing.
    #[test]
    fn without_state_or_log_there_are_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let entries = collect(
            &layers_from(&[]),
            dir.path(),
            None,
            None,
            Path::new("/tmp/x.sock"),
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.id == "state-dir" || e.id == "logs")
        );
        let with = collect(
            &layers_from(&[]),
            dir.path(),
            Some(Path::new("/var/lib/norte")),
            Some(Path::new("/var/log/norte")),
            Path::new("/tmp/x.sock"),
        );
        assert!(with.iter().any(|e| e.id == "state-dir"));
        assert!(with.iter().any(|e| e.id == "logs"));
    }
}
