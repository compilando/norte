//! Nobody launches work without its span (ADR 0127).
//!
//! `tokio::spawn` and `spawn_blocking` do not inherit the span of whoever
//! calls them, and what the launched task logs comes out without its `rpc`
//! nor its `task`. The gate is `crate::blocking::{spawn, spawn_blocking}`.
//! `spawn_blocking` is also blocked by clippy (`clippy.toml`); `tokio::spawn`
//! cannot be, because the integration tests use it heavily and the lint
//! cannot tell them apart. This blocks it in the crate's CODE, which is where
//! it matters.

use std::path::{Path, PathBuf};

/// The `.rs` files under `src/`, recursively.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("src readable") {
        let p = e.expect("readable entry").path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// A file's content BEFORE its first test module: what actually compiles into
/// the real binary.
fn code(text: &str) -> &str {
    text.find("#[cfg(test)]").map_or(text, |i| &text[..i])
}

#[test]
fn nobody_launches_a_task_without_its_span() {
    // Only the two gates. What must not inherit the span goes through
    // `spawn_root`, with its reason written where it is called.
    const ALLOWED: &[&str] = &["src/blocking.rs"];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut bad = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(root)
            .expect("inside the crate")
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("readable source");
        for (n, line) in code(&text).lines().enumerate() {
            if line.contains("tokio::spawn(") || line.contains("tokio::task::spawn(") {
                bad.push(format!("{rel}:{}", n + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "they launch a task without its span; use `crate::blocking::spawn`: {bad:#?}"
    );
}
