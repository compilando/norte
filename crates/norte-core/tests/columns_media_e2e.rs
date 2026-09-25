//! `org.norte.media-info` (`plugins/media-info/`) builds, installs and
//! answers its two columns from headers read under the location token —
//! including a file far bigger than what it reads, which is what
//! `read-prefix` (norte:location 0.2.0) buys.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;
use norte_proto::VPath;

const ID: &str = "org.norte.media-info";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/media-info")
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
        .expect("cargo for media-info");
    assert!(status.success(), "media-info did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("media_info.wasm");
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

/// A PNG header claiming `w`×`h`: signature, IHDR length, type, fields.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 2, 0, 0, 0]);
    v
}

/// A 44-byte WAV header for `secs` seconds at `byte_rate`, plus the payload.
fn wav(byte_rate: u32, secs: u32) -> Vec<u8> {
    let data = byte_rate * secs;
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(36 + data).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&byte_rate.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data.to_le_bytes());
    v.resize(44 + data as usize, 0);
    v
}

fn cells(
    reg: &PluginRegistry,
    rt: &PluginRuntime,
    column: &str,
    dir: Option<&VPath>,
    names: &[Vec<u8>],
) -> Vec<Option<String>> {
    let (id, name, wasm, caps, settings) = reg
        .resolve_columns_of(Some(ID), column)
        .expect("the manifest declares the column");
    norte_core::plugins::run_column_values_for_test(
        rt,
        (id, name, wasm, caps, settings),
        column,
        dir,
        false,
        names,
        names.len(),
    )
}

#[test]
fn media_info_answers_dims_and_duration_from_headers_only() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let dir = tempfile::tempdir().expect("media dir");
    std::fs::write(dir.path().join("tiny.png"), png_header(1, 1)).expect("png");
    std::fs::write(dir.path().join("clip.wav"), wav(8000, 2)).expect("wav");
    std::fs::write(dir.path().join("notes.txt"), b"not media").expect("txt");
    std::fs::write(dir.path().join("broken.png"), b"\x89PN").expect("broken");
    // 200 KiB with a valid header in front: far more than the plugin reads.
    let mut big = png_header(8, 4);
    big.resize(200 * 1024, 0);
    std::fs::write(dir.path().join("big.png"), big).expect("big");
    let names: Vec<Vec<u8>> = ["tiny.png", "clip.wav", "notes.txt", "broken.png", "big.png"]
        .iter()
        .map(|n| n.as_bytes().to_vec())
        .collect();
    let loc = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");

    assert_eq!(
        cells(&reg, &rt, "dims", Some(&loc), &names),
        vec![
            Some("1×1".to_owned()),
            None,
            None,
            None,
            Some("8×4".to_owned()),
        ],
        "dims: images only, a cut header is empty, a big file still answers"
    );
    assert_eq!(
        cells(&reg, &rt, "duration", Some(&loc), &names),
        vec![None, Some("0:02".to_owned()), None, None, None],
        "duration: audio only"
    );
    assert_eq!(
        cells(&reg, &rt, "dims", None, &names),
        vec![None; 5],
        "without a location every cell is empty"
    );
    assert!(
        reg.resolve_columns_of(Some(ID), "nope").is_none(),
        "a column the manifest does not declare is not the plugin's to answer"
    );
}
