//! The plugin template (`plugins/template/`) builds, installs and runs — so
//! the thing a third party copies cannot rot. Its manifest is read from the
//! directory, never duplicated here.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.example.template";

fn template_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/template")
}

/// Builds the template to `wasm32-wasip2`, or `None` without the target.
fn build_template() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(template_dir())
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo for the template");
    assert!(status.success(), "the template did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("norte_plugin_template.wasm");
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

/// Stages the template the way its README says, installs it with the real
/// installer, and consents — what a person does in the manager.
fn install_and_consent(cfg: &Path, wasm: &Path) {
    let stage = cfg.join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    for name in ["plugin.toml", "help.md"] {
        std::fs::copy(template_dir().join(name), stage.join(name)).expect(name);
    }
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    let rep = install(cfg, &stage, false).expect("installs");
    assert_eq!(rep.id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
}

#[test]
fn the_template_builds_installs_and_runs() {
    let Some(wasm) = build_template() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let reg = PluginRegistry::discover(cfg.path()).expect("discover");
    // The command, with the default greeting from `[config.greeting]`.
    let out = reg
        .run_command(&rt, ID, "hello", "norte")
        .expect("hello runs");
    assert_eq!(out, "hello, norte");
    // An unknown command is the guest's error, not a crash.
    assert!(reg.run_command(&rt, ID, "nope", "").is_err());
    // The previewer is found for what it declares, and ships its help page.
    let (id, _, _, _, _) = reg.resolve_previewer("text/plain").expect("previewer");
    assert_eq!(id, ID);
    let info = reg
        .list()
        .plugins
        .into_iter()
        .find(|p| p.id == ID)
        .expect("listed");
    assert!(info.has_help, "help.md travels with the plugin");
    assert!(info.commands.iter().any(|c| c.id == "hello"));

    // A setting written the way the host writes it changes the greeting.
    std::fs::write(
        cfg.path().join("plugins").join(ID).join("config.toml"),
        "greeting = \"hola\"\n",
    )
    .expect("config.toml");
    let reg = PluginRegistry::discover(cfg.path()).expect("rediscover");
    let out = reg
        .run_command(&rt, ID, "hello", "norte")
        .expect("hello runs with the setting");
    assert_eq!(out, "hola, norte");
}
