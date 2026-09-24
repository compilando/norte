//! `org.norte.file-icons` (`plugins/file-icons/`) builds, installs and
//! decorates — through the real guest, over the hostile-name corpus, and with
//! its one setting switched.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.file-icons";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/file-icons")
}

fn build_plugin() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(plugin_dir())
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo for file-icons");
    assert!(status.success(), "file-icons did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("file_icons.wasm");
    assert!(wasm.exists(), "missing {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

fn install_and_consent(cfg: &Path, wasm: &Path) {
    let stage = cfg.join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    for name in ["plugin.toml", "help.md"] {
        std::fs::copy(plugin_dir().join(name), stage.join(name)).expect(name);
    }
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    assert_eq!(install(cfg, &stage, false).expect("installs").id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
}

/// Decorates `names` the way `Backend::plugin_decorate`'s embedded arm does:
/// the consented decorators, instantiated with their manifest capabilities
/// and their resolved settings.
fn decorate(cfg: &Path, rt: &PluginRuntime, names: &[&[u8]]) -> Vec<Option<String>> {
    use norte_plugin_host::decorator_iface::EntryKind;
    let entries: Vec<(&[u8], EntryKind)> = names.iter().map(|n| (*n, EntryKind::File)).collect();
    decorate_kinds(cfg, rt, &entries)
}

fn decorate_kinds(
    cfg: &Path,
    rt: &PluginRuntime,
    entries: &[(&[u8], norte_plugin_host::decorator_iface::EntryKind)],
) -> Vec<Option<String>> {
    use norte_plugin_host::decorator_iface::Entry;
    let reg = PluginRegistry::discover(cfg).expect("discover");
    let decorators = reg.resolve_decorators();
    assert_eq!(decorators.len(), 1, "one consented decorator");
    let ((id, _, wasm, caps, settings), slot) = decorators.into_iter().next().expect("it");
    assert_eq!(id, ID);
    // The manifest says where its glyphs go (ADR 0105): the icon column.
    assert_eq!(slot, norte_plugin_host::DecoratorSlot::Icon);
    let mut inst = rt.instantiate_decorator(&wasm, caps).expect("instantiates");
    inst.set_settings(settings);
    let entries: Vec<Entry> = entries
        .iter()
        .map(|(n, k)| Entry {
            name: n.to_vec(),
            kind: *k,
        })
        .collect();
    let out = inst.decorate(&entries).expect("decorates");
    assert_eq!(out.len(), entries.len(), "positional 1:1");
    out.into_iter().map(|d| d.badge).collect()
}

/// A folder gets the folder icon whatever it is called, a link the link
/// icon, and the host is the one who says which is which (WIT 0.10.0).
#[test]
fn folders_and_links_get_their_icon_from_the_kind() {
    use norte_plugin_host::decorator_iface::EntryKind;
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");
    let icons = decorate_kinds(
        cfg.path(),
        &rt,
        &[
            (b"bcds", EntryKind::Dir),
            (b"main.rs", EntryKind::Dir),
            (b".git", EntryKind::Dir),
            (b"link", EntryKind::Symlink),
            (b"accounts.xlsx", EntryKind::File),
            (b"x", EntryKind::Other),
        ],
    );
    assert_eq!(
        icons,
        vec![
            Some("📁".to_owned()),
            Some("📁".to_owned()),
            Some("🐙".to_owned()),
            Some("🔗".to_owned()),
            Some("📊".to_owned()),
            None,
        ]
    );
}

#[test]
fn file_icons_badges_by_name_and_switches_style() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let names: [&[u8]; 6] = [
        b"main.rs",
        b"README.md",
        b"song.mp3",
        b".bashrc",
        b"x",
        b"caf\xff.md",
    ];
    let badges = decorate(cfg.path(), &rt, &names);
    assert_eq!(
        badges,
        vec![
            Some("🦀".to_owned()),
            Some("📖".to_owned()),
            Some("🎵".to_owned()),
            None,
            None,
            Some("📄".to_owned()),
        ]
    );

    // Every hostile name of the corpus: a badge or nothing, never anything
    // longer than the host's cap or carrying a control character, and the
    // answer stays 1:1 — a name that is bytes is still a name.
    let corpus = norte_testkit::corpus::hostile_names();
    let raw: Vec<&[u8]> = corpus.iter().map(|h| h.bytes.as_slice()).collect();
    let badges = decorate(cfg.path(), &rt, &raw);
    for (h, b) in corpus.iter().zip(&badges) {
        if let Some(b) = b {
            assert!(b.chars().count() <= 8, "{}: {b:?}", h.id);
            assert!(!b.chars().any(char::is_control), "{}: {b:?}", h.id);
        }
    }

    // The setting, written the way the host writes it.
    std::fs::write(
        cfg.path().join("plugins").join(ID).join("config.toml"),
        "style = \"ascii\"\n",
    )
    .expect("config.toml");
    let badges = decorate(cfg.path(), &rt, &[b"main.rs", b"song.mp3"]);
    assert_eq!(badges, vec![Some("{}".to_owned()), Some("~".to_owned())]);
}
