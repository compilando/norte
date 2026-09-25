//! Every navigation that lands goes through `settle_cd`, never `apply_cd`.
//!
//! `apply_cd` only files away the drain; `settle_cd` also sorts by scheme and
//! REQUESTS THE PLUGIN DECORATIONS. Twelve call sites used `apply_cd`, so a
//! listing reached through them had no icons: connecting to an S3 bucket
//! painted its root as `/2025-abisko` (the no-icon form of a folder) with the
//! header out of line, until a `..` re-listed it through `settle_cd`.
//!
//! A sweep and not a behavioural test: each site is a different screen of
//! the event loop, and what they share is exactly this one call.

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("src is readable") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_landing_skips_settle_cd() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    let mut offenders = Vec::new();
    for f in files {
        // `navigate.rs` defines both, and `settle_cd` is built on `apply_cd`.
        if f.ends_with("navigate.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&f).expect("readable source");
        for (i, line) in text.lines().enumerate() {
            if line.contains("apply_cd(") && !line.trim_start().starts_with("//") {
                offenders.push(format!("{}:{}", f.display(), i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these land a cd without settle_cd (no decorations, no scheme sort):\n{}",
        offenders.join("\n")
    );
}
