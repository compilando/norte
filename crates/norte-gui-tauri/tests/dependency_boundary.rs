//! The WINDOW's boundary, checked against cargo's real graph.
//!
//! A frontend that only talks to the daemon cannot drag in the engine or a
//! provider (ADR 0066, decision D10). The case that prompted this test: the
//! window depended on `norte-vfs-local` — the only crate with `unsafe`,
//! `openat2` and `ConfinedRoot` — for ONE string-to-`VPath` conversion
//! (#254). The conversion now lives in `norte-vfs`, the trait's crate, which
//! does not touch disk; this is what keeps that edge from coming back.
//!
//! DEV dependencies do not count: a test can use whatever it needs without
//! it traveling in anyone's binary.

use std::collections::{HashMap, HashSet};

/// What must never reach `norte-gui-tauri` at runtime.
///
/// `norte-vfs-local` is NOT here: `norte-frontend` already consumes it for
/// native paths, and the host lives over `norte-frontend` on purpose. What is
/// watched here is that no toolkit and no core show up.
const PROHIBIDAS: &[&str] = &[
    // The engine and its neighbors: this window talks over a socket (ADR
    // 0066).
    "norte-core",
    "norte-index",
    "norte-ai",
    "norte-plugin-host",
    "norte-compare",
    "norte-sync",
    // And NO provider at all. `norte-vfs-local` is the only crate in the
    // project allowed `unsafe`, and it carries `openat2` and `ConfinedRoot`
    // inside: dragging it into a process whose only transport is a socket,
    // to convert a string into a `VPath`, is the opposite of what the ADR
    // promises (#254). The conversion lives in `norte-vfs`, which does not
    // touch disk.
    "norte-vfs-local",
    "norte-vfs-sftp",
    "norte-vfs-object",
    "norte-vfs-archive",
    // Nor another painting toolkit: this window's renderer is the webview.
    "gpui",
    "ratatui",
    "crossterm",
    "dioxus",
    "iced",
    "slint",
];

#[test]
fn la_ventana_no_arrastra_un_provider_ni_el_core() {
    let salida = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .output()
        .expect("cargo metadata");
    assert!(
        salida.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&salida.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&salida.stdout).expect("metadata is json");
    let nodos = meta["resolve"]["nodes"].as_array().expect("nodes");

    // id → (name, deps that are NOT dev)
    let mut grafo: HashMap<&str, (String, Vec<&str>)> = HashMap::new();
    for n in nodos {
        let id = n["id"].as_str().expect("id");
        let nombre = nombre_de(id, &meta);
        let mut deps = Vec::new();
        for d in n["deps"].as_array().expect("deps") {
            let normal = d["dep_kinds"]
                .as_array()
                .is_none_or(|ks| ks.iter().any(|k| k["kind"].is_null()));
            if normal {
                deps.push(d["pkg"].as_str().expect("pkg"));
            }
        }
        grafo.insert(id, (nombre, deps));
    }

    let raiz = grafo
        .iter()
        .find(|(_, (nombre, _))| nombre == "norte-gui-tauri")
        .map(|(id, _)| *id)
        .expect("norte-gui-tauri is in the graph");

    let mut vistos: HashSet<&str> = HashSet::new();
    let mut pila = vec![raiz];
    let mut culpables: Vec<String> = Vec::new();
    while let Some(id) = pila.pop() {
        if !vistos.insert(id) {
            continue;
        }
        let Some((nombre, deps)) = grafo.get(id) else {
            continue;
        };
        if id != raiz && PROHIBIDAS.contains(&nombre.as_str()) {
            culpables.push(nombre.clone());
        }
        pila.extend(deps.iter().copied());
    }
    culpables.sort();
    culpables.dedup();
    assert!(
        culpables.is_empty(),
        "the window reaches something it should not: {culpables:?}"
    );
}

/// A resolver id's package name, read from `packages`.
fn nombre_de(id: &str, meta: &serde_json::Value) -> String {
    meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["id"].as_str() == Some(id))
        .and_then(|p| p["name"].as_str())
        .unwrap_or_default()
        .to_owned()
}
