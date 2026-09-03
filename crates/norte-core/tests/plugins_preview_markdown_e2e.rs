//! `org.norte.markdown` (`plugins/markdown/`) builds, installs, is chosen for
//! `text/markdown` and renders headings, code and links as styled lines.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{install, PluginRegistry};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.markdown";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/markdown")
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
        .expect("cargo for markdown");
    assert!(status.success(), "markdown did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("markdown_preview.wasm");
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

#[test]
fn markdown_is_chosen_for_its_type_and_renders_styled_lines() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let (id, _, wasm_path, caps, _) = reg
        .resolve_previewer("text/markdown")
        .expect("declares text/markdown");
    assert_eq!(id, ID);
    assert!(
        reg.resolve_previewer("text/plain").is_none(),
        "and nothing else: an exact type, not a glob"
    );

    let doc = b"# Title\n\nSome *text* and `code`.\n\n```rust\nfn a() {}\n```\n\n- one\n- [norte](https://x.y)\n";
    let mut inst = rt.instantiate(&wasm_path, caps).expect("instantiates");
    let styled = inst
        .render_styled_preview("text/markdown", doc)
        .expect("renders");
    let text = |line: &Vec<norte_plugin_host::previewer_iface::Span>| {
        line.iter().map(|s| s.text.as_str()).collect::<String>()
    };
    assert_eq!(text(&styled[0]), "Title");
    assert!(
        styled[0].iter().all(|s| s.role.as_deref() == Some("title")),
        "{:?}",
        styled[0]
    );
    let code = styled
        .iter()
        .find(|l| text(l) == "fn a() {}")
        .expect("the fenced line, without its fence");
    assert_eq!(code[0].role.as_deref(), Some("info"));
    assert!(!styled.iter().any(|l| text(l).contains("```")));
    let link = styled
        .iter()
        .find(|l| text(l).contains("norte"))
        .expect("the link line");
    assert_eq!(text(link), "• norte (https://x.y)");
    let emph = styled
        .iter()
        .flatten()
        .find(|s| s.text == "text")
        .expect("the emphasised word");
    assert!(emph.fg.is_some(), "emphasis carries a colour");

    // The plain twin is the same text, flattened.
    let plain = inst
        .render_preview("text/markdown", doc)
        .expect("renders plain");
    assert!(plain.starts_with("Title\n"));
    assert!(plain.contains("• norte (https://x.y)"));
}
