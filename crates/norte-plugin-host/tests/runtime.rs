//! Tests del runtime wasmtime que NO requieren un componente WASM real: que el
//! motor se construye y que un artefacto basura falla con un error claro.

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

mod support;

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

#[test]
fn previewer_demo_renderiza_y_loguea() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    let out = inst
        .render_preview(
            "text/plain",
            b"linea uno\nlinea dos\nlinea tres\nlinea cuatro",
        )
        .expect("render");
    assert!(out.contains("text/plain"), "cabecera: {out}");
    assert!(out.contains("linea uno") && out.contains("linea tres"));
    assert!(!out.contains("linea cuatro"), "solo 3 líneas");
    assert!(
        inst.logs().iter().any(|l| l.contains("previewer-demo")),
        "host-log: {:?}",
        inst.logs()
    );
}
