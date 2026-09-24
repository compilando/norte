//! Example WASM guest (M4-P2, P2 Task 3): a minimal command.
//!
//! Exports BOTH interfaces of the `norte-plugin` world (the world requires
//! both): `command::run` dispatches by `id` (`echo`/`shout`/`read`/`config`)
//! and logs a line via `host-log::log`; the `read` command calls
//! `host-log::read-scoped` to demonstrate that `fs-read` enforcement lives
//! on the HOST (ADR 0022 D4); the `config` command calls `host-config::get`
//! to demonstrate `[config]`'s delivery (P2) — `arg` is the KEY, the
//! output is the resolved value (default or override) exactly as the guest
//! sees it. `previewer::render` responds "not supported" because this guest
//! is command-category only.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

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
use norte::host::{host_config, host_log};

struct Demo;

impl CommandGuest for Demo {
    fn run(id: String, arg: String) -> Result<String, String> {
        host_log::log("command-demo: run");
        match id.as_str() {
            "echo" => Ok(arg),
            "shout" => Ok(arg.to_uppercase()),
            "read" => {
                // The guest ALWAYS tries to read; the HOST closes the gate
                // if `fs-read` was not declared (host-side enforcement).
                let bytes = host_log::read_scoped("demo")?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            "config" => {
                // P2 Task 3: `arg` is the `[config]` KEY; the host has
                // ALREADY resolved defaults+overrides before instantiating
                // (Task 2), the guest just echoes it — demonstrates the
                // end-to-end delivery.
                host_config::get(&arg).ok_or_else(|| format!("unknown config key: {arg}"))
            }
            // Infinite loop on purpose: the HOST cuts it off by epoch
            // deadline (hard rule 3). Without enforcement, it would hang
            // the host thread.
            "spin" =>
            {
                #[allow(clippy::empty_loop)]
                loop {}
            }
            other => Err(format!("unknown command: {other}")),
        }
    }
}

impl PreviewerGuest for Demo {
    fn render(_input: PreviewInput) -> Result<String, String> {
        Err("command-demo does not provide previews".to_string())
    }

    // ADR 0037 (WIT 0.6.0): `render-styled` is a REQUIRED export of
    // `previewer` — this guest is `command`-only, so it responds with the
    // same "not supported" as `render`.
    fn render_styled(
        _input: PreviewInput,
    ) -> Result<alloc::vec::Vec<alloc::vec::Vec<Span>>, String> {
        Err("command-demo does not provide previews".to_string())
    }
}

export!(Demo);
