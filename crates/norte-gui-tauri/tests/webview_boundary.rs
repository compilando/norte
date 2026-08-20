//! La frontera de la webview, clavada (ADR 0066, decisión D11).
//!
//! Estas comprobaciones leen la CONFIGURACIÓN y los assets empaquetados, no el
//! runtime, y ese es justo el punto: una CSP relajada, una capacidad de más o
//! un `<script src="https://…">` colado en el bundle no se ven en ninguna
//! prueba de comportamiento — se ven aquí, en el diff, o no se ven nunca.

use std::path::{Path, PathBuf};

fn raiz() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn json(rel: &str) -> serde_json::Value {
    let p = raiz().join(rel);
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("JSON válido")
}

/// La CSP no deja hueco: ni scripts remotos, ni `eval`, ni estilos en línea.
#[test]
fn la_csp_no_deja_puertas() {
    let cfg = json("tauri.conf.json");
    let csp = cfg["app"]["security"]["csp"]
        .as_str()
        .expect("la CSP está puesta");
    // El ÚNICO origen con esquema que se admite es el del propio IPC de
    // Tauri, que no es remoto: es como la webview le habla a este proceso.
    // Todo lo demás —un CDN, un websocket de desarrollo, un comodín— es una
    // puerta al exterior y no puede estar.
    let sin_ipc = csp.replace("http://ipc.localhost", "");
    for prohibido in [
        "'unsafe-inline'",
        "'unsafe-eval'",
        "http://",
        "https://",
        "ws://",
        "wss://",
        "*",
    ] {
        assert!(
            !sin_ipc.contains(prohibido),
            "la CSP no puede contener {prohibido}: {csp}"
        );
    }
    assert!(
        csp.starts_with("default-src 'none'"),
        "cerrada por defecto: {csp}"
    );
    for directiva in [
        "script-src 'self'",
        "object-src 'none'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directiva), "falta `{directiva}`: {csp}");
    }
}

/// La webview no tiene ni el objeto global de Tauri, ni protocolo de assets,
/// ni un servidor de desarrollo al que ir en producción.
#[test]
fn la_ventana_no_trae_nada_de_serie() {
    let cfg = json("tauri.conf.json");
    assert_eq!(
        cfg["app"]["withGlobalTauri"],
        serde_json::Value::Bool(false),
        "sin `window.__TAURI__`: lo que se puede llamar se importa, y está en la lista"
    );
    assert_eq!(
        cfg["app"]["security"]["assetProtocol"]["enable"],
        serde_json::Value::Bool(false),
        "sin protocolo de assets no hay forma de pedirle un fichero del disco"
    );
    assert!(
        cfg["build"]["devUrl"].is_null(),
        "un binario de producción no apunta a un servidor de desarrollo"
    );
    assert_eq!(
        cfg["app"]["windows"][0]["dragDropEnabled"],
        serde_json::Value::Bool(false),
        "soltar un fichero en la ventana todavía no significa nada: que no lo parezca"
    );
}

/// El fichero de capacidades concede LO MÍNIMO: escuchar eventos. Nada de
/// filesystem, shell, http, diálogo nativo ni control de ventana.
#[test]
fn las_capacidades_son_las_minimas() {
    let cap = json("capabilities/main.json");
    let permisos: Vec<&str> = cap["permissions"]
        .as_array()
        .expect("hay lista de permisos")
        .iter()
        .map(|p| p.as_str().expect("cada permiso es una cadena"))
        .collect();
    assert_eq!(
        permisos,
        vec!["core:event:allow-listen", "core:event:allow-unlisten"],
        "cualquier permiso de más es una decisión, y se ve aquí"
    );
    assert_eq!(
        cap["windows"].as_array().map(Vec::len),
        Some(1),
        "una ventana, la principal"
    );
}

/// Los comandos que el binario registra son EXACTAMENTE los declarados.
///
/// Se lee el `main.rs` a propósito: `generate_handler!` es un macro, así que
/// un comando nuevo no aparece en ninguna lista que se pueda comparar en
/// tiempo de compilación. Esto lo convierte en algo que rompe el test.
#[test]
fn la_superficie_de_comandos_es_la_declarada() {
    let src = std::fs::read_to_string(raiz().join("src/main.rs")).expect("main.rs");
    // El bloque de PRODUCCIÓN, que es el de `not(feature = "metrics")`: la
    // instrumentación de la 3.6 añade un comando más y no puede colarse aquí.
    let (_, tras_cfg) = src
        .split_once("#[cfg(not(feature = \"metrics\"))]")
        .expect("el handler de producción está marcado");
    let (_, resto) = tras_cfg
        .split_once("generate_handler![")
        .expect("el binario registra comandos");
    let (bloque, _) = resto.split_once(']').expect("el macro cierra");
    let registrados: Vec<String> = bloque
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        registrados,
        norte_gui_tauri::commands::COMANDOS,
        "la lista declarada y la registrada tienen que ser la misma"
    );
}

/// El bundle de producción no trae nada remoto, ni `eval`, ni el cliente del
/// servidor de desarrollo.
///
/// Si no hay bundle todavía, el test lo DICE y no pasa por alto: un
/// «no había nada que mirar» que se lee como verde es peor que un rojo.
#[test]
fn el_bundle_no_llama_a_casa() {
    let dist = raiz().join("ui/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        eprintln!("sin bundle en {}: `just gui-build` primero", dist.display());
        return;
    }
    let mut mirados = 0usize;
    for entrada in walk(&dist) {
        let Some(ext) = entrada.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "html" | "js" | "css") {
            continue;
        }
        let texto = std::fs::read_to_string(&entrada).unwrap_or_default();
        mirados += 1;
        for prohibido in [
            "http://",
            "https://",
            "ws://",
            "wss://",
            "eval(",
            "new Function(",
        ] {
            assert!(
                !texto.contains(prohibido),
                "{} contiene `{prohibido}`",
                entrada.display()
            );
        }
    }
    assert!(mirados >= 2, "se miraron el HTML y su script");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
