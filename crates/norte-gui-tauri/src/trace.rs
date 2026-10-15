//! `NORTE_GUI_TRACE=<file>`: every action the webview sends, its ack, and
//! every update the host sends back, one JSON object per line. The oracle of
//! the end-to-end harness (`scripts/gui-e2e`): what crossed the bridge, read
//! back instead of guessed from a screenshot. Off — and free — without it.

use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

/// The writer's queue, or `None` with tracing off.
fn queue() -> Option<&'static Sender<String>> {
    static QUEUE: OnceLock<Option<Sender<String>>> = OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let path = std::env::var_os("NORTE_GUI_TRACE")?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()?;
            let (tx, rx) = channel::<String>();
            // Its own thread: the lines come from async commands, and a
            // blocking write there would stall the runtime under them.
            std::thread::Builder::new()
                .name("norte-gui-trace".into())
                .spawn(move || {
                    for line in rx {
                        let _ = writeln!(file, "{line}");
                        let _ = file.flush();
                    }
                })
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

/// Records `value` under `kind` (`action`, `ack`, `update`), if tracing.
pub fn record(kind: &str, value: &impl serde::Serialize) {
    let Some(tx) = queue() else {
        return;
    };
    let line = serde_json::json!({ "kind": kind, "value": value });
    let _ = tx.send(line.to_string());
}
