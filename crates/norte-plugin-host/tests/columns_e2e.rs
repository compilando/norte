//! E2E de la interfaz WIT `columns` (ADR 0037 decisión 2, world
//! `norte-columns`): compila el guest REAL `examples-wasm/columns-demo` a
//! `wasm32-wasip2` y lo ejecuta a través de
//! [`PluginRuntime::instantiate_columns`], verificando el contrato
//! POSICIONAL 1:1 (ADR 0037 tabla de decisión 1: `result[i]` valora
//! `entries[i]`, nunca reordenado ni disperso) contra un guest que valora la
//! columna `"name-len"` con el largo en bytes del nombre, y responde `none`
//! para toda la página cuando se le pide un id de columna que no declara.
//!
//! SKIP si el target `wasm32-wasip2` no está instalado.

use norte_plugin_host::{Capabilities, PluginRuntime};

mod support;

#[test]
fn columns_wit_e2e_positional_roundtrip_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns(&wasm, Capabilities::default())
        .expect("instanciar el columns");

    let entries: Vec<Vec<u8>> = vec![b"a.rs".to_vec(), b"README.md".to_vec(), b"x".to_vec()];
    let out = inst
        .column_values("name-len", None, &entries)
        .expect("column_values sin trap");

    assert_eq!(out.len(), entries.len(), "positional 1:1, nunca disperso");
    assert_eq!(out[0].as_deref(), Some("4"), "\"a.rs\" = 4 bytes");
    assert_eq!(out[1].as_deref(), Some("9"), "\"README.md\" = 9 bytes");
    assert_eq!(out[2].as_deref(), Some("1"), "\"x\" = 1 byte");

    // Un id de columna que el guest NO declara: `none` para TODA la página,
    // nunca se adivina ni se omite del vector posicional.
    let unknown = inst
        .column_values("no-declarada", None, &entries)
        .expect("column_values sin trap");
    assert_eq!(unknown, vec![None, None, None]);

    // Un lote VACÍO es un caso límite legítimo (página sin entradas visibles).
    let empty = inst
        .column_values("name-len", None, &[])
        .expect("lote vacío sin trap");
    assert!(empty.is_empty());
}

/// ADR 0057: un guest SIN la capacidad `location` aprobada no resuelve ni un
/// byte, y el host no llega siquiera a mirar el token — el mismo criterio que
/// `fs-read` scoped. El guest sigue contestando: celdas vacías, no una traba.
#[test]
fn sin_la_capability_el_host_niega_antes_de_tocar_nada_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let espia = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(
            &wasm,
            Capabilities::default(), // sin `location`
            Some(espia.clone()),
        )
        .expect("instanciar el columns");

    let out = inst
        .column_values("stat-size", Some("tok"), &[b"a.txt".to_vec()])
        .expect("column_values sin trap");
    assert_eq!(out, vec![None], "sin capacidad, celda vacía");
    assert_eq!(espia.calls(), 0, "el host no resolvió ni un byte");
}

/// Con la capacidad aprobada Y un resolutor inyectado, el guest lee de verdad.
#[test]
fn con_la_capability_el_guest_lee_bajo_el_token_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let espia = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(&wasm, support::caps_con_location(), Some(espia.clone()))
        .expect("instanciar el columns");

    let out = inst
        .column_values(
            "stat-size",
            Some("tok"),
            &[b"a.txt".to_vec(), b"no".to_vec()],
        )
        .expect("column_values sin trap");
    assert_eq!(
        out,
        vec![Some("42".to_owned()), None],
        "lo que el host resuelve llega tal cual; lo que no, celda vacía"
    );
    assert_eq!(espia.calls(), 2, "una llamada por entrada");
}

/// Sin token —el host no pudo abrir el directorio— el guest tampoco falla.
#[test]
fn sin_token_el_guest_sigue_contestando_wasm_real() {
    let Some(wasm) = support::build_guest("columns-demo") else {
        return;
    };
    let espia = std::sync::Arc::new(support::SpyLocation::default());
    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut inst = rt
        .instantiate_columns_with_location(&wasm, support::caps_con_location(), Some(espia.clone()))
        .expect("instanciar el columns");
    let out = inst
        .column_values("stat-size", None, &[b"a.txt".to_vec()])
        .expect("column_values sin trap");
    assert_eq!(out, vec![None]);
    assert_eq!(espia.calls(), 0);
}
