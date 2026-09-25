//! The host's boundary, checked against cargo's REAL graph.
//!
//! Two things this crate must never reach, for different reasons
//! (ADR 0066):
//!
//! - **No painting toolkit.** If the host knew one, it would stop being
//!   everyone else's host — decision D2 says the renderer is an adapter,
//!   not a layer of the architecture.
//! - **The core.** The host talks through the SDK; dragging in the engine,
//!   the providers or the scheduler would reopen exactly the door phase 1
//!   closed.
//!
//! DEV dependencies do not count: a test can use whatever it needs without
//! it travelling in anyone's binary.

use std::collections::{HashMap, HashSet};

/// What must never reach `norte-ui-host` at runtime.
///
/// `norte-vfs-local` HAS been here since #254: native path conversions moved
/// to `norte-vfs` — the trait crate, which never touches disk — so there is
/// no longer any reason for the one crate with `unsafe`, `openat2` and
/// `ConfinedRoot` to show up here.
const FORBIDDEN: &[&str] = &[
    "norte-core",
    "norte-vfs-local",
    "norte-vfs-sftp",
    "norte-vfs-object",
    "norte-vfs-archive",
    "norte-index",
    "norte-ai",
    "norte-plugin-host",
    "norte-compare",
    "norte-sync",
    "tauri",
    "wry",
    "gpui",
    "ratatui",
    "crossterm",
    "dioxus",
    "iced",
    "slint",
];

#[test]
fn the_host_knows_no_toolkit_and_no_core() {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).expect("metadata is json");
    let nodes = meta["resolve"]["nodes"].as_array().expect("nodes");

    // id → (name, deps that are NOT dev-only)
    let mut graph: HashMap<&str, (String, Vec<&str>)> = HashMap::new();
    for n in nodes {
        let id = n["id"].as_str().expect("id");
        let name = name_of(id, &meta);
        let mut deps = Vec::new();
        for d in n["deps"].as_array().expect("deps") {
            let normal = d["dep_kinds"]
                .as_array()
                .is_none_or(|ks| ks.iter().any(|k| k["kind"].is_null()));
            if normal {
                deps.push(d["pkg"].as_str().expect("pkg"));
            }
        }
        graph.insert(id, (name, deps));
    }

    let root = graph
        .iter()
        .find(|(_, (name, _))| name == "norte-ui-host")
        .map(|(id, _)| *id)
        .expect("norte-ui-host is in the graph");

    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack = vec![root];
    let mut offenders: Vec<String> = Vec::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some((name, deps)) = graph.get(id) else {
            continue;
        };
        if id != root && FORBIDDEN.contains(&name.as_str()) {
            offenders.push(name.clone());
        }
        stack.extend(deps.iter().copied());
    }
    offenders.sort();
    offenders.dedup();
    assert!(
        offenders.is_empty(),
        "the host reaches what it must not: {offenders:?}"
    );
}

/// The package name for a resolver id, read from `packages`.
fn name_of(id: &str, meta: &serde_json::Value) -> String {
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["id"].as_str() == Some(id))
        .and_then(|p| p["name"].as_str())
        .unwrap_or_default()
        .to_owned()
}
