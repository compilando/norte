//! Bindings del Component Model generados por wasmtime desde el WIT (M4-P2).
#![allow(missing_docs)] // el código generado no lleva rustdoc

wasmtime::component::bindgen!({
    world: "norte-plugin",
    path: "wit/norte-plugin.wit",
});
