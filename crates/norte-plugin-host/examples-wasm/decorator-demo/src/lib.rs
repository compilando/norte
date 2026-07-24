//! Guest WASM de ejemplo (ADR 0037, G3 plan Task 2): un decorator mínimo.
//!
//! Exporta la interfaz `decorator` del world `norte-decorator`: `decorate`
//! recibe el LOTE de nombres/paths crudos de la página visible (regla 1:
//! bytes, jamás asumidos UTF-8; aquí se hace un `from_utf8_lossy` defensivo
//! solo para la clasificación, nunca para el `text` de un span — este guest
//! no toca texto de usuario, solo mira si "contiene mod") y devuelve, POR
//! CADA entrada en el MISMO orden (contrato posicional 1:1, ADR 0037 tabla de
//! decisión 1), un badge `"M"` si el nombre contiene la subcadena `"mod"`, o
//! ninguno (`badge: none`) en caso contrario. Determinista a propósito: el
//! e2e del host verifica el round-trip posicional exacto sin depender de
//! ningún estado externo.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-decorator",
    path: "wit",
});

use exports::norte::plugin::decorator::{Decoration, Guest as DecoratorGuest};
use norte::plugin::host_log;

struct DecoratorDemo;

impl DecoratorGuest for DecoratorDemo {
    fn decorate(entries: Vec<Vec<u8>>) -> Vec<Decoration> {
        host_log::log(&format!("decorator-demo: {} entradas", entries.len()));
        entries
            .iter()
            .map(|raw| {
                let name = String::from_utf8_lossy(raw);
                if name.contains("mod") {
                    Decoration {
                        badge: Some("M".to_string()),
                        role: Some("warning".to_string()),
                    }
                } else {
                    Decoration {
                        badge: None,
                        role: None,
                    }
                }
            })
            .collect()
    }
}

export!(DecoratorDemo);
