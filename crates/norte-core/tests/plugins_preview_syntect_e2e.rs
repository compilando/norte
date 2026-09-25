//! E2E of the syntect previewer (#29): compiles the REAL guest
//! `norte-plugin-host/examples-wasm/previewer-syntect` to `wasm32-wasip2`,
//! seeds it under consent, resolves it for a code mimetype and RUNS it —
//! verifying it returns the highlighted syntax as 24-bit ANSI (which the
//! frontend sanitizes to pane color, see `norte-frontend::ansi`).
//!
//! SKIP if the `wasm32-wasip2` target is not installed (same as the
//! previewer-demo E2E): the suite stays green on toolchains without that
//! target.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// The plugin's REAL manifest, read from its directory — not a copy.
///
/// It used to be a literal here, and that made this test check what a
/// manifest WOULD have said instead of what the plugin ships. Now
/// `plugin.toml` is the source and the test reads it: if the installed
/// manifest stops declaring `application/json` or loses `fs-read`, this E2E
/// falls over, which is exactly what should happen.
fn manifest() -> String {
    let p = guest_dir("previewer-syntect").join("plugin.toml");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
}

/// The guest's directory, from THIS crate's manifest.
fn guest_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../norte-plugin-host/examples-wasm")
        .join(name)
}

/// Sample JSON: syntect has a `JSON` syntax, so the highlighting is
/// deterministic (keys/values in different colors).
const SAMPLE: &[u8] = b"{\n  \"name\": \"norte\",\n  \"count\": 42\n}\n";

#[test]
fn plugin_preview_syntect_e2e_real_wasm() {
    let Some(wasm) = build_guest("previewer-syntect") else {
        eprintln!("SKIP: target wasm32-wasip2 not installed; no .wasm to run");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.syntect");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), manifest()).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // Fail-closed: without approval it is not chosen even with the .wasm
    // present and the mime matching.
    assert!(
        reg.resolve_previewer("application/json").is_none(),
        "an unconsented previewer is never chosen"
    );

    assert!(reg.set_approval_in_memory("org.norte.syntect", true));
    assert!(reg.set_enabled_in_memory("org.norte.syntect", true));

    let (id, _name, resolved_wasm, caps, _settings) = reg
        .resolve_previewer("application/json")
        .expect("application/json matches the consented previewer's glob");
    assert_eq!(id, "org.norte.syntect");

    // RUNS the real WASM: the core would pass the already-decoded TEXT
    // (§6.2); the guest returns the highlighted syntax as 24-bit ANSI.
    let render = rt
        .instantiate(&resolved_wasm, caps)
        .expect("instantiate the previewer")
        .render_preview("application/json", SAMPLE)
        .expect("previewer-syntect must render");

    // REAL color: at least one 24-bit SGR sequence (`ESC[38;2;r;g;bm`).
    assert!(
        render.contains("\x1b[38;2;"),
        "the render carries 24-bit ANSI color (syntect): {render:?}"
    );
    // The content survives: the JSON key appears in the highlighted text.
    assert!(
        render.contains("name"),
        "the render includes the content's text: {render:?}"
    );
    assert!(
        render.contains("42"),
        "the render includes the numeric value: {render:?}"
    );

    // The STYLED twin is what the viewer calls (#373). It used to wrap
    // `render`'s ANSI in plain spans, and the viewer printed the escapes as
    // text: the colour has to travel in `fg`, never inside `text`.
    let (_, _, wasm, caps, _) = reg
        .resolve_previewer("application/json")
        .expect("still consented");
    let lines = rt
        .instantiate(&wasm, caps)
        .expect("instantiate the previewer")
        .render_styled_preview("application/json", SAMPLE, None)
        .expect("previewer-syntect must render styled");
    let spans: Vec<_> = lines.iter().flatten().collect();
    assert!(
        spans.iter().all(|s| !s.text.contains('\x1b')),
        "no span carries an escape sequence: {lines:?}"
    );
    assert!(
        spans.iter().any(|s| s.fg.is_some()),
        "the highlighting travels in fg: {lines:?}"
    );
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    assert!(
        text.contains("name") && text.contains("42"),
        "the content survives: {text:?}"
    );
}

/// Compiles `examples-wasm/<name>/` to `wasm32-wasip2` (release). `None`
/// (SKIP) if the target is not installed; if it is but it does not build, it
/// is a real failure.
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("could not launch cargo to compile the guest");
    assert!(status.success(), "the {name} guest did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "{} not found", wasm.display());
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
