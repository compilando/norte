//! `NORTE_GUI_TRACE=<file>`: every action the webview sends, its ack, and
//! every update the host sends back, one JSON object per line. The oracle of
//! the end-to-end harness (`scripts/gui-e2e`): what crossed the bridge, read
//! back instead of guessed from a screenshot.
//!
//! **Debug builds only.** It records keystrokes — into the terminal panel
//! too — pastes and file contents, which no denylist of field names can keep
//! out; a published binary must not do that because a variable was set (or
//! inherited). A release build compiles it to nothing.

use std::sync::OnceLock;
use std::sync::mpsc::SyncSender;

static QUEUE: OnceLock<Option<SyncSender<String>>> = OnceLock::new();

/// Opens the trace if asked to and allowed to. Called from `main` before the
/// runtime starts: the open is blocking I/O, and later it would happen
/// inside an async command.
pub fn init() {
    QUEUE.get_or_init(open);
}

/// Whether the trace is on: the renderer's probes report only then.
#[must_use]
pub fn enabled() -> bool {
    QUEUE.get_or_init(open).is_some()
}

#[cfg(not(debug_assertions))]
fn open() -> Option<SyncSender<String>> {
    None
}

#[cfg(debug_assertions)]
fn open() -> Option<SyncSender<String>> {
    use std::io::Write as _;
    /// The most the trace writes, so a long session cannot fill a disk.
    const MAX_BYTES: u64 = 256 * 1024 * 1024;
    let path = std::env::var_os("NORTE_GUI_TRACE")?;
    let mut options = std::fs::OpenOptions::new();
    // A NEW file, owner-only: an existing one — or a symlink planted in a
    // shared directory — is refused rather than appended to.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).ok()?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(4096);
    // Its own thread: the lines come from async commands, and a blocking
    // write there would stall the runtime under them.
    std::thread::Builder::new()
        .name("norte-gui-trace".into())
        .spawn(move || {
            let mut written = 0u64;
            for line in rx {
                written += line.len() as u64 + 1;
                if written > MAX_BYTES {
                    break;
                }
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
        })
        .ok()?;
    Some(tx)
}

/// Records `value` under `kind` (`action`, `ack`, `update`, `dom`), if
/// tracing. A dialog's typed secret is blanked even here.
pub fn record(kind: &str, value: &impl serde::Serialize) {
    let Some(tx) = QUEUE.get_or_init(open) else {
        return;
    };
    let mut value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    redact(&mut value);
    let line = serde_json::json!({ "kind": kind, "value": value });
    // A full queue drops the line rather than blocking the window.
    let _ = tx.try_send(line.to_string());
}

/// Every TEXT field named like a secret, at any depth, replaced by a marker.
/// Only strings: `input_secret: true` is a fact about a dialog, not a secret.
fn redact(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, field) in map.iter_mut() {
                let k = k.to_ascii_lowercase();
                let named =
                    k.contains("secret") || k.contains("password") || k.contains("passphrase");
                if named && field.is_string() {
                    *field = serde_json::Value::String("<redacted>".into());
                } else {
                    redact(field);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialogs_secret_never_reaches_the_trace() {
        let action = norte_ui_host::UiAction::Dialog {
            id: norte_ui_host::ModalId(3),
            choice: "confirm".to_owned(),
            secret: Some("hunter2".to_owned()),
        };
        let mut v = serde_json::to_value(&action).expect("json");
        redact(&mut v);
        let text = v.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
        let mut other = serde_json::json!({
            "secret": null,
            "input_secret": true,
            "nested": [{ "Password": "x" }],
        });
        redact(&mut other);
        assert_eq!(other["secret"], serde_json::Value::Null);
        assert_eq!(other["input_secret"], true, "a flag is not a secret");
        assert_eq!(other["nested"][0]["Password"], "<redacted>");
    }
}
