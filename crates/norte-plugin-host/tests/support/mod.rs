//! Test support: compiles an `examples-wasm/` WASM guest to
//! `wasm32-wasip2` on demand and returns the component's path.
//!
//! If the `wasm32-wasip2` target is not installed, the helper does a SKIP
//! (returns `None`) so the test passes on toolchains without that target; if
//! the target IS there but the guest does not compile, it is a real failure
//! and aborts.

use std::path::PathBuf;
use std::process::Command;

/// Compiles the `examples-wasm/<name>/` guest to `wasm32-wasip2` in
/// release mode and returns the produced `.wasm` as an artifact, with the
/// fingerprint of what it just compiled (ADR 0142): the test is the
/// authority for that file.
///
/// Returns `None` (with a `stderr` warning) if the `wasm32-wasip2` target
/// is not installed.
#[must_use]
pub fn build_guest(name: &str) -> Option<norte_plugin_host::WasmArtifact> {
    build_guest_path(name)
        .map(|p| norte_plugin_host::WasmArtifact::trusting_current(p).expect("the guest is read"))
}

/// Like [`build_guest`], but the PATH: for whoever has to copy it or
/// rewrite it before instantiating it.
#[must_use]
pub fn build_guest_path(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 not installed");
        return None;
    }

    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
    assert!(
        status.success(),
        "guest {name} did not compile (wasm32-wasip2 target present)"
    );

    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "artifact {} was not found", wasm.display());
    Some(wasm)
}

/// Replaces EVERY occurrence of `from` with `to` in `bytes`. Only with
/// equal lengths: it is meant to build a guest "compiled against another
/// version" by rewriting `@0.8.0` in its imports section without shifting
/// so much as one offset in the other sections.
///
/// # Panics
/// If the lengths differ: a replacement that shifts bytes leaves a
/// component no reader can walk, and the test would be testing garbage.
#[must_use]
#[allow(dead_code)]
pub fn rewrite_bytes(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len(), "only same-length replacements");
    let mut out = bytes.to_vec();
    if from.is_empty() {
        return out;
    }
    let mut i = 0;
    while i + from.len() <= out.len() {
        if &out[i..i + from.len()] == from {
            out[i..i + from.len()].copy_from_slice(to);
            i += from.len();
        } else {
            i += 1;
        }
    }
    out
}

/// `true` if `rustup` reports `target` among the installed ones.
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

/// A fake [`LocationHost`](norte_plugin_host::LocationHost) that COUNTS
/// how many times it is asked: this lets a test assert that the host
/// resolved nothing, which is different from it resolving and the guest
/// dropping the data.
// `support` is compiled INSIDE every test binary, and only `columns_e2e`
// uses the spy: in the others it is dead by construction, not by neglect.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct SpyLocation {
    calls: std::sync::atomic::AtomicUsize,
}

#[allow(dead_code)]
impl SpyLocation {
    /// How many times it has been asked something.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn count(&self) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl norte_plugin_host::LocationHost for SpyLocation {
    fn read(&self, _token: &str, rel: &[u8]) -> Result<Vec<u8>, String> {
        self.count();
        if rel == b"a.txt" {
            Ok(b"content".to_vec())
        } else {
            Err("does not exist".into())
        }
    }

    fn read_prefix(&self, token: &str, rel: &[u8], max: u64) -> Result<Vec<u8>, String> {
        let mut bytes = self.read(token, rel)?;
        bytes.truncate(usize::try_from(max).unwrap_or(usize::MAX));
        Ok(bytes)
    }

    fn stat(
        &self,
        _token: &str,
        rel: &[u8],
    ) -> Result<norte_plugin_host::location_iface::Meta, String> {
        self.count();
        if rel != b"a.txt" {
            return Err("does not exist".into());
        }
        Ok(norte_plugin_host::location_iface::Meta {
            kind: norte_plugin_host::location_iface::EntryKind::File,
            size: 42,
            mtime_sec: 1,
            mtime_nsec: 0,
            ctime_sec: 1,
            ctime_nsec: 0,
            ino: 7,
            dev: 9,
            mode: 0o100_644,
        })
    }

    fn list_dir(
        &self,
        _token: &str,
        _rel: &[u8],
    ) -> Result<Vec<norte_plugin_host::location_iface::Dirent>, String> {
        self.count();
        Ok(Vec::new())
    }
}

/// Capabilities with `location = "read"` granted.
#[allow(dead_code)]
#[must_use]
pub fn caps_with_location() -> norte_plugin_host::Capabilities {
    norte_plugin_host::Manifest::from_toml(
        r#"
[plugin]
id = "org.norte.columns"
name = "Columns"
publisher = "norte"
version = "0.1.0"
category = "columns"

[capabilities]
location = "read"
"#,
    )
    .expect("valid test manifest")
    .capabilities
}
