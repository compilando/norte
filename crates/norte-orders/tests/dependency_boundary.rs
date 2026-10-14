//! The grammar's boundary, checked against cargo's REAL graph.
//!
//! The core validates what the AI writes, the frontends colour what the reader
//! types and the CLI runs scripts: all three link this crate, so it may pull in
//! NO other crate of ours at runtime. Dev dependencies do not count.

use std::collections::{HashMap, HashSet};

#[test]
fn the_grammar_reaches_no_other_norte_crate() {
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
        .find(|(_, (name, _))| name == "norte-orders")
        .map(|(id, _)| *id)
        .expect("norte-orders is in the graph");

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
        if id != root && name.starts_with("norte-") {
            culprits.push(name.clone());
        }
        stack.extend(deps.iter().copied());
    }
    culprits.sort();
    culprits.dedup();
    assert!(
        culprits.is_empty(),
        "the grammar reaches crates of ours: {culprits:?}"
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
