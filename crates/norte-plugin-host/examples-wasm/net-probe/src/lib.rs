//! Guest WASM (#30 stage 3a): sonda de RED. `command::run(_, arg)` conecta por
//! TCP a `arg` (`ip:puerto`), envía `ping` y devuelve lo que recibe. Prueba el
//! gating de la capability `net`: con red concedida (allow-list) la conexión va;
//! sin `net`, el `socket_addr_check` por defecto del host la RECHAZA y `connect`
//! falla. `previewer::render` responde no-soportado (guest solo de comando).

use std::io::{Read, Write};
use std::net::TcpStream;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::plugin::host_log;

struct Probe;

impl CommandGuest for Probe {
    fn run(_id: String, arg: String) -> Result<String, String> {
        host_log::log(&format!("net-probe: connect {arg}"));
        let mut stream = TcpStream::connect(&arg).map_err(|e| format!("connect: {e}"))?;
        stream
            .write_all(b"ping")
            .map_err(|e| format!("write: {e}"))?;
        let mut buf = [0u8; 32];
        let n = stream.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    }
}

impl PreviewerGuest for Probe {
    fn render(_input: PreviewInput) -> Result<String, String> {
        Err("net-probe no aporta preview".to_string())
    }

    // ADR 0037 (WIT 0.6.0): export REQUERIDO de `previewer`, guest solo-command.
    fn render_styled(_input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        Err("net-probe no aporta preview".to_string())
    }
}

export!(Probe);
