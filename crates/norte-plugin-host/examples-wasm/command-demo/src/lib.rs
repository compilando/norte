//! Guest WASM de ejemplo (M4-P2): un command mínimo.
//!
//! Exporta AMBAS interfaces del world `norte-plugin` (el world las exige las
//! dos): `command::run` despacha por `id` (`echo`/`shout`/`read`) y registra
//! una línea vía `host-log::log`; el comando `read` llama a
//! `host-log::read-scoped` para demostrar que el enforcement de `fs-read` vive
//! en el HOST (ADR 0022 D4). `previewer::render` responde "no soportado"
//! porque este guest es solo de categoría command.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput};
use norte::plugin::host_log;

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
}

export!(Demo);
