//! Guest WASM de ejemplo (M4-P2, P2 Task 4a): un previewer mínimo.
//!
//! Exporta AMBAS interfaces del world `norte-plugin` (el world las exige las
//! dos): `previewer::render` produce una cabecera + las primeras 3 líneas del
//! contenido y registra una línea vía `host-log::log`; `command::run` responde
//! "no soportado" porque este guest es solo de categoría previewer.
//!
//! Si el plugin declara `[config.banner]`, `render` antepone el valor
//! resuelto (leído vía `host-config::get`, P2) al render — demuestra que el
//! previewer TAMBIÉN recibe `[config]` (no solo `command`, Task 3). Sin
//! `[config]`/settings instaladas, `host-config::get` devuelve `none` y el
//! render es idéntico al de antes de P2 (retrocompatible).
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
use norte::plugin::{host_config, host_log};

struct Demo;

impl PreviewerGuest for Demo {
    fn render(input: PreviewInput) -> Result<String, String> {
        let n = input.content.len();
        host_log::log(&format!("previewer-demo: {n} bytes de {}", input.mimetype));
        let text = String::from_utf8_lossy(&input.content);
        let head: alloc::vec::Vec<&str> = text.lines().take(3).collect();
        // P2 Task 4a: `banner` es OPCIONAL — `none` si el manifiesto no
        // declara `[config.banner]` (o no se instalaron settings), en cuyo
        // caso el prefijo queda vacío y el render es igual que sin P2.
        let banner = host_config::get("banner")
            .map(|b| format!("{b}\n"))
            .unwrap_or_default();
        Ok(format!(
            "{banner}[{}] {n} bytes\n{}",
            input.mimetype,
            head.join("\n")
        ))
    }
}

impl CommandGuest for Demo {
    fn run(_id: String, _arg: String) -> Result<String, String> {
        Err("previewer-demo no aporta comandos".to_string())
    }
}

export!(Demo);
