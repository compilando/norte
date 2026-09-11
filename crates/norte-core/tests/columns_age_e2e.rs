//! `org.norte.age` (`plugins/age/`) builds, installs and answers its column
//! from `stat` alone under the location token, against the WASI wall clock
//! the host links for every guest: a fresh file is «today», an old one is
//! the last bucket with a figure in years.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;
use norte_proto::VPath;

const ID: &str = "org.norte.age";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/age")
}

fn build_plugin() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
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
        .expect("cargo for age");
    assert!(status.success(), "age did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("age.wasm");
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
        .resolve_columns_of(Some(ID), "age")
        .expect("the manifest declares the column");
    norte_core::plugins::run_column_values_for_test(
        rt,
        (id, name, wasm, caps, settings),
        "age",
        dir,
        false,
        names,
        names.len(),
    )
}

#[test]
fn age_buckets_a_fresh_file_as_today_and_an_old_one_as_older() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("fresh.txt"), b"now").expect("fresh");
    let old = dir.path().join("old.txt");
    std::fs::write(&old, b"then").expect("old");
    // Two years back, well past the last default edge (30 days).
    let two_years = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 365 * 86_400);
    let f = std::fs::File::options()
        .write(true)
        .open(&old)
        .expect("reopen");
    f.set_modified(two_years).expect("set mtime");
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    let names: Vec<Vec<u8>> = ["fresh.txt", "old.txt", "sub", "missing"]
        .iter()
        .map(|n| n.as_bytes().to_vec())
        .collect();
    let loc = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");

    let got = cells(&reg, &rt, Some(&loc), &names);
    assert_eq!(
        got[0].as_deref(),
        Some("● now"),
        "written this second: today"
    );
    assert_eq!(
        got[1].as_deref(),
        Some("· 2y"),
        "two years back: the last bucket"
    );
    assert!(
        got[2].as_deref().is_some_and(|c| c.starts_with("● ")),
        "a directory has an mtime too: {:?}",
        got[2]
    );
    assert_eq!(got[3], None, "an entry the host cannot stat stays empty");
    assert_eq!(
        cells(&reg, &rt, None, &names),
        vec![None; 4],
        "without a location every cell is empty"
    );
}
