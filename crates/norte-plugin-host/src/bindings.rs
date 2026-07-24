//! Bindings del Component Model generados por wasmtime desde el WIT (M4-P2).
#![allow(missing_docs)] // el código generado no lleva rustdoc

wasmtime::component::bindgen!({
    world: "norte-plugin",
    path: "wit/norte-plugin.wit",
});

/// Bindings del world `norte-provider` (#30 stage 2). En su propio módulo para
/// no colisionar con los tipos de `norte-plugin`; REUTILIZA las interfaces
/// `host-log` y `host-config` (P2 Task 3) del world de arriba (`with`) para no
/// duplicar el trait `Host` ni el `add_to_linker` — [`crate::runtime::HostState`]
/// las implementa una vez.
pub mod provider_world {
    wasmtime::component::bindgen!({
        world: "norte-provider",
        path: "wit/norte-plugin.wit",
        with: {
            "norte:plugin/host-log": crate::bindings::norte::plugin::host_log,
            "norte:plugin/host-config": crate::bindings::norte::plugin::host_config,
        },
    });
}
