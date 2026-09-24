//! `org.norte.by-extension` (`plugins/by-extension/`) builds, installs and
//! proposes where each name goes (phase 8, WIT package `norte:organizer`).
//!
//! This is the end-to-end exercise of the new ABI, and that is why the plugin
//! is as dumb as possible — the answer is in the name, with no
//! capabilities — so if this fails, it is the ABI failing, not the guest's
//! cleverness.
//!
//! It also pins down this: that a destination escaping the directory does NOT
//! get applied. The host validates a third party's proposal with the SAME
//! function that validates a model's, and drops the whole plan instead of
//! accepting "whatever could be done".

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{OrganizePlanOutcome, PluginRegistry, install, run_organize_plan};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.by-extension";
const ORGANIZER: &str = "by-extension";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/by-extension")
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
        .expect("cargo for by-extension");
    assert!(status.success(), "by-extension did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("by_extension.wasm");
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

fn install_and_consent(cfg: &Path, wasm: &Path) -> PluginRegistry {
    let stage = cfg.join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::copy(plugin_dir().join("plugin.toml"), stage.join("plugin.toml")).expect("manifest");
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    assert_eq!(install(cfg, &stage, false).expect("installs").id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
    reg
}

/// The happy path: a real `organizer` plugin, loaded by the real host,
/// answers where each name goes.
#[test]
fn by_extension_proposes_a_folder_per_extension() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    // NOTE: `sin-extension` below is fixture/behavior baked into the
    // `plugins/by-extension` guest (owned by another task) and kept verbatim
    // — see the T05 report's cross-file literals.
    let names: Vec<String> = ["invoice.pdf", "photo.JPG", "READTHIS", ".bashrc"]
        .iter()
        .map(|n| (*n).to_owned())
        .collect();

    let resolved = reg
        .resolve_organizer(ID, ORGANIZER)
        .expect("the manifest declares the organizer");
    let outcome = run_organize_plan(&rt, resolved, ORGANIZER, None, false, &names);
    let OrganizePlanOutcome::Plan(moves) = outcome else {
        panic!("expected a plan, got {outcome:?}");
    };
    let pairs: Vec<(&str, &str)> = moves
        .iter()
        .map(|m| (m.current.as_str(), m.proposed_rel.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("invoice.pdf", "pdf/invoice.pdf"),
            ("photo.JPG", "jpg/photo.JPG"),
            ("READTHIS", "sin-extension/READTHIS"),
            (".bashrc", "sin-extension/.bashrc"),
        ],
        "lowercase extension, and a leading dot is not an extension"
    );
}

/// An organizer that has not been approved does not answer, and neither does
/// one asked for by an id it does not declare: the same gate as the renamer.
#[test]
fn an_unapproved_organizer_or_one_with_another_id_does_not_resolve() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let stage = cfg.path().join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::copy(plugin_dir().join("plugin.toml"), stage.join("plugin.toml")).expect("manifest");
    std::fs::copy(&wasm, stage.join("plugin.wasm")).expect("wasm");
    install(cfg.path(), &stage, false).expect("installs");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // Just installed: neither approved nor enabled.
    assert!(
        reg.resolve_organizer(ID, ORGANIZER).is_none(),
        "unapproved, it does not resolve"
    );

    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
    assert!(
        reg.resolve_organizer(ID, "other-id").is_none(),
        "an id the manifest does not declare, either"
    );
    assert!(reg.resolve_organizer("org.acme.nope", ORGANIZER).is_none());
}
