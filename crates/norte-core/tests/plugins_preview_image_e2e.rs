//! `org.norte.image-ansi` (`plugins/image-ansi/`) builds, installs, is chosen
//! for `image/png` and renders a picture as half-block spans with `fg` AND
//! `bg` (the two fields D4 added to `norte:plugin@0.9.0`), shrunk to the
//! width the host says.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.image-ansi";

/// An 8×4 RGB PNG: two rows of pure red over two rows of pure blue.
/// Generated once with Python's `zlib` + `struct`; small enough to live
/// here, so the test needs no encoder.
const PNG_8X4: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x3c, 0xaf, 0xe9,
    0xa7, 0x00, 0x00, 0x00, 0x16, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x80,
    0x15, 0x61, 0x17, 0x05, 0x49, 0xe0, 0x91, 0xc2, 0x2e, 0x01, 0x00, 0x3a, 0x7e, 0x1f, 0xe1, 0xd3,
    0xd1, 0x61, 0x79, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/image-ansi")
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
        .expect("cargo for image-ansi");
    assert!(status.success(), "image-ansi did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("image_ansi_preview.wasm");
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
fn image_ansi_is_chosen_for_png_and_paints_two_pixels_per_cell() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let (id, _, wasm_path, caps, _) = reg
        .resolve_previewer("image/png")
        .expect("declares image/png");
    assert_eq!(id, ID);
    assert!(
        reg.resolve_previewer("image/webp").is_none(),
        "webp is guessed by the host but not decoded by this guest"
    );

    let mut inst = rt.instantiate(&wasm_path, caps).expect("instantiates");

    // Full width: 8 px → 8 cells wide, 4 px → 2 lines. Red over red on the
    // first line, blue over blue on the second; equal neighbours merge, so
    // each line is ONE span of eight half blocks.
    let styled = inst
        .render_styled_preview("image/png", PNG_8X4, Some(80))
        .expect("renders");
    assert_eq!(styled.len(), 2, "{styled:?}");
    assert_eq!(styled[0].len(), 1);
    assert_eq!(styled[0][0].text, "▀".repeat(8));
    assert_eq!(styled[0][0].fg, Some((255, 0, 0)));
    assert_eq!(styled[0][0].bg, Some((255, 0, 0)));
    assert_eq!(styled[1][0].fg, Some((0, 0, 255)));
    assert_eq!(styled[1][0].bg, Some((0, 0, 255)));

    // Narrower than the picture: shrunk to the width the host said, aspect
    // kept (4 cells wide → 2 px tall → ONE line, red on top, blue below).
    let narrow = inst
        .render_styled_preview("image/png", PNG_8X4, Some(4))
        .expect("renders narrow");
    assert_eq!(narrow.len(), 1, "{narrow:?}");
    assert_eq!(narrow[0][0].text, "▀".repeat(4));
    assert_eq!(narrow[0][0].fg, Some((255, 0, 0)));
    assert_eq!(narrow[0][0].bg, Some((0, 0, 255)));

    // The plain twin names the picture instead of dumping bytes.
    let plain = inst
        .render_preview("image/png", PNG_8X4)
        .expect("renders plain");
    assert!(plain.starts_with("png 8×4"), "{plain}");

    // A file at the host's read cap is refused with a line, not garbage.
    let mut big = PNG_8X4.to_vec();
    big.resize(1024 * 1024, 0);
    let refused = inst
        .render_styled_preview("image/png", &big, Some(80))
        .expect("a refusal is a rendered line");
    assert_eq!(refused.len(), 1);
    assert!(refused[0][0].text.contains("1 MiB"), "{refused:?}");
}
