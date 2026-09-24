//! The tree's revision, at compile time, so the two windows —`ntc` and the
//! graphical one— can say WHICH binary they are and not just which version
//! the Cargo.toml declares: on a dev machine `just link`'s symlinks point at
//! `target/debug`, and "0.3.0-alpha.3" is the same string ten commits later.
//! `git describe` gives the answer the version does not.
//!
//! Without `.git` (a release tarball, a `cargo install` from crates.io) or
//! without `git` on the PATH, `NORTE_REVISION` is honoured if the packager
//! set it, and otherwise it is left as `unknown`: this never fails the
//! build.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NORTE_REVISION");
    let revision = std::env::var("NORTE_REVISION")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=NORTE_REVISION={revision}");
}

/// `git describe --tags --always --dirty --long` over the tree containing
/// this crate, plus the `rerun-if-changed`s that make it change with HEAD.
fn git_describe() -> Option<String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let top = String::from_utf8(top.stdout).ok()?.trim().to_owned();
    let git_dir = Path::new(&top).join(".git");
    // HEAD changes when the branch moves; the ref it points to, on commit;
    // packed-refs and the index, on tagging or touching the tree (the
    // `-dirty`).
    for f in ["HEAD", "packed-refs", "index"] {
        println!("cargo:rerun-if-changed={}", git_dir.join(f).display());
    }
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(r) = head.strip_prefix("ref: ")
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(r.trim()).display()
        );
    }
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty", "--long"])
        .current_dir(&top)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!s.is_empty()).then_some(s)
}
