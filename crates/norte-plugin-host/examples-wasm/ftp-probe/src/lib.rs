//! WASM guest (#30 stage 3b, PROOF): REAL FTP client with SYNC
//! `suppaftp` over `wasi:sockets`. `command::run(_, arg)` with
//! `arg = "ip:port"` connects to the FTP server, logs in anonymously,
//! lists `/` in BINARY and returns the entry count. De-risks stage 3b's
//! whole stack (gated `net` capability → wasm-compiled sync FTP → real
//! FTP server) without the provider's full port (a separate milestone).
//! `previewer::render` = not-supported.

use suppaftp::FtpStream;
use suppaftp::types::FileType;

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
        Err("ftp-probe does not provide a preview".to_string())
    }

    // ADR 0037 (WIT 0.6.0): REQUIRED export of `previewer`, command-only guest.
    fn render_styled(_input: PreviewInput) -> Result<Vec<Vec<Span>>, String> {
        Err("ftp-probe does not provide a preview".to_string())
    }
}

export!(FtpProbe);
