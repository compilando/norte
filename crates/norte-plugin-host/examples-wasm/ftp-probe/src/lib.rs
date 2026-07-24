//! Guest WASM (#30 stage 3b, PROOF): cliente FTP REAL con `suppaftp` SYNC sobre
//! `wasi:sockets`. `command::run(_, arg)` con `arg = "ip:puerto"` conecta al
//! servidor FTP, se loguea anónimo, lista `/` en BINARIO y devuelve el nº de
//! entradas. De-riskea el stack completo del stage 3b (capability `net` gateada
//! → FTP sync compilado a wasm → servidor FTP real) sin el port entero del
//! provider (milestone aparte). `previewer::render` = no-soportado.

use suppaftp::FtpStream;
use suppaftp::types::FileType;

wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
});

use exports::norte::plugin::command::Guest as CommandGuest;
use exports::norte::plugin::previewer::{Guest as PreviewerGuest, PreviewInput, Span};
use norte::plugin::host_log;

struct FtpProbe;

impl CommandGuest for FtpProbe {
    fn run(_id: String, arg: String) -> Result<String, String> {
        host_log::log(&format!("ftp-probe: connect {arg}"));
        let mut ftp = FtpStream::connect(&arg).map_err(|e| format!("connect: {e}"))?;
        ftp.login("anonymous", "anon@norte")
            .map_err(|e| format!("login: {e}"))?;
        ftp.transfer_type(FileType::Binary)
            .map_err(|e| format!("type: {e}"))?;
        let entries = ftp.list(Some("/")).map_err(|e| format!("list: {e}"))?;
        let _ = ftp.quit();
        Ok(format!("{}", entries.len()))
    }
}

impl PreviewerGuest for FtpProbe {
    fn render(_input: PreviewInput) -> Result<String, String> {
        Err("ftp-probe no aporta preview".to_string())
    }

    // ADR 0037 (WIT 0.6.0): export REQUERIDO de `previewer`, guest solo-command.
    fn render_styled(_input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        Err("ftp-probe no aporta preview".to_string())
    }
}

export!(FtpProbe);
