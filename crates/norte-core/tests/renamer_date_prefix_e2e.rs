//! `org.norte.date-prefix` (`plugins/date-prefix/`) builds, installs and
//! proposes `YYYY-MM-DD_name` from each file's modification time, read under
//! the location token (C3, ADR 0095). The plan that comes out is the same
//! shape the AI plan has, so both frontends review it with the pipeline they
//! already have.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use norte_core::plugins::{PluginRegistry, RenamePlanOutcome, install, run_rename_plan};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.date-prefix";
const RENAMER: &str = "by-date";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/date-prefix")
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
        .expect("cargo for date-prefix");
    assert!(status.success(), "date-prefix did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("date_prefix.wasm");
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

/// A file whose mtime is `secs` after the epoch.
fn touch_at(dir: &Path, name: &str, secs: u64) {
    let p = dir.join(name);
    std::fs::write(&p, b"x").expect("write");
    let f = std::fs::File::options().write(true).open(&p).expect("open");
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .expect("set mtime");
}

#[test]
fn date_prefix_proposes_from_mtime_and_skips_what_is_already_dated() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let dir = tempfile::tempdir().expect("files dir");
    touch_at(dir.path(), "foto.jpg", 1_756_857_600); // 2025-09-03
    touch_at(dir.path(), "2024-01-01_done.txt", 1_756_857_600);
    touch_at(dir.path(), "old.txt", 0); // 1970-01-01
    let names: Vec<String> = ["foto.jpg", "2024-01-01_done.txt", "old.txt", "missing.txt"]
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let loc = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");

    let resolved = reg
        .resolve_renamer(ID, RENAMER)
        .expect("the manifest declares the renamer");
    let outcome = run_rename_plan(&rt, resolved, RENAMER, Some(&loc), false, &names);
    let RenamePlanOutcome::Plan(entries) = outcome else {
        panic!("expected a plan, got {outcome:?}");
    };
    let pairs: Vec<(&str, &str)> = entries
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("foto.jpg", "2025-09-03_foto.jpg"),
            ("old.txt", "1970-01-01_old.txt"),
        ],
        "dated names are left alone and a name that does not exist gets no proposal"
    );

    // Without a location the plugin has no dates to read, and it says so
    // rather than guessing: the frontend shows the sentence.
    let resolved = reg.resolve_renamer(ID, RENAMER).expect("resolves again");
    let outcome = run_rename_plan(&rt, resolved, RENAMER, None, false, &names);
    assert!(
        matches!(outcome, RenamePlanOutcome::Refused(ref why) if why.contains("location")),
        "expected a refusal naming the capability, got {outcome:?}"
    );

    assert!(
        reg.resolve_renamer(ID, "nope").is_none(),
        "a renamer the manifest does not declare is not the plugin's to answer"
    );
}
