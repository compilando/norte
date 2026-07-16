//! Tests del runtime wasmtime que NO requieren un componente WASM real: que el
//! motor se construye y que un artefacto basura falla con un error claro.

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

#[test]
fn runtime_se_construye() {
    let _rt = PluginRuntime::new().expect("engine");
}

#[test]
fn cargar_un_no_componente_falla_claro() {
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("no.wasm");
    std::fs::write(&fake, b"esto no es un componente wasm").unwrap();
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("bytes basura");
    assert!(matches!(err, RuntimeError::Component(_)), "fue {err:?}");
}
