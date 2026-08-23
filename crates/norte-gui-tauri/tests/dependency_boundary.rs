//! La frontera de la VENTANA, comprobada contra el grafo real de cargo.
//!
//! Un frontend que solo habla con el daemon no puede arrastrar el motor ni un
//! provider (ADR 0066, decisión D10). El caso que motivó este test: la ventana
//! dependía de `norte-vfs-local` —el único crate con `unsafe`, `openat2` y
//! `ConfinedRoot`— para UNA conversión de cadena a `VPath` (#254). La
//! conversión vive ahora en `norte-vfs`, que es el crate del trait y no toca
//! disco; esto es lo que impide que la arista vuelva.
//!
//! Las dependencias de DESARROLLO no cuentan: un test puede usar lo que le
//! haga falta sin que viaje en el binario de nadie.

use std::collections::{HashMap, HashSet};

/// Lo que jamás debe alcanzar a `norte-gui-tauri` en tiempo de ejecución.
///
/// `norte-vfs-local` NO está: `norte-frontend` ya lo consume para los paths
/// nativos, y el host vive sobre `norte-frontend` a propósito. Lo que se
/// vigila aquí es que no aparezca un toolkit ni el core.
const PROHIBIDAS: &[&str] = &[
    // El motor y sus vecinos: esta ventana habla por un socket (ADR 0066).
    "norte-core",
    "norte-index",
    "norte-ai",
    "norte-plugin-host",
    "norte-compare",
    "norte-sync",
    // Y NINGÚN provider. `norte-vfs-local` es el único crate del proyecto al
    // que se le permite `unsafe`, y lleva dentro `openat2` y `ConfinedRoot`:
    // arrastrarlo a un proceso cuyo único transporte es un socket, para
    // convertir una cadena en un `VPath`, es lo contrario de lo que la ADR
    // promete (#254). La conversión vive en `norte-vfs`, que no toca disco.
    "norte-vfs-local",
    "norte-vfs-sftp",
    "norte-vfs-object",
    "norte-vfs-archive",
    // Ni otro toolkit de pintado: el renderer de esta ventana es la webview.
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
        .find(|(_, (nombre, _))| nombre == "norte-gui-tauri")
        .map(|(id, _)| *id)
        .expect("norte-gui-tauri está en el grafo");

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
        "la ventana alcanza lo que no debe: {culpables:?}"
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
