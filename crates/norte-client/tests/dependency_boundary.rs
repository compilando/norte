//! La frontera del SDK, comprobada contra el grafo REAL de cargo.
//!
//! Un frontend que solo habla por socket no puede arrastrar el engine, los
//! providers, el índice, la IA, el host de plugins ni la capa de presentación
//! para mandar un JSON-RPC (ADR 0066). Esto no es una convención escrita en un
//! comentario: si alguien añade una de esas dependencias —directa o
//! transitiva, en el árbol de release— este test se pone rojo.
//!
//! Las dependencias de DESARROLLO no cuentan: un test puede usar lo que le
//! haga falta sin que viaje en el binario de nadie.

use std::collections::{HashMap, HashSet};

/// Lo que jamás debe alcanzar a `norte-client` en tiempo de ejecución.
const PROHIBIDAS: &[&str] = &[
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
fn el_sdk_no_alcanza_al_core_ni_a_los_providers() {
    let salida = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--all-features"])
        .output()
        .expect("cargo metadata");
    assert!(
        salida.status.success(),
        "cargo metadata falló: {}",
        String::from_utf8_lossy(&salida.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&salida.stdout).expect("metadata es json");
    let nodos = meta["resolve"]["nodes"].as_array().expect("nodes");

    // id → (nombre, deps que NO son de desarrollo)
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
        .find(|(_, (nombre, _))| nombre == "norte-client")
        .map(|(id, _)| *id)
        .expect("norte-client está en el grafo");

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
        "el SDK alcanza lo que no debe: {culpables:?}"
    );
}

/// El nombre del paquete de un id del resolvedor, leído de `packages`.
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
