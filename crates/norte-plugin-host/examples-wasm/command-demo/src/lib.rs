//! Guest WASM de ejemplo (M4-P2, P2 Task 3): un command mínimo.
//!
//! Exporta AMBAS interfaces del world `norte-plugin` (el world las exige las
//! dos): `command::run` despacha por `id` (`echo`/`shout`/`read`/`config`) y
//! registra una línea vía `host-log::log`; el comando `read` llama a
//! `host-log::read-scoped` para demostrar que el enforcement de `fs-read` vive
//! en el HOST (ADR 0022 D4); el comando `config` llama a `host-config::get`
//! para demostrar la entrega de `[config]` (P2) — `arg` es la CLAVE, la
//! salida es el valor resuelto (default u override) tal cual lo ve el guest.
//! `previewer::render` responde "no soportado" porque este guest es solo de
//! categoría command.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    // `host-log`/`host-config` viven en OTRO paquete desde la partición
    // (ADR 0041 decisión 4); wit-bindgen exige decidir explícitamente qué
    // hacer con los imports de fuera del paquete del world.
    generate_all,
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::host::{host_config, host_log};

struct Demo;

impl CommandGuest for Demo {
    fn run(id: String, arg: String) -> Result<String, String> {
        host_log::log("command-demo: run");
        match id.as_str() {
            "echo" => Ok(arg),
            "shout" => Ok(arg.to_uppercase()),
            "read" => {
                // El guest SIEMPRE intenta leer; el HOST cierra la puerta si
                // `fs-read` no se declaró (enforcement host-side).
                let bytes = host_log::read_scoped("demo")?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            "config" => {
                // P2 Task 3: `arg` es la CLAVE de `[config]`; el host YA
                // resolvió defaults+overrides antes de instanciar (Task 2),
                // el guest solo hace eco — demuestra la entrega end-to-end.
                host_config::get(&arg).ok_or_else(|| format!("clave de config desconocida: {arg}"))
            }
            // Bucle infinito a propósito: el HOST lo corta por deadline de
            // época (regla dura 3). Sin el enforcement, colgaría el hilo host.
            "spin" =>
            {
                #[allow(clippy::empty_loop)]
                loop {}
            }
            other => Err(format!("comando desconocido: {other}")),
        }
    }
}

impl PreviewerGuest for Demo {
    fn render(_input: PreviewInput) -> Result<String, String> {
        Err("command-demo no aporta previews".to_string())
    }

    // ADR 0037 (WIT 0.6.0): `render-styled` es un export REQUERIDO de
    // `previewer` — este guest es solo-`command`, así que responde el mismo
    // "no soportado" que `render`.
    fn render_styled(
        _input: PreviewInput,
    ) -> Result<alloc::vec::Vec<alloc::vec::Vec<Span>>, String> {
        Err("command-demo no aporta previews".to_string())
    }
}

export!(Demo);
