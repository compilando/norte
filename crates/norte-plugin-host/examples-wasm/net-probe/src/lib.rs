//! WASM guest (#30 stage 3a): NETWORK probe. `command::run(_, arg)` connects
//! over TCP to `arg` (`ip:port`), sends `ping` and returns what it
//! receives. Tests the `net` capability's gating: with network granted
//! (allow-list) the connection goes through; without `net`, the host's
//! default `socket_addr_check` REJECTS it and `connect` fails.
//! `previewer::render` responds not-supported (command-only guest).

use std::io::{Read, Write};
use std::net::TcpStream;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::host::host_log;

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
        Err("net-probe does not provide a preview".to_string())
    }

    // ADR 0037 (WIT 0.6.0): REQUIRED export of `previewer`, command-only guest.
    fn render_styled(_input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        Err("net-probe does not provide a preview".to_string())
    }
}

export!(Probe);
