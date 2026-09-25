//! `org.norte.image-thumb` (`plugins/image-thumb/`, ADR 0107) builds,
//! installs, is resolved as the thumbnailer for an image mimetype, and
//! answers a downscaled raster that passes the host's verification —
//! encoding by magic, declared mimetype and dimensions, edge cap.

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{PluginRegistry, install};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.image-thumb";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/image-thumb")
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
        .expect("cargo for image-thumb");
    assert!(status.success(), "image-thumb did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("image_thumb.wasm");
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

/// A real, decodable PNG of `w`×`h` RGB, written by hand: the guest has to
/// DECODE it, so a bare header is not enough — and this crate carries no
/// PNG encoder. zlib with STORED (uncompressed) deflate blocks is valid
/// zlib, and every decoder takes it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::trivially_copy_pass_by_ref,
    reason = "a toy PNG encoder: test sizes, all short"
)]
fn png(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for b in bytes {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    // Raw scanlines: filter byte 0 + RGB per pixel.
    let mut raw = Vec::with_capacity((h * (1 + w * 3)) as usize);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            raw.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128]);
        }
    }
    // zlib: header, stored blocks of at most 65535 bytes, adler32.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut bsum) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        bsum = (bsum + a) % 65_521;
    }
    z.extend_from_slice(&((bsum << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn image_thumb_is_resolved_for_images_and_answers_a_verified_raster() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let (id, _name, wasm_path, caps, settings) = reg
        .resolve_thumbnailer("image/png")
        .expect("the manifest lists image/png");
    assert_eq!(id, ID);
    assert!(
        reg.resolve_thumbnailer("text/plain").is_none(),
        "a mimetype the manifest does not list is nobody's"
    );

    let mut inst = rt
        .instantiate_thumbnail(&wasm_path, caps)
        .expect("instantiates");
    inst.set_settings(settings);
    let t = inst
        .render_thumbnail("image/png", &png(640, 320), 160)
        .expect("a thumbnail");
    assert_eq!(
        (t.width, t.height),
        (160, 80),
        "scaled to the edge, ratio kept"
    );
    // The guest answers JPEG by default, but what comes OUT of the host is
    // what the host itself encoded (ADR 0107 decision 3, second gate): a
    // small raster fits as PNG, so PNG is what reaches the viewer, and the
    // guest's bytes never do.
    assert_eq!(t.mimetype, "image/png");
    assert!(t.bytes.starts_with(b"\x89PNG"), "re-encoded by the host");

    // Garbage in: the guest says no, and that is a guest error, not a raster.
    let err = inst
        .render_thumbnail("image/png", b"definitely not an image", 160)
        .expect_err("no thumbnail of garbage");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(_)),
        "{err}"
    );
}
