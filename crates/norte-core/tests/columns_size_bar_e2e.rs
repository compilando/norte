//! `org.norte.size-bar` (`plugins/size-bar/`) builds, installs and answers
//! its column from `stat` alone under the location token: a bar per file,
//! nothing for a directory, and the biggest file on the page fills it.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;
use norte_proto::VPath;

const ID: &str = "org.norte.size-bar";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/size-bar")
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
        .expect("cargo for size-bar");
    assert!(status.success(), "size-bar did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("size_bar.wasm");
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
    for name in ["plugin.toml", "help.md"] {
        std::fs::copy(plugin_dir().join(name), stage.join(name)).expect(name);
    }
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    assert_eq!(install(cfg, &stage, false).expect("installs").id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
    reg
}

fn cells(
    reg: &PluginRegistry,
    rt: &PluginRuntime,
    dir: Option<&VPath>,
    names: &[Vec<u8>],
) -> Vec<Option<String>> {
    let (id, name, wasm, caps, settings) = reg
        .resolve_columns_of(Some(ID), "bar")
        .expect("the manifest declares the column");
    norte_core::plugins::run_column_values_for_test(
        rt,
        (id, name, wasm, caps, settings),
        "bar",
        dir,
        false,
        names,
        names.len(),
    )
}

#[test]
fn size_bar_draws_a_bar_per_file_and_nothing_for_a_directory() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("big.bin"), vec![0u8; 100_000]).expect("big");
    std::fs::write(dir.path().join("small.txt"), b"hi").expect("small");
    std::fs::write(dir.path().join("empty"), b"").expect("empty");
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    let names: Vec<Vec<u8>> = ["big.bin", "small.txt", "empty", "sub", "missing"]
        .iter()
        .map(|n| n.as_bytes().to_vec())
        .collect();
    let loc = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");

    let got = cells(&reg, &rt, Some(&loc), &names);
    // Defaults: log scale, five cells, the page's biggest file fills it.
    assert_eq!(
        got[0].as_deref(),
        Some("█████"),
        "the biggest file fills the bar"
    );
    let small = got[1].as_deref().expect("a small file has a bar");
    assert_eq!(small.chars().count(), 5);
    assert!(
        small.starts_with('█') && small.ends_with('░'),
        "partial: {small}"
    );
    assert_eq!(
        got[2].as_deref(),
        Some("░░░░░"),
        "an empty file is an empty bar"
    );
    assert_eq!(got[3], None, "a directory has no bar");
    assert_eq!(got[4], None, "an entry the host cannot stat stays empty");
    assert_eq!(
        cells(&reg, &rt, None, &names),
        vec![None; 5],
        "without a location every cell is empty"
    );
}
