//! Bindings del Component Model generados por wasmtime desde el WIT (M4-P2).
//!
//! El WIT son TRES paquetes desde ADR 0041 decisión 4 (`norte:host`,
//! `norte:plugin`, `norte:provider`), así que `path` apunta al DIRECTORIO
//! `wit/` — no a un fichero — y wasmtime resuelve `wit/deps/` solo. Los worlds
//! que viven fuera del paquete de arriba se nombran completos
//! (`norte:provider/norte-provider`).
#![allow(missing_docs)] // el código generado no lleva rustdoc

wasmtime::component::bindgen!({
    world: "norte-plugin",
    path: "wit",
});

/// Bindings del world `norte-provider` (#30 stage 2). En su propio módulo para
/// no colisionar con los tipos de `norte-plugin`; REUTILIZA las interfaces
/// `host-log` y `host-config` (P2 Task 3) del world de arriba (`with`) para no
/// duplicar el trait `Host` ni el `add_to_linker` — [`crate::runtime::HostState`]
/// las implementa una vez.
///
/// El world se nombra COMPLETO: desde la partición del paquete vive en
/// `norte:provider`, no en el paquete de arriba.
pub mod provider_world {
    wasmtime::component::bindgen!({
        world: "norte:provider/norte-provider",
        path: "wit",
        with: {
            "norte:host/host-log": crate::bindings::norte::host::host_log,
            "norte:host/host-config": crate::bindings::norte::host::host_config,
        },
    });
}

/// Bindings del world `norte-decorator` (ADR 0037 decisión 2). Mismo motivo
/// de módulo separado + `with:` que [`provider_world`]: no duplicar el trait
/// `Host` de `host-log`/`host-config` ni su `add_to_linker`.
pub mod decorator_world {
    wasmtime::component::bindgen!({
        world: "norte-decorator",
        path: "wit",
        with: {
            "norte:host/host-log": crate::bindings::norte::host::host_log,
            "norte:host/host-config": crate::bindings::norte::host::host_config,
        },
    });
}

/// Bindings del world `norte-columns` (ADR 0037 decisión 2). Mismo patrón que
/// [`decorator_world`].
pub mod columns_world {
    wasmtime::component::bindgen!({
        world: "norte-columns",
        path: "wit",
        with: {
            "norte:host/host-log": crate::bindings::norte::host::host_log,
            "norte:host/host-config": crate::bindings::norte::host::host_config,
        },
    });
}

pub mod renamer_world {
    wasmtime::component::bindgen!({
        world: "norte:renamer/norte-renamer",
        path: "wit",
        with: {
            "norte:host/host-log": crate::bindings::norte::host::host_log,
            "norte:host/host-config": crate::bindings::norte::host::host_config,
            // La MISMA interfaz de ubicación que el world de columnas: un
            // solo `LocationHost` la sirve a los dos.
            "norte:location/location": crate::bindings::columns_world::norte::location::location,
        },
    });
}

/// Bindings del world `norte-hook` (H1, ADR 0100). Mismo patrón que
/// [`renamer_world`]: las tres interfaces importadas son las de los otros
/// worlds, servidas por el mismo `HostState`.
pub mod hook_world {
    wasmtime::component::bindgen!({
        world: "norte:hook/norte-hook",
        path: "wit",
        with: {
            "norte:host/host-log": crate::bindings::norte::host::host_log,
            "norte:host/host-config": crate::bindings::norte::host::host_config,
            "norte:location/location": crate::bindings::columns_world::norte::location::location,
        },
    });
}
