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
        .column_values("name-len", &entries)
        .expect("column_values sin trap");

    assert_eq!(out.len(), entries.len(), "positional 1:1, nunca disperso");
    assert_eq!(out[0].as_deref(), Some("4"), "\"a.rs\" = 4 bytes");
    assert_eq!(out[1].as_deref(), Some("9"), "\"README.md\" = 9 bytes");
    assert_eq!(out[2].as_deref(), Some("1"), "\"x\" = 1 byte");

    // Un id de columna que el guest NO declara: `none` para TODA la página,
    // nunca se adivina ni se omite del vector posicional.
    let unknown = inst
        .column_values("no-declarada", &entries)
        .expect("column_values sin trap");
    assert_eq!(unknown, vec![None, None, None]);

    // Un lote VACÍO es un caso límite legítimo (página sin entradas visibles).
    let empty = inst
        .column_values("name-len", &[])
        .expect("lote vacío sin trap");
    assert!(empty.is_empty());
}
