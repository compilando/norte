//! The SDK's boundary, checked against cargo's REAL graph.
//!
//! A frontend that only talks over a socket cannot drag in the engine, the
//! providers, the index, AI, the plugin host or the presentation layer just
//! to send a JSON-RPC (ADR 0066). This is not a convention written in a
//! comment: if someone adds one of those dependencies — direct or
//! transitive, in the release tree — this test turns red.
//!
//! DEV dependencies do not count: a test can use whatever it needs without
//! it traveling in anyone's binary.

use std::collections::{HashMap, HashSet};

/// What must never reach `norte-client` at runtime.
const FORBIDDEN: &[&str] = &[
    "norte-core",
    "norte-vfs",
    "norte-vfs-local",
    "norte-vfs-sftp",
    "norte-vfs-object",
    "norte-vfs-archive",
    "norte-vfs-rar",
    "norte-index",
    "norte-ai",
    "norte-plugin-host",
    "norte-frontend",
    "norte-compare",
    "norte-sync",
];

#[test]
fn the_sdk_does_not_reach_the_core_or_the_providers() {
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
        .find(|(_, (name, _))| name == "norte-client")
        .map(|(id, _)| *id)
        .expect("norte-client is in the graph");

    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![root];
    let mut culprits: Vec<String> = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some((name, deps)) = graph.get(id) else {
            continue;
        };
        if id != root && FORBIDDEN.contains(&name.as_str()) {
            culprits.push(name.clone());
        }
        stack.extend(deps.iter().copied());
    }
    culprits.sort();
    culprits.dedup();
    assert!(
        culprits.is_empty(),
        "the SDK reaches what it must not: {culprits:?}"
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
